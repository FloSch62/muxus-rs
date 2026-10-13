//! What the launcher, workspace, history and command button examples share:
//! the documentation's demo sandbox (`hack/demo-env.mjs` through
//! [`crate::features::host_editor::demo`]), the screenshot preferences of
//! `hack/capture.mjs`, recorded demo sessions and a plain window to show a
//! screen in. Nothing here touches the real home directory or data folder.

use std::cell::RefCell;
use std::path::PathBuf;

use muxus_core::api::{
    LayoutVersion1, SessionLogCreateInput, SessionLogDirection, WorkspaceLayoutV1,
    WorkspaceMultiExecGroup, WorkspaceNode, WorkspacePane, WorkspaceSaveRequest,
    WorkspaceTabLayout, WorkspaceTerminalTab,
};
use muxus_core::protocol::{ProfileKind, SessionProfile, SshProfile};
use muxus_session::Backend;
use muxus_store::history::{HistoryEvent, PartPolicy};
use muxus_store::prefs::PrefsState;
use muxus_store::prefs::state::ThemeMode;
use muxus_ui::gpui_kit::{
    AnyView, App, AppContext, Context, IntoElement, ParentElement, Render, Styled, Window,
    WindowOptions, div,
};
use muxus_ui::theme::{ActiveTheme, Mode};

use crate::features::command_buttons::multi_exec::MultiExecState;
use crate::features::host_editor::demo::{DemoSandbox, seed};
use crate::features::workspaces::WorkspaceLayoutHost;
use crate::features::workspaces::layout::{WorkspaceSnapshot, layout_from_core, window_snapshot};
use crate::state::tabs::{TabPatch, TabStatus, TabsState};
use crate::state::workspace_layout::RestoreWorkspaceOptions;

/// The sandbox an example runs on.
pub struct DemoEnv {
    pub sandbox: DemoSandbox,
    pub backend: Backend,
    pub prefs: PrefsState,
    pub dark: bool,
}

/// `MUXUS_THEME=dark`.
pub fn dark_requested() -> bool {
    std::env::var("MUXUS_THEME").as_deref() == Ok("dark")
}

/// `MUXUS_WINDOW=fullscreen`.
pub fn fullscreen_requested() -> bool {
    std::env::var("MUXUS_WINDOW").is_ok_and(|value| value == "fullscreen")
}

fn scratch_dir(name: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os("MUXUS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    std::env::temp_dir().join(format!("muxus-{name}-example-{}", std::process::id()))
}

/// `BASE_PREFS` of the capture script.
pub fn screenshot_prefs(prefs: &mut PrefsState, dark: bool) {
    prefs.theme_mode = if dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    prefs.mono_font_size = 14.;
    prefs.font_family = "JetBrains Mono".into();
    prefs.line_height = 1.1;
    prefs.light_terminal_scheme = "github-light".into();
    prefs.dark_terminal_scheme = "vscode-dark".into();
    prefs.sidebar_width = 250.;
    prefs.command_buttons = Vec::new();
    prefs.keyword_highlights = Vec::new();
    prefs.sidebar_folder_styles = crate::model::demo::demo_folder_styles();
}

/// Lay out a fresh sandbox, seed it like `demo-env.mjs`, and write the
/// screenshot preferences (then `prefs` on top).
pub fn demo_env(name: &str, prefs: impl FnOnce(&mut PrefsState)) -> DemoEnv {
    let dark = dark_requested();
    let scratch = scratch_dir(name);
    let _ = std::fs::remove_dir_all(&scratch);
    let sandbox = DemoSandbox::create(&scratch).expect("create the demo sandbox");
    let backend = sandbox.backend().expect("start the sandboxed backend");
    let prefs = futures_lite::future::block_on(async {
        seed(&backend).await.expect("seed the demo data");
        let mut state = backend.prefs().await.expect("read prefs");
        screenshot_prefs(&mut state, dark);
        prefs(&mut state);
        backend.set_prefs(state.clone()).await.expect("seed prefs");
        state
    });
    DemoEnv {
        sandbox,
        backend,
        prefs,
        dark,
    }
}

/// One recorded session for the history seed.
pub struct DemoSession<'a> {
    pub host: &'a str,
    pub started_at: &'a str,
    /// The session is still running.
    pub active: bool,
    /// The SSH connection was already open (the second line says it was
    /// reused rather than dialed).
    pub reused: bool,
    /// The recorded size in raw bytes (terminal output carries escape
    /// sequences the transcript drops); 0 keeps the text's own size.
    pub raw_size: u64,
    /// Output after the logging notices, one entry per event.
    pub output: &'a [&'a str],
}

/// What a recorded SSH session to `host` starts with: the logging notice
/// and the dial line the recorder writes.
fn opening_events(host: &str, reused: bool) -> [String; 2] {
    let address = match host {
        "web-01" | "web-02" => format!("deploy@{host}.prod.internal:22"),
        other => format!("{other}:22"),
    };
    [
        "Session logging started.\n".to_owned(),
        if reused {
            format!("Reusing the SSH connection to {address} (multiplexed).\n")
        } else {
            format!("Connecting to {address} …\n")
        },
    ]
}

/// Record demo sessions in the backend's history store, as a session with
/// logging switched on leaves them, and wait until the store has them.
pub fn seed_history(backend: &Backend, sessions: &[DemoSession<'_>]) {
    let store = backend.history().clone();
    let policy = PartPolicy {
        max_part_bytes: 5 * 1024 * 1024,
        max_parts: 10,
    };
    for session in sessions {
        let id = store.begin_session(
            &SessionLogCreateInput {
                profile_key: format!("ssh:{}", session.host),
                title: session.host.to_owned(),
                kind: ProfileKind::Ssh,
                host: session.host.to_owned(),
                started_at: session.started_at.to_owned(),
                capture_input: false,
            },
            policy,
        );
        let mut events = Vec::new();
        let opening = opening_events(session.host, session.reused);
        let all = opening
            .iter()
            .map(|text| (SessionLogDirection::System, text.as_str()))
            .chain(
                session
                    .output
                    .iter()
                    .map(|text| (SessionLogDirection::Output, *text)),
            );
        for (index, (direction, text)) in all.enumerate() {
            events.push(HistoryEvent {
                sequence: index as u64 + 1,
                recorded_at: session.started_at.to_owned(),
                elapsed_ms: index as u64 * 10,
                direction,
                raw: text.as_bytes().to_vec(),
                text: text.to_owned(),
                line_timestamps: None,
            });
        }
        // Pad the last event's raw bytes with colour resets, as a prompt's
        // escape sequences would, to reach the size the session reports.
        let natural: u64 = events.iter().map(|event| event.raw.len() as u64).sum();
        if session.raw_size > natural
            && let Some(last) = events.last_mut()
        {
            let missing = (session.raw_size - natural) as usize;
            let reset = b"\x1b[0m";
            let mut padding: Vec<u8> = reset.iter().copied().cycle().take(missing).collect();
            padding.truncate(missing);
            last.raw.extend(padding);
        }
        store.append(&id, events, policy);
        if !session.active {
            store.finish_session(
                &id,
                muxus_core::api::SessionEndStatus::Completed,
                session.started_at,
            );
        }
    }
    // The store writes on its own worker; reading back waits for it.
    let _ = futures_lite::future::block_on(backend.session_history(Default::default()));
}

/// A connected SSH tab, as a window shows one after connecting.
pub fn connected_tab(tabs: &mut TabsState, target: &str) -> String {
    let id = tabs.open(SessionProfile::Ssh(SshProfile::new(target)), target);
    tabs.update(
        &id,
        TabPatch {
            status: Some(TabStatus::Connected),
            conn_id: Some(Some(format!("conn-{target}"))),
            terminal_id: Some(Some(format!("terminal-{id}"))),
            ..Default::default()
        },
    );
    id
}

/// One pane holding a tab per host: enough for a saved-workspace record
/// (`paneLayout` in the capture script).
pub fn demo_pane_layout(name: &str, hosts: &[&str]) -> WorkspaceLayoutV1 {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let tabs: Vec<WorkspaceTabLayout> = hosts
        .iter()
        .enumerate()
        .map(|(index, target)| {
            WorkspaceTabLayout::Terminal(WorkspaceTerminalTab {
                id: format!("{slug}-tab-{index}"),
                title: (*target).to_owned(),
                profile: SessionProfile::Ssh(SshProfile::new(*target)),
                cwd_hint: None,
                color: None,
                pinned: None,
                offer_reconnect: true,
            })
        })
        .collect();
    let first = tabs.first().map(|tab| tab.id().to_owned());
    WorkspaceLayoutV1 {
        version: LayoutVersion1,
        root: Some(WorkspaceNode::Pane(WorkspacePane {
            id: format!("{slug}-pane"),
            tabs,
            active_tab_id: first,
        })),
        active_pane_id: Some(format!("{slug}-pane")),
    }
}

/// Save workspaces (name and hosts) batch by batch, the batches a second
/// apart so the catalog lists later batches first.
pub fn seed_workspaces(backend: &Backend, batches: &[&[(&str, &[&str])]]) {
    futures_lite::future::block_on(async {
        for (index, batch) in batches.iter().enumerate() {
            if index > 0 {
                std::thread::sleep(std::time::Duration::from_millis(1100));
            }
            for (name, hosts) in batch.iter() {
                backend
                    .save_workspace(WorkspaceSaveRequest {
                        id: None,
                        name: (*name).into(),
                        layout: demo_pane_layout(name, hosts),
                        overwrite_locked: false,
                        create_if_missing: false,
                        allocate_default_name: false,
                        multi_exec_groups: Vec::new(),
                    })
                    .await
                    .expect("seed a workspace");
            }
        }
    });
}

/// The window behind a workspace screen: its tabs live here.
pub struct DemoLayoutHost {
    pub tabs: RefCell<TabsState>,
}

impl WorkspaceLayoutHost for DemoLayoutHost {
    fn snapshot(&self, _: &App) -> WorkspaceSnapshot {
        window_snapshot(&self.tabs.borrow(), &MultiExecState::default())
    }

    fn is_blank(&self, _: &App) -> bool {
        self.tabs.borrow().tabs.is_empty()
    }

    fn restore(
        &self,
        layout: &WorkspaceLayoutV1,
        _: &[WorkspaceMultiExecGroup],
        connect_remote: bool,
        _: &mut App,
    ) {
        self.tabs.borrow_mut().restore(
            &layout_from_core(layout),
            RestoreWorkspaceOptions { connect_remote },
        );
    }

    fn active_workspace_changed(&self, _: Option<&str>, _: &str, _: bool, _: &mut App) {}

    fn focus_window(&self, _: &mut App) {}

    fn connect_remote_on_restore(&self, _: &App) -> bool {
        false
    }
}

/// The window an example shows its screen in: the app's background with the
/// screen on top.
pub struct DemoScene {
    content: AnyView,
}

impl Render for DemoScene {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        div()
            .size_full()
            .relative()
            .bg(theme.palette.background_default)
            .text_color(theme.palette.text.primary)
            .child(self.content.clone())
    }
}

/// Run the application with one window built by `build` (fullscreen when
/// asked, so it fills the screenshot monitor).
pub fn run_demo(env: &DemoEnv, build: impl Fn(&mut Window, &mut App) -> AnyView + 'static) {
    let dark = env.dark;
    let backend = env.backend.clone();
    let prefs = env.prefs.clone();
    let fullscreen = fullscreen_requested();
    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            muxus_ui::init(if dark { Mode::Dark } else { Mode::Light }, cx);
            crate::features::settings::SettingsPrefs::init(backend.clone(), prefs.clone(), cx);
            let build = &build;
            let handle =
                muxus_ui::gpui_kit::open_window(WindowOptions::default(), cx, move |window, cx| {
                    if fullscreen {
                        window.toggle_fullscreen();
                    }
                    let content = build(window, cx);
                    cx.new(|_| DemoScene { content })
                });
            if handle.is_err() {
                eprintln!("the window did not open");
                cx.quit();
            }
        });
}
