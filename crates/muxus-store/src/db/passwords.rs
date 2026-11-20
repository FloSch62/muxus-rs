//! Connected password managers (`password_manager_sources` in database.ts): which manager
//! and how it is reached. Never a password; hosts and folders keep a reference to an entry.

use muxus_core::api::PasswordManagerConfig;
use rusqlite::{OptionalExtension, Row, params};

use super::{Database, DbResult, assert_secret_free, require_non_empty};
use crate::ids::nanoid;

/// One connected password manager, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordManagerSourceRow {
    pub id: String,
    /// The kind as stored: a name [`muxus_core::api::PasswordManagerKind`] knows, unless a
    /// newer build wrote one this build does not have.
    pub kind: String,
    pub name: String,
    pub config: PasswordManagerConfig,
    pub created_at: String,
    pub updated_at: String,
}

fn source_from_row(row: &Row<'_>) -> rusqlite::Result<PasswordManagerSourceRow> {
    let config: String = row.get("config_json")?;
    Ok(PasswordManagerSourceRow {
        id: row.get("id")?,
        kind: row.get("kind")?,
        name: row.get("name")?,
        config: serde_json::from_str(&config).unwrap_or_default(),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn config_json(config: &PasswordManagerConfig) -> DbResult<String> {
    let value = serde_json::to_value(config)?;
    assert_secret_free(&value, "passwordManager.config")?;
    Ok(serde_json::to_string(config)?)
}

impl Database {
    /// Every connected password manager, by name.
    pub fn list_password_manager_sources(&self) -> DbResult<Vec<PasswordManagerSourceRow>> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT * FROM password_manager_sources ORDER BY name COLLATE NOCASE, created_at",
        )?;
        let rows = statement.query_map([], source_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn password_manager_source(&self, id: &str) -> DbResult<Option<PasswordManagerSourceRow>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT * FROM password_manager_sources WHERE id = ?",
                [id],
                source_from_row,
            )
            .optional()?)
    }

    pub fn create_password_manager_source(
        &self,
        kind: &str,
        name: &str,
        config: &PasswordManagerConfig,
    ) -> DbResult<PasswordManagerSourceRow> {
        require_non_empty(name, "name")?;
        let config = config_json(config)?;
        let id = nanoid();
        let conn = self.lock();
        conn.execute(
            "INSERT INTO password_manager_sources(id, kind, name, config_json) VALUES (?, ?, ?, ?)",
            params![id, kind, muxus_core::js::js_trim(name), config],
        )?;
        Ok(conn.query_row(
            "SELECT * FROM password_manager_sources WHERE id = ?",
            [&id],
            source_from_row,
        )?)
    }

    /// Rename a password manager or change how it is reached; `None` when it is gone.
    pub fn update_password_manager_source(
        &self,
        id: &str,
        name: &str,
        config: &PasswordManagerConfig,
    ) -> DbResult<Option<PasswordManagerSourceRow>> {
        require_non_empty(name, "name")?;
        let config = config_json(config)?;
        let conn = self.lock();
        conn.execute(
            "
        UPDATE password_manager_sources
        SET name = ?, config_json = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
      ",
            params![muxus_core::js::js_trim(name), config, id],
        )?;
        Ok(conn
            .query_row(
                "SELECT * FROM password_manager_sources WHERE id = ?",
                [id],
                source_from_row,
            )
            .optional()?)
    }

    /// Hosts and folders keep their reference; it reports the missing manager when used.
    pub fn delete_password_manager_source(&self, id: &str) -> DbResult<bool> {
        Ok(self
            .lock()
            .execute("DELETE FROM password_manager_sources WHERE id = ?", [id])?
            > 0)
    }
}
