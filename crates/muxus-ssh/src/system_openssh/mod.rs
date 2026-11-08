//! Hosts set to "Use system OpenSSH" (`server/src/ssh/system-openssh.ts`).
//!
//! Such a host connects through the `ssh` program of the computer rather than the built-in
//! client. The terminal is `ssh` under a PTY, so every prompt and error is OpenSSH's own and all
//! authentication it knows works, GSSAPI included. The PTY belongs to the session layer, which
//! starts [`SystemOpenSshSession::terminal_args`] and reports the terminal's end here.
//!
//! The file browser, status bar and container discovery share that login over a private control
//! socket. Windows OpenSSH has no connection sharing, so sessions there are terminal only.

mod args;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use muxus_core::platform::Platform;
use muxus_core::protocol::SshProfile;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

pub use args::{
    COMMAND_PROBE, COMMAND_PROBE_ANSWER, CONTROL_PERSIST_SECONDS, SshDestination,
    command_probe_args, control_exit_args, find_ssh_binary, saved_host_options, script_ssh_args,
    sftp_ssh_args, ssh_destination, terminal_ssh_args,
};

use crate::connection::BoxFuture;
use crate::connection::manager::SftpChannel;
use crate::sftp::SftpSource;
use crate::types::saved_host_hop_id;

/// How long a file request waits for the login happening in the terminal.
const LOGIN_WAIT: Duration = Duration::from_secs(30);
const LOGIN_POLL: Duration = Duration::from_millis(200);
const SFTP_START_TIMEOUT: Duration = Duration::from_secs(20);
const CONTROL_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
/// `ssh` exits with this for its own failures; otherwise it passes on the remote status.
pub const SSH_OWN_FAILURE: i32 = 255;
/// What the tab says when `ssh` itself failed; its own words are already in the terminal.
pub const SSH_OWN_FAILURE_MESSAGE: &str =
    "ssh could not connect, or lost the connection. Its own message is in the terminal above.";
/// Longest a script run over the shared login may take, and how much it may print.
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(60);
const SCRIPT_MAX_OUTPUT: usize = 16 * 1024 * 1024;
const COMMAND_PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// Per-request timeout of the SFTP session, as on a built-in connection.
const SFTP_REQUEST_TIMEOUT_SECS: u64 = 60;

/// Why a session could not start.
pub const NO_SSH_PROGRAM: &str = "This host uses system OpenSSH, but no ssh program was found. Install the OpenSSH client or turn the option off in the host settings.";
pub const PINNED_HOST_KEY: &str =
    "This link pins a host key, which system OpenSSH sessions cannot verify.";
pub const SAVED_JUMP_HOST: &str = "This host jumps through a host saved in Muxus, which the ssh program cannot reach. Use a jump host from your SSH config, or turn system OpenSSH off.";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How sessions run `ssh`.
#[derive(Debug, Clone, Default)]
pub struct SessionTools {
    /// The `ssh` program; found on PATH when absent.
    pub binary: Option<PathBuf>,
    /// Put before every other argument.
    pub extra_args: Vec<String>,
    /// The environment of every `ssh` this session starts (the local terminals' environment).
    pub env: HashMap<String, String>,
    /// Where control sockets go. Socket paths are limited to about 100 bytes and a TMPDIR can
    /// use most of that, so this is `/tmp` unless a test says otherwise.
    pub control_root: Option<PathBuf>,
}

/// One job's end: its exit status and what it printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

struct State {
    terminal_running: bool,
    leases: usize,
    finished: bool,
    kill_terminal: Option<Box<dyn FnOnce() + Send>>,
}

/// A terminal running the computer's `ssh` program, and the login it shares.
pub struct SystemOpenSshSession {
    pub id: String,
    /// Whether the file browser, statistics and commands can share the terminal's login.
    pub sftp_available: bool,
    profile: SshProfile,
    binary: PathBuf,
    extra_args: Vec<String>,
    env: HashMap<String, String>,
    control_dir: Option<PathBuf>,
    control_path: Option<PathBuf>,
    state: Mutex<State>,
    closed: watch::Sender<bool>,
    sftp: tokio::sync::Mutex<Option<(Arc<SftpChannel>, tokio::process::Child)>>,
    command_probe: tokio::sync::Mutex<Option<bool>>,
    registry: Weak<Mutex<HashMap<String, Arc<SystemOpenSshSession>>>>,
}

impl std::fmt::Debug for SystemOpenSshSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemOpenSshSession")
            .field("id", &self.id)
            .field("sftp_available", &self.sftp_available)
            .finish_non_exhaustive()
    }
}

fn random_suffix(length: usize) -> String {
    use rand::RngExt;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..length)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

/// `mkdtemp(root/muxus-ssh-XXXXXX)`: a fresh directory only this user can enter.
fn private_dir(root: &Path) -> std::io::Result<PathBuf> {
    for _ in 0..16 {
        let dir = root.join(format!("muxus-ssh-{}", random_suffix(6)));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("no free control directory name"))
}

impl SystemOpenSshSession {
    /// The `ssh` program the terminal runs.
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Everything but the program name for the terminal's `ssh`.
    pub fn terminal_args(&self) -> Vec<String> {
        let mut args = self.extra_args.clone();
        args.extend(terminal_ssh_args(
            &self.profile,
            self.control_path.as_deref(),
        ));
        args
    }

    /// The environment the terminal's `ssh` runs with.
    pub fn env(&self) -> &HashMap<String, String> {
        &self.env
    }

    /// The private control socket, when the login is shared.
    pub fn control_path(&self) -> Option<&Path> {
        self.control_path.as_deref()
    }

    /// How the session ends its terminal (kills the PTY).
    pub fn set_terminal_killer(&self, kill: impl FnOnce() + Send + 'static) {
        lock(&self.state).kill_terminal = Some(Box::new(kill));
    }

    /// The terminal's `ssh` exited. File transfers still holding a lease finish first.
    pub fn terminal_ended(&self) {
        lock(&self.state).terminal_running = false;
        self.finish_if_unused();
    }

    /// End the terminal. File transfers still holding a lease finish first.
    pub fn close(&self) {
        let kill = {
            let mut state = lock(&self.state);
            state.terminal_running = false;
            state.kill_terminal.take()
        };
        if let Some(kill) = kill {
            kill();
        }
        self.finish_if_unused();
    }

    /// End everything before returning, leases or not: the backend is shutting down.
    pub fn destroy(&self) {
        let kill = {
            let mut state = lock(&self.state);
            state.terminal_running = false;
            state.kill_terminal.take()
        };
        if let Some(kill) = kill {
            kill();
        }
        self.finish(true);
    }

    pub fn is_finished(&self) -> bool {
        lock(&self.state).finished
    }

    /// Completes once the session has finished: its terminal ended and no lease holds it.
    pub async fn closed(&self) {
        let mut rx = self.closed.subscribe();
        let _ = rx.wait_for(|closed| *closed).await;
    }

    /// A hold on the session for a file operation, a window or a reading.
    pub fn acquire(self: &Arc<Self>) -> Option<SystemOpenSshLease> {
        let mut state = lock(&self.state);
        if state.finished {
            return None;
        }
        state.leases += 1;
        Some(SystemOpenSshLease {
            session: self.clone(),
            released: AtomicBool::new(false),
        })
    }

    fn release(&self) {
        {
            let mut state = lock(&self.state);
            state.leases = state.leases.saturating_sub(1);
        }
        self.finish_if_unused();
    }

    fn finish_if_unused(&self) {
        let unused = {
            let state = lock(&self.state);
            !state.terminal_running && state.leases == 0
        };
        if unused {
            self.finish(false);
        }
    }

    fn finish(&self, wait_for_stop: bool) {
        {
            let mut state = lock(&self.state);
            if state.finished {
                return;
            }
            state.finished = true;
        }
        // Running jobs watch this and end their `ssh`.
        self.closed.send_replace(true);
        if let Ok(mut sftp) = self.sftp.try_lock()
            && let Some((_, mut child)) = sftp.take()
        {
            let _ = child.start_kill();
        }
        if let Some(registry) = self.registry.upgrade() {
            lock(&registry).remove(&self.id);
        }
        self.stop_shared_connection(wait_for_stop);
    }

    fn stop_shared_connection(&self, wait: bool) {
        let (Some(dir), Some(path)) = (self.control_dir.clone(), self.control_path.clone()) else {
            return;
        };
        let remove_dir = move || {
            let _ = std::fs::remove_dir_all(&dir);
        };
        if !path.exists() {
            remove_dir();
            return;
        }
        let mut args = self.extra_args.clone();
        args.extend(control_exit_args(&self.profile, &path));
        let mut command = std::process::Command::new(&self.binary);
        command
            .args(&args)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut stop = move || {
            if let Ok(mut child) = command.spawn() {
                let deadline = Instant::now() + CONTROL_EXIT_TIMEOUT;
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) if Instant::now() >= deadline => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                    }
                }
            }
            remove_dir();
        };
        if wait {
            stop();
        } else {
            let _ = std::thread::Builder::new()
                .name("muxus-ssh-control-exit".into())
                .spawn(stop);
        }
    }

    fn command(&self, args: &[String]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(&self.extra_args)
            .args(args)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    /// Run one job to its end, for its output and exit status.
    async fn run_job(
        &self,
        args: &[String],
        input: &str,
        timeout: Duration,
    ) -> Result<JobOutput, String> {
        let mut child = self.command(args).spawn().map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let input = input.as_bytes().to_vec();
        // A job that exits early must not fail on what is still being written to it.
        let writer = tokio::spawn(async move {
            if let Some(stdin) = &mut stdin {
                let _ = stdin.write_all(&input).await;
                let _ = stdin.shutdown().await;
            }
        });
        let read_out = async {
            let mut out = Vec::new();
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match stdout.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out.len() + n <= SCRIPT_MAX_OUTPUT {
                            out.extend_from_slice(&buffer[..n]);
                        }
                    }
                }
            }
            out
        };
        let read_err = async {
            let mut err = Vec::new();
            let _ = stderr.read_to_end(&mut err).await;
            err
        };
        let mut closed = self.closed.subscribe();
        let run = async {
            let (out, err) = tokio::join!(read_out, read_err);
            let status = child.wait().await;
            (out, err, status)
        };
        let result = tokio::select! {
            result = tokio::time::timeout(timeout, run) => result.ok(),
            _ = closed.wait_for(|closed| *closed) => None,
        };
        writer.abort();
        let Some((out, err, status)) = result else {
            // Dropping the child kills it.
            return Ok(JobOutput {
                code: None,
                stdout: String::new(),
                stderr: String::new(),
            });
        };
        let stderr = String::from_utf8_lossy(&err).into_owned();
        let tail: String = {
            let chars: Vec<char> = stderr.chars().collect();
            chars[chars.len().saturating_sub(2048)..].iter().collect()
        };
        Ok(JobOutput {
            code: status.ok().and_then(|s| s.code()),
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: tail.trim().to_string(),
        })
    }

    fn terminal_running(&self) -> bool {
        lock(&self.state).terminal_running
    }

    /// The socket appears once the login in the terminal has succeeded.
    async fn wait_for_login(&self, control_path: &Path) -> Result<(), String> {
        let deadline = Instant::now() + LOGIN_WAIT;
        while !control_path.exists() {
            if !self.terminal_running() {
                return Err("The terminal session has ended.".into());
            }
            if Instant::now() >= deadline {
                return Err("Finish logging in in the terminal, then open the files again.".into());
            }
            tokio::time::sleep(LOGIN_POLL).await;
        }
        Ok(())
    }

    /// Whether scripts may run here; automatic readings ask first, as they do on built-in
    /// connections. One question per session, however often the status bar polls.
    pub async fn runs_commands(&self) -> Result<bool, String> {
        let Some(control_path) = self.control_path.clone() else {
            return Ok(false);
        };
        let mut probe = self.command_probe.lock().await;
        if let Some(answer) = *probe {
            return Ok(answer);
        }
        // Not logged in yet says nothing about the host, so that one is asked again.
        self.wait_for_login(&control_path).await?;
        let output = self
            .run_job(
                &command_probe_args(&self.profile),
                "",
                COMMAND_PROBE_TIMEOUT,
            )
            .await?;
        let answer = output.code == Some(0) && output.stdout.contains(COMMAND_PROBE_ANSWER);
        *probe = Some(answer);
        Ok(answer)
    }

    /// Feed a script to the host's sh over the shared login and return what it prints.
    pub async fn run_script(&self, script: &str) -> Result<String, String> {
        let Some(control_path) = self.control_path.clone() else {
            return Err("Commands are not available for this session.".into());
        };
        self.wait_for_login(&control_path).await?;
        let output = self
            .run_job(
                &script_ssh_args(&self.profile, &control_path),
                script,
                SCRIPT_TIMEOUT,
            )
            .await?;
        if output.code == Some(SSH_OWN_FAILURE) {
            return Err(if output.stderr.is_empty() {
                "ssh could not run the command.".into()
            } else {
                output.stderr
            });
        }
        Ok(output.stdout)
    }

    /// One SFTP session over the shared login, shared by every file request.
    pub async fn sftp(&self) -> Result<Arc<SftpChannel>, String> {
        let Some(control_path) = self.control_path.clone().filter(|_| self.sftp_available) else {
            return Err("The file browser is not available for this session.".into());
        };
        let mut slot = self.sftp.lock().await;
        if let Some((channel, _)) = slot.as_ref() {
            return Ok(channel.clone());
        }
        self.wait_for_login(&control_path).await?;
        if self.is_finished() {
            return Err("The terminal session has ended.".into());
        }
        let mut child = self
            .command(&sftp_ssh_args(&self.profile, &control_path))
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let errors = tokio::spawn(async move {
            let mut text = Vec::new();
            let _ = stderr.read_to_end(&mut text).await;
            String::from_utf8_lossy(&text).trim().to_string()
        });
        let session = russh_sftp::client::RawSftpSession::new(tokio::io::join(stdout, stdin));
        session.set_timeout(SFTP_REQUEST_TIMEOUT_SECS);
        let started = tokio::time::timeout(SFTP_START_TIMEOUT, session.init()).await;
        let version = match started {
            Ok(Ok(version)) => version,
            failed => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                let stderr = errors.await.unwrap_or_default();
                let message = match failed {
                    Ok(Err(err)) => err.to_string(),
                    _ => "The SFTP session did not start in time.".into(),
                };
                return Err(if stderr.is_empty() { message } else { stderr });
            }
        };
        let channel = Arc::new(SftpChannel {
            session,
            extensions: version.extensions,
        });
        *slot = Some((channel.clone(), child));
        Ok(channel)
    }

    /// Drops the SFTP session (after it failed), so the next request opens a fresh one.
    pub async fn reset_sftp(&self) {
        if let Some((_, mut child)) = self.sftp.lock().await.take() {
            let _ = child.start_kill();
        }
    }
}

impl SftpSource for SystemOpenSshSession {
    fn sftp(&self) -> BoxFuture<'_, Result<Arc<SftpChannel>, String>> {
        Box::pin(SystemOpenSshSession::sftp(self))
    }

    fn reset_sftp(&self) -> BoxFuture<'_, ()> {
        Box::pin(SystemOpenSshSession::reset_sftp(self))
    }
}

/// A hold on a system OpenSSH session; dropping it lets go.
#[derive(Debug)]
pub struct SystemOpenSshLease {
    session: Arc<SystemOpenSshSession>,
    released: AtomicBool,
}

impl SystemOpenSshLease {
    pub fn connection(&self) -> &Arc<SystemOpenSshSession> {
        &self.session
    }

    /// Idempotent.
    pub fn release(&self) {
        if !self.released.swap(true, Ordering::SeqCst) {
            self.session.release();
        }
    }
}

impl Drop for SystemOpenSshLease {
    fn drop(&mut self) {
        self.release();
    }
}

/// The live sessions by the connId their tabs were given.
#[derive(Clone, Default)]
pub struct SystemOpenSshSessions {
    sessions: Arc<Mutex<HashMap<String, Arc<SystemOpenSshSession>>>>,
    defaults: Arc<Mutex<SessionTools>>,
}

impl SystemOpenSshSessions {
    /// How sessions run `ssh` unless a start says otherwise (tests point it at their config).
    pub fn configure(&self, tools: SessionTools) {
        *lock(&self.defaults) = tools;
    }

    /// The configured tools, for a start to add its environment to.
    pub fn defaults(&self) -> SessionTools {
        lock(&self.defaults).clone()
    }

    /// Start a session's bookkeeping; the caller starts [`SystemOpenSshSession::terminal_args`]
    /// under a PTY and reports its end with [`SystemOpenSshSession::terminal_ended`].
    pub fn start(
        &self,
        profile: &SshProfile,
        sftp_enabled: bool,
        tools: &SessionTools,
    ) -> Result<Arc<SystemOpenSshSession>, String> {
        // `ssh` cannot pin a host key for one connection, and failing beats quietly skipping
        // the check the link asked for.
        if profile.host_key_fingerprint.is_some() {
            return Err(PINNED_HOST_KEY.into());
        }
        // A hop to a saved host only means something inside Muxus.
        if profile
            .proxy_jump
            .iter()
            .flatten()
            .any(|hop| saved_host_hop_id(hop).is_some())
        {
            return Err(SAVED_JUMP_HOST.into());
        }
        let env = muxus_core::appimage_env::without_appimage_paths(&tools.env);
        let binary = tools
            .binary
            .clone()
            .or_else(|| find_ssh_binary(&env, Platform::current()))
            .ok_or_else(|| NO_SSH_PROGRAM.to_string())?;
        // A container's tab has no use for the host's login being shared.
        let sharing = sftp_enabled && profile.environment.is_none();
        let sftp_available = sharing && !cfg!(windows);
        let (control_dir, control_path) = if sftp_available {
            let root = tools
                .control_root
                .clone()
                .unwrap_or_else(|| PathBuf::from("/tmp"));
            let dir = private_dir(&root).map_err(|e| e.to_string())?;
            let path = dir.join("c");
            (Some(dir), Some(path))
        } else {
            (None, None)
        };
        let (closed, _) = watch::channel(false);
        let session = Arc::new(SystemOpenSshSession {
            id: format!("system-ssh-{}", random_suffix(10)),
            sftp_available,
            profile: profile.clone(),
            binary,
            extra_args: tools.extra_args.clone(),
            env,
            control_dir,
            control_path,
            state: Mutex::new(State {
                terminal_running: true,
                leases: 0,
                finished: false,
                kill_terminal: None,
            }),
            closed,
            sftp: tokio::sync::Mutex::new(None),
            command_probe: tokio::sync::Mutex::new(None),
            registry: Arc::downgrade(&self.sessions),
        });
        lock(&self.sessions).insert(session.id.clone(), session.clone());
        Ok(session)
    }

    /// A hold on the live session with this connId.
    pub fn acquire(&self, conn_id: &str) -> Option<SystemOpenSshLease> {
        let session = lock(&self.sessions).get(conn_id).cloned()?;
        session.acquire()
    }

    pub fn get(&self, conn_id: &str) -> Option<Arc<SystemOpenSshSession>> {
        lock(&self.sessions).get(conn_id).cloned()
    }

    /// End every session at once (application shutdown).
    pub fn close_all(&self) {
        let sessions: Vec<_> = lock(&self.sessions).values().cloned().collect();
        for session in sessions {
            session.destroy();
        }
    }
}

#[cfg(test)]
mod tests;
