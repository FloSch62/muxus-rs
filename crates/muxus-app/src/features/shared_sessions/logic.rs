//! What the shared sessions views say (`api/shared-sessions.ts` and the texts of the
//! dialogs), apart from any view.

use muxus_core::api::{SharedSessionApplyResult, SharedSessionSource};
use muxus_ui::theme::Status;

use crate::features::launcher::time_format::format_timestamp;

/// Settings → Backup & data: what the group is about.
pub const GROUP_DESCRIPTION: &str = "Host lists you follow from a link or a shared drive. To publish one, right-click a folder in the sidebar and choose Share folder.";

/// Under the fields of the dialog that follows a link.
pub const FOLLOW_NOTE: &str =
    "Hosts are added, updated and removed with the list. What you set on a host yourself stays.";
pub const LINK_PLACEHOLDER: &str = "https://portal.example.com/sessions/you.mxtsessions";
pub const LINK_HELPER: &str = "A web address, or the path of a file such as one on a shared drive.";
pub const FOLDER_HELPER: &str = "Use / to nest, e.g. Teams/Lab.";

/// What to do with a list once it is saved.
pub const SHARE_STEPS: [&str; 3] = [
    "Put the file on a web server, in a Git repository or on a shared drive.",
    "Others choose + → Shared sessions link… and enter its address or path.",
    "To update the list, share again and replace the file.",
];

/// How often a link is checked, by default.
pub const DEFAULT_REFRESH_MINUTES: u32 = 60;

/// One choice of "Check for changes".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefreshChoice {
    pub minutes: u32,
    pub label: String,
}

/// The usual intervals: minutes, the choice's label, and how a row puts it.
const REFRESH_CHOICES: [(u32, &str, &str); 4] = [
    (15, "Every 15 minutes", "every 15 minutes"),
    (60, "Every hour", "every hour"),
    (24 * 60, "Every day", "every day"),
    (0, "Only when I refresh it", "on request"),
];

/// The usual intervals, plus the link's own when it was set to something else.
pub fn refresh_choices(minutes: u32) -> Vec<RefreshChoice> {
    let mut choices: Vec<RefreshChoice> = REFRESH_CHOICES
        .iter()
        .map(|(minutes, label, _)| RefreshChoice {
            minutes: *minutes,
            label: (*label).to_owned(),
        })
        .collect();
    if !choices.iter().any(|choice| choice.minutes == minutes) {
        choices.push(RefreshChoice {
            minutes,
            label: format!("Every {minutes} minutes"),
        });
    }
    choices
}

/// "every hour", "on request", "every 45 minutes".
pub fn refresh_choice_short(minutes: u32) -> String {
    REFRESH_CHOICES
        .iter()
        .find(|(choice, _, _)| *choice == minutes)
        .map(|(_, _, short)| (*short).to_owned())
        .unwrap_or_else(|| format!("every {minutes} minutes"))
}

pub fn shared_host_count(count: u64) -> String {
    format!("{count} {}", if count == 1 { "host" } else { "hosts" })
}

/// Where a link points, without the part that is often personal: a server name or a file
/// name.
pub fn shared_sessions_origin(url: &str) -> String {
    for scheme in ["https://", "http://"] {
        let Some(rest) = url
            .get(..scheme.len())
            .filter(|start| start.eq_ignore_ascii_case(scheme))
            .map(|_| &url[scheme.len()..])
        else {
            continue;
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        // A link is stored without a user name; one that has it anyway is not repeated.
        let host = authority.rsplit('@').next().unwrap_or_default();
        return if host.is_empty() {
            "link".to_owned()
        } else {
            host.to_owned()
        };
    }
    url.split(['/', '\\'])
        .rfind(|part| !part.is_empty())
        .unwrap_or("file")
        .to_owned()
}

/// How a link is doing, in the words its row and its folder both use.
pub fn shared_sessions_state(source: &SharedSessionSource) -> (Status, String) {
    if let Some(error) = source
        .last_error
        .as_deref()
        .filter(|error| !error.is_empty())
    {
        return (Status::Error, error.to_owned());
    }
    match source.last_synced_at.as_deref() {
        Some(at) => (
            Status::Success,
            format!("Up to date · {}", format_timestamp(at)),
        ),
        None => (Status::Info, "Not read yet".to_owned()),
    }
}

/// "3 added · 1 removed", or nothing when a check changed no host.
pub fn shared_session_changes(result: &SharedSessionApplyResult) -> String {
    [
        (result.added, "added"),
        (result.updated, "updated"),
        (result.removed, "removed"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, what)| format!("{count} {what}"))
    .collect::<Vec<_>>()
    .join(" · ")
}

/// What a check the user asked for says when it is done.
pub fn refreshed_message(name: &str, result: Option<&SharedSessionApplyResult>) -> String {
    let changes = result.map(shared_session_changes).unwrap_or_default();
    format!(
        "{name}: {}.",
        if changes.is_empty() {
            "already up to date"
        } else {
            &changes
        }
    )
}

/// What a check the user asked for says when it failed.
pub fn refresh_failed_message(name: &str, error: &str) -> String {
    format!(
        "{name}: {}",
        if error.is_empty() {
            "the shared sessions could not be read."
        } else {
            error
        }
    )
}

/// The line under a link's name in Settings.
pub fn source_caption(source: &SharedSessionSource) -> String {
    [
        shared_sessions_origin(&source.url),
        format!(
            "{} in “{}”",
            shared_host_count(source.host_count),
            source.folder
        ),
        format!("checked {}", refresh_choice_short(source.refresh_minutes)),
    ]
    .join(" · ")
}

/// What the folder of a link says on hover, and whether its last check failed.
pub fn folder_link_tooltip(source: &SharedSessionSource) -> (String, bool) {
    let (tone, text) = shared_sessions_state(source);
    if tone == Status::Error {
        (format!("Shared sessions not updated: {text}"), true)
    } else {
        (
            format!(
                "Shared sessions from {} · {text}",
                shared_sessions_origin(&source.url)
            ),
            false,
        )
    }
}

/// The caption under a host editor's title when a shared sessions link keeps that host up
/// to date. `name` is the link's, when it is known.
pub fn shared_host_caption(name: Option<&str>) -> String {
    let link = match name {
        Some(name) => format!("“{name}”"),
        None => "a shared sessions link".to_owned(),
    };
    format!("Follows {link} — your changes stay unless the list changes the same setting")
}

pub fn stop_following_title(source: &SharedSessionSource) -> String {
    format!("Stop following “{}”?", source.name)
}

/// What happens to the hosts; nothing to say for a link that has none.
pub fn stop_following_description(source: &SharedSessionSource) -> Option<String> {
    (source.host_count > 0).then(|| {
        format!(
            "Its {} in “{}” are deleted.",
            shared_host_count(source.host_count),
            source.folder
        )
    })
}

pub fn keep_hosts_label(source: &SharedSessionSource) -> Option<String> {
    (source.host_count > 0).then(|| {
        format!(
            "Keep the {} as hosts of my own",
            shared_host_count(source.host_count)
        )
    })
}

/// What sharing a folder writes, in a sentence.
pub fn share_summary(host_count: usize, jump_host_count: usize) -> String {
    let through = match jump_host_count {
        0 => String::new(),
        1 => ", and the jump host they connect through,".to_owned(),
        count => format!(", and the {count} jump hosts they connect through,"),
    };
    format!(
        "Saves {}{through} as a list others can follow. No passwords or private keys.",
        shared_host_count(host_count as u64)
    )
}

/// Whether the dialog that follows a link has what it needs.
pub fn link_ready(url: &str, name: &str, folder: &str) -> bool {
    use muxus_core::js::js_trim;
    !js_trim(url).is_empty() && !js_trim(name).is_empty() && !js_trim(folder).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> SharedSessionSource {
        SharedSessionSource {
            id: "lab".into(),
            name: "Lab hosts".into(),
            url: "https://portal.example.net/personal-token/me.mxtsessions".into(),
            folder: "Teams/Lab".into(),
            refresh_minutes: 60,
            host_count: 12,
            last_checked_at: None,
            last_synced_at: None,
            last_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn result(added: u64, updated: u64, removed: u64) -> SharedSessionApplyResult {
        SharedSessionApplyResult {
            source: source(),
            added,
            updated,
            removed,
            unchanged: 5,
        }
    }

    #[test]
    fn names_a_link_by_its_server_or_its_file() {
        assert_eq!(
            shared_sessions_origin("https://portal.example.net/personal-token/me.mxtsessions"),
            "portal.example.net"
        );
        assert_eq!(
            shared_sessions_origin("HTTP://127.0.0.1:8080/list?token=abc"),
            "127.0.0.1:8080"
        );
        assert_eq!(shared_sessions_origin("https://"), "link");
        assert_eq!(
            shared_sessions_origin("/mnt/share/team/lab.muxus-sessions.json"),
            "lab.muxus-sessions.json"
        );
        assert_eq!(
            shared_sessions_origin(r"\\server\share\lab.mxtsessions"),
            "lab.mxtsessions"
        );
        assert_eq!(shared_sessions_origin(r"C:\Lists\lab.json"), "lab.json");
        assert_eq!(shared_sessions_origin("/"), "file");
    }

    #[test]
    fn says_how_a_link_is_doing() {
        let mut link = source();
        assert_eq!(
            shared_sessions_state(&link),
            (Status::Info, "Not read yet".into())
        );
        link.last_synced_at = Some("2026-11-20T08:00:00.000Z".into());
        let (tone, text) = shared_sessions_state(&link);
        assert_eq!(tone, Status::Success);
        assert!(text.starts_with("Up to date · "), "{text}");
        assert_eq!(
            folder_link_tooltip(&link),
            (
                format!("Shared sessions from portal.example.net · {text}"),
                false
            )
        );
        // A failed check says why, whatever was read before.
        link.last_error = Some("portal.example.net answered 403 Forbidden.".into());
        assert_eq!(
            shared_sessions_state(&link),
            (
                Status::Error,
                "portal.example.net answered 403 Forbidden.".into()
            )
        );
        assert_eq!(
            folder_link_tooltip(&link),
            (
                "Shared sessions not updated: portal.example.net answered 403 Forbidden.".into(),
                true
            )
        );
    }

    #[test]
    fn sums_up_what_a_check_changed() {
        assert_eq!(
            shared_session_changes(&result(3, 0, 1)),
            "3 added · 1 removed"
        );
        assert_eq!(
            shared_session_changes(&result(1, 2, 3)),
            "1 added · 2 updated · 3 removed"
        );
        assert_eq!(shared_session_changes(&result(0, 0, 0)), "");
        assert_eq!(
            refreshed_message("Lab hosts", Some(&result(2, 0, 0))),
            "Lab hosts: 2 added."
        );
        assert_eq!(
            refreshed_message("Lab hosts", Some(&result(0, 0, 0))),
            "Lab hosts: already up to date."
        );
        // Another window read the link a moment ago.
        assert_eq!(
            refreshed_message("Lab hosts", None),
            "Lab hosts: already up to date."
        );
        assert_eq!(
            refresh_failed_message("Lab hosts", "The link returned nothing."),
            "Lab hosts: The link returned nothing."
        );
        assert_eq!(
            refresh_failed_message("Lab hosts", ""),
            "Lab hosts: the shared sessions could not be read."
        );
    }

    #[test]
    fn offers_the_usual_intervals_and_the_links_own() {
        let labels = |minutes| -> Vec<String> {
            refresh_choices(minutes)
                .into_iter()
                .map(|choice| choice.label)
                .collect()
        };
        assert_eq!(
            labels(60),
            [
                "Every 15 minutes",
                "Every hour",
                "Every day",
                "Only when I refresh it"
            ]
        );
        assert_eq!(
            labels(45).last().map(String::as_str),
            Some("Every 45 minutes")
        );
        assert_eq!(refresh_choice_short(15), "every 15 minutes");
        assert_eq!(refresh_choice_short(1440), "every day");
        assert_eq!(refresh_choice_short(0), "on request");
        assert_eq!(refresh_choice_short(45), "every 45 minutes");
    }

    #[test]
    fn describes_a_link_in_one_line() {
        assert_eq!(
            source_caption(&source()),
            "portal.example.net · 12 hosts in “Teams/Lab” · checked every hour"
        );
        let mut one = source();
        one.host_count = 1;
        one.refresh_minutes = 0;
        one.url = "/mnt/share/lab.json".into();
        assert_eq!(
            source_caption(&one),
            "lab.json · 1 host in “Teams/Lab” · checked on request"
        );
    }

    #[test]
    fn asks_what_happens_to_the_hosts_only_when_there_are_some() {
        let link = source();
        assert_eq!(stop_following_title(&link), "Stop following “Lab hosts”?");
        assert_eq!(
            stop_following_description(&link).as_deref(),
            Some("Its 12 hosts in “Teams/Lab” are deleted.")
        );
        assert_eq!(
            keep_hosts_label(&link).as_deref(),
            Some("Keep the 12 hosts as hosts of my own")
        );
        let mut empty = source();
        empty.host_count = 0;
        assert_eq!(stop_following_description(&empty), None);
        assert_eq!(keep_hosts_label(&empty), None);
    }

    #[test]
    fn says_what_a_shared_folder_holds() {
        assert_eq!(
            share_summary(4, 0),
            "Saves 4 hosts as a list others can follow. No passwords or private keys."
        );
        assert_eq!(
            share_summary(1, 1),
            "Saves 1 host, and the jump host they connect through, as a list others can follow. No passwords or private keys."
        );
        assert_eq!(
            share_summary(4, 2),
            "Saves 4 hosts, and the 2 jump hosts they connect through, as a list others can follow. No passwords or private keys."
        );
    }

    #[test]
    fn captions_a_host_a_link_keeps_up_to_date() {
        assert_eq!(
            shared_host_caption(Some("Lab hosts")),
            "Follows “Lab hosts” — your changes stay unless the list changes the same setting"
        );
        assert_eq!(
            shared_host_caption(None),
            "Follows a shared sessions link — your changes stay unless the list changes the same setting"
        );
    }

    #[test]
    fn needs_a_link_a_name_and_a_folder() {
        assert!(link_ready("https://x.example/list", "Lab", "Lab"));
        assert!(!link_ready("  ", "Lab", "Lab"));
        assert!(!link_ready("https://x.example/list", " ", "Lab"));
        assert!(!link_ready("https://x.example/list", "Lab", ""));
    }
}
