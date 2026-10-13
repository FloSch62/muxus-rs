//! The schema a fresh database gets, and how existing files are treated,
//! compared with files the TypeScript build created.

use std::path::{Path, PathBuf};

use muxus_store::Database;
use muxus_store::db::DbError;
use rusqlite::Connection;
use serde_json::{Value, json};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/db")
        .join(name)
}

fn read_json(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap()).unwrap()
}

/// `type, name, tbl_name, sql` of every schema object, plus the version
/// bookkeeping, in the shape the fixture generator writes.
fn schema(path: &Path) -> Value {
    let conn = Connection::open(path).unwrap();
    let user_version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let mut migrations = conn
        .prepare("SELECT version, name FROM schema_migrations ORDER BY version")
        .unwrap();
    let migrations: Vec<Value> = migrations
        .query_map([], |row| {
            Ok(json!({ "version": row.get::<_, i64>(0)?, "name": row.get::<_, String>(1)? }))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut objects = conn
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY rowid")
        .unwrap();
    let objects: Vec<Value> = objects
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
    json!({ "userVersion": user_version, "migrations": migrations, "objects": objects })
}

/// Unpack the zstd-compressed database fixture into `dir`.
fn unpack(name: &str, dir: &Path) -> PathBuf {
    let compressed = std::fs::read(fixture(name)).unwrap();
    let bytes = zstd::decode_all(compressed.as_slice()).unwrap();
    let path = dir.join("muxus.sqlite3");
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn a_fresh_database_has_the_typescript_schema_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data/muxus.sqlite3");
    Database::open(&path).unwrap().close().unwrap();
    assert_eq!(schema(&path), read_json("ts-fresh-schema.json"));
}

#[test]
fn separating_history_leaves_the_typescript_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("muxus.sqlite3");
    let db = Database::open(&path).unwrap();
    db.finalize_session_history_separation(false).unwrap();
    db.close().unwrap();
    assert_eq!(schema(&path), read_json("ts-finalized-schema.json"));
}

#[test]
fn a_current_typescript_database_opens_without_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = unpack("ts-populated.sqlite3.zst", dir.path());
    let before = schema(&path);
    assert_eq!(before, read_json("ts-populated-schema.json"));
    let db = Database::open(&path).unwrap();
    assert_eq!(db.applied_migrations().unwrap().len(), 29);
    db.close().unwrap();
    assert_eq!(schema(&path), before);
}

#[test]
fn a_newer_database_is_refused_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("muxus.sqlite3");
    Database::open(&path).unwrap().close().unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO schema_migrations(version, name) VALUES (30, 'from-the-future');
             PRAGMA user_version = 30;",
        )
        .unwrap();
    }
    let before = schema(&path);
    let error = Database::open(&path).unwrap_err();
    assert!(matches!(
        error,
        DbError::NewerSchema {
            found: 30,
            supported: 29
        }
    ));
    assert_eq!(
        error.to_string(),
        "database schema 30 is newer than this Muxus build supports (29)"
    );
    assert_eq!(schema(&path), before);
}

#[cfg(unix)]
#[test]
fn creates_private_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/muxus.sqlite3");
    Database::open(&path).unwrap().close().unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}
