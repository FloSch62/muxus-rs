//! Password vault types.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

/// Public metadata for one SSH password encrypted in the local password vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordVaultCredential {
    pub id: String,
    pub label: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Controls when a master password is required to use a saved credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasswordVaultUnlockPolicy {
    /// Keep the vault key in the operating-system credential store.
    Never,
    /// Unlock once for the lifetime of the backend.
    Startup,
    /// Unlock separately for every saved-credential operation.
    Credential,
}

impl PasswordVaultUnlockPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            PasswordVaultUnlockPolicy::Never => "never",
            PasswordVaultUnlockPolicy::Startup => "startup",
            PasswordVaultUnlockPolicy::Credential => "credential",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "never" => Some(PasswordVaultUnlockPolicy::Never),
            "startup" => Some(PasswordVaultUnlockPolicy::Startup),
            "credential" => Some(PasswordVaultUnlockPolicy::Credential),
            _ => None,
        }
    }
}

/// New vaults use the operating-system credential store unless explicitly overridden.
pub const DEFAULT_PASSWORD_VAULT_UNLOCK_POLICY: PasswordVaultUnlockPolicy =
    PasswordVaultUnlockPolicy::Never;

/// Runtime state of the local password vault.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordVaultStatus {
    pub configured: bool,
    pub unlock_policy: Option<PasswordVaultUnlockPolicy>,
    /// True when using a saved credential currently requires the master password.
    pub locked: bool,
    /// Whether the native OS credential-store integration is usable.
    pub os_key_store_available: bool,
    pub credential_count: usize,
    pub credentials: Vec<PasswordVaultCredential>,
    /// Named secrets not tied to a login, sorted by name.
    pub secrets: Vec<PasswordVaultSecret>,
}

/// Public metadata for a named secret in the password vault, such as an
/// `enable` password. Saved commands refer to it by `id`; the value never
/// leaves the backend except through the master-password reveal.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordVaultSecret {
    pub id: String,
    pub name: String,
    /// Optional account the secret belongs to; plain metadata, never sent.
    pub username: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Body of POST /api/password-vault/secrets and PUT /api/password-vault/secrets/:id.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultSecretInput {
    pub name: String,
    pub username: Option<String>,
    /// Required when creating; left out on an edit that keeps the saved value.
    pub value: Option<String>,
    pub master_password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultSecretSaveResult {
    pub secret: PasswordVaultSecret,
    pub status: PasswordVaultStatus,
}

/// Body of POST /api/password-vault/secrets/:id/send. The backend types the
/// secret into each live terminal session itself, so the value never reaches
/// the UI, session history or log files.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendVaultSecretRequest {
    /// Backend terminal ids: the focused session first, then its mirrored panes.
    pub terminal_ids: Vec<String>,
    /// Press Enter after the secret.
    pub enter: bool,
    /// Needed only while the vault's prompt policy keeps it locked.
    pub master_password: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendVaultSecretResult {
    /// Sessions the secret was typed into.
    pub sent: usize,
}
