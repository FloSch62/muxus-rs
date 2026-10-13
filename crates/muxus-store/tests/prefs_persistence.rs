//! Preferences persistence against what the Electron build writes.
//!
//! `fixtures/prefs/rehydrate-cases.json` was produced by running the real
//! TypeScript store (`client/src/state/prefs.ts` with zustand's `persist`)
//! over each stored `muxus-prefs` value and recording what it wrote back.

use muxus_store::prefs::state::{SidebarPosition, ThemeMode};
use muxus_store::prefs::{
    ClientStateFile, PREFS_STORAGE_NAME, PrefsDocument, PrefsState, PrefsStore, default_state_map,
};
use serde_json::{Value, json};

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    stored: Option<String>,
    written: Vec<String>,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(include_str!("fixtures/prefs/rehydrate-cases.json")).unwrap()
}

/// What the Rust store would write after rehydrating `stored`: the migrated
/// copy, or (for cases the fixture forced a save on) the merged state.
fn rust_write(stored: Option<&str>) -> (String, bool) {
    let rehydration = PrefsDocument::rehydrate(stored);
    (
        rehydration.document.to_envelope_json(),
        rehydration.migrated,
    )
}

#[test]
fn rehydrates_every_stored_shape_byte_for_byte() {
    for case in cases() {
        let stored = case.stored.as_deref().filter(|text| !text.is_empty());
        let (written, _) = rust_write(stored);
        assert_eq!(case.written.len(), 1, "{}", case.name);
        if written != case.written[0] {
            let ours: Value = serde_json::from_str(&written).unwrap();
            let theirs: Value = serde_json::from_str(&case.written[0]).unwrap();
            assert_eq!(ours, theirs, "{}: semantic difference", case.name);
            panic!(
                "{}: same JSON, different bytes\nours:   {written}\ntheirs: {}",
                case.name, case.written[0]
            );
        }
    }
}

#[test]
fn writes_back_exactly_when_zustand_migrates() {
    for case in cases() {
        let stored = case.stored.as_deref().filter(|text| !text.is_empty());
        let (_, migrated) = rust_write(stored);
        let expected = matches!(
            case.name.as_str(),
            "legacy-v0" | "legacy-v8" | "v15-seeded-builtins" | "newer-v18"
        );
        assert_eq!(migrated, expected, "{}", case.name);
    }
}

#[test]
fn defaults_match_the_typescript_store() {
    let empty = cases()
        .into_iter()
        .find(|case| case.name == "empty")
        .unwrap();
    assert_eq!(
        PrefsDocument::default().to_envelope_json(),
        empty.written[0]
    );
    let state: Value = serde_json::from_str::<Value>(&empty.written[0]).unwrap()["state"].clone();
    assert_eq!(Value::Object(default_state_map()), state);
    // Every modeled key serializes under the name the macro matches on.
    let keys: Vec<String> = PrefsState::default()
        .to_json_map()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        PrefsState::KEYS
            .iter()
            .map(|key| key.to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(PrefsDocument::default().prefs(), PrefsState::default());
}

#[test]
fn keeps_unvalidated_values_of_a_current_snapshot() {
    let case = cases()
        .into_iter()
        .find(|case| case.name == "current-v17-unvalidated")
        .unwrap();
    let rehydration = PrefsDocument::rehydrate(case.stored.as_deref());
    let state = rehydration.document.state_map();
    // Stored as is; the typed view falls back where the type is wrong.
    assert_eq!(state["themeMode"], json!("sepia"));
    assert_eq!(state["monoFontSize"], json!("16"));
    let prefs = rehydration.document.prefs();
    assert_eq!(prefs.theme_mode, ThemeMode::Os);
    assert_eq!(prefs.mono_font_size, 14.0);
    assert_eq!(prefs.font_family, "Iosevka");
    assert_eq!(prefs.extra.get("extra"), Some(&json!(1)));
}

#[test]
fn updates_rewrite_only_changed_fields() {
    let case = cases()
        .into_iter()
        .find(|case| case.name == "current-v17-unvalidated")
        .unwrap();
    let mut document = PrefsDocument::rehydrate(case.stored.as_deref()).document;
    let mut prefs = document.prefs();
    assert!(
        !document.update(&prefs),
        "an unchanged value writes nothing"
    );
    prefs.sidebar_position = SidebarPosition::Right;
    prefs.extra.insert("added".into(), json!(true));
    prefs.extra.shift_remove("extra");
    assert!(document.update(&prefs));
    let state = document.state_map();
    assert_eq!(state["sidebarPosition"], json!("right"));
    // Values this build cannot read stay untouched.
    assert_eq!(state["monoFontSize"], json!("16"));
    assert!(!state.contains_key("extra"));
    assert_eq!(state.keys().next_back().map(String::as_str), Some("added"));
}

#[test]
fn store_round_trips_through_client_state_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Muxus").join("client-state.json");
    let legacy = cases()
        .into_iter()
        .find(|case| case.name == "legacy-v8")
        .unwrap();
    let mut file = ClientStateFile::empty(&path);
    file.set_item("muxus-last-backup-at", "2026-01-01T00:00:00.000Z");
    file.set_item(PREFS_STORAGE_NAME, legacy.stored.clone().unwrap());
    file.set_item("other-window-key", "kept");
    file.save().unwrap();

    let (mut store, error) = PrefsStore::open(&path);
    assert!(error.is_none());
    // The migrated copy was written back at once, in place.
    let on_disk = ClientStateFile::load(&path);
    assert_eq!(
        on_disk.get_item(PREFS_STORAGE_NAME),
        Some(legacy.written[0].as_str())
    );
    assert_eq!(
        on_disk.items().keys().collect::<Vec<_>>(),
        vec![
            "muxus-last-backup-at",
            PREFS_STORAGE_NAME,
            "other-window-key"
        ]
    );
    assert_eq!(store.last_backup_at(), Some("2026-01-01T00:00:00.000Z"));

    let mut prefs = store.prefs();
    assert_eq!(prefs.light_terminal_scheme, "paper");
    assert_eq!(prefs.dark_terminal_scheme, "nord");
    prefs.mono_font_size = 15.0;
    store.set(&prefs).unwrap();
    let (reopened, _) = PrefsStore::open(&path);
    assert_eq!(reopened.prefs().mono_font_size, 15.0);
    assert_eq!(
        reopened.client_state().get_item("other-window-key"),
        Some("kept")
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with("}\n") && text.starts_with("{\n  \""));

    // Another writer changes the file; reload takes it without writing.
    let mut other = ClientStateFile::load(&path);
    let mut changed: Value =
        serde_json::from_str(other.get_item(PREFS_STORAGE_NAME).unwrap()).unwrap();
    changed["state"]["themeMode"] = json!("dark");
    other.set_item(PREFS_STORAGE_NAME, changed.to_string());
    other.save().unwrap();
    assert!(store.reload().is_none());
    assert_eq!(store.prefs().theme_mode, ThemeMode::Dark);
}

#[test]
fn unreadable_files_read_as_empty_and_defaults_apply() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("client-state.json");
    std::fs::write(&path, "[1,2,3]").unwrap();
    let (store, error) = PrefsStore::open(&path);
    assert!(error.is_none());
    assert_eq!(store.prefs(), PrefsState::default());
    assert!(store.client_state().items().is_empty());
}
