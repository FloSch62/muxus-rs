//! A range save over [`muxus_session::Backend`]: the same requests, in the
//! same order, as a single save (`rangeSaveSteps` of `HostEditorDialog.tsx`).

use muxus_core::api::{
    HostBatchCreateRequest, HostUpsertRequest, SavedHostProfile, SavedHostProfileInput,
};
use muxus_session::Backend;

use super::logic::draft::{HostDraft, draft_metadata_patch};
use super::logic::range_save::RangeSaveSteps;
use super::logic::session_logging::policy_to_save;

/// The backend calls a range save makes.
pub struct BackendRangeSteps<'a>(pub &'a Backend);

impl RangeSaveSteps for BackendRangeSteps<'_> {
    type Profile = SavedHostProfile;

    async fn create_config_hosts(&self, requests: Vec<HostUpsertRequest>) -> Result<(), String> {
        self.0
            .create_ssh_hosts(HostBatchCreateRequest { hosts: requests })
            .await
            .map(|_| ())
            .map_err(|error| error.message)
    }

    async fn save_profile(&self, input: SavedHostProfileInput) -> Result<SavedHostProfile, String> {
        self.0
            .save_saved_host(input)
            .await
            .map_err(|error| error.message)
    }

    async fn save_settings(
        &self,
        draft: &HostDraft,
        profile: Option<&SavedHostProfile>,
    ) -> Result<(), String> {
        let patch = draft_metadata_patch(draft);
        let key = match profile {
            Some(profile) => {
                self.0
                    .patch_saved_host_metadata(&profile.id, patch)
                    .await
                    .map_err(|error| error.message)?;
                format!("profile:{}", profile.id)
            }
            None => {
                let alias = &draft.alias_text;
                self.0
                    .patch_ssh_host_metadata(alias, patch)
                    .await
                    .map_err(|error| error.message)?;
                format!("ssh:{alias}")
            }
        };
        match policy_to_save(&draft.session_logging) {
            Some(policy) => self
                .0
                .put_session_logging_policy(&key, policy)
                .await
                .map(|_| ())
                .map_err(|error| error.message),
            None => self
                .0
                .delete_session_logging_policy(&key)
                .await
                .map(|_| ())
                .map_err(|error| error.message),
        }
    }
}
