//! The process's `tracing` subscriber (`buildLogger` in `server/src/app.ts`).
//!
//! The TypeScript server sends every record to the console and to the buffer behind the log
//! viewer. The desktop app keeps that and adds the main log file (`<data>/logs/main.log`), so
//! a session that failed hours ago still left its trace after a restart: connection attempts
//! and failures, sessions that ended, file browser and tunnel errors.
//!
//! Muxus's own crates log at the level in effect ([`super::level`], raised by the log
//! viewer's debug switch); the libraries underneath (the SSH engine, TLS, RDP) only their
//! errors, which keeps their protocol chatter (an RDP server's odd control PDUs) out. Records carry hosts, ports,
//! users and error texts, never passwords, passphrases or key material.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use tracing::Level;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;

use super::app_log::AppLogLayer;
use super::level::level_enabled;

/// Whether a record passes: Muxus's crates at the current level, everything else when it
/// is an error.
pub fn record_wanted(metadata: &tracing::Metadata<'_>) -> bool {
    let level = metadata.level();
    if !level_enabled(level) {
        return false;
    }
    is_own_target(metadata.target()) || *level == Level::ERROR
}

fn is_own_target(target: &str) -> bool {
    target.starts_with("muxus")
}

/// Install the subscriber for the whole process: the log viewer's buffer and, when given,
/// the log file. Returns false when a subscriber was already installed.
pub fn install_app_subscriber(file: Option<PathBuf>) -> bool {
    let buffer =
        AppLogLayer::new().with_filter(tracing_subscriber::filter::filter_fn(record_wanted));
    let file = file.map(|path| {
        FileLayer::new(path).with_filter(tracing_subscriber::filter::filter_fn(record_wanted))
    });
    let subscriber = tracing_subscriber::registry().with(buffer).with(file);
    tracing::subscriber::set_global_default(subscriber).is_ok()
}

/// Appends one line per record to a plain-text file, in the main log's format:
/// `2026-10-13T05:02:18.653Z WARN ssh dial failed host=web-01 port=22 err="…"`.
pub struct FileLayer {
    path: PathBuf,
    // One writer at a time keeps lines whole.
    lock: Mutex<()>,
}

impl FileLayer {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }
}

/// One record as a line (no trailing newline): time, level, message, then the fields.
pub fn format_record(
    time: &str,
    level: &Level,
    message: &str,
    fields: &[(String, String)],
) -> String {
    let mut line = format!("{time} {} {message}", level_label(level));
    for (key, value) in fields {
        let _ = write!(line, " {key}={}", quote(value));
    }
    line
}

fn level_label(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

/// Values with spaces, quotes or line breaks are quoted (JSON string rules), so a line stays
/// one line and splits back into its fields.
fn quote(value: &str) -> String {
    let plain = !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '=' || c.is_control());
    if plain {
        value.to_string()
    } else {
        serde_json::Value::String(value.to_string()).to_string()
    }
}

#[derive(Default)]
struct Fields {
    message: String,
    fields: Vec<(String, String)>,
}

impl tracing::field::Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.put(field, value.to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.put(field, format!("{value:?}"));
    }
}

impl Fields {
    fn put(&mut self, field: &tracing::field::Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for FileLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let time = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        let mut line = format_record(
            &time,
            event.metadata().level(),
            &fields.message,
            &fields.fields,
        );
        line.push('\n');
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // Never let logging take the app down.
        if let Ok(mut out) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = out.write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_records_as_one_line_with_quoted_fields() {
        let line = format_record(
            "2026-10-13T05:02:18.653Z",
            &Level::WARN,
            "ssh dial failed",
            &[
                ("host".into(), "web-01".into()),
                ("port".into(), "22".into()),
                (
                    "err".into(),
                    "Connection refused (os error 111)\nnext".into(),
                ),
            ],
        );
        assert_eq!(
            line,
            "2026-10-13T05:02:18.653Z WARN ssh dial failed host=web-01 port=22 \
             err=\"Connection refused (os error 111)\\nnext\""
        );
    }

    #[test]
    fn writes_records_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.log");
        let subscriber = tracing_subscriber::registry().with(
            FileLayer::new(path.clone())
                .with_filter(tracing_subscriber::filter::filter_fn(record_wanted)),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "muxus_ssh::connection", host = "web-01", "ssh transport dialed");
            tracing::info!(target: "russh::client", "protocol chatter");
            tracing::warn!(target: "ironrdp::session", "a library warning");
            tracing::error!(target: "russh::client", "a library error");
            tracing::debug!(target: "muxus_ssh::connection", "debug detail");
        });
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].ends_with(" INFO ssh transport dialed host=web-01"),
            "{text}"
        );
        assert!(lines[1].ends_with(" ERROR a library error"), "{text}");
    }
}
