//! Backups, restores and connection exports (`client/src/data-transfer.ts`),
//! plus the non-visual parts of the Backup & data section
//! (`client/src/components/DataTransferSection.tsx`, in [`section`]).
//!
//! A backup is one portable JSON document (`.muxus`): connections (OpenSSH
//! hosts with their inherited values materialized, saved hosts, the sidebar
//! order), preferences, saved tunnels, logging policies and limits, and
//! folder defaults. It never holds passwords, private keys or recordings.
//! Both builds write the same shape, so a backup made by either restores in
//! the other. Every read and write goes through [`Backend`], the way the
//! TypeScript went through the server routes.

mod sanitize;
pub mod section;
mod validate;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::fmt;

use futures_lite::future::try_zip;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use muxus_core::api::{
    FolderAuthSettings, FolderSettingsRecord, HostBlockOptions, HostKeywordHighlightConfig,
    HostOrderRequest, HostUpsertRequest, LoginSequence, ManagedHostRef, OpenSshMetadataPatch,
    OpenSshProfileMetadata, SavedHostProfile, SavedHostProfileInput, SessionLoggingPolicyInput,
    SshHostEntry, TunnelInput, TunnelRecord,
};
use muxus_core::collate::locale_compare;
use muxus_core::js::{self, Patch};
use muxus_core::protocol::SavedHostSessionProfile;
use muxus_session::login_sequence::editor::is_login_sequence;
use muxus_session::routes::folders::FolderSettingsUpsert;
use muxus_session::{ApiError, Backend};

use crate::hosts::saved::open_ssh_jump_hops;

pub use muxus_store::prefs::LAST_BACKUP_KEY;
pub use sanitize::{apply_preferences_patch, sanitize_preferences};
pub use section::{
    DataSummary, PendingFile, RestoreCategory, RestoreCategoryRow, connection_count,
    dated_transfer_filename, friendly_date, last_backup_line, load_transfer_file,
    openssh_export_filename, plural, proxy_command_count, proxy_command_warning, read_last_backup,
    read_transfer_file, remember_backup, restore_categories, restore_complete_message,
    restore_failed_message, review_subtitle, summary_line,
};
pub use validate::parse_transfer_document;

pub const BACKUP_FORMAT: &str = "muxus-backup";
pub const TRANSFER_VERSION: u32 = 2;
const LEGACY_TRANSFER_VERSION: u32 = 1;
/// Larger files are refused before they are read.
pub const MAX_TRANSFER_FILE_BYTES: u64 = 20 * 1024 * 1024;

/// The preferences a backup carries, in the order it writes them. Window
/// state, workspaces and other per-machine state stay behind.
pub const PREFERENCE_KEYS: &[&str] = &[
    "themeMode",
    "monoFontSize",
    "fontFamily",
    "lineHeight",
    "lightTerminalScheme",
    "darkTerminalScheme",
    "customTerminalSchemes",
    "fontColor",
    "backgroundColor",
    "activePaneBorder",
    "dimInactivePanes",
    "inactivePaneDimStrength",
    "scrollback",
    "cursorBlink",
    "cursorStyle",
    "webglRenderer",
    "localShell",
    "localShellProfiles",
    "defaultLocalShellProfileId",
    "showWslDistributions",
    "copyOnSelect",
    "allowOsc52ClipboardWrite",
    "rightClickAction",
    "terminalFileLinkActivation",
    "pasteWarnMultiline",
    "pasteLineDelayMs",
    "pasteCharDelayMs",
    "confirmCloseConnected",
    "sshSessionSummary",
    "rememberPasswordsByDefault",
    "newSshHostStorage",
    "notifyOnNewVersion",
    "commandButtons",
    "commandButtonGroups",
    "selectedCommandButtonGroup",
    "showCommandBar",
    "commandBarPosition",
    "showStatusBar",
    "statusBarItems",
    "keywordHighlights",
    "keywordHighlightProfiles",
    "sidebarCollapsed",
    "sidebarPosition",
    "fileBrowserPosition",
    "sidebarOpenGesture",
    "sidebarWidth",
    "sidebarCollapsedFolders",
    "sidebarFolderStyles",
    "sidebarFolderOrder",
    "sidebarEmptyFolders",
    "sftpPanelWidth",
];

/// A failed read, backup, export or restore. The message is what the
/// TypeScript showed: its own text, or the backend's answer to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferError {
    pub message: String,
}

impl TransferError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// A value the backend would have refused (the TypeScript sent it and
    /// got the route's validation error back).
    fn invalid(error: serde_json::Error) -> Self {
        Self::new(format!("Invalid input: {error}"))
    }
}

impl fmt::Display for TransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransferError {}

impl From<ApiError> for TransferError {
    fn from(error: ApiError) -> Self {
        Self::new(error.message)
    }
}

/// A transfer file whose envelope and collection shapes have been checked
/// ([`parse_transfer_document`]), or a backup this build just created. The
/// JSON is kept as read, so restoring hands every item to the backend the
/// way the TypeScript did, and the backend remains the strict validator.
#[derive(Debug, Clone, PartialEq)]
pub struct TransferDocument {
    value: Value,
}

impl TransferDocument {
    fn from_validated(value: Value) -> Self {
        Self { value }
    }

    /// 1 for legacy backups, 2 for current ones.
    pub fn version(&self) -> u32 {
        match self.value.get("version").and_then(Value::as_f64) {
            Some(version) if version == f64::from(LEGACY_TRANSFER_VERSION) => {
                LEGACY_TRANSFER_VERSION
            }
            _ => TRANSFER_VERSION,
        }
    }

    pub fn created_at(&self) -> &str {
        self.value
            .get("createdAt")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The Muxus version that wrote the file, when it says.
    pub fn app_version(&self) -> Option<&str> {
        self.value.get("appVersion").and_then(Value::as_str)
    }

    pub fn data(&self) -> &Map<String, Value> {
        self.value
            .get("data")
            .and_then(Value::as_object)
            .unwrap_or_else(|| empty_map())
    }

    fn list(&self, key: &str) -> &[Value] {
        self.data()
            .get(key)
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    pub fn ssh_hosts(&self) -> &[Value] {
        self.list("sshHosts")
    }

    pub fn saved_hosts(&self) -> &[Value] {
        self.list("savedHosts")
    }

    pub fn host_order(&self) -> &[Value] {
        self.list("hostOrder")
    }

    pub fn tunnels(&self) -> &[Value] {
        self.list("tunnels")
    }

    pub fn logging_policies(&self) -> &[Value] {
        self.list("loggingPolicies")
    }

    /// Absent in backups from before folder credentials existed.
    pub fn folder_settings(&self) -> &[Value] {
        self.list("folderSettings")
    }

    pub fn preferences(&self) -> &Map<String, Value> {
        self.data()
            .get("preferences")
            .and_then(Value::as_object)
            .unwrap_or_else(|| empty_map())
    }

    pub fn as_value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }

    /// The file a backup is saved as (`saveTransferDocument`):
    /// `JSON.stringify(document, null, 2)` and a newline, `application/json`.
    pub fn to_file_text(&self) -> String {
        let mut text = serde_json::to_string_pretty(&self.value).unwrap_or_default();
        text.push('\n');
        text
    }
}

fn empty_map() -> &'static Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(Map::new)
}

/// What happens to an item that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferConflictStrategy {
    /// Safest: only missing items are added.
    #[default]
    Keep,
    /// The file's version replaces it, matched by SSH alias or Muxus item id.
    Replace,
}

/// The categories a restore applies. The review dialog starts with all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreSelection {
    pub preferences: bool,
    pub connections: bool,
    pub tunnels: bool,
    pub logging: bool,
}

impl Default for RestoreSelection {
    fn default() -> Self {
        Self {
            preferences: true,
            connections: true,
            tunnels: true,
            logging: true,
        }
    }
}

impl RestoreSelection {
    pub fn get(&self, category: RestoreCategory) -> bool {
        match category {
            RestoreCategory::Preferences => self.preferences,
            RestoreCategory::Connections => self.connections,
            RestoreCategory::Tunnels => self.tunnels,
            RestoreCategory::Logging => self.logging,
        }
    }

    pub fn set(&mut self, category: RestoreCategory, selected: bool) {
        match category {
            RestoreCategory::Preferences => self.preferences = selected,
            RestoreCategory::Connections => self.connections = selected,
            RestoreCategory::Tunnels => self.tunnels = selected,
            RestoreCategory::Logging => self.logging = selected,
        }
    }

    /// How many categories are ticked; Restore needs at least one.
    pub fn selected_count(&self) -> usize {
        [
            self.preferences,
            self.connections,
            self.tunnels,
            self.logging,
        ]
        .into_iter()
        .filter(|selected| *selected)
        .count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestoreResult {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
}

/// Muxus-only metadata in a backup: everything but usage counters and ids.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableHostMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_scheme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_font_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_background_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword_highlights: Option<HostKeywordHighlightConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_button_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paste_line_delay_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paste_char_delay_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_sftp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_compatibility: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_sequence: Option<LoginSequence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
}

impl PortableHostMetadata {
    /// `portableMetadata`.
    pub fn from_metadata(metadata: &OpenSshProfileMetadata, display_name: Option<String>) -> Self {
        Self {
            display_name,
            group: metadata.group.clone(),
            color: metadata.color.clone(),
            icon: metadata.icon.clone(),
            terminal_scheme: metadata.terminal_scheme.clone(),
            terminal_font_color: metadata.terminal_font_color.clone(),
            terminal_background_color: metadata.terminal_background_color.clone(),
            keyword_highlights: metadata.keyword_highlights.clone(),
            command_button_group: metadata.command_button_group.clone(),
            paste_line_delay_ms: metadata.paste_line_delay_ms,
            paste_char_delay_ms: metadata.paste_char_delay_ms,
            disable_sftp: metadata.disable_sftp,
            console_compatibility: metadata.console_compatibility,
            login_sequence: metadata.login_sequence.clone(),
            sort_order: metadata.sort_order,
        }
    }
}

/// An OpenSSH host block in a backup. `options` holds the block's own
/// options with every inherited `Host *` / `Include` value materialized, so
/// the connection behaves the same on a machine with a different config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortableSshHost {
    pub alias: String,
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub options: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<PortableHostMetadata>,
}

impl PortableSshHost {
    /// A host block from typed options (what the session importers produce).
    pub fn new(
        alias: impl Into<String>,
        aliases: Vec<String>,
        description: Option<String>,
        options: &HostBlockOptions,
        metadata: Option<PortableHostMetadata>,
    ) -> Self {
        Self {
            alias: alias.into(),
            aliases,
            description,
            options: to_object(options),
            metadata,
        }
    }
}

/// A host saved in Muxus app data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortableSavedHost {
    pub id: String,
    pub name: String,
    pub profile: SavedHostSessionProfile,
    pub metadata: PortableHostMetadata,
}

/// Connections as a backup or a third-party import carries them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableConnections {
    pub ssh_hosts: Vec<PortableSshHost>,
    pub saved_hosts: Vec<PortableSavedHost>,
    pub host_order: Vec<ManagedHostRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupLoggingPolicy {
    pub profile_key: String,
    pub policy: SessionLoggingPolicyInput,
}

/// A folder's shared SSH defaults. The vault password never leaves the vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableFolderSettings {
    pub path: String,
    pub auth: FolderAuthSettings,
    /// Steps only: a vault step names its secret, whose value stays in the vault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_sequence: Option<LoginSequence>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupDocumentOut<'a> {
    format: &'static str,
    version: u32,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_version: Option<&'a str>,
    data: BackupDataOut,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupDataOut {
    #[serde(flatten)]
    connections: PortableConnections,
    preferences: Map<String, Value>,
    tunnels: Vec<TunnelRecord>,
    logging_policies: Vec<BackupLoggingPolicy>,
    /// Without the storage location: that is a path on this machine.
    history_settings: Map<String, Value>,
    /// Without the log folder, for the same reason.
    log_file_settings: Map<String, Value>,
    folder_settings: Vec<PortableFolderSettings>,
}

/// Read the lightweight counts shown in Settings without downloading the
/// per-profile logging policies.
pub async fn fetch_data_summary(backend: &Backend) -> Result<DataSummary, TransferError> {
    let ((config, saved), tunnels) = try_zip(
        try_zip(backend.ssh_config(), backend.saved_hosts()),
        backend.tunnels(),
    )
    .await?;
    Ok(DataSummary {
        connections: config.hosts.len() + saved.profiles.len(),
        tunnels: tunnels.tunnels.len(),
    })
}

/// Everything a backup holds, read from the backend now.
pub async fn create_backup_document(
    backend: &Backend,
    app_version: Option<&str>,
) -> Result<TransferDocument, TransferError> {
    let ((config, saved), tunnels) = try_zip(
        try_zip(backend.ssh_config(), backend.saved_hosts()),
        backend.tunnels(),
    )
    .await?;
    let profile_keys = ["*".to_string(), "local".to_string()]
        .into_iter()
        .chain(
            config
                .hosts
                .iter()
                .map(|host| format!("ssh:{}", host.alias)),
        )
        .chain(
            saved
                .profiles
                .iter()
                .map(|profile| format!("profile:{}", profile.id)),
        );
    let mut logging_policies = Vec::new();
    for profile_key in profile_keys {
        let policy = backend.session_logging_policy(&profile_key).await?;
        if policy.overridden {
            logging_policies.push(BackupLoggingPolicy {
                profile_key: policy.profile_key,
                policy: SessionLoggingPolicyInput {
                    enabled: policy.enabled,
                    capture_input: policy.capture_input,
                    max_part_bytes: policy.max_part_bytes,
                    max_parts: policy.max_parts,
                    log_to_file: Some(policy.log_to_file),
                },
            });
        }
    }
    let ((storage, log_files), folders) = try_zip(
        try_zip(
            backend.session_history_storage(),
            backend.session_log_files(),
        ),
        backend.folder_settings(),
    )
    .await?;
    let prefs = backend.prefs().await?;

    let document = BackupDocumentOut {
        format: BACKUP_FORMAT,
        version: TRANSFER_VERSION,
        created_at: js::now_iso(),
        app_version,
        data: BackupDataOut {
            connections: portable_connections(&config.hosts, &saved.profiles),
            preferences: backup_preferences(&prefs.to_json_map()),
            tunnels: tunnels.tunnels,
            logging_policies,
            history_settings: without_key(to_object(&storage.settings), "storageLocation"),
            log_file_settings: without_key(to_object(&log_files.settings), "directory"),
            folder_settings: folders
                .folders
                .into_iter()
                .filter(|folder| !folder.auth.is_empty() || folder.login_sequence.is_some())
                .map(|folder| PortableFolderSettings {
                    path: folder.path,
                    auth: folder.auth,
                    login_sequence: folder.login_sequence,
                })
                .collect(),
        },
    };
    let value =
        serde_json::to_value(&document).map_err(|error| TransferError::new(error.to_string()))?;
    Ok(TransferDocument::from_validated(value))
}

/// The OpenSSH hosts and the saved SSH hosts as one ssh_config text for
/// other SSH clients. Muxus-specific settings stay in the backup.
pub async fn create_openssh_export(backend: &Backend) -> Result<String, TransferError> {
    let ((config, saved), folders) = try_zip(
        try_zip(backend.ssh_config(), backend.saved_hosts()),
        backend.folder_settings(),
    )
    .await?;
    let mut blocks = Vec::new();
    for request in openssh_export_requests(&config.hosts, &saved.profiles, &folders.folders) {
        blocks.push(backend.preview_ssh_host(request).await?.text);
    }
    Ok(openssh_export_text(&blocks, &js::now_iso()))
}

/// The Host blocks an export previews, OpenSSH hosts first.
pub fn openssh_export_requests(
    hosts: &[SshHostEntry],
    saved: &[SavedHostProfile],
    folders: &[FolderSettingsRecord],
) -> Vec<HostUpsertRequest> {
    let mut used_aliases: HashSet<String> = hosts
        .iter()
        .flat_map(|host| host.aliases.iter().cloned())
        .collect();
    // Allocate every alias first: a saved host can jump through one exported
    // after it, and its ProxyJump must name that host's block.
    let mut exported_aliases: HashMap<String, String> = HashMap::new();
    for profile in saved {
        let SavedHostSessionProfile::Ssh(ssh) = &profile.profile else {
            continue;
        };
        let base = openssh_export_alias(&profile.name, &ssh.target);
        let mut alias = base.clone();
        let mut suffix = 2;
        while used_aliases.contains(&alias) {
            alias = format!("{base}-{suffix}");
            suffix += 1;
        }
        used_aliases.insert(alias.clone());
        exported_aliases.insert(profile.id.clone(), alias);
    }
    let lookup = |id: &str| exported_aliases.get(id).cloned();
    let native = saved.iter().filter_map(|profile| {
        let alias = exported_aliases.get(&profile.id)?;
        let (inherited, has_password) =
            saved_profile_folder_defaults(profile.metadata.group.as_deref(), folders);
        let mut options = portable_saved_ssh_options(profile, &inherited);
        let proxy_jump = open_ssh_jump_hops(
            options.proxy_jump.as_deref().unwrap_or_default(),
            saved,
            Some(&lookup),
            &HashSet::new(),
        );
        options.proxy_jump = (!proxy_jump.is_empty()).then_some(proxy_jump);
        Some(HostUpsertRequest {
            aliases: vec![alias.clone()],
            description: Some(
                if has_password {
                    "Exported from Muxus app data. Shared folder password omitted."
                } else {
                    "Exported from Muxus app data."
                }
                .to_string(),
            ),
            file: None,
            options,
            previous_alias: None,
        })
    });
    hosts
        .iter()
        .map(|host| HostUpsertRequest {
            aliases: host.aliases.clone(),
            description: host.description.clone(),
            file: None,
            options: portable_ssh_options(host),
            previous_alias: None,
        })
        .chain(native)
        .collect()
}

/// The export file around the previewed blocks.
pub fn openssh_export_text(blocks: &[String], generated_at: &str) -> String {
    let mut parts = vec![
        "# OpenSSH connection export from Muxus".to_string(),
        format!("# Generated {generated_at}"),
        "# Private key files are referenced by path and are not embedded.".to_string(),
        String::new(),
    ];
    parts.extend(blocks.iter().map(|block| js::js_trim(block).to_string()));
    parts.push(String::new());
    parts.join("\n\n")
}

fn portable_saved_ssh_options(
    profile: &SavedHostProfile,
    inherited: &FolderAuthSettings,
) -> HostBlockOptions {
    let SavedHostSessionProfile::Ssh(saved) = &profile.profile else {
        return HostBlockOptions::default();
    };
    HostBlockOptions {
        hostname: Some(saved.target.clone()),
        user: saved.user.clone().or_else(|| inherited.user.clone()),
        port: saved.port.or(inherited.port),
        identity_files: saved
            .identity_files
            .clone()
            .or_else(|| inherited.identity_files.clone()),
        certificate_files: saved.certificate_files.clone(),
        identities_only: saved.identities_only.or(inherited.identities_only),
        identity_agent: saved
            .identity_agent
            .clone()
            .or_else(|| inherited.identity_agent.clone()),
        forward_agent: saved.forward_agent.or(inherited.forward_agent),
        forward_x11: saved.forward_x11,
        proxy_jump: saved.proxy_jump.clone(),
        proxy_command: saved.proxy_command.clone(),
        forwards: saved.forwards.clone(),
        password_only: saved.password_only,
        remote_command: saved.remote_command.clone(),
        request_tty: saved.request_tty,
        strict_host_key_checking: saved.strict_host_key_checking,
        extras: None,
    }
}

/// Materialize the non-secret defaults a saved profile inherits in Muxus,
/// nearest folder first, and whether any of those folders has a password.
fn saved_profile_folder_defaults(
    group: Option<&str>,
    folders: &[FolderSettingsRecord],
) -> (FolderAuthSettings, bool) {
    let normalized = group.unwrap_or_default().replace('\\', "/");
    let parts: Vec<&str> = normalized
        .split('/')
        .map(js::js_trim)
        .filter(|part| !part.is_empty())
        .collect();
    let mut auth = FolderAuthSettings::default();
    let mut has_password = false;
    if parts.is_empty() {
        return (auth, has_password);
    }
    let by_path: HashMap<String, &FolderSettingsRecord> = folders
        .iter()
        .map(|folder| (folder.path.to_lowercase(), folder))
        .collect();
    for length in (1..=parts.len()).rev() {
        let Some(folder) = by_path.get(&parts[..length].join("/").to_lowercase()) else {
            continue;
        };
        has_password |= folder.has_password;
        let from = &folder.auth;
        auth.user = auth.user.or_else(|| from.user.clone());
        auth.port = auth.port.or(from.port);
        auth.identity_files = auth.identity_files.or_else(|| from.identity_files.clone());
        auth.identities_only = auth.identities_only.or(from.identities_only);
        auth.identity_agent = auth.identity_agent.or_else(|| from.identity_agent.clone());
        auth.forward_agent = auth.forward_agent.or(from.forward_agent);
    }
    (auth, has_password)
}

/// A Host alias from a saved host's name (or its target): runs of blanks and
/// pattern characters become `-`.
fn openssh_export_alias(name: &str, target: &str) -> String {
    fn clean(text: &str) -> String {
        let mut out = String::new();
        let mut in_run = false;
        for c in js::js_trim(text).chars() {
            if js::is_js_whitespace(c) || matches!(c, '#' | '*' | '?' | '!') {
                if !in_run {
                    out.push('-');
                }
                in_run = true;
            } else {
                out.push(c);
                in_run = false;
            }
        }
        js::utf16_prefix(out.trim_matches('-'), 240).to_string()
    }
    let from_name = clean(name);
    if !from_name.is_empty() {
        return from_name;
    }
    let from_target = clean(target);
    if !from_target.is_empty() {
        return from_target;
    }
    "muxus-host".to_string()
}

fn portable_connections(
    ssh_hosts: &[SshHostEntry],
    saved_hosts: &[SavedHostProfile],
) -> PortableConnections {
    let ssh: Vec<PortableSshHost> = ssh_hosts
        .iter()
        .map(|host| PortableSshHost {
            alias: host.alias.clone(),
            aliases: host.aliases.clone(),
            description: host.description.clone(),
            // Materialize inherited Host * / Include values so the exported
            // connection behaves the same on a machine with a different config.
            options: portable_ssh_option_map(host),
            metadata: host.metadata.as_ref().map(|metadata| {
                PortableHostMetadata::from_metadata(metadata, metadata.display_name.clone())
            }),
        })
        .collect();
    let saved: Vec<PortableSavedHost> = saved_hosts
        .iter()
        .map(|profile| PortableSavedHost {
            id: profile.id.clone(),
            name: profile.name.clone(),
            profile: profile.profile.clone(),
            metadata: PortableHostMetadata::from_metadata(&profile.metadata, None),
        })
        .collect();
    let mut ordered: Vec<(ManagedHostRef, String, Option<i64>)> = ssh
        .iter()
        .map(|host| {
            let metadata = host.metadata.as_ref();
            (
                ManagedHostRef::Ssh {
                    alias: host.alias.clone(),
                },
                metadata
                    .and_then(|metadata| metadata.display_name.clone())
                    .unwrap_or_else(|| host.alias.clone()),
                metadata.and_then(|metadata| metadata.sort_order),
            )
        })
        .chain(saved.iter().map(|host| {
            (
                ManagedHostRef::Profile {
                    id: host.id.clone(),
                },
                host.name.clone(),
                host.metadata.sort_order,
            )
        }))
        .collect();
    let unordered = js::number::MAX_SAFE_INTEGER as i64;
    ordered.sort_by(|a, b| {
        a.2.unwrap_or(unordered)
            .cmp(&b.2.unwrap_or(unordered))
            .then_with(|| locale_compare(&a.1, &b.1))
    });
    PortableConnections {
        ssh_hosts: ssh,
        saved_hosts: saved,
        host_order: ordered
            .into_iter()
            .map(|(reference, _, _)| reference)
            .collect(),
    }
}

/// `portableSshOptions` as typed options (the export previews these).
fn portable_ssh_options(host: &SshHostEntry) -> HostBlockOptions {
    serde_json::from_value(Value::Object(portable_ssh_option_map(host))).unwrap_or_default()
}

/// `{ ...host.options, hostname: resolved.hostname, … }`: the block's own
/// options in their order, then the resolved values over them. A resolved
/// value that is absent removes the option, as `undefined` does in the file.
fn portable_ssh_option_map(host: &SshHostEntry) -> Map<String, Value> {
    let resolved = &host.resolved;
    let list = |values: &Vec<String>| (!values.is_empty()).then(|| Value::from(values.clone()));
    let mut options = JsObject::from_map(to_object(&host.options));
    options.assign("hostname", Some(Value::from(resolved.hostname.clone())));
    options.assign("user", resolved.user.clone().map(Value::from));
    options.assign("port", Some(Value::from(resolved.port)));
    options.assign("identityFiles", list(&resolved.identity_files));
    options.assign("certificateFiles", list(&resolved.certificate_files));
    options.assign(
        "identitiesOnly",
        Some(Value::from(resolved.identities_only)),
    );
    options.assign("forwardAgent", Some(Value::from(resolved.forward_agent)));
    options.assign("forwardX11", resolved.forward_x11.map(Value::from));
    options.assign("proxyJump", list(&resolved.proxy_jump));
    options.assign(
        "proxyCommand",
        resolved.proxy_command.clone().map(Value::from),
    );
    options.assign(
        "forwards",
        (!resolved.forwards.is_empty())
            .then(|| serde_json::to_value(&resolved.forwards).unwrap_or_default()),
    );
    options.assign("passwordOnly", Some(Value::from(resolved.password_only)));
    options.into_map()
}

fn backup_preferences(prefs: &Map<String, Value>) -> Map<String, Value> {
    PREFERENCE_KEYS
        .iter()
        .filter_map(|key| {
            prefs
                .get(*key)
                .map(|value| (key.to_string(), value.clone()))
        })
        .collect()
}

/// Apply the chosen parts of a backup. Connections and tunnels follow the
/// conflict strategy; preferences and logging settings are replaced when
/// selected. Nothing missing from the file is deleted.
///
/// Restored preferences are written through the backend: whatever holds the
/// preferences in memory must read them again afterwards.
pub async fn restore_transfer_document(
    backend: &Backend,
    document: &TransferDocument,
    selection: RestoreSelection,
    conflicts: TransferConflictStrategy,
) -> Result<RestoreResult, TransferError> {
    let mut result = RestoreResult::default();
    let data = document.data();
    if selection.connections {
        restore_connections(backend, data, conflicts, &mut result).await?;
        restore_folder_settings(backend, document.folder_settings(), conflicts, &mut result)
            .await?;
    }

    if selection.tunnels {
        let current = backend.tunnels().await?;
        let current_ids: HashSet<&str> = current
            .tunnels
            .iter()
            .map(|tunnel| tunnel.id.as_str())
            .collect();
        for tunnel in document.tunnels() {
            let exists = tunnel
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| current_ids.contains(id));
            if exists && conflicts == TransferConflictStrategy::Keep {
                result.skipped += 1;
                continue;
            }
            if exists {
                result.updated += 1;
            } else {
                result.added += 1;
            }
            backend.save_tunnel(portable_tunnel_input(tunnel)?).await?;
        }
    }

    if selection.logging {
        let (storage, log_files) = try_zip(
            backend.session_history_storage(),
            backend.session_log_files(),
        )
        .await?;
        let policies = document.logging_policies();
        for entry in policies {
            let profile_key = entry
                .get("profileKey")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let policy: SessionLoggingPolicyInput =
                deserialize(entry.get("policy").unwrap_or(&Value::Null))?;
            backend
                .put_session_logging_policy(profile_key, policy)
                .await?;
        }
        let mut history = JsObject::from_map(
            data.get("historySettings")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        );
        history.assign(
            "storageLocation",
            storage.settings.storage_location.clone().map(Value::from),
        );
        backend
            .put_session_history_storage(deserialize(&Value::Object(history.into_map()))?)
            .await?;
        let log_file_settings = data.get("logFileSettings").and_then(Value::as_object);
        if let Some(settings) = log_file_settings {
            backend
                .put_session_log_files(log_file_settings_input(
                    settings,
                    log_files.settings.directory.as_deref(),
                )?)
                .await?;
        }
        result.updated += policies.len() + if log_file_settings.is_some() { 2 } else { 1 };
    }

    if selection.preferences {
        let patch = sanitize_preferences(document.preferences());
        let prefs = backend.prefs().await?;
        backend
            .set_prefs(apply_preferences_patch(&prefs, &patch))
            .await?;
        result.updated += 1;
    }

    Ok(result)
}

/// Restore a reviewed third-party connection set through the same
/// conflict-safe path as backups.
pub async fn restore_imported_connections(
    backend: &Backend,
    connections: &PortableConnections,
    conflicts: TransferConflictStrategy,
) -> Result<RestoreResult, TransferError> {
    let data = to_object(connections);
    let mut result = RestoreResult::default();
    restore_connections(backend, &data, conflicts, &mut result).await?;
    Ok(result)
}

async fn restore_connections(
    backend: &Backend,
    data: &Map<String, Value>,
    conflicts: TransferConflictStrategy,
    result: &mut RestoreResult,
) -> Result<(), TransferError> {
    let (config, saved) = try_zip(backend.ssh_config(), backend.saved_hosts()).await?;
    let mut ssh_by_alias: HashMap<&str, &SshHostEntry> = HashMap::new();
    for host in &config.hosts {
        for alias in &host.aliases {
            ssh_by_alias.insert(alias, host);
        }
    }
    let saved_ids: HashSet<&str> = saved
        .profiles
        .iter()
        .map(|profile| profile.id.as_str())
        .collect();
    let list = |key: &str| {
        data.get(key)
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
    };

    // OpenSSH config edits intentionally stay sequential: every request
    // atomically rewrites a config file, so parallel edits could race.
    for host in list("sshHosts") {
        let alias = host
            .get("alias")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let existing = ssh_by_alias.get(alias).copied();
        if existing.is_some() && conflicts == TransferConflictStrategy::Keep {
            result.skipped += 1;
            continue;
        }
        backend
            .upsert_ssh_host(ssh_upsert_request(host, existing)?)
            .await?;
        if let Some(metadata) = host.get("metadata").filter(|metadata| metadata.is_object()) {
            backend
                .patch_ssh_host_metadata(alias, metadata_patch(metadata)?)
                .await?;
        }
        if existing.is_some() {
            result.updated += 1;
        } else {
            result.added += 1;
        }
    }

    for profile in list("savedHosts") {
        let id = profile
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let exists = saved_ids.contains(id);
        if exists && conflicts == TransferConflictStrategy::Keep {
            result.skipped += 1;
            continue;
        }
        backend.save_saved_host(saved_host_input(profile)?).await?;
        backend
            .patch_saved_host_metadata(
                id,
                metadata_patch(profile.get("metadata").unwrap_or(&Value::Null))?,
            )
            .await?;
        if exists {
            result.updated += 1;
        } else {
            result.added += 1;
        }
    }

    let host_order = list("hostOrder");
    if conflicts == TransferConflictStrategy::Replace && !host_order.is_empty() {
        let hosts: Vec<ManagedHostRef> = deserialize(&Value::from(host_order.to_vec()))?;
        backend.put_host_order(HostOrderRequest { hosts }).await?;
    }
    Ok(())
}

async fn restore_folder_settings(
    backend: &Backend,
    entries: &[Value],
    conflicts: TransferConflictStrategy,
    result: &mut RestoreResult,
) -> Result<(), TransferError> {
    if entries.is_empty() {
        return Ok(());
    }
    let current = backend.folder_settings().await?;
    let existing_paths: HashSet<String> = current
        .folders
        .iter()
        .map(|folder| folder.path.to_lowercase())
        .collect();
    for entry in entries {
        let path = entry
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let exists = existing_paths.contains(&path.to_lowercase());
        if exists && conflicts == TransferConflictStrategy::Keep {
            result.skipped += 1;
            continue;
        }
        backend
            .put_folder_settings(folder_settings_upsert(entry)?)
            .await?;
        if exists {
            result.updated += 1;
        } else {
            result.added += 1;
        }
    }
    Ok(())
}

/// The folder write of a restore. A sequence that does not check out is
/// dropped, not the whole folder.
fn folder_settings_upsert(entry: &Value) -> Result<FolderSettingsUpsert, TransferError> {
    Ok(FolderSettingsUpsert {
        path: entry
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        auth: deserialize(entry.get("auth").unwrap_or(&Value::Null))?,
        login_sequence: match entry.get("loginSequence") {
            Some(sequence) if is_login_sequence(sequence) => {
                serde_json::from_value(sequence.clone()).map_or(Patch::Keep, Patch::Set)
            }
            _ => Patch::Keep,
        },
    })
}

/// The ssh_config write of a restored host: a host that already owns the
/// alias (as its primary or a secondary alias) is replaced in its own file.
fn ssh_upsert_request(
    host: &Value,
    existing: Option<&SshHostEntry>,
) -> Result<HostUpsertRequest, TransferError> {
    Ok(HostUpsertRequest {
        aliases: deserialize(host.get("aliases").unwrap_or(&Value::Null))?,
        description: match host.get("description") {
            None => None,
            Some(description) => deserialize(description)?,
        },
        file: existing.map(|host| host.file.clone()),
        options: deserialize(host.get("options").unwrap_or(&Value::Null))?,
        previous_alias: existing.map(|host| host.alias.clone()),
    })
}

/// The profile write of a restored saved host, under the id it was saved with.
fn saved_host_input(profile: &Value) -> Result<SavedHostProfileInput, TransferError> {
    Ok(SavedHostProfileInput {
        id: profile
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string),
        name: deserialize(profile.get("name").unwrap_or(&Value::Null))?,
        profile: deserialize(profile.get("profile").unwrap_or(&Value::Null))?,
    })
}

/// The metadata write of a restored host: every field the backup does not
/// set is cleared, so the host ends up exactly as backed up.
fn metadata_patch(metadata: &Value) -> Result<OpenSshMetadataPatch, TransferError> {
    fn nullable<T: DeserializeOwned>(
        metadata: &Value,
        key: &str,
    ) -> Result<Patch<T>, TransferError> {
        match metadata.get(key) {
            None | Some(Value::Null) => Ok(Patch::Clear),
            Some(value) => deserialize(value).map(Patch::Set),
        }
    }
    fn flag(metadata: &Value, key: &str) -> Result<Option<bool>, TransferError> {
        match metadata.get(key) {
            None | Some(Value::Null) => Ok(Some(false)),
            Some(value) => deserialize(value).map(Some),
        }
    }
    Ok(OpenSshMetadataPatch {
        display_name: nullable(metadata, "displayName")?,
        group: nullable(metadata, "group")?,
        color: nullable(metadata, "color")?,
        icon: nullable(metadata, "icon")?,
        terminal_scheme: nullable(metadata, "terminalScheme")?,
        terminal_font_color: nullable(metadata, "terminalFontColor")?,
        terminal_background_color: nullable(metadata, "terminalBackgroundColor")?,
        keyword_highlights: nullable(metadata, "keywordHighlights")?,
        command_button_group: nullable(metadata, "commandButtonGroup")?,
        paste_line_delay_ms: nullable(metadata, "pasteLineDelayMs")?,
        paste_char_delay_ms: nullable(metadata, "pasteCharDelayMs")?,
        disable_sftp: flag(metadata, "disableSftp")?,
        console_compatibility: flag(metadata, "consoleCompatibility")?,
        login_sequence: match metadata.get("loginSequence") {
            Some(sequence) if is_login_sequence(sequence) => {
                serde_json::from_value(sequence.clone()).map_or(Patch::Clear, Patch::Set)
            }
            _ => Patch::Clear,
        },
    })
}

/// The restored log file settings, written into this machine's log folder.
fn log_file_settings_input(
    settings: &Map<String, Value>,
    directory: Option<&str>,
) -> Result<muxus_core::api::SessionLogFileSettings, TransferError> {
    let mut settings = JsObject::from_map(settings.clone());
    settings.assign("directory", directory.map(Value::from));
    deserialize(&Value::Object(settings.into_map()))
}

/// A saved tunnel without its timestamps.
fn portable_tunnel_input(tunnel: &Value) -> Result<TunnelInput, TransferError> {
    let input = without_key(
        without_key(tunnel.as_object().cloned().unwrap_or_default(), "createdAt"),
        "updatedAt",
    );
    deserialize(&Value::Object(input))
}

fn deserialize<T: DeserializeOwned>(value: &Value) -> Result<T, TransferError> {
    serde_json::from_value(value.clone()).map_err(TransferError::invalid)
}

fn to_object<T: Serialize>(value: &T) -> Map<String, Value> {
    match serde_json::to_value(value) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// The map without `key`, the other keys in their order.
fn without_key(map: Map<String, Value>, key: &str) -> Map<String, Value> {
    map.into_iter().filter(|(name, _)| name != key).collect()
}

/// A JavaScript object being assembled: keys keep the position they were
/// first given, and `undefined` values stay in place until the object is
/// written, where they are left out (as `JSON.stringify` does).
struct JsObject(Vec<(String, Option<Value>)>);

impl JsObject {
    fn from_map(map: Map<String, Value>) -> Self {
        Self(
            map.into_iter()
                .map(|(key, value)| (key, Some(value)))
                .collect(),
        )
    }

    fn assign(&mut self, key: &str, value: Option<Value>) {
        match self.0.iter_mut().find(|(name, _)| name == key) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key.to_string(), value)),
        }
    }

    fn into_map(self) -> Map<String, Value> {
        self.0
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect()
    }
}
