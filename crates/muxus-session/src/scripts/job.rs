//! A script run on many hosts (`client/src/scripts/script-jobs.ts`, the part that runs it).
//!
//! Each host connects the way its terminal tab would: a `dial` session
//! ([`Backend::open_session`]) with its jump hosts, fallback routes, keys, saved passwords and
//! an open connection to the host reused. Its sign-in questions become
//! [`ScriptJobEvent::Prompt`]s for the window to answer. Once the host is connected the script
//! runs there ([`super::runs`]) and its output streams back as [`ScriptJobEvent::Output`].
//!
//! [`ScriptJobHandle::cancel`] starts no more hosts, gives up on dials still connecting (their
//! waiting questions are withdrawn with [`ScriptJobEvent::PromptWithdrawn`]) and stops the
//! scripts still running, with everything they started. Dropping the handle does not stop the
//! job, as closing the Scripts dialog does not.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use muxus_core::api::{MAX_SCRIPT_BYTES, MAX_SCRIPT_TIMEOUT_SECONDS, ScriptRunState};
use muxus_core::js::utf16_len;
use muxus_core::protocol::{
    AuthPromptInfo, AuthPromptResponse, HostKeyChallenge, SshProfile, TerminalClientMessage,
    TerminalServerMessage,
};
use tokio::sync::watch;

use super::plan::{HostOutcome, HostRunState, RunMode, run_job};
use super::runs::ScriptRuns;
use crate::backend::Backend;
use crate::error::{ApiError, ApiResult};
use crate::terminal::{SessionEvent, SessionHandle};

/// A host scripts can run on, with how to reach it.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptTarget {
    /// The sidebar's key for the host (`ssh:<alias>`, `profile:<id>`).
    pub key: String,
    pub label: String,
    /// What its dial connects to (a saved host's id resolves on the backend).
    pub profile: SshProfile,
}

/// One job: a script, the hosts it runs on and how.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptJobRequest {
    pub body: String,
    pub targets: Vec<ScriptTarget>,
    pub mode: RunMode,
    /// Batches and one by one only: a failed host lets its batch finish and runs no more.
    pub stop_on_failure: bool,
    /// Stop a host's run after this long; 0 lets it run.
    pub timeout_seconds: u32,
}

/// A sign-in question of a host's dial.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptPrompt {
    Auth(AuthPromptInfo),
    HostKey(HostKeyChallenge),
}

/// The answer to a [`ScriptPrompt`].
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptPromptAnswer {
    /// `None` gives up on the host's connection.
    Auth(Option<AuthPromptResponse>),
    HostKey(bool),
}

/// A question waiting for the window; answer on `reply` (a dropped reply cancels).
#[derive(Debug, Clone)]
pub struct ScriptPromptRequest {
    pub id: u64,
    /// The key of the host that asks.
    pub host: String,
    pub prompt: ScriptPrompt,
    pub reply: async_channel::Sender<ScriptPromptAnswer>,
}

/// What a job reports, in order.
#[derive(Debug, Clone)]
pub enum ScriptJobEvent {
    /// A host moved on: connecting, running, or how it ended.
    Host {
        key: String,
        state: HostRunState,
        exit_code: Option<i32>,
        error: Option<String>,
        at: Instant,
    },
    /// More of a host's output (stdout and stderr together, as they arrived).
    Output {
        key: String,
        text: String,
    },
    Prompt(ScriptPromptRequest),
    /// The question no longer needs an answer: its dial gave up.
    PromptWithdrawn {
        id: u64,
    },
    /// Every host is done.
    Finished {
        at: Instant,
    },
}

/// The window's hold on a running job.
pub struct ScriptJobHandle {
    events: async_channel::Receiver<ScriptJobEvent>,
    cancel: watch::Sender<bool>,
}

impl ScriptJobHandle {
    /// The next event; `None` once the job has finished and everything was read.
    pub async fn next_event(&self) -> Option<ScriptJobEvent> {
        self.events.recv().await.ok()
    }

    pub fn try_next_event(&self) -> Option<ScriptJobEvent> {
        self.events.try_recv().ok()
    }

    /// Start no more hosts and stop the ones still connecting or running.
    pub fn cancel(&self) {
        self.cancel.send_replace(true);
    }
}

/// Resolves once the job is cancelled (never, if it cannot be any more).
async fn cancellation(cancel: &mut watch::Receiver<bool>) {
    if cancel.wait_for(|cancelled| *cancelled).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// A dial session that gives up when dropped.
struct DialSession(SessionHandle);

impl Drop for DialSession {
    fn drop(&mut self) {
        // A dropped handle would wait out the reattach grace; nobody comes back to a dial.
        self.0.close();
    }
}

/// Takes a question back from the window when its dial gives up before the answer.
struct PromptGuard<'a> {
    id: u64,
    job: &'a Job,
    answered: bool,
}

impl Drop for PromptGuard<'_> {
    fn drop(&mut self) {
        if !self.answered {
            self.job
                .emit(ScriptJobEvent::PromptWithdrawn { id: self.id });
        }
    }
}

struct Job {
    backend: Backend,
    runs: ScriptRuns,
    body: String,
    timeout_seconds: u32,
    events: async_channel::Sender<ScriptJobEvent>,
    cancel: watch::Receiver<bool>,
    next_prompt: AtomicU64,
}

impl Backend {
    /// Run a script on many SSH hosts. The job runs on the backend; the handle reads its
    /// events and cancels it.
    pub fn start_script_job(&self, request: ScriptJobRequest) -> ApiResult<ScriptJobHandle> {
        if request.body.is_empty() || utf16_len(&request.body) > MAX_SCRIPT_BYTES {
            return Err(ApiError::bad_request(
                "The script must be between 1 character and 1 MB.",
            ));
        }
        if request.timeout_seconds > MAX_SCRIPT_TIMEOUT_SECONDS {
            return Err(ApiError::bad_request(
                "The time limit can be at most a week.",
            ));
        }
        let (events_tx, events) = async_channel::unbounded();
        let (cancel, cancel_rx) = watch::channel(false);
        let job = Arc::new(Job {
            backend: self.clone(),
            runs: self.shared.script_runs.clone(),
            body: request.body,
            timeout_seconds: request.timeout_seconds,
            events: events_tx,
            cancel: cancel_rx,
            next_prompt: AtomicU64::new(1),
        });
        let (targets, mode, stop_on_failure) =
            (request.targets, request.mode, request.stop_on_failure);
        self.shared.runtime.spawn(async move {
            run_job(
                &targets,
                mode,
                stop_on_failure,
                || job.cancelled(),
                |target| {
                    let job = job.clone();
                    async move { job.run_on_host(target).await }
                },
                |target| job.state(&target.key, HostRunState::Skipped, None, None),
            )
            .await;
            job.emit(ScriptJobEvent::Finished { at: Instant::now() });
        });
        Ok(ScriptJobHandle { events, cancel })
    }
}

impl Job {
    fn emit(&self, event: ScriptJobEvent) {
        let _ = self.events.try_send(event);
    }

    fn cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    fn state(&self, key: &str, state: HostRunState, exit_code: Option<i32>, error: Option<String>) {
        self.emit(ScriptJobEvent::Host {
            key: key.to_owned(),
            state,
            exit_code,
            error,
            at: Instant::now(),
        });
    }

    /// The host could not run the script: failed, or cancelled when the job was.
    fn fail(&self, key: &str, error: String) -> HostOutcome {
        if self.cancelled() {
            self.state(key, HostRunState::Cancelled, None, None);
            HostOutcome::Cancelled
        } else {
            self.state(key, HostRunState::Failed, None, Some(error));
            HostOutcome::Failed
        }
    }

    async fn run_on_host(&self, target: ScriptTarget) -> HostOutcome {
        let key = target.key.as_str();
        if self.cancelled() {
            self.state(key, HostRunState::Skipped, None, None);
            return HostOutcome::Cancelled;
        }
        self.state(key, HostRunState::Connecting, None, None);
        let (conn_id, dialed) = match self.dial(&target).await {
            Ok(dialed) => dialed,
            Err(error) => return self.fail(key, error),
        };
        if self.cancelled() {
            return self.fail(key, "Cancelled.".into());
        }
        let started = self
            .runs
            .start(&conn_id, &self.body, self.timeout_seconds)
            .await;
        // The run holds its own lease on the connection now.
        drop(dialed);
        let run_id = match started {
            Ok(id) => id,
            Err(error) => return self.fail(key, error.message),
        };
        // A cancel that came while the run was starting found no run to stop.
        if self.cancelled() {
            self.runs.cancel(&run_id);
        }
        self.state(key, HostRunState::Running, None, None);
        let mut changes = match self.runs.watch(&run_id) {
            Ok(changes) => changes,
            Err(error) => return self.fail(key, error.message),
        };
        let mut cancel = self.cancel.clone();
        let mut offset = 0;
        loop {
            let status = match self.runs.status(&run_id, offset) {
                Ok(status) => status,
                Err(error) => return self.fail(key, error.message),
            };
            offset = status.offset;
            if !status.output.is_empty() {
                self.emit(ScriptJobEvent::Output {
                    key: key.to_owned(),
                    text: status.output,
                });
            }
            if status.state != ScriptRunState::Running {
                let outcome = match status.state {
                    ScriptRunState::Completed => HostOutcome::Completed,
                    ScriptRunState::Cancelled => HostOutcome::Cancelled,
                    _ => HostOutcome::Failed,
                };
                self.state(key, outcome.into(), status.exit_code, status.error);
                return outcome;
            }
            tokio::select! {
                changed = changes.changed() => {
                    if changed.is_err() {
                        return self.fail(key, "This run is no longer known.".into());
                    }
                }
                _ = cancellation(&mut cancel) => self.runs.cancel(&run_id),
            }
        }
    }

    /// `dialProfile`: connect `target` with no terminal; its connection id and the dial
    /// session holding it until the run has a lease of its own.
    async fn dial(&self, target: &ScriptTarget) -> Result<(String, DialSession), String> {
        let mut cancel = self.cancel.clone();
        if self.cancelled() {
            return Err("cancelled".into());
        }
        let session = DialSession(self.backend.open_session(TerminalClientMessage::Dial {
            profile: target.profile.clone(),
        }));
        let drive = async {
            let handle = &session.0;
            let mut last_status = String::new();
            let fallback = |last: &str, otherwise: &str| {
                if last.is_empty() {
                    otherwise.to_owned()
                } else {
                    last.to_owned()
                }
            };
            loop {
                let Some(event) = handle.next_event().await else {
                    return Err(fallback(
                        &last_status,
                        "connection closed before it was ready",
                    ));
                };
                match event {
                    SessionEvent::Message(TerminalServerMessage::Status { message, .. }) => {
                        last_status = message;
                    }
                    SessionEvent::Message(TerminalServerMessage::AuthPrompt(mut info)) => {
                        // The window's questions say which host asks.
                        if info.name.as_deref().is_none_or(str::is_empty) {
                            info.name = Some(format!("Sign in to {}", target.label));
                        }
                        match self.ask(&target.key, ScriptPrompt::Auth(info)).await {
                            ScriptPromptAnswer::Auth(Some(response)) => {
                                handle.send(TerminalClientMessage::AuthResponse(response))
                            }
                            _ => {
                                handle.close();
                                return Err(fallback(
                                    &last_status,
                                    "connection closed before it was ready",
                                ));
                            }
                        }
                    }
                    SessionEvent::Message(TerminalServerMessage::HostKey(challenge)) => {
                        let accept = matches!(
                            self.ask(&target.key, ScriptPrompt::HostKey(challenge))
                                .await,
                            ScriptPromptAnswer::HostKey(true)
                        );
                        handle.send(TerminalClientMessage::HostKeyResponse { accept });
                    }
                    SessionEvent::Message(TerminalServerMessage::Ready { conn_id, .. }) => {
                        return Ok(conn_id);
                    }
                    SessionEvent::Message(TerminalServerMessage::Exit { message, .. }) => {
                        return Err(message
                            .filter(|message| !message.is_empty())
                            .unwrap_or_else(|| fallback(&last_status, "connection failed")));
                    }
                    SessionEvent::Closed { .. } => {
                        return Err(fallback(
                            &last_status,
                            "connection closed before it was ready",
                        ));
                    }
                    _ => {}
                }
            }
        };
        // Cancelling gives up on a dial still in progress, its questions included.
        let result = tokio::select! {
            result = drive => result,
            _ = cancellation(&mut cancel) => Err("cancelled".into()),
        };
        result.map(|conn_id| (conn_id, session))
    }

    /// Ask the window and wait for the answer; a dial that gives up takes the question back.
    async fn ask(&self, host: &str, prompt: ScriptPrompt) -> ScriptPromptAnswer {
        let id = self.next_prompt.fetch_add(1, Ordering::Relaxed);
        let cancelled = match &prompt {
            ScriptPrompt::Auth(_) => ScriptPromptAnswer::Auth(None),
            ScriptPrompt::HostKey(_) => ScriptPromptAnswer::HostKey(false),
        };
        let (reply, answer) = async_channel::bounded(1);
        let mut guard = PromptGuard {
            id,
            job: self,
            answered: false,
        };
        self.emit(ScriptJobEvent::Prompt(ScriptPromptRequest {
            id,
            host: host.to_owned(),
            prompt,
            reply,
        }));
        let answer = answer.recv().await.unwrap_or(cancelled);
        guard.answered = true;
        answer
    }
}
