//! Session channels on an authenticated transport: interactive shells, RemoteCommand execs and
//! the shell-integration probe, with the tty, environment, agent-forwarding and X11 requests the
//! TypeScript client sends.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use russh::client::Msg;
use russh::{Channel, ChannelMsg, Pty};

use crate::config::ResolvedTarget;
use crate::types::RequestTty;

/// Per-session channel settings from ssh config. Resolved per connect call: two aliases
/// multiplexed onto one transport keep their own environment and RemoteCommand, the way each
/// `ssh` invocation does under ControlMaster.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionSettings {
    pub env: Option<BTreeMap<String, String>>,
    pub remote_command: Option<String>,
    pub request_tty: Option<RequestTty>,
    /// ForwardX11; `None` follows the local X server's default.
    pub forward_x11: Option<bool>,
}

impl SessionSettings {
    /// The settings of a resolved target, with SendEnv matched against this process.
    pub fn from_resolved(resolved: &ResolvedTarget) -> Self {
        Self {
            env: crate::config::process_session_environment(resolved),
            remote_command: resolved.remote_command.clone(),
            request_tty: resolved.request_tty,
            forward_x11: resolved.forward_x11,
        }
    }
}

/// RequestTTY the way ssh(1) treats it: auto ⇒ a tty only for plain shells.
pub fn wants_pty(request_tty: Option<RequestTty>, has_command: bool) -> bool {
    match request_tty {
        Some(RequestTty::No) => false,
        Some(RequestTty::Yes) | Some(RequestTty::Force) => true,
        _ => !has_command,
    }
}

/// The tty a session asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyOptions {
    pub term: String,
    pub cols: u32,
    pub rows: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Terminal modes (opcode, value).
    pub modes: Vec<(Pty, u32)>,
}

/// xterm sends DEL for Backspace. Advertising the same erase character while requesting the
/// remote PTY prevents DEL from being echoed as visible input (and advancing the cursor) on hosts
/// with a different login default.
pub fn terminal_pty_options(cols: u32, rows: u32, term: &str) -> PtyOptions {
    PtyOptions {
        term: term.to_string(),
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
        modes: vec![(Pty::VERASE, 0x7f)],
    }
}

pub use crate::x11::X11Request;

/// What failed while opening a session channel; the messages match the TypeScript engine's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
    #[error("Unable to request a pseudo-terminal")]
    PtyRejected,
    #[error("Unable to request X11")]
    X11Rejected,
    #[error("Unable to start shell")]
    ShellRejected,
    #[error("Unable to exec")]
    ExecRejected,
    #[error("Unable to start subsystem")]
    SubsystemRejected,
    #[error("Channel open failure: {0}")]
    OpenFailed(String),
    #[error("Not connected")]
    NotConnected,
}

impl ChannelError {
    pub fn is_pty_rejection(&self) -> bool {
        matches!(self, ChannelError::PtyRejected)
    }

    pub fn is_x11_rejection(&self) -> bool {
        matches!(self, ChannelError::X11Rejected)
    }
}

fn open_error(err: russh::Error) -> ChannelError {
    match err {
        russh::Error::ChannelOpenFailure(reason) => ChannelError::OpenFailed(format!("{reason:?}")),
        russh::Error::SendError | russh::Error::Disconnect | russh::Error::HUP => {
            ChannelError::NotConnected
        }
        other => ChannelError::OpenFailed(other.to_string()),
    }
}

/// An open session channel plus anything the server sent before the last request's reply.
pub struct SessionChannel {
    pub channel: Channel<Msg>,
    /// Messages that arrived while waiting for a reply, in order.
    pub pending: VecDeque<ChannelMsg>,
    /// The server accepted X11 forwarding on this channel.
    pub x11_forwarded: bool,
}

impl std::fmt::Debug for SessionChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionChannel")
            .field("id", &self.channel.id())
            .field("x11", &self.x11_forwarded)
            .finish()
    }
}

impl SessionChannel {
    /// The next message: pending ones first.
    pub async fn wait(&mut self) -> Option<ChannelMsg> {
        if let Some(msg) = self.pending.pop_front() {
            return Some(msg);
        }
        self.channel.wait().await
    }

    /// Waits for the reply to a want-reply request; true on success.
    async fn reply(&mut self) -> Result<bool, ChannelError> {
        loop {
            match self.channel.wait().await {
                Some(ChannelMsg::Success) => return Ok(true),
                Some(ChannelMsg::Failure) => return Ok(false),
                Some(ChannelMsg::WindowAdjusted { .. }) => {}
                Some(msg @ (ChannelMsg::Close | ChannelMsg::Eof)) => {
                    self.pending.push_back(msg);
                    return Ok(false);
                }
                Some(other) => self.pending.push_back(other),
                None => return Err(ChannelError::NotConnected),
            }
        }
    }
}

/// How to start a session channel.
#[derive(Debug, Clone, Default)]
pub struct SessionRequest {
    pub pty: Option<PtyOptions>,
    pub env: Option<BTreeMap<String, String>>,
    pub x11: Option<X11Request>,
    /// Request `auth-agent-req@openssh.com` (ForwardAgent with a reachable agent).
    pub agent_forward: bool,
    /// `None` starts the login shell.
    pub command: Option<String>,
}

/// Opens a session channel and runs the requests ssh2 sends for a shell or exec: env, x11-req,
/// agent forwarding, pty-req, then shell/exec.
pub async fn open_session(
    handle: &russh::client::Handle<super::transport::ClientHandler>,
    request: &SessionRequest,
) -> Result<SessionChannel, ChannelError> {
    let channel = handle.channel_open_session().await.map_err(open_error)?;
    let mut session = SessionChannel {
        channel,
        pending: VecDeque::new(),
        x11_forwarded: false,
    };
    let sent = |r: Result<(), russh::Error>| r.map_err(|_| ChannelError::NotConnected);
    for (name, value) in request.env.iter().flatten() {
        sent(
            session
                .channel
                .set_env(false, name.as_str(), value.as_str())
                .await,
        )?;
    }
    if let Some(x11) = &request.x11 {
        sent(
            session
                .channel
                .request_x11(
                    true,
                    x11.single_connection,
                    x11.auth_protocol.as_str(),
                    x11.auth_cookie_hex.as_str(),
                    x11.screen,
                )
                .await,
        )?;
        if !session.reply().await? {
            // The server closes nothing, but the TypeScript engine closed this channel on a
            // refused x11-req; the caller retries on a fresh channel the same way.
            let _ = session.channel.close().await;
            return Err(ChannelError::X11Rejected);
        }
        session.x11_forwarded = true;
    }
    if request.agent_forward {
        sent(session.channel.agent_forward(false).await)?;
    }
    if let Some(pty) = &request.pty {
        sent(
            session
                .channel
                .request_pty(
                    true,
                    &pty.term,
                    pty.cols,
                    pty.rows,
                    pty.pixel_width,
                    pty.pixel_height,
                    &pty.modes,
                )
                .await,
        )?;
        if !session.reply().await? {
            let _ = session.channel.close().await;
            return Err(ChannelError::PtyRejected);
        }
    }
    match &request.command {
        Some(command) => {
            sent(session.channel.exec(true, command.as_bytes()).await)?;
            if !session.reply().await? {
                let _ = session.channel.close().await;
                return Err(ChannelError::ExecRejected);
            }
        }
        None => {
            sent(session.channel.request_shell(true).await)?;
            if !session.reply().await? {
                let _ = session.channel.close().await;
                return Err(ChannelError::ShellRejected);
            }
        }
    }
    Ok(session)
}

/// Runs a command without a tty and collects its output (bounded by size and time).
pub async fn probe_command(
    handle: &russh::client::Handle<super::transport::ClientHandler>,
    command: &str,
    timeout: Duration,
    max_output: usize,
) -> Result<String, ChannelError> {
    let mut session = open_session(
        handle,
        &SessionRequest {
            command: Some(command.to_string()),
            ..SessionRequest::default()
        },
    )
    .await?;
    let mut output = Vec::new();
    let collect = async {
        while let Some(msg) = session.wait().await {
            match msg {
                ChannelMsg::Data { data } => {
                    if output.len() < max_output {
                        output.extend_from_slice(&data);
                    }
                }
                ChannelMsg::Close => break,
                _ => {}
            }
        }
    };
    let _ = tokio::time::timeout(timeout, collect).await;
    let _ = session.channel.close().await;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

/// What a non-interactive command printed and how it ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// `None` when the server sent no exit status (or the time ran out).
    pub exit_status: Option<u32>,
    pub exit_signal: Option<String>,
    /// The command was still running when `timeout` ran out.
    pub timed_out: bool,
}

/// Runs `command` without a tty and collects its output, up to `timeout`.
pub async fn exec_command(
    handle: &russh::client::Handle<super::transport::ClientHandler>,
    command: &str,
    env: Option<&BTreeMap<String, String>>,
    timeout: Duration,
) -> Result<ExecOutput, ChannelError> {
    let request = SessionRequest {
        command: Some(command.to_string()),
        env: env.cloned(),
        ..SessionRequest::default()
    };
    let mut session = open_session(handle, &request).await?;
    let mut out = ExecOutput::default();
    let collect = async {
        while let Some(msg) = session.wait().await {
            match msg {
                ChannelMsg::Data { data } => out.stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, .. } => out.stderr.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => out.exit_status = Some(exit_status),
                ChannelMsg::ExitSignal { signal_name, .. } => {
                    out.exit_signal = Some(format!("{signal_name:?}"))
                }
                ChannelMsg::Close => break,
                _ => {}
            }
        }
    };
    let finished = tokio::time::timeout(timeout, collect).await.is_ok();
    out.timed_out = !finished;
    let _ = session.channel.close().await;
    Ok(out)
}
