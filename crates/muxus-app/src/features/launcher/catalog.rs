//! What the quick launcher offers (the result builders of
//! `client/src/components/QuickLauncherDialog.tsx`).
//!
//! The catalog is the expensive half and does not depend on the query, so it
//! is built once per data change; only the history and quick-connect entries
//! follow what is typed, and the app actions and keymap are a third, cheap
//! list. The view concatenates the three and ranks them with
//! [`super::search::select_quick_launcher_items`].

use std::collections::HashMap;

use muxus_core::api::{
    ForwardInfo, ForwardStatus, ForwardType, PasswordVaultStatus, SNIPPET_MATCH_END,
    SNIPPET_MATCH_START, SavedHostProfile, SessionLogStatus, SessionLogSummary, SshHostEntry,
    TunnelRecord, WorkspaceSummary,
};
use muxus_core::js::{is_js_whitespace, js_trim};
use muxus_core::protocol::{ProfileKind, SessionProfile};
use muxus_store::prefs::command_buttons::{
    CommandButton, CommandButtonGroup, command_button_group_of, command_button_label,
};
use muxus_store::prefs::state::LocalShellProfileConfig;

use super::search::{LauncherItem, QuickLauncherItem, QuickLauncherKind};
use super::time_format::format_timestamp;
use crate::features::settings::vault_secrets::{find_vault_secret, vault_secret_missing};
use crate::hosts::managed::{ManagedHost, managed_host_address, managed_host_display_name};
use crate::hosts::tree::folder_segments;
use crate::keymap::bindings::KeybindingOverrides;
use crate::keymap::commands::key_commands;
use crate::keymap::hints::chord_labels;
use crate::model::profile_kind_of;
use crate::state::session_actions::is_quick_connect_target;
use crate::state::tabs::{TabStatus, TerminalTab};

/// App actions the launcher can run (`LauncherAction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LauncherAction {
    NewLocal,
    NewHost,
    Settings,
    Workspaces,
    History,
    Commands,
    Tunnels,
    Sftp,
    FindTerminal,
}

impl LauncherAction {
    pub fn as_str(self) -> &'static str {
        match self {
            LauncherAction::NewLocal => "new-local",
            LauncherAction::NewHost => "new-host",
            LauncherAction::Settings => "settings",
            LauncherAction::Workspaces => "workspaces",
            LauncherAction::History => "history",
            LauncherAction::Commands => "commands",
            LauncherAction::Tunnels => "tunnels",
            LauncherAction::Sftp => "sftp",
            LauncherAction::FindTerminal => "find-terminal",
        }
    }
}

/// What running a result does (the per-kind payload of `LauncherResult`).
#[derive(Clone, Debug, PartialEq)]
pub enum LauncherPayload {
    Tab {
        tab_id: String,
        reconnect: bool,
    },
    Editor {
        tab_id: String,
        path: String,
    },
    Host {
        host: ManagedHost,
        protocol: ProfileKind,
    },
    QuickConnect {
        target: String,
    },
    Workspace(WorkspaceSummary),
    Command(CommandButton),
    Tunnel {
        tunnel: TunnelRecord,
        running: Option<ForwardInfo>,
    },
    History {
        session: SessionLogSummary,
        history_query: String,
    },
    LocalShell {
        profile: LocalShellProfileConfig,
        wsl: bool,
    },
    Action(LauncherAction),
    Keymap {
        command_id: String,
        chord: Option<String>,
    },
}

/// One row of the launcher.
#[derive(Clone, Debug, PartialEq)]
pub struct LauncherResult {
    pub item: QuickLauncherItem,
    pub payload: LauncherPayload,
    /// Shown instead of the detail; the row cannot be run.
    pub disabled_reason: Option<String>,
}

impl LauncherItem for LauncherResult {
    fn item(&self) -> &QuickLauncherItem {
        &self.item
    }
}

impl LauncherResult {
    pub fn kind(&self) -> QuickLauncherKind {
        match &self.payload {
            LauncherPayload::Tab { .. } => QuickLauncherKind::Tab,
            LauncherPayload::Editor { .. } => QuickLauncherKind::Editor,
            LauncherPayload::Host { .. } => QuickLauncherKind::Host,
            LauncherPayload::QuickConnect { .. } => QuickLauncherKind::QuickConnect,
            LauncherPayload::Workspace(_) => QuickLauncherKind::Workspace,
            LauncherPayload::Command(_) => QuickLauncherKind::Command,
            LauncherPayload::Tunnel { .. } => QuickLauncherKind::Tunnel,
            LauncherPayload::History { .. } => QuickLauncherKind::History,
            LauncherPayload::LocalShell { .. } => QuickLauncherKind::LocalShell,
            LauncherPayload::Action(_) => QuickLauncherKind::Action,
            LauncherPayload::Keymap { .. } => QuickLauncherKind::Keymap,
        }
    }

    pub fn id(&self) -> &str {
        &self.item.id
    }

    /// The second line: the reason a row is disabled, else its detail.
    pub fn secondary(&self) -> &str {
        self.disabled_reason.as_deref().unwrap_or(&self.item.detail)
    }

    /// The small caps tag at the row's end (`ResultKindLabel`); keymap rows
    /// show their chord instead.
    pub fn kind_label(&self) -> &'static str {
        match self.kind() {
            QuickLauncherKind::QuickConnect => "CONNECT",
            QuickLauncherKind::LocalShell => "LOCAL",
            QuickLauncherKind::Editor => "FILE",
            QuickLauncherKind::Tab => "TAB",
            QuickLauncherKind::Host => "HOST",
            QuickLauncherKind::Workspace => "WORKSPACE",
            QuickLauncherKind::Command => "COMMAND",
            QuickLauncherKind::Tunnel => "TUNNEL",
            QuickLauncherKind::History => "HISTORY",
            QuickLauncherKind::Action => "ACTION",
            QuickLauncherKind::Keymap => "KEYMAP",
        }
    }
}

fn result(
    id: String,
    label: impl Into<String>,
    detail: impl Into<String>,
    keywords: Vec<String>,
    priority: i32,
    show_when_empty: bool,
    payload: LauncherPayload,
) -> LauncherResult {
    LauncherResult {
        item: QuickLauncherItem::new(id, label, detail)
            .keywords(keywords)
            .priority(priority)
            .show_when_empty(show_when_empty),
        payload,
        disabled_reason: None,
    }
}

/// Everything the catalog half reads (`buildCatalogResults`' arguments).
#[derive(Clone, Copy)]
pub struct CatalogInput<'a> {
    pub tabs: &'a [TerminalTab],
    pub active_id: Option<&'a str>,
    pub ssh_hosts: &'a [SshHostEntry],
    pub saved_hosts: &'a [SavedHostProfile],
    pub workspaces: &'a [WorkspaceSummary],
    pub active_workspace_id: Option<&'a str>,
    pub open_workspace_window_counts: &'a HashMap<String, usize>,
    pub commands: &'a [CommandButton],
    pub command_groups: &'a [CommandButtonGroup],
    /// The group the command bar shows; its commands lead the empty launcher.
    pub shown_command_group: &'a str,
    /// Names of the secrets saved commands type; never their values.
    pub vault: Option<&'a PasswordVaultStatus>,
    pub local_shell_profiles: &'a [LocalShellProfileConfig],
    pub wsl_shell_profiles: &'a [LocalShellProfileConfig],
    pub tunnels: &'a [TunnelRecord],
    pub forwards: &'a [ForwardInfo],
    pub active_connected: bool,
}

/// Everything the launcher can offer that does not depend on the query.
pub fn build_catalog_results(input: CatalogInput<'_>) -> Vec<LauncherResult> {
    let mut results = Vec::new();

    for tab in input.tabs {
        let Some(profile) = &tab.profile else {
            continue;
        };
        let reconnect = tab.status == TabStatus::Closed;
        let active = input.active_id == Some(tab.id.as_str());
        let mut keywords = vec![profile.kind().as_str().to_owned()];
        keywords.extend(tab.editor_paths.iter().cloned());
        results.push(result(
            format!("tab:{}", tab.id),
            tab.title.clone(),
            format!(
                "{} · {}",
                tab_status_label(tab.status),
                profile_summary(profile)
            ),
            keywords,
            (if active { 500 } else { 0 })
                + match tab.status {
                    TabStatus::Connected => 350,
                    TabStatus::Connecting => 250,
                    _ => 120,
                },
            active || tab.status != TabStatus::Closed,
            LauncherPayload::Tab {
                tab_id: tab.id.clone(),
                reconnect,
            },
        ));
        for path in &tab.editor_paths {
            results.push(result(
                format!("editor:{}:{path}", tab.id),
                basename(path),
                format!("{} · {path}", tab.title),
                vec![
                    "editor".into(),
                    if profile.kind() == ProfileKind::Local {
                        "local file".into()
                    } else {
                        "remote file".into()
                    },
                    path.clone(),
                ],
                if active { 420 } else { 300 },
                tab.active_editor_path.as_deref() == Some(path.as_str()),
                LauncherPayload::Editor {
                    tab_id: tab.id.clone(),
                    path: path.clone(),
                },
            ));
        }
    }

    for entry in input.ssh_hosts {
        let host = ManagedHost::ssh(entry.clone());
        let metadata = entry.metadata.as_ref();
        let group = metadata.and_then(|metadata| metadata.group.as_deref());
        let connect_count = metadata.map_or(0, |metadata| metadata.connect_count);
        let mut keywords = vec!["ssh".to_owned()];
        keywords.extend(entry.aliases.iter().cloned());
        // Each folder level is its own keyword, so typing "EU" finds the hosts
        // in Production/EU without having to type the whole path.
        keywords.extend(folder_segments(group));
        keywords.push(entry.description.clone().unwrap_or_default());
        keywords.push(entry.file.clone());
        keywords.push(entry.resolved.hostname.clone());
        keywords.push(entry.resolved.user.clone().unwrap_or_default());
        results.push(result(
            format!("host:ssh:{}", entry.alias),
            managed_host_display_name(&host).to_owned(),
            format!("{}{}", folder_detail(group), managed_host_address(&host)),
            keywords,
            connect_count.min(80) as i32
                + if metadata.is_some_and(|metadata| metadata.last_connected_at.is_some()) {
                    60
                } else {
                    0
                },
            connect_count > 0,
            LauncherPayload::Host {
                host,
                protocol: ProfileKind::Ssh,
            },
        ));
    }

    for entry in input.saved_hosts {
        let host = ManagedHost::profile(entry.clone());
        let group = entry.metadata.group.as_deref();
        let kind = profile_kind_of(entry.kind);
        let mut keywords = vec![kind.as_str().to_owned(), entry.name.clone()];
        keywords.extend(folder_segments(group));
        keywords.push(managed_host_address(&host));
        results.push(result(
            format!("host:profile:{}", entry.id),
            managed_host_display_name(&host).to_owned(),
            format!("{}{}", folder_detail(group), managed_host_address(&host)),
            keywords,
            entry.metadata.connect_count.min(80) as i32
                + if entry.metadata.last_connected_at.is_some() {
                    60
                } else {
                    0
                },
            entry.metadata.connect_count > 0,
            LauncherPayload::Host {
                host,
                protocol: kind,
            },
        ));
    }

    for (index, workspace) in input.workspaces.iter().enumerate() {
        let other_window_count = input
            .open_workspace_window_counts
            .get(&workspace.id)
            .copied()
            .unwrap_or(0);
        let current = input.active_workspace_id == Some(workspace.id.as_str());
        let detail = if current {
            "Current workspace".to_owned()
        } else if other_window_count > 0 {
            if other_window_count == 1 {
                "Open in another window".to_owned()
            } else {
                format!("Open in {other_window_count} other windows")
            }
        } else {
            format!(
                "Workspace · {}",
                format_timestamp(
                    workspace
                        .last_opened_at
                        .as_deref()
                        .unwrap_or(&workspace.updated_at)
                )
            )
        };
        results.push(result(
            format!("workspace:{}", workspace.id),
            workspace.name.clone(),
            detail,
            vec![
                "workspace".into(),
                if workspace.is_startup {
                    "startup".into()
                } else {
                    String::new()
                },
            ],
            if current { 360 } else { 150 },
            current || index < 5,
            LauncherPayload::Workspace(workspace.clone()),
        ));
    }

    let mut shown_commands = 0;
    for command in input.commands {
        let group_id = command_button_group_of(command, input.command_groups);
        let shown = group_id == input.shown_command_group;
        // Group names only mean something once there is more than one group.
        let group = if input.command_groups.len() > 1 {
            input
                .command_groups
                .iter()
                .find(|candidate| candidate.id == group_id)
                .map(|candidate| candidate.name.clone())
        } else {
            None
        };
        let group_prefix = group
            .as_ref()
            .map(|group| format!("{group} · "))
            .unwrap_or_default();
        if let Some(secret_id) = &command.secret_id {
            let secret = (!secret_id.is_empty())
                .then(|| find_vault_secret(input.vault, secret_id))
                .flatten();
            let missing =
                secret_id.is_empty() || vault_secret_missing(input.vault, secret_id) == Some(true);
            let mut keywords: Vec<String> = ["command", "saved command", "secret", "password"]
                .into_iter()
                .map(Into::into)
                .collect();
            if let Some(secret) = secret {
                keywords.push(secret.name.clone());
            }
            if let Some(group) = &group {
                keywords.push(group.clone());
            }
            let detail = if missing {
                "Secret missing".to_owned()
            } else {
                match secret {
                    Some(secret) => format!("Types secret “{}”", secret.name),
                    None => "Types secret".to_owned(),
                }
            };
            let show = input.active_connected && shown && !missing && {
                shown_commands += 1;
                shown_commands <= 5
            };
            let mut entry = result(
                format!("command:{}", command.id),
                command_button_label(command),
                format!("{group_prefix}{detail}"),
                keywords,
                if shown { 130 } else { 120 },
                show,
                LauncherPayload::Command(command.clone()),
            );
            entry.disabled_reason = if !input.active_connected {
                Some("Connect a terminal to use this command".into())
            } else if missing {
                Some("Its secret is no longer in the password vault".into())
            } else {
                None
            };
            results.push(entry);
            continue;
        }
        let mut keywords = vec![
            "command".to_owned(),
            "saved command".to_owned(),
            command.command.clone(),
        ];
        if let Some(group) = &group {
            keywords.push(group.clone());
        }
        let show = input.active_connected && shown && {
            shown_commands += 1;
            shown_commands <= 5
        };
        let mut entry = result(
            format!("command:{}", command.id),
            command_button_label(command),
            format!(
                "{group_prefix}{} · {}",
                if command.send_enter { "Run" } else { "Insert" },
                one_line(&command.command)
            ),
            keywords,
            if shown { 130 } else { 120 },
            show,
            LauncherPayload::Command(command.clone()),
        );
        entry.disabled_reason =
            (!input.active_connected).then(|| "Connect a terminal to use this command".into());
        results.push(entry);
    }

    let shell_profiles = input
        .local_shell_profiles
        .iter()
        .map(|profile| (profile, false))
        .chain(
            input
                .wsl_shell_profiles
                .iter()
                .map(|profile| (profile, true)),
        );
    for (index, (profile, wsl)) in shell_profiles.enumerate() {
        let shell = if profile.shell.is_empty() {
            "automatic shell"
        } else {
            profile.shell.as_str()
        };
        let command_line = std::iter::once(shell)
            .chain(profile.args.iter().map(String::as_str))
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let name = js_trim(&profile.name);
        let mut keywords = vec!["local".to_owned(), "terminal".into(), "shell".into()];
        if wsl {
            keywords.extend(["wsl".to_owned(), "linux".into()]);
        }
        keywords.push(profile.shell.clone());
        keywords.extend(profile.args.iter().cloned());
        keywords.push(profile.cwd.clone());
        keywords.push(profile.startup_command.clone());
        results.push(result(
            format!("local-shell:{}", profile.id),
            if name.is_empty() {
                "Unnamed shell"
            } else {
                name
            },
            format!(
                "{} · {command_line}",
                if wsl {
                    "WSL distribution"
                } else {
                    "Local shell"
                }
            ),
            keywords,
            190,
            index < 5,
            LauncherPayload::LocalShell {
                profile: profile.clone(),
                wsl,
            },
        ));
    }

    let running_by_tunnel: HashMap<&str, &ForwardInfo> = input
        .forwards
        .iter()
        .filter_map(|forward| forward.tunnel_id.as_deref().map(|id| (id, forward)))
        .collect();
    for tunnel in input.tunnels {
        let running = running_by_tunnel.get(tunnel.id.as_str()).copied();
        results.push(result(
            format!("tunnel:{}", tunnel.id),
            tunnel.name.clone().unwrap_or_else(|| tunnel.target.clone()),
            format!("{} · {}", tunnel_state(running), describe_tunnel(tunnel)),
            vec![
                "tunnel".into(),
                "forward".into(),
                tunnel.target.clone(),
                forward_type_name(tunnel.kind).into(),
            ],
            if running.is_some() { 280 } else { 100 },
            running.is_some(),
            LauncherPayload::Tunnel {
                tunnel: tunnel.clone(),
                running: running.cloned(),
            },
        ));
    }

    results
}

/// Session history and ad-hoc connect: the entries the query itself produces.
pub fn build_query_results(
    ssh_hosts: &[SshHostEntry],
    saved_hosts: &[SavedHostProfile],
    history: &[SessionLogSummary],
    history_query: &str,
    direct_target: &str,
) -> Vec<LauncherResult> {
    let mut results = Vec::new();

    for session in history.iter().take(12) {
        let title_prefix = if session.label.is_some() && session.title != session.host {
            format!("{} · ", session.title)
        } else {
            String::new()
        };
        let snippet = session
            .snippet
            .as_deref()
            .map(|snippet| format!(" · {}", strip_markup(snippet)))
            .unwrap_or_default();
        results.push(result(
            format!("history:{}", session.id),
            session.name(),
            format!(
                "{title_prefix}{} · {}{snippet}",
                session.host,
                format_timestamp(&session.started_at)
            ),
            vec![
                "history".into(),
                "session log".into(),
                session.kind.clone(),
                session.host.clone(),
                session.snippet.clone().unwrap_or_default(),
            ],
            if session.status == SessionLogStatus::Active {
                180
            } else {
                40
            },
            false,
            LauncherPayload::History {
                session: session.clone(),
                history_query: history_query.to_owned(),
            },
        ));
    }

    // The shape check is cheap and rejects most of what gets typed, so the host
    // scans only run for text that could actually be connected to.
    let target = js_trim(direct_target);
    if is_quick_connect_target(target) {
        let normalized = target.to_lowercase();
        let known = ssh_hosts.iter().any(|entry| {
            entry
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase() == normalized)
        }) || saved_hosts.iter().any(|entry| {
            entry.name.to_lowercase() == normalized
                || entry
                    .metadata
                    .display_name
                    .as_ref()
                    .is_some_and(|name| name.to_lowercase() == normalized)
        });
        if !known {
            results.push(result(
                format!("quick-connect:{target}"),
                format!("Connect to {target}"),
                "Ad-hoc SSH connection",
                vec!["ssh".into(), "connect".into(), target.to_owned()],
                720,
                false,
                LauncherPayload::QuickConnect {
                    target: target.to_owned(),
                },
            ));
        }
    }

    results
}

/// Whether the active tab can take a BREAK (`supportsBreak` / `canSendBreak`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakState {
    /// Send BREAK only exists for serial, Telnet and SSH tabs.
    None,
    Waiting,
    Ready,
}

impl BreakState {
    pub fn of(tab: Option<&TerminalTab>) -> Self {
        let Some(tab) = tab else {
            return BreakState::None;
        };
        let supports = tab.profile.as_ref().is_some_and(|profile| {
            matches!(
                profile.kind(),
                ProfileKind::Serial | ProfileKind::Telnet | ProfileKind::Ssh
            )
        });
        if !supports {
            return BreakState::None;
        }
        // An SSH console that has printed nothing yet still counts: a silent
        // console is exactly when BREAK is wanted.
        if tab.status == TabStatus::Connected
            || (tab.status == TabStatus::Connecting && tab.conn_id.is_some())
        {
            BreakState::Ready
        } else {
            BreakState::Waiting
        }
    }
}

fn action_result(
    action: LauncherAction,
    label: &str,
    detail: &str,
    keywords: &[&str],
) -> LauncherResult {
    result(
        format!("action:{}", action.as_str()),
        label,
        detail,
        keywords.iter().map(|word| (*word).to_owned()).collect(),
        70,
        false,
        LauncherPayload::Action(action),
    )
}

/// App actions and the searchable keymap, both independent of the query.
pub fn build_action_results(
    active_connected: bool,
    active_ssh: bool,
    active_break: BreakState,
    keybindings: &KeybindingOverrides,
) -> Vec<LauncherResult> {
    let mut results = vec![
        action_result(
            LauncherAction::NewLocal,
            "New local terminal",
            "Open a fresh local shell",
            &["terminal", "shell", "local"],
        ),
        action_result(
            LauncherAction::NewHost,
            "Add host",
            "Create an SSH, Telnet, serial, RDP, VNC, gNMI, or NETCONF host",
            &["new", "connection", "profile"],
        ),
        action_result(
            LauncherAction::Settings,
            "Open settings",
            "Appearance, terminal, behavior, and shortcuts",
            &["preferences", "configure"],
        ),
        action_result(
            LauncherAction::Workspaces,
            "Manage workspaces",
            "Save or open terminal layouts",
            &["layout", "sessions"],
        ),
        action_result(
            LauncherAction::History,
            "Open session history",
            "Search retained terminal output",
            &["logs", "transcript", "replay"],
        ),
        action_result(
            LauncherAction::Commands,
            "Manage saved commands",
            "Create and edit command buttons",
            &["buttons", "snippets"],
        ),
        action_result(
            LauncherAction::Tunnels,
            "Open port forwarding",
            "Manage saved tunnels and live forwards",
            &["ssh", "forward", "proxy"],
        ),
    ];
    let mut sftp = action_result(
        LauncherAction::Sftp,
        "Toggle file browser",
        "Browse files over SFTP for the active SSH session",
        &["files", "upload", "download"],
    );
    sftp.disabled_reason = (!active_ssh).then(|| "Select a connected SSH session first".into());
    results.push(sftp);
    let mut find = action_result(
        LauncherAction::FindTerminal,
        "Find in active terminal",
        "Search the current terminal buffer",
        &["search", "scrollback"],
    );
    find.disabled_reason = (!active_connected).then(|| "Select a connected terminal first".into());
    results.push(find);

    // Every keyboard command is searchable here, with the chord that runs it:
    // the palette doubles as the discovery path for the keymap.
    for command in key_commands() {
        if !command.palette {
            continue;
        }
        let sends_break = command.id == "terminal.send-break";
        if sends_break && active_break == BreakState::None {
            continue;
        }
        let chords = chord_labels(&command.id, keybindings);
        let mut keywords = command.keywords.clone();
        keywords.extend(chords.iter().cloned());
        keywords.push(category_key(command.category).into());
        keywords.push("shortcut".into());
        let mut entry = result(
            format!("keymap:{}", command.id),
            command.title.clone(),
            command.category.label(),
            keywords,
            60,
            false,
            LauncherPayload::Keymap {
                command_id: command.id.clone(),
                chord: chords.first().cloned(),
            },
        );
        entry.disabled_reason = (sends_break && active_break == BreakState::Waiting)
            .then(|| "Wait for the session to connect first".into());
        results.push(entry);
    }

    results
}

/// The category as the TypeScript keys it (`command.category`).
fn category_key(category: crate::keymap::commands::CommandCategory) -> &'static str {
    use crate::keymap::commands::CommandCategory;
    match category {
        CommandCategory::Panes => "panes",
        CommandCategory::Tabs => "tabs",
        CommandCategory::Terminal => "terminal",
        CommandCategory::App => "app",
    }
}

/// `profileSummary`: the protocol and where a tab connects.
pub fn profile_summary(profile: &SessionProfile) -> String {
    match profile {
        SessionProfile::Ssh(p) => format!("SSH · {}", p.target),
        SessionProfile::Telnet(p) => format!("Telnet · {}:{}", p.host, p.port),
        SessionProfile::Serial(p) => format!("Serial · {}", p.path),
        SessionProfile::Rdp(p) => format!("RDP · {}:{}", p.host, p.port),
        SessionProfile::Vnc(p) => format!("VNC · {}:{}", p.host, p.port),
        SessionProfile::Gnmi(p) => format!("gNMI · {}:{}", p.host, p.port),
        SessionProfile::Netconf(p) => format!("NETCONF · {}:{}", p.host, p.port),
        SessionProfile::Local(p) => match p.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
            Some(cwd) => format!("Local · {cwd}"),
            None => "Local".into(),
        },
    }
}

/// `tabStatusLabel`.
pub fn tab_status_label(status: TabStatus) -> &'static str {
    match status {
        TabStatus::Connected => "Connected",
        TabStatus::Connecting => "Connecting",
        TabStatus::Interrupted => "Connection interrupted",
        TabStatus::Closed | TabStatus::Idle => "Disconnected — reconnect",
    }
}

/// `tunnelState`.
pub fn tunnel_state(running: Option<&ForwardInfo>) -> &'static str {
    match running.map(|forward| forward.status) {
        None => "Stopped",
        Some(ForwardStatus::Starting) => "Starting",
        Some(ForwardStatus::Reconnecting) => "Reconnecting",
        Some(ForwardStatus::Error) => "Needs sign-in",
        Some(ForwardStatus::Active) => "Running",
    }
}

fn forward_type_name(kind: ForwardType) -> &'static str {
    match kind {
        ForwardType::Local => "local",
        ForwardType::Remote => "remote",
        ForwardType::Dynamic => "dynamic",
    }
}

/// `describeTunnel`.
pub fn describe_tunnel(tunnel: &TunnelRecord) -> String {
    if tunnel.kind == ForwardType::Dynamic {
        return format!("SOCKS localhost:{} via {}", tunnel.bind_port, tunnel.target);
    }
    format!(
        "{} {} → {}:{} via {}",
        if tunnel.kind == ForwardType::Local {
            "Local"
        } else {
            "Remote"
        },
        tunnel.bind_port,
        tunnel.target_host.as_deref().unwrap_or("undefined"),
        tunnel
            .target_port
            .map_or_else(|| "undefined".to_owned(), |port| port.to_string()),
        tunnel.target
    )
}

/// Every run of whitespace that `keep` accepts is passed through, the others
/// are replaced by `with`.
fn replace_whitespace_runs(value: &str, replace: impl Fn(&str) -> Option<&'static str>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        match replace(run) {
            Some(with) => out.push_str(with),
            None => out.push_str(run),
        }
        run.clear();
    };
    for c in value.chars() {
        if is_js_whitespace(c) {
            run.push(c);
        } else {
            if !run.is_empty() {
                flush(&mut run, &mut out);
            }
            out.push(c);
        }
    }
    if !run.is_empty() {
        flush(&mut run, &mut out);
    }
    out
}

/// `oneLine`: a multi-line command on one row, line breaks shown as ↵
/// (`value.replace(/\s*\r?\n\s*/g, ' ↵ ').trim()`: a whitespace run holding
/// a line break becomes one arrow).
pub fn one_line(value: &str) -> String {
    let replaced = replace_whitespace_runs(value, |run| run.contains('\n').then_some(" ↵ "));
    js_trim(&replaced).to_owned()
}

/// The last path segment (`basename`).
pub fn basename(path: &str) -> String {
    path.split('/')
        .rfind(|segment| !segment.is_empty())
        .unwrap_or(path)
        .to_owned()
}

/// `stripMarkup`: a search snippet as plain text on one line.
pub fn strip_markup(value: &str) -> String {
    let without_marks: String = value
        .chars()
        .filter(|c| *c != SNIPPET_MATCH_START && *c != SNIPPET_MATCH_END)
        .collect();
    // `/<[^>]*>/g`: from a `<` to the next `>`; a `<` that never closes stays.
    let mut text = String::with_capacity(without_marks.len());
    let mut rest = without_marks.as_str();
    while let Some(start) = rest.find('<') {
        match rest[start..].find('>') {
            Some(end) => {
                text.push_str(&rest[..start]);
                rest = &rest[start + end + 1..];
            }
            None => break,
        }
    }
    text.push_str(rest);
    let collapsed = replace_whitespace_runs(&text, |_| Some(" "));
    js_trim(&collapsed).to_owned()
}

/// `folderDetail`: `Production / EU · `, spaced so a folder path does not
/// read like a URL.
pub fn folder_detail(group: Option<&str>) -> String {
    let segments = folder_segments(group);
    if segments.is_empty() {
        String::new()
    } else {
        format!("{} · ", segments.join(" / "))
    }
}

#[cfg(test)]
mod tests {
    use muxus_core::api::SessionLogStatus;
    use muxus_core::protocol::{LocalProfile, SshProfile, TelnetProfile};
    use muxus_store::prefs::command_buttons::default_command_group;

    use super::*;
    use crate::features::launcher::search::{RESULT_LIMIT, select_quick_launcher_items};
    use crate::state::tabs::{IdGen, TabPatch, TabsState};

    fn ssh_entry(alias: &str, group: Option<&str>, connect_count: u64) -> SshHostEntry {
        SshHostEntry {
            alias: alias.into(),
            aliases: vec![alias.into()],
            file: "/home/demo/.ssh/config".into(),
            resolved: muxus_core::api::ResolvedHostSettings {
                hostname: format!("{alias}.prod.internal"),
                user: Some("deploy".into()),
                port: 22,
                ..Default::default()
            },
            metadata: Some(crate::model::HostMetadata {
                profile_id: format!("ssh:{alias}"),
                group: group.map(Into::into),
                connect_count,
                ..Default::default()
            }),
            description: None,
            options: Default::default(),
        }
    }

    fn session(id: &str, status: SessionLogStatus, snippet: Option<&str>) -> SessionLogSummary {
        SessionLogSummary {
            id: id.into(),
            profile_key: "ssh:web-01".into(),
            title: "web-01".into(),
            label: None,
            kind: "ssh".into(),
            host: "web-01".into(),
            started_at: "2026-07-25T20:20:00.000Z".into(),
            ended_at: None,
            status,
            paused: false,
            capture_input: false,
            pinned: false,
            event_count: 1,
            raw_bytes: 1,
            normalized_bytes: 1,
            part_count: 1,
            snippet: snippet.map(Into::into),
            match_count: None,
        }
    }

    fn empty_input<'a>(
        tabs: &'a [TerminalTab],
        hosts: &'a [SshHostEntry],
        counts: &'a HashMap<String, usize>,
        commands: &'a [CommandButton],
        groups: &'a [CommandButtonGroup],
    ) -> CatalogInput<'a> {
        CatalogInput {
            tabs,
            active_id: tabs.first().map(|tab| tab.id.as_str()),
            ssh_hosts: hosts,
            saved_hosts: &[],
            workspaces: &[],
            active_workspace_id: None,
            open_workspace_window_counts: counts,
            commands,
            command_groups: groups,
            shown_command_group: "default",
            vault: None,
            local_shell_profiles: &[],
            wsl_shell_profiles: &[],
            tunnels: &[],
            forwards: &[],
            active_connected: true,
        }
    }

    #[test]
    fn describes_tabs_hosts_and_their_folders() {
        let mut tabs = TabsState::with_pane("pane-test", IdGen::sequential());
        let id = tabs.open(SessionProfile::Ssh(SshProfile::new("web-01")), "web-01");
        tabs.update(
            &id,
            TabPatch {
                status: Some(TabStatus::Connected),
                ..Default::default()
            },
        );
        let hosts = [ssh_entry("web-01", Some("Production/Web"), 0)];
        let counts = HashMap::new();
        let groups = [default_command_group()];
        let results = build_catalog_results(empty_input(&tabs.tabs, &hosts, &counts, &[], &groups));
        assert_eq!(results[0].item.label, "web-01");
        assert_eq!(results[0].item.detail, "Connected · SSH · web-01");
        assert_eq!(results[0].kind_label(), "TAB");
        assert_eq!(results[0].item.priority, 850);
        assert_eq!(results[1].item.id, "host:ssh:web-01");
        assert_eq!(
            results[1].item.detail,
            "Production / Web · deploy@web-01.prod.internal"
        );
        assert!(results[1].item.keywords.contains(&"Web".to_owned()));
        assert!(!results[1].item.show_when_empty);
    }

    #[test]
    fn offers_quick_connect_only_for_unknown_targets() {
        let hosts = [ssh_entry("web-01", None, 3)];
        let results = build_query_results(&hosts, &[], &[], "", "ops@staging-07:2222");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item.label, "Connect to ops@staging-07:2222");
        assert_eq!(results[0].item.detail, "Ad-hoc SSH connection");
        assert_eq!(results[0].item.priority, 720);
        assert!(build_query_results(&hosts, &[], &[], "", "WEB-01").is_empty());
        assert!(build_query_results(&hosts, &[], &[], "", "two words").is_empty());
    }

    #[test]
    fn lists_history_with_plain_snippets_and_ranks_live_logs() {
        let sessions = [
            session(
                "a",
                SessionLogStatus::Active,
                Some("Connecting to \u{1}deploy\u{2}@web <b>x</b>"),
            ),
            session("b", SessionLogStatus::Completed, None),
        ];
        // "web" reads as a host name nobody saved: it comes with a quick connect.
        let results = build_query_results(&[], &[], &sessions, "web", "web");
        assert_eq!(results.len(), 3);
        assert_eq!(results[2].item.id, "quick-connect:web");
        let results =
            build_query_results(&[ssh_entry("web", None, 0)], &[], &sessions, "web", "web");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].item.priority, 180);
        assert_eq!(results[1].item.priority, 40);
        assert!(results[0].item.detail.starts_with("web-01 · Jul 25, 2026"));
        assert!(
            results[0]
                .item
                .detail
                .ends_with(" · Connecting to deploy@web x")
        );
    }

    #[test]
    fn filters_the_launcher_like_the_screenshot() {
        let mut tabs = TabsState::with_pane("pane-test", IdGen::sequential());
        let id = tabs.open(SessionProfile::Ssh(SshProfile::new("web-01")), "web-01");
        tabs.update(
            &id,
            TabPatch {
                status: Some(TabStatus::Connected),
                ..Default::default()
            },
        );
        let hosts = [
            ssh_entry("web-01", Some("Production/Web"), 0),
            ssh_entry("web-02", Some("Production/Web"), 0),
        ];
        let counts = HashMap::new();
        let groups = [default_command_group()];
        let mut all = build_catalog_results(empty_input(&tabs.tabs, &hosts, &counts, &[], &groups));
        all.extend(build_query_results(&hosts, &[], &[], "web", "web"));
        all.extend(build_action_results(
            true,
            true,
            BreakState::Ready,
            &HashMap::new(),
        ));
        let ids: Vec<&str> = select_quick_launcher_items(&all, "web", RESULT_LIMIT)
            .into_iter()
            .map(|result| result.item.id.as_str())
            .take(4)
            .collect();
        assert_eq!(
            ids,
            [
                "tab:tab-1",
                "quick-connect:web",
                "host:ssh:web-01",
                "host:ssh:web-02"
            ]
        );
    }

    #[test]
    fn disables_commands_without_a_connected_terminal() {
        let commands = [CommandButton {
            id: "a".into(),
            label: "status".into(),
            command: "status\nuptime".into(),
            send_enter: true,
            group_id: None,
            color: None,
            secret_id: None,
        }];
        let groups = [default_command_group()];
        let counts = HashMap::new();
        let mut input = empty_input(&[], &[], &counts, &commands, &groups);
        input.active_connected = false;
        let results = build_catalog_results(input);
        assert_eq!(results[0].item.detail, "Run · status ↵ uptime");
        assert_eq!(
            results[0].secondary(),
            "Connect a terminal to use this command"
        );
        assert!(!results[0].item.show_when_empty);
    }

    #[test]
    fn summarizes_profiles_and_tunnels() {
        assert_eq!(
            profile_summary(&SessionProfile::Telnet(TelnetProfile {
                profile_id: None,
                host: "sw1".into(),
                port: 23
            })),
            "Telnet · sw1:23"
        );
        assert_eq!(
            profile_summary(&SessionProfile::Local(LocalProfile {
                cwd: Some("/srv".into()),
                ..Default::default()
            })),
            "Local · /srv"
        );
        assert_eq!(one_line("  a \r\n  b\n"), "a ↵ b ↵");
        assert_eq!(basename("/etc/nginx/nginx.conf"), "nginx.conf");
        assert_eq!(basename("/"), "/");
        assert_eq!(folder_detail(Some("Production/EU")), "Production / EU · ");
        assert_eq!(folder_detail(None), "");
    }

    #[test]
    fn hides_send_break_for_sessions_without_it() {
        let none = build_action_results(false, false, BreakState::None, &HashMap::new());
        assert!(
            none.iter()
                .all(|result| result.item.id != "keymap:terminal.send-break")
        );
        let waiting = build_action_results(false, false, BreakState::Waiting, &HashMap::new());
        let entry = waiting
            .iter()
            .find(|result| result.item.id == "keymap:terminal.send-break")
            .expect("send break listed");
        assert_eq!(
            entry.disabled_reason.as_deref(),
            Some("Wait for the session to connect first")
        );
        let sftp = waiting
            .iter()
            .find(|result| result.item.id == "action:sftp")
            .unwrap();
        assert_eq!(
            sftp.disabled_reason.as_deref(),
            Some("Select a connected SSH session first")
        );
    }
}
