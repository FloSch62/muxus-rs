//! The latest value of every leaf a subscription delivered, with what makes
//! telemetry readable: per-second rates for counters, a short history for a
//! sparkline, and when each value last changed. Updates are applied to
//! plain data here; the view samples it a few times a second (port of
//! `client/src/management/live-table.ts`).

use std::time::Instant;

use indexmap::IndexMap;
use muxus_core::gnmi_path::{parse_gnmi_path, strip_module_prefix, try_parse_gnmi_path};
use muxus_core::management::GnmiNotification;
use serde_json::Value;

use super::data_tree::{KeyRegistry, guess_list_keys, scalar_text};
use super::requests::now_ms;

pub const HISTORY_POINTS: usize = 60;

#[derive(Debug, Clone, PartialEq)]
pub struct LiveRow {
    pub path: String,
    pub value: Value,
    /// Device timestamp of the last update, in milliseconds.
    pub updated_at: i64,
    /// Local clock when the last update arrived (devices' clocks drift).
    pub received_at: u64,
    pub updates: u64,
    /// Numeric reading of the value, when it has one.
    pub numeric: Option<f64>,
    /// Per second, for values that only ever grew (counters).
    pub rate: Option<f64>,
    /// Rates for counters, values for everything else numeric.
    pub history: Vec<f64>,
    /// Has only increased so far: treated as a counter.
    pub counter: bool,
    /// When the value last changed, for the change highlight.
    pub changed_at: Option<Instant>,
    pub deleted: bool,
    /// Device time of the previous numeric sample, for the rate.
    pub sample_time: Option<i64>,
}

/// A number, or a string that is one (`"123"`, `"-1.5e3"`).
pub fn numeric_value(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64().filter(|value| value.is_finite()),
        Value::String(text) => {
            let text = text.trim();
            let body = text.strip_prefix('-').unwrap_or(text);
            let (mantissa, exponent) = match body.find(['e', 'E']) {
                Some(at) => (&body[..at], Some(&body[at + 1..])),
                None => (body, None),
            };
            let (whole, fraction) = match mantissa.split_once('.') {
                Some((whole, fraction)) => (whole, Some(fraction)),
                None => (mantissa, None),
            };
            let digits = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
            let exponent_ok = exponent.is_none_or(|exponent| {
                digits(exponent.strip_prefix(['+', '-']).unwrap_or(exponent))
            });
            if !digits(whole) || !fraction.is_none_or(digits) || !exponent_ok {
                return None;
            }
            text.parse::<f64>().ok().filter(|value| value.is_finite())
        }
        _ => None,
    }
}

fn nanos_to_millis(timestamp: &str) -> i64 {
    timestamp
        .parse::<i128>()
        .map(|nanos| (nanos / 1_000_000) as i64)
        .unwrap_or_else(|_| now_ms() as i64)
}

fn escape_key(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Leaves of a value delivered at `base`: telemetry often sends a container
/// path with a small JSON object rather than one update per leaf.
pub fn flatten_value(base: &str, value: &Value, registry: &KeyRegistry) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    flatten_into(base, value, registry, &mut out);
    out
}

fn flatten_into(base: &str, value: &Value, registry: &KeyRegistry, out: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(object) => {
            if object.is_empty() {
                out.push((base.to_owned(), value.clone()));
            }
            let prefix = if base == "/" { "" } else { base };
            for (name, child) in object {
                flatten_into(&format!("{prefix}/{name}"), child, registry, out);
            }
        }
        Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
            let schema = match try_parse_gnmi_path(base) {
                Some(parsed) => format!(
                    "/{}",
                    parsed
                        .elems
                        .iter()
                        .map(|elem| strip_module_prefix(&elem.name))
                        .collect::<Vec<_>>()
                        .join("/")
                ),
                None => base.to_owned(),
            };
            let entries: Vec<&serde_json::Map<String, Value>> =
                items.iter().filter_map(Value::as_object).collect();
            let keys: Vec<String> = registry
                .lookup(&schema)
                .map(<[String]>::to_vec)
                .unwrap_or_else(|| guess_list_keys(&entries));
            for entry in entries {
                let predicate: String = keys
                    .iter()
                    .map(|key| {
                        let field = if entry.contains_key(key) {
                            Some(key.as_str())
                        } else {
                            entry
                                .keys()
                                .find(|name| strip_module_prefix(name) == key)
                                .map(String::as_str)
                        };
                        let text = field
                            .and_then(|field| entry.get(field))
                            .map(scalar_text)
                            .unwrap_or_default();
                        format!("[{}={}]", strip_module_prefix(key), escape_key(&text))
                    })
                    .collect();
                let rest: serde_json::Map<String, Value> = entry
                    .iter()
                    .filter(|(name, _)| !keys.iter().any(|key| key == strip_module_prefix(name)))
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();
                flatten_into(
                    &format!("{base}{predicate}"),
                    &Value::Object(rest),
                    registry,
                    out,
                );
            }
        }
        _ => out.push((base.to_owned(), value.clone())),
    }
}

/// A subscription's live values.
#[derive(Debug, Clone)]
pub struct LiveTable {
    pub rows: IndexMap<String, LiveRow>,
    pub registry: KeyRegistry,
    pub notifications: u64,
    pub leaf_updates: u64,
    pub bytes: u64,
    /// Local clock, milliseconds since the epoch.
    pub started_at: u64,
    /// The device has sent the initial state (sync_response).
    pub synced: bool,
    /// Bumped on every change, for views that sample the table.
    pub version: u64,
}

impl Default for LiveTable {
    fn default() -> Self {
        Self {
            rows: IndexMap::new(),
            registry: KeyRegistry::default(),
            notifications: 0,
            leaf_updates: 0,
            bytes: 0,
            started_at: now_ms(),
            synced: false,
            version: 0,
        }
    }
}

impl LiveTable {
    pub fn new() -> Self {
        Self::default()
    }

    fn changed(&mut self) {
        self.version += 1;
    }

    pub fn mark_synced(&mut self) {
        self.synced = true;
        self.changed();
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.notifications = 0;
        self.leaf_updates = 0;
        self.bytes = 0;
        self.started_at = now_ms();
        self.changed();
    }

    pub fn apply(&mut self, notifications: &[GnmiNotification], bytes: u64) {
        let now = Instant::now();
        self.bytes += bytes;
        for notification in notifications {
            self.notifications += 1;
            let time = nanos_to_millis(&notification.timestamp);
            for update in &notification.updates {
                // Keep the update even if its path does not parse.
                if let Ok(path) = parse_gnmi_path(&update.path) {
                    self.registry.learn(&path);
                }
                for (path, value) in
                    flatten_value(&update.path, &update.value.value, &self.registry)
                {
                    self.leaf_updates += 1;
                    self.update(path, value, time, now);
                }
            }
            for path in &notification.deletes {
                let subtree = format!("{path}/");
                let entries = format!("{path}[");
                for (key, row) in self.rows.iter_mut() {
                    if key == path || key.starts_with(&subtree) || key.starts_with(&entries) {
                        row.deleted = true;
                        row.changed_at = Some(now);
                        row.updated_at = time;
                    }
                }
            }
        }
        if !notifications.is_empty() {
            self.changed();
        }
    }

    fn update(&mut self, path: String, value: Value, time: i64, now: Instant) {
        let numeric = numeric_value(&value);
        let Some(row) = self.rows.get_mut(&path) else {
            self.rows.insert(
                path.clone(),
                LiveRow {
                    path,
                    value,
                    updated_at: time,
                    received_at: now_ms(),
                    updates: 1,
                    numeric,
                    rate: None,
                    history: numeric.into_iter().collect(),
                    counter: numeric.is_some(),
                    changed_at: None,
                    deleted: false,
                    sample_time: numeric.map(|_| time),
                },
            );
            return;
        };
        row.updates += 1;
        row.deleted = false;
        if row.value != value {
            row.changed_at = Some(now);
        }
        if let (Some(numeric), Some(previous), Some(sample_time)) =
            (numeric, row.numeric, row.sample_time)
        {
            let seconds = (time - sample_time) as f64 / 1000.0;
            if row.counter && numeric < previous {
                // It went down: a gauge after all, or a counter that was cleared.
                row.counter = false;
                row.rate = None;
                row.history = vec![previous];
            }
            if row.counter {
                if seconds > 0.0 {
                    // A counter's first sample is its raw value; from the first rate on, the history holds rates.
                    if row.rate.is_none() {
                        row.history.clear();
                    }
                    let rate = (numeric - previous) / seconds;
                    row.rate = Some(rate);
                    row.history.push(rate);
                }
            } else {
                row.history.push(numeric);
            }
            if row.history.len() > HISTORY_POINTS {
                let excess = row.history.len() - HISTORY_POINTS;
                row.history.drain(..excess);
            }
        }
        row.value = value;
        row.updated_at = time;
        row.received_at = now_ms();
        if let Some(numeric) = numeric
            && row
                .sample_time
                .is_none_or(|sample_time| time >= sample_time)
        {
            row.numeric = Some(numeric);
            row.sample_time = Some(time);
        }
    }
}

const SI: [&str; 6] = ["", "k", "M", "G", "T", "P"];

/// `Number.prototype.toPrecision` for the magnitudes `format_si` passes it.
fn to_precision(value: f64, digits: usize) -> String {
    if value == 0.0 {
        return format!("{:.*}", digits.saturating_sub(1), 0.0);
    }
    let exponent = value.abs().log10().floor() as i32;
    let decimals = (digits as i32 - 1 - exponent).max(0) as usize;
    let rounded = format!("{value:.decimals$}");
    // Rounding can carry into a new digit (9.995 → 10.00): one decimal fewer then.
    let integer_digits = rounded
        .split('.')
        .next()
        .unwrap_or("")
        .trim_start_matches('-')
        .len() as i32;
    if integer_digits > exponent + 1 && decimals > 0 {
        return format!("{value:.prec$}", prec = decimals - 1);
    }
    rounded
}

/// 1234567 → "1.23 M".
pub fn format_si(value: f64, digits: usize) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    let mut magnitude = value.abs();
    let mut unit = 0;
    while magnitude >= 1000.0 && unit < SI.len() - 1 {
        magnitude /= 1000.0;
        unit += 1;
    }
    let text = if magnitude == 0.0 {
        "0".to_owned()
    } else if magnitude >= 100.0 {
        format!("{magnitude:.0}")
    } else {
        to_precision(magnitude, digits)
    };
    // `.replace(/\.0+$/, '').replace(/(\.\d*?)0+$/, '$1')`
    let text = match text.split_once('.') {
        Some((whole, fraction)) => match fraction.trim_end_matches('0') {
            "" => whole.to_owned(),
            fraction => format!("{whole}.{fraction}"),
        },
        None => text,
    };
    format!(
        "{sign}{text}{}",
        if SI[unit].is_empty() {
            String::new()
        } else {
            format!(" {}", SI[unit])
        }
    )
}

fn with_unit(si: &str, unit: &str) -> String {
    // "1.2 M" + "bit/s" → "1.2 Mbit/s"; "512" + "bit/s" → "512 bit/s".
    if si.contains(' ') {
        format!("{si}{unit}")
    } else {
        format!("{si} {unit}")
    }
}

/// A counter rate in the units its leaf name suggests.
pub fn format_rate(path: &str, rate: f64) -> String {
    let leaf =
        strip_module_prefix(&path[path.rfind('/').map_or(0, |slash| slash + 1)..]).to_lowercase();
    if leaf.contains("octets") || leaf.contains("bytes") {
        return with_unit(&format_si(rate * 8.0, 3), "bit/s");
    }
    if leaf.contains("packets") || leaf.contains("pkts") || leaf.contains("frames") {
        return with_unit(&format_si(rate, 3), "pkt/s");
    }
    with_unit(&format_si(rate, 3), "/s")
}

#[cfg(test)]
mod tests {
    use muxus_core::management::{GnmiUpdate, GnmiValue, GnmiValueType};
    use serde_json::json;

    use super::*;

    fn notification(seconds: i64, value: &str) -> GnmiNotification {
        GnmiNotification {
            timestamp: (seconds as i128 * 1_000_000_000).to_string(),
            prefix: None,
            atomic: None,
            updates: vec![GnmiUpdate {
                path: "/srl_nokia-interfaces:interface[name=e1]/statistics".into(),
                value: GnmiValue {
                    kind: GnmiValueType::JsonIetf,
                    value: json!({"in-octets": value, "oper-state": if seconds > 2 { "down" } else { "up" }}),
                },
                duplicates: None,
            }],
            deletes: vec![],
        }
    }

    const OCTETS: &str = "/srl_nokia-interfaces:interface[name=e1]/statistics/in-octets";

    #[test]
    fn flattens_container_updates_into_leaves_and_computes_counter_rates() {
        let mut table = LiveTable::new();
        table.apply(&[notification(1, "1000")], 100);
        table.apply(&[notification(2, "3000")], 100);
        table.apply(&[notification(3, "6000")], 100);
        let octets = &table.rows[OCTETS];
        assert!(octets.counter);
        assert_eq!(octets.rate, Some(3000.0));
        assert_eq!(octets.history, vec![2000.0, 3000.0]);
        assert_eq!(format_rate(&octets.path, octets.rate.unwrap()), "24 kbit/s");
        let state = &table.rows["/srl_nokia-interfaces:interface[name=e1]/statistics/oper-state"];
        assert_eq!(state.value, json!("down"));
        assert!(state.changed_at.is_some());
        assert_eq!(table.leaf_updates, 6);
        assert_eq!(table.bytes, 300);
    }

    #[test]
    fn treats_a_value_that_goes_down_as_a_gauge() {
        let mut table = LiveTable::new();
        table.apply(&[notification(1, "50")], 0);
        table.apply(&[notification(2, "40")], 0);
        table.apply(&[notification(3, "45")], 0);
        let row = &table.rows[OCTETS];
        assert!(!row.counter);
        assert_eq!(row.rate, None);
        assert_eq!(row.history, vec![50.0, 40.0, 45.0]);
    }

    #[test]
    fn marks_deleted_subtrees_and_keys_list_entries_inside_values() {
        let mut table = LiveTable::new();
        table.apply(&[notification(1, "1")], 0);
        table.apply(
            &[GnmiNotification {
                timestamp: "2000000000".into(),
                prefix: None,
                atomic: None,
                updates: vec![],
                deletes: vec!["/srl_nokia-interfaces:interface[name=e1]".into()],
            }],
            0,
        );
        assert!(table.rows.values().all(|row| row.deleted));
        assert_eq!(
            flatten_value(
                "/acl",
                &json!({"filter": [{"name": "a", "hits": 1}, {"name": "b", "hits": 2}]}),
                &KeyRegistry::default()
            ),
            vec![
                ("/acl/filter[name=a]/hits".to_owned(), json!(1)),
                ("/acl/filter[name=b]/hits".to_owned(), json!(2)),
            ]
        );
        assert_eq!(format_si(1_234_567.0, 3), "1.23 M");
    }

    #[test]
    fn formats_like_to_precision() {
        assert_eq!(format_si(0.0, 3), "0");
        assert_eq!(format_si(12.0, 3), "12");
        assert_eq!(format_si(1.5, 3), "1.5");
        assert_eq!(format_si(999.0, 3), "999");
        assert_eq!(format_si(24_000.0, 3), "24 k");
        assert_eq!(format_si(-2_500_000.0, 3), "-2.5 M");
        assert_eq!(format_si(9.999, 3), "10");
        assert_eq!(numeric_value(&json!("12")), Some(12.0));
        assert_eq!(numeric_value(&json!(" -1.5e3 ")), Some(-1500.0));
        assert_eq!(numeric_value(&json!("1.")), None);
        assert_eq!(numeric_value(&json!("up")), None);
        assert_eq!(format_rate("/a/out-pkts", 5.0), "5 pkt/s");
        assert_eq!(format_rate("/a/errors", 0.5), "0.5 /s");
    }
}
