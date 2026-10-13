//! IronRDP's own server, in process, showing the invented desktop: TLS with a throwaway
//! self-signed certificate, the logon `demo` / `demo`.

use std::num::{NonZeroU16, NonZeroUsize};
use std::sync::Arc;

use ironrdp_server::{
    BitmapUpdate, Credentials, DesktopSize, DisplayUpdate, KeyboardEvent, MouseEvent, PixelFormat,
    RdpServer, RdpServerDisplay, RdpServerDisplayUpdates, RdpServerInputHandler, ServerResult,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::demo_desktop::Desktop;

pub const USER: &str = "demo";
pub const PASSWORD: &str = "demo";

/// A self-signed P-256 certificate for `rdp.muxus.test`, made for tests (valid until 2126).
const CERTIFICATE: &str = "-----BEGIN CERTIFICATE-----
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
const KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgUMEJjrQv5A6WS7Rw
q4UKUmJMYMcFCiMJ29wURxvZd/yhRANCAATBD9HwliQNcdN6TPbVyxUMK4UlYDQp
8jh2zR+EqMSy5j4F0aH2fHf03eg3fjfWzAwF9DXmNA3tu9PX11LIfNVR
-----END PRIVATE KEY-----";

fn tls_acceptor() -> TlsAcceptor {
    let certificate = CertificateDer::from_pem_slice(CERTIFICATE.as_bytes()).unwrap();
    let key = PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![certificate], key)
    .unwrap();
    TlsAcceptor::from(Arc::new(config))
}

struct Input;

impl RdpServerInputHandler for Input {
    fn keyboard(&mut self, event: KeyboardEvent) {
        eprintln!("rdp server: {event:?}");
    }

    fn mouse(&mut self, _: MouseEvent) {}
}

struct Display(Arc<Desktop>);

struct Updates {
    desktop: Arc<Desktop>,
    sent: bool,
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Updates {
    async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
        if std::mem::replace(&mut self.sent, true) {
            return std::future::pending().await;
        }
        let desktop = &self.desktop;
        Ok(Some(DisplayUpdate::Bitmap(BitmapUpdate {
            x: 0,
            y: 0,
            width: NonZeroU16::new(desktop.width as u16).unwrap(),
            height: NonZeroU16::new(desktop.height as u16).unwrap(),
            format: PixelFormat::BgrA32,
            data: bytes::Bytes::from(desktop.bgra()),
            stride: NonZeroUsize::new(desktop.width as usize * 4).unwrap(),
        })))
    }
}

#[async_trait::async_trait]
impl RdpServerDisplay for Display {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: self.0.width as u16,
            height: self.0.height as u16,
        }
    }

    async fn updates(&mut self) -> ServerResult<Box<dyn RdpServerDisplayUpdates>> {
        Ok(Box::new(Updates {
            desktop: self.0.clone(),
            sent: false,
        }))
    }
}

/// Serve on `listener` from a thread of its own (IronRDP's server future is not `Send`).
pub fn serve(listener: std::net::TcpListener, desktop: Arc<Desktop>) {
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let mut server = RdpServer::builder()
                .with_addr(([127, 0, 0, 1], port))
                .with_tls(tls_acceptor())
                .with_input_handler(Input)
                .with_display_handler(Display(desktop))
                .build();
            server.set_credentials(Some(Credentials {
                username: USER.into(),
                password: PASSWORD.into(),
                domain: None,
            }));
            let listener = TcpListener::from_std(listener).unwrap();
            while let Ok((stream, _)) = listener.accept().await {
                let _ = server.run_connection(stream).await;
            }
        });
    });
}
