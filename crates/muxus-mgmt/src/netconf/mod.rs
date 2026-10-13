//! NETCONF over SSH (RFC 6241, RFC 6242): framing, the few bits of XML the
//! backend reads, and the client. The SSH side is not here: the session
//! hands the client an open `netconf` subsystem channel.

pub mod client;
pub mod framing;
pub mod xml;

pub use client::{BoxedChannel, NetconfChannel, NetconfClient, NetconfNotification, NetconfReply};
