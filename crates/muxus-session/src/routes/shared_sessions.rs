//! Session lists published at a link (`server/src/routes/shared-sessions.ts`). The backend
//! keeps the links, reads them and stores their hosts. [`Backend::sync_shared_sessions`]
//! does one whole check: read the link, turn its text into hosts and make the link's hosts
//! match.

use chrono::Utc;
use muxus_core::api::{
    ForwardType, MAX_SHARED_SESSION_HOSTS, SharedSessionApplyRequest, SharedSessionApplyResult,
    SharedSessionFetchResponse, SharedSessionHost, SharedSessionSource, SharedSessionSourceInput,
    SharedSessionSourcesResponse,
};
use muxus_core::folder_paths::normalize_folder_path;
use muxus_core::js::{js_trim, utf16_len, utf16_prefix};
use muxus_core::protocol::SavedHostSessionProfile;
use muxus_core::validate::{Validate, parse_value};
use muxus_import::parse_shared_sessions;
use muxus_store::db::{DbError, SharedSessionCheck};
use serde_json::Value;

use crate::backend::{Backend, Shared};
use crate::error::{ApiError, ApiResult};
use crate::shared_sessions::{fetch_shared_sessions, stored_shared_sessions_location};

const NOT_FOUND: &str = "shared sessions link not found";

/// A list read from a link changes without the user looking at it again, so it does not
/// get to set what runs on this computer or weakens a check: ProxyCommand, agent
/// forwarding, remote forwards and host key checking stay the user's to set.
fn without_local_effects(mut host: SharedSessionHost) -> SharedSessionHost {
    if let SavedHostSessionProfile::Ssh(profile) = &mut host.profile {
        profile.proxy_command = None;
        profile.forward_agent = None;
        profile.strict_host_key_checking = None;
        profile.forwards = profile
            .forwards
            .take()
            .map(|forwards| {
                forwards
                    .into_iter()
                    .filter(|forward| forward.kind != ForwardType::Remote)
                    .collect::<Vec<_>>()
            })
            .filter(|kept| !kept.is_empty());
        profile.use_config = Some(false);
    }
    host
}

/// Check the entries of a list one by one, so a bad one is named in the answer
/// (`parseHosts`).
fn parse_hosts(values: Vec<Value>) -> ApiResult<Vec<SharedSessionHost>> {
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let name = match value.get("name").and_then(Value::as_str) {
                Some(name) => format!("“{}”", utf16_prefix(name, 80)),
                None => format!("entry {}", index + 1),
            };
            let mut host: SharedSessionHost = parse_value(value).map_err(|error| {
                let place = if error.path.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", error.path)
                };
                ApiError::bad_request(format!(
                    "{name} is not a valid host: {}{place}",
                    error.message
                ))
            })?;
            host.group = host
                .group
                .as_deref()
                .map(normalize_folder_path)
                .filter(|group| !group.is_empty());
            Ok(without_local_effects(host))
        })
        .collect()
}

/// What the store refused about a list is the user's to read; anything else is a fault here.
fn refused(error: DbError) -> ApiError {
    match error {
        DbError::Invalid(message) | DbError::Secret(message) | DbError::NotFound(message) => {
            ApiError::bad_request(message)
        }
        other => ApiError::from(other),
    }
}

fn apply(
    shared: &Shared,
    id: &str,
    request: SharedSessionApplyRequest,
) -> ApiResult<SharedSessionApplyResult> {
    let database = &shared.database;
    let hosts = match request {
        SharedSessionApplyRequest::Error { error } => {
            let said = js_trim(&error);
            if said.is_empty() {
                return Err(ApiError::bad_request(
                    "Too small: expected string to have >=1 characters",
                ));
            }
            if utf16_len(said) > 1000 {
                return Err(ApiError::bad_request(
                    "Too big: expected string to have <=1000 characters",
                ));
            }
            let source = database
                .fail_shared_session_check(id, said)?
                .ok_or_else(|| ApiError::not_found(NOT_FOUND))?;
            return Ok(SharedSessionApplyResult {
                added: 0,
                updated: 0,
                removed: 0,
                unchanged: source.host_count,
                source,
            });
        }
        SharedSessionApplyRequest::Hosts { hosts } => hosts,
    };
    if hosts.len() > MAX_SHARED_SESSION_HOSTS {
        return Err(ApiError::bad_request(format!(
            "Too big: expected array to have <={MAX_SHARED_SESSION_HOSTS} items"
        )));
    }
    let applied = parse_hosts(hosts).and_then(|hosts| {
        // An empty list is far more often a broken export than a lab without hosts.
        if hosts.is_empty() {
            return Err(ApiError::bad_request(
                "The shared sessions list has no hosts.",
            ));
        }
        database
            .apply_shared_sessions(id, &hosts, Utc::now())
            .map_err(refused)
    });
    let counts = match applied {
        Ok(counts) => counts.ok_or_else(|| ApiError::not_found(NOT_FOUND))?,
        Err(error) => {
            database.fail_shared_session_check(id, &error.message)?;
            return Err(error);
        }
    };
    let source = database
        .shared_session_source(id)?
        .ok_or_else(|| ApiError::not_found(NOT_FOUND))?;
    Ok(SharedSessionApplyResult {
        source,
        added: counts.added,
        updated: counts.updated,
        removed: counts.removed,
        unchanged: counts.unchanged,
    })
}

impl Backend {
    /// `GET /api/shared-sessions`.
    pub async fn shared_session_sources(&self) -> ApiResult<SharedSessionSourcesResponse> {
        self.blocking(|shared| {
            Ok(SharedSessionSourcesResponse {
                sources: shared.database.list_shared_session_sources()?,
            })
        })
        .await
    }

    /// `PUT /api/shared-sessions`: follow a link, or change one.
    pub async fn save_shared_session_source(
        &self,
        mut input: SharedSessionSourceInput,
    ) -> ApiResult<SharedSessionSource> {
        input.validate()?;
        self.blocking(move |shared| {
            let folder = normalize_folder_path(&input.folder);
            if folder.is_empty() {
                return Err(ApiError::bad_request(
                    "Choose a folder for the shared sessions.",
                ));
            }
            let url = stored_shared_sessions_location(&input.url, &shared.local_env.home)?;
            let input = SharedSessionSourceInput {
                url,
                folder,
                ..input
            };
            shared
                .database
                .save_shared_session_source(&input, Utc::now())
                .map_err(refused)
        })
        .await
    }

    /// `DELETE /api/shared-sessions/:id`: remove a link, and its hosts with it unless they
    /// are kept as hosts of the user's own.
    pub async fn delete_shared_session_source(
        &self,
        id: &str,
        keep_hosts: bool,
    ) -> ApiResult<bool> {
        let id = id.to_owned();
        self.blocking(move |shared| {
            Ok(shared
                .database
                .delete_shared_session_source(&id, keep_hosts)?)
        })
        .await
    }

    /// `POST /api/shared-sessions/:id/fetch`: the text behind a link. Unless forced, a link
    /// that was read within its interval is left alone.
    pub async fn fetch_shared_sessions(
        &self,
        id: &str,
        force: bool,
    ) -> ApiResult<SharedSessionFetchResponse> {
        let id = id.to_owned();
        self.run(|shared| async move {
            let check = {
                let (shared, id) = (shared.clone(), id.clone());
                tokio::task::spawn_blocking(move || {
                    shared
                        .database
                        .begin_shared_session_check(&id, force, Utc::now())
                })
                .await
                .map_err(|error| ApiError::internal(error.to_string()))??
            };
            let source = match check {
                None => return Err(ApiError::not_found(NOT_FOUND)),
                Some(SharedSessionCheck::NotDue) => return Ok(SharedSessionFetchResponse::NotDue),
                Some(SharedSessionCheck::Read(source)) => source,
            };
            match fetch_shared_sessions(&source.url, &shared.local_env.home).await {
                Ok(content) => Ok(SharedSessionFetchResponse::Fetched { content }),
                Err(error) => {
                    let message = error.message.clone();
                    tokio::task::spawn_blocking(move || {
                        shared.database.fail_shared_session_check(&id, &message)
                    })
                    .await
                    .map_err(|error| ApiError::internal(error.to_string()))??;
                    Err(error)
                }
            }
        })
        .await
    }

    /// `POST /api/shared-sessions/:id/apply`: make a link's hosts match the list, or record
    /// why its content could not be used. The hosts the link had stay when a list is
    /// refused.
    pub async fn apply_shared_sessions(
        &self,
        id: &str,
        request: SharedSessionApplyRequest,
    ) -> ApiResult<SharedSessionApplyResult> {
        let id = id.to_owned();
        self.blocking(move |shared| apply(shared, &id, request))
            .await
    }

    /// One check of a link (`syncSharedSessions`): read it and make its hosts match.
    /// `None` when the link is not due. A list that cannot be understood is recorded with
    /// the link, and the hosts it had stay.
    pub async fn sync_shared_sessions(
        &self,
        id: &str,
        force: bool,
    ) -> ApiResult<Option<SharedSessionApplyResult>> {
        let content = match self.fetch_shared_sessions(id, force).await? {
            SharedSessionFetchResponse::NotDue => return Ok(None),
            SharedSessionFetchResponse::Fetched { content } => content,
        };
        let id = id.to_owned();
        self.blocking(move |shared| {
            let source = shared
                .database
                .shared_session_source(&id)?
                .ok_or_else(|| ApiError::not_found(NOT_FOUND))?;
            let request = match parse_shared_sessions(&content, &source.id, &source.folder) {
                Ok(list) => SharedSessionApplyRequest::Hosts {
                    hosts: list
                        .hosts
                        .iter()
                        .map(serde_json::to_value)
                        .collect::<Result<_, _>>()
                        .map_err(|error| ApiError::internal(error.to_string()))?,
                },
                Err(error) => {
                    let message = error.to_string();
                    shared.database.fail_shared_session_check(&id, &message)?;
                    return Err(ApiError::bad_request(message));
                }
            };
            apply(shared, &id, request).map(Some)
        })
        .await
    }
}
