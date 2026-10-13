//! The live OpenSSH config plus Muxus-owned metadata, Host block editing and key discovery
//! (`server/src/routes/ssh.ts`).

use muxus_core::api::{
    ConfigForward, HostBulkDeleteRequest, HostBulkDeleteResponse, HostBulkUpdateRequest,
    HostBulkUpdateResponse, HostPreviewResponse, HostUpsertRequest, OpenSshMetadataPatch,
    OpenSshProfileMetadata, SshConfigResponse, SshKeysResponse,
};
use muxus_core::collate::locale_compare;
use muxus_core::js::utf16_len;
use muxus_ssh::config::edit::{
    UpsertResult, delete_host, delete_hosts, patch_hosts, preview_host, upsert_host,
};
use muxus_ssh::config::{OptionLine, list_hosts, load_config_document};
use muxus_ssh::folder_auth::batch_folder_auth_resolver;

use crate::adapters::StoreFolderAuth;
use crate::backend::Backend;
use crate::error::{ApiError, ApiResult};
use crate::routes::hosts::check_metadata_patch;

const TOO_SMALL_STRING: &str = "Too small: expected string to have >=1 characters";
const TOO_SMALL_NUMBER: &str = "Too small: expected number to be >=1";

fn check_forwards(forwards: &[ConfigForward]) -> Result<(), &'static str> {
    for forward in forwards {
        if forward.bind_port == 0 || forward.target_port == Some(0) {
            return Err(TOO_SMALL_NUMBER);
        }
        if forward.target_host.as_deref().is_some_and(str::is_empty) {
            return Err(TOO_SMALL_STRING);
        }
    }
    Ok(())
}

/// The zod checks of `upsertSchema` the typed request does not already make.
fn check_upsert(request: &HostUpsertRequest) -> ApiResult<()> {
    let problem = if request.aliases.is_empty() {
        Some("Too small: expected array to have >=1 items")
    } else if request.aliases.iter().any(String::is_empty) {
        Some(TOO_SMALL_STRING)
    } else if request
        .description
        .as_deref()
        .is_some_and(|description| utf16_len(description) > 2000)
    {
        Some("Too big: expected string to have <=2000 characters")
    } else if request.options.port == Some(0) {
        Some(TOO_SMALL_NUMBER)
    } else {
        check_forwards(request.options.forwards.as_deref().unwrap_or_default()).err()
    };
    match problem {
        Some(message) => Err(ApiError::bad_request(message)),
        None => Ok(()),
    }
}

fn check_aliases(aliases: &[String]) -> ApiResult<()> {
    if aliases.is_empty() {
        return Err(ApiError::bad_request(
            "Too small: expected array to have >=1 items",
        ));
    }
    if aliases.len() > 5000 {
        return Err(ApiError::bad_request(
            "Too big: expected array to have <=5000 items",
        ));
    }
    if aliases.iter().any(String::is_empty) {
        return Err(ApiError::bad_request(TOO_SMALL_STRING));
    }
    Ok(())
}

impl Backend {
    /// `GET /api/ssh/config`: hosts with resolved values that include folder defaults, so the
    /// list and the editor show exactly what connect will use.
    pub async fn ssh_config(&self) -> ApiResult<SshConfigResponse> {
        self.blocking(|shared| {
            let path = shared.ssh_config_path();
            let doc = load_config_document(&path, &shared.local_env);
            let source = StoreFolderAuth(shared.database.clone());
            let mut folder_auth = batch_folder_auth_resolver(&source);
            let mut fallback = |alias: &str| -> Option<Vec<OptionLine>> {
                folder_auth(alias).map(|defaults| defaults.option_lines)
            };
            let mut hosts = list_hosts(&doc, Some(&mut fallback));
            hosts.sort_by(|a, b| locale_compare(&a.alias, &b.alias));
            let aliases: Vec<&str> = hosts.iter().map(|host| host.alias.as_str()).collect();
            let metadata = shared.database.open_ssh_metadata(&aliases)?;
            for host in &mut hosts {
                host.metadata = metadata
                    .iter()
                    .find(|(alias, _)| *alias == host.alias)
                    .map(|(_, metadata)| metadata.clone());
            }
            Ok(SshConfigResponse {
                path: path.to_string_lossy().into_owned(),
                files: doc
                    .file_order
                    .iter()
                    .map(|file| file.to_string_lossy().into_owned())
                    .collect(),
                hosts,
                error: doc.error.clone(),
            })
        })
        .await
    }

    /// `POST /api/ssh/config/hosts`: create or replace a Host block. A renamed block takes its
    /// Muxus metadata along.
    pub async fn upsert_ssh_host(&self, request: HostUpsertRequest) -> ApiResult<UpsertResult> {
        check_upsert(&request)?;
        self.blocking(move |shared| {
            let result = upsert_host(&request, &shared.ssh_config_path(), &shared.local_env)?;
            if let Some(previous) = &request.previous_alias {
                shared
                    .database
                    .rename_open_ssh_alias(previous, &request.aliases[0])?;
            }
            Ok(result)
        })
        .await
    }

    /// `PATCH /api/ssh/config/hosts`: one change across many hosts, in a single write per
    /// config file.
    pub async fn patch_ssh_hosts(
        &self,
        request: HostBulkUpdateRequest,
    ) -> ApiResult<HostBulkUpdateResponse> {
        check_aliases(&request.aliases)?;
        let options = &request.options;
        if options.user.is_keep()
            && options.port.is_keep()
            && options.forward_agent.is_keep()
            && options.forward_x11.is_keep()
            && options.strict_host_key_checking.is_keep()
        {
            return Err(ApiError::bad_request("at least one option is required"));
        }
        if matches!(options.port, muxus_core::js::Patch::Set(0)) {
            return Err(ApiError::bad_request(TOO_SMALL_NUMBER));
        }
        self.blocking(move |shared| {
            Ok(HostBulkUpdateResponse {
                updated: patch_hosts(
                    &request.aliases,
                    &request.options,
                    &shared.ssh_config_path(),
                    &shared.local_env,
                )?,
            })
        })
        .await
    }

    /// `POST /api/ssh/config/preview`: the exact text an upsert would write.
    pub async fn preview_ssh_host(
        &self,
        request: HostUpsertRequest,
    ) -> ApiResult<HostPreviewResponse> {
        check_upsert(&request)?;
        self.blocking(move |shared| {
            Ok(HostPreviewResponse {
                text: preview_host(&request, &shared.ssh_config_path(), &shared.local_env)?,
            })
        })
        .await
    }

    /// `DELETE /api/ssh/config/hosts/:alias`; the host's logging policy goes with it.
    pub async fn delete_ssh_host(&self, alias: &str) -> ApiResult<()> {
        let alias = alias.to_string();
        self.blocking(move |shared| {
            delete_host(&alias, &shared.ssh_config_path(), &shared.local_env)?;
            shared
                .database
                .delete_session_logging_policy(&format!("ssh:{alias}"))?;
            Ok(())
        })
        .await
    }

    /// `DELETE /api/ssh/config/hosts`: many hosts at once, in a single write per config file.
    pub async fn delete_ssh_hosts(
        &self,
        request: HostBulkDeleteRequest,
    ) -> ApiResult<HostBulkDeleteResponse> {
        check_aliases(&request.aliases)?;
        self.blocking(move |shared| {
            let deleted = delete_hosts(
                &request.aliases,
                &shared.ssh_config_path(),
                &shared.local_env,
            )?;
            for alias in &request.aliases {
                shared
                    .database
                    .delete_session_logging_policy(&format!("ssh:{alias}"))?;
            }
            Ok(HostBulkDeleteResponse { deleted })
        })
        .await
    }

    /// `PATCH /api/ssh/config/hosts/:alias/metadata`.
    pub async fn patch_ssh_host_metadata(
        &self,
        alias: &str,
        patch: OpenSshMetadataPatch,
    ) -> ApiResult<OpenSshProfileMetadata> {
        let mut patch = patch;
        check_metadata_patch(&mut patch)?;
        let alias = alias.to_string();
        self.blocking(move |shared| Ok(shared.database.update_open_ssh_metadata(&alias, &patch)?))
            .await
    }

    /// `GET /api/ssh/keys`: the key picker's agent identities and private key files.
    pub async fn ssh_keys(&self, identity_agent: Option<&str>) -> ApiResult<SshKeysResponse> {
        if identity_agent.is_some_and(|agent| utf16_len(agent) > 4096) {
            return Err(ApiError::bad_request(
                "Too big: expected string to have <=4096 characters",
            ));
        }
        let identity_agent = identity_agent.map(str::to_string);
        self.run(move |shared| async move {
            let lookup = shared.env_lookup.clone();
            let socket = muxus_ssh::key_scan::resolve_agent_socket(
                identity_agent.as_deref(),
                &|name: &str| lookup(name),
                &shared.local_env,
            );
            Ok(
                muxus_ssh::key_scan::list_ssh_keys(&shared.local_env.ssh_dir(), socket.as_deref())
                    .await,
            )
        })
        .await
    }
}
