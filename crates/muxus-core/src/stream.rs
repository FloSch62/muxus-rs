//! The byte-stream contract every terminal transport (SSH shell, local pty, telnet, serial) speaks.
//!
//! A transport is opened by its own crate and hands back a [`TransportChannels`]. The session layer
//! pumps bytes between it and the terminal view. The channels come from `async-channel`, so either
//! side may live on tokio or on the UI executor.
//!
//! This module is a fixed contract shared by several crates: extend it, but do not change existing
//! variants without updating every transport.
//!
//! Two answers do not fit the event stream and travel through [`TransportFeedback`] instead,
//! shared by both ends: how much terminal input has left for the remote end (a paced paste
//! waits for each write to drain before it starts the next delay), and what became of each
//! BREAK.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

/// Terminal geometry in cells, plus the pixel size when the view knows it (kitty graphics and
/// `TIOCSWINSZ` both use it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

/// What the terminal sends to a transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportInput {
    /// Keystrokes and pasted text, already encoded for the remote side.
    Data(Vec<u8>),
    Resize(TerminalSize),
    /// Send BREAK (serial line break, SSH `break` request, telnet IAC BRK) for this long.
    Break {
        duration_ms: u32,
    },
    /// Close the session from the user's side.
    Close,
}

/// What a transport reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportEvent {
    /// Output for the terminal.
    Data(Vec<u8>),
    /// The remote side ended the session. Both fields are `None` when the transport cannot tell.
    Exit {
        code: Option<i32>,
        signal: Option<String>,
    },
    /// The transport failed; the session is over.
    Error(String),
}

/// Both ends of an open transport, as seen from the terminal.
#[derive(Debug, Clone)]
pub struct TransportChannels {
    pub input: async_channel::Sender<TransportInput>,
    pub events: async_channel::Receiver<TransportEvent>,
    /// Drained input and BREAK outcomes, when the transport reports them.
    pub feedback: TransportFeedback,
}

impl TransportChannels {
    /// Creates a connected pair: the terminal-facing channels and the transport-facing ends.
    pub fn pair(capacity: usize) -> (Self, TransportEnds) {
        let (input_tx, input_rx) = async_channel::bounded(capacity);
        let (events_tx, events_rx) = async_channel::bounded(capacity);
        let feedback = TransportFeedback::new();
        (
            Self {
                input: input_tx,
                events: events_rx,
                feedback: feedback.clone(),
            },
            TransportEnds {
                input: input_rx,
                events: events_tx,
                feedback,
            },
        )
    }
}

/// The transport's side of [`TransportChannels`].
#[derive(Debug, Clone)]
pub struct TransportEnds {
    pub input: async_channel::Receiver<TransportInput>,
    pub events: async_channel::Sender<TransportEvent>,
    /// Where the transport reports drained input and BREAK outcomes.
    pub feedback: TransportFeedback,
}

/// What became of one BREAK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakOutcome {
    /// The BREAK went out (or the line was already held in break).
    Sent,
    /// The BREAK could not be sent; the transport's reason (an OS or socket error). The session
    /// shows it as "Could not send BREAK: <reason>".
    Failed(String),
    /// The remote end refused or ignored the request; a complete sentence for the user.
    Declined(String),
}

#[derive(Debug, Default)]
struct FeedbackState {
    /// The transport counts written input; without it nobody may wait for a drain.
    tracks_written: bool,
    /// `TransportInput::Data` messages that have left for the remote end.
    written: u64,
    /// The transport ended: every wait is over.
    closed: bool,
    waiters: Vec<Waker>,
}

#[derive(Debug)]
struct FeedbackInner {
    state: Mutex<FeedbackState>,
    reports_breaks: std::sync::atomic::AtomicBool,
    breaks_tx: async_channel::Sender<BreakOutcome>,
    breaks_rx: async_channel::Receiver<BreakOutcome>,
}

/// Signals a transport gives besides [`TransportEvent`], shared by the terminal side and the
/// transport side of one stream.
///
/// - **Drained input.** A transport that calls [`track_written_input`](Self::track_written_input)
///   counts every `TransportInput::Data` message once it has fully left for the remote end (the
///   socket took it, the serial line transmitted it, the SSH channel window accepted it). The
///   terminal side numbers its own `Data` messages and waits with
///   [`until_written`](Self::until_written). Transports that do not track resolve every wait at
///   once, so waiting never hangs.
/// - **BREAK outcomes.** A transport that calls [`report_breaks`](Self::report_breaks) answers
///   every `TransportInput::Break`, in order, with one [`BreakOutcome`].
#[derive(Debug, Clone)]
pub struct TransportFeedback {
    inner: Arc<FeedbackInner>,
}

impl Default for TransportFeedback {
    fn default() -> Self {
        Self::new()
    }
}

impl TransportFeedback {
    pub fn new() -> Self {
        let (breaks_tx, breaks_rx) = async_channel::unbounded();
        Self {
            inner: Arc::new(FeedbackInner {
                state: Mutex::new(FeedbackState::default()),
                reports_breaks: std::sync::atomic::AtomicBool::new(false),
                breaks_tx,
                breaks_rx,
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, FeedbackState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wake(mut state: MutexGuard<'_, FeedbackState>) {
        let waiters = std::mem::take(&mut state.waiters);
        drop(state);
        for waker in waiters {
            waker.wake();
        }
    }

    // --- transport side ---------------------------------------------------------------------

    /// Declare that this transport counts written input (call before handing out the channels).
    pub fn track_written_input(&self) {
        self.state().tracks_written = true;
    }

    /// One more `Data` message has left for the remote end.
    pub fn input_written(&self) {
        let mut state = self.state();
        state.written += 1;
        Self::wake(state);
    }

    /// Declare that this transport answers every BREAK with an outcome.
    pub fn report_breaks(&self) {
        self.inner
            .reports_breaks
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The outcome of the oldest unanswered BREAK.
    pub fn break_sent(&self, outcome: BreakOutcome) {
        let _ = self.inner.breaks_tx.try_send(outcome);
    }

    /// The transport ended: pending waits finish and no more outcomes arrive.
    pub fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        Self::wake(state);
        self.inner.breaks_tx.close();
    }

    // --- terminal side ----------------------------------------------------------------------

    /// Whether the transport counts written input.
    pub fn tracks_written_input(&self) -> bool {
        self.state().tracks_written
    }

    /// `Data` messages that have left so far.
    pub fn written_inputs(&self) -> u64 {
        self.state().written
    }

    /// Whether the transport has ended.
    pub fn is_closed(&self) -> bool {
        self.state().closed
    }

    /// Completes once `count` `Data` messages have left, the transport ended, or the transport
    /// does not track its writes.
    pub fn until_written(&self, count: u64) -> UntilWritten {
        UntilWritten {
            feedback: self.clone(),
            count,
        }
    }

    /// Whether the transport answers BREAKs.
    pub fn reports_breaks(&self) -> bool {
        self.inner
            .reports_breaks
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The outcome of the next BREAK; `None` once the transport ended.
    pub async fn break_outcome(&self) -> Option<BreakOutcome> {
        self.inner.breaks_rx.recv().await.ok()
    }
}

/// Future returned by [`TransportFeedback::until_written`].
#[derive(Debug)]
pub struct UntilWritten {
    feedback: TransportFeedback,
    count: u64,
}

impl Future for UntilWritten {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.feedback.state();
        if !state.tracks_written || state.closed || state.written >= self.count {
            return Poll::Ready(());
        }
        if !state
            .waiters
            .iter()
            .any(|waker| waker.will_wake(cx.waker()))
        {
            state.waiters.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll_once(future: &mut UntilWritten) -> Poll<()> {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        Pin::new(future).poll(&mut cx)
    }

    #[test]
    fn waits_for_written_input_only_when_the_transport_counts_it() {
        let (channels, ends) = TransportChannels::pair(4);
        // Not tracked: never waits.
        assert_eq!(
            poll_once(&mut channels.feedback.until_written(5)),
            Poll::Ready(())
        );
        ends.feedback.track_written_input();
        let mut wait = channels.feedback.until_written(2);
        assert_eq!(poll_once(&mut wait), Poll::Pending);
        ends.feedback.input_written();
        assert_eq!(poll_once(&mut wait), Poll::Pending);
        ends.feedback.input_written();
        assert_eq!(poll_once(&mut wait), Poll::Ready(()));
        assert_eq!(channels.feedback.written_inputs(), 2);
        // A closed transport releases every wait.
        let mut later = channels.feedback.until_written(10);
        assert_eq!(poll_once(&mut later), Poll::Pending);
        ends.feedback.close();
        assert_eq!(poll_once(&mut later), Poll::Ready(()));
    }

    #[test]
    fn break_outcomes_arrive_in_order() {
        let (channels, ends) = TransportChannels::pair(4);
        assert!(!channels.feedback.reports_breaks());
        ends.feedback.report_breaks();
        assert!(channels.feedback.reports_breaks());
        ends.feedback.break_sent(BreakOutcome::Sent);
        ends.feedback
            .break_sent(BreakOutcome::Failed("gone".into()));
        ends.feedback.close();
        let rx = &channels.feedback.inner.breaks_rx;
        assert_eq!(rx.try_recv().ok(), Some(BreakOutcome::Sent));
        assert_eq!(
            rx.try_recv().ok(),
            Some(BreakOutcome::Failed("gone".into()))
        );
        assert!(rx.try_recv().is_err());
    }
}
