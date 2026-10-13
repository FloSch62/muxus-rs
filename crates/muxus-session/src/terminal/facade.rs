//! A terminal session's stable socket whose window can be replaced (`TransferableTerminalSocket`
//! in `terminal-socket.ts`). The session listens to this facade's close, so moving a tab to
//! another window never tears down the pty, serial port, Telnet stream or SSH channel.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use muxus_core::protocol::{TerminalClientMessage, TerminalServerMessage};
use tokio::runtime::Handle;

use super::{SessionEvent, SessionHandle, SessionInput};

/// Control messages a newly attached window receives again (latest of each, in this order).
pub const REPLAYED_CONTROL_OPS: [&str; 9] = [
    "session",
    "status",
    "auth-prompt",
    "host-key",
    "ready",
    "logging-state",
    "connection-health",
    "paste-progress",
    "login-sequence",
];

/// Pause the transport when this much output waits for the window.
pub(crate) const BACKPRESSURE_HIGH: usize = 4 * 1024 * 1024;
const BACKPRESSURE_POLL: Duration = Duration::from_millis(50);

/// The `op` tag of a server message.
pub(crate) fn server_op(message: &TerminalServerMessage) -> &'static str {
    match message {
        TerminalServerMessage::Session { .. } => "session",
        TerminalServerMessage::TransferReady => "transfer-ready",
        TerminalServerMessage::Status { .. } => "status",
        TerminalServerMessage::ConnectionHealth { .. } => "connection-health",
        TerminalServerMessage::AuthPrompt(_) => "auth-prompt",
        TerminalServerMessage::HostKey(_) => "host-key",
        TerminalServerMessage::Ready { .. } => "ready",
        TerminalServerMessage::LoggingState { .. } => "logging-state",
        TerminalServerMessage::BreakResult { .. } => "break-result",
        TerminalServerMessage::Exit { .. } => "exit",
        TerminalServerMessage::PasteProgress { .. } => "paste-progress",
        TerminalServerMessage::LoginSequence { .. } => "login-sequence",
    }
}

/// What the window sent, as the session handler sees it.
// Frames are short-lived; boxing the message would only add an allocation per frame.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Frame {
    Data(Vec<u8>),
    Message(TerminalClientMessage),
}

type Listener = Arc<dyn Fn(Frame) + Send + Sync>;
type CloseListener = Box<dyn FnOnce() + Send>;

struct Attachment {
    generation: u64,
    events: async_channel::Sender<SessionEvent>,
    unread: Arc<AtomicUsize>,
}

#[derive(Default)]
struct State {
    current: Option<Attachment>,
    generation: u64,
    closed: bool,
    detached: bool,
    detach_timer: Option<tokio::task::JoinHandle<()>>,
    controls: HashMap<&'static str, TerminalServerMessage>,
    buffering_transfer: bool,
    transfer_buffer: VecDeque<Vec<u8>>,
    buffered_bytes: usize,
    close_listeners: Vec<CloseListener>,
    listener: Option<Listener>,
}

/// The facade of one terminal session.
pub(crate) struct Facade {
    pub terminal_id: String,
    state: Mutex<State>,
    reattach_grace: Duration,
    runtime: Handle,
    input: async_channel::Sender<(u64, SessionInput)>,
    registry: Weak<RegistryInner>,
    paused: AtomicBool,
}

impl Facade {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A facade with its first window attached, and the handle for that window.
    pub fn new(
        terminal_id: String,
        reattach_grace: Duration,
        runtime: Handle,
        registry: &SessionRegistry,
    ) -> (Arc<Self>, SessionHandle) {
        let (input_tx, input_rx) = async_channel::unbounded();
        let facade = Arc::new(Self {
            terminal_id,
            state: Mutex::new(State::default()),
            reattach_grace,
            runtime: runtime.clone(),
            input: input_tx,
            registry: Arc::downgrade(&registry.inner),
            paused: AtomicBool::new(false),
        });
        let handle = facade.new_attachment(&mut facade.state());
        let pump = facade.clone();
        runtime.spawn(async move {
            while let Ok((generation, input)) = input_rx.recv().await {
                pump.handle_input(generation, input);
            }
        });
        registry.insert(facade.clone());
        (facade, handle)
    }

    fn new_attachment(&self, state: &mut State) -> SessionHandle {
        state.generation += 1;
        let (tx, rx) = async_channel::unbounded();
        let unread = Arc::new(AtomicUsize::new(0));
        state.current = Some(Attachment {
            generation: state.generation,
            events: tx,
            unread: unread.clone(),
        });
        SessionHandle {
            terminal_id: self.terminal_id.clone(),
            generation: state.generation,
            input: self.input.clone(),
            events: rx,
            unread,
            done: AtomicBool::new(false),
        }
    }

    /// The session handler; frames sent before it is set are dropped.
    pub fn set_listener(&self, listener: impl Fn(Frame) + Send + Sync + 'static) {
        self.state().listener = Some(Arc::new(listener));
    }

    /// Run `listener` once when the session ends.
    pub fn on_close(&self, listener: impl FnOnce() + Send + 'static) {
        let mut state = self.state();
        if state.closed {
            drop(state);
            listener();
            return;
        }
        state.close_listeners.push(Box::new(listener));
    }

    pub fn is_closed(&self) -> bool {
        self.state().closed
    }

    /// Feed a frame to the session handler, as if the window had sent it.
    pub fn push(&self, frame: Frame) {
        let listener = self.state().listener.clone();
        if let Some(listener) = listener {
            listener(frame);
        }
    }

    fn handle_input(self: &Arc<Self>, generation: u64, input: SessionInput) {
        {
            let state = self.state();
            if state.closed || state.current.as_ref().map(|a| a.generation) != Some(generation) {
                // A window this session has moved away from.
                return;
            }
        }
        match input {
            SessionInput::Data(data) => self.push(Frame::Data(data)),
            SessionInput::Message(message) => self.handle_message(message),
            SessionInput::Close => self.finish(None),
            SessionInput::Detach => self.detach(),
        }
    }

    fn handle_message(&self, message: TerminalClientMessage) {
        {
            let mut state = self.state();
            match &message {
                TerminalClientMessage::PrepareTransfer => {
                    state.buffering_transfer = true;
                    if let Some(current) = &state.current {
                        let _ = current
                            .events
                            .try_send(SessionEvent::Message(TerminalServerMessage::TransferReady));
                    }
                    return;
                }
                TerminalClientMessage::CancelTransfer => {
                    state.buffering_transfer = false;
                    Self::flush_transfer_buffer(&mut state);
                    return;
                }
                TerminalClientMessage::AuthResponse(_) => {
                    state.controls.remove("auth-prompt");
                }
                TerminalClientMessage::HostKeyResponse { .. } => {
                    state.controls.remove("host-key");
                }
                _ => {}
            }
        }
        self.push(Frame::Message(message));
    }

    /// Output for the window.
    pub fn send_data(&self, data: Vec<u8>) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        if state.buffering_transfer || state.detached || state.current.is_none() {
            Self::buffer_transfer_data(&mut state, data);
            return;
        }
        if let Some(current) = &state.current {
            current.unread.fetch_add(data.len(), Ordering::SeqCst);
            if current.events.try_send(SessionEvent::Data(data)).is_err() {
                current.unread.store(0, Ordering::SeqCst);
            }
        }
    }

    /// A control message for the window (`sendControl`).
    pub fn send_message(&self, message: TerminalServerMessage) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        let op = server_op(&message);
        if REPLAYED_CONTROL_OPS.contains(&op) {
            state.controls.insert(op, message.clone());
            if op == "ready" {
                state.controls.remove("status");
                state.controls.remove("auth-prompt");
                state.controls.remove("host-key");
            }
        }
        if let Some(current) = &state.current {
            let _ = current.events.try_send(SessionEvent::Message(message));
        }
    }

    /// Unread output: what the window has not taken plus what is held for a transfer.
    fn pending_output(&self) -> usize {
        let state = self.state();
        state
            .current
            .as_ref()
            .map_or(0, |current| current.unread.load(Ordering::SeqCst))
            + state.buffered_bytes
    }

    /// Flow control: a runaway `cat hugefile` must not balloon the window's queue. Called after
    /// each chunk of output; returns once the transport may be read again.
    pub async fn wait_for_room(&self) {
        if !self.paused.load(Ordering::SeqCst) {
            if self.pending_output() <= BACKPRESSURE_HIGH {
                return;
            }
            self.paused.store(true, Ordering::SeqCst);
        }
        while !self.is_closed() && self.pending_output() >= BACKPRESSURE_HIGH / 2 {
            tokio::time::sleep(BACKPRESSURE_POLL).await;
        }
        self.paused.store(false, Ordering::SeqCst);
    }

    /// Move the session to a new window; `None` once it has ended.
    pub fn attach(self: &Arc<Self>, cols: u32, rows: u32) -> Option<SessionHandle> {
        let handle = {
            let mut state = self.state();
            if state.closed {
                return None;
            }
            if let Some(timer) = state.detach_timer.take() {
                timer.abort();
            }
            state.detached = false;
            if let Some(previous) = state.current.take() {
                let _ = previous.events.try_send(SessionEvent::Closed {
                    reason: Some("terminal transferred".into()),
                });
            }
            let handle = self.new_attachment(&mut state);
            if let Some(current) = &state.current {
                for op in REPLAYED_CONTROL_OPS {
                    if let Some(message) = state.controls.get(op) {
                        let _ = current
                            .events
                            .try_send(SessionEvent::Message(message.clone()));
                    }
                }
            }
            state.buffering_transfer = false;
            Self::flush_transfer_buffer(&mut state);
            handle
        };
        self.handle_message(TerminalClientMessage::Resize { cols, rows });
        Some(handle)
    }

    fn flush_transfer_buffer(state: &mut State) {
        let buffered: Vec<Vec<u8>> = state.transfer_buffer.drain(..).collect();
        state.buffered_bytes = 0;
        let Some(current) = &state.current else {
            return;
        };
        for data in buffered {
            current.unread.fetch_add(data.len(), Ordering::SeqCst);
            let _ = current.events.try_send(SessionEvent::Data(data));
        }
    }

    /// Keep the newest bounded tail while no window can apply backpressure.
    fn buffer_transfer_data(state: &mut State, data: Vec<u8>) {
        let mut data = data;
        if data.len() >= BACKPRESSURE_HIGH {
            state.transfer_buffer.clear();
            data = data.split_off(data.len() - BACKPRESSURE_HIGH);
            state.buffered_bytes = 0;
        }
        while !state.transfer_buffer.is_empty()
            && state.buffered_bytes + data.len() > BACKPRESSURE_HIGH
        {
            if let Some(removed) = state.transfer_buffer.pop_front() {
                state.buffered_bytes -= removed.len();
            }
        }
        state.buffered_bytes += data.len();
        state.transfer_buffer.push_back(data);
    }

    /// The window went away unexpectedly: wait for another one for the reattach grace.
    fn detach(self: &Arc<Self>) {
        let mut state = self.state();
        if state.closed || state.detached {
            return;
        }
        state.detached = true;
        state.current = None;
        if self.reattach_grace.is_zero() {
            drop(state);
            self.finish(None);
            return;
        }
        let facade = Arc::downgrade(self);
        let grace = self.reattach_grace;
        state.detach_timer = Some(self.runtime.spawn(async move {
            tokio::time::sleep(grace).await;
            if let Some(facade) = facade.upgrade() {
                facade.finish(None);
            }
        }));
    }

    /// The session closes its socket: the window sees `Closed` and the session's close
    /// listeners run.
    pub fn close(&self) {
        self.finish(None);
    }

    fn finish(&self, reason: Option<String>) {
        let listeners = {
            let mut state = self.state();
            if state.closed {
                return;
            }
            state.closed = true;
            state.detached = false;
            if let Some(timer) = state.detach_timer.take() {
                timer.abort();
            }
            state.transfer_buffer.clear();
            state.buffered_bytes = 0;
            if let Some(current) = state.current.take() {
                let _ = current.events.try_send(SessionEvent::Closed { reason });
            }
            state.listener = None;
            std::mem::take(&mut state.close_listeners)
        };
        self.input.close();
        if let Some(registry) = self.registry.upgrade() {
            registry.remove(&self.terminal_id);
        }
        for listener in listeners {
            listener();
        }
    }
}

#[derive(Default)]
pub(crate) struct RegistryInner {
    sessions: Mutex<HashMap<String, Arc<Facade>>>,
}

impl RegistryInner {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Arc<Facade>>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn remove(&self, terminal_id: &str) {
        self.lock().remove(terminal_id);
    }
}

/// The live terminal sessions by id.
#[derive(Default)]
pub(crate) struct SessionRegistry {
    inner: Arc<RegistryInner>,
}

impl SessionRegistry {
    fn insert(&self, facade: Arc<Facade>) {
        self.inner.lock().insert(facade.terminal_id.clone(), facade);
    }

    pub fn attach(&self, terminal_id: &str, cols: u32, rows: u32) -> Option<SessionHandle> {
        let facade = self.inner.lock().get(terminal_id).cloned()?;
        facade.attach(cols, rows)
    }

    pub fn ids(&self) -> Vec<String> {
        self.inner.lock().keys().cloned().collect()
    }

    /// End every session (backend shutdown).
    pub fn close_all(&self) {
        let sessions: Vec<Arc<Facade>> = self.inner.lock().values().cloned().collect();
        for facade in sessions {
            facade.close();
        }
    }
}
