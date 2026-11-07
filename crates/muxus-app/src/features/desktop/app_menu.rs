//! The macOS app menu (`buildMenu` in `electron/src/main.ts`): the stock
//! items, except that About opens Settings → About. The native About panel
//! can't carry a link back to the repository, docs or issue tracker; the
//! settings page links to GitHub, the docs, release notes and the issue
//! tracker.
//!
//! Other platforms have no app menu; [`install`] only sets it on macOS.

// The `Action` derive names its crate `gpui`.
use muxus_ui::gpui_kit as gpui;

use gpui::{App, KeyBinding, Menu, MenuItem, SystemMenuType, actions};

use crate::features::settings::{SettingsSection, SettingsTarget, open_settings_window};

actions!(
    muxus_app_menu,
    [
        /// "About Muxus": Settings → About.
        AboutMuxus,
        Hide,
        HideOthers,
        ShowAll,
        Quit
    ]
);

/// The application's name in the menu bar (`app.name`).
pub const APP_NAME: &str = "Muxus";

/// The app menu: About first, then Services and the hide and quit items of
/// Electron's `appMenu` role.
pub fn app_menu() -> Menu {
    Menu::new(APP_NAME).items([
        MenuItem::action(format!("About {APP_NAME}"), AboutMuxus),
        MenuItem::separator(),
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action(format!("Hide {APP_NAME}"), Hide),
        MenuItem::action("Hide Others", HideOthers),
        MenuItem::action("Show All", ShowAll),
        MenuItem::separator(),
        MenuItem::action(format!("Quit {APP_NAME}"), Quit),
    ])
}

/// The labels of [`app_menu`]'s items, separators as `None` (tests).
pub fn app_menu_labels(menu: &Menu) -> Vec<Option<String>> {
    menu.items
        .iter()
        .map(|item| match item {
            MenuItem::Action { name, .. } => Some(name.to_string()),
            MenuItem::SystemMenu(menu) => Some(menu.name.to_string()),
            MenuItem::Submenu(menu) => Some(menu.name.to_string()),
            MenuItem::Separator => None,
        })
        .collect()
}

/// Handle the menu's actions and, on macOS, set the menu with the roles'
/// accelerators (⌘H, ⌥⌘H, ⌘Q).
pub fn install(cx: &mut App) {
    cx.on_action(|_: &AboutMuxus, cx| {
        open_settings_window(Some(SettingsTarget::section(SettingsSection::About)), cx);
    });
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Quit, cx| cx.quit());
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.set_menus([app_menu()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_leads_the_stock_app_menu() {
        let menu = app_menu();
        assert_eq!(menu.name.as_ref(), "Muxus");
        assert_eq!(
            app_menu_labels(&menu),
            [
                Some("About Muxus".into()),
                None,
                Some("Services".into()),
                None,
                Some("Hide Muxus".into()),
                Some("Hide Others".into()),
                Some("Show All".into()),
                None,
                Some("Quit Muxus".into()),
            ]
        );
        let MenuItem::Action { action, .. } = &menu.items[0] else {
            panic!("About is an action");
        };
        assert!(action.partial_eq(&AboutMuxus));
    }
}
