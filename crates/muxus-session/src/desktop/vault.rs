//! The store's password vault as the [`DesktopVault`] a desktop logon reads and remembers
//! passwords through. Desktop passwords live under the vault's login-password service next to
//! SSH passwords (`desktopPasswordAccount` keeps their accounts apart).

use std::sync::Arc;

use muxus_rdp::passwords::{DesktopVault, VaultFailure};
use muxus_store::vault::{PasswordVault, VaultError};

/// [`DesktopVault`] over the application's password vault.
#[derive(Clone)]
pub struct StoreDesktopVault(pub Arc<PasswordVault>);

/// Vault work blocks (scrypt, the OS credential store). On the backend's multi-thread runtime
/// it moves the worker aside while it runs; elsewhere (a current-thread test runtime) it just
/// runs.
fn blocking<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

fn failure(error: VaultError) -> VaultFailure {
    match error {
        VaultError::InvalidMasterPassword => VaultFailure::InvalidMasterPassword(error.to_string()),
        VaultError::InvalidMasterPasswordFormat(message) => {
            VaultFailure::InvalidMasterPasswordFormat(message)
        }
        VaultError::CredentialCorrupt => VaultFailure::CredentialCorrupt,
        VaultError::KeyStoreUnavailable(_) => VaultFailure::KeyStoreUnavailable,
        VaultError::AlreadyConfigured => VaultFailure::AlreadyConfigured,
        other => VaultFailure::Other(other.to_string()),
    }
}

impl DesktopVault for StoreDesktopVault {
    fn has_password(&self, account: &str) -> bool {
        self.0.has_ssh_password(account).unwrap_or(false)
    }

    fn configured(&self) -> bool {
        self.0.status().is_ok_and(|status| status.configured)
    }

    fn locked(&self) -> bool {
        self.0.status().map_or(true, |status| status.locked)
    }

    fn password(
        &self,
        account: &str,
        master_password: Option<&str>,
    ) -> Result<Option<String>, VaultFailure> {
        blocking(|| self.0.ssh_password(account, master_password))
            .map(|password| password.map(|password| password.to_string()))
            .map_err(failure)
    }

    fn remember(
        &self,
        account: &str,
        label: &str,
        password: &str,
        master_password: Option<&str>,
    ) -> Result<(), VaultFailure> {
        blocking(|| {
            self.0
                .remember_ssh_password(account, label, password, master_password)
        })
        .map_err(failure)
    }

    fn create(&self, master_password: &str) -> Result<(), VaultFailure> {
        blocking(|| self.0.create(master_password, None)).map_err(failure)
    }
}
