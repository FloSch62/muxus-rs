//! Local, remote and dynamic (SOCKS) port forwards over SSH connections, and saved tunnels that
//! start with Muxus or reconnect on their own (port of `server/src/forwards`).
//!
//! [`ForwardManager`] owns every running forward. Each holds a forward lease on its connection
//! ([`crate::connection::SshConnectionManager`]), so a forward keeps its transport alive after the
//! terminal that opened it is gone. The session layer drives it the way the TypeScript routes and
//! terminal socket do:
//!
//! - `GET/POST/DELETE/PATCH /api/forwards` → [`ForwardManager::list`], [`ForwardManager::start`],
//!   [`ForwardManager::stop`], [`ForwardManager::assign_tunnel`];
//! - saving or deleting a tunnel → [`ForwardManager::tunnel_changed`];
//! - startup → [`ForwardManager::autostart`] with the saved tunnels;
//! - a terminal's *Forward config lines → [`ForwardManager::start_config`] (use
//!   [`ForwardRequest::from_config`]), and [`ForwardManager::stop_session_for_connection`] once
//!   the last terminal on the connection is gone;
//! - shutdown → [`ForwardManager::stop_all`].
//!
//! Status changes (starting, reconnecting, error, active again) are published through
//! [`ForwardManager::subscribe`]. Errors are [`crate::Problem`]s carrying the HTTP status the
//! TypeScript routes answered with.

mod listeners;
mod manager;
mod socks;
mod types;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use manager::{
    ForwardConnections, ForwardManager, ForwardManagerOptions, NEEDS_SIGN_IN,
    TUNNEL_RECONNECT_DELAYS, TUNNEL_RECONNECT_STABLE, TunnelLookup, tunnel_profile,
};
pub use types::{
    ConfigForwardStart, ForwardInfo, ForwardLifecycle, ForwardOrigin, ForwardRequest,
    ForwardRequestExt, ForwardStatus, ForwardType, TunnelRecord, TunnelSshOptions,
};
