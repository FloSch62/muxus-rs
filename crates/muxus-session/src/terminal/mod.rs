//! Terminal sessions: the `/ws/terminal` socket of the TypeScript server, in process
//! (`server/src/ws/terminal-socket.ts`, `terminal-inputs.ts`, `paste-pacer.ts`).
//!
//! [`crate::Backend::open_session`] takes the first message a terminal socket received
//! (`connect`, `dial` or `attach`) and returns a [`SessionHandle`]. The handle carries what the
//! socket carried: terminal bytes both ways, [`TerminalClientMessage`]s in, and
//! [`TerminalServerMessage`]s out, plus [`SessionEvent::Closed`] where the socket closed.
//!
//! # What the UI sees, in order
//!
//! 1. `session { terminalId }` — the id to attach to later.
//! 2. For `connect`: `logging-state`.
//! 3. While connecting: `status` lines (`transient` ones may be replaced by the next), and for
//!    SSH any number of `auth-prompt` / `host-key` round-trips. Each prompt waits for exactly
//!    one answer: `auth-response` for an `auth-prompt` (with `skipped` for its skip action),
//!    `host-key-response` for a `host-key`. Any other message answers a pending prompt with a
//!    cancel. Prompt purposes: `authentication` (password, keyboard-interactive, passphrase),
//!    `ssh-password` (a password with a remember-password offer), `vault-unlock`, `vault-create`
//!    and `vault-repair`.
//! 4. A login sequence, when the host has one, reports `login-sequence { state: running }`
//!    before `ready`; it may ask for the vault's master password with an `auth-prompt`
//!    (`vault-unlock`) at any later time.
//! 5. `ready { connId, … }` (SSH sessions carry `summary`), then terminal bytes. `status`
//!    lines can still follow (a config forward that failed to start comes before `ready`).
//! 6. During the session: `connection-health`, `logging-state` after every change,
//!    `paste-progress`, `break-result` (one per `send-break`), `login-sequence`.
//! 7. `exit { code?, message?, reason }`, then [`SessionEvent::Closed`]. A setup failure is an
//!    `exit` with `reason: failed` and `code: 1`.
//!
//! # Moving a session between windows
//!
//! A session outlives its window. To move a tab, the source sends `prepare-transfer`, waits for
//! `transfer-ready` (output is held from then on), snapshots its screen, and the destination
//! calls [`crate::Backend::attach_session`] (or opens with `attach`). The destination's handle
//! first receives the latest `session`, `status`, `auth-prompt`, `host-key`, `ready`,
//! `logging-state`, `connection-health`, `paste-progress` and `login-sequence`, then the held
//! output, and the session takes its size. The source handle gets
//! `Closed { reason: "terminal transferred" }`. `cancel-transfer` releases held output back to
//! the source.
//!
//! A handle dropped (or [`SessionHandle::detach`]ed) without [`SessionHandle::close`] leaves the
//! session running for the reattach grace (15 s), keeping the newest 4 MiB of output; attaching
//! within it continues the session, otherwise it ends. `close` ends it at once.

mod connect_io;
mod control;
mod facade;
mod inputs;
mod link;
pub mod paste;
mod run;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use muxus_core::protocol::{
    ExitReason, TERMINAL_SESSION_CLOSE_REASON, TerminalClientMessage, TerminalServerMessage,
};
use muxus_core::validate::Validate;

pub use facade::REPLAYED_CONTROL_OPS;
pub(crate) use facade::SessionRegistry;
pub use inputs::{InputRegistration, TerminalInputWriter, TerminalInputs};

use crate::Backend;

/// What the UI sends to a session.
// The enums mirror the socket's frames; boxing the messages would only add indirection to
// short-lived values, as in `muxus_core::protocol`.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionInput {
    /// Terminal input (keystrokes, unpaced pastes).
    Data(Vec<u8>),
    Message(TerminalClientMessage),
    /// The user closed the tab: end the session now (`TERMINAL_SESSION_CLOSE_REASON`).
    Close,
    /// The window went away without closing the tab: keep the session for the reattach grace.
    Detach,
}

/// What a session reports to the UI.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// Terminal output.
    Data(Vec<u8>),
    Message(TerminalServerMessage),
    /// This handle is finished: the session ended, or (with a reason) it moved to another
    /// handle (`terminal transferred`) or never started (`expected connect or attach`).
    Closed {
        reason: Option<String>,
    },
}

/// One window's connection to a terminal session. See the module documentation for the event
/// order.
pub struct SessionHandle {
    terminal_id: String,
    generation: u64,
    input: async_channel::Sender<(u64, SessionInput)>,
    events: async_channel::Receiver<SessionEvent>,
    /// Output bytes delivered to this handle but not yet taken by the UI.
    unread: Arc<AtomicUsize>,
    done: AtomicBool,
}

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHandle")
            .field("terminal_id", &self.terminal_id)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl SessionHandle {
    /// The backend's id for this terminal (`terminal-…`), for [`Backend::attach_session`] and
    /// for typing vault secrets into it. Empty for a handle whose session never started.
    pub fn terminal_id(&self) -> &str {
        &self.terminal_id
    }

    fn push(&self, input: SessionInput) {
        let _ = self.input.try_send((self.generation, input));
    }

    /// Terminal input. Never blocks.
    pub fn send_data(&self, data: impl Into<Vec<u8>>) {
        self.push(SessionInput::Data(data.into()));
    }

    /// A control message. Messages that fail the protocol's validation are dropped, as the
    /// socket dropped frames that failed its schema.
    pub fn send(&self, message: TerminalClientMessage) {
        let mut message = message;
        match message.validate() {
            Ok(()) => self.push(SessionInput::Message(message)),
            Err(error) => tracing::debug!(%error, "dropped an invalid terminal message"),
        }
    }

    /// End the session (the tab was closed).
    pub fn close(&self) {
        if !self.done.swap(true, Ordering::SeqCst) {
            self.push(SessionInput::Close);
        }
    }

    /// Let go of the session without ending it; it waits for a window to attach again.
    pub fn detach(&self) {
        if !self.done.swap(true, Ordering::SeqCst) {
            self.push(SessionInput::Detach);
        }
    }

    fn took(&self, event: &SessionEvent) {
        if let SessionEvent::Data(data) = event {
            self.unread.fetch_sub(data.len(), Ordering::SeqCst);
        }
    }

    /// The next event; `None` after [`SessionEvent::Closed`].
    pub async fn next_event(&self) -> Option<SessionEvent> {
        let event = self.events.recv().await.ok()?;
        self.took(&event);
        Some(event)
    }

    /// The next event if one is waiting.
    pub fn try_next_event(&self) -> Option<SessionEvent> {
        let event = self.events.try_recv().ok()?;
        self.took(&event);
        Some(event)
    }

    /// A handle that only reports `events` and ends.
    fn finished(events: Vec<SessionEvent>) -> Self {
        let (input, _) = async_channel::unbounded();
        let (tx, rx) = async_channel::unbounded();
        for event in events {
            let _ = tx.try_send(event);
        }
        Self {
            terminal_id: String::new(),
            generation: 0,
            input,
            events: rx,
            unread: Arc::default(),
            done: AtomicBool::new(true),
        }
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        // A window that disappears without closing its tab: the renderer crashed or the window
        // closed during a move. The session waits for the reattach grace.
        self.detach();
    }
}

impl Backend {
    /// Open a terminal session with the first message a terminal socket would have received:
    /// `connect` (a shell session), `dial` (an SSH transport with no shell, for tunnels) or
    /// `attach` (an existing session). Anything else ends the handle at once with
    /// `Closed { reason: "expected connect or attach" }`.
    pub fn open_session(&self, first: TerminalClientMessage) -> SessionHandle {
        let mut first = first;
        if first.validate().is_err() {
            return SessionHandle::finished(vec![SessionEvent::Closed {
                reason: Some("expected connect or attach".into()),
            }]);
        }
        match first {
            TerminalClientMessage::Attach {
                terminal_id,
                cols,
                rows,
            } => self.attach_session(&terminal_id, cols, rows),
            TerminalClientMessage::Connect { .. } | TerminalClientMessage::Dial { .. } => {
                run::start(self, first)
            }
            _ => SessionHandle::finished(vec![SessionEvent::Closed {
                reason: Some("expected connect or attach".into()),
            }]),
        }
    }

    /// Attach a window to a live session (after a window handoff or an interruption). A session
    /// that is gone answers with an `exit` and ends the handle.
    pub fn attach_session(&self, terminal_id: &str, cols: u32, rows: u32) -> SessionHandle {
        match self.shared.sessions.attach(terminal_id, cols, rows) {
            Some(handle) => handle,
            None => SessionHandle::finished(vec![
                SessionEvent::Message(TerminalServerMessage::Exit {
                    code: Some(1),
                    message: Some("The terminal session is no longer available.".into()),
                    reason: ExitReason::Disconnected,
                }),
                SessionEvent::Closed { reason: None },
            ]),
        }
    }

    /// Ids of the live terminal sessions.
    pub fn terminal_ids(&self) -> Vec<String> {
        self.shared.sessions.ids()
    }

    /// The live sessions' input writers, for the backend's own typing (vault secrets).
    pub fn terminal_inputs(&self) -> &TerminalInputs {
        &self.shared.terminal_inputs
    }
}

/// The reason a renderer gives when it closes a session on purpose.
pub const CLOSE_REASON: &str = TERMINAL_SESSION_CLOSE_REASON;
