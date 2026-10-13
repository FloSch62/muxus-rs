//! Times, counts and file names as the workbench writes them.
//!
//! The React components format dates with the browser's `en-US` locale
//! (`toLocaleTimeString`, `toLocaleString`); these reproduce that output,
//! including the narrow no-break space Chromium puts before AM/PM.

use chrono::{DateTime, Local, TimeZone, Utc};
use muxus_mgmt::workbench::data_tree::js_number_text;

/// Chromium's separator before the day period.
const NNBSP: char = '\u{202f}';

/// `String(number)` for an `f64`.
pub fn js_number_string(value: f64) -> String {
    match serde_json::Number::from_f64(value) {
        Some(number) => js_number_text(&number),
        None if value.is_nan() => "NaN".into(),
        None if value > 0. => "Infinity".into(),
        None => "-Infinity".into(),
    }
}

/// `new Date(ms).toISOString()`.
pub fn iso_ms(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(muxus_core::js::iso_timestamp)
        .unwrap_or_default()
}

fn local(ms: i64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ms).single()
}

fn twelve_hour(at: &DateTime<Local>, pad_hour: bool) -> String {
    use chrono::Timelike;
    let (pm, hour) = at.hour12();
    let hour = if pad_hour {
        format!("{hour:02}")
    } else {
        hour.to_string()
    };
    format!(
        "{hour}:{:02}:{:02}{NNBSP}{}",
        at.minute(),
        at.second(),
        if pm { "PM" } else { "AM" }
    )
}

/// `toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' })`.
pub fn time_of(ms: u64) -> String {
    local(ms as i64)
        .map(|at| twelve_hour(&at, true))
        .unwrap_or_default()
}

/// `toLocaleTimeString()`.
pub fn locale_time(ms: i64) -> String {
    local(ms)
        .map(|at| twelve_hour(&at, false))
        .unwrap_or_default()
}

/// `toLocaleString()`: `10/12/2026, 2:05:09 PM`.
pub fn locale_date_time(ms: i64) -> String {
    local(ms)
        .map(|at| format!("{}, {}", at.format("%-m/%-d/%Y"), twelve_hour(&at, false)))
        .unwrap_or_default()
}

/// An ISO timestamp (`connectedAt`, `eventTime`) as milliseconds.
pub fn parse_ms(iso: &str) -> Option<i64> {
    muxus_core::js::parse_timestamp(iso).map(|at| at.timestamp_millis())
}

/// The time left on a confirmed commit, `m:ss`.
pub fn countdown(ms: i64) -> String {
    let total = (ms.max(0) as f64 / 1000.).ceil() as i64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// `exportFilename`: `muxus-<slug>-<yyyymmdd-hhmmss>.<ext>`.
pub fn export_filename(title: &str, ext: &str, now: DateTime<Local>) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for c in title.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if dash && !slug.is_empty() {
                slug.push('-');
            }
            dash = false;
            slug.push(c);
        } else {
            dash = true;
        }
    }
    let slug: String = slug.chars().take(40).collect();
    let slug = slug.trim_end_matches('-');
    let slug = if slug.is_empty() { "terminal" } else { slug };
    format!("muxus-{slug}-{}.{ext}", now.format("%Y%m%d-%H%M%S"))
}

/// `1 value`, `2 values`.
pub fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn writes_numbers_like_javascript() {
        assert_eq!(js_number_string(2.5), "2.5");
        assert_eq!(js_number_string(3.0), "3");
        assert_eq!(js_number_string(f64::NAN), "NaN");
        assert_eq!(iso_ms(1_500), "1970-01-01T00:00:01.500Z");
    }

    #[test]
    fn counts_down_in_minutes_and_seconds() {
        assert_eq!(countdown(0), "0:00");
        assert_eq!(countdown(-5), "0:00");
        assert_eq!(countdown(1), "0:01");
        assert_eq!(countdown(61_000), "1:01");
        assert_eq!(countdown(120_000), "2:00");
    }

    #[test]
    fn names_exports_like_the_browser() {
        let at = Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, 3, 4)
                    .unwrap()
                    .and_hms_opt(5, 6, 7)
                    .unwrap(),
            )
            .single()
            .unwrap();
        assert_eq!(
            export_filename("leaf1 telemetry", "csv", at),
            "muxus-leaf1-telemetry-20260304-050607.csv"
        );
        assert_eq!(
            export_filename("gnmi /interface[name=*]/statistics", "json", at),
            "muxus-gnmi-interface-name-statistics-20260304-050607.json"
        );
        assert_eq!(
            export_filename("***", "xml", at),
            "muxus-terminal-20260304-050607.xml"
        );
        assert_eq!(plural(1, "value", "values"), "1 value");
        assert_eq!(plural(0, "value", "values"), "0 values");
    }

    #[test]
    fn formats_local_times_as_en_us() {
        let ms = Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, 10, 12)
                    .unwrap()
                    .and_hms_opt(14, 5, 9)
                    .unwrap(),
            )
            .single()
            .unwrap()
            .timestamp_millis();
        assert_eq!(time_of(ms as u64), "02:05:09\u{202f}PM");
        assert_eq!(locale_time(ms), "2:05:09\u{202f}PM");
        assert_eq!(locale_date_time(ms), "10/12/2026, 2:05:09\u{202f}PM");
        assert_eq!(parse_ms("1970-01-01T00:00:01.000Z"), Some(1000));
    }
}
