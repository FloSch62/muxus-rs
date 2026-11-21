//! Shared session lists: a list published at a link (a MobaXterm shared sessions file, a
//! list Muxus wrote for a folder, or a Muxus backup) whose hosts Muxus keeps up to date
//! inside one sidebar folder.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::protocol::SavedHostSessionProfile;
use crate::validate::{self, Validate, ValidationError, join};

/// Largest shared session list Muxus reads from a link.
pub const MAX_SHARED_SESSIONS_BYTES: usize = 10 * 1024 * 1024;
/// Most hosts one shared session list can keep in Muxus.
pub const MAX_SHARED_SESSION_HOSTS: usize = 10_000;
/// Longest pause between two checks of a shared session list: a week.
pub const MAX_SHARED_SESSIONS_REFRESH_MINUTES: u32 = 7 * 24 * 60;
/// A link that failed is tried again this soon, whatever its interval.
const SHARED_SESSIONS_RETRY_MINUTES: u32 = 15;

/// A session list published at a link, and how following it is going.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedSessionSource {
    pub id: String,
    pub name: String,
    /// Where the list is read from: an http:// or https:// address, or the path of a file
    /// on this computer or a share. Often personal, so never logged.
    pub url: String,
    /// Sidebar folder its hosts live in; the list's own folders nest inside.
    pub folder: String,
    /// Minutes between checks; 0 checks only when asked to.
    pub refresh_minutes: u32,
    /// Hosts currently kept from this list.
    pub host_count: u64,
    /// Last time the link was read, successfully or not.
    pub last_checked_at: Option<String>,
    /// Last time the hosts matched the list.
    pub last_synced_at: Option<String>,
    /// Why the last check failed; absent after a successful one.
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl SharedSessionSource {
    /// Whether the link is due for its next automatic check at `now_ms` (Unix milliseconds).
    pub fn due(&self, now_ms: i64) -> bool {
        shared_session_source_due(
            self.refresh_minutes,
            self.last_checked_at.as_deref(),
            self.last_error.as_deref(),
            now_ms,
        )
    }
}

/// Whether a link is due for its next automatic check (`sharedSessionSourceDue`).
pub fn shared_session_source_due(
    refresh_minutes: u32,
    last_checked_at: Option<&str>,
    last_error: Option<&str>,
    now_ms: i64,
) -> bool {
    if refresh_minutes == 0 {
        return false;
    }
    let Some(last) = last_checked_at.and_then(crate::js::parse_timestamp) else {
        return true;
    };
    let minutes = match last_error.filter(|error| !error.is_empty()) {
        Some(_) => refresh_minutes.min(SHARED_SESSIONS_RETRY_MINUTES),
        None => refresh_minutes,
    };
    now_ms - last.timestamp_millis() >= i64::from(minutes) * 60_000
}

/// A link to follow, or the new settings of one.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedSessionSourceInput {
    /// Omit to add a link; supply to change one (or to restore it from a backup).
    pub id: Option<String>,
    pub name: String,
    pub url: String,
    pub folder: String,
    pub refresh_minutes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedSessionSourcesResponse {
    pub sources: Vec<SharedSessionSource>,
}

/// What reading a link gave: its text, or nothing because it is not due yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum SharedSessionFetchResponse {
    Fetched { content: String },
    NotDue,
}

/// One host of a shared session list, ready to be stored.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedSessionHost {
    /// Stable for the same entry of the same link, so a host keeps its identity across
    /// checks.
    pub id: String,
    pub name: String,
    pub profile: SavedHostSessionProfile,
    /// Full sidebar folder path.
    pub group: Option<String>,
    pub color: Option<String>,
    pub icon: Option<String>,
}

/// The hosts a link lists now, or why its content could not be used. The hosts are checked
/// one by one when they are applied, so one bad entry is named in the answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum SharedSessionApplyRequest {
    Hosts { hosts: Vec<serde_json::Value> },
    Error { error: String },
}

/// What applying a list changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedSessionApplyResult {
    pub source: SharedSessionSource,
    pub added: u64,
    pub updated: u64,
    pub removed: u64,
    pub unchanged: u64,
}

impl Validate for SharedSessionSourceInput {
    /// `sourceSchema` of the shared sessions route.
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::opt_string(&join(path, "id"), &self.id, 1, 200)?;
        validate::trimmed(&join(path, "name"), &mut self.name, 1, 200)?;
        validate::trimmed(&join(path, "url"), &mut self.url, 1, 4096)?;
        validate::string(&join(path, "folder"), &self.folder, 0, 300)?;
        validate::range(
            &join(path, "refreshMinutes"),
            self.refresh_minutes,
            0,
            MAX_SHARED_SESSIONS_REFRESH_MINUTES,
        )
    }
}

impl Validate for SharedSessionHost {
    /// `hostSchema` of the shared sessions route.
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::string(&join(path, "id"), &self.id, 1, 200)?;
        validate::trimmed(&join(path, "name"), &mut self.name, 1, 200)?;
        self.profile.validate_at(&join(path, "profile"))?;
        validate::opt_string(&join(path, "group"), &self.group, 0, 300)?;
        validate::opt_string(&join(path, "color"), &self.color, 0, 64)?;
        validate::opt_string(&join(path, "icon"), &self.icon, 0, 64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HOUR: i64 = 3_600_000;

    fn at(text: &str) -> i64 {
        crate::js::parse_timestamp(text).unwrap().timestamp_millis()
    }

    #[test]
    fn a_link_is_due_once_its_interval_has_passed() {
        let checked = Some("2026-01-01T10:00:00.000Z");
        let now = at("2026-01-01T10:00:00.000Z");
        assert!(!shared_session_source_due(
            60,
            checked,
            None,
            now + HOUR - 1
        ));
        assert!(shared_session_source_due(60, checked, None, now + HOUR));
        // Never read, or read at a time that is no time: due at once.
        assert!(shared_session_source_due(60, None, None, now));
        assert!(shared_session_source_due(60, Some("yesterday"), None, now));
        // 0 checks only when asked to.
        assert!(!shared_session_source_due(0, None, None, now));
        assert!(!shared_session_source_due(
            0,
            checked,
            None,
            now + 100 * HOUR
        ));
    }

    #[test]
    fn a_link_that_failed_is_tried_again_after_a_quarter_of_an_hour_at_the_latest() {
        let checked = Some("2026-01-01T10:00:00.000Z");
        let now = at("2026-01-01T10:00:00.000Z");
        let failed = Some("portal.example.com answered 503.");
        assert!(!shared_session_source_due(
            60,
            checked,
            failed,
            now + 14 * 60_000
        ));
        assert!(shared_session_source_due(
            60,
            checked,
            failed,
            now + 15 * 60_000
        ));
        // A shorter interval stays the shorter one.
        assert!(shared_session_source_due(
            5,
            checked,
            failed,
            now + 5 * 60_000
        ));
        assert!(!shared_session_source_due(
            5,
            checked,
            failed,
            now + 4 * 60_000
        ));
    }

    #[test]
    fn writes_the_shapes_the_routes_exchange() {
        assert_eq!(
            serde_json::to_value(SharedSessionFetchResponse::NotDue).unwrap(),
            json!({ "status": "not-due" })
        );
        assert_eq!(
            serde_json::to_value(SharedSessionFetchResponse::Fetched {
                content: "[Bookmarks]".into()
            })
            .unwrap(),
            json!({ "status": "fetched", "content": "[Bookmarks]" })
        );
        let hosts: SharedSessionApplyRequest =
            serde_json::from_value(json!({ "hosts": [{ "id": "a" }] })).unwrap();
        assert!(matches!(hosts, SharedSessionApplyRequest::Hosts { hosts } if hosts.len() == 1));
        let failed: SharedSessionApplyRequest =
            serde_json::from_value(json!({ "error": "The link returned nothing." })).unwrap();
        assert!(matches!(failed, SharedSessionApplyRequest::Error { .. }));
        // Neither, or both, is no request at all.
        assert!(serde_json::from_value::<SharedSessionApplyRequest>(json!({})).is_err());
        assert!(
            serde_json::from_value::<SharedSessionApplyRequest>(
                json!({ "hosts": [], "error": "x" })
            )
            .is_err()
        );
        // A link's settings take nothing they do not know.
        assert!(
            serde_json::from_value::<SharedSessionSourceInput>(json!({
                "name": "Lab", "url": "https://x", "folder": "Lab", "refreshMinutes": 60,
                "hostCount": 3,
            }))
            .is_err()
        );
        let source = SharedSessionSource {
            id: "s1".into(),
            name: "Lab".into(),
            url: "https://portal.example.com/lab.mxtsessions".into(),
            folder: "Lab".into(),
            refresh_minutes: 60,
            host_count: 0,
            last_checked_at: None,
            last_synced_at: None,
            last_error: None,
            created_at: "2026-01-01 10:00:00".into(),
            updated_at: "2026-01-01 10:00:00".into(),
        };
        assert_eq!(
            serde_json::to_value(&source).unwrap(),
            json!({
                "id": "s1", "name": "Lab",
                "url": "https://portal.example.com/lab.mxtsessions", "folder": "Lab",
                "refreshMinutes": 60, "hostCount": 0,
                "createdAt": "2026-01-01 10:00:00", "updatedAt": "2026-01-01 10:00:00",
            })
        );
    }
}
