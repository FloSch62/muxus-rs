//! Where a "never prompt" vault keeps its raw key: the operating system's
//! credential store (port of `server/src/security/vault-key-store.ts`).
//!
//! The entry is the one the TypeScript build's `@napi-rs/keyring` writes:
//! service `io.github.flosch62.muxus.password-vault`, the vault id as the
//! account, the 32 raw key bytes as the secret. On Linux the Secret Service
//! is used, falling back to the kernel keyutils keyring when no Secret
//! Service is available, exactly as that binding does; Windows uses the
//! Credential Manager and macOS the Keychain. There is never a fallback to an
//! application-owned file.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use zeroize::Zeroizing;

use super::crypto::KEY_BYTES;

/// The credential-store service name every vault key is filed under.
pub const VAULT_KEY_SERVICE: &str = "io.github.flosch62.muxus.password-vault";

/// The credential store could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "The operating-system credential store is unavailable. Choose a master-password prompt policy or start an unlocked Secret Service-compatible keyring."
)]
pub struct KeyStoreUnavailable {
    /// What the platform reported, for logs.
    pub cause: String,
}

impl KeyStoreUnavailable {
    pub fn new(cause: impl Into<String>) -> Self {
        Self {
            cause: cause.into(),
        }
    }
}

/// Which kind of store backs a [`VaultKeyStore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStoreBackend {
    Os,
    Memory,
}

/// Storage for the raw vault key of vaults with the "never" policy.
/// Methods may block (D-Bus, Keychain); call them off async executors.
pub trait VaultKeyStore: Send + Sync {
    fn backend(&self) -> KeyStoreBackend;
    /// The stored key, `None` when there is none.
    fn get(&self, vault_id: &str) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreUnavailable>;
    fn set(&self, vault_id: &str, key: &[u8]) -> Result<(), KeyStoreUnavailable>;
    fn delete(&self, vault_id: &str) -> Result<(), KeyStoreUnavailable>;
}

/// The operating system's credential store.
#[derive(Debug, Default)]
pub struct SystemVaultKeyStore {
    _private: (),
}

impl SystemVaultKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn entry(&self, vault_id: &str) -> Result<keyring_core::Entry, KeyStoreUnavailable> {
        let store = os_store()?;
        store
            .build(VAULT_KEY_SERVICE, vault_id, None)
            .map_err(|error| KeyStoreUnavailable::new(error.to_string()))
    }
}

/// The platform store, created once: Secret Service first on Linux, then the
/// kernel keyring.
fn os_store() -> Result<Arc<keyring_core::CredentialStore>, KeyStoreUnavailable> {
    static STORE: OnceLock<Result<Arc<keyring_core::CredentialStore>, KeyStoreUnavailable>> =
        OnceLock::new();
    STORE.get_or_init(create_os_store).clone()
}

#[cfg(target_os = "linux")]
fn create_os_store() -> Result<Arc<keyring_core::CredentialStore>, KeyStoreUnavailable> {
    match zbus_secret_service_keyring_store::Store::new() {
        Ok(store) => Ok(store),
        Err(secret_service) => linux_keyutils_keyring_store::Store::new()
            .map(|store| store as Arc<keyring_core::CredentialStore>)
            .map_err(|keyutils| KeyStoreUnavailable::new(format!("{secret_service}; {keyutils}"))),
    }
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))
))]
fn create_os_store() -> Result<Arc<keyring_core::CredentialStore>, KeyStoreUnavailable> {
    zbus_secret_service_keyring_store::Store::new()
        .map(|store| store as Arc<keyring_core::CredentialStore>)
        .map_err(|error| KeyStoreUnavailable::new(error.to_string()))
}

#[cfg(target_os = "macos")]
fn create_os_store() -> Result<Arc<keyring_core::CredentialStore>, KeyStoreUnavailable> {
    apple_native_keyring_store::keychain::Store::new()
        .map(|store| store as Arc<keyring_core::CredentialStore>)
        .map_err(|error| KeyStoreUnavailable::new(error.to_string()))
}

#[cfg(target_os = "windows")]
fn create_os_store() -> Result<Arc<keyring_core::CredentialStore>, KeyStoreUnavailable> {
    windows_native_keyring_store::Store::new()
        .map(|store| store as Arc<keyring_core::CredentialStore>)
        .map_err(|error| KeyStoreUnavailable::new(error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn create_os_store() -> Result<Arc<keyring_core::CredentialStore>, KeyStoreUnavailable> {
    Err(KeyStoreUnavailable::new(
        "no credential store on this platform",
    ))
}

impl VaultKeyStore for SystemVaultKeyStore {
    fn backend(&self) -> KeyStoreBackend {
        KeyStoreBackend::Os
    }

    fn get(&self, vault_id: &str) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreUnavailable> {
        match self.entry(vault_id)?.get_secret() {
            Ok(secret) => {
                let key = Zeroizing::new(secret);
                if key.len() != KEY_BYTES {
                    return Err(KeyStoreUnavailable::new(
                        "The OS credential-store entry has an invalid length.",
                    ));
                }
                Ok(Some(key))
            }
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(KeyStoreUnavailable::new(error.to_string())),
        }
    }

    fn set(&self, vault_id: &str, key: &[u8]) -> Result<(), KeyStoreUnavailable> {
        if key.len() != KEY_BYTES {
            return Err(KeyStoreUnavailable::new(format!(
                "Password-vault key must be {KEY_BYTES} bytes."
            )));
        }
        self.entry(vault_id)?
            .set_secret(key)
            .map_err(|error| KeyStoreUnavailable::new(error.to_string()))
    }

    fn delete(&self, vault_id: &str) -> Result<(), KeyStoreUnavailable> {
        let entry = self.entry(vault_id)?;
        // Overwrite first so even a rejected delete cannot leave the usable
        // vault key behind.
        let replacement = Zeroizing::new([0u8; KEY_BYTES]);
        entry
            .set_secret(replacement.as_ref())
            .map_err(|error| KeyStoreUnavailable::new(error.to_string()))?;
        entry
            .delete_credential()
            .map_err(|error| KeyStoreUnavailable::new(error.to_string()))
    }
}

/// In-memory credential store for tests and ephemeral in-memory databases.
#[derive(Debug, Default)]
pub struct MemoryVaultKeyStore {
    values: Mutex<HashMap<String, Zeroizing<Vec<u8>>>>,
}

impl MemoryVaultKeyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl VaultKeyStore for MemoryVaultKeyStore {
    fn backend(&self) -> KeyStoreBackend {
        KeyStoreBackend::Memory
    }

    fn get(&self, vault_id: &str) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreUnavailable> {
        Ok(self
            .values
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(vault_id)
            .cloned())
    }

    fn set(&self, vault_id: &str, key: &[u8]) -> Result<(), KeyStoreUnavailable> {
        self.values
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(vault_id.to_owned(), Zeroizing::new(key.to_vec()));
        Ok(())
    }

    fn delete(&self, vault_id: &str) -> Result<(), KeyStoreUnavailable> {
        self.values
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(vault_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips a key through the real OS credential store. Opt-in
    /// (`MUXUS_OS_KEYRING_SMOKE=1`) because it needs an unlocked keyring and
    /// writes a real entry; it deletes the entry again.
    #[test]
    fn os_store_smoke() {
        if std::env::var("MUXUS_OS_KEYRING_SMOKE").as_deref() != Ok("1") {
            return;
        }
        let store = SystemVaultKeyStore::new();
        let mut id = [0u8; 8];
        crate::ids::random_bytes(&mut id);
        let vault_id = format!(
            "os-smoke-{}",
            id.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        assert_eq!(store.get(&vault_id).unwrap(), None);
        let key = [7u8; 32];
        store.set(&vault_id, &key).unwrap();
        assert_eq!(
            store.get(&vault_id).unwrap().as_deref().map(Vec::as_slice),
            Some(&key[..])
        );
        store.set(&vault_id, &[9u8; 32]).unwrap();
        assert_eq!(
            store.get(&vault_id).unwrap().as_deref().map(Vec::as_slice),
            Some(&[9u8; 32][..])
        );
        store.delete(&vault_id).unwrap();
        assert_eq!(store.get(&vault_id).unwrap(), None);
    }
}
