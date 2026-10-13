//! Failures in the terms the UI acts on: a sentence for the user and whether to ask for the
//! credentials again.
//!
//! Port of `describeRdpError` in `client/src/remote-desktop/rdp-client.ts`. The TypeScript
//! client got socket, TLS and certificate failures back from its proxy as RDCleanPath error
//! codes (WSA, HTTP status, TLS alert); here they come straight from the socket and from
//! rustls, and map to the same sentences.

use std::io;

use ironrdp::connector::sspi::credssp::NStatusCode;
use ironrdp::connector::{ConnectorError, ConnectorErrorKind};

/// Why an RDP connection attempt failed, in the terms the UI acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RdpFailureKind {
    /// The server rejected the user name or password: ask again and retry.
    Credentials,
    /// The server (or the user, for its certificate) refused the connection; redialling would
    /// only be refused again.
    Refused,
    /// Anything else.
    Failed,
}

/// A failed or lost RDP connection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RdpFailure {
    pub message: String,
    pub kind: RdpFailureKind,
}

impl RdpFailure {
    pub fn new(message: impl Into<String>, kind: RdpFailureKind) -> Self {
        Self {
            message: message.into(),
            kind,
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self::new(message, RdpFailureKind::Failed)
    }
}

/// Socket and resolver failures, the ones the proxy reported as Windows socket errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketFailure {
    /// WSAETIMEDOUT (10060).
    TimedOut,
    /// WSAECONNREFUSED (10061).
    Refused,
    /// WSAEHOSTUNREACH (10065).
    HostUnreachable,
    /// WSAENETUNREACH (10051).
    NetworkUnreachable,
    /// WSAECONNRESET (10054).
    Reset,
    /// WSAHOST_NOT_FOUND (11001).
    HostNotFound,
    /// WSATRY_AGAIN (11002).
    TryAgain,
}

impl SocketFailure {
    /// The sentence the TypeScript client shows for this socket error.
    pub fn message(self) -> &'static str {
        match self {
            Self::TimedOut => "The connection timed out.",
            Self::Refused => {
                "The remote computer refused the connection. Is Remote Desktop enabled, and is the port right?"
            }
            Self::HostUnreachable => "The remote computer is unreachable.",
            Self::NetworkUnreachable => "The network is unreachable.",
            Self::Reset => "The remote computer closed the connection.",
            Self::HostNotFound => "The host name could not be resolved.",
            Self::TryAgain => "The host name could not be resolved right now.",
        }
    }

    /// The socket error behind an I/O failure, when it is one the UI explains.
    pub fn from_io(err: &io::Error) -> Option<Self> {
        match err.kind() {
            io::ErrorKind::ConnectionRefused => return Some(Self::Refused),
            io::ErrorKind::TimedOut => return Some(Self::TimedOut),
            io::ErrorKind::HostUnreachable => return Some(Self::HostUnreachable),
            io::ErrorKind::NetworkUnreachable => return Some(Self::NetworkUnreachable),
            io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::UnexpectedEof => return Some(Self::Reset),
            _ => {}
        }
        let text = err.to_string().to_ascii_lowercase();
        if text.contains("temporary failure in name resolution") || text.contains("try again") {
            Some(Self::TryAgain)
        } else if text.contains("failed to lookup address")
            || text.contains("name or service not known")
            || text.contains("no such host")
            || text.contains("nodename nor servname")
        {
            Some(Self::HostNotFound)
        } else {
            None
        }
    }
}

/// The failures the TypeScript client distinguished, before they become sentences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RdpError {
    /// CredSSP reported STATUS_WRONG_PASSWORD.
    WrongPassword,
    /// CredSSP reported STATUS_LOGON_FAILURE.
    LogonFailure,
    /// The server denied this account access (early user authorization).
    AccessDenied,
    /// X.224 negotiation failed; `code` is the RDP_NEG_FAILURE code when the server sent one.
    Negotiation { code: Option<u32>, trace: String },
    /// The TCP path failed.
    Socket(SocketFailure),
    /// The TCP path failed for a reason the UI does not explain.
    ConnectFailed,
    /// The user declined the server's certificate.
    CertificateRejected,
    /// The server sent this TLS alert during the handshake.
    TlsAlert(u8),
    /// Everything else, with the error chain on one line.
    General(String),
}

const NEGOTIATION_MESSAGES: [(&[&str], &str); 5] = [
    (
        &["hybrid_required", "hybrid required"],
        "The server requires Network Level Authentication, which it could not complete.",
    ),
    (
        &["ssl_not_allowed"],
        "The server only allows the legacy RDP security layer, which Muxus does not support. Enable TLS or NLA on the server.",
    ),
    (
        &["ssl_cert_not_on_server"],
        "The server has no TLS certificate configured.",
    ),
    (
        &["ssl_with_user_auth"],
        "The server requires TLS with user authentication (RDSTLS), which Muxus does not support.",
    ),
    (&["ssl_required"], "The server requires TLS but refused it."),
];

fn negotiation_message(code: Option<u32>, trace: &str) -> String {
    let by_code = match code {
        Some(5) => Some(0),
        Some(2) => Some(1),
        Some(3) => Some(2),
        Some(6) => Some(3),
        Some(1) => Some(4),
        _ => None,
    };
    let lower = trace.to_ascii_lowercase();
    let by_trace = || {
        NEGOTIATION_MESSAGES.iter().position(|(patterns, _)| {
            patterns.iter().any(|pattern| {
                if *pattern == "hybrid required" {
                    lower.contains("hybrid") && lower.contains("required")
                } else {
                    lower.contains(pattern)
                }
            })
        })
    };
    match by_code.or_else(by_trace) {
        Some(index) => NEGOTIATION_MESSAGES[index].1.to_owned(),
        None => format!("Security negotiation failed: {}", first_line(trace)),
    }
}

fn first_line(trace: &str) -> &str {
    trace
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("The connection failed.")
}

impl RdpError {
    /// Turn the failure into a sentence and the kind the UI branches on.
    pub fn describe(&self) -> RdpFailure {
        match self {
            Self::WrongPassword | Self::LogonFailure => RdpFailure::new(
                "The user name or password is incorrect.",
                RdpFailureKind::Credentials,
            ),
            Self::AccessDenied => RdpFailure::new(
                "The remote computer denied access to this account.",
                RdpFailureKind::Refused,
            ),
            Self::Negotiation { code, trace } => {
                RdpFailure::new(negotiation_message(*code, trace), RdpFailureKind::Refused)
            }
            Self::Socket(failure) => RdpFailure::failed(failure.message()),
            Self::ConnectFailed => {
                RdpFailure::failed("Muxus could not connect to the remote computer.")
            }
            Self::CertificateRejected => RdpFailure::new(
                "The server certificate was not trusted.",
                RdpFailureKind::Refused,
            ),
            Self::TlsAlert(alert) => {
                RdpFailure::failed(format!("The TLS handshake failed (alert {alert})."))
            }
            Self::General(trace) => {
                let lower = trace.to_ascii_lowercase();
                // Servers other than Windows (FreeRDP, GNOME Remote Desktop) reject an NLA
                // logon with a bare CredSSP error status rather than LOGON_FAILURE.
                if lower.contains("credssp")
                    || lower.contains("ntlm")
                    || lower.contains("sec_e_logon_denied")
                {
                    return Self::WrongPassword.describe();
                }
                if lower.contains("websocket closed")
                    || lower.contains("read frame")
                    || lower.contains("connection reset")
                    || lower.contains("broken pipe")
                {
                    return RdpFailure::failed("The connection to the remote computer was lost.");
                }
                RdpFailure::failed(first_line(trace))
            }
        }
    }
}

/// An error and its sources on one line, the way `anyhow`'s `{:#}` prints them.
pub(crate) fn error_chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

fn io_error_in<'a>(err: &'a (dyn std::error::Error + 'static)) -> Option<&'a io::Error> {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(error) = current {
        if let Some(io) = error.downcast_ref::<io::Error>() {
            return Some(io);
        }
        current = error.source();
    }
    None
}

/// Classify an IronRDP connector error the way the WebAssembly build's `IronError` did.
pub(crate) fn connector_error(err: &ConnectorError) -> RdpError {
    match err.kind() {
        ConnectorErrorKind::Credssp(sspi) => match sspi.nstatus {
            Some(NStatusCode::WRONG_PASSWORD) => RdpError::WrongPassword,
            Some(NStatusCode::LOGON_FAILURE) => RdpError::LogonFailure,
            _ => RdpError::General(error_chain(err)),
        },
        ConnectorErrorKind::AccessDenied => RdpError::AccessDenied,
        ConnectorErrorKind::Negotiation(failure) => RdpError::Negotiation {
            code: Some(u32::from(failure.code())),
            trace: error_chain(err),
        },
        ConnectorErrorKind::Reason(reason) if reason.contains("but server selected") => {
            // A server that only speaks the legacy RDP security layer answers the TLS/NLA
            // request with plain RDP; the proxy reported this as a negotiation failure.
            RdpError::Negotiation {
                code: Some(2),
                trace: error_chain(err),
            }
        }
        _ => {
            // A dropped connection mid-sequence reads as the socket error it was.
            if let Some(socket) = io_error_in(err).and_then(SocketFailure::from_io) {
                return RdpError::Socket(socket);
            }
            RdpError::General(error_chain(err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn general(trace: &str) -> RdpFailure {
        RdpError::General(trace.to_owned()).describe()
    }

    #[test]
    fn turns_ironrdp_errors_into_messages_and_retry_decisions() {
        assert_eq!(
            RdpError::WrongPassword.describe().kind,
            RdpFailureKind::Credentials
        );
        assert_eq!(
            RdpError::LogonFailure.describe().kind,
            RdpFailureKind::Credentials
        );
        // FreeRDP and GNOME Remote Desktop reject NLA with a bare CredSSP status.
        assert_eq!(
            general("[CredSSP] CredSSP: InvalidToken: status 0xc00700ea").kind,
            RdpFailureKind::Credentials
        );
        assert!(
            RdpError::Socket(SocketFailure::Refused)
                .describe()
                .message
                .contains("refused the connection")
        );
        assert_eq!(
            RdpError::CertificateRejected.describe(),
            RdpFailure::new(
                "The server certificate was not trusted.",
                RdpFailureKind::Refused
            )
        );
        assert!(
            RdpError::Negotiation {
                code: None,
                trace: "negotiation: HYBRID_REQUIRED_BY_SERVER".into()
            }
            .describe()
            .message
            .contains("Network Level Authentication")
        );
        assert_eq!(
            general("read frame: WebSocket Closed: code: 1005").message,
            "The connection to the remote computer was lost."
        );
        assert_eq!(
            general("plain failure"),
            RdpFailure::failed("plain failure")
        );
    }

    #[test]
    fn explains_negotiation_failure_codes() {
        let message = |code| {
            RdpError::Negotiation {
                code: Some(code),
                trace: String::new(),
            }
            .describe()
            .message
        };
        assert!(message(5).contains("Network Level Authentication"));
        assert!(message(2).contains("legacy RDP security layer"));
        assert!(message(3).contains("no TLS certificate"));
        assert!(message(6).contains("RDSTLS"));
        assert_eq!(message(1), "The server requires TLS but refused it.");
        assert_eq!(
            message(4),
            "Security negotiation failed: The connection failed."
        );
    }

    #[test]
    fn maps_socket_errors() {
        let refused = io::Error::from(io::ErrorKind::ConnectionRefused);
        assert_eq!(
            SocketFailure::from_io(&refused),
            Some(SocketFailure::Refused)
        );
        let dns =
            io::Error::other("failed to lookup address information: Name or service not known");
        assert_eq!(
            SocketFailure::from_io(&dns),
            Some(SocketFailure::HostNotFound)
        );
        assert_eq!(
            SocketFailure::TryAgain.message(),
            "The host name could not be resolved right now."
        );
        assert_eq!(
            RdpError::TlsAlert(40).describe().message,
            "The TLS handshake failed (alert 40)."
        );
        assert_eq!(
            RdpError::ConnectFailed.describe().message,
            "Muxus could not connect to the remote computer."
        );
    }
}
