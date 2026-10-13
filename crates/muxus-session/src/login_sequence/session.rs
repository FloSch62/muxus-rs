//! A login sequence attached to one terminal session (`server/src/login-sequence/session.ts`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use muxus_core::api::LoginSequence;
use muxus_core::protocol::{
    AuthPromptField, AuthPromptInfo, AuthPromptPurpose, AuthPromptResponse, LoginSequenceState,
    TerminalClientMessage, TerminalServerMessage,
};
use tokio::sync::oneshot;

use super::runner::{AbortSignal, LoginSequenceIo, LoginSequenceProgress, LoginSequenceRunner};

const MASTER_PASSWORD_ATTEMPTS: u32 = 3;

/// Why the vault did not hand out a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    /// The vault needs its master password for this use.
    UnlockRequired,
    /// The master password was wrong or malformed; the message says how.
    InvalidMasterPassword(String),
    /// The stored secret could not be decrypted.
    Corrupt,
    Other(String),
}

/// The password vault, as a login sequence uses it.
#[async_trait]
pub trait SecretVault: Send + Sync {
    /// The display name of a secret.
    fn secret_name(&self, secret_id: &str) -> Option<String>;
    /// The secret's value; `Ok(None)` when it no longer exists.
    async fn secret_value(
        &self,
        secret_id: &str,
        master_password: Option<&str>,
    ) -> Result<Option<String>, VaultError>;
}

/// The session recorder, as a login sequence reports to it.
pub trait LoginSequenceRecorder: Send + Sync {
    /// Plain text a send step typed (recorded only with input capture on).
    fn input(&self, data: &[u8]);
    /// A status line for the logs.
    fn system(&self, message: &str);
}

impl LoginSequenceRecorder for crate::logging::SessionRecorder {
    fn input(&self, data: &[u8]) {
        crate::logging::SessionRecorder::input(self, data);
    }

    fn system(&self, message: &str) {
        crate::logging::SessionRecorder::system(self, message);
    }
}

/// Types into the session's transport directly, never through session
/// logging; `false` once the session no longer takes input.
pub type TerminalInputWriter = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;

pub struct LoginSequenceSessionOptions {
    pub sequence: LoginSequence,
    pub vault: Arc<dyn SecretVault>,
    pub write: TerminalInputWriter,
    pub recorder: Arc<dyn LoginSequenceRecorder>,
    /// Messages for the tab: progress and the master-password prompt.
    pub send: Arc<dyn Fn(TerminalServerMessage) + Send + Sync>,
}

/// A secret as keystrokes: its text, then Enter (a carriage return, as the key sends) when asked.
pub fn secret_keystrokes(value: &str, enter: bool) -> Vec<u8> {
    let mut keys = value.as_bytes().to_vec();
    if enter {
        keys.push(b'\r');
    }
    keys
}

type PendingAnswer = oneshot::Sender<Option<AuthPromptResponse>>;

struct Io {
    options: LoginSequenceSessionOptions,
    answer: Mutex<Option<PendingAnswer>>,
}

impl Io {
    fn answer(&self) -> MutexGuard<'_, Option<PendingAnswer>> {
        self.answer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn secret_name(&self, secret_id: &str) -> Option<String> {
        self.options.vault.secret_name(secret_id)
    }

    async fn ask_master_password(
        &self,
        secret_id: &str,
        error: Option<&str>,
        signal: &AbortSignal,
    ) -> Option<String> {
        if signal.is_aborted() {
            return None;
        }
        let name = self
            .secret_name(secret_id)
            .unwrap_or_else(|| "a saved secret".into());
        let instructions = format!(
            "The login sequence is about to type “{name}”. Enter the master password to use it."
        );
        let (tx, rx) = oneshot::channel();
        *self.answer() = Some(tx);
        (self.options.send)(TerminalServerMessage::AuthPrompt(AuthPromptInfo {
            name: Some("Unlock password vault".into()),
            purpose: Some(AuthPromptPurpose::VaultUnlock),
            instructions: Some(match error {
                Some(error) => format!("{error}\n\n{instructions}"),
                None => instructions,
            }),
            prompts: vec![AuthPromptField {
                prompt: "Master password".into(),
                echo: false,
            }],
            skip_label: Some("Stop login sequence".into()),
            ..AuthPromptInfo::default()
        }));
        let response = tokio::select! {
            response = rx => response.ok().flatten(),
            () = signal.aborted() => None,
        };
        *self.answer() = None;
        response
            .filter(|response| response.skipped != Some(true))
            .map(|response| response.answers.into_iter().next().unwrap_or_default())
    }
}

#[async_trait]
impl LoginSequenceIo for Io {
    fn write(&self, data: &[u8]) -> bool {
        (self.options.write)(data)
    }

    fn typed(&self, data: &[u8]) {
        self.options.recorder.input(data);
    }

    async fn type_secret(
        &self,
        secret_id: &str,
        enter: bool,
        abort: AbortSignal,
    ) -> Result<(), String> {
        let mut master_password: Option<String> = None;
        let mut error: Option<String> = None;
        let mut attempt = 0;
        loop {
            match self
                .options
                .vault
                .secret_value(secret_id, master_password.as_deref())
                .await
            {
                // The value goes from the vault straight to the session; it is
                // never logged or given to a session recorder.
                Ok(Some(value)) => {
                    return if (self.options.write)(&secret_keystrokes(&value, enter)) {
                        Ok(())
                    } else {
                        Err("the session no longer takes input.".into())
                    };
                }
                Ok(None) => return Err("its secret is no longer in the password vault.".into()),
                Err(VaultError::Corrupt) => return Err("its secret could not be decrypted.".into()),
                Err(VaultError::InvalidMasterPassword(message)) => error = Some(message),
                Err(VaultError::UnlockRequired) => {}
                Err(VaultError::Other(message)) => return Err(message),
            }
            if attempt >= MASTER_PASSWORD_ATTEMPTS {
                return Err("the password vault stayed locked.".into());
            }
            attempt += 1;
            master_password = self
                .ask_master_password(secret_id, error.as_deref(), &abort)
                .await;
            if abort.is_aborted() {
                return Ok(());
            }
            if master_password.is_none() {
                return Err("the password vault stayed locked.".into());
            }
        }
    }

    fn progress(&self, update: LoginSequenceProgress) {
        let LoginSequenceProgress {
            state,
            step,
            steps,
            detail,
            message,
        } = update;
        (self.options.send)(TerminalServerMessage::LoginSequence {
            state,
            step,
            steps,
            detail,
            message: message.clone(),
        });
        match state {
            LoginSequenceState::Failed => self.options.recorder.system(&format!(
                "Login sequence stopped: {}",
                message.as_deref().unwrap_or("a step failed.")
            )),
            LoginSequenceState::Cancelled => self.options.recorder.system(&format!(
                "Login sequence cancelled at step {step} of {steps}."
            )),
            LoginSequenceState::Running | LoginSequenceState::Done => {}
        }
    }
}

/// A login sequence attached to one terminal session: it reports progress
/// to the tab, takes the tab's cancel, and asks for the vault's master
/// password over the session's own prompt round-trip when a step needs a
/// locked vault. Plain text it sends counts as typed input; a vault secret
/// goes straight to the transport, so session history and log files only
/// ever hold what the remote side echoes back.
pub struct LoginSequenceSession {
    io: Arc<Io>,
    runner: Arc<LoginSequenceRunner>,
    started: AtomicBool,
}

impl LoginSequenceSession {
    pub fn new(options: LoginSequenceSessionOptions) -> Self {
        let steps = options.sequence.steps.clone();
        let vault = options.vault.clone();
        let io = Arc::new(Io {
            options,
            answer: Mutex::new(None),
        });
        let runner = Arc::new(LoginSequenceRunner::new(
            steps,
            io.clone(),
            Some(Arc::new(move |secret_id: &str| {
                vault.secret_name(secret_id)
            })),
            None,
        ));
        Self {
            io,
            runner,
            started: AtomicBool::new(false),
        }
    }

    /// Session output, from the moment the transport attached.
    pub fn output(&self, data: &[u8]) {
        self.runner.output(data);
    }

    /// Run the steps on the tokio runtime; output received since the session
    /// attached already counts. Starting twice does nothing.
    ///
    /// Like the TypeScript `async` call, the steps run right here up to their
    /// first wait, so the first step's `running` progress is reported before
    /// `start` returns (and before the session announces `ready`).
    pub fn start(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let runner = self.runner.clone();
        let mut run: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
            Box::pin(async move {
                runner.run().await;
            });
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        if run.as_mut().poll(&mut context).is_pending() {
            // The spawned task polls again at once with a real waker.
            tokio::spawn(run);
        }
    }

    /// The session ended.
    pub fn close(&self) {
        self.runner.close();
        if let Some(answer) = self.io.answer().take() {
            let _ = answer.send(None);
        }
    }

    /// Frames meant for the sequence: a cancel, or the answer to its
    /// master-password prompt. Returns whether the message was taken.
    pub fn handle_control(&self, message: &TerminalClientMessage) -> bool {
        match message {
            TerminalClientMessage::CancelLoginSequence => {
                self.runner.cancel();
                true
            }
            TerminalClientMessage::AuthResponse(response) => match self.io.answer().take() {
                Some(answer) => {
                    let _ = answer.send(Some(response.clone()));
                    true
                }
                None => false,
            },
            _ => false,
        }
    }
}
