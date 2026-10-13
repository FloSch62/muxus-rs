//! The settings window on a sandboxed backend, seeded like the documentation
//! screenshots (`hack/capture.mjs` BASE_PREFS plus the keyword rules of its
//! highlighting shot, session logging switched on as `hack/demo-env.mjs`
//! does). Nothing touches the real home directory or data folder.
//!
//! ```bash
//! hack/shot.sh --size 1000x720 --env MUXUS_WINDOW=fullscreen \
//!   --env MUXUS_THEME=dark --env MUXUS_SETTINGS_SECTION=terminal out.png \
//!   -- target/debug/examples/settings
//! ```
//!
//! - `MUXUS_THEME=light|dark|os` (default light)
//! - `MUXUS_SETTINGS_SECTION=<id>` (`appearance`, `terminal`, `local-shells`, …)
//! - `MUXUS_SETTINGS_SCROLL=<px>` scrolls the section down that far
//! - `MUXUS_SETTINGS_SHORTCUTS=1` opens the keyboard shortcuts editor
//! - `MUXUS_WINDOW=fullscreen` fills the monitor (as the capture viewport does)
//! - `MUXUS_DATA_DIR=<dir>` keeps the sandbox there instead of a temp dir

use std::path::PathBuf;
use std::time::Duration;

use muxus_app::features::settings::view::SettingsWindow;
use muxus_app::features::settings::{
    SettingsPrefs, SettingsSection, SettingsTarget, open_settings_window,
};
use muxus_core::api::{
    KeywordHighlightRule, SessionLoggingPolicyInput, X11Defaults, X11Source, X11Status,
};
use muxus_session::{Backend, BackendOptions};
use muxus_store::prefs::PrefsState;
use muxus_store::prefs::state::{FolderStyle, ThemeMode};
use muxus_ui::gpui_kit::{point, px};
use muxus_ui::theme::Mode;

fn scratch_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MUXUS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let dir = std::env::temp_dir().join(format!("muxus-settings-example-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn rule(
    id: &str,
    keyword: &str,
    fg: &str,
    bg: &str,
    case: bool,
    whole: bool,
) -> KeywordHighlightRule {
    serde_json::from_value(serde_json::json!({
        "id": id, "keyword": keyword, "foreground": fg, "background": bg,
        "caseSensitive": case, "wholeWord": whole,
    }))
    .expect("a valid rule")
}

/// `BASE_PREFS` of the capture script.
fn screenshot_prefs(prefs: &mut PrefsState, mode: ThemeMode) {
    prefs.theme_mode = mode;
    prefs.mono_font_size = 14.;
    prefs.font_family = "JetBrains Mono".into();
    prefs.line_height = 1.1;
    prefs.light_terminal_scheme = "github-light".into();
    prefs.dark_terminal_scheme = "vscode-dark".into();
    prefs.sidebar_width = 250.;
    prefs.command_buttons = Vec::new();
    prefs.keyword_highlights = vec![
        rule("r1", "ERROR", "#ffffff", "#dc2626", true, true),
        rule("r2", "WARN", "#1c1c21", "#f6c344", true, true),
        rule("r3", "timeout", "#ffffff", "#2563eb", false, false),
    ];
    for (key, color, icon) in [
        ("folder:production", "#ef4444", "cloud"),
        ("folder:production/web", "#3b82f6", "server"),
        ("folder:production/data", "#f97316", "storage"),
        ("folder:lab", "#22c55e", "lab"),
        ("folder:lab/fabric", "#a855f7", "lan"),
    ] {
        prefs.sidebar_folder_styles.insert(
            key.into(),
            FolderStyle {
                color: Some(color.into()),
                icon: Some(icon.into()),
            },
        );
    }
}

/// The demo hosts of `hack/demo-env.mjs`: seven OpenSSH hosts, four saved
/// hosts and three tunnels (the Backup & data summary counts them).
const DEMO_SSH_CONFIG: &str = "\
Host bastion
  HostName bastion.example.net
  User jump

Host build-01
  HostName build-01.lab.internal
  User ci
  IdentityFile ~/.ssh/id_ed25519

Host cache-01
  HostName cache-01.prod.internal
  User deploy
  IdentityFile ~/.ssh/id_ed25519

Host db-01
  HostName db-01.prod.internal
  User postgres
  ProxyJump bastion
  IdentityFile ~/.ssh/id_ed25519

Host db-primary
  HostName db-primary.prod.internal
  User postgres
  Port 22
  IdentityFile ~/.ssh/id_prod_ed25519
  ProxyJump bastion
  LocalForward 15432 127.0.0.1:5432
  ForwardAgent yes
  ServerAliveInterval 30

Host web-01
  HostName web-01.prod.internal
  User deploy
  IdentityFile ~/.ssh/id_ed25519
  LocalForward 8080 127.0.0.1:8080

Host web-02
  HostName web-02.prod.internal
  User deploy
  IdentityFile ~/.ssh/id_ed25519
";

fn demo_saved_hosts() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"name": "core-switch (console)", "profile": {"kind": "telnet", "host": "192.0.2.20", "port": 23}}),
        serde_json::json!({"name": "design-vm", "profile": {"kind": "vnc", "host": "design-vm.lab.internal", "port": 5900}}),
        serde_json::json!({"name": "lab-console", "profile": {"kind": "serial", "path": "/dev/ttyUSB0", "baudRate": 115200, "dataBits": 8, "stopBits": 1, "parity": "none", "flowControl": "none"}}),
        serde_json::json!({"name": "win-build", "profile": {"kind": "rdp", "host": "win-build.corp.internal", "port": 3389, "username": "CORP\\ci", "sshGateway": {"target": "bastion"}}}),
    ]
}

fn demo_tunnels() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"name": "Grafana", "target": "web-01", "type": "local", "bindPort": 3000, "targetHost": "127.0.0.1", "targetPort": 3000}),
        serde_json::json!({"name": "Lab SOCKS proxy", "target": "bastion", "type": "dynamic", "bindPort": 1080}),
        serde_json::json!({"name": "Postgres (via bastion)", "target": "bastion", "type": "local", "bindPort": 15432, "targetHost": "db-01.prod.internal", "targetPort": 5432}),
    ]
}

async fn seed_demo(backend: &Backend) {
    for host in demo_saved_hosts() {
        match serde_json::from_value(host) {
            Ok(input) => {
                if let Err(error) = backend.save_saved_host(input).await {
                    eprintln!("could not seed a saved host: {error}");
                }
            }
            Err(error) => eprintln!("bad demo host: {error}"),
        }
    }
    for tunnel in demo_tunnels() {
        match serde_json::from_value(tunnel) {
            Ok(input) => {
                if let Err(error) = backend.save_tunnel(input).await {
                    eprintln!("could not seed a tunnel: {error}");
                }
            }
            Err(error) => eprintln!("bad demo tunnel: {error}"),
        }
    }
}

fn main() {
    let mode = match std::env::var("MUXUS_THEME").as_deref() {
        Ok("dark") => ThemeMode::Dark,
        Ok("os") => ThemeMode::Os,
        _ => ThemeMode::Light,
    };
    let section = std::env::var("MUXUS_SETTINGS_SECTION")
        .ok()
        .and_then(|id| SettingsSection::parse(&id))
        .unwrap_or_default();
    let scroll: f32 = std::env::var("MUXUS_SETTINGS_SCROLL")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0.);
    let shortcuts = std::env::var("MUXUS_SETTINGS_SHORTCUTS").is_ok_and(|value| value == "1");
    let fullscreen = std::env::var("MUXUS_WINDOW").is_ok_and(|value| value == "fullscreen");

    let scratch = scratch_dir();
    let home = scratch.join("home");
    let _ = std::fs::create_dir_all(home.join(".ssh"));
    let _ = std::fs::write(home.join(".ssh").join("config"), DEMO_SSH_CONFIG);
    let backend = Backend::start_blocking(BackendOptions::sandbox(scratch.join("data"), &home))
        .expect("start the sandboxed backend");
    let prefs = futures_lite::future::block_on(async {
        let mut prefs = backend.prefs().await.expect("read prefs");
        screenshot_prefs(&mut prefs, mode);
        backend.set_prefs(prefs.clone()).await.expect("seed prefs");
        seed_demo(&backend).await;
        // Logging is off on a fresh install; the demo turns it on.
        let _ = backend
            .put_session_logging_policy(
                "*",
                SessionLoggingPolicyInput {
                    enabled: true,
                    capture_input: false,
                    max_part_bytes: 5 * 1024 * 1024,
                    max_parts: 10,
                    log_to_file: Some(false),
                },
            )
            .await;
        prefs
    });

    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            muxus_ui::init(
                if mode == ThemeMode::Dark {
                    Mode::Dark
                } else {
                    Mode::Light
                },
                cx,
            );
            SettingsPrefs::init(backend.clone(), prefs.clone(), cx);
            let Some((handle, view)) =
                open_settings_window(Some(SettingsTarget::section(section)), cx)
            else {
                eprintln!("the settings window did not open");
                cx.quit();
                return;
            };
            // The demo sandbox shows the Windows app's built-in X server.
            view.update(cx, |view: &mut SettingsWindow, cx| {
                view.pin_x11_status(X11Status {
                    enabled: true,
                    forward_by_default: true,
                    clipboard: false,
                    source: X11Source::Bundled,
                    display: None,
                    defaults: X11Defaults {
                        enabled: true,
                        forward_by_default: true,
                    },
                });
                cx.notify();
            });
            let _ = handle.update(cx, |_, window, cx| {
                if fullscreen {
                    window.toggle_fullscreen();
                }
                if shortcuts {
                    view.update(cx, |view, cx| view.open_shortcuts(window, cx));
                }
            });
            if scroll > 0. {
                let view = view.downgrade();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(1500))
                        .await;
                    let _ = view.update(cx, |view, cx| {
                        view.scroll.set_offset(point(px(0.), px(-scroll)));
                        cx.notify();
                    });
                })
                .detach();
            }
        });
}
