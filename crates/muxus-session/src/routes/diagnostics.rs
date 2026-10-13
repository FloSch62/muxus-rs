//! Connection diagnostics for a session that failed to connect, run from this computer
//! (`server/src/routes/diagnostics.ts`), and status bar host statistics
//! (`routes/host-stats.ts`).

use std::net::IpAddr;
use std::path::PathBuf;

use async_trait::async_trait;
use muxus_core::api::{ConnectionCheckStatus, ConnectionDiagnosticsResponse, HostStatsResponse};
use muxus_core::protocol::TerminalProfile;
use muxus_core::validate::Validate;
use muxus_ssh::connection::ConnectionLeaseOwner;

use crate::adapters::{ConnectionCommand, diagnostic_hops};
use crate::backend::Backend;
use crate::diagnostics::{
    AgentProbe, DiagnosticProbes, FileAccess, LookupError, PingResult, SystemProbes, TcpProbe,
    diagnose_ssh, diagnose_telnet,
};
use crate::error::{ApiError, ApiResult};
use crate::stats::{
    HostStatsReading, host_stats_response, read_local_host_stats, read_remote_host_stats,
};

/// The real network, `ping`, agent and disk, with the backend's own environment and home (so a
/// sandboxed backend never asks the user's real agent).
pub(crate) struct BackendProbes {
    pub env: crate::backend::EnvLookup,
    pub home: PathBuf,
}

#[async_trait]
impl DiagnosticProbes for BackendProbes {
    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, LookupError> {
        SystemProbes.lookup(host).await
    }

    async fn ping(&self, address: IpAddr) -> PingResult {
        SystemProbes.ping(address).await
    }

    async fn connect(&self, address: IpAddr, port: u16, read_greeting: bool) -> TcpProbe {
        SystemProbes.connect(address, port, read_greeting).await
    }

    async fn agent(&self, socket: &str) -> AgentProbe {
        SystemProbes.agent(socket).await
    }

    async fn readable(&self, file: &str) -> FileAccess {
        SystemProbes.readable(file).await
    }

    fn env(&self, name: &str) -> Option<String> {
        (self.env)(name)
    }

    fn home(&self) -> Option<String> {
        Some(self.home.to_string_lossy().into_owned())
    }
}

impl Backend {
    /// `POST /api/diagnostics/connection` for an SSH or Telnet profile.
    pub async fn diagnose_connection(
        &self,
        profile: TerminalProfile,
    ) -> ApiResult<ConnectionDiagnosticsResponse> {
        let mut profile = profile;
        if profile.validate().is_err()
            || matches!(
                profile,
                TerminalProfile::Local(_) | TerminalProfile::Serial(_)
            )
        {
            return Err(ApiError::bad_request("invalid diagnostics request"));
        }
        self.run(move |shared| async move {
            let probes = shared.diagnostic_probes.clone();
            let response = match &profile {
                TerminalProfile::Telnet(telnet) => {
                    diagnose_telnet(&telnet.host, telnet.port, probes.as_ref()).await
                }
                TerminalProfile::Ssh(ssh) => {
                    // A broken plan (missing saved jump host, ProxyJump cycle) is itself the
                    // diagnosis; nothing was dialed.
                    let chain = shared
                        .connections
                        .dial_plan(ssh)
                        .map_err(|error| ApiError::new(422, error.to_string()))?;
                    diagnose_ssh(&diagnostic_hops(&chain), probes.as_ref()).await
                }
                _ => unreachable!("refused above"),
            };
            let failed: Vec<&str> = response
                .checks
                .iter()
                .filter(|check| check.status == ConnectionCheckStatus::Fail)
                .map(|check| check.label.as_str())
                .collect();
            tracing::info!(checked = %response.checked, ?failed, "connection diagnostics");
            Ok(response)
        })
        .await
    }

    /// `GET /api/host-stats/local`: the computer local terminals run on.
    pub async fn local_host_stats(&self) -> ApiResult<HostStatsResponse> {
        self.run(|shared| async move {
            let reading = shared
                .host_stats
                .read("local", || async {
                    Ok(HostStatsReading::Supported(Box::new(
                        read_local_host_stats().await,
                    )))
                })
                .await
                .map_err(ApiError::internal)?;
            Ok(host_stats_response(
                &reading,
                chrono::Utc::now().timestamp_millis(),
            ))
        })
        .await
    }

    /// `GET /api/host-stats/ssh/:connId`: the far end of a live SSH connection. Several windows
    /// polling the same connection share one exec channel.
    pub async fn ssh_host_stats(&self, conn_id: &str) -> ApiResult<HostStatsResponse> {
        let conn_id = conn_id.to_string();
        self.run(move |shared| async move {
            let key = format!("ssh:{conn_id}");
            let connections = shared.connections.clone();
            let reading = shared
                .host_stats
                .read(&key, || async move {
                    let lease = connections
                        .acquire(&conn_id, ConnectionLeaseOwner::Stats)
                        .ok_or_else(|| "\u{0}connection not found".to_string())?;
                    let command = ConnectionCommand(lease.connection().clone());
                    let reading = read_remote_host_stats(&command).await;
                    drop(lease);
                    reading
                })
                .await
                .map_err(|message| match message.strip_prefix('\u{0}') {
                    Some(missing) => ApiError::not_found(missing),
                    None => ApiError::internal(message),
                })?;
            Ok(host_stats_response(
                &reading,
                chrono::Utc::now().timestamp_millis(),
            ))
        })
        .await
    }
}
