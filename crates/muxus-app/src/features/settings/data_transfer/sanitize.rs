//! The second half of the restore barrier (`sanitizePreferences` in
//! `client/src/data-transfer.ts`): structure is validated when the file is
//! read, individual preference values here. Anything that fails is dropped
//! rather than rejecting the whole backup.

use serde_json::{Map, Value};

use muxus_core::js;
use muxus_core::paste_pacing::{
    MAX_PASTE_CHAR_DELAY_MS, MAX_PASTE_LINE_DELAY_MS, valid_paste_delay,
};
use muxus_store::prefs::PrefsState;
use muxus_store::prefs::command_buttons::{
    default_command_group, normalize_command_button_groups, normalize_command_buttons,
};
use muxus_store::prefs::custom_schemes::is_custom_terminal_scheme_array;
use muxus_store::prefs::highlight_profiles::{is_hex_color, is_keyword_highlight_profile_array};
use muxus_store::prefs::layout::{
    MAX_INACTIVE_PANE_DIM_STRENGTH, MAX_SIDEBAR_WIDTH, MIN_INACTIVE_PANE_DIM_STRENGTH,
    MIN_SFTP_PANEL_WIDTH, MIN_SIDEBAR_WIDTH, is_status_bar_item_list,
};
use muxus_store::prefs::state::is_local_shell_profile_array;

use crate::sidebar::folder_icons::is_folder_icon_id;

/// The checked preferences of a backup, as a patch keyed like the stored
/// preferences (`themeMode`, `monoFontSize`, …). Keys whose value fails its
/// check are left out, so the current value stays. A legacy single
/// `terminalScheme` fills both appearances when they are missing.
pub fn sanitize_preferences(input: &Map<String, Value>) -> Map<String, Value> {
    let get = |key: &str| input.get(key);
    let mut output = Map::new();
    let keep = |output: &mut Map<String, Value>, key: &str, ok: bool| {
        if ok && let Some(value) = input.get(key) {
            output.insert(key.to_string(), value.clone());
        }
    };

    keep(
        &mut output,
        "themeMode",
        one_of(get("themeMode"), &["light", "dark", "os"]),
    );
    keep(
        &mut output,
        "monoFontSize",
        finite_range(get("monoFontSize"), 8.0, 24.0),
    );
    keep(
        &mut output,
        "fontFamily",
        bounded_text(get("fontFamily"), 200),
    );
    keep(
        &mut output,
        "lineHeight",
        finite_range(get("lineHeight"), 1.0, 1.6),
    );
    let legacy_scheme = get("terminalScheme").filter(|value| valid_terminal_scheme(value));
    for key in ["lightTerminalScheme", "darkTerminalScheme"] {
        if get(key).is_some_and(valid_terminal_scheme) {
            keep(&mut output, key, true);
        } else if let Some(legacy) = legacy_scheme {
            output.insert(key.to_string(), legacy.clone());
        }
    }
    keep(
        &mut output,
        "customTerminalSchemes",
        get("customTerminalSchemes").is_some_and(is_custom_terminal_scheme_array),
    );
    for key in ["fontColor", "backgroundColor"] {
        let ok = get(key).is_some_and(|value| value.as_str() == Some("") || valid_hex_color(value));
        keep(&mut output, key, ok);
    }
    keep(
        &mut output,
        "activePaneBorder",
        is_bool(get("activePaneBorder")),
    );
    keep(
        &mut output,
        "dimInactivePanes",
        is_bool(get("dimInactivePanes")),
    );
    keep(
        &mut output,
        "inactivePaneDimStrength",
        finite_range(
            get("inactivePaneDimStrength"),
            MIN_INACTIVE_PANE_DIM_STRENGTH,
            MAX_INACTIVE_PANE_DIM_STRENGTH,
        ),
    );
    keep(
        &mut output,
        "scrollback",
        is_integer(get("scrollback")) && finite_range(get("scrollback"), 0.0, 1_000_000.0),
    );
    keep(&mut output, "cursorBlink", is_bool(get("cursorBlink")));
    keep(
        &mut output,
        "cursorStyle",
        one_of(get("cursorStyle"), &["block", "underline", "bar"]),
    );
    keep(&mut output, "webglRenderer", is_bool(get("webglRenderer")));
    keep(
        &mut output,
        "localShell",
        bounded_text(get("localShell"), 4096),
    );
    keep(
        &mut output,
        "localShellProfiles",
        get("localShellProfiles").is_some_and(is_local_shell_profile_array),
    );
    // The default must name a profile restored together with it.
    let default_profile_ok = get("defaultLocalShellProfileId")
        .and_then(Value::as_str)
        .is_some_and(|id| {
            js::utf16_len(id) <= 200
                && (id.is_empty()
                    || output
                        .get("localShellProfiles")
                        .and_then(Value::as_array)
                        .is_some_and(|profiles| {
                            profiles.iter().any(|profile| {
                                profile.get("id").and_then(Value::as_str) == Some(id)
                            })
                        }))
        });
    keep(
        &mut output,
        "defaultLocalShellProfileId",
        default_profile_ok,
    );
    for key in [
        "showWslDistributions",
        "copyOnSelect",
        "allowOsc52ClipboardWrite",
    ] {
        keep(&mut output, key, is_bool(get(key)));
    }
    keep(
        &mut output,
        "rightClickAction",
        one_of(get("rightClickAction"), &["copy-paste", "paste", "menu"]),
    );
    keep(
        &mut output,
        "terminalFileLinkActivation",
        one_of(
            get("terminalFileLinkActivation"),
            &["direct", "alt", "ctrl", "meta"],
        ),
    );
    keep(
        &mut output,
        "pasteWarnMultiline",
        is_bool(get("pasteWarnMultiline")),
    );
    keep(
        &mut output,
        "pasteLineDelayMs",
        get("pasteLineDelayMs")
            .is_some_and(|value| valid_paste_delay(value, MAX_PASTE_LINE_DELAY_MS)),
    );
    keep(
        &mut output,
        "pasteCharDelayMs",
        get("pasteCharDelayMs")
            .is_some_and(|value| valid_paste_delay(value, MAX_PASTE_CHAR_DELAY_MS)),
    );
    for key in [
        "confirmCloseConnected",
        "sshSessionSummary",
        "rememberPasswordsByDefault",
    ] {
        keep(&mut output, key, is_bool(get(key)));
    }
    keep(
        &mut output,
        "newSshHostStorage",
        one_of(get("newSshHostStorage"), &["openssh", "muxus"]),
    );
    keep(
        &mut output,
        "notifyOnNewVersion",
        is_bool(get("notifyOnNewVersion")),
    );

    // Buttons are checked against the groups in the same backup; one from an
    // older release has none, so its buttons all land in the default group.
    let groups =
        normalize_command_button_groups(get("commandButtonGroups").unwrap_or(&Value::Null));
    if let Some(groups) = &groups
        && let Ok(value) = serde_json::to_value(groups)
    {
        output.insert("commandButtonGroups".to_string(), value);
    }
    let fallback_groups = [default_command_group()];
    let buttons = normalize_command_buttons(
        get("commandButtons").unwrap_or(&Value::Null),
        groups.as_deref().unwrap_or(&fallback_groups),
    );
    if let Some(buttons) = buttons
        && let Ok(value) = serde_json::to_value(buttons)
    {
        output.insert("commandButtons".to_string(), value);
    }
    let selected_ok = get("selectedCommandButtonGroup")
        .and_then(Value::as_str)
        .is_some_and(|selected| {
            groups
                .as_ref()
                .is_some_and(|groups| groups.iter().any(|group| group.id == selected))
        });
    keep(&mut output, "selectedCommandButtonGroup", selected_ok);
    keep(
        &mut output,
        "showCommandBar",
        is_bool(get("showCommandBar")),
    );
    keep(
        &mut output,
        "commandBarPosition",
        one_of(get("commandBarPosition"), &["top", "bottom"]),
    );
    keep(&mut output, "showStatusBar", is_bool(get("showStatusBar")));
    keep(
        &mut output,
        "statusBarItems",
        get("statusBarItems").is_some_and(is_status_bar_item_list),
    );
    keep(
        &mut output,
        "keywordHighlights",
        get("keywordHighlights")
            .and_then(Value::as_array)
            .is_some_and(|rules| rules.len() <= 100 && rules.iter().all(valid_keyword_highlight)),
    );
    keep(
        &mut output,
        "keywordHighlightProfiles",
        get("keywordHighlightProfiles").is_some_and(is_keyword_highlight_profile_array),
    );
    keep(
        &mut output,
        "sidebarCollapsed",
        is_bool(get("sidebarCollapsed")),
    );
    keep(
        &mut output,
        "sidebarPosition",
        one_of(get("sidebarPosition"), &["left", "right"]),
    );
    keep(
        &mut output,
        "fileBrowserPosition",
        one_of(get("fileBrowserPosition"), &["pane", "sidebar"]),
    );
    keep(
        &mut output,
        "sidebarOpenGesture",
        one_of(get("sidebarOpenGesture"), &["click", "double-click"]),
    );
    keep(
        &mut output,
        "sidebarWidth",
        finite_range(get("sidebarWidth"), MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH),
    );
    keep(
        &mut output,
        "sftpPanelWidth",
        finite_range(get("sftpPanelWidth"), MIN_SFTP_PANEL_WIDTH, 1200.0),
    );
    keep(
        &mut output,
        "sidebarCollapsedFolders",
        bounded_path_list(get("sidebarCollapsedFolders")),
    );
    keep(
        &mut output,
        "sidebarEmptyFolders",
        bounded_path_list(get("sidebarEmptyFolders")),
    );
    keep(
        &mut output,
        "sidebarFolderStyles",
        valid_folder_styles(get("sidebarFolderStyles")),
    );
    keep(
        &mut output,
        "sidebarFolderOrder",
        valid_folder_order(get("sidebarFolderOrder")),
    );
    output
}

/// Merge a [`sanitize_preferences`] patch into preferences (the store's `set(patch)`).
pub fn apply_preferences_patch(prefs: &PrefsState, patch: &Map<String, Value>) -> PrefsState {
    let mut map = prefs.to_json_map();
    for (key, value) in patch {
        map.insert(key.clone(), value.clone());
    }
    PrefsState::from_json_map(&map)
}

fn is_bool(value: Option<&Value>) -> bool {
    value.is_some_and(Value::is_boolean)
}

fn is_integer(value: Option<&Value>) -> bool {
    super::validate::is_integer(value)
}

fn one_of(value: Option<&Value>, choices: &[&str]) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| choices.contains(&text))
}

fn bounded_text(value: Option<&Value>, max: usize) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| js::utf16_len(text) <= max)
}

/// `validTerminalSchemePreference`.
fn valid_terminal_scheme(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| js::utf16_len(text) <= 100)
}

/// `finiteRange`.
fn finite_range(value: Option<&Value>, min: f64, max: f64) -> bool {
    value
        .and_then(Value::as_f64)
        .is_some_and(|number| number.is_finite() && number >= min && number <= max)
}

/// `validHexColor`: `/^#[0-9a-f]{6}$/i`.
fn valid_hex_color(value: &Value) -> bool {
    value.as_str().is_some_and(is_hex_color)
}

/// Folder keys and paths: bounded in both count and length.
fn bounded_path_list(value: Option<&Value>) -> bool {
    value.and_then(Value::as_array).is_some_and(|entries| {
        entries.len() <= 500
            && entries.iter().all(|entry| {
                entry
                    .as_str()
                    .is_some_and(|text| js::utf16_len(text) <= 400)
            })
    })
}

fn valid_folder_styles(value: Option<&Value>) -> bool {
    value.and_then(Value::as_object).is_some_and(|styles| {
        styles.len() <= 500
            && styles.iter().all(|(key, style)| {
                js::utf16_len(key) <= 400
                    && style.as_object().is_some_and(|style| {
                        style.get("color").is_none_or(valid_hex_color)
                            && style
                                .get("icon")
                                .is_none_or(|icon| icon.as_str().is_some_and(is_folder_icon_id))
                    })
            })
    })
}

fn valid_folder_order(value: Option<&Value>) -> bool {
    value.and_then(Value::as_object).is_some_and(|order| {
        order.len() <= 500
            && order
                .iter()
                .all(|(key, keys)| js::utf16_len(key) <= 400 && bounded_path_list(Some(keys)))
    })
}

/// A global highlighting rule (`validKeywordHighlight`).
fn valid_keyword_highlight(value: &Value) -> bool {
    let Some(rule) = value.as_object() else {
        return false;
    };
    let get = |key: &str| rule.get(key);
    super::validate::non_empty_string(get("id"))
        && get("name").is_none_or(|name| {
            name.as_str()
                .is_some_and(|name| !name.is_empty() && js::utf16_len(name) <= 100)
        })
        && get("keyword")
            .and_then(Value::as_str)
            .is_some_and(|keyword| !keyword.is_empty() && js::utf16_len(keyword) <= 500)
        && get("foreground").is_some_and(valid_hex_color)
        && get("background").is_none_or(valid_hex_color)
        && is_bool(get("caseSensitive"))
        && is_bool(get("wholeWord"))
        && get("regex").is_none_or(Value::is_boolean)
}
