//! SQLite persistence, preferences and secrets, schema-compatible with Muxus.
//!
//! The store reads and writes the same files the TypeScript build does: the
//! `muxus.sqlite3` application database (same schema, same migrations), the
//! session history root, the desktop `client-state.json` preferences and the
//! password vault, whose key may live in the operating-system credential store.
//!
//! Everything here is synchronous and `Send`; async callers run it on a
//! blocking thread (`tokio::task::spawn_blocking`). The session history store
//! is the exception: like the TypeScript worker it owns a thread and exposes
//! async methods.

pub mod db;
pub mod development;
pub mod history;
pub mod ids;
pub mod paths;
pub mod prefs;
pub mod vault;

pub use db::{Database, DbError};
