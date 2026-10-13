//! Settings: the native settings window, its sections and the keyboard
//! shortcuts editor (`client/src/layout/SettingsWindow.tsx`,
//! `components/SettingsDialog.tsx` and the section components it shows).
//!
//! The logic modules (drafts, local shell profiles, vault helpers, data
//! transfer) are plain Rust with unit tests; the GPUI views live in `view`.

pub mod data_transfer;
pub mod draft;
pub mod files;
pub mod hooks;
pub mod js;
pub mod palette;
pub mod prefs_model;
pub mod section;
pub mod texts;
pub mod vault_secrets;
pub mod vault_startup;
pub mod view;

pub use hooks::{SessionImportSource, SettingsHooks, set_settings_hooks};
pub use prefs_model::{PrefsChanged, SettingsPrefs};
pub use section::{SettingsSection, SettingsTarget};
pub use view::{SettingsWindow, ShortcutsDialog, open_settings_window, open_shortcuts};
