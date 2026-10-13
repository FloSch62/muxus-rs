//! Upgrading stored preferences (`migratePrefsState`) and laying them over
//! the live ones (`mergePersistedPrefs`), ported line by line.
//!
//! Both work on JSON, as the TypeScript does: a stored snapshot is untrusted
//! until it has been migrated, and key order is kept so the written file
//! matches the Electron build's byte for byte.

use serde_json::{Map, Value};

use muxus_core::paste_pacing::{
    MAX_PASTE_CHAR_DELAY_MS, MAX_PASTE_LINE_DELAY_MS, valid_paste_delay,
};
use muxus_core::protocol::MAX_SSH_KEEPALIVE_INTERVAL_SECONDS;

use super::builtin_highlight_profiles::{
    is_truthy, with_builtin_highlight_profiles_json, with_builtin_rule_names_json,
};
use super::command_buttons::{
    default_command_group, normalize_command_button_groups, normalize_command_buttons,
};
use super::custom_schemes::is_custom_terminal_scheme_array;
use super::highlight_profiles::is_keyword_highlight_profile_array;
use super::layout::{
    DEFAULT_SIDEBAR_WIDTH, MAX_INACTIVE_PANE_DIM_STRENGTH, MIN_INACTIVE_PANE_DIM_STRENGTH,
    PREVIOUS_DEFAULT_SIDEBAR_WIDTH, is_status_bar_item_list,
};
use super::state::is_local_shell_profile_array;

fn is_one_of(value: Option<&Value>, allowed: &[&str]) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| allowed.contains(&text))
}

fn is_bool(value: Option<&Value>) -> bool {
    value.is_some_and(Value::is_boolean)
}

fn is_string_array(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|entries| entries.iter().all(Value::is_string))
}

fn is_folder_order_map(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|map| map.values().all(is_string_array))
}

fn is_folder_style_map(value: &Value) -> bool {
    value.as_object().is_some_and(|map| {
        map.values().all(|style| {
            style.as_object().is_some_and(|style| {
                style
                    .iter()
                    .all(|(key, entry)| (key == "color" || key == "icon") && entry.is_string())
            })
        })
    })
}

/// `delete state[key]` when `keep` says the value is unusable.
fn drop_unless(state: &mut Map<String, Value>, key: &str, keep: impl Fn(Option<&Value>) -> bool) {
    if !keep(state.get(key)) {
        state.shift_remove(key);
    }
}

/// `{ ...persisted }`: an object's own keys, an array's indices as keys.
fn spread(persisted: &Value) -> Option<Map<String, Value>> {
    match persisted {
        Value::Object(map) => Some(map.clone()),
        Value::Array(entries) => Some(
            entries
                .iter()
                .enumerate()
                .map(|(index, value)| (index.to_string(), value.clone()))
                .collect(),
        ),
        _ => None,
    }
}

/// Upgrade persisted preferences without mutating the storage snapshot.
/// `version` is the stored envelope's version.
pub fn migrate_prefs_state(persisted: &Value, version: f64) -> Value {
    let Some(mut state) = spread(persisted) else {
        return persisted.clone();
    };
    let state = &mut state;
    // A missing or invalid value falls through to the store's System default.
    drop_unless(state, "themeMode", |v| {
        is_one_of(v, &["light", "os", "dark"])
    });
    drop_unless(state, "tabNumberVisibility", |v| {
        is_one_of(v, &["shortcut", "always"])
    });
    drop_unless(state, "sidebarPosition", |v| {
        is_one_of(v, &["left", "right"])
    });
    drop_unless(state, "commandBarPosition", |v| {
        is_one_of(v, &["top", "bottom"])
    });
    drop_unless(state, "fileBrowserPosition", |v| {
        is_one_of(v, &["pane", "sidebar"])
    });
    drop_unless(state, "sidebarOpenGesture", |v| {
        is_one_of(v, &["click", "double-click"])
    });
    drop_unless(state, "terminalFileLinkActivation", |v| {
        is_one_of(v, &["direct", "alt", "ctrl", "meta"])
    });
    drop_unless(state, "sshKeepaliveIntervalSeconds", |v| {
        v.and_then(Value::as_f64).is_some_and(|seconds| {
            seconds.fract() == 0.0
                && seconds >= 0.0
                && seconds <= f64::from(MAX_SSH_KEEPALIVE_INTERVAL_SECONDS)
        })
    });
    drop_unless(state, "sshSessionSummary", is_bool);
    drop_unless(state, "pasteLineDelayMs", |v| {
        v.is_some_and(|v| valid_paste_delay(v, MAX_PASTE_LINE_DELAY_MS))
    });
    drop_unless(state, "pasteCharDelayMs", |v| {
        v.is_some_and(|v| valid_paste_delay(v, MAX_PASTE_CHAR_DELAY_MS))
    });
    drop_unless(state, "showStatusBar", is_bool);
    drop_unless(state, "statusBarItems", |v| {
        v.is_some_and(is_status_bar_item_list)
    });
    drop_unless(state, "rememberPasswordsByDefault", is_bool);
    drop_unless(state, "newSshHostStorage", |v| {
        is_one_of(v, &["openssh", "muxus"])
    });
    drop_unless(state, "activePaneBorder", is_bool);
    drop_unless(state, "dimInactivePanes", is_bool);
    drop_unless(state, "webglRenderer", is_bool);
    drop_unless(state, "showWslDistributions", is_bool);
    drop_unless(state, "inactivePaneDimStrength", |v| {
        v.and_then(Value::as_f64).is_some_and(|strength| {
            strength.is_finite()
                && (MIN_INACTIVE_PANE_DIM_STRENGTH..=MAX_INACTIVE_PANE_DIM_STRENGTH)
                    .contains(&strength)
        })
    });
    // v0 shipped the Muxus scheme as the default; stored copies of that
    // default follow the new one.
    let mut legacy_terminal_scheme = state.get("terminalScheme").cloned();
    if version == 0.0 && legacy_terminal_scheme.as_ref().and_then(Value::as_str) == Some("muxus") {
        legacy_terminal_scheme = Some(Value::String("vscode-dark".into()));
    }
    // v9 split the single scheme preference by effective appearance. Copying
    // the old value to both modes preserves the terminal users saw before the
    // upgrade; they can then choose a different scheme for either mode.
    if version < 9.0
        && let Some(legacy) = legacy_terminal_scheme.filter(Value::is_string)
    {
        if !state
            .get("lightTerminalScheme")
            .is_some_and(Value::is_string)
        {
            state.insert("lightTerminalScheme".into(), legacy.clone());
        }
        if !state
            .get("darkTerminalScheme")
            .is_some_and(Value::is_string)
        {
            state.insert("darkTerminalScheme".into(), legacy);
        }
    }
    state.shift_remove("terminalScheme");
    drop_unless(state, "customTerminalSchemes", |v| {
        v.is_some_and(is_custom_terminal_scheme_array)
    });
    // TERM is fixed by the backend now; remove the retired client override.
    state.shift_remove("termName");
    // Folder presentation arrived in v5. A restored or hand-edited snapshot can
    // carry the wrong shape here, and every reader assumes the right one.
    drop_unless(state, "sidebarCollapsedFolders", |v| {
        v.is_some_and(is_string_array)
    });
    drop_unless(state, "sidebarEmptyFolders", |v| {
        v.is_some_and(is_string_array)
    });
    drop_unless(state, "sidebarFolderStyles", |v| {
        v.is_some_and(is_folder_style_map)
    });
    drop_unless(state, "sidebarFolderOrder", |v| {
        v.is_some_and(is_folder_order_map)
    });
    let local_shell_profiles = state
        .get("localShellProfiles")
        .filter(|v| is_local_shell_profile_array(v))
        .cloned();
    if local_shell_profiles.is_none() {
        state.shift_remove("localShellProfiles");
    }
    match state
        .get("defaultLocalShellProfileId")
        .and_then(Value::as_str)
    {
        None => {
            state.shift_remove("defaultLocalShellProfileId");
        }
        Some(id) if !id.is_empty() => {
            let known = local_shell_profiles
                .as_ref()
                .and_then(Value::as_array)
                .is_some_and(|profiles| {
                    profiles
                        .iter()
                        .any(|profile| profile.get("id").and_then(Value::as_str) == Some(id))
                });
            if !known {
                state.insert(
                    "defaultLocalShellProfileId".into(),
                    Value::String(String::new()),
                );
            }
        }
        Some(_) => {}
    }
    drop_unless(state, "keywordHighlightProfiles", |v| {
        v.is_some_and(is_keyword_highlight_profile_array)
    });
    // Groups and colors arrived in v17. Earlier buttons carry neither and stay
    // in the default group; a malformed entry is dropped rather than all of them.
    let groups =
        normalize_command_button_groups(state.get("commandButtonGroups").unwrap_or(&Value::Null));
    match &groups {
        Some(groups) => {
            state.insert(
                "commandButtonGroups".into(),
                serde_json::to_value(groups).unwrap_or(Value::Null),
            );
        }
        None => {
            state.shift_remove("commandButtonGroups");
        }
    }
    let effective_groups = groups
        .clone()
        .unwrap_or_else(|| vec![default_command_group()]);
    match normalize_command_buttons(
        state.get("commandButtons").unwrap_or(&Value::Null),
        &effective_groups,
    ) {
        Some(buttons) => {
            state.insert(
                "commandButtons".into(),
                serde_json::to_value(buttons).unwrap_or(Value::Null),
            );
        }
        None => {
            state.shift_remove("commandButtons");
        }
    }
    drop_unless(state, "selectedCommandButtonGroup", |v| {
        v.and_then(Value::as_str)
            .is_some_and(|id| effective_groups.iter().any(|group| group.id == id))
    });
    // Built-in platform profiles arrived in v15. Existing installations get
    // them once; one deleted afterwards stays deleted. A snapshot without the
    // key picks them up from the store defaults instead.
    if version < 15.0
        && let Some(profiles) = state
            .get("keywordHighlightProfiles")
            .filter(|v| is_truthy(v))
            .and_then(Value::as_array)
    {
        let next = with_builtin_highlight_profiles_json(profiles);
        state.insert("keywordHighlightProfiles".into(), Value::Array(next));
    }
    // v16 named the built-in rules; copies seeded by v15 get the names as well.
    if version < 16.0
        && let Some(profiles) = state
            .get("keywordHighlightProfiles")
            .filter(|v| is_truthy(v))
            .and_then(Value::as_array)
    {
        let next = with_builtin_rule_names_json(profiles);
        state.insert("keywordHighlightProfiles".into(), Value::Array(next));
    }
    // The sidebar grew in v6 to fit its search box. A stored copy of the old
    // default was never a choice, so it follows; a dragged width is left alone.
    if version < 6.0
        && state.get("sidebarWidth").and_then(Value::as_f64) == Some(PREVIOUS_DEFAULT_SIDEBAR_WIDTH)
    {
        state.insert(
            "sidebarWidth".into(),
            muxus_core::js::js_number(DEFAULT_SIDEBAR_WIDTH),
        );
    }
    Value::Object(std::mem::take(state))
}

/// `JSON.stringify(a) === JSON.stringify(b)`: equal values written in the
/// same key order.
pub fn same_json(left: &Value, right: &Value) -> bool {
    // Compared as written text: map equality alone would ignore key order.
    serde_json::to_string(left).ok() == serde_json::to_string(right).ok()
}

/// Lay persisted preferences over the live ones. Values that did not change
/// keep their live copy (`mergePersistedPrefs`); new keys are appended in the
/// persisted order. A persisted value that is not an object leaves the live
/// state as it is.
pub fn merge_persisted_prefs(
    persisted: &Value,
    current: &Map<String, Value>,
) -> Map<String, Value> {
    let Some(persisted) = spread(persisted) else {
        return current.clone();
    };
    let mut next = current.clone();
    for (key, value) in &persisted {
        // The store's setter is a function in the live state; never replaced.
        if key == "set" {
            continue;
        }
        let keep_live = current.get(key).is_some_and(|live| same_json(live, value));
        if !keep_live {
            next.insert(key.clone(), value.clone());
        }
    }
    next
}
