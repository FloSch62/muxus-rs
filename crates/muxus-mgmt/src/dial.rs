//! Opening a TCP connection to a device, with errors phrased the way the
//! UI shows them (the management part of `server/src/util/dial.ts`).

use std::io;
use std::time::Duration;

use tokio::net::TcpStream;

pub const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a connection could not be made, by the errno Node would report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialFailure {
    /// ECONNREFUSED
    Refused,
    /// ENOTFOUND / EAI_AGAIN
    NotFound,
    /// ETIMEDOUT
    TimedOut,
    /// EHOSTUNREACH / ENETUNREACH
    Unreachable,
    /// EACCES: an SSH gateway refused to open the channel.
    Denied,
    Other,
}

/// A failed connect: what kind, and the underlying message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DialError {
    pub failure: DialFailure,
    pub message: String,
}

impl DialError {
    pub fn new(failure: DialFailure, message: impl Into<String>) -> Self {
        Self {
            failure,
            message: message.into(),
        }
    }

    /// Classify an I/O error from a socket.
    pub fn from_io(err: &io::Error) -> Self {
        let message = err.to_string();
        let lower = message.to_lowercase();
        let failure = match err.kind() {
            io::ErrorKind::ConnectionRefused => DialFailure::Refused,
            io::ErrorKind::TimedOut => DialFailure::TimedOut,
            io::ErrorKind::HostUnreachable | io::ErrorKind::NetworkUnreachable => {
                DialFailure::Unreachable
            }
            _ if lower.contains("lookup")
                || lower.contains("not known")
                || lower.contains("resolve")
                || lower.contains("no such host") =>
            {
                DialFailure::NotFound
            }
            _ => DialFailure::Other,
        };
        Self { failure, message }
    }

    /// Give an SSH channel-open failure the errno a direct socket would have
    /// reported (`channelOpenError`). `administratively_prohibited` is SSH
    /// reason 1.
    pub fn from_channel_open(message: &str, administratively_prohibited: bool) -> Self {
        let lower = message.to_lowercase();
        let failure = if lower.contains("refused") {
            DialFailure::Refused
        } else if lower.contains("timed out")
            || lower.contains("timeout")
            || lower.contains("time out")
        {
            DialFailure::TimedOut
        } else if lower.contains("resolve")
            || lower.contains("not known")
            || lower.contains("no such host")
        {
            DialFailure::NotFound
        } else if administratively_prohibited {
            DialFailure::Denied
        } else {
            DialFailure::Other
        };
        Self {
            failure,
            message: format!("The SSH gateway could not open the connection: {message}"),
        }
    }

    /// The failure as one sentence (`connectFailureMessage`).
    pub fn user_message(&self) -> String {
        match self.failure {
            DialFailure::Refused => {
                "The remote computer refused the connection. Check the port and that the server runs."
                    .into()
            }
            DialFailure::NotFound => "The host name could not be resolved.".into(),
            DialFailure::TimedOut => "The connection timed out.".into(),
            DialFailure::Unreachable => "The remote computer is unreachable.".into(),
            DialFailure::Denied => "The SSH gateway is not allowed to open this connection.".into(),
            DialFailure::Other => self.message.clone(),
        }
    }
}

/// A TCP connection to `host:port`, given up after 15 seconds.
pub async fn connect_tcp(host: &str, port: u16) -> Result<TcpStream, DialError> {
    let connect = TcpStream::connect((host.trim_start_matches('[').trim_end_matches(']'), port));
    let socket = match tokio::time::timeout(TCP_CONNECT_TIMEOUT, connect).await {
        Ok(Ok(socket)) => socket,
        Ok(Err(err)) => return Err(DialError::from_io(&err)),
        Err(_) => {
            return Err(DialError::new(
                DialFailure::TimedOut,
                format!("Timed out connecting to {host}:{port}"),
            ));
        }
    };
    let _ = socket.set_nodelay(true);
    let keepalive = socket2::TcpKeepalive::new().with_time(Duration::from_secs(30));
    let _ = socket2::SockRef::from(&socket).set_tcp_keepalive(&keepalive);
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn words_refused_and_unresolvable_connections() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let refused = connect_tcp("127.0.0.1", port).await.unwrap_err();
        assert_eq!(refused.failure, DialFailure::Refused);
        assert!(
            refused
                .user_message()
                .starts_with("The remote computer refused")
        );
        let gateway = DialError::from_channel_open("Connection refused", false);
        assert_eq!(gateway.failure, DialFailure::Refused);
        assert!(
            gateway
                .message
                .starts_with("The SSH gateway could not open the connection")
        );
        assert_eq!(
            DialError::from_channel_open("prohibited", true).user_message(),
            "The SSH gateway is not allowed to open this connection."
        );
    }
}
