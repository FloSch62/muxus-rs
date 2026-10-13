//! VNC transport: a native RFB client with what noVNC gives Muxus.
//!
//! The TypeScript build runs noVNC in the renderer and relays its WebSocket to the server
//! (`client/src/remote-desktop/vnc-client.ts`, `server/src/remote-desktop/desktop-socket.ts`).
//! Natively [`connect`] talks RFB directly (or through a tunnel the caller opened through an
//! SSH gateway): the security types noVNC supports (None, VNC authentication, RSA-AES,
//! Tight, VeNCrypt without TLS, Plain, XVP, Apple Remote Desktop, MSLogonII, Unix login),
//! its encodings (Raw, CopyRect, RRE, Hextile, Zlib, Tight, TightPNG, ZRLE, JPEG) and
//! pseudo-encodings (cursors, desktop size, QEMU key and LED events, fences, continuous
//! updates, extended clipboard, extended mouse buttons, XVP).
//!
//! The session draws into a shared [`Framebuffer`] and reports [`VncEvent`]s; input goes in
//! through [`VncSession`]. Credential prompts and server-key decisions go to the caller's
//! [`VncHandler`]; nothing here knows about the UI or the store.

mod crypto;
mod decoders;
pub mod errors;
pub mod framebuffer;
pub mod input;
mod keysym_tables;
mod protocol;
pub mod server_key;
mod session;

pub use errors::{VncFailure, VncFailureKind, connect_failure_message, describe_vnc_failure};
pub use framebuffer::{FrameState, Framebuffer, PixelOrder, Rect};
pub use input::{
    KeyEvent, Modifiers, MouseButton, Platform, ViewRect, WheelDelta, WheelUnit, desktop_point,
    fit_rect, keysym_for,
};
pub use server_key::{VncServerKey, server_key_challenge, server_key_subject, vnc_server_key};
pub use session::{
    BoxFuture, CONNECT_TIMEOUT, CursorShape, DesktopStream, HANDSHAKE_TIMEOUT, SessionEnd,
    VncConnectOptions, VncEvent, VncHandler, VncSession, VncTransport, connect,
};
