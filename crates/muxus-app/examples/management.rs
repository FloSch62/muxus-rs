//! The NETCONF/gNMI workbench on its own, connected to a fake gNMI device
//! (an SR Linux–flavoured data tree, see `support/device.rs`), framed where
//! the main window puts a session pane so screenshots line up with the
//! Electron build's.
//!
//! ```text
//! MUXUS_DATA_DIR=<scratch> cargo run -p muxus-app --example management
//! ```
//!
//! - `MUXUS_THEME=light|dark`, `MUXUS_WINDOW=fullscreen|WxH` as for the app.
//! - `MGMT_SCENE=workbench|get|table|raw|history|device|live|set|review|operations|security|error`
//!   puts the workbench into the state a reference screenshot shows.
//! - `MGMT_DEVICE=host:port` uses that gNMI device (plain text) instead of the fake one.
//! - `MGMT_DEVICE_PORT` is the fake device's port (57401, as the reference captures use).
//! - `MGMT_FRAME=0` drops the window frame stand-in and gives the pane the whole window.

#[path = "support/device.rs"]
mod device;

use muxus_app::features::management::{
    ManagementPrompt, ManagementView, ManagementViewEvent, open_management_view,
};
use muxus_core::protocol::{AuthPromptResponse, ManagementProfile};
use muxus_session::{Backend, BackendOptions, RuntimeChoice};
use muxus_ui::gpui_kit::{
    AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Render, Styled,
    TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions, div, point, px, size,
};
use muxus_ui::theme::{ActiveTheme, Mode};

/// Where the main window puts a session pane at 1420×880: below the top bar
/// (52) and the tab strip (40 + its border), right of the 250px sidebar,
/// above the 26px status bar.
const PANE_LEFT: f32 = 250.;
const PANE_TOP: f32 = 93.;
const STATUS_BAR: f32 = 26.;

struct Frame {
    view: Entity<ManagementView>,
    framed: bool,
}

impl Render for Frame {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        if !self.framed {
            return div().size_full().child(self.view.clone());
        }
        div().size_full().relative().bg(p.sidebar).child(
            div()
                .absolute()
                .left(px(PANE_LEFT))
                .top(px(PANE_TOP))
                .right_0()
                .bottom(px(STATUS_BAR))
                .child(self.view.clone()),
        )
    }
}

fn main() {
    let theme = match std::env::var("MUXUS_THEME").as_deref() {
        Ok("dark") => Mode::Dark,
        _ => Mode::Light,
    };
    let scene = std::env::var("MGMT_SCENE").unwrap_or_default();
    let framed = std::env::var("MGMT_FRAME").as_deref() != Ok("0");
    let scratch = std::env::var_os("MUXUS_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("muxus-mgmt-example-{}", std::process::id()))
        });
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).expect("scratch home");
    let mut options = BackendOptions::sandbox(scratch.join("data"), home);
    options.runtime = RuntimeChoice::Own { worker_threads: 2 };
    let backend = Backend::start_blocking(options).expect("start the backend");

    let (host, port) = match std::env::var("MGMT_DEVICE") {
        Ok(address) => {
            let (host, port) = address.rsplit_once(':').expect("host:port");
            (host.to_owned(), port.parse().expect("a port"))
        }
        Err(_) => {
            let _runtime = backend.runtime().enter();
            let port = backend.runtime().block_on(
                device::Device::new().listen(
                    std::env::var("MGMT_DEVICE_PORT")
                        .ok()
                        .and_then(|port| port.parse().ok())
                        .unwrap_or(57401),
                ),
            );
            ("127.0.0.1".to_owned(), port)
        }
    };
    let profile: ManagementProfile = serde_json::from_value(serde_json::json!({
        "kind": "gnmi",
        "host": host,
        "port": port,
        "tls": "plaintext",
    }))
    .expect("a gNMI profile");

    let fullscreen = std::env::var("MUXUS_WINDOW").as_deref() == Ok("fullscreen");
    let (width, height) = std::env::var("MUXUS_WINDOW")
        .ok()
        .and_then(|value| {
            let (w, h) = value.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1420., 880.));

    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            muxus_ui::init(theme, cx);
            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
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
            let profile = profile.clone();
            let scene = scene.clone();
            muxus_ui::gpui_kit::open_window(options, cx, move |window, cx| {
                if fullscreen {
                    window.toggle_fullscreen();
                }
                let view = open_management_view(backend, profile, "leaf1", true, window, cx);
                // The fake device takes any login; skip it as the reference does.
                cx.subscribe(&view, |view, event: &ManagementViewEvent, cx| {
                    if let ManagementViewEvent::Prompt(ManagementPrompt::Auth(_)) = event {
                        view.update(cx, |view, cx| {
                            view.answer_auth(
                                Some(AuthPromptResponse {
                                    answers: Vec::new(),
                                    remember_password: None,
                                    skipped: Some(true),
                                }),
                                cx,
                            )
                        });
                    }
                })
                .detach();
                if !scene.is_empty() {
                    view.update(cx, |view, cx| {
                        view.apply_scene_when_ready(scene, window, cx)
                    });
                }
                cx.new(|_| Frame { view, framed })
            })
            .expect("open the window");
        });
}
