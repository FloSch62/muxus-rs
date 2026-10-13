//! SSH port forwards over live connections (port of `server/src/forwards/forward-manager.ts`).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use super::listeners::{self, RemoteDispatchers, RemoteRoute, Rule};
use super::types::{
    ConfigForwardStart, ForwardInfo, ForwardLifecycle, ForwardOrigin, ForwardRequest,
    ForwardStatus, ForwardType, TunnelRecord,
};
use crate::Problem;
use crate::connection::io::{
    AuthPromptInfo, AuthPromptResponse, BoxFuture, Cancelled, ConnectIo, HostKeyChallenge,
};
use crate::connection::manager::{
    ConnectFailure, ConnectOptions, ConnectionLease, SshConnectionManager,
};
use crate::connection::{ConnectionLeaseOwner, ManagedConnection, SshProfile};
use muxus_core::protocol::DEFAULT_SSH_KEEPALIVE_INTERVAL_SECONDS;

/// Waits before each redial of a dropped tunnel; the last one repeats.
pub const TUNNEL_RECONNECT_DELAYS: [Duration; 5] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

/// Uptime after which a drop starts over at the shortest wait.
pub const TUNNEL_RECONNECT_STABLE: Duration = Duration::from_secs(30);

/// A redial needed a password, passphrase, 2FA code or host key answer.
pub const NEEDS_SIGN_IN: &str = "Sign-in needed — start the tunnel again";

const CONNECTION_LOST: &str = "The SSH connection was lost.";

/// The connection layer as the forward manager uses it. [`SshConnectionManager`] is the real
/// one; tests wrap it to script how dials turn out.
pub trait ForwardConnections: Send + Sync + 'static {
    /// A forward lease on a live connection, or `None` when there is no such connection.
    fn acquire(&self, conn_id: &str) -> Option<ConnectionLease>;
    /// Dial (or join) a connection for a saved tunnel, holding a forward lease on it.
    fn connect(
        &self,
        profile: SshProfile,
        io: Arc<dyn ConnectIo>,
    ) -> BoxFuture<'_, Result<ConnectionLease, ConnectFailure>>;
}

impl ForwardConnections for SshConnectionManager {
    fn acquire(&self, conn_id: &str) -> Option<ConnectionLease> {
        SshConnectionManager::acquire(self, conn_id, ConnectionLeaseOwner::Forward)
    }

    fn connect(
        &self,
        profile: SshProfile,
        io: Arc<dyn ConnectIo>,
    ) -> BoxFuture<'_, Result<ConnectionLease, ConnectFailure>> {
        Box::pin(async move {
            let muxed = SshConnectionManager::connect(
                self,
                &profile,
                io,
                ConnectionLeaseOwner::Forward,
                ConnectOptions::default(),
            )
            .await?;
            Ok(muxed.lease)
        })
    }
}

/// Saved tunnel definitions by id (the database's `tunnel(id)`). It must not call back into the
/// [`ForwardManager`].
pub type TunnelLookup = Arc<dyn Fn(&str) -> Option<TunnelRecord> + Send + Sync>;

/// What the manager needs besides the connections.
#[derive(Clone)]
pub struct ForwardManagerOptions {
    /// Saved tunnel definition, read again whenever a tunnel loses its connection.
    pub tunnel: Option<TunnelLookup>,
    /// Test seam; production uses [`TUNNEL_RECONNECT_DELAYS`].
    pub reconnect_delays: Vec<Duration>,
}

impl Default for ForwardManagerOptions {
    fn default() -> Self {
        Self {
            tunnel: None,
            reconnect_delays: TUNNEL_RECONNECT_DELAYS.to_vec(),
        }
    }
}

impl ForwardManagerOptions {
    /// Default delays, with saved tunnels from `lookup`.
    pub fn with_tunnels(
        lookup: impl Fn(&str) -> Option<TunnelRecord> + Send + Sync + 'static,
    ) -> Self {
        Self {
            tunnel: Some(Arc::new(lookup)),
            ..Self::default()
        }
    }
}

/// Why the manager dials a saved tunnel on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TunnelDial {
    Autostart,
    Reconnect,
}

impl TunnelDial {
    fn as_str(self) -> &'static str {
        match self {
            TunnelDial::Autostart => "autostart",
            TunnelDial::Reconnect => "reconnect",
        }
    }
}

/// Cancels a scheduled or in-flight dial. A dial that is already talking to the server runs to
/// its end and then lets go of the connection, so the connection layer never sees a dial
/// abandoned halfway.
#[derive(Clone)]
struct DialCancel(Arc<watch::Sender<bool>>);

impl DialCancel {
    fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    fn cancel(&self) {
        self.0.send_replace(true);
    }

    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }
}

/// A rule that is up on a connection. Dropping it closes the listener (or the server's), stops
/// watching the connection and releases the connection lease.
struct Binding {
    lease: ConnectionLease,
    listener: Option<AbortHandle>,
    remote: Option<RemoteRoute>,
    watcher: Option<AbortHandle>,
}

impl Binding {
    fn connection(&self) -> &Arc<ManagedConnection> {
        self.lease.connection()
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
        }
        if let Some(listener) = self.listener.take() {
            listener.abort();
        }
        drop(self.remote.take());
        self.lease.release();
    }
}

struct ActiveForward {
    /// Tells this entry apart from any later one (ids are random, but a stale task must never
    /// act on a different entry).
    serial: u64,
    info: ForwardInfo,
    binding: Option<Binding>,
    /// A scheduled or in-flight dial.
    reconnect: Option<DialCancel>,
    /// When the listener last came up on a connection.
    bound_at: Instant,
    /// Redials since the last stable connection.
    attempts: usize,
    /// The window's keepalive fallback that the original dial carried.
    keepalive_interval_seconds: Option<u32>,
}

impl ActiveForward {
    fn new(serial: u64, info: ForwardInfo) -> Self {
        Self {
            serial,
            info,
            binding: None,
            reconnect: None,
            bound_at: Instant::now(),
            attempts: 0,
            keepalive_interval_seconds: None,
        }
    }

    fn rule(&self) -> Rule {
        Rule {
            id: self.info.id.clone(),
            bind_port: self.info.bind_port,
            target_host: self.info.target_host.clone().unwrap_or_default(),
            target_port: self.info.target_port.unwrap_or_default(),
        }
    }
}

impl Drop for ActiveForward {
    /// Removing a forward discards it: no more dials, no listener, no lease.
    fn drop(&mut self) {
        if let Some(cancel) = self.reconnect.take() {
            cancel.cancel();
        }
    }
}

/// One rule on one connection, for deduplicating config forwards.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConfigKey {
    conn_id: String,
    kind: ForwardType,
    bind_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
}

impl ConfigKey {
    fn of_request(req: &ForwardRequest) -> Self {
        Self {
            conn_id: req.conn_id.clone(),
            kind: req.kind,
            bind_port: req.bind_port,
            target_host: req.target_host.clone(),
            target_port: req.target_port,
        }
    }

    fn of_info(info: &ForwardInfo) -> Self {
        Self {
            conn_id: info.conn_id.clone(),
            kind: info.kind,
            bind_port: info.bind_port,
            target_host: info.target_host.clone(),
            target_port: info.target_port,
        }
    }
}

type ConfigOutcome = Option<Result<ForwardInfo, Problem>>;

#[derive(Default)]
struct State {
    /// In start order, like the TypeScript Map.
    forwards: Vec<ActiveForward>,
    /// Config forwards currently being created, keyed by connection and rule.
    pending_config: HashMap<ConfigKey, watch::Receiver<ConfigOutcome>>,
    next_serial: u64,
}

impl State {
    fn serial(&mut self) -> u64 {
        self.next_serial += 1;
        self.next_serial
    }

    fn find_mut(&mut self, id: &str, serial: u64) -> Option<&mut ActiveForward> {
        self.forwards
            .iter_mut()
            .find(|f| f.serial == serial && f.info.id == id)
    }

    fn contains(&self, id: &str, serial: u64) -> bool {
        self.forwards
            .iter()
            .any(|f| f.serial == serial && f.info.id == id)
    }

    fn remove(&mut self, id: &str, serial: u64) -> Option<ActiveForward> {
        let index = self
            .forwards
            .iter()
            .position(|f| f.serial == serial && f.info.id == id)?;
        Some(self.forwards.remove(index))
    }

    fn list(&self) -> Vec<ForwardInfo> {
        self.forwards.iter().map(|f| f.info.clone()).collect()
    }
}

struct Shared {
    connections: Arc<dyn ForwardConnections>,
    options: ForwardManagerOptions,
    state: Mutex<State>,
    remote: RemoteDispatchers,
    changes: watch::Sender<Vec<ForwardInfo>>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tunnel(&self, id: &str) -> Option<TunnelRecord> {
        self.options.tunnel.as_ref().and_then(|lookup| lookup(id))
    }

    /// Tell subscribers about the current list when it changed.
    fn publish(&self, state: &State) {
        let next = state.list();
        self.changes.send_if_modified(|current| {
            if *current == next {
                return false;
            }
            *current = next;
            true
        });
    }
}

/// SSH port forwards over live connections: local (-L), remote (-R) and dynamic (-D, a minimal
/// no-auth SOCKS5 CONNECT server). All listeners bind 127.0.0.1 only — Muxus is a local
/// single-user tool.
///
/// Saved tunnels can also be dialed without anyone at the keyboard: at startup when they start
/// with Muxus, and after a drop when they reconnect. Those dials never prompt (agent, keys and
/// vault passwords only). A reconnecting tunnel keeps its entry while the connection is down: the
/// listener closes and the rule starts again on the new connection under the same forward id.
///
/// Every change to the list (a forward started, stopped, reconnecting, given up, active again) is
/// published to [`subscribe`](Self::subscribe) receivers. The manager spawns tokio tasks, so its
/// methods must run inside a tokio runtime.
#[derive(Clone)]
pub struct ForwardManager {
    shared: Arc<Shared>,
}

impl ForwardManager {
    /// Forwards over the connections of `connections`.
    pub fn new(connections: SshConnectionManager, options: ForwardManagerOptions) -> Self {
        Self::with_connections(Arc::new(connections), options)
    }

    /// Forwards over any connection layer (tests script dials through this).
    pub fn with_connections(
        connections: Arc<dyn ForwardConnections>,
        options: ForwardManagerOptions,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                connections,
                options,
                state: Mutex::new(State::default()),
                remote: RemoteDispatchers::default(),
                changes: watch::channel(Vec::new()).0,
            }),
        }
    }

    /// Every forward, or those on one connection, in start order.
    pub fn list(&self, conn_id: Option<&str>) -> Vec<ForwardInfo> {
        let conn_id = conn_id.filter(|id| !id.is_empty());
        let state = self.shared.state();
        state
            .forwards
            .iter()
            .map(|f| &f.info)
            .filter(|info| conn_id.is_none_or(|id| info.conn_id == id))
            .cloned()
            .collect()
    }

    /// The forward list, republished after every change (status, error, connection, start and
    /// stop). The receiver always holds the latest list.
    pub fn subscribe(&self) -> watch::Receiver<Vec<ForwardInfo>> {
        self.shared.changes.subscribe()
    }

    /// Start a forward the user asked for (`POST /api/forwards`).
    pub async fn start(&self, req: ForwardRequest) -> Result<ForwardInfo, Problem> {
        self.start_with_origin(req, ForwardOrigin::Manual).await
    }

    async fn start_with_origin(
        &self,
        req: ForwardRequest,
        origin: ForwardOrigin,
    ) -> Result<ForwardInfo, Problem> {
        let missing_target = req.target_host.as_deref().is_none_or(str::is_empty)
            || req.target_port.is_none_or(|p| p == 0);
        if req.kind != ForwardType::Dynamic && missing_target {
            return Err(Problem::bad_request(
                "targetHost and targetPort are required for local/remote forwards",
            ));
        }
        let shared = &self.shared;
        // Starting a tunnel by hand takes over from a dial the manager is making for it.
        if let Some(tunnel_id) = &req.tunnel_id {
            self.stop_pending(tunnel_id);
        }
        let lease = shared
            .connections
            .acquire(&req.conn_id)
            .ok_or_else(|| Problem::not_found("connection not found"))?;
        let info = ForwardInfo {
            id: new_forward_id(),
            conn_id: req.conn_id.clone(),
            kind: req.kind,
            bind_port: req.bind_port,
            target_host: req.target_host.clone(),
            target_port: req.target_port,
            origin,
            lifecycle: if req.tunnel_id.is_some() {
                ForwardLifecycle::Independent
            } else {
                ForwardLifecycle::Session
            },
            status: ForwardStatus::Active,
            error: None,
            tunnel_id: req.tunnel_id.clone(),
        };
        // Listed before the listener is up so a stop while binding finds the entry it has to
        // remove.
        let (serial, rule) = {
            let mut state = shared.state();
            let serial = state.serial();
            let entry = ActiveForward::new(serial, info.clone());
            let rule = entry.rule();
            state.forwards.push(entry);
            shared.publish(&state);
            (serial, rule)
        };
        let binding = match bind(shared, info.kind, rule, lease).await {
            Ok(binding) => binding,
            Err(err) => {
                tracing::warn!(
                    conn_id = %info.conn_id, kind = ?info.kind, bind_port = info.bind_port,
                    err = %err.message, "forward could not start"
                );
                let mut state = shared.state();
                if state.remove(&info.id, serial).is_some() {
                    shared.publish(&state);
                }
                return Err(err);
            }
        };
        let mut state = shared.state();
        let Some(entry) = state.find_mut(&info.id, serial) else {
            drop(state);
            drop(binding);
            return Err(Problem::new(
                409,
                "the forward was stopped while it started",
            ));
        };
        install(shared, entry, binding);
        let info = entry.info.clone();
        shared.publish(&state);
        let target = match (&info.target_host, info.target_port) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            _ => "socks".to_string(),
        };
        tracing::info!(
            forward = %info.id, conn_id = %info.conn_id, kind = ?info.kind,
            bind_port = info.bind_port, target = %target,
            tunnel = info.tunnel_id.as_deref().unwrap_or(""), "forward started"
        );
        Ok(info)
    }

    /// Start an ssh-config forward once per connection and rule. Every terminal on a multiplexed
    /// transport runs its resolved config through this method, so aliases can add distinct rules
    /// while repeated/concurrent sessions do not race into duplicate listener binds.
    pub async fn start_config(&self, req: ForwardRequest) -> Result<ConfigForwardStart, Problem> {
        let key = ConfigKey::of_request(&req);
        loop {
            let (tx, rx) = {
                let mut state = self.shared.state();
                let existing = state.forwards.iter().find(|f| {
                    f.info.origin == ForwardOrigin::Config
                        && f.info.lifecycle == ForwardLifecycle::Session
                        && ConfigKey::of_info(&f.info) == key
                });
                if let Some(existing) = existing {
                    return Ok(ConfigForwardStart {
                        info: existing.info.clone(),
                        started: false,
                    });
                }
                match state.pending_config.get(&key) {
                    Some(pending) => (None, pending.clone()),
                    None => {
                        let (tx, rx) = watch::channel(None);
                        state.pending_config.insert(key.clone(), rx.clone());
                        (Some(tx), rx)
                    }
                }
            };
            let Some(tx) = tx else {
                let mut pending = rx;
                match pending.wait_for(Option::is_some).await {
                    Ok(outcome) => {
                        let outcome = outcome
                            .clone()
                            .unwrap_or_else(|| Err(Problem::internal("forward start vanished")));
                        return outcome.map(|info| ConfigForwardStart {
                            info,
                            started: false,
                        });
                    }
                    // The start was abandoned before it finished; try again.
                    Err(_) => continue,
                }
            };
            let _clear = ClearPendingConfig {
                shared: self.shared.clone(),
                key: key.clone(),
                pending: rx,
            };
            let outcome = self
                .start_with_origin(req.clone(), ForwardOrigin::Config)
                .await;
            tx.send_replace(Some(outcome.clone()));
            return outcome.map(|info| ConfigForwardStart {
                info,
                started: true,
            });
        }
    }

    /// Stop a forward (or a pending start or redial) and forget it.
    pub fn stop(&self, id: &str) {
        let mut state = self.shared.state();
        let Some(index) = state.forwards.iter().position(|f| f.info.id == id) else {
            return;
        };
        let stopped = state.forwards.remove(index);
        tracing::info!(
            forward = %stopped.info.id, conn_id = %stopped.info.conn_id,
            bind_port = stopped.info.bind_port,
            tunnel = stopped.info.tunnel_id.as_deref().unwrap_or(""), "forward stopped"
        );
        drop(stopped);
        self.shared.publish(&state);
    }

    /// Adopt a running forward into a saved tunnel (no restart needed).
    pub fn assign_tunnel(&self, id: &str, tunnel_id: &str) -> Option<ForwardInfo> {
        let mut state = self.shared.state();
        let entry = state.forwards.iter_mut().find(|f| f.info.id == id)?;
        entry.info.tunnel_id = Some(tunnel_id.to_string());
        entry.info.lifecycle = ForwardLifecycle::Independent;
        let info = entry.info.clone();
        self.shared.publish(&state);
        Some(info)
    }

    /// Start the saved tunnels marked to start with Muxus, without prompting. Each shows as
    /// `starting` until its first connection is up.
    pub fn autostart(&self, tunnels: &[TunnelRecord]) {
        let shared = &self.shared;
        let mut state = shared.state();
        let present: HashSet<String> = state
            .forwards
            .iter()
            .filter_map(|f| f.info.tunnel_id.clone())
            .collect();
        for tunnel in tunnels {
            if !tunnel.auto_start || present.contains(&tunnel.id) {
                continue;
            }
            let serial = state.serial();
            let mut entry = ActiveForward::new(
                serial,
                ForwardInfo {
                    id: new_forward_id(),
                    conn_id: String::new(),
                    kind: tunnel.kind,
                    bind_port: tunnel.bind_port,
                    target_host: tunnel.target_host.clone(),
                    target_port: tunnel.target_port,
                    origin: ForwardOrigin::Manual,
                    lifecycle: ForwardLifecycle::Independent,
                    status: ForwardStatus::Starting,
                    error: None,
                    tunnel_id: Some(tunnel.id.clone()),
                },
            );
            // No window has supplied the preference yet; later redials keep this.
            entry.keepalive_interval_seconds = Some(DEFAULT_SSH_KEEPALIVE_INTERVAL_SECONDS);
            let cancel = DialCancel::new();
            entry.reconnect = Some(cancel.clone());
            spawn_dial(
                shared,
                entry.info.id.clone(),
                serial,
                TunnelDial::Autostart,
                Duration::ZERO,
                cancel,
            );
            state.forwards.push(entry);
        }
        shared.publish(&state);
    }

    /// Drop a pending start or redial that the saved tunnel no longer asks for, or every one left
    /// behind by a deleted tunnel. Running forwards stay.
    pub fn tunnel_changed(&self, tunnel_id: &str) {
        let tunnel = self.shared.tunnel(tunnel_id);
        let mut state = self.shared.state();
        let before = state.forwards.len();
        state.forwards.retain(|f| {
            if f.info.tunnel_id.as_deref() != Some(tunnel_id)
                || f.info.status == ForwardStatus::Active
            {
                return true;
            }
            tunnel.as_ref().is_some_and(|tunnel| match f.info.status {
                ForwardStatus::Error => true,
                ForwardStatus::Starting => tunnel.auto_start,
                _ => tunnel.auto_reconnect,
            })
        });
        if state.forwards.len() != before {
            self.shared.publish(&state);
        }
    }

    /// Stop forwards owned by the terminal session while preserving saved/manual tunnels.
    pub fn stop_session_for_connection(&self, conn_id: &str) {
        let mut state = self.shared.state();
        let before = state.forwards.len();
        state.forwards.retain(|f| {
            !(f.info.conn_id == conn_id && f.info.lifecycle == ForwardLifecycle::Session)
        });
        if state.forwards.len() != before {
            self.shared.publish(&state);
        }
    }

    /// Stop everything (application shutdown).
    pub fn stop_all(&self) {
        let mut state = self.shared.state();
        state.forwards.clear();
        self.shared.publish(&state);
    }

    /// Stop the tunnel's forwards that are starting, waiting to reconnect or gave up.
    fn stop_pending(&self, tunnel_id: &str) {
        let mut state = self.shared.state();
        let before = state.forwards.len();
        state.forwards.retain(|f| {
            f.info.tunnel_id.as_deref() != Some(tunnel_id) || f.info.status == ForwardStatus::Active
        });
        if state.forwards.len() != before {
            self.shared.publish(&state);
        }
    }
}

/// Forgets a config start once it settled (or was abandoned), unless a newer one took its place.
struct ClearPendingConfig {
    shared: Arc<Shared>,
    key: ConfigKey,
    pending: watch::Receiver<ConfigOutcome>,
}

impl Drop for ClearPendingConfig {
    fn drop(&mut self) {
        let mut state = self.shared.state();
        if state
            .pending_config
            .get(&self.key)
            .is_some_and(|p| p.same_channel(&self.pending))
        {
            state.pending_config.remove(&self.key);
        }
    }
}

/// Bring the rule up on the lease's connection; the lease is released on failure.
async fn bind(
    shared: &Shared,
    kind: ForwardType,
    rule: Rule,
    lease: ConnectionLease,
) -> Result<Binding, Problem> {
    let conn = lease.connection().clone();
    let mut binding = Binding {
        lease,
        listener: None,
        remote: None,
        watcher: None,
    };
    match kind {
        ForwardType::Local => binding.listener = Some(listeners::start_local(conn, rule).await?),
        ForwardType::Dynamic => {
            binding.listener = Some(listeners::start_dynamic(conn, rule).await?)
        }
        ForwardType::Remote => {
            binding.remote = Some(listeners::start_remote(&shared.remote, conn, rule).await?)
        }
    }
    Ok(binding)
}

/// Make a bound rule the entry's, and react when its connection goes away.
fn install(shared: &Arc<Shared>, entry: &mut ActiveForward, mut binding: Binding) {
    let conn = binding.connection().clone();
    entry.info.conn_id = conn.id.clone();
    entry.bound_at = Instant::now();
    entry.keepalive_interval_seconds = conn.profile.keepalive_interval_seconds;
    let weak = Arc::downgrade(shared);
    let (id, serial) = (entry.info.id.clone(), entry.serial);
    binding.watcher = Some(
        tokio::spawn(async move {
            let reason = conn.closed().await;
            if let Some(shared) = weak.upgrade() {
                connection_lost(&shared, &id, serial, &conn, reason);
            }
        })
        .abort_handle(),
    );
    entry.binding = Some(binding);
}

fn connection_lost(
    shared: &Arc<Shared>,
    id: &str,
    serial: u64,
    conn: &Arc<ManagedConnection>,
    reason: Option<String>,
) {
    let tunnel_id = {
        let mut state = shared.state();
        let Some(entry) = state.find_mut(id, serial) else {
            return;
        };
        if !entry
            .binding
            .as_ref()
            .is_some_and(|b| Arc::ptr_eq(b.connection(), conn))
        {
            return;
        }
        entry.binding = None;
        entry.info.tunnel_id.clone()
    };
    // Read outside the lock: the lookup is the caller's code.
    let tunnel = tunnel_id.as_deref().and_then(|t| shared.tunnel(t));
    let mut state = shared.state();
    if !state.contains(id, serial) {
        return;
    }
    if !tunnel.is_some_and(|t| t.auto_reconnect) {
        state.remove(id, serial);
        shared.publish(&state);
        return;
    }
    let Some(entry) = state.find_mut(id, serial) else {
        return;
    };
    if entry.bound_at.elapsed() >= TUNNEL_RECONNECT_STABLE {
        entry.attempts = 0;
    }
    tracing::info!(
        forward = %id, tunnel = ?entry.info.tunnel_id, ?reason,
        "tunnel connection lost; reconnecting"
    );
    schedule_reconnect(
        shared,
        entry,
        reason.unwrap_or_else(|| CONNECTION_LOST.to_string()),
    );
    shared.publish(&state);
}

fn schedule_reconnect(shared: &Arc<Shared>, entry: &mut ActiveForward, error: String) {
    let delays = &shared.options.reconnect_delays;
    let delay = delays
        .get(entry.attempts.min(delays.len().saturating_sub(1)))
        .copied()
        .unwrap_or_default();
    entry.attempts += 1;
    entry.info.status = ForwardStatus::Reconnecting;
    entry.info.error = Some(error);
    let cancel = DialCancel::new();
    entry.reconnect = Some(cancel.clone());
    spawn_dial(
        shared,
        entry.info.id.clone(),
        entry.serial,
        TunnelDial::Reconnect,
        delay,
        cancel,
    );
}

fn spawn_dial(
    shared: &Arc<Shared>,
    id: String,
    serial: u64,
    why: TunnelDial,
    delay: Duration,
    cancel: DialCancel,
) {
    let weak = Arc::downgrade(shared);
    tokio::spawn(async move {
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = cancel.cancelled() => return,
        }
        dial(weak, id, serial, why, cancel).await;
    });
}

/// Dial a saved tunnel without prompts and start its rule on the connection.
async fn dial(weak: Weak<Shared>, id: String, serial: u64, why: TunnelDial, cancel: DialCancel) {
    let Some(shared) = weak.upgrade() else { return };
    let live = |state: &State| !cancel.is_cancelled() && state.contains(&id, serial);
    let tunnel_id = {
        let state = shared.state();
        match state
            .forwards
            .iter()
            .find(|f| f.serial == serial && f.info.id == id)
        {
            Some(entry) if !cancel.is_cancelled() => entry.info.tunnel_id.clone(),
            _ => return,
        }
    };
    let tunnel = tunnel_id.as_deref().and_then(|t| shared.tunnel(t));
    let wanted = |t: &TunnelRecord| match why {
        TunnelDial::Autostart => t.auto_start,
        TunnelDial::Reconnect => t.auto_reconnect,
    };
    let Some(tunnel) = tunnel.filter(wanted) else {
        // Deleted, or switched off while waiting.
        let mut state = shared.state();
        if state.remove(&id, serial).is_some() {
            shared.publish(&state);
        }
        return;
    };
    // The saved definition wins, just as when the tunnel is started again.
    let keepalive = {
        let mut state = shared.state();
        let Some(entry) = state.find_mut(&id, serial) else {
            return;
        };
        entry.info.kind = tunnel.kind;
        entry.info.bind_port = tunnel.bind_port;
        entry.info.target_host = tunnel.target_host.clone();
        entry.info.target_port = tunnel.target_port;
        let keepalive = entry.keepalive_interval_seconds;
        shared.publish(&state);
        keepalive
    };

    // Nobody is there to answer: any prompt or host key question ends the attempt, and only the
    // user can take it from there.
    let io = Arc::new(UnattendedIo::default());
    let connected = shared
        .connections
        .connect(tunnel_profile(&tunnel, keepalive), io.clone())
        .await;
    let lease = match connected {
        Ok(lease) => lease,
        Err(err) => {
            let mut state = shared.state();
            if !live(&state) {
                return;
            }
            let Some(entry) = state.find_mut(&id, serial) else {
                return;
            };
            if io.needs_user() {
                tracing::warn!(
                    forward = %id, tunnel = %tunnel.id, why = why.as_str(),
                    "tunnel dial needs user input; giving up"
                );
                give_up(entry, NEEDS_SIGN_IN.to_string());
            } else {
                retry_or_give_up(&shared, entry, &tunnel, err.to_string(), why);
            }
            shared.publish(&state);
            return;
        }
    };
    let rule = {
        let state = shared.state();
        if !live(&state) {
            drop(state);
            lease.release();
            return;
        }
        let Some(entry) = state.forwards.iter().find(|f| f.serial == serial) else {
            return;
        };
        entry.rule()
    };
    let binding = match bind(&shared, tunnel.kind, rule, lease).await {
        Ok(binding) => binding,
        Err(err) => {
            let mut state = shared.state();
            if !live(&state) {
                return;
            }
            let Some(entry) = state.find_mut(&id, serial) else {
                return;
            };
            // A remote port can stay taken until the server notices the old connection is gone,
            // so a failed bind is worth another try too.
            retry_or_give_up(&shared, entry, &tunnel, err.message, why);
            shared.publish(&state);
            return;
        }
    };
    let mut state = shared.state();
    if !live(&state) {
        drop(state);
        drop(binding);
        return;
    }
    let Some(entry) = state.find_mut(&id, serial) else {
        return;
    };
    install(&shared, entry, binding);
    entry.reconnect = None;
    entry.info.status = ForwardStatus::Active;
    entry.info.error = None;
    tracing::info!(
        forward = %id, tunnel = %tunnel.id, conn_id = %entry.info.conn_id, why = why.as_str(),
        "tunnel connected"
    );
    shared.publish(&state);
}

/// A tunnel that reconnects tries again, also after a failed start; others wait for the user.
fn retry_or_give_up(
    shared: &Arc<Shared>,
    entry: &mut ActiveForward,
    tunnel: &TunnelRecord,
    error: String,
    why: TunnelDial,
) {
    if tunnel.auto_reconnect {
        tracing::info!(
            err = %error, forward = %entry.info.id, tunnel = %tunnel.id, why = why.as_str(),
            "tunnel dial failed; retrying"
        );
        schedule_reconnect(shared, entry, error);
        return;
    }
    tracing::warn!(
        err = %error, forward = %entry.info.id, tunnel = %tunnel.id, why = why.as_str(),
        "tunnel dial failed"
    );
    give_up(entry, error);
}

/// Keep the entry, with its reason, until the user starts or dismisses it.
fn give_up(entry: &mut ActiveForward, error: String) {
    entry.reconnect = None;
    entry.info.status = ForwardStatus::Error;
    entry.info.error = Some(error);
}

/// Answers nothing: a dial nobody watches fails at the first question, and remembers that it
/// was asked one.
#[derive(Default)]
struct UnattendedIo {
    needs_user: AtomicBool,
}

impl UnattendedIo {
    fn needs_user(&self) -> bool {
        self.needs_user.load(Ordering::SeqCst)
    }
}

impl ConnectIo for UnattendedIo {
    fn status(&self, _message: &str, _transient: bool) {}

    fn prompt(
        &self,
        _info: AuthPromptInfo,
    ) -> BoxFuture<'_, Result<AuthPromptResponse, Cancelled>> {
        self.needs_user.store(true, Ordering::SeqCst);
        Box::pin(async { Err(Cancelled) })
    }

    fn host_key(&self, _challenge: HostKeyChallenge) -> BoxFuture<'_, Result<bool, Cancelled>> {
        self.needs_user.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(false) })
    }
}

/// The dial a saved tunnel's start makes (see the client's `dialConnection`): a tunnel-owned
/// profile skips the alias's config, and the keepalive fallback travels along.
pub fn tunnel_profile(
    tunnel: &TunnelRecord,
    keepalive_interval_seconds: Option<u32>,
) -> SshProfile {
    let mut profile = SshProfile::new(tunnel.target.clone());
    if let Some(options) = &tunnel.ssh_options {
        profile.use_config = Some(false);
        profile.user = options.user.clone();
        profile.port = options.port;
        profile.identity_files = options.identity_files.clone();
        profile.identities_only = options.identities_only;
        profile.forward_agent = options.forward_agent;
        profile.proxy_jump = options.proxy_jump.clone();
        profile.password_only = options.password_only;
    }
    profile.keepalive_interval_seconds = keepalive_interval_seconds;
    profile
}

/// An 8-character id from nanoid's URL-safe alphabet.
fn new_forward_id() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"useandom-26T198340PX75pxJACKVERYMINDBUSHWOLF_GQZbfghjklqvwyzrict";
    let mut rng = rand::thread_rng();
    (0..8)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}
