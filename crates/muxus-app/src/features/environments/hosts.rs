//! Keeping what a host runs as a sidebar entry (`client/src/environment-hosts.ts`):
//! a copy of the host whose remote command enters a container, an SSH host
//! that jumps through its hypervisor, or a local terminal profile.

use muxus_core::environments::{EnvironmentTarget, environment_argv, environment_command_line};
use muxus_store::prefs::state::{LocalShellProfileConfig, NewSshHostStorage};

use super::model::EnvironmentHost;
use crate::features::host_editor::logic::draft::{
    EnvironmentDiscoveryDraft, HostDraft, RemoteCommandMode, RouteMode, Storage, blank_draft,
    draft_from_entry, draft_from_saved_ssh_profile,
};
use crate::features::host_editor::logic::native_draft::parse_host_target;
use crate::hosts::saved::saved_host_hop;
use crate::model::{RequestTty, SavedHostProfile, SavedHostSessionProfile, SshHostEntry};

/// The hosts the sidebar knows, to copy a parent host's connection settings from.
#[derive(Clone, Copy, Debug)]
pub struct HostDirectory<'a> {
    pub ssh_hosts: &'a [SshHostEntry],
    pub saved_profiles: &'a [SavedHostProfile],
}

enum Parent<'a> {
    OpenSsh(&'a SshHostEntry),
    Saved(&'a SavedHostProfile),
    Target(String),
}

fn parent_of<'a>(host: &EnvironmentHost, hosts: HostDirectory<'a>) -> Option<Parent<'a>> {
    let profile = host.profile.as_ref()?;
    if let Some(id) = &profile.profile_id {
        let entry = hosts.saved_profiles.iter().find(|saved| &saved.id == id)?;
        return matches!(entry.profile, SavedHostSessionProfile::Ssh(_))
            .then_some(Parent::Saved(entry));
    }
    Some(
        match hosts
            .ssh_hosts
            .iter()
            .find(|candidate| candidate.aliases.contains(&profile.target))
        {
            Some(entry) => Parent::OpenSsh(entry),
            None => Parent::Target(profile.target.clone()),
        },
    )
}

/// Host names become OpenSSH aliases: no spaces or pattern characters
/// (`name.replace(/[\s#*?!,]+/g, '-')`).
fn alias_for(name: &str) -> String {
    let mut alias = String::new();
    let mut in_run = false;
    for c in name.chars() {
        if c.is_whitespace() || matches!(c, '#' | '*' | '?' | '!' | ',') {
            if !in_run {
                alias.push('-');
            }
            in_run = true;
        } else {
            alias.push(c);
            in_run = false;
        }
    }
    alias
}

fn storage(preference: NewSshHostStorage) -> Storage {
    match preference {
        NewSshHostStorage::Muxus => Storage::Muxus,
        NewSshHostStorage::Openssh => Storage::Openssh,
    }
}

/// A new SSH host that runs `target` on `host`: a copy of the host's own
/// connection settings whose remote command enters the container. File
/// browsing and discovery would see the host, not the container, so both are off
/// (`enterHostDraft`).
pub fn enter_host_draft(
    host: &EnvironmentHost,
    hosts: HostDirectory<'_>,
    name: &str,
    target: &EnvironmentTarget,
    new_host_storage: NewSshHostStorage,
) -> Option<HostDraft> {
    let base = match parent_of(host, hosts)? {
        Parent::Saved(entry) => draft_from_saved_ssh_profile(entry, true)?,
        Parent::OpenSsh(entry) => HostDraft {
            // A block named after its host has no HostName; the copy needs one.
            hostname: entry
                .options
                .hostname
                .clone()
                .unwrap_or_else(|| entry.resolved.hostname.clone()),
            ..draft_from_entry(entry, true)
        },
        Parent::Target(target) => HostDraft {
            // A bare target only fills the alias, which the copy replaces; the
            // host it stood for has to stay as the HostName.
            hostname: parse_host_target(&target).host,
            ..blank_draft(&target, "", storage(new_host_storage))
        },
    };
    let alias_text = if base.storage == Storage::Openssh {
        alias_for(name)
    } else {
        name.to_string()
    };
    Some(HostDraft {
        alias_text,
        display_name: String::new(),
        description: String::new(),
        forwards: Vec::new(),
        remote_command_mode: RemoteCommandMode::Command,
        remote_command: environment_command_line(target).ok()?,
        request_tty: Some(RequestTty::Force),
        disable_sftp: true,
        discover_environments: EnvironmentDiscoveryDraft::Off,
        ..base
    })
}

/// Where a host found on another one is added (`addAddressToHosts` options).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JumpHostOptions {
    pub name: String,
    pub address: String,
    pub user: Option<String>,
    pub group: Option<String>,
}

/// A new SSH host for an address only `host` can reach (a lab node, a VM),
/// jumping through it. A hop to a Muxus saved host only exists in Muxus
/// storage, so such a host is stored there (`jumpHostDraft`).
pub fn jump_host_draft(
    host: &EnvironmentHost,
    hosts: HostDirectory<'_>,
    options: &JumpHostOptions,
    new_host_storage: NewSshHostStorage,
) -> HostDraft {
    let parent = parent_of(host, hosts);
    let storage = match parent {
        Some(Parent::Saved(_)) => Storage::Muxus,
        _ => storage(new_host_storage),
    };
    let hop = match &parent {
        Some(Parent::Saved(entry)) => Some(saved_host_hop(&entry.id)),
        Some(Parent::OpenSsh(entry)) => Some(entry.alias.clone()),
        Some(Parent::Target(target)) => Some(target.clone()),
        None => None,
    };
    let base = blank_draft("", options.group.as_deref().unwrap_or(""), storage);
    let mut draft = HostDraft {
        alias_text: if storage == Storage::Openssh {
            alias_for(&options.name)
        } else {
            options.name.clone()
        },
        hostname: options.address.clone(),
        user: options.user.clone().unwrap_or_default(),
        ..base
    };
    if let Some(hop) = hop {
        draft.route_mode = RouteMode::Jump;
        draft.proxy_jump = vec![hop];
    }
    // Only the ssh program can log in to this hop, so it has to be what jumps through it.
    if host.uses_system_open_ssh() {
        draft.system_open_ssh = true;
    }
    draft
}

/// A local terminal profile that runs `target` on this computer (`localEnterProfile`).
pub fn local_enter_profile(name: &str, target: &EnvironmentTarget) -> LocalShellProfileConfig {
    let argv = environment_argv(target).unwrap_or_default();
    let (shell, args) = match argv.split_first() {
        Some((shell, args)) => (shell.clone(), args.to_vec()),
        None => (String::new(), Vec::new()),
    };
    LocalShellProfileConfig {
        id: String::new(),
        name: name.to_string(),
        shell,
        args,
        cwd: String::new(),
        startup_command: String::new(),
    }
}
