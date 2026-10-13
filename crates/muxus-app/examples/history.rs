//! Session history on the documentation's demo sandbox, in the state of the
//! `history` screenshot (`hack/capture.mjs`): web-01's sessions retained,
//! the running one (after `status` and `tailaccess`) selected.
//!
//! ```bash
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen --env TZ=UTC out.png \
//!   -- target-ui-c/debug/examples/history
//! ```
//!
//! - `MUXUS_THEME=light|dark` (default light)
//! - `MUXUS_HISTORY_QUERY=<text>` opens with a search (hits highlighted)
//! - `MUXUS_WINDOW=fullscreen` fills the monitor
//! - `MUXUS_DATA_DIR=<dir>` keeps the sandbox there instead of a temp dir

use muxus_app::features::history::open_history;
use muxus_app::features::launcher::demo::{DemoSession, demo_env, run_demo, seed_history};

/// What the demo host prints for `status` and `tailaccess` (`hack/demo-env.mjs`).
const STATUS: &[&str] = &[
    "deploy@web-01:~$ status\n",
    "UNIT                STATE     CPU      MEMORY\n",
    "edge.service        active    3.1%     218M\n",
    "nginx.service       active    0.4%     24M\n",
    "node-exporter       active    0.2%     18M\n",
    "backup.timer        waiting   -        -\n",
    "\n",
    "host      web-01   uptime 12 days   load 0.31 0.28 0.22\n",
    "deploy@web-01:~$ tailaccess\n",
    "10.20.3.31 - - [12/Mar/2026:09:28:11] \"GET /healthz HTTP/1.1\" 200 12\n",
    "10.20.4.32 - - [12/Mar/2026:09:29:11] \"GET /healthz HTTP/1.1\" 200 12\n",
    "10.20.0.33 - - [12/Mar/2026:09:30:11] \"GET /healthz HTTP/1.1\" 200 12\n",
    "10.20.1.34 - - [12/Mar/2026:09:31:11] \"GET /healthz HTTP/1.1\" 200 12\n",
    "10.20.2.35 - - [12/Mar/2026:09:32:11] \"GET /healthz HTTP/1.1\" 200 12\n",
    "10.20.3.36 - - [12/Mar/2026:09:33:11] \"GET /healthz HTTP/1.1\" 200 12\n",
    "10.20.4.37 - - [12/Mar/2026:09:34:11] \"GET /healthz HTTP/1.1\" 200 12\n",
];

fn main() {
    let query = std::env::var("MUXUS_HISTORY_QUERY").unwrap_or_default();
    let env = demo_env("history", |_| {});
    // The capture run's earlier shots each left a web-01 session behind.
    let prompt: &[&str] = &["deploy@web-01:~$ status\n"];
    seed_history(
        &env.backend,
        &[
            DemoSession {
                host: "web-01",
                started_at: "2026-10-12T17:40:41.000Z",
                active: false,
                reused: false,
                raw_size: 318,
                output: prompt,
            },
            DemoSession {
                host: "web-01",
                started_at: "2026-10-12T17:41:11.000Z",
                active: false,
                reused: false,
                raw_size: 396,
                output: prompt,
            },
            DemoSession {
                host: "web-01",
                started_at: "2026-10-12T17:41:32.000Z",
                active: false,
                reused: false,
                raw_size: 345,
                output: prompt,
            },
            DemoSession {
                host: "web-01",
                started_at: "2026-10-12T17:41:40.000Z",
                active: false,
                reused: false,
                raw_size: 915,
                output: prompt,
            },
            DemoSession {
                host: "web-01",
                started_at: "2026-10-12T17:42:05.000Z",
                active: true,
                reused: true,
                raw_size: 1946,
                output: STATUS,
            },
        ],
    );
    let backend = env.backend.clone();
    run_demo(&env, move |window, cx| {
        open_history(backend.clone(), &query, None, window, cx).into()
    });
}
