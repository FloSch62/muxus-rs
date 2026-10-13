//! Remote desktop sessions (RDP and VNC) on the backend: the native counterpart of
//! `server/src/remote-desktop/desktop-socket.ts` and `desktop-passwords.ts`, together with the
//! connection half of `client/src/components/RemoteDesktopViewImpl.tsx` (starting the RDP or VNC
//! client, retrying after a rejected logon, saying how the session ended).
//!
//! # Using it
//!
//! [`Backend::open_desktop_session`] takes the tab's [`DesktopProfile`] and returns a
//! [`DesktopHandle`]. The session runs on the backend runtime:
//!
//! 1. A saved host is dialled with its stored settings ([`DesktopEvent::Dialing`] says which).
//! 2. With an SSH gateway, the gateway connects first (host key and login prompts arrive as
//!    [`DesktopEvent::HostKey`] / [`DesktopEvent::AuthPrompt`], exactly as for a terminal),
//!    and the desktop is reached over a `direct-tcpip` channel of that connection.
//! 3. RDP needs the whole logon before it connects (NLA runs CredSSP in the client, TLS-only
//!    servers take it in the Client Info PDU): the saved password from the vault, or a
//!    `Remote Desktop logon` prompt. VNC asks in the middle of its security handshake.
//! 4. An RDP certificate or VNC server key that does not verify, or differs from the one
//!    pinned, becomes a [`DesktopEvent::Certificate`]; an accepted one is pinned in the
//!    database (keyed by host, port and gateway).
//! 5. A logon the server rejects is asked for again (not from the vault); anything else ends
//!    the session with [`DesktopEvent::Exit`].
//! 6. Once the desktop is up, [`DesktopEvent::Connected`] hands over the live
//!    [`DesktopConnection`] (input goes straight to it), the saved host's recent connection is
//!    recorded and a password marked to remember is saved. Picture, pointer and clipboard
//!    events follow until [`DesktopEvent::Exit`], always the last event.
//!
//! Answer prompts with [`DesktopHandle::answer_auth`], [`DesktopHandle::answer_host_key`] and
//! [`DesktopHandle::answer_certificate`]. [`DesktopHandle::close`] (or dropping the handle)
//! ends the session.
//!
//! The TypeScript build split this between a control socket, single-use tickets and stream
//! sockets the renderer's IronRDP and noVNC opened; natively the backend runs the clients
//! itself, so tickets, the RDCleanPath proxy and the `retry`/`connected`/`credentials-request`
//! messages have no counterpart.

mod session;
mod vault;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use muxus_core::protocol::{
    AuthPromptInfo, AuthPromptResponse, DesktopCertificateChallenge, DesktopProfile, ExitReason,
    HostKeyChallenge,
};

pub use muxus_rdp as rdp;
pub use muxus_rdp::{
    KeyEvent, Modifiers, MouseButton, ViewRect, WheelDelta, WheelUnit, desktop_point, fit_rect,
};
pub use muxus_vnc as vnc;
pub use session::DEMO_HOST_MAP_ENV;
pub use vault::StoreDesktopVault;

use crate::Backend;

/// How long the view waits for the pane to settle before asking the server to resize.
pub const RESIZE_SETTLE_MS: u64 = muxus_rdp::params::RESIZE_SETTLE_MS;

/// The desktop size RDP asks for when the pane is too small to measure (`width >= 200`).
pub const FALLBACK_DESKTOP_SIZE: (u16, u16) = muxus_rdp::params::FALLBACK_DESKTOP_SIZE;

/// Byte order of the shared framebuffer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PixelOrder {
    /// R, G, B, A.
    #[default]
    Rgba,
    /// B, G, R, A: what GPUI's `RenderImage` takes without a swizzle.
    Bgra,
}

/// What the view tells the session before it connects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DesktopOptions {
    /// The pane size in pixels: RDP asks the server for a desktop this size, VNC asks for it
    /// when the host resizes the remote session.
    pub width: f64,
    pub height: f64,
    pub pixel_order: PixelOrder,
}

impl Default for DesktopOptions {
    fn default() -> Self {
        Self {
            width: f64::from(FALLBACK_DESKTOP_SIZE.0),
            height: f64::from(FALLBACK_DESKTOP_SIZE.1),
            pixel_order: PixelOrder::Rgba,
        }
    }
}

/// The mouse pointer the server wants drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopCursor {
    /// The system's default arrow.
    Default,
    /// No pointer.
    Hidden,
    /// A pointer image: `width`×`height` RGBA, straight alpha.
    Image {
        width: u16,
        height: u16,
        hotspot_x: u16,
        hotspot_y: u16,
        rgba: Arc<Vec<u8>>,
    },
}

/// What a desktop session reports, in order. [`DesktopEvent::Exit`] is always the last.
#[derive(Debug, Clone)]
pub enum DesktopEvent {
    /// A progress line (`transient`) or a note worth keeping (`status` in the TypeScript
    /// protocol). Once the desktop is up, lasting notes become toasts.
    Status {
        message: String,
        transient: bool,
    },
    /// A login, vault or logon question; answer with [`DesktopHandle::answer_auth`].
    AuthPrompt(AuthPromptInfo),
    /// The SSH gateway's host key; answer with [`DesktopHandle::answer_host_key`].
    HostKey(HostKeyChallenge),
    /// An RDP certificate or VNC server key to decide about; answer with
    /// [`DesktopHandle::answer_certificate`].
    Certificate(DesktopCertificateChallenge),
    /// What is dialled: for a saved host its current settings, which may be newer than the
    /// tab's copy (`ready.profile`).
    Dialing(DesktopProfile),
    /// The desktop is up.
    Connected(DesktopConnection),
    /// The desktop has this size now; the framebuffer is entirely dirty and a
    /// [`DesktopEvent::Frame`] follows.
    Resized {
        width: u16,
        height: u16,
    },
    /// The framebuffer changed: take [`DesktopConnection::take_dirty`] and copy those regions.
    Frame,
    Cursor(DesktopCursor),
    /// The server moved the pointer (RDP).
    PointerPosition {
        x: u16,
        y: u16,
    },
    /// The remote clipboard holds this text (clipboard sharing on).
    RemoteClipboard(String),
    /// The name a VNC server announced.
    DesktopName(String),
    /// A VNC bell.
    Bell,
    /// The session ended. `reason` is `failed` before the desktop was ever up, otherwise
    /// `completed` (the server ended it) or `disconnected`.
    Exit {
        message: String,
        reason: ExitReason,
    },
}

/// What the UI sends to a running session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopInput {
    AuthResponse(AuthPromptResponse),
    HostKeyResponse {
        accept: bool,
    },
    CertificateResponse {
        accept: bool,
    },
    /// End the session (the tab's control socket closing).
    Close,
}

/// The UI's end of a desktop session. Dropping it closes the session.
#[derive(Debug)]
pub struct DesktopHandle {
    input: async_channel::Sender<DesktopInput>,
    events: async_channel::Receiver<DesktopEvent>,
}

impl DesktopHandle {
    /// The session's events. Take them from one place: clones share one queue.
    pub fn events(&self) -> async_channel::Receiver<DesktopEvent> {
        self.events.clone()
    }

    /// The next event; `None` once the session is gone (after [`DesktopEvent::Exit`]).
    pub async fn next_event(&self) -> Option<DesktopEvent> {
        self.events.recv().await.ok()
    }

    /// Answer the open auth prompt. `None` dismisses it, which closes the session, as
    /// dismissing the dialog does.
    pub fn answer_auth(&self, response: Option<AuthPromptResponse>) {
        self.send(match response {
            Some(response) => DesktopInput::AuthResponse(response),
            None => DesktopInput::Close,
        });
    }

    pub fn answer_host_key(&self, accept: bool) {
        self.send(DesktopInput::HostKeyResponse { accept });
    }

    pub fn answer_certificate(&self, accept: bool) {
        self.send(DesktopInput::CertificateResponse { accept });
    }

    /// End the session; [`DesktopEvent::Exit`] follows.
    pub fn close(&self) {
        self.send(DesktopInput::Close);
    }

    pub fn send(&self, input: DesktopInput) {
        // A session that is gone has nothing left to answer.
        let _ = self.input.try_send(input);
    }
}

/// A region of the desktop, in desktop pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The framebuffer while it is locked: row-major 4-byte pixels in the [`PixelOrder`] asked for.
#[derive(Debug, Clone, Copy)]
pub struct FramePixels<'a> {
    pub width: u32,
    pub height: u32,
    /// Bytes per row.
    pub stride: usize,
    pub pixels: &'a [u8],
}

/// A live RDP or VNC desktop. Input methods queue and return at once (call them from any
/// thread); clones share the session.
#[derive(Debug, Clone)]
pub enum DesktopConnection {
    Rdp(rdp::RdpSession),
    Vnc(vnc::VncSession),
}

fn vnc_key(event: &KeyEvent) -> vnc::KeyEvent {
    vnc::KeyEvent {
        code: event.code.clone(),
        key: event.key.clone(),
        pressed: event.pressed,
        repeat: event.repeat,
        modifiers: vnc::Modifiers {
            shift: event.modifiers.shift,
            ctrl: event.modifiers.ctrl,
            alt: event.modifiers.alt,
            meta: event.modifiers.meta,
            alt_graph: event.modifiers.alt_graph,
            caps_lock: event.modifiers.caps_lock,
            num_lock: event.modifiers.num_lock,
            scroll_lock: event.modifiers.scroll_lock,
        },
    }
}

fn vnc_button(button: MouseButton) -> vnc::MouseButton {
    match button {
        MouseButton::Left => vnc::MouseButton::Left,
        MouseButton::Middle => vnc::MouseButton::Middle,
        MouseButton::Right => vnc::MouseButton::Right,
        MouseButton::Back => vnc::MouseButton::Back,
        MouseButton::Forward => vnc::MouseButton::Forward,
    }
}

fn vnc_wheel(delta: WheelDelta) -> vnc::WheelDelta {
    vnc::WheelDelta {
        delta_x: delta.delta_x,
        delta_y: delta.delta_y,
        unit: match delta.unit {
            WheelUnit::Pixel => vnc::WheelUnit::Pixel,
            WheelUnit::Line => vnc::WheelUnit::Line,
            WheelUnit::Page => vnc::WheelUnit::Page,
        },
    }
}

impl DesktopConnection {
    pub fn is_rdp(&self) -> bool {
        matches!(self, Self::Rdp(_))
    }

    /// The desktop size as the server set it.
    pub fn desktop_size(&self) -> (u16, u16) {
        match self {
            Self::Rdp(session) => session.desktop_size(),
            Self::Vnc(session) => session.desktop_size(),
        }
    }

    /// The regions changed since the last call (call on every [`DesktopEvent::Frame`]).
    pub fn take_dirty(&self) -> Vec<DirtyRect> {
        match self {
            Self::Rdp(session) => session
                .framebuffer()
                .take_dirty()
                .into_iter()
                .map(|r| DirtyRect {
                    x: r.x,
                    y: r.y,
                    width: r.width,
                    height: r.height,
                })
                .collect(),
            Self::Vnc(session) => session
                .framebuffer()
                .take_dirty()
                .into_iter()
                .map(|r| DirtyRect {
                    x: r.x,
                    y: r.y,
                    width: r.width,
                    height: r.height,
                })
                .collect(),
        }
    }

    /// Read the framebuffer under its lock. Keep `read` short: the session waits on the lock.
    pub fn read_pixels<R>(&self, read: impl FnOnce(FramePixels<'_>) -> R) -> R {
        match self {
            Self::Rdp(session) => {
                let framebuffer = session.framebuffer();
                let state = framebuffer.lock();
                read(FramePixels {
                    width: state.width(),
                    height: state.height(),
                    stride: state.stride(),
                    pixels: state.pixels(),
                })
            }
            Self::Vnc(session) => {
                let framebuffer = session.framebuffer();
                let state = framebuffer.lock();
                read(FramePixels {
                    width: state.width(),
                    height: state.height(),
                    stride: state.stride(),
                    pixels: state.pixels(),
                })
            }
        }
    }

    /// Send a key. Returns `false` when the session does not take it (no RDP scancode for
    /// the key, a view-only VNC session), so the caller may let the event be.
    pub fn key(&self, event: &KeyEvent) -> bool {
        match self {
            Self::Rdp(session) => session.key(event),
            Self::Vnc(session) => session.key(&vnc_key(event)),
        }
    }

    /// The pointer moved to (x, y) in desktop pixels (see [`desktop_point`]).
    pub fn pointer_move(&self, x: u16, y: u16) {
        match self {
            Self::Rdp(session) => session.pointer_move(x, y),
            Self::Vnc(session) => session.pointer_move(x, y),
        }
    }

    pub fn pointer_button(&self, x: u16, y: u16, button: MouseButton, pressed: bool) {
        match self {
            Self::Rdp(session) => session.pointer_button(x, y, button, pressed),
            Self::Vnc(session) => session.pointer_button(x, y, vnc_button(button), pressed),
        }
    }

    /// A wheel event (DOM signs and units) with the pointer at (x, y).
    pub fn wheel(&self, x: u16, y: u16, delta: WheelDelta) {
        match self {
            Self::Rdp(session) => session.wheel(delta),
            Self::Vnc(session) => session.wheel(x, y, vnc_wheel(delta)),
        }
    }

    /// Let go of everything held down (focus left the desktop).
    pub fn release_all(&self) {
        match self {
            Self::Rdp(session) => session.release_all(),
            Self::Vnc(session) => session.release_all(),
        }
    }

    pub fn send_ctrl_alt_del(&self) {
        match self {
            Self::Rdp(session) => session.send_ctrl_alt_del(),
            Self::Vnc(session) => session.send_ctrl_alt_del(),
        }
    }

    /// The Windows key (RDP only; VNC has no toolbar button for it).
    pub fn send_windows_key(&self) {
        if let Self::Rdp(session) = self {
            session.send_windows_key();
        }
    }

    /// The pane is this size now: RDP asks the server for a matching desktop (display
    /// control); VNC does when remote resizing is on, otherwise the view scales.
    pub fn resize(&self, width: f64, height: f64) {
        match self {
            Self::Rdp(session) => session.resize(width, height),
            Self::Vnc(session) => session.resize(width, height),
        }
    }

    /// Offer local clipboard text to the remote side. Repeats of the text last seen on either
    /// side are skipped.
    pub fn send_clipboard_text(&self, text: &str) {
        match self {
            Self::Rdp(session) => session.send_clipboard_text(text, false),
            Self::Vnc(session) => session.send_clipboard_text(text),
        }
    }

    /// End the desktop session.
    pub fn shutdown(&self) {
        match self {
            Self::Rdp(session) => session.shutdown(),
            Self::Vnc(session) => session.disconnect(),
        }
    }

    pub fn is_closed(&self) -> bool {
        match self {
            Self::Rdp(session) => session.is_closed(),
            Self::Vnc(session) => session.is_closed(),
        }
    }
}

impl Backend {
    /// Open an RDP or VNC session for `profile` (the `/ws/desktop` control socket plus the
    /// client that used to run in the renderer). See the [module documentation](self).
    pub fn open_desktop_session(
        &self,
        profile: DesktopProfile,
        options: DesktopOptions,
    ) -> DesktopHandle {
        let (input, inputs) = async_channel::unbounded();
        let (sender, events) = async_channel::unbounded();
        let session = session::Session::new(
            self.database().clone(),
            self.vault().clone(),
            self.connections().clone(),
            sender,
            options,
        );
        self.runtime().spawn(session.run(profile, inputs));
        DesktopHandle { input, events }
    }
}
