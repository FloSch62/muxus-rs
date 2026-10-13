//! An RDP or VNC tab against an in-process server showing the documentation's invented
//! desktop (`hack/demo-vnc.mjs`), placed where `docs/assets/screenshots/remote-desktop.png`
//! shows the pane. The window chrome around it (sidebar, tab strip) is the app's; here it is
//! a plain surface.
//!
//! ```text
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen out.png -- target-ui-c/debug/examples/remote_desktop
//! ```
//!
//! - `MUXUS_THEME=light|dark` (default light)
//! - `MUXUS_DESKTOP=vnc|rdp` (default vnc): the RFB server of the sandbox, or IronRDP's
//!   server (TLS, logon `demo`/`demo`, a self-signed certificate that is accepted)
//! - `MUXUS_DESKTOP_STATE=connected|connecting|ended|idle` (default connected): `connecting`
//!   leaves the logon or certificate question unanswered, `ended` dials a closed port,
//!   `idle` waits for Connect
//! - `MUXUS_DESKTOP_TOOLBAR=pinned|auto` (default pinned): keep the toolbar in view, as the
//!   screenshot caught it right after connecting
//! - `MUXUS_WINDOW=fullscreen` fills the monitor; `MUXUS_DATA_DIR=<dir>` keeps the sandbox
//!
//! Nothing touches the real home directory or data folder; the desktop host names resolve
//! to the loopback servers through `MUXUS_DEMO_HOSTMAP`, as `hack/demo-resolve.mjs` did.

mod demo_desktop;
mod demo_rdp;
mod demo_vnc;

use std::path::PathBuf;
use std::sync::Arc;

use muxus_app::features::remote_desktop::{
    self, DesktopPrompt, RemoteDesktopView, RemoteDesktopViewEvent, open_remote_desktop_view,
};
use muxus_core::protocol::{AuthPromptPurpose, AuthPromptResponse, DesktopProfile};
use muxus_session::desktop::DEMO_HOST_MAP_ENV;
use muxus_session::{Backend, BackendOptions};
use muxus_ui::gpui_kit::{
    App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Subscription, TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions, div,
    point, px, size,
};
use muxus_ui::theme::{ActiveTheme, Mode};

/// The pane of the documentation screenshot: right of the 250px sidebar and its divider,
/// below the tab strip and its border.
const PANE_LEFT: f32 = 250.;
const PANE_TOP: f32 = 93.;

fn scratch_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MUXUS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let dir = std::env::temp_dir().join(format!(
        "muxus-remote-desktop-example-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The window: chrome-coloured surface, the desktop tab in the pane.
struct Shell {
    view: Entity<RemoteDesktopView>,
    _events: Subscription,
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.theme().palette;
        div()
            .relative()
            .size_full()
            .bg(p.sidebar)
            .child(
                div()
                    .absolute()
                    .left(px(PANE_LEFT - 1.))
                    .top_0()
                    .bottom_0()
                    .w(px(1.))
                    .bg(p.divider),
            )
            .child(
                div()
                    .absolute()
                    .left(px(PANE_LEFT))
                    .right_0()
                    .top(px(PANE_TOP - 1.))
                    .h(px(1.))
                    .bg(p.divider),
            )
            .child(
                div()
                    .absolute()
                    .left(px(PANE_LEFT))
                    .top(px(PANE_TOP))
                    .right_0()
                    .bottom_0()
                    .child(self.view.clone()),
            )
    }
}

fn main() {
    let mode = match std::env::var("MUXUS_THEME").as_deref() {
        Ok("dark") => Mode::Dark,
        _ => Mode::Light,
    };
    let rdp = std::env::var("MUXUS_DESKTOP").as_deref() == Ok("rdp");
    let state = std::env::var("MUXUS_DESKTOP_STATE").unwrap_or_else(|_| "connected".into());
    let pinned = std::env::var("MUXUS_DESKTOP_TOOLBAR").as_deref() != Ok("auto");
    let fullscreen = std::env::var("MUXUS_WINDOW").as_deref() == Ok("fullscreen");

    // The servers' ports must be known before anything else runs: the host map is read from
    // the environment.
    let desktop = Arc::new(demo_desktop::draw(1132, 786));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = if state == "ended" {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        closed.local_addr().unwrap().port()
    } else {
        listener.local_addr().unwrap().port()
    };
    let host = if rdp {
        "win-build.corp.internal"
    } else {
        "design-vm.lab.internal"
    };
    // SAFETY: no other thread has started yet.
    unsafe {
        std::env::set_var(
            DEMO_HOST_MAP_ENV,
            serde_json::json!({ host: port }).to_string(),
        );
    }

    let scratch = scratch_dir();
    let home = scratch.join("home");
    let _ = std::fs::create_dir_all(&home);
    let backend = Backend::start_blocking(BackendOptions::sandbox(scratch.join("data"), &home))
        .expect("start the sandboxed backend");
    if rdp {
        demo_rdp::serve(listener, desktop);
    } else {
        let _guard = backend.runtime().enter();
        demo_vnc::serve(listener, desktop, "design-vm");
    }
    let profile: DesktopProfile = serde_json::from_value(if rdp {
        serde_json::json!({ "kind": "rdp", "host": host, "port": 3389, "username": demo_rdp::USER })
    } else {
        serde_json::json!({ "kind": "vnc", "host": host, "port": 5900 })
    })
    .expect("a desktop profile");
    let title = if rdp { "win-build" } else { "design-vm" };

    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx: &mut App| {
            muxus_ui::init(mode, cx);
            remote_desktop::init(cx);
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
            let answer = state != "connecting";
            let connect = state != "idle";
            let opened = muxus_ui::gpui_kit::open_window(options, cx, move |window, cx| {
                if fullscreen {
                    window.toggle_fullscreen();
                }
                let view = open_remote_desktop_view(backend, profile, title, connect, window, cx);
                if pinned {
                    view.update(cx, |view, cx| view.pin_toolbar(true, cx));
                }
                let events = cx.subscribe(&view, move |view, event, cx| match event {
                    RemoteDesktopViewEvent::Prompt(prompt) if answer => {
                        // The app's dialogs answer these; the sandbox answers for the user.
                        view.update(cx, |view, cx| match prompt {
                            DesktopPrompt::Auth(info) => {
                                let skip = matches!(
                                    info.purpose,
                                    Some(AuthPromptPurpose::VaultCreate)
                                        | Some(AuthPromptPurpose::VaultUnlock)
                                );
                                view.answer_auth(
                                    Some(AuthPromptResponse {
                                        answers: if skip {
                                            Vec::new()
                                        } else {
                                            vec![demo_rdp::PASSWORD.into()]
                                        },
                                        remember_password: None,
                                        skipped: skip.then_some(true),
                                    }),
                                    cx,
                                );
                            }
                            DesktopPrompt::HostKey(_) => view.answer_host_key(true, cx),
                            DesktopPrompt::Certificate(_) => view.answer_certificate(true, cx),
                        });
                    }
                    RemoteDesktopViewEvent::Prompt(prompt) => eprintln!("prompt: {prompt:?}"),
                    RemoteDesktopViewEvent::Status {
                        status,
                        failure_reason,
                        ..
                    } => eprintln!("tab: {status:?} {failure_reason:?}"),
                    RemoteDesktopViewEvent::Toast { message, .. } => eprintln!("toast: {message}"),
                    RemoteDesktopViewEvent::PromptsClosed => {}
                });
                cx.new(|_| Shell {
                    view,
                    _events: events,
                })
            });
            if let Err(error) = opened {
                eprintln!("could not open the window: {error}");
                cx.quit();
            }
        });
}
