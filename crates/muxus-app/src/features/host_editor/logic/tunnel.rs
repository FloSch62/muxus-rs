//! The persistent tunnel editor's form (`client/src/components/TunnelEditorDialog.tsx`).

use muxus_core::api::{ForwardType, SshHostEntry, TunnelInput, TunnelRecord, TunnelSshOptions};
use muxus_core::js::js_trim;

use super::draft::{AuthMode, HostDraft, Storage, blank_draft};
use super::js::{has_space, non_empty, port_ok, to_port};

/// Where the tunnel's SSH connection comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionMode {
    /// An ssh_config alias: keys, jumps, user and port stay linked to it.
    Host,
    /// Connection settings kept with the tunnel.
    Custom,
}

/// What the dialog opens on.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TunnelEditorState {
    /// Present when editing an existing tunnel.
    pub tunnel: Option<TunnelRecord>,
    /// Preselected SSH target for new tunnels.
    pub prefill_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TunnelDraft {
    pub id: Option<String>,
    pub name: String,
    pub mode: ConnectionMode,
    pub host_target: String,
    pub custom: HostDraft,
    pub kind: ForwardType,
    pub bind_port: String,
    pub target_host: String,
    pub target_port: String,
    pub auto_start: bool,
    pub auto_reconnect: bool,
}

impl TunnelDraft {
    pub fn new(state: &TunnelEditorState) -> Self {
        let editing = state.tunnel.as_ref();
        let custom_options = editing.and_then(|t| t.ssh_options.as_ref());
        Self {
            id: editing.map(|t| t.id.clone()),
            name: editing.and_then(|t| t.name.clone()).unwrap_or_default(),
            mode: if custom_options.is_none() {
                ConnectionMode::Host
            } else {
                ConnectionMode::Custom
            },
            host_target: if custom_options.is_none() {
                editing
                    .map(|t| t.target.clone())
                    .or_else(|| state.prefill_target.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            },
            custom: custom_draft(editing, state.prefill_target.as_deref()),
            kind: editing.map(|t| t.kind).unwrap_or(ForwardType::Local),
            bind_port: editing.map(|t| t.bind_port.to_string()).unwrap_or_default(),
            target_host: editing
                .and_then(|t| t.target_host.clone())
                .unwrap_or_else(|| "localhost".into()),
            target_port: editing
                .and_then(|t| t.target_port)
                .filter(|port| *port != 0)
                .map(|port| port.to_string())
                .unwrap_or_default(),
            auto_start: editing.is_some_and(|t| t.auto_start),
            auto_reconnect: editing.is_some_and(|t| t.auto_reconnect),
        }
    }

    pub fn editing(&self) -> bool {
        self.id.is_some()
    }

    pub fn connection_ok(&self) -> bool {
        match self.mode {
            ConnectionMode::Host => !js_trim(&self.host_target).is_empty(),
            ConnectionMode::Custom => {
                let host = js_trim(&self.custom.alias_text);
                !host.is_empty()
                    && !has_space(host)
                    && (self.custom.port.is_empty() || port_ok(&self.custom.port))
                    && (self.custom.auth_mode != AuthMode::Key
                        || self
                            .custom
                            .identity_files
                            .iter()
                            .any(|f| !js_trim(f).is_empty()))
            }
        }
    }

    /// Whether Save may run.
    pub fn valid(&self) -> bool {
        self.connection_ok()
            && port_ok(&self.bind_port)
            && (self.kind == ForwardType::Dynamic
                || (!js_trim(&self.target_host).is_empty() && port_ok(&self.target_port)))
    }

    /// The SSH server the diagram names.
    pub fn server_label(&self) -> String {
        let label = match self.mode {
            ConnectionMode::Host => js_trim(&self.host_target),
            ConnectionMode::Custom => js_trim(&self.custom.alias_text),
        };
        if label.is_empty() {
            "SSH server".into()
        } else {
            label.to_owned()
        }
    }

    pub fn to_input(&self) -> TunnelInput {
        let dynamic = self.kind == ForwardType::Dynamic;
        TunnelInput {
            id: self.id.clone(),
            name: non_empty(&self.name),
            target: match self.mode {
                ConnectionMode::Host => js_trim(&self.host_target).to_owned(),
                ConnectionMode::Custom => js_trim(&self.custom.alias_text).to_owned(),
            },
            ssh_options: (self.mode == ConnectionMode::Custom)
                .then(|| custom_ssh_options(&self.custom)),
            kind: self.kind,
            bind_port: to_port(&self.bind_port).unwrap_or(0),
            target_host: (!dynamic).then(|| js_trim(&self.target_host).to_owned()),
            target_port: if dynamic {
                None
            } else {
                to_port(&self.target_port)
            },
            auto_start: Some(self.auto_start),
            auto_reconnect: Some(self.auto_reconnect),
        }
    }

    /// The save button that also starts the tunnel.
    pub fn start_label(&self) -> &'static str {
        if self.editing() {
            "Save & restart"
        } else {
            "Save & start"
        }
    }

    pub fn title(&self) -> &'static str {
        if self.editing() {
            "Edit persistent tunnel"
        } else {
            "New persistent tunnel"
        }
    }
}

fn custom_draft(tunnel: Option<&TunnelRecord>, prefill_target: Option<&str>) -> HostDraft {
    let options = tunnel.and_then(|t| t.ssh_options.as_ref());
    let target = match options {
        None => prefill_target
            .map(str::to_owned)
            .or_else(|| tunnel.map(|t| t.target.clone()))
            .unwrap_or_default(),
        Some(_) => tunnel.map(|t| t.target.clone()).unwrap_or_default(),
    };
    let draft = blank_draft(&target, "", Storage::Openssh);
    let Some(options) = options else {
        return draft;
    };
    HostDraft {
        alias_text: tunnel.map(|t| t.target.clone()).unwrap_or_default(),
        user: options.user.clone().unwrap_or_default(),
        port: options.port.map(|p| p.to_string()).unwrap_or_default(),
        auth_mode: if options.password_only == Some(true) {
            AuthMode::Password
        } else if options
            .identity_files
            .as_ref()
            .is_some_and(|f| !f.is_empty())
        {
            AuthMode::Key
        } else {
            AuthMode::Default
        },
        identity_files: options.identity_files.clone().unwrap_or_default(),
        identities_only: options.identities_only.unwrap_or(false),
        forward_agent: options.forward_agent.unwrap_or(false),
        proxy_jump: options.proxy_jump.clone().unwrap_or_default(),
        ..draft
    }
}

fn custom_ssh_options(draft: &HostDraft) -> TunnelSshOptions {
    let key = draft.auth_mode == AuthMode::Key;
    TunnelSshOptions {
        user: non_empty(&draft.user),
        port: if draft.port.is_empty() {
            None
        } else {
            to_port(&draft.port)
        },
        identity_files: key.then(|| {
            draft
                .identity_files
                .iter()
                .map(|f| js_trim(f).to_owned())
                .filter(|f| !f.is_empty())
                .collect()
        }),
        identities_only: (key && draft.identities_only).then_some(true),
        forward_agent: draft.forward_agent.then_some(true),
        proxy_jump: (!draft.proxy_jump.is_empty()).then(|| draft.proxy_jump.clone()),
        password_only: (draft.auth_mode == AuthMode::Password).then_some(true),
    }
}

/// The Authentication accordion's summary.
pub fn auth_summary(draft: &HostDraft) -> String {
    match draft.auth_mode {
        AuthMode::Password => "Password / interactive".into(),
        AuthMode::Key => {
            let count = draft.identity_files.len();
            format!("{count} key file{}", if count == 1 { "" } else { "s" })
        }
        AuthMode::Default => "Agent & default keys".into(),
    }
}

/// The Jump hosts accordion's summary.
pub fn jump_summary(draft: &HostDraft) -> String {
    if draft.proxy_jump.is_empty() {
        "Direct connection".into()
    } else {
        format!("{} configured", draft.proxy_jump.len())
    }
}

/// The chosen alias's resolved address and its auth and jump chips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSummary {
    pub address: String,
    pub auth: String,
    pub jumps: Option<String>,
}

pub fn host_summary(host: &SshHostEntry) -> HostSummary {
    let resolved = &host.resolved;
    let auth = if resolved.password_only {
        "password".to_owned()
    } else if !resolved.identity_files.is_empty() {
        let count = resolved.identity_files.len();
        format!("{count} key{}", if count == 1 { "" } else { "s" })
    } else {
        "agent / defaults".to_owned()
    };
    let jumps = (!resolved.proxy_jump.is_empty()).then(|| {
        let count = resolved.proxy_jump.len();
        format!("{count} jump{}", if count == 1 { "" } else { "s" })
    });
    HostSummary {
        address: format!(
            "{}{}:{}",
            resolved
                .user
                .as_deref()
                .map(|u| format!("{u}@"))
                .unwrap_or_default(),
            resolved.hostname,
            resolved.port
        ),
        auth,
        jumps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(ssh_options: Option<TunnelSshOptions>) -> TunnelRecord {
        TunnelRecord {
            id: "t1".into(),
            name: Some("Postgres".into()),
            target: "bastion".into(),
            ssh_options,
            kind: ForwardType::Local,
            bind_port: 15432,
            target_host: Some("db-01.prod.internal".into()),
            target_port: Some(5432),
            auto_start: true,
            auto_reconnect: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn opens_a_configured_host_tunnel() {
        let draft = TunnelDraft::new(&TunnelEditorState {
            tunnel: Some(record(None)),
            prefill_target: None,
        });
        assert_eq!(draft.mode, ConnectionMode::Host);
        assert_eq!(draft.host_target, "bastion");
        assert!(draft.valid());
        assert_eq!(draft.start_label(), "Save & restart");
        let input = draft.to_input();
        assert_eq!(
            serde_json::to_value(input).unwrap(),
            serde_json::json!({
                "id": "t1", "name": "Postgres", "target": "bastion", "type": "local",
                "bindPort": 15432, "targetHost": "db-01.prod.internal", "targetPort": 5432,
                "autoStart": true, "autoReconnect": false,
            })
        );
    }

    #[test]
    fn keeps_custom_connection_settings_with_the_tunnel() {
        let options = TunnelSshOptions {
            user: Some("ops".into()),
            port: Some(2222),
            identity_files: Some(vec!["~/.ssh/id".into()]),
            identities_only: Some(true),
            proxy_jump: Some(vec!["jump".into()]),
            ..TunnelSshOptions::default()
        };
        let mut draft = TunnelDraft::new(&TunnelEditorState {
            tunnel: Some(record(Some(options.clone()))),
            prefill_target: None,
        });
        assert_eq!(draft.mode, ConnectionMode::Custom);
        assert_eq!(draft.custom.alias_text, "bastion");
        assert_eq!(draft.custom.auth_mode, AuthMode::Key);
        assert_eq!(auth_summary(&draft.custom), "1 key file");
        assert_eq!(jump_summary(&draft.custom), "1 configured");
        assert_eq!(draft.to_input().ssh_options, Some(options));
        draft.kind = ForwardType::Dynamic;
        let input = draft.to_input();
        assert_eq!((input.target_host, input.target_port), (None, None));
        draft.custom.alias_text = "two words".into();
        assert!(!draft.valid());
    }

    #[test]
    fn starts_new_tunnels_from_the_prefilled_target() {
        let draft = TunnelDraft::new(&TunnelEditorState {
            tunnel: None,
            prefill_target: Some("web-01".into()),
        });
        assert_eq!(draft.host_target, "web-01");
        assert_eq!(draft.target_host, "localhost");
        assert_eq!(draft.title(), "New persistent tunnel");
        assert!(!draft.valid());
        assert_eq!(draft.server_label(), "web-01");
    }
}
