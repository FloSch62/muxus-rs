//! RDP and VNC tabs (port of `client/src/components/RemoteDesktopView*.tsx` and the view half
//! of `client/src/remote-desktop/*`).
//!
//! The session itself runs on the backend (`muxus_session::desktop`): saved-host resolution,
//! the SSH gateway, credentials and the vault, certificate and server-key pinning, retries
//! after a rejected logon. This feature draws what it reports and feeds it input:
//!
//! - [`view::RemoteDesktopView`]: the picture fitted into the pane (tiled images updated from
//!   the session's dirty rectangles, [`picture`]), the hover toolbar (Ctrl+Alt+Del, Windows
//!   key, Reconnect, Disconnect), the connecting and ended screens, auto-reconnect, clipboard
//!   sync, focus and key grabbing, pointer and wheel input, remote resizes after the pane
//!   settles;
//! - [`keys`]: GPUI keystrokes as the DOM `KeyboardEvent`s the RDP and VNC clients take.
//!
//! # Hooking it into the window
//!
//! - Call [`init`] once at startup (after `muxus_ui::init`).
//! - Open a tab or pane for a desktop host with [`open_remote_desktop_view`]; render the
//!   returned entity as the pane's content and keep it while the tab lives (dropping it closes
//!   the session).
//! - Tell it whether its tab is shown with [`view::RemoteDesktopView::set_active`], and pass
//!   the `autoReconnectRemote` preference with `set_auto_reconnect`.
//! - Subscribe to [`view::RemoteDesktopViewEvent`]: tab status (dot, failure and disconnect
//!   reasons), prompts to show in the shared auth / host-key / certificate dialogs (answer with
//!   `answer_auth`, `answer_host_key`, `answer_certificate`), and toasts.
//! - The tab's Reconnect and Disconnect actions are `reconnect` and `disconnect`.

pub mod keys;
pub mod picture;
pub mod view;

use muxus_ui::gpui_kit::{App, KeyBinding, NoAction};

pub use view::{
    DesktopPrompt, Phase, RemoteDesktopView, RemoteDesktopViewEvent, TabStatus,
    open_remote_desktop_view,
};

/// The key context a focused desktop puts on the window's key dispatch path.
pub const KEY_CONTEXT: &str = "RemoteDesktop";

/// Keep chords the window binds for itself out of a focused desktop: the developer
/// inspector's Ctrl+Shift+I (Cmd+Alt+I on macOS) belongs to the remote computer there, as it
/// went to the canvas in the TypeScript build. App shortcuts still run first, as they did.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-i", NoAction, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-i", NoAction, Some(KEY_CONTEXT)),
    ]);
}
