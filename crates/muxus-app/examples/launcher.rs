//! The quick launcher on the documentation's demo sandbox, in the state of
//! the `quick-launcher` screenshot (`hack/capture.mjs`): web-01 connected,
//! "web" typed, its running session in the history.
//!
//! ```bash
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen --env TZ=UTC out.png \
//!   -- target-ui-c/debug/examples/launcher
//! ```
//!
//! - `MUXUS_THEME=light|dark` (default light)
//! - `MUXUS_LAUNCHER_QUERY=<text>` (default `web`; empty shows the suggestions)
//! - `MUXUS_LAUNCHER_SCENE=picker` shows the empty pane's host picker instead
//!   (the `host-picker` capture: opened from the "Saved host" card)
//! - `MUXUS_WINDOW=fullscreen` fills the monitor
//! - `MUXUS_DATA_DIR=<dir>` keeps the sandbox there instead of a temp dir

use std::cell::RefCell;
use std::rc::Rc;

use muxus_app::features::launcher::demo::{
    DemoLayoutHost, DemoSession, connected_tab, demo_env, run_demo, seed_history, seed_workspaces,
};
use muxus_app::features::launcher::{LauncherContext, open_host_picker, open_quick_launcher};
use muxus_app::features::workspaces::{WorkspaceBus, WorkspaceInitialSelection, Workspaces};
use muxus_app::state::tabs::{IdGen, TabsState};
use muxus_store::prefs::command_buttons::DEFAULT_COMMAND_GROUP_ID;
use muxus_ui::gpui_kit::AppContext;

fn main() {
    let query = std::env::var("MUXUS_LAUNCHER_QUERY").unwrap_or_else(|_| "web".into());
    let env = demo_env("launcher", |_| {});
    // web-01 has been connected to before (the shot connects to it first).
    let _ = env.backend.database().record_open_ssh_connection("web-01");
    // The reference capture's one retained session: the web-01 shell it
    // connected, still running.
    let output: &[&str] = &["deploy@web-01:~$ "];
    seed_history(
        &env.backend,
        &[DemoSession {
            host: "web-01",
            started_at: "2026-10-12T17:40:12.000Z",
            active: true,
            reused: false,
            raw_size: 0,
            output,
        }],
    );
    if query.is_empty() {
        // What the reference run had saved by then, the newest open.
        seed_workspaces(
            &env.backend,
            &[
                &[("Fabric lab", &["lab-leaf-01", "lab-spine-01"][..])],
                &[("Workspace 2", &["web-01"][..])],
                &[("Workspace 3", &["web-01"][..])],
                &[("Workspace 4", &["web-01"][..])],
                &[("Workspace 5", &["web-01"][..])],
            ],
        );
    }
    let backend = env.backend.clone();
    let picker = std::env::var("MUXUS_LAUNCHER_SCENE").is_ok_and(|scene| scene == "picker");
    run_demo(&env, move |window, cx| {
        if picker {
            // The bottom-left corner of the empty pane's "Saved host" card at 1420×880.
            let position = muxus_ui::gpui_kit::point(
                muxus_ui::gpui_kit::px(743.),
                muxus_ui::gpui_kit::px(560.),
            );
            return open_host_picker(backend.clone(), position, None, window, cx).into();
        }
        let mut tabs = TabsState::with_pane("pane-1", IdGen::sequential());
        let id = connected_tab(&mut tabs, "web-01");
        let context = LauncherContext {
            tabs: tabs.tabs.clone(),
            active_id: Some(id),
            shown_command_group: DEFAULT_COMMAND_GROUP_ID.into(),
            wsl_shell_profiles: Vec::new(),
            workspaces: query.is_empty().then(|| {
                let host = Rc::new(DemoLayoutHost {
                    tabs: RefCell::new(TabsState::with_pane("pane-1", IdGen::sequential())),
                });
                let initial = WorkspaceInitialSelection::OpenName {
                    name: "Workspace 5".into(),
                };
                let bus = WorkspaceBus::default();
                let workspaces =
                    cx.new(|_| Workspaces::new(backend.clone(), &bus, host, Some(initial)));
                workspaces.update(cx, |workspaces, cx| workspaces.start(cx));
                workspaces
            }),
        };
        let launcher = open_quick_launcher(backend.clone(), context, window, cx);
        let query = query.clone();
        launcher.update(cx, |launcher, cx| launcher.set_query(&query, window, cx));
        launcher.into()
    });
}
