//! Session logging policies, session history limits and log file settings.

use std::path::Path;

use muxus_core::api::{
    SessionHistorySettings, SessionLogFileSettings, SessionLoggingPolicy,
    SessionLoggingPolicyInput, session_log_file_pattern_error,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Database, DbError, DbResult, optional_string, require_non_empty, trimmed_or_none};

/// The policy every host follows until `*` or the host itself overrides it.
pub const DEFAULT_SESSION_LOGGING_POLICY: SessionLoggingPolicyInput = SessionLoggingPolicyInput {
    enabled: false,
    capture_input: false,
    max_part_bytes: 5 * 1024 * 1024,
    max_parts: 10,
    log_to_file: Some(false),
};

const POLICY_COLUMNS: &str = "SELECT enabled, capture_input, max_part_bytes, max_parts, log_to_file
        FROM session_logging_policies WHERE profile_key = ?";

struct PolicyRow {
    enabled: bool,
    capture_input: bool,
    max_part_bytes: u64,
    max_parts: u32,
    log_to_file: bool,
}

fn policy_row(row: &Row<'_>) -> rusqlite::Result<PolicyRow> {
    Ok(PolicyRow {
        enabled: row.get::<_, i64>(0)? == 1,
        capture_input: row.get::<_, i64>(1)? == 1,
        max_part_bytes: row.get::<_, i64>(2)?.max(0) as u64,
        max_parts: row.get::<_, i64>(3)?.clamp(0, i64::from(u32::MAX)) as u32,
        log_to_file: row.get::<_, i64>(4)? == 1,
    })
}

impl Database {
    /// The effective policy for a host key (`ssh:<alias>`, `profile:<id>`, …):
    /// its own row, else the `*` row, else the built-in defaults.
    pub fn session_logging_policy(&self, profile_key: &str) -> DbResult<SessionLoggingPolicy> {
        require_non_empty(profile_key, "profileKey")?;
        let conn = self.lock();
        let exact = conn
            .query_row(POLICY_COLUMNS, [profile_key], policy_row)
            .optional()?;
        let defaults = if profile_key == "*" {
            None
        } else {
            conn.query_row(POLICY_COLUMNS, ["*"], policy_row)
                .optional()?
        };
        let overridden = exact.is_some();
        let row = exact.or(defaults);
        let fallback = DEFAULT_SESSION_LOGGING_POLICY;
        Ok(SessionLoggingPolicy {
            profile_key: profile_key.to_owned(),
            enabled: row.as_ref().map_or(fallback.enabled, |row| row.enabled),
            capture_input: row
                .as_ref()
                .map_or(fallback.capture_input, |row| row.capture_input),
            max_part_bytes: row
                .as_ref()
                .map_or(fallback.max_part_bytes, |row| row.max_part_bytes),
            max_parts: row.as_ref().map_or(fallback.max_parts, |row| row.max_parts),
            log_to_file: row.as_ref().is_some_and(|row| row.log_to_file),
            overridden,
        })
    }

    pub fn save_session_logging_policy(
        &self,
        profile_key: &str,
        input: &SessionLoggingPolicyInput,
    ) -> DbResult<SessionLoggingPolicy> {
        require_non_empty(profile_key, "profileKey")?;
        validate_session_logging_policy(input)?;
        self.lock().execute(
            "
        INSERT INTO session_logging_policies(
          profile_key, enabled, capture_input, max_part_bytes, max_parts, log_to_file
        ) VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT(profile_key) DO UPDATE SET
          enabled = excluded.enabled,
          capture_input = excluded.capture_input,
          max_part_bytes = excluded.max_part_bytes,
          max_parts = excluded.max_parts,
          log_to_file = excluded.log_to_file,
          updated_at = CURRENT_TIMESTAMP
      ",
            params![
                profile_key,
                input.enabled,
                input.capture_input,
                input.max_part_bytes as i64,
                input.max_parts,
                input.log_to_file.unwrap_or(false),
            ],
        )?;
        self.session_logging_policy(profile_key)
    }

    pub fn delete_session_logging_policy(&self, profile_key: &str) -> DbResult<bool> {
        require_non_empty(profile_key, "profileKey")?;
        Ok(self.lock().execute(
            "DELETE FROM session_logging_policies WHERE profile_key = ?",
            [profile_key],
        )? > 0)
    }

    pub fn session_history_settings(&self) -> DbResult<SessionHistorySettings> {
        Ok(self.lock().query_row(
            "
        SELECT storage_location, max_total_bytes, min_free_bytes,
               min_free_percent, max_age_days
        FROM session_history_settings WHERE singleton = 1
      ",
            [],
            |row| {
                Ok(SessionHistorySettings {
                    storage_location: optional_string(row.get(0)?),
                    max_total_bytes: row.get::<_, i64>(1)?.max(0) as u64,
                    min_free_bytes: row.get::<_, i64>(2)?.max(0) as u64,
                    min_free_percent: row.get(3)?,
                    max_age_days: row
                        .get::<_, Option<i64>>(4)?
                        .map(|days| days.clamp(0, i64::from(u32::MAX)) as u32),
                })
            },
        )?)
    }

    pub fn save_session_history_settings(
        &self,
        input: &SessionHistorySettings,
    ) -> DbResult<SessionHistorySettings> {
        validate_session_history_settings(input)?;
        self.lock().execute(
            "
        UPDATE session_history_settings
        SET storage_location = ?,
            max_total_bytes = ?,
            min_free_bytes = ?,
            min_free_percent = ?,
            max_age_days = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE singleton = 1
      ",
            params![
                trimmed_or_none(input.storage_location.as_deref()),
                input.max_total_bytes as i64,
                input.min_free_bytes as i64,
                input.min_free_percent,
                input.max_age_days,
            ],
        )?;
        self.session_history_settings()
    }

    pub fn session_log_file_settings(&self) -> DbResult<SessionLogFileSettings> {
        Ok(self.lock().query_row(
            "
        SELECT directory, filename_pattern, timestamps
        FROM session_log_file_settings WHERE singleton = 1
      ",
            [],
            |row| {
                Ok(SessionLogFileSettings {
                    directory: optional_string(row.get(0)?),
                    filename_pattern: row.get(1)?,
                    timestamps: row.get::<_, i64>(2)? == 1,
                })
            },
        )?)
    }

    pub fn save_session_log_file_settings(
        &self,
        input: &SessionLogFileSettings,
    ) -> DbResult<SessionLogFileSettings> {
        validate_session_log_file_settings(input)?;
        self.lock().execute(
            "
        UPDATE session_log_file_settings
        SET directory = ?,
            filename_pattern = ?,
            timestamps = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE singleton = 1
      ",
            params![
                trimmed_or_none(input.directory.as_deref()),
                muxus_core::js::js_trim(&input.filename_pattern),
                input.timestamps,
            ],
        )?;
        self.session_log_file_settings()
    }

    /// After the history store has imported version-6 history, remove every
    /// payload row and history-only table from the application database. A
    /// one-time VACUUM reclaims old BLOB pages when an upgrade actually
    /// imported payloads.
    pub fn finalize_session_history_separation(&self, compact: bool) -> DbResult<()> {
        let conn = self.lock();
        conn.execute_batch(
            "
      DROP TRIGGER IF EXISTS session_log_events_fts_insert;
      DROP TRIGGER IF EXISTS session_log_events_fts_delete;
      DROP TRIGGER IF EXISTS session_log_events_fts_update;
      DROP TABLE IF EXISTS session_log_events_fts;
      DROP TABLE IF EXISTS session_log_events;
      DROP TABLE IF EXISTS session_logs;
      PRAGMA wal_checkpoint(TRUNCATE);
    ",
        )?;
        if compact {
            conn.execute_batch("VACUUM")?;
        }
        Ok(())
    }

    /// Whether the version-6 history tables still hold sessions to import.
    pub fn has_legacy_session_history(&self) -> DbResult<bool> {
        let conn = self.lock();
        let table = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'session_logs'",
                [],
                |_| Ok(()),
            )
            .optional()?;
        if table.is_none() {
            return Ok(false);
        }
        Ok(conn
            .query_row("SELECT 1 FROM session_logs LIMIT 1", [], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Carry a host's policy over to its new key (an alias rename).
    pub(super) fn move_session_logging_policy(
        conn: &Connection,
        previous: &str,
        next: &str,
    ) -> DbResult<()> {
        conn.execute(
            "
        INSERT INTO session_logging_policies(
          profile_key, enabled, capture_input, max_part_bytes, max_parts, log_to_file
        )
        SELECT ?, enabled, capture_input, max_part_bytes, max_parts, log_to_file
        FROM session_logging_policies
        WHERE profile_key = ?
        ON CONFLICT(profile_key) DO UPDATE SET
          enabled = excluded.enabled,
          capture_input = excluded.capture_input,
          max_part_bytes = excluded.max_part_bytes,
          max_parts = excluded.max_parts,
          log_to_file = excluded.log_to_file,
          updated_at = CURRENT_TIMESTAMP
      ",
            params![next, previous],
        )?;
        conn.execute(
            "DELETE FROM session_logging_policies WHERE profile_key = ?",
            [previous],
        )?;
        Ok(())
    }
}

fn validate_session_logging_policy(input: &SessionLoggingPolicyInput) -> DbResult<()> {
    if input.max_part_bytes < 64 * 1024 || input.max_part_bytes > 1024 * 1024 * 1024 {
        return Err(DbError::Invalid(
            "maxPartBytes must be between 64 KiB and 1 GiB".into(),
        ));
    }
    if input.max_parts < 1 || input.max_parts > 1000 {
        return Err(DbError::Invalid(
            "maxParts must be between 1 and 1000".into(),
        ));
    }
    Ok(())
}

/// Whether a path is the root of its file system (`/`, `C:\`).
fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

fn validate_session_history_settings(input: &SessionHistorySettings) -> DbResult<()> {
    if let Some(location) = input
        .storage_location
        .as_deref()
        .filter(|location| !location.is_empty())
    {
        let path = Path::new(location);
        if !path.is_absolute() {
            return Err(DbError::Invalid(
                "storageLocation must be an absolute path".into(),
            ));
        }
        if is_filesystem_root(&normalize(path)) {
            return Err(DbError::Invalid(
                "storageLocation cannot be the filesystem root".into(),
            ));
        }
    }
    if input.max_total_bytes < 64 * 1024 * 1024 || input.max_total_bytes > MAX_SAFE_INTEGER {
        return Err(DbError::Invalid(
            "maxTotalBytes must be at least 64 MiB".into(),
        ));
    }
    if input.min_free_bytes > MAX_SAFE_INTEGER {
        return Err(DbError::Invalid(
            "minFreeBytes must be a non-negative integer".into(),
        ));
    }
    if !input.min_free_percent.is_finite()
        || input.min_free_percent < 0.0
        || input.min_free_percent > 100.0
    {
        return Err(DbError::Invalid(
            "minFreePercent must be between 0 and 100".into(),
        ));
    }
    if input.max_age_days == Some(0) {
        return Err(DbError::Invalid(
            "maxAgeDays must be a positive integer when enabled".into(),
        ));
    }
    Ok(())
}

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// `path.resolve` without touching the file system: drop `.` and fold `..`.
fn normalize(path: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn validate_session_log_file_settings(input: &SessionLogFileSettings) -> DbResult<()> {
    if let Some(directory) = trimmed_or_none(input.directory.as_deref())
        && !Path::new(&directory).is_absolute()
    {
        return Err(DbError::Invalid(
            "directory must be an absolute path".into(),
        ));
    }
    if let Some(error) = session_log_file_pattern_error(&input.filename_pattern) {
        return Err(DbError::Invalid(error));
    }
    Ok(())
}
