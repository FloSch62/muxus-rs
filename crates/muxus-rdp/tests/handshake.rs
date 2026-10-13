//! The connection front half against a fake server: X.224 negotiation, the TLS upgrade, the
//! certificate prompt and the failures the user sees. Port of the RDCleanPath cases in
//! `tests/unit/server/desktop-socket.test.ts`, which tested the same steps in the proxy.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{RecordingHandler, certificate_der, tls_acceptor};
use muxus_rdp::certificates::certificate_fingerprint;
use muxus_rdp::{RdpConnectOptions, RdpFailureKind, RdpTransport, connect};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// An X.224 Connection Confirm selecting `protocol`.
fn confirm_for(protocol: u8) -> Vec<u8> {
    hex(&format!("030000130ed00000123400020108000{protocol}000000"))
}

/// RDP_NEG_FAILURE with HYBRID_REQUIRED_BY_SERVER.
fn refusal() -> Vec<u8> {
    hex("030000130ed000000000000300080005000000")
}

#[derive(Clone, Copy, PartialEq)]
enum AfterTls {
    /// Read whatever the client sends next, then close.
    Close,
    /// Reset the connection shortly after the handshake.
    Reset,
}

/// An RDP server's front half: X.224 confirm, then TLS, then `after`.
async fn fake_rdp_server(confirm: Vec<u8>, after: AfterTls) -> (u16, Arc<Mutex<Vec<Vec<u8>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let recorded = recorded.clone();
            let confirm = confirm.clone();
            tokio::spawn(async move {
                let mut header = [0u8; 4];
                if socket.read_exact(&mut header).await.is_err() {
                    return;
                }
                let length = usize::from(u16::from_be_bytes([header[2], header[3]]));
                let mut request = header.to_vec();
                request.resize(length, 0);
                if socket.read_exact(&mut request[4..]).await.is_err() {
                    return;
                }
                recorded.lock().unwrap().push(request);
                socket.write_all(&confirm).await.unwrap();
                if confirm == refusal() {
                    let _ = socket.read(&mut [0u8; 16]).await;
                    return;
                }
                if after == AfterTls::Reset {
                    let mut tls = match tls_acceptor().accept(socket).await {
                        Ok(tls) => tls,
                        Err(_) => return,
                    };
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let _ = tls.get_mut().0.set_zero_linger();
                    drop(tls);
                    return;
                }
                let Ok(mut tls) = tls_acceptor().accept(socket).await else {
                    return;
                };
                let _ = tls.read(&mut [0u8; 4096]).await;
            });
        }
    });
    (port, requests)
}

fn options(port: u16) -> RdpConnectOptions {
    RdpConnectOptions {
        username: "alice".into(),
        password: "hunter2".into(),
        ..RdpConnectOptions::new("127.0.0.1", port)
    }
}

#[tokio::test]
async fn shows_the_certificate_before_any_credentials_and_honours_a_refusal() {
    let (port, requests) = fake_rdp_server(confirm_for(1), AfterTls::Close).await;
    let handler = RecordingHandler::new(false);
    let failure = connect(options(port), RdpTransport::Direct, handler.clone())
        .await
        .unwrap_err();
    assert_eq!(failure.kind, RdpFailureKind::Refused);
    assert_eq!(failure.message, "The server certificate was not trusted.");

    let certificates = handler.certificates();
    assert_eq!(certificates.len(), 1);
    let certificate = &certificates[0];
    assert_eq!(certificate.subject, "CN=rdp.muxus.test");
    assert_eq!(
        certificate.fingerprint,
        certificate_fingerprint(certificate_der().as_ref())
    );
    assert_eq!(
        certificate.verification_error.as_deref(),
        Some("DEPTH_ZERO_SELF_SIGNED_CERT")
    );

    // The X.224 Connection Request asks for SSL | HYBRID | HYBRID_EX.
    let request = requests.lock().unwrap()[0].clone();
    assert_eq!(request[0], 3);
    assert_eq!(&request[request.len() - 8..], &hex("010008000b000000")[..]);
    assert_eq!(
        handler.statuses.lock().unwrap().as_slice(),
        [format!("Connecting to 127.0.0.1:{port} …")]
    );
}

#[tokio::test]
async fn reaches_the_server_through_a_tunnel() {
    let (port, requests) = fake_rdp_server(confirm_for(1), AfterTls::Close).await;
    let handler = RecordingHandler::new(false);
    let tunnel = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let failure = connect(
        options(port),
        RdpTransport::Tunnel(Box::new(tunnel)),
        handler.clone(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.message, "The server certificate was not trusted.");
    assert_eq!(handler.certificates().len(), 1);
    assert_eq!(requests.lock().unwrap().len(), 1);
    // A tunnel is already open: nothing is dialled, so no status line.
    assert!(handler.statuses.lock().unwrap().is_empty());
}

#[tokio::test]
async fn survives_the_server_dropping_the_connection_while_the_certificate_prompt_is_open() {
    let (port, _) = fake_rdp_server(confirm_for(1), AfterTls::Reset).await;
    // The reset lands while the prompt is still open; answer afterwards.
    let handler = RecordingHandler::delayed(true, Duration::from_millis(150));
    let failure = connect(options(port), RdpTransport::Direct, handler.clone())
        .await
        .unwrap_err();
    assert_eq!(handler.certificates().len(), 1);
    assert_eq!(failure.kind, RdpFailureKind::Failed);
    assert_eq!(
        failure.message,
        "The remote computer closed the connection."
    );
}

#[tokio::test]
async fn passes_a_server_refusal_back_as_a_negotiation_error() {
    let (port, _) = fake_rdp_server(refusal(), AfterTls::Close).await;
    let handler = RecordingHandler::new(true);
    let failure = connect(options(port), RdpTransport::Direct, handler.clone())
        .await
        .unwrap_err();
    assert_eq!(failure.kind, RdpFailureKind::Refused);
    assert_eq!(
        failure.message,
        "The server requires Network Level Authentication, which it could not complete."
    );
    assert!(handler.certificates().is_empty());
}

#[tokio::test]
async fn reports_a_refused_tcp_connection_with_its_socket_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let failure = connect(
        options(port),
        RdpTransport::Direct,
        RecordingHandler::new(true),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.kind, RdpFailureKind::Failed);
    assert_eq!(
        failure.message,
        "The remote computer refused the connection. Is Remote Desktop enabled, and is the port right?"
    );
}

#[tokio::test]
async fn reports_a_host_name_that_does_not_resolve() {
    let options = RdpConnectOptions::new("muxus-test.invalid", 3389);
    let failure = connect(options, RdpTransport::Direct, RecordingHandler::new(true))
        .await
        .unwrap_err();
    assert_eq!(failure.message, "The host name could not be resolved.");
}

#[tokio::test]
async fn explains_a_server_that_only_speaks_legacy_rdp_security() {
    // A confirm without negotiation data: standard RDP security only.
    let (port, _) = fake_rdp_server(hex("0300000b06d00000123400"), AfterTls::Close).await;
    let failure = connect(
        options(port),
        RdpTransport::Direct,
        RecordingHandler::new(true),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.kind, RdpFailureKind::Refused);
    assert!(
        failure.message.contains("legacy RDP security layer"),
        "{}",
        failure.message
    );
}
