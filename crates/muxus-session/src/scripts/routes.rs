//! The script library, and runs of a script on live SSH connections
//! (`server/src/routes/scripts.ts`).

use muxus_core::api::{
    SavedScript, SavedScriptInput, SavedScriptsResponse, ScriptRunRequest, ScriptRunStatus,
};
use muxus_core::validate::Validate;

use crate::backend::Backend;
use crate::error::{ApiError, ApiResult};

impl Backend {
    /// `GET /api/scripts`.
    pub async fn scripts(&self) -> ApiResult<SavedScriptsResponse> {
        self.blocking(|shared| {
            Ok(SavedScriptsResponse {
                scripts: shared.database.list_scripts()?,
            })
        })
        .await
    }

    /// `PUT /api/scripts`: create a script, or replace the one with the input's id.
    pub async fn save_script(&self, input: SavedScriptInput) -> ApiResult<SavedScript> {
        let mut input = input;
        input.validate().map_err(ApiError::invalid)?;
        self.blocking(move |shared| Ok(shared.database.save_script(&input)?))
            .await
    }

    /// `DELETE /api/scripts/:id`: whether there was one.
    pub async fn delete_script(&self, id: &str) -> ApiResult<bool> {
        let id = id.to_owned();
        self.blocking(move |shared| Ok(shared.database.delete_script(&id)?))
            .await
    }

    /// `POST /api/script-runs`: start a script on a live SSH connection; the run's id.
    pub async fn start_script_run(&self, request: ScriptRunRequest) -> ApiResult<String> {
        let mut request = request;
        request.validate().map_err(ApiError::invalid)?;
        self.run(move |shared| async move {
            shared
                .script_runs
                .start(
                    &request.conn_id,
                    &request.body,
                    request.timeout_seconds.unwrap_or(0),
                )
                .await
        })
        .await
    }

    /// `GET /api/script-runs/:id?since=<offset>`: the run's state and its output from `since`.
    pub async fn script_run_status(&self, id: &str, since: u64) -> ApiResult<ScriptRunStatus> {
        self.shared.script_runs.status(id, since)
    }

    /// `DELETE /api/script-runs/:id`: stop the script and everything it started.
    pub async fn cancel_script_run(&self, id: &str) -> ApiResult<()> {
        self.shared.script_runs.cancel(id);
        Ok(())
    }
}
