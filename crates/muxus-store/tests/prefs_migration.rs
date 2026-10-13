//! Port of the persistence parts of `tests/unit/client/prefs.test.ts`.

use muxus_store::prefs::builtin_highlight_profiles::{
    builtin_highlight_profiles, builtin_highlight_profiles_json, nokia_srlinux_highlight_profile,
    nokia_sros_highlight_profile,
};
use muxus_store::prefs::layout::{
    DEFAULT_INACTIVE_PANE_DIM_STRENGTH, DEFAULT_SIDEBAR_WIDTH, MONO_FONT_FALLBACK,
    clamp_inactive_pane_dim_strength, pane_focus_opacity, terminal_font_stack,
};
use muxus_store::prefs::state::{
    CommandBarPosition, EffectiveThemeMode, FileBrowserPosition, NewSshHostStorage,
    SidebarOpenGesture, SidebarPosition, TabNumberVisibility, TerminalFileLinkActivation,
    ThemeMode, is_local_shell_profile_array,
};
use muxus_store::prefs::{PrefsState, merge_persisted_prefs, migrate_prefs_state};
use serde_json::{Value, json};

fn migrate(state: Value, version: u32) -> Value {
    migrate_prefs_state(&state, f64::from(version))
}

fn ubuntu_profile() -> Value {
    json!({
        "id": "ubuntu",
        "name": "Ubuntu",
        "shell": "wsl.exe",
        "args": ["-d", "Ubuntu"],
        "cwd": "C:\\work",
        "startupCommand": "cd project",
    })
}

fn custom_scheme(id: &str) -> Value {
    json!({
        "id": id,
        "name": format!("{id} name"),
        "colors": {
            "background": "#2e3440", "foreground": "#d8dee9", "cursor": "#d8dee9", "selectionBackground": "#434c5e",
            "black": "#3b4252", "red": "#bf616a", "green": "#a3be8c", "yellow": "#ebcb8b",
            "blue": "#81a1c1", "magenta": "#b48ead", "cyan": "#88c0d0", "white": "#e5e9f0",
            "brightBlack": "#4c566a", "brightRed": "#bf616a", "brightGreen": "#a3be8c", "brightYellow": "#ebcb8b",
            "brightBlue": "#81a1c1", "brightMagenta": "#b48ead", "brightCyan": "#8fbcbb", "brightWhite": "#eceff4"
        }
    })
}

#[test]
fn command_button_groups_preference() {
    let initial = PrefsState::default();
    assert_eq!(
        serde_json::to_value(&initial.command_button_groups).unwrap(),
        json!([{ "id": "default", "name": "Default" }])
    );
    assert_eq!(initial.selected_command_button_group, "default");

    let uptime =
        json!({ "id": "uptime", "label": "Uptime", "command": "uptime", "sendEnter": true });
    // Buttons saved before groups existed stay in the default group.
    assert_eq!(
        migrate(json!({ "commandButtons": [uptime] }), 16),
        json!({ "commandButtons": [uptime] })
    );

    let mut filed = uptime.clone();
    filed["groupId"] = json!("juniper");
    filed["color"] = json!("blue");
    let mut gone = uptime.clone();
    gone["id"] = json!("gone");
    gone["groupId"] = json!("cisco");
    gone["color"] = json!("neon");
    let mut gone_repaired = uptime.clone();
    gone_repaired["id"] = json!("gone");
    assert_eq!(
        migrate(
            json!({
                "commandButtonGroups": [{ "id": "juniper", "name": "Juniper" }, { "id": "juniper", "name": "Twice" }],
                "commandButtons": [filed, gone, { "id": "broken" }],
                "selectedCommandButtonGroup": "cisco",
            }),
            16
        ),
        json!({
            "commandButtonGroups": [{ "id": "default", "name": "Default" }, { "id": "juniper", "name": "Juniper" }],
            "commandButtons": [filed, gone_repaired],
        })
    );
    let kept = migrate(
        json!({ "commandButtonGroups": [{ "id": "juniper", "name": "Juniper" }], "selectedCommandButtonGroup": "juniper" }),
        16,
    );
    assert_eq!(kept["selectedCommandButtonGroup"], json!("juniper"));
}

#[test]
fn ssh_keepalive_preference() {
    assert_eq!(PrefsState::default().ssh_keepalive_interval_seconds, 30.0);
    assert_eq!(PrefsState::default().ssh_keepalive_field(), Some(30));
    assert_eq!(
        migrate(json!({ "sshKeepaliveIntervalSeconds": 60 }), 13),
        json!({ "sshKeepaliveIntervalSeconds": 60 })
    );
    assert_eq!(
        migrate(
            json!({ "sshKeepaliveIntervalSeconds": -1, "monoFontSize": 16 }),
            13
        ),
        json!({ "monoFontSize": 16 })
    );
}

#[test]
fn paste_pacing_preferences() {
    let initial = PrefsState::default();
    assert_eq!(
        (initial.paste_line_delay_ms, initial.paste_char_delay_ms),
        (0.0, 0.0)
    );
    assert_eq!(
        migrate(
            json!({ "pasteLineDelayMs": 250, "pasteCharDelayMs": 5 }),
            17
        ),
        json!({ "pasteLineDelayMs": 250, "pasteCharDelayMs": 5 })
    );
    assert_eq!(
        migrate(
            json!({ "pasteLineDelayMs": 10_001, "pasteCharDelayMs": 1.5, "monoFontSize": 16 }),
            17
        ),
        json!({ "monoFontSize": 16 })
    );
    assert_eq!(
        migrate(
            json!({ "pasteLineDelayMs": "100", "pasteCharDelayMs": -1 }),
            17
        ),
        json!({})
    );
}

#[test]
fn appearance_preference() {
    assert_eq!(PrefsState::default().theme_mode, ThemeMode::Os);
    for mode in ["light", "dark"] {
        assert_eq!(
            migrate(json!({ "themeMode": mode }), 0)["themeMode"],
            json!(mode)
        );
    }
    assert_eq!(
        migrate(json!({ "themeMode": "sepia", "monoFontSize": 16 }), 7),
        json!({ "monoFontSize": 16 })
    );
}

#[test]
fn layout_preferences() {
    let initial = PrefsState::default();
    assert_eq!(initial.sidebar_position, SidebarPosition::Left);
    assert_eq!(initial.file_browser_position, FileBrowserPosition::Pane);
    assert_eq!(initial.command_bar_position, CommandBarPosition::Top);
    let valid = json!({ "sidebarPosition": "right", "fileBrowserPosition": "sidebar", "commandBarPosition": "bottom" });
    assert_eq!(migrate(valid.clone(), 16), valid);
    assert_eq!(
        migrate(
            json!({ "sidebarPosition": "bottom", "fileBrowserPosition": "left", "commandBarPosition": "left", "monoFontSize": 16 }),
            16
        ),
        json!({ "monoFontSize": 16 })
    );
}

#[test]
fn sidebar_open_gesture_preference() {
    assert_eq!(
        PrefsState::default().sidebar_open_gesture,
        SidebarOpenGesture::Click
    );
    for gesture in ["click", "double-click"] {
        assert_eq!(
            migrate(json!({ "sidebarOpenGesture": gesture }), 16),
            json!({ "sidebarOpenGesture": gesture })
        );
    }
    assert_eq!(
        migrate(
            json!({ "sidebarOpenGesture": "triple-click", "monoFontSize": 16 }),
            16
        ),
        json!({ "monoFontSize": 16 })
    );
}

#[test]
fn status_bar_preferences() {
    let initial = PrefsState::default();
    assert!(initial.show_status_bar);
    assert_eq!(
        serde_json::to_value(&initial.status_bar_items).unwrap(),
        json!([
            "hostname", "os", "cpu", "memory", "disk", "network", "uptime", "users"
        ])
    );
    let valid = json!({ "showStatusBar": true, "statusBarItems": ["cpu", "disk"] });
    assert_eq!(migrate(valid.clone(), 17), valid);
    assert_eq!(
        migrate(json!({ "statusBarItems": [] }), 17),
        json!({ "statusBarItems": [] })
    );
    assert_eq!(
        migrate(
            json!({ "showStatusBar": "yes", "statusBarItems": ["cpu", "temperature"], "monoFontSize": 16 }),
            17
        ),
        json!({ "monoFontSize": 16 })
    );
    assert_eq!(
        migrate(json!({ "statusBarItems": ["cpu", "cpu"] }), 17),
        json!({})
    );
}

#[test]
fn terminal_file_link_activation_preference() {
    assert_eq!(
        PrefsState::default().terminal_file_link_activation,
        TerminalFileLinkActivation::Alt
    );
    for activation in ["direct", "alt", "ctrl", "meta"] {
        assert_eq!(
            migrate(json!({ "terminalFileLinkActivation": activation }), 12),
            json!({ "terminalFileLinkActivation": activation })
        );
    }
    assert_eq!(
        migrate(
            json!({ "terminalFileLinkActivation": "double-click", "monoFontSize": 16 }),
            12
        ),
        json!({ "monoFontSize": 16 })
    );
}

#[test]
fn split_pane_focus_preferences() {
    let initial = PrefsState::default();
    assert!(!initial.active_pane_border && !initial.dim_inactive_panes);
    assert_eq!(
        initial.inactive_pane_dim_strength,
        DEFAULT_INACTIVE_PANE_DIM_STRENGTH
    );
    let valid = json!({ "activePaneBorder": false, "dimInactivePanes": true, "inactivePaneDimStrength": 0.4 });
    assert_eq!(migrate(valid.clone(), 10), valid);
    assert_eq!(
        migrate(
            json!({ "activePaneBorder": "yes", "dimInactivePanes": 1, "inactivePaneDimStrength": 0.95 }),
            10
        ),
        json!({})
    );
    assert_eq!(pane_focus_opacity(true, true, 0.4), 1.0);
    assert_eq!(pane_focus_opacity(false, false, 0.4), 1.0);
    assert_eq!(pane_focus_opacity(false, true, 0.4), 0.6);
    assert_eq!(
        clamp_inactive_pane_dim_strength(f64::NAN),
        DEFAULT_INACTIVE_PANE_DIM_STRENGTH
    );
    assert_eq!(pane_focus_opacity(false, true, 1.0), 0.4);
}

#[test]
fn boolean_preferences_keep_booleans_only() {
    assert!(!PrefsState::default().webgl_renderer);
    assert_eq!(
        migrate(json!({ "webglRenderer": true }), 12),
        json!({ "webglRenderer": true })
    );
    assert_eq!(
        migrate(json!({ "webglRenderer": "off", "monoFontSize": 16 }), 12),
        json!({ "monoFontSize": 16 })
    );
    assert!(!PrefsState::default().ssh_session_summary);
    assert_eq!(
        migrate(json!({ "sshSessionSummary": true }), 16),
        json!({ "sshSessionSummary": true })
    );
    assert_eq!(
        migrate(
            json!({ "sshSessionSummary": "yes", "monoFontSize": 16 }),
            16
        ),
        json!({ "monoFontSize": 16 })
    );
}

#[test]
fn new_host_and_password_prompt_defaults() {
    let initial = PrefsState::default();
    assert!(!initial.remember_passwords_by_default);
    assert_eq!(initial.new_ssh_host_storage, NewSshHostStorage::Openssh);
    let valid = json!({ "rememberPasswordsByDefault": true, "newSshHostStorage": "muxus" });
    assert_eq!(migrate(valid.clone(), 16), valid);
    assert_eq!(
        migrate(
            json!({ "rememberPasswordsByDefault": "yes", "newSshHostStorage": "database", "monoFontSize": 16 }),
            16
        ),
        json!({ "monoFontSize": 16 })
    );
}

#[test]
fn tab_number_visibility_preference() {
    assert_eq!(
        PrefsState::default().tab_number_visibility,
        TabNumberVisibility::Shortcut
    );
    assert_eq!(
        migrate(json!({ "tabNumberVisibility": "always" }), 9),
        json!({ "tabNumberVisibility": "always" })
    );
    assert_eq!(
        migrate(json!({ "tabNumberVisibility": "sometimes" }), 9),
        json!({})
    );
    assert_eq!(
        migrate(json!({ "tabNumberVisibility": "never" }), 9),
        json!({})
    );
}

#[test]
fn terminal_color_scheme_preferences() {
    let initial = PrefsState::default();
    assert_eq!(initial.light_terminal_scheme, "vscode-light");
    assert_eq!(initial.dark_terminal_scheme, "vscode-dark");
    let mut prefs = PrefsState {
        light_terminal_scheme: "paper".into(),
        dark_terminal_scheme: "dracula".into(),
        ..PrefsState::default()
    };
    assert_eq!(
        prefs.terminal_scheme_id_for_mode(EffectiveThemeMode::Light),
        "paper"
    );
    assert_eq!(
        prefs.terminal_scheme_id_for_mode(EffectiveThemeMode::Dark),
        "dracula"
    );
    assert_eq!(
        migrate(json!({ "terminalScheme": "nord" }), 8),
        json!({ "lightTerminalScheme": "nord", "darkTerminalScheme": "nord" })
    );
    assert_eq!(
        migrate(
            json!({ "terminalScheme": "nord", "lightTerminalScheme": "paper", "darkTerminalScheme": "dracula" }),
            8
        ),
        json!({ "lightTerminalScheme": "paper", "darkTerminalScheme": "dracula" })
    );

    // Custom schemes: kept when valid, a malformed list dropped.
    assert!(PrefsState::default().custom_terminal_schemes.is_empty());
    let custom = custom_scheme("scheme-custom");
    assert_eq!(
        migrate(json!({ "customTerminalSchemes": [custom] }), 16),
        json!({ "customTerminalSchemes": [custom] })
    );
    let mut broken = custom.clone();
    broken["colors"] = json!({ "background": "#000000" });
    assert_eq!(
        migrate(json!({ "customTerminalSchemes": [broken] }), 16),
        json!({})
    );

    // Deleting a custom scheme returns a theme that used it to its default.
    let other = custom_scheme("scheme-other");
    let schemes: Vec<_> = serde_json::from_value(json!([custom, other])).unwrap();
    prefs.custom_terminal_schemes = schemes;
    prefs.light_terminal_scheme = "scheme-other".into();
    prefs.dark_terminal_scheme = "scheme-custom".into();
    prefs.without_custom_terminal_scheme("scheme-custom");
    assert_eq!(prefs.custom_terminal_schemes.len(), 1);
    assert_eq!(prefs.dark_terminal_scheme, "vscode-dark");
    assert_eq!(prefs.light_terminal_scheme, "scheme-other");
    prefs.dark_terminal_scheme = "dracula".into();
    prefs.without_custom_terminal_scheme("scheme-other");
    assert!(prefs.custom_terminal_schemes.is_empty());
    assert_eq!(prefs.light_terminal_scheme, "vscode-light");
    assert_eq!(prefs.dark_terminal_scheme, "dracula");
}

#[test]
fn terminal_font_stack_includes_the_bundled_fallbacks() {
    assert!(MONO_FONT_FALLBACK.contains("\"Pure Nerd Font\""));
    assert_eq!(
        terminal_font_stack("Iosevka"),
        format!("Iosevka, {MONO_FONT_FALLBACK}")
    );
    assert_eq!(terminal_font_stack("JetBrains Mono"), MONO_FONT_FALLBACK);
    assert_eq!(
        terminal_font_stack("\"JetBrains Mono\""),
        MONO_FONT_FALLBACK
    );
    assert_eq!(terminal_font_stack("monospace"), MONO_FONT_FALLBACK);
    assert_eq!(terminal_font_stack("  "), MONO_FONT_FALLBACK);
    assert_eq!(
        terminal_font_stack("Cascadia Code"),
        format!("\"Cascadia Code\", {MONO_FONT_FALLBACK}")
    );
}

#[test]
fn folder_preferences() {
    let persisted = json!({
        "sidebarCollapsedFolders": ["folder:prod"],
        "sidebarEmptyFolders": ["Prod/EU"],
        "sidebarFolderStyles": { "folder:prod": { "color": "#ef5350", "icon": "cloud" } },
    });
    assert_eq!(migrate(persisted.clone(), 4), persisted);
    let wrong = json!({
        "monoFontSize": 16,
        "sidebarCollapsedFolders": "folder:prod",
        "sidebarEmptyFolders": [1, 2],
        "sidebarFolderStyles": { "folder:prod": "red" },
    });
    assert_eq!(migrate(wrong.clone(), 4), json!({ "monoFontSize": 16 }));
    // The stored snapshot itself is left untouched.
    assert_eq!(wrong["sidebarCollapsedFolders"], json!("folder:prod"));
    let unknown_key = migrate(
        json!({ "sidebarFolderStyles": { "folder:prod": { "colour": "#ef5350" } } }),
        4,
    );
    assert!(unknown_key.get("sidebarFolderStyles").is_none());
    let order = json!({ "sidebarFolderOrder": { "root": ["folder:prod", "folder:lab"] } });
    assert_eq!(migrate(order.clone(), 4), order);
    assert!(
        migrate(
            json!({ "sidebarFolderOrder": { "root": "folder:prod" } }),
            4
        )
        .get("sidebarFolderOrder")
        .is_none()
    );
}

#[test]
fn sidebar_width_migration() {
    assert_eq!(
        migrate(json!({ "sidebarWidth": 248 }), 5),
        json!({ "sidebarWidth": DEFAULT_SIDEBAR_WIDTH as i64 })
    );
    assert_eq!(
        migrate(json!({ "sidebarWidth": 320 }), 5),
        json!({ "sidebarWidth": 320 })
    );
    assert_eq!(
        migrate(json!({ "sidebarWidth": 248 }), 6),
        json!({ "sidebarWidth": 248 })
    );
}

#[test]
fn retired_preferences() {
    let persisted = json!({ "termName": "xterm-kitty", "monoFontSize": 16 });
    assert_eq!(migrate(persisted.clone(), 1), json!({ "monoFontSize": 16 }));
    assert_eq!(persisted["termName"], json!("xterm-kitty"));
    assert_eq!(
        migrate(json!({ "termName": "screen-256color" }), 2),
        json!({})
    );
    assert_eq!(
        migrate(
            json!({ "terminalScheme": "muxus", "termName": "xterm-kitty" }),
            0
        ),
        json!({ "lightTerminalScheme": "vscode-dark", "darkTerminalScheme": "vscode-dark" })
    );
}

#[test]
fn local_shell_profile_preferences() {
    assert!(is_local_shell_profile_array(&json!([ubuntu_profile()])));
    assert!(!is_local_shell_profile_array(&json!([
        ubuntu_profile(),
        ubuntu_profile()
    ])));
    let mut flat_args = ubuntu_profile();
    flat_args["args"] = json!("-d Ubuntu");
    assert_eq!(
        migrate(
            json!({ "monoFontSize": 16, "localShellProfiles": [flat_args] }),
            6
        ),
        json!({ "monoFontSize": 16 })
    );
    assert!(PrefsState::default().show_wsl_distributions);
    assert_eq!(
        migrate(json!({ "showWslDistributions": false }), 16),
        json!({ "showWslDistributions": false })
    );
    assert_eq!(
        migrate(
            json!({ "showWslDistributions": "no", "monoFontSize": 16 }),
            16
        ),
        json!({ "monoFontSize": 16 })
    );
    // A default that names a profile that is gone falls back to automatic.
    assert_eq!(
        migrate(
            json!({ "localShellProfiles": [ubuntu_profile()], "defaultLocalShellProfileId": "gone" }),
            16
        ),
        json!({ "localShellProfiles": [ubuntu_profile()], "defaultLocalShellProfileId": "" })
    );
}

#[test]
fn keyword_highlighting_profile_preferences() {
    let profile = json!({
        "id": "nokia-sros",
        "name": "Nokia SR OS",
        "rules": [{
            "id": "alarm", "keyword": "MAJOR", "foreground": "#ffffff", "background": "#b91c1c",
            "caseSensitive": true, "wholeWord": true
        }],
    });
    // Also covers a built-in profile the user deleted after v15 seeded it.
    assert_eq!(
        migrate(json!({ "keywordHighlightProfiles": [profile] }), 15),
        json!({ "keywordHighlightProfiles": [profile] })
    );
    assert_eq!(
        PrefsState::default().keyword_highlight_profiles,
        builtin_highlight_profiles()
    );

    let mut seeded = builtin_highlight_profiles_json()[0].clone();
    for rule in seeded["rules"].as_array_mut().unwrap() {
        rule.as_object_mut().unwrap().shift_remove("name");
    }
    let migrated = migrate(json!({ "keywordHighlightProfiles": [seeded] }), 15);
    let profiles: Vec<muxus_core::api::KeywordHighlightProfile> =
        serde_json::from_value(migrated["keywordHighlightProfiles"].clone()).unwrap();
    assert_eq!(profiles, vec![nokia_sros_highlight_profile().clone()]);

    let mut edited = serde_json::to_value(nokia_sros_highlight_profile()).unwrap();
    edited["name"] = json!("Edge SR OS");
    edited["rules"] = json!([]);
    let migrated = migrate(json!({ "keywordHighlightProfiles": [profile, edited] }), 14);
    let ids: Vec<&str> = migrated["keywordHighlightProfiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["nokia-sros", "muxus-nokia-sros", "muxus-nokia-srlinux"]
    );
    assert_eq!(migrated["keywordHighlightProfiles"][1], edited);
    assert_eq!(
        serde_json::from_value::<muxus_core::api::KeywordHighlightProfile>(
            migrated["keywordHighlightProfiles"][2].clone()
        )
        .unwrap(),
        *nokia_srlinux_highlight_profile()
    );

    let mut bad = profile.clone();
    bad["rules"][0]["foreground"] = json!("red");
    assert_eq!(
        migrate(
            json!({ "monoFontSize": 16, "keywordHighlightProfiles": [bad] }),
            11
        ),
        json!({ "monoFontSize": 16 })
    );
}

#[test]
fn merging_keeps_live_values_that_did_not_change() {
    let current = muxus_store::prefs::default_state_map();
    let mut saved = current.clone();
    saved.insert("themeMode".into(), json!("dark"));
    saved.insert("commandButtons".into(), json!([]));
    saved.insert("set".into(), json!("not a function"));
    saved.insert("unknown".into(), json!(1));
    let merged = merge_persisted_prefs(&Value::Object(saved), &current);
    assert_eq!(merged["themeMode"], json!("dark"));
    assert_eq!(
        merged["keywordHighlightProfiles"],
        current["keywordHighlightProfiles"]
    );
    assert!(!merged.contains_key("set"));
    assert_eq!(
        merged.keys().next_back().map(String::as_str),
        Some("unknown")
    );
    // Anything but an object leaves the live state alone.
    assert_eq!(merge_persisted_prefs(&Value::Null, &current), current);
    assert_eq!(merge_persisted_prefs(&json!("text"), &current), current);
}

#[test]
fn migration_of_non_objects_returns_them_unchanged() {
    assert_eq!(migrate(Value::Null, 3), Value::Null);
    assert_eq!(migrate(json!(5), 3), json!(5));
    // An array spreads into an object of its indices, as `{ ...array }` does.
    assert_eq!(migrate(json!(["a"]), 3), json!({ "0": "a" }));
}
