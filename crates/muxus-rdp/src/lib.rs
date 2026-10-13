//! RDP transport: a native Remote Desktop client on IronRDP.
//!
//! The TypeScript build runs IronRDP as WebAssembly in the renderer behind a Node proxy that
//! opens the TCP connection, does the X.224 and TLS legs and checks the certificate
//! (`client/src/remote-desktop/rdp-client.ts`, `server/src/remote-desktop/*`). Natively there
//! is no browser in the way, so [`connect`] does all of it: dial (or use a tunnel the caller
//! opened through an SSH gateway), negotiate, upgrade to TLS, hand the server certificate to
//! the caller's [`RdpHandler`], run CredSSP (NLA) and the connection sequence, then run the
//! session on a tokio task.
//!
//! The session draws into a shared [`Framebuffer`] and reports [`RdpEvent`]s; input goes in
//! through [`RdpSession`]'s methods, translated exactly like the TypeScript client translates
//! DOM events ([`input`]). Nothing here knows about the UI or the store: the caller decides
//! about certificates (with [`certificate_challenge`] and its pinned fingerprints) and
//! supplies the credentials.

pub mod certificates;
mod clipboard;
pub mod errors;
pub mod framebuffer;
pub mod input;
pub mod params;
pub mod passwords;
pub mod scancodes;
mod session;

pub use certificates::{PresentedCertificate, certificate_challenge, server_name};
pub use errors::{RdpError, RdpFailure, RdpFailureKind, SocketFailure};
pub use framebuffer::{FrameState, Framebuffer, PixelOrder, Rect};
pub use input::{
    InputOp, KeyEvent, Modifiers, MouseButton, RdpKeyboard, ViewRect, WheelDelta, WheelUnit,
    desktop_point, fit_rect,
};
pub use params::{RdpLogon, rdp_desktop_size, rdp_logon};
pub use passwords::{
    DesktopPasswords, DesktopPrompts, DesktopVault, LogonAnswer, LogonPrompt, VaultFailure,
    VaultPasswordRef,
};
pub use session::{
    BoxFuture, CONNECT_TIMEOUT, CursorShape, DesktopStream, HANDSHAKE_TIMEOUT, RdpConnectOptions,
    RdpEvent, RdpHandler, RdpSession, RdpTransport, SessionEnd, connect,
};
