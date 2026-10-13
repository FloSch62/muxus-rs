//! Errors of [`crate::Backend`] calls.
//!
//! The TypeScript routes answered failures with an HTTP status and an `ApiErrorBody`
//! (`{ message, code? }`); the UI keys its handling on both. [`ApiError`] carries the same three
//! facts so the Rust UI can do what the React client did with a failed `fetch`.

use std::fmt;

use muxus_core::api::ApiErrorBody;
use muxus_core::validate::ValidationError;
use muxus_ssh::Problem;
use muxus_store::DbError;
use muxus_store::history::HistoryError;
use muxus_store::vault::VaultError;

/// A failed backend call: the status the TypeScript route answered with, its message, and an
/// optional machine-readable code (`vault-locked`, `workspace-locked`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
    pub code: Option<String>,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
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

    /// Any failure the TypeScript routes did not map: `sendError`'s 500.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, message)
    }

    /// The body the TypeScript server sent.
    pub fn body(&self) -> ApiErrorBody {
        ApiErrorBody {
            message: self.message.clone(),
            code: self.code.clone(),
        }
    }

    /// A schema failure answered as 400 with the first issue's message, or `fallback` when zod
    /// would have had none.
    pub fn invalid(error: ValidationError) -> Self {
        Self::bad_request(error.message)
    }
}

impl From<Problem> for ApiError {
    fn from(problem: Problem) -> Self {
        Self {
            status: problem.status,
            message: problem.message,
            code: problem.code,
        }
    }
}

impl From<DbError> for ApiError {
    fn from(error: DbError) -> Self {
        Self::internal(error.to_string())
    }
}

impl From<HistoryError> for ApiError {
    fn from(error: HistoryError) -> Self {
        Self::internal(error.to_string())
    }
}

impl From<ValidationError> for ApiError {
    fn from(error: ValidationError) -> Self {
        Self::invalid(error)
    }
}

impl From<std::io::Error> for ApiError {
    fn from(error: std::io::Error) -> Self {
        Self::internal(error.to_string())
    }
}

/// The shared vault → problem mapping of `routes/password-vault.ts` (`sendVaultError`), also
/// used by the folder routes.
impl From<VaultError> for ApiError {
    fn from(error: VaultError) -> Self {
        let message = error.to_string();
        let (status, code) = match &error {
            VaultError::InvalidMasterPassword => (401, "invalid-master-password"),
            VaultError::InvalidMasterPasswordFormat(_) => (400, "invalid-master-password-format"),
            VaultError::AlreadyConfigured => (409, "vault-already-configured"),
            VaultError::NotConfigured => (409, "vault-not-configured"),
            VaultError::PolicyMismatch(_) => (409, "vault-policy-mismatch"),
            VaultError::AutomaticAccess => (423, "vault-automatic-access-unavailable"),
            VaultError::UnlockRequired(_) => (423, "vault-locked"),
            VaultError::InvalidSecret(_) => (400, "invalid-vault-secret"),
            VaultError::SecretNameTaken(_) => (409, "vault-secret-name-taken"),
            VaultError::InvalidSavedPasswordFormat => (400, "invalid-saved-password-format"),
            VaultError::KeyStoreUnavailable(_) => (503, "os-key-store-unavailable"),
            _ => return Self::internal(message),
        };
        Self::with_code(status, message, code)
    }
}

/// Result of a backend call.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_errors_keep_the_typescript_status_and_code() {
        let locked = ApiError::from(VaultError::UnlockRequired(
            muxus_core::api::PasswordVaultUnlockPolicy::Startup,
        ));
        assert_eq!(
            (locked.status, locked.code.as_deref()),
            (423, Some("vault-locked"))
        );
        let wrong = ApiError::from(VaultError::InvalidMasterPassword);
        assert_eq!(wrong.status, 401);
        assert_eq!(wrong.message, "The master password is incorrect.");
        let other = ApiError::from(VaultError::Crypto("boom".into()));
        assert_eq!((other.status, other.code), (500, None));
    }
}
