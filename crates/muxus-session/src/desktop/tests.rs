//! Desktop sessions end to end against in-process servers: an RFB server written here,
//! IronRDP's own server, a fake RDP front end and the SSH test server as a gateway. Ports the
//! cases of `tests/unit/server/desktop-socket.test.ts` that still apply natively (tickets and
//! the RDCleanPath codec have no counterpart), plus the retry and remember flows the view
//! drove in `RemoteDesktopViewImpl.tsx`.

use std::num::NonZeroU16;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ironrdp_server::{
    BitmapUpdate, Credentials, DesktopSize, DisplayUpdate, KeyboardEvent, MouseEvent, PixelFormat,
    RdpServer, RdpServerDisplay, RdpServerDisplayUpdates, RdpServerInputHandler, ServerResult,
};
use muxus_core::protocol::{
    AuthPromptPurpose, AuthPromptResponse, DesktopIdentity, DesktopProfile, ExitReason, TrustState,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

use super::session::Session;
use super::*;
use crate::{Backend, BackendOptions, RuntimeChoice};

const TIMEOUT: Duration = Duration::from_secs(20);

// --- the backend -------------------------------------------------------------------------------

struct Sandbox {
    dir: tempfile::TempDir,
    backend: Backend,
}

impl Sandbox {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut options = BackendOptions::sandbox(dir.path().join("data"), home);
        options.runtime = RuntimeChoice::Own { worker_threads: 2 };
        let backend = Backend::start(options).await.unwrap();
        Self { dir, backend }
    }

    fn write_ssh_config(&self, lines: &[&str]) {
        let ssh = self.dir.path().join("home").join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::write(ssh.join("config"), format!("{}\n", lines.join("\n"))).unwrap();
    }

    fn open(&self, profile: serde_json::Value) -> DesktopHandle {
        let profile: DesktopProfile = serde_json::from_value(profile).unwrap();
        self.backend.open_desktop_session(
            profile,
            DesktopOptions {
                width: 1024.0,
                height: 768.0,
                pixel_order: PixelOrder::Rgba,
            },
        )
    }

    async fn stop(self) {
        self.backend.shutdown().await;
    }
}

async fn next(handle: &DesktopHandle) -> DesktopEvent {
    tokio::time::timeout(TIMEOUT, handle.next_event())
        .await
        .expect("an event in time")
        .expect("the session is still open")
}

/// Events up to the first one `stop` matches (included).
async fn until(handle: &DesktopHandle, stop: impl Fn(&DesktopEvent) -> bool) -> Vec<DesktopEvent> {
    let mut events = Vec::new();
    loop {
        let event = next(handle).await;
        let done = stop(&event);
        if !done && let DesktopEvent::Exit { message, .. } = &event {
            panic!("the session ended early: {message}; before that: {events:?}");
        }
        events.push(event);
        if done {
            return events;
        }
    }
}

/// The next event that is not a status line, a picture update or a pointer change.
async fn next_question(handle: &DesktopHandle) -> DesktopEvent {
    loop {
        match next(handle).await {
            DesktopEvent::Status { .. }
            | DesktopEvent::Dialing(_)
            | DesktopEvent::Frame
            | DesktopEvent::Resized { .. }
            | DesktopEvent::Cursor(_)
            | DesktopEvent::PointerPosition { .. }
            | DesktopEvent::DesktopName(_) => {}
            event => return event,
        }
    }
}

async fn exit_of(handle: &DesktopHandle) -> (String, ExitReason) {
    loop {
        if let DesktopEvent::Exit { message, reason } = next(handle).await {
            let after = tokio::time::timeout(TIMEOUT, handle.next_event())
                .await
                .expect("the queue closes after the exit");
            assert!(after.is_none(), "nothing follows the exit: {after:?}");
            return (message, reason);
        }
    }
}

fn answer(text: &str) -> Option<AuthPromptResponse> {
    Some(AuthPromptResponse {
        answers: vec![text.into()],
        ..AuthPromptResponse::default()
    })
}

fn statuses(events: &[DesktopEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            DesktopEvent::Status { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

/// The next lasting note (a status that is not transient).
async fn next_note(handle: &DesktopHandle) -> String {
    loop {
        match next(handle).await {
            DesktopEvent::Status {
                message,
                transient: false,
            } => return message,
            DesktopEvent::Exit { message, .. } => panic!("ended: {message}"),
            _ => {}
        }
    }
}

fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

// --- an RFB server -----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum RfbSecurity {
    None,
    VncAuth,
}

/// An RFB 3.8 server with a 64×48 desktop painted one colour. VncAuth logins fail for the
/// first `reject` connections.
struct RfbServer {
    port: u16,
    connections: Arc<AtomicUsize>,
    responses: Arc<Mutex<Vec<Vec<u8>>>>,
}

const RFB_COLOUR: [u8; 3] = [0x22, 0x88, 0xcc];

impl RfbServer {
    async fn start(security: RfbSecurity, reject: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let responses = Arc::new(Mutex::new(Vec::new()));
        let (count, recorded) = (connections.clone(), responses.clone());
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(serve_rfb(
                    socket,
                    security,
                    attempt < reject,
                    recorded.clone(),
                ));
            }
        });
        Self {
            port,
            connections,
            responses,
        }
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

async fn serve_rfb(
    mut socket: TcpStream,
    security: RfbSecurity,
    reject: bool,
    responses: Arc<Mutex<Vec<Vec<u8>>>>,
) -> std::io::Result<()> {
    socket.write_all(b"RFB 003.008\n").await?;
    socket.read_exact(&mut [0u8; 12]).await?;
    match security {
        RfbSecurity::None => {
            socket.write_all(&[1, 1]).await?;
            socket.read_exact(&mut [0u8; 1]).await?;
        }
        RfbSecurity::VncAuth => {
            socket.write_all(&[1, 2]).await?;
            socket.read_exact(&mut [0u8; 1]).await?;
            socket.write_all(&[7u8; 16]).await?;
            let mut response = vec![0u8; 16];
            socket.read_exact(&mut response).await?;
            responses.lock().unwrap().push(response);
        }
    }
    if reject {
        let reason = b"Authentication failed";
        socket.write_all(&1u32.to_be_bytes()).await?;
        socket
            .write_all(&(reason.len() as u32).to_be_bytes())
            .await?;
        socket.write_all(reason).await?;
        return Ok(());
    }
    socket.write_all(&0u32.to_be_bytes()).await?;
    socket.read_exact(&mut [0u8; 1]).await?;
    let (width, height) = (64u16, 48u16);
    let name = b"test-desktop";
    let mut init = width.to_be_bytes().to_vec();
    init.extend(height.to_be_bytes());
    init.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    init.extend((name.len() as u32).to_be_bytes());
    init.extend(name);
    socket.write_all(&init).await?;
    let mut shifts = (16u32, 8u32, 0u32);
    loop {
        let mut kind = [0u8; 1];
        socket.read_exact(&mut kind).await?;
        match kind[0] {
            0 => {
                let mut rest = [0u8; 19];
                socket.read_exact(&mut rest).await?;
                shifts = (rest[13].into(), rest[14].into(), rest[15].into());
            }
            2 => {
                let mut head = [0u8; 3];
                socket.read_exact(&mut head).await?;
                let count = usize::from(u16::from_be_bytes([head[1], head[2]]));
                socket.read_exact(&mut vec![0u8; count * 4]).await?;
            }
            3 => {
                let mut rest = [0u8; 9];
                socket.read_exact(&mut rest).await?;
                if rest[0] == 0 {
                    let pixel = (u32::from(RFB_COLOUR[0]) << shifts.0)
                        | (u32::from(RFB_COLOUR[1]) << shifts.1)
                        | (u32::from(RFB_COLOUR[2]) << shifts.2);
                    let mut update = vec![0, 0, 0, 1, 0, 0, 0, 0];
                    update.extend(width.to_be_bytes());
                    update.extend(height.to_be_bytes());
                    update.extend(0i32.to_be_bytes());
                    for _ in 0..usize::from(width) * usize::from(height) {
                        update.extend(pixel.to_le_bytes());
                    }
                    socket.write_all(&update).await?;
                }
            }
            4 => {
                socket.read_exact(&mut [0u8; 7]).await?;
            }
            5 => {
                socket.read_exact(&mut [0u8; 5]).await?;
            }
            6 => {
                let mut head = [0u8; 7];
                socket.read_exact(&mut head).await?;
                let length = u32::from_be_bytes([head[3], head[4], head[5], head[6]]);
                socket.read_exact(&mut vec![0u8; length as usize]).await?;
            }
            _ => return Ok(()),
        }
    }
}

// --- IronRDP's server and a fake RDP front end -------------------------------------------------

/// Self-signed P-256 certificate for the RDP servers (valid until 2126).
const TEST_CERTIFICATE: &str = "-----BEGIN CERTIFICATE-----
MIIBijCCAS+gAwIBAgIUPTlx/wKVtLsVO0U3wZfD4rXynqgwCgYIKoZIzj0EAwIw
GTEXMBUGA1UEAwwOcmRwLm11eHVzLnRlc3QwIBcNMjYwOTI0MTU0NzI4WhgPMjEy
NjA4MzExNTQ3MjhaMBkxFzAVBgNVBAMMDnJkcC5tdXh1cy50ZXN0MFkwEwYHKoZI
zj0CAQYIKoZIzj0DAQcDQgAEwQ/R8JYkDXHTekz21csVDCuFJWA0KfI4ds0fhKjE
suY+BdGh9nx39N3oN3431swMBfQ15jQN7bvT19dSyHzVUaNTMFEwHQYDVR0OBBYE
FL+fNkTerPkje0NzQDJTSuK1IMZMMB8GA1UdIwQYMBaAFL+fNkTerPkje0NzQDJT
SuK1IMZMMA8GA1UdEwEB/wQFMAMBAf8wCgYIKoZIzj0EAwIDSQAwRgIhAMRoLMzy
oaU+Ln9UhnFQhUTn1a5vff4YA5VSCwDmo2m+AiEAr36LIvt2vzYXvOcLiD5S2IC8
qFqy+UwGqP02HkIH6Ys=
-----END CERTIFICATE-----";
const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgUMEJjrQv5A6WS7Rw
q4UKUmJMYMcFCiMJ29wURxvZd/yhRANCAATBD9HwliQNcdN6TPbVyxUMK4UlYDQp
8jh2zR+EqMSy5j4F0aH2fHf03eg3fjfWzAwF9DXmNA3tu9PX11LIfNVR
-----END PRIVATE KEY-----";

fn certificate_der() -> CertificateDer<'static> {
    CertificateDer::from_pem_slice(TEST_CERTIFICATE.as_bytes()).unwrap()
}

fn tls_acceptor() -> TlsAcceptor {
    let key = PrivateKeyDer::from_pem_slice(TEST_KEY.as_bytes()).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![certificate_der()], key)
    .unwrap();
    TlsAcceptor::from(Arc::new(config))
}

/// The certificate's P-256 public key (the BIT STRING after `03 42 00`), which CredSSP binds.
fn certificate_public_key() -> Vec<u8> {
    let der = certificate_der();
    let der = der.as_ref();
    let at = der
        .windows(4)
        .position(|window| window == [0x03, 0x42, 0x00, 0x04])
        .expect("an uncompressed P-256 point");
    der[at + 3..at + 3 + 65].to_vec()
}

struct NoInput;

impl RdpServerInputHandler for NoInput {
    fn keyboard(&mut self, _: KeyboardEvent) {}
    fn mouse(&mut self, _: MouseEvent) {}
}

/// An 800×600 desktop that sends one square and then nothing.
struct Display;

struct Updates(bool);

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Updates {
    async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
        if std::mem::replace(&mut self.0, true) {
            return std::future::pending().await;
        }
        let (width, height) = (32u16, 32u16);
        let data: Vec<u8> = [200u8, 100, 50, 255]
            .iter()
            .copied()
            .cycle()
            .take(usize::from(width) * usize::from(height) * 4)
            .collect();
        Ok(Some(DisplayUpdate::Bitmap(BitmapUpdate {
            x: 0,
            y: 0,
            width: NonZeroU16::new(width).unwrap(),
            height: NonZeroU16::new(height).unwrap(),
            format: PixelFormat::BgrA32,
            data: bytes::Bytes::from(data),
            stride: std::num::NonZeroUsize::new(usize::from(width) * 4).unwrap(),
        })))
    }
}

#[async_trait::async_trait]
impl RdpServerDisplay for Display {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: 800,
            height: 600,
        }
    }

    async fn updates(&mut self) -> ServerResult<Box<dyn RdpServerDisplayUpdates>> {
        Ok(Box::new(Updates(false)))
    }
}

/// IronRDP's server for `alice` / `hunter2`, serving connections one after another.
fn start_rdp_server(nla: bool) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    // IronRDP's server future is not `Send`: give it a thread and a runtime of its own.
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let builder = RdpServer::builder().with_addr(([127, 0, 0, 1], port));
            let builder = if nla {
                builder.with_hybrid(tls_acceptor(), certificate_public_key())
            } else {
                builder.with_tls(tls_acceptor())
            };
            let mut server = builder
                .with_input_handler(NoInput)
                .with_display_handler(Display)
                .build();
            server.set_credentials(Some(Credentials {
                username: "alice".into(),
                password: "hunter2".into(),
                domain: None,
            }));
            let listener = TcpListener::from_std(listener).unwrap();
            while let Ok((stream, _)) = listener.accept().await {
                let _ = server.run_connection(stream).await;
            }
        });
    });
    port
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// An RDP server's front half: an X.224 confirm for TLS (or a refusal), the TLS handshake,
/// then a reset.
async fn fake_rdp_server(refusal: bool) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut header = [0u8; 4];
                if socket.read_exact(&mut header).await.is_err() {
                    return;
                }
                let length = usize::from(u16::from_be_bytes([header[2], header[3]]));
                if socket
                    .read_exact(&mut vec![0u8; length.saturating_sub(4)])
                    .await
                    .is_err()
                {
                    return;
                }
                if refusal {
                    // RDP_NEG_FAILURE with HYBRID_REQUIRED_BY_SERVER.
                    let _ = socket
                        .write_all(&hex("030000130ed000000000000300080005000000"))
                        .await;
                    let _ = socket.read(&mut [0u8; 16]).await;
                    return;
                }
                let _ = socket
                    .write_all(&hex("030000130ed000001234000201080001000000"))
                    .await;
                let Ok(mut tls) = tls_acceptor().accept(socket).await else {
                    return;
                };
                tokio::time::sleep(Duration::from_millis(20)).await;
                let _ = tls.get_mut().0.set_zero_linger();
                drop(tls);
            });
        }
    });
    port
}

// --- saved hosts and refusals ------------------------------------------------------------------

#[tokio::test]
async fn rejects_unknown_saved_hosts() {
    let sandbox = Sandbox::start().await;
    let handle = sandbox.open(serde_json::json!({
        "kind": "rdp", "host": "x", "port": 3389, "profileId": "missing",
    }));
    assert_eq!(
        exit_of(&handle).await,
        (
            "This saved host no longer exists.".to_owned(),
            ExitReason::Failed
        )
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn reports_a_refused_tcp_connection_with_its_socket_error() {
    let sandbox = Sandbox::start().await;
    let port = closed_port();
    let handle = sandbox.open(serde_json::json!({
        "kind": "rdp", "host": "127.0.0.1", "port": port, "username": "bob",
    }));
    let DesktopEvent::AuthPrompt(prompt) = next_question(&handle).await else {
        panic!("expected the logon prompt");
    };
    assert_eq!(prompt.name.as_deref(), Some("Remote Desktop logon"));
    assert_eq!(prompt.host, Some(format!("127.0.0.1:{port}")));
    assert_eq!(prompt.prompts.len(), 1);
    assert_eq!(prompt.prompts[0].prompt, "Password");
    assert!(!prompt.prompts[0].echo);
    handle.answer_auth(answer("pw"));
    assert_eq!(
        exit_of(&handle).await,
        (
            "The remote computer refused the connection. Is Remote Desktop enabled, and is the port right?"
                .to_owned(),
            ExitReason::Failed
        )
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn passes_a_server_refusal_back_as_a_negotiation_error() {
    let sandbox = Sandbox::start().await;
    let port = fake_rdp_server(true).await;
    let handle = sandbox.open(serde_json::json!({
        "kind": "rdp", "host": "127.0.0.1", "port": port, "username": "bob",
    }));
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::AuthPrompt(_)
    ));
    handle.answer_auth(answer("pw"));
    assert_eq!(
        exit_of(&handle).await,
        (
            "The server requires Network Level Authentication, which it could not complete."
                .to_owned(),
            ExitReason::Failed
        )
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn survives_the_server_dropping_the_connection_while_the_certificate_prompt_is_open() {
    let sandbox = Sandbox::start().await;
    let port = fake_rdp_server(false).await;
    let profile = serde_json::json!({
        "kind": "rdp", "host": "127.0.0.1", "port": port, "username": "eve",
    });
    let handle = sandbox.open(profile.clone());
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::AuthPrompt(_)
    ));
    handle.answer_auth(answer("pw"));
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::Certificate(_)
    ));
    // The reset lands while the prompt is still open; answer afterwards.
    tokio::time::sleep(Duration::from_millis(150)).await;
    handle.answer_certificate(true);
    assert_eq!(
        exit_of(&handle).await,
        (
            "The remote computer closed the connection.".to_owned(),
            ExitReason::Failed
        )
    );
    // The backend is still serving: a new session gets as far as its logon prompt.
    let again = sandbox.open(profile);
    assert!(matches!(
        next_question(&again).await,
        DesktopEvent::AuthPrompt(_)
    ));
    again.close();
    assert_eq!(
        exit_of(&again).await,
        ("The connection closed.".to_owned(), ExitReason::Failed)
    );
    sandbox.stop().await;
}

// --- RDP against IronRDP's server --------------------------------------------------------------

#[tokio::test]
async fn pins_the_rdp_certificate_and_trusts_it_on_the_next_connect() {
    let sandbox = Sandbox::start().await;
    let port = start_rdp_server(false);
    let profile = serde_json::json!({
        "kind": "rdp", "host": "127.0.0.1", "port": port, "username": "alice",
    });
    let handle = sandbox.open(profile.clone());
    let DesktopEvent::AuthPrompt(prompt) = next_question(&handle).await else {
        panic!("expected the logon prompt");
    };
    assert_eq!(prompt.name.as_deref(), Some("Remote Desktop logon"));
    assert_eq!(prompt.instructions, None);
    let offer = prompt.remember_password.expect("a remember offer");
    assert_eq!(offer.label, format!("RDP alice@127.0.0.1:{port}"));
    assert!(!offer.existing);
    handle.answer_auth(answer("hunter2"));
    let DesktopEvent::Certificate(challenge) = next_question(&handle).await else {
        panic!("expected the certificate");
    };
    assert_eq!(
        (challenge.host.as_str(), challenge.port),
        ("127.0.0.1", port)
    );
    assert_eq!(challenge.state, TrustState::New);
    assert_eq!(challenge.previous, None);
    let DesktopIdentity::Certificate { subject, .. } = &challenge.identity else {
        panic!("expected a certificate identity");
    };
    assert_eq!(subject, "CN=rdp.muxus.test");
    assert_eq!(challenge.fingerprint.split(':').count(), 32);
    handle.answer_certificate(true);
    let DesktopEvent::Connected(connection) = next_question(&handle).await else {
        panic!("expected the desktop");
    };
    assert!(connection.is_rdp());
    until(&handle, |event| matches!(event, DesktopEvent::Frame)).await;
    let pinned = sandbox
        .backend
        .database()
        .trusted_desktop_identity("127.0.0.1", port, "")
        .unwrap()
        .expect("the certificate is pinned");
    assert_eq!(pinned.fingerprint, challenge.fingerprint);
    assert_eq!(pinned.subject, "CN=rdp.muxus.test");
    handle.close();
    assert_eq!(
        exit_of(&handle).await,
        (
            "The connection closed.".to_owned(),
            ExitReason::Disconnected
        )
    );

    // The pinned certificate is not questioned again.
    let again = sandbox.open(profile);
    assert!(matches!(
        next_question(&again).await,
        DesktopEvent::AuthPrompt(_)
    ));
    again.answer_auth(answer("hunter2"));
    assert!(matches!(
        next_question(&again).await,
        DesktopEvent::Connected(_)
    ));
    drop(again);
    sandbox.stop().await;
}

#[tokio::test]
async fn asks_for_the_rdp_password_again_when_the_server_rejects_it() {
    let sandbox = Sandbox::start().await;
    let port = start_rdp_server(true);
    let handle = sandbox.open(serde_json::json!({
        "kind": "rdp", "host": "127.0.0.1", "port": port, "username": "alice",
    }));
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::AuthPrompt(_)
    ));
    handle.answer_auth(answer("wrong"));
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::Certificate(_)
    ));
    handle.answer_certificate(true);
    let events = until(&handle, |event| {
        matches!(event, DesktopEvent::AuthPrompt(_))
    })
    .await;
    assert!(
        statuses(&events).contains(&"The user name or password is incorrect.".to_owned()),
        "{events:?}"
    );
    let Some(DesktopEvent::AuthPrompt(prompt)) = events.last() else {
        unreachable!()
    };
    assert_eq!(
        prompt.instructions.as_deref(),
        Some("The user name or password was not accepted.")
    );
    handle.answer_auth(answer("hunter2"));
    // The certificate was pinned on the first attempt.
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::Connected(_)
    ));
    handle.close();
    exit_of(&handle).await;
    sandbox.stop().await;
}

// --- VNC ---------------------------------------------------------------------------------------

#[tokio::test]
async fn asks_for_vnc_credentials_when_the_server_wants_them() {
    let sandbox = Sandbox::start().await;
    let server = RfbServer::start(RfbSecurity::VncAuth, 0).await;
    let handle = sandbox.open(serde_json::json!({
        "kind": "vnc", "host": "127.0.0.1", "port": server.port,
    }));
    let DesktopEvent::AuthPrompt(prompt) = next_question(&handle).await else {
        panic!("expected the VNC password prompt");
    };
    assert_eq!(prompt.name.as_deref(), Some("VNC authentication"));
    assert_eq!(prompt.host, Some(format!("127.0.0.1:{}", server.port)));
    assert_eq!(prompt.purpose, Some(AuthPromptPurpose::Authentication));
    assert_eq!(prompt.prompts.len(), 1);
    assert_eq!(prompt.prompts[0].prompt, "Password");
    assert!(!prompt.prompts[0].echo);
    assert!(!prompt.remember_password.expect("a remember offer").existing);
    handle.answer_auth(answer("secret"));
    let DesktopEvent::Connected(connection) = next_question(&handle).await else {
        panic!("expected the desktop");
    };
    until(&handle, |event| matches!(event, DesktopEvent::Frame)).await;
    // The first frame may come in pieces; wait for the whole picture.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        connection.take_dirty();
        let corner = connection.read_pixels(|frame| {
            assert_eq!((frame.width, frame.height), (64, 48));
            let last = frame.stride * 47 + 63 * 4;
            [
                frame.pixels[last],
                frame.pixels[last + 1],
                frame.pixels[last + 2],
            ]
        });
        if corner == RFB_COLOUR {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the picture arrived"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(server.responses.lock().unwrap().len(), 1);
    handle.close();
    assert_eq!(
        exit_of(&handle).await,
        (
            "The connection closed.".to_owned(),
            ExitReason::Disconnected
        )
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn asks_again_when_the_vnc_server_rejects_the_login() {
    let sandbox = Sandbox::start().await;
    let server = RfbServer::start(RfbSecurity::VncAuth, 1).await;
    let handle = sandbox.open(serde_json::json!({
        "kind": "vnc", "host": "127.0.0.1", "port": server.port,
    }));
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::AuthPrompt(_)
    ));
    handle.answer_auth(answer("wrong"));
    let events = until(&handle, |event| {
        matches!(event, DesktopEvent::AuthPrompt(_))
    })
    .await;
    assert!(
        statuses(&events)
            .contains(&"The server rejected the login: Authentication failed".to_owned()),
        "{events:?}"
    );
    let Some(DesktopEvent::AuthPrompt(prompt)) = events.last() else {
        unreachable!()
    };
    assert_eq!(
        prompt.instructions.as_deref(),
        Some("The password was not accepted.")
    );
    handle.answer_auth(answer("secret"));
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::Connected(_)
    ));
    assert_eq!(server.connections(), 2);
    handle.close();
    exit_of(&handle).await;
    sandbox.stop().await;
}

#[tokio::test]
async fn dials_a_saved_host_with_its_current_settings() {
    let sandbox = Sandbox::start().await;
    let server = RfbServer::start(RfbSecurity::None, 0).await;
    let saved = sandbox
        .backend
        .database()
        .save_saved_host_profile(
            &serde_json::from_value(serde_json::json!({
                "name": "design-vm",
                "profile": {
                    "kind": "vnc", "host": "127.0.0.1", "port": server.port,
                    "viewOnly": true, "shareClipboard": false,
                },
            }))
            .unwrap(),
        )
        .unwrap();
    // The tab still holds the settings from before the host was edited.
    let handle = sandbox.open(serde_json::json!({
        "kind": "vnc", "profileId": saved.id, "host": "old-name", "port": 5900,
        "shareClipboard": true,
    }));
    let dialed = loop {
        if let DesktopEvent::Dialing(profile) = next(&handle).await {
            break profile;
        }
    };
    let DesktopProfile::Vnc(dialed) = dialed else {
        panic!("a VNC profile")
    };
    assert_eq!(dialed.profile_id.as_deref(), Some(saved.id.as_str()));
    assert_eq!(
        (dialed.host.as_str(), dialed.port),
        ("127.0.0.1", server.port)
    );
    assert_eq!(dialed.view_only, Some(true));
    assert_eq!(dialed.share_clipboard, Some(false));
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::Connected(_)
    ));
    // The recent connection is recorded once the desktop is up.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let host = sandbox
            .backend
            .database()
            .saved_host_profile(&saved.id)
            .unwrap()
            .unwrap();
        if host.metadata.connect_count == 1 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    handle.close();
    exit_of(&handle).await;
    sandbox.stop().await;
}

#[tokio::test]
async fn keeps_a_connected_session_when_remembering_its_password_fails() {
    let sandbox = Sandbox::start().await;
    let server = RfbServer::start(RfbSecurity::VncAuth, 0).await;
    let handle = sandbox.open(serde_json::json!({
        "kind": "vnc", "host": "127.0.0.1", "port": server.port,
    }));
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::AuthPrompt(_)
    ));
    handle.answer_auth(Some(AuthPromptResponse {
        answers: vec!["secret".into()],
        remember_password: Some(true),
        skipped: None,
    }));
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::Connected(_)
    ));
    // No vault yet: creating one is offered, and a mismatched confirmation fails.
    let DesktopEvent::AuthPrompt(create) = next_question(&handle).await else {
        panic!("expected the vault prompt");
    };
    assert_eq!(create.purpose, Some(AuthPromptPurpose::VaultCreate));
    handle.answer_auth(Some(AuthPromptResponse {
        answers: vec!["master-password-1".into(), "something-else".into()],
        ..AuthPromptResponse::default()
    }));
    assert_eq!(
        next_note(&handle).await,
        "The password could not be remembered: The master-password confirmation did not match."
    );
    handle.close();
    assert_eq!(
        exit_of(&handle).await,
        (
            "The connection closed.".to_owned(),
            ExitReason::Disconnected
        )
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn remembers_a_vnc_password_and_uses_it_next_time() {
    let sandbox = Sandbox::start().await;
    let server = RfbServer::start(RfbSecurity::VncAuth, 0).await;
    let profile = serde_json::json!({ "kind": "vnc", "host": "127.0.0.1", "port": server.port });
    let handle = sandbox.open(profile.clone());
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::AuthPrompt(_)
    ));
    handle.answer_auth(Some(AuthPromptResponse {
        answers: vec!["secret".into()],
        remember_password: Some(true),
        skipped: None,
    }));
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::Connected(_)
    ));
    assert!(matches!(
        next_question(&handle).await,
        DesktopEvent::AuthPrompt(_)
    ));
    handle.answer_auth(Some(AuthPromptResponse {
        answers: vec!["master-password-1".into(), "master-password-1".into()],
        ..AuthPromptResponse::default()
    }));
    assert_eq!(
        next_note(&handle).await,
        format!("Remembered the password for VNC 127.0.0.1:{}.", server.port)
    );
    handle.close();
    exit_of(&handle).await;

    let again = sandbox.open(profile);
    let events = until(&again, |event| matches!(event, DesktopEvent::Connected(_))).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, DesktopEvent::AuthPrompt(_))),
        "{events:?}"
    );
    assert!(
        statuses(&events).contains(&format!(
            "Using the saved password for VNC 127.0.0.1:{}.",
            server.port
        )),
        "{events:?}"
    );
    let responses = server.responses.lock().unwrap().clone();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0], responses[1], "the same password both times");
    again.close();
    exit_of(&again).await;
    sandbox.stop().await;
}

/// `acceptServerKey`: a new key is the user's call and pinned once trusted; the pinned key
/// passes; a different one is a warning; another port is another server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pins_a_vnc_server_key_and_asks_again_when_it_changes() {
    let sandbox = Sandbox::start().await;
    let fingerprint = |seed: u8| {
        (0..32u8)
            .map(|i| format!("{:02X}", seed.wrapping_add(i)))
            .collect::<Vec<_>>()
            .join(":")
    };
    let key = muxus_vnc::VncServerKey {
        bits: 2048,
        fingerprint: fingerprint(0xa0),
        signature: "01-23-45-67-89-ab-cd-ef".into(),
    };
    let session_for = |port: u16| {
        let (sender, events) = async_channel::unbounded();
        let session = Session::new(
            sandbox.backend.database().clone(),
            sandbox.backend.vault().clone(),
            sandbox.backend.connections().clone(),
            sender,
            DesktopOptions::default(),
        );
        session.set_profile(
            serde_json::from_value(serde_json::json!({
                "kind": "vnc", "host": "VNC.lab", "port": port,
            }))
            .unwrap(),
        );
        (session, events)
    };
    let challenge_of = |events: &async_channel::Receiver<DesktopEvent>| match events.try_recv() {
        Ok(DesktopEvent::Certificate(challenge)) => challenge,
        other => panic!("expected a challenge, got {other:?}"),
    };
    async fn asked(events: &async_channel::Receiver<DesktopEvent>) {
        while events.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    let (first, events) = session_for(5900);
    let verdict = tokio::spawn({
        let (first, key) = (first.clone(), key.clone());
        async move { first.accept_server_key(key).await }
    });
    asked(&events).await;
    let challenge = challenge_of(&events);
    assert_eq!(challenge.host, "VNC.lab");
    assert_eq!(challenge.port, 5900);
    assert_eq!(challenge.state, TrustState::New);
    assert_eq!(
        challenge.identity,
        DesktopIdentity::RsaKey {
            bits: 2048,
            signature: key.signature.clone()
        }
    );
    first.answer(DesktopInput::CertificateResponse { accept: true });
    assert!(verdict.await.unwrap());
    let pinned = sandbox
        .backend
        .database()
        .trusted_desktop_identity("vnc.lab", 5900, "")
        .unwrap()
        .unwrap();
    assert_eq!(pinned.fingerprint, key.fingerprint);
    assert_eq!(pinned.subject, "RSA 2048-bit key 01-23-45-67-89-ab-cd-ef");

    // The pinned key passes without a prompt; a different one is a warning.
    let (second, events) = session_for(5900);
    assert!(second.accept_server_key(key.clone()).await);
    assert!(events.is_empty());
    let changed = muxus_vnc::VncServerKey {
        fingerprint: fingerprint(0xb0),
        ..key.clone()
    };
    let verdict = tokio::spawn({
        let second = second.clone();
        async move { second.accept_server_key(changed).await }
    });
    asked(&events).await;
    let challenge = challenge_of(&events);
    assert_eq!(challenge.state, TrustState::Mismatch);
    assert_eq!(
        challenge.previous.as_deref(),
        Some(key.fingerprint.as_str())
    );
    second.answer(DesktopInput::CertificateResponse { accept: false });
    assert!(!verdict.await.unwrap());
    assert_eq!(
        sandbox
            .backend
            .database()
            .trusted_desktop_identity("vnc.lab", 5900, "")
            .unwrap()
            .unwrap()
            .fingerprint,
        key.fingerprint
    );

    // Another port is another server.
    let (other, events) = session_for(5901);
    let verdict = tokio::spawn({
        let (other, key) = (other.clone(), key.clone());
        async move { other.accept_server_key(key).await }
    });
    asked(&events).await;
    assert_eq!(challenge_of(&events).state, TrustState::New);
    other.close();
    assert!(!verdict.await.unwrap(), "a withdrawn question is a no");
    sandbox.stop().await;
}

// --- SSH gateways ------------------------------------------------------------------------------

#[tokio::test]
async fn reaches_the_desktop_through_an_ssh_gateway() {
    use muxus_ssh::testing::server::{ServerSpec, start};
    let sshd = start(
        ServerSpec::new()
            .password("pw")
            .methods(&[russh::MethodKind::Password]),
    )
    .await;
    let desktop = RfbServer::start(RfbSecurity::None, 0).await;
    let sandbox = Sandbox::start().await;
    let port = format!("  Port {}", sshd.port());
    sandbox.write_ssh_config(&[
        "Host lab",
        "  HostName 127.0.0.1",
        "  User tester",
        port.as_str(),
    ]);
    let handle = sandbox.open(serde_json::json!({
        "kind": "vnc", "host": "127.0.0.1", "port": desktop.port,
        "sshGateway": { "target": "lab" },
    }));
    let mut events = Vec::new();
    loop {
        let event = next_question(&handle).await;
        match &event {
            DesktopEvent::HostKey(_) => handle.answer_host_key(true),
            DesktopEvent::AuthPrompt(_) => handle.answer_auth(answer("pw")),
            DesktopEvent::Connected(_) => break,
            DesktopEvent::Exit { message, .. } => panic!("ended: {message}; {events:?}"),
            _ => {}
        }
        events.push(event);
    }
    assert!(
        sshd.events()
            .contains(&format!("direct-tcpip 127.0.0.1:{}", desktop.port)),
        "{:?}",
        sshd.events()
    );
    sshd.disconnect_all().await;
    let (message, reason) = exit_of(&handle).await;
    assert_eq!(reason, ExitReason::Disconnected);
    assert!(
        message.starts_with("The SSH gateway lab disconnected"),
        "{message}"
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn never_dials_around_a_gateway_it_could_not_reach() {
    let desktop = RfbServer::start(RfbSecurity::None, 0).await;
    let sandbox = Sandbox::start().await;
    let port = format!("  Port {}", closed_port());
    sandbox.write_ssh_config(&["Host gw", "  HostName 127.0.0.1", port.as_str()]);
    let handle = sandbox.open(serde_json::json!({
        "kind": "vnc", "host": "127.0.0.1", "port": desktop.port,
        "sshGateway": { "target": "gw" },
    }));
    let (_, reason) = exit_of(&handle).await;
    assert_eq!(reason, ExitReason::Failed);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(desktop.connections(), 0);
    sandbox.stop().await;
}

#[tokio::test]
async fn dismissing_the_logon_prompt_closes_the_session() {
    let sandbox = Sandbox::start().await;
    let handle = sandbox.open(serde_json::json!({
        "kind": "rdp", "host": "127.0.0.1", "port": closed_port(),
    }));
    let DesktopEvent::AuthPrompt(prompt) = next_question(&handle).await else {
        panic!("expected the logon prompt");
    };
    // No user name on the host: it is asked for too.
    assert_eq!(prompt.prompts.len(), 2);
    assert_eq!(prompt.prompts[0].prompt, "User name");
    assert!(prompt.prompts[0].echo);
    handle.answer_auth(None);
    assert_eq!(
        exit_of(&handle).await,
        ("The connection closed.".to_owned(), ExitReason::Failed)
    );
    sandbox.stop().await;
}
