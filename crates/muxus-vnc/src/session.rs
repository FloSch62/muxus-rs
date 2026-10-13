// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a
// copy of the MPL was not distributed with this file, You can obtain one at
// https://mozilla.org/MPL/2.0/. Adapted from noVNC (Copyright The noVNC authors).

//! One VNC connection: dial, the RFB handshake (security types noVNC supports), then the
//! session loop.
//!
//! Port of noVNC's `core/rfb.js` (MPL-2.0) and of `VncConnection`
//! (`client/src/remote-desktop/vnc-client.ts`). noVNC is a state machine fed by WebSocket
//! messages; here the handshake is straight-line async code and the session a task that reads
//! server messages and applies queued input.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use muxus_core::protocol::{DesktopCredentialType, DesktopCredentials};
use tokio::io::{
    AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::crypto;
use crate::decoders::Decoders;
use crate::errors::{VncFailure, VncFailureKind, connect_failure_message, describe_vnc_failure};
use crate::framebuffer::{Framebuffer, PixelOrder, Rect};
use crate::input::{
    KeyEvent, MouseButton, Platform, VncKeyboard, WheelAccumulator, WheelDelta, rfb_keycode,
};
use crate::keysym_tables::{self, XK_ALT_L, XK_CONTROL_L, XK_DELETE};
use crate::protocol::{self, *};
use crate::server_key::{VncServerKey, vnc_server_key};

/// How long dialling may take (`TCP_CONNECT_TIMEOUT_MS` in `server/src/util/dial.ts`).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the server may take to answer a handshake step (not counting the user's prompts).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// Minimum wait between two pointer moves.
const MOUSE_MOVE_DELAY: Duration = Duration::from_millis(17);
/// No more than one remote resize request per this long.
const RESIZE_INTERVAL: Duration = Duration::from_millis(100);

const SECURITY_NONE: u32 = 1;
const SECURITY_VNC_AUTH: u32 = 2;
const SECURITY_RA2NE: u32 = 6;
const SECURITY_TIGHT: u32 = 16;
const SECURITY_VENCRYPT: u32 = 19;
const SECURITY_XVP: u32 = 22;
const SECURITY_ARD: u32 = 30;
const SECURITY_MS_LOGON_II: u32 = 113;
const SECURITY_UNIX_LOGON: u32 = 129;
const SECURITY_PLAIN: u32 = 256;

const SUPPORTED_SECURITY: [u32; 9] = [
    SECURITY_NONE,
    SECURITY_VNC_AUTH,
    SECURITY_RA2NE,
    SECURITY_TIGHT,
    SECURITY_VENCRYPT,
    SECURITY_XVP,
    SECURITY_ARD,
    SECURITY_MS_LOGON_II,
    SECURITY_PLAIN,
];

/// A boxed future, for the handler's asynchronous answers.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A byte stream to the VNC server opened by someone else, such as an SSH channel.
pub trait DesktopStream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> DesktopStream for T {}

/// How to reach the server.
pub enum VncTransport {
    /// Dial `host:port` over TCP.
    Direct,
    /// Use this stream (an SSH gateway's channel).
    Tunnel(Box<dyn DesktopStream>),
}

impl std::fmt::Debug for VncTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct => f.write_str("Direct"),
            Self::Tunnel(_) => f.write_str("Tunnel"),
        }
    }
}

/// The decisions a connection needs from its owner, in the middle of the handshake.
pub trait VncHandler: Send + Sync + 'static {
    /// The server asked for these credentials (only servers with user logins ask for a user
    /// name). Resolve `None` to cancel the logon.
    fn credentials(
        &self,
        types: Vec<DesktopCredentialType>,
    ) -> BoxFuture<'static, Option<DesktopCredentials>>;

    /// An RSA-AES server presented its key; resolve `true` to continue. The handshake waits,
    /// and no credentials are sent until the key is trusted. The owner checks
    /// [`crate::server_key_challenge`] against the pinned fingerprint.
    fn verify_server_key(&self, key: VncServerKey) -> BoxFuture<'static, bool>;

    /// A transient progress line, such as `Connecting to host:5900 …`.
    fn status(&self, message: &str) {
        let _ = message;
    }
}

/// Everything one connection attempt needs, mirroring the host editor's options.
#[derive(Debug, Clone)]
pub struct VncConnectOptions {
    pub host: String,
    pub port: u16,
    /// Watch without sending keyboard or mouse input.
    pub view_only: bool,
    /// Ask the server to resize its desktop to the pane instead of scaling the picture.
    pub resize_remote: bool,
    /// Text clipboard sharing.
    pub share_clipboard: bool,
    /// Share the desktop with other clients (RFB ClientInit).
    pub shared: bool,
    /// The pane size, for remote resizes.
    pub width: f64,
    pub height: f64,
    /// JPEG quality 0–9 and compression 0–9 as noVNC asks for them.
    pub quality_level: u8,
    pub compression_level: u8,
    /// UltraVNC repeater ID (empty: none).
    pub repeater_id: String,
    pub pixel_order: PixelOrder,
    /// Which platform's keyboard quirks apply.
    pub platform: Platform,
}

impl VncConnectOptions {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            view_only: false,
            resize_remote: false,
            share_clipboard: true,
            shared: true,
            width: 0.0,
            height: 0.0,
            quality_level: 6,
            compression_level: 2,
            repeater_id: String::new(),
            pixel_order: PixelOrder::Rgba,
            platform: Platform::current(),
        }
    }

    /// `host:port`, as shown to the user.
    pub fn destination(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// The mouse pointer the server wants drawn locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorShape {
    /// No local pointer (also before the server sent any).
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

/// How a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    /// The server closed the connection.
    Completed,
    /// The connection broke or the server sent something unusable (the detail says what).
    Lost { detail: String },
    /// [`VncSession::disconnect`] closed it.
    Closed,
}

impl SessionEnd {
    /// The sentence the view shows on its "ended" screen.
    pub fn message(&self) -> String {
        match self {
            Self::Completed => "The VNC session ended.".into(),
            Self::Lost { .. } => "The connection to the VNC server was lost.".into(),
            Self::Closed => "The connection closed.".into(),
        }
    }
}

/// What a running session reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VncEvent {
    /// The desktop has this size now (first event, and after every change). The framebuffer kept
    /// what overlaps and is entirely dirty; a [`VncEvent::Frame`] follows unless one is pending.
    Resized {
        width: u16,
        height: u16,
    },
    /// The framebuffer changed; take [`Framebuffer::take_dirty`] on every `Frame`.
    Frame,
    Cursor(CursorShape),
    /// The remote clipboard holds this text (clipboard sharing on, not view-only).
    RemoteClipboard(String),
    DesktopName(String),
    Bell,
    /// The server supports XVP power operations.
    PowerCapability,
    /// The last event.
    Ended(SessionEnd),
}

#[derive(Debug)]
enum Command {
    Key(KeyEvent, Instant),
    PointerMove {
        x: u16,
        y: u16,
    },
    PointerButton {
        x: u16,
        y: u16,
        button: MouseButton,
        pressed: bool,
    },
    Wheel {
        x: u16,
        y: u16,
        delta: WheelDelta,
    },
    ReleaseAll,
    CtrlAltDel,
    Resize {
        width: f64,
        height: f64,
    },
    Clipboard(String),
    Xvp(u8),
    Disconnect,
}

/// A live VNC session. Input methods queue and return at once; clones share the session.
#[derive(Clone)]
pub struct VncSession {
    commands: mpsc::UnboundedSender<Command>,
    events: async_channel::Receiver<VncEvent>,
    framebuffer: Framebuffer,
    desktop_size: Arc<AtomicU32>,
    desktop_name: Arc<Mutex<String>>,
    closed: Arc<AtomicBool>,
    view_only: bool,
}

impl std::fmt::Debug for VncSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VncSession")
            .field("desktop_size", &self.desktop_size())
            .finish_non_exhaustive()
    }
}

fn pack_size(width: u16, height: u16) -> u32 {
    (u32::from(width) << 16) | u32::from(height)
}

impl VncSession {
    /// The session's events. Take them from one place: clones of the receiver share one queue.
    pub fn events(&self) -> async_channel::Receiver<VncEvent> {
        self.events.clone()
    }

    pub fn framebuffer(&self) -> Framebuffer {
        self.framebuffer.clone()
    }

    pub fn desktop_size(&self) -> (u16, u16) {
        let packed = self.desktop_size.load(Ordering::Relaxed);
        ((packed >> 16) as u16, packed as u16)
    }

    /// The desktop name the server announced.
    pub fn desktop_name(&self) -> String {
        self.desktop_name
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed) || self.commands.is_closed()
    }

    fn send(&self, command: Command) {
        if !self.is_closed() {
            // The session is going away; its events say why.
            let _ = self.commands.send(command);
        }
    }

    /// Send a key. Returns `false` in view-only sessions, which take no keyboard input.
    pub fn key(&self, event: &KeyEvent) -> bool {
        if self.view_only {
            return false;
        }
        self.send(Command::Key(event.clone(), Instant::now()));
        true
    }

    /// The pointer moved to (x, y) in desktop pixels (see [`crate::desktop_point`]).
    pub fn pointer_move(&self, x: u16, y: u16) {
        self.send(Command::PointerMove { x, y });
    }

    pub fn pointer_button(&self, x: u16, y: u16, button: MouseButton, pressed: bool) {
        self.send(Command::PointerButton {
            x,
            y,
            button,
            pressed,
        });
    }

    /// A wheel event with the pointer at (x, y).
    pub fn wheel(&self, x: u16, y: u16, delta: WheelDelta) {
        self.send(Command::Wheel { x, y, delta });
    }

    /// Release every held key (focus left the desktop).
    pub fn release_all(&self) {
        self.send(Command::ReleaseAll);
    }

    pub fn send_ctrl_alt_del(&self) {
        self.send(Command::CtrlAltDel);
    }

    /// The pane changed size. With remote resizing on, the server is asked for a desktop of
    /// this size; otherwise the view scales.
    pub fn resize(&self, width: f64, height: f64) {
        self.send(Command::Resize { width, height });
    }

    /// Hand local clipboard text to the server (VNC has no lazy clipboard). Text the server
    /// last offered or that was last sent is skipped.
    pub fn send_clipboard_text(&self, text: &str) {
        self.send(Command::Clipboard(text.to_owned()));
    }

    /// XVP power operations: 2 shutdown, 3 reboot, 4 reset (after [`VncEvent::PowerCapability`]).
    pub fn power(&self, operation: u8) {
        self.send(Command::Xvp(operation));
    }

    /// Close the session; [`VncEvent::Ended`] follows.
    pub fn disconnect(&self) {
        self.send(Command::Disconnect);
    }
}

type Reader = BufReader<ReadHalf<Box<dyn DesktopStream>>>;
type Writer = WriteHalf<Box<dyn DesktopStream>>;

/// noVNC's failure detail for a socket that closed mid-handshake.
fn closed(err: std::io::Error) -> String {
    if err.kind() == std::io::ErrorKind::UnexpectedEof {
        "Connection closed".to_owned()
    } else {
        format!("Connection closed ({err})")
    }
}

/// Fails the way noVNC does while connecting: the detail becomes a sentence for the user.
fn failure(detail: impl Into<String>) -> VncFailure {
    let detail = format!("Failed when connecting: {}", detail.into());
    VncFailure::failed(
        describe_vnc_failure(Some(&detail))
            .unwrap_or_else(|| "Could not open the VNC session.".into()),
    )
}

struct Handshake<'a> {
    reader: Reader,
    writer: Writer,
    handler: &'a dyn VncHandler,
    options: &'a VncConnectOptions,
    version: f32,
    tight: bool,
    asked_for_credentials: bool,
    credentials: Option<DesktopCredentials>,
}

/// What the handshake left: the session's streams and the server's first words.
struct ServerInit {
    reader: Reader,
    writer: Writer,
    width: u16,
    height: u16,
    name: String,
}

/// A handshake step against the server's clock.
async fn timed<T>(
    what: &str,
    future: impl Future<Output = std::io::Result<T>>,
) -> Result<T, VncFailure> {
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(failure(closed(err))),
        Err(_) => Err(failure(format!("timed out waiting for the {what}"))),
    }
}

impl Handshake<'_> {
    async fn read_exact(&mut self, what: &str, len: usize) -> Result<Vec<u8>, VncFailure> {
        let mut buf = vec![0u8; len];
        timed(what, self.reader.read_exact(&mut buf)).await?;
        Ok(buf)
    }

    async fn u8(&mut self, what: &str) -> Result<u8, VncFailure> {
        Ok(self.read_exact(what, 1).await?[0])
    }

    async fn u16(&mut self, what: &str) -> Result<u16, VncFailure> {
        let bytes = self.read_exact(what, 2).await?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    async fn u32(&mut self, what: &str) -> Result<u32, VncFailure> {
        let bytes = self.read_exact(what, 4).await?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), VncFailure> {
        let writer = &mut self.writer;
        timed("server", async {
            writer.write_all(data).await?;
            writer.flush().await
        })
        .await
    }

    /// The credentials the server asks for, from the handler (once per connection attempt).
    async fn credentials(
        &mut self,
        types: &[DesktopCredentialType],
    ) -> Result<DesktopCredentials, VncFailure> {
        if let Some(credentials) = &self.credentials {
            let has_user =
                credentials.username.is_some() || !types.contains(&DesktopCredentialType::Username);
            if has_user && credentials.password.is_some() {
                return Ok(credentials.clone());
            }
        }
        self.asked_for_credentials = true;
        let answer = self
            .handler
            .credentials(types.to_vec())
            .await
            .ok_or_else(|| VncFailure::failed("Logon cancelled."))?;
        self.credentials = Some(answer.clone());
        Ok(answer)
    }

    async fn protocol_version(&mut self) -> Result<(), VncFailure> {
        loop {
            let version = self.read_exact("protocol version", 12).await?;
            let version = String::from_utf8_lossy(&version[4..11]).into_owned();
            self.version = match version.as_str() {
                // UltraVNC repeater.
                "000.000" => {
                    let mut id = format!("ID:{}", self.options.repeater_id).into_bytes();
                    id.resize(250, 0);
                    self.send(&id).await?;
                    continue;
                }
                // 003.006 is UltraVNC.
                "003.003" | "003.006" => 3.3,
                "003.007" => 3.7,
                // Apple Remote Desktop, Intel AMT KVM, RealVNC 4.6 and 5.3.
                "003.008" | "003.889" | "004.000" | "004.001" | "005.000" => 3.8,
                other => return Err(failure(format!("Invalid server version {other}"))),
            };
            let reply = match self.version {
                v if v >= 3.8 => "RFB 003.008\n",
                v if v >= 3.7 => "RFB 003.007\n",
                _ => "RFB 003.003\n",
            };
            return self.send(reply.as_bytes()).await;
        }
    }

    /// The security reason that follows a failure, as the `securityfailure` event carries it.
    async fn security_reason(&mut self) -> Result<Option<String>, VncFailure> {
        let length = self.u32("security reason").await? as usize;
        if length == 0 {
            return Ok(None);
        }
        let reason = self
            .read_exact("security reason", length.min(1 << 16))
            .await?;
        Ok(Some(String::from_utf8_lossy(&reason).into_owned()))
    }

    async fn security_type(&mut self) -> Result<u32, VncFailure> {
        if self.version >= 3.7 {
            // The server sends its list, the client decides.
            let count = self.u8("security types").await?;
            if count == 0 {
                let reason = self.security_reason().await?;
                return Err(VncFailure::security(reason, false));
            }
            let types = self
                .read_exact("security types", usize::from(count))
                .await?;
            let Some(&chosen) = types
                .iter()
                .find(|&&kind| SUPPORTED_SECURITY.contains(&u32::from(kind)))
            else {
                let list = types
                    .iter()
                    .map(u8::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                return Err(failure(format!(
                    "Unsupported security types (types: {list})"
                )));
            };
            self.send(&[chosen]).await?;
            Ok(u32::from(chosen))
        } else {
            // The server decides.
            let scheme = self.u32("security scheme").await?;
            if scheme == 0 {
                let reason = self.security_reason().await?;
                return Err(VncFailure::security(reason, false));
            }
            Ok(scheme)
        }
    }

    /// Authenticate with `scheme`; returns whether a SecurityResult follows.
    async fn authenticate(&mut self, mut scheme: u32) -> Result<bool, VncFailure> {
        loop {
            match scheme {
                SECURITY_NONE => return Ok(self.version >= 3.8),
                SECURITY_VNC_AUTH => {
                    let challenge: [u8; 16] = self
                        .read_exact("auth challenge", 16)
                        .await?
                        .try_into()
                        .expect("16 bytes");
                    let credentials = self.credentials(&[DesktopCredentialType::Password]).await?;
                    let password = credentials.password.unwrap_or_default();
                    let response = crypto::vnc_auth_response(&password, &challenge);
                    self.send(&response).await?;
                    return Ok(true);
                }
                SECURITY_XVP => {
                    // Muxus has no XVP target field; an empty target signs in to the default.
                    let credentials = self
                        .credentials(&[
                            DesktopCredentialType::Username,
                            DesktopCredentialType::Password,
                        ])
                        .await?;
                    let username = credentials.username.unwrap_or_default();
                    let mut message = vec![username.len().min(255) as u8, 0];
                    message.extend(&username.as_bytes()[..username.len().min(255)]);
                    self.send(&message).await?;
                    scheme = SECURITY_VNC_AUTH;
                }
                SECURITY_VENCRYPT => scheme = self.vencrypt().await?,
                SECURITY_PLAIN => {
                    let credentials = self
                        .credentials(&[
                            DesktopCredentialType::Username,
                            DesktopCredentialType::Password,
                        ])
                        .await?;
                    let user = credentials.username.unwrap_or_default();
                    let pass = credentials.password.unwrap_or_default();
                    let mut message = (user.len() as u32).to_be_bytes().to_vec();
                    message.extend((pass.len() as u32).to_be_bytes());
                    message.extend(user.as_bytes());
                    message.extend(pass.as_bytes());
                    self.send(&message).await?;
                    return Ok(true);
                }
                SECURITY_TIGHT => match self.tight_auth().await? {
                    Some(next) => scheme = next,
                    None => return Ok(true),
                },
                SECURITY_UNIX_LOGON => {
                    let credentials = self
                        .credentials(&[
                            DesktopCredentialType::Username,
                            DesktopCredentialType::Password,
                        ])
                        .await?;
                    let user = credentials.username.unwrap_or_default();
                    let pass = credentials.password.unwrap_or_default();
                    let mut message = (user.len() as u32).to_be_bytes().to_vec();
                    message.extend((pass.len() as u32).to_be_bytes());
                    message.extend(user.as_bytes());
                    message.extend(pass.as_bytes());
                    self.send(&message).await?;
                    return Ok(true);
                }
                SECURITY_ARD => {
                    self.ard().await?;
                    return Ok(true);
                }
                SECURITY_MS_LOGON_II => {
                    self.ms_logon().await?;
                    return Ok(true);
                }
                SECURITY_RA2NE => {
                    self.rsa_aes().await?;
                    return Ok(true);
                }
                other => {
                    return Err(failure(format!(
                        "Unsupported auth scheme (scheme: {other})"
                    )));
                }
            }
        }
    }

    /// VeNCrypt 0.2, subtypes without TLS only; returns the chosen subtype.
    async fn vencrypt(&mut self) -> Result<u32, VncFailure> {
        let major = self.u8("vencrypt version").await?;
        let minor = self.u8("vencrypt version").await?;
        if !(major == 0 && minor == 2) {
            return Err(failure(format!(
                "Unsupported VeNCrypt version {major}.{minor}"
            )));
        }
        self.send(&[0, 2]).await?;
        let ack = self.u8("vencrypt ack").await?;
        if ack != 0 {
            return Err(failure(format!("VeNCrypt failure {ack}")));
        }
        let count = self.u8("vencrypt subtypes length").await?;
        if count < 1 {
            return Err(failure("VeNCrypt subtypes empty"));
        }
        let mut subtypes = Vec::new();
        for _ in 0..count {
            subtypes.push(self.u32("vencrypt subtypes").await?);
        }
        // Avoid getting into a loop.
        let Some(&chosen) = subtypes
            .iter()
            .find(|&&kind| kind != SECURITY_VENCRYPT && SUPPORTED_SECURITY.contains(&kind))
        else {
            let list = subtypes
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            return Err(failure(format!(
                "Unsupported security types (types: {list})"
            )));
        };
        self.send(&chosen.to_be_bytes()).await?;
        Ok(chosen)
    }

    /// Tight security: tunnel and sub-authentication negotiation. Returns the sub-scheme, or
    /// `None` when no authentication was selected.
    async fn tight_auth(&mut self) -> Result<Option<u32>, VncFailure> {
        if !self.tight {
            let tunnels = self.u32("num tunnels").await?;
            self.tight = true;
            if tunnels > 0 {
                let mut server_types = Vec::new();
                for _ in 0..tunnels.min(256) {
                    let code = self.u32("tunnel capabilities").await?;
                    let vendor = self.read_exact("tunnel capabilities", 4).await?;
                    let signature = self.read_exact("tunnel capabilities", 8).await?;
                    server_types.push((code, vendor, signature));
                }
                // Siemens touch panels support NOTUNNEL but forget to say so; they advertise
                // their own SICR/SCHANNEL type instead.
                let siemens = server_types.iter().any(|(code, vendor, sig)| {
                    *code == 1 && vendor == b"SICR" && sig == b"SCHANNEL"
                });
                match server_types.iter().find(|(code, _, _)| *code == 0) {
                    Some((_, vendor, signature)) => {
                        if vendor != b"TGHT" || signature != b"NOTUNNEL" {
                            return Err(failure(
                                "Client's tunnel type had the incorrect vendor or signature",
                            ));
                        }
                    }
                    None if siemens => {}
                    None => {
                        return Err(failure(
                            "Server wanted tunnels, but doesn't support the notunnel type",
                        ));
                    }
                }
                self.send(&0u32.to_be_bytes()).await?;
            }
        }
        let count = self.u32("sub auth count").await?;
        if count == 0 {
            // An empty sub-auth list means 'no auth'.
            return Ok(None);
        }
        let mut server_types = Vec::new();
        for _ in 0..count.min(256) {
            let _code = self.u32("sub auth capabilities").await?;
            let name = self.read_exact("sub auth capabilities", 12).await?;
            server_types.push(String::from_utf8_lossy(&name).into_owned());
        }
        for (name, scheme) in [
            ("STDVNOAUTH__", SECURITY_NONE),
            ("STDVVNCAUTH_", SECURITY_VNC_AUTH),
            ("TGHTULGNAUTH", SECURITY_UNIX_LOGON),
        ] {
            if server_types.iter().any(|offered| offered == name) {
                self.send(&scheme.to_be_bytes()).await?;
                return Ok((scheme != SECURITY_NONE).then_some(scheme));
            }
        }
        Err(failure("No supported sub-auth types!"))
    }

    async fn ard(&mut self) -> Result<(), VncFailure> {
        let generator = self.read_exact("read ard", 2).await?;
        let key_length = usize::from(self.u16("read ard").await?);
        let prime = self.read_exact("read ard keylength", key_length).await?;
        let server_key = self.read_exact("read ard keylength", key_length).await?;
        let credentials = self
            .credentials(&[
                DesktopCredentialType::Username,
                DesktopCredentialType::Password,
            ])
            .await?;
        let response = crypto::ard_response(
            &generator,
            &prime,
            &server_key,
            &credentials.username.unwrap_or_default(),
            &credentials.password.unwrap_or_default(),
        );
        let mut message = response.credentials.to_vec();
        message.extend(response.public_key);
        self.send(&message).await
    }

    async fn ms_logon(&mut self) -> Result<(), VncFailure> {
        let params = self.read_exact("mslogonii dh param", 24).await?;
        let credentials = self
            .credentials(&[
                DesktopCredentialType::Username,
                DesktopCredentialType::Password,
            ])
            .await?;
        let take8 = |at: usize| -> [u8; 8] { params[at..at + 8].try_into().expect("8 bytes") };
        let response = crypto::ms_logon_response(
            &take8(0),
            &take8(8),
            &take8(16),
            &credentials.username.unwrap_or_default(),
            &credentials.password.unwrap_or_default(),
        );
        let mut message = response.public_key.to_vec();
        message.extend(response.username);
        message.extend(response.password);
        self.send(&message).await
    }

    /// RSA-AES without session encryption (RA2ne): the server proves its key, both sides
    /// exchange randoms under RSA, and the credentials travel AES-EAX encrypted.
    async fn rsa_aes(&mut self) -> Result<(), VncFailure> {
        let ra2 = |message: &str| failure(format!("RA2: {message}"));
        // 1: Receive the server public key.
        let length_bytes = self.read_exact("RA2", 4).await?;
        let server_bits = u32::from_be_bytes(length_bytes[..4].try_into().expect("4 bytes"));
        if server_bits < 1024 {
            return Err(ra2(&format!(
                "server public key is too short: {server_bits}"
            )));
        } else if server_bits > 8192 {
            return Err(ra2(&format!(
                "server public key is too long: {server_bits}"
            )));
        }
        let server_bytes = server_bits.div_ceil(8) as usize;
        let modulus = self.read_exact("RA2", server_bytes).await?;
        let exponent = self.read_exact("RA2", server_bytes).await?;
        let mut server_public = length_bytes;
        server_public.extend(&modulus);
        server_public.extend(&exponent);

        // Verify the server key before anything else goes out.
        if !self
            .handler
            .verify_server_key(vnc_server_key(&server_public))
            .await
        {
            return Err(VncFailure::new(
                "The VNC server key was not trusted.",
                VncFailureKind::Refused,
            ));
        }
        let server_key = crypto::rsa_public_key(&modulus, &exponent).map_err(failure)?;

        // 2: Send the client public key.
        let client_key = tokio::task::spawn_blocking(crypto::rsa_client_key)
            .await
            .map_err(|err| failure(format!("RA2: {err}")))?
            .map_err(failure)?;
        let client_public = crypto::rsa_public_blob(&client_key);
        self.send(&client_public).await?;
        let client_bytes = 256usize;

        // 3: Send the client random.
        let client_random: [u8; 16] = crypto::random();
        let encrypted = crypto::rsa_encrypt(&server_key, &client_random).map_err(failure)?;
        let mut message = (server_bytes as u16).to_be_bytes().to_vec();
        message.extend(encrypted);
        self.send(&message).await?;

        // 4: Receive the server random.
        if usize::from(self.u16("RA2").await?) != client_bytes {
            return Err(ra2("wrong encrypted message length"));
        }
        let encrypted = self.read_exact("RA2", client_bytes).await?;
        let server_random = crypto::rsa_decrypt(&client_key, &encrypted)
            .filter(|random| random.len() == 16)
            .ok_or_else(|| ra2("corrupted server encrypted random"))?;

        // 5: Session keys.
        let client_session_key = crypto::sha1(&[&server_random[..], &client_random].concat());
        let server_session_key = crypto::sha1(&[&client_random[..], &server_random].concat());
        let mut client_cipher = crypto::Ra2Cipher::new(&client_session_key[..16]);
        let mut server_cipher = crypto::Ra2Cipher::new(&server_session_key[..16]);

        // 6: Exchange hashes of both public keys.
        let server_hash = crypto::sha1(&[&server_public[..], &client_public].concat());
        let client_hash = crypto::sha1(&[&client_public[..], &server_public].concat());
        self.send(&client_cipher.make_message(&client_hash)).await?;
        if self.u16("RA2").await? != 20 {
            return Err(ra2("wrong server hash"));
        }
        let encrypted = self.read_exact("RA2", 20 + 16).await?;
        let received = server_cipher
            .receive_message(20, &encrypted)
            .ok_or_else(|| ra2("failed to authenticate the message"))?;
        if received != server_hash {
            return Err(ra2("wrong server hash"));
        }

        // 7: The subtype says which credentials.
        if self.u16("RA2").await? != 1 {
            return Err(ra2("wrong subtype"));
        }
        let encrypted = self.read_exact("RA2", 1 + 16).await?;
        let subtype = server_cipher
            .receive_message(1, &encrypted)
            .ok_or_else(|| ra2("failed to authenticate the message"))?[0];
        let types: &[DesktopCredentialType] = match subtype {
            1 => &[
                DesktopCredentialType::Username,
                DesktopCredentialType::Password,
            ],
            2 => &[DesktopCredentialType::Password],
            _ => return Err(ra2("wrong subtype")),
        };
        let credentials = self.credentials(types).await?;
        let username = if subtype == 1 {
            credentials.username.unwrap_or_default()
        } else {
            String::new()
        };
        let password = credentials.password.unwrap_or_default();
        let user = &username.as_bytes()[..username.len().min(255)];
        let pass = &password.as_bytes()[..password.len().min(255)];
        let mut plain = vec![user.len() as u8];
        plain.extend(user);
        plain.push(pass.len() as u8);
        plain.extend(pass);
        self.send(&client_cipher.make_message(&plain)).await
    }

    async fn security_result(&mut self) -> Result<(), VncFailure> {
        let status = self.u32("VNC auth response").await?;
        if status == 0 {
            return Ok(());
        }
        let reason = if self.version >= 3.8 {
            self.security_reason().await?
        } else {
            None
        };
        Err(VncFailure::security(reason, self.asked_for_credentials))
    }

    async fn run(mut self) -> Result<ServerInit, VncFailure> {
        self.protocol_version().await?;
        let scheme = self.security_type().await?;
        if self.authenticate(scheme).await? {
            self.security_result().await?;
        }
        // ClientInit.
        self.send(&[u8::from(self.options.shared)]).await?;
        // ServerInit.
        let head = self.read_exact("server initialization", 24).await?;
        let width = u16::from_be_bytes([head[0], head[1]]);
        let height = u16::from_be_bytes([head[2], head[3]]);
        let name_length = u32::from_be_bytes([head[20], head[21], head[22], head[23]]) as usize;
        let name = self
            .read_exact("server init name", name_length.min(1 << 16))
            .await?;
        let name = String::from_utf8_lossy(&name).into_owned();
        if self.tight {
            // TightVNC extends ServerInit with capability lists nothing here uses.
            let counts = self
                .read_exact("TightVNC extended server init header", 8)
                .await?;
            let total = (usize::from(u16::from_be_bytes([counts[0], counts[1]]))
                + usize::from(u16::from_be_bytes([counts[2], counts[3]]))
                + usize::from(u16::from_be_bytes([counts[4], counts[5]])))
                * 16;
            self.read_exact("TightVNC extended server init header", total)
                .await?;
        }
        Ok(ServerInit {
            reader: self.reader,
            writer: self.writer,
            width,
            height,
            name,
        })
    }
}

async fn dial(options: &VncConnectOptions) -> Result<TcpStream, VncFailure> {
    let host = options.host.trim_start_matches('[').trim_end_matches(']');
    let connect = async {
        let addresses = tokio::net::lookup_host((host, options.port)).await?;
        let mut last = None;
        for address in addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(err) => last = Some(err),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("failed to lookup address information")))
    };
    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => return Err(VncFailure::failed(connect_failure_message(&err))),
        Err(_) => return Err(VncFailure::failed("The connection timed out.")),
    };
    // Pointer and key events are tiny; do not let Nagle hold them back.
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

/// Connect, sign in and start the session. Returns once the server sent its ServerInit (noVNC's
/// `connect` event); the session then runs on its own task (call within a tokio runtime).
pub async fn connect(
    options: VncConnectOptions,
    transport: VncTransport,
    handler: Arc<dyn VncHandler>,
) -> Result<VncSession, VncFailure> {
    let stream: Box<dyn DesktopStream> = match transport {
        VncTransport::Direct => {
            handler.status(&format!("Connecting to {} …", options.destination()));
            Box::new(dial(&options).await?)
        }
        VncTransport::Tunnel(stream) => stream,
    };
    let (read_half, writer) = tokio::io::split(stream);
    let handshake = Handshake {
        reader: BufReader::with_capacity(256 * 1024, read_half),
        writer,
        handler: handler.as_ref(),
        options: &options,
        version: 0.0,
        tight: false,
        asked_for_credentials: false,
        credentials: None,
    };
    let init = handshake.run().await?;

    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = async_channel::unbounded();
    let framebuffer = Framebuffer::new(options.pixel_order);
    let desktop_size = Arc::new(AtomicU32::new(pack_size(init.width, init.height)));
    let desktop_name = Arc::new(Mutex::new(init.name.clone()));
    let closed = Arc::new(AtomicBool::new(false));
    let session = VncSession {
        commands: commands_tx,
        events: events_rx,
        framebuffer: framebuffer.clone(),
        desktop_size: desktop_size.clone(),
        desktop_name: desktop_name.clone(),
        closed: closed.clone(),
        view_only: options.view_only,
    };
    // Intel AMT KVM only supports 8/16 bit depths.
    let depth = if init.name == "Intel(r) AMT KVM" {
        8
    } else {
        24
    };
    let mut run = SessionLoop {
        events: events_tx,
        framebuffer,
        desktop_size,
        desktop_name,
        options,
        depth,
        width: 0,
        height: 0,
        keyboard: VncKeyboard::new(Platform::current()),
        decoders: Decoders::new(),
        qemu_key_events: false,
        extended_pointer: false,
        supports_set_desktop_size: false,
        screen_id: 0,
        screen_flags: 0,
        pending_remote_resize: false,
        last_resize: None,
        resize_due: None,
        continuous_updates: false,
        continuous_updates_enabled: false,
        clipboard_formats: 0,
        clipboard_actions: 0,
        clipboard_offer: None,
        last_clipboard: None,
        button_mask: 0,
        pointer: (0, 0),
        last_move: None,
        pending_move: None,
        wheel: WheelAccumulator::default(),
        xvp_version: 0,
    };
    run.keyboard = VncKeyboard::new(run.options.platform);
    tokio::spawn(async move {
        let end = run
            .run(
                init.reader,
                init.writer,
                commands_rx,
                init.width,
                init.height,
                &init.name,
            )
            .await;
        closed.store(true, Ordering::Relaxed);
        end
    });
    Ok(session)
}

struct SessionLoop {
    events: async_channel::Sender<VncEvent>,
    framebuffer: Framebuffer,
    desktop_size: Arc<AtomicU32>,
    desktop_name: Arc<Mutex<String>>,
    options: VncConnectOptions,
    depth: u8,
    width: u16,
    height: u16,
    keyboard: VncKeyboard,
    decoders: Decoders,
    qemu_key_events: bool,
    extended_pointer: bool,
    supports_set_desktop_size: bool,
    screen_id: u32,
    screen_flags: u32,
    pending_remote_resize: bool,
    last_resize: Option<Instant>,
    resize_due: Option<Instant>,
    continuous_updates: bool,
    continuous_updates_enabled: bool,
    /// The server's extended clipboard capabilities.
    clipboard_formats: u32,
    clipboard_actions: u32,
    /// Text announced with Notify, provided when the server requests it.
    clipboard_offer: Option<String>,
    /// The clipboard text last seen on either side.
    last_clipboard: Option<String>,
    button_mask: u16,
    pointer: (u16, u16),
    last_move: Option<Instant>,
    pending_move: Option<(u16, u16)>,
    wheel: WheelAccumulator,
    xvp_version: u8,
}

/// Why the loop stopped.
enum Stop {
    End(SessionEnd),
}

type Step<T = ()> = Result<T, Stop>;

fn lost(detail: impl Into<String>) -> Stop {
    Stop::End(SessionEnd::Lost {
        detail: detail.into(),
    })
}

fn read_failed(err: std::io::Error) -> Stop {
    if err.kind() == std::io::ErrorKind::UnexpectedEof {
        Stop::End(SessionEnd::Completed)
    } else {
        lost(format!("Connection closed ({err})"))
    }
}

fn sleep_until(deadline: Option<Instant>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    match deadline {
        Some(deadline) => Box::pin(tokio::time::sleep_until(deadline.into())),
        None => Box::pin(std::future::pending()),
    }
}

impl SessionLoop {
    fn emit(&self, event: VncEvent) {
        let _ = self.events.try_send(event);
    }

    async fn write(&mut self, writer: &mut Writer, message: &[u8]) -> Step {
        writer
            .write_all(message)
            .await
            .map_err(|err| lost(format!("Connection closed ({err})")))?;
        writer
            .flush()
            .await
            .map_err(|err| lost(format!("Connection closed ({err})")))
    }

    async fn run(
        &mut self,
        mut reader: Reader,
        mut writer: Writer,
        mut commands: mpsc::UnboundedReceiver<Command>,
        width: u16,
        height: u16,
        name: &str,
    ) -> SessionEnd {
        let end = match self
            .run_inner(&mut reader, &mut writer, &mut commands, width, height, name)
            .await
        {
            Ok(()) => SessionEnd::Closed,
            Err(Stop::End(end)) => end,
        };
        self.emit(VncEvent::Ended(end.clone()));
        end
    }

    async fn run_inner(
        &mut self,
        reader: &mut Reader,
        writer: &mut Writer,
        commands: &mut mpsc::UnboundedReceiver<Command>,
        width: u16,
        height: u16,
        name: &str,
    ) -> Step {
        self.emit(VncEvent::DesktopName(name.to_owned()));
        self.resize_framebuffer(width, height);
        // The initial cursor is none, until the server sends one (TigerVNC, Remmina and
        // others show an arrow; noVNC shows nothing).
        self.emit(VncEvent::Cursor(CursorShape::Hidden));
        self.write(writer, &protocol::set_pixel_format(self.depth))
            .await?;
        let encodings = protocol::encodings(
            self.depth,
            self.options.quality_level,
            self.options.compression_level,
        );
        self.write(writer, &protocol::set_encodings(&encodings))
            .await?;
        let request = protocol::framebuffer_update_request(false, 0, 0, self.width, self.height);
        self.write(writer, &request).await?;

        loop {
            let move_due = self
                .pending_move
                .and(self.last_move.map(|last| last + MOUSE_MOVE_DELAY));
            tokio::select! {
                kind = reader.read_u8() => {
                    let kind = kind.map_err(read_failed)?;
                    self.message(kind, reader, writer).await?;
                }
                command = commands.recv() => {
                    let Some(command) = command else {
                        // Every handle is gone.
                        return Ok(());
                    };
                    if matches!(command, Command::Disconnect) {
                        return Ok(());
                    }
                    self.command(command, writer).await?;
                }
                () = sleep_until(move_due) => {
                    if let Some((x, y)) = self.pending_move.take() {
                        self.send_mouse(writer, x, y, self.button_mask).await?;
                        self.last_move = Some(Instant::now());
                    }
                }
                () = sleep_until(self.keyboard.alt_gr_deadline()) => {
                    let messages = self.keyboard.interrupt_alt_gr();
                    self.send_keys(writer, messages).await?;
                }
                () = sleep_until(self.resize_due) => {
                    self.resize_due = None;
                    self.request_remote_resize(writer).await?;
                }
            }
        }
    }

    fn resize_framebuffer(&mut self, width: u16, height: u16) {
        // Servers repeat ExtendedDesktopSize as their desktop settles; a size that did not
        // change keeps the picture as it is.
        if (width, height) == (self.width, self.height) && self.framebuffer.size() != (0, 0) {
            return;
        }
        self.width = width;
        self.height = height;
        let first = self
            .framebuffer
            .lock()
            .resize(u32::from(width), u32::from(height));
        self.desktop_size
            .store(pack_size(width, height), Ordering::Relaxed);
        self.emit(VncEvent::Resized { width, height });
        if first {
            self.emit(VncEvent::Frame);
        }
    }

    async fn resize(&mut self, writer: &mut Writer, width: u16, height: u16) -> Step {
        self.resize_framebuffer(width, height);
        self.update_continuous_updates(writer).await
    }

    async fn update_continuous_updates(&mut self, writer: &mut Writer) -> Step {
        if !self.continuous_updates_enabled {
            return Ok(());
        }
        let message = protocol::enable_continuous_updates(true, 0, 0, self.width, self.height);
        self.write(writer, &message).await
    }

    /// `_requestRemoteResize`: ask the server for a desktop the size of the pane.
    async fn request_remote_resize(&mut self, writer: &mut Writer) -> Step {
        if !self.options.resize_remote
            || self.options.view_only
            || !self.supports_set_desktop_size
            || self.pending_remote_resize
        {
            return Ok(());
        }
        // No more than once every 100 ms.
        if let Some(last) = self.last_resize
            && last.elapsed() < RESIZE_INTERVAL
        {
            self.resize_due = Some(last + RESIZE_INTERVAL);
            return Ok(());
        }
        let width = self.options.width.floor().clamp(0.0, 65535.0) as u16;
        let height = self.options.height.floor().clamp(0.0, 65535.0) as u16;
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return Ok(());
        }
        self.pending_remote_resize = true;
        self.last_resize = Some(Instant::now());
        let message = protocol::set_desktop_size(width, height, self.screen_id, self.screen_flags);
        self.write(writer, &message).await
    }

    async fn send_mouse(&mut self, writer: &mut Writer, x: u16, y: u16, mask: u16) -> Step {
        if self.options.view_only {
            return Ok(());
        }
        let message = if self.extended_pointer && mask & 0x7f80 != 0 {
            protocol::extended_pointer_event(x, y, mask)
        } else {
            protocol::pointer_event(x, y, mask)
        };
        self.write(writer, &message).await
    }

    /// Flush a waiting move, then send a new button state.
    async fn mouse_button(&mut self, writer: &mut Writer, x: u16, y: u16, mask: u16) -> Step {
        if self.pending_move.take().is_some() {
            self.send_mouse(writer, x, y, self.button_mask).await?;
        }
        self.button_mask = mask;
        self.send_mouse(writer, x, y, mask).await
    }

    async fn send_keys(
        &mut self,
        writer: &mut Writer,
        messages: Vec<crate::input::KeyMessage>,
    ) -> Step {
        if self.options.view_only {
            return Ok(());
        }
        for message in messages {
            let scancode = keysym_tables::xt_scancode(&message.code);
            let bytes = match scancode {
                Some(scancode) if self.qemu_key_events => protocol::qemu_extended_key_event(
                    message.keysym,
                    message.down,
                    rfb_keycode(scancode),
                ),
                _ if message.keysym == 0 => continue,
                _ => protocol::key_event(message.keysym, message.down),
            };
            self.write(writer, &bytes).await?;
        }
        Ok(())
    }

    async fn command(&mut self, command: Command, writer: &mut Writer) -> Step {
        match command {
            Command::Key(event, at) => {
                let messages = self.keyboard.key(&event, at);
                self.send_keys(writer, messages).await
            }
            Command::ReleaseAll => {
                let messages = self.keyboard.release_all();
                self.send_keys(writer, messages).await
            }
            Command::CtrlAltDel => {
                let key = |keysym, code: &str, down| crate::input::KeyMessage {
                    keysym,
                    code: code.to_owned(),
                    down,
                };
                let messages = vec![
                    key(XK_CONTROL_L, "ControlLeft", true),
                    key(XK_ALT_L, "AltLeft", true),
                    key(XK_DELETE, "Delete", true),
                    key(XK_DELETE, "Delete", false),
                    key(XK_ALT_L, "AltLeft", false),
                    key(XK_CONTROL_L, "ControlLeft", false),
                ];
                self.send_keys(writer, messages).await
            }
            Command::PointerMove { x, y } => {
                self.pointer = (x, y);
                // Limit moves to one every MOUSE_MOVE_DELAY.
                let now = Instant::now();
                match self.last_move {
                    Some(last) if now.duration_since(last) <= MOUSE_MOVE_DELAY => {
                        self.pending_move = Some((x, y));
                        Ok(())
                    }
                    _ => {
                        self.pending_move = None;
                        self.last_move = Some(now);
                        self.send_mouse(writer, x, y, self.button_mask).await
                    }
                }
            }
            Command::PointerButton {
                x,
                y,
                button,
                pressed,
            } => {
                let mask = if pressed {
                    self.button_mask | button.rfb_mask()
                } else {
                    self.button_mask & !button.rfb_mask()
                };
                self.mouse_button(writer, x, y, mask).await
            }
            Command::Wheel { x, y, delta } => {
                if self.options.view_only {
                    return Ok(());
                }
                for mask in self.wheel.steps(delta, self.button_mask) {
                    self.mouse_button(writer, x, y, mask).await?;
                }
                Ok(())
            }
            Command::Resize { width, height } => {
                self.options.width = width;
                self.options.height = height;
                self.request_remote_resize(writer).await
            }
            Command::Clipboard(text) => self.clipboard_paste(writer, text).await,
            Command::Xvp(operation) => {
                if self.xvp_version < 1 {
                    return Ok(());
                }
                self.write(writer, &protocol::xvp_operation(1, operation))
                    .await
            }
            Command::Disconnect => Ok(()),
        }
    }

    /// `clipboardPasteFrom` plus the TypeScript client's sharing and repeat checks.
    async fn clipboard_paste(&mut self, writer: &mut Writer, text: String) -> Step {
        if !self.options.share_clipboard
            || self.options.view_only
            || self.last_clipboard.as_deref() == Some(text.as_str())
        {
            return Ok(());
        }
        self.last_clipboard = Some(text.clone());
        if self.clipboard_formats & CLIPBOARD_FORMAT_TEXT != 0
            && self.clipboard_actions & CLIPBOARD_ACTION_NOTIFY != 0
        {
            self.clipboard_offer = Some(text);
            let message = protocol::extended_clipboard_notify(CLIPBOARD_FORMAT_TEXT);
            self.write(writer, &message).await
        } else {
            self.write(
                writer,
                &protocol::client_cut_text(&protocol::latin1(&text), false),
            )
            .await
        }
    }

    fn remote_clipboard(&mut self, text: String) {
        if !self.options.share_clipboard || self.options.view_only {
            return;
        }
        self.last_clipboard = Some(text.clone());
        self.emit(VncEvent::RemoteClipboard(text));
    }

    async fn message(&mut self, kind: u8, reader: &mut Reader, writer: &mut Writer) -> Step {
        let io = |err: std::io::Error| read_failed(err);
        match kind {
            0 => {
                self.framebuffer_update(reader, writer).await?;
                if !self.continuous_updates_enabled {
                    let request =
                        protocol::framebuffer_update_request(true, 0, 0, self.width, self.height);
                    self.write(writer, &request).await?;
                }
                Ok(())
            }
            1 => Err(lost("Unexpected SetColorMapEntries message")),
            2 => {
                self.emit(VncEvent::Bell);
                Ok(())
            }
            3 => self.server_cut_text(reader, writer).await,
            150 => {
                // EndOfContinuousUpdates: the server supports them; turn them on the first time.
                let first = !self.continuous_updates;
                self.continuous_updates = true;
                self.continuous_updates_enabled = false;
                if first {
                    self.continuous_updates_enabled = true;
                    self.update_continuous_updates(writer).await?;
                }
                Ok(())
            }
            248 => {
                let mut head = [0u8; 8];
                reader.read_exact(&mut head).await.map_err(io)?;
                let flags = u32::from_be_bytes([head[3], head[4], head[5], head[6]]);
                let length = usize::from(head[7]);
                let mut payload = vec![0u8; length];
                reader.read_exact(&mut payload).await.map_err(io)?;
                payload.truncate(64);
                if flags & (1 << 31) == 0 {
                    return Err(lost("Unexpected fence response"));
                }
                // BlockBefore and BlockAfter hold by processing every message in order.
                let message = protocol::client_fence(flags & 0b11, &payload);
                self.write(writer, &message).await
            }
            250 => {
                let mut body = [0u8; 3];
                reader.read_exact(&mut body).await.map_err(io)?;
                match body[2] {
                    // XVP_FAIL.
                    0 => Ok(()),
                    // XVP_INIT.
                    1 => {
                        self.xvp_version = body[1];
                        self.emit(VncEvent::PowerCapability);
                        Ok(())
                    }
                    other => Err(lost(format!("Illegal server XVP message (msg: {other})"))),
                }
            }
            other => Err(lost(format!("Unexpected server message (type {other})"))),
        }
    }

    async fn server_cut_text(&mut self, reader: &mut Reader, writer: &mut Writer) -> Step {
        let io = |err: std::io::Error| read_failed(err);
        let mut head = [0u8; 7];
        reader.read_exact(&mut head).await.map_err(io)?;
        let length = i32::from_be_bytes([head[3], head[4], head[5], head[6]]);
        let size = length.unsigned_abs() as usize;
        if size > 64 * 1024 * 1024 {
            return Err(lost("clipboard data too large"));
        }
        let mut data = vec![0u8; size];
        reader.read_exact(&mut data).await.map_err(io)?;
        if length >= 0 {
            // Classic ServerCutText: ISO 8859-1.
            let text: String = data.iter().map(|&byte| char::from(byte)).collect();
            self.remote_clipboard(text);
            return Ok(());
        }
        if data.len() < 4 {
            return Err(lost("Unexpected extended clipboard message"));
        }
        let flags = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let formats = flags & 0x0000_ffff;
        let actions = flags & 0xff00_0000;
        if actions & CLIPBOARD_ACTION_CAPS != 0 {
            self.clipboard_formats = formats;
            self.clipboard_actions = actions;
            // Answer with ours.
            return self
                .write(writer, &protocol::extended_clipboard_caps())
                .await;
        }
        if self.options.view_only {
            return Ok(());
        }
        match actions {
            CLIPBOARD_ACTION_REQUEST => {
                if let Some(text) = self.clipboard_offer.clone()
                    && self.clipboard_actions & CLIPBOARD_ACTION_PROVIDE != 0
                    && formats & CLIPBOARD_FORMAT_TEXT != 0
                {
                    return self
                        .write(writer, &protocol::extended_clipboard_provide(&text))
                        .await;
                }
                Ok(())
            }
            CLIPBOARD_ACTION_PEEK => {
                if self.clipboard_actions & CLIPBOARD_ACTION_NOTIFY != 0 {
                    let formats = if self.clipboard_offer.is_some() {
                        CLIPBOARD_FORMAT_TEXT
                    } else {
                        0
                    };
                    return self
                        .write(writer, &protocol::extended_clipboard_notify(formats))
                        .await;
                }
                Ok(())
            }
            CLIPBOARD_ACTION_NOTIFY => {
                if self.clipboard_actions & CLIPBOARD_ACTION_REQUEST != 0
                    && formats & CLIPBOARD_FORMAT_TEXT != 0
                {
                    return self
                        .write(
                            writer,
                            &protocol::extended_clipboard_request(CLIPBOARD_FORMAT_TEXT),
                        )
                        .await;
                }
                Ok(())
            }
            CLIPBOARD_ACTION_PROVIDE => {
                if formats & CLIPBOARD_FORMAT_TEXT == 0 {
                    return Ok(());
                }
                // Whatever we offered is superseded.
                self.clipboard_offer = None;
                if let Some(text) = extended_clipboard_text(formats, &data[4..]) {
                    self.remote_clipboard(text);
                }
                Ok(())
            }
            other => Err(lost(format!(
                "Unexpected action in extended clipboard message: {other}"
            ))),
        }
    }

    async fn framebuffer_update(&mut self, reader: &mut Reader, writer: &mut Writer) -> Step {
        let io = |err: std::io::Error| read_failed(err);
        let mut head = [0u8; 3];
        reader.read_exact(&mut head).await.map_err(io)?;
        let mut rects = u16::from_be_bytes([head[1], head[2]]);
        while rects > 0 {
            let mut header = [0u8; 12];
            reader.read_exact(&mut header).await.map_err(io)?;
            let x = u16::from_be_bytes([header[0], header[1]]);
            let y = u16::from_be_bytes([header[2], header[3]]);
            let width = u16::from_be_bytes([header[4], header[5]]);
            let height = u16::from_be_bytes([header[6], header[7]]);
            let encoding = i32::from_be_bytes([header[8], header[9], header[10], header[11]]);
            let rect = Rect::new(
                u32::from(x),
                u32::from(y),
                u32::from(width),
                u32::from(height),
            );
            rects -= 1;
            match encoding {
                PSEUDO_LAST_RECT => break,
                PSEUDO_VMWARE_CURSOR => self.vmware_cursor(rect, reader).await?,
                PSEUDO_CURSOR => self.cursor(rect, reader).await?,
                PSEUDO_QEMU_EXTENDED_KEY_EVENT => self.qemu_key_events = true,
                PSEUDO_DESKTOP_NAME => {
                    let length = reader.read_u32().await.map_err(io)? as usize;
                    let mut name = vec![0u8; length.min(1 << 16)];
                    reader.read_exact(&mut name).await.map_err(io)?;
                    let name = String::from_utf8_lossy(&name).into_owned();
                    *self
                        .desktop_name
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = name.clone();
                    self.emit(VncEvent::DesktopName(name));
                }
                PSEUDO_DESKTOP_SIZE => self.resize(writer, width, height).await?,
                PSEUDO_EXTENDED_DESKTOP_SIZE => {
                    self.extended_desktop_size(x, y, width, height, reader, writer)
                        .await?;
                }
                PSEUDO_EXTENDED_MOUSE_BUTTONS => self.extended_pointer = true,
                PSEUDO_QEMU_LED_EVENT => {
                    let state = reader.read_u8().await.map_err(io)?;
                    // Scroll Lock is bit 0 and not needed.
                    self.keyboard
                        .set_remote_locks(state & 4 != 0, state & 2 != 0);
                }
                _ => {
                    if !Decoders::supports(encoding) {
                        return Err(lost(format!("Unsupported encoding (encoding: {encoding})")));
                    }
                    self.decoders
                        .decode(encoding, rect, self.depth, reader, &self.framebuffer)
                        .await
                        .map_err(|err| {
                            if err.starts_with("Connection closed") {
                                lost(err)
                            } else {
                                lost(format!("Error decoding rect: {err}"))
                            }
                        })?;
                    let first = self.framebuffer.lock().mark_dirty(rect);
                    if first {
                        self.emit(VncEvent::Frame);
                    }
                }
            }
        }
        Ok(())
    }

    async fn extended_desktop_size(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        reader: &mut Reader,
        writer: &mut Writer,
    ) -> Step {
        let io = |err: std::io::Error| read_failed(err);
        let mut head = [0u8; 4];
        reader.read_exact(&mut head).await.map_err(io)?;
        let screens = usize::from(head[0]);
        let mut data = vec![0u8; screens * 16];
        reader.read_exact(&mut data).await.map_err(io)?;
        let first_update = !self.supports_set_desktop_size;
        self.supports_set_desktop_size = true;
        // Remember the first screen's id and flags.
        if screens > 0 {
            self.screen_id = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            self.screen_flags = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
        }
        // x says why the size changed: 0 the server, 1 this client, 2 another client; with
        // x == 1, y is the server's verdict on our request.
        if x == 1 {
            self.pending_remote_resize = false;
        }
        if x == 1 && y != 0 {
            tracing::warn!(status = y, "the server did not accept the resize request");
        } else {
            self.resize(writer, width, height).await?;
        }
        // There is no window resize to trigger the first request, and only now is it known that
        // the server supports resizing.
        if first_update {
            self.request_remote_resize(writer).await?;
        }
        // The pane may have changed again while waiting for the previous request.
        if x == 1 && y == 0 {
            self.request_remote_resize(writer).await?;
        }
        Ok(())
    }

    async fn cursor(&mut self, rect: Rect, reader: &mut Reader) -> Step {
        let io = |err: std::io::Error| read_failed(err);
        let (w, h) = (rect.width as usize, rect.height as usize);
        let mask_row = w.div_ceil(8);
        let mut pixels = vec![0u8; w * h * 4];
        let mut mask = vec![0u8; mask_row * h];
        reader.read_exact(&mut pixels).await.map_err(io)?;
        reader.read_exact(&mut mask).await.map_err(io)?;
        // The pixels are in our pixel format (R, G, B, padding); the bitmask says which show.
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let at = (y * w + x) * 4;
                let visible = (mask[y * mask_row + x / 8] << (x % 8)) & 0x80 != 0;
                rgba[at..at + 3].copy_from_slice(&pixels[at..at + 3]);
                rgba[at + 3] = if visible { 255 } else { 0 };
            }
        }
        self.update_cursor(rect, rgba);
        Ok(())
    }

    async fn vmware_cursor(&mut self, rect: Rect, reader: &mut Reader) -> Step {
        let io = |err: std::io::Error| read_failed(err);
        let kind = reader.read_u8().await.map_err(io)?;
        reader.read_u8().await.map_err(io)?;
        let pixels = rect.width as usize * rect.height as usize;
        let mut rgba = vec![0u8; pixels * 4];
        match kind {
            // Classic cursor: AND and XOR masks.
            0 => {
                let mut and_mask = vec![0u8; pixels * 4];
                let mut xor_mask = vec![0u8; pixels * 4];
                reader.read_exact(&mut and_mask).await.map_err(io)?;
                reader.read_exact(&mut xor_mask).await.map_err(io)?;
                for pixel in 0..pixels {
                    let at = pixel * 4;
                    let and = u32::from_be_bytes(and_mask[at..at + 4].try_into().expect("4"));
                    let xor = u32::from_be_bytes(xor_mask[at..at + 4].try_into().expect("4"));
                    const PIXEL_MASK: u32 = 0xffff_ff00;
                    let out = if and == 0 {
                        // Fully opaque pixel.
                        [(xor >> 8) as u8, (xor >> 16) as u8, (xor >> 24) as u8, 0xff]
                    } else if and & PIXEL_MASK == PIXEL_MASK && xor == 0 {
                        // Transparent.
                        [0, 0, 0, 0]
                    } else {
                        // Inverted or unhandled pixels: opaque black.
                        [0, 0, 0, 0xff]
                    };
                    rgba[at..at + 4].copy_from_slice(&out);
                }
            }
            // Alpha cursor: RGBA.
            1 => {
                reader.read_exact(&mut rgba).await.map_err(io)?;
            }
            other => {
                tracing::warn!(kind = other, "unsupported VMware cursor type");
                return Err(lost(format!(
                    "The given cursor type is not supported: {other} given."
                )));
            }
        }
        self.update_cursor(rect, rgba);
        Ok(())
    }

    fn update_cursor(&self, rect: Rect, rgba: Vec<u8>) {
        let shape = if rect.width == 0 || rect.height == 0 {
            CursorShape::Hidden
        } else {
            CursorShape::Image {
                width: rect.width as u16,
                height: rect.height as u16,
                // The hotspot travels in the rectangle's position.
                hotspot_x: rect.x as u16,
                hotspot_y: rect.y as u16,
                rgba: Arc::new(rgba),
            }
        };
        self.emit(VncEvent::Cursor(shape));
    }
}

/// The text in an extended clipboard Provide: per format a length-prefixed block, deflated.
fn extended_clipboard_text(formats: u32, compressed: &[u8]) -> Option<String> {
    let mut inflate = flate2::Decompress::new(true);
    let mut plain = Vec::with_capacity(compressed.len() * 4 + 64);
    loop {
        if plain.len() == plain.capacity() {
            if plain.len() > 64 * 1024 * 1024 {
                return None;
            }
            plain.reserve(plain.capacity());
        }
        let before = inflate.total_out();
        let consumed = inflate.total_in() as usize;
        let status = inflate
            .decompress_vec(
                &compressed[consumed..],
                &mut plain,
                flate2::FlushDecompress::Sync,
            )
            .ok()?;
        if status == flate2::Status::StreamEnd
            || (inflate.total_out() == before && inflate.total_in() as usize >= compressed.len())
        {
            break;
        }
        if inflate.total_out() == before && plain.len() < plain.capacity() {
            break;
        }
    }
    let mut at = 0;
    let mut text = None;
    for bit in 0..16 {
        let format = 1u32 << bit;
        if formats & format == 0 {
            continue;
        }
        let size = u32::from_be_bytes(plain.get(at..at + 4)?.try_into().ok()?) as usize;
        at += 4;
        let chunk = plain.get(at..at + size)?;
        at += size;
        if format == CLIPBOARD_FORMAT_TEXT {
            text = Some(chunk.to_vec());
        }
    }
    let text = String::from_utf8_lossy(&text?).into_owned();
    let text = text
        .strip_suffix('\0')
        .unwrap_or(&text)
        .replace("\r\n", "\n");
    Some(text)
}

#[cfg(test)]
mod send_probe {
    use super::*;

    fn assert_send<T: Send>(value: T) -> T {
        value
    }

    /// Callers spawn connections on their own runtimes.
    #[allow(dead_code)]
    fn connect_can_be_spawned(options: VncConnectOptions, handler: Arc<dyn VncHandler>) {
        drop(assert_send(async move {
            connect(options, VncTransport::Direct, handler).await
        }));
    }
}

/// The sign-in methods with their own cryptography, against server halves written here.
#[cfg(test)]
mod auth_tests {
    use super::*;
    use crate::crypto::Ra2Cipher;
    use aes::Aes128;
    use des::cipher::{Block, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
    use md5::{Digest, Md5};
    use num_bigint::BigUint;
    use rsa::traits::PublicKeyParts;
    use rsa::{BoxedUint, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
    use tokio::io::DuplexStream;

    #[derive(Default)]
    struct TestHandler {
        keys: Mutex<Vec<VncServerKey>>,
        trust: bool,
    }

    impl VncHandler for TestHandler {
        fn credentials(
            &self,
            types: Vec<DesktopCredentialType>,
        ) -> BoxFuture<'static, Option<DesktopCredentials>> {
            let username = types
                .contains(&DesktopCredentialType::Username)
                .then(|| "alice".to_owned());
            Box::pin(async move {
                Some(DesktopCredentials {
                    username,
                    password: Some("hunter2".into()),
                    domain: None,
                })
            })
        }

        fn verify_server_key(&self, key: VncServerKey) -> BoxFuture<'static, bool> {
            self.keys.lock().unwrap().push(key);
            let trust = self.trust;
            Box::pin(async move { trust })
        }
    }

    async fn read(stream: &mut DuplexStream, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        stream.read_exact(&mut buf).await.unwrap();
        buf
    }

    async fn greet(stream: &mut DuplexStream, kind: u8) {
        stream.write_all(b"RFB 003.008\n").await.unwrap();
        read(stream, 12).await;
        stream.write_all(&[1, kind]).await.unwrap();
        assert_eq!(read(stream, 1).await, [kind]);
    }

    /// SecurityResult OK and a ServerInit, then whatever the client sends is drained.
    async fn finish(mut stream: DuplexStream) {
        stream.write_all(&0u32.to_be_bytes()).await.unwrap();
        read(&mut stream, 1).await;
        let mut init = vec![
            0, 8, 0, 8, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0,
        ];
        init.extend(0u32.to_be_bytes());
        stream.write_all(&init).await.unwrap();
        let mut sink = [0u8; 256];
        while stream.read(&mut sink).await.unwrap_or(0) > 0 {}
    }

    fn c_string(data: &[u8]) -> String {
        let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
        String::from_utf8_lossy(&data[..end]).into_owned()
    }

    fn pad(value: &[u8], len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len - value.len()];
        out.extend(value);
        out
    }

    async fn sign_in(
        server: impl Future<Output = (String, String)> + Send + 'static,
        client_end: DuplexStream,
        handler: Arc<TestHandler>,
    ) -> Result<(String, String), VncFailure> {
        let server = tokio::spawn(server);
        let options = VncConnectOptions::new("vnc.test", 5900);
        let session = connect(options, VncTransport::Tunnel(Box::new(client_end)), handler).await?;
        session.disconnect();
        Ok(server.await.unwrap())
    }

    #[tokio::test]
    async fn signs_in_to_apple_remote_desktop() {
        let (client_end, mut server_end) = tokio::io::duplex(1 << 16);
        let server = async move {
            greet(&mut server_end, 30).await;
            // Any modulus works for the exchange itself; a prime is not needed to test it.
            let prime: Vec<u8> = (0..128).map(|i| (i * 37 + 11) as u8 | 1).collect();
            let generator = [0u8, 2];
            let private = BigUint::from_bytes_be(&[0x5a; 128]);
            let modulus = BigUint::from_bytes_be(&prime);
            let public = BigUint::from_bytes_be(&generator).modpow(&private, &modulus);
            let mut message = generator.to_vec();
            message.extend(128u16.to_be_bytes());
            message.extend(&prime);
            message.extend(pad(&public.to_bytes_be(), 128));
            server_end.write_all(&message).await.unwrap();
            let encrypted = read(&mut server_end, 128).await;
            let client_public = BigUint::from_bytes_be(&read(&mut server_end, 128).await);
            let shared = pad(&client_public.modpow(&private, &modulus).to_bytes_be(), 128);
            let key = Md5::digest(&shared);
            let cipher = Aes128::new_from_slice(&key).unwrap();
            let mut plain = encrypted.clone();
            for chunk in plain.chunks_mut(16) {
                let mut block = Block::<Aes128>::default();
                block.copy_from_slice(chunk);
                cipher.decrypt_block(&mut block);
                chunk.copy_from_slice(&block);
            }
            let result = (c_string(&plain[..64]), c_string(&plain[64..]));
            tokio::spawn(finish(server_end));
            result
        };
        let handler = Arc::new(TestHandler::default());
        assert_eq!(
            sign_in(server, client_end, handler).await.unwrap(),
            ("alice".into(), "hunter2".into())
        );
    }

    #[tokio::test]
    async fn signs_in_with_ultravnc_mslogon_ii() {
        let (client_end, mut server_end) = tokio::io::duplex(1 << 16);
        let server = async move {
            greet(&mut server_end, 113).await;
            let generator = 5u64.to_be_bytes();
            let prime = 0xffff_ffff_ffff_ffc5u64.to_be_bytes();
            let private = BigUint::from(0x1234_5678_9abc_def1u64);
            let modulus = BigUint::from_bytes_be(&prime);
            let public = BigUint::from_bytes_be(&generator).modpow(&private, &modulus);
            let mut message = generator.to_vec();
            message.extend(prime);
            message.extend(pad(&public.to_bytes_be(), 8));
            server_end.write_all(&message).await.unwrap();
            let client_public = BigUint::from_bytes_be(&read(&mut server_end, 8).await);
            let mut user = read(&mut server_end, 256).await;
            let mut pass = read(&mut server_end, 64).await;
            let secret = pad(&client_public.modpow(&private, &modulus).to_bytes_be(), 8);
            let key: Vec<u8> = secret.iter().map(|b| b.reverse_bits()).collect();
            let cipher = des::Des::new_from_slice(&key).unwrap();
            for data in [&mut user, &mut pass] {
                let mut chain = secret.clone();
                for chunk in data.chunks_mut(8) {
                    let encrypted = chunk.to_vec();
                    let mut block = Block::<des::Des>::default();
                    block.copy_from_slice(chunk);
                    cipher.decrypt_block(&mut block);
                    for (out, (plain, previous)) in chunk.iter_mut().zip(block.iter().zip(&chain)) {
                        *out = plain ^ previous;
                    }
                    chain = encrypted;
                }
            }
            let result = (c_string(&user), c_string(&pass));
            tokio::spawn(finish(server_end));
            result
        };
        let handler = Arc::new(TestHandler::default());
        assert_eq!(
            sign_in(server, client_end, handler).await.unwrap(),
            ("alice".into(), "hunter2".into())
        );
    }

    async fn rsa_aes_server(
        mut server_end: DuplexStream,
        server_key: RsaPrivateKey,
    ) -> (String, String) {
        greet(&mut server_end, 6).await;
        let bytes = 128usize;
        let mut server_public = 1024u32.to_be_bytes().to_vec();
        server_public.extend(pad(&server_key.n_bytes(), bytes));
        server_public.extend(pad(&server_key.e_bytes(), bytes));
        server_end.write_all(&server_public).await.unwrap();
        // The client's key, then its random under our key.
        let client_public = read(&mut server_end, 4 + 512).await;
        assert_eq!(&client_public[..4], &2048u32.to_be_bytes());
        let client_key = RsaPublicKey::new(
            BoxedUint::from_be_slice_vartime(&client_public[4..260]),
            BoxedUint::from_be_slice_vartime(&client_public[260..]),
        )
        .unwrap();
        assert_eq!(read(&mut server_end, 2).await, (bytes as u16).to_be_bytes());
        let encrypted = read(&mut server_end, bytes).await;
        let client_random = server_key.decrypt(Pkcs1v15Encrypt, &encrypted).unwrap();
        let server_random = [9u8; 16];
        let encrypted = client_key
            .encrypt(&mut rand::rng(), Pkcs1v15Encrypt, &server_random)
            .unwrap();
        let mut message = 256u16.to_be_bytes().to_vec();
        message.extend(encrypted);
        server_end.write_all(&message).await.unwrap();
        // Session keys mirror the client's.
        let to_server = crypto::sha1(&[&server_random[..], &client_random].concat());
        let to_client = crypto::sha1(&[&client_random[..], &server_random].concat());
        let mut receive = Ra2Cipher::new(&to_server[..16]);
        let mut send = Ra2Cipher::new(&to_client[..16]);
        let length = read(&mut server_end, 2).await;
        assert_eq!(length, [0, 20]);
        let hash = receive
            .receive_message(20, &read(&mut server_end, 36).await)
            .unwrap();
        assert_eq!(
            hash,
            crypto::sha1(&[&client_public[..], &server_public].concat())
        );
        let server_hash = crypto::sha1(&[&server_public[..], &client_public].concat());
        server_end
            .write_all(&send.make_message(&server_hash))
            .await
            .unwrap();
        // Subtype 1: user name and password.
        server_end
            .write_all(&send.make_message(&[1]))
            .await
            .unwrap();
        let length = u16::from_be_bytes(read(&mut server_end, 2).await.try_into().unwrap());
        let plain = receive
            .receive_message(
                length,
                &read(&mut server_end, usize::from(length) + 16).await,
            )
            .unwrap();
        let user_len = usize::from(plain[0]);
        let user = String::from_utf8(plain[1..1 + user_len].to_vec()).unwrap();
        let pass = String::from_utf8(plain[2 + user_len..].to_vec()).unwrap();
        tokio::spawn(finish(server_end));
        (user, pass)
    }

    #[tokio::test]
    async fn signs_in_with_rsa_aes_after_the_key_is_trusted() {
        let server_key =
            tokio::task::spawn_blocking(|| RsaPrivateKey::new(&mut rand::rng(), 1024).unwrap())
                .await
                .unwrap();
        let (client_end, server_end) = tokio::io::duplex(1 << 16);
        let mut sent = 1024u32.to_be_bytes().to_vec();
        sent.extend(pad(&server_key.n_bytes(), 128));
        sent.extend(pad(&server_key.e_bytes(), 128));
        let handler = Arc::new(TestHandler {
            trust: true,
            ..TestHandler::default()
        });
        let result = sign_in(
            rsa_aes_server(server_end, server_key),
            client_end,
            handler.clone(),
        )
        .await
        .unwrap();
        assert_eq!(result, ("alice".into(), "hunter2".into()));
        // The key was shown as sent, before any credentials.
        assert_eq!(
            handler.keys.lock().unwrap().as_slice(),
            [vnc_server_key(&sent)]
        );
    }

    #[tokio::test]
    async fn stops_before_sending_anything_when_the_key_is_not_trusted() {
        let (client_end, mut server_end) = tokio::io::duplex(1 << 16);
        let server = tokio::spawn(async move {
            greet(&mut server_end, 6).await;
            let mut key = 2048u32.to_be_bytes().to_vec();
            key.extend([0xab; 512]);
            server_end.write_all(&key).await.unwrap();
            // The client hangs up without a word.
            let mut rest = Vec::new();
            server_end.read_to_end(&mut rest).await.unwrap();
            rest
        });
        let handler = Arc::new(TestHandler::default());
        let failure = connect(
            VncConnectOptions::new("vnc.test", 5900),
            VncTransport::Tunnel(Box::new(client_end)),
            handler.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(failure.message, "The VNC server key was not trusted.");
        assert_eq!(failure.kind, VncFailureKind::Refused);
        assert!(server.await.unwrap().is_empty());
        assert_eq!(handler.keys.lock().unwrap()[0].bits, 2048);
    }

    #[test]
    fn des_encrypt_is_used_symmetrically() {
        // Keep BlockCipherEncrypt in scope for the helpers above.
        let cipher = des::Des::new_from_slice(&[1; 8]).unwrap();
        let mut block = Block::<des::Des>::default();
        cipher.encrypt_block(&mut block);
        cipher.decrypt_block(&mut block);
        assert_eq!(block.as_slice(), &[0; 8]);
    }
}
