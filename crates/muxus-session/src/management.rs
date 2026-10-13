//! NETCONF and gNMI sessions on the backend, and the saved request library
//! (the `AppContext` side of `server/src/management/management-socket.ts`,
//! `server/src/remote-desktop/desktop-passwords.ts` and
//! `server/src/routes/management-requests.ts`).
//!
//! The session itself is `muxus_mgmt::session::ManagementSession`; what it
//! needs from the rest of the app comes in through [`BackendManagementEnv`]:
//! saved hosts and recent connections from the database, pinned device
//! certificates, the password vault (with its unlock and create prompts) and
//! SSH connections from the shared connection manager, so a NETCONF session
//! or a gNMI gateway multiplexes onto a live transport like a terminal does.

use std::sync::Arc;

use muxus_core::api::saved_host_hop;
use muxus_core::management::{ManagementProtocol, SavedManagementRequest};
use muxus_core::protocol::{
    AuthPromptField, AuthPromptInfo, AuthPromptPurpose, AuthPromptResponse, HostKeyChallenge,
    ManagementClientMessage, ManagementProfile, ManagementServerMessage, NetconfProfile,
    SavedHostSessionProfile, SshGateway, SshProfile,
};
use muxus_mgmt::dial::DialError;
use muxus_mgmt::library::{self, LibraryError, ManagementRequestStore};
use muxus_mgmt::netconf::BoxedChannel;
use muxus_mgmt::session::{
    BoxFuture as MgmtFuture, BoxedStream, ManagementEnv, ManagementSession, PasswordRef,
    PasswordVault as MgmtPasswordVault, SessionIo, SshLease, TrustedIdentity,
};
use muxus_ssh::connection::{
    BoxFuture, Cancelled, ConnectIo, ConnectOptions, ConnectionLeaseOwner, MuxedConnectionLease,
    PasswordVault as SshPasswordVault, SshConnectionManager, VaultError,
};
use muxus_store::Database;
use muxus_store::vault::PasswordVault;

use crate::adapters::StoreVault;
use crate::{ApiError, ApiResult, Backend};

/// The two ends of a management session: messages in, messages out.
pub struct ManagementChannels {
    pub input: async_channel::Sender<ManagementClientMessage>,
    pub output: async_channel::Receiver<ManagementServerMessage>,
}

impl Backend {
    /// Start a NETCONF or gNMI session on the backend runtime (the `/ws/management`
    /// socket). Send `connect` first; the session ends when `input` closes, the
    /// device goes away or no `connect` arrives within 30 seconds, and then
    /// closes `output`.
    pub fn open_management_session(&self) -> ManagementChannels {
        let env = Arc::new(BackendManagementEnv {
            database: self.database().clone(),
            vault: self.vault().clone(),
            connections: self.connections().clone(),
        });
        let _runtime = self.runtime().enter();
        let (input, output) = ManagementSession::start(env);
        ManagementChannels { input, output }
    }

    /// `GET /api/management/requests?protocol=…&profileId=…`: the requests saved
    /// for this host and for every host of the protocol.
    pub fn management_requests(
        &self,
        protocol: ManagementProtocol,
        profile_id: Option<String>,
    ) -> impl Future<Output = ApiResult<Vec<SavedManagementRequest>>> + Send + 'static {
        let database = self.database().clone();
        self.blocking(move |_| {
            library::list_requests(
                &RequestStore(&database),
                Some(protocol.as_str()),
                profile_id.as_deref(),
            )
            .map_err(library_error)
        })
    }

    /// `PUT /api/management/requests`: create or replace a saved request
    /// (`{ id?, protocol, profileId?, name, request }`).
    pub fn save_management_request(
        &self,
        body: serde_json::Value,
    ) -> impl Future<Output = ApiResult<SavedManagementRequest>> + Send + 'static {
        let database = self.database().clone();
        self.blocking(move |_| {
            library::save_request(&RequestStore(&database), body).map_err(library_error)
        })
    }

    /// `DELETE /api/management/requests/:id`: whether a request was deleted.
    pub fn delete_management_request(
        &self,
        id: String,
    ) -> impl Future<Output = ApiResult<bool>> + Send + 'static {
        let database = self.database().clone();
        self.blocking(move |_| {
            library::delete_request(&RequestStore(&database), &id).map_err(library_error)
        })
    }
}

fn library_error(error: LibraryError) -> ApiError {
    ApiError::new(error.status(), error.to_string())
}

/// The database's saved requests, as the library route uses them.
struct RequestStore<'a>(&'a Database);

impl ManagementRequestStore for RequestStore<'_> {
    fn list(
        &self,
        protocol: ManagementProtocol,
        profile_id: Option<&str>,
    ) -> Result<Vec<SavedManagementRequest>, String> {
        self.0
            .list_management_requests(protocol, profile_id)
            .map_err(|error| error.to_string())
    }

    fn save(
        &self,
        input: muxus_core::management::SavedManagementRequestInput,
    ) -> Result<SavedManagementRequest, String> {
        self.0
            .save_management_request(&input)
            .map_err(|error| error.to_string())
    }

    fn delete(&self, id: &str) -> Result<bool, String> {
        self.0
            .delete_management_request(id)
            .map_err(|error| error.to_string())
    }
}

/// What a management session gets from the backend.
pub struct BackendManagementEnv {
    database: Arc<Database>,
    vault: Arc<PasswordVault>,
    connections: SshConnectionManager,
}

impl ManagementEnv for BackendManagementEnv {
    fn saved_profile(&self, profile_id: &str) -> Option<ManagementProfile> {
        let saved = self
            .database
            .saved_host_profile(profile_id)
            .ok()
            .flatten()?;
        match saved.profile {
            SavedHostSessionProfile::Gnmi(profile) => Some(ManagementProfile::Gnmi(profile)),
            SavedHostSessionProfile::Netconf(profile) => Some(ManagementProfile::Netconf(profile)),
            _ => None,
        }
    }

    fn record_connection(&self, profile_id: &str) {
        if let Err(error) = self.database.record_saved_host_connection(profile_id) {
            tracing::warn!(%error, "could not record recent connection");
        }
    }

    fn trusted_identity(&self, host: &str, port: u16, gateway: &str) -> Option<String> {
        self.database
            .trusted_desktop_identity(host, port, gateway)
            .ok()
            .flatten()
            .map(|identity| identity.fingerprint)
    }

    fn trust_identity(&self, identity: TrustedIdentity) {
        if let Err(error) = self.database.trust_desktop_identity(
            &identity.host,
            identity.port,
            &identity.gateway,
            &identity.fingerprint,
            &identity.subject,
        ) {
            tracing::warn!(%error, "could not pin the device certificate");
        }
    }

    fn passwords(&self) -> Option<Arc<dyn MgmtPasswordVault>> {
        Some(Arc::new(DesktopPasswords {
            vault: StoreVault(self.vault.clone()),
        }))
    }

    fn connect_ssh_gateway<'a>(
        &'a self,
        gateway: &'a SshGateway,
        io: SessionIo,
    ) -> MgmtFuture<'a, Result<Arc<dyn SshLease>, String>> {
        Box::pin(async move {
            let profile = SshProfile {
                profile_id: gateway.profile_id.clone(),
                ..SshProfile::new(gateway.target.clone())
            };
            self.connect(&profile, io).await
        })
    }

    fn connect_netconf<'a>(
        &'a self,
        profile: &'a NetconfProfile,
        io: SessionIo,
    ) -> MgmtFuture<'a, Result<Arc<dyn SshLease>, String>> {
        Box::pin(async move { self.connect(&netconf_ssh_profile(profile), io).await })
    }
}

/// The SSH profile a NETCONF server is dialed with: `ssh -s host netconf`,
/// with the profile's port and user and its gateway as the jump host.
pub fn netconf_ssh_profile(profile: &NetconfProfile) -> SshProfile {
    let user = profile
        .username
        .as_deref()
        .map(muxus_core::js::js_trim)
        .filter(|user| !user.is_empty())
        .map(str::to_owned);
    SshProfile {
        port: Some(profile.port),
        user,
        proxy_jump: profile.ssh_gateway.as_ref().map(|gateway| {
            vec![match &gateway.profile_id {
                Some(id) => saved_host_hop(id),
                None => gateway.target.clone(),
            }]
        }),
        ..SshProfile::new(profile.host.clone())
    }
}

impl BackendManagementEnv {
    async fn connect(
        &self,
        profile: &SshProfile,
        io: SessionIo,
    ) -> Result<Arc<dyn SshLease>, String> {
        let lease = self
            .connections
            .connect(
                profile,
                Arc::new(ManagementConnectIo { io }),
                ConnectionLeaseOwner::Management,
                ConnectOptions::default(),
            )
            .await
            .map_err(|error| error.to_string())?;
        lease.connection().wait_for_post_auth().await;
        Ok(Arc::new(Lease(lease)))
    }
}

/// The SSH connection's prompts over the management session.
struct ManagementConnectIo {
    io: SessionIo,
}

impl ConnectIo for ManagementConnectIo {
    fn status(&self, message: &str, transient: bool) {
        self.io.status(message, transient);
    }

    fn prompt(&self, info: AuthPromptInfo) -> BoxFuture<'_, Result<AuthPromptResponse, Cancelled>> {
        Box::pin(async move { self.io.prompt(info).await.map_err(|_| Cancelled) })
    }

    fn host_key(&self, challenge: HostKeyChallenge) -> BoxFuture<'_, Result<bool, Cancelled>> {
        Box::pin(async move { self.io.host_key(challenge).await.map_err(|_| Cancelled) })
    }
}

/// A management session's hold on an SSH transport; released when the session drops it.
struct Lease(MuxedConnectionLease);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl SshLease for Lease {
    fn open_direct_tcpip<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> MgmtFuture<'a, Result<BoxedStream, DialError>> {
        Box::pin(async move {
            let channel = self
                .0
                .connection()
                .open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0)
                .await
                .map_err(|error| {
                    let prohibited = matches!(
                        error,
                        russh::Error::ChannelOpenFailure(
                            russh::ChannelOpenFailure::AdministrativelyProhibited
                        )
                    );
                    DialError::from_channel_open(&error.to_string(), prohibited)
                })?;
            Ok(Box::new(channel.into_stream()) as BoxedStream)
        })
    }

    fn open_subsystem<'a>(&'a self, name: &'a str) -> MgmtFuture<'a, Result<BoxedChannel, String>> {
        Box::pin(async move {
            let channel = self
                .0
                .connection()
                .handle()
                .channel_open_session()
                .await
                .map_err(|error| error.to_string())?;
            channel
                .request_subsystem(true, name)
                .await
                .map_err(|error| error.to_string())?;
            Ok(Box::new(channel.into_stream()) as BoxedChannel)
        })
    }

    fn address(&self) -> String {
        self.0.target.label()
    }

    fn closed(&self) -> MgmtFuture<'static, Option<String>> {
        let connection = self.0.connection().clone();
        Box::pin(async move { connection.closed().await })
    }
}

/// Saved gNMI passwords in the password vault (`DesktopPasswords`): unlock
/// and create prompts go through the session.
struct DesktopPasswords {
    vault: StoreVault,
}

/// Master-password attempts before the vault stays locked.
const MASTER_PASSWORD_ATTEMPTS: usize = 3;

/// What a master-password attempt does with the password typed.
type VaultOperation<T> =
    Box<dyn Fn(String) -> BoxFuture<'static, Result<T, VaultError>> + Send + Sync>;

impl DesktopPasswords {
    async fn with_master_password<T>(
        &self,
        io: &SessionIo,
        name: &str,
        instructions: &str,
        skip_label: &str,
        operation: VaultOperation<T>,
    ) -> Result<Option<T>, String> {
        let mut error: Option<String> = None;
        for _ in 0..MASTER_PASSWORD_ATTEMPTS {
            let response = io
                .prompt(AuthPromptInfo {
                    name: Some(name.into()),
                    instructions: Some(match &error {
                        Some(error) => format!("{error}\n\n{instructions}"),
                        None => instructions.into(),
                    }),
                    host: None,
                    prompts: vec![AuthPromptField {
                        prompt: "Master password".into(),
                        echo: false,
                    }],
                    purpose: Some(AuthPromptPurpose::VaultUnlock),
                    remember_password: None,
                    skip_label: Some(skip_label.into()),
                })
                .await?;
            if response.skipped == Some(true) {
                return Ok(None);
            }
            let master = response.answers.first().cloned().unwrap_or_default();
            match operation(master).await {
                Ok(value) => return Ok(Some(value)),
                Err(VaultError::InvalidMasterPassword(message))
                | Err(VaultError::InvalidMasterPasswordFormat(message)) => {
                    error = Some(message);
                }
                Err(other) => return Err(other.to_string()),
            }
        }
        io.status("The password vault remains locked.", false);
        Ok(None)
    }

    async fn create_vault(&self, io: &SessionIo) -> Result<bool, String> {
        let response = io
            .prompt(AuthPromptInfo {
                name: Some("Create password vault".into()),
                instructions: Some(
                    "Create a master password to protect viewing and editing saved passwords. \
                     By default, Muxus stores the vault key in the operating-system credential store \
                     so saved passwords can be used without another master-password prompt."
                        .into(),
                ),
                host: None,
                prompts: vec![
                    AuthPromptField {
                        prompt: "Master password".into(),
                        echo: false,
                    },
                    AuthPromptField {
                        prompt: "Confirm master password".into(),
                        echo: false,
                    },
                ],
                purpose: Some(AuthPromptPurpose::VaultCreate),
                remember_password: None,
                skip_label: Some("Not now".into()),
            })
            .await?;
        if response.skipped == Some(true) {
            return Ok(false);
        }
        let master = response.answers.first().cloned().unwrap_or_default();
        let confirmation = response.answers.get(1).cloned().unwrap_or_default();
        if master != confirmation {
            return Err("The master-password confirmation did not match.".into());
        }
        match self.vault.create(&master).await {
            Ok(()) | Err(VaultError::AlreadyConfigured) => Ok(true),
            Err(error) => Err(error.to_string()),
        }
    }
}

impl MgmtPasswordVault for DesktopPasswords {
    fn has(&self, account: &str) -> bool {
        self.vault.has_ssh_password(account)
    }

    fn read<'a>(
        &'a self,
        reference: &'a PasswordRef,
        io: &'a SessionIo,
    ) -> MgmtFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            if !self.vault.has_ssh_password(&reference.account) {
                return Ok(None);
            }
            let result = if !self.vault.status().locked {
                self.vault
                    .ssh_password(&reference.account, None)
                    .await
                    .map(Some)
            } else {
                let vault = self.vault.clone();
                let account = reference.account.clone();
                let operation: VaultOperation<String> = Box::new(move |master| {
                    let (vault, account) = (vault.clone(), account.clone());
                    Box::pin(async move { vault.ssh_password(&account, Some(&master)).await })
                });
                return self
                    .with_master_password(
                        io,
                        "Unlock password vault",
                        &format!(
                            "Enter the master password to use the saved password for {}.",
                            reference.label
                        ),
                        "Use another password",
                        operation,
                    )
                    .await;
            };
            match result {
                Ok(value) => Ok(value),
                Err(VaultError::CredentialVaultCorrupt) => {
                    io.status(
                        &format!(
                            "The saved password for {} is damaged. Enter it again to replace the saved copy.",
                            reference.label
                        ),
                        false,
                    );
                    Ok(None)
                }
                Err(VaultError::KeyStoreUnavailable) => {
                    io.status(
                        &format!(
                            "The OS credential store is unavailable. Enter the password for {}.",
                            reference.label
                        ),
                        false,
                    );
                    Ok(None)
                }
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn remember<'a>(
        &'a self,
        reference: &'a PasswordRef,
        password: &'a str,
        io: &'a SessionIo,
    ) -> MgmtFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if !self.vault.status().configured && !self.create_vault(io).await? {
                return Ok(());
            }
            if self.vault.status().locked {
                let vault = self.vault.clone();
                let (account, label, secret) = (
                    reference.account.clone(),
                    reference.label.clone(),
                    password.to_owned(),
                );
                let operation: VaultOperation<()> = Box::new(move |master| {
                    let vault = vault.clone();
                    let (account, label, secret) = (account.clone(), label.clone(), secret.clone());
                    Box::pin(async move {
                        vault
                            .remember_ssh_password(&account, &label, &secret, Some(&master))
                            .await
                    })
                });
                let saved = self
                    .with_master_password(
                        io,
                        "Unlock password vault",
                        &format!(
                            "Enter the master password to remember the password for {}.",
                            reference.label
                        ),
                        "Not now",
                        operation,
                    )
                    .await?;
                if saved.is_none() {
                    return Ok(());
                }
            } else {
                self.vault
                    .remember_ssh_password(&reference.account, &reference.label, password, None)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            io.status(
                &format!("Remembered the password for {}.", reference.label),
                false,
            );
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dials_netconf_like_ssh_dash_s() {
        let profile = NetconfProfile {
            profile_id: None,
            host: "router1".into(),
            port: 830,
            username: Some("  admin ".into()),
            ssh_gateway: Some(SshGateway {
                target: "bastion".into(),
                profile_id: None,
            }),
        };
        let ssh = netconf_ssh_profile(&profile);
        assert_eq!(ssh.target, "router1");
        assert_eq!(ssh.port, Some(830));
        assert_eq!(ssh.user.as_deref(), Some("admin"));
        assert_eq!(ssh.proxy_jump, Some(vec!["bastion".to_owned()]));
        let saved = NetconfProfile {
            username: Some(" ".into()),
            ssh_gateway: Some(SshGateway {
                target: "bastion".into(),
                profile_id: Some("p1".into()),
            }),
            ..profile
        };
        let ssh = netconf_ssh_profile(&saved);
        assert_eq!(ssh.user, None);
        assert_eq!(ssh.proxy_jump, Some(vec![saved_host_hop("p1")]));
    }

    async fn sandbox() -> (tempfile::TempDir, Backend) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut options = crate::BackendOptions::sandbox(dir.path().join("data"), home);
        options.runtime = crate::RuntimeChoice::Own { worker_threads: 2 };
        let backend = Backend::start(options).await.unwrap();
        (dir, backend)
    }

    #[tokio::test]
    async fn keeps_the_request_library() {
        let (_dir, backend) = sandbox().await;
        let saved = backend
            .save_management_request(serde_json::json!({
                "protocol": "gnmi",
                "name": "  Counters ",
                "request": { "protocol": "gnmi", "operation": "get" },
            }))
            .await
            .unwrap();
        assert_eq!(saved.name, "Counters");
        let own = backend
            .save_management_request(serde_json::json!({
                "protocol": "gnmi",
                "profileId": "p1",
                "name": "Mine",
                "request": {},
            }))
            .await
            .unwrap();
        let listed = backend
            .management_requests(ManagementProtocol::Gnmi, Some("p1".into()))
            .await
            .unwrap();
        assert_eq!(listed.len(), 2);
        let shared = backend
            .management_requests(ManagementProtocol::Gnmi, None)
            .await
            .unwrap();
        assert_eq!(shared.len(), 1);
        let invalid = backend
            .save_management_request(
                serde_json::json!({ "protocol": "x", "name": "a", "request": {} }),
            )
            .await
            .unwrap_err();
        assert_eq!(invalid.status, 400);
        assert!(backend.delete_management_request(own.id).await.unwrap());
        assert!(
            !backend
                .delete_management_request("missing".into())
                .await
                .unwrap()
        );
        backend.shutdown().await;
    }

    #[tokio::test]
    async fn reports_a_device_that_cannot_be_reached() {
        let (_dir, backend) = sandbox().await;
        // A port nothing listens on.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let channels = backend.open_management_session();
        let profile: ManagementProfile = serde_json::from_value(serde_json::json!({
            "kind": "gnmi",
            "host": "127.0.0.1",
            "port": port,
            "tls": "plaintext",
        }))
        .unwrap();
        channels
            .input
            .send(ManagementClientMessage::Connect { profile })
            .await
            .unwrap();
        let mut exit = None;
        while let Ok(message) = channels.output.recv().await {
            if let ManagementServerMessage::Exit { message, .. } = message {
                exit = message;
            }
        }
        assert!(exit.unwrap_or_default().contains("refused"));
        backend.shutdown().await;
    }
}
