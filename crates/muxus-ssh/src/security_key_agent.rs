//! Private ssh-agent for security key IdentityFiles (port of security-key-agent.ts).
//!
//! The key handle in a security key file only works together with the authenticator, through
//! OpenSSH's FIDO middleware. Rather than speak OpenSSH's internal ssh-sk-helper protocol or bind
//! libfido2, Muxus starts a private ssh-agent for the login, loads the file with ssh-add and signs
//! through the standard agent protocol. OpenSSH then handles the passphrase, the PIN (relayed to
//! the Muxus prompt through SSH_ASKPASS) and the touch exactly as it does for `ssh`. Nothing starts
//! until the server accepts the key, and the agent ends with the login.

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use russh::keys::HashAlg;
use russh::keys::agent::AgentIdentity;
use russh::keys::ssh_key::PublicKey;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Notify, oneshot, watch};
use tokio::task::JoinHandle;

use crate::responsive_agent::{
    AgentEndpoint, AgentHooks, AgentOperation, ResponsiveAgent, ResponsiveAgentOptions,
    identity_type,
};
use crate::security_keys::{
    SecurityKeyError, SecurityKeyQuestion, SecurityKeyQuestionKind, classify_askpass_prompt,
    security_key_type_name,
};

const MAX_PASSPHRASE_PROMPTS: u32 = 3;

/// Leaves room for the socket names within macOS's 104-byte socket path limit.
const MAX_SOCKET_DIR_LENGTH: usize = 90;

/// The agent's recent log, kept to explain a failed signature.
const MAX_AGENT_LOG: usize = 4096;

/// ssh-agent logs why a signature failed just before it answers; the log arrives on another pipe
/// than the answer, so a failed signature gives it this long to catch up.
const LOG_SETTLE: Duration = Duration::from_millis(100);

/// How long an OpenSSH tool gets to exit after SIGTERM before it is killed outright.
const TERMINATE_GRACE: Duration = Duration::from_secs(2);

/// How long to keep reading what a finished tool still has in its pipes.
const OUTPUT_DRAIN: Duration = Duration::from_secs(1);

/// Ends ssh-agent once its standard input closes, which happens however Muxus exits.
const WATCHDOG: &str = r#"cat >/dev/null; kill "$1" 2>/dev/null"#;

/// OpenSSH runs $SSH_ASKPASS with the prompt as its argument and reads the answer from its
/// standard output. The script inherits standard input from the OpenSSH tool that runs it, and
/// Muxus starts those tools with a socket there: the script writes the prompt (in hex, so any byte
/// survives) as one line and prints the answer line that comes back for its own process id. Once
/// Muxus is gone the socket is closed and the script fails, which OpenSSH treats as a cancelled
/// prompt. Only POSIX sh, od and tr are needed.
const ASKPASS_RELAY: &str = r#"#!/bin/sh
# Relays an OpenSSH SSH_ASKPASS prompt to Muxus over standard input and prints the answer.
# A touch notification (SSH_ASKPASS_PROMPT=none) needs no answer: Muxus shows its own.
[ "$SSH_ASKPASS_PROMPT" = none ] && exit 0
prompt=$(printf '%s' "$1" | od -A n -t x1 -v | tr -d ' \t\n')
printf 'ask %s %s\n' "$$" "$prompt" >&0 || exit 1
answer="answer $$ "
while IFS= read -r line; do
  case $line in
    "$answer"*) printf '%s\n' "${line#"$answer"}"; exit 0 ;;
    "skip $$") exit 1 ;;
  esac
done
exit 1
"#;

/// The OpenSSH programs that load and use the key: paths, or names looked up in `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSshTools {
    pub ssh_agent: PathBuf,
    pub ssh_add: PathBuf,
}

impl Default for OpenSshTools {
    fn default() -> Self {
        Self {
            ssh_agent: PathBuf::from("ssh-agent"),
            ssh_add: PathBuf::from("ssh-add"),
        }
    }
}

/// The user dismissed a prompt: the caller is cancelling the login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the prompt was cancelled")]
pub struct AskCancelled;

/// What [`SecurityKeyAsk::ask`] returns.
pub type AskFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<String>, AskCancelled>> + Send + 'a>>;

/// Asks the user for the key file's passphrase or the security key's PIN.
///
/// `Ok(None)` (or an empty answer) skips the key; `Err(AskCancelled)` means the login is being
/// cancelled. Either way OpenSSH sees a cancelled prompt.
pub trait SecurityKeyAsk: Send + Sync {
    fn ask(&self, question: SecurityKeyQuestion) -> AskFuture<'_>;
}

impl<F, Fut> SecurityKeyAsk for F
where
    F: Fn(SecurityKeyQuestion) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Option<String>, AskCancelled>> + Send + 'static,
{
    fn ask(&self, question: SecurityKeyQuestion) -> AskFuture<'_> {
        Box::pin(self(question))
    }
}

/// How to sign with one security key IdentityFile.
#[derive(Clone)]
pub struct SecurityKeyFileOptions {
    pub file: PathBuf,
    /// The key's public half, from the key file's header.
    pub public_key: PublicKey,
    /// What to offer the server instead, such as a certificate for the key.
    pub identity: Option<AgentIdentity>,
    /// Resolved SecurityKeyProvider; `None` uses OpenSSH's built-in support.
    pub provider: Option<String>,
    /// Asks for the key file's passphrase or the security key's PIN.
    pub ask: Arc<dyn SecurityKeyAsk>,
    /// Deadline, touch and signature hooks, as for the login agent.
    pub agent_options: ResponsiveAgentOptions,
    pub tools: OpenSshTools,
    /// `$XDG_RUNTIME_DIR`: holds the private directory when it is usable, else the temp
    /// directory does.
    pub runtime_dir: Option<PathBuf>,
}

impl SecurityKeyFileOptions {
    /// Options with the default tools and hooks, and `$XDG_RUNTIME_DIR` from the environment.
    pub fn new(
        file: impl Into<PathBuf>,
        public_key: PublicKey,
        ask: Arc<dyn SecurityKeyAsk>,
    ) -> Self {
        Self {
            file: file.into(),
            public_key,
            identity: None,
            provider: None,
            ask,
            agent_options: ResponsiveAgentOptions::default(),
            tools: OpenSshTools::default(),
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        }
    }
}

/// What ssh-agent logged about a failed signature, in the user's terms.
pub fn agent_failure_reason(log: &str) -> Option<&'static str> {
    let log = log.to_lowercase();
    if log.contains("internal security key support not enabled") {
        return Some(
            "this OpenSSH has no built-in security key support: set SecurityKeyProvider to a FIDO middleware library, or use an OpenSSH built with FIDO support",
        );
    }
    if log.contains("device not found") {
        return Some("no security key is plugged in");
    }
    if log.contains("incorrect passphrase") || log.contains("wrong passphrase") {
        return Some("the PIN was not accepted");
    }
    None
}

/// An OpenSSH tool's complaint without its decorative banner lines.
fn process_reason(stderr: &str) -> String {
    stderr
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('@'))
        .collect::<Vec<_>>()
        .join(" ")
}

fn plain_error(message: impl Into<String>) -> SecurityKeyError {
    SecurityKeyError {
        message: message.into(),
        key: None,
    }
}

fn login_ended() -> SecurityKeyError {
    plain_error("the login ended")
}

/// Signs with a security key IdentityFile through a private ssh-agent that lives as long as this
/// value (see the module docs). Dropping it disposes it.
pub struct SecurityKeyFileAgent {
    shared: Arc<Shared>,
    /// What the server is offered.
    identity: AgentIdentity,
    /// The offered identity's type, reported for every signature in place of the plain key's.
    signature_type: Option<String>,
    provider: Option<String>,
    tools: OpenSshTools,
    runtime_dir: Option<PathBuf>,
    operation_timeout: Option<Duration>,
    session: tokio::sync::OnceCell<Result<ResponsiveAgent, SecurityKeyError>>,
}

/// State shared with the tasks that serve the OpenSSH tools.
struct Shared {
    file: PathBuf,
    public_key: PublicKey,
    ask: Arc<dyn SecurityKeyAsk>,
    hooks: Arc<dyn AgentHooks>,
    state: Mutex<State>,
    log_changed: Notify,
    disposed: watch::Sender<bool>,
}

#[derive(Default)]
struct State {
    disposed: bool,
    directory: Option<PathBuf>,
    /// Prompt relays, ended on dispose.
    relays: Vec<JoinHandle<()>>,
    /// Closing it ends the watchdog, and the watchdog ends the agent.
    watchdog_stdin: Option<ChildStdin>,
    agent_exited: bool,
    agent_log: Vec<u8>,
    passphrase_prompts: u32,
    pin_entered: bool,
    last_question: Option<SecurityKeyQuestion>,
    /// OpenSSH processes (and watchdogs) not reaped yet.
    children: usize,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn name(&self) -> String {
        self.file.file_name().map_or_else(
            || self.file.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    fn agent_hint(&self) -> String {
        format!(
            "load it into your SSH agent with \"ssh-add {}\"",
            self.file.display()
        )
    }

    fn append_log(&self, chunk: &[u8]) {
        {
            let mut state = self.state();
            state.agent_log.extend_from_slice(chunk);
            let excess = state.agent_log.len().saturating_sub(MAX_AGENT_LOG);
            state.agent_log.drain(..excess);
        }
        self.log_changed.notify_waiters();
    }

    fn agent_log(&self) -> String {
        String::from_utf8_lossy(&self.state().agent_log).into_owned()
    }

    /// Why the private agent did not sign: its log, else a PIN that was entered.
    async fn failure_reason(&self) -> Option<String> {
        let deadline = tokio::time::Instant::now() + LOG_SETTLE;
        loop {
            let changed = self.log_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let (reason, pin_entered) = {
                let state = self.state();
                (
                    agent_failure_reason(&String::from_utf8_lossy(&state.agent_log)),
                    state.pin_entered,
                )
            };
            if let Some(reason) = reason {
                return Some(reason.to_string());
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                // The private agent asks for the PIN at most once per signature.
                return pin_entered
                    .then(|| "the PIN was wrong, or it was not touched in time".to_string());
            }
        }
    }

    async fn answer(&self, question: SecurityKeyQuestion) -> Option<String> {
        {
            let mut state = self.state();
            if state.disposed {
                return None;
            }
            // ssh-add asks again after every wrong passphrase; stop where ssh(1) does.
            if question.kind == SecurityKeyQuestionKind::Passphrase {
                state.passphrase_prompts += 1;
                if state.passphrase_prompts > MAX_PASSPHRASE_PROMPTS {
                    return None;
                }
            }
            state.last_question = Some(question.clone());
        }
        let (kind, presence) = (question.kind, question.presence);
        // A dismissed prompt means the caller is cancelling the login.
        let answer = self
            .ask
            .ask(question)
            .await
            .ok()
            .flatten()
            .filter(|answer| !answer.is_empty())?;
        if kind == SecurityKeyQuestionKind::Pin {
            self.state().pin_entered = true;
        }
        if presence {
            self.hooks.on_security_key(&self.public_key);
        }
        Some(answer)
    }

    fn track(self: &Arc<Self>, child: Child) -> Tracked {
        self.state().children += 1;
        Tracked {
            child,
            shared: self.clone(),
        }
    }

    fn keep_relay(&self, relay: JoinHandle<()>) {
        let mut state = self.state();
        if state.disposed {
            relay.abort();
        } else {
            state.relays.push(relay);
        }
    }
}

/// A child process that is terminated once the agent is disposed.
struct Tracked {
    child: Child,
    shared: Arc<Shared>,
}

impl Tracked {
    async fn wait(mut self) -> Option<ExitStatus> {
        let mut disposed = self.shared.disposed.subscribe();
        tokio::select! {
            status = self.child.wait() => return status.ok(),
            _ = disposed.wait_for(|disposed| *disposed) => {}
        }
        terminate(&self.child);
        match tokio::time::timeout(TERMINATE_GRACE, self.child.wait()).await {
            Ok(status) => status.ok(),
            Err(_) => {
                let _ = self.child.start_kill();
                self.child.wait().await.ok()
            }
        }
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        let mut state = self.shared.state();
        state.children = state.children.saturating_sub(1);
    }
}

#[cfg(unix)]
fn terminate(child: &Child) {
    // A child that has not been reaped keeps its pid, so this cannot hit another process.
    if let Some(pid) = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok()) {
        // SAFETY: kill(2) only sends a signal.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
}

#[cfg(not(unix))]
fn terminate(_child: &Child) {}

/// Pauses the dial deadline until dropped.
struct DeadlinePause<'a>(&'a dyn AgentHooks);

impl<'a> DeadlinePause<'a> {
    fn new(hooks: &'a dyn AgentHooks) -> Self {
        hooks.pause_deadline();
        Self(hooks)
    }
}

impl Drop for DeadlinePause<'_> {
    fn drop(&mut self) {
        self.0.resume_deadline();
    }
}

/// The caller's hooks, reporting signatures as made by the offered identity.
struct PrivateAgentHooks {
    inner: Arc<dyn AgentHooks>,
    signature_type: Option<String>,
}

impl AgentHooks for PrivateAgentHooks {
    fn pause_deadline(&self) {
        self.inner.pause_deadline();
    }
    fn resume_deadline(&self) {
        self.inner.resume_deadline();
    }
    fn on_waiting(&self, operation: AgentOperation) {
        self.inner.on_waiting(operation);
    }
    fn on_security_key(&self, key: &PublicKey) {
        self.inner.on_security_key(key);
    }
    fn on_signature(&self, algorithm: &str) {
        self.inner
            .on_signature(self.signature_type.as_deref().unwrap_or(algorithm));
    }
}

struct Added {
    code: Option<i32>,
    stderr: String,
}

enum AgentStartError {
    Spawn(std::io::Error),
    Exited(String),
}

impl SecurityKeyFileAgent {
    pub fn new(options: SecurityKeyFileOptions) -> Self {
        let SecurityKeyFileOptions {
            file,
            public_key,
            identity,
            provider,
            ask,
            agent_options,
            tools,
            runtime_dir,
        } = options;
        let signature_type = identity.as_ref().map(identity_type);
        let identity = identity.unwrap_or_else(|| AgentIdentity::PublicKey {
            key: public_key.clone(),
            comment: public_key.comment().as_str_lossy().to_string(),
        });
        let (disposed, _) = watch::channel(false);
        Self {
            shared: Arc::new(Shared {
                file,
                public_key,
                ask,
                hooks: agent_options.hooks.clone(),
                state: Mutex::new(State::default()),
                log_changed: Notify::new(),
                disposed,
            }),
            identity,
            signature_type,
            provider,
            tools,
            runtime_dir,
            operation_timeout: agent_options.operation_timeout,
            session: tokio::sync::OnceCell::new(),
        }
    }

    /// The identity offered to the server: the certificate when there is one, else the key.
    pub fn identities(&self) -> Vec<AgentIdentity> {
        vec![self.identity.clone()]
    }

    /// The key file this agent signs with.
    pub fn file(&self) -> &Path {
        &self.shared.file
    }

    /// The security key's public half.
    pub fn public_key(&self) -> &PublicKey {
        &self.shared.public_key
    }

    /// Signs `data` with the security key, starting the private agent on first use. As from an
    /// agent signer, the result is `data` followed by the signature.
    // SecurityKeyError carries the key that failed, as the rest of the crate expects.
    #[allow(clippy::result_large_err)]
    pub async fn sign(
        &self,
        hash: Option<HashAlg>,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, SecurityKeyError> {
        // Loading the key, passphrase prompt included, is user time as well.
        let paused = DeadlinePause::new(&*self.shared.hooks);
        let session = self.session.get_or_init(|| self.start()).await;
        drop(paused);
        let client = session.as_ref().map_err(Clone::clone)?;
        {
            let mut state = self.shared.state();
            state.pin_entered = false;
            state.agent_log.clear();
        }
        // The agent holds the plain key; a certificate is signed by that key.
        let plain = AgentIdentity::PublicKey {
            key: self.shared.public_key.clone(),
            comment: String::new(),
        };
        match client.sign(&plain, hash, data).await {
            Ok(signature) => Ok(signature),
            Err(err) => match err.security_key {
                Some(key) => Err(SecurityKeyError {
                    message: self.shared.failure_reason().await.unwrap_or(err.message),
                    key: Some(*key),
                }),
                None => Err(plain_error(err.message)),
            },
        }
    }

    /// Ends the private agent and removes its directory. Safe to call more than once.
    pub fn dispose(&self) {
        let (directory, relays, watchdog) = {
            let mut state = self.shared.state();
            if state.disposed {
                return;
            }
            state.disposed = true;
            (
                state.directory.take(),
                std::mem::take(&mut state.relays),
                state.watchdog_stdin.take(),
            )
        };
        self.shared.disposed.send_replace(true);
        for relay in relays {
            relay.abort();
        }
        drop(watchdog);
        if let Some(directory) = directory {
            let _ = std::fs::remove_dir_all(directory);
        }
    }

    #[cfg(not(unix))]
    #[allow(clippy::result_large_err)]
    async fn start(&self) -> Result<ResponsiveAgent, SecurityKeyError> {
        let key_type = security_key_type_name(self.shared.public_key.algorithm().as_str());
        Err(plain_error(format!(
            "{} is a security key ({key_type}). To use it, {}",
            self.shared.name(),
            self.shared.agent_hint()
        )))
    }

    #[cfg(unix)]
    #[allow(clippy::result_large_err)]
    async fn start(&self) -> Result<ResponsiveAgent, SecurityKeyError> {
        let shared = &self.shared;
        let key_type = security_key_type_name(shared.public_key.algorithm().as_str());
        if shared.state().disposed {
            return Err(login_ended());
        }
        let name = shared.name();
        let setup_failed = |err: std::io::Error| {
            plain_error(format!(
                "could not start an ssh-agent for the security key {name}: {err}"
            ))
        };
        let root = private_directory_root(self.runtime_dir.as_deref(), &std::env::temp_dir());
        let directory = create_private_directory(&root).map_err(setup_failed)?;
        {
            let mut state = shared.state();
            if state.disposed {
                drop(state);
                let _ = std::fs::remove_dir_all(&directory);
                return Err(login_ended());
            }
            // Left behind by a start that was abandoned halfway.
            if let Some(stale) = state.directory.replace(directory.clone()) {
                let _ = std::fs::remove_dir_all(stale);
            }
        }
        let agent_socket = directory.join("agent.sock");
        let askpass = directory.join("askpass");
        write_executable(&askpass, ASKPASS_RELAY).map_err(setup_failed)?;

        let not_found = |tool: &str| {
            plain_error(format!(
                "{name} is a security key ({key_type}). Muxus signs with it through OpenSSH's {tool}, which was not found: install the OpenSSH client, or {}",
                shared.agent_hint()
            ))
        };
        match self.start_agent(&agent_socket, &askpass).await {
            Ok(()) => {}
            Err(AgentStartError::Spawn(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(not_found("ssh-agent"));
            }
            Err(_) if shared.state().disposed => return Err(login_ended()),
            Err(AgentStartError::Spawn(err)) => {
                return Err(plain_error(format!(
                    "could not start an ssh-agent for the security key {name}: {err}"
                )));
            }
            Err(AgentStartError::Exited(reason)) => {
                return Err(plain_error(format!(
                    "could not start an ssh-agent for the security key {name}: {reason}"
                )));
            }
        }

        let added = match self.run_ssh_add(&agent_socket, &askpass).await {
            Ok(added) => added,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(not_found("ssh-add"));
            }
            Err(err) => {
                return Err(plain_error(format!(
                    "could not load the security key {name}: {err}"
                )));
            }
        };
        if shared.state().disposed {
            return Err(login_ended());
        }
        if added.code != Some(0) {
            let reason = process_reason(&added.stderr);
            let retry = shared
                .state()
                .last_question
                .as_ref()
                .is_some_and(|question| question.retry);
            return Err(plain_error(if !reason.is_empty() {
                format!("could not load the security key {name}: {reason}")
            } else if retry {
                format!("could not load the security key {name}: the passphrase was not accepted")
            } else {
                format!("skipped the security key {name}: no passphrase was entered")
            }));
        }
        Ok(ResponsiveAgent::new(
            AgentEndpoint::Socket(agent_socket),
            ResponsiveAgentOptions {
                hooks: Arc::new(PrivateAgentHooks {
                    inner: shared.hooks.clone(),
                    signature_type: self.signature_type.clone(),
                }),
                wait_status: None,
                operation_timeout: self.operation_timeout,
                // The private agent is ours and answers once OpenSSH gives up on the
                // authenticator; a PIN prompt may legitimately stay open for a while.
                security_key_timeout: None,
            },
        ))
    }

    /// ssh-agent runs in the foreground, so its log explains a failed signature. A watchdog
    /// (`cat` reading a pipe from Muxus) ends it if Muxus itself is killed, so the agent can
    /// never outlive Muxus.
    #[cfg(unix)]
    async fn start_agent(&self, socket: &Path, askpass: &Path) -> Result<(), AgentStartError> {
        let shared = &self.shared;
        let mut args: Vec<OsString> = vec!["-D".into(), "-a".into(), socket.into()];
        if let Some(provider) = &self.provider {
            // ssh-add reports a missing provider.
            let allowed =
                std::fs::canonicalize(provider).unwrap_or_else(|_| PathBuf::from(provider));
            args.push("-P".into());
            args.push(allowed.into());
        }
        let (relay, stdin) = relay_pair().map_err(AgentStartError::Spawn)?;
        let mut command = openssh_command(&self.tools.ssh_agent, askpass, self.provider.as_deref());
        command
            .args(&args)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(AgentStartError::Spawn)?;
        // Closes our copy of the child's end of the prompt socket.
        drop(command);
        shared.keep_relay(tokio::spawn(relay_prompts(shared.clone(), relay)));

        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let tracked = shared.track(child);
        let log = tokio::spawn({
            let shared = shared.clone();
            async move {
                let Some(mut stderr) = stderr else { return };
                let mut buffer = [0u8; 1024];
                while let Ok(read @ 1..) = stderr.read(&mut buffer).await {
                    shared.append_log(&buffer[..read]);
                }
            }
        });
        let (ready_tx, ready_rx) = oneshot::channel();
        tokio::spawn(watch_agent_stdout(stdout, ready_tx));
        let (exited_tx, exited_rx) = oneshot::channel();
        tokio::spawn({
            let shared = shared.clone();
            async move {
                let status = tracked.wait().await;
                let _ = tokio::time::timeout(OUTPUT_DRAIN, log).await;
                let watchdog = {
                    let mut state = shared.state();
                    state.agent_exited = true;
                    state.watchdog_stdin.take()
                };
                // The agent is gone, and with it the watchdog's reason to wait.
                drop(watchdog);
                let _ = exited_tx.send(status);
            }
        });

        if ready_rx.await.is_ok() {
            if let Some(pid) = pid {
                self.start_watchdog(pid);
            }
            return Ok(());
        }
        let status = exited_rx.await.ok().flatten();
        let reason = process_reason(&shared.agent_log());
        Err(AgentStartError::Exited(if reason.is_empty() {
            format!(
                "ssh-agent exited with status {}",
                status
                    .and_then(|status| status.code())
                    .map_or_else(|| "null".to_string(), |code| code.to_string())
            )
        } else {
            reason
        }))
    }

    #[cfg(unix)]
    fn start_watchdog(&self, agent_pid: u32) {
        let spawned = Command::new("/bin/sh")
            .arg("-c")
            .arg(WATCHDOG)
            .arg("sh")
            .arg(agent_pid.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();
        let Ok(mut watchdog) = spawned else { return };
        let stdin = watchdog.stdin.take();
        {
            let mut state = self.shared.state();
            if !state.agent_exited && !state.disposed {
                state.watchdog_stdin = stdin;
            }
        }
        tokio::spawn(self.shared.track(watchdog).wait());
    }

    #[cfg(unix)]
    async fn run_ssh_add(&self, agent_socket: &Path, askpass: &Path) -> std::io::Result<Added> {
        let shared = &self.shared;
        let (relay, stdin) = relay_pair()?;
        let mut command = openssh_command(&self.tools.ssh_add, askpass, self.provider.as_deref());
        command
            .arg("--")
            .arg(&shared.file)
            .env("SSH_AUTH_SOCK", agent_socket)
            .stdin(stdin)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        drop(command);
        shared.keep_relay(tokio::spawn(relay_prompts(shared.clone(), relay)));
        let stderr = child.stderr.take();
        let collect = tokio::spawn(async move {
            let mut out = Vec::new();
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_end(&mut out).await;
            }
            out
        });
        let status = shared.track(child).wait().await;
        let stderr = match tokio::time::timeout(OUTPUT_DRAIN, collect).await {
            Ok(Ok(out)) => String::from_utf8_lossy(&out).into_owned(),
            _ => String::new(),
        };
        Ok(Added {
            code: status.and_then(|status| status.code()),
            stderr,
        })
    }

    #[cfg(test)]
    fn running_children(&self) -> usize {
        self.shared.state().children
    }

    #[cfg(test)]
    fn agent_exited(&self) -> bool {
        self.shared.state().agent_exited
    }

    /// Closes the watchdog's pipe the way the kernel does when Muxus dies.
    #[cfg(test)]
    fn close_watchdog_pipe(&self) -> bool {
        self.shared.state().watchdog_stdin.take().is_some()
    }
}

impl Drop for SecurityKeyFileAgent {
    fn drop(&mut self) {
        self.dispose();
    }
}

/// The OpenSSH environment: prompts go through the relay, and nothing of the user's own agent
/// leaks in.
#[cfg(unix)]
fn openssh_command(program: &Path, askpass: &Path, provider: Option<&str>) -> Command {
    let mut command = Command::new(program);
    command
        .env("SSH_ASKPASS", askpass)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env_remove("SSH_AUTH_SOCK")
        .env_remove("SSH_AGENT_PID")
        .env_remove("SSH_SK_PROVIDER")
        .kill_on_drop(true);
    if let Some(provider) = provider {
        command.env("SSH_SK_PROVIDER", provider);
    }
    command
}

/// A socket pair: our end, and the other one as a child's standard input.
#[cfg(unix)]
fn relay_pair() -> std::io::Result<(tokio::net::UnixStream, Stdio)> {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
    ours.set_nonblocking(true)?;
    let ours = tokio::net::UnixStream::from_std(ours)?;
    Ok((ours, Stdio::from(std::os::fd::OwnedFd::from(theirs))))
}

/// Answers the askpass prompts of one OpenSSH tool (and of every askpass it starts).
#[cfg(unix)]
async fn relay_prompts(shared: Arc<Shared>, stream: tokio::net::UnixStream) {
    let (read, mut write) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(1..) => {}
            _ => return,
        }
        let Some((pid, prompt)) = parse_askpass_request(&line) else {
            continue;
        };
        let answer = match prompt {
            Some(prompt) => shared.answer(classify_askpass_prompt(&prompt)).await,
            None => None,
        };
        let reply = match answer {
            Some(answer) => format!("answer {pid} {}\n", first_line(&answer)),
            None => format!("skip {pid}\n"),
        };
        if write.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// `ask <pid> <hex prompt>`: the asking process and its prompt (`None` when unreadable).
fn parse_askpass_request(line: &[u8]) -> Option<(u32, Option<String>)> {
    let line = std::str::from_utf8(line)
        .ok()?
        .trim_end_matches(['\n', '\r']);
    let rest = line.strip_prefix("ask ")?;
    let (pid, hex) = rest.split_once(' ').unwrap_or((rest, ""));
    let pid = pid.parse().ok()?;
    let prompt = hex::decode(hex)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    Some((pid, prompt))
}

/// What OpenSSH keeps of an askpass answer: everything before the first line break.
fn first_line(answer: &str) -> &str {
    answer.split(['\n', '\r', '\0']).next().unwrap_or_default()
}

/// Waits for ssh-agent's "Agent pid", printed once the socket listens, and drains the rest.
async fn watch_agent_stdout(
    stdout: Option<tokio::process::ChildStdout>,
    ready: oneshot::Sender<()>,
) {
    let Some(mut stdout) = stdout else { return };
    let mut ready = Some(ready);
    let mut seen = Vec::new();
    let mut buffer = [0u8; 1024];
    while let Ok(read @ 1..) = stdout.read(&mut buffer).await {
        let Some(tx) = ready.take() else { continue };
        seen.extend_from_slice(&buffer[..read]);
        if agent_ready(&String::from_utf8_lossy(&seen)) {
            let _ = tx.send(());
        } else {
            ready = Some(tx);
            let excess = seen.len().saturating_sub(MAX_AGENT_LOG);
            seen.drain(..excess);
        }
    }
}

/// `/Agent pid \d+/`
fn agent_ready(stdout: &str) -> bool {
    stdout
        .match_indices("Agent pid ")
        .any(|(at, found)| stdout[at + found.len()..].starts_with(|c: char| c.is_ascii_digit()))
}

/// Where the private directory goes: a per-user runtime directory when there is one, else the
/// temp directory, and /tmp when either would make the socket paths too long.
fn private_directory_root(runtime: Option<&Path>, temp: &Path) -> PathBuf {
    let root = match runtime {
        Some(runtime) if runtime.is_absolute() && runtime.exists() => runtime,
        _ => temp,
    };
    if root.join("muxus-sk-XXXXXX").as_os_str().len() > MAX_SOCKET_DIR_LENGTH {
        PathBuf::from("/tmp")
    } else {
        root.to_path_buf()
    }
}

/// `mkdtemp(<root>/muxus-sk-XXXXXX)`, open to the user alone.
#[cfg(unix)]
fn create_private_directory(root: &Path) -> std::io::Result<PathBuf> {
    use rand::Rng;
    use std::os::unix::fs::DirBuilderExt;
    let mut rng = rand::thread_rng();
    loop {
        let suffix: String = (0..6)
            .map(|_| char::from(rng.sample(rand::distributions::Alphanumeric)))
            .collect();
        let path = root.join(format!("muxus-sk-{suffix}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn write_executable(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(path)?;
    file.write_all(content.as_bytes())
}

/// Failure while authenticating with a security key file.
#[derive(Debug, thiserror::Error)]
pub enum SecurityKeyAuthError {
    #[error("the SSH session closed during authentication")]
    Send(#[from] russh::SendError),
    #[error(transparent)]
    SecurityKey(#[from] SecurityKeyError),
}

/// Signs login challenges through a [`SecurityKeyFileAgent`].
pub struct SecurityKeySigner<'a>(pub &'a SecurityKeyFileAgent);

impl russh::Signer for SecurityKeySigner<'_> {
    type Error = SecurityKeyAuthError;

    fn auth_sign(
        &mut self,
        _key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send {
        let agent = self.0;
        async move { Ok(agent.sign(hash_alg, to_sign).await?) }
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests;
