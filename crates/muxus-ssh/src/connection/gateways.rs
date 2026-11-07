//! SSH gateways a desktop, management or web connection is reached through, tried in order
//! (`server/src/ssh/gateways.ts`).

use std::sync::Arc;

use muxus_core::gateways::{gateway_retry_status, stops_fallback};
use muxus_core::protocol::SshGateway;

use super::io::{BoxFuture, ConnectIo};
use super::leases::ConnectionLeaseOwner;
use super::manager::{ConnectFailure, ConnectOptions, MuxedConnectionLease, SshConnectionManager};
use super::profile::SshProfile;

pub use muxus_core::gateways::gateway_candidates;

/// The SSH profile a gateway is dialed with.
pub fn gateway_ssh_profile(gateway: &SshGateway) -> SshProfile {
    SshProfile {
        profile_id: gateway.profile_id.clone(),
        ..SshProfile::new(gateway.target.clone())
    }
}

/// What dials one gateway; the connection manager in production.
pub trait GatewayConnector: Send + Sync {
    fn connect_gateway<'a>(
        &'a self,
        profile: &'a SshProfile,
        io: Arc<dyn ConnectIo>,
        owner: ConnectionLeaseOwner,
    ) -> BoxFuture<'a, Result<MuxedConnectionLease, ConnectFailure>>;
}

impl GatewayConnector for SshConnectionManager {
    fn connect_gateway<'a>(
        &'a self,
        profile: &'a SshProfile,
        io: Arc<dyn ConnectIo>,
        owner: ConnectionLeaseOwner,
    ) -> BoxFuture<'a, Result<MuxedConnectionLease, ConnectFailure>> {
        Box::pin(self.connect(profile, io, owner, ConnectOptions::default()))
    }
}

/// Connect to the first gateway that answers (`connectGateway`). A gateway that fails passes
/// the attempt to the next one, saying so in the status line; a cancelled sign-in or a rejected
/// host key stops there. The last failure is returned.
pub async fn connect_gateway(
    connector: &dyn GatewayConnector,
    candidates: &[SshGateway],
    io: Arc<dyn ConnectIo>,
    owner: ConnectionLeaseOwner,
    closed: &(dyn Fn() -> bool + Send + Sync),
) -> Result<(MuxedConnectionLease, SshGateway), String> {
    if candidates.is_empty() {
        return Err("no SSH gateway is set".into());
    }
    for (index, gateway) in candidates.iter().enumerate() {
        io.status(
            &format!("Connecting to the SSH gateway {} …", gateway.target),
            true,
        );
        let profile = gateway_ssh_profile(gateway);
        match connector.connect_gateway(&profile, io.clone(), owner).await {
            Ok(lease) => return Ok((lease, gateway.clone())),
            Err(err) => {
                let message = err.to_string();
                let Some(next) = candidates.get(index + 1) else {
                    return Err(message);
                };
                if closed() || stops_fallback(&message) {
                    return Err(message);
                }
                io.status(
                    &gateway_retry_status(&gateway.target, &message, &next.target),
                    false,
                );
            }
        }
    }
    unreachable!("the last gateway returns its own failure")
}
