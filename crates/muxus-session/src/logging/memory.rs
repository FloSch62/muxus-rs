//! An in-memory [`HistorySink`], for tests and for running without a store.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use muxus_core::api::{SessionEndStatus, SessionLogCreateInput, SessionLogStatus};

use super::recorder::{FailureSubscription, HistoryEvent, HistorySink, PartLimits};

/// One session as the memory store holds it.
#[derive(Debug, Clone)]
pub struct MemorySession {
    pub input: SessionLogCreateInput,
    pub title: String,
    pub status: SessionLogStatus,
    pub ended_at: Option<String>,
    pub paused: bool,
    pub capture_input: bool,
    pub events: Vec<HistoryEvent>,
}

impl MemorySession {
    /// Every event's normalized text, in order.
    pub fn transcript(&self) -> String {
        self.events
            .iter()
            .map(|event| event.text.as_str())
            .collect()
    }
}

type Listener = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Default)]
struct Store {
    sessions: HashMap<String, MemorySession>,
    order: Vec<String>,
    listeners: HashMap<String, Vec<(u64, Listener)>>,
}

/// Keeps every session in memory. `refuse_appends` simulates a full write queue.
#[derive(Default, Clone)]
pub struct MemoryHistory {
    store: Arc<Mutex<Store>>,
    next_id: Arc<AtomicU64>,
    refuse: Arc<AtomicBool>,
}

impl MemoryHistory {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn session(&self, id: &str) -> Option<MemorySession> {
        self.lock().sessions.get(id).cloned()
    }

    /// Every session, oldest first.
    pub fn sessions(&self) -> Vec<MemorySession> {
        let store = self.lock();
        store
            .order
            .iter()
            .filter_map(|id| store.sessions.get(id).cloned())
            .collect()
    }

    /// Make further appends report a full queue.
    pub fn refuse_appends(&self, refuse: bool) {
        self.refuse.store(refuse, Ordering::SeqCst);
    }

    /// Report a storage failure for a session to its listeners.
    pub fn fail(&self, session_id: &str, message: &str) {
        let listeners: Vec<Listener> = self
            .lock()
            .listeners
            .get(session_id)
            .map(|listeners| {
                listeners
                    .iter()
                    .map(|(_, listener)| listener.clone())
                    .collect()
            })
            .unwrap_or_default();
        for listener in listeners {
            listener(message.to_string());
        }
    }
}

impl HistorySink for MemoryHistory {
    fn begin_session(&self, input: SessionLogCreateInput, _limits: PartLimits) -> String {
        let id = format!(
            "session-{}",
            self.next_id.fetch_add(1, Ordering::SeqCst) + 1
        );
        let session = MemorySession {
            title: input.title.clone(),
            capture_input: input.capture_input,
            input,
            status: SessionLogStatus::Active,
            ended_at: None,
            paused: false,
            events: Vec::new(),
        };
        let mut store = self.lock();
        store.sessions.insert(id.clone(), session);
        store.order.push(id.clone());
        id
    }

    fn append(&self, session_id: &str, events: Vec<HistoryEvent>, _limits: PartLimits) -> bool {
        if self.refuse.load(Ordering::SeqCst) {
            return false;
        }
        if let Some(session) = self.lock().sessions.get_mut(session_id) {
            session.events.extend(events);
        }
        true
    }

    fn set_session_state(&self, session_id: &str, paused: bool, capture_input: bool) {
        if let Some(session) = self.lock().sessions.get_mut(session_id) {
            session.paused = paused;
            session.capture_input = capture_input;
        }
    }

    fn set_session_title(&self, session_id: &str, title: &str) {
        if let Some(session) = self.lock().sessions.get_mut(session_id) {
            session.title = title.to_string();
        }
    }

    fn finish_session(&self, session_id: &str, status: SessionEndStatus, ended_at: &str) {
        if let Some(session) = self.lock().sessions.get_mut(session_id) {
            session.status = status.into();
            session.ended_at = Some(ended_at.to_string());
        }
    }

    fn on_session_failure(
        &self,
        session_id: &str,
        listener: Box<dyn Fn(String) + Send + Sync>,
    ) -> FailureSubscription {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let token = NEXT.fetch_add(1, Ordering::SeqCst);
        self.lock()
            .listeners
            .entry(session_id.to_string())
            .or_default()
            .push((token, Arc::from(listener)));
        let store = self.store.clone();
        let session_id = session_id.to_string();
        FailureSubscription::new(move || {
            let mut store = store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(listeners) = store.listeners.get_mut(&session_id) {
                listeners.retain(|(id, _)| *id != token);
                if listeners.is_empty() {
                    store.listeners.remove(&session_id);
                }
            }
        })
    }
}
