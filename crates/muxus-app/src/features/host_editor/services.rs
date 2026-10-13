//! The writes the editors perform, in the order the TypeScript mutations run
//! them, over [`muxus_session::Backend`]. Every function is async and
//! executor-agnostic, so a view awaits it on GPUI's executor.

use std::collections::HashMap;

use muxus_core::api::{
    FolderSettingsRecord, HostBulkUpdateRequest, OpenSshMetadataPatch, PasswordVaultStatus,
    SavedHostProfile, SessionLoggingPolicy, SessionLoggingPolicyInput, SshConfigResponse,
    TunnelRecord,
};
use muxus_core::js::Patch;
use muxus_session::Backend;
use muxus_session::routes::folders::FolderSettingsUpsert;
use muxus_store::prefs::PrefsState;

use super::logic::bulk_edit::{BulkEditPlan, host_logging_key};
use super::logic::desktop_draft::{
    DesktopHostDraft, DesktopKind, desktop_draft_metadata_patch, desktop_draft_to_input,
};
use super::logic::draft::{
    HostDraft, Storage, draft_aliases, draft_metadata_patch, draft_to_request,
    draft_to_saved_ssh_input,
};
use super::logic::folder::{FolderSettingsWrite, FolderSubmit};
use super::logic::management_draft::{
    ManagementHostDraft, ManagementKind, management_draft_metadata_patch, management_draft_to_input,
};
use super::logic::native_draft::{
    NativeHostDraft, NativeKind, native_draft_metadata_patch, native_draft_to_input,
};
use super::logic::session_logging::{HostSessionLoggingDraft, policy_to_save};
use super::logic::tunnel::TunnelDraft;
use crate::hosts::managed::{ManagedHost, managed_host_key};
use crate::sidebar::folder_mutations::FolderMove;

/// What to open once a save finishes with "Save & connect".
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectTarget {
    /// An ssh_config alias.
    Alias(String),
    /// A host saved in Muxus.
    SavedHost(Box<SavedHostProfile>),
}

/// Everything the editors read besides their own drafts.
#[derive(Debug, Clone, Default)]
pub struct EditorData {
    pub ssh_config: Option<SshConfigResponse>,
    pub saved_hosts: Vec<SavedHostProfile>,
    pub folders: Vec<FolderSettingsRecord>,
    pub prefs: PrefsState,
    pub vault: Option<PasswordVaultStatus>,
    pub app_info: Option<muxus_core::api::AppInfo>,
    pub x11: Option<muxus_core::api::X11Status>,
}

impl EditorData {
    /// Every folder path a picker offers: groups in use, their ancestors and
    /// the empty folders the user created.
    pub fn known_folders(&self) -> Vec<String> {
        let hosts = self
            .ssh_config
            .as_ref()
            .map(|c| c.hosts.as_slice())
            .unwrap_or(&[]);
        crate::hosts::tree::known_folder_paths(
            hosts,
            &self.saved_hosts,
            &self.prefs.sidebar_empty_folders,
        )
    }
}

/// Load what the editors read. A source that fails stays empty; the editor
/// still opens, as the TypeScript does while a query errors.
pub async fn load_editor_data(backend: &Backend) -> EditorData {
    let (ssh_config, saved, folders, prefs, vault, app_info, x11) = futures_lite::future::zip(
        futures_lite::future::zip(
            futures_lite::future::zip(backend.ssh_config(), backend.saved_hosts()),
            futures_lite::future::zip(backend.folder_settings(), backend.prefs()),
        ),
        futures_lite::future::zip(
            futures_lite::future::zip(backend.vault_status(), backend.app_info()),
            backend.x11_status(),
        ),
    )
    .await
    .into_flat();
    EditorData {
        ssh_config: ssh_config.ok(),
        saved_hosts: saved.map(|r| r.profiles).unwrap_or_default(),
        folders: folders.map(|r| r.folders).unwrap_or_default(),
        prefs: prefs.unwrap_or_default(),
        vault: vault.ok(),
        app_info: app_info.ok(),
        x11: x11.ok(),
    }
}

trait Flatten<T> {
    fn into_flat(self) -> T;
}

type Loaded = (
    muxus_session::ApiResult<SshConfigResponse>,
    muxus_session::ApiResult<muxus_core::api::SavedHostProfilesResponse>,
    muxus_session::ApiResult<muxus_core::api::FolderSettingsResponse>,
    muxus_session::ApiResult<PrefsState>,
    muxus_session::ApiResult<PasswordVaultStatus>,
    muxus_session::ApiResult<muxus_core::api::AppInfo>,
    muxus_session::ApiResult<muxus_core::api::X11Status>,
);

#[allow(clippy::type_complexity)]
impl Flatten<Loaded>
    for (
        (
            (
                muxus_session::ApiResult<SshConfigResponse>,
                muxus_session::ApiResult<muxus_core::api::SavedHostProfilesResponse>,
            ),
            (
                muxus_session::ApiResult<muxus_core::api::FolderSettingsResponse>,
                muxus_session::ApiResult<PrefsState>,
            ),
        ),
        (
            (
                muxus_session::ApiResult<PasswordVaultStatus>,
                muxus_session::ApiResult<muxus_core::api::AppInfo>,
            ),
            muxus_session::ApiResult<muxus_core::api::X11Status>,
        ),
    )
{
    fn into_flat(self) -> Loaded {
        let (((a, b), (c, d)), ((e, f), g)) = self;
        (a, b, c, d, e, f, g)
    }
}

/// Write or remove a host's logging override.
async fn save_logging(
    backend: &Backend,
    key: &str,
    policy: Option<SessionLoggingPolicyInput>,
) -> Result<(), String> {
    match policy {
        Some(policy) => backend
            .put_session_logging_policy(key, policy)
            .await
            .map(|_| ())
            .map_err(|e| e.message),
        None => backend
            .delete_session_logging_policy(key)
            .await
            .map(|_| ())
            .map_err(|e| e.message),
    }
}

/// The logging policy an editor starts from.
pub async fn logging_policy(backend: &Backend, key: &str) -> Option<SessionLoggingPolicy> {
    backend.session_logging_policy(key).await.ok()
}

/// Save an SSH host: the Host block (or the Muxus profile), then its
/// metadata, then its logging policy (`SshHostEditorContent`'s chain).
/// `existing_id` is the saved profile being edited (or saved earlier in this
/// dialog), `previous_alias` the Host block being replaced.
pub async fn save_ssh_host(
    backend: &Backend,
    draft: &HostDraft,
    previous_alias: Option<&str>,
    existing_id: Option<&str>,
) -> Result<(ConnectTarget, Option<SavedHostProfile>), String> {
    let patch = draft_metadata_patch(draft);
    let policy = policy_to_save(&draft.session_logging);
    if draft.storage == Storage::Muxus {
        let saved = backend
            .save_saved_host(draft_to_saved_ssh_input(draft, existing_id))
            .await
            .map_err(|e| e.message)?;
        let profile = backend
            .patch_saved_host_metadata(&saved.id, patch)
            .await
            .map_err(|e| e.message)?;
        save_logging(backend, &format!("profile:{}", profile.id), policy).await?;
        return Ok((
            ConnectTarget::SavedHost(Box::new(profile.clone())),
            Some(profile),
        ));
    }
    let request = draft_to_request(draft, previous_alias);
    let alias = request.aliases.first().cloned().unwrap_or_default();
    backend
        .upsert_ssh_host(request)
        .await
        .map_err(|e| e.message)?;
    backend
        .patch_ssh_host_metadata(&alias, patch)
        .await
        .map_err(|e| e.message)?;
    save_logging(backend, &format!("ssh:{alias}"), policy).await?;
    let connect = draft_aliases(draft).into_iter().next().unwrap_or_default();
    Ok((ConnectTarget::Alias(connect), None))
}

/// The text a live preview of the Host block shows.
pub async fn preview_ssh_host(
    backend: &Backend,
    draft: &HostDraft,
    previous_alias: Option<&str>,
) -> Result<String, String> {
    backend
        .preview_ssh_host(draft_to_request(draft, previous_alias))
        .await
        .map(|preview| preview.text)
        .map_err(|e| e.message)
}

/// Save a Telnet or serial host, its metadata and its logging policy.
pub async fn save_native_host(
    backend: &Backend,
    draft: &NativeHostDraft,
    kind: NativeKind,
    existing_id: Option<&str>,
) -> Result<SavedHostProfile, String> {
    let saved = backend
        .save_saved_host(native_draft_to_input(draft, kind, existing_id))
        .await
        .map_err(|e| e.message)?;
    let profile = backend
        .patch_saved_host_metadata(&saved.id, native_draft_metadata_patch(draft))
        .await
        .map_err(|e| e.message)?;
    save_logging(
        backend,
        &format!("profile:{}", profile.id),
        policy_to_save(&draft.session_logging),
    )
    .await?;
    Ok(profile)
}

pub async fn save_desktop_host(
    backend: &Backend,
    draft: &DesktopHostDraft,
    kind: DesktopKind,
    existing_id: Option<&str>,
) -> Result<SavedHostProfile, String> {
    let saved = backend
        .save_saved_host(desktop_draft_to_input(draft, kind, existing_id))
        .await
        .map_err(|e| e.message)?;
    backend
        .patch_saved_host_metadata(&saved.id, desktop_draft_metadata_patch(draft))
        .await
        .map_err(|e| e.message)
}

pub async fn save_management_host(
    backend: &Backend,
    draft: &ManagementHostDraft,
    kind: ManagementKind,
    existing_id: Option<&str>,
) -> Result<SavedHostProfile, String> {
    let saved = backend
        .save_saved_host(management_draft_to_input(draft, kind, existing_id))
        .await
        .map_err(|e| e.message)?;
    backend
        .patch_saved_host_metadata(&saved.id, management_draft_metadata_patch(draft))
        .await
        .map_err(|e| e.message)
}

/// Delete a host from its source.
pub async fn delete_host(backend: &Backend, host: &ManagedHost) -> Result<(), String> {
    match host {
        ManagedHost::Ssh(entry) => backend
            .delete_ssh_host(&entry.alias)
            .await
            .map_err(|e| e.message),
        ManagedHost::Profile(entry) => backend
            .delete_saved_host(&entry.id)
            .await
            .map(|_| ())
            .map_err(|e| e.message),
    }
}

/// `confirmDeleteHost`'s wording.
pub fn delete_question(host: &ManagedHost) -> (String, String) {
    match host {
        ManagedHost::Ssh(entry) => (
            format!("Delete “{}”?", entry.alias),
            format!(
                "The Host block is removed from {}. A backup of the previous file is kept next to it as config.muxus.bak.",
                shorten_ssh_path(&entry.file)
            ),
        ),
        ManagedHost::Profile(entry) => (
            format!("Delete “{}”?", entry.name),
            "This removes the saved host from Muxus. It does not change the remote device or serial port.".into(),
        ),
    }
}

/// `confirmDeleteHosts`: the question for a selection of several hosts (title,
/// description, confirm label). It names the hosts, so a stray Ctrl-click does not
/// disappear into a count, and says what happens to each kind.
pub fn delete_hosts_question(hosts: &[ManagedHost]) -> (String, String, String) {
    let names: Vec<String> = hosts
        .iter()
        .map(|host| {
            format!(
                "“{}”",
                crate::hosts::managed::managed_host_display_name(host)
            )
        })
        .collect();
    let listed = if names.len() > 4 {
        format!("{} and {} more", names[..3].join(", "), names.len() - 3)
    } else if names.len() > 1 {
        format!(
            "{} and {}",
            names[..names.len() - 1].join(", "),
            names[names.len() - 1]
        )
    } else {
        names.first().cloned().unwrap_or_default()
    };
    let mut files: Vec<&str> = Vec::new();
    for host in hosts {
        if let ManagedHost::Ssh(entry) = host
            && !files.contains(&entry.file.as_str())
        {
            files.push(&entry.file);
        }
    }
    let blocks = hosts
        .iter()
        .filter(|host| matches!(host, ManagedHost::Ssh(_)))
        .count();
    let saved = hosts.len() - blocks;
    let mut sentences = vec![format!("{listed}.")];
    if let (true, Some(file)) = (blocks > 0, files.first()) {
        sentences.push(if files.len() == 1 {
            let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
            format!(
                "{blocks} Host block{} removed from {}, and a backup of the previous file is kept next to it as {name}.muxus.bak.",
                if blocks == 1 { " is" } else { "s are" },
                shorten_ssh_path(file)
            )
        } else {
            format!(
                "{blocks} Host blocks are removed from {} config files, and a backup of each previous file is kept next to it with a .muxus.bak suffix.",
                files.len()
            )
        });
    }
    if saved > 0 {
        sentences.push(format!(
            "{saved} host{} saved in Muxus {} removed. No remote device or serial port changes.",
            if saved == 1 { "" } else { "s" },
            if saved == 1 { "is" } else { "are" }
        ));
    }
    (
        format!("Delete {} hosts?", hosts.len()),
        sentences.join(" "),
        format!("Delete {} hosts", hosts.len()),
    )
}

/// `deleteFolder`'s question: what happens to the folder's hosts and its shared
/// credentials (`has_credentials` is `None` while unknown).
pub fn delete_folder_question(
    label: &str,
    path: &str,
    moved_hosts: usize,
    has_credentials: Option<bool>,
) -> (String, String) {
    let parent = crate::hosts::tree::folder_parent_path(path);
    let hosts_text = if moved_hosts == 0 {
        "The folder is empty.".to_string()
    } else {
        format!(
            "{moved_hosts} host{} move{} {}.",
            if moved_hosts == 1 { "" } else { "s" },
            if moved_hosts == 1 { "s" } else { "" },
            if parent.is_empty() {
                "out of every folder".to_string()
            } else {
                format!("up into “{parent}”")
            }
        )
    };
    let description = match has_credentials {
        Some(true) => format!("{hosts_text} Its shared SSH credentials are removed."),
        None => format!("{hosts_text} Any shared SSH credentials on it are removed."),
        Some(false) if moved_hosts == 0 => "The folder is empty, so nothing else changes.".into(),
        Some(false) => format!("{hosts_text} No connection settings change."),
    };
    (format!("Delete “{label}”?"), description)
}

/// `shortenSshPath`: `~/.ssh/…` for anything inside a `.ssh` folder.
pub fn shorten_ssh_path(path: &str) -> String {
    for (start, _) in path.char_indices().rev() {
        let rest = &path[start..];
        let mut chars = rest.chars();
        let sep = |c: Option<char>| matches!(c, Some('/' | '\\'));
        if sep(chars.next()) && chars.as_str().starts_with(".ssh") {
            let after = &chars.as_str()[4..];
            if sep(after.chars().next()) {
                return format!("~/.ssh/{}", &after[1..]);
            }
        }
    }
    path.to_owned()
}

/// Write a host's Muxus metadata.
pub async fn patch_metadata(
    backend: &Backend,
    host: &ManagedHost,
    patch: OpenSshMetadataPatch,
) -> Result<(), String> {
    match host {
        ManagedHost::Ssh(entry) => backend
            .patch_ssh_host_metadata(&entry.alias, patch)
            .await
            .map(|_| ())
            .map_err(|e| e.message),
        ManagedHost::Profile(entry) => backend
            .patch_saved_host_metadata(&entry.id, patch)
            .await
            .map(|_| ())
            .map_err(|e| e.message),
    }
}

/// How a bulk edit went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkEditResult {
    pub attempted: usize,
    /// Hosts with at least one write that did not go through.
    pub failed: usize,
}

impl BulkEditResult {
    /// The toast the client shows afterwards.
    pub fn message(&self) -> (bool, String) {
        if self.failed > 0 {
            (
                false,
                format!(
                    "Updated {} of {} hosts — some changes did not save.",
                    self.attempted - self.failed,
                    self.attempted
                ),
            )
        } else {
            (
                true,
                format!(
                    "Updated {} host{}.",
                    self.attempted,
                    if self.attempted == 1 { "" } else { "s" }
                ),
            )
        }
    }
}

/// Apply a bulk edit. Every OpenSSH host goes in one request that the backend
/// writes all at once, so ssh_config either takes the whole change or none of
/// it — and when it refuses, nothing else is written either. The Muxus-side
/// writes then run per host, eight at a time.
pub async fn apply_bulk_edit(
    backend: &Backend,
    plan: &BulkEditPlan,
) -> Result<BulkEditResult, String> {
    if let Some(open_ssh) = &plan.open_ssh {
        backend
            .patch_ssh_hosts(HostBulkUpdateRequest {
                aliases: open_ssh.aliases.clone(),
                options: open_ssh.options.clone(),
            })
            .await
            .map_err(|e| e.message)?;
    }
    enum Write {
        Profile(muxus_core::api::SavedHostProfileInput),
        Metadata(ManagedHost, OpenSshMetadataPatch),
        Logging(String, Option<SessionLoggingPolicyInput>),
    }
    let mut writes: Vec<(String, Write)> = Vec::new();
    for input in &plan.profiles {
        writes.push((
            format!("profile:{}", input.id.clone().unwrap_or_default()),
            Write::Profile(input.clone()),
        ));
    }
    for (host, patch) in &plan.metadata {
        writes.push((
            managed_host_key(host),
            Write::Metadata(host.clone(), patch.clone()),
        ));
    }
    for write in &plan.logging {
        writes.push((
            write.profile_key.clone(),
            Write::Logging(write.profile_key.clone(), write.policy.clone()),
        ));
    }
    // The TypeScript sends eight at a time; the backend serializes its store
    // writes anyway, so one after the other gives the same result.
    let mut failed = std::collections::HashSet::new();
    for (key, write) in writes {
        let result = match write {
            Write::Profile(input) => backend
                .save_saved_host(input)
                .await
                .map(|_| ())
                .map_err(|e| e.message),
            Write::Metadata(host, patch) => patch_metadata(backend, &host, patch).await,
            Write::Logging(key, policy) => save_logging(backend, &key, policy).await,
        };
        if result.is_err() {
            failed.insert(key);
        }
    }
    Ok(BulkEditResult {
        attempted: plan.hosts.len(),
        failed: failed.len(),
    })
}

/// Load the logging policies of the hosts a bulk edit shows, by their key.
pub async fn logging_policies(
    backend: &Backend,
    hosts: &[ManagedHost],
) -> HashMap<String, SessionLoggingPolicy> {
    let mut policies = HashMap::new();
    for host in hosts {
        let key = host_logging_key(host);
        if let Ok(policy) = backend.session_logging_policy(&key).await {
            policies.insert(key, policy);
        }
    }
    policies
}

/// Apply folder moves: one metadata patch per host (`useApplyFolderMoves`).
pub async fn apply_folder_moves(backend: &Backend, moves: &[FolderMove]) -> (usize, usize) {
    let mut failed = 0;
    for chunk in moves.chunks(8) {
        for item in chunk {
            let patch = OpenSshMetadataPatch {
                group: match &item.group {
                    Some(group) => Patch::Set(group.clone()),
                    None => Patch::Clear,
                },
                ..OpenSshMetadataPatch::default()
            };
            if patch_metadata(backend, &item.host, patch).await.is_err() {
                failed += 1;
            }
        }
    }
    (moves.len(), failed)
}

/// Write a folder's shared credentials and login sequence (`useSaveFolderSettings`).
pub async fn save_folder_settings(
    backend: &Backend,
    write: &FolderSettingsWrite,
) -> Result<(), String> {
    backend
        .put_folder_settings(FolderSettingsUpsert {
            path: write.path.clone(),
            auth: write.auth.clone(),
            login_sequence: match &write.login_sequence {
                None => Patch::Keep,
                Some(None) => Patch::Clear,
                Some(Some(sequence)) => Patch::Set(sequence.clone()),
            },
        })
        .await
        .map_err(|e| e.message)?;
    match &write.password {
        Some(Some(password)) => {
            backend
                .put_folder_password(&write.path, password, write.master_password.as_deref())
                .await
                .map_err(|e| e.message)?;
        }
        Some(None) => {
            backend
                .delete_folder_password(&write.path)
                .await
                .map_err(|e| e.message)?;
        }
        None => {}
    }
    Ok(())
}

/// The backend half of a folder dialog submit (the prefs half is the caller's).
pub async fn apply_folder_submit(backend: &Backend, submit: &FolderSubmit) -> Result<(), String> {
    if !submit.moves.is_empty() {
        let (_, failed) = apply_folder_moves(backend, &submit.moves).await;
        if failed > 0 {
            return Err(format!(
                "{failed} host{} could not be moved.",
                if failed == 1 { "" } else { "s" }
            ));
        }
    }
    let Some(write) = &submit.credentials else {
        if let Some((from, to)) = &submit.move_settings {
            backend
                .move_folder_settings(from, to)
                .await
                .map_err(|e| e.message)?;
        }
        return Ok(());
    };
    if let Some((from, to)) = &submit.move_settings {
        let moved = backend
            .move_folder_settings(from, to)
            .await
            .map_err(|e| e.message)?;
        if moved.destination_preserved {
            return Ok(());
        }
    }
    save_folder_settings(backend, write).await
}

/// Save a persistent tunnel.
pub async fn save_tunnel(backend: &Backend, draft: &TunnelDraft) -> Result<TunnelRecord, String> {
    backend
        .save_tunnel(draft.to_input())
        .await
        .map_err(|e| e.message)
}

/// Whether a session logging draft already matches a loaded policy (used by
/// the editors to know when the policy arrived).
pub fn logging_loaded(draft: &HostSessionLoggingDraft) -> bool {
    draft.loaded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::host_editor::demo::{DemoSandbox, seed};
    use crate::features::host_editor::logic::bulk_edit::{BulkHostChanges, bulk_edit_plan};
    use crate::features::host_editor::logic::draft::{RouteMode, blank_draft, draft_from_entry};
    use crate::features::host_editor::logic::folder::{
        FolderDialogState, FolderDraft, VaultState, plan_submit,
    };
    use crate::features::host_editor::logic::native_draft::blank_native_draft;
    use crate::features::host_editor::logic::tunnel::TunnelEditorState;
    use crate::sidebar::folder_prefs::FolderPrefs;
    use futures_lite::future::block_on;
    use muxus_core::api::{ConfigForward, ForwardType};

    fn sandbox() -> (tempfile::TempDir, DemoSandbox, Backend) {
        let dir = tempfile::tempdir().expect("scratch dir");
        let sandbox = DemoSandbox::create(dir.path()).expect("demo sandbox");
        let backend = sandbox.backend().expect("backend");
        block_on(seed(&backend)).expect("seed");
        (dir, sandbox, backend)
    }

    fn ssh_config(sandbox: &DemoSandbox) -> String {
        std::fs::read_to_string(sandbox.home.join(".ssh").join("config")).expect("ssh config")
    }

    fn entry(backend: &Backend, alias: &str) -> muxus_core::api::SshHostEntry {
        block_on(backend.ssh_config())
            .expect("config")
            .hosts
            .into_iter()
            .find(|h| h.alias == alias)
            .expect("host")
    }

    #[test]
    fn asks_about_several_hosts_by_name_and_kind() {
        let (_dir, _sandbox, backend) = sandbox();
        let web = ManagedHost::ssh(entry(&backend, "web-01"));
        let cache = ManagedHost::ssh(entry(&backend, "cache-01"));
        let saved = block_on(backend.saved_hosts()).expect("saved").profiles;
        let profile = ManagedHost::profile(saved[0].clone());
        let (title, description, label) =
            delete_hosts_question(&[web.clone(), cache.clone(), profile.clone()]);
        assert_eq!(title, "Delete 3 hosts?");
        assert_eq!(label, "Delete 3 hosts");
        assert!(description.starts_with(&format!(
            "“web-01”, “cache-01” and “{}”.",
            crate::hosts::managed::managed_host_display_name(&profile)
        )));
        assert!(description.contains(
            "2 Host blocks are removed from ~/.ssh/config, and a backup of the previous file is kept next to it as config.muxus.bak."
        ));
        assert!(description.ends_with(
            "1 host saved in Muxus is removed. No remote device or serial port changes."
        ));
        let five = vec![web.clone(), cache.clone(), web.clone(), cache.clone(), web];
        assert!(
            delete_hosts_question(&five)
                .1
                .starts_with("“web-01”, “cache-01”, “web-01” and 2 more.")
        );
    }

    #[test]
    fn says_what_deleting_a_folder_does() {
        assert_eq!(
            delete_folder_question("Web", "Production/Web", 2, Some(true)),
            (
                "Delete “Web”?".to_string(),
                "2 hosts move up into “Production”. Its shared SSH credentials are removed."
                    .to_string()
            )
        );
        assert_eq!(
            delete_folder_question("Lab", "Lab", 0, Some(false)).1,
            "The folder is empty, so nothing else changes."
        );
        assert_eq!(
            delete_folder_question("Lab", "Lab", 1, None).1,
            "1 host moves out of every folder. Any shared SSH credentials on it are removed."
        );
    }

    #[test]
    fn saves_a_new_openssh_host_with_its_metadata() {
        let (_dir, sandbox, backend) = sandbox();
        let mut draft = blank_draft("ops@edge-01.lab.test:2222", "Lab/Edge", Storage::Openssh);
        draft.alias_text = "edge-01".into();
        draft.color = Some("#4caf72".into());
        draft.forwards.push(ConfigForward {
            kind: ForwardType::Local,
            bind_port: 8443,
            target_host: Some("127.0.0.1".into()),
            target_port: Some(443),
        });
        draft.session_logging.loaded = true;
        let preview = block_on(preview_ssh_host(&backend, &draft, None)).expect("preview");
        assert!(preview.contains("Host edge-01"));
        let (connect, profile) =
            block_on(save_ssh_host(&backend, &draft, None, None)).expect("save");
        assert_eq!(connect, ConnectTarget::Alias("edge-01".into()));
        assert_eq!(profile, None);
        let config = ssh_config(&sandbox);
        assert!(config.contains("Host edge-01"), "{config}");
        assert!(config.contains("HostName edge-01.lab.test"));
        assert!(config.contains("User ops"));
        assert!(config.contains("Port 2222"));
        assert!(config.contains("LocalForward 8443 127.0.0.1:443"));
        let saved = entry(&backend, "edge-01");
        let metadata = saved.metadata.expect("metadata");
        assert_eq!(metadata.group.as_deref(), Some("Lab/Edge"));
        assert_eq!(metadata.color.as_deref(), Some("#4caf72"));
    }

    #[test]
    fn renames_an_alias_and_keeps_its_metadata() {
        let (_dir, sandbox, backend) = sandbox();
        let mut draft = draft_from_entry(&entry(&backend, "db-primary"), false);
        draft.session_logging.loaded = true;
        draft.alias_text = "db-main".into();
        draft.route_mode = RouteMode::Direct;
        block_on(save_ssh_host(&backend, &draft, Some("db-primary"), None)).expect("save");
        let config = ssh_config(&sandbox);
        assert!(config.contains("Host db-main"));
        assert!(!config.contains("Host db-primary"));
        assert!(config.contains("LocalForward 15432 127.0.0.1:5432"));
        let renamed = entry(&backend, "db-main");
        assert_eq!(renamed.options.proxy_jump, None, "the route is now direct");
        assert_eq!(
            renamed.metadata.and_then(|m| m.display_name).as_deref(),
            Some("db-primary (eu-central)")
        );
    }

    #[test]
    fn saves_muxus_owned_ssh_hosts_and_updates_them_in_place() {
        let (_dir, sandbox, backend) = sandbox();
        let before = ssh_config(&sandbox);
        let mut draft = blank_draft("router.lab.test", "Lab", Storage::Muxus);
        draft.session_logging.loaded = true;
        draft.session_logging.inherit = false;
        draft.session_logging.enabled = true;
        let (connect, profile) =
            block_on(save_ssh_host(&backend, &draft, None, None)).expect("save");
        let profile = profile.expect("a saved profile");
        assert!(matches!(connect, ConnectTarget::SavedHost(_)));
        assert_eq!(ssh_config(&sandbox), before, "ssh_config is unchanged");
        let policy = block_on(backend.session_logging_policy(&format!("profile:{}", profile.id)))
            .expect("policy");
        assert!(policy.overridden && policy.enabled);
        draft.alias_text = "Router".into();
        let (_, again) =
            block_on(save_ssh_host(&backend, &draft, None, Some(&profile.id))).expect("update");
        assert_eq!(again.expect("profile").id, profile.id);
        let saved = block_on(backend.saved_hosts()).expect("hosts").profiles;
        assert_eq!(saved.iter().filter(|p| p.name == "Router").count(), 1);
    }

    #[test]
    fn saves_telnet_hosts_and_deletes_hosts() {
        let (_dir, _sandbox, backend) = sandbox();
        let mut draft = blank_native_draft("10.0.0.9:2323", "Lab");
        draft.name = "Switch".into();
        draft.session_logging.loaded = true;
        let profile =
            block_on(save_native_host(&backend, &draft, NativeKind::Telnet, None)).expect("save");
        assert_eq!(profile.metadata.group.as_deref(), Some("Lab"));
        let host = ManagedHost::profile(profile.clone());
        block_on(delete_host(&backend, &host)).expect("delete");
        let saved = block_on(backend.saved_hosts()).expect("hosts").profiles;
        assert!(!saved.iter().any(|p| p.id == profile.id));
        let ssh = ManagedHost::ssh(entry(&backend, "cache-01"));
        block_on(delete_host(&backend, &ssh)).expect("delete ssh");
        assert!(
            block_on(backend.ssh_config())
                .expect("config")
                .hosts
                .iter()
                .all(|h| h.alias != "cache-01")
        );
        assert_eq!(delete_question(&ssh).0, "Delete “cache-01”?");
    }

    #[test]
    fn applies_a_bulk_edit_to_openssh_and_muxus_hosts() {
        let (_dir, sandbox, backend) = sandbox();
        let hosts = vec![
            ManagedHost::ssh(entry(&backend, "web-01")),
            ManagedHost::ssh(entry(&backend, "web-02")),
        ];
        let changes = BulkHostChanges {
            user: Some("www".into()),
            color: Some(Some("#ef5350".into())),
            ..BulkHostChanges::default()
        };
        let plan = bulk_edit_plan(&hosts, &changes, &HashMap::new());
        let result = block_on(apply_bulk_edit(&backend, &plan)).expect("apply");
        assert_eq!(
            result,
            BulkEditResult {
                attempted: 2,
                failed: 0
            }
        );
        assert_eq!(result.message(), (true, "Updated 2 hosts.".into()));
        let config = ssh_config(&sandbox);
        assert_eq!(config.matches("User www").count(), 2, "{config}");
        assert_eq!(
            entry(&backend, "web-02")
                .metadata
                .and_then(|m| m.color)
                .as_deref(),
            Some("#ef5350")
        );
    }

    #[test]
    fn renames_a_folder_with_its_hosts_and_credentials() {
        let (_dir, _sandbox, backend) = sandbox();
        let data = block_on(load_editor_data(&backend));
        let mut hosts: Vec<ManagedHost> = data
            .ssh_config
            .as_ref()
            .unwrap()
            .hosts
            .iter()
            .cloned()
            .map(ManagedHost::ssh)
            .collect();
        hosts.extend(data.saved_hosts.iter().cloned().map(ManagedHost::profile));
        let state = FolderDialogState::Edit {
            path: "Production/Web".into(),
        };
        let mut draft = FolderDraft::new(&state, &FolderPrefs::default(), None);
        draft.name = "Frontend".into();
        draft.auth_user = "deploy".into();
        let submit =
            plan_submit(&state, &draft, &hosts, None, VaultState::default()).expect("a submit");
        assert_eq!(submit.moves_label.as_deref(), Some("Production/Frontend"));
        block_on(apply_folder_submit(&backend, &submit)).expect("apply");
        let data = block_on(load_editor_data(&backend));
        let group_of = |alias: &str| {
            data.ssh_config
                .as_ref()
                .unwrap()
                .hosts
                .iter()
                .find(|h| h.alias == alias)
                .and_then(|h| h.metadata.as_ref())
                .and_then(|m| m.group.clone())
        };
        assert_eq!(group_of("web-01").as_deref(), Some("Production/Frontend"));
        assert_eq!(group_of("web-02").as_deref(), Some("Production/Frontend"));
        assert_eq!(group_of("db-01").as_deref(), Some("Production/Data"));
        let record = data
            .folders
            .iter()
            .find(|f| f.path == "Production/Frontend")
            .expect("folder settings");
        assert_eq!(record.auth.user.as_deref(), Some("deploy"));
    }

    #[test]
    fn saves_a_tunnel() {
        let (_dir, _sandbox, backend) = sandbox();
        let mut draft = TunnelDraft::new(&TunnelEditorState {
            tunnel: None,
            prefill_target: Some("web-01".into()),
        });
        draft.name = "Web".into();
        draft.bind_port = "18080".into();
        draft.target_port = "80".into();
        let record = block_on(save_tunnel(&backend, &draft)).expect("save");
        assert_eq!(record.target, "web-01");
        assert_eq!(record.bind_port, 18080);
        let edited = TunnelDraft::new(&TunnelEditorState {
            tunnel: Some(record.clone()),
            prefill_target: None,
        });
        assert_eq!(edited.to_input().id, Some(record.id));
    }

    #[test]
    fn shortens_ssh_paths() {
        assert_eq!(shorten_ssh_path("/home/u/.ssh/config"), "~/.ssh/config");
        assert_eq!(
            shorten_ssh_path("/home/u/.ssh/config.d/lab.conf"),
            "~/.ssh/config.d/lab.conf"
        );
        assert_eq!(
            shorten_ssh_path("C:\\Users\\u\\.ssh\\config"),
            "~/.ssh/config"
        );
        assert_eq!(
            shorten_ssh_path("/etc/ssh/ssh_config"),
            "/etc/ssh/ssh_config"
        );
    }
}
