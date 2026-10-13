//! Master-password lifecycle and saved-credential management
//! (`server/src/routes/password-vault.ts`).

use muxus_core::api::{
    PasswordVaultStatus, PasswordVaultUnlockPolicy, SendVaultSecretRequest, SendVaultSecretResult,
    VaultSecretInput, VaultSecretSaveResult,
};
use muxus_core::js::utf16_len;
use muxus_store::vault::{
    SecretInput, VAULT_SECRET_NAME_MAX_LENGTH, VAULT_SECRET_USERNAME_MAX_LENGTH, VaultError,
    secret_input::secret_keystrokes,
};

use crate::backend::{Backend, Shared};
use crate::error::{ApiError, ApiResult};

fn valid_master(password: &str) -> bool {
    !password.is_empty() && utf16_len(password) <= 1024
}

fn valid_credential_id(id: &str) -> bool {
    !id.is_empty() && utf16_len(id) <= 200
}

fn master_required() -> ApiError {
    ApiError::bad_request("A valid master password is required.")
}

fn secret_missing() -> ApiError {
    ApiError::with_code(
        404,
        "The secret is no longer in the password vault.",
        "vault-secret-missing",
    )
}

fn terminal_unavailable() -> ApiError {
    ApiError::with_code(
        409,
        "The session is no longer connected.",
        "terminal-unavailable",
    )
}

fn status(shared: &Shared) -> ApiResult<PasswordVaultStatus> {
    Ok(shared.vault.status()?)
}

/// The zod checks of `secretSchema` (lengths are checked again, in characters, by the vault).
fn valid_secret(input: &VaultSecretInput) -> bool {
    valid_master(&input.master_password)
        && utf16_len(&input.name) <= VAULT_SECRET_NAME_MAX_LENGTH * 4
        && input
            .username
            .as_deref()
            .is_none_or(|username| utf16_len(username) <= VAULT_SECRET_USERNAME_MAX_LENGTH * 4)
        && input
            .value
            .as_deref()
            .is_none_or(|value| utf16_len(value) <= 8192)
}

impl Backend {
    /// `GET /api/password-vault`.
    pub async fn vault_status(&self) -> ApiResult<PasswordVaultStatus> {
        self.blocking(status).await
    }

    /// `POST /api/password-vault/create`; the policy defaults to the vault's default.
    pub async fn create_vault(
        &self,
        password: &str,
        unlock_policy: Option<PasswordVaultUnlockPolicy>,
    ) -> ApiResult<PasswordVaultStatus> {
        if !valid_master(password) {
            return Err(master_required());
        }
        let password = password.to_string();
        self.blocking(move |shared| {
            shared.vault.create(&password, unlock_policy)?;
            status(shared)
        })
        .await
    }

    /// `POST /api/password-vault/repair-automatic-access`.
    pub async fn repair_vault_automatic_access(
        &self,
        master_password: &str,
    ) -> ApiResult<PasswordVaultStatus> {
        if !valid_master(master_password) {
            return Err(master_required());
        }
        let master = master_password.to_string();
        self.blocking(move |shared| {
            shared.vault.repair_automatic_access(&master)?;
            status(shared)
        })
        .await
    }

    /// `POST /api/password-vault/unlock`.
    pub async fn unlock_vault(&self, master_password: &str) -> ApiResult<PasswordVaultStatus> {
        if !valid_master(master_password) {
            return Err(master_required());
        }
        let master = master_password.to_string();
        self.blocking(move |shared| {
            shared.vault.unlock_for_session(&master)?;
            status(shared)
        })
        .await
    }

    /// `PUT /api/password-vault/unlock-policy`.
    pub async fn change_vault_unlock_policy(
        &self,
        master_password: &str,
        unlock_policy: PasswordVaultUnlockPolicy,
    ) -> ApiResult<PasswordVaultStatus> {
        if !valid_master(master_password) {
            return Err(ApiError::bad_request(
                "A valid master password and unlock policy are required.",
            ));
        }
        let master = master_password.to_string();
        self.blocking(move |shared| {
            shared.vault.change_unlock_policy(&master, unlock_policy)?;
            status(shared)
        })
        .await
    }

    /// `POST /api/password-vault/change-master-password`.
    pub async fn change_vault_master_password(
        &self,
        current_password: &str,
        next_password: &str,
    ) -> ApiResult<PasswordVaultStatus> {
        if !valid_master(current_password) || !valid_master(next_password) {
            return Err(ApiError::bad_request(
                "Current and new master passwords are required.",
            ));
        }
        let (current, next) = (current_password.to_string(), next_password.to_string());
        self.blocking(move |shared| {
            shared.vault.change_master_password(&current, &next)?;
            status(shared)
        })
        .await
    }

    /// `POST /api/password-vault/credentials/:id/reveal`.
    pub async fn reveal_vault_credential(
        &self,
        id: &str,
        master_password: &str,
    ) -> ApiResult<String> {
        if !valid_credential_id(id) || !valid_master(master_password) {
            return Err(ApiError::bad_request(
                "A valid credential and master password are required.",
            ));
        }
        let (id, master) = (id.to_string(), master_password.to_string());
        self.blocking(move |shared| {
            shared
                .vault
                .reveal_credential(&id, &master)?
                .map(|password| password.to_string())
                .ok_or_else(|| ApiError::not_found("Saved password not found."))
        })
        .await
    }

    /// `PUT /api/password-vault/credentials/:id`.
    pub async fn update_vault_credential(
        &self,
        id: &str,
        master_password: &str,
        password: &str,
    ) -> ApiResult<PasswordVaultStatus> {
        if !valid_credential_id(id) || !valid_master(master_password) || utf16_len(password) > 8192
        {
            return Err(ApiError::bad_request(
                "A valid credential, master password and password are required.",
            ));
        }
        let (id, master, password) = (
            id.to_string(),
            master_password.to_string(),
            password.to_string(),
        );
        self.blocking(move |shared| {
            if !shared.vault.update_credential(&id, &master, &password)? {
                return Err(ApiError::not_found("Saved password not found."));
            }
            status(shared)
        })
        .await
    }

    /// `DELETE /api/password-vault/credentials/:id`.
    pub async fn delete_vault_credential(&self, id: &str) -> ApiResult<bool> {
        let id = id.to_string();
        self.blocking(move |shared| {
            Ok(valid_credential_id(&id) && shared.vault.delete_credential(&id)?)
        })
        .await
    }

    /// `POST /api/password-vault/secrets`.
    pub async fn create_vault_secret(
        &self,
        input: VaultSecretInput,
    ) -> ApiResult<VaultSecretSaveResult> {
        if !valid_secret(&input) || input.value.is_none() {
            return Err(ApiError::bad_request(
                "A name, a secret and the master password are required.",
            ));
        }
        self.blocking(move |shared| {
            let secret = shared.vault.create_secret(
                &SecretInput {
                    name: input.name,
                    username: input.username,
                    value: input.value,
                },
                &input.master_password,
            )?;
            Ok(VaultSecretSaveResult {
                secret,
                status: status(shared)?,
            })
        })
        .await
    }

    /// `PUT /api/password-vault/secrets/:id`; an absent value keeps the saved one.
    pub async fn update_vault_secret(
        &self,
        id: &str,
        input: VaultSecretInput,
    ) -> ApiResult<VaultSecretSaveResult> {
        if !valid_credential_id(id) || !valid_secret(&input) {
            return Err(ApiError::bad_request(
                "A valid secret, name and master password are required.",
            ));
        }
        let id = id.to_string();
        self.blocking(move |shared| {
            let secret = shared
                .vault
                .update_secret(
                    &id,
                    &SecretInput {
                        name: input.name,
                        username: input.username,
                        value: input.value,
                    },
                    &input.master_password,
                )?
                .ok_or_else(secret_missing)?;
            Ok(VaultSecretSaveResult {
                secret,
                status: status(shared)?,
            })
        })
        .await
    }

    /// `POST /api/password-vault/secrets/:id/send`: types a secret into live sessions on the
    /// backend, so the value never reaches the UI, the request log, session history or a log
    /// file.
    pub async fn send_vault_secret(
        &self,
        id: &str,
        request: SendVaultSecretRequest,
    ) -> ApiResult<SendVaultSecretResult> {
        let valid = valid_credential_id(id)
            && !request.terminal_ids.is_empty()
            && request.terminal_ids.len() <= 256
            && request
                .terminal_ids
                .iter()
                .all(|terminal| !terminal.is_empty() && utf16_len(terminal) <= 200)
            && request.master_password.as_deref().is_none_or(valid_master);
        if !valid {
            return Err(ApiError::bad_request(
                "A valid secret and terminal session are required.",
            ));
        }
        let id = id.to_string();
        self.blocking(move |shared| {
            if !shared.vault.has_secret(&id)? {
                return Err(secret_missing());
            }
            let mut seen = std::collections::HashSet::new();
            let writers: Vec<_> = request
                .terminal_ids
                .iter()
                .filter(|terminal| seen.insert(terminal.as_str()))
                .filter_map(|terminal| shared.terminal_inputs.writer(terminal))
                .collect();
            if writers.is_empty() {
                return Err(terminal_unavailable());
            }
            let value = match shared
                .vault
                .secret_value(&id, request.master_password.as_deref())
            {
                Ok(Some(value)) => value,
                Ok(None) => return Err(secret_missing()),
                Err(VaultError::CredentialCorrupt) => {
                    return Err(ApiError::with_code(
                        422,
                        "The saved secret could not be decrypted.",
                        "vault-secret-corrupt",
                    ));
                }
                Err(error) => return Err(error.into()),
            };
            let keystrokes = secret_keystrokes(&value, request.enter);
            let sent = writers.iter().filter(|write| write(&keystrokes)).count();
            if sent == 0 {
                return Err(terminal_unavailable());
            }
            Ok(SendVaultSecretResult { sent })
        })
        .await
    }

    /// `DELETE /api/password-vault`: master-password-free on purpose. Reset is the recovery
    /// path when the password is forgotten; it can only delete, never reveal data.
    pub async fn reset_vault(&self) -> ApiResult<PasswordVaultStatus> {
        self.blocking(|shared| {
            shared.vault.delete_all()?;
            status(shared)
        })
        .await
    }
}
