//! Status bar statistics (`server/src/stats/host-stats.ts`, `server/src/routes/host-stats.ts`).
//!
//! Read by running one fixed POSIX sh script: over an exec channel on the
//! live SSH transport, or locally for local terminals. Nothing is installed
//! or left behind on the host. The script only prints raw sources under
//! `@@muxus:<name>` markers; all interpretation happens in
//! [`parse_host_stats`], where it can be tested. Linux reports everything
//! from /proc; macOS and the BSDs fall back to sysctl, vm_stat, route and
//! netstat for what they have.

pub mod view;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use muxus_core::api::{
    CpuTime, DiskStats, HostStatsResponse, HostStatsSample, MemoryStats, NetworkStats,
};
use regex::Regex;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::OnceCell;

/// The probe, fed to `sh -s` on stdin.
pub const HOST_STATS_SCRIPT: &str = r#"LC_ALL=C
PATH="$PATH:/usr/bin:/bin:/usr/sbin:/sbin"
export LC_ALL PATH
muxus_section() { printf '\n@@muxus:%s\n' "$1"; }
muxus_section begin
muxus_section hostname; uname -n 2>/dev/null
muxus_section kernel; uname -sr 2>/dev/null
muxus_section os-release; grep -E '^(PRETTY_NAME|NAME|VERSION_ID)=' /etc/os-release 2>/dev/null
muxus_section who; who 2>/dev/null
muxus_section df; df -P -k / 2>/dev/null
if [ -r /proc/stat ]; then
  muxus_section uptime; cat /proc/uptime 2>/dev/null
  muxus_section stat; head -n 1 /proc/stat 2>/dev/null; grep -c '^cpu[0-9]' /proc/stat 2>/dev/null
  muxus_section loadavg; cat /proc/loadavg 2>/dev/null
  muxus_section meminfo; grep -E '^(MemTotal|MemAvailable|MemFree|Buffers|Cached|SwapTotal|SwapFree):' /proc/meminfo 2>/dev/null
  muxus_section route; cat /proc/net/route 2>/dev/null
  muxus_section netdev; cat /proc/net/dev 2>/dev/null
else
  muxus_section sw-vers; sw_vers 2>/dev/null
  muxus_section now; date +%s 2>/dev/null
  muxus_section boottime; sysctl -n kern.boottime 2>/dev/null
  muxus_section bsd-loadavg; sysctl -n vm.loadavg 2>/dev/null
  muxus_section ncpu; sysctl -n hw.ncpu 2>/dev/null
  muxus_section memsize; sysctl -n hw.memsize 2>/dev/null
  muxus_section vm-stat; vm_stat 2>/dev/null
  muxus_section bsd-route; route -n get default 2>/dev/null
  muxus_section netstat; netstat -ibn 2>/dev/null
fi
muxus_section end
"#;

pub const SCRIPT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT: usize = 256 * 1024;

/// One reading of a host.
#[derive(Debug, Clone, PartialEq)]
pub enum HostStatsReading {
    Supported(Box<HostStatsSample>),
    /// The probe printed none of its markers: no POSIX shell answered it.
    Unsupported,
}

/// Split script output into its marked sections; text before the first marker is dropped.
fn sections(output: &str) -> HashMap<String, Vec<String>> {
    let mut result: HashMap<String, Vec<String>> = HashMap::new();
    let mut current: Option<String> = None;
    for line in output
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
    {
        let trimmed = js_trim(line);
        if let Some(name) = trimmed.strip_prefix("@@muxus:").filter(|name| {
            !name.is_empty() && name.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-')
        }) {
            result.insert(name.to_string(), Vec::new());
            current = Some(name.to_string());
        } else if let Some(name) = &current
            && !trimmed.is_empty()
        {
            result
                .entry(name.clone())
                .or_default()
                .push(line.to_string());
        }
    }
    result
}

fn js_trim(text: &str) -> &str {
    muxus_core::js::js_trim(text)
}

/// `Number(value)` when it is finite.
fn finite(value: Option<&str>) -> Option<f64> {
    let text = js_trim(value?);
    if text.is_empty() {
        return Some(0.0);
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("inf") || lower.contains("nan") {
        return None;
    }
    text.parse::<f64>().ok().filter(|number| number.is_finite())
}

fn whitespace_fields(text: &str) -> Vec<&str> {
    js_trim(text)
        .split(|ch: char| muxus_core::js::is_js_whitespace(ch))
        .filter(|field| !field.is_empty())
        .collect()
}

fn os_release_name(lines: &[String]) -> Option<String> {
    let mut fields: HashMap<&str, String> = HashMap::new();
    for line in lines {
        let line = js_trim(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.is_empty() || !key.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_') {
            continue;
        }
        let unquoted = ['"', '\'']
            .iter()
            .find_map(|quote| {
                value
                    .strip_prefix(*quote)
                    .and_then(|inner| inner.strip_suffix(*quote))
                    .filter(|_| value.len() >= 2)
            })
            .unwrap_or(value);
        fields.insert(key, js_trim(unquoted).to_string());
    }
    if let Some(pretty) = fields
        .get("PRETTY_NAME")
        .filter(|pretty| !pretty.is_empty())
    {
        return Some(pretty.clone());
    }
    let name = fields.get("NAME").filter(|name| !name.is_empty())?;
    match fields
        .get("VERSION_ID")
        .filter(|version| !version.is_empty())
    {
        Some(version) => Some(format!("{name} {version}")),
        None => Some(name.clone()),
    }
}

/// The value of `key:` among the lines.
fn colon_field(lines: &[String], key: &str) -> Option<String> {
    lines.iter().find_map(|line| {
        let rest = js_trim(line).strip_prefix(key)?.strip_prefix(':')?;
        let value = muxus_core::js::js_trim_start(rest);
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn sw_vers_name(lines: &[String]) -> Option<String> {
    let name = colon_field(lines, "ProductName")?;
    match colon_field(lines, "ProductVersion") {
        Some(version) => Some(format!("{name} {version}")),
        None => Some(name),
    }
}

/// First line of /proc/stat: user nice system idle iowait irq softirq steal [guest guest_nice].
fn proc_cpu_time(line: Option<&String>) -> Option<CpuTime> {
    let fields = whitespace_fields(line?);
    if fields.first() != Some(&"cpu") {
        return None;
    }
    // guest time is already part of user and nice, so it is not added again.
    let times: Vec<f64> = fields
        .iter()
        .skip(1)
        .take(8)
        .map(|field| finite(Some(field)))
        .collect::<Option<_>>()?;
    if times.len() < 4 {
        return None;
    }
    Some(CpuTime {
        total: times.iter().sum(),
        idle: times[3] + times.get(4).copied().unwrap_or(0.0),
    })
}

fn load_average(line: Option<&str>) -> Option<[f64; 3]> {
    let cleaned = line?.replace(['{', '}'], " ");
    let values: Vec<f64> = whitespace_fields(&cleaned)
        .into_iter()
        .take(3)
        .map(|field| finite(Some(field)))
        .collect::<Option<_>>()?;
    (values.len() == 3).then(|| [values[0], values[1], values[2]])
}

fn proc_memory(lines: &[String]) -> Option<MemoryStats> {
    let mut kib: HashMap<String, u64> = HashMap::new();
    for line in lines {
        let line = js_trim(line);
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        if key.is_empty()
            || !key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            continue;
        }
        let value = rest.trim_start();
        if value.len() == rest.len() {
            continue; // `\s+` needs at least one blank after the colon.
        }
        let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(value) = digits.parse::<u64>() {
            kib.insert(key.to_string(), value);
        }
    }
    let total = *kib.get("MemTotal").filter(|total| **total > 0)?;
    // MemAvailable arrived in Linux 3.14; older kernels get the classic estimate.
    let available = kib.get("MemAvailable").copied().unwrap_or_else(|| {
        kib.get("MemFree").copied().unwrap_or(0)
            + kib.get("Buffers").copied().unwrap_or(0)
            + kib.get("Cached").copied().unwrap_or(0)
    });
    Some(MemoryStats {
        total_bytes: total * 1024,
        available_bytes: available.min(total) * 1024,
        swap_total_bytes: kib.get("SwapTotal").map(|value| value * 1024),
        swap_free_bytes: kib.get("SwapFree").map(|value| value * 1024),
    })
}

/// macOS: hw.memsize plus vm_stat pages; free, inactive and speculative pages count as available.
fn vm_stat_memory(memsize: Option<&str>, lines: &[String]) -> Option<MemoryStats> {
    let total = finite(memsize).filter(|total| *total != 0.0)?;
    let page_size = lines.first().and_then(|line| {
        let start = line.find("page size of ")? + "page size of ".len();
        let digits: String = line[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        line[start + digits.len()..]
            .starts_with(" bytes")
            .then(|| finite(Some(&digits)))
            .flatten()
    });
    let page_size = page_size.filter(|size| *size != 0.0)?;
    let pages = |name: &str| -> f64 {
        let prefix = format!("Pages {name}:");
        lines
            .iter()
            .find_map(|line| {
                let rest = js_trim(line).strip_prefix(&prefix)?;
                let trimmed = rest.trim_start();
                if trimmed.len() == rest.len() {
                    return None;
                }
                let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
                (!digits.is_empty()).then_some(digits)
            })
            .and_then(|digits| finite(Some(&digits)))
            .unwrap_or(0.0)
    };
    let available = (pages("free") + pages("inactive") + pages("speculative")) * page_size;
    Some(MemoryStats {
        total_bytes: total as u64,
        available_bytes: available.min(total) as u64,
        swap_total_bytes: None,
        swap_free_bytes: None,
    })
}

/// The interface of the default route in /proc/net/route, if there is one.
fn proc_default_interface(lines: &[String]) -> Option<String> {
    lines.iter().skip(1).find_map(|line| {
        let fields = whitespace_fields(line);
        let iface = fields.first()?;
        let mask = fields.get(7);
        (fields.get(1) == Some(&"00000000") && mask.is_none_or(|mask| *mask == "00000000"))
            .then(|| iface.to_string())
    })
}

/// Byte counters from /proc/net/dev for the default-route interface. Without
/// a default route, the busiest interface other than loopback stands in.
fn proc_network(lines: &[String], default_interface: Option<&str>) -> Option<NetworkStats> {
    let mut counters = Vec::new();
    for line in lines {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = js_trim(name);
        let fields = whitespace_fields(rest);
        let (Some(received), Some(sent)) = (
            finite(fields.first().copied()),
            finite(fields.get(8).copied()),
        ) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        counters.push(NetworkStats {
            interface: name.to_string(),
            received_bytes: received as u64,
            sent_bytes: sent as u64,
        });
    }
    pick_interface(counters, default_interface)
}

/// `netstat -ibn` (macOS, BSD): the `<Link#n>` row carries an interface's
/// totals. Rows can lack the Address column, so the byte columns are found by
/// their distance from the end of the header.
fn netstat_network(lines: &[String], default_interface: Option<&str>) -> Option<NetworkStats> {
    let header = whitespace_fields(lines.first()?);
    let from_end = |column: &str| {
        header
            .iter()
            .position(|field| *field == column)
            .map(|index| header.len() - index)
    };
    let (received, sent) = (from_end("Ibytes")?, from_end("Obytes")?);
    let mut counters = Vec::new();
    for line in lines.iter().skip(1) {
        let fields = whitespace_fields(line);
        if !fields
            .get(2)
            .is_some_and(|field| field.starts_with("<Link"))
        {
            continue;
        }
        let at = |back: usize| {
            fields
                .len()
                .checked_sub(back)
                .and_then(|index| fields.get(index).copied())
        };
        let (Some(received_bytes), Some(sent_bytes)) = (finite(at(received)), finite(at(sent)))
        else {
            continue;
        };
        counters.push(NetworkStats {
            interface: fields[0].strip_suffix('*').unwrap_or(fields[0]).to_string(),
            received_bytes: received_bytes as u64,
            sent_bytes: sent_bytes as u64,
        });
    }
    pick_interface(counters, default_interface)
}

fn is_loopback_name(name: &str) -> bool {
    name.strip_prefix("lo")
        .is_some_and(|rest| rest.chars().all(|ch| ch.is_ascii_digit()))
}

fn pick_interface(
    counters: Vec<NetworkStats>,
    default_interface: Option<&str>,
) -> Option<NetworkStats> {
    if let Some(preferred) = counters
        .iter()
        .find(|entry| Some(entry.interface.as_str()) == default_interface)
    {
        return Some(preferred.clone());
    }
    counters
        .into_iter()
        .filter(|entry| !is_loopback_name(&entry.interface))
        .fold(None, |busiest, entry| match busiest {
            Some(busiest) if entry.received_bytes <= busiest.received_bytes => Some(busiest),
            _ => Some(entry),
        })
}

fn df_disk(lines: &[String]) -> Option<DiskStats> {
    static DF_LINE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^.*?\s+(\d+)\s+(\d+)\s+(\d+)\s+\d+%\s+(/.*)$").expect("valid regex")
    });
    lines.iter().skip(1).find_map(|line| {
        let captures = DF_LINE.captures(js_trim(line))?;
        let number = |index: usize| captures[index].parse::<u64>().ok();
        Some(DiskStats {
            mount: captures[4].to_string(),
            total_bytes: number(1)? * 1024,
            used_bytes: number(2)? * 1024,
            available_bytes: number(3)? * 1024,
        })
    })
}

/// Turn the script's output into a sample; `None` when no marker came back.
pub fn parse_host_stats(output: &str) -> Option<HostStatsSample> {
    let parts = sections(output);
    if !parts.contains_key("begin") {
        return None;
    }
    let first = |name: &str| {
        parts
            .get(name)
            .and_then(|lines| lines.first())
            .map(|line| js_trim(line).to_string())
    };
    let empty = Vec::new();
    let lines = |name: &str| parts.get(name).unwrap_or(&empty);

    let hostname = first("hostname").filter(|value| !value.is_empty());
    let kernel = first("kernel").filter(|value| !value.is_empty());
    let os = os_release_name(lines("os-release"))
        .or_else(|| sw_vers_name(lines("sw-vers")))
        .or_else(|| kernel.clone());

    let proc_uptime =
        first("uptime").and_then(|line| finite(whitespace_fields(&line).first().copied()));
    static BOOT_SECONDS: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"sec\s*=\s*(\d+)").expect("valid regex"));
    let boot_seconds = first("boottime")
        .and_then(|line| {
            BOOT_SECONDS
                .captures(&line)
                .map(|captures| captures[1].to_string())
        })
        .and_then(|digits| finite(Some(&digits)));
    let now = first("now").and_then(|line| finite(Some(&line)));
    let uptime = proc_uptime.or(match (boot_seconds, now) {
        (Some(boot), Some(now)) => Some(now - boot),
        _ => None,
    });

    let stat = lines("stat");
    let cores = stat
        .get(1)
        .and_then(|line| finite(Some(line)))
        .or_else(|| first("ncpu").and_then(|line| finite(Some(&line))));
    let network = if parts.contains_key("netdev") {
        proc_network(
            lines("netdev"),
            proc_default_interface(lines("route")).as_deref(),
        )
    } else {
        let route = lines("bsd-route").join("\n");
        let interface = route.find("interface:").and_then(|index| {
            whitespace_fields(&route[index + "interface:".len()..])
                .first()
                .map(|name| name.to_string())
        });
        netstat_network(lines("netstat"), interface.as_deref())
    };
    let users = parts.contains_key("who").then(|| {
        lines("who")
            .iter()
            .filter_map(|line| whitespace_fields(line).first().map(|name| name.to_string()))
            .collect()
    });
    let sample = HostStatsSample {
        hostname,
        os,
        kernel,
        uptime_seconds: uptime.filter(|uptime| *uptime >= 0.0),
        cpu_time: proc_cpu_time(stat.first()),
        cores: cores.filter(|cores| *cores > 0.0).map(|cores| cores as u32),
        load_average: load_average(first("loadavg").or_else(|| first("bsd-loadavg")).as_deref()),
        memory: proc_memory(lines("meminfo"))
            .or_else(|| vm_stat_memory(first("memsize").as_deref(), lines("vm-stat"))),
        network,
        users,
        disk: df_disk(lines("df")),
    };
    Some(sample)
}

/// Collect a script run's stdout, giving up on a host that never finishes.
pub async fn collect_output(mut reader: impl AsyncRead + Unpin, timeout: Duration) -> String {
    let mut output = Vec::new();
    let mut buffer = vec![0u8; 16 * 1024];
    let reading = async {
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if output.len() < MAX_OUTPUT {
                        output.extend_from_slice(&buffer[..read]);
                    }
                }
            }
        }
    };
    let _ = tokio::time::timeout(timeout, reading).await;
    String::from_utf8_lossy(&output).into_owned()
}

/// Runs a command on a remote host over its live transport.
#[async_trait]
pub trait RemoteCommand: Send + Sync {
    /// Start `command` with `stdin` as its input (then end of input) and hand
    /// back its standard output. Dropping the reader closes the channel.
    /// Fails when the server refuses the channel.
    async fn exec(
        &self,
        command: &str,
        stdin: Vec<u8>,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, String>;
}

/// Read a remote host. `sh -s` keeps the login shell out of the script's
/// syntax, so fish or tcsh users are covered too. Channel failures (a refused
/// exec, MaxSessions reached) are errors; a host that answers without the
/// markers is reported as unsupported.
pub async fn read_remote_host_stats(
    remote: &dyn RemoteCommand,
) -> Result<HostStatsReading, String> {
    let stdout = remote
        .exec("sh -s", HOST_STATS_SCRIPT.as_bytes().to_vec())
        .await?;
    let output = collect_output(stdout, SCRIPT_TIMEOUT).await;
    Ok(match parse_host_stats(&output) {
        Some(sample) => HostStatsReading::Supported(Box::new(sample)),
        None => HostStatsReading::Unsupported,
    })
}

/// Read the machine local terminals run on.
pub async fn read_local_host_stats() -> HostStatsSample {
    #[cfg(unix)]
    {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;
        let child = tokio::process::Command::new("sh")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();
        let mut sample = match child {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(HOST_STATS_SCRIPT.as_bytes()).await;
                }
                let output = match child.stdout.take() {
                    Some(stdout) => collect_output(stdout, SCRIPT_TIMEOUT).await,
                    None => String::new(),
                };
                let _ = child.start_kill();
                parse_host_stats(&output).unwrap_or_default()
            }
            Err(_) => HostStatsSample::default(),
        };
        if sample.hostname.is_none() {
            sample.hostname = local_hostname();
        }
        sample
    }
    #[cfg(not(unix))]
    {
        HostStatsSample {
            hostname: std::env::var("COMPUTERNAME").ok(),
            ..HostStatsSample::default()
        }
    }
}

#[cfg(unix)]
fn local_hostname() -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname NUL-terminates within it on success.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf8_lossy(&buffer[..end]).into_owned()).filter(|name| !name.is_empty())
}

/// The status bar answer for one reading.
pub fn host_stats_response(reading: &HostStatsReading, sampled_at_ms: i64) -> HostStatsResponse {
    match reading {
        HostStatsReading::Supported(sample) => HostStatsResponse::Ok {
            sampled_at: sampled_at_ms,
            sample: sample.as_ref().clone(),
        },
        HostStatsReading::Unsupported => HostStatsResponse::Unsupported {
            message: "The host did not run the statistics command.".into(),
        },
    }
}

type SharedReading = Arc<OnceCell<Result<HostStatsReading, String>>>;

/// Several windows polling the same host share one reading (one exec
/// channel) instead of opening one each.
#[derive(Default)]
pub struct SharedReadings {
    in_flight: Mutex<HashMap<String, SharedReading>>,
}

impl SharedReadings {
    pub fn new() -> Self {
        Self::default()
    }

    /// The reading for `key`: joins one in progress, or starts `read`.
    pub async fn read<F, Fut>(&self, key: &str, read: F) -> Result<HostStatsReading, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<HostStatsReading, String>>,
    {
        let (cell, owner) = {
            let mut in_flight = self
                .in_flight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match in_flight.get(key) {
                Some(cell) => (cell.clone(), false),
                None => {
                    let cell: SharedReading = Arc::default();
                    in_flight.insert(key.to_string(), cell.clone());
                    (cell, true)
                }
            }
        };
        let result = cell.get_or_init(read).await.clone();
        if owner {
            self.in_flight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(key);
        }
        result
    }
}
