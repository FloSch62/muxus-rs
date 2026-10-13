//! X11 forwarding: display detection, Xauthority, the setup-rewriting proxy and the local X server.
//!
//! Port of `server/src/x11`. The flow per SSH transport:
//!
//! 1. [`LocalX11::attach`] gives the transport an [`X11Transport`] with its own random fake cookie.
//! 2. Before a session channel starts its shell or command, [`X11Transport::request`] says whether
//!    to send `x11-req` and with which parameters ([`X11Request`]). When the server answers with
//!    failure, the SSH layer calls [`X11Transport::mark_refused`] and carries on without X11.
//! 3. For every `x11` channel the server opens, [`X11Transport::open_local`] connects to the local
//!    X server (refuse the channel when that fails), and [`X11Transport::relay`] swaps the fake
//!    cookie for the real one and splices both directions. [`X11Transport::handle_channel`] does
//!    both for a channel that is already accepted.
//!
//! The server only ever sees the fake cookie; the real local credentials never leave this machine.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;

pub mod display;
pub mod local;
pub mod proxy;
pub mod vcxsrv;
pub mod xauthority;

#[cfg(test)]
pub(crate) mod test_support;

pub use display::{
    ParsedDisplay, Platform, X11_TCP_PORT_BASE, X11Endpoint, X11Socket, connect_x11_endpoint,
    parse_display, xauth_target,
};
pub use local::{
    EnvLookup, LocalX11, LocalX11Connection, LocalX11Options, X11_REFUSED_MESSAGE,
    X11_REJECTION_MESSAGE, X11Defaults, X11Domain, X11Request, X11RequestRefused, X11SessionState,
    X11SettingsUpdate, X11Source, X11Status, X11Transport, is_x11_rejection, process_env,
};
pub use proxy::{X11SetupParse, parse_x11_setup, rewrite_x11_setup, splice_x11_connection};
pub use xauthority::{MIT_MAGIC_COOKIE, X11Auth, XauthEntry, XauthTarget};

/// A boxed `Send` future, for the injectable test seams.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Why an X11 operation failed.
///
/// `Clone` so that concurrent callers waiting on one bundled X server start can all receive its
/// outcome.
#[derive(Debug, Clone, thiserror::Error)]
pub enum X11Error {
    #[error("no local X server is available")]
    NoServer,
    #[error("no bundled X server is installed")]
    NoBundledServer,
    #[error("timed out connecting to the local X server")]
    ConnectTimeout,
    /// The remote side of a forwarded connection did not present the fake cookie, or did not
    /// speak X11 at all. The reason is one of the fixed texts the proxy reports.
    #[error("{0}")]
    Rejected(&'static str),
    #[error("the X server is shutting down")]
    ShuttingDown,
    #[error("no free X display number for the bundled X server")]
    NoFreeDisplay,
    #[error("other programs kept taking the display numbers the bundled X server tried")]
    DisplaysTaken,
    #[error("the bundled X server failed to start ({0})")]
    StartFailed(String),
    #[error("the bundled X server did not start in time")]
    StartTimeout,
    #[error("{0}")]
    Io(Arc<io::Error>),
}

impl From<io::Error> for X11Error {
    fn from(err: io::Error) -> Self {
        Self::Io(Arc::new(err))
    }
}
