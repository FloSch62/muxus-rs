//! The per-terminal session recorder (`server/src/session-logging/session-recorder.ts`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use chrono::Local;
use muxus_core::api::{
    SessionEndStatus, SessionLineTimestamp, SessionLogCreateInput, SessionLogDirection,
    SessionLogFileSettings, SessionLoggingPolicy,
};
use muxus_core::js::parse_timestamp;
use muxus_core::protocol::{ProfileKind, TerminalProfile};
use serde::{Deserialize, Serialize};

use super::clock::Clock;
use super::log_file::{SessionLogFile, session_log_file_path};
use super::normalizer::TerminalTextNormalizer;

const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
const MAX_BUFFERED_EVENT_BYTES: usize = 256 * 1024;

/// One recorded event as session history stores it: the exact bytes and
/// their normalized, searchable text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEvent {
    pub sequence: u64,
    pub recorded_at: String,
    pub elapsed_ms: u64,
    pub direction: SessionLogDirection,
    pub raw: Vec<u8>,
    pub text: String,
    /// Absent on system lines, whose event time is their only time.
    pub line_timestamps: Option<Vec<SessionLineTimestamp>>,
}

/// Raw-log rotation limits from the host's logging policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartLimits {
    pub max_part_bytes: u64,
    pub max_parts: u32,
}

impl From<&SessionLoggingPolicy> for PartLimits {
    fn from(policy: &SessionLoggingPolicy) -> Self {
        Self {
            max_part_bytes: policy.max_part_bytes,
            max_parts: policy.max_parts,
        }
    }
}

/// Unsubscribes a history failure listener when dropped.
pub struct FailureSubscription(Option<Box<dyn FnOnce() + Send>>);

impl FailureSubscription {
    pub fn new(unsubscribe: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(unsubscribe)))
    }

    /// A subscription with nothing to undo.
    pub fn none() -> Self {
        Self(None)
    }
}

impl Drop for FailureSubscription {
    fn drop(&mut self) {
        if let Some(unsubscribe) = self.0.take() {
            unsubscribe();
        }
    }
}

/// Where a recorder sends session history. Implemented by the history store;
/// every call must return promptly (queue the disk work), because the
/// recorder runs on the terminal's data path.
pub trait HistorySink: Send + Sync {
    /// Start a retained session; returns its id.
    fn begin_session(&self, input: SessionLogCreateInput, limits: PartLimits) -> String;
    /// Queue one recorder batch without waiting for the disk. `false` is
    /// explicit backpressure: the recorder suspends logging for the session.
    fn append(&self, session_id: &str, events: Vec<HistoryEvent>, limits: PartLimits) -> bool;
    fn set_session_state(&self, session_id: &str, paused: bool, capture_input: bool);
    fn set_session_title(&self, session_id: &str, title: &str);
    fn finish_session(&self, session_id: &str, status: SessionEndStatus, ended_at: &str);
    /// Called with a message when storing this session fails later on.
    fn on_session_failure(
        &self,
        session_id: &str,
        listener: Box<dyn Fn(String) + Send + Sync>,
    ) -> FailureSubscription;
}

/// The recorder's state as the terminal tab shows it (the `logging-state` message).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLoggingState {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub paused: bool,
    pub capture_input: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Plain-text log file currently written for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

/// A change to the recorder from the tab (the `logging` control message).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLoggingPatch {
    pub enabled: Option<bool>,
    pub paused: Option<bool>,
    pub capture_input: Option<bool>,
    pub log_to_file: Option<bool>,
    /// A file the user picked; appended to.
    pub log_file_path: Option<String>,
}

/// Stable policy identity for saved hosts and deterministic ad-hoc endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub profile_key: String,
    pub host: String,
    pub kind: ProfileKind,
}

pub fn session_profile_identity(profile: &TerminalProfile) -> SessionIdentity {
    let saved = |id: &Option<String>| id.as_ref().map(|id| format!("profile:{id}"));
    match profile {
        TerminalProfile::Ssh(ssh) => SessionIdentity {
            profile_key: saved(&ssh.profile_id).unwrap_or_else(|| format!("ssh:{}", ssh.target)),
            host: ssh.target.clone(),
            kind: ProfileKind::Ssh,
        },
        TerminalProfile::Telnet(telnet) => {
            let host = format!("{}:{}", telnet.host, telnet.port);
            SessionIdentity {
                profile_key: saved(&telnet.profile_id).unwrap_or_else(|| format!("telnet:{host}")),
                host,
                kind: ProfileKind::Telnet,
            }
        }
        TerminalProfile::Serial(serial) => SessionIdentity {
            profile_key: saved(&serial.profile_id)
                .unwrap_or_else(|| format!("serial:{}", serial.path)),
            host: serial.path.clone(),
            kind: ProfileKind::Serial,
        },
        TerminalProfile::Local(local) => SessionIdentity {
            profile_key: "local".into(),
            host: local
                .shell
                .as_deref()
                .map(str::trim)
                .filter(|shell| !shell.is_empty())
                .unwrap_or("Local shell")
                .to_string(),
            kind: ProfileKind::Local,
        },
    }
}

/// Everything a recorder is started with.
pub struct RecorderConfig {
    pub history: Arc<dyn HistorySink>,
    /// The host's logging policy.
    pub policy: SessionLoggingPolicy,
    /// Read when a log file starts, so the current Settings apply.
    pub file_settings: Arc<dyn Fn() -> SessionLogFileSettings + Send + Sync>,
    /// The user's home directory, for the default log folder.
    pub home: PathBuf,
    pub identity: SessionIdentity,
    /// The tab title; blank falls back to the host.
    pub title: Option<String>,
    pub clock: Arc<dyn Clock>,
}

type StateListener = Arc<dyn Fn(SessionLoggingState) + Send + Sync>;

struct PendingEvent {
    sequence: u64,
    recorded_at: String,
    elapsed_ms: u64,
    direction: SessionLogDirection,
    raw: Vec<u8>,
    text: String,
    text_units: u64,
    line_timestamps: Vec<SessionLineTimestamp>,
}

/// Failures reported from other threads, applied under the recorder's lock.
enum Deferred {
    HistoryFailed {
        session_id: String,
        message: String,
    },
    FileFailed {
        generation: u64,
        path: String,
        message: String,
    },
}

struct Inner {
    state: SessionLoggingState,
    file: Option<SessionLogFile>,
    file_generation: u64,
    started_at_ms: i64,
    input_normalizer: TerminalTextNormalizer,
    output_normalizer: TerminalTextNormalizer,
    sequence: u64,
    terminal_ended: bool,
    pending: Vec<PendingEvent>,
    first_pending_at: Option<Instant>,
    timer_scheduled: bool,
    failure_subscription: Option<FailureSubscription>,
    title: String,
    /// State snapshots to hand the listener once the lock is released.
    notify: Vec<SessionLoggingState>,
}

struct Shared {
    inner: Mutex<Inner>,
    deferred: Mutex<Vec<Deferred>>,
    listener: Mutex<Option<StateListener>>,
    history: Arc<dyn HistorySink>,
    policy: SessionLoggingPolicy,
    file_settings: Arc<dyn Fn() -> SessionLogFileSettings + Send + Sync>,
    home: PathBuf,
    identity: SessionIdentity,
    clock: Arc<dyn Clock>,
    this: Weak<Shared>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One durable, timestamped recorder. It feeds session history and, on its
/// own schedule, a plain-text log file; pausing and input capture apply to
/// both. Persistence errors disable only logging; they never interrupt the
/// user's terminal transport.
///
/// History batches are coalesced for a quarter second; inside a tokio
/// runtime a timer flushes them, otherwise the next call does.
#[derive(Clone)]
pub struct SessionRecorder {
    shared: Arc<Shared>,
}

impl SessionRecorder {
    pub fn start(config: RecorderConfig) -> Self {
        let title = config
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| config.identity.host.clone());
        let inner = Inner {
            state: SessionLoggingState {
                enabled: false,
                session_id: None,
                paused: false,
                capture_input: config.policy.capture_input,
                warning: None,
                file_path: None,
            },
            file: None,
            file_generation: 0,
            started_at_ms: config.clock.now().timestamp_millis(),
            input_normalizer: TerminalTextNormalizer::new(0),
            output_normalizer: TerminalTextNormalizer::new(4),
            sequence: 0,
            terminal_ended: false,
            pending: Vec::new(),
            first_pending_at: None,
            timer_scheduled: false,
            failure_subscription: None,
            title,
            notify: Vec::new(),
        };
        let shared = Arc::new_cyclic(|this| Shared {
            inner: Mutex::new(inner),
            deferred: Mutex::new(Vec::new()),
            listener: Mutex::new(None),
            history: config.history,
            policy: config.policy,
            file_settings: config.file_settings,
            home: config.home,
            identity: config.identity,
            clock: config.clock,
            this: this.clone(),
        });
        let recorder = Self { shared };
        recorder.with(|shared, inner| {
            if shared.policy.enabled {
                shared.start_logging(inner);
            }
            if shared.policy.log_to_file {
                shared.start_file_log(inner, None);
            }
        });
        recorder
    }

    /// The current state, as the tab shows it.
    pub fn state(&self) -> SessionLoggingState {
        self.with(|_, inner| inner.state.clone())
    }

    /// Called when logging stops on its own (a failed write, a full queue).
    pub fn on_state_change(&self, listener: impl Fn(SessionLoggingState) + Send + Sync + 'static) {
        *lock(&self.shared.listener) = Some(Arc::new(listener));
    }

    /// Bytes the user typed.
    pub fn input(&self, data: &[u8]) {
        self.with(|shared, inner| shared.input(inner, data));
    }

    /// Bytes the session printed.
    pub fn output(&self, data: &[u8]) {
        self.with(|shared, inner| shared.output(inner, data));
    }

    /// A status line for every active log.
    pub fn system(&self, message: &str) {
        self.with(|shared, inner| shared.system(inner, message));
    }

    pub fn set_state(&self, patch: &SessionLoggingPatch) -> SessionLoggingState {
        self.with(|shared, inner| shared.set_state(inner, patch))
    }

    /// Follow a tab rename: the active record and any record started later in
    /// this terminal take the new title; finished records keep theirs.
    pub fn set_title(&self, title: &str) {
        self.with(|shared, inner| {
            let trimmed = title.trim();
            let next = if trimmed.is_empty() {
                shared.identity.host.clone()
            } else {
                trimmed.to_string()
            };
            if next == inner.title {
                return;
            }
            inner.title = next;
            if inner.state.enabled
                && let Some(id) = &inner.state.session_id
            {
                shared.history.set_session_title(id, &inner.title);
            }
        });
    }

    /// The terminal ended; logs are closed with `status`.
    pub fn end(&self, status: SessionEndStatus) {
        self.with(|shared, inner| {
            if inner.terminal_ended {
                return;
            }
            let word = status_word(status);
            shared.finish_logging(inner, status, &format!("Session logging ended ({word})."));
            shared.stop_file_log(inner, &format!("Logging to file ended ({word})"));
            inner.terminal_ended = true;
        });
    }

    fn with<R>(&self, run: impl FnOnce(&Shared, &mut Inner) -> R) -> R {
        let shared = &self.shared;
        let (result, notify) = {
            let mut inner = lock(&shared.inner);
            if inner
                .first_pending_at
                .is_some_and(|at| at.elapsed() >= FLUSH_INTERVAL)
            {
                shared.flush(&mut inner);
            }
            let result = run(shared, &mut inner);
            shared.apply_deferred(&mut inner);
            (result, std::mem::take(&mut inner.notify))
        };
        shared.notify(notify);
        shared.pump();
        result
    }
}

fn status_word(status: SessionEndStatus) -> &'static str {
    match status {
        SessionEndStatus::Completed => "completed",
        SessionEndStatus::Disconnected => "disconnected",
        SessionEndStatus::Failed => "failed",
    }
}

impl Shared {
    fn now_iso(&self) -> String {
        self.clock.now_iso()
    }

    fn notify(&self, states: Vec<SessionLoggingState>) {
        if states.is_empty() {
            return;
        }
        let listener = lock(&self.listener).clone();
        if let Some(listener) = listener {
            for state in states {
                listener(state);
            }
        }
    }

    /// Apply failures reported from elsewhere, unless someone holds the lock
    /// (they apply them before releasing it).
    fn pump(&self) {
        while !lock(&self.deferred).is_empty() {
            let Ok(mut inner) = self.inner.try_lock() else {
                return;
            };
            self.apply_deferred(&mut inner);
            let notify = std::mem::take(&mut inner.notify);
            drop(inner);
            self.notify(notify);
        }
    }

    fn defer(&self, event: Deferred) {
        lock(&self.deferred).push(event);
        self.pump();
    }

    fn apply_deferred(&self, inner: &mut Inner) {
        let events = std::mem::take(&mut *lock(&self.deferred));
        for event in events {
            match event {
                Deferred::HistoryFailed {
                    session_id,
                    message,
                } => {
                    if inner.state.session_id.as_deref() == Some(session_id.as_str()) {
                        self.suspend(inner, &message);
                    }
                }
                Deferred::FileFailed {
                    generation,
                    path,
                    message,
                } => {
                    tracing::warn!(path = %path, err = %message, "session log file write failed");
                    if inner.file.is_none() || inner.file_generation != generation {
                        continue;
                    }
                    inner.file = None;
                    inner.state.file_path = None;
                    if !inner.state.enabled {
                        inner.state.paused = false;
                    }
                    inner.state.warning = Some(format!("Logging to {path} stopped: {message}"));
                    inner.notify.push(inner.state.clone());
                }
            }
        }
    }

    fn start_logging(&self, inner: &mut Inner) {
        if inner.state.enabled || inner.terminal_ended {
            return;
        }
        // Only a log file can be paused here; the new record joins the pause.
        let paused = inner.state.paused;
        let started_at = self.now_iso();
        inner.started_at_ms = parse_timestamp(&started_at).map_or(0, |at| at.timestamp_millis());
        inner.input_normalizer = TerminalTextNormalizer::new(0);
        inner.output_normalizer = TerminalTextNormalizer::new(4);
        inner.sequence = 0;
        inner.pending.clear();
        inner.first_pending_at = None;
        inner.state.enabled = true;
        let id = self.history.begin_session(
            SessionLogCreateInput {
                profile_key: self.identity.profile_key.clone(),
                title: inner.title.clone(),
                kind: self.identity.kind,
                host: self.identity.host.clone(),
                started_at,
                capture_input: inner.state.capture_input,
            },
            PartLimits::from(&self.policy),
        );
        inner.state.session_id = Some(id.clone());
        inner.state.paused = false;
        inner.state.warning = None;
        let this = self.this.clone();
        let failed_id = id.clone();
        inner.failure_subscription = Some(self.history.on_session_failure(
            &id,
            Box::new(move |message| {
                if let Some(shared) = this.upgrade() {
                    shared.defer(Deferred::HistoryFailed {
                        session_id: failed_id.clone(),
                        message,
                    });
                }
            }),
        ));
        self.history_system(inner, "Session logging started.");
        if paused {
            self.history_system(inner, "Session logging paused.");
            inner.state.paused = true;
            self.history
                .set_session_state(&id, true, inner.state.capture_input);
        }
    }

    fn finish_logging(&self, inner: &mut Inner, status: SessionEndStatus, marker: &str) {
        let Some(id) = inner
            .state
            .session_id
            .clone()
            .filter(|_| inner.state.enabled)
        else {
            return;
        };
        // A closing marker is useful in a replay even if the session was paused.
        let paused = inner.state.paused;
        inner.state.paused = false;
        self.flush_normalizer_snapshots(inner, true);
        self.history_system(inner, marker);
        inner.state.enabled = false;
        // A log file that is still written stays paused.
        inner.state.paused = paused && inner.file.is_some();
        self.history.finish_session(&id, status, &self.now_iso());
        inner.failure_subscription = None;
    }

    fn input(&self, inner: &mut Inner, data: &[u8]) {
        if inner.state.paused || inner.terminal_ended || !inner.state.capture_input {
            return;
        }
        if let Some(file) = inner.file.as_mut() {
            file.input(data);
        }
        if !inner.state.enabled {
            return;
        }
        self.flush_output_snapshot(inner);
        let text = inner.input_normalizer.write(data, &self.now_iso());
        let stamps = inner.input_normalizer.take_line_timestamps();
        self.append(inner, SessionLogDirection::Input, data, text, stamps);
    }

    fn output(&self, inner: &mut Inner, data: &[u8]) {
        if (!inner.state.enabled && inner.file.is_none())
            || inner.state.paused
            || inner.terminal_ended
        {
            return;
        }
        if let Some(file) = inner.file.as_mut() {
            file.output(data);
        }
        if !inner.state.enabled {
            return;
        }
        if inner.state.capture_input {
            self.flush_input_snapshot(inner);
        }
        let text = inner.output_normalizer.write(data, &self.now_iso());
        let stamps = inner.output_normalizer.take_line_timestamps();
        self.append(inner, SessionLogDirection::Output, data, text, stamps);
    }

    fn system(&self, inner: &mut Inner, message: &str) {
        if !inner.state.paused
            && !inner.terminal_ended
            && let Some(file) = inner.file.as_mut()
        {
            file.system(message);
        }
        self.history_system(inner, message);
    }

    fn history_system(&self, inner: &mut Inner, message: &str) {
        // Markers must follow every normalized row that was visible before
        // them, even when the reconciler was still retaining an editable prompt.
        self.flush_normalizer_snapshots(inner, false);
        let text = format!("{message}\n");
        self.append_now(
            inner,
            SessionLogDirection::System,
            text.as_bytes().to_vec(),
            text,
            None,
        );
    }

    fn set_state(&self, inner: &mut Inner, patch: &SessionLoggingPatch) -> SessionLoggingState {
        if inner.terminal_ended {
            return inner.state.clone();
        }
        match patch.log_to_file {
            Some(false) => self.stop_file_log(inner, "Logging to file stopped"),
            Some(true) => self.start_file_log(inner, patch.log_file_path.as_deref()),
            None => {}
        }
        match patch.enabled {
            Some(false) if inner.state.enabled => {
                self.finish_logging(
                    inner,
                    SessionEndStatus::Completed,
                    "Session logging stopped.",
                );
            }
            Some(true) if !inner.state.enabled => self.start_logging(inner),
            _ => {}
        }
        if !inner.state.enabled && inner.file.is_none() {
            return inner.state.clone();
        }
        if let Some(paused) = patch.paused.filter(|&paused| paused != inner.state.paused) {
            if paused {
                self.flush_normalizer_snapshots(inner, false);
                self.system(inner, "Session logging paused.");
                inner.state.paused = true;
            } else {
                inner.state.paused = false;
                self.system(inner, "Session logging resumed.");
            }
        }
        if let Some(capture) = patch
            .capture_input
            .filter(|&capture| capture != inner.state.capture_input)
        {
            if !capture {
                self.flush_input_snapshot(inner);
            }
            inner.state.capture_input = capture;
            self.system(
                inner,
                if capture {
                    "Input recording enabled."
                } else {
                    "Input recording suppressed."
                },
            );
        }
        if inner.state.enabled
            && let Some(id) = &inner.state.session_id
        {
            self.history
                .set_session_state(id, inner.state.paused, inner.state.capture_input);
        }
        inner.state.clone()
    }

    /// Start writing a log file: `requested` is appended to, otherwise a new
    /// file is named from the log file settings. A failure only sets a warning.
    fn start_file_log(&self, inner: &mut Inner, requested: Option<&str>) {
        if inner.file.is_some() || inner.terminal_ended {
            return;
        }
        let settings = (self.file_settings)();
        let target = match requested {
            Some(path) => PathBuf::from(path),
            None => session_log_file_path(
                &settings,
                &self.home,
                &self.identity.host,
                &inner.title,
                self.identity.kind.as_str(),
                self.clock.now().with_timezone(&Local).naive_local(),
            ),
        };
        let mut file = match SessionLogFile::open(
            &target,
            requested.is_some(),
            settings.timestamps,
            self.clock.clone(),
        ) {
            Ok(file) => file,
            Err(err) => {
                let reason = err.to_string();
                inner.state.warning = Some(format!(
                    "Could not write the log file {}: {reason}",
                    target.display()
                ));
                tracing::warn!(path = %target.display(), err = %reason, "session log file could not be opened");
                return;
            }
        };
        inner.file_generation += 1;
        let generation = inner.file_generation;
        let path = file.path().to_string_lossy().into_owned();
        let this = self.this.clone();
        let failed_path = path.clone();
        file.on_failure(move |message| {
            if let Some(shared) = this.upgrade() {
                shared.defer(Deferred::FileFailed {
                    generation,
                    path: failed_path,
                    message,
                });
            }
        });
        let name = if inner.title == self.identity.host {
            self.identity.host.clone()
        } else {
            format!("{} ({})", inner.title, self.identity.host)
        };
        file.system(&format!(
            "Logging to file started: {name} at {}.",
            self.now_iso()
        ));
        if inner.state.paused {
            file.system("Session logging paused.");
        }
        inner.file = Some(file);
        inner.state.file_path = Some(path);
        inner.state.warning = None;
    }

    fn stop_file_log(&self, inner: &mut Inner, marker: &str) {
        let Some(mut file) = inner.file.take() else {
            return;
        };
        file.close(&format!("{marker} at {}.", self.now_iso()));
        inner.state.file_path = None;
        if !inner.state.enabled {
            inner.state.paused = false;
        }
    }

    fn append(
        &self,
        inner: &mut Inner,
        direction: SessionLogDirection,
        raw: &[u8],
        text: String,
        line_timestamps: Vec<SessionLineTimestamp>,
    ) {
        if !inner.state.enabled
            || inner.state.paused
            || inner.terminal_ended
            || (raw.is_empty() && text.is_empty())
        {
            return;
        }
        let recorded_at = self.now_iso();
        let elapsed_ms = self.elapsed_ms(inner, &recorded_at);
        let text_units = text.encode_utf16().count() as u64;
        match inner.pending.last_mut() {
            Some(previous)
                if previous.direction == direction
                    && previous.raw.len() + raw.len() <= MAX_BUFFERED_EVENT_BYTES =>
            {
                let offset = previous.text_units;
                previous
                    .line_timestamps
                    .extend(
                        line_timestamps
                            .into_iter()
                            .map(|stamp| SessionLineTimestamp {
                                offset: stamp.offset + offset,
                                recorded_at: stamp.recorded_at,
                            }),
                    );
                previous.raw.extend_from_slice(raw);
                previous.text.push_str(&text);
                previous.text_units += text_units;
            }
            _ => {
                inner.sequence += 1;
                inner.pending.push(PendingEvent {
                    sequence: inner.sequence,
                    recorded_at,
                    elapsed_ms,
                    direction,
                    raw: raw.to_vec(),
                    text,
                    text_units,
                    line_timestamps,
                });
            }
        }
        inner.first_pending_at.get_or_insert_with(Instant::now);
        if raw.len() >= MAX_BUFFERED_EVENT_BYTES {
            self.flush(inner);
            return;
        }
        self.schedule_flush(inner);
    }

    fn schedule_flush(&self, inner: &mut Inner) {
        if inner.timer_scheduled {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        inner.timer_scheduled = true;
        let this = self.this.clone();
        runtime.spawn(async move {
            tokio::time::sleep(FLUSH_INTERVAL).await;
            if let Some(shared) = this.upgrade() {
                SessionRecorder { shared }.with(|shared, inner| {
                    inner.timer_scheduled = false;
                    shared.flush(inner);
                });
            }
        });
    }

    fn elapsed_ms(&self, inner: &Inner, recorded_at: &str) -> u64 {
        let at =
            parse_timestamp(recorded_at).map_or(inner.started_at_ms, |at| at.timestamp_millis());
        (at - inner.started_at_ms).max(0) as u64
    }

    fn append_now(
        &self,
        inner: &mut Inner,
        direction: SessionLogDirection,
        raw: Vec<u8>,
        text: String,
        line_timestamps: Option<Vec<SessionLineTimestamp>>,
    ) {
        if !inner.state.enabled
            || inner.state.paused
            || inner.terminal_ended
            || (raw.is_empty() && text.is_empty())
        {
            return;
        }
        let recorded_at = self.now_iso();
        let elapsed_ms = self.elapsed_ms(inner, &recorded_at);
        inner.sequence += 1;
        let event = HistoryEvent {
            sequence: inner.sequence,
            recorded_at,
            elapsed_ms,
            direction,
            raw,
            text,
            line_timestamps,
        };
        self.persist(inner, vec![event]);
    }

    fn flush(&self, inner: &mut Inner) {
        inner.first_pending_at = None;
        let events: Vec<HistoryEvent> = inner
            .pending
            .drain(..)
            .map(|event| HistoryEvent {
                sequence: event.sequence,
                recorded_at: event.recorded_at,
                elapsed_ms: event.elapsed_ms,
                direction: event.direction,
                raw: event.raw,
                text: event.text,
                line_timestamps: Some(event.line_timestamps),
            })
            .collect();
        if !events.is_empty() {
            self.persist(inner, events);
        }
    }

    fn persist(&self, inner: &mut Inner, events: Vec<HistoryEvent>) {
        let Some(id) = inner.state.session_id.clone() else {
            return;
        };
        if !self
            .history
            .append(&id, events, PartLimits::from(&self.policy))
        {
            self.suspend(
                inner,
                "Session logging suspended: the history write queue is full.",
            );
        }
    }

    fn flush_normalizer_snapshots(&self, inner: &mut Inner, last: bool) {
        self.flush(inner);
        let input = if last {
            inner.input_normalizer.finish()
        } else {
            inner.input_normalizer.drain()
        };
        let output = if last {
            inner.output_normalizer.finish()
        } else {
            inner.output_normalizer.drain()
        };
        if !input.is_empty() {
            let stamps = inner.input_normalizer.take_line_timestamps();
            self.append_now(
                inner,
                SessionLogDirection::Input,
                Vec::new(),
                input,
                Some(stamps),
            );
        }
        if !output.is_empty() {
            let stamps = inner.output_normalizer.take_line_timestamps();
            self.append_now(
                inner,
                SessionLogDirection::Output,
                Vec::new(),
                output,
                Some(stamps),
            );
        }
    }

    fn flush_input_snapshot(&self, inner: &mut Inner) {
        self.flush(inner);
        let input = inner.input_normalizer.drain();
        if !input.is_empty() {
            let stamps = inner.input_normalizer.take_line_timestamps();
            self.append_now(
                inner,
                SessionLogDirection::Input,
                Vec::new(),
                input,
                Some(stamps),
            );
        }
    }

    fn flush_output_snapshot(&self, inner: &mut Inner) {
        self.flush(inner);
        let output = inner.output_normalizer.drain();
        if !output.is_empty() {
            let stamps = inner.output_normalizer.take_line_timestamps();
            self.append_now(
                inner,
                SessionLogDirection::Output,
                Vec::new(),
                output,
                Some(stamps),
            );
        }
    }

    fn suspend(&self, inner: &mut Inner, message: &str) {
        if !inner.state.enabled || inner.terminal_ended {
            return;
        }
        inner.state.enabled = false;
        // A log file that is still written stays paused.
        if inner.file.is_none() {
            inner.state.paused = false;
        }
        inner.state.warning = Some(message.to_string());
        inner.pending.clear();
        inner.first_pending_at = None;
        if let Some(id) = &inner.state.session_id {
            self.history
                .finish_session(id, SessionEndStatus::Failed, &self.now_iso());
        }
        inner.failure_subscription = None;
        tracing::warn!(session_id = ?inner.state.session_id, reason = %message, "terminal session logging suspended");
        inner.notify.push(inner.state.clone());
    }
}
