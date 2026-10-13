//! SSH connections: the dial plan, the auth ladder, jump hosts, connection sharing and session
//! channels (port of `connection-manager.ts`, `jump-hops.ts` and `connection-leases.ts`).

pub mod auth;
pub mod chain;
pub mod deadline;
pub mod health;
pub mod io;
pub mod jump_hops;
pub mod leases;
pub mod manager;
pub mod profile;
pub mod session;
pub mod transport;

pub use chain::{ChainHop, DialPlanError, build_chain, mux_key};
pub use io::{
    AuthPromptInfo, AuthPromptPurpose, AuthPromptResponse, BoxFuture, Cancelled, ConnectIo,
    HostKeyChallenge, HostKeyState, PasswordVault, PromptField, VaultError, VaultStatus,
};
pub use leases::{ConnectionLeaseOwner, TransportLease};
pub use manager::{
    ConnectFailure, ConnectOptions, ConnectionInfo, ManagedConnection, ManagerOptions,
    MuxedConnectionLease, ShellTransport, SshConnectionManager, SshSessionSummary, TerminalShell,
};
pub use profile::SshProfile;
pub use session::{SessionChannel, SessionSettings, X11Request};
pub use transport::{ConnectError, ConnectErrorKind, HopTransport, ServerChannelSink};

#[cfg(test)]
pub(crate) mod tests;
