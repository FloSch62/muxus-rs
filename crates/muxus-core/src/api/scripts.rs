//! The script library and script runs (`SavedScript`, `ScriptRun*` in
//! `shared/src/api-types.ts`; limits from `server/src/routes/scripts.ts`).

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::validate::{self, Validate, ValidationError, join};

/// Largest script Muxus stores or runs, in characters (UTF-16 code units).
pub const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
/// Longest per-host time limit for a script run: a week.
pub const MAX_SCRIPT_TIMEOUT_SECONDS: u32 = 7 * 24 * 3600;

/// A script kept in Muxus to run on hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedScript {
    pub id: String,
    pub name: String,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedScriptInput {
    /// Omit to create; supply to replace.
    pub id: Option<String>,
    pub name: String,
    pub body: String,
}

impl Validate for SavedScriptInput {
    /// `scriptSchema` (strict: unknown keys are refused).
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::opt_string(&join(path, "id"), &self.id, 1, 200)?;
        validate::trimmed(&join(path, "name"), &mut self.name, 1, 200)?;
        validate::string(&join(path, "body"), &self.body, 0, MAX_SCRIPT_BYTES)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedScriptsResponse {
    pub scripts: Vec<SavedScript>,
}

/// Run a script on one live SSH connection (POST /api/script-runs).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScriptRunRequest {
    pub conn_id: String,
    pub body: String,
    /// Stop the run after this long; absent or 0 lets it run.
    pub timeout_seconds: Option<u32>,
}

impl Validate for ScriptRunRequest {
    /// `runSchema` (strict).
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::string(&join(path, "connId"), &self.conn_id, 1, 200)?;
        validate::string(&join(path, "body"), &self.body, 1, MAX_SCRIPT_BYTES)?;
        validate::opt_range(
            &join(path, "timeoutSeconds"),
            self.timeout_seconds,
            0,
            MAX_SCRIPT_TIMEOUT_SECONDS,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScriptRunState {
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

/// A run's progress (GET /api/script-runs/:id?since=<offset>).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptRunStatus {
    pub id: String,
    pub state: ScriptRunState,
    pub exit_code: Option<i32>,
    /// Output (stdout and stderr, interleaved) from `since` on.
    pub output: String,
    /// Pass back as `since` to get only what came after.
    pub offset: u64,
    /// The run printed more than Muxus keeps; the start was dropped.
    pub truncated: bool,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::validate::parse_value;

    #[test]
    fn script_bodies_follow_the_route_limits() {
        let script: SavedScriptInput =
            parse_value(json!({ "name": "  Disk space ", "body": "df -h\n" })).unwrap();
        assert_eq!(script.name, "Disk space");
        assert!(parse_value::<SavedScriptInput>(json!({ "name": " ", "body": "" })).is_err());
        assert!(
            parse_value::<SavedScriptInput>(json!({ "name": "x", "body": "", "extra": 1 }))
                .is_err()
        );
        assert!(
            parse_value::<SavedScriptInput>(
                json!({ "name": "x", "body": "a".repeat(MAX_SCRIPT_BYTES + 1) })
            )
            .is_err()
        );
        assert!(parse_value::<ScriptRunRequest>(json!({ "connId": "c", "body": "" })).is_err());
        assert!(
            parse_value::<ScriptRunRequest>(
                json!({ "connId": "c", "body": "id", "timeoutSeconds": MAX_SCRIPT_TIMEOUT_SECONDS + 1 })
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_string(&ScriptRunState::TimedOut).unwrap(),
            r#""timed-out""#
        );
    }
}
