//! Shared SSH defaults of sidebar folders (`folder_settings`).

use muxus_core::api::{FolderAuthSettings, LoginSequence};
use muxus_core::folder_paths::{
    folder_path_key, folder_path_segments, is_descendant_folder_path, normalize_folder_path,
    rename_folder_path_under,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::hosts::login_sequence_from_json;
use super::{Database, DbResult, assert_secret_free, require_non_empty};
use crate::ids::nanoid;

/// One folder's shared SSH defaults, keyed by its case-folded path.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSettingsRow {
    pub id: String,
    pub path: String,
    pub path_key: String,
    pub auth: FolderAuthSettings,
    pub login_sequence: Option<LoginSequence>,
    pub created_at: String,
    pub updated_at: String,
}

/// What a folder rename or move did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FolderMove {
    pub moved: usize,
    /// Rows dropped because their destination already had settings; their
    /// vault passwords should be deleted too.
    pub dropped: Vec<FolderSettingsRow>,
}

fn folder_from_row(row: &Row<'_>) -> rusqlite::Result<FolderSettingsRow> {
    let auth: String = row.get("auth_json")?;
    let login_sequence: Option<String> = row.get("login_sequence_json")?;
    Ok(FolderSettingsRow {
        id: row.get("id")?,
        path: row.get("path")?,
        path_key: row.get("path_key")?,
        auth: serde_json::from_str(&auth).unwrap_or_default(),
        login_sequence: login_sequence_from_json(login_sequence.as_deref()),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn list(conn: &Connection) -> DbResult<Vec<FolderSettingsRow>> {
    let mut statement = conn.prepare("SELECT * FROM folder_settings ORDER BY path_key")?;
    let rows = statement.query_map([], folder_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn for_path(conn: &Connection, path: &str) -> DbResult<Option<FolderSettingsRow>> {
    let key = folder_path_key(path);
    if key.is_empty() {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT * FROM folder_settings WHERE path_key = ?",
            [key],
            folder_from_row,
        )
        .optional()?)
}

fn occupant(conn: &Connection, key: &str) -> DbResult<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT id FROM folder_settings WHERE path_key = ?",
            [key],
            |row| row.get(0),
        )
        .optional()?)
}

const MOVE_ROW_SQL: &str = "
        UPDATE folder_settings
        SET path_key = ?, path = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
      ";

impl Database {
    /// Every folder's settings, by path key.
    pub fn list_folder_settings(&self) -> DbResult<Vec<FolderSettingsRow>> {
        list(&self.lock())
    }

    /// Exact-path lookup; `path` may be any capitalization of the folder.
    pub fn folder_settings_for_path(&self, path: &str) -> DbResult<Option<FolderSettingsRow>> {
        for_path(&self.lock(), path)
    }

    /// Store a folder's defaults (the caller cleans them first).
    pub fn upsert_folder_settings(
        &self,
        path: &str,
        auth: &FolderAuthSettings,
    ) -> DbResult<FolderSettingsRow> {
        let normalized = normalize_folder_path(path);
        require_non_empty(&normalized, "path")?;
        assert_secret_free(&serde_json::to_value(auth)?, "folder.auth")?;
        let conn = self.lock();
        conn.execute(
            "
        INSERT INTO folder_settings(id, path_key, path, auth_json)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(path_key) DO UPDATE SET
          path = excluded.path,
          auth_json = excluded.auth_json,
          updated_at = CURRENT_TIMESTAMP
      ",
            params![
                nanoid(),
                folder_path_key(&normalized),
                normalized,
                serde_json::to_string(auth)?
            ],
        )?;
        for_path(&conn, &normalized)?
            .ok_or_else(|| super::DbError::NotFound("folder settings not found".into()))
    }

    /// Set or clear (`None`) a folder's login sequence; the row must exist.
    pub fn set_folder_login_sequence(
        &self,
        id: &str,
        sequence: Option<&LoginSequence>,
    ) -> DbResult<()> {
        let json = match sequence {
            Some(sequence) => {
                let value = serde_json::to_value(sequence)?;
                assert_secret_free(&value, "folder.loginSequence")?;
                Some(serde_json::to_string(sequence)?)
            }
            None => None,
        };
        self.lock().execute(
            "
        UPDATE folder_settings
        SET login_sequence_json = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
      ",
            params![json, id],
        )?;
        Ok(())
    }

    /// Remove one settings row only — descendants keep their own settings.
    pub fn remove_folder_settings_row(&self, id: &str) -> DbResult<()> {
        self.lock()
            .execute("DELETE FROM folder_settings WHERE id = ?", [id])?;
        Ok(())
    }

    /// Carry settings across a folder rename or move: the folder itself and
    /// every descendant follow the path rewrite. When a destination path
    /// already has a row (a merge), the destination keeps its settings and
    /// the source row is dropped.
    pub fn move_folder_settings(&self, from: &str, to: &str) -> DbResult<FolderMove> {
        let source = normalize_folder_path(from);
        let target = normalize_folder_path(to);
        require_non_empty(&source, "from")?;
        require_non_empty(&target, "to")?;
        if source == target {
            return Ok(FolderMove::default());
        }
        self.immediate(|conn| {
            let mut result = FolderMove::default();
            for row in list(conn)? {
                let Some(next) = rename_folder_path_under(&row.path, &source, &target) else {
                    continue;
                };
                let next_key = folder_path_key(&next);
                if occupant(conn, &next_key)?.is_some_and(|id| id != row.id) {
                    conn.execute("DELETE FROM folder_settings WHERE id = ?", [&row.id])?;
                    result.dropped.push(row);
                    continue;
                }
                conn.execute(MOVE_ROW_SQL, params![next_key, next, row.id])?;
                result.moved += 1;
            }
            Ok(result)
        })
    }

    /// Delete a folder's settings the way deleting the folder treats its
    /// hosts: the folder's own row is removed and descendant rows are
    /// promoted one level up (colliding promotions are dropped in favor of
    /// the existing row). Returns every removed row.
    pub fn delete_folder_settings(&self, path: &str) -> DbResult<Vec<FolderSettingsRow>> {
        let target = normalize_folder_path(path);
        require_non_empty(&target, "path")?;
        self.immediate(|conn| {
            let mut removed = Vec::new();
            if let Some(own) = for_path(conn, &target)? {
                conn.execute("DELETE FROM folder_settings WHERE id = ?", [&own.id])?;
                removed.push(own);
            }
            let segments = folder_path_segments(&target);
            let parent = &segments[..segments.len().saturating_sub(1)];
            let depth = segments.len();
            for row in list(conn)? {
                if !is_descendant_folder_path(&row.path, &target) {
                    continue;
                }
                let tail = folder_path_segments(&row.path).into_iter().skip(depth);
                let next = parent
                    .iter()
                    .copied()
                    .chain(tail)
                    .collect::<Vec<_>>()
                    .join("/");
                let next_key = folder_path_key(&next);
                if occupant(conn, &next_key)?.is_some_and(|id| id != row.id) {
                    conn.execute("DELETE FROM folder_settings WHERE id = ?", [&row.id])?;
                    removed.push(row);
                    continue;
                }
                conn.execute(MOVE_ROW_SQL, params![next_key, next, row.id])?;
            }
            Ok(removed)
        })
    }
}

/// Collapse a folder chain (nearest first) so the nearest folder wins per
/// field (`mergeFolderAuth`).
pub fn merge_folder_auth<'a>(
    chain: impl IntoIterator<Item = &'a FolderAuthSettings>,
) -> FolderAuthSettings {
    let mut merged = FolderAuthSettings::default();
    for auth in chain {
        merged.user = merged.user.or_else(|| auth.user.clone());
        merged.port = merged.port.or(auth.port);
        merged.identity_files = merged
            .identity_files
            .or_else(|| auth.identity_files.clone());
        merged.identities_only = merged.identities_only.or(auth.identities_only);
        merged.identity_agent = merged
            .identity_agent
            .or_else(|| auth.identity_agent.clone());
        merged.forward_agent = merged.forward_agent.or(auth.forward_agent);
    }
    merged
}

impl Database {
    /// The settings rows of a folder and its ancestors that have any, nearest
    /// first: what folder defaults and folder passwords resolve from.
    pub fn folder_settings_chain(&self, group: Option<&str>) -> DbResult<Vec<FolderSettingsRow>> {
        let Some(group) = group else {
            return Ok(Vec::new());
        };
        let conn = self.lock();
        let mut rows = Vec::new();
        for path in muxus_core::folder_paths::folder_chain(group) {
            if let Some(row) = for_path(&conn, &path)? {
                rows.push(row);
            }
        }
        Ok(rows)
    }
}
