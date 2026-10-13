//! The client against an in-process RFB server: sign-in, the picture, input, clipboard,
//! resizes and the failures the user sees. Includes the VNC cases of
//! `tests/unit/server/desktop-socket.test.ts` that still apply natively.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_core::protocol::{DesktopCredentialType, DesktopCredentials};
use muxus_vnc::{
    BoxFuture, CursorShape, KeyEvent, Modifiers, MouseButton, PixelOrder, Platform, SessionEnd,
    VncConnectOptions, VncEvent, VncFailureKind, VncHandler, VncServerKey, VncSession,
    VncTransport, WheelDelta, WheelUnit, connect,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Handler {
    answers: Mutex<Vec<Option<DesktopCredentials>>>,
    asked: Mutex<Vec<Vec<DesktopCredentialType>>>,
    keys: Mutex<Vec<VncServerKey>>,
}

impl Handler {
    fn answering(answers: Vec<Option<DesktopCredentials>>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers),
            ..Self::default()
        })
    }
}

impl VncHandler for Handler {
    fn credentials(
        &self,
        types: Vec<DesktopCredentialType>,
    ) -> BoxFuture<'static, Option<DesktopCredentials>> {
        self.asked.lock().unwrap().push(types);
        let mut answers = self.answers.lock().unwrap();
        let answer = if answers.is_empty() {
            None
        } else {
            answers.remove(0)
        };
        Box::pin(async move { answer })
    }

    fn verify_server_key(&self, key: VncServerKey) -> BoxFuture<'static, bool> {
        self.keys.lock().unwrap().push(key);
        Box::pin(async { true })
    }
}

fn password(password: &str) -> Option<DesktopCredentials> {
    Some(DesktopCredentials {
        username: None,
        password: Some(password.into()),
        domain: None,
    })
}

async fn listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

async fn read_n(socket: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    tokio::time::timeout(TIMEOUT, socket.read_exact(&mut buf))
        .await
        .expect("client data in time")
        .expect("client data");
    buf
}

/// RFB 3.8 version exchange and the security type list.
async fn greet(socket: &mut TcpStream, types: &[u8]) -> u8 {
    socket.write_all(b"RFB 003.008\n").await.unwrap();
    assert_eq!(read_n(socket, 12).await, b"RFB 003.008\n");
    socket.write_all(&[types.len() as u8]).await.unwrap();
    socket.write_all(types).await.unwrap();
    read_n(socket, 1).await[0]
}

/// SecurityResult OK, ClientInit, ServerInit, then the client's setup messages.
async fn init(socket: &mut TcpStream, width: u16, height: u16, name: &str) -> Vec<i32> {
    socket.write_all(&0u32.to_be_bytes()).await.unwrap();
    assert_eq!(read_n(socket, 1).await, [1], "shared ClientInit");
    let mut server_init = width.to_be_bytes().to_vec();
    server_init.extend(height.to_be_bytes());
    // 32 bpp, depth 24, little endian, true colour, 255 max, shifts 16/8/0.
    server_init.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    server_init.extend((name.len() as u32).to_be_bytes());
    server_init.extend(name.as_bytes());
    socket.write_all(&server_init).await.unwrap();
    // SetPixelFormat: depth 24, red in the low byte.
    let format = read_n(socket, 20).await;
    assert_eq!(&format[..8], &[0, 0, 0, 0, 32, 24, 0, 1]);
    assert_eq!(&format[14..17], &[0, 8, 16]);
    // SetEncodings.
    let head = read_n(socket, 4).await;
    assert_eq!(head[0], 2);
    let count = usize::from(u16::from_be_bytes([head[2], head[3]]));
    let list = read_n(socket, count * 4).await;
    let encodings = list
        .chunks(4)
        .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    // The first, non-incremental update request for the whole screen.
    let mut request = vec![3, 0, 0, 0, 0, 0];
    request.extend(width.to_be_bytes());
    request.extend(height.to_be_bytes());
    assert_eq!(read_n(socket, 10).await, request);
    encodings
}

fn rect_header(x: u16, y: u16, width: u16, height: u16, encoding: i32) -> Vec<u8> {
    let mut header = Vec::new();
    for value in [x, y, width, height] {
        header.extend(value.to_be_bytes());
    }
    header.extend(encoding.to_be_bytes());
    header
}

fn update(rects: &[Vec<u8>]) -> Vec<u8> {
    let mut message = vec![0, 0];
    message.extend((rects.len() as u16).to_be_bytes());
    for rect in rects {
        message.extend(rect);
    }
    message
}

fn options(port: u16) -> VncConnectOptions {
    VncConnectOptions {
        platform: Platform::Other,
        ..VncConnectOptions::new("127.0.0.1", port)
    }
}

async fn next_event(session: &VncSession) -> VncEvent {
    tokio::time::timeout(TIMEOUT, session.events().recv())
        .await
        .expect("an event in time")
        .expect("the session is open")
}

async fn wait_for(session: &VncSession, matches: impl Fn(&VncEvent) -> bool) -> VncEvent {
    loop {
        let event = next_event(session).await;
        if matches(&event) {
            return event;
        }
        if let VncEvent::Ended(end) = &event {
            panic!("the session ended early: {end:?}");
        }
    }
}

async fn wait_for_pixel(framebuffer: &muxus_vnc::Framebuffer, x: u32, y: u32, rgba: [u8; 4]) {
    tokio::time::timeout(TIMEOUT, async {
        while framebuffer.lock().rgba_at(x, y) != Some(rgba) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("pixel ({x}, {y}) never became {rgba:?}"));
}

#[tokio::test]
async fn draws_and_sends_input_without_authentication() {
    let (listener, port) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert_eq!(greet(&mut socket, &[1]).await, 1);
        let encodings = init(&mut socket, 32, 16, "desk").await;
        assert_eq!(&encodings[..3], &[1, 7, -260]);
        // A raw 2x1 rect and a solid RRE rect.
        let mut raw = rect_header(1, 1, 2, 1, 0);
        raw.extend([10, 20, 30, 0, 40, 50, 60, 0]);
        let mut rre = rect_header(8, 8, 4, 4, 2);
        rre.extend(0u32.to_be_bytes());
        rre.extend([1, 2, 3, 0]);
        socket.write_all(&update(&[raw, rre])).await.unwrap();
        // The client asks for the next, incremental update.
        assert_eq!(
            read_n(&mut socket, 10).await,
            [3, 1, 0, 0, 0, 0, 0, 32, 0, 16]
        );
        // Keys (no QEMU extension: keysyms) and the pointer.
        let mut input = Vec::new();
        input.push(read_n(&mut socket, 8).await);
        input.push(read_n(&mut socket, 8).await);
        input.push(read_n(&mut socket, 6).await);
        input.push(read_n(&mut socket, 6).await);
        input.push(read_n(&mut socket, 6).await);
        input
    });

    let handler = Handler::answering(vec![]);
    let session = connect(options(port), VncTransport::Direct, handler.clone())
        .await
        .expect("connected");
    assert!(handler.asked.lock().unwrap().is_empty());
    assert_eq!(session.desktop_name(), "desk");
    assert_eq!(
        next_event(&session).await,
        VncEvent::DesktopName("desk".into())
    );
    assert_eq!(
        next_event(&session).await,
        VncEvent::Resized {
            width: 32,
            height: 16
        }
    );
    assert_eq!(next_event(&session).await, VncEvent::Frame);
    assert_eq!(
        next_event(&session).await,
        VncEvent::Cursor(CursorShape::Hidden)
    );
    let framebuffer = session.framebuffer();
    wait_for_pixel(&framebuffer, 2, 1, [40, 50, 60, 255]).await;
    wait_for_pixel(&framebuffer, 11, 11, [1, 2, 3, 255]).await;
    assert!(!framebuffer.take_dirty().is_empty());

    let key = |key: &str, pressed| KeyEvent {
        code: "KeyA".into(),
        key: key.into(),
        pressed,
        repeat: false,
        modifiers: Modifiers::default(),
    };
    assert!(session.key(&key("a", true)));
    assert!(session.key(&key("a", false)));
    session.pointer_button(3, 4, MouseButton::Left, true);
    session.pointer_button(3, 4, MouseButton::Left, false);
    tokio::time::sleep(Duration::from_millis(30)).await;
    session.pointer_move(5, 6);
    let input = tokio::time::timeout(TIMEOUT, server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input[0], [4, 1, 0, 0, 0, 0, 0, 0x61]);
    assert_eq!(input[1], [4, 0, 0, 0, 0, 0, 0, 0x61]);
    assert_eq!(input[2], [5, 1, 0, 3, 0, 4]);
    assert_eq!(input[3], [5, 0, 0, 3, 0, 4]);
    assert_eq!(input[4], [5, 0, 0, 5, 0, 6]);
}

/// The VNC authentication response the server expects for `password`.
fn expected_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    use des::cipher::{Block, BlockCipherEncrypt, KeyInit};
    let mut key = [0u8; 8];
    for (slot, byte) in key.iter_mut().zip(password.bytes()) {
        *slot = byte.reverse_bits();
    }
    let cipher = des::Des::new_from_slice(&key).unwrap();
    let mut out = *challenge;
    for chunk in out.chunks_mut(8) {
        let mut block = Block::<des::Des>::default();
        block.copy_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    out
}

/// A VncAuth server that accepts `secret`; returns the results it sent.
async fn vnc_auth_server(listener: TcpListener, secret: &'static str) -> bool {
    let (mut socket, _) = listener.accept().await.unwrap();
    assert_eq!(greet(&mut socket, &[2]).await, 2);
    let challenge = *b"Muxus challenge!";
    socket.write_all(&challenge).await.unwrap();
    let response = read_n(&mut socket, 16).await;
    if response == expected_response(secret, &challenge) {
        init(&mut socket, 8, 8, "secured").await;
        // Stay until the client hangs up.
        let _ = socket.read(&mut [0u8; 64]).await;
        true
    } else {
        socket.write_all(&1u32.to_be_bytes()).await.unwrap();
        let reason = b"Authentication failed";
        socket
            .write_all(&(reason.len() as u32).to_be_bytes())
            .await
            .unwrap();
        socket.write_all(reason).await.unwrap();
        false
    }
}

#[tokio::test]
async fn asks_for_vnc_credentials_when_the_server_wants_them() {
    let (listener, port) = listener().await;
    let server = tokio::spawn(vnc_auth_server(listener, "secret"));
    let handler = Handler::answering(vec![password("secret")]);
    let session = connect(options(port), VncTransport::Direct, handler.clone())
        .await
        .expect("signed in");
    assert_eq!(
        handler.asked.lock().unwrap().as_slice(),
        [vec![DesktopCredentialType::Password]]
    );
    session.disconnect();
    assert_eq!(
        wait_for(&session, |event| matches!(event, VncEvent::Ended(_))).await,
        VncEvent::Ended(SessionEnd::Closed)
    );
    assert!(
        tokio::time::timeout(TIMEOUT, server)
            .await
            .unwrap()
            .unwrap()
    );
}

#[tokio::test]
async fn reports_a_rejected_password_so_the_caller_asks_again() {
    let (listener, port) = listener().await;
    let server = tokio::spawn(vnc_auth_server(listener, "secret"));
    let failure = connect(
        options(port),
        VncTransport::Direct,
        Handler::answering(vec![password("wrong")]),
    )
    .await
    .unwrap_err();
    assert!(
        !tokio::time::timeout(TIMEOUT, server)
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(failure.kind, VncFailureKind::Credentials);
    assert_eq!(
        failure.message,
        "The server rejected the login: Authentication failed"
    );
    assert_eq!(
        failure.security_failure.as_deref(),
        Some("Authentication failed")
    );
}

#[tokio::test]
async fn cancels_when_the_user_does() {
    let (listener, port) = listener().await;
    tokio::spawn(vnc_auth_server(listener, "secret"));
    let failure = connect(
        options(port),
        VncTransport::Direct,
        Handler::answering(vec![None]),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.message, "Logon cancelled.");
}

#[tokio::test]
async fn reports_a_refusal_before_any_sign_in() {
    let (listener, port) = listener().await;
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(b"RFB 003.008\n").await.unwrap();
        read_n(&mut socket, 12).await;
        let reason = b"Too many security failures";
        socket.write_all(&[0]).await.unwrap();
        socket
            .write_all(&(reason.len() as u32).to_be_bytes())
            .await
            .unwrap();
        socket.write_all(reason).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    });
    let failure = connect(
        options(port),
        VncTransport::Direct,
        Handler::answering(vec![]),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.kind, VncFailureKind::Refused);
    assert_eq!(
        failure.message,
        "The VNC server refused the connection: Too many security failures"
    );
}

#[tokio::test]
async fn explains_servers_that_want_tls() {
    let (listener, port) = listener().await;
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(b"RFB 003.008\n").await.unwrap();
        read_n(&mut socket, 12).await;
        // Only TLS (18) and RA2 with session encryption (5).
        socket.write_all(&[2, 18, 5]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    });
    let failure = connect(
        options(port),
        VncTransport::Direct,
        Handler::answering(vec![]),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.kind, VncFailureKind::Failed);
    assert!(
        failure.message.contains("may require TLS encryption"),
        "{}",
        failure.message
    );
}

#[tokio::test]
async fn reports_a_refused_tcp_connection() {
    let (listener, port) = listener().await;
    drop(listener);
    let failure = connect(
        options(port),
        VncTransport::Direct,
        Handler::answering(vec![]),
    )
    .await
    .unwrap_err();
    assert_eq!(
        failure.message,
        "The remote computer refused the connection. Check the port and that the server runs."
    );
}

#[tokio::test]
async fn reaches_a_server_through_a_tunnel_with_vencrypt_plain() {
    let (listener, port) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert_eq!(greet(&mut socket, &[19]).await, 19);
        socket.write_all(&[0, 2]).await.unwrap();
        assert_eq!(read_n(&mut socket, 2).await, [0, 2]);
        // Ack, then TLSVnc (258) and Plain (256): only Plain works without TLS.
        socket.write_all(&[0, 2]).await.unwrap();
        socket.write_all(&258u32.to_be_bytes()).await.unwrap();
        socket.write_all(&256u32.to_be_bytes()).await.unwrap();
        assert_eq!(read_n(&mut socket, 4).await, 256u32.to_be_bytes());
        let lengths = read_n(&mut socket, 8).await;
        let user_len = u32::from_be_bytes(lengths[..4].try_into().unwrap()) as usize;
        let pass_len = u32::from_be_bytes(lengths[4..].try_into().unwrap()) as usize;
        let user = String::from_utf8(read_n(&mut socket, user_len).await).unwrap();
        let pass = String::from_utf8(read_n(&mut socket, pass_len).await).unwrap();
        init(&mut socket, 4, 4, "plain").await;
        (user, pass)
    });
    let handler = Handler::answering(vec![Some(DesktopCredentials {
        username: Some("ops".into()),
        password: Some("pässword".into()),
        domain: None,
    })]);
    let tunnel = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    connect(
        options(port),
        VncTransport::Tunnel(Box::new(tunnel)),
        handler.clone(),
    )
    .await
    .expect("signed in");
    let (user, pass) = tokio::time::timeout(TIMEOUT, server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((user.as_str(), pass.as_str()), ("ops", "pässword"));
    assert_eq!(
        handler.asked.lock().unwrap().as_slice(),
        [vec![
            DesktopCredentialType::Username,
            DesktopCredentialType::Password
        ]]
    );
}

#[tokio::test]
async fn shares_the_clipboard_and_follows_the_server_size() {
    let (listener, port) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        greet(&mut socket, &[1]).await;
        init(&mut socket, 64, 48, "desk").await;
        // Classic ServerCutText in ISO 8859-1.
        let text = b"caf\xe9";
        let mut cut = vec![3, 0, 0, 0];
        cut.extend((text.len() as u32).to_be_bytes());
        cut.extend(text);
        socket.write_all(&cut).await.unwrap();
        // The client offers its clipboard (no extended clipboard: Latin-1 ClientCutText).
        let head = read_n(&mut socket, 8).await;
        assert_eq!(&head[..4], &[6, 0, 0, 0]);
        let len = u32::from_be_bytes(head[4..].try_into().unwrap()) as usize;
        let offered = read_n(&mut socket, len).await;
        // ExtendedDesktopSize: the server supports resizing; the client asks for its pane.
        let mut size = rect_header(0, 0, 64, 48, -308);
        size.extend([1, 0, 0, 0]);
        size.extend(7u32.to_be_bytes());
        size.extend([0, 0, 0, 0, 0, 64, 0, 48]);
        size.extend(0u32.to_be_bytes());
        socket.write_all(&update(&[size])).await.unwrap();
        // The resize request goes out while the update is read, the next update request after.
        let request = read_n(&mut socket, 24).await;
        read_n(&mut socket, 10).await;
        assert_eq!(request[0], 251);
        let width = u16::from_be_bytes([request[2], request[3]]);
        let height = u16::from_be_bytes([request[4], request[5]]);
        assert_eq!(u32::from_be_bytes(request[8..12].try_into().unwrap()), 7);
        // Granted: this client asked (x = 1), status OK (y = 0).
        let mut granted = rect_header(1, 0, width, height, -308);
        granted.extend([1, 0, 0, 0]);
        granted.extend(7u32.to_be_bytes());
        granted.extend([0, 0, 0, 0]);
        granted.extend(width.to_be_bytes());
        granted.extend(height.to_be_bytes());
        granted.extend(0u32.to_be_bytes());
        // And a cursor: 2x1, left pixel visible.
        let mut cursor = rect_header(1, 0, 2, 1, -239);
        cursor.extend([255, 0, 0, 0, 0, 255, 0, 0, 0b1000_0000]);
        socket.write_all(&update(&[granted, cursor])).await.unwrap();
        read_n(&mut socket, 10).await;
        (offered, width, height)
    });
    let session = connect(
        VncConnectOptions {
            resize_remote: true,
            width: 100.7,
            height: 80.0,
            pixel_order: PixelOrder::Bgra,
            ..options(port)
        },
        VncTransport::Direct,
        Handler::answering(vec![]),
    )
    .await
    .unwrap();
    assert_eq!(
        wait_for(&session, |event| matches!(
            event,
            VncEvent::RemoteClipboard(_)
        ))
        .await,
        VncEvent::RemoteClipboard("café".into())
    );
    // The text the server just offered is not sent back; new text is.
    session.send_clipboard_text("café");
    session.send_clipboard_text("hi ✓");
    assert_eq!(
        wait_for(&session, |event| matches!(
            event,
            VncEvent::Resized { width: 100, .. }
        ))
        .await,
        VncEvent::Resized {
            width: 100,
            height: 80
        }
    );
    let cursor = wait_for(&session, |event| {
        matches!(event, VncEvent::Cursor(CursorShape::Image { .. }))
    })
    .await;
    assert_eq!(
        cursor,
        VncEvent::Cursor(CursorShape::Image {
            width: 2,
            height: 1,
            hotspot_x: 1,
            hotspot_y: 0,
            rgba: Arc::new(vec![255, 0, 0, 255, 0, 255, 0, 0]),
        })
    );
    let (offered, width, height) = tokio::time::timeout(TIMEOUT, server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(offered, b"hi ?");
    assert_eq!((width, height), (100, 80));
    assert_eq!(session.desktop_size(), (100, 80));
}

#[tokio::test]
async fn uses_qemu_key_events_extended_clipboard_fences_and_continuous_updates() {
    let (listener, port) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        greet(&mut socket, &[1]).await;
        init(&mut socket, 16, 16, "qemu").await;
        // QEMU extended key events and LED state (Caps Lock on), extended mouse buttons.
        let qemu = rect_header(0, 0, 0, 0, -258);
        let mut led = rect_header(0, 0, 0, 0, -261);
        led.push(4);
        let buttons = rect_header(0, 0, 0, 0, -316);
        socket
            .write_all(&update(&[qemu, led, buttons]))
            .await
            .unwrap();
        read_n(&mut socket, 10).await;
        // Extended clipboard capabilities: text, all actions.
        let mut caps = vec![3, 0, 0, 0];
        caps.extend((-8i32).to_be_bytes());
        caps.extend([0x1f, 0, 0, 1]);
        caps.extend(0u32.to_be_bytes());
        socket.write_all(&caps).await.unwrap();
        // The client answers with its own.
        let answer = read_n(&mut socket, 16).await;
        assert_eq!(&answer[8..12], &[0x1f, 0, 0, 1]);
        // EndOfContinuousUpdates: continuous updates get switched on.
        socket.write_all(&[150]).await.unwrap();
        assert_eq!(
            read_n(&mut socket, 10).await,
            [150, 1, 0, 0, 0, 0, 0, 16, 0, 16]
        );
        // A fence request is echoed.
        socket
            .write_all(&[248, 0, 0, 0, 0x80, 0, 0, 3, 2, b'o', b'k'])
            .await
            .unwrap();
        assert_eq!(
            read_n(&mut socket, 11).await,
            [248, 0, 0, 0, 0, 0, 0, 3, 2, b'o', b'k']
        );
        // Remote copy: Notify, the client Requests, the server Provides.
        let mut notify = vec![3, 0, 0, 0];
        notify.extend((-4i32).to_be_bytes());
        notify.extend([0x08, 0, 0, 1]);
        socket.write_all(&notify).await.unwrap();
        assert_eq!(
            read_n(&mut socket, 12).await,
            [6, 0, 0, 0, 0xff, 0xff, 0xff, 0xfc, 0x02, 0, 0, 1]
        );
        let mut plain = 11u32.to_be_bytes().to_vec();
        plain.extend(b"line\r\nnext\0");
        let mut compress = flate2::Compress::new(flate2::Compression::default(), true);
        let mut deflated = Vec::with_capacity(256);
        compress
            .compress_vec(&plain[..15], &mut deflated, flate2::FlushCompress::Sync)
            .unwrap();
        let mut provide = vec![3, 0, 0, 0];
        provide.extend((-(4 + deflated.len() as i32)).to_be_bytes());
        provide.extend([0x10, 0, 0, 1]);
        provide.extend(&deflated);
        socket.write_all(&provide).await.unwrap();
        // Keys now go as QEMU extended key events; Caps Lock is synced first.
        let mut keys = Vec::new();
        for _ in 0..4 {
            keys.push(read_n(&mut socket, 12).await);
        }
        // Local copy: Notify (with the back button right after it), then the server Requests
        // and the client Provides.
        assert_eq!(
            read_n(&mut socket, 12).await,
            [6, 0, 0, 0, 0xff, 0xff, 0xff, 0xfc, 0x08, 0, 0, 1]
        );
        // The back button goes out as an extended pointer event.
        let pointer = read_n(&mut socket, 7).await;
        let mut request = vec![3, 0, 0, 0];
        request.extend((-4i32).to_be_bytes());
        request.extend([0x02, 0, 0, 1]);
        socket.write_all(&request).await.unwrap();
        let head = read_n(&mut socket, 8).await;
        assert_eq!(&head[..4], &[6, 0, 0, 0]);
        let len = (-i32::from_be_bytes(head[4..].try_into().unwrap())) as usize;
        let provided = read_n(&mut socket, len).await;
        (keys, provided, pointer)
    });
    let session = connect(
        options(port),
        VncTransport::Direct,
        Handler::answering(vec![]),
    )
    .await
    .unwrap();
    assert_eq!(
        wait_for(&session, |event| matches!(
            event,
            VncEvent::RemoteClipboard(_)
        ))
        .await,
        VncEvent::RemoteClipboard("line\nnext".into())
    );
    let event = KeyEvent {
        code: "KeyB".into(),
        key: "b".into(),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::default(),
    };
    session.key(&event);
    session.key(&KeyEvent {
        pressed: false,
        ..event
    });
    session.send_clipboard_text("from muxus");
    session.pointer_button(1, 2, MouseButton::Back, true);
    let (keys, provided, pointer) = tokio::time::timeout(TIMEOUT, server)
        .await
        .unwrap()
        .unwrap();
    // Caps Lock (scancode 0x3a) press and release to match the local state, then B (0x30).
    assert_eq!(keys[0], [255, 0, 0, 1, 0, 0, 0xff, 0xe5, 0, 0, 0, 0x3a]);
    assert_eq!(keys[1], [255, 0, 0, 0, 0, 0, 0xff, 0xe5, 0, 0, 0, 0x3a]);
    assert_eq!(keys[2], [255, 0, 0, 1, 0, 0, 0, 0x62, 0, 0, 0, 0x30]);
    assert_eq!(keys[3], [255, 0, 0, 0, 0, 0, 0, 0x62, 0, 0, 0, 0x30]);
    assert_eq!(&provided[..4], &[0x10, 0, 0, 1]);
    let mut inflate = flate2::Decompress::new(true);
    let mut text = Vec::with_capacity(64);
    inflate
        .decompress_vec(&provided[4..], &mut text, flate2::FlushDecompress::Sync)
        .unwrap();
    assert_eq!(&text[4..], b"from muxus\0");
    assert_eq!(pointer, [5, 0x80, 0, 1, 0, 2, 1]);
}

#[tokio::test]
async fn ends_cleanly_or_lost_and_drops_input_in_view_only_sessions() {
    let (listener, port) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        greet(&mut socket, &[1]).await;
        init(&mut socket, 4, 4, "watch").await;
        // View-only: input never arrives; the next thing the client sends would be an update
        // request, which only comes after an update.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut probe = [0u8; 1];
        let pending =
            tokio::time::timeout(Duration::from_millis(100), socket.read(&mut probe)).await;
        assert!(pending.is_err(), "no input in view-only mode");
        // An unknown message type ends the session as lost.
        socket.write_all(&[77]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    });
    let session = connect(
        VncConnectOptions {
            view_only: true,
            ..options(port)
        },
        VncTransport::Direct,
        Handler::answering(vec![]),
    )
    .await
    .unwrap();
    assert!(!session.key(&KeyEvent::default()));
    session.pointer_button(1, 1, MouseButton::Left, true);
    session.wheel(
        1,
        1,
        WheelDelta {
            delta_x: 0.0,
            delta_y: 120.0,
            unit: WheelUnit::Pixel,
        },
    );
    session.send_ctrl_alt_del();
    session.send_clipboard_text("nope");
    let end = wait_for(&session, |event| matches!(event, VncEvent::Ended(_))).await;
    assert_eq!(
        end,
        VncEvent::Ended(SessionEnd::Lost {
            detail: "Unexpected server message (type 77)".into()
        })
    );
    tokio::time::timeout(TIMEOUT, server)
        .await
        .unwrap()
        .unwrap();
}
