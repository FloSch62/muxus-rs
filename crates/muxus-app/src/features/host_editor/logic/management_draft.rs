//! gNMI and NETCONF hosts (`client/src/components/host-editor/management-draft.ts`).

use muxus_core::api::{OpenSshMetadataPatch, SavedHostProfile, SavedHostProfileInput};
use muxus_core::js::js_trim;
use muxus_core::management::{GNMI_DEFAULT_PORT, GnmiEncoding, GnmiTlsMode, NETCONF_DEFAULT_PORT};
use muxus_core::protocol::{GnmiProfile, NetconfProfile, SavedHostSessionProfile, SshGateway};

use super::desktop_draft::port_in_range;
use super::js::{has_space, js_number, non_empty};
use super::native_draft::{optional_patch, parse_host_target, text_patch};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManagementKind {
    Gnmi,
    Netconf,
}

impl ManagementKind {
    /// `DEFAULT_MANAGEMENT_PORTS`.
    pub fn default_port(self) -> u16 {
        match self {
            ManagementKind::Gnmi => GNMI_DEFAULT_PORT,
            ManagementKind::Netconf => NETCONF_DEFAULT_PORT,
        }
    }
}

/// Form state for gNMI and NETCONF hosts. One draft serves both so switching
/// the connection type while creating keeps what was typed; an empty port
/// follows the selected protocol's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagementHostDraft {
    pub name: String,
    pub group: String,
    pub color: Option<String>,
    pub host: String,
    pub port: String,
    pub username: String,
    pub ssh_gateway: Option<SshGateway>,
    pub tls: GnmiTlsMode,
    pub tls_server_name: String,
    pub ca_file: String,
    pub cert_file: String,
    pub key_file: String,
    /// `None` picks the best encoding the device offers.
    pub encoding: Option<GnmiEncoding>,
}

pub fn blank_management_draft(prefill_target: &str, group: &str) -> ManagementHostDraft {
    let target = parse_host_target(prefill_target);
    ManagementHostDraft {
        name: String::new(),
        group: group.to_owned(),
        color: None,
        host: target.host,
        port: target.port.unwrap_or_default(),
        username: target.user.unwrap_or_default(),
        ssh_gateway: None,
        tls: GnmiTlsMode::Verify,
        tls_server_name: String::new(),
        ca_file: String::new(),
        cert_file: String::new(),
        key_file: String::new(),
        encoding: None,
    }
}

impl Default for ManagementHostDraft {
    fn default() -> Self {
        blank_management_draft("", "")
    }
}

/// `managementDraftFromProfile`; `None` for other kinds (the TypeScript throws).
pub fn management_draft_from_profile(
    saved: &SavedHostProfile,
    duplicate: bool,
) -> Option<ManagementHostDraft> {
    let (kind, host, port, username, gateway, gnmi) = match &saved.profile {
        SavedHostSessionProfile::Gnmi(p) => (
            ManagementKind::Gnmi,
            &p.host,
            p.port,
            &p.username,
            &p.ssh_gateway,
            Some(p),
        ),
        SavedHostSessionProfile::Netconf(p) => (
            ManagementKind::Netconf,
            &p.host,
            p.port,
            &p.username,
            &p.ssh_gateway,
            None,
        ),
        _ => return None,
    };
    let text = |value: Option<&Option<String>>| value.and_then(Clone::clone).unwrap_or_default();
    Some(ManagementHostDraft {
        name: if duplicate {
            format!("{} copy", saved.name)
        } else {
            saved.name.clone()
        },
        group: saved.metadata.group.clone().unwrap_or_default(),
        color: saved.metadata.color.clone(),
        host: host.clone(),
        port: if port == kind.default_port() {
            String::new()
        } else {
            port.to_string()
        },
        username: username.clone().unwrap_or_default(),
        ssh_gateway: gateway.clone(),
        tls: gnmi.and_then(|p| p.tls).unwrap_or(GnmiTlsMode::Verify),
        tls_server_name: text(gnmi.map(|p| &p.tls_server_name)),
        ca_file: text(gnmi.map(|p| &p.ca_file)),
        cert_file: text(gnmi.map(|p| &p.cert_file)),
        key_file: text(gnmi.map(|p| &p.key_file)),
        encoding: gnmi.and_then(|p| p.encoding),
    })
}

pub fn management_draft_port(draft: &ManagementHostDraft, kind: ManagementKind) -> f64 {
    if js_trim(&draft.port).is_empty() {
        f64::from(kind.default_port())
    } else {
        js_number(&draft.port)
    }
}

pub fn management_draft_problem(
    draft: &ManagementHostDraft,
    kind: ManagementKind,
) -> Option<String> {
    if js_trim(&draft.name).is_empty() {
        return Some("A name is required — it labels this host in Muxus.".into());
    }
    if js_trim(&draft.host).is_empty() {
        return Some("Enter a hostname or IP address.".into());
    }
    if has_space(js_trim(&draft.host)) {
        return Some("The host cannot contain spaces.".into());
    }
    if !port_in_range(management_draft_port(draft, kind)) {
        return Some("Port must be between 1 and 65535.".into());
    }
    if kind == ManagementKind::Gnmi
        && draft.tls != GnmiTlsMode::Plaintext
        && js_trim(&draft.cert_file).is_empty() != js_trim(&draft.key_file).is_empty()
    {
        return Some("A client certificate needs both the certificate and its key file.".into());
    }
    None
}

pub fn management_draft_to_input(
    draft: &ManagementHostDraft,
    kind: ManagementKind,
    existing_id: Option<&str>,
) -> SavedHostProfileInput {
    let host = js_trim(&draft.host).to_owned();
    let port = management_draft_port(draft, kind) as u16;
    let username = non_empty(&draft.username);
    let tls = draft.tls != GnmiTlsMode::Plaintext;
    let when_tls = |text: &str| if tls { non_empty(text) } else { None };
    let profile = match kind {
        ManagementKind::Gnmi => SavedHostSessionProfile::Gnmi(GnmiProfile {
            profile_id: None,
            host,
            port,
            username,
            tls: (draft.tls != GnmiTlsMode::Verify).then_some(draft.tls),
            tls_server_name: when_tls(&draft.tls_server_name),
            ca_file: when_tls(&draft.ca_file),
            cert_file: when_tls(&draft.cert_file),
            key_file: when_tls(&draft.key_file),
            ssh_gateway: draft.ssh_gateway.clone(),
            encoding: draft.encoding,
        }),
        ManagementKind::Netconf => SavedHostSessionProfile::Netconf(NetconfProfile {
            profile_id: None,
            host,
            port,
            username,
            ssh_gateway: draft.ssh_gateway.clone(),
        }),
    };
    SavedHostProfileInput {
        id: existing_id.map(str::to_owned),
        name: js_trim(&draft.name).to_owned(),
        profile,
    }
}

/// Muxus-only metadata written right after the host itself saves.
pub fn management_draft_metadata_patch(draft: &ManagementHostDraft) -> OpenSshMetadataPatch {
    OpenSshMetadataPatch {
        group: text_patch(&draft.group),
        color: optional_patch(&draft.color),
        ..OpenSshMetadataPatch::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_both_halves_of_a_client_certificate() {
        let mut draft = blank_management_draft("admin@leaf1:57401", "Lab");
        draft.name = "leaf1".into();
        assert_eq!(management_draft_problem(&draft, ManagementKind::Gnmi), None);
        draft.cert_file = "client.pem".into();
        assert!(
            management_draft_problem(&draft, ManagementKind::Gnmi)
                .unwrap()
                .contains("both")
        );
        draft.tls = GnmiTlsMode::Plaintext;
        assert_eq!(management_draft_problem(&draft, ManagementKind::Gnmi), None);
        // NETCONF has no TLS files to pair.
        draft.tls = GnmiTlsMode::Verify;
        assert_eq!(
            management_draft_problem(&draft, ManagementKind::Netconf),
            None
        );
    }

    #[test]
    fn writes_tls_options_only_with_tls() {
        let mut draft = blank_management_draft("leaf1", "");
        draft.name = "leaf1".into();
        draft.ca_file = " ca.pem ".into();
        draft.tls = GnmiTlsMode::SkipVerify;
        draft.encoding = Some(GnmiEncoding::JsonIetf);
        assert_eq!(
            serde_json::to_value(
                management_draft_to_input(&draft, ManagementKind::Gnmi, None).profile
            )
            .unwrap(),
            serde_json::json!({
                "kind": "gnmi", "host": "leaf1", "port": 57400, "tls": "skip-verify",
                "caFile": "ca.pem", "encoding": "json_ietf",
            })
        );
        draft.tls = GnmiTlsMode::Plaintext;
        assert_eq!(
            serde_json::to_value(
                management_draft_to_input(&draft, ManagementKind::Gnmi, None).profile
            )
            .unwrap(),
            serde_json::json!({
                "kind": "gnmi", "host": "leaf1", "port": 57400, "tls": "plaintext", "encoding": "json_ietf",
            })
        );
        assert_eq!(
            serde_json::to_value(
                management_draft_to_input(&draft, ManagementKind::Netconf, None).profile
            )
            .unwrap(),
            serde_json::json!({ "kind": "netconf", "host": "leaf1", "port": 830 })
        );
    }
}
