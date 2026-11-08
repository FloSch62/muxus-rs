//! What the Windows taskbar jump list and the macOS Dock menu offer (port of
//! `electron/src/launch-menu.ts`): a new window and the recent hosts and workspaces, each
//! opened again through the command line (`--host`, `--workspace`, `--new-window`).
//!
//! This is the menus' content; `launch_menu_os` hands it to the platform.

use muxus_core::api::{CommandLineLaunch, SavedHostProfile, SshHostEntry, WorkspaceSummary};
use muxus_import::locale_compare;
pub use muxus_os::jump_list::{JumpListCategory, JumpListTask};

/// electron-builder's appId, which the installer stamps on its shortcuts. The windows and the
/// jump list must carry it to belong to that taskbar entry.
pub const APP_USER_MODEL_ID: &str = "io.github.flosch62.muxus";

const RECENT_HOSTS: usize = 5;
const RECENT_WORKSPACES: usize = 3;
/// Windows leaves out an entry with a longer tooltip.
const DESCRIPTION_MAX_LENGTH: usize = 260;

/// A recently used host or workspace, and the launch that opens it again (`--host` or
/// `--workspace`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentLaunch {
    pub title: String,
    pub launch: CommandLineLaunch,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentLaunches {
    pub hosts: Vec<RecentLaunch>,
    pub workspaces: Vec<RecentLaunch>,
}

/// A host as the menus read it: the name `--host` takes, its title and its last connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostUse {
    pub name: String,
    pub title: String,
    pub used_at: Option<String>,
}

impl HostUse {
    /// ssh_config hosts launch by alias, the only name `--host` has for them.
    pub fn ssh(entry: &SshHostEntry) -> Self {
        let metadata = entry.metadata.as_ref();
        Self {
            name: entry.alias.clone(),
            title: metadata
                .and_then(|metadata| metadata.display_name.clone())
                .unwrap_or_else(|| entry.alias.clone()),
            used_at: metadata.and_then(|metadata| metadata.last_connected_at.clone()),
        }
    }

    /// Saved hosts launch by ID, so a rename keeps the entry working.
    pub fn saved(profile: &SavedHostProfile) -> Self {
        Self {
            name: profile.id.clone(),
            title: profile
                .metadata
                .display_name
                .clone()
                .unwrap_or_else(|| profile.name.clone()),
            used_at: profile.metadata.last_connected_at.clone(),
        }
    }
}

fn newest_first(mut entries: Vec<(String, RecentLaunch)>) -> Vec<RecentLaunch> {
    entries.sort_by(|(left_at, left), (right_at, right)| {
        right_at
            .cmp(left_at)
            .then_with(|| locale_compare(&left.title, &right.title))
    });
    let mut unique: Vec<RecentLaunch> = Vec::new();
    for (_, entry) in entries {
        // A host listed in several ssh_config blocks is still one entry.
        if !unique
            .iter()
            .any(|seen| launch_name(&seen.launch) == launch_name(&entry.launch))
        {
            unique.push(entry);
        }
    }
    unique
}

fn launch_name(launch: &CommandLineLaunch) -> Option<&str> {
    match launch {
        CommandLineLaunch::Host { name }
        | CommandLineLaunch::Folder { name }
        | CommandLineLaunch::Workspace { name }
        | CommandLineLaunch::Connect { name }
        | CommandLineLaunch::Url { name } => Some(name),
        CommandLineLaunch::NewWindow => None,
    }
}

/// Hosts by their last connection and workspaces by their last opening, or save, newest
/// first. Saved hosts and workspaces are launched by ID ([`HostUse::saved`]).
pub fn recent_launches(hosts: &[HostUse], workspaces: &[WorkspaceSummary]) -> RecentLaunches {
    RecentLaunches {
        hosts: newest_first(
            hosts
                .iter()
                .filter_map(|host| {
                    let used_at = host.used_at.clone().filter(|at| !at.is_empty())?;
                    Some((
                        used_at,
                        RecentLaunch {
                            title: host.title.clone(),
                            launch: CommandLineLaunch::Host {
                                name: host.name.clone(),
                            },
                        },
                    ))
                })
                .collect(),
        ),
        workspaces: newest_first(
            workspaces
                .iter()
                .map(|workspace| {
                    (
                        workspace
                            .last_opened_at
                            .clone()
                            .unwrap_or_else(|| workspace.updated_at.clone()),
                        RecentLaunch {
                            title: workspace.name.clone(),
                            launch: CommandLineLaunch::Workspace {
                                name: workspace.id.clone(),
                            },
                        },
                    )
                })
                .collect(),
        ),
    }
}

/// One argument, quoted so that `CommandLineToArgvW` reads it back unchanged.
pub fn windows_argument(value: &str) -> String {
    if !value.is_empty() && !value.chars().any(|c| c.is_whitespace() || c == '"') {
        return value.into();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in value.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Backslashes before a quote double, and the quote is escaped.
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    // Trailing backslashes double before the closing quote.
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

/// How a jump list entry starts Muxus: the executable, plus the app folder in a source checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpListLauncher {
    pub program: String,
    pub args: Vec<String>,
}

fn launch_flag(launch: &CommandLineLaunch) -> String {
    match launch {
        CommandLineLaunch::Host { name } => format!("--host={name}"),
        CommandLineLaunch::Workspace { name } => format!("--workspace={name}"),
        CommandLineLaunch::Folder { name } => format!("--folder={name}"),
        CommandLineLaunch::Connect { name } => format!("--connect={name}"),
        CommandLineLaunch::Url { name } => name.clone(),
        CommandLineLaunch::NewWindow => "--new-window".into(),
    }
}

/// Recent hosts and workspaces above the New window task. Entries the user removed
/// (`removed_args`) stay out until restart; Windows reports a removal once and hides a category
/// that brings it straight back.
pub fn jump_list_categories(
    recent: &RecentLaunches,
    launcher: &JumpListLauncher,
    removed_args: &std::collections::HashSet<String>,
) -> Vec<JumpListCategory> {
    let task = |title: &str, description: &str, flag: &str| JumpListTask {
        title: title.into(),
        description: description.chars().take(DESCRIPTION_MAX_LENGTH).collect(),
        program: launcher.program.clone(),
        args: launcher
            .args
            .iter()
            .map(String::as_str)
            .chain([flag])
            .map(windows_argument)
            .collect::<Vec<_>>()
            .join(" "),
        icon_path: launcher.program.clone(),
        icon_index: 0,
    };
    let recent_tasks = |entries: &[RecentLaunch], limit: usize, verb: &str| -> Vec<JumpListTask> {
        entries
            .iter()
            .map(|entry| {
                task(
                    &entry.title,
                    &format!("{verb} {}", entry.title),
                    &launch_flag(&entry.launch),
                )
            })
            .filter(|item| !removed_args.contains(&item.args))
            .take(limit)
            .collect()
    };
    [
        JumpListCategory::Custom {
            name: "Recent hosts".into(),
            items: recent_tasks(&recent.hosts, RECENT_HOSTS, "Connect to"),
        },
        JumpListCategory::Custom {
            name: "Recent workspaces".into(),
            items: recent_tasks(&recent.workspaces, RECENT_WORKSPACES, "Open workspace"),
        },
        JumpListCategory::Tasks {
            items: vec![task(
                "New window",
                "Open a new Muxus window",
                "--new-window",
            )],
        },
    ]
    .into_iter()
    .filter(|category| !category.items().is_empty())
    .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockMenuEntry {
    Launch {
        label: String,
        launch: CommandLineLaunch,
    },
    Heading {
        label: String,
    },
    Separator,
}

/// New Window, then the recent hosts and workspaces under a heading each.
pub fn dock_menu_entries(recent: &RecentLaunches) -> Vec<DockMenuEntry> {
    let section = |label: &str, entries: &[RecentLaunch]| -> Vec<DockMenuEntry> {
        if entries.is_empty() {
            return Vec::new();
        }
        [
            DockMenuEntry::Separator,
            DockMenuEntry::Heading {
                label: label.into(),
            },
        ]
        .into_iter()
        .chain(entries.iter().map(|entry| DockMenuEntry::Launch {
            label: entry.title.clone(),
            launch: entry.launch.clone(),
        }))
        .collect()
    };
    let mut entries = vec![DockMenuEntry::Launch {
        label: "New Window".into(),
        launch: CommandLineLaunch::NewWindow,
    }];
    let hosts = &recent.hosts[..recent.hosts.len().min(RECENT_HOSTS)];
    let workspaces = &recent.workspaces[..recent.workspaces.len().min(RECENT_WORKSPACES)];
    entries.extend(section("Recent Hosts", hosts));
    entries.extend(section("Recent Workspaces", workspaces));
    entries
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn at(minute: u32) -> Option<String> {
        Some(format!("2026-10-26 09:{minute:02}:00"))
    }

    fn launcher() -> JumpListLauncher {
        JumpListLauncher {
            program: "C:\\Program Files\\Muxus\\Muxus.exe".into(),
            args: Vec::new(),
        }
    }

    fn host_use(name: &str, title: &str, used_at: Option<String>) -> HostUse {
        HostUse {
            name: name.into(),
            title: title.into(),
            used_at,
        }
    }

    fn host(name: &str, title: &str) -> RecentLaunch {
        RecentLaunch {
            title: title.into(),
            launch: CommandLineLaunch::Host { name: name.into() },
        }
    }

    fn hosts(count: usize) -> Vec<RecentLaunch> {
        (1..=count)
            .map(|index| host(&format!("edge-{index}"), &format!("edge-{index}")))
            .collect()
    }

    fn workspaces(count: usize) -> Vec<RecentLaunch> {
        (1..=count)
            .map(|index| RecentLaunch {
                title: format!("Shift {index}"),
                launch: CommandLineLaunch::Workspace {
                    name: format!("ws-{index}"),
                },
            })
            .collect()
    }

    fn workspace(id: &str, name: &str, updated: u32, opened: Option<u32>) -> WorkspaceSummary {
        WorkspaceSummary {
            id: id.into(),
            name: name.into(),
            is_locked: false,
            is_startup: false,
            created_at: at(0).unwrap(),
            updated_at: at(updated).unwrap(),
            last_opened_at: opened.and_then(at),
        }
    }

    // ---- recent hosts and workspaces ----

    #[test]
    fn orders_hosts_from_both_sources_by_their_last_connection() {
        let recent = recent_launches(
            &[
                host_use("core-1", "core-1", at(10)),
                host_use("edge-1", "Edge Paris", at(30)),
                host_use("never-used", "Unused", None),
                host_use("saved-1", "Console 1", at(20)),
                host_use("saved-2", "Console 2", None),
            ],
            &[],
        );
        assert_eq!(
            recent.hosts,
            [
                host("edge-1", "Edge Paris"),
                host("saved-1", "Console 1"),
                host("core-1", "core-1"),
            ]
        );
    }

    #[test]
    fn lists_an_alias_from_several_ssh_config_blocks_once() {
        let block = host_use("edge-1", "edge-1", at(5));
        assert_eq!(recent_launches(&[block.clone(), block], &[]).hosts.len(), 1);
    }

    #[test]
    fn orders_workspaces_by_last_opening_else_last_change_and_opens_them_by_id() {
        let recent = recent_launches(
            &[],
            &[
                workspace("ws-old", "Old", 1, Some(2)),
                workspace("ws-saved", "Never opened", 40, None),
                workspace("ws-night", "Night shift", 3, Some(50)),
            ],
        );
        let launches: Vec<(&str, &CommandLineLaunch)> = recent
            .workspaces
            .iter()
            .map(|entry| (entry.title.as_str(), &entry.launch))
            .collect();
        let ws = |name: &str| CommandLineLaunch::Workspace { name: name.into() };
        assert_eq!(
            launches,
            [
                ("Night shift", &ws("ws-night")),
                ("Never opened", &ws("ws-saved")),
                ("Old", &ws("ws-old")),
            ]
        );
    }

    // ---- Windows command-line arguments ----

    #[test]
    fn quotes_only_what_command_line_to_argv_w_would_split_or_unquote() {
        assert_eq!(windows_argument("--host=edge-1"), "--host=edge-1");
        assert_eq!(windows_argument("--host=core sw 1"), "\"--host=core sw 1\"");
        assert_eq!(
            windows_argument("C:\\Users\\Jane Doe\\muxus\\electron"),
            "\"C:\\Users\\Jane Doe\\muxus\\electron\""
        );
        assert_eq!(windows_argument("a\"b"), "\"a\\\"b\"");
        assert_eq!(windows_argument("a\\\"b"), "\"a\\\\\\\"b\"");
        assert_eq!(windows_argument("C:\\my dir\\"), "\"C:\\my dir\\\\\"");
        assert_eq!(windows_argument(""), "\"\"");
    }

    // ---- taskbar jump list ----

    #[test]
    fn lists_recent_hosts_and_workspaces_above_the_new_window_task() {
        let categories = jump_list_categories(
            &RecentLaunches {
                hosts: hosts(1),
                workspaces: workspaces(1),
            },
            &launcher(),
            &HashSet::new(),
        );
        let entry = |title: &str, description: &str, args: &str| JumpListTask {
            title: title.into(),
            description: description.into(),
            program: launcher().program,
            args: args.into(),
            icon_path: launcher().program,
            icon_index: 0,
        };
        assert_eq!(
            categories,
            [
                JumpListCategory::Custom {
                    name: "Recent hosts".into(),
                    items: vec![entry("edge-1", "Connect to edge-1", "--host=edge-1")],
                },
                JumpListCategory::Custom {
                    name: "Recent workspaces".into(),
                    items: vec![entry(
                        "Shift 1",
                        "Open workspace Shift 1",
                        "--workspace=ws-1"
                    )],
                },
                JumpListCategory::Tasks {
                    items: vec![entry(
                        "New window",
                        "Open a new Muxus window",
                        "--new-window"
                    )],
                },
            ]
        );
    }

    #[test]
    fn keeps_five_hosts_and_three_workspaces() {
        let categories = jump_list_categories(
            &RecentLaunches {
                hosts: hosts(8),
                workspaces: workspaces(6),
            },
            &launcher(),
            &HashSet::new(),
        );
        let titles = |index: usize| -> Vec<String> {
            categories[index]
                .items()
                .iter()
                .map(|item| item.title.clone())
                .collect()
        };
        assert_eq!(
            titles(0),
            ["edge-1", "edge-2", "edge-3", "edge-4", "edge-5"]
        );
        assert_eq!(titles(1), ["Shift 1", "Shift 2", "Shift 3"]);
    }

    #[test]
    fn leaves_out_entries_the_user_removed_and_fills_the_place_with_the_next_one() {
        let categories = jump_list_categories(
            &RecentLaunches {
                hosts: hosts(6),
                workspaces: Vec::new(),
            },
            &launcher(),
            &HashSet::from(["--host=edge-2".to_string()]),
        );
        let args: Vec<&str> = categories[0]
            .items()
            .iter()
            .map(|item| item.args.as_str())
            .collect();
        assert_eq!(
            args,
            [
                "--host=edge-1",
                "--host=edge-3",
                "--host=edge-4",
                "--host=edge-5",
                "--host=edge-6"
            ]
        );
    }

    #[test]
    fn shows_only_the_new_window_task_until_something_has_been_used() {
        let categories =
            jump_list_categories(&RecentLaunches::default(), &launcher(), &HashSet::new());
        assert_eq!(categories.len(), 1);
        assert!(matches!(categories[0], JumpListCategory::Tasks { .. }));
    }

    #[test]
    fn starts_electron_with_the_app_folder_from_a_source_checkout() {
        let categories = jump_list_categories(
            &RecentLaunches {
                hosts: vec![host("core sw", "core sw")],
                workspaces: Vec::new(),
            },
            &JumpListLauncher {
                program: "C:\\src\\node_modules\\electron\\dist\\electron.exe".into(),
                args: vec!["C:\\Users\\Jane Doe\\muxus\\electron".into()],
            },
            &HashSet::new(),
        );
        assert_eq!(
            categories[0].items()[0].args,
            "\"C:\\Users\\Jane Doe\\muxus\\electron\" \"--host=core sw\""
        );
    }

    #[test]
    fn shortens_a_tooltip_windows_would_reject() {
        let categories = jump_list_categories(
            &RecentLaunches {
                hosts: vec![host("long", &"x".repeat(300))],
                workspaces: Vec::new(),
            },
            &launcher(),
            &HashSet::new(),
        );
        assert_eq!(categories[0].items()[0].description.chars().count(), 260);
    }

    #[test]
    fn uses_the_bundle_identifier_the_packages_give_muxus() {
        let plist = include_str!("../../../../../packaging/build-dmg.sh");
        assert!(plist.contains(&format!("<string>{APP_USER_MODEL_ID}</string>")));
    }

    // ---- macOS Dock menu ----

    #[test]
    fn offers_new_window_then_recent_hosts_and_workspaces_under_headings() {
        let entries = dock_menu_entries(&RecentLaunches {
            hosts: hosts(1),
            workspaces: workspaces(1),
        });
        let launch = |label: &str, launch: CommandLineLaunch| DockMenuEntry::Launch {
            label: label.into(),
            launch,
        };
        let heading = |label: &str| DockMenuEntry::Heading {
            label: label.into(),
        };
        assert_eq!(
            entries,
            [
                launch("New Window", CommandLineLaunch::NewWindow),
                DockMenuEntry::Separator,
                heading("Recent Hosts"),
                launch(
                    "edge-1",
                    CommandLineLaunch::Host {
                        name: "edge-1".into()
                    }
                ),
                DockMenuEntry::Separator,
                heading("Recent Workspaces"),
                launch(
                    "Shift 1",
                    CommandLineLaunch::Workspace {
                        name: "ws-1".into()
                    }
                ),
            ]
        );
    }

    #[test]
    fn keeps_five_hosts_and_three_workspaces_and_drops_empty_sections() {
        let kinds = |recent: &RecentLaunches| -> Vec<&'static str> {
            dock_menu_entries(recent)
                .iter()
                .filter_map(|entry| match entry {
                    DockMenuEntry::Launch { launch, .. } => Some(match launch {
                        CommandLineLaunch::NewWindow => "new-window",
                        CommandLineLaunch::Host { .. } => "host",
                        _ => "workspace",
                    }),
                    _ => None,
                })
                .collect()
        };
        let mut expected = vec!["new-window"];
        expected.extend(["host"; 5]);
        expected.extend(["workspace"; 3]);
        assert_eq!(
            kinds(&RecentLaunches {
                hosts: hosts(7),
                workspaces: workspaces(4)
            }),
            expected
        );
        assert_eq!(
            dock_menu_entries(&RecentLaunches::default()),
            [DockMenuEntry::Launch {
                label: "New Window".into(),
                launch: CommandLineLaunch::NewWindow
            }]
        );
    }
}
