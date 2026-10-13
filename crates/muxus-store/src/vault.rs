//! The local password vault (port of `server/src/security/password-vault.ts`).
//!
//! A random 32-byte vault key encrypts each saved password and named secret
//! with AES-256-GCM. The database holds only a master-password-wrapped copy
//! of that key (scrypt + AES-256-GCM). With the "never" policy the raw key is
//! also kept in the operating-system credential store; "startup" keeps it in
//! memory once unlocked; "credential" derives it for each operation.
//!
//! Everything is byte-compatible with vaults the TypeScript build created:
//! same KDF parameters as stored, same associated data, same row layout, same
//! credential-store entry.
//!
//! Methods block (scrypt takes a noticeable fraction of a second, the OS
//! credential store may do D-Bus round trips); async callers run them on a
//! blocking thread.

mod crypto;
pub mod key_store;
pub mod secret_input;

use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use muxus_core::api::{
    DEFAULT_PASSWORD_VAULT_UNLOCK_POLICY, PasswordVaultCredential, PasswordVaultSecret,
    PasswordVaultStatus, PasswordVaultUnlockPolicy,
};
use muxus_core::js;
use zeroize::Zeroizing;

pub use crypto::{DEFAULT_KDF, PasswordVaultKdf};
pub use key_store::{
    KeyStoreBackend, KeyStoreUnavailable, MemoryVaultKeyStore, SystemVaultKeyStore, VaultKeyStore,
};

use crate::Database;
use crate::db::{
    CredentialRefInput, DbError, EncryptedCredentialInput, EncryptedCredentialRecord,
    EncryptedCredentialSecret, PasswordVaultConfig,
};
use crate::ids::random_base64url;
use crypto::{KEY_BYTES, LEGACY_MASTER_KEY_AAD, MASTER_KEY_AAD, SALT_BYTES, SecretKey};

pub const PASSWORD_VAULT_PROVIDER: &str = "muxus-master-vault";
pub const SSH_PASSWORD_SERVICE: &str = "muxus/ssh-password/v1";
/// Named secrets that are not tied to a login, such as an `enable` password.
pub const NAMED_SECRET_SERVICE: &str = "muxus/named-secret/v1";
pub const MASTER_PASSWORD_MIN_LENGTH: usize = 8;
pub const MASTER_PASSWORD_MAX_BYTES: usize = 1024;
pub const SSH_PASSWORD_MAX_BYTES: usize = 8192;
pub const VAULT_SECRET_NAME_MAX_LENGTH: usize = 80;
pub const VAULT_SECRET_USERNAME_MAX_LENGTH: usize = 256;

const FORMAT_VERSION: i64 = 3;
const CREDENTIAL_FORMAT_VERSION: i64 = 1;
const LEGACY_MASTER_PASSWORD_MIN_LENGTH: usize = 8;
const LEGACY_DEVICE_KEY_FILENAME: &str = "muxus-vault-device.key";

/// Why a vault operation failed. Messages match the TypeScript's.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("Password vault is not configured.")]
    NotConfigured,
    #[error("Password vault is already configured.")]
    AlreadyConfigured,
    #[error("{0}")]
    PolicyMismatch(String),
    /// The master password is needed (the vault is locked under this policy).
    #[error("Enter the master password to unlock the saved credential.")]
    UnlockRequired(PasswordVaultUnlockPolicy),
    /// A "never" vault whose OS credential-store copy is missing or stale.
    #[error(
        "The OS credential-store copy is unavailable. Enter the master password to restore it."
    )]
    AutomaticAccess,
    #[error("The master password is incorrect.")]
    InvalidMasterPassword,
    #[error("{0}")]
    InvalidMasterPasswordFormat(String),
    #[error("The SSH password is too long to save.")]
    InvalidSavedPasswordFormat,
    #[error("{0}")]
    InvalidSecret(String),
    #[error("A secret named “{0}” already exists.")]
    SecretNameTaken(String),
    #[error("A saved password could not be decrypted.")]
    CredentialCorrupt,
    #[error(transparent)]
    KeyStoreUnavailable(#[from] KeyStoreUnavailable),
    /// The key derivation itself failed (bad stored parameters).
    #[error("{0}")]
    Kdf(String),
    #[error("{0}")]
    Crypto(String),
    #[error(transparent)]
    Database(#[from] DbError),
}

impl VaultError {
    /// Whether asking for the master password and retrying would help
    /// (`VaultUnlockRequiredError`, which `VaultAutomaticAccessError` extends).
    pub fn is_unlock_required(&self) -> bool {
        matches!(
            self,
            VaultError::UnlockRequired(_) | VaultError::AutomaticAccess
        )
    }
}

pub type VaultResult<T> = Result<T, VaultError>;

/// How a vault is set up.
#[derive(Clone, Default)]
pub struct VaultOptions {
    /// scrypt parameters for new master keys; [`DEFAULT_KDF`] when absent.
    pub kdf: Option<PasswordVaultKdf>,
    /// The credential store; the OS store for a file database, an in-memory
    /// one for an in-memory database, when absent.
    pub key_store: Option<Arc<dyn VaultKeyStore>>,
}

/// A named secret to create or update.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SecretInput {
    pub name: String,
    pub username: Option<String>,
    /// Required to create; absent on an edit that keeps the saved value.
    pub value: Option<String>,
}

struct State {
    key: Option<SecretKey>,
    os_key_store_available: bool,
    initialized: bool,
}

/// The password vault over one application database.
pub struct PasswordVault {
    database: Arc<Database>,
    key_store: Arc<dyn VaultKeyStore>,
    kdf: PasswordVaultKdf,
    state: Mutex<State>,
}

impl std::fmt::Debug for PasswordVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordVault")
            .field("kdf", &self.kdf)
            .field("locked", &self.is_locked())
            .finish()
    }
}

/// A key to use for one operation: the unlocked vault key, or one derived
/// just for it ("credential" policy), wiped after use either way.
struct CredentialKey {
    key: SecretKey,
}

impl PasswordVault {
    pub fn new(database: Arc<Database>, options: VaultOptions) -> Self {
        let key_store = options.key_store.unwrap_or_else(|| {
            if database.filename().is_none() {
                Arc::new(MemoryVaultKeyStore::new()) as Arc<dyn VaultKeyStore>
            } else {
                Arc::new(SystemVaultKeyStore::new())
            }
        });
        Self {
            database,
            key_store,
            kdf: options.kdf.unwrap_or(DEFAULT_KDF),
            state: Mutex::new(State {
                key: None,
                os_key_store_available: true,
                initialized: false,
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn is_locked(&self) -> bool {
        self.state().key.is_none()
    }

    /// Remove the obsolete device key file, retry pending OS-key cleanup and
    /// load the OS-stored key of a "never" vault.
    pub fn initialize(&self) -> VaultResult<()> {
        if self.state().initialized {
            return Ok(());
        }
        if let Some(filename) = self.database.filename() {
            remove_legacy_device_key(filename);
        }
        let config = self
            .database
            .password_vault_config()?
            .map(|record| record.config);
        let automatic = config
            .as_ref()
            .filter(|config| config.unlock_policy == PasswordVaultUnlockPolicy::Never);
        self.retry_pending_os_key_cleanup(automatic.map(|config| config.vault_id.as_str()))?;
        if let Some(config) = automatic {
            match self.key_store.get(&config.vault_id) {
                Ok(Some(key)) => {
                    if let Some(key) = to_key(&key)
                        && self.is_verified_key(config, &key)?
                    {
                        self.ensure_key_check(config, &key)?;
                        self.set_key(&key);
                    }
                }
                Ok(None) => {}
                Err(_) => self.state().os_key_store_available = false,
            }
        }
        self.state().initialized = true;
        Ok(())
    }

    pub fn status(&self) -> VaultResult<PasswordVaultStatus> {
        let config = self.database.password_vault_config()?;
        let credentials: Vec<PasswordVaultCredential> = self
            .database
            .list_encrypted_credentials(PASSWORD_VAULT_PROVIDER, SSH_PASSWORD_SERVICE)?
            .into_iter()
            .map(|record| PasswordVaultCredential {
                id: record.id,
                label: record.label.unwrap_or_else(|| "Saved SSH password".into()),
                created_at: record.created_at,
                updated_at: record.updated_at,
            })
            .collect();
        let state = self.state();
        Ok(PasswordVaultStatus {
            configured: config.is_some(),
            unlock_policy: config.map(|config| config.config.unlock_policy),
            locked: state.key.is_none(),
            os_key_store_available: state.os_key_store_available,
            credential_count: credentials.len(),
            credentials,
            secrets: {
                drop(state);
                self.secrets()?
            },
        })
    }

    /// Named secrets, sorted by name.
    pub fn secrets(&self) -> VaultResult<Vec<PasswordVaultSecret>> {
        Ok(self
            .database
            .list_encrypted_credentials(PASSWORD_VAULT_PROVIDER, NAMED_SECRET_SERVICE)?
            .into_iter()
            .map(|record| PasswordVaultSecret {
                id: record.id,
                name: record.label.unwrap_or_else(|| "Secret".into()),
                username: record.username.filter(|username| !username.is_empty()),
                created_at: record.created_at,
                updated_at: record.updated_at,
            })
            .collect())
    }

    /// Create the vault under a master password.
    pub fn create(
        &self,
        master_password: &str,
        unlock_policy: Option<PasswordVaultUnlockPolicy>,
    ) -> VaultResult<()> {
        let unlock_policy = unlock_policy.unwrap_or(DEFAULT_PASSWORD_VAULT_UNLOCK_POLICY);
        validate_master_password(master_password)?;
        if self.database.password_vault_config()?.is_some() {
            return Err(VaultError::AlreadyConfigured);
        }
        let vault_key = crypto::random_key();
        let vault_id = random_base64url(16);
        let mut salt = vec![0u8; SALT_BYTES];
        crate::ids::random_bytes(&mut salt);
        let mut os_key_stored = false;
        let result = (|| {
            let master_key = crypto::derive_key(master_password, &salt, self.kdf)?;
            let wrapped = crypto::seal(vault_key.as_ref(), &master_key, MASTER_KEY_AAD)?;
            if unlock_policy == PasswordVaultUnlockPolicy::Never {
                self.store_os_key(&vault_id, &vault_key)?;
                os_key_stored = true;
            }
            self.database
                .create_password_vault_config(&PasswordVaultConfig {
                    format_version: FORMAT_VERSION,
                    vault_id: vault_id.clone(),
                    unlock_policy,
                    kdf_algorithm: "scrypt".into(),
                    kdf_salt: salt.clone(),
                    kdf_cost: i64::from(self.kdf.cost),
                    kdf_block_size: i64::from(self.kdf.block_size),
                    kdf_parallelism: i64::from(self.kdf.parallelism),
                    master_key_nonce: wrapped.nonce,
                    master_key_ciphertext: wrapped.ciphertext,
                    master_key_tag: wrapped.auth_tag,
                    key_check: Some(crypto::vault_key_check(&vault_key)),
                })?;
            if unlock_policy != PasswordVaultUnlockPolicy::Credential {
                self.set_key(&vault_key);
            }
            Ok(())
        })();
        if result.is_err() && os_key_stored {
            let _ = self.key_store.delete(&vault_id);
        }
        result
    }

    /// Unlock a "startup" vault for the rest of this process.
    pub fn unlock_for_session(&self, master_password: &str) -> VaultResult<()> {
        let config = self.require_config()?;
        if config.unlock_policy != PasswordVaultUnlockPolicy::Startup {
            return Err(VaultError::PolicyMismatch(
                "Session unlock is available only with the startup prompt policy.".into(),
            ));
        }
        let vault_key = unwrap_master_key(master_password, &config)?;
        self.ensure_key_check(&config, &vault_key)?;
        self.set_key(&vault_key);
        Ok(())
    }

    /// Restores "never prompt" access after a database or OS-keyring restore.
    pub fn repair_automatic_access(&self, master_password: &str) -> VaultResult<()> {
        let config = self.require_config()?;
        let vault_key = unwrap_master_key(master_password, &config)?;
        let checked = self.ensure_key_check(&config, &vault_key)?;
        self.store_os_key(&checked.vault_id, &vault_key)?;
        let next = PasswordVaultConfig {
            unlock_policy: PasswordVaultUnlockPolicy::Never,
            ..checked.clone()
        };
        if let Err(error) = self.database.update_password_vault_config(&next) {
            let _ = self.key_store.delete(&checked.vault_id);
            return Err(error.into());
        }
        self.set_key(&vault_key);
        Ok(())
    }

    pub fn change_unlock_policy(
        &self,
        master_password: &str,
        unlock_policy: PasswordVaultUnlockPolicy,
    ) -> VaultResult<()> {
        let config = self.require_config()?;
        let vault_key = unwrap_master_key(master_password, &config)?;
        let checked = self.ensure_key_check(&config, &vault_key)?;
        if unlock_policy == PasswordVaultUnlockPolicy::Never {
            self.store_os_key(&checked.vault_id, &vault_key)?;
            if let Err(error) = self
                .database
                .update_password_vault_config(&PasswordVaultConfig {
                    unlock_policy,
                    ..checked.clone()
                })
            {
                let _ = self.key_store.delete(&checked.vault_id);
                return Err(error.into());
            }
            self.set_key(&vault_key);
            return Ok(());
        }
        let was_automatic = checked.unlock_policy == PasswordVaultUnlockPolicy::Never;
        if was_automatic {
            self.database
                .queue_password_vault_key_cleanup(&checked.vault_id)?;
        }
        self.database
            .update_password_vault_config(&PasswordVaultConfig {
                unlock_policy,
                ..checked.clone()
            })?;
        if was_automatic {
            self.delete_pending_os_key_best_effort(&checked.vault_id)?;
        }
        if unlock_policy == PasswordVaultUnlockPolicy::Startup {
            self.set_key(&vault_key);
        } else {
            self.lock();
        }
        Ok(())
    }

    /// Forget the in-memory key.
    pub fn lock(&self) {
        self.state().key = None;
    }

    /// Release the vault (locks it).
    pub fn dispose(&self) {
        self.lock();
    }

    pub fn change_master_password(
        &self,
        current_password: &str,
        next_password: &str,
    ) -> VaultResult<()> {
        validate_master_password(next_password)?;
        let config = self.require_config()?;
        let vault_key = unwrap_master_key(current_password, &config)?;
        let checked = self.ensure_key_check(&config, &vault_key)?;
        let mut salt = vec![0u8; SALT_BYTES];
        crate::ids::random_bytes(&mut salt);
        let master_key = crypto::derive_key(next_password, &salt, self.kdf)?;
        let wrapped = crypto::seal(vault_key.as_ref(), &master_key, MASTER_KEY_AAD)?;
        self.database
            .update_password_vault_config(&PasswordVaultConfig {
                format_version: FORMAT_VERSION,
                kdf_algorithm: "scrypt".into(),
                kdf_salt: salt,
                kdf_cost: i64::from(self.kdf.cost),
                kdf_block_size: i64::from(self.kdf.block_size),
                kdf_parallelism: i64::from(self.kdf.parallelism),
                master_key_nonce: wrapped.nonce,
                master_key_ciphertext: wrapped.ciphertext,
                master_key_tag: wrapped.auth_tag,
                ..checked.clone()
            })?;
        if checked.unlock_policy == PasswordVaultUnlockPolicy::Never {
            self.store_os_key_best_effort(&checked.vault_id, &vault_key);
        }
        if checked.unlock_policy != PasswordVaultUnlockPolicy::Credential {
            self.set_key(&vault_key);
        }
        Ok(())
    }

    pub fn has_ssh_password(&self, account: &str) -> VaultResult<bool> {
        Ok(self
            .database
            .encrypted_credential(PASSWORD_VAULT_PROVIDER, SSH_PASSWORD_SERVICE, account)?
            .is_some())
    }

    /// A saved SSH password, following the prompt policy.
    pub fn ssh_password(
        &self,
        account: &str,
        master_password: Option<&str>,
    ) -> VaultResult<Option<Zeroizing<String>>> {
        let Some(record) = self.database.encrypted_credential(
            PASSWORD_VAULT_PROVIDER,
            SSH_PASSWORD_SERVICE,
            account,
        )?
        else {
            return Ok(None);
        };
        let access = self.credential_key(master_password)?;
        decrypt_record(&record, &access.key).map(Some)
    }

    /// Encrypt a successful SSH password under the vault key.
    pub fn remember_ssh_password(
        &self,
        account: &str,
        label: &str,
        password: &str,
        master_password: Option<&str>,
    ) -> VaultResult<()> {
        validate_saved_password(password)?;
        let access = self.credential_key(master_password)?;
        let plaintext = Zeroizing::new(password.as_bytes().to_vec());
        self.database.upsert_encrypted_credential_atomically(
            &CredentialRefInput {
                provider: PASSWORD_VAULT_PROVIDER.into(),
                service: SSH_PASSWORD_SERVICE.into(),
                account: account.into(),
                label: Some(label.into()),
                username: None,
            },
            |reference| seal_credential(&plaintext, &access.key, &reference.id),
        )?;
        Ok(())
    }

    /// Any saved value, always behind the master password.
    pub fn reveal_credential(
        &self,
        id: &str,
        master_password: &str,
    ) -> VaultResult<Option<Zeroizing<String>>> {
        let Some(record) = self
            .database
            .encrypted_credential_by_id(id, PASSWORD_VAULT_PROVIDER)?
        else {
            return Ok(None);
        };
        let config = self.require_config()?;
        let management_key = unwrap_master_key(master_password, &config)?;
        self.ensure_key_check(&config, &management_key)?;
        decrypt_record(&record, &management_key).map(Some)
    }

    /// Replace a saved value; behind the master password.
    pub fn update_credential(
        &self,
        id: &str,
        master_password: &str,
        password: &str,
    ) -> VaultResult<bool> {
        validate_saved_password(password)?;
        let Some(record) = self
            .database
            .encrypted_credential_by_id(id, PASSWORD_VAULT_PROVIDER)?
        else {
            return Ok(false);
        };
        let config = self.require_config()?;
        let management_key = unwrap_master_key(master_password, &config)?;
        self.ensure_key_check(&config, &management_key)?;
        let secret = seal_credential(password.as_bytes(), &management_key, &record.id)?;
        self.database
            .upsert_encrypted_credential(&EncryptedCredentialInput {
                reference: CredentialRefInput {
                    provider: record.provider,
                    service: record.service,
                    account: record.account,
                    label: record.label,
                    username: record.username,
                },
                secret,
            })?;
        Ok(true)
    }

    pub fn has_secret(&self, id: &str) -> VaultResult<bool> {
        Ok(self.secret_record(id)?.is_some())
    }

    /// A named secret's value for typing into a session. Like a saved SSH
    /// password, it follows the prompt policy.
    pub fn secret_value(
        &self,
        id: &str,
        master_password: Option<&str>,
    ) -> VaultResult<Option<Zeroizing<String>>> {
        let Some(record) = self.secret_record(id)? else {
            return Ok(None);
        };
        let access = self.credential_key(master_password)?;
        decrypt_record(&record, &access.key).map(Some)
    }

    /// Save a new named secret; always takes the master password.
    pub fn create_secret(
        &self,
        input: &SecretInput,
        master_password: &str,
    ) -> VaultResult<PasswordVaultSecret> {
        let (name, username) = self.secret_details(&input.name, input.username.as_deref(), None)?;
        let value = input.value.as_deref().unwrap_or_default();
        validate_secret_value(value)?;
        let config = self.require_config()?;
        let management_key = unwrap_master_key(master_password, &config)?;
        self.ensure_key_check(&config, &management_key)?;
        let record = self.database.upsert_encrypted_credential_atomically(
            &CredentialRefInput {
                provider: PASSWORD_VAULT_PROVIDER.into(),
                service: NAMED_SECRET_SERVICE.into(),
                account: random_base64url(16),
                label: Some(name),
                username,
            },
            |reference| seal_credential(value.as_bytes(), &management_key, &reference.id),
        )?;
        self.secrets()?
            .into_iter()
            .find(|secret| secret.id == record.id)
            .ok_or_else(|| VaultError::Database(DbError::NotFound("secret not found".into())))
    }

    /// Rename a named secret, and replace its value when one is given.
    pub fn update_secret(
        &self,
        id: &str,
        input: &SecretInput,
        master_password: &str,
    ) -> VaultResult<Option<PasswordVaultSecret>> {
        let Some(record) = self.secret_record(id)? else {
            return Ok(None);
        };
        let (name, username) =
            self.secret_details(&input.name, input.username.as_deref(), Some(id))?;
        if let Some(value) = &input.value {
            validate_secret_value(value)?;
        }
        let config = self.require_config()?;
        let management_key = unwrap_master_key(master_password, &config)?;
        self.ensure_key_check(&config, &management_key)?;
        match &input.value {
            Some(value) => {
                let secret = seal_credential(value.as_bytes(), &management_key, &record.id)?;
                self.database
                    .upsert_encrypted_credential(&EncryptedCredentialInput {
                        reference: CredentialRefInput {
                            provider: record.provider.clone(),
                            service: record.service.clone(),
                            account: record.account.clone(),
                            label: Some(name),
                            username,
                        },
                        secret,
                    })?;
            }
            None => {
                self.database.update_credential_ref_details(
                    &record.id,
                    PASSWORD_VAULT_PROVIDER,
                    &name,
                    username.as_deref(),
                )?;
            }
        }
        Ok(self
            .secrets()?
            .into_iter()
            .find(|secret| secret.id == record.id))
    }

    pub fn delete_credential(&self, id: &str) -> VaultResult<bool> {
        Ok(self
            .database
            .delete_encrypted_credential(id, PASSWORD_VAULT_PROVIDER)?)
    }

    /// Remove a saved SSH password by its vault account; needs no vault key.
    pub fn delete_ssh_password(&self, account: &str) -> VaultResult<bool> {
        let Some(record) = self.database.encrypted_credential(
            PASSWORD_VAULT_PROVIDER,
            SSH_PASSWORD_SERVICE,
            account,
        )?
        else {
            return Ok(false);
        };
        Ok(self
            .database
            .delete_encrypted_credential(&record.id, PASSWORD_VAULT_PROVIDER)?)
    }

    /// Keep a saved password's display label in step with a folder rename.
    pub fn relabel_ssh_password(&self, account: &str, label: &str) -> VaultResult<()> {
        self.database.update_credential_ref_label(
            PASSWORD_VAULT_PROVIDER,
            SSH_PASSWORD_SERVICE,
            account,
            label,
        )?;
        Ok(())
    }

    /// Reset the vault: every saved value and the vault itself.
    pub fn delete_all(&self) -> VaultResult<()> {
        let config = self
            .database
            .password_vault_config()?
            .map(|record| record.config);
        let automatic =
            config.filter(|config| config.unlock_policy == PasswordVaultUnlockPolicy::Never);
        if let Some(config) = &automatic {
            self.database
                .queue_password_vault_key_cleanup(&config.vault_id)?;
        }
        self.lock();
        self.database
            .delete_password_vault_data(PASSWORD_VAULT_PROVIDER)?;
        if let Some(config) = &automatic {
            self.delete_pending_os_key_best_effort(&config.vault_id)?;
        }
        Ok(())
    }

    fn secret_record(&self, id: &str) -> VaultResult<Option<EncryptedCredentialRecord>> {
        Ok(self
            .database
            .encrypted_credential_by_id(id, PASSWORD_VAULT_PROVIDER)?
            .filter(|record| record.service == NAMED_SECRET_SERVICE))
    }

    /// A trimmed name no other secret uses, and the optional user name.
    fn secret_details(
        &self,
        name: &str,
        username: Option<&str>,
        id: Option<&str>,
    ) -> VaultResult<(String, Option<String>)> {
        let name = js::js_trim(name).to_owned();
        let username = username
            .map(js::js_trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if name.is_empty() {
            return Err(VaultError::InvalidSecret("Give the secret a name.".into()));
        }
        if js::code_point_len(&name) > VAULT_SECRET_NAME_MAX_LENGTH {
            return Err(VaultError::InvalidSecret(
                "The secret name is too long.".into(),
            ));
        }
        if username
            .as_deref()
            .is_some_and(|username| js::code_point_len(username) > VAULT_SECRET_USERNAME_MAX_LENGTH)
        {
            return Err(VaultError::InvalidSecret(
                "The user name is too long.".into(),
            ));
        }
        let key = name.to_lowercase();
        if self
            .secrets()?
            .iter()
            .any(|secret| Some(secret.id.as_str()) != id && secret.name.to_lowercase() == key)
        {
            return Err(VaultError::SecretNameTaken(name));
        }
        Ok((name, username))
    }

    fn require_config(&self) -> VaultResult<PasswordVaultConfig> {
        self.database
            .password_vault_config()?
            .map(|record| record.config)
            .ok_or(VaultError::NotConfigured)
    }

    fn credential_key(&self, master_password: Option<&str>) -> VaultResult<CredentialKey> {
        let config = self.require_config()?;
        if let Some(key) = &self.state().key {
            return Ok(CredentialKey { key: key.clone() });
        }
        let Some(master_password) = master_password else {
            if config.unlock_policy == PasswordVaultUnlockPolicy::Never {
                return Err(VaultError::AutomaticAccess);
            }
            return Err(VaultError::UnlockRequired(config.unlock_policy));
        };
        let vault_key = unwrap_master_key(master_password, &config)?;
        let checked = self.ensure_key_check(&config, &vault_key)?;
        if checked.unlock_policy == PasswordVaultUnlockPolicy::Credential {
            return Ok(CredentialKey { key: vault_key });
        }
        if checked.unlock_policy == PasswordVaultUnlockPolicy::Never {
            self.store_os_key_best_effort(&checked.vault_id, &vault_key);
        }
        self.set_key(&vault_key);
        Ok(CredentialKey { key: vault_key })
    }

    fn set_key(&self, key: &[u8; KEY_BYTES]) {
        self.state().key = Some(Zeroizing::new(*key));
    }

    fn store_os_key(&self, vault_id: &str, key: &[u8; KEY_BYTES]) -> VaultResult<()> {
        match self.key_store.set(vault_id, key) {
            Ok(()) => {
                self.state().os_key_store_available = true;
                Ok(())
            }
            Err(error) => {
                self.state().os_key_store_available = false;
                Err(error.into())
            }
        }
    }

    /// The master-password copy remains authoritative. A cache write must not
    /// discard a key that was successfully unwrapped.
    fn store_os_key_best_effort(&self, vault_id: &str, key: &[u8; KEY_BYTES]) {
        let _ = self.store_os_key(vault_id, key);
    }

    fn retry_pending_os_key_cleanup(
        &self,
        active_automatic_vault_id: Option<&str>,
    ) -> VaultResult<()> {
        for vault_id in self.database.pending_password_vault_key_cleanup()? {
            if Some(vault_id.as_str()) == active_automatic_vault_id {
                // The policy change never committed, so automatic access is
                // still the configured policy and its key must remain available.
                self.database.finish_password_vault_key_cleanup(&vault_id)?;
                continue;
            }
            self.delete_pending_os_key_best_effort(&vault_id)?;
        }
        Ok(())
    }

    /// Moving away from automatic access and resetting the vault are recovery
    /// paths; an unavailable credential store must not block either action.
    /// The durable queue retries deletion on the next initialization.
    fn delete_pending_os_key_best_effort(&self, vault_id: &str) -> VaultResult<()> {
        match self.key_store.delete(vault_id) {
            Ok(()) => {
                self.state().os_key_store_available = true;
                self.database.finish_password_vault_key_cleanup(vault_id)?;
            }
            Err(_) => self.state().os_key_store_available = false,
        }
        Ok(())
    }

    /// Whether a key from the OS store is this vault's key.
    fn is_verified_key(
        &self,
        config: &PasswordVaultConfig,
        key: &[u8; KEY_BYTES],
    ) -> VaultResult<bool> {
        if let Some(check) = &config.key_check {
            return Ok(crypto::same(&crypto::vault_key_check(key), check));
        }
        // Draft vaults predate the durable key check. An existing credential
        // can authenticate their stored key once; empty vaults require the
        // master password before automatic access is enabled.
        let records = self
            .database
            .list_encrypted_credentials(PASSWORD_VAULT_PROVIDER, SSH_PASSWORD_SERVICE)?;
        let Some(record) = records.first() else {
            return Ok(false);
        };
        Ok(crypto::open(
            &record.nonce,
            &record.ciphertext,
            &record.auth_tag,
            key,
            &crypto::credential_aad(&record.id),
        )
        .is_some())
    }

    /// Store the key check when it is missing or stale.
    fn ensure_key_check(
        &self,
        config: &PasswordVaultConfig,
        key: &[u8; KEY_BYTES],
    ) -> VaultResult<PasswordVaultConfig> {
        let check = crypto::vault_key_check(key);
        if config
            .key_check
            .as_deref()
            .is_some_and(|stored| crypto::same(&check, stored))
        {
            return Ok(config.clone());
        }
        let next = PasswordVaultConfig {
            key_check: Some(check),
            ..config.clone()
        };
        self.database.update_password_vault_config(&next)?;
        Ok(next)
    }
}

fn to_key(bytes: &[u8]) -> Option<SecretKey> {
    let array: [u8; KEY_BYTES] = bytes.try_into().ok()?;
    Some(Zeroizing::new(array))
}

fn seal_credential(
    plaintext: &[u8],
    key: &[u8; KEY_BYTES],
    id: &str,
) -> Result<EncryptedCredentialSecret, VaultError> {
    let sealed = crypto::seal(plaintext, key, &crypto::credential_aad(id))?;
    Ok(EncryptedCredentialSecret {
        format_version: CREDENTIAL_FORMAT_VERSION,
        nonce: sealed.nonce,
        ciphertext: sealed.ciphertext,
        auth_tag: sealed.auth_tag,
    })
}

fn decrypt_record(
    record: &EncryptedCredentialRecord,
    key: &[u8; KEY_BYTES],
) -> VaultResult<Zeroizing<String>> {
    let plaintext = crypto::open(
        &record.nonce,
        &record.ciphertext,
        &record.auth_tag,
        key,
        &crypto::credential_aad(&record.id),
    )
    .ok_or(VaultError::CredentialCorrupt)?;
    // Node decodes invalid UTF-8 with replacement characters.
    Ok(Zeroizing::new(
        String::from_utf8_lossy(&plaintext).into_owned(),
    ))
}

/// Derive the master key and unwrap the vault key with it.
fn unwrap_master_key(
    master_password: &str,
    config: &PasswordVaultConfig,
) -> VaultResult<SecretKey> {
    validate_master_password_length(
        master_password,
        if config.format_version == 2 {
            LEGACY_MASTER_PASSWORD_MIN_LENGTH
        } else {
            MASTER_PASSWORD_MIN_LENGTH
        },
    )?;
    let kdf = PasswordVaultKdf {
        cost: u32::try_from(config.kdf_cost)
            .map_err(|_| VaultError::Kdf("Invalid scrypt params".into()))?,
        block_size: u32::try_from(config.kdf_block_size)
            .map_err(|_| VaultError::Kdf("Invalid scrypt params".into()))?,
        parallelism: u32::try_from(config.kdf_parallelism)
            .map_err(|_| VaultError::Kdf("Invalid scrypt params".into()))?,
    };
    let master_key = crypto::derive_key(master_password, &config.kdf_salt, kdf)?;
    let aad = if config.format_version == 2 {
        LEGACY_MASTER_KEY_AAD
    } else {
        MASTER_KEY_AAD
    };
    let key = crypto::open(
        &config.master_key_nonce,
        &config.master_key_ciphertext,
        &config.master_key_tag,
        master_key.as_ref(),
        aad,
    )
    .ok_or(VaultError::InvalidMasterPassword)?;
    to_key(&key).ok_or(VaultError::InvalidMasterPassword)
}

pub fn validate_master_password(password: &str) -> VaultResult<()> {
    validate_master_password_length(password, MASTER_PASSWORD_MIN_LENGTH)
}

fn validate_master_password_length(password: &str, minimum: usize) -> VaultResult<()> {
    if js::code_point_len(password) < minimum {
        return Err(VaultError::InvalidMasterPasswordFormat(format!(
            "Master password must contain at least {minimum} characters."
        )));
    }
    if password.len() > MASTER_PASSWORD_MAX_BYTES {
        return Err(VaultError::InvalidMasterPasswordFormat(
            "Master password is too long.".into(),
        ));
    }
    Ok(())
}

fn validate_saved_password(password: &str) -> VaultResult<()> {
    if password.len() > SSH_PASSWORD_MAX_BYTES {
        return Err(VaultError::InvalidSavedPasswordFormat);
    }
    Ok(())
}

fn validate_secret_value(value: &str) -> VaultResult<()> {
    if value.is_empty() {
        return Err(VaultError::InvalidSecret(
            "Enter the secret to save.".into(),
        ));
    }
    if value.len() > SSH_PASSWORD_MAX_BYTES {
        return Err(VaultError::InvalidSecret(
            "The secret is too long to save.".into(),
        ));
    }
    Ok(())
}

/// Wipe and remove the device key file an obsolete draft build left beside
/// the database. Failures are ignored; it is retried on the next start.
fn remove_legacy_device_key(database: &Path) {
    use std::io::{Seek, Write};
    let filename = database
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(LEGACY_DEVICE_KEY_FILENAME);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&filename)
    else {
        return;
    };
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return;
    }
    let wiped = file
        .write_all(&[0u8; KEY_BYTES])
        .and_then(|()| file.set_len(0))
        .and_then(|()| file.rewind())
        .and_then(|()| file.sync_all());
    drop(file);
    if wiped.is_ok() {
        let _ = std::fs::remove_file(&filename);
    }
}

fn json_account(parts: serde_json::Value) -> String {
    URL_SAFE_NO_PAD.encode(
        serde_json::to_string(&parts)
            .expect("JSON array")
            .as_bytes(),
    )
}

/// Identifies one SSH login: base64url of `["user", "host", port]`.
pub fn ssh_password_account(user: &str, host: &str, port: u16) -> String {
    json_account(serde_json::json!([user, host.to_lowercase(), port]))
}

pub fn ssh_password_label(user: &str, host: &str, port: u16) -> String {
    format!("{user}@{host}:{port}")
}

/// A folder's shared password, keyed by the folder-settings row ID rather
/// than its path so renaming the folder keeps the credential. The JSON-array
/// shape can never collide with [`ssh_password_account`]'s triples.
pub fn folder_password_account(folder_settings_id: &str) -> String {
    json_account(serde_json::json!(["folder", folder_settings_id]))
}

pub fn folder_password_label(path: &str) -> String {
    format!("Folder {path}")
}

/// The protocols a desktop or management password belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopPasswordProtocol {
    Rdp,
    Vnc,
    Gnmi,
}

/// Identifies one RDP, VNC or gNMI login. The protocol leads the JSON array,
/// so it can never collide with an SSH triple or a folder password.
/// `localhost:5900` behind two gateways is two machines, and an RDP user name
/// means a different account in each domain, so both are part of the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPasswordTarget {
    pub protocol: DesktopPasswordProtocol,
    pub user: String,
    pub host: String,
    pub port: u16,
    /// Stable gateway identity (`""` for a direct connection).
    pub gateway: String,
    /// How the gateway is shown to people; only the label uses it.
    pub gateway_label: Option<String>,
    /// RDP logon domain; `""` when none is set.
    pub domain: String,
}

impl DesktopPasswordProtocol {
    fn as_str(self) -> &'static str {
        match self {
            DesktopPasswordProtocol::Rdp => "rdp",
            DesktopPasswordProtocol::Vnc => "vnc",
            DesktopPasswordProtocol::Gnmi => "gnmi",
        }
    }
}

pub fn desktop_password_account(target: &DesktopPasswordTarget) -> String {
    json_account(serde_json::json!([
        target.protocol.as_str(),
        target.user,
        target.host.to_lowercase(),
        target.port,
        target.gateway,
        target.domain.to_lowercase(),
    ]))
}

pub fn desktop_password_label(target: &DesktopPasswordTarget) -> String {
    let user = if !target.domain.is_empty() && !target.user.is_empty() {
        format!("{}\\{}", target.domain, target.user)
    } else {
        target.user.clone()
    };
    let name = if user.is_empty() {
        target.host.clone()
    } else {
        format!("{user}@{}", target.host)
    };
    let route = if target.gateway.is_empty() {
        String::new()
    } else {
        let shown = target
            .gateway_label
            .as_deref()
            .filter(|label| !label.is_empty())
            .unwrap_or(&target.gateway);
        format!(" via {shown}")
    };
    let protocol = match target.protocol {
        DesktopPasswordProtocol::Gnmi => "gNMI".to_owned(),
        other => other.as_str().to_uppercase(),
    };
    format!("{protocol} {name}:{}{route}", target.port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_match_the_typescript_encoding() {
        // Buffer.from(JSON.stringify(['alice', 'router.example', 22])).toString('base64url')
        assert_eq!(
            ssh_password_account("alice", "Router.Example", 22),
            "WyJhbGljZSIsInJvdXRlci5leGFtcGxlIiwyMl0"
        );
        assert_eq!(folder_password_account("abc"), "WyJmb2xkZXIiLCJhYmMiXQ");
        let target = DesktopPasswordTarget {
            protocol: DesktopPasswordProtocol::Rdp,
            user: "ci".into(),
            host: "Win".into(),
            port: 3389,
            gateway: "ssh:bastion".into(),
            gateway_label: Some("bastion".into()),
            domain: "LAB".into(),
        };
        assert_eq!(
            desktop_password_label(&target),
            "RDP LAB\\ci@Win:3389 via bastion"
        );
        assert_eq!(
            String::from_utf8(
                URL_SAFE_NO_PAD
                    .decode(desktop_password_account(&target))
                    .unwrap()
            )
            .unwrap(),
            r#"["rdp","ci","win",3389,"ssh:bastion","lab"]"#
        );
    }
}
