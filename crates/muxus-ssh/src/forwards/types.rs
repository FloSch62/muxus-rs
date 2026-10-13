//! Forward and saved-tunnel DTOs (`ForwardRequest`, `ForwardInfo`, `TunnelRecord` and
//! `TunnelSshOptions` in `shared/src/api-types.ts`), shared through `muxus-core`, plus the small
//! constructors the forward layer uses on them.

pub use muxus_core::api::{
    ForwardInfo, ForwardLifecycle, ForwardOrigin, ForwardRequest, ForwardStatus, ForwardType,
    TunnelRecord, TunnelSshOptions,
};

use crate::types::ConfigForward;

/// Constructors for forward requests.
pub trait ForwardRequestExt: Sized {
    /// A SOCKS forward listening on `bind_port`.
    fn dynamic(conn_id: impl Into<String>, bind_port: u16) -> Self;
    /// A local or remote forward to `host:port`.
    fn to(
        conn_id: impl Into<String>,
        kind: ForwardType,
        bind_port: u16,
        host: impl Into<String>,
        port: u16,
    ) -> Self;
    /// The request a terminal session makes for a *Forward line of its resolved config.
    fn from_config(conn_id: impl Into<String>, forward: &ConfigForward) -> Self;
    /// The same rule, realizing a saved tunnel.
    fn with_tunnel(self, tunnel_id: impl Into<String>) -> Self;
}

impl ForwardRequestExt for ForwardRequest {
    fn dynamic(conn_id: impl Into<String>, bind_port: u16) -> Self {
        Self {
            conn_id: conn_id.into(),
            kind: ForwardType::Dynamic,
            bind_port,
            target_host: None,
            target_port: None,
            tunnel_id: None,
        }
    }

    fn to(
        conn_id: impl Into<String>,
        kind: ForwardType,
        bind_port: u16,
        host: impl Into<String>,
        port: u16,
    ) -> Self {
        Self {
            conn_id: conn_id.into(),
            kind,
            bind_port,
            target_host: Some(host.into()),
            target_port: Some(port),
            tunnel_id: None,
        }
    }

    fn from_config(conn_id: impl Into<String>, forward: &ConfigForward) -> Self {
        Self {
            conn_id: conn_id.into(),
            kind: forward.kind,
            bind_port: forward.bind_port,
            target_host: forward.target_host.clone(),
            target_port: forward.target_port,
            tunnel_id: None,
        }
    }

    fn with_tunnel(mut self, tunnel_id: impl Into<String>) -> Self {
        self.tunnel_id = Some(tunnel_id.into());
        self
    }
}

/// What [`ForwardManager::start_config`](super::ForwardManager::start_config) did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigForwardStart {
    pub info: ForwardInfo,
    /// False when the rule was already running (or being started) on the connection.
    pub started: bool,
}
