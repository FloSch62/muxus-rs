//! What the status bar shows from host statistics (`client/src/host-stats.ts`):
//! which items exist, how two readings become rates, and how values are written.

use std::cmp::Ordering;
use std::collections::HashMap;

use muxus_core::api::{HostStatsSample, MemoryStats};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Status bar items, in the order the bar shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusBarItem {
    Hostname,
    Os,
    Cpu,
    Memory,
    Disk,
    Network,
    Uptime,
    Users,
}

pub const STATUS_BAR_ITEMS: [StatusBarItem; 8] = [
    StatusBarItem::Hostname,
    StatusBarItem::Os,
    StatusBarItem::Cpu,
    StatusBarItem::Memory,
    StatusBarItem::Disk,
    StatusBarItem::Network,
    StatusBarItem::Uptime,
    StatusBarItem::Users,
];

impl StatusBarItem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hostname => "Host name",
            Self::Os => "Operating system",
            Self::Cpu => "CPU usage",
            Self::Memory => "Memory usage",
            Self::Disk => "Disk usage",
            Self::Network => "Network traffic",
            Self::Uptime => "Uptime",
            Self::Users => "Logged-in users",
        }
    }
}

/// A stored item list, when it is one: known items, none repeated.
pub fn status_bar_item_list(value: &Value) -> Option<Vec<StatusBarItem>> {
    let items: Vec<StatusBarItem> = serde_json::from_value(value.clone()).ok()?;
    let mut seen = items.clone();
    seen.sort();
    seen.dedup();
    (seen.len() == items.len()).then_some(items)
}

/// Turn an item on or off, keeping the bar's order.
pub fn with_status_bar_item(
    items: &[StatusBarItem],
    item: StatusBarItem,
    shown: bool,
) -> Vec<StatusBarItem> {
    STATUS_BAR_ITEMS
        .iter()
        .copied()
        .filter(|entry| {
            if *entry == item {
                shown
            } else {
                items.contains(entry)
            }
        })
        .collect()
}

/// A reading with the time it describes, in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedSample {
    pub sample: HostStatsSample,
    /// The host's uptime when it reports one, else the backend clock.
    pub at: f64,
}

pub fn timed_sample(sample: HostStatsSample, sampled_at_ms: i64) -> TimedSample {
    let at = sample
        .uptime_seconds
        .unwrap_or(sampled_at_ms as f64 / 1000.0);
    TimedSample { sample, at }
}

/// What the bar shows: the latest reading plus the rates since the one before.
#[derive(Debug, Clone, PartialEq)]
pub struct HostStatsView {
    pub sample: HostStatsSample,
    /// Busy share of all cores since the previous reading, 0 to 1.
    pub cpu: Option<f64>,
    /// Bytes per second since the previous reading.
    pub receive_rate: Option<f64>,
    pub send_rate: Option<f64>,
}

/// Rates need two readings of the same host. A counter that went backwards
/// (a reboot, an interface that was reset or replaced) gives no rate this
/// round instead of a negative or absurd one.
pub fn host_stats_view(current: &TimedSample, previous: Option<&TimedSample>) -> HostStatsView {
    let mut view = HostStatsView {
        sample: current.sample.clone(),
        cpu: None,
        receive_rate: None,
        send_rate: None,
    };
    let Some(previous) = previous else {
        return view;
    };
    let (now, before) = (&current.sample, &previous.sample);
    if let (Some(now_cpu), Some(before_cpu)) = (now.cpu_time, before.cpu_time) {
        let total = now_cpu.total - before_cpu.total;
        let idle = now_cpu.idle - before_cpu.idle;
        if total > 0.0 && idle >= 0.0 && idle <= total {
            view.cpu = Some(1.0 - idle / total);
        }
    }
    let seconds = current.at - previous.at;
    if seconds > 0.0
        && let (Some(now_net), Some(before_net)) = (&now.network, &before.network)
        && now_net.interface == before_net.interface
    {
        let received = now_net.received_bytes as f64 - before_net.received_bytes as f64;
        let sent = now_net.sent_bytes as f64 - before_net.sent_bytes as f64;
        if received >= 0.0 {
            view.receive_rate = Some(received / seconds);
        }
        if sent >= 0.0 {
            view.send_rate = Some(sent / seconds);
        }
    }
    view
}

const BYTE_UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// `Number.prototype.toFixed`: the nearest `digits`-decimal value, ties up.
pub fn js_to_fixed(value: f64, digits: usize) -> String {
    // Wide enough to show the exact binary value of any status bar number.
    let wide = format!("{:.*}", digits + 60, value.abs());
    let (whole, fraction) = wide.split_once('.').unwrap_or((&wide, ""));
    let mut kept: Vec<u8> = whole.bytes().chain(fraction.bytes().take(digits)).collect();
    if fraction
        .as_bytes()
        .get(digits)
        .is_some_and(|digit| *digit >= b'5')
    {
        let mut index = kept.len();
        loop {
            if index == 0 {
                kept.insert(0, b'1');
                break;
            }
            index -= 1;
            if kept[index] == b'9' {
                kept[index] = b'0';
            } else {
                kept[index] += 1;
                break;
            }
        }
    }
    let split = kept.len() - digits;
    let text = String::from_utf8(kept).unwrap_or_default();
    let mut out = String::new();
    if value < 0.0 && text.bytes().any(|byte| byte != b'0') {
        out.push('-');
    }
    out.push_str(&text[..split]);
    if digits > 0 {
        out.push('.');
        out.push_str(&text[split..]);
    }
    out
}

/// Binary multiples with the familiar short names: 1.5 GB, 820 MB, 12 KB.
pub fn format_bytes(bytes: f64) -> String {
    let mut value = bytes.max(0.0);
    let mut unit = 0;
    while value >= 1024.0 && unit < BYTE_UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let digits = if unit == 0 || value >= 100.0 { 0 } else { 1 };
    let text = js_to_fixed(value, digits);
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text} {}", BYTE_UNITS[unit])
}

pub fn format_rate(bytes_per_second: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_second))
}

pub fn format_percent(fraction: f64) -> String {
    format!("{}%", (fraction.clamp(0.0, 1.0) * 100.0 + 0.5).floor())
}

/// Two largest units, the way `uptime` reads: 12d 4h, 3h 20m, 5m.
pub fn format_uptime(seconds: f64) -> String {
    let minutes = (seconds / 60.0).floor() as i64;
    let days = minutes / 1440;
    let hours = (minutes % 1440) / 60;
    let rest = minutes % 60;
    if days > 0 {
        return if hours > 0 {
            format!("{days}d {hours}h")
        } else {
            format!("{days}d")
        };
    }
    if hours > 0 {
        return if rest > 0 {
            format!("{hours}h {rest}m")
        } else {
            format!("{hours}h")
        };
    }
    format!("{rest}m")
}

pub fn memory_used(memory: &MemoryStats) -> u64 {
    memory.total_bytes.saturating_sub(memory.available_bytes)
}

/// `localeCompare` for user names: case-insensitive first, lowercase before uppercase.
fn locale_cmp(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| b.cmp(a))
}

/// Login sessions per user, busiest first: "alice (2), bob".
pub fn describe_users(users: &[String]) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for user in users {
        match index.get(user.as_str()) {
            Some(&at) => counts[at].1 += 1,
            None => {
                index.insert(user, counts.len());
                counts.push((user.clone(), 1));
            }
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| locale_cmp(&a.0, &b.0)));
    counts
        .into_iter()
        .map(|(user, count)| {
            if count > 1 {
                format!("{user} ({count})")
            } else {
                user
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Usage at which a meter turns to a warning, and to an error.
pub const METER_WARNING: f64 = 0.8;
pub const METER_CRITICAL: f64 = 0.95;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterLevel {
    Normal,
    Warning,
    Critical,
}

pub fn meter_level(fraction: f64) -> MeterLevel {
    if fraction >= METER_CRITICAL {
        MeterLevel::Critical
    } else if fraction >= METER_WARNING {
        MeterLevel::Warning
    } else {
        MeterLevel::Normal
    }
}
