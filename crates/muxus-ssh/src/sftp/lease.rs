//! A file browser window's hold on its SSH transport (port of
//! `server/src/ws/sftp-lease-socket.ts`).
//!
//! A detached SFTP window owns a transport lease for as long as it is open. In TypeScript the
//! window's WebSocket lifetime gave reliable cleanup even when the window crashed; here the guard
//! does: dropping it releases the lease, and so does the connection closing underneath it.

use crate::Problem;
use crate::connection::leases::ConnectionLeaseOwner;
use crate::connection::manager::{ConnectionLease, SshConnectionManager};
use crate::system_openssh::SystemOpenSshLease;

/// A hold on what file operations run over: a built-in connection, or the login a system
/// OpenSSH terminal shares. Dropping it lets go.
#[derive(Debug)]
pub(crate) enum SftpLease {
    Ssh(ConnectionLease),
    System(SystemOpenSshLease),
}

impl SftpLease {
    /// The system OpenSSH session's lease, else one on the built-in connection.
    pub(crate) fn acquire(
        manager: &SshConnectionManager,
        conn_id: &str,
        owner: ConnectionLeaseOwner,
    ) -> Option<Self> {
        if let Some(lease) = manager.system_sessions().acquire(conn_id) {
            return Some(Self::System(lease));
        }
        manager.acquire(conn_id, owner).map(Self::Ssh)
    }

    pub(crate) fn source(&self) -> std::sync::Arc<dyn super::SftpSource> {
        match self {
            Self::Ssh(lease) => lease.connection().clone(),
            Self::System(lease) => lease.connection().clone(),
        }
    }

    pub(crate) fn connection_id(&self) -> &str {
        match self {
            Self::Ssh(lease) => &lease.connection().id,
            Self::System(lease) => &lease.connection().id,
        }
    }

    pub(crate) fn release(&self) {
        match self {
            Self::Ssh(lease) => lease.release(),
            Self::System(lease) => lease.release(),
        }
    }

    /// Completes when the connection underneath closes (`SftpConnection.onClose`). It holds
    /// the connection, not this lease, so waiting keeps nothing open.
    pub(crate) fn connection_closed(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        match self {
            Self::Ssh(lease) => {
                let connection = lease.connection().clone();
                Box::pin(async move {
                    connection.closed().await;
                })
            }
            Self::System(lease) => {
                let connection = lease.connection().clone();
                Box::pin(async move { connection.closed().await })
            }
        }
    }
}

/// Keeps a connection alive while a file browser (or editor) window shows it.
#[derive(Debug)]
pub struct SftpWindowLease {
    lease: SftpLease,
    owner: ConnectionLeaseOwner,
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
        SftpLease::acquire(manager, conn_id, owner)
            .map(|lease| Self { lease, owner })
            .ok_or_else(|| Problem::not_found("connection not found"))
    }

    /// The connection this window holds.
    pub fn connection_id(&self) -> &str {
        self.lease.connection_id()
    }

    pub fn owner(&self) -> ConnectionLeaseOwner {
        match &self.lease {
            SftpLease::Ssh(lease) => lease.owner(),
            SftpLease::System(_) => self.owner,
        }
    }

    /// Completes when the connection closes underneath the window (TypeScript closed the socket
    /// with 1011 "connection closed"), with the reason when known. The lease is released then.
    pub async fn closed(&self) -> Option<String> {
        let reason = match &self.lease {
            SftpLease::Ssh(lease) => lease.connection().closed().await,
            SftpLease::System(lease) => {
                lease.connection().closed().await;
                None
            }
        };
        self.lease.release();
        reason
    }

    /// Lets go of the transport now; idempotent, and what dropping the guard does.
    pub fn release(&self) {
        self.lease.release();
    }
}
