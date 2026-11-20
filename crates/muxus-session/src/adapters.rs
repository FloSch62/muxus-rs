//! The store and the SSH layer, behind the traits the lower crates asked for.
//!
//! The lower layers never reach into each other: `muxus-ssh` asks for a [`muxus_ssh::PasswordVault`],
//! a [`FolderAuthSource`] and the [`LinkedPasswords`] of external password managers, the
//! recorder for a [`HistorySink`], login sequences for a [`SecretVault`] and a
//! [`LoginSequenceSource`], host statistics for a [`RemoteCommand`], connection diagnostics
//! for [`DiagnosticHop`]s, and `muxus-passwords` for the [`SourceStore`] its managers are
//! kept in. This module implements them over `muxus-store`, `muxus-ssh` and
//! `muxus-passwords`, the way `server/src/app.ts` wired the TypeScript server.

use std::sync::Arc;

use async_trait::async_trait;
use muxus_core::api::{
    ExternalPasswordRef, LoginSequence, PasswordManagerConfig, PasswordManagerKind,
    SessionEndStatus, SessionLogCreateInput,
};
use muxus_core::protocol::{AuthPromptInfo, AuthPromptResponse};
use muxus_passwords::{
    PasswordManagerIo, PasswordManagers, PromptCancelled, SourceStore, StoredSource,
};
use muxus_rdp::passwords::DesktopPrompts;
use muxus_ssh::connection::chain::expanded_proxy_command;
use muxus_ssh::connection::session::{SessionRequest, open_session};
use muxus_ssh::connection::{
    BoxFuture, Cancelled, ChainHop, ConnectIo, LinkedPasswords, ManagedConnection,
};
use muxus_ssh::folder_auth::{FolderAuthSource, FolderSettingsRow as SshFolderRow};
use muxus_store::Database;
use muxus_store::db::{HostRef, PasswordManagerSourceRow};
use muxus_store::history::{PartPolicy, SessionHistoryStore, SessionStatePatch};
use muxus_store::vault::{PasswordVault, VaultError as StoreVaultError};
use russh::ChannelMsg;
use tokio::io::{AsyncRead, AsyncWriteExt};

use crate::diagnostics::DiagnosticHop;
use crate::logging::{FailureSubscription, HistoryEvent, HistorySink, PartLimits};
use crate::login_sequence::{
    HostLoginSequence, LoginSequenceHost, LoginSequenceSource, SecretVault, VaultError,
};
use crate::stats::RemoteCommand;

// --- folders ---------------------------------------------------------------------------------

/// Folder settings for the SSH dial plan (`folderAuthResolver(database)`).
#[derive(Clone)]
pub struct StoreFolderAuth(pub Arc<Database>);

impl FolderAuthSource for StoreFolderAuth {
    fn group_for_alias(&self, alias: &str) -> Option<String> {
        self.0.group_for_alias(alias).ok().flatten()
    }

    fn folder_settings_for_path(&self, path: &str) -> Option<SshFolderRow> {
        let row = self.0.folder_settings_for_path(path).ok().flatten()?;
        Some(SshFolderRow {
            id: row.id,
            path: row.path,
            auth: row.auth,
        })
    }

    fn group_for_saved_host(&self, id: &str) -> Option<String> {
        self.0.group_for_saved_host(id).ok().flatten()
    }
}

// --- external password managers --------------------------------------------------------------

/// The connected password managers, kept in the database (`new PasswordManagers(database)`).
pub struct StorePasswordSources(pub Arc<Database>);

fn stored_source(row: PasswordManagerSourceRow) -> StoredSource {
    StoredSource {
        id: row.id,
        kind: row.kind,
        name: row.name,
        config: row.config,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

impl SourceStore for StorePasswordSources {
    fn list(&self) -> Result<Vec<StoredSource>, String> {
        self.0
            .list_password_manager_sources()
            .map(|rows| rows.into_iter().map(stored_source).collect())
            .map_err(|error| error.to_string())
    }

    fn get(&self, id: &str) -> Result<Option<StoredSource>, String> {
        self.0
            .password_manager_source(id)
            .map(|row| row.map(stored_source))
            .map_err(|error| error.to_string())
    }

    fn create(
        &self,
        kind: PasswordManagerKind,
        name: &str,
        config: &PasswordManagerConfig,
    ) -> Result<StoredSource, String> {
        self.0
            .create_password_manager_source(kind.as_str(), name, config)
            .map(stored_source)
            .map_err(|error| error.to_string())
    }

    fn update(
        &self,
        id: &str,
        name: &str,
        config: &PasswordManagerConfig,
    ) -> Result<Option<StoredSource>, String> {
        self.0
            .update_password_manager_source(id, name, config)
            .map(|row| row.map(stored_source))
            .map_err(|error| error.to_string())
    }

    fn delete(&self, id: &str) -> Result<bool, String> {
        self.0
            .delete_password_manager_source(id)
            .map_err(|error| error.to_string())
    }
}

/// The connected password managers for SSH logins: a linked entry is read, and its
/// manager unlocked, through the connection's own status lines and prompts.
pub struct LinkedEntries(pub Arc<PasswordManagers>);

struct ConnectPrompts(Arc<dyn ConnectIo>);

#[async_trait]
impl PasswordManagerIo for ConnectPrompts {
    fn status(&self, message: &str, transient: bool) {
        self.0.status(message, transient);
    }

    async fn prompt(&self, info: AuthPromptInfo) -> Result<AuthPromptResponse, PromptCancelled> {
        self.0.prompt(info).await.map_err(|_| PromptCancelled)
    }
}

impl LinkedPasswords for LinkedEntries {
    fn password_for<'a>(
        &'a self,
        entry: &'a ExternalPasswordRef,
        io: Arc<dyn ConnectIo>,
    ) -> BoxFuture<'a, Result<Option<String>, Cancelled>> {
        Box::pin(async move {
            self.0
                .password_for(entry, &ConnectPrompts(io))
                .await
                .map_err(|_| Cancelled)
        })
    }
}

/// An RDP, VNC or gNMI login's prompts, as the password managers ask them. A prompt that
/// was withdrawn reads as skipped there; the closing session ends the login anyway.
struct LogonPrompts<'a>(&'a dyn DesktopPrompts);

#[async_trait]
impl PasswordManagerIo for LogonPrompts<'_> {
    fn status(&self, message: &str, transient: bool) {
        self.0.status(message, transient);
    }

    async fn prompt(&self, info: AuthPromptInfo) -> Result<AuthPromptResponse, PromptCancelled> {
        Ok(self.0.prompt(info).await)
    }
}

/// The password of the password-manager entry a saved host is linked to, for an RDP, VNC
/// or gNMI login in progress. `None` when the host has no entry or it could not be read,
/// which `prompts` was then told.
pub(crate) async fn linked_host_password(
    managers: &PasswordManagers,
    database: &Database,
    profile_id: Option<&str>,
    prompts: &dyn DesktopPrompts,
) -> Option<String> {
    let entry = database
        .password_ref_for_saved_host(profile_id?)
        .ok()
        .flatten()?;
    managers
        .password_for(&entry, &LogonPrompts(prompts))
        .await
        .ok()
        .flatten()
}

// --- the password vault ----------------------------------------------------------------------

/// The store's password vault for SSH logins and login sequences. Vault work (scrypt, the OS
/// credential store) runs on the blocking pool.
#[derive(Clone)]
pub struct StoreVault(pub Arc<PasswordVault>);

impl StoreVault {
    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&PasswordVault) -> T + Send + 'static,
    ) -> Result<T, String> {
        let vault = self.0.clone();
        tokio::task::spawn_blocking(move || work(&vault))
            .await
            .map_err(|error| error.to_string())
    }
}

fn ssh_vault_error(error: StoreVaultError) -> muxus_ssh::connection::VaultError {
    use muxus_ssh::connection::VaultError as Ssh;
    match error {
        StoreVaultError::InvalidMasterPassword => Ssh::InvalidMasterPassword(error.to_string()),
        StoreVaultError::InvalidMasterPasswordFormat(message) => {
            Ssh::InvalidMasterPasswordFormat(message)
        }
        StoreVaultError::CredentialCorrupt => Ssh::CredentialVaultCorrupt,
        StoreVaultError::KeyStoreUnavailable(_) => Ssh::KeyStoreUnavailable,
        StoreVaultError::AlreadyConfigured => Ssh::AlreadyConfigured,
        other => Ssh::Other(other.to_string()),
    }
}

impl muxus_ssh::connection::PasswordVault for StoreVault {
    fn status(&self) -> muxus_ssh::connection::VaultStatus {
        match self.0.status() {
            Ok(status) => muxus_ssh::connection::VaultStatus {
                configured: status.configured,
                locked: status.locked,
            },
            Err(_) => muxus_ssh::connection::VaultStatus::default(),
        }
    }

    fn has_ssh_password(&self, account: &str) -> bool {
        self.0.has_ssh_password(account).unwrap_or(false)
    }

    fn ssh_password<'a>(
        &'a self,
        account: &'a str,
        master_password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, muxus_ssh::connection::VaultError>> {
        let account = account.to_string();
        let master = master_password.map(str::to_string);
        Box::pin(async move {
            let result = self
                .blocking(move |vault| vault.ssh_password(&account, master.as_deref()))
                .await
                .map_err(muxus_ssh::connection::VaultError::Other)?;
            match result {
                Ok(Some(password)) => Ok(password.to_string()),
                Ok(None) => Err(muxus_ssh::connection::VaultError::Other(
                    "The saved password is no longer in the password vault.".into(),
                )),
                Err(error) => Err(ssh_vault_error(error)),
            }
        })
    }

    fn remember_ssh_password<'a>(
        &'a self,
        account: &'a str,
        label: &'a str,
        password: &'a str,
        master_password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), muxus_ssh::connection::VaultError>> {
        let (account, label, password) =
            (account.to_string(), label.to_string(), password.to_string());
        let master = master_password.map(str::to_string);
        Box::pin(async move {
            self.blocking(move |vault| {
                vault.remember_ssh_password(&account, &label, &password, master.as_deref())
            })
            .await
            .map_err(muxus_ssh::connection::VaultError::Other)?
            .map_err(ssh_vault_error)
        })
    }

    fn create<'a>(
        &'a self,
        master_password: &'a str,
    ) -> BoxFuture<'a, Result<(), muxus_ssh::connection::VaultError>> {
        let master = master_password.to_string();
        Box::pin(async move {
            self.blocking(move |vault| vault.create(&master, None))
                .await
                .map_err(muxus_ssh::connection::VaultError::Other)?
                .map_err(ssh_vault_error)
        })
    }
}

#[async_trait]
impl SecretVault for StoreVault {
    fn secret_name(&self, secret_id: &str) -> Option<String> {
        self.0
            .secrets()
            .ok()?
            .into_iter()
            .find(|secret| secret.id == secret_id)
            .map(|secret| secret.name)
    }

    async fn secret_value(
        &self,
        secret_id: &str,
        master_password: Option<&str>,
    ) -> Result<Option<String>, VaultError> {
        let id = secret_id.to_string();
        let master = master_password.map(str::to_string);
        let result = self
            .blocking(move |vault| vault.secret_value(&id, master.as_deref()))
            .await
            .map_err(VaultError::Other)?;
        match result {
            Ok(value) => Ok(value.map(|value| value.to_string())),
            Err(error) if error.is_unlock_required() => Err(VaultError::UnlockRequired),
            Err(
                error @ (StoreVaultError::InvalidMasterPassword
                | StoreVaultError::InvalidMasterPasswordFormat(_)),
            ) => Err(VaultError::InvalidMasterPassword(error.to_string())),
            Err(StoreVaultError::CredentialCorrupt) => Err(VaultError::Corrupt),
            Err(error) => Err(VaultError::Other(error.to_string())),
        }
    }
}

// --- session history -------------------------------------------------------------------------

/// The history store as the recorder's sink.
#[derive(Clone)]
pub struct StoreHistory(pub Arc<SessionHistoryStore>);

fn part_policy(limits: PartLimits) -> PartPolicy {
    PartPolicy {
        max_part_bytes: limits.max_part_bytes,
        max_parts: limits.max_parts,
    }
}

impl HistorySink for StoreHistory {
    fn begin_session(&self, input: SessionLogCreateInput, limits: PartLimits) -> String {
        self.0.begin_session(&input, part_policy(limits))
    }

    fn append(&self, session_id: &str, events: Vec<HistoryEvent>, limits: PartLimits) -> bool {
        let events = events
            .into_iter()
            .map(|event| muxus_store::history::HistoryEvent {
                sequence: event.sequence,
                recorded_at: event.recorded_at,
                elapsed_ms: event.elapsed_ms,
                direction: event.direction,
                raw: event.raw,
                text: event.text,
                line_timestamps: event.line_timestamps,
            })
            .collect();
        self.0.append(session_id, events, part_policy(limits))
    }

    fn set_session_state(&self, session_id: &str, paused: bool, capture_input: bool) {
        self.0.set_session_state(
            session_id,
            SessionStatePatch {
                paused: Some(paused),
                capture_input: Some(capture_input),
            },
        );
    }

    fn set_session_title(&self, session_id: &str, title: &str) {
        self.0.set_session_title(session_id, title);
    }

    fn finish_session(&self, session_id: &str, status: SessionEndStatus, ended_at: &str) {
        self.0.finish_session(session_id, status, ended_at);
    }

    fn on_session_failure(
        &self,
        session_id: &str,
        listener: Box<dyn Fn(String) + Send + Sync>,
    ) -> FailureSubscription {
        let subscription = self.0.on_session_failure(session_id, listener);
        FailureSubscription::new(move || drop(subscription))
    }
}

// --- login sequences -------------------------------------------------------------------------

/// Hosts' and folders' login sequences from the database.
#[derive(Clone)]
pub struct StoreLoginSequences(pub Arc<Database>);

impl LoginSequenceSource for StoreLoginSequences {
    fn host_login_sequence(&self, host: &LoginSequenceHost) -> Option<HostLoginSequence> {
        let host = match host {
            LoginSequenceHost::Alias(alias) => HostRef::Alias(alias),
            LoginSequenceHost::Profile(id) => HostRef::ProfileId(id),
        };
        let entry = self.0.host_login_sequence(host).ok().flatten()?;
        Some(HostLoginSequence {
            login_sequence: entry.login_sequence,
            group: entry.group,
        })
    }

    fn folder_login_sequence(&self, path: &str) -> Option<LoginSequence> {
        self.0
            .folder_settings_for_path(path)
            .ok()
            .flatten()
            .and_then(|row| row.login_sequence)
    }
}

// --- host statistics -------------------------------------------------------------------------

/// Commands on a live SSH transport, for host statistics.
#[derive(Clone)]
pub struct ConnectionCommand(pub Arc<ManagedConnection>);

#[async_trait]
impl RemoteCommand for ConnectionCommand {
    async fn exec(
        &self,
        command: &str,
        stdin: Vec<u8>,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, String> {
        let request = SessionRequest {
            command: Some(command.to_string()),
            ..SessionRequest::default()
        };
        let mut session = open_session(self.0.handle(), &request)
            .await
            .map_err(|error| error.to_string())?;
        // A command that already finished (and closed its channel) never reads its input;
        // that is not a failure of the exec itself.
        let _ = session.channel.data(&stdin[..]).await;
        let _ = session.channel.eof().await;
        let (reader, mut writer) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            while let Some(message) = session.wait().await {
                match message {
                    ChannelMsg::Data { data } => {
                        // The reader was dropped: stop and close the channel.
                        if writer.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    ChannelMsg::Close | ChannelMsg::Eof => break,
                    _ => {}
                }
            }
            let _ = session.channel.close().await;
        });
        Ok(Box::new(reader))
    }
}

// --- connection diagnostics ------------------------------------------------------------------

/// An SSH dial plan as the diagnostics see it, with each ProxyCommand's `%` tokens expanded.
pub fn diagnostic_hops(plan: &[ChainHop]) -> Vec<DiagnosticHop> {
    plan.iter()
        .map(|hop| DiagnosticHop {
            host: hop.spec.host.clone(),
            hostname: hop.resolved.hostname.clone(),
            port: hop.port,
            user: hop.user.clone(),
            proxy_command: expanded_proxy_command(hop),
            password_only: hop.resolved.password_only,
            identity_files: hop.resolved.identity_files.clone(),
            identity_agent: hop.resolved.identity_agent.clone(),
        })
        .collect()
}
