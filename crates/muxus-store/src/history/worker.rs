//! The history worker: owns the history database and the open segment
//! writers, and runs every operation in the order it was requested (port of
//! `history-worker.js`). Filesystem, compression, SQLite, quota and search
//! work all happen on its thread; the facade only copies bounded batches.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use muxus_core::api::{
    SessionEndStatus, SessionHistoryResponse, SessionHistorySettings, SessionHistoryStorageStatus,
    SessionLineTimestamp, SessionLogCreateInput, SessionLogDetail, SessionLogDirection,
    SessionLogEvent, SessionLogStatus, SessionLogSummary,
};
use muxus_core::js;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params, params_from_iter};

use super::disk::{DiskStatus, disk_status};
use super::schema;
use super::search::{decode_cursor, encode_cursor, escape_like, fts_expression, sql_string};
use super::segment::{SEGMENT_HEADER, decode_segment, encode_frame, part_name, safe_session_name};
use super::{HistoryEvent, PartPolicy, SessionHistoryQuery, resolve_path};

/// Re-check quota after this many newly retained bytes…
const QUOTA_CHECK_BYTES: u64 = 1024 * 1024;
/// …or after this long, whichever comes first.
const QUOTA_CHECK_INTERVAL: Duration = Duration::from_millis(2_000);
/// Leading context retained before the first match when anchoring a preview.
const MATCH_CONTEXT_CHUNKS: usize = 50;
/// How often idle retention and WAL maintenance run.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30 * 60);

const QUOTA_EXHAUSTED: &str = "session history quota is exhausted";
const QUOTA_WARNING: &str = "Session logging suspended: history quota or minimum free-space reserve is exhausted. \
                             Unpin/delete history or raise the storage limit.";

/// A failed operation; its message reaches the caller unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure(pub String);

impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        Failure(error.to_string())
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Failure(error.to_string())
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure(message)
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Failure(message.to_owned())
    }
}

pub(crate) type Outcome<T> = Result<T, Failure>;

/// One request for the worker thread.
pub(crate) type Job = Box<dyn FnOnce(&mut Worker) + Send>;

/// The active part of one recording session.
struct Writer {
    part: u32,
    file: File,
    partial_path: PathBuf,
    raw_bytes: u64,
    normalized_bytes: u64,
    event_count: u64,
    first_sequence: Option<u64>,
    last_sequence: Option<u64>,
}

pub(crate) struct Worker {
    root: PathBuf,
    database_file: PathBuf,
    sessions_root: PathBuf,
    trash_root: PathBuf,
    db: Connection,
    settings: SessionHistorySettings,
    writers: HashMap<String, Writer>,
    bytes_since_quota_check: u64,
    last_quota_check: Option<Instant>,
    quota_suspended: bool,
    quota_warning: Option<String>,
    closed: bool,
}

impl Worker {
    /// Open (or create) the history database under `root`, bring its schema up
    /// to date, clear the trash, import a version-6 application database's
    /// history when given one, and close sessions a crash left active.
    pub(crate) fn open(
        root: PathBuf,
        settings: SessionHistorySettings,
        legacy_database: Option<PathBuf>,
    ) -> Outcome<Self> {
        let database_file = root.join("session-history.sqlite");
        let sessions_root = root.join("sessions");
        let trash_root = root.join(".trash");
        create_private_dir_all(&sessions_root)?;
        create_private_dir_all(&trash_root)?;
        let db = Connection::open(&database_file)?;
        // Some filesystems own permission policy.
        let _ = set_private_file(&database_file);
        db.execute_batch(schema::SCHEMA)?;
        // Existing recordings keep NULL metadata and use their event time as a fallback.
        if !has_column(&db, "transcript_chunks", "line_timestamps")? {
            db.execute_batch(schema::ADD_LINE_TIMESTAMPS)?;
        }
        db.execute_batch(schema::RECORD_LINE_TIMESTAMPS)?;
        // A user-chosen name shown instead of the recorded title; NULL keeps the title.
        if !has_column(&db, "session_logs", "label")? {
            db.execute_batch(schema::ADD_LABEL)?;
        }
        db.execute_batch(schema::RECORD_LABEL)?;

        let mut worker = Worker {
            root,
            database_file,
            sessions_root,
            trash_root,
            db,
            settings,
            writers: HashMap::new(),
            bytes_since_quota_check: 0,
            last_quota_check: None,
            quota_suspended: false,
            quota_warning: None,
            closed: false,
        };
        worker.remove_trash()?;
        if let Some(legacy) = legacy_database.filter(|path| path.as_os_str() != ":memory:") {
            worker.import_legacy_history(&legacy)?;
        }
        worker.recover_interrupted_sessions()?;
        Ok(worker)
    }

    /// Process requests in order until the store closes or goes away.
    ///
    /// The first retention pass stats every file under the history root,
    /// which is thousands of files on a long-used install, and the app waits
    /// for the store to be ready. So it runs straight after the first request
    /// is answered instead; requests are handled in order, so every later one
    /// still sees its result.
    pub(crate) fn run(mut self, jobs: mpsc::Receiver<Job>) {
        let mut startup_retention_pending = true;
        let mut next_maintenance = Instant::now() + MAINTENANCE_INTERVAL;
        loop {
            let wait = next_maintenance.saturating_duration_since(Instant::now());
            match jobs.recv_timeout(wait) {
                Ok(job) => {
                    job(&mut self);
                    if self.closed {
                        return;
                    }
                    if startup_retention_pending {
                        startup_retention_pending = false;
                        // A later write/status request surfaces persistent storage failures.
                        let _ = self.enforce_retention(true);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = self.enforce_retention(true);
                    let _ = self
                        .db
                        .execute_batch("PRAGMA wal_checkpoint(PASSIVE); PRAGMA optimize;");
                    next_maintenance = Instant::now() + MAINTENANCE_INTERVAL;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = self.close();
                    return;
                }
            }
        }
    }

    pub(crate) fn begin_session(
        &mut self,
        id: &str,
        input: &SessionLogCreateInput,
        policy: PartPolicy,
    ) -> Outcome<bool> {
        let _ = policy;
        self.enforce_retention(false)?;
        if self.quota_suspended {
            return Err(self.quota_failure());
        }
        self.db.execute(
            "
    INSERT INTO session_logs(
      id, profile_key, title, kind, host, started_at, status, capture_input
    ) VALUES (?, ?, ?, ?, ?, ?, 'active', ?)
  ",
            params![
                id,
                input.profile_key,
                input.title,
                input.kind.as_str(),
                input.host,
                input.started_at,
                i64::from(input.capture_input)
            ],
        )?;
        let writer = self.open_writer(id, 1)?;
        self.writers.insert(id.to_owned(), writer);
        Ok(true)
    }

    pub(crate) fn append(
        &mut self,
        session_id: &str,
        events: &[HistoryEvent],
        policy: PartPolicy,
    ) -> Outcome<bool> {
        if self.quota_suspended {
            return Err(self.quota_failure());
        }
        let session: Option<(String, i64)> = self
            .db
            .query_row(
                "SELECT status, current_part, current_part_bytes FROM session_logs WHERE id = ?",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((status, current_part)) = session else {
            return Ok(false);
        };
        if status != "active" {
            return Ok(false);
        }
        if !self.writers.contains_key(session_id) {
            let writer = self.open_writer(session_id, current_part.max(1) as u32)?;
            self.writers.insert(session_id.to_owned(), writer);
        }
        let mut parts = Vec::with_capacity(events.len());
        let mut raw_bytes = 0u64;
        let mut normalized_bytes = 0u64;
        for event in events {
            let frame = encode_frame(
                event.sequence,
                &event.recorded_at,
                event.elapsed_ms,
                event.direction,
                &event.raw,
            );
            let (count, part_raw, part) = {
                let writer = &self.writers[session_id];
                (writer.event_count, writer.raw_bytes, writer.part)
            };
            if count > 0 && part_raw + event.raw.len() as u64 > policy.max_part_bytes {
                self.finalize_writer(session_id)?;
                let writer = self.open_writer(session_id, part + 1)?;
                self.writers.insert(session_id.to_owned(), writer);
                self.trim_session_parts(session_id, policy.max_parts)?;
            }
            let writer = self.writers.get_mut(session_id).expect("writer is open");
            writer.file.write_all(&frame)?;
            writer.raw_bytes += event.raw.len() as u64;
            writer.event_count += 1;
            writer.first_sequence.get_or_insert(event.sequence);
            writer.last_sequence = Some(event.sequence);
            raw_bytes += event.raw.len() as u64;
            let text_bytes = event.text.len() as u64;
            writer.normalized_bytes += text_bytes;
            normalized_bytes += text_bytes;
            parts.push(writer.part);
        }
        let (part, part_bytes) = {
            let writer = &self.writers[session_id];
            (writer.part, writer.raw_bytes)
        };
        // One SQLite transaction per recorder batch (normally every 250 ms).
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut insert = tx.prepare(
                "
      INSERT INTO transcript_chunks(
        session_id, part_number, first_sequence, last_sequence,
        recorded_at, elapsed_ms, direction, normalized_text, line_timestamps
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
    ",
            )?;
            for (event, part) in events.iter().zip(&parts) {
                if event.text.is_empty() {
                    continue;
                }
                let line_timestamps = match &event.line_timestamps {
                    Some(stamps) if !stamps.is_empty() => Some(
                        serde_json::to_string(stamps)
                            .map_err(|error| Failure(error.to_string()))?,
                    ),
                    _ => None,
                };
                insert.execute(params![
                    session_id,
                    part,
                    event.sequence as i64,
                    event.sequence as i64,
                    event.recorded_at,
                    event.elapsed_ms as i64,
                    event.direction.as_str(),
                    event.text,
                    line_timestamps
                ])?;
            }
            tx.execute(
                "
      UPDATE session_logs
      SET current_part = ?,
          current_part_bytes = ?,
          event_count = event_count + ?,
          raw_bytes = raw_bytes + ?,
          normalized_bytes = normalized_bytes + ?
      WHERE id = ? AND status = 'active'
    ",
                params![
                    part,
                    part_bytes as i64,
                    events.len() as i64,
                    raw_bytes as i64,
                    normalized_bytes as i64,
                    session_id
                ],
            )?;
        }
        tx.commit()?;
        self.bytes_since_quota_check += raw_bytes + normalized_bytes;
        if self.bytes_since_quota_check >= QUOTA_CHECK_BYTES || self.quota_check_due() {
            self.enforce_retention(false)?;
        }
        if self.quota_suspended {
            return Err(Failure(self.quota_warning.clone().unwrap_or_default()));
        }
        Ok(true)
    }

    pub(crate) fn set_session_state(
        &mut self,
        session_id: &str,
        paused: Option<bool>,
        capture_input: Option<bool>,
    ) -> Outcome<bool> {
        let row: Option<(i64, i64)> = self
            .db
            .query_row(
                "SELECT paused, capture_input FROM session_logs WHERE id = ?",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((current_paused, current_capture)) = row else {
            return Ok(false);
        };
        self.db.execute(
            "UPDATE session_logs SET paused = ?, capture_input = ? WHERE id = ?",
            params![
                paused.map_or(current_paused, i64::from),
                capture_input.map_or(current_capture, i64::from),
                session_id
            ],
        )?;
        Ok(true)
    }

    pub(crate) fn finish_session(
        &mut self,
        session_id: &str,
        status: SessionEndStatus,
        ended_at: &str,
    ) -> Outcome<bool> {
        self.finalize_writer(session_id)?;
        let status = SessionLogStatus::from(status);
        self.db.execute(
            "
    UPDATE session_logs
    SET status = ?, ended_at = ?, paused = 0, current_part_bytes = 0
    WHERE id = ? AND status = 'active'
  ",
            params![status.as_str(), ended_at, session_id],
        )?;
        self.enforce_retention(true)?;
        Ok(true)
    }

    pub(crate) fn search(
        &mut self,
        input: &SessionHistoryQuery,
    ) -> Outcome<SessionHistoryResponse> {
        let limit = if input.limit == 0 {
            50
        } else {
            input.limit.clamp(1, 100)
        } as usize;
        let mut args: Vec<Value> = Vec::new();
        let mut filters: Vec<&str> = Vec::new();
        let mut with_sql = "";
        let mut join_sql = "";
        let mut select_snippet = "NULL AS snippet, NULL AS match_count";
        if let Some(expression) = fts_expression(input.query.as_deref()) {
            with_sql = "
      WITH event_matches AS (
        SELECT chunks.session_id, MIN(chunks.id) AS match_id, COUNT(*) AS match_count
        FROM transcript_chunks_fts
        JOIN transcript_chunks AS chunks ON chunks.id = transcript_chunks_fts.rowid
        WHERE transcript_chunks_fts MATCH ?
        GROUP BY chunks.session_id
      )
    ";
            args.push(Value::Text(expression.clone()));
            join_sql = "
      LEFT JOIN event_matches ON event_matches.session_id = logs.id
    ";
            // snippet() centers the excerpt on the matched tokens; char(1)/char(2)
            // delimit the highlight so the UI never parses transcript text as markup.
            // Placeholders bind in textual order: CTE match, snippet match, then filters.
            select_snippet = "(
      SELECT snippet(transcript_chunks_fts, 0, char(1), char(2), '…', 16)
      FROM transcript_chunks_fts
      WHERE transcript_chunks_fts.rowid = event_matches.match_id
        AND transcript_chunks_fts MATCH ?
    ) AS snippet, event_matches.match_count AS match_count";
            args.push(Value::Text(expression));
            let metadata = format!(
                "%{}%",
                escape_like(js::js_trim(input.query.as_deref().unwrap_or_default()))
            );
            filters.push(
                "(
      event_matches.session_id IS NOT NULL
      OR logs.title LIKE ? ESCAPE '\\' COLLATE NOCASE
      OR logs.label LIKE ? ESCAPE '\\' COLLATE NOCASE
      OR logs.host LIKE ? ESCAPE '\\' COLLATE NOCASE
    )",
            );
            args.push(Value::Text(metadata.clone()));
            args.push(Value::Text(metadata.clone()));
            args.push(Value::Text(metadata));
        }
        if let Some(profile_key) = non_empty(&input.profile_key) {
            filters.push("logs.profile_key = ?");
            args.push(Value::Text(profile_key.to_owned()));
        }
        if let Some(host) = non_empty(&input.host) {
            filters.push("logs.host LIKE ? ESCAPE '\\' COLLATE NOCASE");
            args.push(Value::Text(format!("%{}%", escape_like(host))));
        }
        if let Some(kind) = non_empty(&input.kind) {
            filters.push("logs.kind = ?");
            args.push(Value::Text(kind.to_owned()));
        }
        if let Some(after) = non_empty(&input.started_after) {
            filters.push("logs.started_at >= ?");
            args.push(Value::Text(after.to_owned()));
        }
        if let Some(before) = non_empty(&input.started_before) {
            filters.push("logs.started_at <= ?");
            args.push(Value::Text(before.to_owned()));
        }
        if let Some((started_at, id)) = decode_cursor(input.cursor.as_deref()) {
            filters.push("(logs.started_at < ? OR (logs.started_at = ? AND logs.id < ?))");
            args.push(Value::Text(started_at.clone()));
            args.push(Value::Text(started_at));
            args.push(Value::Text(id));
        }
        let where_sql = if filters.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", filters.join(" AND "))
        };
        args.push(Value::Integer(limit as i64 + 1));
        let sql = format!(
            "
    {with_sql}
    SELECT logs.*, {select_snippet}, (
      SELECT COUNT(*) FROM session_parts AS parts WHERE parts.session_id = logs.id
    ) AS retained_part_count
    FROM session_logs AS logs
    {join_sql}
    {where_sql}
    ORDER BY logs.started_at DESC, logs.id DESC
    LIMIT ?
  "
        );
        let mut statement = self.db.prepare(&sql)?;
        let mut sessions = statement
            .query_map(params_from_iter(args), summary_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        let next_cursor = match sessions.last() {
            Some(last) if has_more => Some(encode_cursor(&last.started_at, &last.id)),
            _ => None,
        };
        Ok(SessionHistoryResponse {
            sessions,
            next_cursor,
        })
    }

    pub(crate) fn detail(
        &mut self,
        id: &str,
        event_limit: Option<usize>,
        match_query: Option<&str>,
    ) -> Outcome<Option<SessionLogDetail>> {
        let summary = self
            .db
            .query_row(
                "
    SELECT logs.*, NULL AS snippet, NULL AS match_count, (
      SELECT COUNT(*) FROM session_parts AS parts WHERE parts.session_id = logs.id
    ) AS retained_part_count
    FROM session_logs AS logs WHERE logs.id = ?
  ",
                [id],
                summary_from_row,
            )
            .optional()?;
        let Some(summary) = summary else {
            return Ok(None);
        };
        let total_chunks: i64 = self.db.query_row(
            "SELECT COUNT(*) AS total FROM transcript_chunks WHERE session_id = ?",
            [id],
            |row| row.get(0),
        )?;
        let total_chunks = total_chunks.max(0) as usize;
        let window_start = match event_limit {
            Some(limit) => self.match_window_start(id, match_query, limit, total_chunks)?,
            None => None,
        };
        const COLUMNS: &str =
            "first_sequence, recorded_at, elapsed_ms, direction, normalized_text, line_timestamps";
        let (sql, args): (String, Vec<Value>) = match (window_start, event_limit) {
            (Some(start), Some(limit)) => (
                format!(
                    "
        SELECT {COLUMNS}
        FROM transcript_chunks WHERE session_id = ? AND first_sequence >= ?
        ORDER BY first_sequence LIMIT ?
      "
                ),
                vec![
                    Value::Text(id.to_owned()),
                    Value::Integer(start),
                    Value::Integer(limit as i64),
                ],
            ),
            (None, Some(limit)) => (
                format!(
                    "
          SELECT * FROM (
            SELECT {COLUMNS}
            FROM transcript_chunks WHERE session_id = ?
            ORDER BY first_sequence DESC LIMIT ?
          ) ORDER BY first_sequence
        "
                ),
                vec![Value::Text(id.to_owned()), Value::Integer(limit as i64)],
            ),
            _ => (
                format!(
                    "
          SELECT {COLUMNS}
          FROM transcript_chunks WHERE session_id = ? ORDER BY first_sequence
        "
                ),
                vec![Value::Text(id.to_owned())],
            ),
        };
        let mut statement = self.db.prepare(&sql)?;
        let events = statement
            .query_map(params_from_iter(args), |row| {
                let direction: String = row.get("direction")?;
                let line_timestamps: Option<String> = row.get("line_timestamps")?;
                Ok((
                    row.get::<_, i64>("first_sequence")?,
                    row.get::<_, String>("recorded_at")?,
                    row.get::<_, i64>("elapsed_ms")?,
                    direction,
                    row.get::<_, String>("normalized_text")?,
                    line_timestamps,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let events = events
            .into_iter()
            .map(
                |(sequence, recorded_at, elapsed_ms, direction, text, line_timestamps)| {
                    let direction = SessionLogDirection::parse(&direction)
                        .ok_or_else(|| Failure(format!("invalid event direction: {direction}")))?;
                    let line_timestamps = match line_timestamps.filter(|value| !value.is_empty()) {
                        Some(json) => Some(
                            serde_json::from_str::<Vec<SessionLineTimestamp>>(&json)
                                .map_err(|error| Failure(error.to_string()))?,
                        ),
                        None => None,
                    };
                    Ok(SessionLogEvent {
                        sequence: sequence.max(0) as u64,
                        recorded_at,
                        elapsed_ms: elapsed_ms.max(0) as u64,
                        direction,
                        text,
                        line_timestamps,
                    })
                },
            )
            .collect::<Outcome<Vec<_>>>()?;
        let events_truncated = events.len() < total_chunks;
        Ok(Some(SessionLogDetail {
            summary,
            events,
            events_truncated,
        }))
    }

    /// When a preview is opened from a search hit, anchor the limited event
    /// window on the first matching chunk (with some leading context) instead
    /// of the newest events, so the match is actually visible in the preview.
    /// Returns the first_sequence to start from, or `None` to keep the
    /// default newest-events window.
    fn match_window_start(
        &self,
        session_id: &str,
        match_query: Option<&str>,
        event_limit: usize,
        total_chunks: usize,
    ) -> Outcome<Option<i64>> {
        if event_limit == 0 || total_chunks <= event_limit {
            return Ok(None);
        }
        let Some(expression) = fts_expression(match_query) else {
            return Ok(None);
        };
        let matched: Option<i64> = self.db.query_row(
            "
    SELECT MIN(chunks.first_sequence) AS seq
    FROM transcript_chunks_fts
    JOIN transcript_chunks AS chunks ON chunks.id = transcript_chunks_fts.rowid
    WHERE transcript_chunks_fts MATCH ? AND chunks.session_id = ?
  ",
            params![expression, session_id],
            |row| row.get(0),
        )?;
        let Some(match_sequence) = matched else {
            return Ok(None);
        };
        let newest_window: Option<i64> = self.db.query_row(
            "
    SELECT MIN(first_sequence) AS seq FROM (
      SELECT first_sequence FROM transcript_chunks
      WHERE session_id = ? ORDER BY first_sequence DESC LIMIT ?
    )
  ",
            params![session_id, event_limit as i64],
            |row| row.get(0),
        )?;
        if let Some(newest) = newest_window
            && match_sequence >= newest
        {
            return Ok(None);
        }
        // Context is capped at half the window so the match itself always fits.
        let context_rows = MATCH_CONTEXT_CHUNKS.min(event_limit / 2);
        if context_rows < 1 {
            return Ok(Some(match_sequence));
        }
        let context: Option<i64> = self
            .db
            .query_row(
                "
    SELECT first_sequence FROM transcript_chunks
    WHERE session_id = ? AND first_sequence < ?
    ORDER BY first_sequence DESC LIMIT 1 OFFSET ?
  ",
                params![session_id, match_sequence, context_rows as i64 - 1],
                |row| row.get(0),
            )
            .optional()?;
        Ok(Some(context.unwrap_or(0)))
    }

    pub(crate) fn raw_events(&mut self, id: &str) -> Outcome<Option<Vec<HistoryEvent>>> {
        let known: Option<String> = self
            .db
            .query_row("SELECT id FROM session_logs WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .optional()?;
        if known.is_none() {
            return Ok(None);
        }
        let mut events = Vec::new();
        let parts: Vec<String> = {
            let mut statement = self
                .db
                .prepare("SELECT part_number, filename FROM session_parts WHERE session_id = ? ORDER BY part_number")?;
            statement
                .query_map([id], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<_>>()?
        };
        for filename in parts {
            let file = self.root.join(&filename);
            if !file.exists() {
                continue;
            }
            let compressed = fs::read(&file)?;
            let raw = zstd::decode_all(compressed.as_slice())?;
            events.extend(decode_segment(&raw)?.events);
        }
        if let Some(writer) = self.writers.get(id) {
            writer.file.sync_data()?;
            let decoded = decode_segment(&fs::read(&writer.partial_path)?)?;
            let after = events
                .last()
                .map_or(0, |event: &HistoryEvent| event.sequence);
            events.extend(
                decoded
                    .events
                    .into_iter()
                    .filter(|event| event.sequence > after),
            );
        }
        events.sort_by_key(|event| event.sequence);
        Ok(Some(events))
    }

    pub(crate) fn set_pinned(&mut self, id: &str, pinned: bool) -> Outcome<bool> {
        let changed = self.db.execute(
            "UPDATE session_logs SET pinned = ? WHERE id = ?",
            params![i64::from(pinned), id],
        )?;
        if !pinned {
            self.enforce_retention(true)?;
        }
        Ok(changed > 0)
    }

    /// An empty label clears the name.
    pub(crate) fn set_label(&mut self, id: &str, label: &str) -> Outcome<bool> {
        let label = (!label.is_empty()).then_some(label);
        Ok(self.db.execute(
            "UPDATE session_logs SET label = ? WHERE id = ?",
            params![label, id],
        )? > 0)
    }

    /// Finished records keep the title they were recorded under.
    pub(crate) fn set_title(&mut self, session_id: &str, title: &str) -> Outcome<bool> {
        Ok(self.db.execute(
            "UPDATE session_logs SET title = ? WHERE id = ? AND status = 'active'",
            params![title, session_id],
        )? > 0)
    }

    pub(crate) fn update_settings(&mut self, settings: SessionHistorySettings) -> Outcome<bool> {
        self.settings = settings;
        self.enforce_retention(true)?;
        Ok(true)
    }

    fn open_writer(&mut self, session_id: &str, part: u32) -> Outcome<Writer> {
        let directory = self.session_directory(session_id);
        create_private_dir_all(&directory)?;
        let partial_path = directory.join(format!("{}.muxlog.partial", part_name(part)));
        let exists = partial_path.exists();
        let mut file = if exists {
            fs::OpenOptions::new().append(true).open(&partial_path)?
        } else {
            create_private_file(&partial_path)?
        };
        if !exists {
            file.write_all(&SEGMENT_HEADER)?;
        }
        Ok(Writer {
            part,
            file,
            partial_path,
            raw_bytes: 0,
            normalized_bytes: 0,
            event_count: 0,
            first_sequence: None,
            last_sequence: None,
        })
    }

    fn finalize_writer(&mut self, session_id: &str) -> Outcome<()> {
        let Some(writer) = self.writers.remove(session_id) else {
            return Ok(());
        };
        writer.file.sync_data()?;
        drop(writer.file);
        if writer.event_count == 0 {
            remove_file_if_exists(&writer.partial_path)?;
            return Ok(());
        }
        let relative = self.finalize_partial(session_id, writer.part, &writer.partial_path)?;
        let stored_bytes = fs::metadata(self.root.join(&relative))?.len();
        self.db.execute(
            "
    INSERT INTO session_parts(
      session_id, part_number, filename, raw_bytes, stored_bytes,
      event_count, normalized_bytes, first_sequence, last_sequence
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(session_id, part_number) DO UPDATE SET
      filename = excluded.filename,
      raw_bytes = excluded.raw_bytes,
      stored_bytes = excluded.stored_bytes,
      event_count = excluded.event_count,
      normalized_bytes = excluded.normalized_bytes,
      first_sequence = excluded.first_sequence,
      last_sequence = excluded.last_sequence
  ",
            params![
                session_id,
                writer.part,
                relative,
                writer.raw_bytes as i64,
                stored_bytes as i64,
                writer.event_count as i64,
                writer.normalized_bytes as i64,
                writer.first_sequence.map(|value| value as i64),
                writer.last_sequence.map(|value| value as i64)
            ],
        )?;
        Ok(())
    }

    /// Cut a partial segment back to its last complete frame, compress it to
    /// `NNNNNN.muxlog.zst` (written aside and renamed into place) and remove
    /// the partial file. Returns the part's path relative to the root.
    fn finalize_partial(
        &self,
        session_id: &str,
        part: u32,
        partial_path: &Path,
    ) -> Outcome<String> {
        let raw = fs::read(partial_path)?;
        let decoded = decode_segment(&raw)?;
        if decoded.valid_end < raw.len() {
            fs::OpenOptions::new()
                .write(true)
                .open(partial_path)?
                .set_len(decoded.valid_end as u64)?;
        }
        let compressed = zstd::encode_all(&raw[..decoded.valid_end], 3)?;
        let final_path = self
            .session_directory(session_id)
            .join(format!("{}.muxlog.zst", part_name(part)));
        let mut temporary = final_path.clone().into_os_string();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        {
            let mut file = create_private_file_truncating(&temporary)?;
            file.write_all(&compressed)?;
            file.sync_all()?;
        }
        fs::rename(&temporary, &final_path)?;
        remove_file_if_exists(partial_path)?;
        Ok(self.relative(&final_path))
    }

    fn trim_session_parts(&mut self, session_id: &str, max_parts: u32) -> Outcome<()> {
        let mut parts: Vec<(i64, String, i64, i64, i64)> = {
            let mut statement = self.db.prepare(
                "
      SELECT part_number, filename, raw_bytes, event_count, normalized_bytes
      FROM session_parts WHERE session_id = ? ORDER BY part_number
    ",
            )?;
            statement
                .query_map([session_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?
        };
        // Reserve one slot for the newly opened active part.
        while parts.len() >= max_parts as usize && !parts.is_empty() {
            let (part_number, filename, raw_bytes, event_count, normalized_bytes) = parts.remove(0);
            remove_file_if_exists(&self.root.join(&filename))?;
            let tx = self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "DELETE FROM transcript_chunks WHERE session_id = ? AND part_number = ?",
                params![session_id, part_number],
            )?;
            tx.execute(
                "DELETE FROM session_parts WHERE session_id = ? AND part_number = ?",
                params![session_id, part_number],
            )?;
            tx.execute(
                "
        UPDATE session_logs
        SET raw_bytes = MAX(0, raw_bytes - ?),
            event_count = MAX(0, event_count - ?),
            normalized_bytes = MAX(0, normalized_bytes - ?)
        WHERE id = ?
      ",
                params![raw_bytes, event_count, normalized_bytes, session_id],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Delete a finished session and its files; active sessions are never
    /// deleted. Quota deletions skip the checkpoint and the follow-up pass,
    /// which their caller runs itself.
    pub(crate) fn delete_session(&mut self, id: &str, quota_delete: bool) -> Outcome<bool> {
        let status: Option<String> = self
            .db
            .query_row(
                "SELECT status FROM session_logs WHERE id = ?",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        match status.as_deref() {
            None | Some("active") => return Ok(false),
            _ => {}
        }
        let directory = self.session_directory(id);
        if directory.exists() {
            let trash = self.trash_root.join(format!(
                "{}-{}",
                safe_session_name(id),
                chrono::Utc::now().timestamp_millis()
            ));
            fs::rename(&directory, &trash)?;
            self.db.execute(
                "DELETE FROM session_logs WHERE id = ? AND status <> ?",
                params![id, "active"],
            )?;
            remove_dir_all_if_exists(&trash)?;
        } else {
            self.db.execute(
                "DELETE FROM session_logs WHERE id = ? AND status <> ?",
                params![id, "active"],
            )?;
        }
        if !quota_delete {
            self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            self.enforce_retention(false)?;
        }
        Ok(true)
    }

    fn quota_check_due(&self) -> bool {
        self.last_quota_check
            .is_none_or(|last| last.elapsed() >= QUOTA_CHECK_INTERVAL)
    }

    fn quota_failure(&self) -> Failure {
        Failure(
            self.quota_warning
                .clone()
                .unwrap_or_else(|| QUOTA_EXHAUSTED.to_owned()),
        )
    }

    /// Age expiry, then quota and free-space cleanup of the oldest finished,
    /// unpinned sessions; suspends logging when that is not enough. Unless
    /// forced, runs at most every [`QUOTA_CHECK_BYTES`] or 2 seconds.
    pub(crate) fn enforce_retention(&mut self, force: bool) -> Outcome<()> {
        if !force && self.bytes_since_quota_check < QUOTA_CHECK_BYTES && !self.quota_check_due() {
            return Ok(());
        }
        self.bytes_since_quota_check = 0;
        self.last_quota_check = Some(Instant::now());

        if let Some(days) = self.settings.max_age_days.filter(|days| *days > 0) {
            let cutoff =
                js::iso_timestamp(chrono::Utc::now() - chrono::Duration::days(i64::from(days)));
            let expired: Vec<String> = {
                let mut statement = self.db.prepare(
                    "
      SELECT id FROM session_logs
      WHERE pinned = 0 AND status <> 'active' AND started_at < ?
      ORDER BY started_at
    ",
                )?;
                statement
                    .query_map([cutoff], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            for id in expired {
                self.delete_session(&id, true)?;
            }
        }

        let mut disk = self.disk_status()?;
        let max_total = self.settings.max_total_bytes;
        let reserve = self
            .settings
            .min_free_bytes
            .max((disk.total_bytes as f64 * self.settings.min_free_percent / 100.0).floor() as u64);
        let quota_target = (max_total as f64 * 0.85).floor() as u64;
        let quota_cleanup = disk.usage_bytes > max_total;
        let needs_cleanup = |disk: &DiskStatus| {
            disk.free_bytes < reserve
                || disk.usage_bytes
                    > if quota_cleanup {
                        quota_target
                    } else {
                        max_total
                    }
        };
        while needs_cleanup(&disk) {
            let candidate: Option<String> = self
                .db
                .query_row(
                    "
      SELECT id FROM session_logs
      WHERE pinned = 0 AND status <> 'active'
      ORDER BY COALESCE(ended_at, started_at), started_at
      LIMIT 1
    ",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(candidate) = candidate else { break };
            self.delete_session(&candidate, true)?;
            self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            disk = self.disk_status()?;
        }
        // `disk` is current here: every deletion above re-measured it.
        if disk.usage_bytes > max_total || disk.free_bytes < reserve {
            self.quota_suspended = true;
            self.quota_warning = Some(QUOTA_WARNING.to_owned());
        } else {
            self.quota_suspended = false;
            self.quota_warning = None;
        }
        Ok(())
    }

    pub(crate) fn storage_status(
        &mut self,
        configured_location: Option<&str>,
    ) -> Outcome<SessionHistoryStorageStatus> {
        self.enforce_retention(false)?;
        let disk = self.disk_status()?;
        Ok(SessionHistoryStorageStatus {
            settings: self.settings.clone(),
            active_storage_location: self.root.to_string_lossy().into_owned(),
            usage_bytes: disk.usage_bytes,
            free_bytes: disk.free_bytes,
            quota_suspended: self.quota_suspended,
            warning: self.quota_warning.clone(),
            restart_required: configured_location
                .filter(|location| !location.is_empty())
                .is_some_and(|location| {
                    resolve_path(Path::new(location)) != resolve_path(&self.root)
                }),
        })
    }

    fn disk_status(&self) -> Outcome<DiskStatus> {
        Ok(disk_status(&self.root)?)
    }

    /// Close sessions a crash left active: finalize every partial segment
    /// they left behind and mark them disconnected.
    fn recover_interrupted_sessions(&mut self) -> Outcome<()> {
        let active: Vec<String> = {
            let mut statement = self
                .db
                .prepare("SELECT id, current_part FROM session_logs WHERE status = 'active'")?;
            statement
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for id in active {
            let directory = self.session_directory(&id);
            if directory.exists() {
                let mut entries: Vec<(u32, PathBuf)> = Vec::new();
                for entry in fs::read_dir(&directory)? {
                    let entry = entry?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if let Some(part) = partial_part_number(&name) {
                        entries.push((part, entry.path()));
                    }
                }
                entries.sort();
                for (part, partial) in entries {
                    let decoded = decode_segment(&fs::read(&partial)?)?;
                    if decoded.events.is_empty() {
                        remove_file_if_exists(&partial)?;
                        continue;
                    }
                    let relative = self.finalize_partial(&id, part, &partial)?;
                    let raw_bytes: u64 = decoded
                        .events
                        .iter()
                        .map(|event| event.raw.len() as u64)
                        .sum();
                    let normalized: i64 = self.db.query_row(
                        "
          SELECT COALESCE(SUM(length(CAST(normalized_text AS BLOB))), 0) AS bytes
          FROM transcript_chunks WHERE session_id = ? AND part_number = ?
        ",
                        params![id, part],
                        |row| row.get(0),
                    )?;
                    let stored_bytes = fs::metadata(self.root.join(&relative))?.len();
                    self.db.execute(
                        "
          INSERT OR REPLACE INTO session_parts(
            session_id, part_number, filename, raw_bytes, stored_bytes,
            event_count, normalized_bytes, first_sequence, last_sequence
          ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
                        params![
                            id,
                            part,
                            relative,
                            raw_bytes as i64,
                            stored_bytes as i64,
                            decoded.events.len() as i64,
                            normalized,
                            decoded.events.first().map(|event| event.sequence as i64),
                            decoded.events.last().map(|event| event.sequence as i64)
                        ],
                    )?;
                }
            }
            self.db.execute(
                "
      UPDATE session_logs
      SET status = 'disconnected',
          ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP),
          paused = 0,
          current_part_bytes = 0
      WHERE id = ?
    ",
                [&id],
            )?;
        }
        Ok(())
    }

    /// Move the history of a version-6 application database (BLOB events in
    /// `session_log_events`) into this root. Sessions already imported are
    /// skipped, so an interrupted import resumes.
    fn import_legacy_history(&mut self, legacy_file: &Path) -> Outcome<()> {
        if !legacy_file.exists() || resolve_path(legacy_file) == resolve_path(&self.database_file) {
            return Ok(());
        }
        self.db.execute_batch(&format!(
            "ATTACH DATABASE {} AS legacy",
            sql_string(&legacy_file.to_string_lossy())
        ))?;
        let imported = self.import_attached_legacy_history();
        let detached = self.db.execute_batch("DETACH DATABASE legacy");
        imported?;
        detached?;
        Ok(())
    }

    fn import_attached_legacy_history(&mut self) -> Outcome<()> {
        let table: Option<String> = self
            .db
            .query_row(
                "SELECT name FROM legacy.sqlite_master WHERE type = 'table' AND name = 'session_logs'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if table.is_none() {
            return Ok(());
        }
        struct LegacySession {
            id: String,
            profile_key: String,
            title: String,
            kind: String,
            host: String,
            started_at: String,
            ended_at: Option<String>,
            status: String,
            capture_input: i64,
        }
        let sessions: Vec<LegacySession> = {
            let mut statement = self
                .db
                .prepare("SELECT * FROM legacy.session_logs ORDER BY started_at")?;
            statement
                .query_map([], |row| {
                    Ok(LegacySession {
                        id: row.get("id")?,
                        profile_key: row.get("profile_key")?,
                        title: row.get("title")?,
                        kind: row.get("kind")?,
                        host: row.get("host")?,
                        started_at: row.get("started_at")?,
                        ended_at: row.get("ended_at")?,
                        status: row.get("status")?,
                        capture_input: row.get("capture_input")?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?
        };
        for session in sessions {
            let id = session.id;
            let exists: Option<String> = self
                .db
                .query_row("SELECT id FROM session_logs WHERE id = ?", [&id], |row| {
                    row.get(0)
                })
                .optional()?;
            if exists.is_some() {
                continue;
            }
            let status = if session.status == "active" {
                "disconnected".to_owned()
            } else {
                session.status
            };
            self.db.execute(
                "
        INSERT INTO session_logs(
          id, profile_key, title, kind, host, started_at, ended_at, status,
          paused, capture_input, event_count, raw_bytes, normalized_bytes,
          current_part, current_part_bytes
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, ?, 0, 0, 0, 1, 0)
      ",
                params![
                    id,
                    session.profile_key,
                    session.title,
                    session.kind,
                    session.host,
                    session.started_at,
                    session.ended_at,
                    status,
                    session.capture_input
                ],
            )?;
            type LegacyEvent = (i64, i64, String, i64, String, Vec<u8>, String);
            let events: Vec<LegacyEvent> = {
                let mut statement = self.db.prepare(
                    "
        SELECT part_number, sequence, recorded_at, elapsed_ms, direction,
               raw_data, normalized_text
        FROM legacy.session_log_events
        WHERE session_id = ? ORDER BY part_number, sequence
      ",
                )?;
                statement
                    .query_map([&id], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<_>>()?
            };
            struct OpenPart {
                part: u32,
                partial: PathBuf,
                file: File,
                raw_bytes: u64,
                normalized_bytes: u64,
                event_count: u64,
                first_sequence: i64,
                last_sequence: i64,
            }
            let mut current: Option<OpenPart> = None;
            for (part_number, sequence, recorded_at, elapsed_ms, direction, raw, text) in events {
                let part = part_number.max(1) as u32;
                if current.as_ref().is_none_or(|open| open.part != part) {
                    if let Some(open) = current.take() {
                        self.finish_legacy_part(
                            &id,
                            open.part,
                            &open.partial,
                            open.file,
                            LegacyPartCounters {
                                raw_bytes: open.raw_bytes,
                                normalized_bytes: open.normalized_bytes,
                                event_count: open.event_count,
                                first_sequence: open.first_sequence,
                                last_sequence: open.last_sequence,
                            },
                        )?;
                    }
                    let directory = self.session_directory(&id);
                    create_private_dir_all(&directory)?;
                    let partial = directory.join(format!("{}.muxlog.partial", part_name(part)));
                    let mut file = create_private_file_truncating(&partial)?;
                    file.write_all(&SEGMENT_HEADER)?;
                    current = Some(OpenPart {
                        part,
                        partial,
                        file,
                        raw_bytes: 0,
                        normalized_bytes: 0,
                        event_count: 0,
                        first_sequence: sequence,
                        last_sequence: sequence,
                    });
                }
                let open = current.as_mut().expect("a part is open");
                let parsed_direction = SessionLogDirection::parse(&direction)
                    .ok_or_else(|| Failure(format!("invalid event direction: {direction}")))?;
                open.file.write_all(&encode_frame(
                    sequence.max(0) as u64,
                    &recorded_at,
                    elapsed_ms.max(0) as u64,
                    parsed_direction,
                    &raw,
                ))?;
                open.raw_bytes += raw.len() as u64;
                open.normalized_bytes += text.len() as u64;
                open.event_count += 1;
                open.last_sequence = sequence;
                if !text.is_empty() {
                    self.db.execute(
                        "
        INSERT INTO transcript_chunks(
          session_id, part_number, first_sequence, last_sequence,
          recorded_at, elapsed_ms, direction, normalized_text
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
      ",
                        params![
                            id,
                            part_number,
                            sequence,
                            sequence,
                            recorded_at,
                            elapsed_ms,
                            direction,
                            text
                        ],
                    )?;
                }
            }
            if let Some(open) = current.take() {
                self.finish_legacy_part(
                    &id,
                    open.part,
                    &open.partial,
                    open.file,
                    LegacyPartCounters {
                        raw_bytes: open.raw_bytes,
                        normalized_bytes: open.normalized_bytes,
                        event_count: open.event_count,
                        first_sequence: open.first_sequence,
                        last_sequence: open.last_sequence,
                    },
                )?;
            }
            let totals: (i64, i64, i64, i64) = self.db.query_row(
                "
        SELECT
          COALESCE(SUM(raw_bytes), 0) AS raw_bytes,
          COALESCE(SUM(normalized_bytes), 0) AS normalized_bytes,
          COALESCE(SUM(event_count), 0) AS event_count,
          COALESCE(MAX(part_number), 1) AS current_part
        FROM session_parts WHERE session_id = ?
      ",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            self.db.execute(
                "
        UPDATE session_logs
        SET raw_bytes = ?, normalized_bytes = ?, event_count = ?,
            current_part = ?, current_part_bytes = 0
        WHERE id = ?
      ",
                params![totals.0, totals.1, totals.2, totals.3, id],
            )?;
        }
        Ok(())
    }

    fn finish_legacy_part(
        &mut self,
        id: &str,
        part: u32,
        partial: &Path,
        file: File,
        counters: LegacyPartCounters,
    ) -> Outcome<()> {
        file.sync_data()?;
        drop(file);
        let relative = self.finalize_partial(id, part, partial)?;
        let stored_bytes = fs::metadata(self.root.join(&relative))?.len();
        self.db.execute(
            "
          INSERT INTO session_parts(
            session_id, part_number, filename, raw_bytes, stored_bytes,
            event_count, normalized_bytes, first_sequence, last_sequence
          ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
            params![
                id,
                part,
                relative,
                counters.raw_bytes as i64,
                stored_bytes as i64,
                counters.event_count as i64,
                counters.normalized_bytes as i64,
                counters.first_sequence,
                counters.last_sequence
            ],
        )?;
        Ok(())
    }

    fn remove_trash(&self) -> Outcome<()> {
        for entry in fs::read_dir(&self.trash_root)? {
            let path = entry?.path();
            if path.is_dir() {
                remove_dir_all_if_exists(&path)?;
            } else {
                remove_file_if_exists(&path)?;
            }
        }
        Ok(())
    }

    /// Finalize every open part, mark sessions still active as disconnected
    /// and checkpoint the WAL. The worker stops after this.
    pub(crate) fn close(&mut self) -> Outcome<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let ids: Vec<String> = self.writers.keys().cloned().collect();
        let mut first_error = None;
        for id in ids {
            if let Err(error) = self.finalize_writer(&id) {
                first_error.get_or_insert(error);
            }
        }
        self.db.execute_batch(
            "
    UPDATE session_logs
    SET status = 'disconnected',
        ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP),
        paused = 0,
        current_part_bytes = 0
    WHERE status = 'active';
    PRAGMA wal_checkpoint(TRUNCATE);
    PRAGMA optimize;
  ",
        )?;
        first_error.map_or(Ok(()), Err)
    }

    fn session_directory(&self, id: &str) -> PathBuf {
        self.sessions_root.join(safe_session_name(id))
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }
}

struct LegacyPartCounters {
    raw_bytes: u64,
    normalized_bytes: u64,
    event_count: u64,
    first_sequence: i64,
    last_sequence: i64,
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}

/// `NNNNNN.muxlog.partial` → `NNNNNN`.
fn partial_part_number(name: &str) -> Option<u32> {
    let digits = name.strip_suffix(".muxlog.partial")?;
    (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

fn has_column(db: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut statement = db.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>("name"))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.iter().any(|name| name == column))
}

/// `summaryFromRow`: falsy text and counts read as absent, like the TypeScript.
fn summary_from_row(row: &Row<'_>) -> rusqlite::Result<SessionLogSummary> {
    let status: String = row.get("status")?;
    let status = SessionLogStatus::parse(&status).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("invalid session status: {status}").into(),
        )
    })?;
    let count = |name: &str| -> rusqlite::Result<u64> {
        Ok(row.get::<_, Option<i64>>(name)?.unwrap_or(0).max(0) as u64)
    };
    let text = |name: &str| -> rusqlite::Result<Option<String>> {
        Ok(row
            .get::<_, Option<String>>(name)?
            .filter(|value| !value.is_empty()))
    };
    Ok(SessionLogSummary {
        id: row.get("id")?,
        profile_key: row.get("profile_key")?,
        title: row.get("title")?,
        label: text("label")?,
        kind: row.get("kind")?,
        host: row.get("host")?,
        started_at: row.get("started_at")?,
        ended_at: text("ended_at")?,
        status,
        paused: count("paused")? == 1,
        capture_input: count("capture_input")? == 1,
        pinned: count("pinned")? == 1,
        event_count: count("event_count")?,
        raw_bytes: count("raw_bytes")?,
        normalized_bytes: count("normalized_bytes")?,
        part_count: count("retained_part_count")?,
        snippet: text("snippet")?,
        match_count: row
            .get::<_, Option<i64>>("match_count")?
            .filter(|count| *count != 0)
            .map(|count| count as u64),
    })
}

/// `mkdirSync(path, { recursive: true, mode: 0o700 })`.
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// `openSync(path, 'wx', 0o600)`.
fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// `openSync(path, 'w', 0o600)` / `writeFileSync(path, …, { mode: 0o600 })`.
fn create_private_file_truncating(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn set_private_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// `rmSync(path, { force: true })`.
fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// `rmSync(path, { recursive: true, force: true })`.
pub(crate) fn remove_dir_all_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}
