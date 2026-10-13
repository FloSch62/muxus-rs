//! Durable session history (port of `server/src/session-logging/history-store.ts`
//! and `history-worker.js`, storage and query side).
//!
//! A history root holds `session-history.sqlite` (session metadata, the
//! normalized transcript in FTS5-indexed chunks, and the part index) and
//! `sessions/<id>/NNNNNN.muxlog.{partial,zst}` raw segments with every byte a
//! session sent and received. The layout and schema are the TypeScript
//! build's, so both builds can share one root.
//!
//! [`SessionHistoryStore`] is an async facade over a dedicated worker thread.
//! Writes from a live terminal (`begin_session`, `append`, …) only queue
//! bounded copies and never wait for disk; their failures reach the
//! session's [`SessionHistoryStore::on_session_failure`] listeners. Queries
//! are `async` and executor-agnostic (they await an `async-channel` reply),
//! so they work from tokio and from the UI executor alike.
//!
//! Recording itself (what counts as an event, terminal text normalization)
//! lives in `muxus-session`; it hands this store finished [`HistoryEvent`]s.

mod disk;
mod schema;
mod search;
mod segment;
mod worker;

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread::JoinHandle;

use muxus_core::api::{
    SessionEndStatus, SessionHistoryResponse, SessionHistorySettings, SessionHistoryStorageStatus,
    SessionLineTimestamp, SessionLogCreateInput, SessionLogDetail, SessionLogDirection,
    SessionLoggingPolicy,
};

use crate::ids;
use worker::{Failure, Job, Outcome, Worker};

/// A batch larger than this, or one that would push the queued writes past
/// it, is refused so a slow disk cannot grow memory without bound.
pub const MAX_PENDING_WRITE_BYTES: u64 = 8 * 1024 * 1024;

/// One recorded event, as the recorder hands it over and as raw exports
/// read it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEvent {
    /// Per-session sequence number, from 1.
    pub sequence: u64,
    /// ISO-8601 time the event was recorded.
    pub recorded_at: String,
    /// Milliseconds since the session started.
    pub elapsed_ms: u64,
    pub direction: SessionLogDirection,
    /// The bytes exactly as they crossed the session.
    pub raw: Vec<u8>,
    /// Normalized, searchable text; empty for events with no visible text.
    /// Raw exports read it back empty.
    pub text: String,
    /// When each line's visible content last changed; `None` on older recordings.
    pub line_timestamps: Option<Vec<SessionLineTimestamp>>,
}

/// How a session's raw log is split into parts (from its logging policy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartPolicy {
    /// Rotate before a part grows beyond this many raw bytes.
    pub max_part_bytes: u64,
    /// Newest parts kept per session; older ones are dropped with their transcript.
    pub max_parts: u32,
}

impl From<&SessionLoggingPolicy> for PartPolicy {
    fn from(policy: &SessionLoggingPolicy) -> Self {
        Self {
            max_part_bytes: policy.max_part_bytes,
            max_parts: policy.max_parts,
        }
    }
}

/// A history list or search (`SessionHistoryQuery`). Empty strings count as unset.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionHistoryQuery {
    /// Full-text query; the last word matches as a prefix. Also matches the
    /// title, name and host.
    pub query: Option<String>,
    pub profile_key: Option<String>,
    /// Substring of the host, case-insensitive.
    pub host: Option<String>,
    /// Profile kind (`ssh`, `local`, `serial`, `telnet`).
    pub kind: Option<String>,
    /// ISO-8601 bounds on the start time, inclusive.
    pub started_after: Option<String>,
    pub started_before: Option<String>,
    /// Page size, 1–100; 0 means the default of 50.
    pub limit: u32,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// A live change to a recording session; `None` keeps the current value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionStatePatch {
    pub paused: Option<bool>,
    pub capture_input: Option<bool>,
}

/// Where and how to open a history root.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryOpenOptions {
    /// The history root; `None` uses a temporary root removed on close (for
    /// an in-memory application database).
    pub root: Option<PathBuf>,
    pub settings: SessionHistorySettings,
    /// A version-6 application database whose `session_logs` and
    /// `session_log_events` are imported into the root on open. The caller
    /// drops those tables afterwards (`Database::finalize_session_history_separation`).
    pub legacy_database_path: Option<PathBuf>,
}

/// Why a history operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    #[error("session history store is closed")]
    Closed,
    /// The worker stopped without answering.
    #[error("session history worker stopped")]
    WorkerStopped,
    /// Storage, database or input failure; the message is user-presentable.
    #[error("{0}")]
    Failed(String),
}

impl From<Failure> for HistoryError {
    fn from(failure: Failure) -> Self {
        HistoryError::Failed(failure.0)
    }
}

type Listener = Arc<dyn Fn(String) + Send + Sync>;

struct Shared {
    listeners: Mutex<HashMap<String, Vec<(u64, Listener)>>>,
    next_listener: AtomicU64,
    pending_write_bytes: AtomicU64,
    closed: AtomicBool,
}

impl Shared {
    fn notify_failure(&self, session_id: &str, message: &str) {
        let listeners: Vec<Listener> = self
            .listeners
            .lock()
            .expect("listener lock")
            .get(session_id)
            .map(|entries| {
                entries
                    .iter()
                    .map(|(_, listener)| listener.clone())
                    .collect()
            })
            .unwrap_or_default();
        for listener in listeners {
            listener(message.to_owned());
        }
    }
}

/// Keeps a failure listener registered; dropping it (or calling
/// [`FailureSubscription::unsubscribe`]) removes the listener.
#[must_use = "dropping the subscription removes the listener"]
pub struct FailureSubscription {
    shared: Weak<Shared>,
    session_id: String,
    token: u64,
    keep: bool,
}

impl FailureSubscription {
    /// Remove the listener now.
    pub fn unsubscribe(self) {}

    /// Keep the listener for as long as the store lives.
    pub fn detach(mut self) {
        self.keep = true;
    }
}

impl Drop for FailureSubscription {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut listeners = shared.listeners.lock().expect("listener lock");
        if let Some(entries) = listeners.get_mut(&self.session_id) {
            entries.retain(|(token, _)| *token != self.token);
            if entries.is_empty() {
                listeners.remove(&self.session_id);
            }
        }
    }
}

/// Async facade for the history worker thread.
pub struct SessionHistoryStore {
    root: PathBuf,
    jobs: mpsc::Sender<Job>,
    thread: Mutex<Option<JoinHandle<()>>>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for SessionHistoryStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionHistoryStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl SessionHistoryStore {
    /// Open a history root: create it (0700), bring its database up to date,
    /// import legacy history, recover sessions a crash left active, and wait
    /// until the worker is ready. The startup retention pass runs right after
    /// and finishes before any later request is answered.
    pub async fn open(options: HistoryOpenOptions) -> Result<Self, HistoryError> {
        let temporary = options.root.is_none();
        let root = match options.root {
            Some(root) => resolve_path(&root),
            None => {
                create_temporary_root().map_err(|error| HistoryError::Failed(error.to_string()))?
            }
        };
        if root.parent().is_none() {
            return Err(HistoryError::Failed(
                "the filesystem root cannot be used as the session history location".into(),
            ));
        }
        worker::create_private_dir_all(&root)
            .map_err(|error| HistoryError::Failed(error.to_string()))?;
        let (ready_tx, ready_rx) = async_channel::bounded::<Result<(), String>>(1);
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
        let worker_root = root.clone();
        let settings = options.settings;
        let legacy = options.legacy_database_path;
        let thread = std::thread::Builder::new()
            .name("muxus-session-history".into())
            .spawn(move || {
                match Worker::open(worker_root.clone(), settings, legacy) {
                    Ok(worker) => {
                        let _ = ready_tx.try_send(Ok(()));
                        worker.run(jobs_rx);
                    }
                    Err(failure) => {
                        let _ = ready_tx.try_send(Err(failure.0));
                    }
                }
                if temporary {
                    let _ = worker::remove_dir_all_if_exists(&worker_root);
                }
            })
            .map_err(|error| HistoryError::Failed(error.to_string()))?;
        match ready_rx.recv().await {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                let _ = thread.join();
                return Err(HistoryError::Failed(message));
            }
            Err(_) => {
                let _ = thread.join();
                return Err(HistoryError::WorkerStopped);
            }
        }
        let store = Self {
            root,
            jobs: jobs_tx,
            thread: Mutex::new(Some(thread)),
            shared: Arc::new(Shared {
                listeners: Mutex::new(HashMap::new()),
                next_listener: AtomicU64::new(1),
                pending_write_bytes: AtomicU64::new(0),
                closed: AtomicBool::new(false),
            }),
        };
        // `ready`: answered once open-time work is done; startup retention follows.
        store.call(|_| Ok(())).await?;
        Ok(store)
    }

    /// The resolved history root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Start recording a session and return its new id at once. Failures
    /// (quota exhausted, storage errors) reach the session's failure listeners.
    pub fn begin_session(&self, input: &SessionLogCreateInput, policy: PartPolicy) -> String {
        let id = ids::nanoid();
        let session_id = id.clone();
        let input = input.clone();
        self.post(&id, move |worker| {
            worker.begin_session(&session_id, &input, policy).map(drop)
        });
        id
    }

    /// Queue one recorder batch without waiting for disk. `false` is explicit
    /// backpressure (the store is closed or too many bytes are queued):
    /// callers must suspend logging for that session.
    pub fn append(&self, session_id: &str, events: Vec<HistoryEvent>, policy: PartPolicy) -> bool {
        let bytes: u64 = events
            .iter()
            .map(|event| event.raw.len() as u64 + event.text.len() as u64 + 64)
            .sum();
        if self.shared.closed.load(Ordering::SeqCst) || bytes > MAX_PENDING_WRITE_BYTES {
            return false;
        }
        let reserved = self.shared.pending_write_bytes.try_update(
            Ordering::SeqCst,
            Ordering::SeqCst,
            |pending| (pending + bytes <= MAX_PENDING_WRITE_BYTES).then_some(pending + bytes),
        );
        if reserved.is_err() {
            return false;
        }
        let shared = self.shared.clone();
        let id = session_id.to_owned();
        let job_id = id.clone();
        let queued = self.post(&id, move |worker| {
            let outcome = worker.append(&job_id, &events, policy).map(drop);
            shared
                .pending_write_bytes
                .fetch_sub(bytes, Ordering::SeqCst);
            outcome
        });
        if !queued {
            self.shared
                .pending_write_bytes
                .fetch_sub(bytes, Ordering::SeqCst);
        }
        true
    }

    /// Pause/resume recording or change input capture for a live session.
    pub fn set_session_state(&self, session_id: &str, patch: SessionStatePatch) {
        let id = session_id.to_owned();
        self.post(session_id, move |worker| {
            worker
                .set_session_state(&id, patch.paused, patch.capture_input)
                .map(drop)
        });
    }

    /// The tab was renamed; an active record takes the new title.
    pub fn set_session_title(&self, session_id: &str, title: &str) {
        let id = session_id.to_owned();
        let title = title.to_owned();
        self.post(session_id, move |worker| {
            worker.set_title(&id, &title).map(drop)
        });
    }

    /// Finish a session: compress its last part and record how it ended.
    pub fn finish_session(&self, session_id: &str, status: SessionEndStatus, ended_at: &str) {
        let id = session_id.to_owned();
        let ended_at = ended_at.to_owned();
        self.post(session_id, move |worker| {
            worker.finish_session(&id, status, &ended_at).map(drop)
        });
    }

    /// Call `listener` with the message of every failed write for this
    /// session. Listeners run on the history thread: they must not block on
    /// the store.
    pub fn on_session_failure(
        &self,
        session_id: &str,
        listener: impl Fn(String) + Send + Sync + 'static,
    ) -> FailureSubscription {
        let token = self.shared.next_listener.fetch_add(1, Ordering::SeqCst);
        self.shared
            .listeners
            .lock()
            .expect("listener lock")
            .entry(session_id.to_owned())
            .or_default()
            .push((token, Arc::new(listener)));
        FailureSubscription {
            shared: Arc::downgrade(&self.shared),
            session_id: session_id.to_owned(),
            token,
            keep: false,
        }
    }

    /// One page of sessions, newest first.
    pub async fn session_history(
        &self,
        query: SessionHistoryQuery,
    ) -> Result<SessionHistoryResponse, HistoryError> {
        self.call(move |worker| worker.search(&query)).await
    }

    /// One session with its transcript events. With `event_limit`, only that
    /// many events are returned: the newest ones, or — when `match_query`
    /// matches a chunk outside that window — a window anchored on the first
    /// match with some leading context.
    pub async fn session_log(
        &self,
        id: &str,
        event_limit: Option<usize>,
        match_query: Option<&str>,
    ) -> Result<Option<SessionLogDetail>, HistoryError> {
        let id = id.to_owned();
        let match_query = match_query.map(str::to_owned);
        self.call(move |worker| worker.detail(&id, event_limit, match_query.as_deref()))
            .await
    }

    /// Every retained raw event of a session, in sequence order, including
    /// the active part of a live one. `None` for an unknown session.
    pub async fn raw_session_log_events(
        &self,
        id: &str,
    ) -> Result<Option<Vec<HistoryEvent>>, HistoryError> {
        let id = id.to_owned();
        self.call(move |worker| worker.raw_events(&id)).await
    }

    /// Delete a finished session and its files; `false` for an unknown or active one.
    pub async fn delete_session(&self, id: &str) -> Result<bool, HistoryError> {
        let id = id.to_owned();
        self.call(move |worker| worker.delete_session(&id, false))
            .await
    }

    /// Pin or unpin a session; pinned sessions are exempt from retention.
    pub async fn set_pinned(&self, id: &str, pinned: bool) -> Result<bool, HistoryError> {
        let id = id.to_owned();
        self.call(move |worker| worker.set_pinned(&id, pinned))
            .await
    }

    /// Name a session. An empty label clears the name and shows the recorded
    /// title again.
    pub async fn set_label(&self, id: &str, label: &str) -> Result<bool, HistoryError> {
        let id = id.to_owned();
        let label = label.to_owned();
        self.call(move |worker| worker.set_label(&id, &label)).await
    }

    /// Apply new global limits and run retention with them.
    pub async fn update_settings(
        &self,
        settings: SessionHistorySettings,
    ) -> Result<(), HistoryError> {
        self.call(move |worker| worker.update_settings(settings).map(drop))
            .await
    }

    /// Usage, free space and quota state. `configured_location` is the saved
    /// storage location; when it differs from the open root the answer says a
    /// restart is required.
    pub async fn storage_status(
        &self,
        configured_location: Option<&str>,
    ) -> Result<SessionHistoryStorageStatus, HistoryError> {
        let configured = configured_location.map(str::to_owned);
        self.call(move |worker| worker.storage_status(configured.as_deref()))
            .await
    }

    /// Finalize open parts, mark live sessions disconnected, checkpoint and
    /// stop the worker. A temporary root is removed. Later calls fail with
    /// [`HistoryError::Closed`]; closing twice is harmless.
    pub async fn close(&self) -> Result<(), HistoryError> {
        if self.shared.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let (reply_tx, reply_rx) = async_channel::bounded::<Outcome<()>>(1);
        let sent = self
            .jobs
            .send(Box::new(move |worker: &mut Worker| {
                let _ = reply_tx.try_send(worker.close());
            }))
            .is_ok();
        // The worker may already have stopped after a fatal storage failure.
        let result = if sent {
            reply_rx.recv().await.ok()
        } else {
            None
        };
        if let Some(thread) = self.thread.lock().expect("thread lock").take() {
            let _ = thread.join();
        }
        match result {
            Some(Err(failure)) => Err(failure.into()),
            _ => Ok(()),
        }
    }

    /// Run `job` on the worker and await its answer.
    async fn call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Worker) -> Outcome<T> + Send + 'static,
    ) -> Result<T, HistoryError> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(HistoryError::Closed);
        }
        let (reply_tx, reply_rx) = async_channel::bounded::<Outcome<T>>(1);
        self.jobs
            .send(Box::new(move |worker: &mut Worker| {
                let _ = reply_tx.try_send(job(worker));
            }))
            .map_err(|_| HistoryError::WorkerStopped)?;
        reply_rx
            .recv()
            .await
            .map_err(|_| HistoryError::WorkerStopped)?
            .map_err(HistoryError::from)
    }

    /// Queue a fire-and-forget job whose failure goes to the session's
    /// listeners. Returns whether it was queued.
    fn post(
        &self,
        session_id: &str,
        job: impl FnOnce(&mut Worker) -> Outcome<()> + Send + 'static,
    ) -> bool {
        let shared = self.shared.clone();
        let id = session_id.to_owned();
        if shared.closed.load(Ordering::SeqCst) {
            shared.notify_failure(&id, &HistoryError::Closed.to_string());
            return false;
        }
        let notify = shared.clone();
        let sent = self.jobs.send(Box::new(move |worker: &mut Worker| {
            if let Err(failure) = job(worker) {
                notify.notify_failure(&id, &failure.0);
            }
        }));
        if sent.is_err() {
            shared.notify_failure(session_id, &HistoryError::WorkerStopped.to_string());
            return false;
        }
        true
    }
}

/// `path.resolve`: absolute, with `.` and `..` folded lexically.
pub(crate) fn resolve_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

/// `mkdtempSync(os.tmpdir()/muxus-session-history-…)`.
fn create_temporary_root() -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir();
    loop {
        let candidate = base.join(format!(
            "muxus-session-history-{}",
            ids::random_base64url(6).replace('-', "_")
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_paths_lexically() {
        assert_eq!(
            resolve_path(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(resolve_path(Path::new("/")), PathBuf::from("/"));
    }
}
