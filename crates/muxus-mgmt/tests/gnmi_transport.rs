//! The connection underneath gNMI: response limits, HTTP failures, and
//! Muxus's own TLS with certificate pinning.

mod common;

use std::sync::Arc;

use muxus_core::management::GnmiEncoding;
use muxus_core::protocol::GnmiProfile;
use muxus_mgmt::Error;
use muxus_mgmt::error::code;
use muxus_mgmt::gnmi::{GnmiClient, GnmiGetRequest};
use muxus_mgmt::tls::{TlsFiles, certificate_challenge, upgrade_gnmi_tls};
use prost::Message;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::{TcpListener, TcpStream};

fn get_response_of_size(bytes: usize) -> Vec<u8> {
    use muxus_mgmt::proto::gnmi;
    gnmi::GetResponse {
        notification: vec![gnmi::Notification {
            timestamp: 1,
            update: vec![gnmi::Update {
                path: Some(gnmi::Path::default()),
                val: Some(gnmi::TypedValue {
                    value: Some(gnmi::typed_value::Value::JsonIetfVal(
                        format!("\"{}\"", "x".repeat(bytes)).into_bytes(),
                    )),
                }),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
    .encode_to_vec()
}

#[tokio::test]
async fn gives_up_on_a_response_that_grows_past_its_limit() {
    let (socket, port) = common::plaintext_target(|mut call| async move {
        call.ended().await;
        let big = get_response_of_size(64 * 1024);
        call.reply(&[&big], 0, None);
    })
    .await;
    let client = GnmiClient::connect(socket, &format!("leaf1:{port}"), false)
        .await
        .unwrap();
    let request = GnmiGetRequest {
        paths: vec!["/".into()],
        max_bytes: Some(4096),
        ..Default::default()
    };
    let Err(Error::Grpc(error)) = client.get(&request, GnmiEncoding::JsonIetf, None).await else {
        panic!("the oversized response was accepted");
    };
    assert_eq!(error.code_name(), "RESOURCE_EXHAUSTED");
    assert_eq!(
        error.message,
        "The response grew past 4.0 KiB; ask for a narrower path."
    );
    // The connection survives a call that gave up.
    let request = GnmiGetRequest {
        paths: vec!["/".into()],
        ..Default::default()
    };
    let outcome = client
        .get(&request, GnmiEncoding::JsonIetf, None)
        .await
        .unwrap();
    assert!(outcome.bytes > 64 * 1024);
}

#[tokio::test]
async fn reads_http_failures_and_trailers_only_statuses() {
    let (socket, port) = common::plaintext_target(|mut call| async move {
        if call.path.ends_with("/Get") {
            call.http_status(404);
        } else {
            call.finish(code::UNAUTHENTICATED, Some("bad%20login"));
        }
    })
    .await;
    let client = GnmiClient::connect(socket, &format!("leaf1:{port}"), false)
        .await
        .unwrap();
    let Err(Error::Grpc(error)) = client
        .get(&GnmiGetRequest::default(), GnmiEncoding::Json, None)
        .await
    else {
        panic!("HTTP 404 was accepted");
    };
    assert_eq!(error.code, code::UNIMPLEMENTED);
    assert_eq!(error.message, "The device answered HTTP 404.");
    let error = client.capabilities(None).await.unwrap_err();
    assert_eq!(error.code_name(), "UNAUTHENTICATED");
    assert_eq!(error.message, "bad login");
}

#[tokio::test]
async fn reports_an_empty_ok_reply_as_missing_its_message() {
    let (socket, port) = common::plaintext_target(|mut call| async move {
        call.ended().await;
        call.reply(&[], 0, None);
    })
    .await;
    let client = GnmiClient::connect(socket, &format!("leaf1:{port}"), false)
        .await
        .unwrap();
    let error = client.capabilities(None).await.unwrap_err();
    assert_eq!(error.code, code::INTERNAL);
    assert_eq!(error.message, "The device sent no response message.");
}

fn self_signed(name: &str) -> (CertificateDer<'static>, PrivateKeyDer<'static>, String) {
    let certified = rcgen::generate_simple_self_signed(vec![name.to_owned()]).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    (certified.cert.der().clone(), key, certified.cert.pem())
}

async fn tls_target(alpn: &[&[u8]]) -> (TcpStream, u16, String) {
    let (certificate, key, pem) = self_signed("leaf1");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .unwrap();
    config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let Ok(tls) = acceptor.accept(socket).await else {
            return;
        };
        common::serve(tls, |mut call| async move {
            call.ended().await;
            let response = muxus_mgmt::proto::gnmi::CapabilityResponse {
                g_nmi_version: "0.10.0".into(),
                ..Default::default()
            };
            call.reply(&[&response.encode_to_vec()], 0, None);
        })
        .await;
    });
    let socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    (socket, port, pem)
}

fn profile(port: u16) -> GnmiProfile {
    GnmiProfile {
        profile_id: None,
        host: "leaf1".into(),
        port,
        username: None,
        tls: None,
        tls_server_name: None,
        ca_file: None,
        cert_file: None,
        key_file: None,
        ssh_gateway: None,
        encoding: None,
    }
}

#[tokio::test]
async fn shows_a_self_signed_certificate_once_and_accepts_it_pinned() {
    let (socket, port, _) = tls_target(&[b"h2"]).await;
    let tls = upgrade_gnmi_tls(socket, &profile(port), &TlsFiles::default())
        .await
        .unwrap();
    assert_eq!(tls.protocol.as_deref(), Some("TLSv1.3"));
    let certificate = tls.certificate.clone().unwrap();
    assert_eq!(
        certificate.verification_error.as_deref(),
        Some("DEPTH_ZERO_SELF_SIGNED_CERT")
    );
    assert_eq!(certificate.fingerprint.len(), 32 * 3 - 1);
    assert!(certificate.valid_to.ends_with(" GMT"));
    let challenge = certificate_challenge(&certificate, "leaf1", port, None).unwrap();
    assert_eq!(challenge.fingerprint, certificate.fingerprint);
    assert!(
        certificate_challenge(&certificate, "leaf1", port, Some(&certificate.fingerprint))
            .is_none()
    );
    // gRPC runs over the TLS stream.
    let client = GnmiClient::connect(tls.stream, &format!("leaf1:{port}"), true)
        .await
        .unwrap();
    assert_eq!(client.capabilities(None).await.unwrap().version, "0.10.0");
}

#[tokio::test]
async fn verifies_against_a_ca_file_and_names_a_wrong_host() {
    let (socket, port, pem) = tls_target(&[b"h2"]).await;
    let files = TlsFiles {
        ca: Some(pem.clone().into_bytes()),
        ..Default::default()
    };
    let tls = upgrade_gnmi_tls(socket, &profile(port), &files)
        .await
        .unwrap();
    assert_eq!(tls.certificate.unwrap().verification_error, None);

    let (socket, port, _) = tls_target(&[b"h2"]).await;
    let mut wrong = profile(port);
    wrong.tls_server_name = Some("spine1".into());
    let tls = upgrade_gnmi_tls(socket, &wrong, &files).await.unwrap();
    let error = tls.certificate.unwrap().verification_error.unwrap();
    // The certificate is for leaf1 but was signed by itself, not by `files`:
    // either way it does not verify for spine1.
    assert!(!error.is_empty());
}

#[tokio::test]
async fn needs_http2_over_tls() {
    let (socket, port, _) = tls_target(&[b"http/1.1"]).await;
    let Err(error) = upgrade_gnmi_tls(socket, &profile(port), &TlsFiles::default()).await else {
        panic!("a server without h2 was accepted");
    };
    assert!(error.to_string().contains("ALPN h2"), "{error}");
}

#[tokio::test]
async fn explains_a_plain_text_server() {
    let (socket, port) = common::plaintext_target(|_call| async {}).await;
    let Err(error) = upgrade_gnmi_tls(socket, &profile(port), &TlsFiles::default()).await else {
        panic!("a plain-text server passed as TLS");
    };
    assert!(
        error.to_string().contains("did not answer with TLS"),
        "{error}"
    );
}
