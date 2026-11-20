//! Connected password managers and the entry picker (`server/src/routes/password-managers.ts`).
//! Entries are listed with their titles and user names only: nothing here returns a
//! password, which is read on the backend at the moment a login needs it.

use muxus_core::api::{
    PasswordManagerConfig, PasswordManagerEntry, PasswordManagerInput, PasswordManagerKind,
    PasswordManagerSource, PasswordManagersResponse,
};
use muxus_core::js::{js_trim, utf16_len};
use muxus_passwords::{PasswordManagerProblem, PasswordManagersError};

use crate::backend::Backend;
use crate::error::{ApiError, ApiResult};

/// The status each problem answered with (`STATUS`). An unlock password that is wrong is
/// not a 401: the window takes that for a rejected backend token.
fn status(problem: PasswordManagerProblem) -> u16 {
    match problem {
        PasswordManagerProblem::Unavailable => 503,
        PasswordManagerProblem::Locked => 423,
        PasswordManagerProblem::SignedOut => 409,
        PasswordManagerProblem::InvalidUnlock => 422,
        PasswordManagerProblem::EntryMissing => 404,
        PasswordManagerProblem::Timeout => 504,
        PasswordManagerProblem::Failed => 502,
    }
}

/// `sendPasswordManagerError`.
fn api_error(error: PasswordManagersError) -> ApiError {
    match error {
        PasswordManagersError::Provider(error) => ApiError::with_code(
            status(error.problem),
            error.message,
            &format!("password-manager-{}", error.problem.as_str()),
        ),
        PasswordManagersError::SourceMissing => {
            ApiError::with_code(404, error.to_string(), "password-manager-missing")
        }
        PasswordManagersError::Store(message) => ApiError::internal(message),
    }
}

/// The limits of `configSchema`.
fn valid_config(config: &PasswordManagerConfig) -> bool {
    let within = |value: &Option<String>, max: usize| {
        value.as_deref().is_none_or(|value| utf16_len(value) <= max)
    };
    within(&config.path, 4096)
        && within(&config.key_file, 4096)
        && within(&config.program, 4096)
        && within(&config.account, 200)
}

/// `sourceSchema`, and the database a KeePass source cannot do without.
fn check(input: &PasswordManagerInput) -> ApiResult<()> {
    let valid_name = input
        .name
        .as_deref()
        .is_none_or(|name| utf16_len(name) <= 200);
    if !valid_name || !valid_config(&input.config) {
        return Err(ApiError::bad_request(
            "A valid password manager is required.",
        ));
    }
    let has_path = input
        .config
        .path
        .as_deref()
        .is_some_and(|path| !js_trim(path).is_empty());
    if input.kind == PasswordManagerKind::Keepass && !has_path {
        return Err(ApiError::bad_request("Choose the KeePass database file."));
    }
    Ok(())
}

impl Backend {
    /// `GET /api/password-managers`: every connected manager with where it stands, and
    /// the kinds whose program is on this computer.
    pub async fn password_managers_list(&self) -> ApiResult<PasswordManagersResponse> {
        self.run(|shared| async move { shared.password_managers.list().await.map_err(api_error) })
            .await
    }

    /// `POST /api/password-managers`.
    pub async fn create_password_manager(
        &self,
        input: PasswordManagerInput,
    ) -> ApiResult<PasswordManagerSource> {
        check(&input)?;
        self.run(|shared| async move {
            shared
                .password_managers
                .create(&input)
                .await
                .map_err(api_error)
        })
        .await
    }

    /// `PUT /api/password-managers/:id`: a new name, or another way to reach it.
    pub async fn update_password_manager(
        &self,
        id: &str,
        input: PasswordManagerInput,
    ) -> ApiResult<PasswordManagerSource> {
        check(&input)?;
        let id = id.to_owned();
        self.run(|shared| async move {
            shared
                .password_managers
                .update(&id, &input)
                .await
                .map_err(api_error)?
                .ok_or_else(|| api_error(PasswordManagersError::SourceMissing))
        })
        .await
    }

    /// `DELETE /api/password-managers/:id`; whether there was one. Hosts and folders keep
    /// their reference to an entry, which says the manager is gone when it is used.
    pub async fn delete_password_manager(&self, id: &str) -> ApiResult<bool> {
        let id = id.to_owned();
        self.blocking(move |shared| shared.password_managers.remove(&id).map_err(api_error))
            .await
    }

    /// `POST /api/password-managers/:id/unlock`, with the manager's own password.
    pub async fn unlock_password_manager(
        &self,
        id: &str,
        password: &str,
    ) -> ApiResult<PasswordManagerSource> {
        if utf16_len(password) > 8192 {
            return Err(ApiError::bad_request("A password is required."));
        }
        let (id, password) = (id.to_owned(), password.to_owned());
        self.run(|shared| async move {
            shared
                .password_managers
                .unlock(&id, &password)
                .await
                .map_err(api_error)
        })
        .await
    }

    /// `POST /api/password-managers/:id/lock`.
    pub async fn lock_password_manager(&self, id: &str) -> ApiResult<PasswordManagerSource> {
        let id = id.to_owned();
        self.run(
            |shared| async move { shared.password_managers.lock(&id).await.map_err(api_error) },
        )
        .await
    }

    /// `GET /api/password-managers/:id/entries`; `sync` is its `?sync=1`, which pulls
    /// changes from the manager's server first.
    pub async fn password_manager_entries(
        &self,
        id: &str,
        sync: bool,
    ) -> ApiResult<Vec<PasswordManagerEntry>> {
        let id = id.to_owned();
        self.run(|shared| async move {
            shared
                .password_managers
                .entries(&id, sync)
                .await
                .map_err(api_error)
        })
        .await
    }
}
