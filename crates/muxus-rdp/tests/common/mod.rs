//! Shared pieces of the RDP integration tests: the fake server's certificate and a handler that
//! records what it was asked.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_rdp::{BoxFuture, PresentedCertificate, RdpHandler};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

/// Self-signed P-256 certificate for the fake RDP server (valid until 2126).
pub const TEST_CERTIFICATE: &str = "-----BEGIN CERTIFICATE-----
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
pub const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgUMEJjrQv5A6WS7Rw
q4UKUmJMYMcFCiMJ29wURxvZd/yhRANCAATBD9HwliQNcdN6TPbVyxUMK4UlYDQp
8jh2zR+EqMSy5j4F0aH2fHf03eg3fjfWzAwF9DXmNA3tu9PX11LIfNVR
-----END PRIVATE KEY-----";

pub fn certificate_der() -> CertificateDer<'static> {
    CertificateDer::from_pem_slice(TEST_CERTIFICATE.as_bytes()).expect("test certificate")
}

pub fn tls_acceptor() -> TlsAcceptor {
    let key = PrivateKeyDer::from_pem_slice(TEST_KEY.as_bytes()).expect("test key");
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocol versions")
    .with_no_client_auth()
    .with_single_cert(vec![certificate_der()], key)
    .expect("server config");
    TlsAcceptor::from(Arc::new(config))
}

/// Answers certificate prompts with `accept` after `delay`, remembering what it was shown.
pub struct RecordingHandler {
    pub accept: bool,
    pub delay: Duration,
    pub certificates: Mutex<Vec<PresentedCertificate>>,
    pub statuses: Mutex<Vec<String>>,
}

impl RecordingHandler {
    pub fn new(accept: bool) -> Arc<Self> {
        Self::delayed(accept, Duration::ZERO)
    }

    pub fn delayed(accept: bool, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            accept,
            delay,
            certificates: Mutex::new(Vec::new()),
            statuses: Mutex::new(Vec::new()),
        })
    }

    pub fn certificates(&self) -> Vec<PresentedCertificate> {
        self.certificates.lock().unwrap().clone()
    }
}

impl RdpHandler for RecordingHandler {
    fn accept_certificate(&self, certificate: PresentedCertificate) -> BoxFuture<'static, bool> {
        self.certificates.lock().unwrap().push(certificate);
        let (accept, delay) = (self.accept, self.delay);
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            accept
        })
    }

    fn status(&self, message: &str) {
        self.statuses.lock().unwrap().push(message.to_owned());
    }
}
