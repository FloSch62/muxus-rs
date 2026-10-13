//! The GPUI views of the main window.
//!
//! Development aids (read once at startup):
//! - `MUXUS_THEME=light|dark|os` overrides the appearance preference.
//! - `MUXUS_WINDOW=fullscreen|maximized|<w>x<h>` sets the initial window state;
//!   `hack/shot.sh` uses `fullscreen` so a capture is exactly the viewport.
//! - `MUXUS_SCENE=…` opens the tabs a docs screenshot shows (see
//!   `hack/capture.mjs` and [`scenes`]), on the demo catalog with the demo
//!   hosts' shells run locally.

mod closing;
mod confirm;
mod desktop;
mod features;
mod host_actions;
mod launcher;
mod overlays;
mod panes;
mod paste_dialog;
mod perf;
mod prefs;
mod prompts;
mod scenes;
mod session_views;
mod sftp;
mod shell;
#[cfg(test)]
mod shell_tests;
mod shortcuts;
mod sidebar;
mod sidebar_dnd;
mod sidebar_menus;
mod sidebar_panel;
mod status_bar;
mod tab_actions;
mod terminal;
mod terminal_actions;
mod toast;
mod top_bar;
mod window_features;

use std::sync::Arc;

use muxus_app::data_source::{BackendSource, DataSource, LocalSource, LoginEnvironment};
use muxus_app::features::desktop::events as desktop_events;
use muxus_app::features::desktop::main_log::{
    MainLogLevel, init_main_log, install_crash_capture, main_log,
};
use muxus_app::features::desktop::single_instance::{self, Acquired, PrimaryInstance};
use muxus_app::features::desktop::updates::{DesktopUpdateInit, init_desktop_updates};
use muxus_app::features::settings::SettingsPrefs;
use muxus_app::model::demo::{demo_catalog, demo_folder_styles};
use muxus_app::model::demo_shell::DemoSource;
use muxus_app::sidebar::folder_prefs::FolderPrefs;
use muxus_session::data_dir::DataLocation;
use muxus_session::{Backend, BackendOptions, UPDATE_FEED_VAR};
use muxus_ui::gpui_kit::{AppContext, Bounds, WindowBounds, WindowOptions, size};
use muxus_ui::theme::Mode;

use prefs::{Prefs, ThemeMode};
use shell::AppShell;

/// Initial window state requested through `MUXUS_WINDOW`.
enum InitialWindow {
    Normal(f32, f32),
    Maximized,
    Fullscreen,
}

fn initial_window() -> InitialWindow {
    match std::env::var("MUXUS_WINDOW").as_deref() {
        Ok("fullscreen") => InitialWindow::Fullscreen,
        Ok("maximized") => InitialWindow::Maximized,
        Ok(value) => value
            .split_once('x')
            .and_then(|(w, h)| Some(InitialWindow::Normal(w.parse().ok()?, h.parse().ok()?)))
            // The Electron app's first-run size.
            .unwrap_or(InitialWindow::Normal(1440., 900.)),
        Err(_) => InitialWindow::Normal(1440., 900.),
    }
}

/// `BASE_PREFS` of `hack/capture.mjs`: what every documentation shot runs with.
fn screenshot_prefs(prefs: &mut Prefs) {
    prefs.mono_font_size = 14.;
    prefs.font_family = "JetBrains Mono".into();
    prefs.line_height = 1.1;
    prefs.light_terminal_scheme = "github-light".into();
    prefs.dark_terminal_scheme = "vscode-dark".into();
    prefs.sidebar_width = 250.;
    prefs.sidebar_folder_styles = demo_folder_styles();
}

/// Start the backend on this process's data folder and home, with the
/// login shell's environment for local terminals. `None` (and the reason)
/// when it cannot start; the window then offers local shells only.
fn start_backend(login: &LoginEnvironment) -> Result<Backend, String> {
    let mut options = BackendOptions::from_process();
    options.local_environment = Some(muxus_local::LocalEnvironment {
        vars: login.vars(),
        ..muxus_local::LocalEnvironment::current()
    });
    Backend::start_blocking(options).map_err(|err| err.to_string())
}

/// Keeps the single-instance lock (and its socket) for the whole run.
struct InstanceGuard(#[allow(dead_code)] PrimaryInstance);

impl muxus_ui::gpui_kit::Global for InstanceGuard {}

pub fn run() {
    let argv: Vec<String> = std::env::args().collect();
    let theme_override = match std::env::var("MUXUS_THEME").as_deref() {
        Ok("dark") => Some(ThemeMode::Dark),
        Ok("light") => Some(ThemeMode::Light),
        Ok("os") => Some(ThemeMode::Os),
        _ => None,
    };
    let scene = std::env::var("MUXUS_SCENE").unwrap_or_default();
    let initial = initial_window();
    let use_backend = scene.is_empty() && std::env::var("MUXUS_BACKEND").as_deref() != Ok("0");

    // The shell's log, crash capture and the one running instance per data folder come
    // first (`main.ts`): a second launch hands its command line over and exits before it
    // could seed or open the database the first one is using.
    let mut primary = None;
    if use_backend {
        let location = DataLocation::from_process();
        if let Err(error) = location.create_folder() {
            eprintln!(
                "could not create {}: {error}",
                location.paths.user_data.display()
            );
        }
        let log_file = init_main_log(&location.paths.user_data);
        // The backend's records (connections, sessions, file browser and tunnel errors)
        // go to the log viewer and to the same file.
        muxus_session::logging::subscriber::install_app_subscriber(log_file);
        install_crash_capture();
        main_log(
            MainLogLevel::Info,
            &format!(
                "Muxus {} starting on {}/{}",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            None,
        );
        match single_instance::acquire(&location.paths.user_data, &argv) {
            Ok(Acquired::Secondary { delivered }) => {
                if !delivered {
                    main_log(
                        MainLogLevel::Warn,
                        "another instance is running but did not take the launch",
                        None,
                    );
                }
                return;
            }
            Ok(Acquired::Primary(instance)) => primary = Some(instance),
            Err(error) => main_log(
                MainLogLevel::Warn,
                "could not take the single-instance lock",
                Some(&error),
            ),
        }
    }
    let initial_launch = use_backend
        .then(|| desktop_events::initial_launch(&argv))
        .flatten();

    // GUI launches do not inherit shell startup files: ask the login shell for
    // PATH (as Electron does at startup). Scenes keep the environment they
    // were started with, so screenshots are reproducible.
    let login = if scene.is_empty() {
        LoginEnvironment::import_in_background()
    } else {
        LoginEnvironment::current()
    };
    // The backend owns the data folder: hosts, sessions, preferences.
    // `MUXUS_BACKEND=0` opens a window with local shells only.
    let started = std::time::Instant::now();
    let backend = use_backend.then(|| start_backend(&login));
    let stored_prefs = match &backend {
        Some(Ok(backend)) => futures_lite::future::block_on(backend.prefs()).ok(),
        _ => None,
    };
    if let Some(perf) = perf::perf() {
        perf.note(&format!(
            "startup backend_and_prefs_ms={}",
            started.elapsed().as_millis()
        ));
    }
    let backend_error = match &backend {
        Some(Err(message)) => {
            main_log(
                MainLogLevel::Error,
                "the backend failed to start",
                Some(message),
            );
            Some(message.clone())
        }
        _ => None,
    };
    let backend = backend.and_then(Result::ok);
    let mut base_prefs = stored_prefs.unwrap_or_default();
    if let Some(mode) = theme_override {
        base_prefs.theme_mode = mode;
    }
    let theme_mode = base_prefs.theme_mode;

    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            let mode = if theme_mode == ThemeMode::Dark {
                Mode::Dark
            } else {
                Mode::Light
            };
            muxus_ui::init(mode, cx);
            if let Some(instance) = primary {
                let events = instance.events();
                cx.set_global(InstanceGuard(instance));
                desktop::listen(events, cx);
            }
            if let Some(backend) = backend.clone() {
                // Every window reads the one preferences entity and applies its changes live.
                SettingsPrefs::init(backend.clone(), base_prefs.clone(), cx);
                cx.set_global(MainWindowEnv {
                    backend: backend.clone(),
                });
                features::install(&backend, cx);
                // The release feed still names the Electron builds; a Rust build offers
                // updates only from a feed it is pointed at.
                if std::env::var(UPDATE_FEED_VAR).is_ok_and(|feed| !feed.trim().is_empty()) {
                    init_desktop_updates(&backend, DesktopUpdateInit::default(), cx);
                }
            }
            // Closing the last window quits, and the backend shuts down first
            // (`onClose`: tunnels, sessions, the history writer).
            if let Some(backend) = backend.clone() {
                cx.on_app_quit(move |_| {
                    let backend = backend.clone();
                    async move { backend.shutdown().await }
                })
                .detach();
            }
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let options = main_window_options(&initial, cx);
            let fullscreen = matches!(initial, InitialWindow::Fullscreen);
            let scene = scene.clone();
            let login = login.clone();
            let backend = backend.clone();
            let base_prefs = base_prefs.clone();
            let backend_error = backend_error.clone();
            let initial_launch = initial_launch.clone();
            // The window's root is the app shell itself, without the component
            // library's window wrapper, which would draw its own border around
            // a floating window (the frame is `muxus_ui::widgets::window_frame`).
            cx.open_window(options, move |window, cx| {
                if fullscreen {
                    window.toggle_fullscreen();
                }
                cx.new(|cx| {
                    let mut prefs = base_prefs.clone();
                    let (data, catalog): (Arc<dyn DataSource>, _) = if let Some(backend) =
                        backend.clone()
                    {
                        (
                            Arc::new(BackendSource::new(backend)),
                            muxus_app::model::Catalog::default(),
                        )
                    } else if scene.is_empty() {
                        (
                            Arc::new(LocalSource::new(login.clone())),
                            muxus_app::model::Catalog::default(),
                        )
                    } else {
                        screenshot_prefs(&mut prefs);
                        let mut catalog = demo_catalog();
                        if scene == "many" {
                            scenes::add_many_hosts(&mut catalog, 800);
                        }
                        let base =
                            std::env::temp_dir().join(format!("muxus-demo-{}", std::process::id()));
                        (
                            Arc::new(
                                DemoSource::new(base, login.clone()).with_catalog(catalog.clone()),
                            ),
                            catalog,
                        )
                    };
                    let folders = FolderPrefs {
                        collapsed: prefs.sidebar_collapsed_folders.clone(),
                        styles: prefs.sidebar_folder_styles.clone(),
                        order: prefs.sidebar_folder_order.clone(),
                        empty_folders: prefs.sidebar_empty_folders.clone(),
                    };
                    let mut shell = AppShell::new(data, catalog, folders, prefs, window, cx);
                    let initial_workspace = match &initial_launch {
                        Some(muxus_core::api::CommandLineLaunch::Workspace { name }) => Some(
                            muxus_app::features::workspaces::WorkspaceInitialSelection::OpenName {
                                name: name.clone(),
                            },
                        ),
                        Some(_) => {
                            Some(muxus_app::features::workspaces::WorkspaceInitialSelection::Blank)
                        }
                        None => None,
                    };
                    shell.install_window_features(initial_workspace, window, cx);
                    if let Some(message) = &backend_error {
                        shell.show_toast(
                            muxus_ui::theme::Status::Error,
                            format!("The Muxus backend could not start: {message}"),
                            cx,
                        );
                    }
                    scenes::open_scene(&mut shell, &scene, cx);
                    // A workspace launch is the window's initial workspace; the rest run now.
                    if let Some(launch) = initial_launch.clone().filter(|launch| {
                        !matches!(launch, muxus_core::api::CommandLineLaunch::Workspace { .. })
                    }) {
                        shell.run_launch(launch, cx);
                    }
                    shell
                })
            })
            .expect("open the main window");
        });
}

/// What a further main window opens with (`openHostInNewWindow`, a workspace in a new
/// window).
pub enum MainWindowLaunch {
    /// A session of a tab opened (or moved) into a window of its own. A move carries the
    /// live session (`transfer`: its terminal id and buffer).
    Session {
        profile: Box<muxus_app::model::SessionProfile>,
        title: String,
        color: Option<String>,
        transfer: Option<(String, String)>,
    },
    Host(Box<muxus_app::hosts::ManagedHost>),
    /// A saved workspace (`id`), or a new empty one named `name`.
    Workspace {
        id: Option<String>,
        name: String,
    },
}

/// What every main window is built from.
#[derive(Clone)]
struct MainWindowEnv {
    backend: Backend,
}

impl muxus_ui::gpui_kit::Global for MainWindowEnv {}

/// Open another main window on the running backend.
pub fn open_main_window(launch: MainWindowLaunch, cx: &mut muxus_ui::gpui_kit::App) {
    let Some(env) = cx.try_global::<MainWindowEnv>().cloned() else {
        return;
    };
    let prefs = SettingsPrefs::global(cx)
        .map(|shared| shared.read(cx).prefs().clone())
        .unwrap_or_default();
    let options = main_window_options(&InitialWindow::Normal(1440., 900.), cx);
    let opened = cx.open_window(options, move |window, cx| {
        cx.new(|cx| {
            let folders = FolderPrefs {
                collapsed: prefs.sidebar_collapsed_folders.clone(),
                styles: prefs.sidebar_folder_styles.clone(),
                order: prefs.sidebar_folder_order.clone(),
                empty_folders: prefs.sidebar_empty_folders.clone(),
            };
            let mut shell = AppShell::new(
                Arc::new(BackendSource::new(env.backend.clone())),
                muxus_app::model::Catalog::default(),
                folders,
                prefs,
                window,
                cx,
            );
            let initial = match &launch {
                MainWindowLaunch::Workspace { id: Some(id), .. } => {
                    muxus_app::features::workspaces::WorkspaceInitialSelection::Open {
                        id: id.clone(),
                    }
                }
                MainWindowLaunch::Workspace { id: None, name } => {
                    muxus_app::features::workspaces::WorkspaceInitialSelection::New {
                        id: muxus_store::prefs::command_buttons::new_preference_id("workspace"),
                        name: name.clone(),
                    }
                }
                MainWindowLaunch::Host(_) | MainWindowLaunch::Session { .. } => {
                    muxus_app::features::workspaces::WorkspaceInitialSelection::Blank
                }
            };
            shell.install_window_features(Some(initial), window, cx);
            shell.start_window_launch(launch, cx);
            shell
        })
    });
    if let Err(error) = opened {
        main_log(MainLogLevel::Error, "could not open a window", Some(&error));
    }
}

/// The main window's options: the Electron app's first-run size, its own frame, the app id
/// the desktop file names.
fn main_window_options(initial: &InitialWindow, cx: &mut muxus_ui::gpui_kit::App) -> WindowOptions {
    let (width, height) = match initial {
        InitialWindow::Normal(w, h) => (*w, *h),
        _ => (1440., 900.),
    };
    let bounds = Bounds::centered(
        None,
        size(
            muxus_ui::zoom::window_px(width),
            muxus_ui::zoom::window_px(height),
        ),
        cx,
    );
    muxus_ui::widgets::window_frame::app_window_options(
        "Muxus",
        match initial {
            InitialWindow::Maximized => WindowBounds::Maximized(bounds),
            _ => WindowBounds::Windowed(bounds),
        },
        size(
            muxus_ui::zoom::window_px(800.),
            muxus_ui::zoom::window_px(500.),
        ),
        muxus_app::features::desktop::APP_ID,
    )
}
