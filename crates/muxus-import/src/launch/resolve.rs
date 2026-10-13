//! Resolving command-line targets and connection links against the host and
//! workspace catalogs (port of `client/src/command-line-launch.ts`), plus
//! the decisions and messages of `CommandLineLaunchHandler.tsx`.

use std::sync::LazyLock;

use muxus_core::api::{CommandLineLaunch, SavedHostProfile, SshHostEntry, WorkspaceSummary};
use muxus_core::connection_links::{
    ConnectionLink, ConnectionLinkScheme, connection_link_target, parse_connection_link,
};
use muxus_core::js;
use muxus_core::protocol::{SavedHostSessionProfile, SessionProfile, SshProfile, TelnetProfile};
use regex::Regex;

use super::host_tree::{HostTree, ManagedHost, strip_config_extension};

/// The outcome of looking a name up.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetResolution<T> {
    Found(T),
    NotFound,
    Ambiguous { count: usize },
}

/// A folder (or ssh_config file group) and every host below it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedHostFolder<'a> {
    pub label: String,
    pub hosts: Vec<ManagedHost<'a>>,
}

fn normalized(value: &str) -> String {
    js::js_trim(value).to_lowercase()
}

fn result_for<T>(mut matches: Vec<T>) -> TargetResolution<T> {
    match matches.len() {
        0 => TargetResolution::NotFound,
        1 => TargetResolution::Found(matches.remove(0)),
        count => TargetResolution::Ambiguous { count },
    }
}

/// `new Map(hosts.map((host) => [key, host])).values()`: first position, last value.
fn unique_hosts<'a>(hosts: Vec<ManagedHost<'a>>) -> Vec<ManagedHost<'a>> {
    let mut keys: Vec<String> = Vec::new();
    let mut unique: Vec<ManagedHost<'a>> = Vec::new();
    for host in hosts {
        let key = host.key();
        match keys.iter().position(|existing| *existing == key) {
            Some(index) => unique[index] = host,
            None => {
                keys.push(key);
                unique.push(host);
            }
        }
    }
    unique
}

fn all_hosts<'a>(
    ssh_hosts: &'a [SshHostEntry],
    saved_profiles: &'a [SavedHostProfile],
) -> impl Iterator<Item = ManagedHost<'a>> {
    ssh_hosts
        .iter()
        .map(ManagedHost::Ssh)
        .chain(saved_profiles.iter().map(ManagedHost::Profile))
}

/// Resolve `--host`: stable aliases, saved-host names and IDs first, then
/// user-editable display names.
pub fn resolve_command_line_host<'a>(
    name: &str,
    ssh_hosts: &'a [SshHostEntry],
    saved_profiles: &'a [SavedHostProfile],
) -> TargetResolution<ManagedHost<'a>> {
    let target = normalized(name);
    let identity_matches = unique_hosts(
        all_hosts(ssh_hosts, saved_profiles)
            .filter(|host| match host {
                ManagedHost::Ssh(entry) => entry
                    .aliases
                    .iter()
                    .any(|alias| normalized(alias) == target),
                ManagedHost::Profile(entry) => {
                    normalized(&entry.name) == target || normalized(&entry.id) == target
                }
            })
            .collect(),
    );
    if !identity_matches.is_empty() {
        return result_for(identity_matches);
    }

    result_for(unique_hosts(
        all_hosts(ssh_hosts, saved_profiles)
            .filter(|host| {
                let display_name = match host {
                    ManagedHost::Ssh(entry) => entry
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.display_name.as_deref()),
                    ManagedHost::Profile(entry) => entry.metadata.display_name.as_deref(),
                };
                normalized(display_name.unwrap_or("")) == target
            })
            .collect(),
    ))
}

/// Resolve `--folder`: a full folder path, then an ssh_config file group's
/// label or file name, then a unique folder name.
pub fn resolve_command_line_folder<'a>(
    name: &str,
    ssh_hosts: &'a [SshHostEntry],
    saved_profiles: &'a [SavedHostProfile],
    files: &[String],
    root_file: Option<&str>,
) -> TargetResolution<ResolvedHostFolder<'a>> {
    let target = normalized(name);
    let tree = HostTree::build(ssh_hosts, saved_profiles, files, root_file);
    let folder_result = |matches: Vec<usize>| match result_for(matches) {
        TargetResolution::Found(folder) => TargetResolution::Found(ResolvedHostFolder {
            label: tree.folders[folder].path.clone(),
            hosts: tree.folder_hosts(folder),
        }),
        TargetResolution::NotFound => TargetResolution::NotFound,
        TargetResolution::Ambiguous { count } => TargetResolution::Ambiguous { count },
    };

    let path_matches: Vec<usize> = (0..tree.folders.len())
        .filter(|&folder| normalized(&tree.folders[folder].path) == target)
        .collect();
    if !path_matches.is_empty() {
        return folder_result(path_matches);
    }

    let file_matches: Vec<_> = tree
        .file_groups
        .iter()
        .filter(|group| {
            let filename = group.tooltip.as_deref().map(|tooltip| {
                strip_config_extension(tooltip.rsplit(['/', '\\']).next().unwrap_or(tooltip), true)
            });
            normalized(&group.label) == target
                || filename
                    .is_some_and(|filename| !filename.is_empty() && normalized(filename) == target)
        })
        .collect();
    if !file_matches.is_empty() {
        return match result_for(file_matches) {
            TargetResolution::Found(group) => TargetResolution::Found(ResolvedHostFolder {
                label: group.label.clone(),
                hosts: group.hosts.iter().map(|&index| tree.hosts[index]).collect(),
            }),
            TargetResolution::NotFound => TargetResolution::NotFound,
            TargetResolution::Ambiguous { count } => TargetResolution::Ambiguous { count },
        };
    }

    folder_result(
        (0..tree.folders.len())
            .filter(|&folder| normalized(&tree.folders[folder].label) == target)
            .collect(),
    )
}

/// Resolve `--workspace`: IDs before names.
pub fn resolve_command_line_workspace<'a>(
    name: &str,
    workspaces: &'a [WorkspaceSummary],
) -> TargetResolution<&'a WorkspaceSummary> {
    let target = normalized(name);
    let id_matches: Vec<_> = workspaces
        .iter()
        .filter(|workspace| normalized(&workspace.id) == target)
        .collect();
    if !id_matches.is_empty() {
        return result_for(id_matches);
    }
    result_for(
        workspaces
            .iter()
            .filter(|workspace| normalized(&workspace.name) == target)
            .collect(),
    )
}

/// What an ssh:// or telnet:// link opens.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionLinkLaunch<'a> {
    Host {
        host: ManagedHost<'a>,
        profile: SessionProfile,
    },
    AdHoc {
        profile: SessionProfile,
        title: String,
    },
}

struct LinkCandidate<'a> {
    host: ManagedHost<'a>,
    /// Matched by alias or saved-host name rather than by address.
    identity: bool,
    user: Option<&'a str>,
}

fn default_port(scheme: ConnectionLinkScheme) -> u16 {
    match scheme {
        ConnectionLinkScheme::Ssh => 22,
        ConnectionLinkScheme::Telnet => 23,
    }
}

fn link_candidates<'a>(
    link: &ConnectionLink,
    ssh_hosts: &'a [SshHostEntry],
    saved_profiles: &'a [SavedHostProfile],
) -> Vec<LinkCandidate<'a>> {
    let host = normalized(&link.host);
    let port = link.port.unwrap_or(default_port(link.scheme));
    let mut candidates = Vec::new();
    if link.scheme == ConnectionLinkScheme::Ssh {
        for entry in ssh_hosts {
            let identity = entry.aliases.iter().any(|alias| normalized(alias) == host);
            // An alias keeps working with another user or port, like `ssh -p`.
            if identity
                || (normalized(&entry.resolved.hostname) == host && entry.resolved.port == port)
            {
                candidates.push(LinkCandidate {
                    host: ManagedHost::Ssh(entry),
                    identity,
                    user: entry.resolved.user.as_deref(),
                });
            }
        }
    }
    for entry in saved_profiles {
        let (address, saved_port, user) = match (&entry.profile, link.scheme) {
            (SavedHostSessionProfile::Ssh(profile), ConnectionLinkScheme::Ssh) => (
                profile.target.as_str(),
                profile.port.unwrap_or(22),
                profile.user.as_deref(),
            ),
            (SavedHostSessionProfile::Telnet(profile), ConnectionLinkScheme::Telnet) => {
                (profile.host.as_str(), profile.port, None)
            }
            _ => continue,
        };
        let identity = normalized(&entry.name) == host;
        if !identity && !(normalized(address) == host && saved_port == port) {
            continue;
        }
        // A saved Muxus host always dials with its own fields, so a link that asks
        // for another user or port is not that host.
        if link.port.is_some_and(|link_port| saved_port != link_port) {
            continue;
        }
        if link.scheme == ConnectionLinkScheme::Ssh
            && link.user.is_some()
            && user != link.user.as_deref()
        {
            continue;
        }
        candidates.push(LinkCandidate {
            host: ManagedHost::Profile(entry),
            identity,
            user,
        });
    }
    candidates
}

fn link_title(link: &ConnectionLink) -> String {
    if link.scheme == ConnectionLinkScheme::Ssh {
        return connection_link_target(link);
    }
    let host = if link.host.contains(':') {
        format!("[{}]", link.host)
    } else {
        link.host.clone()
    };
    match link.port {
        Some(port) if port != default_port(ConnectionLinkScheme::Telnet) => {
            format!("{host}:{port}")
        }
        _ => host,
    }
}

/// Open a link with a saved host when it names one: by ssh_config alias or
/// saved-host name first, else by host name and port. Several matches are
/// narrowed by the link's user; a choice that is still ambiguous, or no match
/// at all, connects like quick connect instead of guessing.
pub fn resolve_connection_link<'a>(
    link: &ConnectionLink,
    ssh_hosts: &'a [SshHostEntry],
    saved_profiles: &'a [SavedHostProfile],
) -> ConnectionLinkLaunch<'a> {
    let candidates = link_candidates(link, ssh_hosts, saved_profiles);
    let identities: Vec<&LinkCandidate<'a>> = candidates
        .iter()
        .filter(|candidate| candidate.identity)
        .collect();
    let mut matches: Vec<&LinkCandidate<'a>> = if identities.is_empty() {
        candidates.iter().collect()
    } else {
        identities
    };
    if matches.len() > 1
        && let Some(link_user) = link.user.as_deref()
    {
        matches.retain(|candidate| candidate.user == Some(link_user));
    }
    let pin = link.host_key_fingerprint.clone();
    let matched = (matches.len() == 1).then(|| matches[0].host);

    match matched {
        Some(host @ ManagedHost::Profile(entry)) => {
            let mut profile = entry.profile.clone();
            profile.set_profile_id(Some(entry.id.clone()));
            if let SavedHostSessionProfile::Ssh(ssh) = &mut profile
                && pin.is_some()
            {
                ssh.host_key_fingerprint = pin;
            }
            ConnectionLinkLaunch::Host {
                host,
                profile: profile.into(),
            }
        }
        Some(host @ ManagedHost::Ssh(entry)) => {
            // Dialed like `ssh [user@]alias [-p port]`, so the link's user and port
            // win; a plain alias keeps the tab recognisable as that host.
            let resolved = &entry.resolved;
            let link_host = normalized(&link.host);
            let alias = entry
                .aliases
                .iter()
                .find(|candidate| normalized(candidate) == link_host)
                .unwrap_or(&entry.alias);
            let target = connection_link_target(&ConnectionLink {
                scheme: ConnectionLinkScheme::Ssh,
                host: alias.clone(),
                user: link
                    .user
                    .clone()
                    .filter(|user| Some(user) != resolved.user.as_ref()),
                port: link.port.filter(|port| *port != resolved.port),
                host_key_fingerprint: None,
            });
            ConnectionLinkLaunch::Host {
                host,
                profile: SessionProfile::Ssh(SshProfile {
                    host_key_fingerprint: pin,
                    ..SshProfile::new(target)
                }),
            }
        }
        None if link.scheme == ConnectionLinkScheme::Telnet => ConnectionLinkLaunch::AdHoc {
            profile: SessionProfile::Telnet(TelnetProfile {
                profile_id: None,
                host: link.host.clone(),
                port: link
                    .port
                    .unwrap_or(default_port(ConnectionLinkScheme::Telnet)),
            }),
            title: link_title(link),
        },
        None => ConnectionLinkLaunch::AdHoc {
            profile: SessionProfile::Ssh(SshProfile {
                host_key_fingerprint: pin,
                ..SshProfile::new(connection_link_target(link))
            }),
            title: link_title(link),
        },
    }
}

/// A link as it may appear in a notification: short, and only printable characters.
pub fn link_label(url: &str) -> String {
    static OTHER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{C}").expect("valid regex"));
    let printable = OTHER.replace_all(js::js_trim(url), "\u{FFFD}");
    if js::utf16_len(&printable) > 80 {
        format!("{}…", js::utf16_prefix(&printable, 79))
    } else {
        printable.into_owned()
    }
}

/// The notification for a name that matched nothing, or more than one thing.
pub fn resolution_error<T>(
    kind: CommandLineTargetKind,
    name: &str,
    resolution: &TargetResolution<T>,
) -> Option<String> {
    let label = kind.label();
    match resolution {
        TargetResolution::Found(_) => None,
        TargetResolution::NotFound => Some(format!("No {label} named “{name}” was found.")),
        TargetResolution::Ambiguous { .. } => Some(format!(
            "More than one {label} matches “{name}”. Use its exact {}.",
            if kind == CommandLineTargetKind::Folder {
                "path"
            } else {
                "name or ID"
            }
        )),
    }
}

/// The named-target kinds of a [`CommandLineLaunch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandLineTargetKind {
    Host,
    Folder,
    Workspace,
}

impl CommandLineTargetKind {
    fn label(self) -> &'static str {
        match self {
            CommandLineTargetKind::Host => "host",
            CommandLineTargetKind::Folder => "folder",
            CommandLineTargetKind::Workspace => "workspace",
        }
    }
}

/// What a command-line request does once the catalogs are loaded
/// (`CommandLineLaunchHandler`): the caller performs the action and shows
/// `toast`, an error toast when `action` is [`LaunchAction::Fail`].
// Short-lived and moved once; boxing the profile would only add indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum LaunchAction<'a> {
    /// Dial an ad-hoc `[user@]host[:port]` like `ssh <target>`, so ssh_config
    /// still supplies keys and jumps.
    ConnectTarget {
        target: String,
    },
    /// Open a session profile in a new tab.
    OpenProfile {
        profile: SessionProfile,
        title: String,
        color: Option<String>,
    },
    /// Connect a saved or ssh_config host the usual way.
    ConnectHost {
        host: ManagedHost<'a>,
    },
    /// Open every host of a folder in tabs; the caller reports how many
    /// sessions started with [`folder_launch_message`].
    LaunchFolder {
        folder: ResolvedHostFolder<'a>,
    },
    /// Show a workspace: the caller says whether it is the active one, open
    /// in another window, or opens it in a new window ([`workspace_message`]).
    OpenWorkspace {
        workspace: &'a WorkspaceSummary,
    },
    Fail,
}

/// The catalogs a request is resolved against.
#[derive(Debug, Clone, Copy)]
pub struct LaunchCatalogs<'a> {
    pub ssh_hosts: &'a [SshHostEntry],
    pub saved_profiles: &'a [SavedHostProfile],
    /// Every ssh_config file, root first.
    pub files: &'a [String],
    /// The root ssh_config file.
    pub root_file: Option<&'a str>,
    pub workspaces: &'a [WorkspaceSummary],
}

/// Whether a request needs the host catalog before it can run.
pub fn needs_host_catalog(request: &CommandLineLaunch) -> bool {
    matches!(
        request,
        CommandLineLaunch::Host { .. }
            | CommandLineLaunch::Folder { .. }
            | CommandLineLaunch::Url { .. }
    )
}

/// Shown when the host catalog failed to load for a request that needs it.
pub const HOST_CATALOG_ERROR: &str =
    "Could not load the host catalog for the command-line request.";

/// Shown when performing the action failed without a message of its own.
pub const LAUNCH_FAILED: &str = "The command-line launch failed.";

/// Decide what a request does and what to tell the user.
pub fn plan_command_line_launch<'a>(
    request: &CommandLineLaunch,
    catalogs: LaunchCatalogs<'a>,
) -> (LaunchAction<'a>, Option<String>) {
    match request {
        CommandLineLaunch::Connect { name } => (
            LaunchAction::ConnectTarget {
                target: name.clone(),
            },
            Some(format!("Connecting to “{name}”.")),
        ),
        CommandLineLaunch::Url { name } => match parse_connection_link(name) {
            Err(error) => (
                LaunchAction::Fail,
                Some(format!("Cannot open “{}”. {error}", link_label(name))),
            ),
            Ok(link) => {
                match resolve_connection_link(&link, catalogs.ssh_hosts, catalogs.saved_profiles) {
                    ConnectionLinkLaunch::AdHoc { profile, title } => {
                        let message = format!("Connecting to “{title}”.");
                        (
                            LaunchAction::OpenProfile {
                                profile,
                                title,
                                color: None,
                            },
                            Some(message),
                        )
                    }
                    ConnectionLinkLaunch::Host { host, profile } => {
                        let name = host.display_name().to_owned();
                        let message = format!("Connecting to “{name}”.");
                        (
                            LaunchAction::OpenProfile {
                                profile,
                                title: name,
                                color: host.color().map(str::to_owned),
                            },
                            Some(message),
                        )
                    }
                }
            }
        },
        CommandLineLaunch::Host { name } => {
            match resolve_command_line_host(name, catalogs.ssh_hosts, catalogs.saved_profiles) {
                TargetResolution::Found(host) => (
                    LaunchAction::ConnectHost { host },
                    Some(format!("Connecting to “{}”.", host.display_name())),
                ),
                other => (
                    LaunchAction::Fail,
                    resolution_error(CommandLineTargetKind::Host, name, &other),
                ),
            }
        }
        CommandLineLaunch::Folder { name } => match resolve_command_line_folder(
            name,
            catalogs.ssh_hosts,
            catalogs.saved_profiles,
            catalogs.files,
            catalogs.root_file,
        ) {
            TargetResolution::Found(folder) => (LaunchAction::LaunchFolder { folder }, None),
            other => (
                LaunchAction::Fail,
                resolution_error(CommandLineTargetKind::Folder, name, &other),
            ),
        },
        CommandLineLaunch::Workspace { name } => {
            match resolve_command_line_workspace(name, catalogs.workspaces) {
                TargetResolution::Found(workspace) => {
                    (LaunchAction::OpenWorkspace { workspace }, None)
                }
                other => (
                    LaunchAction::Fail,
                    resolution_error(CommandLineTargetKind::Workspace, name, &other),
                ),
            }
        }
    }
}

/// The success toast after a folder launch started `started` sessions; none
/// when nothing started.
pub fn folder_launch_message(label: &str, started: usize) -> Option<String> {
    (started > 0).then(|| {
        format!(
            "Launching {started} session{} from “{label}”.",
            if started == 1 { "" } else { "s" }
        )
    })
}

/// Where a resolved workspace ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceLaunchOutcome {
    /// It is the active workspace of the receiving window.
    AlreadyOpen,
    /// Another window shows it and was brought to the front.
    Focused,
    /// It was opened in a new window.
    NewWindow,
}

pub fn workspace_message(name: &str, outcome: WorkspaceLaunchOutcome) -> String {
    match outcome {
        WorkspaceLaunchOutcome::AlreadyOpen => format!("Workspace “{name}” is already open."),
        WorkspaceLaunchOutcome::Focused => format!("Switched to the window showing “{name}”."),
        WorkspaceLaunchOutcome::NewWindow => {
            format!("Opening workspace “{name}” in a new window.")
        }
    }
}
