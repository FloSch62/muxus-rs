//! Files the settings hand to the user: names (`client/src/save-file.ts`)
//! and the exported log's lines (`formatLogEntry` of `client/src/api/logs.ts`).

use chrono::{Datelike, Local, Timelike};
use muxus_core::api::AppLogEntry;

/// `muxus-<slug>-YYYYMMDD-HHMMSS.<ext>` for exports, in local time.
pub fn export_filename(title: &str, ext: &str) -> String {
    export_filename_at(title, ext, Local::now().naive_local())
}

pub fn export_filename_at(title: &str, ext: &str, now: chrono::NaiveDateTime) -> String {
    let lower = title.to_lowercase();
    let mut slug = String::new();
    let mut gap = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            gap = false;
            slug.push(c);
        } else {
            gap = true;
        }
    }
    let slug: String = slug.chars().take(40).collect();
    let slug = if slug.is_empty() {
        "terminal".to_string()
    } else {
        slug
    };
    format!(
        "muxus-{slug}-{:04}{:02}{:02}-{:02}{:02}{:02}.{ext}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// One line of the exported log.
pub fn format_log_entry(entry: &AppLogEntry) -> String {
    let at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(entry.ts)
        .map(muxus_core::js::iso_timestamp)
        .unwrap_or_default();
    let level = serde_json::to_value(entry.level)
        .ok()
        .and_then(|value| value.as_str().map(str::to_uppercase))
        .unwrap_or_default();
    let source = serde_json::to_value(entry.source)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    let context = entry
        .context
        .as_ref()
        .and_then(|context| serde_json::to_string(context).ok())
        .map(|json| format!(" {json}"))
        .unwrap_or_default();
    format!("{at} {level:<5} {source:<6} {}{context}", entry.msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::api::{AppLogLevel, AppLogSource};

    #[test]
    fn export_names_slug_the_title() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 1, 2)
            .unwrap()
            .and_hms_opt(3, 4, 5)
            .unwrap();
        assert_eq!(
            export_filename_at("Debug log", "log", now),
            "muxus-debug-log-20260102-030405.log"
        );
        assert_eq!(
            export_filename_at("  ** ", "json", now),
            "muxus-terminal-20260102-030405.json"
        );
        assert_eq!(
            export_filename_at("SR OS highlighting profile", "muxus-highlight.json", now),
            "muxus-sr-os-highlighting-profile-20260102-030405.muxus-highlight.json"
        );
    }

    #[test]
    fn log_lines_pad_level_and_source() {
        let entry = AppLogEntry {
            ts: 0,
            level: AppLogLevel::Warn,
            source: AppLogSource::Server,
            msg: "dial failed".into(),
            context: Some(
                serde_json::json!({"host": "web"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        };
        assert_eq!(
            format_log_entry(&entry),
            "1970-01-01T00:00:00.000Z WARN  server dial failed {\"host\":\"web\"}"
        );
    }
}
