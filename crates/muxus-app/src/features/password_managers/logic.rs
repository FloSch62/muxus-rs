//! What the password-manager views show and send, apart from drawing it: the texts of
//! `PasswordManagersGroup.tsx`, `PasswordManagerDialog.tsx`, `PasswordManagerUnlockForm.tsx`,
//! `PasswordEntryField.tsx` and `PasswordEntryPicker.tsx`, and the picker's search.

use muxus_core::api::{
    ExternalPasswordRef, PasswordManagerConfig, PasswordManagerEntry, PasswordManagerInput,
    PasswordManagerKind, PasswordManagerSource, PasswordManagerState,
};
use muxus_core::js::js_trim;
use muxus_ui::theme::Status;

/// Rows drawn at once; a longer result asks for a narrower search instead.
pub const MAX_ROWS: usize = 150;

pub const GROUP_DESCRIPTION: &str = "Log in with passwords kept in KeePass, 1Password, Bitwarden, Proton Pass, LastPass, Dashlane or pass. Pick an entry in a host's Authentication settings or in a folder's shared credentials. Muxus stores which entry to use, never the password, and reads it from the manager each time.";

pub const PICKER_DESCRIPTION: &str = "Muxus remembers which entry to use, not the password. It is read from the password manager each time you connect.";

pub const DISCONNECT_DESCRIPTION: &str = "Hosts and folders that use an entry from it will ask for their password again. Nothing changes in the password manager itself.";

/// What each kind needs before Muxus can read from it, and the program it runs
/// (`KIND_NOTES`).
pub fn kind_program(kind: PasswordManagerKind) -> Option<&'static str> {
    match kind {
        PasswordManagerKind::Keepass => None,
        PasswordManagerKind::Onepassword => Some("op"),
        PasswordManagerKind::Bitwarden => Some("bw"),
        PasswordManagerKind::Protonpass => Some("pass-cli"),
        PasswordManagerKind::Lastpass => Some("lpass"),
        PasswordManagerKind::Dashlane => Some("dcli"),
        PasswordManagerKind::Pass => Some("gpg"),
    }
}

pub fn kind_note(kind: PasswordManagerKind) -> &'static str {
    match kind {
        PasswordManagerKind::Keepass => {
            "Reads the .kdbx file directly, so KeePass, KeePassXC and compatible apps all work and none has to be running. You unlock it in Muxus with the database password; databases that need a hardware key are not supported."
        }
        PasswordManagerKind::Onepassword => {
            "Uses the 1Password CLI (op). Turn on “Integrate with 1Password CLI” in the 1Password app under Settings → Developer; the app then asks you to approve access."
        }
        PasswordManagerKind::Bitwarden => {
            "Uses the Bitwarden CLI (bw), which also works with Vaultwarden. Log in once with “bw login” in a terminal; Muxus then unlocks the vault with your master password."
        }
        PasswordManagerKind::Protonpass => {
            "Uses the Proton Pass CLI (pass-cli). Log in once with “pass-cli login” in a terminal."
        }
        PasswordManagerKind::Lastpass => {
            "Uses the LastPass CLI (lpass). Log in once with “lpass login <email>” in a terminal."
        }
        PasswordManagerKind::Dashlane => {
            "Uses the Dashlane CLI (dcli). Run “dcli sync” once in a terminal to log in and unlock it."
        }
        PasswordManagerKind::Pass => {
            "Reads the standard Unix password store, as pass and gopass keep it, and decrypts entries with GnuPG. The passphrase prompt comes from gpg-agent."
        }
    }
}

/// A kind in the connect dialog's list: its name, and its program when that is missing.
pub fn kind_choice_label(kind: PasswordManagerKind, installed: &[PasswordManagerKind]) -> String {
    match kind_program(kind) {
        Some(program) if !installed.contains(&kind) => {
            format!("{}   {program} not found", kind.label())
        }
        _ => kind.label().to_owned(),
    }
}

/// Whether the kind's program is neither on this computer nor named in the dialog.
pub fn missing_program(
    kind: PasswordManagerKind,
    installed: &[PasswordManagerKind],
    program: &str,
) -> bool {
    kind_program(kind).is_some() && !installed.contains(&kind) && js_trim(program).is_empty()
}

/// The helper text under "Path to <program>".
pub fn program_helper(kind: PasswordManagerKind, missing: bool) -> String {
    match kind_program(kind) {
        Some(program) if missing => {
            format!("{program} was not found on this computer. Install it, or enter where it is.")
        }
        _ => "Leave empty to find it on PATH.".to_owned(),
    }
}

/// Whether the dialog still lacks what its kind cannot do without: a KeePass database.
pub fn incomplete(kind: PasswordManagerKind, config: &PasswordManagerConfig) -> bool {
    kind == PasswordManagerKind::Keepass
        && config
            .path
            .as_deref()
            .is_none_or(|path| js_trim(path).is_empty())
}

/// What the dialog saves: the name only when one was typed.
pub fn manager_input(
    kind: PasswordManagerKind,
    name: &str,
    config: &PasswordManagerConfig,
) -> PasswordManagerInput {
    let name = js_trim(name);
    PasswordManagerInput {
        kind,
        name: (!name.is_empty()).then(|| name.to_owned()),
        config: config.clone(),
    }
}

/// Whether Muxus itself can unlock this manager, with the password it asks for.
pub fn unlocks_in_muxus(kind: PasswordManagerKind) -> bool {
    matches!(
        kind,
        PasswordManagerKind::Keepass | PasswordManagerKind::Bitwarden
    )
}

pub fn unlock_password_label(kind: PasswordManagerKind) -> &'static str {
    if kind == PasswordManagerKind::Bitwarden {
        "Bitwarden master password"
    } else {
        "Database password"
    }
}

/// A database protected by a key file alone has no password to type.
pub fn unlock_password_optional(source: &PasswordManagerSource) -> bool {
    source.kind == PasswordManagerKind::Keepass
        && source
            .config
            .key_file
            .as_deref()
            .is_some_and(|file| !file.is_empty())
}

pub fn unlock_helper(source: &PasswordManagerSource) -> String {
    format!("Unlocks {} until you lock it or quit Muxus.", source.name)
}

/// A source's state in the Settings list (`sourceState`).
pub fn source_state(source: &PasswordManagerSource) -> (Status, String) {
    let said = |fallback: &str| {
        source
            .message
            .clone()
            .unwrap_or_else(|| fallback.to_owned())
    };
    match source.state {
        PasswordManagerState::Ready => (
            Status::Success,
            if unlocks_in_muxus(source.kind) {
                "Unlocked"
            } else {
                "Ready"
            }
            .to_owned(),
        ),
        PasswordManagerState::Locked => (Status::Info, said("Locked")),
        PasswordManagerState::SignedOut => (Status::Warning, said("Not signed in")),
        PasswordManagerState::Unavailable => (Status::Error, said("Unavailable")),
    }
}

/// A source's state beside its name in the picker's list (`stateLabel`).
pub fn state_label(state: PasswordManagerState) -> &'static str {
    match state {
        PasswordManagerState::Ready => "",
        PasswordManagerState::Locked => "Locked",
        PasswordManagerState::SignedOut => "Not signed in",
        PasswordManagerState::Unavailable => "Unavailable",
    }
}

/// The line under a source's name in Settings: its kind, and its file or account.
pub fn source_caption(source: &PasswordManagerSource) -> String {
    let place = source
        .config
        .path
        .as_deref()
        .or(source.config.account.as_deref())
        .filter(|place| !place.is_empty());
    match place {
        Some(place) => format!("{} · {place}", source.kind.label()),
        None => source.kind.label().to_owned(),
    }
}

/// The line under an entry's title in the picker: its user name and address.
pub fn entry_caption(entry: &PasswordManagerEntry) -> Option<String> {
    let parts: Vec<&str> = [entry.username.as_deref(), entry.url.as_deref()]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Entries whose title, user name, address or group holds every search word.
pub fn filter_entries(entries: &[PasswordManagerEntry], query: &str) -> Vec<PasswordManagerEntry> {
    let lowered = query.to_lowercase();
    let words: Vec<&str> = lowered.split_whitespace().collect();
    if words.is_empty() {
        return entries.to_vec();
    }
    entries
        .iter()
        .filter(|entry| {
            let text = [
                Some(entry.title.as_str()),
                entry.username.as_deref(),
                entry.url.as_deref(),
                entry.group.as_deref(),
            ]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
            words.iter().all(|word| text.contains(word))
        })
        .cloned()
        .collect()
}

/// What the entry is for, offered as a search while nothing is typed and it finds something.
pub fn offered_suggestion(
    entries: &[PasswordManagerEntry],
    suggestion: Option<&str>,
    query: &str,
) -> Option<String> {
    let suggested = js_trim(suggestion?);
    (!suggested.is_empty() && query.is_empty() && !filter_entries(entries, suggested).is_empty())
        .then(|| suggested.to_owned())
}

/// The reference a pick stores: where the entry is, and what it was called.
pub fn entry_reference(source_id: &str, entry: &PasswordManagerEntry) -> ExternalPasswordRef {
    ExternalPasswordRef {
        source_id: source_id.to_owned(),
        entry_id: entry.id.clone(),
        title: entry.title.clone(),
        username: entry.username.clone().filter(|name| !name.is_empty()),
    }
}

/// The source the picker shows: the chosen one while it is still connected, else the first.
pub fn shown_source<'a>(
    sources: &'a [PasswordManagerSource],
    chosen: Option<&str>,
) -> Option<&'a PasswordManagerSource> {
    sources
        .iter()
        .find(|source| Some(source.id.as_str()) == chosen)
        .or_else(|| sources.first())
}

/// What the picker says where the list would be.
pub fn empty_entries_text(query: &str) -> &'static str {
    if query.is_empty() {
        "This password manager has no entries with a password."
    } else {
        "No entry matches."
    }
}

/// The line under a linked entry: where its password comes from. `sources` is `None` until
/// the list has loaded: a slow answer is not a missing manager. The flag says the entry's
/// manager is gone.
pub fn linked_entry_caption(
    entry: &ExternalPasswordRef,
    sources: Option<&[PasswordManagerSource]>,
) -> (String, bool) {
    let source =
        sources.and_then(|sources| sources.iter().find(|source| source.id == entry.source_id));
    match (sources, source) {
        (Some(_), None) => (
            "Its password manager is no longer connected.".to_owned(),
            true,
        ),
        (_, Some(source)) => (format!("Password from {}", source.name), false),
        (None, None) => ("Password from a password manager".to_owned(), false),
    }
}

/// A linked entry's title as the field shows it.
pub fn linked_entry_title(entry: &ExternalPasswordRef) -> &str {
    if entry.title.is_empty() {
        "Untitled entry"
    } else {
        &entry.title
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        id: &str,
        title: &str,
        username: Option<&str>,
        url: Option<&str>,
        group: Option<&str>,
    ) -> PasswordManagerEntry {
        PasswordManagerEntry {
            id: id.into(),
            title: title.into(),
            username: username.map(str::to_owned),
            url: url.map(str::to_owned),
            group: group.map(str::to_owned),
        }
    }

    fn entries() -> Vec<PasswordManagerEntry> {
        vec![
            entry(
                "1",
                "Core router",
                Some("admin"),
                None,
                Some("Network / Core"),
            ),
            entry(
                "2",
                "web1",
                Some("root"),
                Some("ssh://web1.example.com"),
                Some("Servers"),
            ),
            entry(
                "3",
                "WEB2",
                Some("deploy"),
                Some("ssh://web2.example.com"),
                Some("Servers"),
            ),
        ]
    }

    fn ids(query: &str) -> Vec<String> {
        filter_entries(&entries(), query)
            .into_iter()
            .map(|entry| entry.id)
            .collect()
    }

    fn source(kind: PasswordManagerKind, state: PasswordManagerState) -> PasswordManagerSource {
        PasswordManagerSource {
            id: "source-1".into(),
            kind,
            name: "Work".into(),
            config: PasswordManagerConfig::default(),
            state,
            message: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    // --- tests/unit/client/password-entry-picker.test.ts ---

    #[test]
    fn returns_everything_for_an_empty_search() {
        assert_eq!(ids(""), ["1", "2", "3"]);
        assert_eq!(ids("   "), ["1", "2", "3"]);
    }

    #[test]
    fn matches_title_user_name_address_and_group_without_regard_to_case() {
        assert_eq!(ids("web"), ["2", "3"]);
        assert_eq!(ids("ADMIN"), ["1"]);
        assert_eq!(ids("example.com"), ["2", "3"]);
        assert_eq!(ids("network"), ["1"]);
    }

    #[test]
    fn needs_every_word_in_any_field() {
        assert_eq!(ids("servers root"), ["2"]);
        assert_eq!(ids("web deploy"), ["3"]);
        assert!(ids("router root").is_empty());
    }

    // --- the views' texts ---

    #[test]
    fn offers_what_the_entry_is_for_as_a_search_only_when_it_finds_something() {
        let entries = entries();
        assert_eq!(
            offered_suggestion(&entries, Some(" web1 "), "").as_deref(),
            Some("web1")
        );
        assert_eq!(offered_suggestion(&entries, Some("web1"), "w"), None);
        assert_eq!(offered_suggestion(&entries, Some("mail"), ""), None);
        assert_eq!(offered_suggestion(&entries, Some("  "), ""), None);
        assert_eq!(offered_suggestion(&entries, None, ""), None);
    }

    #[test]
    fn stores_where_a_picked_entry_is_and_what_it_was_called() {
        let picked = entry_reference("source-1", &entries()[1]);
        assert_eq!(
            picked,
            ExternalPasswordRef {
                source_id: "source-1".into(),
                entry_id: "2".into(),
                title: "web1".into(),
                username: Some("root".into()),
            }
        );
        let nameless = entry_reference("source-1", &entry("4", "wifi", Some(""), None, None));
        assert_eq!(nameless.username, None);
        assert_eq!(
            entry_caption(&entries()[1]).as_deref(),
            Some("root · ssh://web1.example.com")
        );
        assert_eq!(entry_caption(&entries()[0]).as_deref(), Some("admin"));
        assert_eq!(entry_caption(&entry("4", "wifi", None, None, None)), None);
    }

    #[test]
    fn says_where_each_manager_stands() {
        let keepass = source(PasswordManagerKind::Keepass, PasswordManagerState::Ready);
        assert_eq!(source_state(&keepass), (Status::Success, "Unlocked".into()));
        let onepassword = source(
            PasswordManagerKind::Onepassword,
            PasswordManagerState::Ready,
        );
        assert_eq!(
            source_state(&onepassword),
            (Status::Success, "Ready".into())
        );
        let locked = PasswordManagerSource {
            message: Some("Enter the database password to unlock it.".into()),
            ..source(PasswordManagerKind::Keepass, PasswordManagerState::Locked)
        };
        assert_eq!(
            source_state(&locked),
            (
                Status::Info,
                "Enter the database password to unlock it.".into()
            )
        );
        assert_eq!(
            source_state(&source(
                PasswordManagerKind::Bitwarden,
                PasswordManagerState::SignedOut
            )),
            (Status::Warning, "Not signed in".into())
        );
        assert_eq!(
            source_state(&source(
                PasswordManagerKind::Pass,
                PasswordManagerState::Unavailable
            )),
            (Status::Error, "Unavailable".into())
        );
        assert_eq!(state_label(PasswordManagerState::Ready), "");
        assert_eq!(state_label(PasswordManagerState::Locked), "Locked");
    }

    #[test]
    fn unlocks_keepass_and_bitwarden_only_and_names_their_passwords() {
        assert!(unlocks_in_muxus(PasswordManagerKind::Keepass));
        assert!(unlocks_in_muxus(PasswordManagerKind::Bitwarden));
        assert!(!unlocks_in_muxus(PasswordManagerKind::Onepassword));
        assert!(!unlocks_in_muxus(PasswordManagerKind::Pass));
        assert_eq!(
            unlock_password_label(PasswordManagerKind::Keepass),
            "Database password"
        );
        assert_eq!(
            unlock_password_label(PasswordManagerKind::Bitwarden),
            "Bitwarden master password"
        );
        let mut keyed = source(PasswordManagerKind::Keepass, PasswordManagerState::Locked);
        assert!(!unlock_password_optional(&keyed));
        keyed.config.key_file = Some("~/work.keyx".into());
        assert!(unlock_password_optional(&keyed));
        assert_eq!(
            unlock_helper(&keyed),
            "Unlocks Work until you lock it or quit Muxus."
        );
    }

    #[test]
    fn describes_a_source_by_its_kind_and_where_it_is() {
        let mut keepass = source(PasswordManagerKind::Keepass, PasswordManagerState::Locked);
        keepass.config.path = Some("~/Passwords.kdbx".into());
        assert_eq!(
            source_caption(&keepass),
            "KeePass / KeePassXC · ~/Passwords.kdbx"
        );
        let mut onepassword = source(
            PasswordManagerKind::Onepassword,
            PasswordManagerState::Ready,
        );
        onepassword.config.account = Some("my.1password.eu".into());
        assert_eq!(source_caption(&onepassword), "1Password · my.1password.eu");
        assert_eq!(
            source_caption(&source(
                PasswordManagerKind::Bitwarden,
                PasswordManagerState::Ready
            )),
            "Bitwarden"
        );
    }

    #[test]
    fn asks_for_what_a_kind_cannot_do_without() {
        let installed = [PasswordManagerKind::Keepass, PasswordManagerKind::Pass];
        assert_eq!(
            kind_choice_label(PasswordManagerKind::Keepass, &installed),
            "KeePass / KeePassXC"
        );
        assert_eq!(
            kind_choice_label(PasswordManagerKind::Bitwarden, &installed),
            "Bitwarden   bw not found"
        );
        assert_eq!(
            kind_choice_label(PasswordManagerKind::Pass, &installed),
            "pass / gopass"
        );
        assert!(missing_program(
            PasswordManagerKind::Bitwarden,
            &installed,
            "  "
        ));
        assert!(!missing_program(
            PasswordManagerKind::Bitwarden,
            &installed,
            "/opt/bw"
        ));
        assert!(!missing_program(PasswordManagerKind::Pass, &installed, ""));
        assert!(!missing_program(PasswordManagerKind::Keepass, &[], ""));
        assert_eq!(
            program_helper(PasswordManagerKind::Bitwarden, true),
            "bw was not found on this computer. Install it, or enter where it is."
        );
        assert_eq!(
            program_helper(PasswordManagerKind::Bitwarden, false),
            "Leave empty to find it on PATH."
        );

        let mut config = PasswordManagerConfig::default();
        assert!(incomplete(PasswordManagerKind::Keepass, &config));
        assert!(!incomplete(PasswordManagerKind::Bitwarden, &config));
        config.path = Some("   ".into());
        assert!(incomplete(PasswordManagerKind::Keepass, &config));
        config.path = Some("~/Passwords.kdbx".into());
        assert!(!incomplete(PasswordManagerKind::Keepass, &config));
        let input = manager_input(PasswordManagerKind::Keepass, "  ", &config);
        assert_eq!(input.name, None);
        assert_eq!(
            manager_input(PasswordManagerKind::Keepass, " Work ", &config)
                .name
                .as_deref(),
            Some("Work")
        );
        // Every kind says what it needs.
        for kind in muxus_core::api::PASSWORD_MANAGER_KINDS {
            assert!(!kind_note(kind).is_empty());
        }
    }

    #[test]
    fn names_the_manager_a_linked_entry_comes_from_or_says_it_is_gone() {
        let linked = ExternalPasswordRef {
            source_id: "source-1".into(),
            entry_id: "2".into(),
            title: "web1".into(),
            username: Some("root".into()),
        };
        let sources = [source(
            PasswordManagerKind::Keepass,
            PasswordManagerState::Locked,
        )];
        assert_eq!(
            linked_entry_caption(&linked, Some(&sources)),
            ("Password from Work".into(), false)
        );
        assert_eq!(
            linked_entry_caption(&linked, Some(&[])),
            ("Its password manager is no longer connected.".into(), true)
        );
        // Not known yet: a slow answer is not a missing manager.
        assert_eq!(
            linked_entry_caption(&linked, None),
            ("Password from a password manager".into(), false)
        );
        assert_eq!(linked_entry_title(&linked), "web1");
        assert_eq!(
            linked_entry_title(&ExternalPasswordRef {
                title: String::new(),
                ..linked
            }),
            "Untitled entry"
        );
    }

    #[test]
    fn shows_the_chosen_manager_while_it_is_connected_else_the_first() {
        let first = source(PasswordManagerKind::Keepass, PasswordManagerState::Locked);
        let second = PasswordManagerSource {
            id: "source-2".into(),
            ..source(PasswordManagerKind::Bitwarden, PasswordManagerState::Ready)
        };
        let sources = [first, second];
        assert_eq!(
            shown_source(&sources, Some("source-2")).unwrap().id,
            "source-2"
        );
        assert_eq!(shown_source(&sources, Some("gone")).unwrap().id, "source-1");
        assert_eq!(shown_source(&sources, None).unwrap().id, "source-1");
        assert!(shown_source(&[], Some("source-1")).is_none());
        assert_eq!(empty_entries_text("x"), "No entry matches.");
        assert_eq!(
            empty_entries_text(""),
            "This password manager has no entries with a password."
        );
    }
}
