//! Theme tokens and shared widgets that reproduce the Muxus look in GPUI.
//!
//! The look is MUI as `client/src/theme.ts` configures it. [`theme::Theme`]
//! holds the resolved tokens; the widgets in [`widgets`] draw the MUI
//! components Muxus uses with the same geometry, so a screen built from them
//! lines up with the Electron app at the pixel level.

pub mod assets;
pub mod color;
pub mod fonts;
pub mod icon;
pub mod label;
pub mod syntax;
pub mod text;
pub mod theme;
pub mod widgets;
pub mod zoom;

/// The UI toolkit as every Muxus crate builds with it: `gpui-kit`, with [`zoom::px`] in
/// place of its `px` so every CSS length follows the interface scale.
pub mod gpui_kit {
    pub use crate::zoom::px;
    pub use ::gpui_kit::*;
}

use ::gpui_kit::{App, KeyBinding, NoAction};

/// Key context the terminal view (`muxus-term-view`) puts on its element.
pub const TERMINAL_KEY_CONTEXT: &str = "Terminal";

/// gpui-component binds Ctrl+Shift+I (Cmd+Alt+I on macOS) to its element
/// inspector in debug builds, window-wide. Inside a terminal that chord
/// belongs to the program running there, so it is unbound in the terminal's
/// key context; elsewhere it still opens the inspector, as Ctrl+Shift+I opens
/// the developer tools in the Electron build.
pub fn keep_terminal_chords(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-i", NoAction, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-i", NoAction, Some(TERMINAL_KEY_CONTEXT)),
    ]);
}

/// Initialize everything the widgets need: the component layers, the bundled
/// fonts and the theme for `mode`. Call once at startup, before opening
/// windows, and register [`assets::Assets`] with the application.
pub fn init(mode: theme::Mode, cx: &mut App) {
    ::gpui_kit::init(cx);
    keep_terminal_chords(cx);
    fonts::register(cx);
    theme::Theme::install(mode, cx);
    widgets::init(cx);
}
