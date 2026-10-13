//! Port of `tests/unit/server/telnet-transport.test.ts`, against a loopback server.

use std::time::Duration;

use muxus_core::stream::{TerminalSize, TransportChannels, TransportEvent, TransportInput};
use muxus_telnet::codec::{
    BRK, DO, DONT, IAC, OPT_BINARY as BINARY, OPT_ECHO as ECHO, OPT_NAWS as NAWS,
    OPT_TERMINAL_TYPE as TTYPE, SB, SE, WILL, WONT,
};
use muxus_telnet::{TelnetCodec, TelnetProfile, connect};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

#[derive(Default)]
struct Harness {
    sent: Vec<Vec<u8>>,
    received: Vec<u8>,
}

impl Harness {
    fn feed(&mut self, codec: &mut TelnetCodec, bytes: &[u8]) {
        let fed = codec.feed(bytes);
        self.sent.extend(fed.replies);
        self.received.extend(fed.terminal);
    }
}

#[test]
fn parses_fragmented_negotiation_without_leaking_iac_bytes_to_the_terminal() {
    let mut codec = TelnetCodec::new(80, 24);
    let mut test = Harness::default();
    test.feed(&mut codec, &[b"hello".as_slice(), &[IAC, WILL]].concat());
    test.feed(&mut codec, &[[ECHO].as_slice(), b" world"].concat());
    assert_eq!(String::from_utf8(test.received).unwrap(), "hello world");
    assert_eq!(test.sent, vec![vec![IAC, DO, ECHO]]);
}

#[test]
fn answers_terminal_type_send_sub_negotiation_across_chunk_boundaries() {
    let mut codec = TelnetCodec::new(80, 24);
    let mut test = Harness::default();
    test.feed(&mut codec, &[IAC, DO, TTYPE]);
    test.feed(&mut codec, &[IAC, SB, TTYPE]);
    test.feed(&mut codec, &[1, IAC]);
    test.feed(&mut codec, &[SE]);
    assert_eq!(test.sent[0], vec![IAC, WILL, TTYPE]);
    assert_eq!(
        test.sent[1],
        [
            [IAC, SB, TTYPE, 0].as_slice(),
            b"xterm-256color",
            &[IAC, SE]
        ]
        .concat()
    );
}

#[test]
fn advertises_and_updates_the_network_window_size() {
    let mut codec = TelnetCodec::new(132, 43);
    let mut test = Harness::default();
    test.feed(&mut codec, &[IAC, DO, NAWS]);
    assert_eq!(
        test.sent,
        vec![
            vec![IAC, WILL, NAWS],
            vec![IAC, SB, NAWS, 0, 132, 0, 43, IAC, SE]
        ]
    );
    assert_eq!(
        codec.resize(200, 50),
        Some(vec![IAC, SB, NAWS, 0, 200, 0, 50, IAC, SE])
    );
    // A size byte equal to IAC is doubled.
    assert_eq!(
        codec.resize(255, 1),
        Some(vec![IAC, SB, NAWS, 0, 255, 255, 0, 1, IAC, SE])
    );
}

#[test]
fn sends_no_window_size_before_the_server_asks_for_it() {
    let mut codec = TelnetCodec::new(80, 24);
    assert_eq!(codec.resize(100, 30), None);
}

#[test]
fn refuses_unsupported_local_and_remote_options() {
    let mut codec = TelnetCodec::new(80, 24);
    let mut test = Harness::default();
    test.feed(&mut codec, &[IAC, DO, 39, IAC, WILL, 34]);
    assert_eq!(test.sent, vec![vec![IAC, WONT, 39], vec![IAC, DONT, 34]]);
}

#[test]
fn acknowledges_options_switched_off() {
    let mut codec = TelnetCodec::new(80, 24);
    let mut test = Harness::default();
    test.feed(
        &mut codec,
        &[
            IAC, WILL, ECHO, IAC, WILL, ECHO, IAC, WONT, ECHO, IAC, WONT, ECHO,
        ],
    );
    test.feed(
        &mut codec,
        &[IAC, DO, BINARY, IAC, DONT, BINARY, IAC, DONT, BINARY],
    );
    assert_eq!(
        test.sent,
        vec![
            vec![IAC, DO, ECHO],
            vec![IAC, DONT, ECHO],
            vec![IAC, WILL, BINARY],
            vec![IAC, WONT, BINARY],
        ]
    );
}

#[test]
fn escapes_iac_and_translates_terminal_newlines_until_binary_mode_is_active() {
    let mut codec = TelnetCodec::new(80, 24);
    assert_eq!(codec.encode(&[65, 13, 255]), vec![65, 13, 10, 255, 255]);
    assert_eq!(codec.encode(b"a\nb\r\n"), b"a\r\nb\r\n".to_vec());
    codec.feed(&[IAC, DO, BINARY]);
    assert_eq!(codec.encode(&[13, 255]), vec![13, 255, 255]);
}

#[test]
fn consumes_a_break_from_the_far_end_without_passing_it_to_the_terminal() {
    let mut codec = TelnetCodec::new(80, 24);
    let mut test = Harness::default();
    test.feed(&mut codec, &[65, IAC]);
    test.feed(&mut codec, &[BRK, 66]);
    assert_eq!(String::from_utf8(test.received).unwrap(), "AB");
    assert!(test.sent.is_empty());
}

#[test]
fn decodes_nvt_cr_nul_while_preserving_cr_lf() {
    let mut codec = TelnetCodec::new(80, 24);
    let mut test = Harness::default();
    test.feed(&mut codec, &[65, 13]);
    test.feed(&mut codec, &[0, 66, 13, 10, 67]);
    assert_eq!(test.received, vec![65, 13, 66, 13, 10, 67]);
    // A CR at the very end is terminal data once the connection closes.
    test.feed(&mut codec, &[13]);
    assert_eq!(codec.flush(), Some(vec![13]));
    assert_eq!(codec.flush(), None);
}

fn size() -> TerminalSize {
    TerminalSize {
        cols: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    }
}

async fn listener() -> (TcpListener, TelnetProfile) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (
        listener,
        TelnetProfile {
            profile_id: None,
            host: "127.0.0.1".into(),
            port,
        },
    )
}

/// Read from the server side until `pattern` has arrived.
async fn read_until(socket: &mut TcpStream, wire: &mut Vec<u8>, pattern: &[u8]) {
    let mut buffer = [0u8; 4096];
    while !wire.windows(pattern.len()).any(|window| window == pattern) {
        let read = timeout(Duration::from_secs(5), socket.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert!(read > 0, "client closed before {pattern:?} arrived");
        wire.extend_from_slice(&buffer[..read]);
    }
}

/// Terminal output until `pattern` has arrived.
async fn output_until(channels: &TransportChannels, pattern: &str) -> String {
    let mut output = Vec::new();
    while !String::from_utf8_lossy(&output).contains(pattern) {
        match timeout(Duration::from_secs(5), channels.events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            TransportEvent::Data(data) => output.extend(data),
            other => panic!("unexpected {other:?}"),
        }
    }
    String::from_utf8(output).unwrap()
}

#[tokio::test]
async fn preserves_an_immediate_server_banner_and_carries_terminal_input_over_tcp() {
    let (listener, settings) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket
            .write_all(&[[IAC, WILL, ECHO].as_slice(), b"login:"].concat())
            .await
            .unwrap();
        let mut wire = Vec::new();
        read_until(&mut socket, &mut wire, b"admin\r\n").await;
        wire
    });
    let channels = connect(&settings, size()).await.unwrap();
    // Nothing reads the events until the banner is already there.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(output_until(&channels, "login:").await.contains("login:"));

    channels
        .input
        .send(TransportInput::Data(b"admin\r".to_vec()))
        .await
        .unwrap();
    let wire = server.await.unwrap();
    assert!(wire.windows(3).any(|window| window == [IAC, DO, ECHO]));
    assert!(wire.windows(7).any(|window| window == b"admin\r\n"));
    channels.input.send(TransportInput::Close).await.unwrap();
}

#[tokio::test]
async fn sends_break_as_a_bare_iac_brk_while_input_keeps_its_iac_escaping() {
    let (listener, settings) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut wire = Vec::new();
        read_until(&mut socket, &mut wire, b"after").await;
        // The terminal closing the session closes the socket.
        let mut rest = [0u8; 16];
        let read = timeout(Duration::from_secs(5), socket.read(&mut rest))
            .await
            .unwrap()
            .unwrap();
        (wire, read)
    });
    let channels = connect(&settings, size()).await.unwrap();
    channels
        .input
        .send(TransportInput::Data(vec![0x62, 0x65, 0x66, IAC]))
        .await
        .unwrap();
    channels
        .input
        .send(TransportInput::Break { duration_ms: 250 })
        .await
        .unwrap();
    channels
        .input
        .send(TransportInput::Data(b"after".to_vec()))
        .await
        .unwrap();
    channels.input.send(TransportInput::Close).await.unwrap();
    let (wire, read_after_close) = server.await.unwrap();
    assert_eq!(
        wire,
        [[0x62, 0x65, 0x66, IAC, IAC, IAC, BRK].as_slice(), b"after"].concat()
    );
    assert_eq!(read_after_close, 0);
    // No events follow a close from the terminal side.
    assert!(
        timeout(Duration::from_secs(5), channels.events.recv())
            .await
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn negotiates_window_size_and_follows_resizes() {
    let (listener, settings) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(&[IAC, DO, NAWS]).await.unwrap();
        let mut wire = Vec::new();
        read_until(
            &mut socket,
            &mut wire,
            &[IAC, SB, NAWS, 0, 120, 0, 40, IAC, SE],
        )
        .await;
        wire
    });
    let channels = connect(&settings, size()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    channels
        .input
        .send(TransportInput::Resize(TerminalSize {
            cols: 120,
            rows: 40,
            ..size()
        }))
        .await
        .unwrap();
    let wire = server.await.unwrap();
    assert!(wire.starts_with(&[IAC, WILL, NAWS, IAC, SB, NAWS, 0, 80, 0, 24, IAC, SE]));
}

#[tokio::test]
async fn reports_the_server_closing_the_connection_as_an_exit() {
    let (listener, settings) = listener().await;
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(b"bye\r").await.unwrap();
    });
    let channels = connect(&settings, size()).await.unwrap();
    let mut output = Vec::new();
    loop {
        match timeout(Duration::from_secs(5), channels.events.recv())
            .await
            .unwrap()
        {
            Ok(TransportEvent::Data(data)) => output.extend(data),
            Ok(TransportEvent::Exit { code, signal }) => {
                assert_eq!((code, signal), (None, None));
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    // The CR held back for a possible NUL is delivered at the end.
    assert_eq!(output, b"bye\r");
    assert!(channels.events.recv().await.is_err());
}

#[tokio::test]
async fn words_connection_failures_for_the_user() {
    let (listener, settings) = listener().await;
    drop(listener);
    let refused = connect(&settings, size()).await.unwrap_err();
    assert_eq!(
        refused.to_string(),
        format!("Telnet connection refused by 127.0.0.1:{}", settings.port)
    );

    let missing = TelnetProfile {
        profile_id: None,
        host: "no-such-host.invalid".into(),
        port: 23,
    };
    let not_found = connect(&missing, size()).await.unwrap_err();
    assert_eq!(
        not_found.to_string(),
        "Telnet host not found: no-such-host.invalid"
    );
}
