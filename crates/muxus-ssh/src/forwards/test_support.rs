//! Fixtures for the forward tests: a relay in front of the in-process sshd whose connections a
//! test can cut, loopback echo servers, saved tunnels a test can edit, and a connection layer
//! whose dials a test scripts.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{AbortHandle, JoinHandle};

use super::manager::{ForwardConnections, ForwardManager, ForwardManagerOptions};
use super::types::{ForwardInfo, ForwardStatus, ForwardType, TunnelRecord};
use crate::connection::io::{
    AuthPromptInfo, BoxFuture, ConnectIo, HostKeyChallenge, HostKeyState, PromptField,
};
use crate::connection::manager::{
    ConnectFailure, ConnectOptions, ConnectionLease, MuxedConnectionLease, SshConnectionManager,
};
use crate::connection::tests::{Sandbox, TestIo};
use crate::connection::transport::ConnectError;
use crate::connection::{ConnectionLeaseOwner, SshProfile};
use crate::known_hosts::KnownHostsStore;
use crate::testing::server::{ServerSpec, TestServer, start};

/// The password the lab's sshd takes.
pub const PASSWORD: &str = "secret";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// A TCP relay to a backend port. [`cut`](Self::cut) ends every relayed connection at once, the
/// way a network drop or a restarted sshd would; new connections still go through.
pub struct Relay {
    port: u16,
    accepted: Arc<AtomicUsize>,
    live: Arc<Mutex<Vec<AbortHandle>>>,
    task: JoinHandle<()>,
}

impl Relay {
    pub async fn start(backend: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let live = Arc::new(Mutex::new(Vec::new()));
        let (counter, connections) = (accepted.clone(), live.clone());
        let task = tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let splice = tokio::spawn(async move {
                    let Ok(mut outbound) = TcpStream::connect(("127.0.0.1", backend)).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                });
                lock(&connections).push(splice.abort_handle());
            }
        });
        Self {
            port,
            accepted,
            live,
            task,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Connections the relay took so far.
    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    pub fn cut(&self) {
        for splice in lock(&self.live).drain(..) {
            splice.abort();
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
        self.cut();
    }
}

/// A loopback TCP echo server.
pub struct Echo {
    pub port: u16,
    task: JoinHandle<()>,
}

pub async fn echo_server() -> Echo {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = socket.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    Echo { port, task }
}

impl Drop for Echo {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A loopback port nothing listens on right now.
pub fn free_port() -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    probe.local_addr().unwrap().port()
}

/// Round-trip a line through a forwarded local port.
pub async fn echo_through(port: u16, text: &str) -> io::Result<String> {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).await?;
    socket.write_all(text.as_bytes()).await?;
    let mut reply = vec![0u8; text.len()];
    tokio::time::timeout(Duration::from_secs(10), socket.read_exact(&mut reply))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no echo"))??;
    Ok(String::from_utf8_lossy(&reply).into_owned())
}

/// The in-process sshd behind a [`Relay`], a scratch HOME whose ssh_config has `Host db` pointing
/// at the relay, and a connection manager for that HOME.
pub struct Lab {
    pub server: TestServer,
    pub relay: Relay,
    pub sandbox: Sandbox,
    pub manager: SshConnectionManager,
}

impl Lab {
    /// An sshd that lets the agent's key in, so nothing ever prompts.
    pub async fn keyed() -> Self {
        let key = crate::testing::agent::ed25519();
        let mut spec = ServerSpec::new().password(PASSWORD);
        spec.authorized_keys = vec![key.public_key().clone()];
        let sandbox = Sandbox::new();
        let socket = crate::testing::agent::start_agent(sandbox.dir.path(), &[key]).await;
        Self::start(spec, sandbox, Some(socket.to_string_lossy().into_owned())).await
    }

    /// An sshd that only takes the password, and no agent.
    pub async fn password_only() -> Self {
        Self::start(ServerSpec::new().password(PASSWORD), Sandbox::new(), None).await
    }

    async fn start(spec: ServerSpec, sandbox: Sandbox, agent: Option<String>) -> Self {
        let server = start(spec).await;
        let relay = Relay::start(server.port()).await;
        let port = format!("  Port {}", relay.port());
        sandbox.write_config(&["Host db", "  HostName 127.0.0.1", &port, "  User tester"]);
        let mut options = sandbox.options();
        if let Some(socket) = agent {
            options.env_lookup =
                Arc::new(move |name: &str| (name == "SSH_AUTH_SOCK").then(|| socket.clone()));
        }
        let manager = SshConnectionManager::new(options);
        Self {
            server,
            relay,
            sandbox,
            manager,
        }
    }

    /// Trust the sshd's host key, as an earlier interactive dial would have.
    pub fn trust(&self) {
        let blob = crate::security_keys::public_key_blob(&self.server.host_key);
        KnownHostsStore::new(vec![self.sandbox.known_hosts.clone()], vec![])
            .record("127.0.0.1", self.relay.port(), &blob)
            .unwrap();
    }

    /// What the forwarding panel does first: dial with prompts answered (and trust the host key).
    pub async fn dial(&self, profile: &SshProfile) -> MuxedConnectionLease {
        self.manager
            .connect(
                profile,
                TestIo::accepting(PASSWORD),
                ConnectionLeaseOwner::Dial,
                ConnectOptions::default(),
            )
            .await
            .unwrap()
    }

    /// An interactive dial of `db` carrying a keepalive fallback of 25 seconds.
    pub async fn dial_db(&self) -> MuxedConnectionLease {
        self.dial(&SshProfile {
            keepalive_interval_seconds: Some(25),
            ..SshProfile::new("db")
        })
        .await
    }

    /// Forward leases on a connection.
    pub fn forward_leases(&self, conn_id: &str) -> usize {
        self.manager
            .lease_count(conn_id, Some(&[ConnectionLeaseOwner::Forward]))
    }

    /// Drop a live connection from the server's side, with the reason the client reports.
    pub fn drop_connection(&self, lease: &MuxedConnectionLease, reason: Option<&str>) {
        if let Some(reason) = reason {
            lease
                .connection()
                .transport
                .set_close_reason(reason.to_string());
        }
        self.relay.cut();
    }
}

/// How one scripted dial turns out.
#[derive(Debug, Clone)]
pub enum Dial {
    /// Dial for real, through the lab.
    Real,
    /// Fail with this message.
    Fail(&'static str),
    /// Ask for a password, then fail.
    Prompt,
    /// Ask about a changed host key, then fail.
    HostKey,
}

/// The lab's connection manager, with dials that turn out the way the test scripts them.
pub struct ScriptedConnections {
    real: SshConnectionManager,
    script: Mutex<VecDeque<Dial>>,
    then: Mutex<Dial>,
    profiles: Mutex<Vec<SshProfile>>,
    host_key_answers: Mutex<Vec<bool>>,
}

impl ScriptedConnections {
    /// Dials go through for real unless scripted otherwise.
    pub fn new(real: SshConnectionManager) -> Arc<Self> {
        Arc::new(Self {
            real,
            script: Mutex::new(VecDeque::new()),
            then: Mutex::new(Dial::Real),
            profiles: Mutex::new(Vec::new()),
            host_key_answers: Mutex::new(Vec::new()),
        })
    }

    /// The next dials, in order.
    pub fn script(&self, steps: impl IntoIterator<Item = Dial>) {
        lock(&self.script).extend(steps);
    }

    /// Every dial after the scripted ones.
    pub fn then(&self, step: Dial) {
        *lock(&self.then) = step;
    }

    pub fn calls(&self) -> usize {
        lock(&self.profiles).len()
    }

    pub fn profiles(&self) -> Vec<SshProfile> {
        lock(&self.profiles).clone()
    }

    pub fn host_key_answers(&self) -> Vec<bool> {
        lock(&self.host_key_answers).clone()
    }
}

fn failure(message: &str) -> ConnectFailure {
    ConnectFailure::Connect(ConnectError::other(message))
}

impl ForwardConnections for ScriptedConnections {
    fn acquire(&self, conn_id: &str) -> Option<ConnectionLease> {
        ForwardConnections::acquire(&self.real, conn_id)
    }

    fn connect(
        &self,
        profile: SshProfile,
        io: Arc<dyn ConnectIo>,
    ) -> BoxFuture<'_, Result<ConnectionLease, ConnectFailure>> {
        lock(&self.profiles).push(profile.clone());
        let step = lock(&self.script)
            .pop_front()
            .unwrap_or_else(|| lock(&self.then).clone());
        Box::pin(async move {
            match step {
                Dial::Real => ForwardConnections::connect(&self.real, profile, io).await,
                Dial::Fail(message) => Err(failure(message)),
                Dial::Prompt => {
                    let prompt = AuthPromptInfo {
                        prompts: vec![PromptField {
                            prompt: "Password:".into(),
                            echo: false,
                        }],
                        ..AuthPromptInfo::default()
                    };
                    let _ = io.prompt(prompt).await;
                    Err(failure("authentication cancelled"))
                }
                Dial::HostKey => {
                    let challenge = HostKeyChallenge {
                        host: "db".into(),
                        port: 22,
                        key_type: "ssh-ed25519".into(),
                        fingerprint: "SHA256:x".into(),
                        state: HostKeyState::Mismatch,
                        previous: None,
                        hop: None,
                    };
                    let accepted = io.host_key(challenge).await.unwrap_or(false);
                    lock(&self.host_key_answers).push(accepted);
                    Err(failure("host key rejected"))
                }
            }
        })
    }
}

/// Saved tunnels the test can edit while the manager runs.
#[derive(Clone, Default)]
pub struct SavedTunnels(Arc<Mutex<Vec<TunnelRecord>>>);

impl SavedTunnels {
    pub fn new(tunnels: Vec<TunnelRecord>) -> Self {
        Self(Arc::new(Mutex::new(tunnels)))
    }

    pub fn set(&self, tunnels: Vec<TunnelRecord>) {
        *lock(&self.0) = tunnels;
    }

    pub fn options(&self, delays: &[Duration]) -> ForwardManagerOptions {
        let saved = self.0.clone();
        ForwardManagerOptions {
            tunnel: Some(Arc::new(move |id: &str| {
                lock(&saved).iter().find(|t| t.id == id).cloned()
            })),
            reconnect_delays: delays.to_vec(),
        }
    }
}

/// `tunnel-1`: a dynamic forward on `db` that reconnects.
pub fn saved_tunnel() -> TunnelRecord {
    TunnelRecord {
        id: "tunnel-1".into(),
        name: None,
        target: "db".into(),
        ssh_options: None,
        kind: ForwardType::Dynamic,
        bind_port: 0,
        target_host: None,
        target_port: None,
        auto_start: false,
        auto_reconnect: true,
        created_at: "2026-10-08T00:00:00Z".into(),
        updated_at: "2026-10-08T00:00:00Z".into(),
    }
}

/// Waits (up to ten seconds) until the forward list satisfies `pred`, and returns that list.
pub async fn wait_for(
    manager: &ForwardManager,
    what: &str,
    pred: impl Fn(&[ForwardInfo]) -> bool,
) -> Vec<ForwardInfo> {
    let mut changes = manager.subscribe();
    let reached =
        tokio::time::timeout(Duration::from_secs(10), changes.wait_for(|list| pred(list))).await;
    match reached {
        Ok(Ok(list)) => list.clone(),
        _ => panic!("timed out waiting for {what}: {:?}", manager.list(None)),
    }
}

/// Waits until the first forward has `status`.
pub async fn wait_for_status(manager: &ForwardManager, status: ForwardStatus) -> Vec<ForwardInfo> {
    wait_for(manager, &format!("{status:?}"), |list| {
        list.first().is_some_and(|f| f.status == status)
    })
    .await
}

/// Lets spawned tasks run without letting time pass.
pub async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

/// Moves paused time forward and lets whatever came due run.
pub async fn advance(by: Duration) {
    tokio::time::advance(by).await;
    settle().await;
}
