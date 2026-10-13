//! Helpers the gNOI and gNSI tools share (`tools/common.tsx`,
//! `tools/rotation.tsx`).

use chrono::{DateTime, Utc};
use muxus_core::gnmi_path::GnmiPathElem;
use serde_json::{Map, Value, json};

use super::format::locale_date_time;

/// `Number(value ?? 0)` for what a gRPC JSON message holds.
pub fn number_of(value: Option<&Value>) -> f64 {
    match value {
        None | Some(Value::Null) => 0.,
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.),
        Some(Value::String(text)) => {
            let text = muxus_core::js::js_trim(text);
            if text.is_empty() {
                0.
            } else {
                text.parse().unwrap_or(f64::NAN)
            }
        }
        Some(Value::Bool(flag)) => f64::from(u8::from(*flag)),
        Some(_) => f64::NAN,
    }
}

/// Nanoseconds as milliseconds, with two decimals below 10 ms.
pub fn nanos_to_ms(value: Option<&Value>) -> String {
    let ms = number_of(value) / 1e6;
    if ms < 10. {
        format!("{ms:.2}")
    } else if ms < 100. {
        format!("{ms:.1}")
    } else {
        format!("{ms:.0}")
    }
}

/// Unix nanoseconds (as gNOI sends them) or seconds as a local date and time.
pub fn device_time(value: Option<&Value>, seconds: bool) -> String {
    let text = match value {
        None | Some(Value::Null) => return "—".into(),
        Some(Value::String(text)) if text.is_empty() || text == "0" => return "—".into(),
        Some(Value::Number(number)) if number.as_f64() == Some(0.) => return "—".into(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(other) => return other.to_string(),
    };
    let ms = if seconds {
        text.parse::<f64>().map(|s| (s * 1000.) as i64).ok()
    } else {
        text.parse::<i128>()
            .ok()
            .map(|nanos| (nanos / 1_000_000) as i64)
    };
    match ms {
        Some(ms) => locale_date_time(ms),
        None => text,
    }
}

/// gNMI path elements → `gnoi.types.Path` JSON (origin and keyed elements).
pub fn gnoi_path(elems: &[GnmiPathElem], origin: Option<&str>) -> Value {
    let mut path = Map::new();
    if let Some(origin) = origin.filter(|origin| !origin.is_empty()) {
        path.insert("origin".into(), Value::String(origin.to_owned()));
    }
    path.insert(
        "elem".into(),
        Value::Array(
            elems
                .iter()
                .map(|elem| {
                    let mut out = Map::new();
                    out.insert("name".into(), Value::String(elem.name.clone()));
                    if let Some(keys) = &elem.keys {
                        out.insert(
                            "key".into(),
                            Value::Object(
                                keys.iter()
                                    .map(|(key, value)| (key.to_owned(), json!(value)))
                                    .collect(),
                            ),
                        );
                    }
                    Value::Object(out)
                })
                .collect(),
        ),
    );
    Value::Object(path)
}

/// A fresh version string for a gNSI upload; devices refuse to re-upload the same one.
pub fn rotation_version(now: DateTime<Utc>) -> String {
    // `new Date().toISOString().replace(/[-:.]/g, '').replace('T', '-').slice(0, 15)`
    let iso = muxus_core::js::iso_timestamp(now)
        .replace(['-', ':', '.'], "")
        .replacen('T', "-", 1);
    format!("muxus-{}", iso.chars().take(15).collect::<String>())
}

/// How long a rotation has been pending.
pub fn elapsed(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds} s")
    } else {
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use muxus_core::gnmi_path::parse_gnmi_path;

    #[test]
    fn formats_durations_from_nanoseconds() {
        assert_eq!(nanos_to_ms(Some(&json!(1_234_567))), "1.23");
        assert_eq!(nanos_to_ms(Some(&json!("45600000"))), "45.6");
        assert_eq!(nanos_to_ms(Some(&json!(123_400_000))), "123");
        assert_eq!(nanos_to_ms(None), "0.00");
    }

    #[test]
    fn device_times_need_a_value() {
        assert_eq!(device_time(None, false), "—");
        assert_eq!(device_time(Some(&json!("0")), false), "—");
        assert_eq!(device_time(Some(&json!(0)), true), "—");
        assert_eq!(device_time(Some(&json!("soon")), false), "soon");
        assert!(device_time(Some(&json!("1700000000000000000")), false).contains("2023"));
    }

    #[test]
    fn writes_gnoi_paths() {
        let path = parse_gnmi_path("/interface[name=e1]/subinterface").unwrap();
        assert_eq!(
            gnoi_path(&path.elems, Some("openconfig")),
            json!({"origin": "openconfig", "elem": [{"name": "interface", "key": {"name": "e1"}}, {"name": "subinterface"}]})
        );
        assert_eq!(gnoi_path(&[], None), json!({"elem": []}));
    }

    #[test]
    fn names_rotation_versions_by_time() {
        let at = Utc.with_ymd_and_hms(2026, 10, 12, 9, 8, 7).unwrap();
        assert_eq!(rotation_version(at), "muxus-20261012-090807");
        assert_eq!(elapsed(59), "59 s");
        assert_eq!(elapsed(125), "2 min 5 s");
    }
}
