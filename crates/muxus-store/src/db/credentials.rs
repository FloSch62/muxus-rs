//! Credential references, the password vault header and encrypted secrets.
//!
//! These rows hold only ciphertext and metadata; the vault module owns the
//! cryptography.

use muxus_core::api::PasswordVaultUnlockPolicy;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Database, DbError, DbResult, require_non_empty, trimmed_or_none};
use crate::ids::nanoid;

/// Where a secret lives: a logical provider, a service namespace and an account.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CredentialRefInput {
    /// Logical credential provider, for example "muxus-master-vault".
    pub provider: String,
    /// Stable application/service namespace in that provider.
    pub service: String,
    /// Account/key used to retrieve the secret from the provider.
    pub account: String,
    pub label: Option<String>,
    /// Plain account name shown beside a named secret.
    pub username: Option<String>,
}

/// A stored credential reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRefRecord {
    pub id: String,
    pub provider: String,
    pub service: String,
    pub account: String,
    pub label: Option<String>,
    pub username: Option<String>,
}

/// The `password_vault` row: the KDF parameters and the master-password
/// wrapped vault key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordVaultConfig {
    /// 2 (draft vaults) or 3.
    pub format_version: i64,
    pub vault_id: String,
    pub unlock_policy: PasswordVaultUnlockPolicy,
    /// Always "scrypt".
    pub kdf_algorithm: String,
    pub kdf_salt: Vec<u8>,
    pub kdf_cost: i64,
    pub kdf_block_size: i64,
    pub kdf_parallelism: i64,
    pub master_key_nonce: Vec<u8>,
    pub master_key_ciphertext: Vec<u8>,
    pub master_key_tag: Vec<u8>,
    /// HMAC proof for rejecting stale or corrupted OS credential-store keys.
    pub key_check: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordVaultConfigRecord {
    pub config: PasswordVaultConfig,
    pub created_at: String,
    pub updated_at: String,
}

/// One sealed value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedCredentialSecret {
    /// Always 1.
    pub format_version: i64,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub auth_tag: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedCredentialInput {
    pub reference: CredentialRefInput,
    pub secret: EncryptedCredentialSecret,
}

/// A credential reference joined with its sealed value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedCredentialRecord {
    pub id: String,
    pub provider: String,
    pub service: String,
    pub account: String,
    pub label: Option<String>,
    pub username: Option<String>,
    pub format_version: i64,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub auth_tag: Vec<u8>,
    pub created_at: String,
    pub updated_at: String,
}

const ENCRYPTED_COLUMNS: &str = "
        SELECT refs.id, refs.provider, refs.service, refs.account, refs.label,
               refs.username, refs.created_at, secrets.updated_at, secrets.format_version,
               secrets.nonce, secrets.ciphertext, secrets.auth_tag
        FROM credential_refs AS refs
        JOIN credential_secrets AS secrets ON secrets.credential_ref_id = refs.id";

fn encrypted_from_row(row: &Row<'_>) -> rusqlite::Result<EncryptedCredentialRecord> {
    Ok(EncryptedCredentialRecord {
        id: row.get("id")?,
        provider: row.get("provider")?,
        service: row.get("service")?,
        account: row.get("account")?,
        label: row.get("label")?,
        username: row.get("username")?,
        format_version: row.get("format_version")?,
        nonce: row.get("nonce")?,
        ciphertext: row.get("ciphertext")?,
        auth_tag: row.get("auth_tag")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn vault_config_from_row(row: &Row<'_>) -> rusqlite::Result<PasswordVaultConfigRecord> {
    let policy: String = row.get("unlock_policy")?;
    Ok(PasswordVaultConfigRecord {
        config: PasswordVaultConfig {
            format_version: row.get("format_version")?,
            vault_id: row.get("vault_id")?,
            unlock_policy: PasswordVaultUnlockPolicy::parse(&policy).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    format!("unknown unlock policy {policy}").into(),
                )
            })?,
            kdf_algorithm: row.get("kdf_algorithm")?,
            kdf_salt: row.get("kdf_salt")?,
            kdf_cost: row.get("kdf_cost")?,
            kdf_block_size: row.get("kdf_block_size")?,
            kdf_parallelism: row.get("kdf_parallelism")?,
            master_key_nonce: row.get("master_key_nonce")?,
            master_key_ciphertext: row.get("master_key_ciphertext")?,
            master_key_tag: row.get("master_key_tag")?,
            key_check: row.get("key_check")?,
        },
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn upsert_credential_ref(
    conn: &Connection,
    input: &CredentialRefInput,
) -> DbResult<CredentialRefRecord> {
    require_non_empty(&input.provider, "provider")?;
    require_non_empty(&input.service, "service")?;
    require_non_empty(&input.account, "account")?;
    let existing: Option<String> = conn
        .query_row(
            "
        SELECT id FROM credential_refs
        WHERE provider = ? AND service = ? AND account = ?
      ",
            params![input.provider, input.service, input.account],
            |row| row.get(0),
        )
        .optional()?;
    let id = existing.unwrap_or_else(nanoid);
    let label = trimmed_or_none(input.label.as_deref());
    let username = trimmed_or_none(input.username.as_deref());
    conn.execute(
        "
        INSERT INTO credential_refs(id, provider, service, account, label, username)
        VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT(provider, service, account) DO UPDATE SET
          label = excluded.label,
          username = excluded.username,
          updated_at = CURRENT_TIMESTAMP
      ",
        params![
            id,
            input.provider,
            input.service,
            input.account,
            label,
            username
        ],
    )?;
    Ok(CredentialRefRecord {
        id,
        provider: input.provider.clone(),
        service: input.service.clone(),
        account: input.account.clone(),
        label,
        username,
    })
}

fn encrypted_credential(
    conn: &Connection,
    provider: &str,
    service: &str,
    account: &str,
) -> DbResult<Option<EncryptedCredentialRecord>> {
    Ok(conn
        .query_row(
            &format!("{ENCRYPTED_COLUMNS}\n        WHERE refs.provider = ? AND refs.service = ? AND refs.account = ?"),
            params![provider, service, account],
            encrypted_from_row,
        )
        .optional()?)
}

impl Database {
    /// Create or relabel the reference for (provider, service, account).
    pub fn upsert_credential_ref(
        &self,
        input: &CredentialRefInput,
    ) -> DbResult<CredentialRefRecord> {
        upsert_credential_ref(&self.lock(), input)
    }

    pub fn password_vault_config(&self) -> DbResult<Option<PasswordVaultConfigRecord>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT * FROM password_vault WHERE singleton = 1",
                [],
                vault_config_from_row,
            )
            .optional()?)
    }

    pub fn create_password_vault_config(&self, input: &PasswordVaultConfig) -> DbResult<()> {
        self.lock().execute(
            "
        INSERT INTO password_vault(
          singleton, format_version, vault_id, unlock_policy,
          kdf_algorithm, kdf_salt, kdf_cost,
          kdf_block_size, kdf_parallelism, master_key_nonce,
          master_key_ciphertext, master_key_tag, key_check
        ) VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      ",
            params![
                input.format_version,
                input.vault_id,
                input.unlock_policy.as_str(),
                input.kdf_algorithm,
                input.kdf_salt,
                input.kdf_cost,
                input.kdf_block_size,
                input.kdf_parallelism,
                input.master_key_nonce,
                input.master_key_ciphertext,
                input.master_key_tag,
                input.key_check,
            ],
        )?;
        Ok(())
    }

    pub fn update_password_vault_config(&self, input: &PasswordVaultConfig) -> DbResult<()> {
        let conn = self.lock();
        let changed = conn.execute(
            "
        UPDATE password_vault
        SET format_version = ?, vault_id = ?, unlock_policy = ?,
            kdf_algorithm = ?, kdf_salt = ?,
            kdf_cost = ?, kdf_block_size = ?, kdf_parallelism = ?,
            master_key_nonce = ?, master_key_ciphertext = ?,
            master_key_tag = ?, key_check = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE singleton = 1
      ",
            params![
                input.format_version,
                input.vault_id,
                input.unlock_policy.as_str(),
                input.kdf_algorithm,
                input.kdf_salt,
                input.kdf_cost,
                input.kdf_block_size,
                input.kdf_parallelism,
                input.master_key_nonce,
                input.master_key_ciphertext,
                input.master_key_tag,
                input.key_check,
            ],
        )?;
        if changed != 1 {
            return Err(DbError::NotFound("password vault is not configured".into()));
        }
        self.flush_sensitive_pages(&conn, false)
    }

    /// Vault ids whose OS credential-store key still has to be deleted.
    pub fn pending_password_vault_key_cleanup(&self) -> DbResult<Vec<String>> {
        let conn = self.lock();
        let mut statement =
            conn.prepare("SELECT vault_id FROM password_vault_key_cleanup ORDER BY created_at")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn queue_password_vault_key_cleanup(&self, vault_id: &str) -> DbResult<()> {
        self.lock().execute(
            "INSERT OR IGNORE INTO password_vault_key_cleanup(vault_id) VALUES (?)",
            [vault_id],
        )?;
        Ok(())
    }

    pub fn finish_password_vault_key_cleanup(&self, vault_id: &str) -> DbResult<()> {
        self.lock().execute(
            "DELETE FROM password_vault_key_cleanup WHERE vault_id = ?",
            [vault_id],
        )?;
        Ok(())
    }

    pub fn encrypted_credential(
        &self,
        provider: &str,
        service: &str,
        account: &str,
    ) -> DbResult<Option<EncryptedCredentialRecord>> {
        encrypted_credential(&self.lock(), provider, service, account)
    }

    pub fn encrypted_credential_by_id(
        &self,
        id: &str,
        provider: &str,
    ) -> DbResult<Option<EncryptedCredentialRecord>> {
        Ok(self
            .lock()
            .query_row(
                &format!("{ENCRYPTED_COLUMNS}\n        WHERE refs.id = ? AND refs.provider = ?"),
                params![id, provider],
                encrypted_from_row,
            )
            .optional()?)
    }

    /// Every sealed value of one service, by label (case-insensitive) then account.
    pub fn list_encrypted_credentials(
        &self,
        provider: &str,
        service: &str,
    ) -> DbResult<Vec<EncryptedCredentialRecord>> {
        let conn = self.lock();
        let mut statement = conn.prepare(&format!(
            "{ENCRYPTED_COLUMNS}\n        WHERE refs.provider = ? AND refs.service = ?\n        ORDER BY refs.label COLLATE NOCASE, refs.account"
        ))?;
        let rows = statement.query_map(params![provider, service], encrypted_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn upsert_encrypted_credential(
        &self,
        input: &EncryptedCredentialInput,
    ) -> DbResult<EncryptedCredentialRecord> {
        let secret = input.secret.clone();
        self.upsert_encrypted_credential_atomically(&input.reference, |_| Ok::<_, DbError>(secret))
    }

    /// Reserve the credential reference and create its authenticated
    /// ciphertext in one transaction. If sealing fails, no reference-only row
    /// survives. `seal` receives the reference (its id is bound into the
    /// ciphertext's associated data).
    pub fn upsert_encrypted_credential_atomically<E: From<DbError>>(
        &self,
        input: &CredentialRefInput,
        seal: impl FnOnce(&CredentialRefRecord) -> Result<EncryptedCredentialSecret, E>,
    ) -> Result<EncryptedCredentialRecord, E> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(DbError::from)?;
        let reference = upsert_credential_ref(&tx, input)?;
        let encrypted = seal(&reference)?;
        tx.execute(
            "
          INSERT INTO credential_secrets(
            credential_ref_id, format_version, nonce, ciphertext, auth_tag
          ) VALUES (?, ?, ?, ?, ?)
          ON CONFLICT(credential_ref_id) DO UPDATE SET
            format_version = excluded.format_version,
            nonce = excluded.nonce,
            ciphertext = excluded.ciphertext,
            auth_tag = excluded.auth_tag,
            updated_at = CURRENT_TIMESTAMP
        ",
            params![
                reference.id,
                encrypted.format_version,
                encrypted.nonce,
                encrypted.ciphertext,
                encrypted.auth_tag
            ],
        )
        .map_err(DbError::from)?;
        tx.commit().map_err(DbError::from)?;
        self.flush_sensitive_pages(&conn, false)?;
        let record = encrypted_credential(&conn, &input.provider, &input.service, &input.account)?;
        Ok(record.ok_or_else(|| DbError::NotFound("credential not found".into()))?)
    }

    /// Refresh a credential's display label (a folder password after a rename).
    pub fn update_credential_ref_label(
        &self,
        provider: &str,
        service: &str,
        account: &str,
        label: &str,
    ) -> DbResult<bool> {
        Ok(self.lock().execute(
            "
            UPDATE credential_refs
            SET label = ?, updated_at = CURRENT_TIMESTAMP
            WHERE provider = ? AND service = ? AND account = ?
          ",
            params![label, provider, service, account],
        )? == 1)
    }

    /// Rename a credential and change its user name; the ciphertext stays as it is.
    pub fn update_credential_ref_details(
        &self,
        id: &str,
        provider: &str,
        label: &str,
        username: Option<&str>,
    ) -> DbResult<bool> {
        Ok(self.lock().execute(
            "
            UPDATE credential_refs
            SET label = ?, username = ?, updated_at = CURRENT_TIMESTAMP
            WHERE id = ? AND provider = ?
          ",
            params![
                muxus_core::js::js_trim(label),
                trimmed_or_none(username),
                id,
                provider
            ],
        )? == 1)
    }

    pub fn delete_encrypted_credential(&self, id: &str, provider: &str) -> DbResult<bool> {
        let conn = self.lock();
        let deleted = conn.execute(
            "DELETE FROM credential_refs WHERE id = ? AND provider = ?",
            params![id, provider],
        )? == 1;
        if deleted {
            self.flush_sensitive_pages(&conn, false)?;
        }
        Ok(deleted)
    }

    /// Delete every credential of a provider and the vault header, then
    /// compact the file so no sealed value survives in free pages.
    pub fn delete_password_vault_data(&self, provider: &str) -> DbResult<()> {
        self.immediate(|conn| {
            conn.execute("DELETE FROM credential_refs WHERE provider = ?", [provider])?;
            conn.execute("DELETE FROM password_vault WHERE singleton = 1", [])?;
            Ok(())
        })?;
        let conn = self.lock();
        self.flush_sensitive_pages(&conn, true)
    }
}
