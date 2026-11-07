//! Hands the launch menu ([`super::launch_menu`]) to the operating system, as
//! `refreshLaunchMenu` and `startLaunchMenu` in `electron/src/main.ts` do: the Windows
//! taskbar jump list (`muxus_os::jump_list`, Electron's `app.setJumpList`) or the macOS Dock
//! menu (GPUI's `set_dock_menu`, Electron's `app.dock.setMenu`). Only the backend sees every
//! connection, from any window, so the recent hosts and workspaces are read from it every
//! 15 s, as often as windows reread the host list, and the menu is set again only when it
//! changed.
//!
//! Linux has neither; its desktop file carries the New Window action instead.

use std::collections::HashSet;
use std::time::Duration;

use muxus_core::api::CommandLineLaunch;
use muxus_os::jump_list::{self, JumpListCategory, JumpListResult};
use muxus_session::Backend;
// The `Action` derive names its crate `gpui`.
use muxus_ui::gpui_kit as gpui;

use gpui::{Action, App, AsyncApp, MenuItem};

use super::launch_menu::{
    APP_USER_MODEL_ID, DockMenuEntry, HostUse, JumpListLauncher, RecentLaunches, dock_menu_entries,
    jump_list_categories, recent_launches,
};
use super::main_log::{MainLogLevel, main_log};

/// How often the menu is read again (`LAUNCH_MENU_REFRESH_MS`).
pub const LAUNCH_MENU_REFRESH: Duration = Duration::from_secs(15);

/// The name the operating system shows for the app (`app.name`).
const APP_NAME: &str = "Muxus";

/// A Dock menu entry: open this launch as a later process's command line would
/// (`deliverCommandLineLaunch`).
#[derive(Debug, Clone, PartialEq, Action)]
#[action(namespace = muxus_dock_menu, no_json)]
pub struct DockMenuLaunch {
    pub launch: CommandLineLaunch,
}

/// A Dock menu heading ("Recent Hosts"). Nothing handles it, so macOS shows it disabled
/// (`{ label, enabled: false }`): the menu validates its items by whether their action is
/// handled.
#[derive(Debug, Clone, PartialEq, Action)]
#[action(namespace = muxus_dock_menu, no_json)]
pub struct DockMenuHeading;

/// The menu a platform has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMenuKind {
    /// The Windows taskbar jump list.
    JumpList,
    /// The macOS Dock menu.
    Dock,
}

/// The launch menu of this build: the Dock menu on macOS, the jump list on Windows, except in
/// Microsoft Store packages, which keep the package's identity and get no jump list
/// (`hasJumpList`).
pub fn launch_menu_kind(store_distribution: bool) -> Option<LaunchMenuKind> {
    if cfg!(target_os = "macos") {
        Some(LaunchMenuKind::Dock)
    } else if jump_list::SUPPORTED && !store_distribution {
        Some(LaunchMenuKind::JumpList)
    } else {
        None
    }
}

/// The installer's shortcut ID, so windows and the jump list share its taskbar button; a
/// development build gets its own.
pub fn app_user_model_id(development: bool) -> String {
    if development {
        format!("{APP_USER_MODEL_ID}.development")
    } else {
        APP_USER_MODEL_ID.to_string()
    }
}

/// Before the first window opens: give the process the AppUserModelID the jump list belongs to
/// (`app.setAppUserModelId`). GPUI sets it on Windows and leaves a packaged process's own
/// identity alone; elsewhere it does nothing.
pub fn set_app_user_model_id(app_id: &str, cx: &App) {
    cx.set_app_identity(app_id, APP_NAME);
}

/// The Dock menu's items: launches, separators and disabled headings.
pub fn dock_menu_items(entries: &[DockMenuEntry]) -> Vec<MenuItem> {
    entries
        .iter()
        .map(|entry| match entry {
            DockMenuEntry::Launch { label, launch } => MenuItem::action(
                label.clone(),
                DockMenuLaunch {
                    launch: launch.clone(),
                },
            ),
            DockMenuEntry::Heading { label } => {
                MenuItem::action(label.clone(), DockMenuHeading).disabled(true)
            }
            DockMenuEntry::Separator => MenuItem::separator(),
        })
        .collect()
}

/// What earlier refreshes handed the platform.
#[derive(Debug, Default)]
pub struct LaunchMenuState {
    /// The jump list last set (`launchMenuKey`), kept only once Windows took it.
    jump_list: Option<Vec<JumpListCategory>>,
    /// The Dock menu last set (`launchMenuKey`).
    dock: Option<Vec<DockMenuEntry>>,
    /// The last result of setting the jump list; a failure is logged once, not on every retry
    /// (`lastJumpListResult`).
    last_result: Option<JumpListResult>,
    /// Entries the user removed from the jump list. Windows reports them only until the list
    /// is next set, so they are kept until restart (`removedJumpListArgs`).
    removed: HashSet<String>,
}

impl LaunchMenuState {
    /// The Dock menu for `recent`, or `None` when it is the menu already set.
    pub fn dock_update(&mut self, recent: &RecentLaunches) -> Option<Vec<DockMenuEntry>> {
        let entries = dock_menu_entries(recent);
        if self.dock.as_ref() == Some(&entries) {
            return None;
        }
        self.dock = Some(entries.clone());
        Some(entries)
    }

    /// Note the entries Windows reports removed (`removed_now`, their arguments), then the
    /// jump list for `recent`, or `None` when it is the list already set.
    pub fn jump_list_update(
        &mut self,
        recent: &RecentLaunches,
        launcher: &JumpListLauncher,
        removed_now: impl IntoIterator<Item = String>,
    ) -> Option<Vec<JumpListCategory>> {
        self.removed
            .extend(removed_now.into_iter().filter(|args| !args.is_empty()));
        let categories = jump_list_categories(recent, launcher, &self.removed);
        (self.jump_list.as_ref() != Some(&categories)).then_some(categories)
    }

    /// Record what setting `categories` reported. The list counts as set only when Windows
    /// took all of it, so the next refresh tries a failed list again. Returns the warning for
    /// the main log, if this failure is new.
    pub fn jump_list_set(
        &mut self,
        categories: Vec<JumpListCategory>,
        result: JumpListResult,
    ) -> Option<String> {
        let warning = if result == JumpListResult::Ok {
            self.jump_list = Some(categories);
            None
        } else if self.last_result != Some(result) {
            Some(format!(
                "the jump list was only partly set ({})",
                result.as_str()
            ))
        } else {
            None
        };
        self.last_result = Some(result);
        warning
    }
}

/// How the jump list starts Muxus: this executable. (Electron also passes the app folder from a
/// source checkout, where the executable is Electron itself.)
fn jump_list_launcher() -> JumpListLauncher {
    JumpListLauncher {
        program: std::env::current_exe()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
        args: Vec::new(),
    }
}

/// Keep `kind` up to date for the whole run (`startLaunchMenu`): read now, then every
/// [`LAUNCH_MENU_REFRESH`]. A Dock menu entry runs `deliver` with its launch.
pub fn start_launch_menu(
    backend: Backend,
    kind: LaunchMenuKind,
    app_id: String,
    deliver: impl Fn(CommandLineLaunch, &mut App) + 'static,
    cx: &mut App,
) {
    if kind == LaunchMenuKind::Dock {
        cx.on_action(move |action: &DockMenuLaunch, cx| deliver(action.launch.clone(), cx));
    }
    let launcher = jump_list_launcher();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut state = LaunchMenuState::default();
        loop {
            if let Err(error) = refresh(&backend, kind, &app_id, &launcher, &mut state, cx).await {
                main_log(
                    MainLogLevel::Warn,
                    "could not update the jump list or Dock menu",
                    Some(&error),
                );
            }
            cx.background_executor().timer(LAUNCH_MENU_REFRESH).await;
        }
    })
    .detach();
}

/// One refresh (`refreshLaunchMenu`).
async fn refresh(
    backend: &Backend,
    kind: LaunchMenuKind,
    app_id: &str,
    launcher: &JumpListLauncher,
    state: &mut LaunchMenuState,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let ((ssh, saved), workspaces) = futures_lite::future::zip(
        futures_lite::future::zip(backend.ssh_config(), backend.saved_hosts()),
        backend.workspaces(),
    )
    .await;
    let (ssh, saved, workspaces) = (
        ssh.map_err(|error| format!("/api/ssh/config: {error}"))?,
        saved.map_err(|error| format!("/api/profiles: {error}"))?,
        workspaces.map_err(|error| format!("/api/workspaces: {error}"))?,
    );
    let hosts: Vec<HostUse> = ssh
        .hosts
        .iter()
        .map(HostUse::ssh)
        .chain(saved.profiles.iter().map(HostUse::saved))
        .collect();
    let recent = recent_launches(&hosts, &workspaces);
    match kind {
        LaunchMenuKind::Dock => {
            if let Some(entries) = state.dock_update(&recent) {
                cx.update(|cx| cx.set_dock_menu(dock_menu_items(&entries)));
            }
        }
        LaunchMenuKind::JumpList => {
            let id = app_id.to_string();
            let removed = cx
                .background_executor()
                .spawn(async move { jump_list::removed_jump_list_args(&id) })
                .await
                // `getJumpListSettings` reports no removed entries when it cannot read them.
                .unwrap_or_default();
            if let Some(categories) = state.jump_list_update(&recent, launcher, removed) {
                let id = app_id.to_string();
                let (categories, result) = cx
                    .background_executor()
                    .spawn(async move {
                        let result = jump_list::set_jump_list(&id, &categories);
                        (categories, result)
                    })
                    .await;
                if let Some(warning) = state.jump_list_set(categories, result) {
                    main_log(MainLogLevel::Warn, &warning, None);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::launch_menu::RecentLaunch;
    use super::*;

    fn recent(hosts: &[&str]) -> RecentLaunches {
        RecentLaunches {
            hosts: hosts
                .iter()
                .map(|name| RecentLaunch {
                    title: name.to_string(),
                    launch: CommandLineLaunch::Host {
                        name: name.to_string(),
                    },
                })
                .collect(),
            workspaces: Vec::new(),
        }
    }

    fn launcher() -> JumpListLauncher {
        JumpListLauncher {
            program: r"C:\Program Files\Muxus\muxus.exe".into(),
            args: Vec::new(),
        }
    }

    fn args(categories: &[JumpListCategory]) -> Vec<Vec<String>> {
        categories
            .iter()
            .map(|category| {
                category
                    .items()
                    .iter()
                    .map(|item| item.args.clone())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn names_the_jump_list_after_the_installer_shortcut() {
        assert_eq!(app_user_model_id(false), "io.github.flosch62.muxus");
        assert_eq!(
            app_user_model_id(true),
            "io.github.flosch62.muxus.development"
        );
    }

    #[test]
    fn has_a_menu_only_where_the_platform_offers_one() {
        let expected = if cfg!(target_os = "macos") {
            Some(LaunchMenuKind::Dock)
        } else if cfg!(windows) {
            Some(LaunchMenuKind::JumpList)
        } else {
            None
        };
        assert_eq!(launch_menu_kind(false), expected);
        // Store packages keep the package's identity and get no jump list.
        let store = if cfg!(target_os = "macos") {
            Some(LaunchMenuKind::Dock)
        } else {
            None
        };
        assert_eq!(launch_menu_kind(true), store);
    }

    #[test]
    fn sets_the_dock_menu_again_only_when_it_changed() {
        let mut state = LaunchMenuState::default();
        assert!(state.dock_update(&recent(&["edge-1"])).is_some());
        assert_eq!(state.dock_update(&recent(&["edge-1"])), None);
        assert!(state.dock_update(&recent(&["edge-2", "edge-1"])).is_some());
    }

    #[test]
    fn sets_the_jump_list_again_only_when_it_changed() {
        let mut state = LaunchMenuState::default();
        let first = state
            .jump_list_update(&recent(&["edge-1"]), &launcher(), [])
            .unwrap();
        assert_eq!(state.jump_list_set(first, JumpListResult::Ok), None);
        assert_eq!(
            state.jump_list_update(&recent(&["edge-1"]), &launcher(), []),
            None
        );
        assert!(
            state
                .jump_list_update(&recent(&["edge-2", "edge-1"]), &launcher(), [])
                .is_some()
        );
    }

    #[test]
    fn tries_a_failed_jump_list_again_and_warns_once_per_failure() {
        let mut state = LaunchMenuState::default();
        let list = state
            .jump_list_update(&recent(&["edge-1"]), &launcher(), [])
            .unwrap();
        assert_eq!(
            state.jump_list_set(list, JumpListResult::CustomCategoryAccessDenied),
            Some("the jump list was only partly set (customCategoryAccessDeniedError)".into())
        );
        // Not kept: the next refresh sets it again, and the same failure stays quiet.
        let list = state
            .jump_list_update(&recent(&["edge-1"]), &launcher(), [])
            .unwrap();
        assert_eq!(
            state.jump_list_set(list, JumpListResult::CustomCategoryAccessDenied),
            None
        );
        let list = state
            .jump_list_update(&recent(&["edge-1"]), &launcher(), [])
            .unwrap();
        assert_eq!(
            state.jump_list_set(list, JumpListResult::Error),
            Some("the jump list was only partly set (error)".into())
        );
        let list = state
            .jump_list_update(&recent(&["edge-1"]), &launcher(), [])
            .unwrap();
        assert_eq!(state.jump_list_set(list, JumpListResult::Ok), None);
        // A failure after a success is new again.
        let list = state
            .jump_list_update(&recent(&["edge-2"]), &launcher(), [])
            .unwrap();
        assert!(state.jump_list_set(list, JumpListResult::Error).is_some());
    }

    #[test]
    fn keeps_entries_the_user_removed_out_until_restart() {
        let mut state = LaunchMenuState::default();
        let list = state
            .jump_list_update(
                &recent(&["edge-1", "edge-2"]),
                &launcher(),
                ["--host=edge-1".to_string(), String::new()],
            )
            .unwrap();
        assert_eq!(
            args(&list),
            [
                vec!["--host=edge-2".to_string()],
                vec!["--new-window".into()]
            ]
        );
        state.jump_list_set(list, JumpListResult::Ok);
        // Windows no longer reports the removal once the list was set; it stays out.
        assert_eq!(
            state.jump_list_update(&recent(&["edge-1", "edge-2"]), &launcher(), []),
            None
        );
    }

    #[test]
    fn builds_dock_items_with_disabled_headings() {
        let entries = [
            DockMenuEntry::Launch {
                label: "New Window".into(),
                launch: CommandLineLaunch::NewWindow,
            },
            DockMenuEntry::Separator,
            DockMenuEntry::Heading {
                label: "Recent Hosts".into(),
            },
            DockMenuEntry::Launch {
                label: "Edge Paris".into(),
                launch: CommandLineLaunch::Host {
                    name: "edge-1".into(),
                },
            },
        ];
        let items = dock_menu_items(&entries);
        assert_eq!(items.len(), 4);
        let MenuItem::Action {
            name,
            action,
            disabled,
            ..
        } = &items[0]
        else {
            panic!("New Window is an action");
        };
        assert_eq!(name.as_ref(), "New Window");
        assert!(!disabled);
        assert!(action.partial_eq(&DockMenuLaunch {
            launch: CommandLineLaunch::NewWindow
        }));
        assert!(matches!(items[1], MenuItem::Separator));
        let MenuItem::Action {
            name,
            action,
            disabled,
            ..
        } = &items[2]
        else {
            panic!("a heading is a disabled action");
        };
        assert_eq!(name.as_ref(), "Recent Hosts");
        assert!(disabled);
        assert!(action.partial_eq(&DockMenuHeading));
        let MenuItem::Action { action, .. } = &items[3] else {
            panic!("a host is an action");
        };
        assert!(action.partial_eq(&DockMenuLaunch {
            launch: CommandLineLaunch::Host {
                name: "edge-1".into()
            }
        }));
    }
}
