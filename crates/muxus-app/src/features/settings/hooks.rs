//! What the settings ask of the rest of the app. Some settings rows open
//! things that live in the main window (the log viewer, the session import
//! dialogs) or in the desktop shell (the in-app updater, link handlers); the
//! app registers them here with [`set_settings_hooks`]. A row whose hook is
//! missing says so in a toast instead of doing nothing.

use std::rc::Rc;

use muxus_core::api::DesktopUpdateState;
use muxus_ui::gpui_kit::{App, Global, Window};

/// Where sessions come from in the Backup & data section's import buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionImportSource {
    MobaXterm,
    SecureCrt,
}

pub type Hook = Rc<dyn Fn(&mut Window, &mut App)>;
pub type ImportHook = Rc<dyn Fn(SessionImportSource, &mut Window, &mut App)>;
pub type UpdateStateHook = Rc<dyn Fn(&App) -> Option<DesktopUpdateState>>;

/// The app's side of the settings (see the module docs).
#[derive(Clone, Default)]
pub struct SettingsHooks {
    /// Debug → View logs (`setLogViewerOpen(true)`).
    pub open_log_viewer: Option<Hook>,
    /// Backup & data → Import from MobaXterm / SecureCRT.
    pub import_sessions: Option<ImportHook>,
    /// The in-app updater (`DesktopUpdateControls.tsx`); without it About
    /// shows the plain update check.
    pub desktop_update: Option<DesktopUpdateHooks>,
}

/// The desktop shell's updater: its state and the actions the About page offers.
#[derive(Clone)]
pub struct DesktopUpdateHooks {
    pub state: UpdateStateHook,
    pub check: Hook,
    pub download: Hook,
    /// Restart to install a downloaded update (asked for first).
    pub install: Hook,
    pub open_store: Hook,
}

impl Global for SettingsHooks {}

/// Register the app's hooks (replacing earlier ones).
pub fn set_settings_hooks(hooks: SettingsHooks, cx: &mut App) {
    cx.set_global(hooks);
}

pub fn settings_hooks(cx: &App) -> SettingsHooks {
    cx.try_global::<SettingsHooks>()
        .cloned()
        .unwrap_or_default()
}
