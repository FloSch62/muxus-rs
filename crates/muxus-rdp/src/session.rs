//! One RDP connection: dial, negotiate, verify the server, log on, then run the session.
//!
//! Port of `RdpConnection` in `client/src/remote-desktop/rdp-client.ts` together with the
//! connection half the Node proxy did for the WebAssembly client (`rdp-proxy.ts`): X.224
//! negotiation, the TLS upgrade and the certificate check now happen here, directly against
//! the server or through a tunnel the caller opened (an SSH gateway).

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ironrdp::cliprdr::CliprdrClient;
use ironrdp::cliprdr::pdu::{ClipboardFormat, ClipboardFormatId, FormatDataResponse};
use ironrdp::connector::connection_activation::ConnectionActivationState;
use ironrdp::connector::sspi::generator::NetworkRequest;
use ironrdp::connector::{
    self, BitmapConfig, ClientConnector, ConnectionResult, ConnectorResult, Credentials,
    DesktopSize, ServerName,
};
use ironrdp::core::{IntoOwned as _, WriteBuf};
use ironrdp::displaycontrol::client::DisplayControlClient;
use ironrdp::dvc::DrdynvcClient;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::input::{
    Database, MouseButton as IronButton, MousePosition, Operation, Scancode, WheelRotations,
};
use ironrdp::pdu::geometry::InclusiveRectangle;
use ironrdp::pdu::input::fast_path::FastPathInputEvent;
use ironrdp::pdu::rdp::capability_sets::{
    BitmapCodecs, MajorPlatformType, RailSupportLevel, client_codecs_capabilities,
};
use ironrdp::pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{
    ActiveStage, ActiveStageBuilder, ActiveStageOutput, GracefulDisconnectReason,
};
use ironrdp_tokio::{TokioFramed, single_sequence_step_read, split_tokio_framed};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;

use crate::certificates::{
    DeferredVerifier, PresentedCertificate, crypto_provider, presented_certificate,
    subject_public_key, trusted_certificate_authorities,
};
use crate::clipboard::{ClipboardSignal, TextClipboardBackend, offers_text, response_text};
use crate::errors::{RdpError, RdpFailure, SocketFailure, connector_error, error_chain};
use crate::framebuffer::{Framebuffer, PixelOrder, Rect};
use crate::input::{
    InputOp, KeyEvent, MouseButton, RdpKeyboard, WheelDelta, WheelUnit, ctrl_alt_del_ops,
    wheel_ops, windows_key_ops,
};
use crate::params::{rdp_desktop_size, rdp_logon};

/// How long dialling may take (`TCP_CONNECT_TIMEOUT_MS` in `server/src/util/dial.ts`).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long X.224 negotiation and the TLS handshake may each take (`HANDSHAKE_TIMEOUT_MS` in
/// `rdp-proxy.ts`).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a requested shutdown waits for the server to end the session.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// A boxed future, for the handler's asynchronous answers.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A byte stream to the RDP server opened by someone else, such as an SSH `direct-tcpip`
/// channel through a gateway.
pub trait DesktopStream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> DesktopStream for T {}

/// How to reach the server.
pub enum RdpTransport {
    /// Dial `host:port` over TCP.
    Direct,
    /// Use this stream; through an SSH gateway only the gateway resolves the address.
    Tunnel(Box<dyn DesktopStream>),
}

impl std::fmt::Debug for RdpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct => f.write_str("Direct"),
            Self::Tunnel(_) => f.write_str("Tunnel"),
        }
    }
}

/// The decisions a connection needs from its owner.
pub trait RdpHandler: Send + Sync + 'static {
    /// The server's TLS certificate, before any credentials go out. Resolve `true` to
    /// continue. The owner checks [`crate::certificate_challenge`] against the pinned
    /// fingerprint and asks the user when it returns a challenge.
    fn accept_certificate(&self, certificate: PresentedCertificate) -> BoxFuture<'static, bool>;

    /// A transient progress line, such as `Connecting to host:3389 …`.
    fn status(&self, message: &str) {
        let _ = message;
    }
}

/// Everything one connection attempt needs. Credentials are the whole logon: NLA runs CredSSP
/// in the client, and TLS-only servers take them in the Client Info PDU.
#[derive(Debug, Clone)]
pub struct RdpConnectOptions {
    /// Host as the user configured it: dialled, sent as SNI (names only) and used for CredSSP.
    pub host: String,
    pub port: u16,
    /// `DOMAIN\user` and `user@domain` work as typed.
    pub username: String,
    pub password: String,
    pub domain: Option<String>,
    /// The pane size in pixels; the server is asked for [`rdp_desktop_size`] of it.
    pub width: f64,
    pub height: f64,
    /// Text clipboard redirection; off leaves the CLIPRDR channel out entirely.
    pub share_clipboard: bool,
    /// The client name the server sees (`%CLIENTNAME%`); 15 characters at most.
    pub client_name: String,
    /// Byte order of the shared framebuffer.
    pub pixel_order: PixelOrder,
}

impl RdpConnectOptions {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            username: String::new(),
            password: String::new(),
            domain: None,
            width: f64::from(crate::params::FALLBACK_DESKTOP_SIZE.0),
            height: f64::from(crate::params::FALLBACK_DESKTOP_SIZE.1),
            share_clipboard: true,
            // What the WebAssembly client reported; servers show it as the client name.
            client_name: "ironrdp-web".to_owned(),
            pixel_order: PixelOrder::Rgba,
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

/// The mouse pointer the server wants drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorShape {
    /// The system's default arrow.
    Default,
    /// No pointer.
    Hidden,
    /// A pointer image: `width`×`height` RGBA, straight (not premultiplied) alpha.
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
    /// The server ended the session (logoff, disconnect from the remote side, admin action).
    Completed { reason: String },
    /// The connection broke.
    Failed(RdpFailure),
    /// [`RdpSession::shutdown`] closed it.
    Closed,
}

impl SessionEnd {
    /// The sentence the view shows on its "ended" screen.
    pub fn message(&self) -> String {
        match self {
            Self::Completed { reason } if reason.is_empty() => "The remote session ended.".into(),
            Self::Completed { reason } => format!("The remote session ended: {reason}"),
            Self::Failed(failure) => failure.message.clone(),
            Self::Closed => "The connection closed.".into(),
        }
    }
}

/// What a running session reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RdpEvent {
    /// The desktop has this size now (first event, and after the server resized it). The
    /// framebuffer kept what overlaps and is entirely dirty; a [`RdpEvent::Frame`] follows
    /// unless one is already pending.
    Resized {
        width: u16,
        height: u16,
    },
    /// The framebuffer changed; [`Framebuffer::take_dirty`] says where. Sent when damage
    /// appears while none is pending, so take the dirty regions on every `Frame`.
    Frame,
    Cursor(CursorShape),
    /// The server moved the pointer.
    PointerPosition {
        x: u16,
        y: u16,
    },
    /// The remote clipboard holds this text (only with clipboard sharing on).
    RemoteClipboard(String),
    /// The last event.
    Ended(SessionEnd),
}

#[derive(Debug)]
enum Command {
    Input(Vec<InputOp>),
    Resize { width: u16, height: u16 },
    Clipboard { text: String, force: bool },
    Shutdown,
}

/// A live RDP session. Input methods queue and return at once; [`RdpSession::events`] reports
/// what the server does. Clones share the session.
#[derive(Clone)]
pub struct RdpSession {
    commands: mpsc::UnboundedSender<Command>,
    keyboard: Arc<Mutex<RdpKeyboard>>,
    events: async_channel::Receiver<RdpEvent>,
    framebuffer: Framebuffer,
    /// Width in the high half, height in the low half.
    desktop_size: Arc<AtomicU32>,
    closed: Arc<AtomicBool>,
    share_clipboard: bool,
}

impl std::fmt::Debug for RdpSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RdpSession")
            .field("desktop_size", &self.desktop_size())
            .finish_non_exhaustive()
    }
}

fn pack_size(width: u16, height: u16) -> u32 {
    (u32::from(width) << 16) | u32::from(height)
}

impl RdpSession {
    /// The session's events. Take them from one place: clones of the receiver share one queue.
    pub fn events(&self) -> async_channel::Receiver<RdpEvent> {
        self.events.clone()
    }

    /// The shared screen.
    pub fn framebuffer(&self) -> Framebuffer {
        self.framebuffer.clone()
    }

    pub fn desktop_size(&self) -> (u16, u16) {
        let packed = self.desktop_size.load(Ordering::Relaxed);
        ((packed >> 16) as u16, packed as u16)
    }

    /// Whether the session has ended.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed) || self.commands.is_closed()
    }

    fn send(&self, command: Command) {
        if self.is_closed() {
            return;
        }
        // The session is going away; its events report why.
        let _ = self.commands.send(command);
    }

    fn apply(&self, ops: Vec<InputOp>) {
        if !ops.is_empty() {
            self.send(Command::Input(ops));
        }
    }

    /// Send a key. Returns `false` for keys RDP has no scancode for, so the caller may let them
    /// be (and not swallow the event).
    pub fn key(&self, event: &KeyEvent) -> bool {
        let ops = self
            .keyboard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .key(event);
        match ops {
            Some(ops) => {
                self.apply(ops);
                true
            }
            None => false,
        }
    }

    /// The pointer moved to (x, y) in desktop pixels (see [`crate::desktop_point`]).
    pub fn pointer_move(&self, x: u16, y: u16) {
        self.apply(vec![InputOp::MouseMove { x, y }]);
    }

    /// A button went down or up at (x, y).
    pub fn pointer_button(&self, x: u16, y: u16, button: MouseButton, pressed: bool) {
        self.apply(vec![
            InputOp::MouseMove { x, y },
            InputOp::MouseButton { button, pressed },
        ]);
    }

    pub fn wheel(&self, delta: WheelDelta) {
        self.apply(wheel_ops(delta));
    }

    /// Let go of everything held down, e.g. when focus leaves mid-chord.
    pub fn release_all(&self) {
        let ops = self
            .keyboard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .release_all();
        self.apply(ops);
    }

    pub fn send_ctrl_alt_del(&self) {
        self.apply(ctrl_alt_del_ops());
    }

    pub fn send_windows_key(&self) {
        self.apply(windows_key_ops());
    }

    /// Ask the server for a desktop matching the pane (display control channel). Servers
    /// without display control keep their size; the view scales instead.
    pub fn resize(&self, width: f64, height: f64) {
        let (width, height) = rdp_desktop_size(width, height);
        if (width, height) == self.desktop_size() {
            return;
        }
        self.send(Command::Resize { width, height });
    }

    /// Offer local clipboard text to the remote side (announced, fetched on paste). Repeats of
    /// the text last seen on either side are skipped unless `force` is set.
    pub fn send_clipboard_text(&self, text: &str, force: bool) {
        if !self.share_clipboard {
            return;
        }
        self.send(Command::Clipboard {
            text: text.to_owned(),
            force,
        });
    }

    /// End the session; [`RdpEvent::Ended`] follows.
    pub fn shutdown(&self) {
        self.send(Command::Shutdown);
    }
}

trait AsyncReadWrite: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite> AsyncReadWrite for T {}
type RdpStream = Box<dyn AsyncReadWrite + Send + Sync + Unpin>;

/// Connect, verify the server and log on. Returns once the desktop is up; the session then
/// runs on its own task (call from within a tokio runtime).
pub async fn connect(
    options: RdpConnectOptions,
    transport: RdpTransport,
    handler: Arc<dyn RdpHandler>,
) -> Result<RdpSession, RdpFailure> {
    let (stream, client_addr) = open_stream(&options, transport, handler.as_ref())
        .await
        .map_err(|err| err.describe())?;
    let (clipboard_tx, clipboard_rx) = mpsc::unbounded_channel();
    let (result, framed) = establish(&options, stream, client_addr, clipboard_tx, handler)
        .await
        .map_err(|err| err.describe())?;

    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = async_channel::unbounded();
    let framebuffer = Framebuffer::new(options.pixel_order);
    let size = result.desktop_size;
    let desktop_size = Arc::new(AtomicU32::new(pack_size(size.width, size.height)));
    let closed = Arc::new(AtomicBool::new(false));
    let session = RdpSession {
        commands: commands_tx,
        keyboard: Arc::new(Mutex::new(RdpKeyboard::new())),
        events: events_rx,
        framebuffer: framebuffer.clone(),
        desktop_size: desktop_size.clone(),
        closed: closed.clone(),
        share_clipboard: options.share_clipboard,
    };
    let run = SessionLoop {
        events: events_tx,
        framebuffer,
        desktop_size,
        pixel_format: match options.pixel_order {
            PixelOrder::Rgba => PixelFormat::RgbA32,
            PixelOrder::Bgra => PixelFormat::BgrA32,
        },
        clipboard_text: None,
        pending_paste: false,
    };
    tokio::spawn(async move {
        let end = run.run(result, framed, commands_rx, clipboard_rx).await;
        closed.store(true, Ordering::Relaxed);
        end
    });
    Ok(session)
}

fn bare_host(host: &str) -> &str {
    host.trim_start_matches('[').trim_end_matches(']')
}

async fn dial(host: &str, port: u16) -> Result<TcpStream, RdpError> {
    let addresses = tokio::net::lookup_host((bare_host(host), port))
        .await
        .map_err(|err| {
            RdpError::Socket(
                SocketFailure::from_io(&err)
                    .filter(|failure| {
                        matches!(
                            failure,
                            SocketFailure::TryAgain | SocketFailure::HostNotFound
                        )
                    })
                    .unwrap_or(SocketFailure::HostNotFound),
            )
        })?;
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect(address).await {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(err) => last_error = Some(err),
        }
    }
    Err(match last_error {
        Some(err) => SocketFailure::from_io(&err).map_or(RdpError::ConnectFailed, RdpError::Socket),
        None => RdpError::Socket(SocketFailure::HostNotFound),
    })
}

async fn open_stream(
    options: &RdpConnectOptions,
    transport: RdpTransport,
    handler: &dyn RdpHandler,
) -> Result<(RdpStream, SocketAddr), RdpError> {
    // Browsers hid the local address; any address will do for the Client Info PDU.
    let placeholder = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 33899));
    match transport {
        RdpTransport::Direct => {
            handler.status(&format!("Connecting to {} …", options.destination()));
            let stream = tokio::time::timeout(CONNECT_TIMEOUT, dial(&options.host, options.port))
                .await
                .map_err(|_| RdpError::Socket(SocketFailure::TimedOut))??;
            let client_addr = stream.local_addr().unwrap_or(placeholder);
            Ok((Box::new(stream), client_addr))
        }
        RdpTransport::Tunnel(mut tunnel) => {
            // The connection's framing needs a `Sync` stream; relay the tunnel through an
            // in-memory pipe so any stream will do.
            let (near, mut far) = tokio::io::duplex(256 * 1024);
            tokio::spawn(async move {
                let _ = tokio::io::copy_bidirectional(&mut tunnel, &mut far).await;
            });
            Ok((Box::new(near), placeholder))
        }
    }
}

fn tls_alert(err: &std::io::Error) -> Option<u8> {
    let rustls_error = err.get_ref()?.downcast_ref::<rustls::Error>()?;
    match rustls_error {
        rustls::Error::AlertReceived(alert) => Some(u8::from(*alert)),
        _ => None,
    }
}

struct NoNetworkClient;

impl ironrdp_tokio::NetworkClient for NoNetworkClient {
    async fn send(&mut self, _request: &NetworkRequest) -> ConnectorResult<Vec<u8>> {
        // Without a KDC (proxy) configured, CredSSP negotiates NTLM and never gets here.
        Err(connector::general_err!("Kerberos is not available"))
    }
}

fn connector_config(options: &RdpConnectOptions) -> connector::Config {
    let logon = rdp_logon(&options.username, options.domain.as_deref());
    let (width, height) = rdp_desktop_size(options.width, options.height);
    connector::Config {
        credentials: Credentials::UsernamePassword {
            username: logon.username,
            password: options.password.clone(),
        },
        domain: (!logon.domain.is_empty()).then_some(logon.domain),
        enable_tls: true,
        enable_credssp: true,
        enable_standard_rdp_security: false,
        keyboard_type: ironrdp::pdu::gcc::KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        // The server should use its default active input locale.
        keyboard_layout: 0,
        keyboard_functional_keys_count: 12,
        connection_type: ironrdp::pdu::gcc::ConnectionType::Lan,
        ime_file_name: String::new(),
        dig_product_id: String::new(),
        desktop_size: DesktopSize { width, height },
        monitor_layout: None,
        bitmap: Some(BitmapConfig {
            color_depth: 16,
            lossy_compression: true,
            codecs: client_codecs_capabilities(&[]).unwrap_or_else(|_| BitmapCodecs(Vec::new())),
        }),
        client_build: 0,
        client_name: options.client_name.clone(),
        // Hardcoded like FreeRDP does.
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_owned(),
        platform: MajorPlatformType::UNSPECIFIED,
        compression_type: None,
        enable_server_pointer: true,
        // Log on with the supplied credentials instead of stopping at the server's own logon
        // screen (TLS-only servers such as xrdp).
        autologon: true,
        enable_audio_playback: false,
        enable_audio_capture: false,
        request_data: None,
        pointer_software_rendering: false,
        multitransport_flags: None,
        support_dyn_vc_gfx_protocol: false,
        performance_flags: PerformanceFlags::default(),
        desktop_scale_factor: 0,
        hardware_id: None,
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: RailSupportLevel::SUPPORTED,
    }
}

async fn establish(
    options: &RdpConnectOptions,
    stream: RdpStream,
    client_addr: SocketAddr,
    clipboard: mpsc::UnboundedSender<ClipboardSignal>,
    handler: Arc<dyn RdpHandler>,
) -> Result<(ConnectionResult, TokioFramed<RdpStream>), RdpError> {
    let mut connector = ClientConnector::new(connector_config(options), client_addr);
    if options.share_clipboard {
        connector.attach_static_channel(CliprdrClient::new(Box::new(TextClipboardBackend::new(
            clipboard,
        ))));
    }
    connector.attach_static_channel(
        DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new()))),
    );

    let mut framed = TokioFramed::new(stream);
    let should_upgrade = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        connect_begin(&mut framed, &mut connector),
    )
    .await
    .map_err(|_| RdpError::Socket(SocketFailure::TimedOut))?
    .map_err(|err| connector_error(&err))?;

    let (stream, leftover) = framed.into_inner();
    let config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .map_err(|err| RdpError::General(err.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(DeferredVerifier::new()))
        .with_no_client_auth();
    let host = bare_host(&options.host);
    let sni = match host.parse::<std::net::IpAddr>() {
        Ok(address) => rustls::pki_types::ServerName::IpAddress(address.into()),
        Err(_) => rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|err| RdpError::General(err.to_string()))?,
    };
    let tls = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        TlsConnector::from(Arc::new(config)).connect(sni, stream),
    )
    .await
    .map_err(|_| RdpError::ConnectFailed)?
    .map_err(|err| tls_alert(&err).map_or(RdpError::ConnectFailed, RdpError::TlsAlert))?;

    let chain = tls
        .get_ref()
        .1
        .peer_certificates()
        .map(<[_]>::to_vec)
        .unwrap_or_default();
    let certificate =
        presented_certificate(&chain, &options.host, trusted_certificate_authorities())
            .map_err(RdpError::General)?;
    let leaf = certificate.chain[0].clone();
    if !handler.accept_certificate(certificate).await {
        return Err(RdpError::CertificateRejected);
    }
    let server_public_key = subject_public_key(&leaf).map_err(RdpError::General)?;

    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let stream: RdpStream = Box::new(tls);
    let mut framed = TokioFramed::new_with_leftover(stream, leftover);
    let result = connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoNetworkClient,
        ServerName::new(host),
        server_public_key,
    )
    .await
    .map_err(|err| connector_error(&err))?;
    Ok((result, framed))
}

fn inclusive_to_rect(region: &InclusiveRectangle) -> Rect {
    Rect::new(
        u32::from(region.left),
        u32::from(region.top),
        u32::from(region.right.saturating_sub(region.left)) + 1,
        u32::from(region.bottom.saturating_sub(region.top)) + 1,
    )
}

fn iron_button(button: MouseButton) -> IronButton {
    match button {
        MouseButton::Left => IronButton::Left,
        MouseButton::Middle => IronButton::Middle,
        MouseButton::Right => IronButton::Right,
        MouseButton::Back => IronButton::X1,
        MouseButton::Forward => IronButton::X2,
    }
}

/// Wheel units in the pixels IronRDP's input database counts (the WebAssembly client's
/// conversion: 50 pixels a line, 38 lines a page).
fn wheel_pixels(amount: i16, unit: WheelUnit) -> i16 {
    const LINES_TO_PIXELS: i32 = 50;
    const PAGES_TO_LINES: i32 = 38;
    let amount = i32::from(amount);
    let pixels = match unit {
        WheelUnit::Pixel => amount,
        WheelUnit::Line => amount * LINES_TO_PIXELS,
        WheelUnit::Page => amount * PAGES_TO_LINES * LINES_TO_PIXELS,
    };
    pixels.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// The fast-path events for a batch of device events, tracked through the input database.
fn fastpath_events(database: &mut Database, ops: &[InputOp]) -> Vec<FastPathInputEvent> {
    let mut events = Vec::new();
    for op in ops {
        let operation = match *op {
            InputOp::KeyPressed(code) => Operation::KeyPressed(Scancode::from_u16(code)),
            InputOp::KeyReleased(code) => Operation::KeyReleased(Scancode::from_u16(code)),
            InputOp::MouseMove { x, y } => Operation::MouseMove(MousePosition { x, y }),
            InputOp::MouseButton { button, pressed } => {
                if pressed {
                    Operation::MouseButtonPressed(iron_button(button))
                } else {
                    Operation::MouseButtonReleased(iron_button(button))
                }
            }
            InputOp::Wheel {
                vertical,
                amount,
                unit,
            } => Operation::WheelRotations(WheelRotations {
                is_vertical: vertical,
                rotation_units: wheel_pixels(amount, unit),
            }),
            InputOp::SynchronizeLocks {
                scroll_lock,
                num_lock,
                caps_lock,
            } => {
                events.push(ironrdp::input::synchronize_event(
                    scroll_lock,
                    num_lock,
                    caps_lock,
                    false,
                ));
                continue;
            }
            InputOp::ReleaseAll => {
                events.extend(database.release_all());
                continue;
            }
        };
        events.extend(database.apply([operation]));
    }
    events
}

struct SessionLoop {
    events: async_channel::Sender<RdpEvent>,
    framebuffer: Framebuffer,
    desktop_size: Arc<AtomicU32>,
    pixel_format: PixelFormat,
    /// The clipboard text last seen on either side.
    clipboard_text: Option<String>,
    /// A paste of the remote clipboard is in flight.
    pending_paste: bool,
}

type Reader = TokioFramed<tokio::io::ReadHalf<RdpStream>>;
type Writer = TokioFramed<tokio::io::WriteHalf<RdpStream>>;

enum Flow {
    Continue,
    End(SessionEnd),
}

impl SessionLoop {
    fn emit(&self, event: RdpEvent) -> bool {
        self.events.try_send(event).is_ok()
    }

    fn resize_framebuffer(&self, width: u16, height: u16) {
        let first = self
            .framebuffer
            .lock()
            .resize(u32::from(width), u32::from(height));
        self.desktop_size
            .store(pack_size(width, height), Ordering::Relaxed);
        self.emit(RdpEvent::Resized { width, height });
        if first {
            self.emit(RdpEvent::Frame);
        }
    }

    fn draw(&self, image: &DecodedImage, region: &InclusiveRectangle) {
        let rect = inclusive_to_rect(region);
        let first = {
            let mut state = self.framebuffer.lock();
            state.copy_from_image(rect, image.data(), image.stride());
            state.mark_dirty(rect)
        };
        if first {
            self.emit(RdpEvent::Frame);
        }
    }

    async fn run(
        mut self,
        result: ConnectionResult,
        framed: TokioFramed<RdpStream>,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut clipboard: mpsc::UnboundedReceiver<ClipboardSignal>,
    ) -> SessionEnd {
        let end = self
            .run_inner(result, framed, &mut commands, &mut clipboard)
            .await;
        self.emit(RdpEvent::Ended(end.clone()));
        end
    }

    async fn run_inner(
        &mut self,
        result: ConnectionResult,
        framed: TokioFramed<RdpStream>,
        commands: &mut mpsc::UnboundedReceiver<Command>,
        clipboard: &mut mpsc::UnboundedReceiver<ClipboardSignal>,
    ) -> SessionEnd {
        let (mut reader, mut writer) = split_tokio_framed(framed);
        let size = result.desktop_size;
        let mut image = DecodedImage::new(self.pixel_format, size.width, size.height);
        self.resize_framebuffer(size.width, size.height);
        let activation_factory = result.activation_factory;
        let mut stage = ActiveStageBuilder {
            static_channels: result.static_channels,
            user_channel_id: result.user_channel_id,
            io_channel_id: result.io_channel_id,
            message_channel_id: result.message_channel_id,
            share_id: result.share_id,
            compression_type: result.compression_type,
            enable_server_pointer: result.enable_server_pointer,
            pointer_software_rendering: result.pointer_software_rendering,
        }
        .build();
        let mut database = Database::new();
        // Drives CLIPRDR's lock timeouts, as the WebAssembly client does.
        let mut cleanup = tokio::time::interval(Duration::from_secs(5));
        let mut shutdown_deadline: Option<Pin<Box<tokio::time::Sleep>>> = None;
        let mut commands_open = true;

        loop {
            let outputs = tokio::select! {
                frame = read_pdu(&mut reader) => {
                    let (action, payload) = match frame {
                        Ok(frame) => frame,
                        Err(err) => {
                            if shutdown_deadline.is_some() {
                                return SessionEnd::Closed;
                            }
                            return SessionEnd::Failed(
                                RdpError::General(format!("read frame: {}", error_chain(&err))).describe(),
                            );
                        }
                    };
                    match stage.process(&mut image, action, &payload) {
                        Ok(outputs) => outputs,
                        Err(err) => return SessionEnd::Failed(
                            RdpError::General(error_chain(&err)).describe(),
                        ),
                    }
                }
                command = commands.recv(), if commands_open => {
                    let command = command.unwrap_or_else(|| {
                        // Every handle is gone: nobody can see the desktop any more.
                        commands_open = false;
                        Command::Shutdown
                    });
                    match self.command(command, &mut stage, &mut image, &mut database) {
                        Ok(CommandOutcome::Outputs(outputs)) => outputs,
                        Ok(CommandOutcome::Shutdown(outputs)) => {
                            if shutdown_deadline.is_none() {
                                shutdown_deadline = Some(Box::pin(tokio::time::sleep(SHUTDOWN_TIMEOUT)));
                            }
                            outputs
                        }
                        Err(end) => return end,
                    }
                }
                signal = clipboard.recv() => match signal {
                    Some(signal) => match self.clipboard_signal(signal, &mut stage) {
                        Ok(outputs) => outputs,
                        Err(end) => return end,
                    },
                    None => Vec::new(),
                },
                _ = cleanup.tick() => match stage.get_svc_processor_mut::<CliprdrClient>() {
                    Some(cliprdr) => match cliprdr.drive_timeouts() {
                        Ok(messages) => match stage.process_svc_processor_messages(messages) {
                            Ok(frame) if !frame.is_empty() => vec![ActiveStageOutput::ResponseFrame(frame)],
                            Ok(_) => Vec::new(),
                            Err(err) => return SessionEnd::Failed(RdpError::General(error_chain(&err)).describe()),
                        },
                        Err(_) => Vec::new(),
                    },
                    None => Vec::new(),
                },
                _ = async {
                    match shutdown_deadline.as_mut() {
                        Some(deadline) => deadline.await,
                        None => std::future::pending().await,
                    }
                } => return SessionEnd::Closed,
            };

            for output in outputs {
                match self
                    .output(
                        output,
                        &mut reader,
                        &mut writer,
                        &mut stage,
                        &mut image,
                        &activation_factory,
                    )
                    .await
                {
                    Flow::Continue => {}
                    Flow::End(end) => {
                        if shutdown_deadline.is_some() && matches!(end, SessionEnd::Failed(_)) {
                            return SessionEnd::Closed;
                        }
                        return end;
                    }
                }
            }
        }
    }

    fn command(
        &mut self,
        command: Command,
        stage: &mut ActiveStage,
        image: &mut DecodedImage,
        database: &mut Database,
    ) -> Result<CommandOutcome, SessionEnd> {
        let failed = |err: &dyn std::error::Error| {
            SessionEnd::Failed(RdpError::General(error_chain(err)).describe())
        };
        match command {
            Command::Input(ops) => {
                let events = fastpath_events(database, &ops);
                stage
                    .process_fastpath_input(image, &events)
                    .map(CommandOutcome::Outputs)
                    .map_err(|err| failed(&err))
            }
            Command::Resize { width, height } => {
                match stage.encode_resize(u32::from(width), u32::from(height), None, None) {
                    Some(Ok(frame)) => Ok(CommandOutcome::Outputs(vec![
                        ActiveStageOutput::ResponseFrame(frame),
                    ])),
                    Some(Err(err)) => Err(failed(&err)),
                    // No display control: the server keeps its size and the view scales.
                    None => {
                        tracing::debug!(width, height, "resize ignored: no display control");
                        Ok(CommandOutcome::Outputs(Vec::new()))
                    }
                }
            }
            Command::Clipboard { text, force } => {
                if !force && self.clipboard_text.as_deref() == Some(text.as_str()) {
                    return Ok(CommandOutcome::Outputs(Vec::new()));
                }
                self.clipboard_text = Some(text);
                self.offer_clipboard(stage).map(CommandOutcome::Outputs)
            }
            Command::Shutdown => stage
                .graceful_shutdown()
                .map(CommandOutcome::Shutdown)
                .map_err(|err| failed(&err)),
        }
    }

    /// Announce that we hold text; the server fetches it when the user pastes.
    fn offer_clipboard(
        &mut self,
        stage: &mut ActiveStage,
    ) -> Result<Vec<ActiveStageOutput>, SessionEnd> {
        let Some(cliprdr) = stage.get_svc_processor_mut::<CliprdrClient>() else {
            return Ok(Vec::new());
        };
        let messages = match cliprdr
            .initiate_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
        {
            Ok(messages) => messages,
            Err(_) => return Ok(Vec::new()),
        };
        self.svc_frame(stage, messages)
    }

    fn svc_frame(
        &self,
        stage: &mut ActiveStage,
        messages: ironrdp::cliprdr::CliprdrSvcMessages<ironrdp::cliprdr::Client>,
    ) -> Result<Vec<ActiveStageOutput>, SessionEnd> {
        match stage.process_svc_processor_messages(messages) {
            Ok(frame) if frame.is_empty() => Ok(Vec::new()),
            Ok(frame) => Ok(vec![ActiveStageOutput::ResponseFrame(frame)]),
            Err(err) => Err(SessionEnd::Failed(
                RdpError::General(error_chain(&err)).describe(),
            )),
        }
    }

    fn clipboard_signal(
        &mut self,
        signal: ClipboardSignal,
        stage: &mut ActiveStage,
    ) -> Result<Vec<ActiveStageOutput>, SessionEnd> {
        match signal {
            ClipboardSignal::RemoteFormats(formats) => {
                if !offers_text(&formats) {
                    return Ok(Vec::new());
                }
                let Some(cliprdr) = stage.get_svc_processor_mut::<CliprdrClient>() else {
                    return Ok(Vec::new());
                };
                let Ok(messages) = cliprdr.initiate_paste(ClipboardFormatId::CF_UNICODETEXT) else {
                    return Ok(Vec::new());
                };
                self.pending_paste = true;
                self.svc_frame(stage, messages)
            }
            ClipboardSignal::DataResponse(response) => {
                if !std::mem::take(&mut self.pending_paste) {
                    return Ok(Vec::new());
                }
                if let Some(text) = response_text(&response) {
                    self.clipboard_text = Some(text.clone());
                    self.emit(RdpEvent::RemoteClipboard(text));
                }
                Ok(Vec::new())
            }
            ClipboardSignal::DataRequest(format) => {
                let response = match (&self.clipboard_text, format) {
                    (Some(text), ClipboardFormatId::CF_UNICODETEXT) => {
                        FormatDataResponse::new_unicode_string(text).into_owned()
                    }
                    _ => FormatDataResponse::new_error().into_owned(),
                };
                let Some(cliprdr) = stage.get_svc_processor_mut::<CliprdrClient>() else {
                    return Ok(Vec::new());
                };
                let Ok(messages) = cliprdr.submit_format_data(response) else {
                    return Ok(Vec::new());
                };
                self.svc_frame(stage, messages)
            }
            ClipboardSignal::ForceUpdate => {
                if self.clipboard_text.is_some() {
                    self.offer_clipboard(stage)
                } else {
                    // MS-RDPECLIP 1.3.2.1: initialization ends with the client's Format List.
                    // Without one the server never moves on, so an empty clipboard says so.
                    let Some(cliprdr) = stage.get_svc_processor_mut::<CliprdrClient>() else {
                        return Ok(Vec::new());
                    };
                    match cliprdr.initiate_copy(&[]) {
                        Ok(messages) => self.svc_frame(stage, messages),
                        Err(_) => Ok(Vec::new()),
                    }
                }
            }
        }
    }

    async fn output(
        &mut self,
        output: ActiveStageOutput,
        reader: &mut Reader,
        writer: &mut Writer,
        stage: &mut ActiveStage,
        image: &mut DecodedImage,
        activation_factory: &connector::connection_activation::ConnectionActivationFactory,
    ) -> Flow {
        match output {
            ActiveStageOutput::ResponseFrame(frame) => {
                if frame.is_empty() {
                    return Flow::Continue;
                }
                if let Err(err) = write_frame(writer, &frame).await {
                    return lost(&err);
                }
            }
            ActiveStageOutput::GraphicsUpdate(region) => self.draw(image, &region),
            ActiveStageOutput::PointerDefault => {
                self.emit(RdpEvent::Cursor(CursorShape::Default));
            }
            ActiveStageOutput::PointerHidden => {
                self.emit(RdpEvent::Cursor(CursorShape::Hidden));
            }
            ActiveStageOutput::PointerPosition { x, y } => {
                self.emit(RdpEvent::PointerPosition { x, y });
            }
            ActiveStageOutput::PointerBitmap(pointer) => {
                self.emit(RdpEvent::Cursor(CursorShape::Image {
                    width: pointer.width,
                    height: pointer.height,
                    hotspot_x: pointer.hotspot_x,
                    hotspot_y: pointer.hotspot_y,
                    rgba: Arc::new(pointer.bitmap_data.clone()),
                }));
            }
            ActiveStageOutput::DeactivateAll => {
                // Deactivation-Reactivation Sequence ([MS-RDPBCGR] 1.3.1.3): the server
                // resized the desktop (display control) or reconnected the session.
                let mut activation = activation_factory.create();
                let mut buf = WriteBuf::new();
                loop {
                    let written = match activation_step(reader, &mut activation, &mut buf).await {
                        Ok(written) => written,
                        Err(err) => {
                            return Flow::End(SessionEnd::Failed(
                                RdpError::General(format!("read frame: {}", error_chain(&err)))
                                    .describe(),
                            ));
                        }
                    };
                    if written.size().is_some()
                        && let Err(err) = write_frame(writer, buf.filled()).await
                    {
                        return lost(&err);
                    }
                    if let ConnectionActivationState::Finalized {
                        desktop_size,
                        share_id,
                        enable_server_pointer,
                        pointer_software_rendering,
                        static_channel_chunk_size,
                        window_support_level,
                        ..
                    } = activation.connection_activation_state()
                    {
                        *image = DecodedImage::new(
                            self.pixel_format,
                            desktop_size.width,
                            desktop_size.height,
                        );
                        if !stage.reactivate(
                            activation.io_channel_id(),
                            activation.user_channel_id(),
                            share_id,
                            enable_server_pointer,
                            pointer_software_rendering,
                            static_channel_chunk_size,
                        ) {
                            return Flow::End(SessionEnd::Failed(
                                RdpError::General("invalid static channel chunk size".into())
                                    .describe(),
                            ));
                        }
                        stage.set_window_support_level(window_support_level);
                        self.resize_framebuffer(desktop_size.width, desktop_size.height);
                        break;
                    }
                }
            }
            ActiveStageOutput::Terminate(reason) => {
                let reason = match reason {
                    GracefulDisconnectReason::Other(text) => text,
                    other => other.to_string(),
                };
                return Flow::End(SessionEnd::Completed { reason });
            }
            ActiveStageOutput::AutoReconnectFailed => {
                return Flow::End(SessionEnd::Failed(
                    RdpError::General("automatic reconnect rejected by server".into()).describe(),
                ));
            }
            ActiveStageOutput::MonitorLayout(_)
            | ActiveStageOutput::WindowingOrders(_)
            | ActiveStageOutput::SaveSessionInfo { .. }
            | ActiveStageOutput::MultitransportRequest(_)
            | ActiveStageOutput::AutoDetect(_)
            | ActiveStageOutput::AutoReconnectCookie(_) => {}
        }
        Flow::Continue
    }
}

/// A write to the server failed: the connection is gone.
fn lost(err: &std::io::Error) -> Flow {
    Flow::End(SessionEnd::Failed(
        RdpError::General(format!("write frame: broken pipe: {}", error_chain(err))).describe(),
    ))
}

enum CommandOutcome {
    Outputs(Vec<ActiveStageOutput>),
    /// A graceful shutdown request went out; the server should end the session.
    Shutdown(Vec<ActiveStageOutput>),
}

/// One step of the reactivation sequence, boxed so the future is `Send`: rustc cannot prove it
/// for the `&dyn PduHint` IronRDP holds across an await when the call sits inside another
/// async fn (rust-lang/rust#110338).
fn activation_step<'a>(
    reader: &'a mut Reader,
    activation: &'a mut connector::connection_activation::ConnectionActivationSequence,
    buf: &'a mut WriteBuf,
) -> BoxFuture<'a, ConnectorResult<connector::Written>> {
    Box::pin(single_sequence_step_read(reader, activation, buf))
}

/// X.224 negotiation up to the TLS upgrade, boxed for the same reason as [`activation_step`].
fn connect_begin<'a>(
    framed: &'a mut TokioFramed<RdpStream>,
    connector: &'a mut ClientConnector,
) -> BoxFuture<'a, ConnectorResult<ironrdp_tokio::ShouldUpgrade>> {
    Box::pin(ironrdp_tokio::connect_begin(framed, connector))
}

/// CredSSP and the rest of the connection sequence, boxed like [`connect_begin`].
fn connect_finalize<'a>(
    upgraded: ironrdp_tokio::Upgraded,
    connector: ClientConnector,
    framed: &'a mut TokioFramed<RdpStream>,
    network: &'a mut NoNetworkClient,
    server_name: ServerName,
    server_public_key: Vec<u8>,
) -> BoxFuture<'a, ConnectorResult<ConnectionResult>> {
    Box::pin(ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        framed,
        network,
        server_name,
        server_public_key,
        None,
    ))
}

/// [`Reader::read_pdu`], boxed for the same reason as [`activation_step`].
fn read_pdu(
    reader: &mut Reader,
) -> BoxFuture<'_, std::io::Result<(ironrdp::pdu::Action, ironrdp_tokio::bytes::BytesMut)>> {
    Box::pin(reader.read_pdu())
}

/// Write one frame, boxed for the same reason as [`activation_step`].
fn write_frame<'a>(writer: &'a mut Writer, frame: &'a [u8]) -> BoxFuture<'a, std::io::Result<()>> {
    use ironrdp_tokio::FramedWrite as _;
    Box::pin(writer.write_all(frame))
}

#[cfg(test)]
mod send_probe {
    use super::*;

    fn assert_send<T: Send>(value: T) -> T {
        value
    }

    /// Callers spawn connections on their own runtimes.
    #[allow(dead_code)]
    fn connect_can_be_spawned(options: RdpConnectOptions, handler: Arc<dyn RdpHandler>) {
        drop(assert_send(async move {
            connect(options, RdpTransport::Direct, handler).await
        }));
    }
}
