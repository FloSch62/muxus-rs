//! Domain and REST types (port of `shared/src/api-types.ts`).
//!
//! In the TypeScript these are the bodies of the HTTP routes; in Rust they are
//! the arguments and results of `muxus_session::Backend` methods and the rows
//! the store keeps. The JSON shape is identical to the TypeScript's.

mod app;
mod diagnostics;
mod files;
mod forwards;
mod history;
mod hosts;
mod vault;
mod workspace;

pub use app::*;
pub use diagnostics::*;
pub use files::*;
pub use forwards::*;
pub use history::*;
pub use hosts::*;
pub use vault::*;
pub use workspace::*;

pub use crate::session_log_file::{
    DEFAULT_SESSION_LOG_FILE_PATTERN, SESSION_LOG_FILE_PLACEHOLDERS, SessionLogFileNameValues,
    session_log_file_name, session_log_file_pattern_error,
};
