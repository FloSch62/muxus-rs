//! A whole session against IronRDP's own server, in process: logon (TLS and NLA), the picture,
//! keyboard and mouse input as the server receives it, display-control resizes and shutdown.

mod common;

use std::num::{NonZeroU16, NonZeroUsize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{RecordingHandler, certificate_der, tls_acceptor};
use ironrdp::cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp::core::IntoOwned as _;
use ironrdp_server::{
    BitmapUpdate, CliprdrServerFactory, Credentials, DesktopSize, DisplayUpdate, KeyboardEvent,
    MouseButton as ServerButton, MouseEvent, PixelFormat, RdpServer, RdpServerDisplay,
    RdpServerDisplayUpdates, RdpServerInputHandler, ServerEvent, ServerEventSender, ServerResult,
};
use ironrdp_tokio::bytes::Bytes;
use muxus_rdp::{
    KeyEvent, Modifiers, MouseButton, PixelOrder, RdpConnectOptions, RdpEvent, RdpFailureKind,
    RdpSession, RdpTransport, SessionEnd, connect,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use x509_parser::prelude::{FromDer, X509Certificate};

const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, PartialEq)]
enum Received {
    KeyDown(u8, bool),
    KeyUp(u8, bool),
    Sync(u8),
    Move(u16, u16),
    Button(u16, u16, &'static str, bool),
    Scroll(i16),
}

struct Input(mpsc::UnboundedSender<Received>);

impl RdpServerInputHandler for Input {
    fn keyboard(&mut self, event: KeyboardEvent) {
        let received = match event {
            KeyboardEvent::Pressed { code, extended } => Received::KeyDown(code, extended),
            KeyboardEvent::Released { code, extended } => Received::KeyUp(code, extended),
            KeyboardEvent::Synchronize(flags) => Received::Sync(flags.bits()),
            _ => return,
        };
        let _ = self.0.send(received);
    }

    fn mouse(&mut self, event: MouseEvent) {
        let received = match event {
            MouseEvent::Move { x, y } => Received::Move(x, y),
            MouseEvent::Button {
                x,
                y,
                button,
                pressed,
            } => Received::Button(
                x,
                y,
                match button {
                    ServerButton::Left => "left",
                    ServerButton::Right => "right",
                    ServerButton::Middle => "middle",
                    _ => "other",
                },
                pressed,
            ),
            MouseEvent::VerticalScroll { value } => Received::Scroll(value),
            _ => return,
        };
        let _ = self.0.send(received);
    }
}

struct Display {
    size: Arc<Mutex<DesktopSize>>,
    /// Shared: the server asks for a fresh update stream after every reactivation.
    updates: Arc<tokio::sync::Mutex<mpsc::Receiver<DisplayUpdate>>>,
    sender: mpsc::Sender<DisplayUpdate>,
}

struct Updates(Arc<tokio::sync::Mutex<mpsc::Receiver<DisplayUpdate>>>);

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Updates {
    async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
        match self.0.lock().await.recv().await {
            Some(update) => Ok(Some(update)),
            None => std::future::pending().await,
        }
    }
}

#[async_trait::async_trait]
impl RdpServerDisplay for Display {
    async fn size(&mut self) -> DesktopSize {
        *self.size.lock().unwrap()
    }

    async fn updates(&mut self) -> ServerResult<Box<dyn RdpServerDisplayUpdates>> {
        Ok(Box::new(Updates(self.updates.clone())))
    }

    fn request_layout(
        &mut self,
        layout: ironrdp::displaycontrol::pdu::DisplayControlMonitorLayout,
    ) {
        let Some(monitor) = layout.monitors().first() else {
            return;
        };
        let (width, height) = monitor.dimensions();
        let size = DesktopSize {
            width: width as u16,
            height: height as u16,
        };
        *self.size.lock().unwrap() = size;
        let _ = self.sender.try_send(DisplayUpdate::Resize(size));
    }
}

struct Server {
    port: u16,
    received: mpsc::UnboundedReceiver<Received>,
    display: mpsc::Sender<DisplayUpdate>,
    clipboard: ServerClipboard,
}

type EventSender = Arc<Mutex<Option<mpsc::UnboundedSender<ServerEvent>>>>;

/// The server's side of CLIPRDR: pastes whatever the client offers and offers `offer`.
#[derive(Clone)]
struct ServerClipboard {
    events: EventSender,
    offer: Arc<Mutex<String>>,
    pasted: Arc<Mutex<Vec<String>>>,
}

impl ServerClipboard {
    fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(None)),
            offer: Arc::new(Mutex::new(String::new())),
            pasted: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn send(&self, message: ClipboardMessage) {
        if let Some(sender) = self.events.lock().unwrap().as_ref() {
            let _ = sender.send(ServerEvent::Clipboard(message));
        }
    }

    /// Copy `text` on the server: announce it to the client.
    fn copy(&self, text: &str) {
        *self.offer.lock().unwrap() = text.to_owned();
        self.send(ClipboardMessage::SendInitiateCopy(vec![
            ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
        ]));
    }
}

#[derive(Debug)]
struct ServerClipboardBackend {
    events: EventSender,
    offer: Arc<Mutex<String>>,
    pasted: Arc<Mutex<Vec<String>>>,
}

ironrdp::core::impl_as_any!(ServerClipboardBackend);

impl ServerClipboardBackend {
    fn send(&self, message: ClipboardMessage) {
        if let Some(sender) = self.events.lock().unwrap().as_ref() {
            let _ = sender.send(ServerEvent::Clipboard(message));
        }
    }
}

impl CliprdrBackend for ServerClipboardBackend {
    fn temporary_directory(&self) -> &str {
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {}

    fn on_request_format_list(&mut self) {}

    fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        if formats
            .iter()
            .any(|format| format.id() == ClipboardFormatId::CF_UNICODETEXT)
        {
            self.send(ClipboardMessage::SendInitiatePaste(
                ClipboardFormatId::CF_UNICODETEXT,
            ));
        }
    }

    fn on_format_data_request(&mut self, _: FormatDataRequest) {
        let text = self.offer.lock().unwrap().clone();
        self.send(ClipboardMessage::SendFormatData(
            FormatDataResponse::new_unicode_string(&text).into_owned(),
        ));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if let Ok(text) = response.to_unicode_string() {
            self.pasted
                .lock()
                .unwrap()
                .push(text.trim_end_matches('\0').to_owned());
        }
    }

    fn on_file_contents_request(&mut self, _: FileContentsRequest) {}

    fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _: LockDataId) {}

    fn on_unlock(&mut self, _: LockDataId) {}
}

impl CliprdrBackendFactory for ServerClipboard {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(ServerClipboardBackend {
            events: self.events.clone(),
            offer: self.offer.clone(),
            pasted: self.pasted.clone(),
        })
    }
}

impl ServerEventSender for ServerClipboard {
    fn set_sender(&mut self, sender: mpsc::UnboundedSender<ServerEvent>) {
        *self.events.lock().unwrap() = Some(sender);
    }
}

impl CliprdrServerFactory for ServerClipboard {}

#[derive(Clone, Copy, PartialEq)]
enum Security {
    Tls,
    Nla,
}

async fn start_server(security: Security, expected: Credentials) -> Server {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (received_tx, received) = mpsc::unbounded_channel();
    let (display_tx, display_rx) = mpsc::channel(16);
    let display = Display {
        size: Arc::new(Mutex::new(DesktopSize {
            width: 800,
            height: 600,
        })),
        updates: Arc::new(tokio::sync::Mutex::new(display_rx)),
        sender: display_tx.clone(),
    };
    let clipboard = ServerClipboard::new();
    let server_clipboard = clipboard.clone();
    // IronRDP's server future is not `Send`: give it a thread and a runtime of its own.
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let builder = RdpServer::builder().with_addr(([127, 0, 0, 1], port));
            let builder = match security {
                Security::Tls => builder.with_tls(tls_acceptor()),
                Security::Nla => {
                    let der = certificate_der();
                    let (_, certificate) = X509Certificate::from_der(der.as_ref()).unwrap();
                    let public_key = certificate.public_key().subject_public_key.data.to_vec();
                    builder.with_hybrid(tls_acceptor(), public_key)
                }
            };
            let mut server = builder
                .with_input_handler(Input(received_tx))
                .with_display_handler(display)
                .with_cliprdr_factory(Some(Box::new(server_clipboard)))
                .build();
            server.set_credentials(Some(expected));
            let listener = TcpListener::from_std(listener).unwrap();
            if let Ok((stream, _)) = listener.accept().await {
                let _ = server.run_connection(stream).await;
            }
        });
    });
    Server {
        port,
        received,
        display: display_tx,
        clipboard,
    }
}

fn alice() -> Credentials {
    Credentials {
        username: "alice".into(),
        password: "hunter2".into(),
        domain: Some("CORP".into()),
    }
}

fn options(port: u16, password: &str) -> RdpConnectOptions {
    RdpConnectOptions {
        // DOMAIN\user splits the way mstsc splits it.
        username: "CORP\\alice".into(),
        password: password.into(),
        width: 1024.0,
        height: 768.0,
        share_clipboard: false,
        ..RdpConnectOptions::new("127.0.0.1", port)
    }
}

async fn next_event(session: &RdpSession) -> RdpEvent {
    tokio::time::timeout(TIMEOUT, session.events().recv())
        .await
        .expect("an event in time")
        .expect("the session is open")
}

async fn wait_for(session: &RdpSession, mut matches: impl FnMut(&RdpEvent) -> bool) -> RdpEvent {
    loop {
        let event = next_event(session).await;
        if matches(&event) {
            return event;
        }
        if let RdpEvent::Ended(end) = &event {
            panic!("the session ended early: {end:?}");
        }
    }
}

async fn next_received(server: &mut Server) -> Received {
    tokio::time::timeout(TIMEOUT, server.received.recv())
        .await
        .expect("input in time")
        .expect("the server is running")
}

/// The input the server has seen, waiting until `count` events arrived.
async fn received(server: &mut Server, count: usize) -> Vec<Received> {
    let mut events = Vec::new();
    while events.len() < count {
        events.push(next_received(server).await);
    }
    events
}

fn solid(width: u16, height: u16, x: u16, y: u16, bgra: [u8; 4]) -> DisplayUpdate {
    let pixels = usize::from(width) * usize::from(height);
    let data: Vec<u8> = bgra.iter().copied().cycle().take(pixels * 4).collect();
    DisplayUpdate::Bitmap(BitmapUpdate {
        x,
        y,
        width: NonZeroU16::new(width).unwrap(),
        height: NonZeroU16::new(height).unwrap(),
        format: PixelFormat::BgrA32,
        data: Bytes::from(data),
        stride: NonZeroUsize::new(usize::from(width) * 4).unwrap(),
    })
}

fn assert_close(actual: Option<[u8; 4]>, expected: [u8; 4]) {
    let actual = actual.expect("a pixel inside the desktop");
    let close = actual
        .iter()
        .zip(expected)
        .all(|(&a, e)| a.abs_diff(e) <= 24);
    assert!(close, "{actual:?} is not close to {expected:?}");
}

fn key(code: &str, pressed: bool, modifiers: Modifiers) -> KeyEvent {
    KeyEvent {
        code: code.into(),
        key: String::new(),
        pressed,
        repeat: false,
        modifiers,
    }
}

#[tokio::test]
async fn draws_types_clicks_resizes_and_shuts_down() {
    let mut server = start_server(Security::Tls, alice()).await;
    let handler = RecordingHandler::new(true);
    let session = connect(
        RdpConnectOptions {
            pixel_order: PixelOrder::Rgba,
            ..options(server.port, "hunter2")
        },
        RdpTransport::Direct,
        handler.clone(),
    )
    .await
    .expect("connected");
    assert_eq!(handler.certificates().len(), 1);

    // The server decides the size; the framebuffer follows it.
    assert_eq!(
        next_event(&session).await,
        RdpEvent::Resized {
            width: 800,
            height: 600
        }
    );
    assert_eq!(session.desktop_size(), (800, 600));
    assert_eq!(next_event(&session).await, RdpEvent::Frame);
    let framebuffer = session.framebuffer();
    assert_eq!(
        framebuffer.take_dirty(),
        [muxus_rdp::Rect::new(0, 0, 800, 600)]
    );

    // A red square arrives as a frame with its dirty region.
    server
        .display
        .send(solid(64, 32, 16, 8, [0, 0, 255, 255]))
        .await
        .unwrap();
    wait_for(&session, |event| *event == RdpEvent::Frame).await;
    let dirty = framebuffer.take_dirty();
    assert!(!dirty.is_empty());
    {
        let state = framebuffer.lock();
        // The server encodes lossily (RemoteFX): allow a little rounding.
        assert_close(state.rgba_at(20, 10), [255, 0, 0, 255]);
        assert_close(state.rgba_at(79, 39), [255, 0, 0, 255]);
        assert_eq!(state.rgba_at(81, 41), Some([0, 0, 0, 255]));
    }

    // Shift stays held while typing: one synchronize event, then the keys.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    assert!(session.key(&key("ShiftLeft", true, shift)));
    assert!(session.key(&key("KeyA", true, shift)));
    assert!(session.key(&key("KeyA", false, shift)));
    assert!(session.key(&key("ShiftLeft", false, Modifiers::default())));
    assert!(!session.key(&key("Unidentified", true, Modifiers::default())));
    assert_eq!(
        received(&mut server, 5).await,
        [
            Received::Sync(0),
            Received::KeyDown(0x2a, false),
            Received::KeyDown(0x1e, false),
            Received::KeyUp(0x1e, false),
            Received::KeyUp(0x2a, false),
        ]
    );

    // Extended keys keep their prefix; the toolbar chord goes out in order.
    session.send_ctrl_alt_del();
    assert_eq!(
        received(&mut server, 6).await,
        [
            Received::KeyDown(0x1d, false),
            Received::KeyDown(0x38, false),
            Received::KeyDown(0x53, true),
            Received::KeyUp(0x53, true),
            Received::KeyUp(0x38, false),
            Received::KeyUp(0x1d, false),
        ]
    );

    // A click moves first, then presses.
    session.pointer_button(5, 6, MouseButton::Left, true);
    session.pointer_button(5, 6, MouseButton::Left, false);
    assert_eq!(
        received(&mut server, 3).await,
        [
            Received::Move(5, 6),
            Received::Button(5, 6, "left", true),
            Received::Button(5, 6, "left", false),
        ]
    );

    // Releasing everything lets go of a held key.
    session.key(&key("KeyQ", true, Modifiers::default()));
    session.release_all();
    assert_eq!(
        received(&mut server, 2).await,
        [Received::KeyDown(0x10, false), Received::KeyUp(0x10, false)]
    );

    // The pane grew: display control asks the server, which deactivates and reactivates.
    session.resize(1000.4, 700.0);
    assert_eq!(
        wait_for(&session, |event| matches!(event, RdpEvent::Resized { .. })).await,
        RdpEvent::Resized {
            width: 1000,
            height: 700
        }
    );
    assert_eq!(framebuffer.size(), (1000, 700));
    assert_eq!(next_event(&session).await, RdpEvent::Frame);
    assert_eq!(
        framebuffer.take_dirty(),
        [muxus_rdp::Rect::new(0, 0, 1000, 700)]
    );

    // Pictures keep arriving at the new size.
    server
        .display
        .send(solid(10, 10, 990, 690, [0, 255, 0, 255]))
        .await
        .unwrap();
    wait_for(&session, |event| *event == RdpEvent::Frame).await;
    assert_close(framebuffer.lock().rgba_at(995, 695), [0, 255, 0, 255]);

    session.shutdown();
    let end = wait_for(&session, |event| matches!(event, RdpEvent::Ended(_))).await;
    assert!(
        matches!(
            end,
            RdpEvent::Ended(SessionEnd::Closed | SessionEnd::Completed { .. })
        ),
        "{end:?}"
    );
    assert!(session.is_closed());
}

#[tokio::test]
async fn logs_on_with_network_level_authentication() {
    let server = start_server(Security::Nla, alice()).await;
    let session = connect(
        RdpConnectOptions {
            pixel_order: PixelOrder::Bgra,
            ..options(server.port, "hunter2")
        },
        RdpTransport::Direct,
        RecordingHandler::new(true),
    )
    .await
    .expect("NLA logon");
    assert!(matches!(
        next_event(&session).await,
        RdpEvent::Resized { .. }
    ));
    assert_eq!(next_event(&session).await, RdpEvent::Frame);
    let framebuffer = session.framebuffer();
    framebuffer.take_dirty();
    server
        .display
        .send(solid(64, 64, 0, 0, [10, 20, 30, 255]))
        .await
        .unwrap();
    wait_for(&session, |event| *event == RdpEvent::Frame).await;
    let state = framebuffer.lock();
    // Stored as B, G, R, A; read back as R, G, B, A.
    let stored = &state.pixels()[..4];
    assert_close(
        Some([stored[0], stored[1], stored[2], stored[3]]),
        [10, 20, 30, 255],
    );
    assert_close(state.rgba_at(0, 0), [30, 20, 10, 255]);
}

#[tokio::test]
async fn asks_for_the_password_again_when_nla_rejects_it() {
    let server = start_server(Security::Nla, alice()).await;
    let failure = connect(
        options(server.port, "wrong"),
        RdpTransport::Direct,
        RecordingHandler::new(true),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.kind, RdpFailureKind::Credentials, "{failure:?}");
    assert_eq!(failure.message, "The user name or password is incorrect.");
}

/// A TCP relay in front of `port` that the test can cut.
async fn relay(port: u16) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut upstream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });
    (relay_port, task)
}

#[tokio::test]
async fn reports_a_lost_connection() {
    let server = start_server(Security::Tls, alice()).await;
    let (port, relay) = relay(server.port).await;
    let session = connect(
        options(port, "hunter2"),
        RdpTransport::Direct,
        RecordingHandler::new(true),
    )
    .await
    .unwrap();
    assert!(matches!(
        next_event(&session).await,
        RdpEvent::Resized { .. }
    ));
    relay.abort();
    let end = wait_for(&session, |event| matches!(event, RdpEvent::Ended(_))).await;
    assert_eq!(
        end,
        RdpEvent::Ended(SessionEnd::Failed(muxus_rdp::RdpFailure::new(
            "The connection to the remote computer was lost.",
            RdpFailureKind::Failed
        )))
    );
    // Input after the end goes nowhere.
    session.send_windows_key();
    assert!(session.is_closed());
}

#[tokio::test]
async fn shares_text_on_the_clipboard_both_ways() {
    let server = start_server(Security::Tls, alice()).await;
    let session = connect(
        RdpConnectOptions {
            share_clipboard: true,
            ..options(server.port, "hunter2")
        },
        RdpTransport::Direct,
        RecordingHandler::new(true),
    )
    .await
    .unwrap();
    // Give CLIPRDR a moment to finish its capability exchange.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Copied on the server: the client fetches it and reports the text.
    server.clipboard.copy("from the server");
    assert_eq!(
        wait_for(&session, |event| matches!(
            event,
            RdpEvent::RemoteClipboard(_)
        ))
        .await,
        RdpEvent::RemoteClipboard("from the server".into())
    );

    // Offering back what the remote side just copied is a no-op.
    session.send_clipboard_text("from the server", false);
    let pasted = server.clipboard.pasted.clone();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(pasted.lock().unwrap().is_empty());

    // Copied locally: announced, then fetched by the server's paste.
    session.send_clipboard_text("from muxus", false);
    let wait_for_pastes = |count: usize| {
        let pasted = pasted.clone();
        async move {
            tokio::time::timeout(TIMEOUT, async {
                while pasted.lock().unwrap().len() < count {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("the server pasted");
        }
    };
    wait_for_pastes(1).await;
    assert_eq!(pasted.lock().unwrap().as_slice(), ["from muxus"]);

    // The same text again is not announced again; forcing announces it anyway.
    session.send_clipboard_text("from muxus", false);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(pasted.lock().unwrap().len(), 1);
    session.send_clipboard_text("from muxus", true);
    wait_for_pastes(2).await;
}
