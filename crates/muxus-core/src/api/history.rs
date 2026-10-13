//! Session history and session log file types.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::js;
use crate::protocol::ProfileKind;

/// Effective retention and privacy policy for one host (or "*" for defaults).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLoggingPolicy {
    pub profile_key: String,
    pub enabled: bool,
    /// Record bytes sent by the user. Disabled by default because commands can
    /// contain passwords, tokens, and other values that should not be retained.
    pub capture_input: bool,
    /// Rotate before a retained raw-log part grows beyond this many bytes.
    pub max_part_bytes: u64,
    /// Number of newest rotated parts retained for each session.
    pub max_parts: u32,
    /// Start a plain-text log file when a session opens, independent of history.
    pub log_to_file: bool,
    /// True when an exact per-host override exists instead of inherited defaults.
    pub overridden: bool,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLoggingPolicyInput {
    pub enabled: bool,
    pub capture_input: bool,
    pub max_part_bytes: u64,
    pub max_parts: u32,
    /// Absent in policies saved before log files existed; treated as off.
    pub log_to_file: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionLogStatus {
    Active,
    Completed,
    Disconnected,
    Failed,
}

impl SessionLogStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionLogStatus::Active => "active",
            SessionLogStatus::Completed => "completed",
            SessionLogStatus::Disconnected => "disconnected",
            SessionLogStatus::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(SessionLogStatus::Active),
            "completed" => Some(SessionLogStatus::Completed),
            "disconnected" => Some(SessionLogStatus::Disconnected),
            "failed" => Some(SessionLogStatus::Failed),
            _ => None,
        }
    }
}

/// How a finished session ended (the statuses a session can finish with).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionEndStatus {
    Completed,
    Disconnected,
    Failed,
}

impl From<SessionEndStatus> for SessionLogStatus {
    fn from(value: SessionEndStatus) -> Self {
        match value {
            SessionEndStatus::Completed => SessionLogStatus::Completed,
            SessionEndStatus::Disconnected => SessionLogStatus::Disconnected,
            SessionEndStatus::Failed => SessionLogStatus::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionLogDirection {
    Input,
    Output,
    System,
}

impl SessionLogDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionLogDirection::Input => "input",
            SessionLogDirection::Output => "output",
            SessionLogDirection::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "input" => Some(SessionLogDirection::Input),
            "output" => Some(SessionLogDirection::Output),
            "system" => Some(SessionLogDirection::System),
            _ => None,
        }
    }
}

/// Durable session metadata returned by history list/search.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogSummary {
    pub id: String,
    pub profile_key: String,
    pub title: String,
    /// User-chosen name shown instead of the recorded title.
    pub label: Option<String>,
    /// The session's profile kind (`ssh`, `local`, `serial`, `telnet`); kept as
    /// text because old databases admit `openssh` as well.
    pub kind: String,
    pub host: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: SessionLogStatus,
    pub paused: bool,
    pub capture_input: bool,
    /// Pinned sessions are excluded from age and quota eviction.
    pub pinned: bool,
    pub event_count: u64,
    pub raw_bytes: u64,
    pub normalized_bytes: u64,
    pub part_count: u64,
    /// Search-context excerpt centered on the first full-text match, present
    /// only for matching queries. Matched ranges are wrapped in
    /// [`SNIPPET_MATCH_START`]/[`SNIPPET_MATCH_END`] sentinels.
    pub snippet: Option<String>,
    /// Number of matching transcript chunks, present only for full-text matches.
    pub match_count: Option<u64>,
}

impl SessionLogSummary {
    /// The name a retained session is listed and exported under.
    pub fn name(&self) -> &str {
        session_log_name(&self.title, self.label.as_deref())
    }

    /// The kind as a profile kind, when it is one.
    pub fn profile_kind(&self) -> Option<ProfileKind> {
        ProfileKind::parse(&self.kind)
    }
}

/// The name a retained session is listed and exported under.
pub fn session_log_name<'a>(title: &'a str, label: Option<&'a str>) -> &'a str {
    label.unwrap_or(title)
}

/// Control characters delimiting highlighted ranges inside a search snippet.
pub const SNIPPET_MATCH_START: char = '\u{0001}';
pub const SNIPPET_MATCH_END: char = '\u{0002}';

/// UTF-16 offset of a normalized line and the time its visible content last changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLineTimestamp {
    pub offset: u64,
    pub recorded_at: String,
}

/// One timestamped, normalized replay event. Raw bytes remain in the backend.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogEvent {
    pub sequence: u64,
    pub recorded_at: String,
    pub elapsed_ms: u64,
    pub direction: SessionLogDirection,
    pub text: String,
    /// Absent on older recordings, whose event timestamps are approximate.
    pub line_timestamps: Option<Vec<SessionLineTimestamp>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogDetail {
    #[serde(flatten)]
    pub summary: SessionLogSummary,
    pub events: Vec<SessionLogEvent>,
    /// True when only the newest preview events were returned.
    pub events_truncated: bool,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryResponse {
    pub sessions: Vec<SessionLogSummary>,
    /// Opaque key for the next page. Exact result counts are intentionally omitted.
    pub next_cursor: Option<String>,
}

/// Global history limits. The per-host policy still owns segment size/count.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistorySettings {
    /// Configured root, or `None` when Muxus uses its platform default.
    pub storage_location: Option<String>,
    /// Hard quota measured from actual files, including SQLite WAL/SHM files.
    pub max_total_bytes: u64,
    pub min_free_bytes: u64,
    #[serde(with = "js::number")]
    pub min_free_percent: f64,
    /// Completed, unpinned sessions older than this are removed; `None` disables it.
    pub max_age_days: Option<u32>,
}

/// Same fields as [`SessionHistorySettings`]; the input of a settings save.
pub type SessionHistorySettingsInput = SessionHistorySettings;

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryStorageStatus {
    pub settings: SessionHistorySettings,
    pub active_storage_location: String,
    pub usage_bytes: u64,
    pub free_bytes: u64,
    pub quota_suspended: bool,
    pub warning: Option<String>,
    /// A changed storage path is picked up on the next Muxus launch.
    pub restart_required: bool,
}

/// Where and how plain-text session log files are written.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogFileSettings {
    /// Configured folder, or `None` when Muxus uses its platform default.
    pub directory: Option<String>,
    /// File name below the folder; see `SESSION_LOG_FILE_PLACEHOLDERS`.
    pub filename_pattern: String,
    /// Prefix every line with the UTC time it last changed, like the clean log.
    pub timestamps: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogFileStatus {
    pub settings: SessionLogFileSettings,
    /// The folder new log files go to, with the default resolved.
    pub active_directory: String,
}

/// The session a recording starts for (`SessionLogCreateInput` in the TypeScript).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogCreateInput {
    pub profile_key: String,
    pub title: String,
    /// `ssh`, `local`, `serial` or `telnet`.
    pub kind: ProfileKind,
    pub host: String,
    pub started_at: String,
    pub capture_input: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_flattens_the_summary() {
        let summary = SessionLogSummary {
            id: "s1".into(),
            profile_key: "ssh:web".into(),
            title: "web".into(),
            label: None,
            kind: "ssh".into(),
            host: "web".into(),
            started_at: "2026-01-01T00:00:00.000Z".into(),
            ended_at: None,
            status: SessionLogStatus::Active,
            paused: false,
            capture_input: false,
            event_count: 0,
            raw_bytes: 0,
            normalized_bytes: 0,
            part_count: 0,
            pinned: false,
            snippet: None,
            match_count: None,
        };
        let detail = SessionLogDetail {
            summary,
            events: vec![],
            events_truncated: false,
        };
        let json = serde_json::to_string(&detail).unwrap();
        assert_eq!(
            json,
            r#"{"id":"s1","profileKey":"ssh:web","title":"web","kind":"ssh","host":"web","startedAt":"2026-01-01T00:00:00.000Z","status":"active","paused":false,"captureInput":false,"pinned":false,"eventCount":0,"rawBytes":0,"normalizedBytes":0,"partCount":0,"events":[],"eventsTruncated":false}"#
        );
        let back: SessionLogDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(back, detail);
    }

    #[test]
    fn settings_write_percentages_like_javascript() {
        let settings = SessionHistorySettings {
            storage_location: None,
            max_total_bytes: 5_368_709_120,
            min_free_bytes: 2_147_483_648,
            min_free_percent: 5.0,
            max_age_days: None,
        };
        assert_eq!(
            serde_json::to_string(&settings).unwrap(),
            r#"{"maxTotalBytes":5368709120,"minFreeBytes":2147483648,"minFreePercent":5}"#
        );
    }
}
