//! Session history storage, ported from tests/unit/server/session-history-store.test.ts,
//! session-history-migration.test.ts and the storage cases of
//! session-history-routes.test.ts.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use muxus_core::api::{
    SessionEndStatus, SessionHistorySettings, SessionLineTimestamp, SessionLogCreateInput,
    SessionLogDirection, SessionLogStatus,
};
use muxus_store::history::{
    HistoryEvent, HistoryOpenOptions, PartPolicy, SessionHistoryQuery, SessionHistoryStore,
};

fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}

fn settings() -> SessionHistorySettings {
    SessionHistorySettings {
        storage_location: None,
        max_total_bytes: 5 * 1024 * 1024 * 1024,
        min_free_bytes: 0,
        min_free_percent: 0.0,
        max_age_days: None,
    }
}

fn open(root: &Path, settings: SessionHistorySettings) -> SessionHistoryStore {
    block_on(SessionHistoryStore::open(HistoryOpenOptions {
        root: Some(root.to_path_buf()),
        settings,
        legacy_database_path: None,
    }))
    .expect("history store opens")
}

fn input(
    profile_key: &str,
    title: &str,
    kind: &str,
    host: &str,
    started_at: &str,
) -> SessionLogCreateInput {
    SessionLogCreateInput {
        profile_key: profile_key.into(),
        title: title.into(),
        kind: muxus_core::protocol::ProfileKind::parse(kind).expect("kind"),
        host: host.into(),
        started_at: started_at.into(),
        capture_input: false,
    }
}

fn event(
    sequence: u64,
    recorded_at: &str,
    elapsed_ms: u64,
    raw: &[u8],
    text: &str,
) -> HistoryEvent {
    HistoryEvent {
        sequence,
        recorded_at: recorded_at.into(),
        elapsed_ms,
        direction: SessionLogDirection::Output,
        raw: raw.to_vec(),
        text: text.into(),
        line_timestamps: None,
    }
}

fn query(text: Option<&str>, limit: u32) -> SessionHistoryQuery {
    SessionHistoryQuery {
        query: text.map(str::to_owned),
        limit,
        ..Default::default()
    }
}

fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn temp_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("muxus-history-test-")
        .tempdir()
        .unwrap()
}

#[test]
fn upgrades_an_existing_transcript_database_and_retains_line_times_after_reopening() {
    let root = temp_root();
    let store = open(root.path(), settings());
    let policy = PartPolicy {
        max_part_bytes: 64 * 1024,
        max_parts: 2,
    };
    let time = "2026-09-24T10:00:00.000Z";
    let id = store.begin_session(&input("local", "Local", "local", "local", time), policy);
    assert!(store.append(&id, vec![event(1, time, 0, b"old\n", "old\n")], policy));
    block_on(store.close()).unwrap();
    let database = rusqlite::Connection::open(root.path().join("session-history.sqlite")).unwrap();
    database
        .execute_batch(
            "ALTER TABLE transcript_chunks DROP COLUMN line_timestamps; DELETE FROM history_schema_migrations WHERE version = 2",
        )
        .unwrap();
    drop(database);

    let store = open(root.path(), settings());
    let detail = block_on(store.session_log(&id, None, None))
        .unwrap()
        .unwrap();
    assert_eq!(detail.events[0].text, "old\n");
    assert_eq!(detail.events[0].line_timestamps, None);
    let next = store.begin_session(&input("local", "New", "local", "local", time), policy);
    let line_timestamps = vec![SessionLineTimestamp {
        offset: 0,
        recorded_at: time.into(),
    }];
    let mut with_times = event(1, time, 0, b"new\n", "new\n");
    with_times.line_timestamps = Some(line_timestamps.clone());
    assert!(store.append(&next, vec![with_times], policy));
    store.finish_session(&next, SessionEndStatus::Completed, time);
    block_on(store.close()).unwrap();

    let store = open(root.path(), settings());
    let detail = block_on(store.session_log(&next, None, None))
        .unwrap()
        .unwrap();
    assert_eq!(detail.events[0].line_timestamps, Some(line_timestamps));
    block_on(store.close()).unwrap();
}

#[test]
fn rotates_compressed_raw_segments_and_searches_normalized_chunks() {
    let root = temp_root();
    let store = open(root.path(), settings());
    let policy = PartPolicy {
        max_part_bytes: 64 * 1024,
        max_parts: 2,
    };
    let id = store.begin_session(
        &input(
            "ssh:edge",
            "Edge router",
            "ssh",
            "edge",
            "2026-07-24T10:00:00.000Z",
        ),
        policy,
    );
    for sequence in 1..=4u64 {
        let text = if sequence == 3 {
            "BGP neighbor established\n".to_owned()
        } else {
            format!("event {sequence}\n")
        };
        assert!(store.append(
            &id,
            vec![event(
                sequence,
                &format!("2026-07-24T10:00:0{sequence}.000Z"),
                sequence * 1_000,
                &vec![sequence as u8; 40 * 1024],
                &text,
            )],
            policy,
        ));
    }
    store.finish_session(&id, SessionEndStatus::Completed, "2026-07-24T10:01:00.000Z");

    let detail = block_on(store.session_log(&id, None, None))
        .unwrap()
        .unwrap();
    assert_eq!(detail.summary.status, SessionLogStatus::Completed);
    assert_eq!(detail.summary.event_count, 2);
    assert_eq!(detail.summary.part_count, 2);
    assert!(!detail.summary.pinned);
    assert_eq!(
        detail
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    let raw = block_on(store.raw_session_log_events(&id))
        .unwrap()
        .unwrap();
    assert_eq!(
        raw.iter().map(|event| event.sequence).collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert_eq!(raw[0].raw, vec![3u8; 40 * 1024]);
    let found = block_on(store.session_history(query(Some("BGP established"), 20))).unwrap();
    assert_eq!(
        found
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        vec![id.as_str()]
    );
    assert!(
        block_on(store.session_history(query(Some("event 1"), 20)))
            .unwrap()
            .sessions
            .is_empty()
    );

    let segments = entries(&root.path().join("sessions").join(&id));
    assert_eq!(segments.len(), 2);
    assert!(segments.iter().all(|file| file.ends_with(".muxlog.zst")));
    block_on(store.close()).unwrap();
}

#[test]
fn centers_snippets_on_the_match_counts_matches_and_anchors_previews() {
    let root = temp_root();
    let store = open(root.path(), settings());
    let policy = PartPolicy {
        max_part_bytes: 1024 * 1024,
        max_parts: 10,
    };
    let id = store.begin_session(
        &input(
            "ssh:lab",
            "Lab switch",
            "ssh",
            "lab",
            "2026-07-25T10:00:00.000Z",
        ),
        policy,
    );
    let events: Vec<HistoryEvent> = (1..=30u64)
        .map(|sequence| {
            let text = match sequence {
                5 => "ssh admin@192.168.7.44\n".to_owned(),
                12 => format!("{}ping 192.168.7.44 ok\n", "padding ".repeat(60)),
                _ => format!("filler line {sequence}\n"),
            };
            event(
                sequence,
                &format!("2026-07-25T10:00:{sequence:02}.000Z"),
                sequence * 1_000,
                text.as_bytes(),
                &text,
            )
        })
        .collect();
    assert!(store.append(&id, events, policy));
    store.finish_session(&id, SessionEndStatus::Completed, "2026-07-25T10:01:00.000Z");

    // A partially typed IP matches as a prefix and reports every occurrence.
    let by_ip = block_on(store.session_history(query(Some("192.168.7"), 20))).unwrap();
    assert_eq!(by_ip.sessions.len(), 1);
    assert_eq!(by_ip.sessions[0].id, id);
    assert_eq!(by_ip.sessions[0].match_count, Some(2));
    assert!(
        by_ip.sessions[0]
            .snippet
            .as_deref()
            .unwrap()
            .contains("\u{1}192.168.7\u{2}")
    );

    // The excerpt centers on a match buried deep inside a long chunk instead
    // of showing the chunk's first characters.
    let by_command = block_on(store.session_history(query(Some("ping"), 20))).unwrap();
    let snippet = by_command.sessions[0].snippet.clone().unwrap();
    assert!(snippet.contains("\u{1}ping\u{2}"));
    assert!(snippet.contains('…'));

    // Separator-only extra tokens carry no signal and must not empty the result.
    assert_eq!(
        block_on(store.session_history(query(Some("ping ->"), 20)))
            .unwrap()
            .sessions
            .len(),
        1
    );

    // A limited preview opened from a search anchors on the first match with
    // leading context; without a query it keeps returning the newest events.
    let anchored = block_on(store.session_log(&id, Some(10), Some("ping")))
        .unwrap()
        .unwrap();
    assert!(anchored.events_truncated);
    assert!(
        anchored
            .events
            .iter()
            .any(|event| event.text.contains("ping"))
    );
    assert!(anchored.events[0].sequence < 12);
    let newest = block_on(store.session_log(&id, Some(10), None))
        .unwrap()
        .unwrap();
    assert_eq!(
        newest
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (21..=30).collect::<Vec<_>>()
    );
    block_on(store.close()).unwrap();
}

#[test]
fn keeps_pinned_sessions_out_of_age_retention_and_uses_cursor_pages() {
    let root = temp_root();
    let store = open(root.path(), settings());
    let policy = PartPolicy {
        max_part_bytes: 1024 * 1024,
        max_parts: 2,
    };
    let mut ids = Vec::new();
    for day in 1..=3 {
        let id = store.begin_session(
            &input(
                "ssh:edge",
                &format!("Day {day}"),
                "ssh",
                "edge",
                &format!("2020-01-0{day}T10:00:00.000Z"),
            ),
            policy,
        );
        assert!(store.append(
            &id,
            vec![event(
                1,
                &format!("2020-01-0{day}T10:00:01.000Z"),
                1_000,
                format!("day {day}").as_bytes(),
                &format!("day {day}\n")
            )],
            policy,
        ));
        store.finish_session(
            &id,
            SessionEndStatus::Completed,
            &format!("2020-01-0{day}T10:01:00.000Z"),
        );
        ids.push(id);
    }

    let first = block_on(store.session_history(query(None, 2))).unwrap();
    assert_eq!(
        first
            .sessions
            .iter()
            .map(|s| s.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Day 3", "Day 2"]
    );
    assert!(first.next_cursor.is_some());
    let second = block_on(store.session_history(SessionHistoryQuery {
        limit: 2,
        cursor: first.next_cursor.clone(),
        ..Default::default()
    }))
    .unwrap();
    assert_eq!(
        second
            .sessions
            .iter()
            .map(|s| s.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Day 1"]
    );
    assert_eq!(second.next_cursor, None);

    assert!(block_on(store.set_pinned(&ids[0], true)).unwrap());
    block_on(store.update_settings(SessionHistorySettings {
        max_age_days: Some(1),
        ..settings()
    }))
    .unwrap();
    let remaining = block_on(store.session_history(query(None, 10)))
        .unwrap()
        .sessions;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, ids[0]);
    assert!(remaining[0].pinned);
    assert!(block_on(store.set_pinned(&ids[0], false)).unwrap());
    assert!(
        block_on(store.session_history(query(None, 10)))
            .unwrap()
            .sessions
            .is_empty()
    );
    block_on(store.close()).unwrap();
}

#[test]
fn names_sessions_searches_by_name_and_upgrades_databases_without_names() {
    let root = temp_root();
    let store = open(root.path(), settings());
    let policy = PartPolicy {
        max_part_bytes: 64 * 1024,
        max_parts: 2,
    };
    let time = "2026-09-24T10:00:00.000Z";
    let id = store.begin_session(
        &input("ssh:core", "Core router", "ssh", "core", time),
        policy,
    );
    store.finish_session(&id, SessionEndStatus::Completed, time);
    block_on(store.close()).unwrap();
    let database = rusqlite::Connection::open(root.path().join("session-history.sqlite")).unwrap();
    database
        .execute_batch("ALTER TABLE session_logs DROP COLUMN label; DELETE FROM history_schema_migrations WHERE version = 3")
        .unwrap();
    drop(database);

    let store = open(root.path(), settings());
    let detail = block_on(store.session_log(&id, None, None))
        .unwrap()
        .unwrap();
    assert_eq!(detail.summary.title, "Core router");
    assert_eq!(detail.summary.label, None);
    assert!(block_on(store.set_label(&id, "Pre-change config")).unwrap());
    assert!(!block_on(store.set_label("missing", "Nothing")).unwrap());
    block_on(store.close()).unwrap();

    let store = open(root.path(), settings());
    let named = block_on(store.session_history(query(Some("pre-change"), 20)))
        .unwrap()
        .sessions;
    assert_eq!(named.len(), 1);
    assert_eq!(named[0].id, id);
    assert_eq!(named[0].title, "Core router");
    assert_eq!(named[0].label.as_deref(), Some("Pre-change config"));
    assert_eq!(named[0].name(), "Pre-change config");
    let by_title = block_on(store.session_history(query(Some("core router"), 20)))
        .unwrap()
        .sessions;
    assert_eq!(
        by_title.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec![id.as_str()]
    );
    assert!(block_on(store.set_label(&id, "")).unwrap());
    assert_eq!(
        block_on(store.session_log(&id, None, None))
            .unwrap()
            .unwrap()
            .summary
            .label,
        None
    );
    assert!(
        block_on(store.session_history(query(Some("pre-change"), 20)))
            .unwrap()
            .sessions
            .is_empty()
    );
    block_on(store.close()).unwrap();
}

#[test]
fn applies_retention_at_open_before_answering_any_later_request() {
    let root = temp_root();
    let store = open(root.path(), settings());
    let policy = PartPolicy {
        max_part_bytes: 1024 * 1024,
        max_parts: 2,
    };
    let id = store.begin_session(
        &input(
            "ssh:edge",
            "Expired",
            "ssh",
            "edge",
            "2020-01-01T10:00:00.000Z",
        ),
        policy,
    );
    store.finish_session(&id, SessionEndStatus::Completed, "2020-01-01T10:01:00.000Z");
    block_on(store.close()).unwrap();

    // The startup pass runs after the store is ready, but the worker still
    // finishes it before the next request.
    let store = open(
        root.path(),
        SessionHistorySettings {
            max_age_days: Some(1),
            ..settings()
        },
    );
    assert!(
        block_on(store.session_history(query(None, 10)))
            .unwrap()
            .sessions
            .is_empty()
    );
    assert!(entries(&root.path().join("sessions")).is_empty());
    block_on(store.close()).unwrap();
}

#[test]
fn never_evicts_an_active_session_and_reports_quota_suspension() {
    let root = temp_root();
    let store = open(root.path(), settings());
    let policy = PartPolicy {
        max_part_bytes: 4 * 1024 * 1024,
        max_parts: 2,
    };
    let pinned = store.begin_session(
        &input(
            "ssh:pinned",
            "Pinned",
            "ssh",
            "pinned",
            "2026-01-01T10:00:00.000Z",
        ),
        policy,
    );
    assert!(store.append(
        &pinned,
        vec![event(
            1,
            "2026-01-01T10:00:01.000Z",
            1_000,
            b"keep me",
            "keep me\n"
        )],
        policy
    ));
    store.finish_session(
        &pinned,
        SessionEndStatus::Completed,
        "2026-01-01T10:01:00.000Z",
    );
    block_on(store.set_pinned(&pinned, true)).unwrap();

    let baseline = block_on(store.storage_status(None)).unwrap();
    // A deliberately tiny limit exercises the behavior without allocating tens of MiB.
    block_on(store.update_settings(SessionHistorySettings {
        max_total_bytes: baseline.usage_bytes + 512 * 1024,
        ..settings()
    }))
    .unwrap();
    let active = store.begin_session(
        &input(
            "ssh:active",
            "Active",
            "ssh",
            "active",
            "2026-01-02T10:00:00.000Z",
        ),
        policy,
    );
    let (failure_tx, failure_rx) = std::sync::mpsc::channel::<String>();
    let failure_tx = Mutex::new(failure_tx);
    let subscription = store.on_session_failure(&active, move |message| {
        let _ = failure_tx.lock().unwrap().send(message);
    });
    let mut large = vec![0u8; 1_200_000];
    muxus_store::ids::random_bytes(&mut large);
    assert!(store.append(
        &active,
        vec![event(
            1,
            "2026-01-02T10:00:01.000Z",
            1_000,
            &large,
            "large active event\n"
        )],
        policy
    ));

    let message = failure_rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("a failure is reported");
    let lower = message.to_lowercase();
    assert!(
        lower.contains("quota") || lower.contains("free-space"),
        "{message}"
    );
    assert!(
        block_on(store.storage_status(None))
            .unwrap()
            .quota_suspended
    );
    let sessions = block_on(store.session_history(query(None, 10)))
        .unwrap()
        .sessions;
    assert!(sessions.iter().any(|s| s.id == pinned && s.pinned));
    assert!(
        sessions
            .iter()
            .any(|s| s.id == active && s.status == SessionLogStatus::Active)
    );
    subscription.unsubscribe();
    block_on(store.close()).unwrap();
}

#[test]
fn truncates_an_incomplete_final_frame_and_marks_a_crash_interrupted_session() {
    let root = temp_root();
    let store = open(root.path(), settings());
    block_on(store.close()).unwrap();

    let database = rusqlite::Connection::open(root.path().join("session-history.sqlite")).unwrap();
    database
        .execute(
            "INSERT INTO session_logs(
               id, profile_key, title, kind, host, started_at, status,
               capture_input, event_count, raw_bytes, current_part, current_part_bytes
             ) VALUES (?, ?, ?, ?, ?, ?, 'active', 0, 1, 5, 1, 5)",
            [
                "interrupted",
                "ssh:edge",
                "Interrupted",
                "ssh",
                "edge",
                "2026-01-01T10:00:00.000Z",
            ],
        )
        .unwrap();
    drop(database);

    let directory = root.path().join("sessions").join("interrupted");
    std::fs::create_dir_all(&directory).unwrap();
    let mut segment = vec![0x4d, 0x55, 0x58, 0x4c, 0x4f, 0x47, 0x01, 0x0a];
    let raw = b"hello";
    segment.extend_from_slice(&((25 + raw.len()) as u32).to_be_bytes());
    segment.extend_from_slice(&1u64.to_be_bytes());
    segment.extend_from_slice(&1_767_261_601_000u64.to_be_bytes());
    segment.extend_from_slice(&1_000u64.to_be_bytes());
    segment.push(2);
    segment.extend_from_slice(raw);
    segment.extend_from_slice(&[0, 0, 0, 100, 0, 0, 0]);
    std::fs::write(directory.join("000001.muxlog.partial"), segment).unwrap();

    let store = open(root.path(), settings());
    let detail = block_on(store.session_log("interrupted", None, None))
        .unwrap()
        .unwrap();
    assert_eq!(detail.summary.status, SessionLogStatus::Disconnected);
    assert_eq!(detail.summary.part_count, 1);
    let events = block_on(store.raw_session_log_events("interrupted"))
        .unwrap()
        .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.raw.clone())
            .collect::<Vec<_>>(),
        vec![b"hello".to_vec()]
    );
    assert_eq!(events[0].recorded_at, "2026-01-01T10:00:01.000Z");
    assert_eq!(entries(&directory), vec!["000001.muxlog.zst"]);
    block_on(store.close()).unwrap();
}

/// The version-6 application database layout, before history moved to its own root.
const LEGACY_HISTORY_SCHEMA: &str = "
  CREATE TABLE session_logs (
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
    event_count INTEGER NOT NULL DEFAULT 0 CHECK(event_count >= 0),
    raw_bytes INTEGER NOT NULL DEFAULT 0 CHECK(raw_bytes >= 0),
    normalized_bytes INTEGER NOT NULL DEFAULT 0 CHECK(normalized_bytes >= 0),
    current_part INTEGER NOT NULL DEFAULT 1 CHECK(current_part >= 1),
    current_part_bytes INTEGER NOT NULL DEFAULT 0 CHECK(current_part_bytes >= 0)
  ) STRICT;
  CREATE TABLE session_log_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES session_logs(id) ON DELETE CASCADE,
    part_number INTEGER NOT NULL CHECK(part_number >= 1),
    sequence INTEGER NOT NULL CHECK(sequence >= 1),
    recorded_at TEXT NOT NULL,
    elapsed_ms INTEGER NOT NULL CHECK(elapsed_ms >= 0),
    direction TEXT NOT NULL CHECK(direction IN ('input', 'output', 'system')),
    raw_data BLOB NOT NULL,
    normalized_text TEXT NOT NULL,
    UNIQUE(session_id, sequence)
  ) STRICT;
";

#[test]
fn imports_legacy_blob_events_from_a_version_6_application_database() {
    let directory = tempfile::Builder::new()
        .prefix("muxus-history-migration-")
        .tempdir()
        .unwrap();
    let database_path = directory.path().join("muxus.sqlite");
    let legacy = rusqlite::Connection::open(&database_path).unwrap();
    legacy.execute_batch(LEGACY_HISTORY_SCHEMA).unwrap();
    legacy
        .execute(
            "INSERT INTO session_logs(
               id, profile_key, title, kind, host, started_at, ended_at,
               status, capture_input, event_count, raw_bytes, normalized_bytes
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                "legacy-session",
                "ssh:legacy",
                "Legacy router",
                "ssh",
                "legacy",
                "2026-01-01T10:00:00.000Z",
                "2026-01-01T10:01:00.000Z",
                "completed",
                0,
                1,
                13,
                13
            ],
        )
        .unwrap();
    legacy
        .execute(
            "INSERT INTO session_log_events(
               session_id, part_number, sequence, recorded_at, elapsed_ms,
               direction, raw_data, normalized_text
             ) VALUES (?, 1, 1, ?, 1000, 'output', ?, ?)",
            rusqlite::params![
                "legacy-session",
                "2026-01-01T10:00:01.000Z",
                b"legacy output".to_vec(),
                "legacy output"
            ],
        )
        .unwrap();
    drop(legacy);

    let history_path = directory.path().join("history");
    let store = block_on(SessionHistoryStore::open(HistoryOpenOptions {
        root: Some(history_path.clone()),
        settings: settings(),
        legacy_database_path: Some(database_path.clone()),
    }))
    .unwrap();
    let found = block_on(store.session_history(query(Some("legacy output"), 10)))
        .unwrap()
        .sessions;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "legacy-session");
    assert_eq!(found[0].title, "Legacy router");
    assert_eq!(found[0].event_count, 1);
    assert_eq!(found[0].raw_bytes, 13);
    let raw = block_on(store.raw_session_log_events("legacy-session"))
        .unwrap()
        .unwrap();
    assert_eq!(raw[0].raw, b"legacy output");
    block_on(store.close()).unwrap();

    // A second open skips sessions already imported.
    let store = block_on(SessionHistoryStore::open(HistoryOpenOptions {
        root: Some(history_path),
        settings: settings(),
        legacy_database_path: Some(database_path),
    }))
    .unwrap();
    assert_eq!(
        block_on(store.session_history(query(None, 10)))
            .unwrap()
            .sessions
            .len(),
        1
    );
    block_on(store.close()).unwrap();
}

fn seed_session(store: &SessionHistoryStore) -> String {
    let policy = PartPolicy {
        max_part_bytes: 1024 * 1024,
        max_parts: 2,
    };
    let id = store.begin_session(
        &input(
            "ssh:production",
            "Production",
            "ssh",
            "production",
            "2026-07-24T12:00:00.000Z",
        ),
        policy,
    );
    assert!(store.append(
        &id,
        vec![event(
            1,
            "2026-07-24T12:00:01.000Z",
            1_000,
            b"\x1b[32mdeploy complete\x1b[0m\r\n",
            "deploy complete\n"
        )],
        policy,
    ));
    store.finish_session(&id, SessionEndStatus::Completed, "2026-07-24T12:00:02.000Z");
    id
}

fn temporary_store() -> SessionHistoryStore {
    block_on(SessionHistoryStore::open(HistoryOpenOptions {
        root: None,
        settings: SessionHistorySettings {
            storage_location: None,
            max_total_bytes: 5 * 1024 * 1024 * 1024,
            min_free_bytes: 0,
            min_free_percent: 0.0,
            max_age_days: None,
        },
        legacy_database_path: None,
    }))
    .unwrap()
}

#[test]
fn full_text_searches_retained_transcripts_like_the_history_route() {
    let store = temporary_store();
    let id = seed_session(&store);
    let found = block_on(store.session_history(query(Some("deploy"), 50)))
        .unwrap()
        .sessions;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].title, "Production");
    assert_eq!(found[0].match_count, Some(1));
    assert!(
        found[0]
            .snippet
            .as_deref()
            .unwrap()
            .contains("\u{1}deploy\u{2} complete")
    );
    let detail = block_on(store.session_log(&id, Some(5_000), Some("deploy")))
        .unwrap()
        .unwrap();
    assert_eq!(detail.summary.id, id);
    assert_eq!(
        detail
            .events
            .iter()
            .map(|event| event.text.as_str())
            .collect::<Vec<_>>(),
        vec!["deploy complete\n"]
    );
    let raw = block_on(store.raw_session_log_events(&id))
        .unwrap()
        .unwrap();
    assert_eq!(raw[0].elapsed_ms, 1000);
    assert_eq!(raw[0].direction, SessionLogDirection::Output);
    let root = store.root().to_path_buf();
    block_on(store.close()).unwrap();
    assert!(!root.exists(), "a temporary root is removed on close");
}

#[test]
fn pins_and_names_sessions_like_the_history_routes() {
    let store = temporary_store();
    let id = seed_session(&store);
    assert!(block_on(store.set_pinned(&id, true)).unwrap());
    let listed = block_on(store.session_history(query(None, 50)))
        .unwrap()
        .sessions;
    assert!(listed[0].pinned);
    assert!(block_on(store.set_label(&id, "Pre-change config")).unwrap());
    let found = block_on(store.session_history(query(Some("pre-change"), 50)))
        .unwrap()
        .sessions;
    assert_eq!(found[0].label.as_deref(), Some("Pre-change config"));
    assert_eq!(found[0].title, "Production");
    assert!(block_on(store.set_label(&id, "")).unwrap());
    let detail = block_on(store.session_log(&id, None, None))
        .unwrap()
        .unwrap();
    let json = serde_json::to_value(&detail).unwrap();
    assert!(json.get("label").is_none());
    assert!(!block_on(store.delete_session("missing")).unwrap());
    assert!(block_on(store.delete_session(&id)).unwrap());
    assert!(
        block_on(store.session_log(&id, None, None))
            .unwrap()
            .is_none()
    );
    block_on(store.close()).unwrap();
}

#[test]
fn reports_actual_storage_usage_and_a_pending_location_change() {
    let store = temporary_store();
    seed_session(&store);
    let status = block_on(store.storage_status(None)).unwrap();
    assert!(status.usage_bytes > 0);
    assert!(status.free_bytes > 0);
    assert!(!status.quota_suspended);
    assert!(!status.restart_required);
    assert_eq!(
        status.active_storage_location,
        store.root().to_string_lossy()
    );
    let moved = block_on(store.storage_status(Some("/tmp/muxus-history-on-another-disk"))).unwrap();
    assert!(moved.restart_required);
    let same =
        block_on(store.storage_status(Some(&store.root().join("x").join("..").to_string_lossy())))
            .unwrap();
    assert!(!same.restart_required);
    block_on(store.close()).unwrap();
    assert!(matches!(
        block_on(store.storage_status(None)),
        Err(muxus_store::history::HistoryError::Closed)
    ));
    assert!(!store.append(
        "x",
        vec![],
        PartPolicy {
            max_part_bytes: 1,
            max_parts: 1
        }
    ));
}

#[test]
fn refuses_the_filesystem_root() {
    let error = block_on(SessionHistoryStore::open(HistoryOpenOptions {
        root: Some(PathBuf::from("/")),
        settings: settings(),
        legacy_database_path: None,
    }))
    .unwrap_err();
    assert!(error.to_string().contains("filesystem root"));
}

#[test]
fn records_live_state_changes_and_reads_the_active_part() {
    let root = temp_root();
    let store = open(root.path(), settings());
    let policy = PartPolicy {
        max_part_bytes: 1024 * 1024,
        max_parts: 2,
    };
    let id = store.begin_session(
        &input(
            "local",
            "Shell",
            "local",
            "local",
            "2026-02-01T10:00:00.000Z",
        ),
        policy,
    );
    assert!(store.append(
        &id,
        vec![event(1, "2026-02-01T10:00:01.000Z", 1, b"one", "one\n")],
        policy
    ));
    store.set_session_state(
        &id,
        muxus_store::history::SessionStatePatch {
            paused: Some(true),
            capture_input: Some(true),
        },
    );
    store.set_session_title(&id, "Renamed");
    let live = block_on(store.session_log(&id, None, None))
        .unwrap()
        .unwrap();
    assert!(live.summary.paused);
    assert!(live.summary.capture_input);
    assert_eq!(live.summary.title, "Renamed");
    assert_eq!(live.summary.part_count, 0);
    let raw = block_on(store.raw_session_log_events(&id))
        .unwrap()
        .unwrap();
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0].raw, b"one");
    // Active sessions are never deleted.
    assert!(!block_on(store.delete_session(&id)).unwrap());
    block_on(store.close()).unwrap();

    let store = open(root.path(), settings());
    let closed = block_on(store.session_log(&id, None, None))
        .unwrap()
        .unwrap();
    assert_eq!(closed.summary.status, SessionLogStatus::Disconnected);
    assert!(!closed.summary.paused);
    assert_eq!(closed.summary.part_count, 1);
    // Finished records keep the title they were recorded under.
    store.set_session_title(&id, "Too late");
    assert_eq!(
        block_on(store.session_log(&id, None, None))
            .unwrap()
            .unwrap()
            .summary
            .title,
        "Renamed"
    );
    block_on(store.close()).unwrap();
}
