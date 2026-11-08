//! Sessions into what a host runs (`client/src/environment-sessions.ts`): the
//! profile of a container shell, a lab node, an SSH to a guest through its
//! hypervisor, a VNC display, and the URL of a published port.

use muxus_core::environments::{
    ContainerlabLab, ContainerlabNode, EnvironmentTarget, environment_title,
};

use super::model::EnvironmentHost;
use crate::hosts::saved::saved_host_hop;
use crate::model::{LocalProfile, SessionProfile, SshGateway, SshProfile, VncProfile};
use crate::state::tabs::SessionSetEntry;

/// " · <host>" after a title, for sessions on a remote host.
pub fn host_suffix(host: &EnvironmentHost) -> String {
    if host.profile.is_some() {
        format!(" · {}", host.label)
    } else {
        String::new()
    }
}

/// The profile of a session that enters `target` on `host` (`environmentProfile`).
pub fn environment_profile(host: &EnvironmentHost, target: EnvironmentTarget) -> SessionProfile {
    match &host.profile {
        Some(profile) => SessionProfile::Ssh(SshProfile {
            environment: Some(target),
            ..profile.clone()
        }),
        None => SessionProfile::Local(LocalProfile {
            environment: Some(target),
            ..LocalProfile::default()
        }),
    }
}

/// A terminal into a container, pod, guest or console (`openEnvironmentTarget`).
pub fn environment_entry(host: &EnvironmentHost, target: EnvironmentTarget) -> SessionSetEntry {
    SessionSetEntry {
        title: format!("{}{}", environment_title(&target), host_suffix(host)),
        profile: environment_profile(host, target),
        color: host.color.clone(),
    }
}

/// How a hop through `host` is written in a ProxyJump list.
fn jump_through(host: &EnvironmentHost) -> Option<String> {
    let profile = host.profile.as_ref()?;
    Some(match &profile.profile_id {
        Some(id) => saved_host_hop(id),
        None => profile.target.clone(),
    })
}

fn bracketed(address: &str) -> String {
    if address.contains(':') {
        format!("[{address}]")
    } else {
        address.to_string()
    }
}

/// SSH to an address only the host can reach (a lab node, a VM on a private
/// bridge), jumping through the host. This computer's own addresses need no jump.
pub fn ssh_through_host(host: &EnvironmentHost, address: &str, user: Option<&str>) -> SshProfile {
    let user = user.filter(|user| !user.is_empty());
    SshProfile {
        proxy_jump: jump_through(host).map(|jump| vec![jump]),
        ..SshProfile::new(format!(
            "{}{}",
            user.map(|user| format!("{user}@")).unwrap_or_default(),
            bracketed(address)
        ))
    }
}

/// SSH to a guest's address as `user` (`openSshToAddress`).
pub fn ssh_to_address_entry(
    host: &EnvironmentHost,
    address: &str,
    name: &str,
    user: Option<&str>,
) -> SessionSetEntry {
    SessionSetEntry {
        profile: SessionProfile::Ssh(ssh_through_host(host, address, user)),
        title: format!("{name}{}", host_suffix(host)),
        color: host.color.clone(),
    }
}

/// A VNC display on the host, through the host when it is remote (`openVncOnHost`).
pub fn vnc_entry(
    host: &EnvironmentHost,
    display_host: &str,
    port: u16,
    name: &str,
) -> SessionSetEntry {
    let gateway = host.profile.as_ref().map(|profile| SshGateway {
        target: profile.target.clone(),
        profile_id: profile.profile_id.clone(),
    });
    SessionSetEntry {
        profile: SessionProfile::Vnc(VncProfile {
            profile_id: None,
            host: display_host.to_string(),
            port,
            username: None,
            ssh_gateway: gateway,
            ssh_gateway_fallbacks: None,
            resize_remote: None,
            view_only: None,
            share_clipboard: None,
        }),
        title: format!("{name}{}", host_suffix(host)),
        color: host.color.clone(),
    }
}

// ---------------------------------------------------------------------------
// Containerlab
// ---------------------------------------------------------------------------

/// Kinds that are plain Linux containers: entered with exec, not SSH.
const EXEC_KINDS: [&str; 3] = ["linux", "k8s-kind", "ext-container"];

/// Whether a lab node opens with SSH to its management address.
pub fn node_uses_ssh(node: &ContainerlabNode) -> bool {
    node.ipv4.is_some() && !EXEC_KINDS.contains(&node.kind.as_str())
}

/// Network operating systems in containerlab log in as admin unless a topology says otherwise.
pub const NODE_USER: &str = "admin";

/// The session a lab node opens (`containerlabNodeEntry`).
pub fn containerlab_node_entry(
    host: &EnvironmentHost,
    lab: &ContainerlabLab,
    node: &ContainerlabNode,
) -> SessionSetEntry {
    let title = format!("{}{}", node.name, host_suffix(host));
    if node_uses_ssh(node)
        && let Some(ipv4) = &node.ipv4
    {
        return SessionSetEntry {
            profile: SessionProfile::Ssh(ssh_through_host(host, ipv4, Some(NODE_USER))),
            title,
            color: host.color.clone(),
        };
    }
    SessionSetEntry {
        profile: environment_profile(
            host,
            EnvironmentTarget::Container {
                runtime: lab.runtime,
                container: node.container.clone(),
                shell: None,
            },
        ),
        title,
        color: host.color.clone(),
    }
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

const HTTPS_PORTS: [u16; 5] = [443, 8006, 8443, 9443, 10443];

pub fn port_scheme(port: u16) -> &'static str {
    if HTTPS_PORTS.contains(&port) {
        "https"
    } else {
        "http"
    }
}

/// The address a published port answers on, seen from its host.
pub fn loopback_for(address: Option<&str>) -> String {
    match address {
        None | Some("") | Some("0.0.0.0") | Some("::") => "127.0.0.1".into(),
        Some(address) => address.to_string(),
    }
}

/// The URL of a port on this computer.
pub fn local_port_url(scheme: &str, address: Option<&str>, port: u16) -> String {
    let target = loopback_for(address);
    let host = if target == "127.0.0.1" {
        "localhost".to_string()
    } else {
        bracketed(&target)
    };
    format!("{scheme}://{host}:{port}/")
}

/// The URL of a local forward to a remote port.
pub fn forwarded_port_url(scheme: &str, local_port: u16) -> String {
    format!("{scheme}://127.0.0.1:{local_port}/")
}

// ---------------------------------------------------------------------------
// Kubus
// ---------------------------------------------------------------------------

/// Kubus's own route for a pod's details (`kubusPodRoute`).
pub fn kubus_pod_route(context: &str, namespace: &str, pod: &str) -> String {
    let selection = format!("{context}|{namespace}|{pod}");
    format!("/r/core/v1/pods?sel={}", form_encode(&selection))
}

/// `URLSearchParams` value encoding.
fn form_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
