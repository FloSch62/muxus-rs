//! Preferences and other UI state (`client-state.json`), which the Electron main process kept
//! for the renderer (`electron/src/main.ts`, `store-bridge.ts`). Every window reads and writes
//! them through the backend, so they stay in one place; the file is the TypeScript build's, so
//! both builds share it.

use muxus_store::paths::AppPaths;
use muxus_store::prefs::PrefsState;

use crate::backend::{Backend, Shared};
use crate::error::{ApiError, ApiResult};

fn prefs_store(shared: &Shared) -> std::sync::MutexGuard<'_, muxus_store::prefs::PrefsStore> {
    shared
        .prefs
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Backend {
    /// Where this backend keeps its data (database, preferences, window placement files).
    pub fn paths(&self) -> &AppPaths {
        &self.shared.config.paths
    }

    /// The preferences.
    pub async fn prefs(&self) -> ApiResult<PrefsState> {
        self.blocking(|shared| Ok(prefs_store(shared).prefs()))
            .await
    }

    /// Store changed preferences (written at once, like the zustand store's `set`).
    pub async fn set_prefs(&self, prefs: PrefsState) -> ApiResult<()> {
        self.blocking(move |shared| {
            prefs_store(shared)
                .set(&prefs)
                .map_err(|error| ApiError::internal(error.to_string()))
        })
        .await
    }

    /// Read the file again, taking what another window or the other build saved.
    pub async fn reload_prefs(&self) -> ApiResult<PrefsState> {
        self.blocking(|shared| {
            let mut store = prefs_store(shared);
            if let Some(error) = store.reload() {
                tracing::warn!(%error, "could not write the migrated preferences");
            }
            Ok(store.prefs())
        })
        .await
    }

    /// Another item of `client-state.json` (the renderer's other local storage).
    pub async fn client_state_item(&self, name: &str) -> ApiResult<Option<String>> {
        let name = name.to_string();
        self.blocking(move |shared| {
            Ok(prefs_store(shared)
                .client_state()
                .get_item(&name)
                .map(str::to_string))
        })
        .await
    }

    /// Set (`Some`) or remove (`None`) an item of `client-state.json`.
    pub async fn set_client_state_item(&self, name: &str, value: Option<&str>) -> ApiResult<()> {
        let (name, value) = (name.to_string(), value.map(str::to_string));
        self.blocking(move |shared| {
            let mut store = prefs_store(shared);
            let file = store.client_state_mut();
            match value {
                Some(value) => file.set_item(name, value),
                None => {
                    file.remove_item(&name);
                }
            }
            file.save()
                .map_err(|error| ApiError::internal(error.to_string()))
        })
        .await
    }
}
