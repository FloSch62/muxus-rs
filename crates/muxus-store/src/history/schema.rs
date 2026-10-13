//! The history database schema, verbatim from the TypeScript worker so both
//! builds create byte-identical `sqlite_schema` entries.

/// Pragmas and every `CREATE … IF NOT EXISTS` statement of version 1.
pub(crate) const SCHEMA: &str = r#"
  PRAGMA foreign_keys = ON;
  PRAGMA busy_timeout = 5000;
  PRAGMA journal_mode = WAL;
  PRAGMA synchronous = NORMAL;

  CREATE TABLE IF NOT EXISTS history_schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  ) STRICT;

  CREATE TABLE IF NOT EXISTS session_logs (
    id TEXT PRIMARY KEY,
    profile_key TEXT NOT NULL,
    title TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('openssh', 'ssh', 'local', 'serial', 'telnet')),
    host TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    status TEXT NOT NULL CHECK(status IN ('active', 'completed', 'disconnected', 'failed')),
    paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0, 1)),
    capture_input INTEGER NOT NULL CHECK(capture_input IN (0, 1)),
    pinned INTEGER NOT NULL DEFAULT 0 CHECK(pinned IN (0, 1)),
    event_count INTEGER NOT NULL DEFAULT 0 CHECK(event_count >= 0),
    raw_bytes INTEGER NOT NULL DEFAULT 0 CHECK(raw_bytes >= 0),
    normalized_bytes INTEGER NOT NULL DEFAULT 0 CHECK(normalized_bytes >= 0),
    current_part INTEGER NOT NULL DEFAULT 1 CHECK(current_part >= 1),
    current_part_bytes INTEGER NOT NULL DEFAULT 0 CHECK(current_part_bytes >= 0)
  ) STRICT;

  CREATE TABLE IF NOT EXISTS session_parts (
    session_id TEXT NOT NULL REFERENCES session_logs(id) ON DELETE CASCADE,
    part_number INTEGER NOT NULL CHECK(part_number >= 1),
    filename TEXT NOT NULL,
    raw_bytes INTEGER NOT NULL CHECK(raw_bytes >= 0),
    stored_bytes INTEGER NOT NULL CHECK(stored_bytes >= 0),
    event_count INTEGER NOT NULL CHECK(event_count >= 0),
    normalized_bytes INTEGER NOT NULL CHECK(normalized_bytes >= 0),
    first_sequence INTEGER,
    last_sequence INTEGER,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY(session_id, part_number)
  ) STRICT, WITHOUT ROWID;

  CREATE TABLE IF NOT EXISTS transcript_chunks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES session_logs(id) ON DELETE CASCADE,
    part_number INTEGER NOT NULL CHECK(part_number >= 1),
    first_sequence INTEGER NOT NULL CHECK(first_sequence >= 1),
    last_sequence INTEGER NOT NULL CHECK(last_sequence >= first_sequence),
    recorded_at TEXT NOT NULL,
    elapsed_ms INTEGER NOT NULL CHECK(elapsed_ms >= 0),
    direction TEXT NOT NULL CHECK(direction IN ('input', 'output', 'system')),
    normalized_text TEXT NOT NULL
  ) STRICT;

  CREATE INDEX IF NOT EXISTS session_logs_started
    ON session_logs(started_at DESC, id DESC);
  CREATE INDEX IF NOT EXISTS session_logs_profile_started
    ON session_logs(profile_key, started_at DESC);
  CREATE INDEX IF NOT EXISTS session_logs_retention
    ON session_logs(pinned, status, started_at);
  CREATE INDEX IF NOT EXISTS transcript_chunks_session_sequence
    ON transcript_chunks(session_id, first_sequence);
  CREATE INDEX IF NOT EXISTS transcript_chunks_session_part
    ON transcript_chunks(session_id, part_number);

  CREATE VIRTUAL TABLE IF NOT EXISTS transcript_chunks_fts USING fts5(
    normalized_text,
    content = 'transcript_chunks',
    content_rowid = 'id',
    tokenize = 'unicode61'
  );

  CREATE TRIGGER IF NOT EXISTS transcript_chunks_fts_insert
  AFTER INSERT ON transcript_chunks BEGIN
    INSERT INTO transcript_chunks_fts(rowid, normalized_text)
    VALUES (new.id, new.normalized_text);
  END;
  CREATE TRIGGER IF NOT EXISTS transcript_chunks_fts_delete
  AFTER DELETE ON transcript_chunks BEGIN
    INSERT INTO transcript_chunks_fts(transcript_chunks_fts, rowid, normalized_text)
    VALUES ('delete', old.id, old.normalized_text);
  END;
  CREATE TRIGGER IF NOT EXISTS transcript_chunks_fts_update
  AFTER UPDATE ON transcript_chunks BEGIN
    INSERT INTO transcript_chunks_fts(transcript_chunks_fts, rowid, normalized_text)
    VALUES ('delete', old.id, old.normalized_text);
    INSERT INTO transcript_chunks_fts(rowid, normalized_text)
    VALUES (new.id, new.normalized_text);
  END;

  INSERT OR IGNORE INTO history_schema_migrations(version, name)
  VALUES (1, 'hybrid-session-history');
"#;

/// Version 2 adds per-line timestamps to transcript chunks.
pub(crate) const ADD_LINE_TIMESTAMPS: &str =
    "ALTER TABLE transcript_chunks ADD COLUMN line_timestamps TEXT";
pub(crate) const RECORD_LINE_TIMESTAMPS: &str = "INSERT OR IGNORE INTO history_schema_migrations(version, name) VALUES (2, 'transcript-line-timestamps')";

/// Version 3 adds a user-chosen session name; NULL keeps the recorded title.
pub(crate) const ADD_LABEL: &str = "ALTER TABLE session_logs ADD COLUMN label TEXT";
pub(crate) const RECORD_LABEL: &str =
    "INSERT OR IGNORE INTO history_schema_migrations(version, name) VALUES (3, 'session-labels')";
