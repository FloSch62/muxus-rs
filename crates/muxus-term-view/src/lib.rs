//! GPUI terminal view widget: the native counterpart of Muxus's xterm.js terminal
//! (`client/src/components/TerminalViewImpl.tsx` and its addons).
//!
//! [`TerminalView`] paints a [`muxus_term::terminal::Emulator`] on the cell grid and drives it
//! from any transport that speaks [`muxus_core::stream::TransportChannels`]: program output is
//! read from the channels, keystrokes, pastes, mouse reports, BREAK and resizes are written back.
//! It reproduces xterm's behaviour as Muxus configures it — key encoding (kitty keyboard
//! included), selection, search with overview-ruler marks, web/file/OSC 8 links, keyword
//! highlighting, kitty/Sixel/iTerm2 images, the bell, the scrollbar and the WCAG contrast floor —
//! and the look of the pane around it.
//!
//! The app embeds a view in a tab or split pane with [`new_terminal_view`], keeps its settings in
//! sync with [`TerminalView::set_settings`] and reacts to [`TerminalViewEvent`]s. Keyboard
//! shortcuts are the app's: bind the [actions](crate#actions) in the `Terminal` key context.

mod keys;
mod paint;
mod view;

pub use muxus_term;
pub use view::{TerminalView, new_terminal_view};

use muxus_core::stream::TerminalSize;
use muxus_term::keywords::KeywordHighlightRule;
use muxus_term::links::LinkActivation;
use muxus_term::palette::{TERMINAL_MINIMUM_CONTRAST_RATIO, TerminalTheme, terminal_scheme};
use muxus_term::right_click::RightClickAction;
use muxus_term::terminal::CursorShape;

gpui_kit::actions!(
    terminal,
    [
        /// Copy the selection (propagates when nothing is selected, so the key reaches the shell).
        Copy,
        /// Paste the clipboard.
        Paste,
        SelectAll,
        /// Open find in terminal.
        Find,
        FindNext,
        FindPrevious,
        CloseFind,
        /// Clear the screen and scrollback, keeping the prompt line.
        ClearScrollback,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        ScrollPageUp,
        ScrollPageDown,
        ScrollToTop,
        ScrollToBottom,
    ]
);

/// The preferences a terminal reads (`PrefsState` in `client/src/state/prefs.ts`, resolved for one
/// tab: the scheme and colors with host overrides applied, the effective keyword rules).
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalSettings {
    pub font_family: String,
    /// Base font size; zoom is added on top per view.
    pub font_size: f32,
    /// Line height multiplier (1.0 = font metrics).
    pub line_height: f32,
    pub cursor_style: CursorShape,
    pub cursor_blink: bool,
    /// Scrollback lines; takes effect for new terminals.
    pub scrollback: usize,
    /// The resolved theme (`themeWithColorOverrides(terminalScheme(..))`).
    pub theme: TerminalTheme,
    /// The scheme reads as light (selection and border handling differ).
    pub theme_is_light: bool,
    /// The application chrome is dark: the pane border disappears on dark schemes.
    pub app_dark: bool,
    pub copy_on_select: bool,
    pub right_click: RightClickAction,
    pub link_activation: LinkActivation,
    /// File paths are clickable (local shells, SSH sessions with SFTP).
    pub file_links: bool,
    pub allow_osc52_write: bool,
    /// Ask before pasting text with line breaks ([`TerminalViewEvent::ConfirmPaste`]).
    pub paste_warn_multiline: bool,
    /// The effective keyword-highlighting rules (global, profile and host rules resolved).
    pub keyword_rules: Vec<KeywordHighlightRule>,
    pub minimum_contrast_ratio: f64,
    pub is_mac: bool,
}

impl Default for TerminalSettings {
    /// Muxus's default preferences: JetBrains Mono 14, line height 1.0, blinking block cursor,
    /// 10 000 lines of scrollback, the VS Code dark scheme.
    fn default() -> Self {
        Self {
            font_family: "JetBrains Mono".into(),
            font_size: 14.0,
            line_height: 1.0,
            cursor_style: CursorShape::Block,
            cursor_blink: true,
            scrollback: 10_000,
            theme: terminal_scheme(None, &[]).theme.clone(),
            theme_is_light: false,
            app_dark: true,
            copy_on_select: false,
            right_click: RightClickAction::CopyPaste,
            link_activation: LinkActivation::Alt,
            file_links: false,
            allow_osc52_write: true,
            paste_warn_multiline: true,
            keyword_rules: Vec::new(),
            minimum_contrast_ratio: TERMINAL_MINIMUM_CONTRAST_RATIO,
            is_mac: cfg!(target_os = "macos"),
        }
    }
}

/// What a terminal view tells the app.
#[derive(Debug, Clone, PartialEq)]
pub enum TerminalViewEvent {
    /// The program set the window title (OSC 0/2).
    TitleChanged(String),
    Bell,
    /// The shell reported its working directory.
    CwdChanged(String),
    /// Open an http(s) URL in the browser (the configured link gesture on a web or OSC 8 link).
    OpenUrl(String),
    /// Open a file path as displayed; resolve it with `muxus_term::links::resolve_terminal_file_path`.
    OpenFile(String),
    /// Right click in context-menu mode: show the menu at this window position. The selection is
    /// the one that existed at the click.
    ContextMenu {
        position: gpui_kit::Point<gpui_kit::Pixels>,
        selection: String,
    },
    /// A multi-line paste waits for the app's confirmation; call
    /// [`TerminalView::paste_confirmed`] with the (possibly edited) text.
    ConfirmPaste(String),
    /// The clipboard held no text (an image, or nothing): the app may upload an image and paste
    /// its path.
    ClipboardWithoutText,
    /// The grid changed size (sent to the transport as well).
    Resized(TerminalSize),
    /// The user typed or pasted this, already sent to the session (multi-execution mirrors it).
    Input(Vec<u8>),
    /// The user typed into an ended session (the app offers to reconnect).
    InputAfterExit(Vec<u8>),
    /// Find was opened (the app shows its search box).
    SearchOpened,
    /// The session ended.
    Exited {
        code: Option<i32>,
        signal: Option<String>,
    },
    /// The transport failed.
    Failed(String),
}
