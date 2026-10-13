//! Runs one login sequence over a live session (`server/src/login-sequence/runner.ts`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use muxus_core::api::{LoginSequenceStep, LoginWaitStep};
use muxus_core::protocol::LoginSequenceState;
use tokio::sync::{Notify, watch};

use super::matcher::{LoginOutputBuffer, MatchError, compile_wait_pattern};

/// What the tab shows while a sequence runs, and where it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginSequenceProgress {
    pub state: LoginSequenceState,
    /// 1-based step that is running, or where the sequence stopped.
    pub step: u32,
    pub steps: u32,
    pub detail: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginSequenceOutcome {
    Done,
    Failed {
        step: u32,
        message: String,
    },
    Cancelled {
        step: u32,
    },
    /// The session ended while the sequence ran; nobody is left to tell.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopReason {
    Cancelled,
    Closed,
}

/// Lets a vault step notice that the sequence was stopped while it waits
/// (for the master password, say).
#[derive(Debug, Clone)]
pub struct AbortSignal {
    rx: watch::Receiver<Option<StopReason>>,
}

impl AbortSignal {
    pub fn is_aborted(&self) -> bool {
        self.rx.borrow().is_some()
    }

    /// Resolves once the sequence is stopped.
    pub async fn aborted(&self) {
        let mut rx = self.rx.clone();
        let _ = rx.wait_for(Option::is_some).await;
    }
}

/// The session a sequence types into.
#[async_trait]
pub trait LoginSequenceIo: Send + Sync {
    /// Type bytes into the session, past session logging; `false` once the session is gone.
    fn write(&self, data: &[u8]) -> bool;
    /// Plain text a send step typed, for input capture. Secrets never pass through here.
    fn typed(&self, data: &[u8]);
    /// Type a vault secret, asking for the master password when the vault
    /// needs it; `Err` says why it could not.
    async fn type_secret(
        &self,
        secret_id: &str,
        enter: bool,
        abort: AbortSignal,
    ) -> Result<(), String>;
    fn progress(&self, update: LoginSequenceProgress);
}

const LABEL_MAX: usize = 60;

fn quoted(text: &str) -> String {
    let mut flat = String::with_capacity(text.len());
    let mut in_control = false;
    for ch in text.chars() {
        if ch.is_control() {
            if !in_control {
                flat.push(' ');
            }
            in_control = true;
        } else {
            flat.push(ch);
            in_control = false;
        }
    }
    if flat.encode_utf16().count() > LABEL_MAX {
        let mut units = 0;
        let cut: String = flat
            .chars()
            .take_while(|ch| {
                units += ch.len_utf16();
                units < LABEL_MAX
            })
            .collect();
        format!("“{cut}…”")
    } else {
        format!("“{flat}”")
    }
}

/// What a step does, as the tab shows it while the step runs.
pub fn describe_login_step(step: &LoginSequenceStep, secret_name: Option<&str>) -> String {
    match step {
        LoginSequenceStep::Wait(wait) => {
            if wait.regex == Some(true) {
                format!("Waiting for a match of {}", quoted(&wait.pattern))
            } else {
                format!("Waiting for {}", quoted(&wait.pattern))
            }
        }
        LoginSequenceStep::Send(send) => {
            if send.text.is_empty() {
                return "Pressing Enter".into();
            }
            format!(
                "Typing {}{}",
                quoted(&send.text),
                if send.enter { " and Enter" } else { "" }
            )
        }
        LoginSequenceStep::Secret(secret) => format!(
            "Typing the secret {}{}",
            quoted(secret_name.unwrap_or("from the password vault")),
            if secret.enter { " and Enter" } else { "" }
        ),
    }
}

type SecretName = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Runs one login sequence over a live session. Output is fed in as it
/// arrives (from the moment the session attaches, so a banner sent before
/// the first step starts still counts) and each wait step consumes what it
/// matched. Checks are coalesced: a flood of small chunks costs one match
/// attempt per wake-up rather than one per chunk.
pub struct LoginSequenceRunner {
    steps: Vec<LoginSequenceStep>,
    io: Arc<dyn LoginSequenceIo>,
    secret_name: Option<SecretName>,
    buffer: Mutex<LoginOutputBuffer>,
    wake: Notify,
    running: AtomicBool,
    stop: watch::Sender<Option<StopReason>>,
}

impl LoginSequenceRunner {
    pub fn new(
        steps: Vec<LoginSequenceStep>,
        io: Arc<dyn LoginSequenceIo>,
        secret_name: Option<SecretName>,
        buffer: Option<LoginOutputBuffer>,
    ) -> Self {
        Self {
            steps,
            io,
            secret_name,
            buffer: Mutex::new(buffer.unwrap_or_default()),
            wake: Notify::new(),
            running: AtomicBool::new(true),
            stop: watch::channel(None).0,
        }
    }

    fn buffer(&self) -> MutexGuard<'_, LoginOutputBuffer> {
        self.buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn output(&self, data: &[u8]) {
        if !self.running.load(Ordering::SeqCst) {
            return;
        }
        self.buffer().append(data);
        self.wake.notify_one();
    }

    /// Stop at the current step at the user's request; the session stays open.
    pub fn cancel(&self) {
        self.stop_with(StopReason::Cancelled);
    }

    /// The session ended: stop quietly.
    pub fn close(&self) {
        self.stop_with(StopReason::Closed);
    }

    fn stop_with(&self, reason: StopReason) {
        if self.running.load(Ordering::SeqCst) {
            self.stop.send_if_modified(|current| {
                if current.is_some() {
                    return false;
                }
                *current = Some(reason);
                true
            });
        }
    }

    fn stopped(&self, step: u32) -> Option<LoginSequenceOutcome> {
        (*self.stop.borrow()).map(|reason| match reason {
            StopReason::Cancelled => LoginSequenceOutcome::Cancelled { step },
            StopReason::Closed => LoginSequenceOutcome::Closed,
        })
    }

    fn finish(&self, update: LoginSequenceProgress) {
        self.running.store(false, Ordering::SeqCst);
        self.io.progress(update);
    }

    pub async fn run(&self) -> LoginSequenceOutcome {
        let total = self.steps.len() as u32;
        let mut current = 0;
        let mut detail = String::new();
        let mut outcome = LoginSequenceOutcome::Done;
        for (index, step) in self.steps.iter().enumerate() {
            current = index as u32 + 1;
            if let Some(stopped) = self.stopped(current) {
                outcome = stopped;
                break;
            }
            let secret_name = match step {
                LoginSequenceStep::Secret(secret) => self
                    .secret_name
                    .as_ref()
                    .and_then(|name| name(&secret.secret_id)),
                _ => None,
            };
            detail = describe_login_step(step, secret_name.as_deref());
            self.io.progress(LoginSequenceProgress {
                state: LoginSequenceState::Running,
                step: current,
                steps: total,
                detail: detail.clone(),
                message: None,
            });
            if let Err(stopped) = self.run_step(step, current).await {
                outcome = stopped;
                break;
            }
        }
        let update = |state, message| LoginSequenceProgress {
            state,
            step: current,
            steps: total,
            detail: detail.clone(),
            message,
        };
        match &outcome {
            LoginSequenceOutcome::Done => {
                self.finish(LoginSequenceProgress {
                    step: total,
                    ..update(LoginSequenceState::Done, None)
                });
            }
            LoginSequenceOutcome::Failed { message, .. } => {
                self.finish(update(LoginSequenceState::Failed, Some(message.clone())));
            }
            LoginSequenceOutcome::Cancelled { .. } => {
                self.finish(update(LoginSequenceState::Cancelled, None))
            }
            LoginSequenceOutcome::Closed => self.running.store(false, Ordering::SeqCst),
        }
        outcome
    }

    async fn run_step(
        &self,
        step: &LoginSequenceStep,
        number: u32,
    ) -> Result<(), LoginSequenceOutcome> {
        match step {
            LoginSequenceStep::Wait(wait) => self.wait_for(wait, number).await,
            LoginSequenceStep::Send(send) => {
                let mut data = send.text.clone().into_bytes();
                if send.enter {
                    data.push(b'\r');
                }
                if data.is_empty() {
                    return Ok(());
                }
                if !self.io.write(&data) {
                    return Err(LoginSequenceOutcome::Closed);
                }
                self.io.typed(&data);
                Ok(())
            }
            LoginSequenceStep::Secret(secret) => {
                let signal = AbortSignal {
                    rx: self.stop.subscribe(),
                };
                let result = self
                    .io
                    .type_secret(&secret.secret_id, secret.enter, signal)
                    .await;
                if let Some(stopped) = self.stopped(number) {
                    return Err(stopped);
                }
                result.map_err(|problem| LoginSequenceOutcome::Failed {
                    step: number,
                    message: format!("Step {number}: {problem}"),
                })
            }
        }
    }

    async fn wait_for(
        &self,
        step: &LoginWaitStep,
        number: u32,
    ) -> Result<(), LoginSequenceOutcome> {
        let pattern =
            compile_wait_pattern(&step.pattern, step.regex == Some(true)).map_err(|reason| {
                LoginSequenceOutcome::Failed {
                    step: number,
                    message: format!("Step {number} has an invalid pattern: {reason}"),
                }
            })?;
        let deadline = tokio::time::sleep(Duration::from_secs(step.timeout_seconds.into()));
        tokio::pin!(deadline);
        let mut stop = self.stop.subscribe();
        loop {
            if let Some(stopped) = self.stopped(number) {
                return Err(stopped);
            }
            let found = {
                let mut buffer = self.buffer();
                let found = buffer.find(&pattern);
                if let Ok(Some(found)) = &found {
                    buffer.consume(found.end);
                }
                found
            };
            match found {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => {}
                Err(MatchError::PatternTimeout) => {
                    return Err(LoginSequenceOutcome::Failed {
                        step: number,
                        message: format!(
                            "Step {number}'s pattern took too long to match, so it was stopped."
                        ),
                    });
                }
                Err(err) => {
                    return Err(LoginSequenceOutcome::Failed {
                        step: number,
                        message: format!("Step {number} could not match: {err}"),
                    });
                }
            }
            tokio::select! {
                () = self.wake.notified() => {}
                _ = stop.changed() => {}
                () = &mut deadline => {
                    let what = describe_login_step(&LoginSequenceStep::Wait(step.clone()), None);
                    let what = what.strip_prefix('W').map_or(what.clone(), |rest| format!("w{rest}"));
                    return Err(LoginSequenceOutcome::Failed {
                        step: number,
                        message: format!("Step {number} timed out after {} s {what}.", step.timeout_seconds),
                    });
                }
            }
        }
    }
}
