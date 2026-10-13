//! The log viewer over a fixed buffer (the entries the reference capture
//! serves), on a sandboxed backend. Nothing touches the real home directory.
//!
//! ```text
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen out.png -- target-ui-b/debug/examples/logs
//! ```
//!
//! `MUXUS_THEME=dark` for the dark variant.

#[path = "support/scene.rs"]
mod scene;

use muxus_app::features::logs::view::LogViewerDialog;
use muxus_core::api::{AppLogEntry, AppLogsResponse};
use muxus_ui::gpui_kit::AppContext;

/// 2026-07-25 18:17:03.120 UTC, the first entry's time.
const T0: i64 = 1_785_003_423_120;

fn entry(
    offset: i64,
    level: &str,
    source: &str,
    msg: &str,
    context: serde_json::Value,
) -> AppLogEntry {
    serde_json::from_value(serde_json::json!({
        "ts": T0 + offset,
        "level": level,
        "source": source,
        "msg": msg,
        "context": context,
    }))
    .unwrap()
}

fn logs() -> AppLogsResponse {
    use serde_json::json;
    AppLogsResponse {
        debug_enabled: false,
        capacity: 2000,
        entries: vec![
            entry(
                0,
                "info",
                "app",
                "Muxus 0.11.1 starting",
                json!({ "platform": "linux" }),
            ),
            entry(412, "info", "server", "listening", json!({ "port": 41873 })),
            entry(
                2050,
                "info",
                "server",
                "ssh connected",
                json!({ "host": "web-01", "user": "deploy" }),
            ),
            entry(
                2210,
                "info",
                "server",
                "forward started",
                json!({ "type": "local", "bind": 8080 }),
            ),
            entry(
                9310,
                "warn",
                "server",
                "keepalive missed",
                json!({ "host": "bastion", "missed": 1 }),
            ),
            entry(
                15002,
                "error",
                "server",
                "ssh connect failed",
                json!({ "host": "db-01", "err": "connect ECONNREFUSED 10.0.4.12:22" }),
            ),
            entry(
                15890,
                "info",
                "server",
                "connection diagnostics",
                json!({ "checked": "db-01.prod.internal:22", "failed": ["TCP"] }),
            ),
            entry(
                31770,
                "info",
                "app",
                "workspace saved",
                json!({ "tabs": 3 }),
            ),
        ],
    }
}

fn main() {
    let scratch = tempfile::tempdir().unwrap();
    let (backend, runtime) = scene::demo_backend(scratch.path());
    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            muxus_ui::init(scene::mode(), cx);
            let backend = backend.clone();
            scene::open(cx, move |window, cx| {
                let viewer = cx.new(|cx| {
                    let mut viewer = LogViewerDialog::new(backend, window, cx);
                    viewer.stop_polling();
                    viewer.set_logs(logs(), cx);
                    viewer
                });
                viewer.into()
            });
        });
    drop(runtime);
}
