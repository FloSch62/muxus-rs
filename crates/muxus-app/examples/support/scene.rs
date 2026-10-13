//! What the import, forwarding, log, vault and notice examples share: the
//! theme and window choice from the environment, a sandboxed backend on the
//! documentation's demo data, and a window that shows one entity over the
//! app's background, the way the reference screenshots frame it.

#![allow(dead_code)]

use std::path::Path;

use muxus_app::features::host_editor::demo::{DemoSandbox, seed};
use muxus_session::{Backend, BackendOptions, RuntimeChoice};
use muxus_ui::gpui_kit::{
    AnyView, App, AppContext, Bounds, Context, IntoElement, ParentElement, Render, Styled,
    TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions, div, point, px, size,
};
use muxus_ui::theme::{ActiveTheme, Mode};

/// `MUXUS_THEME=dark` picks the dark theme.
pub fn mode() -> Mode {
    match std::env::var("MUXUS_THEME").as_deref() {
        Ok("dark") => Mode::Dark,
        _ => Mode::Light,
    }
}

/// `MUXUS_SCENE`, or `default`.
pub fn scene(default: &str) -> String {
    std::env::var("MUXUS_SCENE").unwrap_or_else(|_| default.to_owned())
}

/// A point from `MUXUS_AT=x,y` (CSS px), for popups placed where the
/// reference has them.
pub fn at() -> Option<(f32, f32)> {
    let value = std::env::var("MUXUS_AT").ok()?;
    let (x, y) = value.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// A backend on a fresh demo sandbox under `root` (hosts, folders, saved
/// hosts and tunnels of `hack/demo-env.mjs`), on its own runtime.
pub fn demo_backend(root: &Path) -> (Backend, tokio::runtime::Runtime) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let sandbox = DemoSandbox::create(root).expect("create the demo sandbox");
    let mut options = BackendOptions::sandbox(&sandbox.data, &sandbox.home);
    options.autostart_tunnels = false;
    options.runtime = RuntimeChoice::Handle(runtime.handle().clone());
    let backend = runtime
        .block_on(Backend::start(options))
        .expect("start the sandboxed backend");
    runtime
        .block_on(seed(&backend))
        .expect("seed the demo data");
    (backend, runtime)
}

/// The window's root: the app background with the scene's view on top.
pub struct Frame {
    pub view: AnyView,
}

impl Render for Frame {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.theme().palette;
        div()
            .size_full()
            .relative()
            .bg(p.background_default)
            .child(self.view.clone())
    }
}

/// Open the 1420×880 window (fullscreen with `MUXUS_WINDOW=fullscreen`)
/// showing what `build` returns.
pub fn open(cx: &mut App, build: impl FnOnce(&mut Window, &mut App) -> AnyView + 'static) {
    let fullscreen = std::env::var("MUXUS_WINDOW").as_deref() == Ok("fullscreen");
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
    muxus_ui::gpui_kit::open_window(options, cx, move |window, cx| {
        if fullscreen {
            window.toggle_fullscreen();
        }
        let view = build(window, cx);
        cx.new(|_| Frame { view })
    })
    .expect("open the window");
}
