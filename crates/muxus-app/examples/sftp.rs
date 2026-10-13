//! The file browser and remote editor against an in-process SSH/SFTP server
//! seeded like `hack/demo-env.mjs` (web-01's `/home/deploy`), placed where the
//! documentation screenshots show them.
//!
//! ```text
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_SCENE=sftp out.png -- \
//!     target-ui-b/debug/examples/sftp
//! ```
//!
//! `MUXUS_SCENE`: `sftp` (beside the terminal, `sftp.png`), `remote-editor`
//! (`remote-editor.png`), `sidebar` (docked in the sidebar), `window` (the
//! SFTP window). `MUXUS_THEME=dark` for the dark variants. Everything lives in
//! a scratch folder; nothing touches the real home directory.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{Local, TimeZone};
use muxus_app::features::sftp::dialogs::DialogHost;
use muxus_app::features::sftp::editor::workspace::{
    EditorSource, RemoteEditorOptions, RemoteEditorWorkspace,
};
use muxus_app::features::sftp::panel::{SftpPanel, SftpPanelEvent, SftpPanelOptions};
use muxus_app::features::sftp::window::{SftpWindow, SftpWindowLaunch};
use muxus_app::features::sftp::{self, services};
use muxus_core::protocol::{AuthPromptResponse, TerminalClientMessage, TerminalServerMessage};
use muxus_session::{Backend, BackendOptions, RuntimeChoice, SessionEvent, SessionHandle};
use muxus_ssh::testing::server::{ServerSpec, TestServer, start as start_sshd};
use muxus_ssh::testing::sftp_server::{SftpServerOptions, register};
use muxus_ui::gpui_kit::{
    App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Render, Styled,
    TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions, div, point, px, size,
};
use muxus_ui::theme::{ActiveTheme, Mode};

const DOCKER_COMPOSE: &str = r#"services:
  edge:
    image: ghcr.io/acme/edge:2.8.1
    restart: unless-stopped
    ports:
      - "80:8080"
    environment:
      LOG_LEVEL: info
      UPSTREAM: http://cache-01:6379
    healthcheck:
      test: ["CMD", "curl", "-fsS", "http://localhost:8080/healthz"]
      interval: 10s
"#;

const DEPLOY_SH: &str = r#"#!/usr/bin/env bash
set -euo pipefail

RELEASE="${1:-latest}"
echo "==> pulling edge:${RELEASE}"
docker compose pull edge
echo "==> restarting"
docker compose up -d --no-deps edge
docker compose ps
"#;

const README: &str = "# web-01

Front-end node behind the load balancer.

- `deploy.sh <tag>` rolls a new edge release
- `docker compose logs -f edge` tails the access log
- metrics: http://web-01:9100/metrics
";

/// web-01's home as the demo shows it, every entry dated 2026-07-25 20:17.
fn seed_remote(root: &Path) -> PathBuf {
    let home = root.join("home/deploy");
    for dir in ["backups", "bin", "conf", "logs"] {
        std::fs::create_dir_all(home.join(dir)).unwrap();
    }
    std::fs::write(home.join("docker-compose.yml"), DOCKER_COMPOSE).unwrap();
    std::fs::write(home.join("deploy.sh"), DEPLOY_SH).unwrap();
    std::fs::write(home.join("README.md"), README).unwrap();
    std::fs::write(
        home.join("conf/edge.env"),
        "LOG_LEVEL=info\nUPSTREAM=http://cache-01:6379\nTIMEOUT=5s\n",
    )
    .unwrap();
    std::fs::write(home.join("logs/error.log"), "no errors in the last 24h\n").unwrap();
    let when = Local
        .with_ymd_and_hms(2026, 7, 25, 20, 17, 0)
        .single()
        .unwrap();
    let time = std::time::UNIX_EPOCH + Duration::from_secs(when.timestamp() as u64);
    for name in [
        "backups",
        "bin",
        "conf",
        "logs",
        "docker-compose.yml",
        "deploy.sh",
        "README.md",
    ] {
        std::fs::File::open(home.join(name))
            .unwrap()
            .set_modified(time)
            .unwrap();
    }
    home
}

struct Demo {
    backend: Backend,
    conn_id: String,
    _server: TestServer,
    _session: SessionHandle,
    _runtime: tokio::runtime::Runtime,
}

/// Start the server and the backend, sign in to web-01 and return its connection.
fn start_demo(scratch: &Path) -> Demo {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let remote = scratch.join("remote");
    seed_remote(&remote);
    register(
        &remote,
        SftpServerOptions {
            home: "/home/deploy".into(),
            owner: "deploy".into(),
            group: "deploy".into(),
            ..SftpServerOptions::default()
        },
    );
    let server = runtime.block_on(start_sshd(ServerSpec {
        sftp_root: Some(remote.clone()),
        ..ServerSpec::new()
            .password("pw")
            .methods(&[russh::MethodKind::Password])
    }));
    let home = scratch.join("home");
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(
        home.join(".ssh/config"),
        format!(
            "Host web-01\n  HostName 127.0.0.1\n  User deploy\n  Port {}\n",
            server.port()
        ),
    )
    .unwrap();
    let mut options = BackendOptions::sandbox(scratch.join("data"), &home);
    options.runtime = RuntimeChoice::Handle(runtime.handle().clone());
    let backend = runtime
        .block_on(Backend::start(options))
        .expect("the backend starts");
    let connect: TerminalClientMessage = serde_json::from_value(serde_json::json!({
        "op": "connect",
        "profile": { "kind": "ssh", "target": "web-01" },
        "cols": 80,
        "rows": 24
    }))
    .unwrap();
    let session = backend.open_session(connect);
    let conn_id = runtime.block_on(async {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(20), session.next_event())
                .await
                .expect("the session answers")
                .expect("the session stays open");
            if let SessionEvent::Message(message) = event {
                match message {
                    TerminalServerMessage::HostKey(_) => {
                        session.send(TerminalClientMessage::HostKeyResponse { accept: true })
                    }
                    TerminalServerMessage::AuthPrompt(_) => {
                        session.send(TerminalClientMessage::AuthResponse(AuthPromptResponse {
                            answers: vec!["pw".into()],
                            remember_password: None,
                            skipped: None,
                        }))
                    }
                    TerminalServerMessage::Ready { conn_id, .. } => break conn_id,
                    TerminalServerMessage::Exit { message, .. } => {
                        panic!("the demo session ended: {message:?}")
                    }
                    _ => {}
                }
            }
        }
    });
    Demo {
        backend,
        conn_id,
        _server: server,
        _session: session,
        _runtime: runtime,
    }
}

/// Where the reference screenshot has the file browser: below the top bar and
/// the tab strip, against the right edge.
struct Scene {
    panel: Entity<SftpPanel>,
    workspace: Option<Entity<RemoteEditorWorkspace>>,
    dialogs: Entity<DialogHost>,
    sidebar: bool,
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.theme().palette;
        let chrome = div()
            .h(px(93.))
            .flex_none()
            .bg(p.sidebar)
            .border_b_1()
            .border_color(p.divider);
        let body = if self.sidebar {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .w(px(250.))
                        .h_full()
                        .flex_none()
                        .bg(p.sidebar)
                        .border_r_1()
                        .border_color(p.divider)
                        .child(self.panel.clone()),
                )
                .child(div().flex_1().h_full().bg(p.background_default))
        } else if let Some(workspace) = &self.workspace {
            // The documentation shot: the 250px sidebar, the editor in the pane.
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .w(px(251.))
                        .h_full()
                        .flex_none()
                        .bg(p.sidebar)
                        .border_r_1()
                        .border_color(p.divider),
                )
                .child(div().flex_1().min_w_0().h_full().child(workspace.clone()))
                .child(self.panel.clone())
        } else {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(div().flex_1().h_full().bg(p.background_default))
                .child(self.panel.clone())
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.background_default)
            .child(chrome)
            .child(body)
            .child(self.dialogs.clone())
    }
}

/// The window's root: a placed scene, or the SFTP window itself.
enum Wrapper {
    Scene(Entity<Scene>),
    Window(Entity<SftpWindow>),
}

impl Render for Wrapper {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        match self {
            Wrapper::Scene(scene) => div().size_full().child(scene.clone()),
            Wrapper::Window(window) => div().size_full().child(window.clone()),
        }
    }
}

fn main() {
    let scratch = tempfile::tempdir().unwrap();
    let demo = start_demo(scratch.path());
    let scene = std::env::var("MUXUS_SCENE").unwrap_or_else(|_| "sftp".into());
    let mode = match std::env::var("MUXUS_THEME").as_deref() {
        Ok("dark") => Mode::Dark,
        _ => Mode::Light,
    };
    let fullscreen = std::env::var("MUXUS_WINDOW").as_deref() == Ok("fullscreen");
    let backend = demo.backend.clone();
    let conn_id = demo.conn_id.clone();
    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx: &mut App| {
            muxus_ui::init(mode, cx);
            services::init(
                backend.clone(),
                std::rc::Rc::new(|status, message, details, _| {
                    eprintln!("[{status:?}] {message} {}", details.unwrap_or_default());
                }),
                cx,
            );
            let bounds = Bounds::centered(None, size(px(1420.), px(880.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Muxus".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(18.))),
                }),
                window_decorations: Some(WindowDecorations::Client),
                app_id: Some("muxus".into()),
                ..Default::default()
            };
            let conn_id = conn_id.clone();
            let scene = scene.clone();
            muxus_ui::gpui_kit::open_window(options, cx, move |window, cx| {
                if fullscreen {
                    window.toggle_fullscreen();
                }
                if scene == "window" {
                    let launch = SftpWindowLaunch {
                        conn_id: conn_id.clone(),
                        title: "web-01".into(),
                        path: "/home/deploy".into(),
                    };
                    let root = cx.new(|cx| SftpWindow::new(launch, window, cx));
                    return cx.new(|_| Wrapper::Window(root));
                }
                let sidebar = scene == "sidebar";
                let mut panel_options = SftpPanelOptions::new(conn_id.clone());
                panel_options.host_label = Some("web-01".into());
                panel_options.opens_in_new_window = true;
                if sidebar {
                    panel_options.movable = true;
                    panel_options.in_sidebar = true;
                    panel_options.follow_toggle = true;
                    panel_options.follow_terminal_folder = true;
                }
                let panel = cx.new(|cx| SftpPanel::new(panel_options, window, cx));
                let dialogs = cx.new(|_| DialogHost::new());
                sftp::local_open::install_dialog_host(dialogs.clone(), cx);
                let workspace = (scene == "remote-editor").then(|| {
                    let mut options = RemoteEditorOptions::new(
                        "tab-web-01",
                        EditorSource::Sftp(Some(conn_id.clone())),
                    );
                    options.active = true;
                    cx.new(|cx| RemoteEditorWorkspace::new(options, window, cx))
                });
                if let Some(workspace) = workspace.clone() {
                    let target = workspace.clone();
                    window
                        .subscribe(&panel, cx, move |_, event: &SftpPanelEvent, window, cx| {
                            if let SftpPanelEvent::OpenFile(path) = event {
                                target.update(cx, |workspace, cx| {
                                    workspace.open(path.clone(), window, cx)
                                });
                            }
                        })
                        .detach();
                    // As the shot does: double-click docker-compose.yml.
                    workspace.update(cx, |workspace, cx| {
                        workspace.open("/home/deploy/docker-compose.yml".into(), window, cx)
                    });
                    panel.update(cx, |panel, cx| panel.select("docker-compose.yml", cx));
                }
                let scene = cx.new(|_| Scene {
                    panel,
                    workspace,
                    dialogs,
                    sidebar,
                });
                cx.new(|_| Wrapper::Scene(scene))
            })
            .expect("open the window");
        });
    drop(demo);
}
