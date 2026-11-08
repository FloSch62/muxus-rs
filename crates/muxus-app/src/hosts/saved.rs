//! Hosts saved by Muxus rather than in ssh_config. Port of the parts of
//! `client/src/saved-hosts.ts` the sidebar needs.

use std::collections::HashSet;

use crate::hosts::search::{matches_tokens, search_tokens, searchable_text};
use crate::model::{SavedHostProfile, SavedHostSessionProfile, SshProfile};
use crate::util::locale_compare;

/// ProxyJump hops naming a saved Muxus host carry this prefix.
const SAVED_HOST_HOP_PREFIX: &str = "muxus-host:";

/// The alias a saved host is exported under, by profile id, during an export.
pub type ExportedAlias<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The ProxyJump hop that names a saved Muxus host.
pub fn saved_host_hop(profile_id: &str) -> String {
    format!("{SAVED_HOST_HOP_PREFIX}{profile_id}")
}

/// Saved Muxus SSH host a ProxyJump hop names, or `None` for ssh hops.
pub fn saved_host_hop_id(hop: &str) -> Option<&str> {
    hop.strip_prefix(SAVED_HOST_HOP_PREFIX)
        .filter(|id| !id.is_empty())
}

pub fn saved_host_display_name(profile: &SavedHostProfile) -> &str {
    profile
        .metadata
        .display_name
        .as_deref()
        .unwrap_or(&profile.name)
}

/// How a ProxyJump hop reads in the UI: saved Muxus hosts by name, others as written.
pub fn jump_hop_label(hop: &str, profiles: Option<&[SavedHostProfile]>) -> String {
    let Some(id) = saved_host_hop_id(hop) else {
        return hop.to_string();
    };
    profiles
        .and_then(|profiles| profiles.iter().find(|candidate| candidate.id == id))
        .map(|profile| saved_host_display_name(profile).to_string())
        .unwrap_or_else(|| "Deleted Muxus host".to_string())
}

/// Spell a jump list the way OpenSSH understands it. ssh cannot look up a saved
/// Muxus host, so each one becomes the alias it is exported under or, without
/// one, `user@host:port` preceded by its own jump hosts. A key cannot travel
/// in a -J list, so outside an export this is best effort.
pub fn open_ssh_jump_hops(
    hops: &[String],
    profiles: &[SavedHostProfile],
    exported_alias: Option<ExportedAlias<'_>>,
    seen: &HashSet<String>,
) -> Vec<String> {
    hops.iter()
        .flat_map(|hop| {
            let Some(id) = saved_host_hop_id(hop) else {
                return vec![hop.clone()];
            };
            if let Some(alias) = exported_alias.and_then(|lookup| lookup(id)) {
                return vec![alias];
            }
            let saved = profiles
                .iter()
                .find(|candidate| candidate.id == id)
                .map(|p| &p.profile);
            let Some(SavedHostSessionProfile::Ssh(SshProfile {
                target,
                user,
                port,
                proxy_jump,
                ..
            })) = saved
            else {
                return Vec::new();
            };
            if seen.contains(id) {
                return Vec::new();
            }
            let host = if port.is_some() && target.contains(':') {
                format!("[{target}]")
            } else {
                target.clone()
            };
            let address = format!(
                "{}{host}{}",
                user.as_deref()
                    .map(|user| format!("{user}@"))
                    .unwrap_or_default(),
                port.map(|port| format!(":{port}")).unwrap_or_default(),
            );
            let mut nested_seen = seen.clone();
            nested_seen.insert(id.to_string());
            let mut out = open_ssh_jump_hops(
                proxy_jump.as_deref().unwrap_or_default(),
                profiles,
                exported_alias,
                &nested_seen,
            );
            out.push(address);
            out
        })
        .collect()
}

pub fn saved_host_address(profile: &SavedHostProfile) -> String {
    /// `user@host:port via gateway` for the kinds reached over the network.
    fn network_address(
        host: &str,
        port: u16,
        username: Option<&str>,
        gateway: Option<&crate::model::SshGateway>,
    ) -> String {
        let address = match username.filter(|name| !name.is_empty()) {
            Some(username) => format!("{username}@{host}:{port}"),
            None => format!("{host}:{port}"),
        };
        match gateway {
            Some(gateway) => format!("{address} via {}", gateway.target),
            None => address,
        }
    }
    match &profile.profile {
        SavedHostSessionProfile::Ssh(SshProfile {
            target, user, port, ..
        }) => {
            let target = match user {
                Some(user) => format!("{user}@{target}"),
                None => target.clone(),
            };
            match port {
                Some(port) if *port != 22 => format!("{target}:{port}"),
                _ => target,
            }
        }
        SavedHostSessionProfile::Rdp(p) => network_address(
            &p.host,
            p.port,
            p.username.as_deref(),
            p.ssh_gateway.as_ref(),
        ),
        SavedHostSessionProfile::Vnc(p) => network_address(
            &p.host,
            p.port,
            p.username.as_deref(),
            p.ssh_gateway.as_ref(),
        ),
        SavedHostSessionProfile::Gnmi(p) => network_address(
            &p.host,
            p.port,
            p.username.as_deref(),
            p.ssh_gateway.as_ref(),
        ),
        SavedHostSessionProfile::Netconf(p) => network_address(
            &p.host,
            p.port,
            p.username.as_deref(),
            p.ssh_gateway.as_ref(),
        ),
        SavedHostSessionProfile::Web(p) => match &p.ssh_gateway {
            Some(gateway) => format!("{} via {}", p.url, gateway.target),
            None => p.url.clone(),
        },
        SavedHostSessionProfile::Telnet(p) => format!("{}:{}", p.host, p.port),
        SavedHostSessionProfile::Serial(p) => format!("{} · {} baud", p.path, p.baud_rate),
    }
}

/// Everything a search may look at, lower-cased.
pub fn saved_host_search_text(profile: &SavedHostProfile) -> String {
    let address = saved_host_address(profile);
    searchable_text([
        Some(profile.name.as_str()),
        profile.metadata.display_name.as_deref(),
        profile.metadata.group.as_deref(),
        Some(profile.kind.as_str()),
        Some(address.as_str()),
    ])
}

/// Match a saved profile against tokens the caller split once.
pub fn matches_saved_host(profile: &SavedHostProfile, tokens: &[&str]) -> bool {
    tokens.is_empty() || matches_tokens(&saved_host_search_text(profile), tokens)
}

pub fn filter_saved_hosts<'a>(
    profiles: &'a [SavedHostProfile],
    query: &str,
) -> Vec<&'a SavedHostProfile> {
    let normalized = query.trim().to_lowercase();
    let tokens = search_tokens(&normalized);
    let mut out: Vec<&SavedHostProfile> = profiles
        .iter()
        .filter(|profile| matches_saved_host(profile, &tokens))
        .collect();
    out.sort_by(|a, b| locale_compare(saved_host_display_name(a), saved_host_display_name(b)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HostMetadata, RdpProfile, SshGateway, TelnetProfile};

    fn telnet(host: &str) -> SavedHostSessionProfile {
        SavedHostSessionProfile::Telnet(TelnetProfile {
            profile_id: None,
            host: host.into(),
            port: 23,
        })
    }

    fn saved(id: &str, name: &str, profile: SavedHostSessionProfile) -> SavedHostProfile {
        SavedHostProfile {
            id: id.into(),
            kind: profile.kind(),
            name: name.into(),
            profile,
            metadata: HostMetadata {
                profile_id: id.into(),
                ..Default::default()
            },
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn reads_hop_ids() {
        assert_eq!(saved_host_hop_id(&saved_host_hop("abc")), Some("abc"));
        assert_eq!(saved_host_hop_id("muxus-host:"), None);
        assert_eq!(saved_host_hop_id("bastion"), None);
    }

    #[test]
    fn spells_addresses_per_kind() {
        let rdp = saved(
            "r",
            "win",
            SavedHostSessionProfile::Rdp(RdpProfile {
                profile_id: None,
                host: "win.corp".into(),
                port: 3389,
                username: Some("CORP\\ci".into()),
                ssh_gateway: Some(SshGateway {
                    target: "bastion".into(),
                    profile_id: None,
                }),
                domain: None,
                share_clipboard: None,
                ssh_gateway_fallbacks: None,
            }),
        );
        assert_eq!(
            saved_host_address(&rdp),
            "CORP\\ci@win.corp:3389 via bastion"
        );
        let telnet = saved("t", "sw", telnet("192.0.2.20"));
        assert_eq!(saved_host_address(&telnet), "192.0.2.20:23");
    }

    #[test]
    fn filters_and_sorts_by_display_name() {
        let profiles = [
            saved("b", "zeta", telnet("z")),
            saved("a", "Alpha", telnet("a")),
        ];
        let names: Vec<_> = filter_saved_hosts(&profiles, "")
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, ["Alpha", "zeta"]);
        let names: Vec<_> = filter_saved_hosts(&profiles, "telnet z")
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, ["zeta"]);
    }

    /// `saved-hosts.test.ts`, the two native hosts it searches.
    fn native_hosts() -> Vec<SavedHostProfile> {
        let mut serial = saved(
            "serial-console",
            "Rack console",
            SavedHostSessionProfile::Serial(crate::model::SerialProfile {
                profile_id: Some("serial-console".into()),
                baud_rate: 115_200,
                ..crate::model::SerialProfile::new("COM3")
            }),
        );
        serial.metadata.group = Some("Lab".into());
        let telnet = saved(
            "telnet-router",
            "Core router",
            SavedHostSessionProfile::Telnet(TelnetProfile {
                profile_id: Some("telnet-router".into()),
                host: "router.example.test".into(),
                port: 2323,
            }),
        );
        vec![serial, telnet]
    }

    fn ids(profiles: Vec<&SavedHostProfile>) -> Vec<&str> {
        profiles.iter().map(|profile| profile.id.as_str()).collect()
    }

    #[test]
    fn searches_native_hosts_by_name_kind_group_and_address() {
        let profiles = native_hosts();
        assert_eq!(
            ids(filter_saved_hosts(&profiles, "lab")),
            ["serial-console"]
        );
        assert_eq!(
            ids(filter_saved_hosts(&profiles, "telnet")),
            ["telnet-router"]
        );
        assert_eq!(
            ids(filter_saved_hosts(&profiles, "2323")),
            ["telnet-router"]
        );
    }

    #[test]
    fn sorts_by_display_name_and_formats_platform_native_addresses() {
        let profiles = native_hosts();
        assert_eq!(
            ids(filter_saved_hosts(&profiles, "")),
            ["telnet-router", "serial-console"]
        );
        assert_eq!(saved_host_address(&profiles[0]), "COM3 · 115200 baud");
        assert_eq!(saved_host_address(&profiles[1]), "router.example.test:2323");
    }

    fn ssh(
        id: &str,
        target: &str,
        user: Option<&str>,
        port: Option<u16>,
        jumps: &[&str],
    ) -> SavedHostProfile {
        saved(
            id,
            id,
            SavedHostSessionProfile::Ssh(SshProfile {
                user: user.map(Into::into),
                port,
                proxy_jump: (!jumps.is_empty())
                    .then(|| jumps.iter().map(|hop| hop.to_string()).collect()),
                ..SshProfile::new(target)
            }),
        )
    }

    /// The export helper (`openSshJumpHops`): ssh cannot look up a saved Muxus host.
    #[test]
    fn spells_jump_lists_the_way_openssh_reads_them() {
        let profiles = vec![
            ssh("edge", "edge.example.test", Some("ops"), Some(2222), &[]),
            ssh(
                "inner",
                "inner.example.test",
                None,
                None,
                &["muxus-host:edge"],
            ),
            ssh("v6", "2001:db8::1", None, Some(22), &[]),
            ssh(
                "loop",
                "loop.example.test",
                None,
                None,
                &["muxus-host:loop"],
            ),
            saved("console", "console", telnet("192.0.2.1")),
        ];
        let hops = |list: &[&str], alias: Option<ExportedAlias<'_>>| {
            let list: Vec<String> = list.iter().map(|hop| hop.to_string()).collect();
            open_ssh_jump_hops(&list, &profiles, alias, &HashSet::new())
        };
        // ssh_config hops pass through; a saved host becomes its address, preceded by
        // its own jump hosts.
        assert_eq!(
            hops(&["bastion", "muxus-host:inner"], None),
            [
                "bastion",
                "ops@edge.example.test:2222",
                "inner.example.test"
            ]
        );
        // An IPv6 target with a port is bracketed.
        assert_eq!(hops(&["muxus-host:v6"], None), ["[2001:db8::1]:22"]);
        // During an export, the alias a saved host is exported under wins.
        let exported = |id: &str| (id == "edge").then(|| "muxus-edge".to_string());
        assert_eq!(
            hops(&["muxus-host:inner"], Some(&exported)),
            ["muxus-edge", "inner.example.test"]
        );
        // A loop ends, and hosts that are gone or not SSH drop out.
        assert_eq!(hops(&["muxus-host:loop"], None), ["loop.example.test"]);
        assert!(hops(&["muxus-host:console", "muxus-host:gone"], None).is_empty());
    }
}
