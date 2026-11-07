//! What Muxus knows about the containers, clusters and VMs of each host
//! (`client/src/environments.ts` and `state/environments.ts`): the hosts that
//! can be read, their entries, and the summaries the sidebar and the launcher
//! show. Pure data; [`super::hub`] does the reading.

use std::collections::HashMap;

use muxus_core::environments::{
    ContainerInfo, ContainerRuntime, EnvironmentProblem, HostEnvironment, KubernetesPod,
};

use crate::hosts::managed::{ManagedHost, editable_managed_host_for_profile};
use crate::model::{
    SavedHostProfile, SavedHostSessionProfile, SessionProfile, SshHostEntry, SshProfile,
};

/// The key of this computer's own entry. Hosts use their managed-host key.
pub const LOCAL_ENVIRONMENT_KEY: &str = "local";

/// A host whose containers, clusters and VMs Muxus can list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnvironmentHost {
    pub key: String,
    pub label: String,
    /// How sessions on this host connect; `None` for this computer.
    pub profile: Option<SshProfile>,
    pub color: Option<String>,
    /// The host's own choice to be read on connect; `None` follows Settings.
    pub discover: Option<bool>,
    /// It connects with the computer's `ssh` program, so no feature can dial it or jump through it.
    pub system_open_ssh: Option<bool>,
}

impl EnvironmentHost {
    /// `LOCAL_ENVIRONMENT_HOST`.
    pub fn local() -> Self {
        Self {
            key: LOCAL_ENVIRONMENT_KEY.into(),
            label: "This computer".into(),
            ..Self::default()
        }
    }

    pub fn is_local(&self) -> bool {
        self.profile.is_none()
    }

    pub fn uses_system_open_ssh(&self) -> bool {
        self.system_open_ssh == Some(true)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EnvironmentStatus {
    /// Being read now; earlier results, if any, stay visible.
    #[default]
    Loading,
    Ready,
    /// The host cannot run commands (a router CLI, a console server).
    Unsupported,
    Error,
    /// No live connection to read it over; connecting again reads it.
    Offline,
}

/// One host's entry (`HostEnvironmentsEntry`).
#[derive(Clone, Debug, PartialEq)]
pub struct HostEnvironmentsEntry {
    pub host: EnvironmentHost,
    pub status: EnvironmentStatus,
    pub message: Option<String>,
    pub environments: Vec<HostEnvironment>,
    pub problems: Vec<EnvironmentProblem>,
    /// Milliseconds since the epoch.
    pub sampled_at: Option<u64>,
    /// Read without being asked, on connect.
    pub automatic: bool,
}

impl HostEnvironmentsEntry {
    fn new(host: EnvironmentHost) -> Self {
        Self {
            host,
            status: EnvironmentStatus::Loading,
            message: None,
            environments: Vec::new(),
            problems: Vec::new(),
            sampled_at: None,
            automatic: true,
        }
    }

    pub fn key(&self) -> &str {
        &self.host.key
    }
}

/// A partial update of an entry (`Partial<HostEnvironmentsEntry>`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntryPatch {
    pub status: Option<EnvironmentStatus>,
    pub message: Option<Option<String>>,
    pub environments: Option<Vec<HostEnvironment>>,
    pub problems: Option<Vec<EnvironmentProblem>>,
    pub sampled_at: Option<Option<u64>>,
    pub automatic: Option<bool>,
}

impl EntryPatch {
    pub fn status(status: EnvironmentStatus, message: Option<String>) -> Self {
        Self {
            status: Some(status),
            message: Some(message),
            ..Self::default()
        }
    }
}

/// The entries, in the order hosts were found (`useEnvironmentsStore`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnvironmentsStore {
    entries: HashMap<String, HostEnvironmentsEntry>,
    order: Vec<String>,
}

impl EnvironmentsStore {
    pub fn get(&self, key: &str) -> Option<&HostEnvironmentsEntry> {
        self.entries.get(key)
    }

    /// The entries in the order they were found.
    pub fn entries(&self) -> impl Iterator<Item = &HostEnvironmentsEntry> {
        self.order.iter().filter_map(|key| self.entries.get(key))
    }

    pub fn keys(&self) -> Vec<String> {
        self.order.clone()
    }

    /// Create or update the entry of `host`: the host's own fields are refreshed and `patch`
    /// applied on top.
    pub fn update(&mut self, host: &EnvironmentHost, patch: EntryPatch) {
        if !self.entries.contains_key(&host.key) {
            self.order.push(host.key.clone());
        }
        let entry = self
            .entries
            .entry(host.key.clone())
            .or_insert_with(|| HostEnvironmentsEntry::new(host.clone()));
        entry.host = host.clone();
        if let Some(status) = patch.status {
            entry.status = status;
        }
        if let Some(message) = patch.message {
            entry.message = message;
        }
        if let Some(environments) = patch.environments {
            entry.environments = environments;
        }
        if let Some(problems) = patch.problems {
            entry.problems = problems;
        }
        if let Some(sampled_at) = patch.sampled_at {
            entry.sampled_at = sampled_at;
        }
        if let Some(automatic) = patch.automatic {
            entry.automatic = automatic;
        }
    }

    pub fn remove(&mut self, key: &str) {
        if self.entries.remove(key).is_some() {
            self.order.retain(|candidate| candidate != key);
        }
    }
}

/// The SSH host a sidebar entry stands for; other protocols run no commands
/// (`environmentHostForManaged`).
pub fn environment_host_for_managed(host: &ManagedHost) -> Option<EnvironmentHost> {
    match host {
        ManagedHost::Ssh(entry) => Some(EnvironmentHost {
            key: host.key(),
            label: host.display_name().to_string(),
            profile: Some(SshProfile::new(entry.alias.clone())),
            color: entry.metadata.as_ref().and_then(|m| m.color.clone()),
            discover: entry
                .metadata
                .as_ref()
                .and_then(|m| m.discover_environments),
            system_open_ssh: entry.metadata.as_ref().and_then(|m| m.system_open_ssh),
        }),
        ManagedHost::Profile(entry) => {
            let SavedHostSessionProfile::Ssh(profile) = &entry.profile else {
                return None;
            };
            Some(EnvironmentHost {
                key: host.key(),
                label: host.display_name().to_string(),
                profile: Some(SshProfile {
                    profile_id: Some(entry.id.clone()),
                    ..profile.clone()
                }),
                color: entry.metadata.color.clone(),
                discover: entry.metadata.discover_environments,
                system_open_ssh: entry.metadata.system_open_ssh,
            })
        }
    }
}

/// The host a live tab is connected to: a sidebar host, or an ad-hoc target
/// (`environmentHostForProfile`).
pub fn environment_host_for_profile(
    profile: &SessionProfile,
    ssh_hosts: &[SshHostEntry],
    saved_profiles: &[SavedHostProfile],
) -> Option<EnvironmentHost> {
    let SessionProfile::Ssh(ssh) = profile else {
        return None;
    };
    if let Some(managed) = editable_managed_host_for_profile(profile, ssh_hosts, saved_profiles) {
        return environment_host_for_managed(&managed);
    }
    if ssh.profile_id.is_some() || ssh.use_config == Some(false) {
        return None;
    }
    Some(EnvironmentHost {
        key: format!("target:{}", ssh.target),
        label: ssh.target.clone(),
        profile: Some(SshProfile::new(ssh.target.clone())),
        ..EnvironmentHost::default()
    })
}

/// What discovery needs to know about one tab of the window.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveTab {
    pub profile: Option<SessionProfile>,
    pub connected: bool,
    pub conn_id: Option<String>,
}

fn same_host(profile: Option<&SessionProfile>, host: &EnvironmentHost) -> bool {
    let (Some(SessionProfile::Ssh(profile)), Some(target)) = (profile, &host.profile) else {
        return false;
    };
    match &target.profile_id {
        Some(id) => profile.profile_id.as_deref() == Some(id.as_str()),
        None => profile.profile_id.is_none() && profile.target == target.target,
    }
}

/// A transport to the host some tab already holds (`liveConnectionFor`).
pub fn live_connection_for(tabs: &[LiveTab], host: &EnvironmentHost) -> Option<String> {
    tabs.iter()
        .find(|tab| tab.connected && tab.conn_id.is_some() && same_host(tab.profile.as_ref(), host))
        .and_then(|tab| tab.conn_id.clone())
}

/// Whether a host is read without being asked: its own choice, else the global setting.
pub fn discovery_enabled_for(host: &EnvironmentHost, global: bool) -> bool {
    host.discover.unwrap_or(global)
}

/// The hosts the window's connected SSH tabs reach, by key.
pub fn connected_hosts(
    tabs: &[LiveTab],
    ssh_hosts: &[SshHostEntry],
    saved_profiles: &[SavedHostProfile],
) -> Vec<EnvironmentHost> {
    let mut hosts: Vec<EnvironmentHost> = Vec::new();
    for tab in tabs {
        let Some(profile @ SessionProfile::Ssh(_)) = &tab.profile else {
            continue;
        };
        if !tab.connected || tab.conn_id.is_none() {
            continue;
        }
        if let Some(host) = environment_host_for_profile(profile, ssh_hosts, saved_profiles)
            && !hosts.iter().any(|known| known.key == host.key)
        {
            hosts.push(host);
        }
    }
    hosts
}

/// The keys `forgetDisconnectedHosts` drops: hosts nobody is connected to, except
/// this computer, `keep` (the host the panel is open on) and hosts being read.
pub fn disconnected_keys(
    store: &EnvironmentsStore,
    keep: Option<&str>,
    connected: impl Fn(&EnvironmentHost) -> bool,
    reading: impl Fn(&str) -> bool,
) -> Vec<String> {
    store
        .entries()
        .filter(|entry| {
            let key = entry.key();
            Some(key) != keep && !reading(key) && !entry.host.is_local() && !connected(&entry.host)
        })
        .map(|entry| entry.key().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// What a host runs, summarized for badges and the launcher
// ---------------------------------------------------------------------------

/// The tools whose logos Muxus shows (`EnvironmentBadgeKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EnvironmentKind {
    Docker,
    Podman,
    Containerlab,
    Kubernetes,
    Proxmox,
    Libvirt,
}

impl EnvironmentKind {
    pub fn runtime(runtime: ContainerRuntime) -> Self {
        match runtime {
            ContainerRuntime::Docker => EnvironmentKind::Docker,
            ContainerRuntime::Podman => EnvironmentKind::Podman,
        }
    }

    /// `ENVIRONMENT_KINDS[kind].label`.
    pub fn label(self) -> &'static str {
        match self {
            EnvironmentKind::Docker => "Docker",
            EnvironmentKind::Podman => "Podman",
            EnvironmentKind::Containerlab => "Containerlab",
            EnvironmentKind::Kubernetes => "Kubernetes",
            EnvironmentKind::Proxmox => "Proxmox VE",
            EnvironmentKind::Libvirt => "libvirt",
        }
    }

    /// The kind of one discovered environment.
    pub fn of(environment: &HostEnvironment) -> Self {
        match environment {
            HostEnvironment::Containers(containers) => Self::runtime(containers.runtime),
            HostEnvironment::Containerlab(_) => EnvironmentKind::Containerlab,
            HostEnvironment::Kubernetes(_) => EnvironmentKind::Kubernetes,
            HostEnvironment::Proxmox(_) => EnvironmentKind::Proxmox,
            HostEnvironment::Libvirt(_) => EnvironmentKind::Libvirt,
        }
    }
}

/// One kind of thing a host runs, counting what is up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentBadge {
    pub kind: EnvironmentKind,
    pub count: usize,
    pub label: String,
}

pub fn is_running(state: &str) -> bool {
    let normalized = state.to_lowercase();
    normalized == "running" || normalized == "up" || normalized.starts_with("up ")
}

/// Containers of a runtime that are not a lab's nodes; those are listed under their lab.
pub fn plain_containers(containers: &[ContainerInfo]) -> Vec<&ContainerInfo> {
    containers
        .iter()
        .filter(|container| container.containerlab.is_none())
        .collect()
}

pub fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// One badge per kind of thing the host runs, counting what is up (`environmentBadges`).
pub fn environment_badges(environments: &[HostEnvironment]) -> Vec<EnvironmentBadge> {
    let mut badges = Vec::new();
    for environment in environments {
        match environment {
            HostEnvironment::Containers(containers) => {
                let running = plain_containers(&containers.containers)
                    .into_iter()
                    .filter(|c| is_running(&c.state))
                    .count();
                if running > 0 {
                    let kind = EnvironmentKind::runtime(containers.runtime);
                    badges.push(EnvironmentBadge {
                        kind,
                        count: running,
                        label: format!(
                            "{}: {}",
                            kind.label(),
                            plural(running, "running container", "running containers")
                        ),
                    });
                }
            }
            HostEnvironment::Containerlab(lab) => badges.push(EnvironmentBadge {
                kind: EnvironmentKind::Containerlab,
                count: lab.labs.len(),
                label: format!("Containerlab: {}", plural(lab.labs.len(), "lab", "labs")),
            }),
            HostEnvironment::Kubernetes(kube) => badges.push(EnvironmentBadge {
                kind: EnvironmentKind::Kubernetes,
                count: kube.contexts.len(),
                label: format!(
                    "Kubernetes: {}",
                    plural(kube.contexts.len(), "context", "contexts")
                ),
            }),
            // A hypervisor counts its running guests, and shows even when all are
            // stopped: they are what the host is for.
            HostEnvironment::Proxmox(pve) => {
                if pve.guests.is_empty() {
                    continue;
                }
                let running = pve.guests.iter().filter(|g| g.status == "running").count();
                badges.push(EnvironmentBadge {
                    kind: EnvironmentKind::Proxmox,
                    count: running,
                    label: format!(
                        "Proxmox: {}",
                        plural(running, "running guest", "running guests")
                    ),
                });
            }
            HostEnvironment::Libvirt(libvirt) => {
                if libvirt.domains.is_empty() {
                    continue;
                }
                let running = libvirt
                    .domains
                    .iter()
                    .filter(|d| d.state == "running")
                    .count();
                badges.push(EnvironmentBadge {
                    kind: EnvironmentKind::Libvirt,
                    count: running,
                    label: format!("libvirt: {}", plural(running, "running VM", "running VMs")),
                });
            }
        }
    }
    badges
}

/// The workload a pod belongs to: a ReplicaSet's pods belong to its Deployment (`podWorkload`).
pub fn pod_workload(pod: &KubernetesPod) -> Option<(String, String)> {
    let owner = pod.owner.as_ref()?;
    if owner.kind == "ReplicaSet"
        && let Some((deployment, hash)) = owner.name.rsplit_once('-')
        && !deployment.is_empty()
        && (5..=10).contains(&hash.len())
        && hash
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Some(("Deployment".into(), deployment.to_string()));
    }
    Some((owner.kind.clone(), owner.name.clone()))
}

/// How long ago a reading was taken (`age`).
pub fn age(sampled_at: Option<u64>, now: u64) -> Option<String> {
    let sampled_at = sampled_at?;
    if sampled_at == 0 {
        return None;
    }
    let seconds = (now.saturating_sub(sampled_at) as f64 / 1000.).round();
    if seconds < 45. {
        return Some("just now".into());
    }
    let minutes = (seconds / 60.).round();
    if minutes < 60. {
        return Some(format!("{minutes} min ago"));
    }
    Some(format!("{} h ago", (minutes / 60.).round()))
}
