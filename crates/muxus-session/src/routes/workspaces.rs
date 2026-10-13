//! Saved workspaces (`server/src/routes/workspaces.ts`).

use muxus_core::api::{WorkspaceRecord, WorkspaceSaveRequest, WorkspaceSummary};
use muxus_core::js::{js_trim, utf16_len};
use muxus_core::validate::Validate;
use muxus_store::DbError;
use muxus_store::db::{DEFAULT_SNAPSHOT_GRACE_SECONDS, StoredWorkspace};

use crate::backend::Backend;
use crate::error::{ApiError, ApiResult};

/// Body of `PATCH /api/workspaces/:id`: exactly one change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspacePatch {
    Rename(String),
    SetLocked(bool),
}

fn record(workspace: StoredWorkspace) -> ApiResult<WorkspaceRecord> {
    Ok(workspace.to_record()?)
}

fn workspace_not_found() -> ApiError {
    ApiError::not_found("workspace not found")
}

impl Backend {
    /// `GET /api/workspaces`.
    pub async fn workspaces(&self) -> ApiResult<Vec<WorkspaceSummary>> {
        self.blocking(|shared| Ok(shared.database.list_workspace_summaries()?))
            .await
    }

    /// `GET /api/workspaces/latest`.
    pub async fn latest_workspace(&self) -> ApiResult<Option<WorkspaceRecord>> {
        self.blocking(|shared| shared.database.latest_workspace()?.map(record).transpose())
            .await
    }

    /// `GET /api/workspaces/startup`.
    pub async fn startup_workspace(&self) -> ApiResult<Option<WorkspaceRecord>> {
        self.blocking(|shared| shared.database.startup_workspace()?.map(record).transpose())
            .await
    }

    /// `PUT /api/workspaces/startup`: `None` clears the startup workspace.
    pub async fn set_startup_workspace(
        &self,
        id: Option<&str>,
    ) -> ApiResult<Option<WorkspaceRecord>> {
        if id.is_some_and(str::is_empty) {
            return Err(ApiError::bad_request("invalid startup workspace"));
        }
        let id = id.map(str::to_string);
        self.blocking(move |shared| {
            let workspace = shared.database.set_startup_workspace(id.as_deref())?;
            if id.is_some() && workspace.is_none() {
                return Err(workspace_not_found());
            }
            workspace.map(record).transpose()
        })
        .await
    }

    /// `GET /api/workspaces/:id`.
    pub async fn workspace(&self, id: &str) -> ApiResult<WorkspaceRecord> {
        let id = id.to_string();
        self.blocking(move |shared| {
            record(
                shared
                    .database
                    .workspace(&id)?
                    .ok_or_else(workspace_not_found)?,
            )
        })
        .await
    }

    /// `PUT /api/workspaces`.
    pub async fn save_workspace(
        &self,
        request: WorkspaceSaveRequest,
    ) -> ApiResult<WorkspaceRecord> {
        let mut request = request;
        request.validate().map_err(ApiError::invalid)?;
        self.blocking(
            move |shared| match shared.database.put_workspace(&request) {
                Ok(saved) => record(saved),
                Err(DbError::NotFound(_)) => Err(ApiError::with_code(
                    404,
                    "workspace not found",
                    "workspace-not-found",
                )),
                Err(error @ DbError::WorkspaceLocked(_)) => Err(ApiError::with_code(
                    409,
                    error.to_string(),
                    "workspace-locked",
                )),
                Err(error) => Err(error.into()),
            },
        )
        .await
    }

    /// `PATCH /api/workspaces/:id`: rename or lock.
    pub async fn patch_workspace(
        &self,
        id: &str,
        patch: WorkspacePatch,
    ) -> ApiResult<WorkspaceRecord> {
        let patch = match patch {
            WorkspacePatch::Rename(name) => {
                let name = js_trim(&name).to_string();
                let length = utf16_len(&name);
                if length < 1 {
                    return Err(ApiError::bad_request(
                        "Too small: expected string to have >=1 characters",
                    ));
                }
                if length > 200 {
                    return Err(ApiError::bad_request(
                        "Too big: expected string to have <=200 characters",
                    ));
                }
                WorkspacePatch::Rename(name)
            }
            other => other,
        };
        let id = id.to_string();
        self.blocking(move |shared| {
            let workspace = match &patch {
                WorkspacePatch::Rename(name) => shared.database.rename_workspace(&id, name)?,
                WorkspacePatch::SetLocked(locked) => {
                    shared.database.set_workspace_locked(&id, *locked)?
                }
            };
            record(workspace.ok_or_else(workspace_not_found)?)
        })
        .await
    }

    /// `POST /api/workspaces/:id/open`.
    pub async fn open_workspace(&self, id: &str) -> ApiResult<WorkspaceRecord> {
        let id = id.to_string();
        self.blocking(move |shared| {
            record(
                shared
                    .database
                    .open_workspace(&id)?
                    .ok_or_else(workspace_not_found)?,
            )
        })
        .await
    }

    /// `DELETE /api/workspaces/:id`.
    pub async fn delete_workspace(&self, id: &str) -> ApiResult<bool> {
        let id = id.to_string();
        self.blocking(move |shared| {
            let deleted = shared.database.delete_workspace(&id)?;
            if deleted {
                shared
                    .database
                    .prune_terminal_snapshots(DEFAULT_SNAPSHOT_GRACE_SECONDS)?;
            }
            Ok(deleted)
        })
        .await
    }
}
