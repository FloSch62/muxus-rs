//! Errors of the management backend, and how the UI sees them.

use muxus_core::gnmi_path::GnmiPathError;
use muxus_core::management::ManagementError;

/// gRPC status names, by code.
pub const GRPC_STATUS_NAMES: [&str; 17] = [
    "OK",
    "CANCELLED",
    "UNKNOWN",
    "INVALID_ARGUMENT",
    "DEADLINE_EXCEEDED",
    "NOT_FOUND",
    "ALREADY_EXISTS",
    "PERMISSION_DENIED",
    "RESOURCE_EXHAUSTED",
    "FAILED_PRECONDITION",
    "ABORTED",
    "OUT_OF_RANGE",
    "UNIMPLEMENTED",
    "INTERNAL",
    "UNAVAILABLE",
    "DATA_LOSS",
    "UNAUTHENTICATED",
];

/// The gRPC status codes the backend produces or checks for.
pub mod code {
    pub const OK: i32 = 0;
    pub const CANCELLED: i32 = 1;
    pub const UNKNOWN: i32 = 2;
    pub const INVALID_ARGUMENT: i32 = 3;
    pub const DEADLINE_EXCEEDED: i32 = 4;
    pub const PERMISSION_DENIED: i32 = 7;
    pub const RESOURCE_EXHAUSTED: i32 = 8;
    pub const UNIMPLEMENTED: i32 = 12;
    pub const INTERNAL: i32 = 13;
    pub const UNAVAILABLE: i32 = 14;
    pub const DATA_LOSS: i32 = 15;
    pub const UNAUTHENTICATED: i32 = 16;
}

/// A gRPC status other than OK, from the device or from the call itself.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct GrpcError {
    pub code: i32,
    pub message: String,
}

impl GrpcError {
    /// An empty message reads as the status name, like the TypeScript.
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        let message = message.into();
        let message = if message.is_empty() {
            status_name(code)
        } else {
            message
        };
        Self { code, message }
    }

    /// `CANCELLED: Cancelled.`, what cancelling a call ends it with.
    pub fn cancelled() -> Self {
        Self::new(code::CANCELLED, "Cancelled.")
    }

    /// The status name (`UNAVAILABLE`), or `STATUS_<n>` for codes past the list.
    pub fn code_name(&self) -> String {
        usize::try_from(self.code)
            .ok()
            .and_then(|index| GRPC_STATUS_NAMES.get(index))
            .map_or_else(
                || format!("STATUS_{}", self.code),
                |name| (*name).to_owned(),
            )
    }
}

fn status_name(code: i32) -> String {
    usize::try_from(code)
        .ok()
        .and_then(|index| GRPC_STATUS_NAMES.get(index))
        .map_or_else(|| format!("gRPC status {code}"), |name| (*name).to_owned())
}

impl From<tonic::Status> for GrpcError {
    fn from(status: tonic::Status) -> Self {
        GrpcError::new(status.code() as i32, status.message())
    }
}

/// A NETCONF transport problem; RPC errors themselves arrive as reply text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct NetconfError {
    pub message: String,
    /// The request was cancelled from the UI.
    pub cancelled: bool,
}

impl NetconfError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cancelled: false,
        }
    }

    pub fn cancelled() -> Self {
        Self {
            message: "Cancelled.".into(),
            cancelled: true,
        }
    }
}

/// gNOI/gNSI JSON that does not fit the message it is meant for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProtoJsonError(pub String);

/// Anything a management request can fail with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Grpc(#[from] GrpcError),
    #[error(transparent)]
    Netconf(#[from] NetconfError),
    #[error(transparent)]
    Path(#[from] GnmiPathError),
    #[error(transparent)]
    ProtoJson(#[from] ProtoJsonError),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(message: impl Into<String>) -> Self {
        Error::Other(message.into())
    }
}

/// How a failure is shown to the UI (`describeError`).
pub fn describe_error(err: &Error) -> ManagementError {
    match err {
        Error::Grpc(err) => ManagementError {
            message: err.message.clone(),
            code: Some(err.code_name()),
            cancelled: (err.code == code::CANCELLED).then_some(true),
        },
        Error::Netconf(err) => ManagementError {
            message: err.message.clone(),
            code: None,
            cancelled: err.cancelled.then_some(true),
        },
        Error::Path(err) => ManagementError {
            message: format!("Invalid path: {}", err.message),
            code: Some("INVALID_ARGUMENT".into()),
            cancelled: None,
        },
        Error::ProtoJson(err) => ManagementError {
            message: format!("Invalid request: {}", err.0),
            code: Some("INVALID_ARGUMENT".into()),
            cancelled: None,
        },
        Error::Other(message) => ManagementError {
            message: message.clone(),
            code: None,
            cancelled: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_statuses_and_describes_failures() {
        let err = GrpcError::new(code::UNAVAILABLE, "");
        assert_eq!(err.message, "UNAVAILABLE");
        assert_eq!(GrpcError::new(42, "").message, "gRPC status 42");
        assert_eq!(GrpcError::new(42, "x").code_name(), "STATUS_42");
        let described = describe_error(&GrpcError::cancelled().into());
        assert_eq!(described.code.as_deref(), Some("CANCELLED"));
        assert_eq!(described.cancelled, Some(true));
        let path = muxus_core::gnmi_path::parse_gnmi_path("/a[b").unwrap_err();
        assert!(
            describe_error(&path.into())
                .message
                .starts_with("Invalid path: ")
        );
    }
}
