//! The host catalog model: the shared domain types of `muxus-core` (ports of
//! `shared/src/api-types.ts` and `ws-protocol.ts`), re-exported under the
//! names the app uses, plus the few helpers the UI needs on top of them.
//!
//! Everything the window reads about hosts comes through these types, whether
//! it was loaded from the backend or from the demo catalog
//! ([`crate::data_source`]).

pub mod demo;
pub mod demo_shell;
pub mod palette;
pub mod session_logging;

pub use muxus_core::api::{
    ConfigForward, ForwardType as ForwardKind, HostBlockOptions, ResolvedHostSettings,
    SavedHostKind, SavedHostProfile, SshConfigResponse as SshConfig, SshHostEntry,
};
pub use muxus_core::management::GnmiTlsMode;
pub use muxus_core::protocol::{
    GnmiProfile, LocalProfile, NetconfProfile, ProfileKind, RdpProfile, RequestTty,
    SavedHostSessionProfile, SerialProfile, SessionProfile, SshGateway, SshProfile,
    StrictHostKeyChecking, TelnetProfile, TerminalProfile, VncProfile,
};
pub use muxus_store::prefs::state::FolderStyle;

/// `OpenSshProfileMetadata`: Muxus-only metadata on an ssh_config alias or a
/// saved host. Connection details still resolve live from OpenSSH.
pub type HostMetadata = muxus_core::api::OpenSshProfileMetadata;

/// A local shell profile with the backend's default shell.
pub fn local_profile() -> SessionProfile {
    SessionProfile::Local(LocalProfile::default())
}

/// A plain SSH profile for `target`, every option at its default.
pub fn ssh_profile(target: impl Into<String>) -> SessionProfile {
    SessionProfile::Ssh(SshProfile::new(target))
}

/// A Telnet profile.
pub fn telnet_profile(host: impl Into<String>, port: u16) -> SessionProfile {
    SessionProfile::Telnet(TelnetProfile {
        profile_id: None,
        host: host.into(),
        port,
    })
}

/// Helpers on [`SavedHostProfile`].
pub trait SavedHostProfileExt {
    /// The connection kind (`SavedHostProfile.kind` widened to every profile kind).
    fn profile_kind(&self) -> ProfileKind;
    /// The stored profile as a session profile.
    fn session_profile(&self) -> SessionProfile;
}

impl SavedHostProfileExt for SavedHostProfile {
    fn profile_kind(&self) -> ProfileKind {
        profile_kind_of(self.kind)
    }

    fn session_profile(&self) -> SessionProfile {
        self.profile.clone().into()
    }
}

/// A saved host's kind as a profile kind.
pub fn profile_kind_of(kind: SavedHostKind) -> ProfileKind {
    match kind {
        SavedHostKind::Ssh => ProfileKind::Ssh,
        SavedHostKind::Telnet => ProfileKind::Telnet,
        SavedHostKind::Serial => ProfileKind::Serial,
        SavedHostKind::Rdp => ProfileKind::Rdp,
        SavedHostKind::Vnc => ProfileKind::Vnc,
        SavedHostKind::Gnmi => ProfileKind::Gnmi,
        SavedHostKind::Netconf => ProfileKind::Netconf,
    }
}

/// Everything the sidebar reads about hosts: both host sources.
#[derive(Clone, Debug, PartialEq)]
pub struct Catalog {
    pub ssh: SshConfig,
    pub profiles: Vec<SavedHostProfile>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            ssh: SshConfig {
                path: String::new(),
                files: Vec::new(),
                hosts: Vec::new(),
                error: None,
            },
            profiles: Vec::new(),
        }
    }
}

/// The JSON spelling of a unit enum value (`"accept-new"`, `"json_ietf"`), as
/// the TypeScript strings these enums replace.
pub fn wire_name<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        _ => String::new(),
    }
}
