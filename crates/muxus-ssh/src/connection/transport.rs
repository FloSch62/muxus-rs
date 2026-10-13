//! Dialing one hop: the byte transport (TCP, ProxyCommand or a jump host's direct-tcpip
//! channel), the SSH handshake with host key verification, and the auth ladder.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use russh::client::{Config, Handle, Msg, Session};
use russh::keys::PublicKeyOrCertificate;
use russh::{Channel, ChannelOpenFailure, Disconnect};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::watch;

use super::auth::{AuthContext, AuthError, AuthLadder, AuthReport, commit_remembered_password};
use super::chain::{ChainHop, expanded_proxy_command, pinned_host_key_preference};
use super::deadline::PausableDeadline;
use super::health::{
    KeepaliveOptions, ObservedStream, TransportActivity, TransportHealth, ssh_keepalive_options,
};
use super::io::{ConnectIo, HostKeyChallenge, HostKeyState, PasswordVault};
use crate::algorithms::{AlgorithmSettings, connection_algorithms};
use crate::key_scan::resolve_agent_socket;
use crate::known_hosts::{
    GLOBAL_KNOWN_HOSTS, KnownHostVerdict, KnownHostsStore, default_known_hosts_path,
    fingerprint_like, fingerprint_sha256, host_key_matches_fingerprint, host_key_type,
};
use crate::local_env::LocalEnv;
use crate::responsive_agent::{
    AgentEndpoint, AgentHooks, AgentOperation, ResponsiveAgent, ResponsiveAgentOptions,
};
use crate::types::StrictHostKeyChecking;

/// Environment variable lookup (SSH_AUTH_SOCK, `$VAR` indirections).
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Any byte stream an SSH session can run over.
pub trait TransportStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> TransportStream for T {}

pub type BoxedStream = Box<dyn TransportStream>;

pub use muxus_core::protocol::AgentForwardingState as AgentForwarding;

/// What one dialed hop learned while connecting, kept for the session summary.
#[derive(Debug, Clone)]
pub struct HandshakeFacts {
    /// Server software from its identification string.
    pub server_software: Option<String>,
    pub kex: Option<String>,
    pub cipher: Option<String>,
    /// Client-to-server compression as negotiated.
    pub compression: Option<String>,
    /// Methods the server accepted, in order.
    pub auth_methods: Vec<String>,
    /// Signature algorithm of the key that logged in, when known.
    pub auth_key_algorithm: Option<String>,
    /// The config asked for compression (`Compression yes`).
    pub compression_requested: bool,
    pub agent_forwarding: AgentForwarding,
}

/// Receives channels the server opens towards us (X11, remote forwards). The handler asks every
/// registered sink in turn and gives the channel to the first that accepts it; a channel nobody
/// accepts is refused.
pub trait ServerChannelSink: Send + Sync {
    /// Whether this sink takes X11 connections.
    fn accepts_x11(&self) -> bool {
        false
    }
    /// An X11 connection from a remote client, already accepted.
    fn x11(&self, _channel: Channel<Msg>, _originator_address: &str, _originator_port: u32) {}
    /// Whether this sink owns the remote forward bound at `connected_address:connected_port`.
    fn accepts_forwarded_tcpip(&self, _connected_address: &str, _connected_port: u32) -> bool {
        false
    }
    /// A connection to a remote forward (`tcpip-forward`), already accepted.
    fn forwarded_tcpip(
        &self,
        _channel: Channel<Msg>,
        _connected_address: &str,
        _connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
    ) {
    }
}

/// Host key verification for one hop.
struct HostKeyVerifier {
    hop: ChainHop,
    io: Arc<dyn ConnectIo>,
    store: KnownHostsStore,
    deadline: Arc<PausableDeadline>,
}

/// State shared between the SSH session task and the rest of Muxus.
pub(crate) struct HandlerShared {
    closed: watch::Sender<bool>,
    close_reason: Mutex<Option<String>>,
    names: Mutex<Option<(String, String, String)>>,
    server_software: Mutex<Option<String>>,
    verifier: Mutex<Option<Arc<HostKeyVerifier>>>,
    accepted_key: Mutex<Option<Vec<u8>>>,
    banner_io: Mutex<Option<Arc<dyn ConnectIo>>>,
    sinks: Mutex<Vec<Arc<dyn ServerChannelSink>>>,
    /// Local agent socket for `auth-agent@openssh.com` channels; `None` refuses them.
    agent_forward_socket: Mutex<Option<String>>,
}

impl HandlerShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            closed: watch::channel(false).0,
            close_reason: Mutex::new(None),
            names: Mutex::new(None),
            server_software: Mutex::new(None),
            verifier: Mutex::new(None),
            accepted_key: Mutex::new(None),
            banner_io: Mutex::new(None),
            sinks: Mutex::new(Vec::new()),
            agent_forward_socket: Mutex::new(None),
        })
    }

    fn set_close_reason(&self, reason: String) {
        let mut slot = lock(&self.close_reason);
        if slot.is_none() {
            *slot = Some(reason);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The russh client handler for one hop.
pub struct ClientHandler {
    shared: Arc<HandlerShared>,
}

impl Drop for ClientHandler {
    fn drop(&mut self) {
        // The session task owns the handler, so this runs exactly when the session ends.
        let _ = self.shared.closed.send(true);
    }
}

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(io) = lock(&self.shared.banner_io).clone() {
            io.status(banner.trim_end(), false);
        }
        Ok(())
    }

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let blob = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                crate::security_keys::public_key_blob(key)
            }
            // Certificates are never requested (no cert host key algorithms are offered).
            PublicKeyOrCertificate::Certificate(_) => return Ok(false),
        };
        if let Some(accepted) = lock(&self.shared.accepted_key).clone() {
            // A re-key must present the key the user already accepted.
            return Ok(accepted == blob);
        }
        let verifier = lock(&self.shared.verifier).clone();
        let Some(verifier) = verifier else {
            return Ok(false);
        };
        let accepted = verify_host_key(&verifier, &blob).await;
        if accepted {
            *lock(&self.shared.accepted_key) = Some(blob);
        }
        Ok(accepted)
    }

    async fn kex_done(
        &mut self,
        _shared_secret: Option<&[u8]>,
        names: &russh::Names,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        *lock(&self.shared.names) = Some((
            names.kex.as_ref().to_string(),
            names.cipher.as_ref().to_string(),
            match names.client_compression {
                russh::compression::Compression::None => "none".to_string(),
                russh::compression::Compression::Zlib => "zlib".to_string(),
                russh::compression::Compression::ZlibOpenSSH => "zlib@openssh.com".to_string(),
            },
        ));
        let id = String::from_utf8_lossy(session.remote_sshid()).into_owned();
        // "SSH-2.0-OpenSSH_9.6p1 Ubuntu" → "OpenSSH_9.6p1 Ubuntu"
        let software = id
            .splitn(3, '-')
            .nth(2)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        *lock(&self.shared.server_software) = software;
        Ok(())
    }

    async fn server_channel_open_x11(
        &mut self,
        channel: Channel<Msg>,
        originator_address: &str,
        originator_port: u32,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let sink = lock(&self.shared.sinks)
            .iter()
            .find(|s| s.accepts_x11())
            .cloned();
        match sink {
            Some(sink) => {
                reply.accept().await;
                sink.x11(channel, originator_address, originator_port);
            }
            None => {
                reply
                    .reject(ChannelOpenFailure::AdministrativelyProhibited)
                    .await
            }
        }
        Ok(())
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let sink = lock(&self.shared.sinks)
            .iter()
            .find(|s| s.accepts_forwarded_tcpip(connected_address, connected_port))
            .cloned();
        match sink {
            Some(sink) => {
                reply.accept().await;
                sink.forwarded_tcpip(
                    channel,
                    connected_address,
                    connected_port,
                    originator_address,
                    originator_port,
                );
            }
            None => reply.reject(ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }

    async fn server_channel_open_agent_forward(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let socket = lock(&self.shared.agent_forward_socket).clone();
        let Some(socket) = socket else {
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        };
        reply.accept().await;
        // Agent forwarding is a transparent stream to the local agent.
        tokio::spawn(async move {
            let Ok(mut local) = connect_local_agent(&socket).await else {
                return;
            };
            let mut remote = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut remote, &mut local).await;
        });
        Ok(())
    }

    async fn disconnected(
        &mut self,
        reason: russh::client::DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        match reason {
            russh::client::DisconnectReason::ReceivedDisconnect(info) => {
                if !info.message.is_empty() {
                    self.shared.set_close_reason(info.message);
                }
                Ok(())
            }
            russh::client::DisconnectReason::Error(err) => {
                self.shared.set_close_reason(err.to_string());
                Err(err)
            }
        }
    }
}

#[cfg(unix)]
async fn connect_local_agent(socket: &str) -> std::io::Result<BoxedStream> {
    Ok(Box::new(tokio::net::UnixStream::connect(socket).await?))
}

#[cfg(windows)]
async fn connect_local_agent(socket: &str) -> std::io::Result<BoxedStream> {
    Ok(Box::new(
        tokio::net::windows::named_pipe::ClientOptions::new().open(socket)?,
    ))
}

/// The default store, or a per-host one when the config redirects the files.
pub fn known_hosts_for(
    hop: &ChainHop,
    default: &KnownHostsStore,
    env: &LocalEnv,
) -> KnownHostsStore {
    let r = &hop.resolved;
    if r.user_known_hosts_files.is_none() && r.global_known_hosts_files.is_none() {
        return default.clone();
    }
    let user = r
        .user_known_hosts_files
        .as_ref()
        .map(|files| files.iter().map(PathBuf::from).collect())
        .unwrap_or_else(|| vec![default_known_hosts_path(env)]);
    let global = r
        .global_known_hosts_files
        .as_ref()
        .map(|files| files.iter().map(PathBuf::from).collect())
        .unwrap_or_else(|| vec![PathBuf::from(GLOBAL_KNOWN_HOSTS)]);
    KnownHostsStore::new(user, global)
}

async fn verify_host_key(v: &HostKeyVerifier, key: &[u8]) -> bool {
    let host = &v.hop.resolved.hostname;
    let port = v.hop.port;
    let key_type = host_key_type(key);
    if let Some(pinned) = &v.hop.host_key_fingerprint {
        // Checked before known_hosts: no stored key or StrictHostKeyChecking setting may let a
        // key through that the link rules out.
        if !host_key_matches_fingerprint(key, pinned) {
            tracing::warn!(
                host,
                port,
                key_type,
                "host key does not match the fingerprint in the link"
            );
            v.io.status(
                &format!(
                    "HOST KEY MISMATCH for {host}: the link expects {pinned}, the server offered {key_type} {} — refusing to connect.",
                    fingerprint_like(key, pinned)
                ),
                false,
            );
            return false;
        }
        v.io.status(
            &format!("The {key_type} host key of {host} matches the fingerprint in the link."),
            false,
        );
    }
    let verdict = v.store.verify(host, port, key);
    tracing::debug!(
        host,
        port,
        key_type,
        ?verdict,
        "host key verified against known_hosts"
    );
    let (changed, previous) = match verdict {
        KnownHostVerdict::Ok => return true,
        KnownHostVerdict::Revoked => {
            tracing::warn!(host, port, "host key is revoked in known_hosts");
            v.io.status(
                &format!("HOST KEY REVOKED for {host} — remove the @revoked entry from known_hosts if this is intentional."),
                false,
            );
            return false;
        }
        KnownHostVerdict::Changed { previous } => (true, Some(previous)),
        KnownHostVerdict::Unknown => (false, None),
    };
    let strict = v
        .hop
        .resolved
        .strict_host_key_checking
        .unwrap_or(StrictHostKeyChecking::Ask);
    if strict == StrictHostKeyChecking::Yes {
        tracing::warn!(
            host,
            port,
            changed,
            "refusing host key under StrictHostKeyChecking=yes"
        );
        v.io.status(
            &if changed {
                format!("HOST KEY CHANGED for {host} and StrictHostKeyChecking is yes — refusing to connect.")
            } else {
                format!(
                    "No {key_type} host key for {host} in known_hosts and StrictHostKeyChecking is yes — refusing to connect."
                )
            },
            false,
        );
        return false;
    }
    // `no` on a *changed* key still asks below: silently trusting a swapped key is a footgun
    // OpenSSH's degraded continue-mode doesn't map to.
    if !changed
        && matches!(
            strict,
            StrictHostKeyChecking::AcceptNew | StrictHostKeyChecking::No
        )
    {
        if let Err(err) = v.store.record(host, port, key) {
            tracing::warn!(%err, "could not record the host key");
        }
        v.io.status(
            &format!(
                "Automatically accepted the new host key for {host} ({}).",
                fingerprint_sha256(key)
            ),
            false,
        );
        return true;
    }
    v.deadline.pause();
    let accepted =
        v.io.host_key(HostKeyChallenge {
            host: host.clone(),
            port,
            key_type: key_type.clone(),
            fingerprint: fingerprint_sha256(key),
            state: if changed {
                HostKeyState::Mismatch
            } else {
                HostKeyState::New
            },
            previous,
            hop: v.hop.hop_label.clone(),
        })
        .await
        .unwrap_or(false);
    v.deadline.resume();
    tracing::debug!(host, port, accepted, "host key decision from the user");
    if accepted && let Err(err) = v.store.record(host, port, key) {
        tracing::warn!(%err, "could not record the host key");
    }
    accepted
}

/// One authenticated SSH connection to one hop.
pub struct HopTransport {
    pub(crate) handle: Handle<ClientHandler>,
    pub(crate) shared: Arc<HandlerShared>,
    pub activity: Arc<TransportActivity>,
    pub keepalive: KeepaliveOptions,
    pub facts: HandshakeFacts,
    /// `user@host:port`.
    pub label: String,
    /// The agent socket this hop authenticated and forwards with.
    pub agent_socket: Option<String>,
    /// Completes when post-authentication work (saving a password) is done.
    pub(crate) post_auth: Mutex<Option<tokio::task::JoinHandle<()>>>,
    closed_rx: watch::Receiver<bool>,
}

impl std::fmt::Debug for HopTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HopTransport")
            .field("label", &self.label)
            .finish()
    }
}

impl HopTransport {
    pub fn is_closed(&self) -> bool {
        *self.closed_rx.borrow() || self.handle.is_closed()
    }

    /// Completes once the session has ended.
    pub async fn closed(&self) {
        let mut rx = self.closed_rx.clone();
        let _ = rx.wait_for(|closed| *closed).await;
    }

    /// Why the transport closed, when known.
    pub fn close_reason(&self) -> Option<String> {
        lock(&self.shared.close_reason).clone()
    }

    pub(crate) fn set_close_reason(&self, reason: String) {
        self.shared.set_close_reason(reason);
    }

    /// Ends the session; idempotent.
    pub fn close(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let _ = this
                .handle
                .disconnect(Disconnect::ByApplication, "", "en")
                .await;
        });
    }

    /// Passive keepalive health of this hop's own transport.
    pub fn watch_health(&self) -> Option<watch::Receiver<TransportHealth>> {
        self.activity.observe(self.keepalive.interval)
    }

    /// Registers a receiver for server-initiated channels.
    pub fn add_channel_sink(&self, sink: Arc<dyn ServerChannelSink>) {
        lock(&self.shared.sinks).push(sink);
    }

    pub fn remove_channel_sink(&self, sink: &Arc<dyn ServerChannelSink>) {
        lock(&self.shared.sinks).retain(|s| !Arc::ptr_eq(s, sink));
    }

    /// The SSH handle, for opening channels and global requests.
    pub fn handle(&self) -> &Handle<ClientHandler> {
        &self.handle
    }

    /// direct-tcpip channel to `host:port`: the `ssh -W` equivalent.
    pub async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Channel<Msg>, russh::Error> {
        self.handle
            .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0)
            .await
    }

    /// Waits for post-authentication password-vault prompts and saving.
    pub async fn wait_for_post_auth(&self) {
        let task = lock(&self.post_auth).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

/// Failure to reach or log in to a hop, worded for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ConnectError {
    pub message: String,
    pub kind: ConnectErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectErrorKind {
    Refused,
    Resolve,
    Timeout,
    Authentication,
    Cancelled,
    HostKey,
    Negotiation,
    /// A jump host refused to open the channel to the next hop.
    ChannelRefused,
    Other,
}

impl ConnectError {
    pub fn new(kind: ConnectErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind,
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(ConnectErrorKind::Other, message)
    }
}

/// Everything a dial needs besides the hop itself.
#[derive(Clone)]
pub(crate) struct DialContext {
    pub io: Arc<dyn ConnectIo>,
    pub vault: Option<Arc<dyn PasswordVault>>,
    pub known_hosts: KnownHostsStore,
    pub env: LocalEnv,
    pub env_lookup: EnvLookup,
    pub agent_operation_timeout: Duration,
    pub agent_wait_status: Option<Duration>,
    pub security_key_tools: crate::security_key_agent::OpenSshTools,
}

/// Agent hooks that pause the dial deadline and narrate waits.
struct DialAgentHooks {
    deadline: Arc<PausableDeadline>,
    io: Arc<dyn ConnectIo>,
    hop_name: String,
    host: String,
    socket: String,
    signature: Mutex<Option<String>>,
}

impl AgentHooks for DialAgentHooks {
    fn pause_deadline(&self) {
        self.deadline.pause();
    }
    fn resume_deadline(&self) {
        self.deadline.resume();
    }
    fn on_waiting(&self, operation: AgentOperation) {
        tracing::info!(host = %self.host, ?operation, socket = %self.socket, "waiting for the ssh agent to respond");
        self.io.status(
            match operation {
                AgentOperation::Sign => "Waiting for the SSH agent to approve signing…",
                AgentOperation::Identities => "Waiting for the SSH agent to list identities…",
            },
            true,
        );
    }
    fn on_security_key(&self, key: &russh::keys::PublicKey) {
        let label = crate::security_keys::security_key_label(
            key.algorithm().as_str(),
            &crate::security_keys::public_key_blob(key),
        );
        tracing::info!(host = %self.host, key = %label, "waiting for a security key touch");
        self.io.status(
            &format!(
                "Touch your security key to log in to {} ({label})…",
                self.hop_name
            ),
            true,
        );
    }
    fn on_signature(&self, algorithm: &str) {
        *lock(&self.signature) = Some(algorithm.to_string());
    }
}

/// Opens the byte transport and logs in to one hop. `sock` is the jump host channel when the
/// hop sits behind one.
pub(crate) async fn dial_hop(
    hop: &ChainHop,
    sock: Option<BoxedStream>,
    cx: &DialContext,
) -> Result<Arc<HopTransport>, ConnectError> {
    let agent_socket = resolve_agent_socket(
        hop.resolved.identity_agent.as_deref(),
        &*cx.env_lookup,
        &cx.env,
    );
    let hop_name = hop.display_name().to_string();
    let ready_timeout = Duration::from_secs_f64(hop.resolved.connect_timeout.unwrap_or(20.0));
    let deadline = Arc::new(PausableDeadline::new(ready_timeout));

    let algorithms =
        connection_algorithms(&AlgorithmSettings::from(&pinned_host_key_preference(hop)))
            .map_err(|e| ConnectError::new(ConnectErrorKind::Negotiation, e.to_string()))?;
    for note in &algorithms.notes {
        tracing::debug!(host = %hop.resolved.hostname, "{note}");
        cx.io.status(note, false);
    }

    let agent = match (&agent_socket, hop.resolved.identities_only) {
        (Some(socket), false) => Some(Arc::new(ResponsiveAgent::new(
            AgentEndpoint::Socket(PathBuf::from(socket)),
            ResponsiveAgentOptions {
                hooks: Arc::new(DialAgentHooks {
                    deadline: deadline.clone(),
                    io: cx.io.clone(),
                    hop_name: hop_name.clone(),
                    host: hop.resolved.hostname.clone(),
                    socket: socket.clone(),
                    signature: Mutex::new(None),
                }),
                wait_status: cx.agent_wait_status,
                operation_timeout: Some(cx.agent_operation_timeout),
                security_key_timeout: Some(crate::responsive_agent::DEFAULT_SECURITY_KEY_TIMEOUT),
            },
        ))),
        _ => None,
    };

    let keepalive = ssh_keepalive_options(
        hop.resolved.server_alive_interval,
        hop.resolved.server_alive_count_max,
    );
    let config = Arc::new(Config {
        preferred: algorithms.preferred(),
        keepalive_interval: (!keepalive.interval.is_zero()).then_some(keepalive.interval),
        keepalive_max: keepalive.count_max,
        // Interactive input consists of tiny packets. Disable Nagle explicitly so a keystroke
        // never waits for a previous packet's acknowledgement.
        nodelay: true,
        gex: russh::client::GexParams::new(2048, 4096, 8192).unwrap_or_default(),
        ..Config::default()
    });

    let shared = HandlerShared::new();
    *lock(&shared.verifier) = Some(Arc::new(HostKeyVerifier {
        hop: hop.clone(),
        io: cx.io.clone(),
        store: known_hosts_for(hop, &cx.known_hosts, &cx.env),
        deadline: deadline.clone(),
    }));
    *lock(&shared.banner_io) = Some(cx.io.clone());
    let closed_rx = shared.closed.subscribe();
    let activity = TransportActivity::new();

    tracing::debug!(
        host = %hop.resolved.hostname,
        port = hop.port,
        user = %hop.user,
        agent = agent_socket.is_some(),
        identities_only = hop.resolved.identities_only,
        proxy_command = hop.resolved.proxy_command.is_some(),
        via_jump = sock.is_some(),
        ?ready_timeout,
        "dialing ssh host"
    );

    let proxy_status: std::sync::OnceLock<ProxyCommandStatus> = std::sync::OnceLock::new();
    let work = async {
        let stream: BoxedStream = match sock {
            Some(sock) => sock,
            None => match expanded_proxy_command(hop) {
                Some(command) => {
                    let stream = ProxyCommandStream::spawn(&command).map_err(|e| {
                        ConnectError::other(format!("ProxyCommand could not start: {e}"))
                    })?;
                    let _ = proxy_status.set(stream.status());
                    Box::new(stream)
                }
                None => {
                    let tcp =
                        tokio::net::TcpStream::connect((hop.resolved.hostname.as_str(), hop.port))
                            .await
                            .map_err(|e| io_connect_error(&e, hop))?;
                    let _ = tcp.set_nodelay(true);
                    Box::new(tcp)
                }
            },
        };
        let observed = ObservedStream::new(stream, activity.clone());
        let mut handle = russh::client::connect_stream(
            config,
            observed,
            ClientHandler {
                shared: shared.clone(),
            },
        )
        .await
        .map_err(|e| handshake_error(e, &shared))?;
        let mut ladder = AuthLadder::new(AuthContext {
            hop: hop.clone(),
            io: cx.io.clone(),
            vault: cx.vault.clone(),
            deadline: deadline.clone(),
            agent: agent.clone(),
            env: cx.env.clone(),
            env_lookup: cx.env_lookup.clone(),
            security_key_tools: cx.security_key_tools.clone(),
        });
        let report = ladder.run(&mut handle).await;
        Ok::<_, ConnectError>((handle, report))
    };

    let outcome = tokio::select! {
        result = work => result,
        _ = deadline.expired() => Err(ConnectError::new(ConnectErrorKind::Timeout, "Timed out while waiting for SSH readiness.")),
    };
    deadline.clear();
    let (handle, report) = match outcome {
        Ok(pair) => pair,
        Err(err) => {
            // A ProxyCommand that died explains the failure better than the closed stream.
            let err = match proxy_status.get() {
                Some(status) => match status.failure(Duration::from_secs(1)).await {
                    Some(message) => ConnectError::other(message),
                    None => err,
                },
                None => err,
            };
            tracing::warn!(err = %err, host = %hop.resolved.hostname, port = hop.port, user = %hop.user, "ssh dial failed");
            return Err(friendly_connect_error(err, hop, None));
        }
    };
    let report: AuthReport = match report {
        Ok(report) => report,
        Err(err) => {
            let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
            let err = match err {
                AuthError::Cancelled => {
                    ConnectError::new(ConnectErrorKind::Cancelled, "authentication cancelled")
                }
                AuthError::Exhausted {
                    security_key_problem,
                } => {
                    let err = ConnectError::new(
                        ConnectErrorKind::Authentication,
                        "All configured authentication methods failed",
                    );
                    tracing::warn!(err = %err, host = %hop.resolved.hostname, "ssh dial failed");
                    return Err(friendly_connect_error(
                        err,
                        hop,
                        security_key_problem.as_deref(),
                    ));
                }
                AuthError::Transport(message) => ConnectError::other(message),
            };
            tracing::warn!(err = %err, host = %hop.resolved.hostname, "ssh dial failed");
            return Err(friendly_connect_error(err, hop, None));
        }
    };
    tracing::debug!(host = %hop.resolved.hostname, port = hop.port, user = %hop.user, "ssh connection ready");

    let names = lock(&shared.names).clone();
    let agent_forwarding = if !hop.resolved.forward_agent {
        AgentForwarding::Off
    } else if agent_socket.is_some() {
        AgentForwarding::On
    } else {
        AgentForwarding::NoAgent
    };
    if hop.resolved.forward_agent {
        *lock(&shared.agent_forward_socket) = agent_socket.clone();
    }
    let facts = HandshakeFacts {
        server_software: lock(&shared.server_software).clone(),
        kex: names.as_ref().map(|n| n.0.clone()),
        cipher: names.as_ref().map(|n| n.1.clone()),
        compression: names.as_ref().map(|n| n.2.clone()),
        auth_methods: report.methods.clone(),
        auth_key_algorithm: report.key_algorithm.clone(),
        compression_requested: hop.resolved.compression == Some(true),
        agent_forwarding,
    };
    // Saving a password never holds readiness on the remember-password dialog.
    let post_auth = match (report.remember, &cx.vault) {
        (Some(candidate), Some(vault)) => Some(tokio::spawn(commit_remembered_password(
            cx.io.clone(),
            vault.clone(),
            candidate,
        ))),
        _ => None,
    };
    *lock(&shared.banner_io) = None;
    *lock(&shared.verifier) = None;
    Ok(Arc::new(HopTransport {
        handle,
        shared,
        activity,
        keepalive,
        facts,
        label: hop.label(),
        agent_socket,
        post_auth: Mutex::new(post_auth),
        closed_rx,
    }))
}

fn io_connect_error(err: &std::io::Error, hop: &ChainHop) -> ConnectError {
    let where_ = format!("{}:{}", hop.resolved.hostname, hop.port);
    match err.kind() {
        std::io::ErrorKind::ConnectionRefused => ConnectError::new(
            ConnectErrorKind::Refused,
            format!("connection refused by {where_} — is sshd running?"),
        ),
        std::io::ErrorKind::TimedOut => ConnectError::new(
            ConnectErrorKind::Timeout,
            format!("connection to {where_} timed out"),
        ),
        _ if err.to_string().contains("failed to lookup address")
            || err.to_string().contains("Name or service not known")
            || err.to_string().contains("nodename nor servname") =>
        {
            ConnectError::new(
                ConnectErrorKind::Resolve,
                format!("could not resolve host {}", hop.resolved.hostname),
            )
        }
        _ => ConnectError::other(err.to_string()),
    }
}

/// The handshake failed: the SSH engine's error in the TypeScript engine's wording, so
/// `friendly_connect_error` adds the same hints.
fn handshake_error(err: russh::Error, shared: &HandlerShared) -> ConnectError {
    match err {
        russh::Error::NoCommonAlgo { kind, .. } => {
            let what = match kind {
                russh::AlgorithmKind::Kex => "key exchange algorithm",
                russh::AlgorithmKind::Key => "host key format",
                russh::AlgorithmKind::Cipher => "cipher",
                russh::AlgorithmKind::Compression => "compression algorithm",
                russh::AlgorithmKind::Mac => "MAC",
            };
            ConnectError::new(
                ConnectErrorKind::Negotiation,
                format!("Handshake failed: no matching {what}"),
            )
        }
        russh::Error::UnknownKey => {
            ConnectError::new(ConnectErrorKind::HostKey, "Host key verification failed.")
        }
        russh::Error::IO(io) => ConnectError::other(io.to_string()),
        russh::Error::Disconnect => ConnectError::other(
            lock(&shared.close_reason)
                .clone()
                .unwrap_or_else(|| "SSH connection closed before it became ready.".into()),
        ),
        other => ConnectError::other(other.to_string()),
    }
}

/// Translate the common failure modes into user-actionable messages. `security_key_problem`
/// explains why a security key could not take part.
pub fn friendly_connect_error(
    err: ConnectError,
    hop: &ChainHop,
    security_key_problem: Option<&str>,
) -> ConnectError {
    let msg = &err.message;
    let where_ = format!("{}:{}", hop.resolved.hostname, hop.port);
    if msg.contains("ECONNREFUSED") {
        return ConnectError::new(
            ConnectErrorKind::Refused,
            format!("connection refused by {where_} — is sshd running?"),
        );
    }
    if msg.contains("ENOTFOUND") || msg.contains("EAI_AGAIN") {
        return ConnectError::new(
            ConnectErrorKind::Resolve,
            format!("could not resolve host {}", hop.resolved.hostname),
        );
    }
    if msg.contains("ETIMEDOUT") || msg.to_lowercase().contains("timed out") {
        return ConnectError::new(
            ConnectErrorKind::Timeout,
            format!("connection to {where_} timed out"),
        );
    }
    if msg.contains("All configured authentication methods failed") {
        return ConnectError::new(
            ConnectErrorKind::Authentication,
            match security_key_problem {
                Some(problem) => format!("authentication to {where_} failed: {problem}"),
                None => format!("authentication to {where_} failed"),
            },
        );
    }
    if msg
        .to_lowercase()
        .starts_with("handshake failed: no matching")
    {
        return ConnectError::new(
            ConnectErrorKind::Negotiation,
            format!(
                "{msg} — if {where_} only speaks legacy algorithms, add KexAlgorithms/Ciphers/HostKeyAlgorithms lines to this host's ssh config (Advanced options in the host editor)"
            ),
        );
    }
    err
}

const PROXY_STDERR_LIMIT: usize = 8 * 1024;

/// Runs ProxyCommand with the user's platform shell and exposes its stdin/stdout as the byte
/// stream the SSH session expects. This matches OpenSSH's shell-command semantics, including
/// pipes and quoted arguments.
pub struct ProxyCommandStream {
    stdout: tokio::process::ChildStdout,
    stdin: tokio::process::ChildStdin,
    child: Arc<Mutex<tokio::process::Child>>,
    stderr: Arc<Mutex<String>>,
}

impl ProxyCommandStream {
    pub fn spawn(command: &str) -> std::io::Result<Self> {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/C").arg(command);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = tokio::process::Command::new("/bin/sh");
            c.arg("-c").arg(command);
            c
        };
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("no stdout"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no stdin"))?;
        let stderr_pipe = child.stderr.take();
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(mut pipe) = stderr_pipe {
            let sink = stderr.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 1024];
                while let Ok(n) = pipe.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let mut s = lock(&sink);
                    s.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if s.len() > PROXY_STDERR_LIMIT {
                        let cut = s.len() - PROXY_STDERR_LIMIT;
                        let cut = (cut..s.len())
                            .find(|i| s.is_char_boundary(*i))
                            .unwrap_or(s.len());
                        s.drain(..cut);
                    }
                }
            });
        }
        Ok(Self {
            stdout,
            stdin,
            child: Arc::new(Mutex::new(child)),
            stderr,
        })
    }

    /// A handle that reports how the command ended.
    pub fn status(&self) -> ProxyCommandStatus {
        ProxyCommandStatus {
            child: self.child.clone(),
            stderr: self.stderr.clone(),
        }
    }

    fn exit_error(&self) -> Option<std::io::Error> {
        self.status().exit_message().map(std::io::Error::other)
    }
}

/// How a ProxyCommand ended, read after the fact.
#[derive(Clone)]
pub struct ProxyCommandStatus {
    child: Arc<Mutex<tokio::process::Child>>,
    stderr: Arc<Mutex<String>>,
}

impl ProxyCommandStatus {
    /// Waits up to `wait` for the command to exit; its failure message when it failed.
    pub async fn failure(&self, wait: Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(message) = self.exit_message() {
                return Some(message);
            }
            if lock(&self.child).try_wait().ok().flatten().is_some()
                || tokio::time::Instant::now() >= deadline
            {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn exit_message(&self) -> Option<String> {
        let status = lock(&self.child).try_wait().ok().flatten()?;
        if status.success() {
            return None;
        }
        let reason = match status.code() {
            Some(code) => format!("exit code {code}"),
            None => "a signal".to_string(),
        };
        let detail = lock(&self.stderr).trim().to_string();
        Some(if detail.is_empty() {
            format!("ProxyCommand failed with {reason}")
        } else {
            format!("ProxyCommand failed with {reason}: {detail}")
        })
    }
}

impl AsyncRead for ProxyCommandStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.stdout).poll_read(cx, buf) {
            Poll::Ready(Ok(())) if buf.filled().len() == before => match self.exit_error() {
                Some(err) => Poll::Ready(Err(err)),
                None => Poll::Ready(Ok(())),
            },
            other => other,
        }
    }
}

impl AsyncWrite for ProxyCommandStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}
