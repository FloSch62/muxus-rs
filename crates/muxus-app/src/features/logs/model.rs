//! What the log viewer shows and copies (`components/LogViewerDialog.tsx`,
//! `formatLogEntry` in `client/src/api/logs.ts`): the level filter, the text
//! filter, one line per entry and the export document.

use muxus_core::api::{AppLogEntry, AppLogLevel, AppLogSource, AppLogsResponse};

/// The Level select's choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LevelFilter {
    #[default]
    All,
    Debug,
    Info,
    Warn,
    Error,
}

impl LevelFilter {
    pub const ALL: [LevelFilter; 5] = [
        LevelFilter::All,
        LevelFilter::Debug,
        LevelFilter::Info,
        LevelFilter::Warn,
        LevelFilter::Error,
    ];

    /// The menu item's text.
    pub fn label(self) -> &'static str {
        match self {
            LevelFilter::All => "All",
            LevelFilter::Debug => "Debug +",
            LevelFilter::Info => "Info +",
            LevelFilter::Warn => "Warning +",
            LevelFilter::Error => "Error",
        }
    }

    /// Minimum severity per filter choice; 'all' shows trace upward.
    fn rank(self) -> u8 {
        match self {
            LevelFilter::All => 0,
            LevelFilter::Debug => 1,
            LevelFilter::Info => 2,
            LevelFilter::Warn => 3,
            LevelFilter::Error => 4,
        }
    }
}

fn level_rank(level: AppLogLevel) -> u8 {
    match level {
        AppLogLevel::Trace => 0,
        AppLogLevel::Debug => 1,
        AppLogLevel::Info => 2,
        AppLogLevel::Warn => 3,
        AppLogLevel::Error => 4,
        AppLogLevel::Fatal => 5,
    }
}

/// The palette slot an entry's line is drawn in (`levelColor`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelInk {
    Error,
    Warning,
    Secondary,
    Primary,
}

pub fn level_ink(level: AppLogLevel) -> LevelInk {
    match level {
        AppLogLevel::Error | AppLogLevel::Fatal => LevelInk::Error,
        AppLogLevel::Warn => LevelInk::Warning,
        AppLogLevel::Debug | AppLogLevel::Trace => LevelInk::Secondary,
        AppLogLevel::Info => LevelInk::Primary,
    }
}

fn level_name(level: AppLogLevel) -> &'static str {
    match level {
        AppLogLevel::Trace => "trace",
        AppLogLevel::Debug => "debug",
        AppLogLevel::Info => "info",
        AppLogLevel::Warn => "warn",
        AppLogLevel::Error => "error",
        AppLogLevel::Fatal => "fatal",
    }
}

fn source_name(source: AppLogSource) -> &'static str {
    match source {
        AppLogSource::Server => "server",
        AppLogSource::App => "app",
    }
}

/// `new Date(ms).toISOString()`.
pub fn iso_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "Invalid Date".into())
}

fn pad_end(text: &str, width: usize) -> String {
    format!("{text:<width$}")
}

/// One text line per entry, shared by the viewer, copy, and export
/// (`formatLogEntry`).
pub fn format_log_entry(entry: &AppLogEntry) -> String {
    let context = entry
        .context
        .as_ref()
        .map(|context| {
            format!(
                " {}",
                serde_json::to_string(context).unwrap_or_else(|_| "{}".into())
            )
        })
        .unwrap_or_default();
    format!(
        "{} {} {} {}{context}",
        iso_time(entry.ts),
        pad_end(&level_name(entry.level).to_uppercase(), 5),
        pad_end(source_name(entry.source), 6),
        entry.msg
    )
}

/// Whether an entry passes the level filter and the (already trimmed,
/// lowercased) text filter.
pub fn matches(entry: &AppLogEntry, filter: LevelFilter, needle: &str) -> bool {
    if level_rank(entry.level) < filter.rank() {
        return false;
    }
    needle.is_empty() || format_log_entry(entry).to_lowercase().contains(needle)
}

/// The entries the viewer lists, oldest first.
pub fn visible_entries<'a>(
    entries: &'a [AppLogEntry],
    filter: LevelFilter,
    query: &str,
) -> Vec<&'a AppLogEntry> {
    let needle = muxus_core::js::js_trim(query).to_lowercase();
    entries
        .iter()
        .filter(|entry| matches(entry, filter, &needle))
        .collect()
}

/// `<shown> of <total> entries`.
pub fn count_label(shown: usize, total: usize) -> String {
    format!("{shown} of {total} entries")
}

/// What Copy and Export write: a two-line header, then the listed entries.
pub fn export_text(entries: &[&AppLogEntry], debug_enabled: bool, now_ms: i64) -> String {
    let mut lines = vec![
        format!("# Muxus diagnostic log — exported {}", iso_time(now_ms)),
        format!(
            "# {} entries, debug logging {}",
            entries.len(),
            if debug_enabled { "on" } else { "off" }
        ),
    ];
    lines.extend(entries.iter().map(|entry| format_log_entry(entry)));
    lines.join("\n")
}

/// The notice above the list while only info and above is captured.
pub const DEBUG_OFF_NOTICE: &str = "Debug logging is off — only informational messages, warnings and errors are captured. Turn on debug mode in the settings for connection-level detail.";

/// The list's empty state.
pub const EMPTY_TEXT: &str =
    "Nothing captured yet. Connection attempts, warnings and errors show up here as they happen.";

/// Whether the notice shows: once the logs loaded, while debug logging is off.
pub fn shows_debug_notice(logs: Option<&AppLogsResponse>) -> bool {
    logs.is_some_and(|logs| !logs.debug_enabled)
}

/// Follow new output like a terminal until the user scrolls back up: the
/// view is pinned while its end is less than 24px away.
pub fn pinned_to_end(scroll_height: f32, scroll_top: f32, client_height: f32) -> bool {
    scroll_height - scroll_top - client_height < 24.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(level: AppLogLevel, msg: &str, context: Option<serde_json::Value>) -> AppLogEntry {
        AppLogEntry {
            ts: 1_760_288_584_123,
            level,
            source: AppLogSource::Server,
            msg: msg.into(),
            context: context.and_then(|value| value.as_object().cloned()),
        }
    }

    #[test]
    fn formats_one_line_per_entry() {
        assert_eq!(
            format_log_entry(&entry(
                AppLogLevel::Warn,
                "ssh connect failed",
                Some(serde_json::json!({ "host": "web-01", "port": 22, "err": "timed out" }))
            )),
            "2025-10-12T17:03:04.123Z WARN  server ssh connect failed {\"host\":\"web-01\",\"port\":22,\"err\":\"timed out\"}"
        );
        let app = AppLogEntry {
            source: AppLogSource::App,
            ..entry(AppLogLevel::Debug, "window opened", None)
        };
        assert_eq!(
            format_log_entry(&app),
            "2025-10-12T17:03:04.123Z DEBUG app    window opened"
        );
    }

    #[test]
    fn filters_by_minimum_level_and_text() {
        let entries = vec![
            entry(AppLogLevel::Trace, "trace line", None),
            entry(AppLogLevel::Debug, "debug line", None),
            entry(AppLogLevel::Info, "info about web-01", None),
            entry(AppLogLevel::Warn, "warn line", None),
            entry(AppLogLevel::Error, "error line", None),
            entry(AppLogLevel::Fatal, "fatal line", None),
        ];
        let msgs = |filter, query| {
            visible_entries(&entries, filter, query)
                .iter()
                .map(|entry| entry.msg.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(msgs(LevelFilter::All, "").len(), 6);
        assert_eq!(msgs(LevelFilter::Debug, "").len(), 5);
        assert_eq!(
            msgs(LevelFilter::Warn, ""),
            ["warn line", "error line", "fatal line"]
        );
        assert_eq!(msgs(LevelFilter::Error, ""), ["error line", "fatal line"]);
        // The text filter matches the whole formatted line, case-insensitively.
        assert_eq!(msgs(LevelFilter::All, "  WEB-01 "), ["info about web-01"]);
        assert_eq!(msgs(LevelFilter::All, "ERROR"), ["error line"]);
        assert_eq!(
            msgs(LevelFilter::Warn, "server"),
            ["warn line", "error line", "fatal line"]
        );
    }

    #[test]
    fn colors_lines_by_level() {
        assert_eq!(level_ink(AppLogLevel::Fatal), LevelInk::Error);
        assert_eq!(level_ink(AppLogLevel::Warn), LevelInk::Warning);
        assert_eq!(level_ink(AppLogLevel::Trace), LevelInk::Secondary);
        assert_eq!(level_ink(AppLogLevel::Info), LevelInk::Primary);
    }

    #[test]
    fn exports_a_header_and_the_listed_entries() {
        let entries = [entry(AppLogLevel::Info, "hello", None)];
        let listed: Vec<&AppLogEntry> = entries.iter().collect();
        assert_eq!(
            export_text(&listed, false, 1_760_288_584_000),
            "# Muxus diagnostic log — exported 2025-10-12T17:03:04.000Z\n# 1 entries, debug logging off\n2025-10-12T17:03:04.123Z INFO  server hello"
        );
        assert_eq!(count_label(3, 10), "3 of 10 entries");
        assert!(pinned_to_end(1000., 680., 300.));
        assert!(!pinned_to_end(1000., 600., 300.));
    }
}
