//! The in-memory app log behind the log viewer (`server/src/logging/log-buffer.ts`).
//!
//! One buffer per process: the desktop shell and the backend share it, so
//! shell milestones and backend logs land in the same timeline the log
//! viewer and export read. Backend `tracing` events reach it through
//! [`AppLogLayer`]; JSON log lines (pino's format) through [`AppLogBuffer::write_json_line`].

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, OnceLock};

use muxus_core::api::{AppLogEntry, AppLogLevel, AppLogSource};
use serde_json::{Map, Value};

/// Entries kept; older ones are dropped.
pub const APP_LOG_CAPACITY: usize = 5000;

/// Record fields that describe the process, not the event.
const NOISE_KEYS: [&str; 5] = ["level", "time", "msg", "pid", "hostname"];

/// pino numeric levels to names; values in between round down.
fn level_name(value: &Value) -> AppLogLevel {
    let Some(value) = value.as_f64() else {
        return AppLogLevel::Info;
    };
    match value {
        v if v >= 60.0 => AppLogLevel::Fatal,
        v if v >= 50.0 => AppLogLevel::Error,
        v if v >= 40.0 => AppLogLevel::Warn,
        v if v >= 30.0 => AppLogLevel::Info,
        v if v >= 20.0 => AppLogLevel::Debug,
        _ => AppLogLevel::Trace,
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A bounded ring of log entries.
#[derive(Debug)]
pub struct AppLogBuffer {
    capacity: usize,
    entries: Mutex<VecDeque<AppLogEntry>>,
}

impl AppLogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
        }
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<AppLogEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn append(&self, entry: AppLogEntry) {
        let mut entries = self.lock();
        if entries.len() == self.capacity {
            entries.pop_front();
        }
        entries.push_back(entry);
    }

    /// Oldest first.
    pub fn entries(&self) -> Vec<AppLogEntry> {
        self.lock().iter().cloned().collect()
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Direct entry point for the desktop shell (and anything else without a logger).
    pub fn append_app_log(
        &self,
        level: AppLogLevel,
        msg: &str,
        context: Option<Map<String, Value>>,
    ) {
        self.append(AppLogEntry {
            ts: now_ms(),
            level,
            source: AppLogSource::App,
            msg: msg.to_string(),
            context,
        });
    }

    /// Mirror one serialized pino-style JSON record. Unparseable lines are ignored.
    pub fn write_json_line(&self, line: &str) {
        let Ok(Value::Object(record)) = serde_json::from_str::<Value>(line) else {
            return;
        };
        // Per-request lines would flush real events out of the ring within
        // minutes of routine polling. Request *errors* stay.
        let msg = record.get("msg").and_then(Value::as_str);
        if matches!(msg, Some("incoming request" | "request completed")) {
            return;
        }
        let context: Map<String, Value> = record
            .iter()
            .filter(|(key, _)| !NOISE_KEYS.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        self.append(AppLogEntry {
            ts: record
                .get("time")
                .and_then(Value::as_f64)
                .map_or_else(now_ms, |time| time as i64),
            level: record.get("level").map_or(AppLogLevel::Info, level_name),
            source: AppLogSource::Server,
            msg: msg.unwrap_or_default().to_string(),
            context: (!context.is_empty()).then_some(context),
        });
    }
}

/// The process-wide buffer.
pub fn app_log() -> &'static AppLogBuffer {
    static BUFFER: OnceLock<AppLogBuffer> = OnceLock::new();
    BUFFER.get_or_init(|| AppLogBuffer::new(APP_LOG_CAPACITY))
}

pub fn app_log_entries() -> Vec<AppLogEntry> {
    app_log().entries()
}

pub fn clear_app_log() {
    app_log().clear();
}

pub fn append_app_log(level: AppLogLevel, msg: &str, context: Option<Map<String, Value>>) {
    app_log().append_app_log(level, msg, context);
}

/// A `tracing` layer that copies backend events into a buffer, as the
/// pino destination does in the TypeScript backend: the `message` field is
/// the entry's text, every other field its context.
pub struct AppLogLayer {
    buffer: &'static AppLogBuffer,
}

impl AppLogLayer {
    /// Feeds the process-wide buffer.
    pub fn new() -> Self {
        Self { buffer: app_log() }
    }

    pub fn with_buffer(buffer: &'static AppLogBuffer) -> Self {
        Self { buffer }
    }
}

impl Default for AppLogLayer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    context: Map<String, Value>,
}

impl tracing::field::Visit for FieldVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.put(field, Value::String(value.to_string()));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.put(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.put(field, Value::from(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.put(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.put(field, Value::Bool(value));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.put(field, Value::String(format!("{value:?}")));
    }
}

impl FieldVisitor {
    fn put(&mut self, field: &tracing::field::Field, value: Value) {
        if field.name() == "message" {
            self.message = Some(match value {
                Value::String(text) => text,
                other => other.to_string(),
            });
        } else {
            self.context.insert(field.name().to_string(), value);
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for AppLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => AppLogLevel::Error,
            tracing::Level::WARN => AppLogLevel::Warn,
            tracing::Level::INFO => AppLogLevel::Info,
            tracing::Level::DEBUG => AppLogLevel::Debug,
            tracing::Level::TRACE => AppLogLevel::Trace,
        };
        self.buffer.append(AppLogEntry {
            ts: now_ms(),
            level,
            source: AppLogSource::Server,
            msg: visitor.message.unwrap_or_default(),
            context: (!visitor.context.is_empty()).then_some(visitor.context),
        });
    }
}
