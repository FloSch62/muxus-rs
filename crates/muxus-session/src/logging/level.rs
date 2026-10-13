//! The backend's log level and the debug-logging switch of the log viewer
//! (`baseLogLevel` in `server/src/app.ts`, `PUT /api/logs/settings`).
//!
//! All records go both to the console and to the in-memory buffer behind the log viewer; the
//! level is the only gate. The app installs [`level_filter`] on its `tracing` subscriber so the
//! switch takes effect at once.

use std::sync::atomic::{AtomicU8, Ordering};

use muxus_core::api::AppLogLevel;
use tracing::Level;

const UNSET: u8 = u8::MAX;
static CURRENT: AtomicU8 = AtomicU8::new(UNSET);

fn rank(level: AppLogLevel) -> u8 {
    match level {
        AppLogLevel::Trace => 0,
        AppLogLevel::Debug => 1,
        AppLogLevel::Info => 2,
        AppLogLevel::Warn => 3,
        AppLogLevel::Error => 4,
        AppLogLevel::Fatal => 5,
    }
}

fn from_rank(rank: u8) -> AppLogLevel {
    match rank {
        0 => AppLogLevel::Trace,
        1 => AppLogLevel::Debug,
        2 => AppLogLevel::Info,
        3 => AppLogLevel::Warn,
        4 => AppLogLevel::Error,
        _ => AppLogLevel::Fatal,
    }
}

/// Parses a pino level name (`LOG_LEVEL`).
pub fn parse_level(name: &str) -> Option<AppLogLevel> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "trace" => AppLogLevel::Trace,
        "debug" => AppLogLevel::Debug,
        "info" => AppLogLevel::Info,
        "warn" => AppLogLevel::Warn,
        "error" => AppLogLevel::Error,
        "fatal" => AppLogLevel::Fatal,
        _ => return None,
    })
}

/// The level the backend returns to when debug logging is switched off: `LOG_LEVEL`, else info.
pub fn base_log_level() -> AppLogLevel {
    std::env::var("LOG_LEVEL")
        .ok()
        .and_then(|value| parse_level(&value))
        .unwrap_or(AppLogLevel::Info)
}

/// The level in effect.
pub fn current_log_level() -> AppLogLevel {
    match CURRENT.load(Ordering::SeqCst) {
        UNSET => base_log_level(),
        rank => from_rank(rank),
    }
}

/// Whether the backend logs at debug level or below.
pub fn debug_enabled() -> bool {
    rank(current_log_level()) <= rank(AppLogLevel::Debug)
}

/// Switch debug logging. An explicit `LOG_LEVEL=trace` stays trace; switching off returns to
/// the base level. Returns whether a change happened.
pub fn set_debug_enabled(enabled: bool) -> bool {
    let was = debug_enabled();
    let base = base_log_level();
    let next = if enabled {
        if base == AppLogLevel::Trace {
            AppLogLevel::Trace
        } else {
            AppLogLevel::Debug
        }
    } else {
        base
    };
    CURRENT.store(rank(next), Ordering::SeqCst);
    was != enabled
}

/// Whether a `tracing` event at `level` passes the current level.
pub fn level_enabled(level: &Level) -> bool {
    let event = match *level {
        Level::TRACE => AppLogLevel::Trace,
        Level::DEBUG => AppLogLevel::Debug,
        Level::INFO => AppLogLevel::Info,
        Level::WARN => AppLogLevel::Warn,
        Level::ERROR => AppLogLevel::Error,
    };
    rank(event) >= rank(current_log_level())
}

/// A per-layer filter following the current level, for the app's `tracing` subscriber.
pub fn level_filter()
-> tracing_subscriber::filter::FilterFn<impl Fn(&tracing::Metadata<'_>) -> bool + Clone> {
    tracing_subscriber::filter::filter_fn(|metadata| level_enabled(metadata.level()))
}
