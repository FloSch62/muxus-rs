//! Shared credential defaults for sidebar folders (`server/src/routes/folders.ts`).
//!
//! Non-secret settings live in the `folder_settings` table; the folder password goes through
//! the encrypted password vault, referenced by the settings row's stable id.

use muxus_core::api::{
    FolderAuthSettings, FolderSettingsRecord, FolderSettingsResponse, LoginSequence,
};
use muxus_core::folder_paths::{folder_path_key, is_descendant_folder_path, normalize_folder_path};
use muxus_core::js::{Patch, utf16_len};
use muxus_core::validate::Validate;
use muxus_store::db::FolderSettingsRow;
use muxus_store::vault::{folder_password_account, folder_password_label};
use serde::{Deserialize, Serialize};

use crate::backend::{Backend, Shared};
use crate::error::{ApiError, ApiResult};

/// Body of `PUT /api/folders/settings`. An absent login sequence keeps the folder's current
/// one; `Patch::Clear` removes it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSettingsUpsert {
    pub path: String,
    pub auth: FolderAuthSettings,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub login_sequence: Patch<LoginSequence>,
}

/// Answer of `POST /api/folders/settings/move`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderMoveResponse {
    pub moved: usize,
    /// The destination already had settings and kept them; a rename dialog must not save the
    /// source's fields over them.
    pub destination_preserved: bool,
}

fn check_path(path: &str) -> Result<(), String> {
    let length = utf16_len(path);
    if length < 1 {
        return Err("Too small: expected string to have >=1 characters".into());
    }
    if length > 300 {
        return Err("Too big: expected string to have <=300 characters".into());
    }
    Ok(())
}

fn too_big(limit: usize, what: &str) -> String {
    format!(
        "Too big: expected {what} to have <={limit} {}",
        if what == "array" {
            "items"
        } else {
            "characters"
        }
    )
}

/// The zod limits of `authSchema` (length caps only; [`clean_auth`] trims).
fn check_auth(auth: &FolderAuthSettings) -> Result<(), String> {
    if auth
        .user
        .as_deref()
        .is_some_and(|user| utf16_len(user) > 200)
    {
        return Err(too_big(200, "string"));
    }
    if auth.port == Some(0) {
        return Err("Too small: expected number to be >=1".into());
    }
    if let Some(files) = &auth.identity_files {
        if files.len() > 10 {
            return Err(too_big(10, "array"));
        }
        if files.iter().any(|file| utf16_len(file) > 1024) {
            return Err(too_big(1024, "string"));
        }
    }
    if auth
        .identity_agent
        .as_deref()
        .is_some_and(|agent| utf16_len(agent) > 1024)
    {
        return Err(too_big(1024, "string"));
    }
    Ok(())
}

/// Drop empty strings and empty lists so "cleared" fields don't linger.
pub fn clean_auth(auth: &FolderAuthSettings) -> FolderAuthSettings {
    let trimmed = |value: &Option<String>| {
        value
            .as_deref()
            .map(muxus_core::js::js_trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let identity_files: Vec<String> = auth
        .identity_files
        .iter()
        .flatten()
        .map(|file| muxus_core::js::js_trim(file).to_string())
        .filter(|file| !file.is_empty())
        .collect();
    FolderAuthSettings {
        user: trimmed(&auth.user),
        port: auth.port,
        identity_files: (!identity_files.is_empty()).then_some(identity_files),
        identities_only: auth.identities_only,
        identity_agent: trimmed(&auth.identity_agent),
        forward_agent: auth.forward_agent,
    }
}

fn has_password(shared: &Shared, row: &FolderSettingsRow) -> bool {
    shared
        .vault
        .has_ssh_password(&folder_password_account(&row.id))
        .unwrap_or(false)
}

fn record(shared: &Shared, row: FolderSettingsRow) -> FolderSettingsRecord {
    FolderSettingsRecord {
        has_password: has_password(shared, &row),
        id: row.id,
        path: row.path,
        auth: row.auth,
        login_sequence: row.login_sequence,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

impl Backend {
    /// `GET /api/folders/settings`.
    pub async fn folder_settings(&self) -> ApiResult<FolderSettingsResponse> {
        self.blocking(|shared| {
            let rows = shared.database.list_folder_settings()?;
            Ok(FolderSettingsResponse {
                folders: rows.into_iter().map(|row| record(shared, row)).collect(),
            })
        })
        .await
    }

    /// `PUT /api/folders/settings`: `None` when the folder ends up with nothing to keep (no
    /// settings, no login sequence, no password), so it has no row at all.
    pub async fn put_folder_settings(
        &self,
        request: FolderSettingsUpsert,
    ) -> ApiResult<Option<FolderSettingsRecord>> {
        self.blocking(move |shared| {
            let mut request = request;
            check_path(&request.path)
                .and_then(|()| check_auth(&request.auth))
                .and_then(|()| match &mut request.login_sequence {
                    Patch::Set(sequence) => sequence
                        .validate_at("loginSequence")
                        .map_err(|error| error.message),
                    _ => Ok(()),
                })
                .map_err(ApiError::bad_request)?;
            if normalize_folder_path(&request.path).is_empty() {
                return Err(ApiError::bad_request("a folder path is required"));
            }
            let auth = clean_auth(&request.auth);
            let database = &shared.database;
            let mut row = database.upsert_folder_settings(&request.path, &auth)?;
            if !request.login_sequence.is_keep() {
                let sequence = match &request.login_sequence {
                    Patch::Set(sequence) => Some(sequence),
                    _ => None,
                };
                database.set_folder_login_sequence(&row.id, sequence)?;
                row = database.folder_settings_for_path(&row.path)?.unwrap_or(row);
            }
            // A folder with no settings and no password needs no row at all.
            if auth.is_empty() && row.login_sequence.is_none() && !has_password(shared, &row) {
                database.remove_folder_settings_row(&row.id)?;
                return Ok(None);
            }
            Ok(Some(record(shared, row)))
        })
        .await
    }

    /// `POST /api/folders/settings/move`: rename or re-parent; settings and vault labels follow
    /// the path rewrite.
    pub async fn move_folder_settings(
        &self,
        from: &str,
        to: &str,
    ) -> ApiResult<FolderMoveResponse> {
        let (from, to) = (from.to_string(), to.to_string());
        self.blocking(move |shared| {
            if check_path(&from).is_err()
                || check_path(&to).is_err()
                || normalize_folder_path(&from).is_empty()
                || normalize_folder_path(&to).is_empty()
            {
                return Err(ApiError::bad_request("both folder paths are required"));
            }
            let database = &shared.database;
            // Tell rename callers whether the destination already owned the root settings row.
            // They must not save the source dialog fields over that retained row after the
            // destination-wins merge.
            let source_key = folder_path_key(&from);
            let target_key = folder_path_key(&to);
            let destination_preserved =
                source_key != target_key && database.folder_settings_for_path(&to)?.is_some();
            let moved = database.move_folder_settings(&from, &to)?;
            for row in &moved.dropped {
                shared
                    .vault
                    .delete_ssh_password(&folder_password_account(&row.id))?;
            }
            for row in database.list_folder_settings()? {
                if row.path_key != target_key && !is_descendant_folder_path(&row.path, &to) {
                    continue;
                }
                shared.vault.relabel_ssh_password(
                    &folder_password_account(&row.id),
                    &folder_password_label(&row.path),
                )?;
            }
            Ok(FolderMoveResponse {
                moved: moved.moved,
                destination_preserved,
            })
        })
        .await
    }

    /// `DELETE /api/folders/settings`: drop the folder's settings and promote its descendants
    /// one level up. Returns how many rows were removed.
    pub async fn delete_folder_settings(&self, path: &str) -> ApiResult<usize> {
        let path = path.to_string();
        self.blocking(move |shared| {
            if check_path(&path).is_err() || normalize_folder_path(&path).is_empty() {
                return Err(ApiError::bad_request("a folder path is required"));
            }
            let removed = shared.database.delete_folder_settings(&path)?;
            for row in &removed {
                shared
                    .vault
                    .delete_ssh_password(&folder_password_account(&row.id))?;
            }
            Ok(removed.len())
        })
        .await
    }

    /// `PUT /api/folders/settings/password`.
    pub async fn put_folder_password(
        &self,
        path: &str,
        password: &str,
        master_password: Option<&str>,
    ) -> ApiResult<FolderSettingsRecord> {
        let (path, password) = (path.to_string(), password.to_string());
        let master = master_password.map(str::to_string);
        self.blocking(move |shared| {
            let valid = check_path(&path).is_ok()
                && !password.is_empty()
                && utf16_len(&password) <= 8192
                && master
                    .as_deref()
                    .is_none_or(|master| !master.is_empty() && utf16_len(master) <= 1024)
                && !normalize_folder_path(&path).is_empty();
            if !valid {
                return Err(ApiError::bad_request(
                    "a folder path and password are required",
                ));
            }
            let database = &shared.database;
            let existing = database.folder_settings_for_path(&path)?;
            let row = match existing.clone() {
                Some(row) => row,
                None => database.upsert_folder_settings(&path, &FolderAuthSettings::default())?,
            };
            let saved = shared.vault.remember_ssh_password(
                &folder_password_account(&row.id),
                &folder_password_label(&row.path),
                &password,
                master.as_deref(),
            );
            match saved {
                Ok(()) => Ok(record(shared, row)),
                Err(error) => {
                    // Don't leave behind an empty row created only to key this password.
                    if existing.is_none() {
                        database.remove_folder_settings_row(&row.id)?;
                    }
                    Err(error.into())
                }
            }
        })
        .await
    }

    /// `DELETE /api/folders/settings/password`.
    pub async fn delete_folder_password(&self, path: &str) -> ApiResult<bool> {
        let path = path.to_string();
        self.blocking(move |shared| {
            if check_path(&path).is_err() || normalize_folder_path(&path).is_empty() {
                return Err(ApiError::bad_request("a folder path is required"));
            }
            let Some(row) = shared.database.folder_settings_for_path(&path)? else {
                return Ok(false);
            };
            let deleted = shared
                .vault
                .delete_ssh_password(&folder_password_account(&row.id))?;
            if clean_auth(&row.auth).is_empty() && row.login_sequence.is_none() {
                shared.database.remove_folder_settings_row(&row.id)?;
            }
            Ok(deleted)
        })
        .await
    }
}
