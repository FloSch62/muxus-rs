//! The host editor on the documentation's demo sandbox (`hack/demo-env.mjs`
//! hosts, folders, saved hosts and tunnels in a scratch home), opened the way
//! the docs screenshots open it. Nothing touches the real home directory.
//!
//! ```bash
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_EDITOR_SCENE=route out.png \
//!   -- target-ui-a/debug/examples/host_editor
//! ```
//!
//! - `MUXUS_THEME=light|dark` (default light)
//! - `MUXUS_EDITOR_SCENE`: `general` (db-primary, the default), `route`,
//!   `forwards`, `advanced`, `auth`, `appearance`, `login`, `logging`,
//!   `highlighting`, `serial` (lab-console, line settings), `telnet`
//!   (core-switch), `rdp` (win-build), `vnc`, `new` (Add host), `new-gnmi`
//! - `MUXUS_WINDOW=fullscreen` fills the monitor
//! - `MUXUS_DATA_DIR=<dir>` keeps the sandbox there instead of a temp dir

use std::path::PathBuf;

use muxus_app::features::host_editor::demo::{DemoSandbox, seed};
use muxus_app::features::host_editor::view::{
    HostEditor, HostEditorTarget, NewHost, Section, open_host_editor,
};
use muxus_app::hosts::managed::ManagedHost;
use muxus_core::api::{SavedHostKind, X11Defaults, X11Source, X11Status};
use muxus_store::prefs::state::ThemeMode;
use muxus_ui::gpui_kit::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, WindowOptions,
    div,
};
use muxus_ui::theme::{ActiveTheme, Mode};

fn scratch_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MUXUS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    std::env::temp_dir().join(format!("muxus-host-editor-example-{}", std::process::id()))
}

struct Scene {
    editor: Entity<HostEditor>,
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        div()
            .size_full()
            .bg(theme.palette.background_default)
            .child(self.editor.clone())
    }
}

fn main() {
    let dark = std::env::var("MUXUS_THEME").as_deref() == Ok("dark");
    let scene = std::env::var("MUXUS_EDITOR_SCENE").unwrap_or_else(|_| "general".into());
    let fullscreen = std::env::var("MUXUS_WINDOW").is_ok_and(|value| value == "fullscreen");

    let scratch = scratch_dir();
    let _ = std::fs::remove_dir_all(&scratch);
    let sandbox = DemoSandbox::create(&scratch).expect("create the demo sandbox");
    let backend = sandbox.backend().expect("start the sandboxed backend");
    let (config, saved) = futures_lite::future::block_on(async {
        seed(&backend).await.expect("seed the demo data");
        let mut prefs = backend.prefs().await.expect("read prefs");
        prefs.theme_mode = if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        prefs.light_terminal_scheme = "github-light".into();
        prefs.dark_terminal_scheme = "vscode-dark".into();
        prefs.command_buttons = Vec::new();
        prefs.keyword_highlights = Vec::new();
        backend.set_prefs(prefs).await.expect("seed prefs");
        (
            backend.ssh_config().await.expect("read the ssh config"),
            backend
                .saved_hosts()
                .await
                .expect("read the saved hosts")
                .profiles,
        )
    });
    let ssh = |alias: &str| {
        ManagedHost::ssh(
            config
                .hosts
                .iter()
                .find(|h| h.alias == alias)
                .cloned()
                .expect("a demo host"),
        )
    };
    let profile = |name: &str| {
        ManagedHost::profile(
            saved
                .iter()
                .find(|p| p.name == name)
                .cloned()
                .expect("a demo saved host"),
        )
    };
    let (target, section) = match scene.as_str() {
        "route" => (HostEditorTarget::Edit(ssh("db-primary")), Section::Route),
        "forwards" => (HostEditorTarget::Edit(ssh("db-primary")), Section::Forwards),
        "advanced" => (HostEditorTarget::Edit(ssh("db-primary")), Section::Advanced),
        "auth" => (HostEditorTarget::Edit(ssh("db-primary")), Section::Auth),
        "appearance" => (
            HostEditorTarget::Edit(ssh("db-primary")),
            Section::Appearance,
        ),
        "login" => (HostEditorTarget::Edit(ssh("db-primary")), Section::Login),
        "logging" => (HostEditorTarget::Edit(ssh("db-primary")), Section::Logging),
        "highlighting" => (
            HostEditorTarget::Edit(ssh("db-primary")),
            Section::Highlighting,
        ),
        "serial" => (
            HostEditorTarget::Edit(profile("lab-console")),
            Section::Line,
        ),
        "telnet" => (
            HostEditorTarget::Edit(profile("core-switch (console)")),
            Section::General,
        ),
        "rdp" => (
            HostEditorTarget::Edit(profile("win-build")),
            Section::General,
        ),
        "vnc" => (
            HostEditorTarget::Edit(profile("design-vm")),
            Section::General,
        ),
        "new" => (HostEditorTarget::New(NewHost::default()), Section::General),
        "new-gnmi" => (
            HostEditorTarget::New(NewHost {
                kind: Some(SavedHostKind::Gnmi),
                ..NewHost::default()
            }),
            Section::Security,
        ),
        _ => (HostEditorTarget::Edit(ssh("db-primary")), Section::General),
    };

    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            muxus_ui::init(if dark { Mode::Dark } else { Mode::Light }, cx);
            let backend = backend.clone();
            let target = target.clone();
            let handle =
                muxus_ui::gpui_kit::open_window(WindowOptions::default(), cx, move |window, cx| {
                    if fullscreen {
                        window.toggle_fullscreen();
                    }
                    let editor = open_host_editor(window, cx, backend.clone(), target.clone());
                    editor.update(cx, |editor, cx| {
                        editor.set_section(section, cx);
                        // The demo sandbox shows the Windows app's built-in X server, as the docs do.
                        editor.pin_x11_status(X11Status {
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
                    });
                    cx.new(|_| Scene { editor })
                });
            if handle.is_err() {
                eprintln!("the window did not open");
                cx.quit();
            }
        });
}
