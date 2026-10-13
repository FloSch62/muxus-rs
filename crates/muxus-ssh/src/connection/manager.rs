//! Dials SSH targets exactly the way `ssh <target>` would, using OpenSSH as the source for
//! connection details: alias resolution, ProxyJump chains (each hop resolved, verified and
//! authenticated in its own right), ProxyCommand transports, agent, CertificateFile/IdentityFile,
//! keyboard-interactive and password auth in OpenSSH order, and host keys checked against the
//! real known_hosts files.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::Channel;
use russh::client::Msg;
use tokio::sync::watch;

use super::chain::{
    ChainHop, ChainSources, DialPlanError, build_chain, find_metadata_alias, merge_config_forwards,
    mux_key,
};
use super::health::TransportHealth;
use super::io::{ConnectIo, PasswordVault};
use super::jump_hops::{JUMP_HOP_IDLE, JumpHop, JumpHopOptions, JumpHopRegistry};
use super::leases::{
    ConnectionLeaseOwner, ConnectionLeaseRegistry, LeaseableConnection, TransportLease,
};
use super::profile::SshProfile;
use super::session::{
    ChannelError, SessionChannel, SessionRequest, SessionSettings, open_session, probe_command,
    terminal_pty_options, wants_pty,
};
use super::transport::{
    AgentForwarding, BoxedStream, ConnectError, ConnectErrorKind, DialContext, HandshakeFacts,
    HopTransport, ServerChannelSink, dial_hop,
};
use crate::config::{ConfigDocument, parse_host_spec};
use crate::folder_auth::FolderAuthDefaults;
use crate::known_hosts::KnownHostsStore;
use crate::local_env::LocalEnv;
use crate::remote_shell_integration::{
    MAX_PROBE_OUTPUT, OpenShellError, PROBE_TIMEOUT, RemoteFileKind, RemoteShellHost,
    RemoteShellSftp, RemoteShellTransportLost, open_remote_shell,
};
use crate::responsive_agent::{DEFAULT_AGENT_OPERATION_TIMEOUT, DEFAULT_AGENT_WAIT_STATUS};
use crate::types::ConfigForward;

type Lookup<T> = Arc<dyn Fn(&str) -> T + Send + Sync>;

/// What the manager needs from the rest of Muxus. Every field is optional except the account
/// facts, so tests can build one with only what they exercise.
#[derive(Clone)]
pub struct ManagerOptions {
    pub env: LocalEnv,
    /// Environment lookup (SSH_AUTH_SOCK, $VAR indirections).
    pub env_lookup: Lookup<Option<String>>,
    pub known_hosts: KnownHostsStore,
    pub load_config: Arc<dyn Fn() -> ConfigDocument + Send + Sync>,
    pub vault: Option<Arc<dyn PasswordVault>>,
    /// Current database-owned SSH profile, keyed by stable profile ID.
    pub saved_ssh_profile: Option<Lookup<Option<SshProfile>>>,
    /// Folder-inherited connection defaults per alias (Muxus sidebar folders).
    pub folder_auth: Option<Lookup<Option<FolderAuthDefaults>>>,
    /// Folder defaults for Muxus-owned profiles, keyed by stable profile ID.
    pub profile_folder_auth: Option<Lookup<Option<FolderAuthDefaults>>>,
    /// Muxus-owned per-host compatibility settings; never written to ssh_config.
    pub disable_sftp_for_host: Option<Lookup<bool>>,
    pub console_compatibility_for_host: Option<Lookup<bool>>,
    pub disable_sftp_for_profile: Option<Lookup<bool>>,
    pub console_compatibility_for_profile: Option<Lookup<bool>>,
    pub agent_operation_timeout: Duration,
    pub agent_wait_status: Option<Duration>,
    /// Local X server for X11 forwarding; absent disables it.
    pub x11: Option<Arc<crate::x11::LocalX11>>,
    pub jump_hop_idle: Duration,
    /// The OpenSSH programs security key IdentityFiles sign through.
    pub security_key_tools: crate::security_key_agent::OpenSshTools,
}

impl ManagerOptions {
    /// Defaults for the given account: the user's known_hosts and ssh_config, the process
    /// environment, no vault, no folders.
    pub fn new(env: LocalEnv) -> Self {
        let config_path = env.default_ssh_config_path();
        let config_env = env.clone();
        Self {
            known_hosts: KnownHostsStore::default_for(&env),
            load_config: Arc::new(move || {
                crate::config::load_config_document(&config_path, &config_env)
            }),
            env,
            env_lookup: Arc::new(|name: &str| std::env::var(name).ok()),
            vault: None,
            saved_ssh_profile: None,
            folder_auth: None,
            profile_folder_auth: None,
            disable_sftp_for_host: None,
            console_compatibility_for_host: None,
            disable_sftp_for_profile: None,
            console_compatibility_for_profile: None,
            agent_operation_timeout: DEFAULT_AGENT_OPERATION_TIMEOUT,
            agent_wait_status: Some(DEFAULT_AGENT_WAIT_STATUS),
            x11: None,
            jump_hop_idle: JUMP_HOP_IDLE,
            security_key_tools: crate::security_key_agent::OpenSshTools::default(),
        }
    }
}

/// How a transport was reached and what it negotiated.
#[derive(Debug, Clone)]
pub struct TransportFacts {
    pub handshake: HandshakeFacts,
    /// Jump hosts in dialing order; empty for a direct connection.
    pub jump_hosts: Vec<String>,
    /// The first hop was reached through a ProxyCommand.
    pub proxy_command: bool,
}

pub use muxus_core::api::ConnectionInfo;
pub use muxus_core::protocol::SshSessionSummary;

/// One SSH transport to a target, shared by every lease on it.
pub struct ManagedConnection {
    pub id: String,
    pub transport: Arc<HopTransport>,
    pub profile: SshProfile,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Dial-plan identity for connection sharing; see [`mux_key`].
    pub mux_key: String,
    /// Concrete OpenSSH alias eligible for Muxus-owned recent-use metadata.
    pub metadata_alias: Option<String>,
    /// Whether callers may open SFTP channels on this transport.
    pub sftp_available: bool,
    pub facts: TransportFacts,
    console_compatibility: bool,
    target: ChainHop,
    config_forwards: Mutex<Vec<ConfigForward>>,
    replacement_token: Mutex<Option<String>>,
    superseded: AtomicBool,
    health: watch::Receiver<TransportHealth>,
    closed: watch::Receiver<bool>,
    ending: AtomicBool,
    jump_route: Option<Arc<JumpHop>>,
    /// The transport's X11 forwarding state (fake cookie, refusal).
    x11: Option<crate::x11::X11Transport>,
    sftp: tokio::sync::Mutex<Option<Arc<SftpChannel>>>,
}

/// Per-request timeout of the shared SFTP session. Transfers keep several requests in flight and
/// ssh2 never timed SFTP requests out, so only a stalled server, not a slow link, should hit it.
const SFTP_REQUEST_TIMEOUT_SECS: u64 = 60;

/// A connection's SFTP session: the raw protocol session, so file operations can use OpenSSH
/// extensions (`posix-rename@openssh.com`) and directory long names, plus the extensions the
/// server advertised.
pub struct SftpChannel {
    pub session: russh_sftp::client::RawSftpSession,
    pub extensions: HashMap<String, String>,
}

impl std::fmt::Debug for SftpChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpChannel")
            .field("extensions", &self.extensions)
            .finish()
    }
}

impl std::fmt::Debug for ManagedConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedConnection")
            .field("id", &self.id)
            .field("mux_key", &self.mux_key)
            .finish()
    }
}

impl LeaseableConnection for ManagedConnection {
    fn id(&self) -> &str {
        &self.id
    }

    fn close(&self) {
        if self.ending.swap(true, Ordering::SeqCst) {
            return;
        }
        self.transport.close();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Routes X11 channels of one transport to the local X server.
struct X11Sink(crate::x11::X11Transport);

impl ServerChannelSink for X11Sink {
    fn accepts_x11(&self) -> bool {
        true
    }

    fn x11(&self, channel: Channel<Msg>, originator_address: &str, originator_port: u32) {
        let transport = self.0.clone();
        let address = originator_address.to_string();
        tokio::spawn(async move {
            if let Err(err) = transport
                .handle_channel(channel.into_stream(), &address, originator_port)
                .await
            {
                tracing::debug!(%err, "forwarded X11 connection ended");
            }
        });
    }
}

impl ManagedConnection {
    /// Force-close the transport, regardless of active leases.
    pub fn close(&self) {
        LeaseableConnection::close(self);
    }

    /// Current passive keepalive health of the transport (any hop of its route).
    pub fn health(&self) -> TransportHealth {
        *self.health.borrow()
    }

    /// Health updates.
    pub fn subscribe_health(&self) -> watch::Receiver<TransportHealth> {
        self.health.clone()
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    /// Completes when the transport has closed; returns why, when known.
    pub async fn closed(&self) -> Option<String> {
        let mut rx = self.closed.clone();
        let _ = rx.wait_for(|c| *c).await;
        self.transport.close_reason()
    }

    /// The resolved final hop the transport was dialed for.
    pub fn target(&self) -> &ChainHop {
        &self.target
    }

    /// *Forward lines resolved from ssh config: auto-started once the session is up.
    pub fn config_forwards(&self) -> Vec<ConfigForward> {
        lock(&self.config_forwards).clone()
    }

    pub fn superseded(&self) -> bool {
        self.superseded.load(Ordering::SeqCst)
    }

    /// Whether the server answered an x11-req on this transport with failure.
    pub fn x11_refused(&self) -> bool {
        self.x11.as_ref().is_some_and(|x| x.refused())
    }

    /// The transport's X11 forwarding state, when X11 is configured.
    pub fn x11(&self) -> Option<&crate::x11::X11Transport> {
        self.x11.as_ref()
    }

    /// Waits for post-authentication password-vault prompts and saving.
    pub async fn wait_for_post_auth(&self) {
        self.transport.wait_for_post_auth().await;
    }

    /// direct-tcpip channel from the server to `host:port`.
    pub async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u32,
        originator_address: &str,
        originator_port: u32,
    ) -> Result<Channel<Msg>, russh::Error> {
        self.transport
            .handle
            .channel_open_direct_tcpip(host, port, originator_address, originator_port)
            .await
    }

    /// The SSH handle, for global requests such as `tcpip-forward`.
    pub fn handle(&self) -> &russh::client::Handle<super::transport::ClientHandler> {
        &self.transport.handle
    }

    /// Runs a non-interactive command (no tty) on this transport, as stats and diagnostics do.
    pub async fn exec(
        &self,
        command: &str,
        timeout: Duration,
    ) -> Result<super::session::ExecOutput, ChannelError> {
        super::session::exec_command(&self.transport.handle, command, None, timeout).await
    }

    /// One SFTP session per connection, shared by every file operation.
    pub async fn sftp(&self) -> Result<Arc<SftpChannel>, String> {
        if !self.sftp_available {
            return Err("SFTP is disabled for this host.".into());
        }
        let mut slot = self.sftp.lock().await;
        if let Some(session) = slot.as_ref() {
            return Ok(session.clone());
        }
        // A transient channel-open failure must not poison every future SFTP operation on an
        // otherwise healthy transport, so nothing is cached until the session is up.
        let channel = self
            .transport
            .handle
            .channel_open_session()
            .await
            .map_err(|e| e.to_string())?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| e.to_string())?;
        let session = russh_sftp::client::RawSftpSession::new(channel.into_stream());
        session.set_timeout(SFTP_REQUEST_TIMEOUT_SECS);
        let version = session.init().await.map_err(|e| e.to_string())?;
        let session = Arc::new(SftpChannel {
            session,
            extensions: version.extensions,
        });
        *slot = Some(session.clone());
        Ok(session)
    }

    /// Drops the cached SFTP session (after it failed), so the next call opens a fresh one.
    pub async fn reset_sftp(&self) {
        *self.sftp.lock().await = None;
    }

    /// Opens the terminal session: an integrated shell, a RemoteCommand exec, or a plain shell.
    /// `cwd` is a best-effort start directory, honoured only by integrated shells.
    pub async fn shell(
        &self,
        cols: u32,
        rows: u32,
        term: &str,
        session: &SessionSettings,
        cwd: Option<&str>,
    ) -> Result<SessionChannel, OpenShellError> {
        let pty = wants_pty(session.request_tty, session.remote_command.is_some())
            .then(|| terminal_pty_options(cols, rows, term));
        let env = if self.console_compatibility {
            None
        } else {
            session.env.clone()
        };
        let agent_forward = self.facts.handshake.agent_forwarding == AgentForwarding::On;
        // For the same reason console hosts only get x11-req when they ask for it.
        let x11_request = match &self.x11 {
            Some(x11) if !(self.console_compatibility && session.forward_x11 != Some(true)) => {
                x11.request(session.forward_x11).await
            }
            _ => None,
        };
        let request = SessionRequest {
            pty,
            env,
            x11: x11_request,
            agent_forward,
            command: None,
        };
        match self.open_shell_with(session, &request, cwd).await {
            Err(OpenShellError::Channel(message))
                if request.x11.is_some() && message == ChannelError::X11Rejected.to_string() =>
            {
                // Like ssh(1), a refused x11-req costs X11, not the session. The refused channel
                // was closed, so the retry opens a fresh one.
                if let Some(x11) = &self.x11 {
                    x11.mark_refused();
                }
                tracing::info!(host = %self.host, "ssh server refused X11 forwarding");
                let request = SessionRequest {
                    x11: None,
                    ..request
                };
                self.open_shell_with(session, &request, cwd).await
            }
            other => other,
        }
    }

    async fn open_shell_with(
        &self,
        session: &SessionSettings,
        request: &SessionRequest,
        cwd: Option<&str>,
    ) -> Result<SessionChannel, OpenShellError> {
        let channel_error = |e: ChannelError| OpenShellError::Channel(e.to_string());
        if let Some(command) = &session.remote_command {
            let exec = SessionRequest {
                command: Some(command.clone()),
                ..request.clone()
            };
            return match open_session(&self.transport.handle, &exec).await {
                Err(err)
                    if self.console_compatibility
                        && exec.pty.is_some()
                        && err.is_pty_rejection() =>
                {
                    open_session(
                        &self.transport.handle,
                        &SessionRequest { pty: None, ..exec },
                    )
                    .await
                    .map_err(channel_error)
                }
                other => other.map_err(channel_error),
            };
        }
        if request.pty.is_some() && self.sftp_available {
            let host = ConnectionShellHost {
                conn: self,
                request,
            };
            return open_remote_shell(&host, cwd).await;
        }
        // Console compatibility drops SendEnv/SetEnv: an env request before pty-req is an order
        // some appliances answer with a protocol-error disconnect, and a serial console has no
        // environment. Console appliances disagree about pty-req: some reject it, others refuse
        // a shell without one. Ask for the tty ssh(1) would and, like ssh(1), carry on without it
        // when the server says no.
        match open_session(&self.transport.handle, request).await {
            Err(err)
                if self.console_compatibility
                    && request.pty.is_some()
                    && err.is_pty_rejection() =>
            {
                open_session(
                    &self.transport.handle,
                    &SessionRequest {
                        pty: None,
                        ..request.clone()
                    },
                )
                .await
                .map_err(channel_error)
            }
            other => other.map_err(channel_error),
        }
    }
}

/// [`RemoteShellHost`] over a live connection.
struct ConnectionShellHost<'a> {
    conn: &'a ManagedConnection,
    request: &'a SessionRequest,
}

struct ConnectionSftp(Arc<SftpChannel>);

impl RemoteShellSftp for ConnectionSftp {
    async fn stat(&self, path: &str) -> Result<RemoteFileKind, String> {
        let meta = self
            .0
            .session
            .stat(path)
            .await
            .map_err(|e| e.to_string())?
            .attrs;
        Ok(if meta.is_dir() {
            RemoteFileKind::Directory
        } else if meta.is_regular() {
            RemoteFileKind::File
        } else {
            RemoteFileKind::Other
        })
    }

    async fn mkdir(&self, path: &str, mode: u32) -> Result<(), String> {
        use russh_sftp::protocol::FileAttributes;
        let session = &self.0.session;
        session
            .mkdir(path, FileAttributes::empty())
            .await
            .map_err(|e| e.to_string())?;
        let attrs = FileAttributes {
            permissions: Some(mode),
            ..Default::default()
        };
        let _ = session.setstat(path, attrs).await;
        Ok(())
    }

    async fn write_file(&self, path: &str, content: &str, mode: u32) -> Result<(), String> {
        use russh_sftp::protocol::{FileAttributes, OpenFlags};
        const CHUNK: usize = 32 * 1024;
        let session = &self.0.session;
        let attrs = FileAttributes {
            permissions: Some(mode),
            ..Default::default()
        };
        let handle = session
            .open(
                path,
                OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
                attrs,
            )
            .await
            .map_err(|e| e.to_string())?
            .handle;
        let mut written = Ok(());
        for (index, chunk) in content.as_bytes().chunks(CHUNK).enumerate() {
            let offset = (index * CHUNK) as u64;
            if let Err(e) = session.write(handle.as_str(), offset, chunk.to_vec()).await {
                written = Err(e.to_string());
                break;
            }
        }
        let closed = session.close(handle).await.map_err(|e| e.to_string());
        written.and(closed.map(|_| ()))
    }
}

impl RemoteShellHost for ConnectionShellHost<'_> {
    type Channel = SessionChannel;
    type Sftp = ConnectionSftp;

    async fn probe(&self, command: &str) -> Result<String, String> {
        probe_command(
            &self.conn.transport.handle,
            command,
            PROBE_TIMEOUT,
            MAX_PROBE_OUTPUT,
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn sftp(&self) -> Result<ConnectionSftp, String> {
        self.conn.sftp().await.map(ConnectionSftp)
    }

    async fn open_exec(&self, command: &str) -> Result<SessionChannel, String> {
        let request = SessionRequest {
            command: Some(command.to_string()),
            ..self.request.clone()
        };
        open_session(&self.conn.transport.handle, &request)
            .await
            .map_err(|e| e.to_string())
    }

    async fn open_shell(&self) -> Result<SessionChannel, String> {
        open_session(&self.conn.transport.handle, self.request)
            .await
            .map_err(|e| e.to_string())
    }

    fn transport_lost(&self) -> Option<Option<String>> {
        self.conn
            .transport
            .is_closed()
            .then(|| self.conn.transport.close_reason())
    }
}

/// A consumer's hold on a (possibly shared) transport.
pub type ConnectionLease = TransportLease<ManagedConnection>;

/// A lease from [`SshConnectionManager::connect`].
pub struct MuxedConnectionLease {
    pub lease: ConnectionLease,
    /// True when this lease multiplexes onto a pre-existing transport instead of a fresh dial.
    pub reused: bool,
    /// This connect call's resolved final hop (not the dialing call's): the source for per-alias
    /// session settings on a shared transport.
    pub target: ChainHop,
}

impl std::fmt::Debug for MuxedConnectionLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MuxedConnectionLease")
            .field("lease", &self.lease)
            .field("reused", &self.reused)
            .finish()
    }
}

impl std::fmt::Debug for TerminalShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalShell")
            .field("lease", &self.lease)
            .field("transport", &self.transport)
            .finish()
    }
}

impl MuxedConnectionLease {
    pub fn connection(&self) -> &Arc<ManagedConnection> {
        self.lease.connection()
    }

    pub fn release(&self) {
        self.lease.release();
    }
}

/// How a terminal's transport came about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellTransport {
    /// Fresh transport: this session owns starting its config forwards.
    New,
    /// Multiplexed onto a live transport that already has them.
    Shared,
    /// Dedicated transport dialed because the shared one refused another session; its sibling
    /// still owns the config forwards.
    Overflow,
}

/// An open terminal session.
pub struct TerminalShell {
    pub lease: MuxedConnectionLease,
    pub channel: SessionChannel,
    pub transport: ShellTransport,
    /// Port forwards are started after the shell opens, so they are left to the caller.
    pub summary: SshSessionSummary,
}

/// Options for one connect call.
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// Force-reconnect group token: skip transports established before this request; requests
    /// sharing a token share one replacement connection.
    pub fresh_transport: Option<String>,
    /// Internal fallbacks (MaxSessions overflow, plain-console retry): dial an own transport
    /// without joining or retiring anyone else's.
    pub dedicated_transport: bool,
    pub force_plain_shell: bool,
}

type DialResult = Result<Arc<ManagedConnection>, ConnectError>;

struct PendingDial {
    id: u64,
    result: watch::Receiver<Option<DialResult>>,
    fresh_token: Option<String>,
}

struct ManagerInner {
    options: ManagerOptions,
    connections: ConnectionLeaseRegistry<ManagedConnection>,
    jump_hops: JumpHopRegistry,
    /// In-flight dials by mux key, so simultaneous sessions share one TCP connection and one
    /// auth round-trip. A list, not a single slot: overlapping force-reconnect gestures each keep
    /// their own dial joinable until it settles, so a straggler always finds the dial of its own
    /// gesture.
    pending_dials: Mutex<HashMap<String, Vec<PendingDial>>>,
    next_dial: Mutex<u64>,
    /// Dial plans whose optional integration setup killed the remote transport; reset on app
    /// restart.
    automatic_plain_shell_keys: Mutex<HashSet<String>>,
}

/// Shares SSH transports between terminals, file browsers, forwards and editors.
#[derive(Clone)]
pub struct SshConnectionManager {
    inner: Arc<ManagerInner>,
}

/// Why connecting failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectFailure {
    #[error(transparent)]
    Plan(#[from] DialPlanError),
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error("{0}")]
    Shell(String),
}

impl SshConnectionManager {
    pub fn new(options: ManagerOptions) -> Self {
        let idle = options.jump_hop_idle;
        Self {
            inner: Arc::new(ManagerInner {
                options,
                connections: ConnectionLeaseRegistry::new(),
                jump_hops: JumpHopRegistry::new(idle),
                pending_dials: Mutex::new(HashMap::new()),
                next_dial: Mutex::new(0),
                automatic_plain_shell_keys: Mutex::new(HashSet::new()),
            }),
        }
    }

    pub fn options(&self) -> &ManagerOptions {
        &self.inner.options
    }

    pub fn known_hosts(&self) -> &KnownHostsStore {
        &self.inner.options.known_hosts
    }

    /// Acquire an independent consumer lease on an existing SSH transport.
    pub fn acquire(&self, id: &str, owner: ConnectionLeaseOwner) -> Option<ConnectionLease> {
        self.inner.connections.acquire(id, owner)
    }

    /// Live lease count on a connection, optionally restricted to owner kinds.
    pub fn lease_count(&self, id: &str, owners: Option<&[ConnectionLeaseOwner]>) -> usize {
        self.inner.connections.lease_count(id, owners)
    }

    /// Live transports (forwarding panel, connection reuse when starting tunnels).
    pub fn list(&self) -> Vec<ConnectionInfo> {
        // A superseded transport still carries its existing tunnels, but new consumers must land
        // on the replacement, not the corpse-to-be.
        self.inner
            .connections
            .list()
            .into_iter()
            .filter(|c| !c.superseded())
            .map(|c| ConnectionInfo {
                id: c.id.clone(),
                target: c.profile.target.clone(),
                host: c.host.clone(),
                port: c.port,
                user: c.user.clone(),
                metadata_alias: c.metadata_alias.clone(),
                sftp_available: c.sftp_available,
            })
            .collect()
    }

    /// The live transport with this id.
    pub fn get(&self, id: &str) -> Option<Arc<ManagedConnection>> {
        self.inner.connections.get(id)
    }

    /// Force-closes every transport and jump hop (application shutdown).
    pub fn close_all(&self) {
        self.inner.connections.close_all();
        self.inner.jump_hops.close_all();
    }

    fn chain_for(
        &self,
        doc: &ConfigDocument,
        profile: &SshProfile,
    ) -> Result<Vec<ChainHop>, DialPlanError> {
        let o = &self.inner.options;
        let folder = o.folder_auth.clone();
        let profile_folder = o.profile_folder_auth.clone();
        let saved = o.saved_ssh_profile.clone();
        let folder_fn = folder
            .as_ref()
            .map(|f| f.as_ref() as &(dyn Fn(&str) -> Option<FolderAuthDefaults> + Send + Sync));
        let profile_folder_fn = profile_folder
            .as_ref()
            .map(|f| f.as_ref() as &(dyn Fn(&str) -> Option<FolderAuthDefaults> + Send + Sync));
        let saved_fn = saved
            .as_ref()
            .map(|f| f.as_ref() as &(dyn Fn(&str) -> Option<SshProfile> + Send + Sync));
        build_chain(
            doc,
            profile,
            ChainSources {
                folder_auth: folder_fn,
                profile_folder_auth: profile_folder_fn,
                saved_profile: saved_fn,
            },
        )
    }

    /// The hops `connect` dials for a profile, in dialing order, the target last.
    pub fn dial_plan(&self, profile: &SshProfile) -> Result<Vec<ChainHop>, DialPlanError> {
        let profile = self.resolve_profile(profile)?;
        self.chain_for(&(self.inner.options.load_config)(), &profile)
    }

    /// The ssh_config alias whose Muxus metadata a session to this profile uses, as `connect`
    /// finds it.
    pub fn metadata_alias_for(&self, profile: &SshProfile) -> Option<String> {
        if !crate::connection::profile::uses_config(profile) || profile.profile_id.is_some() {
            return None;
        }
        find_metadata_alias(
            &(self.inner.options.load_config)(),
            &parse_host_spec(&profile.target).host,
        )
    }

    /// A stable database ID is a reference, not permission for a client or a stale workspace to
    /// replace the saved connection fields. Resolve it at the boundary so edits and deletions
    /// take effect on the next dial.
    pub fn resolve_profile(&self, profile: &SshProfile) -> Result<SshProfile, DialPlanError> {
        let Some(id) = &profile.profile_id else {
            return Ok(profile.clone());
        };
        let saved = self
            .inner
            .options
            .saved_ssh_profile
            .as_ref()
            .and_then(|f| f(id));
        let Some(saved) = saved.filter(|s| {
            s.profile_id.as_deref() == Some(id) && !crate::connection::profile::uses_config(s)
        }) else {
            return Err(DialPlanError(format!(
                "saved SSH profile \"{id}\" was not found"
            )));
        };
        // Connection fields come from the database. The keepalive fallback is an application
        // preference supplied by the current UI, so the wire value always wins (including its
        // absence), which must clear anything a client stored. So does a link's host key
        // fingerprint, which only tightens verification.
        Ok(SshProfile {
            keepalive_interval_seconds: profile.keepalive_interval_seconds,
            host_key_fingerprint: profile.host_key_fingerprint.clone(),
            ..saved
        })
    }

    fn lookup_bool(f: &Option<Lookup<bool>>, key: Option<&str>) -> bool {
        match (f, key) {
            (Some(f), Some(key)) => f(key),
            _ => false,
        }
    }

    /// Connect with OpenSSH-ControlMaster-style multiplexing: a session whose dial plan matches
    /// a live, healthy transport attaches to it as a new lease instead of opening another TCP
    /// connection. Split panes, SFTP, tunnels and repeated connects to one host share a single
    /// SSH connection, which also keeps Muxus inside server-side connection caps (MaxStartups,
    /// per-user limits). Concurrent dials to the same plan (workspace restore) collapse into one
    /// connection and one auth round-trip. A transport that still has to be dialed rides any jump
    /// hop already connected for its route.
    pub async fn connect(
        &self,
        profile: &SshProfile,
        io: Arc<dyn ConnectIo>,
        owner: ConnectionLeaseOwner,
        opts: ConnectOptions,
    ) -> Result<MuxedConnectionLease, ConnectFailure> {
        let profile = self.resolve_profile(profile)?;
        let doc = (self.inner.options.load_config)();
        let chain = self.chain_for(&doc, &profile)?;
        let target = chain
            .last()
            .cloned()
            .ok_or_else(|| DialPlanError("empty dial plan".into()))?;
        let o = &self.inner.options;
        let metadata_alias = if crate::connection::profile::uses_config(&profile) {
            find_metadata_alias(&doc, &target.spec.host)
        } else {
            None
        };
        let regular_key = mux_key(&chain, false, false);
        let console_compatibility =
            Self::lookup_bool(&o.console_compatibility_for_host, metadata_alias.as_deref())
                || Self::lookup_bool(
                    &o.console_compatibility_for_profile,
                    profile.profile_id.as_deref(),
                )
                || lock(&self.inner.automatic_plain_shell_keys).contains(&regular_key)
                || opts.force_plain_shell;
        let disable_sftp = console_compatibility
            || Self::lookup_bool(&o.disable_sftp_for_host, metadata_alias.as_deref())
            || Self::lookup_bool(&o.disable_sftp_for_profile, profile.profile_id.as_deref());
        let key = mux_key(&chain, disable_sftp, console_compatibility);
        let config_forwards = target.resolved.forwards.clone();
        let label = target.label();
        tracing::info!(target = %profile.target, %label, hops = chain.len(), ?owner, "ssh connect requested");

        let fresh_token = if opts.dedicated_transport {
            None
        } else {
            opts.fresh_transport.clone()
        };
        if !opts.dedicated_transport {
            // A force-reconnect request skips every pre-existing transport but may still share
            // the replacement its own gesture already established.
            if let Some(shared) = self.acquire_shared(&key, owner, &target, fresh_token.as_deref())
            {
                let conn = shared.connection();
                let merged = merge_config_forwards(&conn.config_forwards(), &config_forwards);
                *lock(&conn.config_forwards) = merged;
                tracing::debug!(%label, conn_id = %conn.id, "reusing multiplexed ssh transport");
                io.status(
                    &format!("Reusing the SSH connection to {label} (multiplexed)."),
                    true,
                );
                return Ok(shared);
            }
            // A failed dial fails every session waiting on it: auth prompts and errors surface
            // on the session that started the dial. A fresh request never waits on the dial it
            // is trying to escape: it joins an in-flight dial only when that dial belongs to its
            // own force-reconnect group. Regular requests join the newest dial.
            let pending = {
                let dials = lock(&self.inner.pending_dials);
                let list = dials.get(&key);
                match &fresh_token {
                    Some(token) => list
                        .and_then(|l| {
                            l.iter()
                                .find(|d| d.fresh_token.as_deref() == Some(token.as_str()))
                        })
                        .map(|d| d.result.clone()),
                    None => list.and_then(|l| l.last()).map(|d| d.result.clone()),
                }
            };
            if let Some(mut pending) = pending {
                io.status(
                    &format!("Waiting for the SSH connection to {label} …"),
                    true,
                );
                let result = pending
                    .wait_for(Option::is_some)
                    .await
                    .map(|r| r.clone())
                    .ok()
                    .flatten();
                match result {
                    Some(Ok(conn)) => {
                        if let Some(lease) = self.inner.connections.acquire(&conn.id, owner) {
                            let merged =
                                merge_config_forwards(&conn.config_forwards(), &config_forwards);
                            *lock(&conn.config_forwards) = merged;
                            return Ok(MuxedConnectionLease {
                                lease,
                                reused: true,
                                target,
                            });
                        }
                        // The transport died between ready and acquire; dial our own below.
                    }
                    Some(Err(err)) => return Err(err.into()),
                    None => {}
                }
            }
        }

        let (tx, rx) = watch::channel(None);
        let dial_id = {
            let mut next = lock(&self.inner.next_dial);
            *next += 1;
            *next
        };
        if !opts.dedicated_transport {
            lock(&self.inner.pending_dials)
                .entry(key.clone())
                .or_default()
                .push(PendingDial {
                    id: dial_id,
                    result: rx,
                    fresh_token: fresh_token.clone(),
                });
        }
        let result = self
            .dial_chain(
                &chain,
                &profile,
                io.clone(),
                owner,
                &key,
                metadata_alias,
                disable_sftp,
                console_compatibility,
                fresh_token.clone(),
            )
            .await;
        if let (Ok(lease), Some(token)) = (&result, &fresh_token) {
            // Tag before waiters observe the connection, and retire the replaced transports so
            // no later session multiplexes onto them.
            *lock(&lease.connection().replacement_token) = Some(token.clone());
            self.retire_replaced_transports(&key, &lease.connection().id);
        }
        let _ = tx.send(Some(match &result {
            Ok(lease) => Ok(lease.connection().clone()),
            Err(err) => Err(err.clone()),
        }));
        {
            let mut dials = lock(&self.inner.pending_dials);
            if let Some(list) = dials.get_mut(&key) {
                list.retain(|d| d.id != dial_id);
                if list.is_empty() {
                    dials.remove(&key);
                }
            }
        }
        let lease = result?;
        Ok(MuxedConnectionLease {
            lease,
            reused: false,
            target,
        })
    }

    /// Least-busy live, healthy transport with the same dial plan, if any. A superseded transport
    /// is never handed out again; a force-reconnect request reuses only the replacement its own
    /// gesture established.
    fn acquire_shared(
        &self,
        key: &str,
        owner: ConnectionLeaseOwner,
        target: &ChainHop,
        fresh_token: Option<&str>,
    ) -> Option<MuxedConnectionLease> {
        let mut candidates: Vec<Arc<ManagedConnection>> = self
            .inner
            .connections
            .list()
            .into_iter()
            .filter(|c| {
                c.mux_key == key
                    && c.health() == TransportHealth::Healthy
                    && !c.is_closed()
                    && match fresh_token {
                        Some(token) => lock(&c.replacement_token).as_deref() == Some(token),
                        None => !c.superseded(),
                    }
            })
            .collect();
        candidates.sort_by_key(|c| self.inner.connections.lease_count(&c.id, None));
        for conn in candidates {
            if let Some(lease) = self.inner.connections.acquire(&conn.id, owner) {
                return Some(MuxedConnectionLease {
                    lease,
                    reused: true,
                    target: target.clone(),
                });
            }
        }
        None
    }

    /// A live replacement makes every older same-plan transport ineligible for new consumers.
    /// Their existing leases (tunnels, transfers) stay valid, and the transport still closes only
    /// after the last of them releases.
    fn retire_replaced_transports(&self, key: &str, replacement_id: &str) {
        for conn in self.inner.connections.list() {
            if conn.id == replacement_id || conn.mux_key != key || conn.superseded() {
                continue;
            }
            conn.superseded.store(true, Ordering::SeqCst);
            tracing::info!(conn_id = %conn.id, host = %conn.host, "ssh transport superseded by a forced replacement");
        }
    }

    /// Terminal entry point: connect (sharing a transport when possible) and open the shell
    /// channel. When a shared transport refuses another session channel (sshd MaxSessions and
    /// similar per-connection caps), falls back to a dedicated transport: a new pane never fails
    /// just because the multiplexed connection is full.
    #[allow(clippy::too_many_arguments)]
    pub async fn connect_shell(
        &self,
        profile: &SshProfile,
        io: Arc<dyn ConnectIo>,
        cols: u32,
        rows: u32,
        term: &str,
        fresh_transport: Option<String>,
        cwd: Option<&str>,
    ) -> Result<TerminalShell, ConnectFailure> {
        let opts = ConnectOptions {
            fresh_transport,
            ..ConnectOptions::default()
        };
        let lease = self
            .connect(profile, io.clone(), ConnectionLeaseOwner::Terminal, opts)
            .await?;
        let settings = SessionSettings::from_resolved(&lease.target.resolved);
        let shell = lease
            .connection()
            .clone()
            .shell(cols, rows, term, &settings, cwd)
            .await;
        match shell {
            Ok(channel) => {
                let transport = if lease.reused {
                    ShellTransport::Shared
                } else {
                    ShellTransport::New
                };
                Ok(self.opened(lease, channel, transport, &*io).await)
            }
            Err(OpenShellError::TransportLost(lost)) => {
                lease.release();
                self.retry_plain_shell(
                    profile,
                    io,
                    cols,
                    rows,
                    term,
                    &lease,
                    ShellTransport::New,
                    lost,
                )
                .await
            }
            Err(OpenShellError::Channel(err)) => {
                lease.release();
                if !lease.reused {
                    return Err(ConnectFailure::Shell(err));
                }
                tracing::info!(host = %lease.connection().host, %err, "shared ssh transport refused a session; dialing a dedicated connection");
                io.status(
                    "The shared SSH connection refused another session — opening a dedicated one …",
                    true,
                );
                let dedicated = self
                    .connect(
                        profile,
                        io.clone(),
                        ConnectionLeaseOwner::Terminal,
                        ConnectOptions {
                            dedicated_transport: true,
                            ..ConnectOptions::default()
                        },
                    )
                    .await?;
                let settings = SessionSettings::from_resolved(&dedicated.target.resolved);
                match dedicated
                    .connection()
                    .clone()
                    .shell(cols, rows, term, &settings, cwd)
                    .await
                {
                    Ok(channel) => Ok(self
                        .opened(dedicated, channel, ShellTransport::Overflow, &*io)
                        .await),
                    Err(OpenShellError::TransportLost(lost)) => {
                        dedicated.release();
                        self.retry_plain_shell(
                            profile,
                            io,
                            cols,
                            rows,
                            term,
                            &dedicated,
                            ShellTransport::Overflow,
                            lost,
                        )
                        .await
                    }
                    Err(OpenShellError::Channel(err)) => {
                        dedicated.release();
                        Err(ConnectFailure::Shell(err))
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn retry_plain_shell(
        &self,
        profile: &SshProfile,
        io: Arc<dyn ConnectIo>,
        cols: u32,
        rows: u32,
        term: &str,
        failed: &MuxedConnectionLease,
        transport: ShellTransport,
        error: RemoteShellTransportLost,
    ) -> Result<TerminalShell, ConnectFailure> {
        // A probe-triggered disconnect can happen for config aliases and direct profiles alike.
        // The non-compatibility mux key is therefore the stable runtime identity to remember,
        // without silently persisting host edits.
        lock(&self.inner.automatic_plain_shell_keys).insert(failed.connection().mux_key.clone());
        tracing::info!(host = %failed.connection().host, err = ?error.transport_error, "remote shell integration disconnected the transport; retrying as a plain console");
        io.status(
            "This server rejected SSH shell integration — reconnecting in plain-console mode …",
            true,
        );
        let compatible = self
            .connect(
                profile,
                io.clone(),
                ConnectionLeaseOwner::Terminal,
                ConnectOptions {
                    dedicated_transport: true,
                    force_plain_shell: true,
                    ..ConnectOptions::default()
                },
            )
            .await?;
        let settings = SessionSettings::from_resolved(&compatible.target.resolved);
        match compatible
            .connection()
            .clone()
            .shell(cols, rows, term, &settings, None)
            .await
        {
            Ok(channel) => Ok(self.opened(compatible, channel, transport, &*io).await),
            Err(err) => {
                compatible.release();
                Err(ConnectFailure::Shell(err.to_string()))
            }
        }
    }

    /// Report X11 trouble and describe what the new session ended up with.
    async fn opened(
        &self,
        lease: MuxedConnectionLease,
        channel: SessionChannel,
        transport: ShellTransport,
        io: &dyn ConnectIo,
    ) -> TerminalShell {
        let conn = lease.connection().clone();
        let forward_x11 = lease.target.resolved.forward_x11;
        let x11_state = match conn.x11() {
            Some(x11) => {
                if let Some(message) = x11.status_message(forward_x11).await {
                    io.status(&message, false);
                }
                x11.session_state(forward_x11, channel.x11_forwarded).await
            }
            None => {
                if channel.x11_forwarded {
                    crate::x11::X11SessionState::On
                } else {
                    crate::x11::X11SessionState::Off
                }
            }
        };
        let facts = &conn.facts;
        use muxus_core::protocol::CompressionState;
        let compression = match facts.handshake.compression.as_deref() {
            Some(c) if c != "none" => CompressionState::On,
            _ if facts.handshake.compression_requested => CompressionState::Unsupported,
            _ => CompressionState::Off,
        };
        let summary = SshSessionSummary {
            user: conn.user.clone(),
            host: conn.host.clone(),
            port: conn.port,
            jump_hosts: facts.jump_hosts.clone(),
            proxy_command: facts.proxy_command,
            shared: transport == ShellTransport::Shared,
            server_software: facts.handshake.server_software.clone(),
            auth_methods: facts.handshake.auth_methods.clone(),
            auth_key_algorithm: facts.handshake.auth_key_algorithm.clone(),
            cipher: facts.handshake.cipher.clone(),
            kex: facts.handshake.kex.clone(),
            compression,
            sftp: conn.sftp_available,
            x11: match x11_state {
                crate::x11::X11SessionState::On => muxus_core::protocol::X11State::On,
                crate::x11::X11SessionState::Off => muxus_core::protocol::X11State::Off,
                crate::x11::X11SessionState::Refused => muxus_core::protocol::X11State::Refused,
                crate::x11::X11SessionState::NoServer => muxus_core::protocol::X11State::NoServer,
            },
            agent_forwarding: facts.handshake.agent_forwarding,
            // Port forwards are started after the shell opens, so the caller fills them in.
            forwards: Vec::new(),
        };
        TerminalShell {
            lease,
            channel,
            transport,
            summary,
        }
    }

    fn dial_context(&self, io: Arc<dyn ConnectIo>) -> DialContext {
        let o = &self.inner.options;
        DialContext {
            io,
            vault: o.vault.clone(),
            known_hosts: o.known_hosts.clone(),
            env: o.env.clone(),
            env_lookup: o.env_lookup.clone(),
            agent_operation_timeout: o.agent_operation_timeout,
            agent_wait_status: o.agent_wait_status,
            security_key_tools: o.security_key_tools.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn dial_chain(
        &self,
        chain: &[ChainHop],
        profile: &SshProfile,
        io: Arc<dyn ConnectIo>,
        owner: ConnectionLeaseOwner,
        key: &str,
        metadata_alias: Option<String>,
        disable_sftp: bool,
        console_compatibility: bool,
        fresh_token: Option<String>,
    ) -> Result<ConnectionLease, ConnectError> {
        match self
            .dial_chain_once(
                chain,
                profile,
                io.clone(),
                owner,
                key,
                metadata_alias.clone(),
                disable_sftp,
                console_compatibility,
                fresh_token.clone(),
                true,
            )
            .await
        {
            Err(DialChainError::StaleJumpHop { hop, cause }) => {
                hop.mark_superseded();
                tracing::info!(hop = %hop.label, err = %cause, "shared ssh jump host connection failed; dialing the jump host again");
                self.dial_chain_once(
                    chain,
                    profile,
                    io,
                    owner,
                    key,
                    metadata_alias,
                    disable_sftp,
                    console_compatibility,
                    fresh_token,
                    false,
                )
                .await
                .map_err(DialChainError::into_connect_error)
            }
            other => other.map_err(DialChainError::into_connect_error),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn dial_chain_once(
        &self,
        chain: &[ChainHop],
        profile: &SshProfile,
        io: Arc<dyn ConnectIo>,
        owner: ConnectionLeaseOwner,
        key: &str,
        metadata_alias: Option<String>,
        disable_sftp: bool,
        console_compatibility: bool,
        fresh_token: Option<String>,
        reuse_jump_hops: bool,
    ) -> Result<ConnectionLease, DialChainError> {
        // Ride the deepest jump hop this route is already logged in to, so a second connection
        // behind a jump host costs no second login there.
        let mut first = 0;
        let mut route: Option<Arc<JumpHop>> = None;
        if reuse_jump_hops && chain.len() >= 2 {
            for i in (0..chain.len() - 1).rev() {
                route = self
                    .inner
                    .jump_hops
                    .reusable(&mux_key(&chain[..=i], false, false), fresh_token.as_deref());
                if route.is_some() {
                    first = i + 1;
                    break;
                }
            }
        }
        let reused = route.clone();
        if let Some(hop) = &reused {
            hop.retain();
        }
        let cx = self.dial_context(io.clone());
        let mut dialed: Option<Arc<HopTransport>> = None;
        let mut post_auth: Vec<Arc<HopTransport>> = Vec::new();
        let result: Result<(), DialChainError> = async {
            for (i, hop) in chain.iter().enumerate().skip(first) {
                let reusing = route.is_some()
                    && reused
                        .as_ref()
                        .zip(route.as_ref())
                        .is_some_and(|(a, b)| Arc::ptr_eq(a, b));
                let via = match &route {
                    Some(r) => format!(
                        " via {}{}",
                        r.label,
                        if reusing {
                            " over its open connection"
                        } else {
                            ""
                        }
                    ),
                    None => String::new(),
                };
                io.status(
                    &format!(
                        "Connecting to {}@{}:{}{via} …",
                        hop.user, hop.resolved.hostname, hop.port
                    ),
                    true,
                );
                let mut sock: Option<BoxedStream> = None;
                if let Some(r) = &route {
                    match r
                        .transport
                        .open_direct_tcpip(&hop.resolved.hostname, hop.port)
                        .await
                    {
                        Ok(channel) => sock = Some(Box::new(channel.into_stream())),
                        Err(err) => {
                            let refusal = matches!(err, russh::Error::ChannelOpenFailure(_));
                            // A refusal from a live jump host is about the target. Anything else
                            // means the shared connection itself is gone or unusable.
                            if reusing && (r.is_closed() || !refusal) {
                                return Err(DialChainError::StaleJumpHop {
                                    hop: r.clone(),
                                    cause: err.to_string(),
                                });
                            }
                            return Err(DialChainError::Connect(ConnectError::new(
                                if refusal {
                                    ConnectErrorKind::ChannelRefused
                                } else {
                                    ConnectErrorKind::Other
                                },
                                format!(
                                    "jump host could not reach {}:{}: {}",
                                    hop.resolved.hostname,
                                    hop.port,
                                    describe_open_error(&err)
                                ),
                            )));
                        }
                    }
                }
                let transport = dial_hop(hop, sock, &cx)
                    .await
                    .map_err(DialChainError::Connect)?;
                post_auth.push(transport.clone());
                if i == chain.len() - 1 {
                    dialed = Some(transport);
                    break;
                }
                let next = self.inner.jump_hops.add(JumpHopOptions {
                    key: mux_key(&chain[..=i], false, false),
                    label: hop.spec.host.clone(),
                    transport,
                    parent: route.clone(),
                    replacement_token: fresh_token.clone(),
                });
                next.retain();
                if let Some(previous) = route.take() {
                    previous.release();
                }
                route = Some(next);
            }
            Ok(())
        }
        .await;
        if let Err(err) = result {
            // Hops dialed for this chain close unless another chain rides them.
            if let Some(r) = route {
                r.release();
            }
            tracing::debug!(%err, hops = chain.len(), from = first, "ssh dial chain aborted");
            return Err(err);
        }
        let Some(transport) = dialed else {
            return Err(DialChainError::Connect(ConnectError::other(
                "empty dial plan",
            )));
        };
        let target = chain.last().cloned().expect("non-empty chain");
        let jumps: Vec<Arc<JumpHop>> = route.as_ref().map(|r| r.route()).unwrap_or_default();

        // Combined health: the target's own keepalives and every hop of its route.
        let (health_tx, health_rx) = watch::channel(TransportHealth::Healthy);
        let mut sources: Vec<watch::Receiver<TransportHealth>> =
            jumps.iter().map(|h| h.subscribe_health()).collect();
        if let Some(own) = transport.watch_health() {
            sources.push(own);
        }
        if !sources.is_empty() {
            tokio::spawn(combine_health(sources, health_tx));
        }

        // Wait for vault saves of every hop together, like the TypeScript Promise.all. The
        // hops' tasks are taken here, before the combined task replaces the target's own:
        // taken inside the spawned task, the target's slot could already hold the combined
        // task, which would then wait for itself while every caller returned at once.
        let pending: Vec<tokio::task::JoinHandle<()>> = post_auth
            .iter()
            .filter_map(|hop| {
                hop.post_auth
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
            })
            .collect();
        let combined_post = tokio::spawn(async move {
            for task in pending {
                let _ = task.await;
            }
        });
        *transport
            .post_auth
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(combined_post);

        let x11 = self.inner.options.x11.as_ref().map(|local| local.attach());
        if let Some(x11) = &x11 {
            transport.add_channel_sink(Arc::new(X11Sink(x11.clone())));
        }
        let (closed_tx, closed_rx) = watch::channel(false);
        let id = new_connection_id();
        let conn = Arc::new(ManagedConnection {
            id: id.clone(),
            transport: transport.clone(),
            profile: profile.clone(),
            host: target.resolved.hostname.clone(),
            port: target.port,
            user: target.user.clone(),
            mux_key: key.to_string(),
            metadata_alias,
            sftp_available: !disable_sftp,
            facts: TransportFacts {
                handshake: transport.facts.clone(),
                jump_hosts: chain[..chain.len() - 1]
                    .iter()
                    .map(|h| h.spec.host.clone())
                    .collect(),
                proxy_command: chain[0].resolved.proxy_command.is_some(),
            },
            console_compatibility,
            config_forwards: Mutex::new(target.resolved.forwards.clone()),
            target,
            replacement_token: Mutex::new(None),
            superseded: AtomicBool::new(false),
            health: health_rx,
            closed: closed_rx,
            ending: AtomicBool::new(false),
            jump_route: route.clone(),
            x11,
            sftp: tokio::sync::Mutex::new(None),
        });
        let lease = self.inner.connections.register(conn.clone(), owner);

        // A dying hop takes the whole chain with it; surface that as a close.
        for hop in &jumps {
            let hop = hop.clone();
            let weak = Arc::downgrade(&conn);
            tokio::spawn(async move {
                hop.closed().await;
                if let Some(conn) = weak.upgrade()
                    && !conn.is_closed()
                    && !conn.ending.load(Ordering::SeqCst)
                {
                    conn.transport
                        .set_close_reason(format!("SSH jump host {} disconnected.", hop.label));
                    conn.transport.close();
                }
            });
        }
        let registry_conn = Arc::downgrade(&conn);
        let manager = self.clone();
        tokio::spawn(async move {
            transport.closed().await;
            let _ = closed_tx.send(true);
            if let Some(conn) = registry_conn.upgrade() {
                manager.inner.connections.mark_closed(&conn);
                // Jump hops end with the last chain routed through them.
                if let Some(route) = &conn.jump_route {
                    route.release();
                }
            }
        });
        Ok(lease)
    }
}

async fn combine_health(
    mut sources: Vec<watch::Receiver<TransportHealth>>,
    tx: watch::Sender<TransportHealth>,
) {
    loop {
        let next = if sources
            .iter()
            .any(|s| *s.borrow() == TransportHealth::Suspect)
        {
            TransportHealth::Suspect
        } else {
            TransportHealth::Healthy
        };
        if *tx.borrow() != next {
            let _ = tx.send(next);
        }
        if sources.is_empty() || tx.is_closed() {
            return;
        }
        let changes = sources
            .iter_mut()
            .map(|s| Box::pin(s.changed()))
            .collect::<Vec<_>>();
        let (result, index) = select_all(changes).await;
        if result.is_err() {
            sources.remove(index);
        }
    }
}

/// Waits for the first of several futures (a small local stand-in for `futures::select_all`).
async fn select_all<F: Future + Unpin>(mut futures: Vec<F>) -> (F::Output, usize) {
    std::future::poll_fn(move |cx| {
        for (i, f) in futures.iter_mut().enumerate() {
            if let std::task::Poll::Ready(out) = std::pin::Pin::new(f).poll(cx) {
                return std::task::Poll::Ready((out, i));
            }
        }
        std::task::Poll::Pending
    })
    .await
}

fn describe_open_error(err: &russh::Error) -> String {
    match err {
        russh::Error::ChannelOpenFailure(reason) => match reason {
            russh::ChannelOpenFailure::AdministrativelyProhibited => {
                "administratively prohibited".into()
            }
            russh::ChannelOpenFailure::ConnectFailed => "connection failed".into(),
            russh::ChannelOpenFailure::UnknownChannelType => "unknown channel type".into(),
            russh::ChannelOpenFailure::ResourceShortage => "resource shortage".into(),
            other => format!("{other:?}"),
        },
        other => other.to_string(),
    }
}

fn new_connection_id() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ModuleSymbhasOwnPr-0123456789ABCDEFGHNRVfgctiUvz_KqYTJkLxpZXIjQW";
    let mut rng = rand::thread_rng();
    (0..10)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

#[derive(Debug, thiserror::Error)]
enum DialChainError {
    #[error(transparent)]
    Connect(ConnectError),
    /// A shared jump hop could not carry another chain; the chain dials it again.
    #[error("shared connection to jump host {} failed", hop.label)]
    StaleJumpHop { hop: Arc<JumpHop>, cause: String },
}

impl DialChainError {
    fn into_connect_error(self) -> ConnectError {
        match self {
            DialChainError::Connect(err) => err,
            stale @ DialChainError::StaleJumpHop { .. } => ConnectError::other(stale.to_string()),
        }
    }
}
