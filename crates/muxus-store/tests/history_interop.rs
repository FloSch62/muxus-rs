//! A history root written by the TypeScript build reads back identically.
//!
//! `fixtures/history/ts-root` was produced by `fixtures/history/make-ts-root.mts`
//! with the TypeScript `SessionHistoryStore`: its database (zstd-compressed to
//! keep the fixture small), its segment files, what the TypeScript store
//! answered for a set of queries (`expected.json`), and its `sqlite_master`
//! (`schema.json`).

use std::future::Future;
use std::path::Path;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use base64::Engine;
use muxus_core::api::SessionHistorySettings;
use muxus_store::history::{
    HistoryEvent, HistoryOpenOptions, SessionHistoryQuery, SessionHistoryStore,
};
use serde_json::{Value, json};

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

fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/history/ts-root")
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

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The TypeScript fixture, unpacked into a scratch root.
fn unpack(root: &Path) {
    copy_dir(&fixture().join("sessions"), &root.join("sessions"));
    let compressed = std::fs::read(fixture().join("session-history.sqlite.zst")).unwrap();
    std::fs::write(
        root.join("session-history.sqlite"),
        zstd::decode_all(compressed.as_slice()).unwrap(),
    )
    .unwrap();
}

/// A raw event the way the TypeScript test script wrote it (raw as base64).
fn raw_json(events: Vec<HistoryEvent>) -> Value {
    Value::Array(
        events
            .into_iter()
            .map(|event| {
                json!({
                    "sequence": event.sequence,
                    "recordedAt": event.recorded_at,
                    "elapsedMs": event.elapsed_ms,
                    "direction": event.direction,
                    "raw": base64::engine::general_purpose::STANDARD.encode(&event.raw),
                    "text": event.text,
                })
            })
            .collect(),
    )
}

fn query(text: Option<&str>, limit: u32) -> SessionHistoryQuery {
    SessionHistoryQuery {
        query: text.map(str::to_owned),
        limit,
        ..Default::default()
    }
}

#[test]
fn reads_a_history_root_written_by_the_typescript_build() {
    let expected: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture().join("expected.json")).unwrap())
            .unwrap();
    let root = tempfile::tempdir().unwrap();
    unpack(root.path());
    let store = block_on(SessionHistoryStore::open(HistoryOpenOptions {
        root: Some(root.path().to_path_buf()),
        settings: settings(),
        legacy_database_path: None,
    }))
    .unwrap();
    let core = expected["ids"]["core"].as_str().unwrap();
    let shell = expected["ids"]["shell"].as_str().unwrap();
    let to_json = |value: &dyn erased::Json| value.json();

    assert_eq!(
        to_json(&block_on(store.session_history(query(None, 10))).unwrap()),
        expected["all"]
    );
    assert_eq!(
        to_json(&block_on(store.session_history(query(Some("neighbor 10.0"), 10))).unwrap()),
        expected["neighbor"]
    );
    assert_eq!(
        to_json(&block_on(store.session_history(query(Some("pre-change"), 10))).unwrap()),
        expected["byName"]
    );
    assert_eq!(
        to_json(&block_on(store.session_history(query(None, 1))).unwrap()),
        expected["page"]
    );
    assert_eq!(
        to_json(&block_on(store.session_log(core, None, None)).unwrap()),
        expected["coreDetail"]
    );
    assert_eq!(
        to_json(&block_on(store.session_log(core, Some(1), Some("show"))).unwrap()),
        expected["coreLimited"]
    );
    assert_eq!(
        to_json(&block_on(store.session_log(shell, None, None)).unwrap()),
        expected["shellDetail"]
    );
    assert_eq!(
        raw_json(
            block_on(store.raw_session_log_events(core))
                .unwrap()
                .unwrap()
        ),
        expected["coreRaw"]
    );
    assert_eq!(
        raw_json(
            block_on(store.raw_session_log_events(shell))
                .unwrap()
                .unwrap()
        ),
        expected["shellRaw"]
    );
    block_on(store.close()).unwrap();
}

#[test]
fn creates_the_same_schema_as_the_typescript_build() {
    let expected: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture().join("schema.json")).unwrap())
            .unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = block_on(SessionHistoryStore::open(HistoryOpenOptions {
        root: Some(root.path().to_path_buf()),
        settings: settings(),
        legacy_database_path: None,
    }))
    .unwrap();
    block_on(store.close()).unwrap();
    let db = rusqlite::Connection::open(root.path().join("session-history.sqlite")).unwrap();
    // `sqlite_stat*` tables are SQLite's own statistics, created by `PRAGMA
    // optimize`; the bundled SQLite also keeps `sqlite_stat4`, Node's does not.
    let mut statement = db
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_stat%' ORDER BY type, name")
        .unwrap();
    let schema: Vec<Value> = statement
        .query_map([], |row| {
            Ok(json!({
                "type": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "tbl_name": row.get::<_, String>(2)?,
                "sql": row.get::<_, Option<String>>(3)?,
            }))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let typescript: Vec<Value> = expected["schema"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| !row["name"].as_str().unwrap().starts_with("sqlite_stat"))
        .cloned()
        .collect();
    assert_eq!(schema, typescript);
    let mut statement = db
        .prepare("SELECT version, name FROM history_schema_migrations ORDER BY version")
        .unwrap();
    let migrations: Vec<Value> = statement
        .query_map([], |row| {
            Ok(json!({ "version": row.get::<_, i64>(0)?, "name": row.get::<_, String>(1)? }))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(Value::Array(migrations), expected["migrations"]);
}

/// Writes a root for the TypeScript build to read (`check-rust-root.mts`).
/// Run with `MUXUS_HISTORY_EXPORT=<dir> cargo test -- --ignored export_a_root`.
#[test]
#[ignore = "manual interop check against the TypeScript build"]
fn export_a_root_for_the_typescript_build() {
    use muxus_core::api::{SessionEndStatus, SessionLogCreateInput, SessionLogDirection};
    use muxus_core::protocol::ProfileKind;
    use muxus_store::history::PartPolicy;

    let target = std::path::PathBuf::from(
        std::env::var("MUXUS_HISTORY_EXPORT").expect("MUXUS_HISTORY_EXPORT"),
    );
    let root = target.join("rust-root");
    let _ = std::fs::remove_dir_all(&root);
    let store = block_on(SessionHistoryStore::open(HistoryOpenOptions {
        root: Some(root.clone()),
        settings: settings(),
        legacy_database_path: None,
    }))
    .unwrap();
    let small = PartPolicy {
        max_part_bytes: 40,
        max_parts: 10,
    };
    let event = |sequence: u64, direction, raw: &str, text: &str| HistoryEvent {
        sequence,
        recorded_at: format!("2026-04-01T08:00:0{sequence}.000Z"),
        elapsed_ms: sequence * 1000,
        direction,
        raw: raw.as_bytes().to_vec(),
        text: text.into(),
        line_timestamps: None,
    };
    let id = store.begin_session(
        &SessionLogCreateInput {
            profile_key: "ssh:edge".into(),
            title: "Edge".into(),
            kind: ProfileKind::Ssh,
            host: "edge".into(),
            started_at: "2026-04-01T08:00:00.000Z".into(),
            capture_input: true,
        },
        small,
    );
    store.append(
        &id,
        vec![
            event(
                1,
                SessionLogDirection::Output,
                "edge# show interfaces\r\n",
                "edge# show interfaces\n",
            ),
            event(2, SessionLogDirection::Input, "exit\r", "exit\n"),
            event(
                3,
                SessionLogDirection::Output,
                "Interface eth0 is up, line protocol up\r\n",
                "Interface eth0 is up, line protocol up\n",
            ),
        ],
        small,
    );
    store.finish_session(&id, SessionEndStatus::Completed, "2026-04-01T08:00:09.000Z");
    let live = store.begin_session(
        &SessionLogCreateInput {
            profile_key: "local".into(),
            title: "Live".into(),
            kind: ProfileKind::Local,
            host: "local".into(),
            started_at: "2026-04-01T09:00:00.000Z".into(),
            capture_input: false,
        },
        PartPolicy {
            max_part_bytes: 1 << 20,
            max_parts: 2,
        },
    );
    store.append(
        &live,
        vec![event(
            1,
            SessionLogDirection::Output,
            "still running",
            "still running",
        )],
        PartPolicy {
            max_part_bytes: 1 << 20,
            max_parts: 2,
        },
    );
    block_on(store.set_label(&id, "Before upgrade")).unwrap();
    let expected = json!({
        "ids": { "edge": id, "live": live },
        "search": serde_json::to_value(block_on(store.session_history(query(Some("line protocol"), 10))).unwrap()).unwrap(),
        "edgeDetail": serde_json::to_value(block_on(store.session_log(&id, None, None)).unwrap()).unwrap(),
        "edgeRaw": raw_json(block_on(store.raw_session_log_events(&id)).unwrap().unwrap()),
    });
    // Closing marks the live session disconnected and compresses its part,
    // exactly as the TypeScript close does; the listing is taken after that.
    block_on(store.close()).unwrap();
    let store = block_on(SessionHistoryStore::open(HistoryOpenOptions {
        root: Some(root),
        settings: settings(),
        legacy_database_path: None,
    }))
    .unwrap();
    let mut expected = expected;
    expected["all"] =
        serde_json::to_value(block_on(store.session_history(query(None, 10))).unwrap()).unwrap();
    block_on(store.close()).unwrap();
    std::fs::write(
        target.join("rust-expected.json"),
        serde_json::to_string_pretty(&expected).unwrap(),
    )
    .unwrap();
}

/// Serialize any answer the way the TypeScript route would send it.
mod erased {
    pub trait Json {
        fn json(&self) -> serde_json::Value;
    }

    impl<T: serde::Serialize> Json for T {
        fn json(&self) -> serde_json::Value {
            serde_json::to_value(self).unwrap()
        }
    }
}
