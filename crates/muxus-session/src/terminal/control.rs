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

    pub fn set_on_message(
        &self,
        on_message: impl Fn(TerminalClientMessage) + Send + Sync + 'static,
    ) {
        self.state().on_message = Some(Arc::new(on_message));
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
}
