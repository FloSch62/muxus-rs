//! Session profiles and the session message protocol (port of
//! `shared/src/ws-protocol.ts`).
//!
//! In the TypeScript these are the JSON control frames of the `/ws/terminal`,
//! `/ws/desktop` and `/ws/management` sockets; in Rust they travel over the
//! in-process `SessionHandle` channels, but keep the same shape (tagged by
//! `op`, camelCase fields) and the same validation, so the profiles stored in
//! workspaces and saved hosts stay interchangeable with the TypeScript build.
//!
//! Each zod schema is a type here plus a [`Validate`] implementation that
//! applies the schema's trimming and limits. Defaults (`port: 23`, serial
//! 8-N-1, …) are filled in by deserialization, as zod's `.default()` does.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::api::ConfigForward;
use crate::connection_links::normalize_host_key_fingerprint;
use crate::management::{
    GNMI_DEFAULT_PORT, GnmiDataType, GnmiEncoding, GnmiSetEncoding, GnmiSubscriptionListMode,
    GnmiSubscriptionMode, GnmiTlsMode, ManagementSessionInfo, ManagementStreamMessage,
    NETCONF_DEFAULT_PORT,
};
use crate::paste_pacing::{
    MAX_PACED_PASTE_LENGTH, MAX_PASTE_CHAR_DELAY_MS, MAX_PASTE_LINE_DELAY_MS,
};
use crate::validate::{self, Validate, ValidationError, join};

/// Fixed subprotocol selected by the server for terminal sockets.
pub const TERMINAL_WS_PROTOCOL: &str = "muxus.terminal.v1";
/// Authentication is offered as a non-selected subprotocol so it stays out of request URLs.
pub const TERMINAL_WS_AUTH_PREFIX: &str = "muxus.auth.";
/// Clean-close reason that explicitly ends the backend terminal lifecycle.
pub const TERMINAL_SESSION_CLOSE_REASON: &str = "terminal session closed";

/// Upper bound for the Muxus-wide ServerAliveInterval fallback, in seconds.
pub const MAX_SSH_KEEPALIVE_INTERVAL_SECONDS: u32 = 3600;
/// The ServerAliveInterval fallback Muxus uses unless Settings say otherwise.
pub const DEFAULT_SSH_KEEPALIVE_INTERVAL_SECONDS: u32 = 30;
/// How long Send BREAK holds the line unless a serial host says otherwise (tcsendbreak's length).
pub const DEFAULT_BREAK_DURATION_MS: u32 = 250;
/// Upper bound for a serial host's break duration, in milliseconds.
pub const MAX_BREAK_DURATION_MS: u32 = 10_000;

/// Protocols offered by browser WebSocket clients during the HTTP upgrade.
pub fn terminal_web_socket_protocols(token: &str) -> [String; 2] {
    [
        TERMINAL_WS_PROTOCOL.to_owned(),
        format!("{TERMINAL_WS_AUTH_PREFIX}{token}"),
    ]
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

/// A local shell. Secrets are never part of any profile — they travel only in
/// `auth-response` replies.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalProfile {
    /// Login shell override; empty/absent picks the backend's default.
    pub shell: Option<String>,
    /// Arguments passed to the shell executable without command-line re-parsing.
    pub args: Option<Vec<String>>,
    pub cwd: Option<String>,
    /// Text entered after the interactive shell starts, followed by Enter.
    pub startup_command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestTty {
    No,
    Yes,
    Force,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StrictHostKeyChecking {
    Yes,
    No,
    AcceptNew,
    Ask,
}

/// An SSH session. Everything the profile leaves out — HostName, User, Port,
/// keys, ProxyJump, forwards — resolves from ssh_config like `ssh <target>`.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshProfile {
    /// Stable Muxus database profile when this is a saved host.
    pub profile_id: Option<String>,
    /// Host alias from ~/.ssh/config, or an ad-hoc "[user@]host[:port]".
    pub target: String,
    /// False for a self-contained saved host or tunnel; jump aliases may still resolve from config.
    pub use_config: Option<bool>,
    /// Muxus-wide fallback for ServerAliveInterval, in seconds. An explicit
    /// ssh_config value still wins for the host or jump hop.
    pub keepalive_interval_seconds: Option<u32>,
    /// Quick-connect overrides on top of config resolution.
    pub user: Option<String>,
    pub port: Option<u16>,
    /// Tunnel-owned overrides; passwords/passphrases still travel only in prompts.
    pub identity_files: Option<Vec<String>>,
    pub certificate_files: Option<Vec<String>>,
    pub identities_only: Option<bool>,
    /// Agent socket path, environment indirection, SSH_AUTH_SOCK, or none.
    pub identity_agent: Option<String>,
    pub forward_agent: Option<bool>,
    /// Absent = the platform default (on with the bundled Windows X server).
    #[serde(rename = "forwardX11")]
    pub forward_x11: Option<bool>,
    pub proxy_jump: Option<Vec<String>>,
    pub proxy_command: Option<String>,
    pub forwards: Option<Vec<ConfigForward>>,
    pub password_only: Option<bool>,
    pub remote_command: Option<String>,
    pub request_tty: Option<RequestTty>,
    pub strict_host_key_checking: Option<StrictHostKeyChecking>,
    /// Host key the target must present, from an ssh:// link's fingerprint, in
    /// canonical form. It only tightens verification and is never saved with a host.
    pub host_key_fingerprint: Option<String>,
}

impl SshProfile {
    /// A profile that dials `target` through ssh_config.
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            ..Self::default()
        }
    }
}

fn default_telnet_port() -> u16 {
    23
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelnetProfile {
    /// Stable Muxus database profile when this is a saved host.
    pub profile_id: Option<String>,
    pub host: String,
    #[serde(default = "default_telnet_port")]
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SerialParity {
    #[default]
    None,
    Even,
    Odd,
    Mark,
    Space,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SerialFlowControl {
    #[default]
    None,
    Hardware,
    Software,
}

/// Serial stop bits: 1, 1.5 or 2 (a JavaScript number on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StopBits {
    #[default]
    One,
    OnePointFive,
    Two,
}

impl Serialize for StopBits {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            StopBits::One => serializer.serialize_u8(1),
            StopBits::OnePointFive => serializer.serialize_f64(1.5),
            StopBits::Two => serializer.serialize_u8(2),
        }
    }
}

impl<'de> Deserialize<'de> for StopBits {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        if value == 1.0 {
            Ok(StopBits::One)
        } else if value == 1.5 {
            Ok(StopBits::OnePointFive)
        } else if value == 2.0 {
            Ok(StopBits::Two)
        } else {
            Err(serde::de::Error::custom("Invalid input"))
        }
    }
}

fn default_baud_rate() -> u32 {
    115_200
}

fn default_data_bits() -> u8 {
    8
}

/// A serial line. Defaults are the conventional 8-N-1 at 115200 baud.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialProfile {
    /// Stable Muxus database profile when this is a saved host.
    pub profile_id: Option<String>,
    /// OS-native device path: COM3, /dev/ttyUSB0, /dev/tty.usbserial-…, etc.
    pub path: String,
    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,
    /// 5, 6, 7 or 8.
    #[serde(default = "default_data_bits")]
    pub data_bits: u8,
    #[serde(default)]
    pub stop_bits: StopBits,
    #[serde(default)]
    pub parity: SerialParity,
    #[serde(default)]
    pub flow_control: SerialFlowControl,
    /// How long Send BREAK holds the line; absent means [`DEFAULT_BREAK_DURATION_MS`].
    pub break_duration_ms: Option<u32>,
}

impl SerialProfile {
    /// A profile for `path` with every default filled in.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            profile_id: None,
            path: path.into(),
            baud_rate: default_baud_rate(),
            data_bits: default_data_bits(),
            stop_bits: StopBits::One,
            parity: SerialParity::None,
            flow_control: SerialFlowControl::None,
            break_duration_ms: None,
        }
    }
}

/// SSH host a remote desktop or management session is reached through, the
/// way `ssh -L` would carry it: the far end resolves the destination.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshGateway {
    /// ssh_config alias, or the saved SSH host's target when `profile_id` is set.
    pub target: String,
    /// Muxus-owned SSH host; absent resolves `target` through ssh_config.
    pub profile_id: Option<String>,
}

fn default_rdp_port() -> u16 {
    3389
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RdpProfile {
    /// Stable Muxus database profile when this is a saved host.
    pub profile_id: Option<String>,
    pub host: String,
    #[serde(default = "default_rdp_port")]
    pub port: u16,
    /// Logon name; `DOMAIN\user` and `user@domain` work as typed.
    pub username: Option<String>,
    pub domain: Option<String>,
    pub ssh_gateway: Option<SshGateway>,
    /// Text clipboard redirection; absent means on, as in mstsc.
    pub share_clipboard: Option<bool>,
}

fn default_vnc_port() -> u16 {
    5900
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VncProfile {
    /// Stable Muxus database profile when this is a saved host.
    pub profile_id: Option<String>,
    pub host: String,
    #[serde(default = "default_vnc_port")]
    pub port: u16,
    /// Only servers with user logins ask for one (VeNCrypt, Apple Remote Desktop, UltraVNC).
    pub username: Option<String>,
    pub ssh_gateway: Option<SshGateway>,
    /// Ask the server to resize its desktop to the pane instead of scaling the picture.
    pub resize_remote: Option<bool>,
    /// Watch without sending keyboard or mouse input.
    pub view_only: Option<bool>,
    /// Text clipboard sharing; absent means on.
    pub share_clipboard: Option<bool>,
}

fn default_gnmi_port() -> u16 {
    GNMI_DEFAULT_PORT
}

/// A gNMI target: gRPC over HTTP/2, TLS by default. Passwords travel only in
/// `auth-response` replies or come from the password vault.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GnmiProfile {
    /// Stable Muxus database profile when this is a saved host.
    pub profile_id: Option<String>,
    pub host: String,
    #[serde(default = "default_gnmi_port")]
    pub port: u16,
    pub username: Option<String>,
    /// Absent means `verify`: check the certificate, pin one that does not verify.
    pub tls: Option<GnmiTlsMode>,
    /// Name the certificate must carry, and the SNI sent, when it differs from `host`.
    pub tls_server_name: Option<String>,
    /// PEM bundle of authorities to verify the device certificate against.
    pub ca_file: Option<String>,
    /// Client certificate and key for mutual TLS.
    pub cert_file: Option<String>,
    pub key_file: Option<String>,
    pub ssh_gateway: Option<SshGateway>,
    /// Preferred encoding; absent picks the best one the device supports.
    pub encoding: Option<GnmiEncoding>,
}

fn default_netconf_port() -> u16 {
    NETCONF_DEFAULT_PORT
}

/// A NETCONF server, reached over SSH (RFC 6242). The host resolves through
/// ssh_config like `ssh -s host netconf`, so keys, users and jump hosts set
/// there apply.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetconfProfile {
    /// Stable Muxus database profile when this is a saved host.
    pub profile_id: Option<String>,
    pub host: String,
    #[serde(default = "default_netconf_port")]
    pub port: u16,
    pub username: Option<String>,
    pub ssh_gateway: Option<SshGateway>,
}

/// The `kind` of a session profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    Local,
    Ssh,
    Telnet,
    Serial,
    Rdp,
    Vnc,
    Gnmi,
    Netconf,
}

impl ProfileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileKind::Local => "local",
            ProfileKind::Ssh => "ssh",
            ProfileKind::Telnet => "telnet",
            ProfileKind::Serial => "serial",
            ProfileKind::Rdp => "rdp",
            ProfileKind::Vnc => "vnc",
            ProfileKind::Gnmi => "gnmi",
            ProfileKind::Netconf => "netconf",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            ProfileKind::Local,
            ProfileKind::Ssh,
            ProfileKind::Telnet,
            ProfileKind::Serial,
            ProfileKind::Rdp,
            ProfileKind::Vnc,
            ProfileKind::Gnmi,
            ProfileKind::Netconf,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == value)
    }

    /// Remote screens drawn over the desktop stream.
    pub fn is_desktop(self) -> bool {
        matches!(self, ProfileKind::Rdp | ProfileKind::Vnc)
    }

    /// Model-driven management sessions.
    pub fn is_management(self) -> bool {
        matches!(self, ProfileKind::Gnmi | ProfileKind::Netconf)
    }

    /// Sessions with a terminal: everything but remote desktops and management sessions.
    pub fn is_terminal(self) -> bool {
        !self.is_desktop() && !self.is_management()
    }
}

/// Where a session runs (`sessionProfileSchema`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SessionProfile {
    Local(LocalProfile),
    Ssh(SshProfile),
    Telnet(TelnetProfile),
    Serial(SerialProfile),
    Rdp(RdpProfile),
    Vnc(VncProfile),
    Gnmi(GnmiProfile),
    Netconf(NetconfProfile),
}

impl SessionProfile {
    pub fn kind(&self) -> ProfileKind {
        match self {
            SessionProfile::Local(_) => ProfileKind::Local,
            SessionProfile::Ssh(_) => ProfileKind::Ssh,
            SessionProfile::Telnet(_) => ProfileKind::Telnet,
            SessionProfile::Serial(_) => ProfileKind::Serial,
            SessionProfile::Rdp(_) => ProfileKind::Rdp,
            SessionProfile::Vnc(_) => ProfileKind::Vnc,
            SessionProfile::Gnmi(_) => ProfileKind::Gnmi,
            SessionProfile::Netconf(_) => ProfileKind::Netconf,
        }
    }

    pub fn is_desktop(&self) -> bool {
        self.kind().is_desktop()
    }

    pub fn is_management(&self) -> bool {
        self.kind().is_management()
    }

    pub fn is_terminal(&self) -> bool {
        self.kind().is_terminal()
    }

    /// The saved host this profile was opened from, if any.
    pub fn profile_id(&self) -> Option<&str> {
        match self {
            SessionProfile::Local(_) => None,
            SessionProfile::Ssh(p) => p.profile_id.as_deref(),
            SessionProfile::Telnet(p) => p.profile_id.as_deref(),
            SessionProfile::Serial(p) => p.profile_id.as_deref(),
            SessionProfile::Rdp(p) => p.profile_id.as_deref(),
            SessionProfile::Vnc(p) => p.profile_id.as_deref(),
            SessionProfile::Gnmi(p) => p.profile_id.as_deref(),
            SessionProfile::Netconf(p) => p.profile_id.as_deref(),
        }
    }

    /// The terminal subset of this profile, when it is one.
    pub fn as_terminal(&self) -> Option<TerminalProfile> {
        Some(match self.clone() {
            SessionProfile::Local(p) => TerminalProfile::Local(p),
            SessionProfile::Ssh(p) => TerminalProfile::Ssh(p),
            SessionProfile::Telnet(p) => TerminalProfile::Telnet(p),
            SessionProfile::Serial(p) => TerminalProfile::Serial(p),
            _ => return None,
        })
    }
}

/// Sessions rendered as a terminal (`terminalProfileSchema`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TerminalProfile {
    Local(LocalProfile),
    Ssh(SshProfile),
    Telnet(TelnetProfile),
    Serial(SerialProfile),
}

impl From<TerminalProfile> for SessionProfile {
    fn from(profile: TerminalProfile) -> Self {
        match profile {
            TerminalProfile::Local(p) => SessionProfile::Local(p),
            TerminalProfile::Ssh(p) => SessionProfile::Ssh(p),
            TerminalProfile::Telnet(p) => SessionProfile::Telnet(p),
            TerminalProfile::Serial(p) => SessionProfile::Serial(p),
        }
    }
}

/// Sessions drawn as a remote screen (`desktopProfileSchema`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DesktopProfile {
    Rdp(RdpProfile),
    Vnc(VncProfile),
}

impl From<DesktopProfile> for SessionProfile {
    fn from(profile: DesktopProfile) -> Self {
        match profile {
            DesktopProfile::Rdp(p) => SessionProfile::Rdp(p),
            DesktopProfile::Vnc(p) => SessionProfile::Vnc(p),
        }
    }
}

/// Model-driven management sessions (`managementProfileSchema`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ManagementProfile {
    Gnmi(GnmiProfile),
    Netconf(NetconfProfile),
}

impl From<ManagementProfile> for SessionProfile {
    fn from(profile: ManagementProfile) -> Self {
        match profile {
            ManagementProfile::Gnmi(p) => SessionProfile::Gnmi(p),
            ManagementProfile::Netconf(p) => SessionProfile::Netconf(p),
        }
    }
}

/// The profile of a saved host: every kind but local shells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SavedHostSessionProfile {
    Ssh(SshProfile),
    Telnet(TelnetProfile),
    Serial(SerialProfile),
    Rdp(RdpProfile),
    Vnc(VncProfile),
    Gnmi(GnmiProfile),
    Netconf(NetconfProfile),
}

impl SavedHostSessionProfile {
    pub fn kind(&self) -> crate::api::SavedHostKind {
        use crate::api::SavedHostKind;
        match self {
            SavedHostSessionProfile::Ssh(_) => SavedHostKind::Ssh,
            SavedHostSessionProfile::Telnet(_) => SavedHostKind::Telnet,
            SavedHostSessionProfile::Serial(_) => SavedHostKind::Serial,
            SavedHostSessionProfile::Rdp(_) => SavedHostKind::Rdp,
            SavedHostSessionProfile::Vnc(_) => SavedHostKind::Vnc,
            SavedHostSessionProfile::Gnmi(_) => SavedHostKind::Gnmi,
            SavedHostSessionProfile::Netconf(_) => SavedHostKind::Netconf,
        }
    }

    pub fn profile_id(&self) -> Option<&str> {
        match self {
            SavedHostSessionProfile::Ssh(p) => p.profile_id.as_deref(),
            SavedHostSessionProfile::Telnet(p) => p.profile_id.as_deref(),
            SavedHostSessionProfile::Serial(p) => p.profile_id.as_deref(),
            SavedHostSessionProfile::Rdp(p) => p.profile_id.as_deref(),
            SavedHostSessionProfile::Vnc(p) => p.profile_id.as_deref(),
            SavedHostSessionProfile::Gnmi(p) => p.profile_id.as_deref(),
            SavedHostSessionProfile::Netconf(p) => p.profile_id.as_deref(),
        }
    }

    /// Set (or clear) the stored profile id.
    pub fn set_profile_id(&mut self, id: Option<String>) {
        match self {
            SavedHostSessionProfile::Ssh(p) => p.profile_id = id,
            SavedHostSessionProfile::Telnet(p) => p.profile_id = id,
            SavedHostSessionProfile::Serial(p) => p.profile_id = id,
            SavedHostSessionProfile::Rdp(p) => p.profile_id = id,
            SavedHostSessionProfile::Vnc(p) => p.profile_id = id,
            SavedHostSessionProfile::Gnmi(p) => p.profile_id = id,
            SavedHostSessionProfile::Netconf(p) => p.profile_id = id,
        }
    }
}

impl From<SavedHostSessionProfile> for SessionProfile {
    fn from(profile: SavedHostSessionProfile) -> Self {
        match profile {
            SavedHostSessionProfile::Ssh(p) => SessionProfile::Ssh(p),
            SavedHostSessionProfile::Telnet(p) => SessionProfile::Telnet(p),
            SavedHostSessionProfile::Serial(p) => SessionProfile::Serial(p),
            SavedHostSessionProfile::Rdp(p) => SessionProfile::Rdp(p),
            SavedHostSessionProfile::Vnc(p) => SessionProfile::Vnc(p),
            SavedHostSessionProfile::Gnmi(p) => SessionProfile::Gnmi(p),
            SavedHostSessionProfile::Netconf(p) => SessionProfile::Netconf(p),
        }
    }
}

// ---------------------------------------------------------------------------
// Profile validation
// ---------------------------------------------------------------------------

fn profile_id(path: &str, value: &Option<String>) -> Result<(), ValidationError> {
    validate::opt_string(&join(path, "profileId"), value, 1, 200)
}

fn port(path: &str, value: u16) -> Result<(), ValidationError> {
    validate::range(path, value, 1, 65535)
}

impl Validate for LocalProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::opt_string(&join(path, "shell"), &self.shell, 0, 4096)?;
        if let Some(args) = &self.args {
            validate::items(&join(path, "args"), args.len(), 0, 64)?;
            validate::each_string(&join(path, "args"), args, 0, 4096)?;
        }
        validate::opt_string(&join(path, "cwd"), &self.cwd, 0, 4096)?;
        validate::opt_string(
            &join(path, "startupCommand"),
            &self.startup_command,
            0,
            32_768,
        )
    }
}

impl Validate for ConfigForward {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        port(&join(path, "bindPort"), self.bind_port)?;
        validate::opt_string(&join(path, "targetHost"), &self.target_host, 1, 4096)?;
        if let Some(target_port) = self.target_port {
            port(&join(path, "targetPort"), target_port)?;
        }
        Ok(())
    }
}

impl Validate for SshProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        profile_id(path, &self.profile_id)?;
        validate::string(&join(path, "target"), &self.target, 1, usize::MAX)?;
        validate::opt_range(
            &join(path, "keepaliveIntervalSeconds"),
            self.keepalive_interval_seconds,
            1,
            MAX_SSH_KEEPALIVE_INTERVAL_SECONDS,
        )?;
        if let Some(value) = self.port {
            port(&join(path, "port"), value)?;
        }
        for (key, files) in [
            ("identityFiles", &self.identity_files),
            ("certificateFiles", &self.certificate_files),
        ] {
            if let Some(files) = files {
                validate::items(&join(path, key), files.len(), 0, 32)?;
                validate::each_string(&join(path, key), files, 1, 4096)?;
            }
        }
        validate::opt_string(&join(path, "identityAgent"), &self.identity_agent, 1, 4096)?;
        if let Some(hops) = &self.proxy_jump {
            validate::items(&join(path, "proxyJump"), hops.len(), 0, 8)?;
            validate::each_string(&join(path, "proxyJump"), hops, 1, 500)?;
        }
        validate::opt_string(&join(path, "proxyCommand"), &self.proxy_command, 1, 32_768)?;
        if let Some(forwards) = &mut self.forwards {
            validate::items(&join(path, "forwards"), forwards.len(), 0, 64)?;
            forwards.validate_at(&join(path, "forwards"))?;
        }
        validate::opt_string(
            &join(path, "remoteCommand"),
            &self.remote_command,
            1,
            32_768,
        )?;
        if let Some(fingerprint) = &self.host_key_fingerprint {
            let at = join(path, "hostKeyFingerprint");
            validate::string(&at, fingerprint, 0, 200)?;
            if normalize_host_key_fingerprint(fingerprint).as_deref() != Some(fingerprint.as_str())
            {
                return Err(ValidationError::new(at, "invalid host key fingerprint"));
            }
        }
        Ok(())
    }
}

impl Validate for TelnetProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        profile_id(path, &self.profile_id)?;
        validate::trimmed(&join(path, "host"), &mut self.host, 1, 253)?;
        port(&join(path, "port"), self.port)
    }
}

impl Validate for SerialProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        profile_id(path, &self.profile_id)?;
        validate::trimmed(&join(path, "path"), &mut self.path, 1, 4096)?;
        validate::range(&join(path, "baudRate"), self.baud_rate, 1, 12_000_000)?;
        if !(5..=8).contains(&self.data_bits) {
            return Err(ValidationError::new(
                join(path, "dataBits"),
                "Invalid input",
            ));
        }
        validate::opt_range(
            &join(path, "breakDurationMs"),
            self.break_duration_ms,
            1,
            MAX_BREAK_DURATION_MS,
        )
    }
}

impl Validate for SshGateway {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::trimmed(&join(path, "target"), &mut self.target, 1, 500)?;
        profile_id(path, &self.profile_id)
    }
}

impl Validate for RdpProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        profile_id(path, &self.profile_id)?;
        validate::trimmed(&join(path, "host"), &mut self.host, 1, 253)?;
        port(&join(path, "port"), self.port)?;
        validate::opt_trimmed(&join(path, "username"), &mut self.username, 0, 256)?;
        validate::opt_trimmed(&join(path, "domain"), &mut self.domain, 0, 256)?;
        self.ssh_gateway.validate_at(&join(path, "sshGateway"))
    }
}

impl Validate for VncProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        profile_id(path, &self.profile_id)?;
        validate::trimmed(&join(path, "host"), &mut self.host, 1, 253)?;
        port(&join(path, "port"), self.port)?;
        validate::opt_trimmed(&join(path, "username"), &mut self.username, 0, 256)?;
        self.ssh_gateway.validate_at(&join(path, "sshGateway"))
    }
}

impl Validate for GnmiProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        profile_id(path, &self.profile_id)?;
        validate::trimmed(&join(path, "host"), &mut self.host, 1, 253)?;
        port(&join(path, "port"), self.port)?;
        validate::opt_trimmed(&join(path, "username"), &mut self.username, 0, 256)?;
        validate::opt_trimmed(
            &join(path, "tlsServerName"),
            &mut self.tls_server_name,
            1,
            253,
        )?;
        validate::opt_trimmed(&join(path, "caFile"), &mut self.ca_file, 1, 4096)?;
        validate::opt_trimmed(&join(path, "certFile"), &mut self.cert_file, 1, 4096)?;
        validate::opt_trimmed(&join(path, "keyFile"), &mut self.key_file, 1, 4096)?;
        self.ssh_gateway.validate_at(&join(path, "sshGateway"))
    }
}

impl Validate for NetconfProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        profile_id(path, &self.profile_id)?;
        validate::trimmed(&join(path, "host"), &mut self.host, 1, 253)?;
        port(&join(path, "port"), self.port)?;
        validate::opt_trimmed(&join(path, "username"), &mut self.username, 0, 256)?;
        self.ssh_gateway.validate_at(&join(path, "sshGateway"))
    }
}

impl Validate for SessionProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        match self {
            SessionProfile::Local(p) => p.validate_at(path),
            SessionProfile::Ssh(p) => p.validate_at(path),
            SessionProfile::Telnet(p) => p.validate_at(path),
            SessionProfile::Serial(p) => p.validate_at(path),
            SessionProfile::Rdp(p) => p.validate_at(path),
            SessionProfile::Vnc(p) => p.validate_at(path),
            SessionProfile::Gnmi(p) => p.validate_at(path),
            SessionProfile::Netconf(p) => p.validate_at(path),
        }
    }
}

impl Validate for TerminalProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        match self {
            TerminalProfile::Local(p) => p.validate_at(path),
            TerminalProfile::Ssh(p) => p.validate_at(path),
            TerminalProfile::Telnet(p) => p.validate_at(path),
            TerminalProfile::Serial(p) => p.validate_at(path),
        }
    }
}

impl Validate for DesktopProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        match self {
            DesktopProfile::Rdp(p) => p.validate_at(path),
            DesktopProfile::Vnc(p) => p.validate_at(path),
        }
    }
}

impl Validate for ManagementProfile {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        match self {
            ManagementProfile::Gnmi(p) => p.validate_at(path),
            ManagementProfile::Netconf(p) => p.validate_at(path),
        }
    }
}

impl Validate for SavedHostSessionProfile {
    /// The saved-host route's schema: a saved SSH host never stores the
    /// keepalive fallback (an application preference each connect sends for
    /// itself) nor a link's host key fingerprint, so both are dropped.
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        match self {
            SavedHostSessionProfile::Ssh(p) => {
                p.keepalive_interval_seconds = None;
                p.host_key_fingerprint = None;
                p.validate_at(path)
            }
            SavedHostSessionProfile::Telnet(p) => p.validate_at(path),
            SavedHostSessionProfile::Serial(p) => p.validate_at(path),
            SavedHostSessionProfile::Rdp(p) => p.validate_at(path),
            SavedHostSessionProfile::Vnc(p) => p.validate_at(path),
            SavedHostSessionProfile::Gnmi(p) => p.validate_at(path),
            SavedHostSessionProfile::Netconf(p) => p.validate_at(path),
        }
    }
}

// ---------------------------------------------------------------------------
// Authentication prompts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthPromptPurpose {
    Authentication,
    SshPassword,
    VaultUnlock,
    VaultRepair,
    VaultCreate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthPromptField {
    pub prompt: String,
    /// False masks the input.
    pub echo: bool,
}

/// Offer to encrypt a successful password in the local vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberPasswordOffer {
    pub label: String,
    /// True when remembering will replace an older saved password.
    pub existing: bool,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthPromptInfo {
    pub name: Option<String>,
    pub instructions: Option<String>,
    /// Which host in the connection chain is asking ("bastion", "user@web1").
    pub host: Option<String>,
    pub prompts: Vec<AuthPromptField>,
    pub purpose: Option<AuthPromptPurpose>,
    pub remember_password: Option<RememberPasswordOffer>,
    /// Secondary action that continues without answering this prompt.
    pub skip_label: Option<String>,
}

/// Answers to the last auth prompt, in prompt order.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthPromptResponse {
    pub answers: Vec<String>,
    pub remember_password: Option<bool>,
    pub skipped: Option<bool>,
}

impl Validate for AuthPromptResponse {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::items(&join(path, "answers"), self.answers.len(), 0, 16)?;
        validate::each_string(&join(path, "answers"), &self.answers, 0, 8192)
    }
}

/// Host key verification: `new` = first contact (TOFU), `mismatch` = KEY CHANGED.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustState {
    New,
    Mismatch,
}

/// A `host-key` challenge.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyChallenge {
    pub host: String,
    pub port: u16,
    pub key_type: String,
    /// SHA256:… fingerprint, OpenSSH presentation.
    pub fingerprint: String,
    pub state: TrustState,
    /// Previously recorded fingerprint when state is `mismatch`.
    pub previous: Option<String>,
    /// Set when this is an intermediate ProxyJump hop, not the final target.
    pub hop: Option<String>,
}

// ---------------------------------------------------------------------------
// Terminal sessions
// ---------------------------------------------------------------------------

/// What the UI sends to a terminal session.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum TerminalClientMessage {
    #[serde(rename_all = "camelCase")]
    Connect {
        profile: TerminalProfile,
        /// Replacement-group token: skip SSH transports established before
        /// this request and dial a replacement. Connects carrying the same
        /// token share one replacement connection (one login for a
        /// force-reconnected window).
        fresh_transport: Option<String>,
        /// User-facing tab title retained in session history.
        title: Option<String>,
        /// Directory the tab's previous shell was in. A replacement SSH shell
        /// started through the remote shell integration changes into it
        /// during startup; every other shell ignores it.
        cwd: Option<String>,
        cols: u32,
        rows: u32,
    },
    /// Establish an SSH transport with no shell attached — the `ssh -N` of
    /// the protocol, used to start tunnels without opening a terminal. The
    /// backend runs the same status/auth-prompt/host-key round-trips, then
    /// answers `ready`; the transport lives until every lease on it is gone.
    Dial {
        #[serde(with = "tagged_ssh")]
        profile: SshProfile,
    },
    /// Attach to a live terminal after an interruption or window handoff.
    #[serde(rename_all = "camelCase")]
    Attach {
        terminal_id: String,
        cols: u32,
        rows: u32,
    },
    /// Freeze outbound bytes before the source window snapshots its buffer.
    PrepareTransfer,
    /// Resume the source window when a prepared handoff is abandoned.
    CancelTransfer,
    Resize {
        cols: u32,
        rows: u32,
    },
    /// Answers to the last `auth-prompt`, in prompt order.
    AuthResponse(AuthPromptResponse),
    /// Verdict on the last `host-key` challenge.
    HostKeyResponse {
        accept: bool,
    },
    /// Change only the current session; persisted policy is managed by the store.
    #[serde(rename_all = "camelCase")]
    SetLogging {
        enabled: Option<bool>,
        paused: Option<bool>,
        capture_input: Option<bool>,
        /// Start or stop writing this session to a plain-text log file.
        log_to_file: Option<bool>,
        /// Absolute file to append to; absent names a new file from the log file settings.
        log_file_path: Option<String>,
    },
    /// The tab was renamed; the active history record takes the new title.
    SetTitle {
        title: String,
    },
    /// Paste text paced by the backend, whose timers keep running while the
    /// window is hidden. Line breaks go out as CR, the way a terminal pastes;
    /// pastes queue behind one still running.
    #[serde(rename_all = "camelCase")]
    Paste {
        text: String,
        /// Enclose the whole paste, not each line, in bracketed-paste markers.
        bracketed: bool,
        line_delay_ms: u32,
        char_delay_ms: u32,
    },
    /// Stop the paced paste that is running and drop the queued ones.
    PasteCancel,
    /// Send a BREAK to this session only: a held break on a serial line, IAC
    /// BRK on Telnet, or an RFC 4335 `break` request on an SSH channel.
    SendBreak,
    /// Stop the host's login sequence; the session itself stays open.
    CancelLoginSequence,
}

/// An [`SshProfile`] in a field the schema types as `sshProfileSchema`: written
/// with its `kind: "ssh"` tag, and refused with any other kind.
mod tagged_ssh {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::SshProfile;

    pub fn serialize<S: Serializer>(
        profile: &SshProfile,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "lowercase")]
        enum Tagged<'a> {
            Ssh(&'a SshProfile),
        }
        Tagged::Ssh(profile).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SshProfile, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "lowercase")]
        enum Tagged {
            Ssh(SshProfile),
        }
        let Tagged::Ssh(profile) = Tagged::deserialize(deserializer)?;
        Ok(profile)
    }
}

impl Validate for TerminalClientMessage {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        match self {
            TerminalClientMessage::Connect {
                profile,
                fresh_transport,
                title,
                cwd,
                cols,
                rows,
            } => {
                profile.validate_at(&join(path, "profile"))?;
                validate::opt_string(&join(path, "freshTransport"), fresh_transport, 1, 100)?;
                validate::opt_trimmed(&join(path, "title"), title, 1, 500)?;
                validate::opt_string(&join(path, "cwd"), cwd, 1, 4096)?;
                validate::positive(&join(path, "cols"), *cols)?;
                validate::positive(&join(path, "rows"), *rows)
            }
            TerminalClientMessage::Dial { profile } => profile.validate_at(&join(path, "profile")),
            TerminalClientMessage::Attach {
                terminal_id,
                cols,
                rows,
            } => {
                validate::string(&join(path, "terminalId"), terminal_id, 1, 200)?;
                validate::positive(&join(path, "cols"), *cols)?;
                validate::positive(&join(path, "rows"), *rows)
            }
            TerminalClientMessage::Resize { cols, rows } => {
                validate::positive(&join(path, "cols"), *cols)?;
                validate::positive(&join(path, "rows"), *rows)
            }
            TerminalClientMessage::AuthResponse(response) => response.validate_at(path),
            TerminalClientMessage::SetLogging {
                enabled,
                paused,
                capture_input,
                log_to_file,
                log_file_path,
            } => {
                validate::opt_trimmed(&join(path, "logFilePath"), log_file_path, 1, 4096)?;
                if enabled.is_none()
                    && paused.is_none()
                    && capture_input.is_none()
                    && log_to_file.is_none()
                {
                    return Err(ValidationError::custom(path));
                }
                Ok(())
            }
            TerminalClientMessage::SetTitle { title } => {
                validate::trimmed(&join(path, "title"), title, 1, 500)
            }
            TerminalClientMessage::Paste {
                text,
                line_delay_ms,
                char_delay_ms,
                ..
            } => {
                validate::string(&join(path, "text"), text, 1, MAX_PACED_PASTE_LENGTH)?;
                validate::range(
                    &join(path, "lineDelayMs"),
                    *line_delay_ms,
                    0,
                    MAX_PASTE_LINE_DELAY_MS,
                )?;
                validate::range(
                    &join(path, "charDelayMs"),
                    *char_delay_ms,
                    0,
                    MAX_PASTE_CHAR_DELAY_MS,
                )
            }
            TerminalClientMessage::PrepareTransfer
            | TerminalClientMessage::CancelTransfer
            | TerminalClientMessage::HostKeyResponse { .. }
            | TerminalClientMessage::PasteCancel
            | TerminalClientMessage::SendBreak
            | TerminalClientMessage::CancelLoginSequence => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompressionState {
    On,
    Off,
    /// Compression was asked for and the server offered none.
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum X11State {
    On,
    Off,
    /// The server answered the X11 request with failure.
    Refused,
    /// The host asks for X11 but no local X server was found.
    NoServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentForwardingState {
    On,
    Off,
    /// The host asks for agent forwarding but no agent is reachable.
    NoAgent,
}

/// What an SSH session ended up with once its shell opened: how it was
/// reached, what the transport negotiated and which optional features are
/// active. Terminals print it on connect when the user has asked for it.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshSessionSummary {
    pub user: String,
    pub host: String,
    pub port: u16,
    /// Jump hosts in dialing order; empty when the target was dialed directly.
    pub jump_hosts: Vec<String>,
    /// The first hop was reached through a ProxyCommand.
    pub proxy_command: bool,
    /// The session joined a connection that was already open.
    pub shared: bool,
    /// Server software from its identification string, such as "OpenSSH_9.6p1".
    pub server_software: Option<String>,
    /// Methods the server accepted, in order; several for a multi-factor login.
    pub auth_methods: Vec<String>,
    /// Signature algorithm of the key that logged in.
    pub auth_key_algorithm: Option<String>,
    pub cipher: Option<String>,
    pub kex: Option<String>,
    pub compression: CompressionState,
    /// Whether the file browser may open SFTP on this connection.
    pub sftp: bool,
    pub x11: X11State,
    pub agent_forwarding: AgentForwardingState,
    /// Port forwards from the host's configuration running on the connection.
    pub forwards: Vec<ConfigForward>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionHealth {
    Healthy,
    Suspect,
}

/// Whether the session ended normally, setup failed, or a live transport was lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExitReason {
    Completed,
    Failed,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasteProgressState {
    Running,
    Done,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoginSequenceState {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// What a terminal session reports to the UI.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum TerminalServerMessage {
    /// Stable backend identity used for reattachment and window handoff.
    #[serde(rename_all = "camelCase")]
    Session {
        terminal_id: String,
    },
    /// Outbound terminal bytes are frozen and can now be snapshotted without a gap.
    TransferReady,
    /// Connection progress worth echoing into the terminal ("Connecting …").
    Status {
        message: String,
        transient: Option<bool>,
    },
    /// Passive SSH transport health derived from the keepalive lifecycle.
    ConnectionHealth {
        state: ConnectionHealth,
    },
    /// Interactive auth (password, 2FA, key passphrase).
    AuthPrompt(AuthPromptInfo),
    HostKey(HostKeyChallenge),
    /// Transport attached; SSH connIds also key follow-up SFTP/forward calls.
    #[serde(rename_all = "camelCase")]
    Ready {
        conn_id: String,
        host: Option<String>,
        user: Option<String>,
        sftp_available: Option<bool>,
        /// SSH terminal sessions only.
        summary: Option<SshSessionSummary>,
    },
    /// Current durable-log state, emitted at start and after every live change.
    #[serde(rename_all = "camelCase")]
    LoggingState {
        enabled: bool,
        session_id: Option<String>,
        paused: bool,
        capture_input: bool,
        /// Present when storage/backpressure suspended logging for this session.
        warning: Option<String>,
        /// Plain-text log file this session is being written to, if any.
        file_path: Option<String>,
    },
    /// Answer to `send-break`; `message` says why the BREAK did not go out.
    BreakResult {
        ok: bool,
        message: Option<String>,
    },
    Exit {
        code: Option<i32>,
        message: Option<String>,
        reason: ExitReason,
    },
    /// Paced paste progress over every queued paste, throttled while it runs.
    PasteProgress {
        state: PasteProgressState,
        /// The line being sent, from 1, and the lines of every queued paste.
        line: u64,
        lines: u64,
        /// UTF-16 code units sent and queued.
        sent: u64,
        total: u64,
    },
    /// The host's login sequence started a step, finished, or stopped.
    LoginSequence {
        state: LoginSequenceState,
        /// 1-based step that is running, or where the sequence stopped.
        step: u32,
        steps: u32,
        /// What the step does, such as Waiting for “Password:”.
        detail: String,
        /// Why a failed sequence stopped.
        message: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Remote desktops
// ---------------------------------------------------------------------------

pub const DESKTOP_RDP_WS_PATH: &str = "/ws/desktop/rdp";
pub const DESKTOP_VNC_WS_PATH: &str = "/ws/desktop/vnc";
/// A VNC stream socket offers its ticket as this subprotocol prefix.
pub const DESKTOP_TICKET_PROTOCOL_PREFIX: &str = "muxus.ticket.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopCredentialType {
    Username,
    Password,
}

/// What the UI sends to a remote desktop session.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum DesktopClientMessage {
    Connect {
        profile: DesktopProfile,
    },
    AuthResponse(AuthPromptResponse),
    HostKeyResponse {
        accept: bool,
    },
    CertificateResponse {
        accept: bool,
    },
    /// Start another attempt with a fresh ticket. `rejected` means the server
    /// refused the last credentials, so they are asked for again.
    Retry {
        rejected: Option<bool>,
    },
    /// A VNC server asked for these credentials mid-handshake.
    CredentialsRequest {
        types: Vec<DesktopCredentialType>,
    },
    /// The remote desktop accepted the login; a password marked to remember is saved now.
    Connected,
    /// The RSA key a VNC server presented in an RSA-AES handshake.
    ServerKey {
        bits: u32,
        /// SHA-256 of the key as the server sent it (length, modulus, exponent).
        fingerprint: String,
        /// The first 8 bytes of its SHA-1, which VNC servers and viewers show.
        signature: String,
    },
}

fn is_colon_hex_fingerprint(value: &str) -> bool {
    // ^[0-9A-F]{2}(?::[0-9A-F]{2}){31}$
    let parts: Vec<&str> = value.split(':').collect();
    parts.len() == 32
        && parts.iter().all(|part| {
            part.len() == 2
                && part
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
        })
}

fn is_dash_hex_signature(value: &str) -> bool {
    // ^[0-9a-f]{2}(?:-[0-9a-f]{2}){7}$
    let parts: Vec<&str> = value.split('-').collect();
    parts.len() == 8
        && parts.iter().all(|part| {
            part.len() == 2
                && part
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

impl Validate for DesktopClientMessage {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        match self {
            DesktopClientMessage::Connect { profile } => {
                profile.validate_at(&join(path, "profile"))
            }
            DesktopClientMessage::AuthResponse(response) => response.validate_at(path),
            DesktopClientMessage::CredentialsRequest { types } => {
                validate::items(&join(path, "types"), types.len(), 1, 2)
            }
            DesktopClientMessage::ServerKey {
                bits,
                fingerprint,
                signature,
            } => {
                validate::range(&join(path, "bits"), *bits, 1024, 8192)?;
                validate::pattern(
                    &join(path, "fingerprint"),
                    fingerprint,
                    is_colon_hex_fingerprint(fingerprint),
                    "/^[0-9A-F]{2}(?::[0-9A-F]{2}){31}$/",
                )?;
                validate::pattern(
                    &join(path, "signature"),
                    signature,
                    is_dash_hex_signature(signature),
                    "/^[0-9a-f]{2}(?:-[0-9a-f]{2}){7}$/",
                )
            }
            DesktopClientMessage::HostKeyResponse { .. }
            | DesktopClientMessage::CertificateResponse { .. }
            | DesktopClientMessage::Retry { .. }
            | DesktopClientMessage::Connected => Ok(()),
        }
    }
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DesktopCredentials {
    pub username: Option<String>,
    pub password: Option<String>,
    pub domain: Option<String>,
}

/// What kind of identity a [`DesktopCertificateChallenge`] is about.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DesktopIdentity {
    /// An RDP server's (or gNMI device's) TLS certificate.
    #[serde(rename_all = "camelCase")]
    Certificate {
        subject: String,
        issuer: String,
        valid_from: String,
        valid_to: String,
        /// Why the certificate could not be verified against trusted authorities.
        verification_error: Option<String>,
    },
    /// The RSA key of a VNC server using RSA-AES.
    RsaKey {
        bits: u32,
        /// Short form VNC servers and viewers show, to compare against.
        signature: String,
    },
}

/// A server identity that is not trusted yet.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopCertificateChallenge {
    pub host: String,
    pub port: u16,
    /// Colon-separated SHA-256 of the certificate or key; the value that is pinned.
    pub fingerprint: String,
    pub state: TrustState,
    pub previous: Option<String>,
    #[serde(flatten)]
    pub identity: DesktopIdentity,
}

/// What a remote desktop session reports to the UI.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum DesktopServerMessage {
    Status {
        message: String,
        transient: Option<bool>,
    },
    AuthPrompt(AuthPromptInfo),
    HostKey(HostKeyChallenge),
    Certificate(DesktopCertificateChallenge),
    /// A ticket for the stream, plus the logon for RDP (NLA runs in the
    /// client). `profile` is what the backend dialed: for a saved host, its
    /// current settings rather than the tab's snapshot.
    Ready {
        ticket: String,
        profile: DesktopProfile,
        credentials: Option<DesktopCredentials>,
    },
    /// Answer to `credentials-request`.
    Credentials {
        credentials: DesktopCredentials,
    },
    /// Answer to `server-key`: whether the VNC client may continue with this key.
    ServerKeyVerdict {
        accept: bool,
    },
    Exit {
        message: Option<String>,
        reason: ExitReason,
    },
}

// ---------------------------------------------------------------------------
// Management sessions
// ---------------------------------------------------------------------------

const MAX_MANAGEMENT_PAYLOAD: usize = 16 * 1024 * 1024;

fn request_id(path: &str, id: &str) -> Result<(), ValidationError> {
    validate::string(&join(path, "id"), id, 1, 64)
}

fn gnmi_path_text(path: &str, value: &str) -> Result<(), ValidationError> {
    validate::string(path, value, 0, 4096)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GnmiSetValue {
    pub path: String,
    /// JSON text for json/json_ietf, raw text for ascii.
    pub value: String,
    pub encoding: GnmiSetEncoding,
}

/// gNMI commit-confirmed extension.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum GnmiCommit {
    #[serde(rename_all = "camelCase")]
    Commit {
        id: String,
        rollback_seconds: Option<u32>,
    },
    Confirm {
        id: String,
    },
    Cancel {
        id: String,
    },
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GnmiSubscription {
    pub path: String,
    #[serde(default)]
    pub mode: GnmiSubscriptionMode,
    pub sample_interval_ms: Option<u64>,
    pub suppress_redundant: Option<bool>,
    pub heartbeat_interval_ms: Option<u64>,
}

/// What the UI sends to a management session.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ManagementClientMessage {
    Connect {
        profile: ManagementProfile,
    },
    AuthResponse(AuthPromptResponse),
    HostKeyResponse {
        accept: bool,
    },
    CertificateResponse {
        accept: bool,
    },
    GnmiCapabilities {
        id: String,
    },
    #[serde(rename_all = "camelCase")]
    GnmiGet {
        id: String,
        prefix: Option<String>,
        paths: Vec<String>,
        #[serde(rename = "type", default)]
        data_type: GnmiDataType,
        encoding: Option<GnmiEncoding>,
        /// gNMI depth extension: levels below each path to return.
        depth: Option<u32>,
        /// Abandon the request once the response grows past this many bytes.
        max_bytes: Option<u64>,
    },
    GnmiSet {
        id: String,
        prefix: Option<String>,
        #[serde(default)]
        updates: Vec<GnmiSetValue>,
        #[serde(default)]
        replaces: Vec<GnmiSetValue>,
        #[serde(default)]
        deletes: Vec<String>,
        commit: Option<GnmiCommit>,
    },
    #[serde(rename_all = "camelCase")]
    GnmiSubscribe {
        id: String,
        prefix: Option<String>,
        mode: GnmiSubscriptionListMode,
        encoding: Option<GnmiEncoding>,
        updates_only: Option<bool>,
        subscriptions: Vec<GnmiSubscription>,
    },
    /// Ask a POLL subscription for a fresh snapshot.
    GnmiPoll {
        id: String,
    },
    #[serde(rename_all = "camelCase")]
    NetconfRpc {
        id: String,
        /// The operation element(s) the `<rpc>` wraps.
        xml: String,
        timeout_ms: Option<u64>,
    },
    /// A unary gNOI/gNSI call; `method` must be one Muxus knows.
    GrpcCall {
        id: String,
        method: String,
        request: Option<serde_json::Map<String, serde_json::Value>>,
    },
    /// A streaming gNOI/gNSI call; `request` is the first message, if any.
    GrpcStream {
        id: String,
        method: String,
        request: Option<serde_json::Map<String, serde_json::Value>>,
    },
    /// Another message on an open bidirectional call (a Rotate's finalize, say).
    GrpcSend {
        id: String,
        message: serde_json::Map<String, serde_json::Value>,
    },
    /// No more messages from this side of an open call.
    GrpcCloseSend {
        id: String,
    },
    #[serde(rename_all = "camelCase")]
    GnoiFileGet {
        id: String,
        remote_file: String,
    },
    /// Start an upload; the content follows in `gnoi-file-chunk` messages.
    #[serde(rename_all = "camelCase")]
    GnoiFilePut {
        id: String,
        remote_file: String,
        /// Octal digits written as a decimal number (644 is rw-r--r--), as devices report them.
        permissions: Option<u32>,
        size: u64,
    },
    GnoiFileChunk {
        id: String,
        /// Base64, at most a few MiB per chunk.
        data: String,
        last: Option<bool>,
    },
    /// Open a fresh TLS connection to the device and report its certificate.
    TlsProbe {
        id: String,
    },
    /// Stop a running request or subscription.
    Cancel {
        id: String,
    },
}

impl Validate for GnmiSetValue {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        gnmi_path_text(&join(path, "path"), &self.path)?;
        validate::string(&join(path, "value"), &self.value, 0, MAX_MANAGEMENT_PAYLOAD)
    }
}

impl Validate for ManagementClientMessage {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        use ManagementClientMessage as M;
        match self {
            M::Connect { profile } => profile.validate_at(&join(path, "profile")),
            M::AuthResponse(response) => response.validate_at(path),
            M::HostKeyResponse { .. } | M::CertificateResponse { .. } => Ok(()),
            M::GnmiCapabilities { id }
            | M::GnmiPoll { id }
            | M::GrpcCloseSend { id }
            | M::TlsProbe { id }
            | M::Cancel { id } => request_id(path, id),
            M::GnmiGet {
                id,
                prefix,
                paths,
                depth,
                max_bytes,
                ..
            } => {
                request_id(path, id)?;
                if let Some(prefix) = prefix {
                    gnmi_path_text(&join(path, "prefix"), prefix)?;
                }
                validate::items(&join(path, "paths"), paths.len(), 1, 128)?;
                validate::each_string(&join(path, "paths"), paths, 0, 4096)?;
                validate::opt_range(&join(path, "depth"), *depth, 1, 64)?;
                validate::opt_range(
                    &join(path, "maxBytes"),
                    *max_bytes,
                    1024,
                    1024 * 1024 * 1024,
                )
            }
            M::GnmiSet {
                id,
                prefix,
                updates,
                replaces,
                deletes,
                commit,
            } => {
                request_id(path, id)?;
                if let Some(prefix) = prefix {
                    gnmi_path_text(&join(path, "prefix"), prefix)?;
                }
                validate::items(&join(path, "updates"), updates.len(), 0, 512)?;
                updates.validate_at(&join(path, "updates"))?;
                validate::items(&join(path, "replaces"), replaces.len(), 0, 512)?;
                replaces.validate_at(&join(path, "replaces"))?;
                validate::items(&join(path, "deletes"), deletes.len(), 0, 512)?;
                validate::each_string(&join(path, "deletes"), deletes, 0, 4096)?;
                match commit {
                    Some(GnmiCommit::Commit {
                        id,
                        rollback_seconds,
                    }) => {
                        validate::string(&join(path, "commit.id"), id, 1, 200)?;
                        validate::opt_range(
                            &join(path, "commit.rollbackSeconds"),
                            *rollback_seconds,
                            1,
                            86_400,
                        )
                    }
                    Some(GnmiCommit::Confirm { id } | GnmiCommit::Cancel { id }) => {
                        validate::string(&join(path, "commit.id"), id, 1, 200)
                    }
                    None => Ok(()),
                }
            }
            M::GnmiSubscribe {
                id,
                prefix,
                subscriptions,
                ..
            } => {
                request_id(path, id)?;
                if let Some(prefix) = prefix {
                    gnmi_path_text(&join(path, "prefix"), prefix)?;
                }
                validate::items(&join(path, "subscriptions"), subscriptions.len(), 1, 128)?;
                for (index, subscription) in subscriptions.iter().enumerate() {
                    let at = join(path, &format!("subscriptions.{index}"));
                    gnmi_path_text(&join(&at, "path"), &subscription.path)?;
                    validate::opt_range(
                        &join(&at, "sampleIntervalMs"),
                        subscription.sample_interval_ms,
                        1,
                        86_400_000,
                    )?;
                    validate::opt_range(
                        &join(&at, "heartbeatIntervalMs"),
                        subscription.heartbeat_interval_ms,
                        1,
                        86_400_000,
                    )?;
                }
                Ok(())
            }
            M::NetconfRpc {
                id,
                xml,
                timeout_ms,
            } => {
                request_id(path, id)?;
                validate::string(&join(path, "xml"), xml, 1, MAX_MANAGEMENT_PAYLOAD)?;
                validate::opt_range(&join(path, "timeoutMs"), *timeout_ms, 1000, 3_600_000)
            }
            M::GrpcCall { id, method, .. } | M::GrpcStream { id, method, .. } => {
                request_id(path, id)?;
                validate::string(&join(path, "method"), method, 1, 200)
            }
            M::GrpcSend { id, .. } => request_id(path, id),
            M::GnoiFileGet { id, remote_file } => {
                request_id(path, id)?;
                validate::string(&join(path, "remoteFile"), remote_file, 1, 4096)
            }
            M::GnoiFilePut {
                id,
                remote_file,
                permissions,
                ..
            } => {
                request_id(path, id)?;
                validate::string(&join(path, "remoteFile"), remote_file, 1, 4096)?;
                validate::opt_range(&join(path, "permissions"), *permissions, 0, 7777)
            }
            M::GnoiFileChunk { id, data, .. } => {
                request_id(path, id)?;
                validate::string(&join(path, "data"), data, 0, 8 * 1024 * 1024)
            }
        }
    }
}

/// What a management session reports to the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum ManagementServerMessage {
    Status {
        message: String,
        transient: Option<bool>,
    },
    AuthPrompt(AuthPromptInfo),
    HostKey(HostKeyChallenge),
    Certificate(DesktopCertificateChallenge),
    /// `profile` is what the backend dialed: for a saved host, its current settings.
    Ready {
        info: ManagementSessionInfo,
        profile: ManagementProfile,
    },
    Stream(ManagementStreamMessage),
    Exit {
        message: Option<String>,
        reason: ExitReason,
    },
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
enum ManagementServerWire {
    Status {
        message: String,
        transient: Option<bool>,
    },
    AuthPrompt(AuthPromptInfo),
    HostKey(HostKeyChallenge),
    Certificate(DesktopCertificateChallenge),
    Ready {
        info: ManagementSessionInfo,
        profile: ManagementProfile,
    },
    Exit {
        message: Option<String>,
        reason: ExitReason,
    },
}

impl Serialize for ManagementServerMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = match self.clone() {
            ManagementServerMessage::Stream(message) => return message.serialize(serializer),
            ManagementServerMessage::Status { message, transient } => {
                ManagementServerWire::Status { message, transient }
            }
            ManagementServerMessage::AuthPrompt(info) => ManagementServerWire::AuthPrompt(info),
            ManagementServerMessage::HostKey(challenge) => ManagementServerWire::HostKey(challenge),
            ManagementServerMessage::Certificate(challenge) => {
                ManagementServerWire::Certificate(challenge)
            }
            ManagementServerMessage::Ready { info, profile } => {
                ManagementServerWire::Ready { info, profile }
            }
            ManagementServerMessage::Exit { message, reason } => {
                ManagementServerWire::Exit { message, reason }
            }
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ManagementServerMessage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        let op = value
            .get("op")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if ManagementStreamMessage::OPS.contains(&op) {
            return serde_json::from_value(value)
                .map(ManagementServerMessage::Stream)
                .map_err(D::Error::custom);
        }
        Ok(
            match serde_json::from_value(value).map_err(D::Error::custom)? {
                ManagementServerWire::Status { message, transient } => {
                    ManagementServerMessage::Status { message, transient }
                }
                ManagementServerWire::AuthPrompt(info) => ManagementServerMessage::AuthPrompt(info),
                ManagementServerWire::HostKey(challenge) => {
                    ManagementServerMessage::HostKey(challenge)
                }
                ManagementServerWire::Certificate(challenge) => {
                    ManagementServerMessage::Certificate(challenge)
                }
                ManagementServerWire::Ready { info, profile } => {
                    ManagementServerMessage::Ready { info, profile }
                }
                ManagementServerWire::Exit { message, reason } => {
                    ManagementServerMessage::Exit { message, reason }
                }
            },
        )
    }
}

/// Parse and validate a session profile, like `sessionProfileSchema.parse`.
pub fn parse_session_profile(value: serde_json::Value) -> Result<SessionProfile, ValidationError> {
    validate::parse_value(value)
}

/// Parse and validate a terminal message, like `terminalClientMessageSchema.parse`.
pub fn parse_terminal_client_message(
    value: serde_json::Value,
) -> Result<TerminalClientMessage, ValidationError> {
    validate::parse_value(value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn profile(value: serde_json::Value) -> Result<SessionProfile, ValidationError> {
        parse_session_profile(value)
    }

    fn message(value: serde_json::Value) -> Result<TerminalClientMessage, ValidationError> {
        parse_terminal_client_message(value)
    }

    #[test]
    fn offers_a_fixed_selected_protocol_and_a_separate_auth_protocol() {
        assert_eq!(
            terminal_web_socket_protocols("secret"),
            [
                TERMINAL_WS_PROTOCOL.to_owned(),
                "muxus.auth.secret".to_owned()
            ]
        );
    }

    #[test]
    fn accepts_a_minimal_local_profile() {
        assert!(profile(json!({ "kind": "local" })).is_ok());
    }

    #[test]
    fn accepts_structured_local_shell_arguments_and_startup_commands() {
        let input = json!({
            "kind": "local",
            "shell": "wsl.exe",
            "args": ["-d", "Ubuntu"],
            "cwd": "C:\\work",
            "startupCommand": "cd project\nnpm run dev",
        });
        let parsed = profile(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), input);
    }

    #[test]
    fn bounds_local_shell_launch_data() {
        assert!(profile(json!({ "kind": "local", "args": vec!["arg"; 65] })).is_err());
        assert!(profile(json!({ "kind": "local", "startupCommand": "x".repeat(32_769) })).is_err());
    }

    #[test]
    fn accepts_a_full_ssh_profile() {
        assert!(
            profile(json!({
                "kind": "ssh",
                "target": "web",
                "port": 2222,
                "user": "alice",
                "useConfig": false,
                "keepaliveIntervalSeconds": 30,
                "identityFiles": ["~/.ssh/work_ed25519"],
                "identitiesOnly": true,
                "proxyJump": ["bastion", "ops@relay:2200"],
            }))
            .is_ok()
        );
    }

    #[test]
    fn bounds_the_muxus_ssh_keepalive_fallback() {
        assert!(
            profile(json!({ "kind": "ssh", "target": "web", "keepaliveIntervalSeconds": 30 }))
                .is_ok()
        );
        assert!(
            profile(json!({ "kind": "ssh", "target": "web", "keepaliveIntervalSeconds": 0 }))
                .is_err()
        );
        assert!(
            profile(json!({ "kind": "ssh", "target": "web", "keepaliveIntervalSeconds": 3601 }))
                .is_err()
        );
    }

    #[test]
    fn accepts_telnet_profiles_and_supplies_the_standard_port() {
        assert_eq!(
            serde_json::to_value(
                profile(json!({ "kind": "telnet", "host": "router.local" })).unwrap()
            )
            .unwrap(),
            json!({ "kind": "telnet", "host": "router.local", "port": 23 })
        );
        assert!(profile(json!({ "kind": "telnet", "host": "router.local", "port": 2323 })).is_ok());
    }

    #[test]
    fn accepts_serial_profiles_and_supplies_conventional_8n1_defaults() {
        let parsed = profile(json!({ "kind": "serial", "path": "/dev/ttyUSB0" })).unwrap();
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            r#"{"kind":"serial","path":"/dev/ttyUSB0","baudRate":115200,"dataBits":8,"stopBits":1,"parity":"none","flowControl":"none"}"#
        );
        assert!(
            profile(json!({
                "kind": "serial",
                "path": "COM3",
                "baudRate": 9600,
                "dataBits": 7,
                "stopBits": 2,
                "parity": "even",
                "flowControl": "hardware",
            }))
            .is_ok()
        );
        let half = profile(json!({ "kind": "serial", "path": "COM3", "stopBits": 1.5 })).unwrap();
        assert!(
            serde_json::to_string(&half)
                .unwrap()
                .contains(r#""stopBits":1.5"#)
        );
    }

    #[test]
    fn drops_retired_term_overrides_from_legacy_profiles() {
        let parsed =
            profile(json!({ "kind": "ssh", "target": "web", "term": "xterm-kitty" })).unwrap();
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            json!({ "kind": "ssh", "target": "web" })
        );
    }

    #[test]
    fn rejects_ssh_profiles_without_a_target() {
        assert!(profile(json!({ "kind": "ssh", "target": "" })).is_err());
        assert!(profile(json!({ "kind": "ssh" })).is_err());
    }

    #[test]
    fn rejects_out_of_range_ports() {
        assert!(profile(json!({ "kind": "ssh", "target": "x", "port": 0 })).is_err());
        assert!(profile(json!({ "kind": "ssh", "target": "x", "port": 65536 })).is_err());
        assert!(profile(json!({ "kind": "telnet", "host": "x", "port": 0 })).is_err());
    }

    #[test]
    fn keeps_a_serial_break_duration_within_1ms_and_10s() {
        let parsed =
            profile(json!({ "kind": "serial", "path": "COM3", "breakDurationMs": 500 })).unwrap();
        let SessionProfile::Serial(serial) = parsed else {
            panic!("serial")
        };
        assert_eq!(serial.break_duration_ms, Some(500));
        for value in [json!(0), json!(10_001), json!(2.5)] {
            assert!(
                profile(json!({ "kind": "serial", "path": "COM3", "breakDurationMs": value }))
                    .is_err()
            );
        }
    }

    #[test]
    fn rejects_invalid_serial_framing_and_baud_rates() {
        assert!(profile(json!({ "kind": "serial", "path": "COM3", "baudRate": 0 })).is_err());
        assert!(profile(json!({ "kind": "serial", "path": "COM3", "dataBits": 9 })).is_err());
        assert!(profile(json!({ "kind": "serial", "path": "", "parity": "none" })).is_err());
    }

    #[test]
    fn bounds_tunnel_owned_ssh_configuration() {
        let hops: Vec<String> = (0..9).map(|index| format!("jump-{index}")).collect();
        assert!(profile(json!({ "kind": "ssh", "target": "x", "proxyJump": hops })).is_err());
        assert!(profile(json!({ "kind": "ssh", "target": "x", "identityFiles": [""] })).is_err());
    }

    #[test]
    fn accepts_canonical_fingerprints_only() {
        let fingerprint = "SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8";
        for value in [
            fingerprint.to_owned(),
            "MD5:c1:b1:30:29:d7:b8:de:6c:97:77:10:d7:46:41:63:87".to_owned(),
            "ssh-rsa MD5:c1:b1:30:29:d7:b8:de:6c:97:77:10:d7:46:41:63:87".to_owned(),
        ] {
            assert!(
                profile(json!({ "kind": "ssh", "target": "x", "hostKeyFingerprint": value }))
                    .is_ok(),
                "{value}"
            );
        }
        for value in [
            String::new(),
            format!("{fingerprint}="),
            fingerprint.to_lowercase(),
            "SHA256:nope".to_owned(),
            "ssh-rsa-c1-b1-30-29-d7-b8-de-6c-97-77-10-d7-46-41-63-87".to_owned(),
        ] {
            assert!(
                profile(json!({ "kind": "ssh", "target": "x", "hostKeyFingerprint": value }))
                    .is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn accepts_connect_with_profile_and_dimensions() {
        assert!(
            message(json!({
                "op": "connect",
                "profile": { "kind": "ssh", "target": "example.com" },
                "freshTransport": "fresh-1a2b",
                "title": "Production",
                "cols": 80,
                "rows": 24,
            }))
            .is_ok()
        );
    }

    #[test]
    fn accepts_attaching_another_window_to_a_live_terminal() {
        assert!(
            message(
                json!({ "op": "attach", "terminalId": "terminal-live", "cols": 120, "rows": 40 })
            )
            .is_ok()
        );
    }

    #[test]
    fn accepts_a_break_request_without_parameters() {
        let parsed = message(json!({ "op": "send-break" })).unwrap();
        assert_eq!(parsed, TerminalClientMessage::SendBreak);
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            json!({ "op": "send-break" })
        );
    }

    #[test]
    fn accepts_preparing_and_cancelling_a_live_terminal_handoff() {
        assert!(message(json!({ "op": "prepare-transfer" })).is_ok());
        assert!(message(json!({ "op": "cancel-transfer" })).is_ok());
    }

    #[test]
    fn rejects_connect_with_non_positive_dimensions() {
        assert!(
            message(
                json!({ "op": "connect", "profile": { "kind": "local" }, "cols": 0, "rows": 24 })
            )
            .is_err()
        );
    }

    #[test]
    fn accepts_resize_auth_response_host_key_response() {
        assert!(message(json!({ "op": "resize", "cols": 120, "rows": 40 })).is_ok());
        assert!(
            message(
                json!({ "op": "auth-response", "answers": ["hunter2"], "rememberPassword": true })
            )
            .is_ok()
        );
        assert!(message(json!({ "op": "auth-response", "answers": [], "skipped": true })).is_ok());
        assert!(message(json!({ "op": "host-key-response", "accept": true })).is_ok());
    }

    #[test]
    fn bounds_authentication_answers() {
        assert!(message(json!({ "op": "auth-response", "answers": ["x".repeat(8193)] })).is_err());
    }

    #[test]
    fn requires_an_actual_lifecycle_pause_or_privacy_change_for_logging_controls() {
        assert!(message(json!({ "op": "set-logging", "enabled": true })).is_ok());
        assert!(message(json!({ "op": "set-logging", "paused": true })).is_ok());
        assert!(message(json!({ "op": "set-logging", "captureInput": false })).is_ok());
        assert!(message(json!({ "op": "set-logging" })).is_err());
    }

    #[test]
    fn starts_a_log_file_with_or_without_a_chosen_path_never_with_a_path_alone() {
        assert!(message(json!({ "op": "set-logging", "logToFile": true })).is_ok());
        assert!(
            message(json!({ "op": "set-logging", "logToFile": true, "logFilePath": "/home/user/router.log" })).is_ok()
        );
        assert!(
            message(json!({ "op": "set-logging", "logFilePath": "/home/user/router.log" }))
                .is_err()
        );
        assert!(
            message(json!({ "op": "set-logging", "logToFile": true, "logFilePath": "   " }))
                .is_err()
        );
    }

    #[test]
    fn accepts_tab_renames_with_a_non_blank_title() {
        assert!(message(json!({ "op": "set-title", "title": "Pre-change" })).is_ok());
        assert!(message(json!({ "op": "set-title", "title": "   " })).is_err());
        assert!(message(json!({ "op": "set-title", "title": "x".repeat(501) })).is_err());
    }

    #[test]
    fn accepts_a_shell_less_dial_for_ssh_targets_only() {
        assert!(
            message(json!({ "op": "dial", "profile": { "kind": "ssh", "target": "web" } })).is_ok()
        );
        assert!(message(json!({ "op": "dial", "profile": { "kind": "local" } })).is_err());
    }

    #[test]
    fn rejects_unknown_ops() {
        assert!(message(json!({ "op": "exec", "cmd": "rm -rf /" })).is_err());
    }

    #[test]
    fn server_messages_keep_their_wire_shape() {
        let host_key = TerminalServerMessage::HostKey(HostKeyChallenge {
            host: "web".into(),
            port: 22,
            key_type: "ssh-ed25519".into(),
            fingerprint: "SHA256:x".into(),
            state: TrustState::New,
            previous: None,
            hop: None,
        });
        assert_eq!(
            serde_json::to_string(&host_key).unwrap(),
            r#"{"op":"host-key","host":"web","port":22,"keyType":"ssh-ed25519","fingerprint":"SHA256:x","state":"new"}"#
        );
        let prompt = TerminalServerMessage::AuthPrompt(AuthPromptInfo {
            prompts: vec![AuthPromptField {
                prompt: "Password: ".into(),
                echo: false,
            }],
            purpose: Some(AuthPromptPurpose::SshPassword),
            ..Default::default()
        });
        assert_eq!(
            serde_json::to_string(&prompt).unwrap(),
            r#"{"op":"auth-prompt","prompts":[{"prompt":"Password: ","echo":false}],"purpose":"ssh-password"}"#
        );
        let exit = TerminalServerMessage::Exit {
            code: Some(0),
            message: None,
            reason: ExitReason::Completed,
        };
        assert_eq!(
            serde_json::to_string(&exit).unwrap(),
            r#"{"op":"exit","code":0,"reason":"completed"}"#
        );
    }

    #[test]
    fn desktop_certificates_flatten_their_identity() {
        let message = DesktopServerMessage::Certificate(DesktopCertificateChallenge {
            host: "win".into(),
            port: 3389,
            fingerprint: "AA:BB".into(),
            state: TrustState::New,
            previous: None,
            identity: DesktopIdentity::RsaKey {
                bits: 2048,
                signature: "aa-bb".into(),
            },
        });
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            json,
            r#"{"op":"certificate","host":"win","port":3389,"fingerprint":"AA:BB","state":"new","kind":"rsa-key","bits":2048,"signature":"aa-bb"}"#
        );
        assert_eq!(
            serde_json::from_str::<DesktopServerMessage>(&json).unwrap(),
            message
        );
    }

    #[test]
    fn desktop_server_keys_are_checked() {
        let fingerprint = vec!["AB"; 32].join(":");
        let ok: Result<DesktopClientMessage, _> = validate::parse_value(json!({
            "op": "server-key", "bits": 2048, "fingerprint": fingerprint, "signature": "01-23-45-67-89-ab-cd-ef"
        }));
        assert!(ok.is_ok());
        let bad: Result<DesktopClientMessage, _> = validate::parse_value(json!({
            "op": "server-key", "bits": 2048, "fingerprint": "ab", "signature": "01-23-45-67-89-ab-cd-ef"
        }));
        assert!(bad.is_err());
    }

    #[test]
    fn management_messages_route_stream_ops() {
        let json = r#"{"op":"gnmi-sync","id":"r1"}"#;
        let parsed: ManagementServerMessage = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed,
            ManagementServerMessage::Stream(ManagementStreamMessage::GnmiSync { id: "r1".into() })
        );
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
        let status: ManagementServerMessage =
            serde_json::from_str(r#"{"op":"status","message":"hi"}"#).unwrap();
        assert!(matches!(status, ManagementServerMessage::Status { .. }));
        let get: ManagementClientMessage =
            validate::parse_value(json!({ "op": "gnmi-get", "id": "1", "paths": ["/system"] }))
                .unwrap();
        assert_eq!(
            serde_json::to_string(&get).unwrap(),
            r#"{"op":"gnmi-get","id":"1","paths":["/system"],"type":"all"}"#
        );
        assert!(
            validate::parse_value::<ManagementClientMessage>(
                json!({ "op": "gnmi-get", "id": "1", "paths": [] })
            )
            .is_err()
        );
    }
}
