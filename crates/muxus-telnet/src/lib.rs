//! Telnet transport.
//!
//! [`connect`] opens a session and hands back `muxus_core::stream`
//! channels; [`TelnetCodec`] is the protocol state machine on its own.

pub mod codec;
pub mod transport;

pub use codec::TelnetCodec;
pub use muxus_core::protocol::TelnetProfile;
pub use transport::{CONNECT_TIMEOUT, TelnetError, connect, start};
