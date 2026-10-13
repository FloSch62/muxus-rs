//! Request-level failures that carry the HTTP status the TypeScript routes answered with, so the
//! session layer can map them onto the same user-facing outcomes.

/// A failed request: the status code the TypeScript server would have answered with, the
/// message, and an optional machine-readable code (e.g. `host-exists`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Problem {
    pub status: u16,
    pub message: String,
    pub code: Option<String>,
}

impl Problem {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            code: None,
        }
    }

    pub fn with_code(status: u16, message: impl Into<String>, code: &str) -> Self {
        Self {
            status,
            message: message.into(),
            code: Some(code.to_string()),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, message)
    }
}

impl From<std::io::Error> for Problem {
    fn from(err: std::io::Error) -> Self {
        Self::internal(err.to_string())
    }
}
