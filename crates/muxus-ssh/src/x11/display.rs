//! `$DISPLAY` parsing and connecting to the local X server it names.

use std::io;
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use super::X11Error;
use super::xauthority::{XauthTarget, xauth_target_for_ip};

/// X display numbers map to TCP ports 6000 + n.
pub const X11_TCP_PORT_BASE: u16 = 6000;

/// How long connecting to the local X server may take.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The operating system whose conventions apply (Node's `process.platform`). Injectable so tests
/// can exercise every platform's rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
    /// Other Unix-like systems: X11 Unix sockets without the Linux abstract namespace.
    Other,
}

impl Platform {
    /// The platform this binary runs on.
    pub const fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// Where a local X server accepts connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum X11Endpoint {
    Unix {
        path: PathBuf,
        /// Linux abstract-namespace fallback: the socket name without its leading NUL byte.
        abstract_name: Option<String>,
    },
    Tcp {
        host: String,
        port: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDisplay {
    pub endpoint: X11Endpoint,
    /// Display number as Xauthority records it ("0").
    pub number: String,
    pub screen: u32,
}

/// Parse $DISPLAY the way ssh(1) connects to it: `:n`/`unix:n` use the X11 Unix socket (TCP on
/// Windows, where X servers only listen there), a leading `/` is a socket path such as XQuartz's
/// launchd socket, and `host:n` is TCP port 6000 + n. A trailing `.screen` is optional
/// everywhere.
///
/// Display or screen numbers too large for a TCP port or a `u32` are not displays.
pub fn parse_display(display: &str, platform: Platform) -> Option<ParsedDisplay> {
    let value = display.trim();
    // `^(.*):(\d+)(?:\.(\d+))?$`: the part after the last colon is the display number.
    let (raw_host, rest) = value.rsplit_once(':')?;
    if raw_host.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }
    let (number, screen_text) = match rest.split_once('.') {
        Some((number, screen)) => (number, Some(screen)),
        None => (rest, None),
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(number) || screen_text.is_some_and(|screen| !digits(screen)) {
        return None;
    }
    let screen = match screen_text {
        Some(text) => text.parse::<u32>().ok()?,
        None => 0,
    };
    let parsed = |endpoint| {
        Some(ParsedDisplay {
            endpoint,
            number: number.to_string(),
            screen,
        })
    };

    if raw_host.starts_with('/') {
        // XQuartz's socket file is literally named "…/org.xquartz:0".
        return parsed(X11Endpoint::Unix {
            path: PathBuf::from(format!("{raw_host}:{number}")),
            abstract_name: None,
        });
    }

    let host = raw_host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(raw_host);
    let local = host.is_empty() || host == "unix";
    if local && platform != Platform::Windows {
        let socket = format!("/tmp/.X11-unix/X{number}");
        return parsed(X11Endpoint::Unix {
            abstract_name: (platform == Platform::Linux).then(|| socket.clone()),
            path: PathBuf::from(socket),
        });
    }
    let port = number
        .parse::<u32>()
        .ok()
        .and_then(|n| u16::try_from(n).ok())
        .and_then(|n| X11_TCP_PORT_BASE.checked_add(n))?;
    parsed(X11Endpoint::Tcp {
        host: if local {
            "127.0.0.1".to_string()
        } else {
            host.to_string()
        },
        port,
    })
}

/// Which Xauthority records apply to a connection to `endpoint`; `peer` is the connected socket's
/// remote address ([`X11Socket::peer_ip`]).
pub fn xauth_target(endpoint: &X11Endpoint, peer: Option<IpAddr>) -> XauthTarget {
    match (endpoint, peer) {
        (X11Endpoint::Unix { .. }, _) => XauthTarget::Local,
        (X11Endpoint::Tcp { .. }, Some(ip)) => xauth_target_for_ip(ip),
        (X11Endpoint::Tcp { .. }, None) => super::xauthority::xauth_target_for_peer(None),
    }
}

/// A stream to the local X server.
#[derive(Debug)]
pub enum X11Socket {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
}

impl X11Socket {
    /// The IP address of a TCP display's server; `None` for Unix sockets.
    pub fn peer_ip(&self) -> Option<IpAddr> {
        match self {
            Self::Tcp(stream) => stream.peer_addr().ok().map(|addr| addr.ip()),
            #[cfg(unix)]
            Self::Unix(_) => None,
        }
    }
}

macro_rules! delegate {
    ($self:ident, $stream:ident => $call:expr) => {
        match $self.get_mut() {
            X11Socket::Tcp($stream) => $call,
            #[cfg(unix)]
            X11Socket::Unix($stream) => $call,
        }
    };
}

impl AsyncRead for X11Socket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        delegate!(self, stream => Pin::new(stream).poll_read(cx, buf))
    }
}

impl AsyncWrite for X11Socket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        delegate!(self, stream => Pin::new(stream).poll_write(cx, buf))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, stream => Pin::new(stream).poll_flush(cx))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, stream => Pin::new(stream).poll_shutdown(cx))
    }
}

/// Open a stream to the X server; Unix sockets fall back to the abstract namespace.
pub async fn connect_x11_endpoint(
    endpoint: &X11Endpoint,
    timeout: Duration,
) -> Result<X11Socket, X11Error> {
    match endpoint {
        X11Endpoint::Tcp { host, port } => {
            let stream = with_timeout(timeout, TcpStream::connect((host.as_str(), *port))).await?;
            // X11 is small requests waiting on replies; Nagle plus delayed ACKs would stall
            // them. Xlib and ssh(1) disable it on X connections too.
            stream.set_nodelay(true)?;
            Ok(X11Socket::Tcp(stream))
        }
        X11Endpoint::Unix {
            path,
            abstract_name,
        } => connect_unix(path, abstract_name.as_deref(), timeout).await,
    }
}

async fn with_timeout<T>(
    timeout: Duration,
    connect: impl Future<Output = io::Result<T>>,
) -> Result<T, X11Error> {
    match tokio::time::timeout(timeout, connect).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(X11Error::ConnectTimeout),
    }
}

#[cfg(unix)]
async fn connect_unix(
    path: &std::path::Path,
    abstract_name: Option<&str>,
    timeout: Duration,
) -> Result<X11Socket, X11Error> {
    use tokio::net::UnixStream;

    let Some(abstract_name) = abstract_name else {
        let stream = with_timeout(timeout, UnixStream::connect(path)).await?;
        return Ok(X11Socket::Unix(stream));
    };
    if path.exists()
        && let Ok(stream) = with_timeout(timeout, UnixStream::connect(path)).await
    {
        return Ok(X11Socket::Unix(stream));
    }
    connect_abstract(abstract_name, timeout).await
}

#[cfg(target_os = "linux")]
async fn connect_abstract(name: &str, timeout: Duration) -> Result<X11Socket, X11Error> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixStream};

    let name = name.as_bytes().to_vec();
    let connect = tokio::task::spawn_blocking(move || {
        let addr = SocketAddr::from_abstract_name(&name)?;
        let stream = UnixStream::connect_addr(&addr)?;
        stream.set_nonblocking(true)?;
        Ok::<_, io::Error>(stream)
    });
    let stream = match tokio::time::timeout(timeout, connect).await {
        Ok(Ok(result)) => result?,
        Ok(Err(join)) => return Err(io::Error::other(join).into()),
        Err(_) => return Err(X11Error::ConnectTimeout),
    };
    Ok(X11Socket::Unix(tokio::net::UnixStream::from_std(stream)?))
}

#[cfg(all(unix, not(target_os = "linux")))]
async fn connect_abstract(_name: &str, _timeout: Duration) -> Result<X11Socket, X11Error> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "abstract Unix sockets exist only on Linux",
    )
    .into())
}

#[cfg(not(unix))]
async fn connect_unix(
    _path: &std::path::Path,
    _abstract_name: Option<&str>,
    _timeout: Duration,
) -> Result<X11Socket, X11Error> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "X11 displays on Unix sockets are not supported on this platform",
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unix(path: &str, abstract_name: Option<&str>) -> X11Endpoint {
        X11Endpoint::Unix {
            path: PathBuf::from(path),
            abstract_name: abstract_name.map(str::to_string),
        }
    }

    fn tcp(host: &str, port: u16) -> X11Endpoint {
        X11Endpoint::Tcp {
            host: host.to_string(),
            port,
        }
    }

    #[test]
    fn maps_n_to_the_x11_unix_socket_with_the_abstract_namespace_as_a_linux_fallback() {
        assert_eq!(
            parse_display(":0", Platform::Linux),
            Some(ParsedDisplay {
                endpoint: unix("/tmp/.X11-unix/X0", Some("/tmp/.X11-unix/X0")),
                number: "0".into(),
                screen: 0,
            })
        );
        assert_eq!(
            parse_display("unix:1.2", Platform::MacOs),
            Some(ParsedDisplay {
                endpoint: unix("/tmp/.X11-unix/X1", None),
                number: "1".into(),
                screen: 2,
            })
        );
    }

    #[test]
    fn keeps_the_display_suffix_in_xquartz_launchd_socket_paths() {
        assert_eq!(
            parse_display(
                "/private/tmp/com.apple.launchd.AbC/org.xquartz:0",
                Platform::MacOs
            )
            .map(|parsed| parsed.endpoint),
            Some(unix(
                "/private/tmp/com.apple.launchd.AbC/org.xquartz:0",
                None
            ))
        );
    }

    #[test]
    fn uses_tcp_6000_plus_n_for_host_displays_and_for_n_on_windows() {
        let endpoint =
            |display: &str, platform| parse_display(display, platform).map(|p| p.endpoint);
        assert_eq!(
            endpoint("localhost:10.0", Platform::Linux),
            Some(tcp("localhost", 6010))
        );
        assert_eq!(
            endpoint("workstation.example:1", Platform::Linux),
            Some(tcp("workstation.example", 6001))
        );
        assert_eq!(
            endpoint("[fd00::5]:2", Platform::Linux),
            Some(tcp("fd00::5", 6002))
        );
        assert_eq!(
            endpoint(":0.0", Platform::Windows),
            Some(tcp("127.0.0.1", 6000))
        );
    }

    #[test]
    fn rejects_values_that_are_not_displays() {
        assert_eq!(parse_display("wayland-0", Platform::Linux), None);
        assert_eq!(parse_display("", Platform::Linux), None);
        assert_eq!(parse_display(":0.1.2", Platform::Linux), None);
        assert_eq!(parse_display("host:x", Platform::Linux), None);
        assert_eq!(parse_display("host:99999", Platform::Linux), None);
    }

    #[test]
    fn trims_whitespace_and_reads_the_screen() {
        let parsed = parse_display("  :3.1\n", Platform::Other).unwrap();
        assert_eq!(parsed.endpoint, unix("/tmp/.X11-unix/X3", None));
        assert_eq!((parsed.number.as_str(), parsed.screen), ("3", 1));
    }

    #[test]
    fn unix_sockets_are_local_for_xauthority() {
        assert_eq!(
            xauth_target(&unix("/tmp/.X11-unix/X0", None), None),
            XauthTarget::Local
        );
        assert_eq!(
            xauth_target(&tcp("localhost", 6000), Some("127.0.0.1".parse().unwrap())),
            XauthTarget::Local
        );
    }

    #[tokio::test]
    async fn connects_over_tcp_with_nodelay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let socket = connect_x11_endpoint(&tcp("127.0.0.1", port), DEFAULT_CONNECT_TIMEOUT)
            .await
            .unwrap();
        let X11Socket::Tcp(stream) = &socket else {
            panic!("expected TCP");
        };
        assert!(stream.nodelay().unwrap());
        assert_eq!(socket.peer_ip(), Some("127.0.0.1".parse().unwrap()));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn falls_back_to_the_abstract_namespace_when_the_socket_file_is_missing() {
        use std::os::linux::net::SocketAddrExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = tempfile::tempdir().unwrap();
        // A unique abstract name so parallel test runs never collide.
        let name = format!("muxus-x11-test-{}", dir.path().display());
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind_addr(&addr).unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::UnixListener::from_std(listener).unwrap();
        let endpoint = X11Endpoint::Unix {
            path: dir.path().join("X0"),
            abstract_name: Some(name),
        };
        let (connected, accepted) = tokio::join!(
            connect_x11_endpoint(&endpoint, DEFAULT_CONNECT_TIMEOUT),
            listener.accept()
        );
        let mut socket = connected.unwrap();
        let (mut server, _) = accepted.unwrap();
        socket.write_all(b"hi").await.unwrap();
        let mut buf = [0u8; 2];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hi");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reports_a_missing_unix_socket_without_a_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = X11Endpoint::Unix {
            path: dir.path().join("X0"),
            abstract_name: None,
        };
        assert!(matches!(
            connect_x11_endpoint(&endpoint, DEFAULT_CONNECT_TIMEOUT).await,
            Err(X11Error::Io(_))
        ));
    }
}
