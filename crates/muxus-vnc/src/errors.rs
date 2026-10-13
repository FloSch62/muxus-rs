//! Failures in the terms the UI acts on.
//!
//! Port of `describeVncFailure` (`client/src/remote-desktop/vnc-client.ts`) and of the branches
//! `RemoteDesktopViewImpl.tsx` takes on noVNC's `disconnect` event.

use std::io;

/// Why a VNC connection attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VncFailureKind {
    /// The server rejected the credentials it asked for: ask again (not the vault) and retry.
    Credentials,
    /// The server refused before any sign-in (blacklisted, too many clients, ACL), or the user
    /// did not trust its key: redialling would only be refused again.
    Refused,
    /// Anything else.
    Failed,
}

/// A failed VNC connection attempt.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct VncFailure {
    pub message: String,
    pub kind: VncFailureKind,
    /// The server's reason when it rejected the security handshake.
    pub security_failure: Option<String>,
}

impl VncFailure {
    pub fn new(message: impl Into<String>, kind: VncFailureKind) -> Self {
        Self {
            message: message.into(),
            kind,
            security_failure: None,
        }
    }

    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self::new(message, VncFailureKind::Failed)
    }

    /// The server rejected the security handshake; `asked_for_credentials` tells a rejected
    /// login from a refusal before any sign-in.
    pub(crate) fn security(reason: Option<String>, asked_for_credentials: bool) -> Self {
        let reason = reason
            .filter(|reason| !reason.is_empty())
            .unwrap_or_else(|| "Authentication failed.".to_owned());
        if asked_for_credentials {
            Self {
                message: format!("The server rejected the login: {reason}"),
                kind: VncFailureKind::Credentials,
                security_failure: Some(reason),
            }
        } else {
            Self {
                message: format!("The VNC server refused the connection: {reason}"),
                kind: VncFailureKind::Refused,
                security_failure: Some(reason),
            }
        }
    }
}

/// Plain-language versions of the failures noVNC reports while connecting.
pub fn describe_vnc_failure(details: Option<&str>) -> Option<String> {
    let details = details.filter(|details| !details.is_empty())?;
    let lower = details.to_ascii_lowercase();
    if lower.contains("unsupported security types") {
        return Some(
            "The VNC server offers no sign-in method Muxus supports; it may require TLS encryption. \
             Allow VNC password authentication (VncAuth) on the server, or reach it through an SSH \
             gateway without TLS."
                .to_owned(),
        );
    }
    if lower.contains("unsupported vencrypt") {
        return Some("The server uses a VeNCrypt version Muxus does not support.".to_owned());
    }
    if lower.contains("security negotiation failed") || lower.contains("security handshake failed")
    {
        return Some("The VNC server rejected the password.".to_owned());
    }
    // The backend closes the stream with a readable reason when it cannot connect.
    if let Some(start) = details.find("Connection closed (code: ")
        && details.ends_with(')')
        && let Some(reason_at) = details[start..].find(", reason: ")
    {
        let code = &details[start + "Connection closed (code: ".len()..start + reason_at];
        if !code.is_empty() && code.chars().all(|c| c.is_ascii_digit()) {
            let reason = &details[start + reason_at + ", reason: ".len()..details.len() - 1];
            return Some(reason.to_owned());
        }
    }
    if lower.contains("connection closed") {
        return Some("The connection to the VNC server closed during the handshake.".to_owned());
    }
    let mut message = details;
    for prefix in [
        "RFB failure: ",
        "Failed when connecting: ",
        "Failed while connected: ",
    ] {
        if let Some(rest) = message.strip_prefix(prefix) {
            message = rest;
            break;
        }
    }
    Some(message.to_owned())
}

/// A TCP connect failure as one sentence (`connectFailureMessage` in `server/src/util/dial.ts`).
pub fn connect_failure_message(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::ConnectionRefused => {
            return "The remote computer refused the connection. Check the port and that the server runs."
                .to_owned();
        }
        io::ErrorKind::TimedOut => return "The connection timed out.".to_owned(),
        io::ErrorKind::HostUnreachable | io::ErrorKind::NetworkUnreachable => {
            return "The remote computer is unreachable.".to_owned();
        }
        io::ErrorKind::PermissionDenied => {
            return "The SSH gateway is not allowed to open this connection.".to_owned();
        }
        _ => {}
    }
    let text = err.to_string();
    let lower = text.to_ascii_lowercase();
    if lower.contains("failed to lookup address")
        || lower.contains("name or service not known")
        || lower.contains("temporary failure in name resolution")
        || lower.contains("no such host")
        || lower.contains("nodename nor servname")
    {
        return "The host name could not be resolved.".to_owned();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_what_novnc_only_logs() {
        assert!(
            describe_vnc_failure(Some("RFB failure: Unsupported security types (types: 19)"))
                .unwrap()
                .contains("no sign-in method")
        );
        assert_eq!(
            describe_vnc_failure(Some(
                "Failed when connecting: Connection closed (code: 1011, reason: The host name could not be resolved.)"
            ))
            .as_deref(),
            Some("The host name could not be resolved.")
        );
        assert_eq!(describe_vnc_failure(None), None);
    }

    #[test]
    fn covers_the_other_branches() {
        assert_eq!(
            describe_vnc_failure(Some(
                "Failed when connecting: Unsupported VeNCrypt version 0.1"
            ))
            .as_deref(),
            Some("The server uses a VeNCrypt version Muxus does not support.")
        );
        assert_eq!(
            describe_vnc_failure(Some("Security handshake failed")).as_deref(),
            Some("The VNC server rejected the password.")
        );
        assert_eq!(
            describe_vnc_failure(Some(
                "Failed when connecting: Connection closed (code: 1006)"
            ))
            .as_deref(),
            Some("The connection to the VNC server closed during the handshake.")
        );
        assert_eq!(
            describe_vnc_failure(Some("Failed while connected: Error decoding rect: boom"))
                .as_deref(),
            Some("Error decoding rect: boom")
        );
    }

    #[test]
    fn tells_a_rejected_login_from_a_refusal() {
        let login = VncFailure::security(Some("bad password".into()), true);
        assert_eq!(login.kind, VncFailureKind::Credentials);
        assert_eq!(login.message, "The server rejected the login: bad password");
        let refused = VncFailure::security(None, false);
        assert_eq!(refused.kind, VncFailureKind::Refused);
        assert_eq!(
            refused.message,
            "The VNC server refused the connection: Authentication failed."
        );
    }

    #[test]
    fn phrases_connect_failures() {
        assert_eq!(
            connect_failure_message(&io::Error::from(io::ErrorKind::ConnectionRefused)),
            "The remote computer refused the connection. Check the port and that the server runs."
        );
        assert_eq!(
            connect_failure_message(&io::Error::other(
                "failed to lookup address information: Name or service not known"
            )),
            "The host name could not be resolved."
        );
    }
}
