//! Port of `tests/unit/client/settings-search.test.ts`.

use std::collections::BTreeSet;

use regex::Regex;

use super::*;

fn linux() -> SettingsSearchContext {
    SettingsSearchContext {
        desktop: true,
        platform: Some("linux".into()),
    }
}

fn windows() -> SettingsSearchContext {
    SettingsSearchContext {
        platform: Some("win32".into()),
        ..linux()
    }
}

fn labels(query: &str) -> Vec<&'static str> {
    labels_in(query, &linux())
}

fn labels_in(query: &str, context: &SettingsSearchContext) -> Vec<&'static str> {
    search_settings(query, context)
        .into_iter()
        .map(|entry| entry.label)
        .collect()
}

#[test]
fn finds_nothing_without_a_query() {
    assert!(labels("").is_empty());
    assert!(labels("   ").is_empty());
}

#[test]
fn puts_settings_whose_label_starts_with_the_query_first() {
    assert_eq!(labels("font")[..2], ["Font family", "Font size"]);
    assert_eq!(labels("dark")[0], "Dark terminal theme");
}

#[test]
fn matches_every_word_in_any_field_and_in_any_order() {
    assert_eq!(labels("ssh keepalive"), ["SSH keepalive interval"]);
    assert_eq!(labels("keepalive ssh"), ["SSH keepalive interval"]);
    assert!(labels("dark mode").contains(&"Theme"));
    assert_eq!(labels("ctrl+d"), ["Close the tab when its shell exits"]);
}

#[test]
fn ignores_case_accents_spaces_and_punctuation_inside_a_word() {
    assert_eq!(
        labels("OSC52"),
        ["Allow terminal clipboard writes (OSC 52)"]
    );
    assert_eq!(labels("webgl"), ["GPU renderer (WebGL)"]);
    assert!(labels("Thème").contains(&"Theme"));
}

#[test]
fn finds_every_setting_of_a_section_by_its_name() {
    let x11 = search_settings("x11 forwarding", &windows());
    assert!(
        x11.iter()
            .any(|entry| entry.section == SettingsSection::X11)
    );
    assert!(
        x11.iter()
            .any(|entry| entry.label == "Enable X11 forwarding")
    );
}

#[test]
fn leaves_out_settings_that_do_not_exist_here() {
    assert!(labels("wsl distributions").is_empty());
    assert_eq!(
        labels_in("wsl distributions", &windows()),
        ["WSL distributions", "List installed distributions"]
    );
    let browser = SettingsSearchContext {
        desktop: false,
        ..linux()
    };
    assert!(labels_in("ssh links", &browser).is_empty());
    assert_eq!(labels("ssh links"), ["SSH links"]);
}

#[test]
fn drops_the_count_from_a_group_title() {
    assert_eq!(
        settings_title_key("Saved profiles \u{b7} 3"),
        "Saved profiles"
    );
    assert_eq!(settings_title_key("Vault"), "Vault");
}

#[test]
fn lists_each_setting_once() {
    let keys: Vec<String> = SETTINGS_SEARCH_INDEX.iter().map(|e| e.key()).collect();
    let unique: BTreeSet<&String> = keys.iter().collect();
    assert_eq!(unique.len(), keys.len());
}

// ---------------------------------------------------------------------------
// The index against the settings views
// ---------------------------------------------------------------------------

/// The views that render one section, or a part of it.
const FILE_SECTIONS: &[(&str, SettingsSection)] = &[
    ("view/about.rs", SettingsSection::About),
    ("view/appearance.rs", SettingsSection::Appearance),
    ("view/custom_schemes.rs", SettingsSection::Appearance),
    ("view/terminal.rs", SettingsSection::Terminal),
    ("view/local_shells.rs", SettingsSection::LocalShells),
    ("view/logging.rs", SettingsSection::Logging),
    ("view/highlighting.rs", SettingsSection::Highlighting),
    ("view/behavior.rs", SettingsSection::Behavior),
    ("../desktop/links_view.rs", SettingsSection::Behavior),
    ("view/x11.rs", SettingsSection::X11),
    ("view/keyboard.rs", SettingsSection::Keyboard),
    ("view/passwords.rs", SettingsSection::Passwords),
    ("view/data.rs", SettingsSection::Data),
    ("view/debug.rs", SettingsSection::Debug),
];

/// Fields of the profile or scheme picked in a list, found through their group.
const LIST_EDITOR_ROWS: [&str; 3] = ["Scheme", "Profile", "Name"];

/// Settings the index already lists whose rows other ports bring to these
/// views. Being listed here lets a row be missing from the views; once the
/// row is there, this list no longer matters for it.
const NOT_YET_IN_VIEWS: &[(SettingsSection, &str)] = &[
    (
        SettingsSection::Behavior,
        "Close the tab when its shell exits",
    ),
    (SettingsSection::Behavior, "Show program status on tabs"),
];

fn source(file: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/features/settings")
        .join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

type Found = (BTreeSet<String>, BTreeSet<String>);

fn settings_in_source(section: SettingsSection, text: &str, found: &mut Found) {
    let (rows, groups) = found;
    let id = section.id();
    // `row(&theme, "Label")` and the helpers that build a row from a label.
    let row_patterns = [
        r#"(?:^|[^.\w])row\(\s*&?theme,\s*"([^"]+)""#,
        r#"\.switch_row\(\s*&?theme,\s*"[^"]*",\s*"([^"]+)""#,
        r#"\.select_row\(\s*&?theme,\s*"([^"]+)""#,
        r#"\.color_override_row\(\s*&?theme,\s*"([^"]+)""#,
        // Link handlers label their rows from a table.
        r#"\(ConnectionLinkScheme::\w+,\s*"([^"]+)"\)"#,
    ];
    for pattern in row_patterns {
        for capture in Regex::new(pattern).unwrap().captures_iter(text) {
            let label = &capture[1];
            if !LIST_EDITOR_ROWS.contains(&label) {
                rows.insert(format!("{id}|{label}"));
            }
        }
    }
    let group_patterns = [
        r#"(?:^|[^.\w])group\(\s*"([^"]+)""#,
        // A counted title, `Saved profiles · 3`, falls back to its bare name.
        r#"(?:^|[^.\w])group\(\s*texts::counted_title\(\s*"([^"]+)""#,
        // The logging policy forms name their groups from a table.
        r#"PolicyForm::\w+\s*=>\s*\(\s*"([^"]+)""#,
    ];
    for pattern in group_patterns {
        for capture in Regex::new(pattern).unwrap().captures_iter(text) {
            groups.insert(format!("{id}|{}", &capture[1]));
        }
    }
}

fn source_settings() -> Found {
    let mut found = Found::default();
    for (file, section) in FILE_SECTIONS {
        settings_in_source(*section, &source(file), &mut found);
    }
    found
}

fn indexed_settings() -> Found {
    let (mut rows, mut groups) = Found::default();
    for entry in SETTINGS_SEARCH_INDEX {
        let id = entry.section.id();
        match entry.kind {
            EntryKind::Group => {
                groups.insert(format!("{id}|{}", entry.label));
            }
            EntryKind::Row => {
                rows.insert(format!("{id}|{}", entry.focus_label()));
            }
        }
        if let Some(group) = entry.group {
            groups.insert(format!("{id}|{group}"));
        }
    }
    (rows, groups)
}

#[test]
fn lists_every_setting_row_and_group_in_the_section_that_shows_it() {
    let (source_rows, source_groups) = source_settings();
    let (indexed_rows, indexed_groups) = indexed_settings();
    let unindexed: Vec<&String> = source_rows.difference(&indexed_rows).collect();
    assert!(
        unindexed.is_empty(),
        "rows missing from SETTINGS_SEARCH_INDEX: {unindexed:?}"
    );
    let pending: BTreeSet<String> = NOT_YET_IN_VIEWS
        .iter()
        .map(|(section, label)| format!("{}|{label}", section.id()))
        .collect();
    let stale: Vec<&String> = indexed_rows
        .difference(&source_rows)
        .filter(|row| !pending.contains(*row))
        .collect();
    assert!(stale.is_empty(), "indexed rows no view shows: {stale:?}");
    assert_eq!(indexed_groups, source_groups);
}

#[test]
fn knows_every_view_that_renders_settings() {
    let view = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/features/settings/view");
    let known: BTreeSet<&str> = FILE_SECTIONS.iter().map(|(file, _)| *file).collect();
    let row = Regex::new(
        r#"(?:(?:^|[^.\w])row\(\s*&?theme,\s*"|(?:^|[^.\w])group\(\s*"[^"]|\.switch_row\(|\.select_row\()"#,
    )
    .unwrap();
    let mut rendering = Vec::new();
    for entry in std::fs::read_dir(view).unwrap() {
        let path = entry.unwrap().path();
        let name = format!("view/{}", path.file_name().unwrap().to_string_lossy());
        // The kit defines the helpers; common wraps them.
        if name == "view/kit.rs" || name == "view/common.rs" {
            continue;
        }
        if row.is_match(&std::fs::read_to_string(&path).unwrap()) {
            rendering.push(name);
        }
    }
    for name in &rendering {
        assert!(
            known.contains(name.as_str()),
            "{name} renders settings: map it to its section"
        );
    }
}
