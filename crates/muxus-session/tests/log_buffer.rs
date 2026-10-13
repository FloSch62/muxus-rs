//! Port of `tests/unit/server/log-buffer.test.ts`. Each test uses a buffer of
//! its own; the process-wide one is checked in a single test.

use muxus_core::api::{AppLogLevel, AppLogSource};
use muxus_session::logging::app_log::{
    APP_LOG_CAPACITY, AppLogBuffer, AppLogLayer, app_log_entries, append_app_log, clear_app_log,
};
use serde_json::json;

fn leaked(capacity: usize) -> &'static AppLogBuffer {
    Box::leak(Box::new(AppLogBuffer::new(capacity)))
}

#[test]
fn stores_shell_entries_with_level_source_and_context() {
    let buffer = AppLogBuffer::new(APP_LOG_CAPACITY);
    let context = json!({ "err": "EADDRINUSE" }).as_object().unwrap().clone();
    buffer.append_app_log(
        AppLogLevel::Error,
        "the embedded server failed to start",
        Some(context.clone()),
    );
    let entries = buffer.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].level, AppLogLevel::Error);
    assert_eq!(entries[0].source, AppLogSource::App);
    assert_eq!(entries[0].msg, "the embedded server failed to start");
    assert_eq!(entries[0].context, Some(context));
    assert!(entries[0].ts > 0);
}

#[test]
fn drops_the_oldest_entries_beyond_the_capacity() {
    let buffer = AppLogBuffer::new(APP_LOG_CAPACITY);
    for index in 0..APP_LOG_CAPACITY + 10 {
        buffer.append_app_log(AppLogLevel::Info, &format!("entry {index}"), None);
    }
    let entries = buffer.entries();
    assert_eq!(entries.len(), APP_LOG_CAPACITY);
    assert_eq!(entries[0].msg, "entry 10");
    assert_eq!(
        entries.last().unwrap().msg,
        format!("entry {}", APP_LOG_CAPACITY + 9)
    );
}

#[test]
fn clears_on_demand_including_the_process_wide_buffer() {
    let buffer = AppLogBuffer::new(10);
    buffer.append_app_log(AppLogLevel::Info, "before", None);
    buffer.clear();
    assert!(buffer.entries().is_empty());

    clear_app_log();
    append_app_log(AppLogLevel::Warn, "process-wide", None);
    assert_eq!(app_log_entries().last().unwrap().msg, "process-wide");
    clear_app_log();
    assert!(app_log_entries().is_empty());
}

#[test]
fn parses_serialized_pino_records_into_server_entries() {
    let buffer = AppLogBuffer::new(APP_LOG_CAPACITY);
    buffer.write_json_line(
        &json!({
            "level": 40,
            "time": 1_753_862_400_000_i64,
            "pid": 4242,
            "hostname": "box",
            "host": "db.example.com",
            "err": { "type": "Error", "message": "agent did not respond" },
            "msg": "ssh dial failed",
        })
        .to_string(),
    );
    let entries = buffer.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        serde_json::to_value(&entries[0]).unwrap(),
        json!({
            "ts": 1_753_862_400_000_i64,
            "level": "warn",
            "source": "server",
            "msg": "ssh dial failed",
            "context": {
                "host": "db.example.com",
                "err": { "type": "Error", "message": "agent did not respond" },
            },
        })
    );
}

#[test]
fn skips_per_request_noise_and_unparseable_lines() {
    let buffer = AppLogBuffer::new(APP_LOG_CAPACITY);
    buffer.write_json_line(
        &json!({ "level": 30, "time": 1, "msg": "incoming request", "reqId": "req-1" }).to_string(),
    );
    buffer.write_json_line(
        &json!({ "level": 30, "time": 2, "msg": "request completed", "reqId": "req-1" })
            .to_string(),
    );
    buffer.write_json_line("not json\n");
    buffer.write_json_line(
        &json!({ "level": 50, "time": 3, "msg": "request errored", "reqId": "req-1" }).to_string(),
    );
    let entries = buffer.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        (entries[0].level, entries[0].msg.as_str()),
        (AppLogLevel::Error, "request errored")
    );
}

#[test]
fn maps_unknown_pino_levels_to_sane_names() {
    let buffer = AppLogBuffer::new(APP_LOG_CAPACITY);
    buffer.write_json_line(&json!({ "level": 60, "time": 1, "msg": "fatal thing" }).to_string());
    buffer.write_json_line(&json!({ "level": 5, "time": 2, "msg": "below trace" }).to_string());
    buffer.write_json_line(&json!({ "level": "nope", "time": 3, "msg": "no level" }).to_string());
    let levels: Vec<AppLogLevel> = buffer.entries().iter().map(|entry| entry.level).collect();
    assert_eq!(
        levels,
        vec![AppLogLevel::Fatal, AppLogLevel::Trace, AppLogLevel::Info]
    );
}

#[test]
fn records_tracing_events_from_the_backend() {
    use tracing_subscriber::layer::SubscriberExt;
    let buffer = leaked(10);
    let subscriber = tracing_subscriber::registry().with(AppLogLayer::with_buffer(buffer));
    tracing::subscriber::with_default(subscriber, || {
        tracing::warn!(host = "db.example.com", attempt = 2, "ssh dial failed");
        tracing::debug!("details");
    });
    let entries = buffer.entries();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        serde_json::to_value(&entries[0]).unwrap()["context"],
        json!({ "host": "db.example.com", "attempt": 2 })
    );
    assert_eq!(
        (entries[0].level, entries[0].source, entries[0].msg.as_str()),
        (AppLogLevel::Warn, AppLogSource::Server, "ssh dial failed")
    );
    assert_eq!(
        (entries[1].level, entries[1].context.is_none()),
        (AppLogLevel::Debug, true)
    );
}
