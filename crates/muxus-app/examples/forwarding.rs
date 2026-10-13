//! The port forwarding panel on the documentation's demo sandbox: the saved
//! tunnels of `hack/demo-env.mjs` (Grafana, Postgres via bastion, the lab
//! SOCKS proxy) and a live web-01 connection whose ssh_config LocalForward is
//! running. web-01 is an in-process SSH server reached through a
//! `ProxyCommand`, so the panel shows its real host name. Nothing touches the
//! real home directory.
//!
//! ```text
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen out.png -- target-ui-b/debug/examples/forwarding
//! ```
//!
//! `MUXUS_SCENE`: `panel` (default, `tunnels.png`), `menu` (a tunnel's
//! options), `adhoc` ("Forward on web-01"). `MUXUS_THEME=dark` for the dark
//! variant.

use std::path::Path;
use std::time::Duration;

use muxus_app::features::forwarding::panel::{ForwardingPanel, ForwardingPanelEvent};
use muxus_app::features::host_editor::demo::{DemoSandbox, seed};
use muxus_core::protocol::{AuthPromptResponse, TerminalClientMessage, TerminalServerMessage};
use muxus_session::{Backend, BackendOptions, RuntimeChoice, SessionEvent, SessionHandle};
use muxus_ssh::testing::server::{ServerSpec, TestServer, start as start_sshd};
use muxus_ui::gpui_kit::{
    App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Render, Styled,
    TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions, div, point, px, size,
};
use muxus_ui::theme::{ActiveTheme, Mode};

struct Demo {
    backend: Backend,
    _server: TestServer,
    _session: SessionHandle,
    _runtime: tokio::runtime::Runtime,
}

/// Point web-01 at the in-process server, keeping its demo host name.
fn route_web_01(home: &Path, port: u16) {
    let path = home.join(".ssh/config");
    let config = std::fs::read_to_string(&path).unwrap();
    let config = config.replacen(
        "    HostName web-01.prod.internal\n",
        &format!("    HostName web-01.prod.internal\n    ProxyCommand nc 127.0.0.1 {port}\n"),
        1,
    );
    std::fs::write(path, config).unwrap();
}

fn start_demo(scratch: &Path) -> Demo {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(start_sshd(
        ServerSpec::new()
            .password("pw")
            .methods(&[russh::MethodKind::Password]),
    ));
    let sandbox = DemoSandbox::create(scratch).unwrap();
    route_web_01(&sandbox.home, server.port());
    let mut options = BackendOptions::sandbox(&sandbox.data, &sandbox.home);
    options.autostart_tunnels = false;
    options.runtime = RuntimeChoice::Handle(runtime.handle().clone());
    let backend = runtime.block_on(Backend::start(options)).unwrap();
    runtime.block_on(seed(&backend)).unwrap();
    let connect: TerminalClientMessage = serde_json::from_value(serde_json::json!({
        "op": "connect",
        "profile": { "kind": "ssh", "target": "web-01" },
        "cols": 80,
        "rows": 24
    }))
    .unwrap();
    let session = backend.open_session(connect);
    runtime.block_on(async {
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
                    TerminalServerMessage::Ready { .. } => break,
                    TerminalServerMessage::Exit { message, .. } => {
                        panic!("the demo session ended: {message:?}")
                    }
                    _ => {}
                }
            }
        }
    });
    // Let the config forward come up.
    std::thread::sleep(Duration::from_millis(500));
    Demo {
        backend,
        _server: server,
        _session: session,
        _runtime: runtime,
    }
}

/// The app's frame around the panel: the 52px top bar and the canvas.
struct Scene {
    panel: Entity<ForwardingPanel>,
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.theme().palette;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.background_default)
            .child(
                div()
                    .h(px(52.))
                    .flex_none()
                    .bg(p.sidebar)
                    .border_b_1()
                    .border_color(p.divider),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().flex_1().h_full().bg(p.background_default))
                    .child(self.panel.clone()),
            )
    }
}

fn main() {
    let scratch = tempfile::tempdir().unwrap();
    let demo = start_demo(scratch.path());
    let scene = std::env::var("MUXUS_SCENE").unwrap_or_else(|_| "panel".into());
    let mode = match std::env::var("MUXUS_THEME").as_deref() {
        Ok("dark") => Mode::Dark,
        _ => Mode::Light,
    };
    let fullscreen = std::env::var("MUXUS_WINDOW").as_deref() == Ok("fullscreen");
    let backend = demo.backend.clone();
    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx: &mut App| {
            muxus_ui::init(mode, cx);
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
            let backend = backend.clone();
            let scene = scene.clone();
            muxus_ui::gpui_kit::open_window(options, cx, move |window, cx| {
                if fullscreen {
                    window.toggle_fullscreen();
                }
                let panel = cx.new(|cx| ForwardingPanel::new(backend.clone(), cx));
                window
                    .subscribe(&panel, cx, |_, event: &ForwardingPanelEvent, _, _| {
                        if let ForwardingPanelEvent::Prompt { .. } = event {
                            eprintln!("the demo tunnels need no sign-in");
                        }
                    })
                    .detach();
                if scene != "panel" {
                    // Wait for the lists, then open the scene's popup.
                    let target = panel.clone();
                    let scene = scene.clone();
                    window
                        .spawn(cx, async move |cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(1500))
                                .await;
                            let _ = cx.update(|window, cx| {
                                target.update(cx, |panel, cx| {
                                    if scene == "menu" {
                                        let id = panel
                                            .tunnels()
                                            .iter()
                                            .find(|tunnel| {
                                                tunnel.name.as_deref() == Some("Grafana")
                                            })
                                            .map(|tunnel| tunnel.id.clone());
                                        if let Some(id) = id {
                                            panel.show_menu(&id, cx);
                                        }
                                    } else if let Some(conn) = panel.connections().first().cloned()
                                    {
                                        panel.show_adhoc(&conn.id, window, cx);
                                    }
                                });
                            });
                        })
                        .detach();
                }
                cx.new(|_| Scene { panel })
            })
            .expect("open the window");
        });
    drop(demo);
}
