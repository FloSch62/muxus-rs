//! Saved hosts (`server/src/routes/profiles.ts`), the sidebar order (`routes/host-order.ts`) and
//! terminal snapshots (`routes/terminal-snapshots.ts`).

use std::collections::HashSet;

use muxus_core::api::{
    HostOrderRequest, ManagedHostRef, OpenSshMetadataPatch, SavedHostProfile,
    SavedHostProfileInput, SavedHostProfilesResponse, TERMINAL_SNAPSHOT_MAX_CHARS,
    TerminalSnapshotRecord,
};
use muxus_core::js::utf16_len;
use muxus_core::validate::Validate;

use crate::backend::Backend;
use crate::error::{ApiError, ApiResult};

/// The patch must name at least one field (`metadataPatchSchema`'s refine).
pub(crate) fn check_metadata_patch(patch: &mut OpenSshMetadataPatch) -> ApiResult<()> {
    patch.validate().map_err(ApiError::invalid)
}

impl Backend {
    /// `GET /api/profiles`: Muxus-owned SSH, Telnet, serial, RDP, VNC, gNMI and NETCONF hosts.
    pub async fn saved_hosts(&self) -> ApiResult<SavedHostProfilesResponse> {
        self.blocking(|shared| {
            Ok(SavedHostProfilesResponse {
                profiles: shared.database.list_saved_host_profiles()?,
            })
        })
        .await
    }

    /// `PUT /api/profiles`.
    pub async fn save_saved_host(
        &self,
        input: SavedHostProfileInput,
    ) -> ApiResult<SavedHostProfile> {
        let mut input = input;
        input.validate().map_err(ApiError::invalid)?;
        self.blocking(move |shared| Ok(shared.database.save_saved_host_profile(&input)?))
            .await
    }

    /// `PATCH /api/profiles/:id/metadata`.
    pub async fn patch_saved_host_metadata(
        &self,
        id: &str,
        patch: OpenSshMetadataPatch,
    ) -> ApiResult<SavedHostProfile> {
        let mut patch = patch;
        check_metadata_patch(&mut patch)?;
        let id = id.to_string();
        self.blocking(move |shared| Ok(shared.database.update_saved_host_metadata(&id, &patch)?))
            .await
    }

    /// `DELETE /api/profiles/:id`.
    pub async fn delete_saved_host(&self, id: &str) -> ApiResult<bool> {
        let id = id.to_string();
        self.blocking(move |shared| Ok(shared.database.delete_saved_host_profile(&id)?))
            .await
    }

    /// `PUT /api/hosts/order`: one visual sidebar order spanning OpenSSH hosts and saved hosts.
    pub async fn put_host_order(&self, request: HostOrderRequest) -> ApiResult<()> {
        if request.hosts.len() > 10_000 {
            return Err(ApiError::bad_request(
                "Too big: expected array to have <=10000 items",
            ));
        }
        let mut keys = HashSet::new();
        for host in &request.hosts {
            let (prefix, value) = match host {
                ManagedHostRef::Ssh { alias } => ("ssh", alias),
                ManagedHostRef::Profile { id } => ("profile", id),
            };
            let length = utf16_len(value);
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
            keys.insert(format!("{prefix}:{value}"));
        }
        if keys.len() != request.hosts.len() {
            return Err(ApiError::bad_request("host order contains duplicates"));
        }
        self.blocking(move |shared| Ok(shared.database.reorder_managed_hosts(&request.hosts)?))
            .await
    }

    /// `GET /api/terminal-snapshots/:tabId`.
    pub async fn terminal_snapshot(
        &self,
        tab_id: &str,
    ) -> ApiResult<Option<TerminalSnapshotRecord>> {
        let tab_id = tab_id.to_string();
        self.blocking(move |shared| Ok(shared.database.terminal_snapshot(&tab_id)?))
            .await
    }

    /// `PUT /api/terminal-snapshots/:tabId`. A missing format version is version 1.
    pub async fn save_terminal_snapshot(
        &self,
        tab_id: &str,
        data: &str,
        format_version: Option<u32>,
    ) -> ApiResult<()> {
        let length = utf16_len(data);
        if !(1..=TERMINAL_SNAPSHOT_MAX_CHARS).contains(&length) || format_version == Some(0) {
            return Err(ApiError::bad_request("invalid terminal snapshot"));
        }
        let (tab_id, data) = (tab_id.to_string(), data.to_string());
        self.blocking(move |shared| {
            Ok(shared.database.save_terminal_snapshot(
                &tab_id,
                &data,
                format_version.unwrap_or(1),
            )?)
        })
        .await
    }
}
