//! Named vault secrets as the settings and the command buttons see them
//! (`client/src/vault-secrets.ts`, the parts that need no live terminal).

use muxus_core::api::{PasswordVaultSecret, PasswordVaultStatus};
use muxus_store::prefs::command_buttons::CommandButton;

use crate::state::tabs::{TerminalTab, is_multi_exec_target};

pub fn find_vault_secret<'a>(
    status: Option<&'a PasswordVaultStatus>,
    secret_id: &str,
) -> Option<&'a PasswordVaultSecret> {
    status?.secrets.iter().find(|secret| secret.id == secret_id)
}

/// Whether a saved reference has lost its secret. Unknown (`None`) until
/// the vault status has loaded, so nothing is marked missing prematurely.
pub fn vault_secret_missing(status: Option<&PasswordVaultStatus>, secret_id: &str) -> Option<bool> {
    let status = status?;
    Some(find_vault_secret(Some(status), secret_id).is_none())
}

/// Saved commands that type this secret, which lose it when it is deleted.
pub fn command_buttons_using_secret<'a>(
    buttons: &'a [CommandButton],
    secret_id: &str,
) -> Vec<&'a CommandButton> {
    buttons
        .iter()
        .filter(|button| button.secret_id.as_deref() == Some(secret_id))
        .collect()
}

/// Match every search word against a secret's name or user name.
pub fn filter_vault_secrets<'a>(
    secrets: &'a [PasswordVaultSecret],
    query: &str,
) -> Vec<&'a PasswordVaultSecret> {
    let query = muxus_core::js::js_trim(query).to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return secrets.iter().collect();
    }
    secrets
        .iter()
        .filter(|secret| {
            let searchable = format!(
                "{} {}",
                secret.name,
                secret.username.as_deref().unwrap_or_default()
            )
            .to_lowercase();
            words.iter().all(|word| searchable.contains(word))
        })
        .collect()
}

/// Backend terminal ids a secret typed into `tab_id` goes to: that session,
/// then every connected session multi-execution mirrors it into.
pub fn secret_terminal_ids(
    tab_id: &str,
    tabs: &[TerminalTab],
    mirror_tab_ids: &[String],
) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for id in std::iter::once(tab_id).chain(mirror_tab_ids.iter().map(String::as_str)) {
        let tab = tabs.iter().find(|tab| tab.id == id);
        let terminal_id = tab
            .filter(|tab| is_multi_exec_target(tab))
            .and_then(|tab| tab.terminal_id.clone());
        match terminal_id {
            Some(terminal_id) => {
                if !ids.contains(&terminal_id) {
                    ids.push(terminal_id);
                }
            }
            None if id == tab_id => return Vec::new(),
            None => {}
        }
    }
    ids
}

/// `quotedList`: “a”, “a” and “b”, or “a”, “b”, “c” and 2 more.
pub fn quoted_list(labels: &[String]) -> String {
    let quoted: Vec<String> = labels
        .iter()
        .take(3)
        .map(|label| format!("\u{201c}{label}\u{201d}"))
        .collect();
    let rest = labels.len() - quoted.len();
    if rest > 0 {
        return format!("{} and {rest} more", quoted.join(", "));
    }
    match quoted.split_last() {
        None => String::new(),
        Some((only, [])) => only.clone(),
        Some((last, head)) => format!("{} and {last}", head.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tabs::{IdGen, TabPatch, TabStatus, TabsState};

    fn secrets() -> Vec<PasswordVaultSecret> {
        vec![
            PasswordVaultSecret {
                id: "s1".into(),
                name: "Core enable".into(),
                username: None,
                created_at: String::new(),
                updated_at: String::new(),
            },
            PasswordVaultSecret {
                id: "s2".into(),
                name: "Lab sudo".into(),
                username: Some("admin".into()),
                created_at: String::new(),
                updated_at: String::new(),
            },
        ]
    }

    fn status() -> PasswordVaultStatus {
        PasswordVaultStatus {
            configured: true,
            unlock_policy: None,
            locked: false,
            os_key_store_available: true,
            credential_count: 0,
            credentials: Vec::new(),
            secrets: secrets(),
        }
    }

    #[test]
    fn reports_a_secret_as_missing_only_once_the_vault_status_is_known() {
        let status = status();
        assert_eq!(
            find_vault_secret(Some(&status), "s2").map(|s| s.name.as_str()),
            Some("Lab sudo")
        );
        assert_eq!(vault_secret_missing(None, "s1"), None);
        assert_eq!(vault_secret_missing(Some(&status), "s1"), Some(false));
        assert_eq!(vault_secret_missing(Some(&status), "deleted"), Some(true));
    }

    fn button(id: &str, secret: Option<&str>) -> CommandButton {
        CommandButton {
            id: id.into(),
            label: id.into(),
            command: String::new(),
            send_enter: true,
            group_id: None,
            color: None,
            secret_id: secret.map(Into::into),
        }
    }

    #[test]
    fn finds_the_saved_commands_that_still_type_a_secret() {
        let buttons = [
            button("a", Some("s1")),
            button("b", None),
            button("c", Some("s1")),
        ];
        let ids: Vec<&str> = command_buttons_using_secret(&buttons, "s1")
            .iter()
            .map(|button| button.id.as_str())
            .collect();
        assert_eq!(ids, ["a", "c"]);
        assert!(command_buttons_using_secret(&buttons, "s2").is_empty());
    }

    #[test]
    fn searches_secrets_by_name_and_user_name() {
        let secrets = secrets();
        let names = |found: Vec<&PasswordVaultSecret>| {
            found
                .iter()
                .map(|secret| secret.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(filter_vault_secrets(&secrets, "ADMIN")), ["s2"]);
        assert_eq!(names(filter_vault_secrets(&secrets, "core enable")), ["s1"]);
        assert_eq!(names(filter_vault_secrets(&secrets, " ")), ["s1", "s2"]);
    }

    #[test]
    fn goes_to_the_focused_session_and_the_connected_sessions_mirroring_it() {
        let mut tabs = TabsState::with_pane("pane-test", IdGen::sequential());
        let mut connect = |title: &str, terminal: &str| {
            let id = tabs.open(crate::model::local_profile(), title);
            tabs.update(
                &id,
                TabPatch {
                    status: Some(TabStatus::Connected),
                    terminal_id: Some(Some(terminal.into())),
                    ..TabPatch::default()
                },
            );
            id
        };
        let first = connect("one", "terminal-1");
        let second = connect("two", "terminal-2");
        let third = connect("three", "terminal-3");
        tabs.update(
            &third,
            TabPatch {
                status: Some(TabStatus::Closed),
                ..TabPatch::default()
            },
        );
        let all = tabs.tabs.clone();
        assert_eq!(secret_terminal_ids(&first, &all, &[]), ["terminal-1"]);
        assert_eq!(
            secret_terminal_ids(
                &first,
                &all,
                &[second.clone(), third.clone(), "gone".into()]
            ),
            ["terminal-1", "terminal-2"]
        );
        // Nothing is typed anywhere when the focused session itself is not connected.
        assert!(secret_terminal_ids(&third, &all, &[first, second]).is_empty());
    }

    #[test]
    fn quotes_lists_like_the_confirmations() {
        let labels = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(quoted_list(&labels(&[])), "");
        assert_eq!(quoted_list(&labels(&["a"])), "\u{201c}a\u{201d}");
        assert_eq!(
            quoted_list(&labels(&["a", "b"])),
            "\u{201c}a\u{201d} and \u{201c}b\u{201d}"
        );
        assert_eq!(
            quoted_list(&labels(&["a", "b", "c", "d", "e"])),
            "\u{201c}a\u{201d}, \u{201c}b\u{201d}, \u{201c}c\u{201d} and 2 more"
        );
    }
}
