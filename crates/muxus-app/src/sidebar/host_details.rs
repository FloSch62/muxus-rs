//! What a row deliberately does not draw. Jump chain, key, auth mode and
//! forwards are reference material, not things you act on from the list, so
//! they live in the row's hover card instead of as a row of grey glyphs.
//! Port of `client/src/components/sidebar/host-details.ts`.

use crate::hosts::ManagedHost;
use crate::hosts::saved::jump_hop_label;
use crate::model::{GnmiTlsMode, SavedHostProfile, SavedHostSessionProfile};
use muxus_core::protocol::SshGateway;

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn forwards_line(count: usize) -> String {
    format!(
        "{count} port forward{} on connect",
        if count > 1 { "s" } else { "" }
    )
}

/// One line for a jump route and its fallbacks: "via a → b (then c, d → e)". An alias whose
/// ProxyJump was removed from ssh_config still tries its saved fallbacks after dialing
/// directly, so that reads "Direct, then via c" (`jumpRouteLine`).
fn jump_route_line(
    route: &[String],
    fallbacks: &[Vec<String>],
    label: &dyn Fn(&str) -> String,
) -> Option<String> {
    let chain = |hops: &[String]| {
        hops.iter()
            .map(|hop| label(hop))
            .collect::<Vec<_>>()
            .join(" → ")
    };
    let then = fallbacks
        .iter()
        .map(|route| chain(route))
        .collect::<Vec<_>>()
        .join(", ");
    match (route.is_empty(), then.is_empty()) {
        (true, true) => None,
        (true, false) => Some(format!("Direct, then via {then}")),
        (false, true) => Some(format!("via {}", chain(route))),
        (false, false) => Some(format!("via {} (then {then})", chain(route))),
    }
}

/// "a" or "a (then b, c)" for a gateway and its fallbacks.
fn gateway_chain(gateway: &SshGateway, fallbacks: Option<&[SshGateway]>) -> String {
    match fallbacks.filter(|fallbacks| !fallbacks.is_empty()) {
        Some(fallbacks) => format!(
            "{} (then {})",
            gateway.target,
            fallbacks
                .iter()
                .map(|fallback| fallback.target.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => gateway.target.clone(),
    }
}

/// The hover card's detail lines for a host, below its name and address.
pub fn host_detail_lines(
    host: &ManagedHost,
    saved_profiles: Option<&[SavedHostProfile]>,
) -> Vec<String> {
    let mut lines = Vec::new();
    let entry = match host {
        ManagedHost::Profile(entry) => entry,
        ManagedHost::Ssh(entry) => {
            if let Some(description) = entry.description.as_deref().filter(|d| !d.is_empty()) {
                lines.push(description.to_string());
            }
            let resolved = &entry.resolved;
            let fallbacks = entry
                .metadata
                .as_ref()
                .and_then(|m| m.proxy_jump_fallbacks.clone())
                .unwrap_or_default();
            if let Some(route) =
                jump_route_line(&resolved.proxy_jump, &fallbacks, &|hop| hop.to_string())
            {
                lines.push(route);
            }
            if !resolved.identity_files.is_empty() {
                let keys: Vec<_> = resolved
                    .identity_files
                    .iter()
                    .map(|f| file_name(f))
                    .collect();
                lines.push(format!("Key {}", keys.join(", ")));
            }
            if resolved.password_only {
                lines.push("Password authentication".into());
            }
            let metadata = entry.metadata.as_ref();
            if metadata.is_some_and(|m| m.system_open_ssh == Some(true)) {
                lines.push("System OpenSSH".into());
            }
            if metadata.is_some_and(|m| m.console_compatibility == Some(true)) {
                lines.push("Console compatibility".into());
            } else if metadata.is_some_and(|m| m.disable_sftp == Some(true)) {
                lines.push("SFTP disabled".into());
            }
            if !resolved.forwards.is_empty() {
                lines.push(forwards_line(resolved.forwards.len()));
            }
            return lines;
        }
    };
    match &entry.profile {
        SavedHostSessionProfile::Rdp(p) => {
            if let Some(gateway) = &p.ssh_gateway {
                lines.push(format!(
                    "via SSH gateway {}",
                    gateway_chain(gateway, p.ssh_gateway_fallbacks.as_deref())
                ));
            }
            if let Some(domain) = p.domain.as_deref().filter(|d| !d.is_empty()) {
                lines.push(format!("Domain {domain}"));
            }
            if p.share_clipboard == Some(false) {
                lines.push("Clipboard not shared".into());
            }
        }
        SavedHostSessionProfile::Vnc(p) => {
            if let Some(gateway) = &p.ssh_gateway {
                lines.push(format!(
                    "via SSH gateway {}",
                    gateway_chain(gateway, p.ssh_gateway_fallbacks.as_deref())
                ));
            }
            if p.view_only == Some(true) {
                lines.push("View only".into());
            }
            if p.share_clipboard == Some(false) {
                lines.push("Clipboard not shared".into());
            }
        }
        SavedHostSessionProfile::Gnmi(p) => {
            if let Some(gateway) = &p.ssh_gateway {
                lines.push(format!(
                    "via SSH gateway {}",
                    gateway_chain(gateway, p.ssh_gateway_fallbacks.as_deref())
                ));
            }
            lines.push(
                match p.tls {
                    Some(GnmiTlsMode::Plaintext) => "Plain text (no TLS)",
                    Some(GnmiTlsMode::SkipVerify) => "TLS, certificate not checked",
                    _ => "TLS",
                }
                .into(),
            );
            if p.cert_file.is_some() {
                lines.push("Client certificate".into());
            }
        }
        SavedHostSessionProfile::Netconf(p) => {
            if let Some(gateway) = &p.ssh_gateway {
                lines.push(format!(
                    "via SSH gateway {}",
                    gateway_chain(gateway, p.ssh_gateway_fallbacks.as_deref())
                ));
            }
        }
        SavedHostSessionProfile::Web(p) => {
            if let Some(gateway) = &p.ssh_gateway {
                lines.push(format!("via SOCKS5 on {}", gateway.target));
            }
        }
        SavedHostSessionProfile::Ssh(p) => {
            let proxy_jump = p.proxy_jump.as_deref().unwrap_or_default();
            let fallbacks = p.proxy_jump_fallbacks.as_deref().unwrap_or_default();
            if let Some(route) = jump_route_line(proxy_jump, fallbacks, &|hop| {
                jump_hop_label(hop, saved_profiles)
            }) {
                lines.push(route);
            }
            let identity_files = p.identity_files.as_deref().unwrap_or_default();
            if !identity_files.is_empty() {
                let keys: Vec<_> = identity_files.iter().map(|f| file_name(f)).collect();
                lines.push(format!("Key {}", keys.join(", ")));
            }
            if p.password_only == Some(true) {
                lines.push("Password authentication".into());
            }
            if entry.metadata.system_open_ssh == Some(true) {
                lines.push("System OpenSSH".into());
            }
            if entry.metadata.console_compatibility == Some(true) {
                lines.push("Console compatibility".into());
            } else if entry.metadata.disable_sftp == Some(true) {
                lines.push("SFTP disabled".into());
            }
            let forwards = p.forwards.as_deref().unwrap_or_default();
            if !forwards.is_empty() {
                lines.push(forwards_line(forwards.len()));
            }
        }
        SavedHostSessionProfile::Telnet(_) | SavedHostSessionProfile::Serial(_) => {}
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::test_support::ssh_host;
    use crate::model::{ConfigForward, ForwardKind};

    #[test]
    fn lists_an_ssh_config_hosts_reference_material() {
        let mut entry = ssh_host("db-primary", None);
        entry.resolved.proxy_jump = vec!["bastion".into()];
        entry.resolved.identity_files = vec!["/home/demo/.ssh/id_prod_ed25519".into()];
        entry.resolved.forwards = vec![ConfigForward {
            kind: ForwardKind::Local,
            bind_port: 15432,
            target_host: Some("127.0.0.1".into()),
            target_port: Some(5432),
        }];
        assert_eq!(
            host_detail_lines(&ManagedHost::ssh(entry), None),
            [
                "via bastion",
                "Key id_prod_ed25519",
                "1 port forward on connect"
            ]
        );
    }

    #[test]
    fn folds_fallback_routes_into_the_one_route_line_of_the_hover_card() {
        let mut entry = ssh_host("db-primary", None);
        entry.resolved.proxy_jump = vec!["edge".into()];
        entry.metadata = Some(crate::model::HostMetadata {
            proxy_jump_fallbacks: Some(vec![
                vec!["backup".into()],
                vec!["dr-1".into(), "dr-2".into()],
            ]),
            system_open_ssh: Some(true),
            ..Default::default()
        });
        let lines = host_detail_lines(&ManagedHost::ssh(entry), None);
        assert_eq!(lines[0], "via edge (then backup, dr-1 → dr-2)");
        assert_eq!(lines.iter().filter(|l| l.contains("backup")).count(), 1);
        assert!(lines.contains(&"System OpenSSH".to_string()));
    }

    #[test]
    fn reads_direct_then_the_fallbacks_when_the_route_is_gone() {
        let routes = [vec!["c".to_string()]];
        assert_eq!(
            jump_route_line(&[], &routes, &|hop| hop.to_string()).as_deref(),
            Some("Direct, then via c")
        );
        assert_eq!(jump_route_line(&[], &[], &|hop| hop.to_string()), None);
    }
}
