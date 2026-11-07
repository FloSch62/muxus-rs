//! Quick launcher entries for what runs on the hosts read so far
//! (`client/src/environment-launcher.ts`).

use muxus_core::environments::{EnvironmentTarget, HostEnvironment, LibvirtUri, ProxmoxGuestType};

use super::model::{
    EnvironmentHost, EnvironmentKind, HostEnvironmentsEntry, is_running, plain_containers,
};
use super::sessions::{containerlab_node_entry, environment_entry, node_uses_ssh};
use crate::state::tabs::SessionSetEntry;

/// What running a launcher entry does.
#[derive(Clone, Debug, PartialEq)]
pub enum EnvironmentLaunch {
    /// Open a session: a container's shell, a lab node, a guest's console.
    Open(SessionSetEntry),
    /// Show the host in the containers and VMs panel (clusters and labs).
    ShowHost(EnvironmentHost),
}

/// One container, lab node, guest, VM or cluster the launcher can open.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentLaunchItem {
    pub id: String,
    pub kind: EnvironmentKind,
    pub label: String,
    pub detail: String,
    pub keywords: Vec<String>,
    pub launch: EnvironmentLaunch,
}

/// Everything running on the hosts read so far, ready to open from the quick
/// launcher. Only what is running is offered; clusters and labs open their
/// host in the panel (`environmentLaunchItems`).
pub fn environment_launch_items(entries: &[HostEnvironmentsEntry]) -> Vec<EnvironmentLaunchItem> {
    let mut items = Vec::new();
    for entry in entries {
        let host = &entry.host;
        let where_ = host.label.clone();
        for environment in &entry.environments {
            match environment {
                HostEnvironment::Containers(runtime) => {
                    let kind = EnvironmentKind::runtime(runtime.runtime);
                    for container in plain_containers(&runtime.containers) {
                        if !is_running(&container.state) {
                            continue;
                        }
                        let project = container
                            .compose_project
                            .as_ref()
                            .map(|project| format!(" · {project}"))
                            .unwrap_or_default();
                        items.push(EnvironmentLaunchItem {
                            id: format!(
                                "environment:{}:{}:{}",
                                host.key,
                                runtime.runtime.as_str(),
                                container.name
                            ),
                            kind,
                            label: container.name.clone(),
                            detail: format!(
                                "{where_} › {}{project} · {}",
                                kind.label(),
                                container.image
                            ),
                            keywords: vec![
                                "container".into(),
                                runtime.runtime.as_str().into(),
                                container.image.clone(),
                                container.compose_project.clone().unwrap_or_default(),
                                where_.clone(),
                            ],
                            launch: EnvironmentLaunch::Open(environment_entry(
                                host,
                                EnvironmentTarget::Container {
                                    runtime: runtime.runtime,
                                    container: container.name.clone(),
                                    shell: None,
                                },
                            )),
                        });
                    }
                }
                HostEnvironment::Containerlab(labs) => {
                    for lab in &labs.labs {
                        items.push(EnvironmentLaunchItem {
                            id: format!("environment:{}:lab:{}", host.key, lab.name),
                            kind: EnvironmentKind::Containerlab,
                            label: lab.name.clone(),
                            detail: format!(
                                "{where_} › Containerlab lab · {} nodes",
                                lab.nodes.len()
                            ),
                            keywords: vec![
                                "containerlab".into(),
                                "lab".into(),
                                "topology".into(),
                                where_.clone(),
                            ],
                            launch: EnvironmentLaunch::ShowHost(host.clone()),
                        });
                        for node in &lab.nodes {
                            if !is_running(&node.state) {
                                continue;
                            }
                            items.push(EnvironmentLaunchItem {
                                id: format!(
                                    "environment:{}:lab:{}:{}",
                                    host.key, lab.name, node.name
                                ),
                                kind: EnvironmentKind::Containerlab,
                                label: node.name.clone(),
                                detail: format!(
                                    "{where_} › {} · {}{}{}",
                                    lab.name,
                                    node.kind,
                                    node.ipv4
                                        .as_ref()
                                        .map(|ip| format!(" · {ip}"))
                                        .unwrap_or_default(),
                                    if node_uses_ssh(node) { " · SSH" } else { "" }
                                ),
                                keywords: vec![
                                    "containerlab".into(),
                                    "node".into(),
                                    lab.name.clone(),
                                    node.kind.clone(),
                                    node.ipv4.clone().unwrap_or_default(),
                                    where_.clone(),
                                ],
                                launch: EnvironmentLaunch::Open(containerlab_node_entry(
                                    host, lab, node,
                                )),
                            });
                        }
                    }
                }
                HostEnvironment::Kubernetes(kube) => {
                    for context in &kube.contexts {
                        let current = kube.current_context.as_deref() == Some(context.as_str());
                        items.push(EnvironmentLaunchItem {
                            id: format!("environment:{}:kubernetes:{context}", host.key),
                            kind: EnvironmentKind::Kubernetes,
                            label: context.clone(),
                            detail: format!(
                                "{where_} › Kubernetes context{}",
                                if current { " · current" } else { "" }
                            ),
                            keywords: vec![
                                "kubernetes".into(),
                                "k8s".into(),
                                "kubectl".into(),
                                "cluster".into(),
                                "pods".into(),
                                where_.clone(),
                            ],
                            launch: EnvironmentLaunch::ShowHost(host.clone()),
                        });
                    }
                }
                HostEnvironment::Proxmox(pve) => {
                    for guest in &pve.guests {
                        if guest.status != "running" {
                            continue;
                        }
                        let qemu = guest.guest_type == ProxmoxGuestType::Qemu;
                        items.push(EnvironmentLaunchItem {
                            id: format!("environment:{}:proxmox:{}", host.key, guest.vmid),
                            kind: EnvironmentKind::Proxmox,
                            label: guest.name.clone(),
                            detail: format!(
                                "{where_} › Proxmox {} {} · {}",
                                if qemu { "VM" } else { "CT" },
                                guest.vmid,
                                if qemu { "serial console" } else { "pct enter" }
                            ),
                            keywords: vec![
                                "proxmox".into(),
                                "vm".into(),
                                "lxc".into(),
                                guest.vmid.to_string(),
                                where_.clone(),
                            ],
                            launch: EnvironmentLaunch::Open(environment_entry(
                                host,
                                if qemu {
                                    EnvironmentTarget::ProxmoxSerial { vmid: guest.vmid }
                                } else {
                                    EnvironmentTarget::ProxmoxLxc { vmid: guest.vmid }
                                },
                            )),
                        });
                    }
                }
                HostEnvironment::Libvirt(libvirt) => {
                    for domain in &libvirt.domains {
                        if domain.state != "running" {
                            continue;
                        }
                        let target = EnvironmentTarget::LibvirtConsole {
                            domain: domain.name.clone(),
                            uri: domain.uri,
                        };
                        items.push(EnvironmentLaunchItem {
                            id: format!(
                                "environment:{}:libvirt:{}:{}",
                                host.key,
                                domain.uri.map(LibvirtUri::as_str).unwrap_or(""),
                                domain.name
                            ),
                            kind: EnvironmentKind::Libvirt,
                            label: domain.name.clone(),
                            detail: format!(
                                "{where_} › libvirt VM{} · serial console",
                                if domain.uri == Some(LibvirtUri::Session) {
                                    " (user session)"
                                } else {
                                    ""
                                }
                            ),
                            keywords: vec![
                                "libvirt".into(),
                                "virsh".into(),
                                "kvm".into(),
                                "qemu".into(),
                                "vm".into(),
                                where_.clone(),
                            ],
                            launch: EnvironmentLaunch::Open(environment_entry(host, target)),
                        });
                    }
                }
            }
        }
    }
    items
}
