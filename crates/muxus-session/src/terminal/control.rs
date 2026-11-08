//! Control frames split from the byte stream, with a waiter for the auth round-trips
//! (`ControlChannel` in `terminal-socket.ts`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use muxus_core::protocol::TerminalClientMessage;
use tokio::sync::oneshot;

type Intercept = Arc<dyn Fn(&TerminalClientMessage) -> bool + Send + Sync>;
type OnMessage = Arc<dyn Fn(TerminalClientMessage) + Send + Sync>;

#[derive(Default)]
struct State {
    waiters: VecDeque<oneshot::Sender<TerminalClientMessage>>,
    closed: bool,
    intercept: Option<Intercept>,
    on_message: Option<OnMessage>,
    /// The newest resize from before the session was attached.
    pending_resize: Option<TerminalClientMessage>,
}

/// The session's control messages: a prompt waits for the next one with [`ControlChannel::next`];
/// once the session is attached they go to its handler. Frames the session layers own (logging,
/// paste, login-sequence controls) are taken by the intercept first.
#[derive(Default)]
pub(crate) struct ControlChannel {
    state: Mutex<State>,
}

/// The session ended while something waited for a control message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConnectionClosed;

impl ControlChannel {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn set_intercept(
        &self,
        intercept: impl Fn(&TerminalClientMessage) -> bool + Send + Sync + 'static,
    ) {
        self.state().intercept = Some(Arc::new(intercept));
    }

    /// The attached session's handler. A resize that came while the session connected reaches
    /// it first.
    pub fn set_on_message(
        &self,
        on_message: impl Fn(TerminalClientMessage) + Send + Sync + 'static,
    ) {
        let on_message: OnMessage = Arc::new(on_message);
        let pending = {
            let mut state = self.state();
            state.on_message = Some(on_message.clone());
            state.pending_resize.take()
        };
        if let Some(resize) = pending {
            on_message(resize);
        }
    }

    pub fn push(&self, message: TerminalClientMessage) {
        let intercept = self.state().intercept.clone();
        if intercept.is_some_and(|intercept| intercept(&message)) {
            return;
        }
        let mut message = message;
        let on_message = {
            let mut state = self.state();
            // A BREAK has no line to go to until the session is attached, and must not be taken
            // as the answer to an auth or host-key prompt.
            if matches!(message, TerminalClientMessage::SendBreak) && state.on_message.is_none() {
                return;
            }
            // Muxus fix (`terminal-socket.ts` loses these): a resize is never the answer to a
            // prompt, and one that comes before the session is attached waits for it, the
            // newest replacing the others, so the remote pty takes the window's size.
            if matches!(message, TerminalClientMessage::Resize { .. }) {
                match state.on_message.clone() {
                    Some(on_message) => {
                        drop(state);
                        on_message(message);
                    }
                    None => state.pending_resize = Some(message),
                }
                return;
            }
            while let Some(waiter) = state.waiters.pop_front() {
                match waiter.send(message) {
                    Ok(()) => return,
                    // That wait was abandoned; the next one (or the handler) takes it.
                    Err(returned) => message = returned,
                }
            }
            state.on_message.clone()
        };
        if let Some(on_message) = on_message {
            on_message(message);
        }
    }

    /// Claim the next control frame (an auth or host-key answer) now; await the returned
    /// future for it. A prompt claims its answer before it is sent, so a quick answer from
    /// another thread cannot arrive before anyone waits for it.
    pub fn claim_next(
        &self,
    ) -> impl std::future::Future<Output = Result<TerminalClientMessage, ConnectionClosed>>
    + Send
    + 'static {
        let receiver = {
            let mut state = self.state();
            if state.closed {
                None
            } else {
                let (tx, rx) = oneshot::channel();
                state.waiters.push_back(tx);
                Some(rx)
            }
        };
        async move {
            match receiver {
                Some(receiver) => receiver.await.map_err(|_| ConnectionClosed),
                None => Err(ConnectionClosed),
            }
        }
    }

    /// The session ended: every waiter fails.
    pub fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        state.waiters.clear();
        state.intercept = None;
        state.on_message = None;
        state.pending_resize = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn answers_go_to_waiters_then_to_the_handler() {
        let control = Arc::new(ControlChannel::default());
        let waiting = {
            let control = control.clone();
            tokio::spawn(control.claim_next())
        };
        tokio::task::yield_now().await;
        // A BREAK before the session is attached is dropped, never taken as an answer.
        control.push(TerminalClientMessage::SendBreak);
        control.push(TerminalClientMessage::HostKeyResponse { accept: true });
        assert_eq!(
            waiting.await.unwrap(),
            Ok(TerminalClientMessage::HostKeyResponse { accept: true })
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        control.set_on_message(move |message| sink.lock().unwrap().push(message));
        control.set_intercept(|message| matches!(message, TerminalClientMessage::PasteCancel));
        control.push(TerminalClientMessage::PasteCancel);
        control.push(TerminalClientMessage::SendBreak);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![TerminalClientMessage::SendBreak]
        );
        control.close();
        assert_eq!(control.claim_next().await, Err(ConnectionClosed));
    }

    #[tokio::test]
    async fn a_resize_while_connecting_waits_for_the_session_and_answers_no_prompt() {
        let resize = |cols, rows| TerminalClientMessage::Resize { cols, rows };
        let control = Arc::new(ControlChannel::default());
        let waiting = {
            let control = control.clone();
            tokio::spawn(control.claim_next())
        };
        tokio::task::yield_now().await;
        // The window is resized while a password prompt waits: the prompt still gets its answer.
        control.push(resize(100, 30));
        control.push(resize(149, 44));
        control.push(TerminalClientMessage::HostKeyResponse { accept: true });
        assert_eq!(
            waiting.await.unwrap(),
            Ok(TerminalClientMessage::HostKeyResponse { accept: true })
        );
        // The attached session hears the newest size first, then the ones after it.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        control.set_on_message(move |message| sink.lock().unwrap().push(message));
        control.push(resize(120, 40));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![resize(149, 44), resize(120, 40)]
        );
    }
}
