//! Port of `tests/unit/server/logs-routes.test.ts`. The log level is process-wide, so the cases
//! run in one test, in order.

mod support;

use muxus_core::api::AppLogLevel;
use muxus_session::logging::app_log::{AppLogLayer, clear_app_log};
use muxus_session::logging::level::level_filter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;

#[tokio::test]
async fn the_log_viewer_reads_the_buffer_and_switches_debug_logging() {
    let subscriber =
        tracing_subscriber::registry().with(AppLogLayer::new().with_filter(level_filter()));
    let _guard = tracing::subscriber::set_default(subscriber);
    clear_app_log();
    let sandbox = support::start().await;
    let backend = &sandbox.backend;

    // Serves buffered server log entries.
    tracing::warn!(host = "db.example.com", "ssh dial failed");
    let logs = backend.app_logs().await.unwrap();
    assert!(!logs.debug_enabled);
    assert!(logs.capacity > 0);
    let entry = logs
        .entries
        .iter()
        .find(|entry| entry.msg == "ssh dial failed")
        .expect("the warning is buffered");
    assert_eq!(entry.level, AppLogLevel::Warn);
    assert_eq!(
        entry
            .context
            .as_ref()
            .and_then(|context| context.get("host")),
        Some(&serde_json::json!("db.example.com"))
    );

    // Debug records are kept only while debug mode is on.
    tracing::debug!("hidden detail");
    assert!(
        !backend
            .app_logs()
            .await
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.msg == "hidden detail")
    );
    assert!(backend.put_log_settings(true).await.unwrap().debug_enabled);
    tracing::debug!("visible detail");
    let after = backend.app_logs().await.unwrap();
    assert!(after.debug_enabled);
    assert!(
        after
            .entries
            .iter()
            .any(|entry| entry.msg == "visible detail" && entry.level == AppLogLevel::Debug)
    );

    // Switching off restores the base level and leaves an audit trail.
    assert!(!backend.put_log_settings(false).await.unwrap().debug_enabled);
    let messages: Vec<String> = backend
        .app_logs()
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.msg)
        .collect();
    assert!(messages.contains(&"debug logging enabled".to_string()));
    assert!(messages.contains(&"debug logging disabled".to_string()));

    // Clears the buffer.
    tracing::warn!("to be cleared");
    assert!(backend.clear_app_logs().await.unwrap());
    assert!(
        !backend
            .app_logs()
            .await
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.msg == "to be cleared")
    );
    sandbox.stop().await;
}
