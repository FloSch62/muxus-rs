//! The reviewed-session model every importer produces and its conversion
//! into portable connections (port of `client/src/session-import.ts`).

use std::collections::HashSet;
use std::sync::Arc;

use muxus_core::api::{HostBlockOptions, saved_host_hop};
use muxus_core::protocol::{
    RdpProfile, RequestTty, SavedHostSessionProfile, SerialFlowControl, SerialParity,
    SerialProfile, SshGateway, SshProfile, StopBits, VncProfile,
};
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::portable::{
    PortableConnections, PortableHostMetadata, PortableSavedHost, PortableSshHost,
};
use crate::text::stable_import_id;

/// SSH host a session is reached through. Sessions sharing the same hop (and
/// the same hops before it) share one value, so it is imported once.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedJumpHost {
    /// Stable inside one parsed file; identifies the hop and every hop before it.
    pub id: String,
    /// ssh_config alias used when jump hosts are written to OpenSSH config.
    pub alias: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub identity_file: Option<String>,
    /// Hop dialed before this one, if the chain has several.
    pub via: Option<Arc<ImportedJumpHost>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportedAuthMode {
    Key,
    Password,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedSshSession {
    /// Stable inside one parsed file and used by review selection controls.
    pub id: String,
    pub name: String,
    pub alias: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub folder: Option<String>,
    pub auth_mode: ImportedAuthMode,
    pub identity_file: Option<String>,
    /// Command run in a terminal instead of the login shell.
    pub remote_command: Option<String>,
    /// Last jump host before the target.
    pub jump_host: Option<Arc<ImportedJumpHost>>,
}

/// An RDP or VNC session; the enum variant carries which.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedDesktopSession {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub folder: Option<String>,
    /// SSH host the desktop is tunnelled through.
    pub jump_host: Option<Arc<ImportedJumpHost>>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedSerialSession {
    pub id: String,
    pub name: String,
    /// Deterministic across imports so keep/replace can identify this profile.
    pub profile_id: String,
    pub path: String,
    pub baud_rate: u32,
    pub data_bits: u8,
    pub stop_bits: StopBits,
    pub parity: SerialParity,
    pub flow_control: SerialFlowControl,
    pub folder: Option<String>,
}

/// One importable session, tagged by `kind` as in the TypeScript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ImportedSession {
    Ssh(ImportedSshSession),
    Rdp(ImportedDesktopSession),
    Vnc(ImportedDesktopSession),
    Serial(ImportedSerialSession),
}

/// `kind` of an [`ImportedSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportedSessionKind {
    Ssh,
    Rdp,
    Vnc,
    Serial,
}

impl ImportedSessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ImportedSessionKind::Ssh => "ssh",
            ImportedSessionKind::Rdp => "rdp",
            ImportedSessionKind::Vnc => "vnc",
            ImportedSessionKind::Serial => "serial",
        }
    }
}

impl ImportedSession {
    pub fn kind(&self) -> ImportedSessionKind {
        match self {
            ImportedSession::Ssh(_) => ImportedSessionKind::Ssh,
            ImportedSession::Rdp(_) => ImportedSessionKind::Rdp,
            ImportedSession::Vnc(_) => ImportedSessionKind::Vnc,
            ImportedSession::Serial(_) => ImportedSessionKind::Serial,
        }
    }

    pub fn id(&self) -> &str {
        match self {
            ImportedSession::Ssh(session) => &session.id,
            ImportedSession::Rdp(session) | ImportedSession::Vnc(session) => &session.id,
            ImportedSession::Serial(session) => &session.id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            ImportedSession::Ssh(session) => &session.name,
            ImportedSession::Rdp(session) | ImportedSession::Vnc(session) => &session.name,
            ImportedSession::Serial(session) => &session.name,
        }
    }

    pub fn folder(&self) -> Option<&str> {
        match self {
            ImportedSession::Ssh(session) => session.folder.as_deref(),
            ImportedSession::Rdp(session) | ImportedSession::Vnc(session) => {
                session.folder.as_deref()
            }
            ImportedSession::Serial(session) => session.folder.as_deref(),
        }
    }

    /// The SSH host the session goes through, for SSH and desktop sessions.
    pub fn jump_host(&self) -> Option<&Arc<ImportedJumpHost>> {
        match self {
            ImportedSession::Ssh(session) => session.jump_host.as_ref(),
            ImportedSession::Rdp(session) | ImportedSession::Vnc(session) => {
                session.jump_host.as_ref()
            }
            ImportedSession::Serial(_) => None,
        }
    }
}

/// Where imported SSH hosts are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportedSshStorage {
    /// Host blocks in ssh_config that also work with `ssh` in any terminal.
    #[default]
    Openssh,
    /// Saved Muxus hosts; ssh_config is not changed.
    Muxus,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedImportedSession {
    /// Stable inside one parsed file and used as the list key.
    pub id: String,
    /// Session name, or the hostname/path when the source omitted a name.
    pub name: String,
    pub folder: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedSessionParseResult {
    pub sessions: Vec<ImportedSession>,
    /// Recognizable session entries that were unsupported or incomplete.
    pub ignored_count: usize,
    /// Every ignored session together with the reason it cannot be imported.
    pub skipped_sessions: Vec<SkippedImportedSession>,
}

/// Why a file could not be imported. The message is shown to the user as is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ImportError(pub String);

impl ImportError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Convert reviewed third-party rows into the portable restore pipeline.
///
/// Jump hosts the sessions go through are appended after the sessions, once
/// each, in a "<source> jump hosts" folder: as saved Muxus SSH hosts the
/// sessions name as hops, or as Host blocks when SSH hosts go to ssh_config.
pub fn imported_connections<'a>(
    sessions: impl IntoIterator<Item = &'a ImportedSession>,
    source_name: &str,
    ssh_storage: ImportedSshStorage,
) -> PortableConnections {
    let mut ssh_hosts = Vec::new();
    let mut saved_hosts = Vec::new();
    let id_prefix = id_prefix(source_name);
    let mut jumps = JumpHosts::default();
    let muxus = ssh_storage == ImportedSshStorage::Muxus;

    let jump_profile_id =
        |jump: &ImportedJumpHost| stable_import_id(&format!("{id_prefix}-jump"), &jump.id);
    let jump_hop = |jumps: &mut JumpHosts, jump: &Arc<ImportedJumpHost>| {
        jumps.include(jump);
        if muxus {
            saved_host_hop(&jump_profile_id(jump))
        } else {
            jump.alias.clone()
        }
    };

    for session in sessions {
        match session {
            ImportedSession::Ssh(session) => {
                let proxy_jump = session
                    .jump_host
                    .as_ref()
                    .map(|jump| vec![jump_hop(&mut jumps, jump)]);
                let route = SshRoute {
                    user: session.username.clone(),
                    port: (session.port != 22).then_some(session.port),
                    identity_files: session.identity_file.clone().map(|file| vec![file]),
                    proxy_jump,
                    password_only: (session.auth_mode == ImportedAuthMode::Password)
                        .then_some(true),
                    // Interactive commands such as `sudo su -` need a terminal, as in the source app.
                    remote_command: session.remote_command.clone(),
                    request_tty: session.remote_command.as_ref().map(|_| RequestTty::Yes),
                };
                if muxus {
                    saved_hosts.push(PortableSavedHost {
                        id: stable_import_id(&format!("{id_prefix}-ssh"), &session.id),
                        name: session.name.clone(),
                        profile: SavedHostSessionProfile::Ssh(route.saved_profile(&session.host)),
                        metadata: PortableHostMetadata::in_group(session.folder.as_deref()),
                    });
                    continue;
                }
                ssh_hosts.push(PortableSshHost {
                    alias: session.alias.clone(),
                    aliases: vec![session.alias.clone()],
                    description: Some(format!("Imported from {source_name}.")),
                    options: route.host_block(&session.host),
                    metadata: Some(PortableHostMetadata {
                        display_name: Some(session.name.clone()),
                        group: session.folder.clone(),
                        ..PortableHostMetadata::default()
                    }),
                });
            }
            ImportedSession::Rdp(desktop) | ImportedSession::Vnc(desktop) => {
                let ssh_gateway = desktop.jump_host.as_ref().map(|jump| {
                    jumps.include(jump);
                    if muxus {
                        SshGateway {
                            target: jump.host.clone(),
                            profile_id: Some(jump_profile_id(jump)),
                        }
                    } else {
                        SshGateway {
                            target: jump.alias.clone(),
                            profile_id: None,
                        }
                    }
                });
                let (kind, profile) = match session {
                    ImportedSession::Rdp(_) => (
                        "rdp",
                        SavedHostSessionProfile::Rdp(RdpProfile {
                            profile_id: None,
                            host: desktop.host.clone(),
                            port: desktop.port,
                            username: desktop.username.clone(),
                            domain: None,
                            ssh_gateway,
                            share_clipboard: None,
                        }),
                    ),
                    _ => (
                        "vnc",
                        SavedHostSessionProfile::Vnc(VncProfile {
                            profile_id: None,
                            host: desktop.host.clone(),
                            port: desktop.port,
                            username: desktop.username.clone(),
                            ssh_gateway,
                            resize_remote: None,
                            view_only: None,
                            share_clipboard: None,
                        }),
                    ),
                };
                saved_hosts.push(PortableSavedHost {
                    id: stable_import_id(&format!("{id_prefix}-{kind}"), &desktop.id),
                    name: desktop.name.clone(),
                    profile,
                    metadata: PortableHostMetadata::in_group(desktop.folder.as_deref()),
                });
            }
            ImportedSession::Serial(serial) => {
                saved_hosts.push(PortableSavedHost {
                    id: serial.profile_id.clone(),
                    name: serial.name.clone(),
                    profile: SavedHostSessionProfile::Serial(SerialProfile {
                        profile_id: None,
                        path: serial.path.clone(),
                        baud_rate: serial.baud_rate,
                        data_bits: serial.data_bits,
                        stop_bits: serial.stop_bits,
                        parity: serial.parity,
                        flow_control: serial.flow_control,
                        break_duration_ms: None,
                    }),
                    metadata: PortableHostMetadata::in_group(serial.folder.as_deref()),
                });
            }
        }
    }

    let jump_group = format!("{source_name} jump hosts");
    // Every hop before a jump host was included with it, so this never grows
    // while it is walked.
    let ordered: Vec<Arc<ImportedJumpHost>> = jumps.ordered.clone();
    for jump in &ordered {
        let proxy_jump = jump.via.as_ref().map(|via| vec![jump_hop(&mut jumps, via)]);
        let route = SshRoute {
            user: jump.username.clone(),
            port: (jump.port != 22).then_some(jump.port),
            identity_files: jump.identity_file.clone().map(|file| vec![file]),
            password_only: jump.identity_file.is_none().then_some(true),
            proxy_jump,
            remote_command: None,
            request_tty: None,
        };
        if muxus {
            saved_hosts.push(PortableSavedHost {
                id: jump_profile_id(jump),
                name: jump.name.clone(),
                profile: SavedHostSessionProfile::Ssh(route.saved_profile(&jump.host)),
                metadata: PortableHostMetadata::in_group(Some(&jump_group)),
            });
            continue;
        }
        ssh_hosts.push(PortableSshHost {
            alias: jump.alias.clone(),
            aliases: vec![jump.alias.clone()],
            description: Some(format!("Jump host imported from {source_name}.")),
            options: route.host_block(&jump.host),
            metadata: Some(PortableHostMetadata {
                display_name: Some(jump.name.clone()),
                group: Some(jump_group.clone()),
                ..PortableHostMetadata::default()
            }),
        });
    }

    PortableConnections {
        ssh_hosts,
        saved_hosts,
        host_order: Vec::new(),
    }
}

/// `sourceName.toLowerCase().replace(/[^a-z0-9]+/g, '-')`.
fn id_prefix(source_name: &str) -> String {
    let lower = source_name.to_lowercase();
    let mut prefix = String::with_capacity(lower.len());
    let mut in_run = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            prefix.push(c);
            in_run = false;
        } else if !in_run {
            prefix.push('-');
            in_run = true;
        }
    }
    prefix
}

/// Jump hosts in first-use order, each hop once.
#[derive(Default)]
struct JumpHosts {
    seen: HashSet<String>,
    ordered: Vec<Arc<ImportedJumpHost>>,
}

impl JumpHosts {
    /// Add a hop and every hop before it that is not known yet.
    fn include(&mut self, jump: &Arc<ImportedJumpHost>) {
        let mut hop = Some(jump);
        while let Some(current) = hop {
            if !self.seen.insert(current.id.clone()) {
                break;
            }
            self.ordered.push(Arc::clone(current));
            hop = current.via.as_ref();
        }
    }
}

/// The connection fields an imported SSH session or jump host sets.
struct SshRoute {
    user: Option<String>,
    port: Option<u16>,
    identity_files: Option<Vec<String>>,
    proxy_jump: Option<Vec<String>>,
    password_only: Option<bool>,
    remote_command: Option<String>,
    request_tty: Option<RequestTty>,
}

impl SshRoute {
    fn saved_profile(self, target: &str) -> SshProfile {
        SshProfile {
            target: target.to_owned(),
            use_config: Some(false),
            user: self.user,
            port: self.port,
            identity_files: self.identity_files,
            proxy_jump: self.proxy_jump,
            password_only: self.password_only,
            remote_command: self.remote_command,
            request_tty: self.request_tty,
            ..SshProfile::default()
        }
    }

    fn host_block(self, hostname: &str) -> HostBlockOptions {
        HostBlockOptions {
            hostname: Some(hostname.to_owned()),
            user: self.user,
            port: self.port,
            identity_files: self.identity_files,
            proxy_jump: self.proxy_jump,
            password_only: self.password_only,
            remote_command: self.remote_command,
            request_tty: self.request_tty,
            ..HostBlockOptions::default()
        }
    }
}
