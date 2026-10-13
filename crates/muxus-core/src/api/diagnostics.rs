//! Host statistics and connection diagnostics.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CpuTime {
    pub total: f64,
    pub idle: f64,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStats {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: Option<u64>,
    pub swap_free_bytes: Option<u64>,
}

/// Byte counters of the interface that carries the default route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStats {
    pub interface: String,
    pub received_bytes: u64,
    pub sent_bytes: u64,
}

/// The filesystem holding `/` (the system drive on Windows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskStats {
    pub mount: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
}

/// One reading of a host's counters for the status bar. Every field is
/// optional: a host reports what its platform exposes. Rates (CPU busy
/// share, network throughput) come from the difference between two readings.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatsSample {
    pub hostname: Option<String>,
    /// Distribution or product name with its version, such as "Ubuntu 24.04.1 LTS".
    pub os: Option<String>,
    /// Kernel name and release, such as "Linux 6.8.0-45-generic".
    pub kernel: Option<String>,
    pub uptime_seconds: Option<f64>,
    /// Cumulative CPU time over all cores in any one unit; busy = 1 - Δidle / Δtotal.
    pub cpu_time: Option<CpuTime>,
    pub cores: Option<u32>,
    pub load_average: Option<[f64; 3]>,
    pub memory: Option<MemoryStats>,
    pub network: Option<NetworkStats>,
    /// One user name per login session, as `who` lists them.
    pub users: Option<Vec<String>>,
    pub disk: Option<DiskStats>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum HostStatsResponse {
    #[serde(rename_all = "camelCase")]
    Ok {
        /// Backend clock at the reading, for rates when the host reports no uptime.
        sampled_at: i64,
        sample: HostStatsSample,
    },
    /// The host ran no POSIX shell for the probe: a network appliance,
    /// Windows, a forced command.
    Unsupported { message: String },
}

/// `info` states a fact without judging it; `skipped` means the check could not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionCheckStatus {
    Ok,
    Warn,
    Fail,
    Info,
    Skipped,
}

/// One step of a connection diagnosis, printed as one row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionCheck {
    /// Short row label: DNS, Ping, TCP, SSH, Agent, Key files, Route.
    pub label: String,
    pub status: ConnectionCheckStatus,
    pub detail: String,
}

/// Network checks run from this computer against the first host a session
/// dials: name resolution, ICMP echo, a TCP connect per address and, for
/// SSH, the server's greeting. Hosts behind a jump host or ProxyCommand are
/// only reachable through it, so the checks stop there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionDiagnosticsResponse {
    /// The endpoint the checks ran against, as host:port.
    pub checked: String,
    pub checks: Vec<ConnectionCheck>,
    /// Best guess at the cause, read from the lowest layer that failed.
    pub conclusion: String,
}
