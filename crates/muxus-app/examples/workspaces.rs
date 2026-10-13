//! The workspace dialog on the documentation's demo sandbox, in the state of
//! the `workspaces` screenshot (`hack/capture.mjs`): an unsaved, idle window
//! and a believable saved catalog.
//!
//! ```bash
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen --env TZ=UTC out.png \
//!   -- target-ui-c/debug/examples/workspaces
//! ```
//!
//! - `MUXUS_THEME=light|dark` (default light)
//! - `MUXUS_WORKSPACES_SCENE`: `catalog` (default), `active` (the window
//!   shows "Morning check", locked, with a disconnected tab), `menu` (a
//!   row's actions), `save-as` (the name field)
//! - `MUXUS_WINDOW=fullscreen` fills the monitor
//! - `MUXUS_DATA_DIR=<dir>` keeps the sandbox there instead of a temp dir

use std::cell::RefCell;
use std::rc::Rc;

use muxus_app::features::launcher::demo::{
    DemoLayoutHost, connected_tab, demo_env, run_demo, seed_workspaces,
};
use muxus_app::features::workspaces::{
    WorkspaceBus, WorkspaceDialogContext, WorkspaceInitialSelection, Workspaces, open_workspaces,
};
use muxus_app::state::tabs::{IdGen, TabPatch, TabStatus, TabsState};
use muxus_ui::gpui_kit::AppContext;

fn main() {
    let scene = std::env::var("MUXUS_WORKSPACES_SCENE").unwrap_or_else(|_| "catalog".into());
    let env = demo_env("workspaces", |_| {});
    // The other shots leave an autosaved "Workspace 1" behind first.
    seed_workspaces(
        &env.backend,
        &[
            &[("Workspace 1", &["web-01"][..])],
            &[
                ("Morning check", &["web-01", "web-02", "cache-01"][..]),
                ("Fabric lab", &["lab-leaf-01", "lab-spine-01"][..]),
                ("Incident 4821", &["db-01", "bastion"][..]),
            ],
        ],
    );
    let backend = env.backend.clone();
    run_demo(&env, move |window, cx| {
        let mut tabs = TabsState::with_pane("pane-1", IdGen::sequential());
        let initial = if scene == "active" {
            let id = connected_tab(&mut tabs, "web-01");
            tabs.update(
                &id,
                TabPatch {
                    status: Some(TabStatus::Closed),
                    ..Default::default()
                },
            );
            None
        } else {
            Some(WorkspaceInitialSelection::Blank)
        };
        let host = Rc::new(DemoLayoutHost {
            tabs: RefCell::new(tabs),
        });
        let bus = WorkspaceBus::default();
        let workspaces = cx.new(|_| Workspaces::new(backend.clone(), &bus, host.clone(), initial));
        workspaces.update(cx, |workspaces, cx| workspaces.start(cx));
        let context = WorkspaceDialogContext {
            tabs: host.tabs.borrow().tabs.clone(),
        };
        open_workspaces(workspaces, context, window, cx).into()
    });
}
