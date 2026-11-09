//! RDP and VNC hosts (`client/src/components/host-editor/desktop-draft.ts`).

use muxus_core::api::{OpenSshMetadataPatch, SavedHostProfile, SavedHostProfileInput};
use muxus_core::js::js_trim;
use muxus_core::protocol::{RdpProfile, SavedHostSessionProfile, SshGateway, VncProfile};

use super::js::{has_space, js_number, non_empty};

/// The fallbacks to save (`savedGatewayFallbacks`): picked ones only, never
/// the primary again, and none without a primary. Saved hosts are told apart
/// by their ID, not by their address.
pub fn saved_gateway_fallbacks(
    gateway: Option<&SshGateway>,
    fallbacks: &[SshGateway],
) -> Option<Vec<SshGateway>> {
    let gateway = gateway?;
    let same = |a: &SshGateway, b: &SshGateway| match &a.profile_id {
        Some(id) => b.profile_id.as_ref() == Some(id),
        None => b.profile_id.is_none() && a.target == b.target,
    };
    let mut kept: Vec<SshGateway> = Vec::new();
    for fallback in fallbacks {
        if fallback.target.is_empty()
            || same(fallback, gateway)
            || kept.iter().any(|item| same(item, fallback))
        {
            continue;
        }
        kept.push(fallback.clone());
    }
    (!kept.is_empty()).then_some(kept)
}

/// `Number.isInteger(port) && port >= 1 && port <= 65535`.
pub fn port_in_range(port: f64) -> bool {
    port.is_finite() && port.fract() == 0. && (1. ..=65_535.).contains(&port)
}
use super::native_draft::{group_patch, optional_patch, parse_host_target};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DesktopKind {
    Rdp,
    Vnc,
}

impl DesktopKind {
    /// `DEFAULT_DESKTOP_PORTS`.
    pub fn default_port(self) -> u16 {
        match self {
            DesktopKind::Rdp => 3389,
            DesktopKind::Vnc => 5900,
        }
    }
}

/// Form state for Muxus-owned RDP and VNC hosts. One draft serves both kinds
/// so switching the connection type while creating keeps what was typed; an
/// empty port follows the selected protocol's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopHostDraft {
    pub name: String,
    pub group: String,
    pub color: Option<String>,
    pub host: String,
    pub port: String,
    pub username: String,
    pub domain: String,
    /// SSH host the desktop is reached through; `None` connects directly.
    pub ssh_gateway: Option<SshGateway>,
    /// Gateways tried in order when `ssh_gateway` fails.
    pub ssh_gateway_fallbacks: Vec<SshGateway>,
    pub share_clipboard: bool,
    pub resize_remote: bool,
    pub view_only: bool,
}

pub fn blank_desktop_draft(prefill_target: &str, group: &str) -> DesktopHostDraft {
    let target = parse_host_target(prefill_target);
    DesktopHostDraft {
        name: String::new(),
        group: group.to_owned(),
        color: None,
        host: target.host,
        port: target.port.unwrap_or_default(),
        username: target.user.unwrap_or_default(),
        domain: String::new(),
        ssh_gateway: None,
        ssh_gateway_fallbacks: Vec::new(),
        share_clipboard: true,
        resize_remote: false,
        view_only: false,
    }
}

impl Default for DesktopHostDraft {
    fn default() -> Self {
        blank_desktop_draft("", "")
    }
}

/// `desktopDraftFromProfile`; `None` for other kinds (the TypeScript throws).
pub fn desktop_draft_from_profile(
    saved: &SavedHostProfile,
    duplicate: bool,
) -> Option<DesktopHostDraft> {
    let name = if duplicate {
        format!("{} copy", saved.name)
    } else {
        saved.name.clone()
    };
    let group = saved.metadata.group.clone().unwrap_or_default();
    let color = saved.metadata.color.clone();
    let port = |port: u16, kind: DesktopKind| {
        if port == kind.default_port() {
            String::new()
        } else {
            port.to_string()
        }
    };
    Some(match &saved.profile {
        SavedHostSessionProfile::Rdp(profile) => DesktopHostDraft {
            name,
            group,
            color,
            host: profile.host.clone(),
            port: port(profile.port, DesktopKind::Rdp),
            username: profile.username.clone().unwrap_or_default(),
            domain: profile.domain.clone().unwrap_or_default(),
            ssh_gateway: profile.ssh_gateway.clone(),
            ssh_gateway_fallbacks: profile.ssh_gateway_fallbacks.clone().unwrap_or_default(),
            share_clipboard: profile.share_clipboard != Some(false),
            resize_remote: false,
            view_only: false,
        },
        SavedHostSessionProfile::Vnc(profile) => DesktopHostDraft {
            name,
            group,
            color,
            host: profile.host.clone(),
            port: port(profile.port, DesktopKind::Vnc),
            username: profile.username.clone().unwrap_or_default(),
            domain: String::new(),
            ssh_gateway: profile.ssh_gateway.clone(),
            ssh_gateway_fallbacks: profile.ssh_gateway_fallbacks.clone().unwrap_or_default(),
            share_clipboard: profile.share_clipboard != Some(false),
            resize_remote: profile.resize_remote == Some(true),
            view_only: profile.view_only == Some(true),
        },
        _ => return None,
    })
}

/// The port a save writes: the typed one, or the protocol's default.
pub fn desktop_draft_port(draft: &DesktopHostDraft, kind: DesktopKind) -> f64 {
    if js_trim(&draft.port).is_empty() {
        f64::from(kind.default_port())
    } else {
        js_number(&draft.port)
    }
}

pub fn desktop_draft_problem(draft: &DesktopHostDraft, kind: DesktopKind) -> Option<String> {
    if js_trim(&draft.name).is_empty() {
        return Some("A name is required — it labels this host in Muxus.".into());
    }
    if js_trim(&draft.host).is_empty() {
        return Some("Enter a hostname or IP address.".into());
    }
    if has_space(js_trim(&draft.host)) {
        return Some("The host cannot contain spaces.".into());
    }
    let port = desktop_draft_port(draft, kind);
    if !port_in_range(port) {
        return Some("Port must be between 1 and 65535.".into());
    }
    None
}

pub fn desktop_draft_to_input(
    draft: &DesktopHostDraft,
    kind: DesktopKind,
    existing_id: Option<&str>,
) -> SavedHostProfileInput {
    let host = js_trim(&draft.host).to_owned();
    let port = desktop_draft_port(draft, kind) as u16;
    let username = non_empty(&draft.username);
    let share_clipboard = (!draft.share_clipboard).then_some(false);
    let fallbacks =
        saved_gateway_fallbacks(draft.ssh_gateway.as_ref(), &draft.ssh_gateway_fallbacks);
    let profile = match kind {
        DesktopKind::Rdp => SavedHostSessionProfile::Rdp(RdpProfile {
            profile_id: None,
            host,
            port,
            username,
            domain: non_empty(&draft.domain),
            ssh_gateway: draft.ssh_gateway.clone(),
            ssh_gateway_fallbacks: fallbacks,
            share_clipboard,
        }),
        DesktopKind::Vnc => SavedHostSessionProfile::Vnc(VncProfile {
            profile_id: None,
            host,
            port,
            username,
            ssh_gateway: draft.ssh_gateway.clone(),
            resize_remote: draft.resize_remote.then_some(true),
            view_only: draft.view_only.then_some(true),
            ssh_gateway_fallbacks: fallbacks,
            share_clipboard,
        }),
    };
    SavedHostProfileInput {
        id: existing_id.map(str::to_owned),
        name: js_trim(&draft.name).to_owned(),
        profile,
    }
}

/// Muxus-only metadata written right after the host itself saves.
pub fn desktop_draft_metadata_patch(draft: &DesktopHostDraft) -> OpenSshMetadataPatch {
    OpenSshMetadataPatch {
        group: group_patch(&draft.group),
        color: optional_patch(&draft.color),
        ..OpenSshMetadataPatch::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::api::{OpenSshProfileMetadata, SavedHostKind};

    fn gateway(target: &str, profile_id: Option<&str>) -> SshGateway {
        SshGateway {
            target: target.into(),
            profile_id: profile_id.map(str::to_owned),
        }
    }

    #[test]
    fn keeps_no_fallback_gateway_without_a_first_one() {
        assert_eq!(saved_gateway_fallbacks(None, &[gateway("b", None)]), None);
        assert_eq!(
            saved_gateway_fallbacks(Some(&gateway("a", None)), &[]),
            None
        );
    }

    #[test]
    fn tells_saved_gateways_apart_by_id_not_by_their_address() {
        assert_eq!(
            saved_gateway_fallbacks(
                Some(&gateway("h", Some("p1"))),
                &[
                    gateway("h", Some("p2")),
                    gateway("h", None),
                    gateway("h", Some("p1")),
                    gateway("h", None),
                    gateway("", None),
                ],
            ),
            Some(vec![gateway("h", Some("p2")), gateway("h", None)])
        );
    }

    #[test]
    fn keeps_gateway_fallbacks_through_edit_and_duplicate() {
        let mut host = rdp_host(3389);
        if let SavedHostSessionProfile::Rdp(profile) = &mut host.profile {
            profile.ssh_gateway = Some(gateway("bastion", None));
            profile.ssh_gateway_fallbacks = Some(vec![gateway("bastion-b", None)]);
        }
        for duplicate in [false, true] {
            let draft = desktop_draft_from_profile(&host, duplicate).unwrap();
            let SavedHostSessionProfile::Rdp(saved) =
                desktop_draft_to_input(&draft, DesktopKind::Rdp, None).profile
            else {
                panic!("an RDP profile");
            };
            assert_eq!(
                saved.ssh_gateway_fallbacks,
                Some(vec![gateway("bastion-b", None)])
            );
        }
    }

    fn rdp_host(port: u16) -> SavedHostProfile {
        SavedHostProfile {
            id: "win".into(),
            kind: SavedHostKind::Rdp,
            name: "win-build".into(),
            profile: SavedHostSessionProfile::Rdp(RdpProfile {
                profile_id: Some("win".into()),
                host: "win-build.corp.internal".into(),
                port,
                username: Some("CORP\\ci".into()),
                domain: None,
                ssh_gateway: Some(SshGateway {
                    target: "bastion".into(),
                    profile_id: None,
                }),
                share_clipboard: Some(false),
                ssh_gateway_fallbacks: None,
            }),
            metadata: OpenSshProfileMetadata {
                profile_id: "win".into(),
                group: Some("Lab".into()),
                color: Some("#3b82f6".into()),
                ..OpenSshProfileMetadata::default()
            },
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn hides_the_default_port_and_restores_options() {
        let draft = desktop_draft_from_profile(&rdp_host(3389), false).unwrap();
        assert_eq!(draft.port, "");
        assert_eq!(draft.username, "CORP\\ci");
        assert!(!draft.share_clipboard);
        assert_eq!(
            draft.ssh_gateway.as_ref().map(|g| g.target.as_str()),
            Some("bastion")
        );
        assert_eq!(
            desktop_draft_from_profile(&rdp_host(3390), true)
                .unwrap()
                .port,
            "3390"
        );
        assert_eq!(
            desktop_draft_from_profile(&rdp_host(3390), true)
                .unwrap()
                .name,
            "win-build copy"
        );
    }

    #[test]
    fn saves_only_what_differs_from_the_defaults() {
        let mut draft = blank_desktop_draft("ops@vm.lab:5901", "Lab");
        draft.name = "VM".into();
        assert_eq!(draft.username, "ops");
        assert_eq!(desktop_draft_problem(&draft, DesktopKind::Vnc), None);
        draft.view_only = true;
        assert_eq!(
            serde_json::to_value(desktop_draft_to_input(&draft, DesktopKind::Vnc, None)).unwrap(),
            serde_json::json!({
                "name": "VM",
                "profile": { "kind": "vnc", "host": "vm.lab", "port": 5901, "username": "ops", "viewOnly": true },
            })
        );
        draft.port = String::new();
        let rdp = desktop_draft_to_input(&draft, DesktopKind::Rdp, Some("x"));
        assert_eq!(
            serde_json::to_value(rdp.profile).unwrap(),
            serde_json::json!({ "kind": "rdp", "host": "vm.lab", "port": 3389, "username": "ops" })
        );
    }

    #[test]
    fn validates_name_host_and_port() {
        let mut draft = blank_desktop_draft("", "");
        assert!(
            desktop_draft_problem(&draft, DesktopKind::Rdp)
                .unwrap()
                .contains("name")
        );
        draft.name = "x".into();
        assert!(
            desktop_draft_problem(&draft, DesktopKind::Rdp)
                .unwrap()
                .contains("hostname")
        );
        draft.host = "a b".into();
        assert!(
            desktop_draft_problem(&draft, DesktopKind::Rdp)
                .unwrap()
                .contains("spaces")
        );
        draft.host = "ab".into();
        draft.port = "70000".into();
        assert!(
            desktop_draft_problem(&draft, DesktopKind::Rdp)
                .unwrap()
                .contains("Port")
        );
    }
}
