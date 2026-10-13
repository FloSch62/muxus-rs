//! The updater's HTTP layer: a small trait so tests never reach the network, and the real
//! client (hyper over rustls with the webpki roots, redirects followed, as electron-updater's
//! `net` requests did).
//!
//! Plain `http://` is refused except on loopback: the feed's checksums are what make a
//! download trustworthy, so the feed itself must not travel in the clear.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Bytes;
use tokio::io::AsyncWriteExt;

const MAX_REDIRECTS: usize = 10;
/// Feeds and release metadata are small; anything bigger is not a feed.
pub const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
/// How long a request may take to start answering.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a download may stall between two chunks.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// The server answered with this status.
    Status(u16),
    /// Connecting, TLS, a timeout or a broken transfer.
    Network(String),
    /// The URL is not one the updater may use.
    Refused(String),
    /// Writing the download failed.
    Io(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Status(status) => write!(f, "the server answered {status}"),
            HttpError::Network(message) => write!(f, "network error: {message}"),
            HttpError::Refused(message) => write!(f, "refused: {message}"),
            HttpError::Io(message) => write!(f, "could not write the download: {message}"),
        }
    }
}

impl std::error::Error for HttpError {}

/// Download progress: bytes so far and the total when the server said.
pub type Progress<'a> = &'a (dyn Fn(u64, Option<u64>) + Send + Sync);

/// What the updater asks of the network.
#[async_trait]
pub trait UpdateHttp: Send + Sync {
    /// GET a small document (`limit` bytes at most); non-2xx answers are [`HttpError::Status`].
    async fn get(&self, url: &str, accept: &str, limit: usize) -> Result<Vec<u8>, HttpError>;

    /// GET `url` into `file` (created or truncated), reporting progress.
    async fn download(
        &self,
        url: &str,
        file: &Path,
        progress: Progress<'_>,
    ) -> Result<(), HttpError>;
}

/// The real client.
#[derive(Clone)]
pub struct HyperHttp {
    user_agent: String,
    tls: Arc<rustls::ClientConfig>,
}

impl HyperHttp {
    /// `version` goes into the User-Agent (`Muxus/<version>`).
    pub fn new(version: &str) -> Self {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Self {
            user_agent: format!("Muxus/{version}"),
            tls: Arc::new(tls),
        }
    }

    /// Follow redirects to the final answer; its status, headers and body.
    async fn open(
        &self,
        url: &str,
        accept: &str,
    ) -> Result<hyper::Response<hyper::body::Incoming>, HttpError> {
        let mut uri: hyper::Uri = url
            .parse()
            .map_err(|_| HttpError::Refused(format!("not a URL: {url}")))?;
        for _ in 0..=MAX_REDIRECTS {
            check_allowed(&uri)?;
            let response = tokio::time::timeout(RESPONSE_TIMEOUT, self.request(&uri, accept))
                .await
                .map_err(|_| HttpError::Network("the server did not answer in time".into()))??;
            let status = response.status().as_u16();
            if (300..400).contains(&status)
                && let Some(location) = response
                    .headers()
                    .get(hyper::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
            {
                uri = resolve_location(&uri, location)
                    .ok_or_else(|| HttpError::Network(format!("bad redirect to {location}")))?;
                continue;
            }
            return Ok(response);
        }
        Err(HttpError::Network("too many redirects".into()))
    }

    async fn request(
        &self,
        uri: &hyper::Uri,
        accept: &str,
    ) -> Result<hyper::Response<hyper::body::Incoming>, HttpError> {
        let https = uri.scheme_str() == Some("https");
        let host = uri
            .host()
            .ok_or_else(|| HttpError::Refused("no host".into()))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
        let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|error| HttpError::Network(error.to_string()))?;
        let _ = tcp.set_nodelay(true);
        let path = uri
            .path_and_query()
            .map(|path| path.as_str().to_string())
            .unwrap_or_else(|| "/".into());
        let request = hyper::Request::get(path)
            .header(
                hyper::header::HOST,
                uri.authority().map(|a| a.as_str()).unwrap_or(&host),
            )
            .header(hyper::header::ACCEPT, accept)
            .header(hyper::header::USER_AGENT, &self.user_agent)
            .header(hyper::header::CACHE_CONTROL, "no-cache")
            .body(Empty::<Bytes>::new())
            .map_err(|error| HttpError::Network(error.to_string()))?;
        if https {
            let connector = tokio_rustls::TlsConnector::from(self.tls.clone());
            let name = rustls::pki_types::ServerName::try_from(host.clone())
                .map_err(|error| HttpError::Network(error.to_string()))?;
            let tls = connector
                .connect(name, tcp)
                .await
                .map_err(|error| HttpError::Network(error.to_string()))?;
            send(hyper_util::rt::TokioIo::new(tls), request).await
        } else {
            send(hyper_util::rt::TokioIo::new(tcp), request).await
        }
    }
}

async fn send<T>(
    io: T,
    request: hyper::Request<Empty<Bytes>>,
) -> Result<hyper::Response<hyper::body::Incoming>, HttpError>
where
    T: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|error| HttpError::Network(error.to_string()))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    sender
        .send_request(request)
        .await
        .map_err(|error| HttpError::Network(error.to_string()))
}

/// https anywhere; http only to this machine.
fn check_allowed(uri: &hyper::Uri) -> Result<(), HttpError> {
    match uri.scheme_str() {
        Some("https") => Ok(()),
        Some("http") if is_loopback(uri.host().unwrap_or_default()) => Ok(()),
        _ => Err(HttpError::Refused(format!(
            "updates are only fetched over https, not {uri}"
        ))),
    }
}

fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn resolve_location(base: &hyper::Uri, location: &str) -> Option<hyper::Uri> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.parse().ok();
    }
    let authority = base.authority()?.as_str();
    let scheme = base.scheme_str()?;
    if location.starts_with('/') {
        return format!("{scheme}://{authority}{location}").parse().ok();
    }
    let path = base.path();
    let folder = &path[..=path.rfind('/').unwrap_or(0)];
    format!("{scheme}://{authority}{folder}{location}")
        .parse()
        .ok()
}

#[async_trait]
impl UpdateHttp for HyperHttp {
    async fn get(&self, url: &str, accept: &str, limit: usize) -> Result<Vec<u8>, HttpError> {
        let response = self.open(url, accept).await?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(HttpError::Status(status));
        }
        let body = tokio::time::timeout(
            RESPONSE_TIMEOUT,
            Limited::new(response.into_body(), limit).collect(),
        )
        .await
        .map_err(|_| HttpError::Network("the transfer stalled".into()))?
        .map_err(|error| HttpError::Network(error.to_string()))?;
        Ok(body.to_bytes().to_vec())
    }

    async fn download(
        &self,
        url: &str,
        file: &Path,
        progress: Progress<'_>,
    ) -> Result<(), HttpError> {
        let response = self.open(url, "application/octet-stream").await?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(HttpError::Status(status));
        }
        let total = response
            .headers()
            .get(hyper::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let mut out = tokio::fs::File::create(file)
            .await
            .map_err(|error| HttpError::Io(error.to_string()))?;
        let mut body = response.into_body();
        let mut received = 0u64;
        progress(0, total);
        loop {
            let frame = tokio::time::timeout(STALL_TIMEOUT, body.frame())
                .await
                .map_err(|_| HttpError::Network("the download stalled".into()))?;
            let Some(frame) = frame else { break };
            let frame = frame.map_err(|error| HttpError::Network(error.to_string()))?;
            if let Ok(chunk) = frame.into_data() {
                out.write_all(&chunk)
                    .await
                    .map_err(|error| HttpError::Io(error.to_string()))?;
                received += chunk.len() as u64;
                progress(received, total);
            }
        }
        out.flush()
            .await
            .map_err(|error| HttpError::Io(error.to_string()))?;
        out.sync_all()
            .await
            .map_err(|error| HttpError::Io(error.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_or_loopback_http() {
        let allowed = |url: &str| check_allowed(&url.parse().unwrap()).is_ok();
        assert!(allowed("https://github.com/FloSch62/muxus/releases/latest"));
        assert!(allowed("http://127.0.0.1:8080/latest-linux.yml"));
        assert!(allowed("http://[::1]:8080/latest-linux.yml"));
        assert!(allowed("http://localhost/x"));
        assert!(!allowed("http://github.com/FloSch62/muxus/releases/latest"));
        assert!(!allowed("ftp://127.0.0.1/x"));
    }

    #[test]
    fn follows_relative_redirects() {
        let base: hyper::Uri = "https://github.com/a/b/releases/latest".parse().unwrap();
        assert_eq!(
            resolve_location(&base, "/a/b/releases/tag/v1")
                .unwrap()
                .to_string(),
            "https://github.com/a/b/releases/tag/v1"
        );
        assert_eq!(
            resolve_location(&base, "tag/v1").unwrap().to_string(),
            "https://github.com/a/b/releases/tag/v1"
        );
        assert_eq!(
            resolve_location(&base, "https://objects.example/x")
                .unwrap()
                .to_string(),
            "https://objects.example/x"
        );
    }
}
