//! What a connection attempt needs from the UI and from the password vault. Both are async
//! callback traits the session layer implements; this crate never blocks on a user.

use std::future::Future;
use std::pin::Pin;

/// A boxed, sendable future (the callback traits are object safe).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub use muxus_core::protocol::{
    AuthPromptField as PromptField, AuthPromptInfo, AuthPromptPurpose, AuthPromptResponse,
    HostKeyChallenge, RememberPasswordOffer, TrustState as HostKeyState,
};

/// A response with these answers.
pub fn answers<I: IntoIterator<Item = S>, S: Into<String>>(answers: I) -> AuthPromptResponse {
    AuthPromptResponse {
        answers: answers.into_iter().map(Into::into).collect(),
        remember_password: None,
        skipped: None,
    }
}

/// The user dismissed a prompt; the login is cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("authentication cancelled")]
pub struct Cancelled;

/// Interactive hooks a connection attempt needs from the UI.
pub trait ConnectIo: Send + Sync {
    /// A progress or warning line for the terminal; transient lines may be replaced by the next.
    fn status(&self, message: &str, transient: bool);
    /// Ask the user to answer auth prompts (password, 2FA, key passphrase).
    fn prompt(&self, info: AuthPromptInfo) -> BoxFuture<'_, Result<AuthPromptResponse, Cancelled>>;
    /// Ask the user to accept a new or changed host key.
    fn host_key(&self, challenge: HostKeyChallenge) -> BoxFuture<'_, Result<bool, Cancelled>>;
}

/// Lock state of the password vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VaultStatus {
    pub configured: bool,
    pub locked: bool,
}

/// Failures the vault reports, mirroring the TypeScript error classes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    #[error("{0}")]
    InvalidMasterPassword(String),
    #[error("{0}")]
    InvalidMasterPasswordFormat(String),
    #[error("the saved credential is damaged")]
    CredentialVaultCorrupt,
    #[error("the OS credential store is unavailable")]
    KeyStoreUnavailable,
    #[error("the password vault is already configured")]
    AlreadyConfigured,
    #[error("{0}")]
    Other(String),
}

/// The encrypted password vault (implemented by the store layer).
pub trait PasswordVault: Send + Sync {
    fn status(&self) -> VaultStatus;
    fn has_ssh_password(&self, account: &str) -> bool;
    /// The saved password; `master_password` unlocks a locked vault for this operation.
    fn ssh_password<'a>(
        &'a self,
        account: &'a str,
        master_password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, VaultError>>;
    fn remember_ssh_password<'a>(
        &'a self,
        account: &'a str,
        label: &'a str,
        password: &'a str,
        master_password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), VaultError>>;
    /// Creates the vault with the default unlock policy.
    fn create<'a>(&'a self, master_password: &'a str) -> BoxFuture<'a, Result<(), VaultError>>;
}
