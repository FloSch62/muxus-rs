//! The dialogs around host editing on the documentation's demo sandbox:
//! launch group, organize, folder, bulk edit and the persistent tunnel editor.
//!
//! ```bash
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_DIALOG_SCENE=launch out.png \
//!   -- target-ui-a/debug/examples/host_dialogs
//! ```
//!
//! - `MUXUS_THEME=light|dark` (default light)
//! - `MUXUS_DIALOG_SCENE`: `launch` (Production), `organize` (db-primary),
//!   `folder` (edit Lab), `folder-new`, `move` (two hosts), `bulk`
//!   (the Production hosts), `tunnel` (new), `tunnel-edit` (Postgres via bastion)
//! - `MUXUS_WINDOW=fullscreen` fills the monitor
//! - `MUXUS_DATA_DIR=<dir>` keeps the sandbox there instead of a temp dir

use std::path::PathBuf;

use muxus_app::features::host_editor::demo::{DemoSandbox, seed};
use muxus_app::features::host_editor::dialogs::bulk::open_bulk_edit;
use muxus_app::features::host_editor::dialogs::folder::open_folder_dialog;
use muxus_app::features::host_editor::dialogs::launch_group::{LaunchTarget, open_launch_group};
use muxus_app::features::host_editor::dialogs::organize::open_organize;
use muxus_app::features::host_editor::dialogs::tunnel::open_tunnel_editor;
use muxus_app::features::host_editor::logic::folder::FolderDialogState;
use muxus_app::features::host_editor::logic::tunnel::TunnelEditorState;
use muxus_app::features::host_editor::palette::demo;
use muxus_app::hosts::managed::ManagedHost;
use muxus_app::sidebar::folder_prefs::FolderPrefs;
use muxus_store::prefs::state::{FolderStyle, ThemeMode};
use muxus_ui::gpui_kit::{
    AnyView, AppContext, Context, IntoElement, ParentElement, Render, Styled, Window,
    WindowOptions, div,
};
use muxus_ui::theme::{ActiveTheme, Mode};

fn scratch_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MUXUS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    std::env::temp_dir().join(format!("muxus-host-dialogs-example-{}", std::process::id()))
}

struct Scene {
    dialog: AnyView,
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        div()
            .size_full()
            .bg(theme.palette.background_default)
            .child(self.dialog.clone())
    }
}

fn folder_prefs() -> FolderPrefs {
    let mut prefs = FolderPrefs::default();
    for (key, color, icon) in [
        ("folder:production", demo::RED, "cloud"),
        ("folder:production/web", demo::BLUE, "server"),
        ("folder:production/data", demo::ORANGE, "storage"),
        ("folder:lab", demo::GREEN, "lab"),
        ("folder:lab/fabric", demo::PURPLE, "lan"),
    ] {
        prefs.set_folder_style(
            key,
            Some(FolderStyle {
                color: Some(color.into()),
                icon: Some(icon.into()),
            }),
        );
    }
    prefs
}

fn main() {
    let dark = std::env::var("MUXUS_THEME").as_deref() == Ok("dark");
    let scene = std::env::var("MUXUS_DIALOG_SCENE").unwrap_or_else(|_| "launch".into());
    let fullscreen = std::env::var("MUXUS_WINDOW").is_ok_and(|value| value == "fullscreen");

    let scratch = scratch_dir();
    let _ = std::fs::remove_dir_all(&scratch);
    let sandbox = DemoSandbox::create(&scratch).expect("create the demo sandbox");
    let backend = sandbox.backend().expect("start the sandboxed backend");
    let (config, saved, tunnels) = futures_lite::future::block_on(async {
        seed(&backend).await.expect("seed the demo data");
        let mut prefs = backend.prefs().await.expect("read prefs");
        prefs.theme_mode = if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
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
            backend.tunnels().await.expect("read the tunnels").tunnels,
        )
    });
    let ssh = move |alias: &str| {
        ManagedHost::ssh(
            config
                .hosts
                .iter()
                .find(|h| h.alias == alias)
                .cloned()
                .expect("a demo host"),
        )
    };
    let _ = &saved;
    // The Production subtree in sidebar order: Data, Web, then the folder's own hosts.
    let production = [
        "db-01",
        "db-primary",
        "web-01",
        "web-02",
        "bastion",
        "cache-01",
    ];

    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            muxus_ui::init(if dark { Mode::Dark } else { Mode::Light }, cx);
            let backend = backend.clone();
            let scene = scene.clone();
            let hosts: Vec<ManagedHost> = production.iter().map(|a| ssh(a)).collect();
            let organize_host = ssh("db-primary");
            let tunnels = tunnels.clone();
            let opened =
                muxus_ui::gpui_kit::open_window(WindowOptions::default(), cx, move |window, cx| {
                    if fullscreen {
                        window.toggle_fullscreen();
                    }
                    let dialog: AnyView = match scene.as_str() {
                        "organize" => {
                            open_organize(window, cx, backend.clone(), organize_host.clone()).into()
                        }
                        "folder" => open_folder_dialog(
                            window,
                            cx,
                            backend.clone(),
                            FolderDialogState::Edit { path: "Lab".into() },
                            folder_prefs(),
                        )
                        .into(),
                        "folder-new" => open_folder_dialog(
                            window,
                            cx,
                            backend.clone(),
                            FolderDialogState::New {
                                parent_path: Some("Lab".into()),
                            },
                            folder_prefs(),
                        )
                        .into(),
                        "move" => open_folder_dialog(
                            window,
                            cx,
                            backend.clone(),
                            FolderDialogState::MoveHosts {
                                host_keys: vec!["ssh:web-01".into(), "ssh:web-02".into()],
                                host_name: None,
                                current_path: "Production/Web".into(),
                            },
                            folder_prefs(),
                        )
                        .into(),
                        "bulk" => open_bulk_edit(
                            window,
                            cx,
                            backend.clone(),
                            production.iter().map(|a| format!("ssh:{a}")).collect(),
                        )
                        .into(),
                        "tunnel" => open_tunnel_editor(
                            window,
                            cx,
                            backend.clone(),
                            TunnelEditorState::default(),
                        )
                        .into(),
                        "tunnel-edit" => open_tunnel_editor(
                            window,
                            cx,
                            backend.clone(),
                            TunnelEditorState {
                                tunnel: tunnels
                                    .iter()
                                    .find(|t| t.name.as_deref() == Some("Postgres (via bastion)"))
                                    .cloned(),
                                prefill_target: None,
                            },
                        )
                        .into(),
                        _ => open_launch_group(
                            window,
                            cx,
                            LaunchTarget {
                                label: "Production".into(),
                                hosts: hosts.clone(),
                            },
                            false,
                        )
                        .into(),
                    };
                    cx.new(|_| Scene { dialog })
                });
            if opened.is_err() {
                eprintln!("the window did not open");
                cx.quit();
            }
        });
}
