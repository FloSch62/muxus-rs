//! A new gNMI or NETCONF host for the device an SSH host reaches (`managementHostPrefill` in
//! `client/src/components/host-editor/management-draft.ts`).

use muxus_core::api::{SavedHostProfile, saved_host_hop_id};
use muxus_core::protocol::{SavedHostSessionProfile, SshGateway};

use crate::features::host_editor::view::NewHost;
use crate::hosts::managed::{ManagedHost, managed_host_display_name};
use crate::model::SavedHostKind;

/// Same address and user, same folder, through the same jump host. NETCONF keeps an
/// ssh_config alias as its host so that config's keys and jump hosts still apply; otherwise the
/// last jump hop becomes the SSH gateway, which reaches its own earlier hops when it is a saved
/// host or ssh_config alias.
pub fn management_host_prefill(
    host: &ManagedHost,
    gnmi: bool,
    saved_profiles: &[SavedHostProfile],
) -> NewHost {
    let protocol = if gnmi { "gNMI" } else { "NETCONF" };
    let kind = if gnmi {
        SavedHostKind::Gnmi
    } else {
        SavedHostKind::Netconf
    };
    let prefill_name = Some(format!("{} ({protocol})", managed_host_display_name(host)));
    match host {
        ManagedHost::Ssh(entry) => {
            let resolved = &entry.resolved;
            let target = if gnmi {
                resolved.hostname.clone()
            } else {
                entry.alias.clone()
            };
            let user = resolved
                .user
                .as_deref()
                .map(|user| format!("{user}@"))
                .unwrap_or_default();
            NewHost {
                prefill_target: Some(format!("{user}{target}")),
                prefill_name,
                prefill_gateway: if gnmi {
                    jump_gateway(&resolved.proxy_jump, saved_profiles)
                } else {
                    None
                },
                group: entry.metadata.as_ref().and_then(|m| m.group.clone()),
                kind: Some(kind),
                draft: None,
            }
        }
        ManagedHost::Profile(entry) => {
            let (address, user, hops) = match &entry.profile {
                SavedHostSessionProfile::Ssh(profile) => {
                    let address = match profile.target.split_once('@') {
                        Some((_, rest)) => rest.to_string(),
                        None => profile.target.clone(),
                    };
                    let user = profile.user.clone().or_else(|| {
                        profile
                            .target
                            .split_once('@')
                            .map(|(user, _)| user.to_string())
                    });
                    (
                        address,
                        user,
                        profile.proxy_jump.clone().unwrap_or_default(),
                    )
                }
                _ => (String::new(), None, Vec::new()),
            };
            // The SSH port means nothing to gNMI or NETCONF: keep only the host.
            let parts: Vec<&str> = address.split(':').collect();
            let target = if parts.len() == 2 {
                parts[0].to_string()
            } else {
                address.clone()
            };
            let user = user.map(|user| format!("{user}@")).unwrap_or_default();
            NewHost {
                prefill_target: Some(format!("{user}{target}")),
                prefill_name,
                prefill_gateway: jump_gateway(&hops, saved_profiles),
                group: entry.metadata.group.clone(),
                kind: Some(kind),
                draft: None,
            }
        }
    }
}

/// The SSH gateway standing in for a jump list: its last hop, as a saved host when it names one.
fn jump_gateway(hops: &[String], saved_profiles: &[SavedHostProfile]) -> Option<SshGateway> {
    let last = hops.last()?;
    if last.eq_ignore_ascii_case("none") {
        return None;
    }
    let Some(id) = saved_host_hop_id(last) else {
        return Some(SshGateway {
            target: last.clone(),
            profile_id: None,
        });
    };
    saved_profiles
        .iter()
        .find(|candidate| candidate.id == id)
        .and_then(|candidate| match &candidate.profile {
            SavedHostSessionProfile::Ssh(profile) => Some(SshGateway {
                target: profile.target.clone(),
                profile_id: Some(id.to_string()),
            }),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use muxus_core::api::saved_host_hop;

    use super::*;
    use crate::hosts::test_support::ssh_host;

    fn saved_ssh(id: &str, target: &str, proxy_jump: Option<Vec<String>>) -> SavedHostProfile {
        let mut profile: SavedHostProfile = serde_json::from_value(serde_json::json!({
            "id": id, "kind": "ssh", "name": id,
            "profile": { "kind": "ssh", "target": target, "useConfig": false },
            "metadata": { "profileId": id, "connectCount": 0, "group": "Lab" },
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        if let SavedHostSessionProfile::Ssh(ssh) = &mut profile.profile {
            ssh.proxy_jump = proxy_jump;
        }
        profile
    }

    #[test]
    fn carries_an_ssh_config_hosts_jump_host_over_to_a_gnmi_host() {
        let mut entry = ssh_host("router1", None);
        entry.resolved.user = Some("admin".into());
        entry.resolved.proxy_jump = vec!["edge".into(), "bastion".into()];
        let host = ManagedHost::Ssh(Arc::new(entry));

        let gnmi = management_host_prefill(&host, true, &[]);
        assert_eq!(
            gnmi.prefill_target.as_deref(),
            Some("admin@router1.example.test")
        );
        assert_eq!(
            gnmi.prefill_gateway,
            Some(SshGateway {
                target: "bastion".into(),
                profile_id: None
            })
        );
        assert_eq!(gnmi.kind, Some(SavedHostKind::Gnmi));
        // NETCONF keeps the alias, whose config already jumps.
        let netconf = management_host_prefill(&host, false, &[]);
        assert_eq!(netconf.prefill_target.as_deref(), Some("admin@router1"));
        assert_eq!(netconf.prefill_gateway, None);
    }

    #[test]
    fn names_a_saved_jump_host_as_a_saved_gateway() {
        let bastion = saved_ssh("b1", "ops@bastion.example.test", None);
        let router = saved_ssh(
            "r1",
            "admin@10.0.0.1:2222",
            Some(vec![saved_host_hop("b1")]),
        );
        let saved = [bastion, router.clone()];
        let prefill =
            management_host_prefill(&ManagedHost::Profile(Arc::new(router)), false, &saved);
        assert_eq!(prefill.prefill_target.as_deref(), Some("admin@10.0.0.1"));
        assert_eq!(prefill.group.as_deref(), Some("Lab"));
        assert_eq!(
            prefill.prefill_gateway,
            Some(SshGateway {
                target: "ops@bastion.example.test".into(),
                profile_id: Some("b1".into())
            })
        );
        // A deleted saved hop has no gateway, and `none` means a direct connection.
        assert_eq!(jump_gateway(&[saved_host_hop("gone")], &saved), None);
        assert_eq!(jump_gateway(&["none".into()], &saved), None);
    }
}
