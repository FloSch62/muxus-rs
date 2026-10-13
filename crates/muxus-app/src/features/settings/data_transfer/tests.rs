//! Port of `tests/unit/client/data-transfer.test.ts`. The TypeScript mocks
//! `fetch`; these run against a sandbox backend (scratch data folder and
//! home) and check what ends up stored, or the request a restore computes.

use std::path::PathBuf;

use futures_lite::future::block_on;
use serde_json::{Map, Value, json};

use muxus_core::api::{
    FolderSettingsRecord, HostUpsertRequest, LoginSequence, OpenSshMetadataPatch,
    PasswordVaultUnlockPolicy, SavedHostProfile, SavedHostProfileInput, SessionLogFileSettings,
    SessionLoggingPolicyInput, SshHostEntry,
};
use muxus_core::js::Patch;
use muxus_session::routes::folders::FolderSettingsUpsert;
use muxus_session::{Backend, BackendOptions, RuntimeChoice};
use muxus_store::prefs::PrefsState;
use muxus_store::prefs::command_buttons::CommandButton;
use muxus_store::prefs::custom_schemes::CustomTerminalScheme;

use super::section::{
    BACKUP_SAVED, TRANSFER_FILE_TOO_LARGE, friendly_date_at, proxy_command_warning,
};
use super::*;

struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    backend: Backend,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut options = BackendOptions::sandbox(dir.path().join("data"), &home);
        options.runtime = RuntimeChoice::Own { worker_threads: 1 };
        options.autostart_tunnels = false;
        let backend = Backend::start_blocking(options).unwrap();
        Self {
            _dir: dir,
            home,
            backend,
        }
    }

    fn ssh_config_path(&self) -> PathBuf {
        self.home.join(".ssh").join("config")
    }

    fn write_ssh_config(&self, text: &str) {
        std::fs::create_dir_all(self.home.join(".ssh")).unwrap();
        std::fs::write(self.ssh_config_path(), text).unwrap();
    }

    fn ssh_config(&self) -> String {
        std::fs::read_to_string(self.ssh_config_path()).unwrap_or_default()
    }

    /// The TypeScript suite's `beforeEach` prefs, then `change`.
    fn set_prefs(&self, change: impl FnOnce(&mut PrefsState)) {
        let mut prefs = block_on(self.backend.prefs()).unwrap();
        prefs.keyword_highlight_profiles = Vec::new();
        change(&mut prefs);
        block_on(self.backend.set_prefs(prefs)).unwrap();
    }

    fn backup(&self) -> TransferDocument {
        block_on(create_backup_document(&self.backend, None)).unwrap()
    }

    fn save_saved_host(&self, id: &str, name: &str, profile: Value) {
        block_on(self.backend.save_saved_host(SavedHostProfileInput {
            id: Some(id.to_string()),
            name: name.to_string(),
            profile: serde_json::from_value(profile).unwrap(),
        }))
        .unwrap();
    }

    fn patch_saved_metadata(&self, id: &str, patch: Value) {
        block_on(
            self.backend
                .patch_saved_host_metadata(id, serde_json::from_value(patch).unwrap()),
        )
        .unwrap();
    }

    fn put_folder(&self, path: &str, auth: Value, sequence: Option<Value>) {
        block_on(self.backend.put_folder_settings(FolderSettingsUpsert {
            path: path.to_string(),
            auth: serde_json::from_value(auth).unwrap(),
            login_sequence: sequence.map_or(Patch::Keep, |sequence| {
                Patch::Set(serde_json::from_value(sequence).unwrap())
            }),
        }))
        .unwrap();
    }

    fn saved_hosts(&self) -> Vec<SavedHostProfile> {
        block_on(self.backend.saved_hosts()).unwrap().profiles
    }

    fn folders(&self) -> Vec<FolderSettingsRecord> {
        block_on(self.backend.folder_settings()).unwrap().folders
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        block_on(self.backend.shutdown());
    }
}

const GIB: u64 = 1024 * 1024 * 1024;

fn history_settings() -> Value {
    json!({ "maxTotalBytes": 5 * GIB, "minFreeBytes": 2 * GIB, "minFreePercent": 5 })
}

fn connections() -> Map<String, Value> {
    object(json!({
        "sshHosts": [{
            "alias": "production",
            "aliases": ["production"],
            "options": { "hostname": "prod.example.com", "user": "deploy" },
            "metadata": { "group": "Work" },
        }],
        "savedHosts": [],
        "hostOrder": [{ "kind": "ssh", "alias": "production" }],
    }))
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

/// A version-2 backup of `connections()` plus `data`.
fn backup_text(data: Value) -> String {
    let mut full = connections();
    full.insert("preferences".into(), json!({}));
    full.insert("tunnels".into(), json!([]));
    full.insert("loggingPolicies".into(), json!([]));
    full.insert("historySettings".into(), history_settings());
    full.extend(object(data));
    json!({
        "format": BACKUP_FORMAT,
        "version": TRANSFER_VERSION,
        "createdAt": "2026-10-04T12:00:00.000Z",
        "data": full,
    })
    .to_string()
}

fn parse_error(text: &str) -> String {
    parse_transfer_document(text).unwrap_err().message
}

fn sanitize(input: Value) -> Map<String, Value> {
    sanitize_preferences(&object(input))
}

// ---------------------------------------------------------------------------
// Muxus transfer file parsing
// ---------------------------------------------------------------------------

#[test]
fn accepts_a_versioned_full_backup() {
    let mut data = connections();
    data.insert("preferences".into(), json!({}));
    data.insert("tunnels".into(), json!([]));
    data.insert("loggingPolicies".into(), json!([]));
    data.insert("historySettings".into(), history_settings());
    let document = json!({
        "format": BACKUP_FORMAT,
        "version": TRANSFER_VERSION,
        "createdAt": "2026-07-24T12:00:00.000Z",
        "appVersion": "0.1.0",
        "data": data,
    });
    let parsed = parse_transfer_document(&document.to_string()).unwrap();
    assert_eq!(parsed.as_value(), &document);
    assert_eq!(parsed.version(), 2);
    assert_eq!(parsed.app_version(), Some("0.1.0"));
    assert_eq!(parsed.created_at(), "2026-07-24T12:00:00.000Z");
}

#[test]
fn keeps_reading_legacy_version_1_backups() {
    let document = json!({
        "format": BACKUP_FORMAT,
        "version": 1,
        "createdAt": "2026-07-24T12:00:00.000Z",
        "data": {
            "sshHosts": [],
            "savedHosts": [{
                "id": "legacy-telnet",
                "name": "Console",
                "profile": { "kind": "telnet", "host": "console.example.test", "port": 23 },
                "metadata": {},
            }],
            "hostOrder": [{ "kind": "profile", "id": "legacy-telnet" }],
            "preferences": {},
            "tunnels": [],
            "loggingPolicies": [],
            "historySettings": history_settings(),
        },
    });
    let parsed = parse_transfer_document(&document.to_string()).unwrap();
    assert_eq!(parsed.as_value(), &document);
    assert_eq!(parsed.version(), 1);
}

#[test]
fn requires_version_2_for_saved_ssh_profiles() {
    let text = json!({
        "format": BACKUP_FORMAT,
        "version": 1,
        "createdAt": "2026-07-24T12:00:00.000Z",
        "data": {
            "sshHosts": [],
            "savedHosts": [{
                "id": "saved-ssh",
                "name": "Router",
                "profile": { "kind": "ssh", "target": "router.example.test", "useConfig": false },
                "metadata": {},
            }],
            "hostOrder": [{ "kind": "profile", "id": "saved-ssh" }],
            "preferences": {},
            "tunnels": [],
            "loggingPolicies": [],
            "historySettings": history_settings(),
        },
    })
    .to_string();
    assert_eq!(
        parse_error(&text),
        "The connection data in this file is incomplete or too large."
    );
}

#[test]
fn rejects_invalid_json_with_a_useful_error() {
    assert_eq!(parse_error("{not json"), "This file is not valid JSON.");
}

#[test]
fn rejects_future_versions_before_inspecting_their_data() {
    let text = json!({
        "format": BACKUP_FORMAT,
        "version": TRANSFER_VERSION + 1,
        "createdAt": "2026-07-24T12:00:00.000Z",
        "data": {},
    })
    .to_string();
    assert_eq!(
        parse_error(&text),
        format!(
            "Muxus transfer version {} is not supported.",
            TRANSFER_VERSION + 1
        )
    );
}

#[test]
fn rejects_malformed_connection_entries_before_restore_can_write() {
    let text = json!({
        "format": BACKUP_FORMAT,
        "version": TRANSFER_VERSION,
        "createdAt": "2026-07-24T12:00:00.000Z",
        "data": {
            "sshHosts": [{ "alias": "broken", "aliases": [], "options": {} }],
            "savedHosts": [],
            "hostOrder": [],
            "preferences": {},
            "tunnels": [],
            "loggingPolicies": [],
            "historySettings": history_settings(),
        },
    })
    .to_string();
    assert_eq!(
        parse_error(&text),
        "The connection data in this file is incomplete or too large."
    );
}

#[test]
fn rejects_files_that_are_not_muxus_backups() {
    assert_eq!(parse_error("[]"), "This is not a Muxus transfer file.");
    assert_eq!(
        parse_error(r#"{"format":"muxus-backup"}"#),
        "This file is missing a supported transfer version."
    );
    assert_eq!(
        parse_error(r#"{"format":"other","version":2}"#),
        "This is not a Muxus backup file."
    );
    assert_eq!(
        parse_error(r#"{"format":"muxus-backup","version":2,"createdAt":"never","data":{}}"#),
        "The Muxus transfer file is incomplete."
    );
    // Fractional versions print the way JavaScript prints them.
    assert_eq!(
        parse_error(r#"{"format":"muxus-backup","version":2.5}"#),
        "Muxus transfer version 2.5 is not supported."
    );
}

#[test]
fn rejects_duplicate_or_unknown_items() {
    let unknown_order =
        backup_text(json!({ "hostOrder": [{ "kind": "ssh", "alias": "elsewhere" }] }));
    assert_eq!(
        parse_error(&unknown_order),
        "The connection data contains duplicate or unknown items."
    );
    let tunnel = json!({ "id": "t", "target": "db", "type": "local", "bindPort": 5432 });
    let duplicate_tunnels = backup_text(json!({ "tunnels": [tunnel.clone(), tunnel] }));
    assert_eq!(
        parse_error(&duplicate_tunnels),
        "The backup contains duplicate saved items."
    );
}

// ---------------------------------------------------------------------------
// backing up session log files
// ---------------------------------------------------------------------------

#[test]
fn backup_keeps_each_policys_log_file_choice_and_the_settings_without_the_folder() {
    let sandbox = Sandbox::new();
    let backend = &sandbox.backend;
    block_on(backend.put_session_logging_policy(
        "*",
        SessionLoggingPolicyInput {
            enabled: false,
            capture_input: false,
            max_part_bytes: 1024 * 1024,
            max_parts: 2,
            log_to_file: Some(true),
        },
    ))
    .unwrap();
    let logs = sandbox.home.join("logs");
    block_on(backend.put_session_log_files(SessionLogFileSettings {
        directory: Some(logs.to_string_lossy().into_owned()),
        filename_pattern: "{host}/{date}.log".into(),
        timestamps: true,
    }))
    .unwrap();

    let document = sandbox.backup();

    assert_eq!(
        document.data()["loggingPolicies"],
        json!([{
            "profileKey": "*",
            "policy": {
                "enabled": false,
                "captureInput": false,
                "maxPartBytes": 1024 * 1024,
                "maxParts": 2,
                "logToFile": true,
            },
        }])
    );
    assert_eq!(
        document.data()["logFileSettings"],
        json!({ "filenamePattern": "{host}/{date}.log", "timestamps": true })
    );
    assert_eq!(
        parse_transfer_document(&document.to_file_text()).unwrap(),
        document
    );
}

#[test]
fn rejects_malformed_log_file_settings() {
    let text =
        backup_text(json!({ "logFileSettings": { "filenamePattern": "", "timestamps": "yes" } }));
    assert_eq!(
        parse_error(&text),
        "The backup data is incomplete or too large."
    );
}

#[test]
fn accepts_saved_tunnels_with_or_without_their_switches_and_rejects_malformed_ones() {
    let tunnel = json!({ "id": "tunnel-1", "target": "db", "type": "dynamic", "bindPort": 1080 });
    let mut switched = tunnel.clone();
    switched["id"] = json!("tunnel-2");
    switched["autoStart"] = json!(true);
    switched["autoReconnect"] = json!(true);
    let parsed =
        parse_transfer_document(&backup_text(json!({ "tunnels": [tunnel, switched] }))).unwrap();
    assert_eq!(parsed.tunnels(), &[tunnel.clone(), switched][..]);
    for (key, value) in [("autoStart", "yes"), ("autoReconnect", "yes")] {
        let mut malformed = tunnel.clone();
        malformed[key] = json!(value);
        assert_eq!(
            parse_error(&backup_text(json!({ "tunnels": [malformed] }))),
            "The backup data is incomplete or too large."
        );
    }
}

#[test]
fn restores_the_log_file_settings_into_this_machines_log_folder() {
    let sandbox = Sandbox::new();
    let backend = &sandbox.backend;
    let logs = sandbox.home.join("srv-logs").to_string_lossy().into_owned();
    block_on(backend.put_session_log_files(SessionLogFileSettings {
        directory: Some(logs.clone()),
        filename_pattern: "{host}.log".into(),
        timestamps: false,
    }))
    .unwrap();
    let document = parse_transfer_document(&backup_text(json!({
        "logFileSettings": { "filenamePattern": "{kind}/{host}.log", "timestamps": true },
    })))
    .unwrap();

    // The write the TypeScript sends: the file's settings, this machine's folder.
    let input = log_file_settings_input(
        document.data()["logFileSettings"].as_object().unwrap(),
        Some(&logs),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&input).unwrap(),
        json!({ "filenamePattern": "{kind}/{host}.log", "timestamps": true, "directory": logs })
    );

    let result = block_on(restore_transfer_document(
        backend,
        &document,
        RestoreSelection {
            preferences: false,
            connections: false,
            tunnels: false,
            logging: true,
        },
        TransferConflictStrategy::Replace,
    ))
    .unwrap();
    assert_eq!(
        result,
        RestoreResult {
            added: 0,
            updated: 2,
            skipped: 0
        }
    );
    let stored = block_on(backend.session_log_files()).unwrap().settings;
    assert_eq!(
        stored,
        SessionLogFileSettings {
            directory: Some(logs),
            filename_pattern: "{kind}/{host}.log".into(),
            timestamps: true,
        }
    );
    let storage = block_on(backend.session_history_storage())
        .unwrap()
        .settings;
    assert_eq!(storage.max_total_bytes, 5 * GIB);
    assert_eq!(storage.storage_location, None);
}

#[test]
fn restores_logging_policies() {
    let sandbox = Sandbox::new();
    let document = parse_transfer_document(&backup_text(json!({
        "loggingPolicies": [{
            "profileKey": "local",
            "policy": { "enabled": true, "captureInput": false, "maxPartBytes": 1048576, "maxParts": 3 },
        }],
    })))
    .unwrap();
    let selection = RestoreSelection {
        preferences: false,
        connections: false,
        tunnels: false,
        logging: true,
    };
    let result = block_on(restore_transfer_document(
        &sandbox.backend,
        &document,
        selection,
        TransferConflictStrategy::Keep,
    ))
    .unwrap();
    assert_eq!(result.updated, 2);
    let policy = block_on(sandbox.backend.session_logging_policy("local")).unwrap();
    assert!(policy.overridden && policy.enabled && !policy.log_to_file);
    assert_eq!(policy.max_parts, 3);
}

// ---------------------------------------------------------------------------
// backing up preferences
// ---------------------------------------------------------------------------

#[test]
fn keeps_a_secret_command_button_as_a_reference_to_the_vault() {
    let sandbox = Sandbox::new();
    let enable = CommandButton {
        id: "enable".into(),
        label: "Enable".into(),
        command: String::new(),
        send_enter: true,
        group_id: None,
        color: None,
        secret_id: Some("vault-secret-1".into()),
    };
    sandbox.set_prefs(|prefs| prefs.command_buttons = vec![enable.clone()]);

    let document = sandbox.backup();

    let expected = json!([{
        "id": "enable",
        "label": "Enable",
        "command": "",
        "sendEnter": true,
        "secretId": "vault-secret-1",
    }]);
    assert_eq!(document.preferences()["commandButtons"], expected);
    assert_eq!(
        sanitize(json!({ "commandButtons": expected.clone() }))["commandButtons"],
        expected
    );
}

#[test]
fn includes_display_and_update_notification_choices() {
    let sandbox = Sandbox::new();
    sandbox.set_prefs(|prefs| {
        *prefs = PrefsState::from_json_map(&object(json!({
            "notifyOnNewVersion": false,
            "showCommandBar": false,
            "commandBarPosition": "bottom",
            "sidebarPosition": "right",
            "fileBrowserPosition": "sidebar",
            "sidebarOpenGesture": "double-click",
            "showStatusBar": true,
            "statusBarItems": ["memory", "network"],
            "backgroundColor": "#102030",
            "lightTerminalScheme": "paper",
            "darkTerminalScheme": "dracula",
            "activePaneBorder": false,
            "dimInactivePanes": true,
            "inactivePaneDimStrength": 0.35,
            "terminalFileLinkActivation": "ctrl",
        })));
    });

    let document = sandbox.backup();
    let prefs = document.preferences();

    assert_eq!(prefs["notifyOnNewVersion"], json!(false));
    assert_eq!(prefs["backgroundColor"], json!("#102030"));
    assert_eq!(prefs["showCommandBar"], json!(false));
    assert_eq!(prefs["commandBarPosition"], json!("bottom"));
    assert_eq!(prefs["sidebarPosition"], json!("right"));
    assert_eq!(prefs["fileBrowserPosition"], json!("sidebar"));
    assert_eq!(prefs["sidebarOpenGesture"], json!("double-click"));
    assert_eq!(prefs["showStatusBar"], json!(true));
    assert_eq!(prefs["statusBarItems"], json!(["memory", "network"]));
    assert_eq!(prefs["lightTerminalScheme"], json!("paper"));
    assert_eq!(prefs["darkTerminalScheme"], json!("dracula"));
    assert_eq!(prefs["activePaneBorder"], json!(false));
    assert_eq!(prefs["dimInactivePanes"], json!(true));
    assert_eq!(prefs["inactivePaneDimStrength"], json!(0.35));
    assert_eq!(prefs["terminalFileLinkActivation"], json!("ctrl"));
}

/// A custom copy of the Nord scheme, as `customSchemeCopyOf` makes one.
fn nord_copy() -> Value {
    json!({
        "id": "scheme-nord-copy",
        "name": "Nord copy",
        "colors": {
            "background": "#2e3440", "foreground": "#d8dee9", "cursor": "#d8dee9",
            "selectionBackground": "#434c5e", "black": "#3b4252", "red": "#bf616a",
            "green": "#a3be8c", "yellow": "#ebcb8b", "blue": "#81a1c1", "magenta": "#b48ead",
            "cyan": "#88c0d0", "white": "#e5e9f0", "brightBlack": "#4c566a",
            "brightRed": "#bf616a", "brightGreen": "#a3be8c", "brightYellow": "#ebcb8b",
            "brightBlue": "#81a1c1", "brightMagenta": "#b48ead", "brightCyan": "#8fbcbb",
            "brightWhite": "#eceff4",
        },
    })
}

#[test]
fn includes_custom_terminal_color_schemes() {
    let sandbox = Sandbox::new();
    let custom: CustomTerminalScheme = serde_json::from_value(nord_copy()).unwrap();
    sandbox.set_prefs(|prefs| {
        prefs.custom_terminal_schemes = vec![custom.clone()];
        prefs.dark_terminal_scheme = custom.id.clone();
    });

    let document = sandbox.backup();

    assert_eq!(
        document.preferences()["customTerminalSchemes"],
        json!([nord_copy()])
    );
    assert_eq!(
        document.preferences()["darkTerminalScheme"],
        json!("scheme-nord-copy")
    );
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

#[test]
fn includes_saved_local_shell_profiles_and_their_default_selection() {
    let sandbox = Sandbox::new();
    sandbox.set_prefs(|prefs| {
        prefs.local_shell_profiles = vec![serde_json::from_value(ubuntu_profile()).unwrap()];
        prefs.default_local_shell_profile_id = "ubuntu".into();
        prefs.show_wsl_distributions = false;
    });

    let document = sandbox.backup();

    assert_eq!(
        document.preferences()["localShellProfiles"],
        json!([ubuntu_profile()])
    );
    assert_eq!(
        document.preferences()["defaultLocalShellProfileId"],
        json!("ubuntu")
    );
    assert_eq!(document.preferences()["showWslDistributions"], json!(false));
}

fn nokia_profile() -> Value {
    json!({
        "id": "nokia-sros",
        "name": "Nokia SR OS",
        "rules": [{
            "id": "alarm",
            "keyword": "MAJOR",
            "foreground": "#ffffff",
            "caseSensitive": true,
            "wholeWord": true,
        }],
    })
}

#[test]
fn includes_reusable_keyword_highlighting_profiles() {
    let sandbox = Sandbox::new();
    sandbox.set_prefs(|prefs| {
        prefs.keyword_highlight_profiles = vec![serde_json::from_value(nokia_profile()).unwrap()];
    });

    let document = sandbox.backup();

    assert_eq!(
        document.preferences()["keywordHighlightProfiles"],
        json!([nokia_profile()])
    );
}

#[test]
fn backup_carries_every_preference_key_in_order() {
    let sandbox = Sandbox::new();
    let document = sandbox.backup();
    let keys: Vec<&str> = document.preferences().keys().map(String::as_str).collect();
    assert_eq!(keys, PREFERENCE_KEYS);
    // The document is laid out as the TypeScript writes it.
    let top: Vec<&str> = document
        .as_value()
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(top, ["format", "version", "createdAt", "data"]);
    let data: Vec<&str> = document.data().keys().map(String::as_str).collect();
    assert_eq!(
        data,
        [
            "sshHosts",
            "savedHosts",
            "hostOrder",
            "preferences",
            "tunnels",
            "loggingPolicies",
            "historySettings",
            "logFileSettings",
            "folderSettings",
        ]
    );
    let text = document.to_file_text();
    assert!(text.starts_with("{\n  \"format\": \"muxus-backup\",\n  \"version\": 2,\n"));
    assert!(text.ends_with("}\n"));
    // The history location and the log folder are paths on this machine.
    assert!(
        document.data()["historySettings"]
            .get("storageLocation")
            .is_none()
    );
    assert!(
        document.data()["logFileSettings"]
            .get("directory")
            .is_none()
    );
    let with_version = block_on(create_backup_document(&sandbox.backend, Some("0.11.1"))).unwrap();
    assert_eq!(with_version.app_version(), Some("0.11.1"));
}

// ---------------------------------------------------------------------------
// restoring preferences
// ---------------------------------------------------------------------------

#[test]
fn restores_each_terminal_file_link_activation() {
    for value in ["direct", "alt", "ctrl", "meta"] {
        assert_eq!(
            sanitize(json!({ "terminalFileLinkActivation": value }))["terminalFileLinkActivation"],
            json!(value)
        );
    }
}

#[test]
fn drops_malformed_activation_settings() {
    assert!(
        !sanitize(json!({ "terminalFileLinkActivation": "double-click" }))
            .contains_key("terminalFileLinkActivation")
    );
}

#[test]
fn restores_paste_delays_in_range_and_drops_the_rest() {
    let restored = sanitize(json!({ "pasteLineDelayMs": 300, "pasteCharDelayMs": 0 }));
    assert_eq!(restored["pasteLineDelayMs"], json!(300));
    assert_eq!(restored["pasteCharDelayMs"], json!(0));
    let restored = sanitize(json!({ "pasteLineDelayMs": 60_000, "pasteCharDelayMs": "slow" }));
    assert!(!restored.contains_key("pasteLineDelayMs"));
    assert!(!restored.contains_key("pasteCharDelayMs"));
}

#[test]
fn restores_separate_light_and_dark_scheme_selections() {
    let restored =
        sanitize(json!({ "lightTerminalScheme": "paper", "darkTerminalScheme": "dracula" }));
    assert_eq!(restored["lightTerminalScheme"], json!("paper"));
    assert_eq!(restored["darkTerminalScheme"], json!("dracula"));
}

#[test]
fn restores_a_legacy_single_scheme_into_both_appearances() {
    let restored = sanitize(json!({ "terminalScheme": "nord" }));
    assert_eq!(restored["lightTerminalScheme"], json!("nord"));
    assert_eq!(restored["darkTerminalScheme"], json!("nord"));
}

#[test]
fn drops_malformed_scheme_selections() {
    let restored = sanitize(json!({
        "lightTerminalScheme": 42,
        "darkTerminalScheme": "x".repeat(101),
    }));
    assert!(!restored.contains_key("lightTerminalScheme"));
    assert!(!restored.contains_key("darkTerminalScheme"));
}

#[test]
fn restores_custom_schemes_and_drops_a_malformed_list() {
    let restored = sanitize(json!({
        "customTerminalSchemes": [nord_copy()],
        "darkTerminalScheme": "scheme-nord-copy",
    }));
    assert_eq!(restored["customTerminalSchemes"], json!([nord_copy()]));
    assert_eq!(restored["darkTerminalScheme"], json!("scheme-nord-copy"));
    let mut unnamed = nord_copy();
    unnamed["name"] = json!("");
    assert!(
        !sanitize(json!({ "customTerminalSchemes": [unnamed] }))
            .contains_key("customTerminalSchemes")
    );
}

#[test]
fn restores_the_pane_focus_indicators_independently() {
    let restored = sanitize(json!({
        "activePaneBorder": false,
        "dimInactivePanes": true,
        "inactivePaneDimStrength": 0.35,
    }));
    assert_eq!(restored["activePaneBorder"], json!(false));
    assert_eq!(restored["dimInactivePanes"], json!(true));
    assert_eq!(restored["inactivePaneDimStrength"], json!(0.35));
}

#[test]
fn drops_malformed_or_unreadable_dimming_choices() {
    let restored = sanitize(json!({
        "activePaneBorder": "yes",
        "dimInactivePanes": "yes",
        "inactivePaneDimStrength": 0.9,
    }));
    assert!(!restored.contains_key("activePaneBorder"));
    assert!(!restored.contains_key("dimInactivePanes"));
    assert!(!restored.contains_key("inactivePaneDimStrength"));
}

#[test]
fn restores_bounded_local_shell_profiles_and_their_default() {
    let restored = sanitize(json!({
        "localShellProfiles": [ubuntu_profile()],
        "defaultLocalShellProfileId": "ubuntu",
    }));
    assert_eq!(restored["localShellProfiles"], json!([ubuntu_profile()]));
    assert_eq!(restored["defaultLocalShellProfileId"], json!("ubuntu"));
    // A default without its profile in the same backup is left alone.
    assert!(
        !sanitize(json!({ "defaultLocalShellProfileId": "ubuntu" }))
            .contains_key("defaultLocalShellProfileId")
    );
    assert_eq!(
        sanitize(json!({ "defaultLocalShellProfileId": "" }))["defaultLocalShellProfileId"],
        json!("")
    );
}

#[test]
fn drops_malformed_local_shell_profiles() {
    let mut broken = ubuntu_profile();
    broken["args"] = json!("-d Ubuntu");
    assert!(
        !sanitize(json!({ "localShellProfiles": [broken] })).contains_key("localShellProfiles")
    );
}

#[test]
fn restores_whether_wsl_distributions_are_listed() {
    assert_eq!(
        sanitize(json!({ "showWslDistributions": false }))["showWslDistributions"],
        json!(false)
    );
    assert!(
        !sanitize(json!({ "showWslDistributions": "false" })).contains_key("showWslDistributions")
    );
}

#[test]
fn restores_the_ssh_session_summary_switch() {
    assert_eq!(
        sanitize(json!({ "sshSessionSummary": true }))["sshSessionSummary"],
        json!(true)
    );
    assert!(!sanitize(json!({ "sshSessionSummary": "on" })).contains_key("sshSessionSummary"));
}

#[test]
fn the_ssh_session_summary_is_part_of_a_backup() {
    let sandbox = Sandbox::new();
    sandbox.set_prefs(|prefs| prefs.ssh_session_summary = true);
    assert_eq!(
        sandbox.backup().preferences()["sshSessionSummary"],
        json!(true)
    );
}

#[test]
fn restores_new_host_and_password_prompt_defaults() {
    let restored =
        sanitize(json!({ "rememberPasswordsByDefault": true, "newSshHostStorage": "muxus" }));
    assert_eq!(restored["rememberPasswordsByDefault"], json!(true));
    assert_eq!(restored["newSshHostStorage"], json!("muxus"));
    let malformed =
        sanitize(json!({ "rememberPasswordsByDefault": 1, "newSshHostStorage": "database" }));
    assert!(!malformed.contains_key("rememberPasswordsByDefault"));
    assert!(!malformed.contains_key("newSshHostStorage"));
}

#[test]
fn new_host_and_password_prompt_defaults_are_part_of_a_backup() {
    let sandbox = Sandbox::new();
    sandbox.set_prefs(|prefs| {
        prefs.remember_passwords_by_default = true;
        prefs.new_ssh_host_storage = muxus_store::prefs::state::NewSshHostStorage::Muxus;
    });
    let document = sandbox.backup();
    assert_eq!(
        document.preferences()["rememberPasswordsByDefault"],
        json!(true)
    );
    assert_eq!(document.preferences()["newSshHostStorage"], json!("muxus"));
}

#[test]
fn restores_bounded_reusable_highlighting_profiles() {
    assert_eq!(
        sanitize(json!({ "keywordHighlightProfiles": [nokia_profile()] }))["keywordHighlightProfiles"],
        json!([nokia_profile()])
    );
}

#[test]
fn drops_malformed_reusable_highlighting_profiles() {
    let mut unnamed = nokia_profile();
    unnamed["name"] = json!("");
    assert!(
        !sanitize(json!({ "keywordHighlightProfiles": [unnamed] }))
            .contains_key("keywordHighlightProfiles")
    );
}

#[test]
fn restores_well_formed_folder_state() {
    let input = json!({
        "sidebarCollapsedFolders": ["folder:prod", "folder:prod/eu"],
        "sidebarEmptyFolders": ["Staging/Blue"],
        "sidebarFolderStyles": { "folder:prod": { "color": "#ef5350", "icon": "cloud" } },
    });
    assert_eq!(Value::Object(sanitize(input.clone())), input);
}

#[test]
fn drops_an_oversized_folder_list() {
    let too_many: Vec<String> = (0..501).map(|index| format!("folder:{index}")).collect();
    assert!(
        !sanitize(json!({ "sidebarEmptyFolders": too_many })).contains_key("sidebarEmptyFolders")
    );
    assert!(
        !sanitize(json!({ "sidebarCollapsedFolders": ["x".repeat(401)] }))
            .contains_key("sidebarCollapsedFolders")
    );
}

#[test]
fn drops_folder_styles_with_a_bad_colour_or_an_unknown_icon() {
    assert!(
        !sanitize(json!({ "sidebarFolderStyles": { "a": { "color": "red" } } }))
            .contains_key("sidebarFolderStyles")
    );
    assert!(
        !sanitize(json!({ "sidebarFolderStyles": { "a": { "icon": "skull" } } }))
            .contains_key("sidebarFolderStyles")
    );
    assert_eq!(
        sanitize(json!({ "sidebarFolderStyles": { "a": { "icon": "lab" } } }))["sidebarFolderStyles"],
        json!({ "a": { "icon": "lab" } })
    );
}

#[test]
fn restores_a_hex_font_color_or_the_scheme_default() {
    assert_eq!(
        sanitize(json!({ "fontColor": "#AABB00" }))["fontColor"],
        json!("#AABB00")
    );
    assert_eq!(sanitize(json!({ "fontColor": "" }))["fontColor"], json!(""));
    for malformed in [json!("red"), json!("#fff"), json!(42)] {
        assert!(!sanitize(json!({ "fontColor": malformed })).contains_key("fontColor"));
    }
}

#[test]
fn restores_a_hex_background_color_or_the_scheme_default() {
    assert_eq!(
        sanitize(json!({ "backgroundColor": "#102030" }))["backgroundColor"],
        json!("#102030")
    );
    assert_eq!(
        sanitize(json!({ "backgroundColor": "" }))["backgroundColor"],
        json!("")
    );
    for malformed in [json!("black"), json!("#000"), json!(42)] {
        assert!(!sanitize(json!({ "backgroundColor": malformed })).contains_key("backgroundColor"));
    }
}

#[test]
fn restores_only_boolean_switches() {
    for key in [
        "allowOsc52ClipboardWrite",
        "webglRenderer",
        "notifyOnNewVersion",
        "showCommandBar",
    ] {
        assert_eq!(sanitize(json!({ key: false }))[key], json!(false), "{key}");
        assert!(!sanitize(json!({ key: "yes" })).contains_key(key), "{key}");
    }
}

#[test]
fn restores_command_button_groups_and_each_button_in_its_group() {
    let bgp = json!({
        "id": "bgp",
        "label": "BGP",
        "command": "show bgp summary",
        "sendEnter": true,
        "groupId": "juniper",
        "color": "purple",
    });
    let restored = sanitize(json!({
        "commandButtonGroups": [{ "id": "juniper", "name": "Juniper" }],
        "commandButtons": [bgp.clone()],
        "selectedCommandButtonGroup": "juniper",
    }));
    assert_eq!(
        restored["commandButtonGroups"],
        json!([{ "id": "default", "name": "Default" }, { "id": "juniper", "name": "Juniper" }])
    );
    assert_eq!(restored["commandButtons"], json!([bgp.clone()]));
    assert_eq!(restored["selectedCommandButtonGroup"], json!("juniper"));

    // A backup without groups files its buttons under the default group.
    let restored = sanitize(json!({
        "commandButtons": [bgp.clone()],
        "selectedCommandButtonGroup": "juniper",
    }));
    assert_eq!(
        restored["commandButtons"],
        json!([{ "id": "bgp", "label": "BGP", "command": "show bgp summary", "sendEnter": true, "color": "purple" }])
    );
    assert!(!restored.contains_key("commandButtonGroups"));
    assert!(!restored.contains_key("selectedCommandButtonGroup"));

    // A malformed button is dropped without losing the rest.
    let mut unfiled = bgp;
    unfiled.as_object_mut().unwrap().remove("groupId");
    assert_eq!(
        sanitize(json!({ "commandButtons": [{ "id": "x", "command": 7 }, unfiled.clone()] }))["commandButtons"],
        json!([unfiled])
    );
}

#[test]
fn restores_the_layout_preferences() {
    let restored = sanitize(json!({
        "sidebarPosition": "right",
        "fileBrowserPosition": "sidebar",
        "commandBarPosition": "bottom",
    }));
    assert_eq!(restored["sidebarPosition"], json!("right"));
    assert_eq!(restored["fileBrowserPosition"], json!("sidebar"));
    assert_eq!(restored["commandBarPosition"], json!("bottom"));
    let unknown = sanitize(json!({
        "sidebarPosition": "top",
        "fileBrowserPosition": "left",
        "commandBarPosition": "left",
    }));
    assert!(unknown.is_empty());
}

#[test]
fn restores_the_status_bar_and_its_known_items() {
    let restored =
        sanitize(json!({ "showStatusBar": true, "statusBarItems": ["uptime", "users"] }));
    assert_eq!(restored["showStatusBar"], json!(true));
    assert_eq!(restored["statusBarItems"], json!(["uptime", "users"]));
    assert!(
        sanitize(json!({ "showStatusBar": "on", "statusBarItems": ["uptime", "gpu"] })).is_empty()
    );
}

#[test]
fn restores_how_sidebar_entries_open() {
    assert_eq!(
        sanitize(json!({ "sidebarOpenGesture": "double-click" }))["sidebarOpenGesture"],
        json!("double-click")
    );
    assert!(sanitize(json!({ "sidebarOpenGesture": "hover" })).is_empty());
}

#[test]
fn restores_a_well_formed_folder_order_map() {
    let input = json!({
        "sidebarFolderOrder": { "root": ["folder:prod"], "folder:prod": ["folder:prod/eu"] },
    });
    assert_eq!(Value::Object(sanitize(input.clone())), input);
    assert!(sanitize(json!({ "sidebarFolderOrder": { "root": [1, 2] } })).is_empty());
    let too_many: Map<String, Value> = (0..501)
        .map(|index| (format!("folder:{index}"), json!([])))
        .collect();
    assert!(sanitize(json!({ "sidebarFolderOrder": too_many })).is_empty());
}

#[test]
fn restoring_preferences_merges_them_into_the_stored_ones() {
    let sandbox = Sandbox::new();
    let document = parse_transfer_document(&backup_text(json!({
        "preferences": { "monoFontSize": 18, "fontColor": "red", "cursorStyle": "bar" },
    })))
    .unwrap();
    let result = block_on(restore_transfer_document(
        &sandbox.backend,
        &document,
        RestoreSelection {
            preferences: true,
            connections: false,
            tunnels: false,
            logging: false,
        },
        TransferConflictStrategy::Keep,
    ))
    .unwrap();
    assert_eq!(result.updated, 1);
    let prefs = block_on(sandbox.backend.prefs()).unwrap();
    assert_eq!(prefs.mono_font_size, 18.0);
    assert_eq!(
        prefs.cursor_style,
        muxus_store::prefs::state::CursorStyle::Bar
    );
    // The malformed color is dropped, so the current one stays.
    assert_eq!(prefs.font_color, "");
}

// ---------------------------------------------------------------------------
// folder credentials and login sequences
// ---------------------------------------------------------------------------

#[test]
fn exports_folder_auth_defaults_but_never_password_only_rows() {
    let sandbox = Sandbox::new();
    let backend = &sandbox.backend;
    block_on(backend.create_vault(
        "correct horse battery",
        Some(PasswordVaultUnlockPolicy::Never),
    ))
    .unwrap();
    sandbox.put_folder("Prod", json!({ "user": "root", "port": 2222 }), None);
    block_on(backend.put_folder_password("Prod", "folder secret", None)).unwrap();
    // A folder whose only setting is its vault password: the password
    // cannot leave the vault, so there is nothing to export.
    block_on(backend.put_folder_password("Lab", "folder secret", None)).unwrap();
    assert_eq!(sandbox.folders().len(), 2);

    let document = sandbox.backup();

    assert_eq!(
        document.data()["folderSettings"],
        json!([{ "path": "Prod", "auth": { "user": "root", "port": 2222 } }])
    );
}

fn login_sequence() -> Value {
    json!({
        "steps": [
            { "id": "w", "kind": "wait", "pattern": "Password:", "timeoutSeconds": 10 },
            { "id": "k", "kind": "secret", "secretId": "vault-secret-1", "enter": true },
        ],
    })
}

#[test]
fn exports_host_and_folder_sequences_as_references() {
    let sandbox = Sandbox::new();
    sandbox.put_folder("Network", json!({}), Some(login_sequence()));
    sandbox.put_folder("Lab", json!({ "user": "lab" }), None);
    sandbox.save_saved_host(
        "console",
        "Console",
        json!({ "kind": "telnet", "host": "console.example.test", "port": 23 }),
    );
    sandbox.patch_saved_metadata("console", json!({ "loginSequence": { "steps": [] } }));

    let document = sandbox.backup();

    let mut folders = document.data()["folderSettings"]
        .as_array()
        .unwrap()
        .clone();
    folders.sort_by_key(|folder| folder["path"].as_str().unwrap().to_string());
    assert_eq!(
        folders,
        vec![
            json!({ "path": "Lab", "auth": { "user": "lab" } }),
            json!({ "path": "Network", "auth": {}, "loginSequence": login_sequence() }),
        ]
    );
    assert_eq!(
        document.saved_hosts()[0]["metadata"]["loginSequence"],
        json!({ "steps": [] })
    );
    // A round trip through the file keeps it, and keeps every secret out of it.
    let text = document.to_file_text();
    let parsed = parse_transfer_document(&text).unwrap();
    assert!(
        parsed
            .folder_settings()
            .iter()
            .any(|folder| folder.get("loginSequence") == Some(&login_sequence()))
    );
    assert!(text.contains("vault-secret-1"));
}

#[test]
fn restores_valid_sequences_and_drops_a_malformed_one_rather_than_the_folder() {
    let sandbox = Sandbox::new();
    let mut data = connections();
    data.insert("sshHosts".into(), json!([]));
    data.insert("hostOrder".into(), json!([]));
    data.insert("preferences".into(), json!({}));
    data.insert("tunnels".into(), json!([]));
    data.insert("loggingPolicies".into(), json!([]));
    data.insert(
        "historySettings".into(),
        json!({ "maxTotalBytes": 1, "minFreeBytes": 0, "minFreePercent": 0 }),
    );
    data.insert(
        "folderSettings".into(),
        json!([
            { "path": "Network", "auth": {}, "loginSequence": login_sequence() },
            { "path": "Broken", "auth": { "user": "x" }, "loginSequence": { "steps": [{ "kind": "wait" }] } },
        ]),
    );
    let text = json!({
        "format": "muxus-backup",
        "version": 2,
        "createdAt": "2026-08-10T00:00:00.000Z",
        "data": data,
    })
    .to_string();
    let document = parse_transfer_document(&text).unwrap();

    // The writes the TypeScript sends.
    let writes: Vec<Value> = document
        .folder_settings()
        .iter()
        .map(|entry| serde_json::to_value(folder_settings_upsert(entry).unwrap()).unwrap())
        .collect();
    assert_eq!(
        writes,
        vec![
            json!({ "path": "Network", "auth": {}, "loginSequence": login_sequence() }),
            json!({ "path": "Broken", "auth": { "user": "x" } }),
        ]
    );

    block_on(restore_transfer_document(
        &sandbox.backend,
        &document,
        RestoreSelection {
            preferences: false,
            connections: true,
            tunnels: false,
            logging: false,
        },
        TransferConflictStrategy::Replace,
    ))
    .unwrap();

    let folders = sandbox.folders();
    let network = folders
        .iter()
        .find(|folder| folder.path == "Network")
        .unwrap();
    let expected: LoginSequence = serde_json::from_value(login_sequence()).unwrap();
    assert_eq!(network.login_sequence.as_ref(), Some(&expected));
    let broken = folders
        .iter()
        .find(|folder| folder.path == "Broken")
        .unwrap();
    assert_eq!(broken.auth.user.as_deref(), Some("x"));
    assert_eq!(broken.login_sequence, None);
}

#[test]
fn keeps_the_per_host_terminal_override_in_portable_metadata() {
    let sandbox = Sandbox::new();
    sandbox.save_saved_host(
        "console",
        "Console",
        json!({ "kind": "telnet", "host": "console.example.test", "port": 23 }),
    );
    sandbox.patch_saved_metadata(
        "console",
        json!({
            "terminalScheme": "solarized-dark",
            "terminalFontColor": "#fdf6e3",
            "terminalBackgroundColor": "#002b36",
            "pasteLineDelayMs": 200,
            "pasteCharDelayMs": 0,
        }),
    );

    let document = sandbox.backup();
    let metadata = &document.saved_hosts()[0]["metadata"];

    assert_eq!(metadata["terminalScheme"], json!("solarized-dark"));
    assert_eq!(metadata["terminalFontColor"], json!("#fdf6e3"));
    assert_eq!(metadata["terminalBackgroundColor"], json!("#002b36"));
    // An explicit 0 is an override too: this host is never paced.
    assert_eq!(metadata["pasteLineDelayMs"], json!(200));
    assert_eq!(metadata["pasteCharDelayMs"], json!(0));
}

// ---------------------------------------------------------------------------
// OpenSSH export
// ---------------------------------------------------------------------------

fn saved_ssh(id: &str, name: &str, profile: Value, metadata: Value) -> SavedHostProfile {
    let mut ssh = json!({ "kind": "ssh", "useConfig": false, "profileId": id });
    ssh.as_object_mut().unwrap().extend(object(profile));
    let mut meta = json!({ "profileId": id, "connectCount": 0 });
    meta.as_object_mut().unwrap().extend(object(metadata));
    serde_json::from_value(json!({
        "id": id,
        "kind": "ssh",
        "name": name,
        "profile": ssh,
        "metadata": meta,
        "createdAt": "2026-08-07T00:00:00.000Z",
        "updatedAt": "2026-08-07T00:00:00.000Z",
    }))
    .unwrap()
}

fn folder(path: &str, auth: Value, has_password: bool) -> FolderSettingsRecord {
    serde_json::from_value(json!({
        "id": path,
        "path": path,
        "auth": auth,
        "hasPassword": has_password,
        "createdAt": "2026-08-07T00:00:00.000Z",
        "updatedAt": "2026-08-07T00:00:00.000Z",
    }))
    .unwrap()
}

#[test]
fn export_materializes_non_secret_folder_defaults_and_marks_an_omitted_folder_password() {
    let saved = [saved_ssh(
        "saved-1",
        "Core router",
        json!({ "target": "router.example.test", "user": "profile-user" }),
        json!({ "group": "Prod/EU" }),
    )];
    let folders = [
        folder(
            "Prod",
            json!({ "user": "folder-user", "identityFiles": ["~/.ssh/prod"], "identityAgent": "SSH_AUTH_SOCK" }),
            true,
        ),
        folder("Prod/EU", json!({ "port": 2222 }), false),
    ];

    let requests = openssh_export_requests(&[], &saved, &folders);

    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.aliases, ["Core-router"]);
    assert_eq!(
        request.description.as_deref(),
        Some("Exported from Muxus app data. Shared folder password omitted.")
    );
    assert_eq!(
        request.options.hostname.as_deref(),
        Some("router.example.test")
    );
    assert_eq!(request.options.user.as_deref(), Some("profile-user"));
    assert_eq!(request.options.port, Some(2222));
    assert_eq!(
        request.options.identity_files.as_deref(),
        Some(&["~/.ssh/prod".to_string()][..])
    );
    assert_eq!(
        request.options.identity_agent.as_deref(),
        Some("SSH_AUTH_SOCK")
    );
}

#[test]
fn export_writes_the_previewed_blocks() {
    let sandbox = Sandbox::new();
    sandbox.write_ssh_config("Host web\n  HostName web.example.test\n  User deploy\n");
    sandbox.save_saved_host(
        "saved-1",
        "Core router",
        json!({ "kind": "ssh", "target": "router.example.test", "useConfig": false, "user": "profile-user" }),
    );
    sandbox.patch_saved_metadata("saved-1", json!({ "group": "Prod/EU" }));
    sandbox.put_folder("Prod/EU", json!({ "port": 2222 }), None);

    let text = block_on(create_openssh_export(&sandbox.backend)).unwrap();

    assert!(text.starts_with("# OpenSSH connection export from Muxus\n\n# Generated "));
    assert!(
        text.contains(
            "\n\n# Private key files are referenced by path and are not embedded.\n\n\n\n"
        )
    );
    assert!(text.contains("Host web\n"));
    assert!(text.contains("Host Core-router\n"));
    assert!(text.contains("# Exported from Muxus app data.\n"));
    assert!(text.contains("router.example.test"));
    assert!(text.contains("2222"));
    assert!(text.ends_with("\n\n"));
}

#[test]
fn export_text_joins_the_blocks_as_the_typescript_does() {
    assert_eq!(
        openssh_export_text(&["Host a\n  User x\n".into(), "  Host b".into()], "T"),
        "# OpenSSH connection export from Muxus\n\n# Generated T\n\n\
         # Private key files are referenced by path and are not embedded.\n\n\n\n\
         Host a\n  User x\n\nHost b\n\n"
    );
}

#[test]
fn points_proxy_jump_at_the_exported_block_of_a_saved_jump_host() {
    use muxus_core::api::saved_host_hop;
    let saved = [
        // Listed before the jump host it names, whose alias it still needs.
        saved_ssh(
            "app",
            "App",
            json!({
                "target": "app.internal",
                "proxyJump": [saved_host_hop("bastion"), "edge", saved_host_hop("deleted")],
            }),
            json!({}),
        ),
        saved_ssh(
            "bastion",
            "Bastion EU",
            json!({ "target": "bastion.example.test", "user": "jump", "identityFiles": ["~/.ssh/bastion"] }),
            json!({}),
        ),
    ];

    let requests = openssh_export_requests(&[], &saved, &[]);

    let summary: Vec<(Vec<String>, Option<Vec<String>>)> = requests
        .into_iter()
        .map(|request| (request.aliases, request.options.proxy_jump))
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                vec!["App".to_string()],
                Some(vec!["Bastion-EU".to_string(), "edge".to_string()])
            ),
            (vec!["Bastion-EU".to_string()], None),
        ]
    );
}

#[test]
fn export_aliases_avoid_taken_names_and_fall_back_to_the_target() {
    let saved = [
        saved_ssh("a", "web", json!({ "target": "a.example.test" }), json!({})),
        saved_ssh("b", " #?! ", json!({ "target": "b host" }), json!({})),
        saved_ssh("c", "", json!({ "target": "*" }), json!({})),
    ];
    let config: Vec<SshHostEntry> = vec![
        serde_json::from_value(json!({
            "alias": "web",
            "aliases": ["web"],
            "file": "/tmp/config",
            "options": {},
            "resolved": {
                "hostname": "web", "port": 22, "identityFiles": [], "certificateFiles": [],
                "identitiesOnly": false, "forwardAgent": false, "proxyJump": [],
                "forwards": [], "passwordOnly": false,
            },
        }))
        .unwrap(),
    ];
    let aliases: Vec<String> = openssh_export_requests(&config, &saved, &[])
        .into_iter()
        .skip(1)
        .map(|request| request.aliases[0].clone())
        .collect();
    assert_eq!(aliases, ["web-2", "b-host", "muxus-host"]);
}

// ---------------------------------------------------------------------------
// restoring SSH hosts and saved hosts
// ---------------------------------------------------------------------------

fn imported_secondary_alias() -> PortableConnections {
    serde_json::from_value(json!({
        "sshHosts": [{
            "alias": "prod-alt",
            "aliases": ["prod-alt"],
            "options": { "hostname": "new.example.com" },
        }],
        "savedHosts": [],
        "hostOrder": [],
    }))
    .unwrap()
}

#[test]
fn keeps_a_host_when_an_imported_alias_matches_one_of_its_secondary_aliases() {
    let sandbox = Sandbox::new();
    let config = "Host production prod-alt\n  HostName old.example.com\n";
    sandbox.write_ssh_config(config);

    let result = block_on(restore_imported_connections(
        &sandbox.backend,
        &imported_secondary_alias(),
        TransferConflictStrategy::Keep,
    ))
    .unwrap();

    assert_eq!(
        result,
        RestoreResult {
            added: 0,
            updated: 0,
            skipped: 1
        }
    );
    assert_eq!(sandbox.ssh_config(), config);
}

#[test]
fn uses_the_owning_primary_alias_when_replacing_through_a_secondary_alias() {
    let sandbox = Sandbox::new();
    sandbox.write_ssh_config("Host production prod-alt\n  HostName old.example.com\n");
    let existing = block_on(sandbox.backend.ssh_config()).unwrap().hosts[0].clone();
    let file = existing.file.clone();

    // The write the TypeScript sends.
    let host = serde_json::to_value(&imported_secondary_alias().ssh_hosts[0]).unwrap();
    assert_eq!(
        ssh_upsert_request(&host, Some(&existing)).unwrap(),
        HostUpsertRequest {
            aliases: vec!["prod-alt".into()],
            description: None,
            file: Some(file),
            options: serde_json::from_value(json!({ "hostname": "new.example.com" })).unwrap(),
            previous_alias: Some("production".into()),
        }
    );

    let result = block_on(restore_imported_connections(
        &sandbox.backend,
        &imported_secondary_alias(),
        TransferConflictStrategy::Replace,
    ))
    .unwrap();

    assert_eq!(
        result,
        RestoreResult {
            added: 0,
            updated: 1,
            skipped: 0
        }
    );
    let config = sandbox.ssh_config();
    assert!(config.contains("Host prod-alt"), "{config}");
    assert!(config.contains("new.example.com"), "{config}");
    assert!(!config.contains("Host production"), "{config}");
}

fn imported_serial() -> PortableConnections {
    serde_json::from_value(json!({
        "sshHosts": [],
        "savedHosts": [{
            "id": "securecrt-serial-console",
            "name": "Rack console",
            "profile": {
                "kind": "serial",
                "path": "/dev/ttyUSB0",
                "baudRate": 115200,
                "dataBits": 8,
                "stopBits": 1,
                "parity": "none",
                "flowControl": "none",
            },
            "metadata": {
                "group": "Lab",
                "terminalScheme": "nord",
                "terminalFontColor": "#eceff4",
                "terminalBackgroundColor": "#2e3440",
            },
        }],
        "hostOrder": [],
    }))
    .unwrap()
}

#[test]
fn keeps_a_native_profile_with_the_same_deterministic_import_id() {
    let sandbox = Sandbox::new();
    sandbox.save_saved_host(
        "securecrt-serial-console",
        "Old console",
        json!({ "kind": "serial", "path": "/dev/ttyS0" }),
    );

    let result = block_on(restore_imported_connections(
        &sandbox.backend,
        &imported_serial(),
        TransferConflictStrategy::Keep,
    ))
    .unwrap();

    assert_eq!(
        result,
        RestoreResult {
            added: 0,
            updated: 0,
            skipped: 1
        }
    );
    assert_eq!(sandbox.saved_hosts()[0].name, "Old console");
}

#[test]
fn upserts_and_organizes_a_selected_serial_profile() {
    let sandbox = Sandbox::new();
    let imported = imported_serial();

    // The metadata write the TypeScript sends.
    let metadata = serde_json::to_value(&imported.saved_hosts[0].metadata).unwrap();
    let patch: OpenSshMetadataPatch = metadata_patch(&metadata).unwrap();
    assert_eq!(
        serde_json::to_value(&patch).unwrap(),
        json!({
            "displayName": null,
            "group": "Lab",
            "color": null,
            "icon": null,
            "terminalScheme": "nord",
            "terminalFontColor": "#eceff4",
            "terminalBackgroundColor": "#2e3440",
            "keywordHighlights": null,
            "commandButtonGroup": null,
            "pasteLineDelayMs": null,
            "pasteCharDelayMs": null,
            "disableSftp": false,
            "consoleCompatibility": false,
            "loginSequence": null,
        })
    );

    let result = block_on(restore_imported_connections(
        &sandbox.backend,
        &imported,
        TransferConflictStrategy::Replace,
    ))
    .unwrap();

    assert_eq!(
        result,
        RestoreResult {
            added: 1,
            updated: 0,
            skipped: 0
        }
    );
    let saved = sandbox.saved_hosts();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].id, "securecrt-serial-console");
    assert_eq!(saved[0].name, "Rack console");
    let SavedHostSessionProfile::Serial(serial) = &saved[0].profile else {
        panic!("not a serial profile");
    };
    assert_eq!(serial.path, "/dev/ttyUSB0");
    assert_eq!(serial.baud_rate, 115_200);
    assert_eq!(saved[0].metadata.group.as_deref(), Some("Lab"));
    assert_eq!(saved[0].metadata.terminal_scheme.as_deref(), Some("nord"));
    assert_eq!(
        saved[0].metadata.terminal_background_color.as_deref(),
        Some("#2e3440")
    );
}

#[test]
fn restores_muxus_only_ssh_hosts_through_the_native_profile_store() {
    let sandbox = Sandbox::new();
    let imported: PortableConnections = serde_json::from_value(json!({
        "sshHosts": [],
        "savedHosts": [{
            "id": "mobaxterm-ssh-router",
            "name": "Core router",
            "profile": {
                "kind": "ssh",
                "target": "router.example.test",
                "useConfig": false,
                "user": "admin",
                "passwordOnly": true,
            },
            "metadata": { "group": "Lab" },
        }],
        "hostOrder": [],
    }))
    .unwrap();

    let result = block_on(restore_imported_connections(
        &sandbox.backend,
        &imported,
        TransferConflictStrategy::Replace,
    ))
    .unwrap();

    assert_eq!(
        result,
        RestoreResult {
            added: 1,
            updated: 0,
            skipped: 0
        }
    );
    let saved = sandbox.saved_hosts();
    let SavedHostSessionProfile::Ssh(ssh) = &saved[0].profile else {
        panic!("not an SSH profile");
    };
    assert_eq!(ssh.target, "router.example.test");
    assert_eq!(ssh.user.as_deref(), Some("admin"));
    assert_eq!(ssh.password_only, Some(true));
    // ssh_config is never touched.
    assert!(!sandbox.ssh_config_path().exists());
}

#[test]
fn a_backup_restores_into_a_fresh_installation() {
    let source = Sandbox::new();
    source.write_ssh_config(
        "Host *\n  ForwardAgent yes\n\nHost production\n  HostName prod.example.com\n  User deploy\n",
    );
    block_on(source.backend.patch_ssh_host_metadata(
        "production",
        serde_json::from_value(json!({ "group": "Work", "displayName": "Prod" })).unwrap(),
    ))
    .unwrap();
    source.save_saved_host(
        "console",
        "Console",
        json!({ "kind": "telnet", "host": "console.example.test", "port": 23 }),
    );
    block_on(
        source.backend.save_tunnel(
            serde_json::from_value(json!({
                "id": "tunnel-1", "target": "production", "type": "dynamic", "bindPort": 1080,
            }))
            .unwrap(),
        ),
    )
    .unwrap();
    let document = source.backup();

    // Inherited `Host *` values travel with the host, its own options first.
    let options = document.ssh_hosts()[0]["options"].as_object().unwrap();
    let keys: Vec<&str> = options.keys().map(String::as_str).collect();
    assert_eq!(&keys[..2], ["hostname", "user"]);
    assert_eq!(options["forwardAgent"], json!(true));
    assert_eq!(options["port"], json!(22));
    assert_eq!(
        document.ssh_hosts()[0]["metadata"],
        json!({ "displayName": "Prod", "group": "Work" })
    );
    assert_eq!(
        document.host_order(),
        &[
            json!({ "kind": "profile", "id": "console" }),
            json!({ "kind": "ssh", "alias": "production" }),
        ][..]
    );

    let target = Sandbox::new();
    let pending =
        read_transfer_file("muxus-backup.muxus", document.to_file_text().as_bytes()).unwrap();
    let result = block_on(restore_transfer_document(
        &target.backend,
        &pending.document,
        RestoreSelection::default(),
        TransferConflictStrategy::Keep,
    ))
    .unwrap();
    // Two connections and a tunnel added; history limits, log files and preferences updated.
    assert_eq!(
        result,
        RestoreResult {
            added: 3,
            updated: 3,
            skipped: 0
        }
    );
    let config = block_on(target.backend.ssh_config()).unwrap();
    assert_eq!(config.hosts[0].alias, "production");
    assert_eq!(config.hosts[0].resolved.hostname, "prod.example.com");
    assert!(config.hosts[0].resolved.forward_agent);
    assert_eq!(
        config.hosts[0]
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.display_name.as_deref()),
        Some("Prod")
    );
    assert_eq!(target.saved_hosts()[0].id, "console");
    assert_eq!(
        block_on(target.backend.tunnels()).unwrap().tunnels[0].id,
        "tunnel-1"
    );

    // Restoring again keeps everything that is already there.
    let again = block_on(restore_transfer_document(
        &target.backend,
        &pending.document,
        RestoreSelection {
            preferences: false,
            connections: true,
            tunnels: true,
            logging: false,
        },
        TransferConflictStrategy::Keep,
    ))
    .unwrap();
    assert_eq!(
        again,
        RestoreResult {
            added: 0,
            updated: 0,
            skipped: 3
        }
    );
}

#[test]
fn data_summary_counts_connections_and_tunnels() {
    let sandbox = Sandbox::new();
    sandbox.write_ssh_config("Host a\n  HostName a.test\n\nHost b\n  HostName b.test\n");
    sandbox.save_saved_host(
        "console",
        "Console",
        json!({ "kind": "telnet", "host": "console.example.test" }),
    );
    assert_eq!(
        block_on(fetch_data_summary(&sandbox.backend)).unwrap(),
        DataSummary {
            connections: 3,
            tunnels: 0
        }
    );
}

// ---------------------------------------------------------------------------
// Backup & data section helpers (DataTransferSection.tsx)
// ---------------------------------------------------------------------------

#[test]
fn section_texts() {
    assert_eq!(plural(1, "tunnel"), "1 tunnel");
    assert_eq!(plural(0, "connection"), "0 connections");
    assert_eq!(summary_line(None, true), "Reading your Muxus data…");
    assert_eq!(
        summary_line(
            Some(&DataSummary {
                connections: 12,
                tunnels: 1
            }),
            false
        ),
        "12 connections  ·  1 tunnel"
    );
    assert_eq!(
        summary_line(None, false),
        "Settings, connections, tunnels and logging policies"
    );
    assert_eq!(last_backup_line(None), "No backup yet.");
    assert_eq!(BACKUP_SAVED, "Backup saved.");
    assert!(dated_transfer_filename().starts_with("muxus-backup-"));
    assert!(dated_transfer_filename().ends_with(".muxus"));
    assert!(openssh_export_filename().starts_with("muxus-openssh-connections-"));
    assert!(openssh_export_filename().ends_with(".conf"));
}

#[test]
fn friendly_dates_show_only_the_time_today() {
    use chrono::{Local, TimeZone};
    let now = Local.with_ymd_and_hms(2026, 10, 12, 15, 0, 0).unwrap();
    let today = Local
        .with_ymd_and_hms(2026, 10, 12, 9, 5, 0)
        .unwrap()
        .to_utc()
        .to_rfc3339();
    assert_eq!(friendly_date_at(&today, now), "09:05 AM");
    let earlier = Local
        .with_ymd_and_hms(2026, 7, 4, 21, 30, 0)
        .unwrap()
        .to_utc()
        .to_rfc3339();
    assert_eq!(friendly_date_at(&earlier, now), "Jul 4, 2026, 09:30 PM");
}

#[test]
fn restore_messages() {
    assert_eq!(
        restore_complete_message(&RestoreResult {
            added: 2,
            updated: 0,
            skipped: 1
        }),
        "Restore complete — 2 added · 1 kept."
    );
    assert_eq!(
        restore_complete_message(&RestoreResult::default()),
        "Restore complete."
    );
    assert_eq!(
        restore_failed_message("Config file is read-only."),
        "Config file is read-only. Some items may already have been restored; it is safe to \
         review the file and try again."
    );
    assert_eq!(
        proxy_command_warning(1),
        "1 connection contains a ProxyCommand. Muxus does not run it during restore, but it \
         will run as a local shell command when that connection is opened. Restore only backups \
         you trust."
    );
}

#[test]
fn review_dialog_counts_the_categories_and_proxy_commands() {
    let text = backup_text(json!({
        "sshHosts": [
            { "alias": "jump", "aliases": ["jump"], "options": { "proxyCommand": "nc %h %p" } },
            { "alias": "plain", "aliases": ["plain"], "options": { "proxyCommand": "  " } },
        ],
        "savedHosts": [{
            "id": "router",
            "name": "Router",
            "profile": { "kind": "ssh", "target": "r", "useConfig": false, "proxyCommand": "ssh -W %h:%p gw" },
            "metadata": {},
        }],
        "hostOrder": [],
    }));
    let mut value: Value = serde_json::from_str(&text).unwrap();
    value["appVersion"] = json!("0.11.1");
    let pending = read_transfer_file("backup.muxus", value.to_string().as_bytes()).unwrap();
    let document = &pending.document;

    assert_eq!(connection_count(document), 3);
    assert_eq!(proxy_command_count(document), 2);
    let rows = restore_categories(document);
    assert_eq!(
        rows.iter().map(|row| row.count).collect::<Vec<_>>(),
        [None, Some(3), Some(0), Some(0)]
    );
    assert_eq!(rows[1].label, "Connections");
    let subtitle = review_subtitle(&pending);
    assert!(subtitle.starts_with("backup.muxus · "));
    assert!(subtitle.ends_with(" · Muxus 0.11.1"));

    let mut selection = RestoreSelection::default();
    assert_eq!(selection.selected_count(), 4);
    for category in RestoreCategory::ALL {
        selection.set(category, false);
        assert!(!selection.get(category));
    }
    assert_eq!(selection.selected_count(), 0);
    assert_eq!(
        TransferConflictStrategy::default(),
        TransferConflictStrategy::Keep
    );
}

#[test]
fn refuses_files_over_the_size_limit() {
    let big = vec![b' '; MAX_TRANSFER_FILE_BYTES as usize + 1];
    assert_eq!(
        read_transfer_file("big.muxus", &big).unwrap_err().message,
        TRANSFER_FILE_TOO_LARGE
    );
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.muxus");
    assert_eq!(
        load_transfer_file(&missing).unwrap_err().message,
        "Could not read that file."
    );
    let path = dir.path().join("muxus-backup-2026-10-04.muxus");
    std::fs::write(&path, backup_text(json!({}))).unwrap();
    let pending = load_transfer_file(&path).unwrap();
    assert_eq!(pending.filename, "muxus-backup-2026-10-04.muxus");
}

#[test]
fn remembers_the_last_backup() {
    let sandbox = Sandbox::new();
    assert_eq!(block_on(read_last_backup(&sandbox.backend)), None);
    block_on(remember_backup(
        &sandbox.backend,
        "2026-10-04T12:00:00.000Z",
    ))
    .unwrap();
    assert_eq!(
        block_on(read_last_backup(&sandbox.backend)).as_deref(),
        Some("2026-10-04T12:00:00.000Z")
    );
    // A stored value that is not a time is ignored.
    block_on(remember_backup(&sandbox.backend, "yesterday")).unwrap();
    assert_eq!(block_on(read_last_backup(&sandbox.backend)), None);
}
