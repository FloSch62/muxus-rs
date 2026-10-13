//! Paced pastes (`server/src/ws/paste-pacer.ts`).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use muxus_core::js::utf16_len;
use muxus_core::paste_pacing::{
    BRACKETED_PASTE_END, BRACKETED_PASTE_START, PastePacing, paste_pacing_steps, pasted_line_count,
    prepare_paste_text,
};
use muxus_core::protocol::PasteProgressState;
use tokio::sync::watch;
use tokio::time::Instant;

/// Least time between two progress reports of a running paste.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// A paste to type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteRequest {
    pub text: String,
    pub bracketed: bool,
    pub line_delay_ms: u32,
    pub char_delay_ms: u32,
}

/// Progress over every queued paste (the `paste-progress` message).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteProgress {
    pub state: PasteProgressState,
    pub line: u64,
    pub lines: u64,
    pub sent: u64,
    pub total: u64,
}

/// Writes one step; settles once the transport has taken it. An error means the transport
/// failed underneath.
pub type PasteWrite =
    Arc<dyn Fn(Vec<u8>) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;
/// Receives progress reports.
pub type PasteReport = Arc<dyn Fn(PasteProgress) + Send + Sync>;

struct QueuedPaste {
    prepared: String,
    bracketed: bool,
    lines: u64,
    pacing: PastePacing,
}

#[derive(Default)]
struct State {
    queue: VecDeque<QueuedPaste>,
    /// Aborts the running paste.
    controller: Option<watch::Sender<bool>>,
    closed: bool,
    sent: u64,
    total: u64,
    lines_done: u64,
    lines: u64,
    reported: bool,
    last_report_at: Option<Instant>,
}

struct Inner {
    state: Mutex<State>,
    write: PasteWrite,
    report: PasteReport,
}

/// Types pastes into one terminal session at their pace, one paste after the other. The waits
/// run on backend timers, which a hidden or throttled window does not slow down, and each one
/// starts once the transport has taken the write before it, so a slow link cannot eat into the
/// delay. Must be used inside a tokio runtime.
#[derive(Clone)]
pub struct PastePacer {
    inner: Arc<Inner>,
}

impl PastePacer {
    pub fn new(write: PasteWrite, report: PasteReport) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                write,
                report,
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn enqueue(&self, request: PasteRequest) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        let prepared = prepare_paste_text(&request.text, request.bracketed);
        if prepared.is_empty() {
            return;
        }
        let lines = pasted_line_count(&prepared) as u64;
        state.total += utf16_len(&prepared) as u64;
        state.lines += lines;
        state.queue.push_back(QueuedPaste {
            prepared,
            bracketed: request.bracketed,
            lines,
            pacing: PastePacing {
                line_delay_ms: request.line_delay_ms,
                char_delay_ms: request.char_delay_ms,
            },
        });
        if state.controller.is_none() {
            self.start_run(&mut state);
        }
    }

    /// Stop at the user's request, closing an open bracket so the shell leaves paste mode.
    pub fn cancel(&self) {
        let mut state = self.state();
        state.queue.clear();
        if let Some(controller) = &state.controller {
            let _ = controller.send(true);
        }
    }

    /// The session is gone: stop without writing or reporting anything more.
    pub fn close(&self) {
        self.state().closed = true;
        self.cancel();
    }

    fn start_run(&self, state: &mut State) {
        let (tx, rx) = watch::channel(false);
        state.controller = Some(tx);
        let pacer = self.clone();
        tokio::spawn(async move { pacer.run(rx).await });
    }

    fn report_progress(
        &self,
        state: &mut State,
        progress: PasteProgressState,
        force: bool,
    ) -> Option<PasteProgress> {
        let now = Instant::now();
        if !force
            && state.reported
            && state
                .last_report_at
                .is_some_and(|last| now.duration_since(last) < PROGRESS_INTERVAL)
        {
            return None;
        }
        state.reported = true;
        state.last_report_at = Some(now);
        Some(PasteProgress {
            state: progress,
            line: (state.lines_done + 1).min(state.lines),
            lines: state.lines,
            sent: state.sent,
            total: state.total,
        })
    }

    fn report(&self, progress: Option<PasteProgress>) {
        if let Some(progress) = progress {
            (self.inner.report)(progress);
        }
    }

    async fn run(self, signal: watch::Receiver<bool>) {
        let aborted = || *signal.borrow();
        let mut bracket_open = false;
        let mut failed = false;
        'pastes: loop {
            if aborted() {
                break;
            }
            let Some(paste) = self.state().queue.pop_front() else {
                break;
            };
            let mut steps = paste_pacing_steps(&paste.prepared, paste.pacing).peekable();
            while let Some(step) = steps.next() {
                if aborted() {
                    break 'pastes;
                }
                let last = steps.peek().is_none();
                let mut data = step.data.clone();
                if paste.bracketed && !bracket_open {
                    data = format!("{BRACKETED_PASTE_START}{data}");
                    bracket_open = true;
                }
                if paste.bracketed && last {
                    data.push_str(BRACKETED_PASTE_END);
                    bracket_open = false;
                }
                let write = (self.inner.write)(data.into_bytes());
                let mut abort = signal.clone();
                // Settle with the write, or as soon as the paste is stopped.
                let written = tokio::select! {
                    result = write => result,
                    _ = abort.wait_for(|stop| *stop) => Ok(()),
                };
                if written.is_err() {
                    // The transport failed underneath; the session is ending.
                    failed = true;
                    break 'pastes;
                }
                let progress = {
                    let mut state = self.state();
                    state.sent += utf16_len(&step.data) as u64;
                    if step.line_end {
                        state.lines_done += 1;
                    }
                    (step.delay_ms > 0 && !aborted())
                        .then(|| {
                            self.report_progress(&mut state, PasteProgressState::Running, false)
                        })
                        .flatten()
                };
                if step.delay_ms > 0 && !aborted() {
                    self.report(progress);
                    let mut abort = signal.clone();
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(step.delay_ms.into())) => {}
                        _ = abort.wait_for(|stop| *stop) => {}
                    }
                }
            }
        }
        let cancelled = aborted() || failed;
        let closed = self.state().closed;
        if cancelled && bracket_open && !closed && !failed {
            let _ = (self.inner.write)(BRACKETED_PASTE_END.as_bytes().to_vec()).await;
        }
        let progress = {
            let mut state = self.state();
            state.controller = None;
            let progress = (!state.closed && state.reported)
                .then(|| {
                    let outcome = if cancelled {
                        PasteProgressState::Cancelled
                    } else {
                        PasteProgressState::Done
                    };
                    self.report_progress(&mut state, outcome, true)
                })
                .flatten();
            state.sent = 0;
            state.lines_done = 0;
            state.reported = false;
            state.total = state
                .queue
                .iter()
                .map(|paste| utf16_len(&paste.prepared) as u64)
                .sum();
            state.lines = state.queue.iter().map(|paste| paste.lines).sum();
            progress
        };
        self.report(progress);
        // A paste sent after a cancel but before this run wound down starts afresh.
        let mut state = self.state();
        if !state.closed && !state.queue.is_empty() && state.controller.is_none() {
            self.start_run(&mut state);
        }
    }
}
