//! Reading the text of a shared session list (`server/src/shared-sessions/fetch.ts`): a
//! file on this computer or a share, or a web address with its redirects followed.
//!
//! Errors say why a link could not be read without repeating the link, which is often
//! personal: a web address is named by its host, a file by its name.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Bytes;
use muxus_core::api::MAX_SHARED_SESSIONS_BYTES;
use muxus_core::js::js_trim;
use muxus_core::text_decoding::decode_imported_text;
use tokio::time::Instant;
use url::Url;

use crate::error::{ApiError, ApiResult};

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;
const TOO_LARGE: &str = "The shared sessions file is larger than 10 MB.";

/// Where a shared session list is read from: a web address, or a file on this computer or
/// a share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedSessionsLocation {
    Url(Url),
    File(PathBuf),
}

/// `/^[a-z][a-z0-9+.-]*:\/\//i`.
fn has_scheme(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once("://") else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

/// `path.normalize`: `.` and `..` resolved without asking the file system.
fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Where a link points. Web addresses are http or https, and a password in one is refused:
/// it would be stored and backed up in plain text. Anything else is a file: an absolute
/// path, `~/…`, a `\\server\share` path or a `file://` address.
pub fn shared_sessions_location(value: &str, home: &Path) -> ApiResult<SharedSessionsLocation> {
    let text = js_trim(value);
    if has_scheme(text) {
        let url = Url::parse(text)
            .map_err(|_| ApiError::bad_request("That link is not a valid address."))?;
        if url.scheme() == "file" {
            let encoded_separator = url.path().to_ascii_lowercase().contains("%2f")
                || (cfg!(windows) && url.path().to_ascii_lowercase().contains("%5c"));
            return match url.to_file_path() {
                Ok(path) if !encoded_separator => Ok(SharedSessionsLocation::File(path)),
                _ => Err(ApiError::bad_request(
                    "That file:// link does not name a file on this computer.",
                )),
            };
        }
        if url.scheme() != "https" && url.scheme() != "http" {
            return Err(ApiError::bad_request(
                "Shared sessions are read from https:// or http:// links, or from a file.",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(ApiError::bad_request(
                "A link with a user name or password in it is not supported.",
            ));
        }
        return Ok(SharedSessionsLocation::Url(url));
    }
    let under_home = text
        .strip_prefix('~')
        .filter(|rest| rest.is_empty() || rest.starts_with(['/', '\\']));
    // What follows the `~` is joined as written; a backslash separates only on Windows.
    let separators: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    let expanded = match under_home {
        Some(rest) => home.join(rest.trim_start_matches(separators)),
        None => PathBuf::from(text),
    };
    // Rooted counts as absolute, as for Node: `\folder\file` on Windows names a file too.
    if !expanded.has_root() {
        return Err(ApiError::bad_request(
            "Enter the full link, starting with https://, or the full path of the file.",
        ));
    }
    Ok(SharedSessionsLocation::File(normalized(&expanded)))
}

/// The form a location is stored in: the address, or the path with `~` and `file://`
/// resolved.
pub fn stored_shared_sessions_location(value: &str, home: &Path) -> ApiResult<String> {
    Ok(match shared_sessions_location(value, home)? {
        SharedSessionsLocation::Url(url) => url.to_string(),
        SharedSessionsLocation::File(path) => path.to_string_lossy().into_owned(),
    })
}

fn problem(message: impl Into<String>) -> ApiError {
    ApiError::new(502, message)
}

/// `url.host`: the host, with the port when it is not the scheme's own.
fn host_of(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

/// Why a file could not be read. Only its name is repeated, not the folders above it.
fn file_problem(error: &std::io::Error, file: &Path) -> ApiError {
    use std::io::ErrorKind;
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    problem(match error.kind() {
        ErrorKind::TimedOut => format!(
            "{name} did not open within {} seconds. Is the drive connected?",
            TIMEOUT.as_secs()
        ),
        ErrorKind::NotFound | ErrorKind::NotADirectory => format!(
            "{name} was not found. Check the path, and that the drive or share is connected."
        ),
        ErrorKind::PermissionDenied => format!("{name} cannot be read: access is denied."),
        ErrorKind::IsADirectory => {
            format!("{name} is a folder. Enter the path of the session list in it.")
        }
        _ => format!("{name} could not be read: {error}"),
    })
}

async fn read_file(file: &Path) -> ApiResult<String> {
    let read = async {
        let info = tokio::fs::metadata(file).await?;
        if info.is_dir() {
            return Err(std::io::Error::from(std::io::ErrorKind::IsADirectory));
        }
        if info.len() > MAX_SHARED_SESSIONS_BYTES as u64 {
            return Ok(None);
        }
        tokio::fs::read(file).await.map(Some)
    };
    // A share that went away can hang rather than fail.
    let bytes = match tokio::time::timeout(TIMEOUT, read).await {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::from(std::io::ErrorKind::TimedOut)),
    }
    .map_err(|error| file_problem(&error, file))?;
    match bytes {
        Some(bytes) if bytes.len() <= MAX_SHARED_SESSIONS_BYTES => Ok(decode_imported_text(&bytes)),
        _ => Err(problem(TOO_LARGE)),
    }
}

/// What a web server answered: the list, or where to look instead.
enum Answer {
    Body(Vec<u8>),
    Other {
        status: hyper::StatusCode,
        reason: Option<String>,
        location: Option<String>,
    },
}

/// What kept a request from being answered.
enum RequestError {
    /// The name did not resolve.
    NotFound,
    /// No connection; the reason in the words of the operating system's error name.
    Unreachable(&'static str),
    /// The server's certificate did not verify.
    Certificate(String),
    TooLarge,
    Other(String),
}

impl From<std::io::Error> for RequestError {
    fn from(error: std::io::Error) -> Self {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::ConnectionRefused => RequestError::Unreachable("ECONNREFUSED"),
            ErrorKind::ConnectionReset => RequestError::Unreachable("ECONNRESET"),
            ErrorKind::HostUnreachable => RequestError::Unreachable("EHOSTUNREACH"),
            ErrorKind::NetworkUnreachable => RequestError::Unreachable("ENETUNREACH"),
            _ => {
                // A failed TLS handshake arrives as an I/O error around the TLS one.
                let certificate = error
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<rustls::Error>())
                    .and_then(|inner| match inner {
                        rustls::Error::InvalidCertificate(reason) => Some(format!("{reason:?}")),
                        _ => None,
                    });
                match certificate {
                    Some(reason) => RequestError::Certificate(reason),
                    None => RequestError::Other(error.to_string()),
                }
            }
        }
    }
}

impl From<hyper::Error> for RequestError {
    fn from(error: hyper::Error) -> Self {
        RequestError::Other(error.to_string())
    }
}

/// The TLS setup for reading lists. The operating system's authorities too: lab portals
/// tend to use a company CA.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("the default protocol versions")
            .with_root_certificates(muxus_rdp::certificates::trusted_certificate_authorities())
            .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

async fn exchange<T>(io: T, request: hyper::Request<Empty<Bytes>>) -> Result<Answer, RequestError>
where
    T: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let response = sender.send_request(request).await?;
    let status = response.status();
    if status != hyper::StatusCode::OK {
        let text = |value: &hyper::header::HeaderValue| value.to_str().ok().map(str::to_owned);
        // hyper keeps the reason only when the server worded it its own way.
        let reason = response
            .extensions()
            .get::<hyper::ext::ReasonPhrase>()
            .map(|reason| String::from_utf8_lossy(reason.as_bytes()).into_owned())
            .or_else(|| status.canonical_reason().map(str::to_owned));
        return Ok(Answer::Other {
            status,
            reason,
            location: response
                .headers()
                .get(hyper::header::LOCATION)
                .and_then(text),
        });
    }
    match Limited::new(response.into_body(), MAX_SHARED_SESSIONS_BYTES)
        .collect()
        .await
    {
        Ok(body) => Ok(Answer::Body(body.to_bytes().to_vec())),
        Err(error) if error.is::<http_body_util::LengthLimitError>() => Err(RequestError::TooLarge),
        Err(error) => Err(RequestError::Other(error.to_string())),
    }
}

async fn request(url: &Url) -> Result<Answer, RequestError> {
    let https = url.scheme() == "https";
    let host = url.host_str().unwrap_or_default();
    let name = host.trim_start_matches('[').trim_end_matches(']');
    let port = url.port_or_known_default().unwrap_or(80);
    let addresses: Vec<_> = tokio::net::lookup_host((name, port))
        .await
        .map_err(|_| RequestError::NotFound)?
        .collect();
    let mut connected = Err(RequestError::NotFound);
    for address in addresses {
        match tokio::net::TcpStream::connect(address).await {
            Ok(stream) => {
                connected = Ok(stream);
                break;
            }
            Err(error) => connected = Err(error.into()),
        }
    }
    let tcp = connected?;
    let _ = tcp.set_nodelay(true);
    let mut target = url.path().to_owned();
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    let request = hyper::Request::get(target)
        .header(hyper::header::HOST, host_of(url))
        .header(hyper::header::ACCEPT, "*/*")
        .header(hyper::header::USER_AGENT, "Muxus")
        .body(Empty::<Bytes>::new())
        .map_err(|error| RequestError::Other(error.to_string()))?;
    if https {
        let server = rustls::pki_types::ServerName::try_from(name.to_owned())
            .map_err(|error| RequestError::Other(error.to_string()))?;
        let tls = tokio_rustls::TlsConnector::from(tls_config())
            .connect(server, tcp)
            .await?;
        exchange(hyper_util::rt::TokioIo::new(tls), request).await
    } else {
        exchange(hyper_util::rt::TokioIo::new(tcp), request).await
    }
}

/// Why a link could not be read, without repeating the link.
fn fetch_problem(error: RequestError, url: &Url) -> ApiError {
    let host = host_of(url);
    problem(match error {
        RequestError::TooLarge => TOO_LARGE.to_owned(),
        RequestError::NotFound => format!(
            "{} could not be found. Check the link and your network or VPN.",
            url.host_str().unwrap_or_default()
        ),
        RequestError::Unreachable(code) => format!("{host} could not be reached ({code})."),
        RequestError::Certificate(reason) => {
            format!("The certificate of {host} is not trusted ({reason}).")
        }
        RequestError::Other(detail) => format!("{host} could not be read: {detail}"),
    })
}

/// Read the text of a shared session list: a file, or a web address with its redirects
/// followed.
pub async fn fetch_shared_sessions(value: &str, home: &Path) -> ApiResult<String> {
    let mut url = match shared_sessions_location(value, home)? {
        SharedSessionsLocation::File(path) => return read_file(&path).await,
        SharedSessionsLocation::Url(url) => url,
    };
    let deadline = Instant::now() + TIMEOUT;
    for _ in 0..=MAX_REDIRECTS {
        let answer = match tokio::time::timeout_at(deadline, request(&url)).await {
            Ok(answer) => answer.map_err(|error| fetch_problem(error, &url))?,
            Err(_) => {
                return Err(problem(format!(
                    "{} did not answer within {} seconds.",
                    host_of(&url),
                    TIMEOUT.as_secs()
                )));
            }
        };
        match answer {
            Answer::Body(body) => return Ok(decode_imported_text(&body)),
            Answer::Other {
                status,
                location: Some(location),
                ..
            } if status.is_redirection() => {
                let next = url
                    .join(&location)
                    .map_err(|_| ApiError::bad_request("That link is not a valid address."))?;
                // A web address may only lead to another one, never to a file here.
                match shared_sessions_location(next.as_str(), home)? {
                    SharedSessionsLocation::Url(next) => url = next,
                    SharedSessionsLocation::File(_) => {
                        return Err(problem(format!(
                            "{} redirects to a file, which is not followed.",
                            host_of(&url)
                        )));
                    }
                }
            }
            Answer::Other { status, reason, .. } => {
                let reason = reason
                    .filter(|reason| !reason.is_empty())
                    .map(|reason| format!(" {reason}"))
                    .unwrap_or_default();
                return Err(problem(format!(
                    "{} answered {}{reason}.",
                    host_of(&url),
                    status.as_u16()
                )));
            }
        }
    }
    Err(problem("The link redirects too many times."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\Users\me")
        } else {
            PathBuf::from("/home/me")
        }
    }

    fn refused(value: &str) -> String {
        let error = shared_sessions_location(value, &home()).unwrap_err();
        assert_eq!(error.status, 400);
        error.message
    }

    #[test]
    fn tells_web_addresses_from_files() {
        assert_eq!(
            stored_shared_sessions_location(
                "  https://portal.example.net/me.mxtsessions  ",
                &home()
            )
            .unwrap(),
            "https://portal.example.net/me.mxtsessions"
        );
        // Stored the way the address parses: a bare host gets its path.
        assert_eq!(
            stored_shared_sessions_location("HTTP://Portal.Example.net", &home()).unwrap(),
            "http://portal.example.net/"
        );
        assert_eq!(
            refused("ftp://portal.example.net/list"),
            "Shared sessions are read from https:// or http:// links, or from a file."
        );
        assert_eq!(
            refused("https://me:secret@portal.example.net/list"),
            "A link with a user name or password in it is not supported."
        );
        assert_eq!(
            refused("https://me@portal.example.net/list"),
            "A link with a user name or password in it is not supported."
        );
        assert_eq!(refused("https://"), "That link is not a valid address.");
        for relative in [
            "portal.example.net/list",
            "lists/lab.json",
            "~lists/lab.json",
        ] {
            assert_eq!(
                refused(relative),
                "Enter the full link, starting with https://, or the full path of the file."
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolves_paths_the_way_they_are_stored() {
        let stored = |value: &str| stored_shared_sessions_location(value, &home()).unwrap();
        assert_eq!(stored("~/lists/lab.json"), "/home/me/lists/lab.json");
        assert_eq!(stored("~"), "/home/me");
        assert_eq!(
            stored("/mnt/share/../team/./lab.json"),
            "/mnt/team/lab.json"
        );
        assert_eq!(
            stored("file:///mnt/share/lab%20hosts.mxtsessions"),
            "/mnt/share/lab hosts.mxtsessions"
        );
        assert_eq!(stored("file://localhost/mnt/lab.json"), "/mnt/lab.json");
        assert_eq!(
            refused("file://server/share/lab.json"),
            "That file:// link does not name a file on this computer."
        );
        assert_eq!(
            refused("file:///mnt/a%2Fb.json"),
            "That file:// link does not name a file on this computer."
        );
    }

    #[test]
    fn names_a_web_address_by_its_host() {
        let host = |text: &str| host_of(&Url::parse(text).unwrap());
        assert_eq!(
            host("https://portal.example.net/personal-token/list"),
            "portal.example.net"
        );
        assert_eq!(
            host("https://portal.example.net:443/list"),
            "portal.example.net"
        );
        assert_eq!(host("http://127.0.0.1:8080/list"), "127.0.0.1:8080");
        assert_eq!(host("http://[::1]:8080/list"), "[::1]:8080");
    }
}
