//! The preferences the window reads: `PrefsState` from `muxus-store` (the
//! port of `client/src/state/prefs.ts`, every field with its default), plus
//! the labels and icons the chrome shows for some of them.

pub use muxus_store::prefs::PrefsState as Prefs;
pub use muxus_store::prefs::layout::STATUS_BAR_ITEMS;
pub use muxus_store::prefs::state::{SidebarPosition as Side, TabNumberVisibility, ThemeMode};

/// `APPEARANCE_OPTIONS`, in menu order.
pub const THEME_MODE_OPTIONS: [ThemeMode; 3] = [ThemeMode::Light, ThemeMode::Os, ThemeMode::Dark];

/// The label the appearance menu shows for a mode.
pub fn theme_mode_label(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::Light => "Light",
        ThemeMode::Dark => "Dark",
        ThemeMode::Os => "System",
    }
}

/// The icon `APPEARANCE_OPTIONS` shows for a mode.
pub fn theme_mode_icon(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::Light => "LightModeOutlined",
        ThemeMode::Dark => "DarkModeOutlined",
        ThemeMode::Os => "BrightnessAutoOutlined",
    }
}

/// `paneFocusOpacity`: inactive panes fade only when the preference asks.
pub fn pane_opacity(prefs: &Prefs, emphasized: bool) -> f32 {
    muxus_store::prefs::layout::pane_focus_opacity(
        emphasized,
        prefs.dim_inactive_panes,
        prefs.inactive_pane_dim_strength,
    ) as f32
}
