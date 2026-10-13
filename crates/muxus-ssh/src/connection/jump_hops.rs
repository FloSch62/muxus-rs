//! Authenticated ProxyJump hops shared by every chain routed through them, the way OpenSSH's
//! ControlMaster lets a second `ssh -J` ride the first one's jump connection. A host that refuses
//! a second session on one connection (Cisco IOS and many other appliances) then still gets a
//! dedicated connection for a duplicated tab without another login on the jump host.
//!
//! Each hop holds a reference on the hop before it; a target transport holds one on the last hop
//! of its route. The connection ends shortly after its last reference, and its loss is reported
//! to everything routed through it.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::sync::watch;

use super::health::TransportHealth;
use super::transport::HopTransport;

/// How long a jump hop outlives its last chain. A chain replaced right away (the plain-console
/// redial after a console server dropped the first transport, a reconnect) then rides the same
/// login.
pub const JUMP_HOP_IDLE: Duration = Duration::from_secs(5);

struct HopState {
    refs: i64,
    ending: bool,
    closed: bool,
    parent_released: bool,
    superseded: bool,
    idle_timer: Option<tokio::task::AbortHandle>,
}

pub struct JumpHop {
    /// Dial-plan identity of the route up to and including this hop.
    pub key: String,
    /// What the user wrote for this hop ("bastion"), for status and close reasons.
    pub label: String,
    pub transport: Arc<HopTransport>,
    pub parent: Option<Arc<JumpHop>>,
    /// Force-reconnect group that dialed this hop.
    pub replacement_token: Option<String>,
    state: Mutex<HopState>,
    health: watch::Receiver<TransportHealth>,
    closed: watch::Receiver<bool>,
    idle: Duration,
    registry: Weak<RegistryInner>,
    me: Weak<JumpHop>,
}

impl std::fmt::Debug for JumpHop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JumpHop")
            .field("key", &self.key)
            .field("label", &self.label)
            .finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl JumpHop {
    /// Live, not winding down, and answering keepalives.
    pub fn usable(&self) -> bool {
        let s = lock(&self.state);
        !s.closed && !s.ending && !s.superseded && *self.health.borrow() == TransportHealth::Healthy
    }

    pub fn is_closed(&self) -> bool {
        lock(&self.state).closed
    }

    pub fn superseded(&self) -> bool {
        lock(&self.state).superseded
    }

    /// Replaced by a forced fresh route or failed a reuse: never handed out again.
    pub fn mark_superseded(&self) {
        lock(&self.state).superseded = true;
    }

    pub fn health(&self) -> TransportHealth {
        *self.health.borrow()
    }

    /// A receiver of this hop's health.
    pub fn subscribe_health(&self) -> watch::Receiver<TransportHealth> {
        self.health.clone()
    }

    /// Completes when the hop's connection has closed.
    pub async fn closed(&self) {
        let mut rx = self.closed.clone();
        let _ = rx.wait_for(|c| *c).await;
    }

    /// This hop and every hop before it, nearest the target first.
    pub fn route(self: &Arc<Self>) -> Vec<Arc<JumpHop>> {
        let mut hops = vec![self.clone()];
        let mut hop = self.parent.clone();
        while let Some(h) = hop {
            hop = h.parent.clone();
            hops.push(h);
        }
        hops
    }

    pub fn retain(&self) {
        let mut s = lock(&self.state);
        s.refs += 1;
        if let Some(timer) = s.idle_timer.take() {
            timer.abort();
        }
    }

    pub fn release(&self) {
        let mut s = lock(&self.state);
        s.refs -= 1;
        if s.refs > 0 || s.ending || s.closed || s.idle_timer.is_some() {
            return;
        }
        let me = self.me.clone();
        let idle = self.idle;
        let task = tokio::spawn(async move {
            tokio::time::sleep(idle).await;
            if let Some(hop) = me.upgrade() {
                let expired = {
                    let mut s = lock(&hop.state);
                    s.idle_timer = None;
                    s.refs <= 0
                };
                if expired {
                    hop.close();
                }
            }
        });
        s.idle_timer = Some(task.abort_handle());
    }

    /// Close regardless of references (idle expiry, app shutdown).
    pub fn close(&self) {
        {
            let mut s = lock(&self.state);
            if let Some(timer) = s.idle_timer.take() {
                timer.abort();
            }
            s.ending = true;
        }
        self.transport.close();
        self.release_parent();
    }

    fn mark_closed(&self) {
        {
            let mut s = lock(&self.state);
            if s.closed {
                return;
            }
            s.closed = true;
            if let Some(timer) = s.idle_timer.take() {
                timer.abort();
            }
        }
        if let Some(registry) = self.registry.upgrade() {
            lock(&registry.hops).retain(|h| !std::ptr::eq(Arc::as_ptr(h), self));
        }
        self.release_parent();
    }

    fn release_parent(&self) {
        let release = {
            let mut s = lock(&self.state);
            !std::mem::replace(&mut s.parent_released, true)
        };
        if release && let Some(parent) = &self.parent {
            parent.release();
        }
    }
}

struct RegistryInner {
    hops: Mutex<Vec<Arc<JumpHop>>>,
}

/// Live jump hops by route, for chains dialed after them.
pub struct JumpHopRegistry {
    inner: Arc<RegistryInner>,
    idle: Duration,
}

/// What a newly dialed hop is.
pub struct JumpHopOptions {
    pub key: String,
    pub label: String,
    pub transport: Arc<HopTransport>,
    pub parent: Option<Arc<JumpHop>>,
    pub replacement_token: Option<String>,
}

impl JumpHopRegistry {
    pub fn new(idle: Duration) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                hops: Mutex::new(Vec::new()),
            }),
            idle,
        }
    }

    /// Newest usable hop for a route. A force-reconnect request rides only the hops its own
    /// gesture dialed, never the ones it is replacing.
    pub fn reusable(&self, key: &str, fresh_token: Option<&str>) -> Option<Arc<JumpHop>> {
        lock(&self.inner.hops)
            .iter()
            .rfind(|hop| {
                hop.key == key && hop.usable() && {
                    fresh_token.is_none_or(|token| hop.replacement_token.as_deref() == Some(token))
                }
            })
            .cloned()
    }

    pub fn add(&self, options: JumpHopOptions) -> Arc<JumpHop> {
        if let Some(token) = &options.replacement_token {
            // Routes dialed before a force reconnect keep serving the transports already on
            // them, but no later chain should land on them.
            for hop in lock(&self.inner.hops).iter() {
                if hop.key == options.key && hop.replacement_token.as_deref() != Some(token) {
                    hop.mark_superseded();
                }
            }
        }
        if let Some(parent) = &options.parent {
            parent.retain();
        }
        let (health_tx, health_rx) = watch::channel(TransportHealth::Healthy);
        let (closed_tx, closed_rx) = watch::channel(false);
        let transport = options.transport.clone();
        let hop = Arc::new_cyclic(|me| JumpHop {
            key: options.key,
            label: options.label,
            transport: options.transport,
            parent: options.parent,
            replacement_token: options.replacement_token,
            state: Mutex::new(HopState {
                refs: 0,
                ending: false,
                closed: false,
                parent_released: false,
                superseded: false,
                idle_timer: None,
            }),
            health: health_rx,
            closed: closed_rx,
            idle: self.idle,
            registry: Arc::downgrade(&self.inner),
            me: me.clone(),
        });
        if let Some(mut health) = transport.watch_health() {
            let tx = health_tx.clone();
            tokio::spawn(async move {
                while health.changed().await.is_ok() {
                    let state = *health.borrow_and_update();
                    if tx.send(state).is_err() {
                        return;
                    }
                }
            });
        }
        let weak = Arc::downgrade(&hop);
        tokio::spawn(async move {
            transport.closed().await;
            if let Some(hop) = weak.upgrade() {
                hop.mark_closed();
            }
            let _ = closed_tx.send(true);
            drop(health_tx);
        });
        lock(&self.inner.hops).push(hop.clone());
        hop
    }

    pub fn close_all(&self) {
        let hops: Vec<Arc<JumpHop>> = lock(&self.inner.hops).clone();
        for hop in hops {
            hop.close();
        }
    }
}
