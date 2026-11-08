//! What a host set to "Use system OpenSSH" cannot do (`client/src/system-openssh.ts`).

use muxus_core::api::TunnelRecord;

use super::managed::ManagedHost;
use crate::model::SshHostEntry;

/// What a greyed-out control says when a system OpenSSH host cannot do it.
pub const SYSTEM_OPENSSH_UNAVAILABLE: &str = "Not available with system OpenSSH";
pub const SYSTEM_OPENSSH_WINDOWS_UNAVAILABLE: &str = "Not available with system OpenSSH on Windows";

/// Says so under a menu item that a system OpenSSH session cannot use.
pub trait SystemOpenSshMenuItem: Sized {
    fn unavailable_with_system_open_ssh(self, system_open_ssh: bool) -> Self;
}

impl SystemOpenSshMenuItem for muxus_ui::widgets::MenuItem {
    fn unavailable_with_system_open_ssh(self, system_open_ssh: bool) -> Self {
        if system_open_ssh {
            self.detail(SYSTEM_OPENSSH_UNAVAILABLE)
        } else {
            self
        }
    }
}

/// A greyed-out action's name, for controls that show only one line of text.
pub fn unavailable_with_system_open_ssh(label: &str) -> String {
    format!("{label} (not available with system OpenSSH)")
}

pub fn managed_host_uses_system_open_ssh(host: &ManagedHost) -> bool {
    match host {
        ManagedHost::Ssh(entry) => entry
            .metadata
            .as_ref()
            .is_some_and(|m| m.system_open_ssh == Some(true)),
        ManagedHost::Profile(entry) => entry.metadata.system_open_ssh == Some(true),
    }
}

/// An ssh_config alias set to use system OpenSSH.
pub fn alias_uses_system_open_ssh(alias: &str, hosts: &[SshHostEntry]) -> bool {
    hosts.iter().any(|host| {
        host.aliases.iter().any(|a| a == alias)
            && host
                .metadata
                .as_ref()
                .is_some_and(|m| m.system_open_ssh == Some(true))
    })
}

/// A tunnel linked to a host's config dials that host, which needs the built-in client.
pub fn tunnel_uses_system_open_ssh(tunnel: &TunnelRecord, hosts: &[SshHostEntry]) -> bool {
    tunnel.ssh_options.is_none() && alias_uses_system_open_ssh(&tunnel.target, hosts)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::hosts::managed::managed_host_supports_sftp;
    use crate::model::{HostMetadata, SavedHostProfile};

    fn entry(metadata: HostMetadata) -> SshHostEntry {
        SshHostEntry {
            aliases: vec!["krb".into(), "krb2".into()],
            metadata: Some(metadata),
            ..crate::hosts::test_support::ssh_host("krb", None)
        }
    }

    fn openssh(metadata: HostMetadata) -> ManagedHost {
        ManagedHost::Ssh(Arc::new(entry(metadata)))
    }

    fn saved(metadata: HostMetadata) -> ManagedHost {
        let mut profile: SavedHostProfile = serde_json::from_value(serde_json::json!({
            "id": "p1", "kind": "ssh", "name": "krb",
            "profile": { "kind": "ssh", "target": "krb" },
            "metadata": { "profileId": "p1", "connectCount": 0 },
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        profile.metadata = metadata;
        ManagedHost::Profile(Arc::new(profile))
    }

    fn system() -> HostMetadata {
        HostMetadata {
            system_open_ssh: Some(true),
            ..HostMetadata::default()
        }
    }

    fn tunnel(target: &str, own_settings: bool) -> TunnelRecord {
        let mut record: TunnelRecord = serde_json::from_value(serde_json::json!({
            "id": "t1", "target": target, "type": "dynamic", "bindPort": 1080,
            "autoStart": false, "autoReconnect": false,
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        if own_settings {
            record.ssh_options = Some(Default::default());
        }
        record
    }

    #[test]
    fn recognizes_hosts_of_both_kinds() {
        assert!(managed_host_uses_system_open_ssh(&openssh(system())));
        assert!(managed_host_uses_system_open_ssh(&saved(system())));
        assert!(!managed_host_uses_system_open_ssh(&openssh(
            HostMetadata::default()
        )));
        assert!(!managed_host_uses_system_open_ssh(&saved(
            HostMetadata::default()
        )));
    }

    #[test]
    fn greys_a_tunnel_only_when_it_dials_the_host_through_its_config() {
        let hosts = [entry(system())];
        assert!(tunnel_uses_system_open_ssh(&tunnel("krb", false), &hosts));
        assert!(tunnel_uses_system_open_ssh(&tunnel("krb2", false), &hosts));
        assert!(!tunnel_uses_system_open_ssh(
            &tunnel("other", false),
            &hosts
        ));
        // A tunnel with its own connection settings does not touch the host's mode.
        assert!(!tunnel_uses_system_open_ssh(&tunnel("krb", true), &hosts));
    }

    #[test]
    fn ignores_console_compatibility_which_only_shapes_the_built_in_client() {
        let compat = HostMetadata {
            console_compatibility: Some(true),
            ..HostMetadata::default()
        };
        let both = HostMetadata {
            system_open_ssh: Some(true),
            ..compat.clone()
        };
        assert!(!managed_host_supports_sftp(&openssh(compat.clone())));
        assert!(managed_host_supports_sftp(&openssh(both.clone())));
        assert!(!managed_host_supports_sftp(&saved(compat)));
        assert!(managed_host_supports_sftp(&saved(both)));
        let off = HostMetadata {
            disable_sftp: Some(true),
            system_open_ssh: Some(true),
            ..HostMetadata::default()
        };
        assert!(!managed_host_supports_sftp(&openssh(off)));
    }
}
