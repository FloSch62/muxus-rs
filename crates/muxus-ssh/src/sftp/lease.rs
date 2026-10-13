//! A file browser window's hold on its SSH transport (port of
//! `server/src/ws/sftp-lease-socket.ts`).
//!
//! A detached SFTP window owns a transport lease for as long as it is open. In TypeScript the
//! window's WebSocket lifetime gave reliable cleanup even when the window crashed; here the guard
//! does: dropping it releases the lease, and so does the connection closing underneath it.

use crate::Problem;
use crate::connection::leases::ConnectionLeaseOwner;
use crate::connection::manager::{ConnectionLease, SshConnectionManager};

/// Keeps a connection alive while a file browser (or editor) window shows it.
#[derive(Debug)]
pub struct SftpWindowLease {
    lease: ConnectionLease,
}

impl SftpWindowLease {
    /// Takes an SFTP lease on `conn_id`. A stale id answers 404 "connection not found" (the
    /// socket closed with 1008 in TypeScript).
    pub fn acquire(manager: &SshConnectionManager, conn_id: &str) -> Result<Self, Problem> {
        Self::acquire_as(manager, conn_id, ConnectionLeaseOwner::Sftp)
    }

    /// [`acquire`](Self::acquire) for another consumer kind, e.g. a remote editor window.
    pub fn acquire_as(
        manager: &SshConnectionManager,
        conn_id: &str,
        owner: ConnectionLeaseOwner,
    ) -> Result<Self, Problem> {
        manager
            .acquire(conn_id, owner)
            .map(|lease| Self { lease })
            .ok_or_else(|| Problem::not_found("connection not found"))
    }

    /// The connection this window holds.
    pub fn connection_id(&self) -> &str {
        &self.lease.connection().id
    }

    pub fn owner(&self) -> ConnectionLeaseOwner {
        self.lease.owner()
    }

    /// Completes when the connection closes underneath the window (TypeScript closed the socket
    /// with 1011 "connection closed"), with the reason when known. The lease is released then.
    pub async fn closed(&self) -> Option<String> {
        let reason = self.lease.connection().closed().await;
        self.lease.release();
        reason
    }

    /// Lets go of the transport now; idempotent, and what dropping the guard does.
    pub fn release(&self) {
        self.lease.release();
    }
}
