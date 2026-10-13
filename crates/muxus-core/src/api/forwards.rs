//! Port forwards, saved tunnels and live SSH connections.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardType {
    Local,
    Remote,
    Dynamic,
}

impl ForwardType {
    pub fn as_str(self) -> &'static str {
        match self {
            ForwardType::Local => "local",
            ForwardType::Remote => "remote",
            ForwardType::Dynamic => "dynamic",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "local" => Some(ForwardType::Local),
            "remote" => Some(ForwardType::Remote),
            "dynamic" => Some(ForwardType::Dynamic),
            _ => None,
        }
    }
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardRequest {
    pub conn_id: String,
    #[serde(rename = "type")]
    pub kind: ForwardType,
    /// Listen port — local port for local/dynamic, remote bind port for remote.
    pub bind_port: u16,
    /// Target of the tunnel; unused for dynamic (SOCKS picks per request).
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    /// Saved tunnel this forward realizes (running-state matching in the UI).
    pub tunnel_id: Option<String>,
}

/// Where a running forward came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardOrigin {
    /// Auto-started from a *Forward line in ssh config.
    Config,
    /// Added at runtime.
    Manual,
}

/// Session forwards belong to the terminal that created the connection and
/// stop with it. Independent forwards are explicitly saved/manual tunnels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardLifecycle {
    Session,
    Independent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardStatus {
    Active,
    /// A tunnel that starts with Muxus is making its first dial.
    Starting,
    /// An auto-reconnect tunnel is waiting to dial again.
    Reconnecting,
    /// The backend stopped trying, because the dial needs the user or the
    /// tunnel does not reconnect.
    Error,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardInfo {
    pub id: String,
    /// The connection carrying the forward; empty until a started tunnel first connects.
    pub conn_id: String,
    #[serde(rename = "type")]
    pub kind: ForwardType,
    pub bind_port: u16,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    pub origin: ForwardOrigin,
    pub lifecycle: ForwardLifecycle,
    pub status: ForwardStatus,
    /// Why the forward is not active (the last failed attempt).
    pub error: Option<String>,
    /// Saved tunnel this forward realizes, when it was started from one.
    pub tunnel_id: Option<String>,
}

/// A saved tunnel definition — a forwarding rule bound to an SSH target,
/// started and stopped independently of any terminal (the MobaXterm tunnel
/// workflow). Starting one reuses a live connection to the target when there
/// is one, otherwise dials a shell-less transport.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelRecord {
    pub id: String,
    /// Display name; falls back to the rule description in the UI.
    pub name: Option<String>,
    /// SSH target — a config alias when `ssh_options` is absent, otherwise a hostname.
    pub target: String,
    /// Present for a tunnel-owned connection profile. Absent means "resolve
    /// this target from OpenSSH config". Passwords and passphrases are never
    /// stored. An empty object is meaningful: it selects an ad-hoc connection
    /// using the default SSH agent/key discovery without inheriting an
    /// alias's settings.
    pub ssh_options: Option<TunnelSshOptions>,
    #[serde(rename = "type")]
    pub kind: ForwardType,
    pub bind_port: u16,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    /// Start without prompting when Muxus starts.
    pub auto_start: bool,
    /// Redial and restart the forward when its SSH connection drops.
    pub auto_reconnect: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Create/update a saved tunnel (id present = update).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelInput {
    pub id: Option<String>,
    pub name: Option<String>,
    pub target: String,
    pub ssh_options: Option<TunnelSshOptions>,
    #[serde(rename = "type")]
    pub kind: ForwardType,
    pub bind_port: u16,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    /// Absent = off.
    pub auto_start: Option<bool>,
    /// Absent = off.
    pub auto_reconnect: Option<bool>,
}

/// Safe-to-persist SSH settings owned by a saved tunnel.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TunnelSshOptions {
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Option<Vec<String>>,
    pub identities_only: Option<bool>,
    pub forward_agent: Option<bool>,
    /// Ordered ProxyJump specs: config aliases or "[user@]host[:port]".
    pub proxy_jump: Option<Vec<String>>,
    /// Skip public keys and prompt for password/keyboard-interactive auth.
    pub password_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelsResponse {
    pub tunnels: Vec<TunnelRecord>,
}

/// Live SSH transport summary (forwarding panel, connection reuse).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub id: String,
    /// What was dialed — config alias or ad-hoc "[user@]host[:port]".
    pub target: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Config alias when the target matches a Host block in ~/.ssh/config.
    pub metadata_alias: Option<String>,
    /// False when this transport belongs to a host with SFTP disabled.
    pub sftp_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionsResponse {
    pub connections: Vec<ConnectionInfo>,
}

// ---------------------------------------------------------------------------
// Route validation (`server/src/routes/tunnels.ts`)
// ---------------------------------------------------------------------------

impl crate::validate::Validate for TunnelSshOptions {
    /// The tunnel route's strict `sshOptions` schema (unknown keys are refused
    /// by deserialization).
    fn validate_at(&mut self, path: &str) -> Result<(), crate::validate::ValidationError> {
        use crate::validate::{self, join};
        validate::opt_trimmed(&join(path, "user"), &mut self.user, 1, 200)?;
        if let Some(port) = self.port {
            validate::range(&join(path, "port"), port, 1, 65535)?;
        }
        if let Some(files) = &mut self.identity_files {
            validate::items(&join(path, "identityFiles"), files.len(), 0, 32)?;
            validate::each_trimmed(&join(path, "identityFiles"), files, 1, 4096)?;
        }
        if let Some(hops) = &mut self.proxy_jump {
            validate::items(&join(path, "proxyJump"), hops.len(), 0, 8)?;
            validate::each_trimmed(&join(path, "proxyJump"), hops, 1, 500)?;
        }
        Ok(())
    }
}

impl crate::validate::Validate for TunnelInput {
    /// `tunnelSchema` of the tunnel route.
    fn validate_at(&mut self, path: &str) -> Result<(), crate::validate::ValidationError> {
        use crate::validate::{self, ValidationError, join};
        validate::opt_string(&join(path, "id"), &self.id, 1, usize::MAX)?;
        validate::opt_string(&join(path, "name"), &self.name, 0, 200)?;
        validate::trimmed(&join(path, "target"), &mut self.target, 1, 500)?;
        if let Some(options) = &mut self.ssh_options {
            options.validate_at(&join(path, "sshOptions"))?;
        }
        validate::range(&join(path, "bindPort"), self.bind_port, 1, 65535)?;
        validate::opt_string(&join(path, "targetHost"), &self.target_host, 1, 500)?;
        if let Some(port) = self.target_port {
            validate::range(&join(path, "targetPort"), port, 1, 65535)?;
        }
        let has_target = self
            .target_host
            .as_deref()
            .is_some_and(|host| !host.is_empty())
            && self.target_port.is_some();
        if self.kind != ForwardType::Dynamic && !has_target {
            return Err(ValidationError::new(
                path,
                "targetHost and targetPort are required for local/remote tunnels",
            ));
        }
        Ok(())
    }
}
