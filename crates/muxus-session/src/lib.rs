//! The Muxus backend, in process: what the Node server of the TypeScript build did, as a library
//! the GPUI app calls (`server/src/{app,routes,ws,session-logging,login-sequence,diagnostics,stats}`).
//!
//! # Using it
//!
//! [`Backend::start`] opens the data folder ([`data_dir`]), the database, the password vault,
//! session history and the SSH connection manager, then starts the saved tunnels marked to start
//! with Muxus. Every HTTP route of the TypeScript server is an `async` method on [`Backend`] with
//! the same validation, errors ([`ApiError`]: status, message, code) and side effects. The
//! `/ws/terminal` socket is [`Backend::open_session`], which hands back a [`SessionHandle`]
//! speaking the `muxus_core::protocol` terminal messages. Call [`Backend::shutdown`] before the
//! process exits.
//!
//! # Threading model
//!
//! The backend owns a multi-thread tokio runtime (or uses one the caller hands it, see
//! [`RuntimeChoice`]). Nothing a caller does ever runs backend work on the caller's executor:
//!
//! - Each `Backend` method spawns its work onto the backend runtime and returns a future that
//!   only waits for a `tokio::sync::oneshot` reply. Those futures are executor-agnostic, so the
//!   UI awaits them on GPUI's executor (or blocks on them from a plain thread) without a tokio
//!   context.
//! - Store work (SQLite, the vault's scrypt, keyring access) runs on the runtime's blocking pool.
//!   Session history has its own worker thread inside `muxus-store`.
//! - A terminal session is a set of tasks on the backend runtime. Its [`SessionHandle`] talks to
//!   them over `async-channel` queues: input never blocks (`try_send` works from any thread),
//!   events are awaited or polled with [`SessionHandle::try_next_event`]. Output is
//!   backpressured: past 4 MiB of unread output the session stops reading its transport until
//!   the UI catches up.
//! - Local ptys and serial lines run on their own threads (`muxus-local`, `muxus-serial`).
//!
//! `Backend` is cheap to clone and `Send + Sync`; every clone shares one backend.

pub mod adapters;
mod backend;
pub mod data_dir;
pub mod desktop;
pub mod diagnostics;
mod error;
pub mod links;
pub mod logging;
pub mod login_sequence;
pub mod management;
mod prefs;
pub mod recovery;
pub mod routes;
pub mod stats;
pub mod terminal;
pub mod updater;

pub use backend::{
    Backend, BackendOptions, EnvLookup, RuntimeChoice, UPDATE_FEED_VAR, process_update_manifest_url,
};
pub use error::{ApiError, ApiResult};
pub use terminal::{SessionEvent, SessionHandle};
