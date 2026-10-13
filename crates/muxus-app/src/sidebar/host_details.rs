//! What a row deliberately does not draw. Jump chain, key, auth mode and
//! forwards are reference material, not things you act on from the list, so
//! they live in the row's hover card instead of as a row of grey glyphs.
//! Port of `client/src/components/sidebar/host-details.ts`.

use crate::hosts::ManagedHost;
use crate::hosts::saved::jump_hop_label;
use crate::model::{GnmiTlsMode, SavedHostProfile, SavedHostSessionProfile};

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn forwards_line(count: usize) -> String {
    format!(
        "{count} port forward{} on connect",
        if count > 1 { "s" } else { "" }
    )
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
            if !resolved.proxy_jump.is_empty() {
                lines.push(format!("via {}", resolved.proxy_jump.join(" → ")));
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
                lines.push(format!("via SSH gateway {}", gateway.target));
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
                lines.push(format!("via SSH gateway {}", gateway.target));
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
                lines.push(format!("via SSH gateway {}", gateway.target));
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
                lines.push(format!("via SSH gateway {}", gateway.target));
            }
        }
        SavedHostSessionProfile::Ssh(p) => {
            let proxy_jump = p.proxy_jump.as_deref().unwrap_or_default();
            if !proxy_jump.is_empty() {
                let hops: Vec<_> = proxy_jump
                    .iter()
                    .map(|hop| jump_hop_label(hop, saved_profiles))
                    .collect();
                lines.push(format!("via {}", hops.join(" → ")));
            }
            let identity_files = p.identity_files.as_deref().unwrap_or_default();
            if !identity_files.is_empty() {
                let keys: Vec<_> = identity_files.iter().map(|f| file_name(f)).collect();
                lines.push(format!("Key {}", keys.join(", ")));
            }
            if p.password_only == Some(true) {
                lines.push("Password authentication".into());
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
}
