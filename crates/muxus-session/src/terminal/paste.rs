//! Paced pastes (`server/src/ws/paste-pacer.ts`).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use muxus_core::api::TerminalEnterKey;
use muxus_core::js::utf16_len;
use muxus_core::paste_pacing::{
    BRACKETED_PASTE_END, BRACKETED_PASTE_START, PastePacing, PastePromptWait, paste_pacing_steps,
    pasted_line_count, prepare_paste_text, with_enter_key,
};
use muxus_core::protocol::PasteProgressState;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;

use super::paste_prompt::{PastePromptWatch, PromptWatchResult, pasted_line_text};

/// Least time between two progress reports of a running paste.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// A paste to type.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PasteRequest {
    pub text: String,
    pub bracketed: bool,
    pub line_delay_ms: u32,
    pub char_delay_ms: u32,
    /// Wait for the prompt after each line and stop at a rejected one.
    pub prompt: Option<PastePromptWait>,
    /// The host's Enter key for line breaks outside a bracket; CR when absent.
    pub enter_key: Option<TerminalEnterKey>,
}

/// Progress over every queued paste (the `paste-progress` message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteProgress {
    pub state: PasteProgressState,
    pub line: u64,
    pub lines: u64,
    pub sent: u64,
    pub total: u64,
    /// Why a stopped paste stopped.
    pub message: Option<String>,
    /// Numbers the session's stopped pastes, so a returning window can tell a stop it has noted.
    pub stop_id: Option<u64>,
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
    enter_key: TerminalEnterKey,
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
    /// Reads output while a paste that waits for the prompt runs.
    watch: Option<PastePromptWatch>,
    stops: u64,
}

impl State {
    fn aborted(&self) -> bool {
        self.controller
            .as_ref()
            .is_some_and(|controller| *controller.borrow())
    }
}

struct Inner {
    state: Mutex<State>,
    /// Wakes a read of the answer when output arrives.
    output: Notify,
    write: PasteWrite,
    report: PasteReport,
}

/// Types pastes into one terminal session at their pace, one paste after the other. The waits
/// run on backend timers, which a hidden or throttled window does not slow down, and each one
/// starts once the transport has taken the write before it, so a slow link cannot eat into the
/// delay. A paste that waits for the prompt reads the session's output after each line instead,
/// and stops at the first line the device rejects. Must be used inside a tokio runtime.
#[derive(Clone)]
pub struct PastePacer {
    inner: Arc<Inner>,
}

impl PastePacer {
    pub fn new(write: PasteWrite, report: PasteReport) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                output: Notify::new(),
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

    /// A paste is being typed.
    pub fn running(&self) -> bool {
        self.state().controller.is_some()
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
        // A cancelled run still winding down recounts the queue for the run after it.
        if !state.aborted() {
            state.total += utf16_len(&prepared) as u64;
            state.lines += lines;
        }
        state.queue.push_back(QueuedPaste {
            prepared,
            bracketed: request.bracketed,
            enter_key: if request.bracketed {
                TerminalEnterKey::Cr
            } else {
                request.enter_key.unwrap_or(TerminalEnterKey::Cr)
            },
            lines,
            pacing: PastePacing {
                line_delay_ms: request.line_delay_ms,
                char_delay_ms: request.char_delay_ms,
                // A bracketed paste reaches the program whole, with no prompt after each line.
                prompt: if request.bracketed {
                    None
                } else {
                    request.prompt
                },
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

    /// Output from the session; only a paste waiting for the prompt reads it.
    pub fn output(&self, data: &[u8]) {
        let mut state = self.state();
        let Some(watch) = state.watch.as_mut() else {
            return;
        };
        watch.append(data);
        drop(state);
        // Coalesced: a flood of small chunks costs one check once the reader runs.
        self.inner.output.notify_one();
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
            message: None,
            stop_id: None,
        })
    }

    fn report(&self, progress: Option<PasteProgress>) {
        if let Some(progress) = progress {
            (self.inner.report)(progress);
        }
    }

    fn report_running(&self) {
        let progress = {
            let mut state = self.state();
            self.report_progress(&mut state, PasteProgressState::Running, false)
        };
        self.report(progress);
    }

    /// Stop by itself: nothing after this, queued pastes included, is sent.
    fn stop(&self) {
        let mut state = self.state();
        state.queue.clear();
        if let Some(controller) = &state.controller {
            let _ = controller.send(true);
        }
    }

    async fn run(self, signal: watch::Receiver<bool>) {
        let aborted = || *signal.borrow();
        // Why the paste stopped by itself.
        let mut stopped: Option<String> = None;
        let mut bracket_open = false;
        let mut failed = false;
        // A paste sent in the same tick as a cancel stays queued for the next run.
        'pastes: while !aborted() {
            let Some(paste) = self.state().queue.pop_front() else {
                break;
            };
            {
                let mut state = self.state();
                state.watch = None;
                if let Some(prompt) = &paste.pacing.prompt {
                    match PastePromptWatch::new(&prompt.prompt_pattern, &prompt.error_pattern) {
                        Ok(watch) => state.watch = Some(watch),
                        Err(reason) => {
                            drop(state);
                            stopped = Some(format!(
                                "The prompt or error pattern cannot be used: {reason}"
                            ));
                            self.stop();
                        }
                    }
                }
            }
            // Set once this paste has sent a line, whose answer can still come while the next
            // is typed.
            let mut answering = false;
            // The line being typed, and the last one sent whole: the line the output read now
            // answers.
            let mut typed = String::new();
            let mut sent_line: (u64, String) = (0, String::new());
            let mut steps = paste_pacing_steps(&paste.prepared, &paste.pacing).peekable();
            while let Some(step) = steps.next() {
                if aborted() {
                    break 'pastes;
                }
                let last = steps.peek().is_none();
                // Steps split prepared text at CR; the Enter key only changes what goes out.
                let mut data = with_enter_key(&step.data, paste.enter_key);
                if paste.bracketed && !bracket_open {
                    data = format!("{BRACKETED_PASTE_START}{data}");
                    bracket_open = true;
                }
                if paste.bracketed && last {
                    data.push_str(BRACKETED_PASTE_END);
                    bracket_open = false;
                }
                // Only output from here on answers this line.
                let watched = step.line_end && {
                    let mut state = self.state();
                    state.watch.as_mut().map(PastePromptWatch::reset).is_some()
                };
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
                {
                    let mut state = self.state();
                    state.sent += utf16_len(&step.data) as u64;
                    if step.line_end {
                        state.lines_done += 1;
                    }
                }
                typed.push_str(&step.data);
                if step.line_end {
                    sent_line = (sent_line.0 + 1, std::mem::take(&mut typed));
                }
                let mut answer = None;
                match &paste.pacing.prompt {
                    Some(prompt) if watched && !aborted() => {
                        answering = true;
                        self.report_running();
                        answer = self
                            .wait_for_prompt(prompt.timeout_ms, step.delay_ms, &signal)
                            .await;
                    }
                    _ if step.delay_ms > 0 && !aborted() => {
                        self.report_running();
                        let delay = Duration::from_millis(step.delay_ms.into());
                        if answering {
                            answer = self.read_answer(delay, false, &signal).await;
                        } else {
                            let mut abort = signal.clone();
                            tokio::select! {
                                () = tokio::time::sleep(delay) => {}
                                _ = abort.wait_for(|stop| *stop) => {}
                            }
                        }
                    }
                    _ => {}
                }
                if let Some(problem) =
                    stop_reason(answer.as_ref(), sent_line.0, paste.lines, &sent_line.1)
                {
                    stopped = Some(problem);
                    self.stop();
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
            state.watch = None;
            let progress = if state.closed {
                None
            } else if let Some(message) = stopped {
                state.stops += 1;
                Some(PasteProgress {
                    state: PasteProgressState::Stopped,
                    line: state.lines_done,
                    lines: state.lines,
                    sent: state.sent,
                    total: state.total,
                    message: Some(message),
                    stop_id: Some(state.stops),
                })
            } else if state.reported {
                let outcome = if cancelled {
                    PasteProgressState::Cancelled
                } else {
                    PasteProgressState::Done
                };
                self.report_progress(&mut state, outcome, true)
            } else {
                None
            };
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

    /// Wait for the answer to the line just sent: the prompt, an error line or a failing
    /// pattern ends the wait. Without the prompt in time, the line falls back to its fixed
    /// delay, and an error that comes meanwhile still counts.
    async fn wait_for_prompt(
        &self,
        timeout_ms: u32,
        delay_ms: u32,
        signal: &watch::Receiver<bool>,
    ) -> Option<PromptWatchResult> {
        let answer = self
            .read_answer(Duration::from_millis(timeout_ms.into()), true, signal)
            .await;
        if answer.is_some() || *signal.borrow() || delay_ms <= timeout_ms {
            return answer;
        }
        self.read_answer(
            Duration::from_millis((delay_ms - timeout_ms).into()),
            false,
            signal,
        )
        .await
    }

    /// Read the answer for up to `limit`; the prompt is looked for only while it is awaited.
    async fn read_answer(
        &self,
        limit: Duration,
        until_prompt: bool,
        signal: &watch::Receiver<bool>,
    ) -> Option<PromptWatchResult> {
        let deadline = Instant::now() + limit;
        let mut abort = signal.clone();
        loop {
            let woken = self.inner.output.notified();
            let result = self
                .state()
                .watch
                .as_mut()
                .and_then(|watch| watch.check(until_prompt));
            if result.is_some() {
                return result;
            }
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => return None,
                _ = abort.wait_for(|stop| *stop) => return None,
                () = woken => {}
            }
        }
    }
}

/// Why a paste has to stop at this answer to its line `line` of `lines`, if it does.
fn stop_reason(
    answer: Option<&PromptWatchResult>,
    line: u64,
    lines: u64,
    text: &str,
) -> Option<String> {
    let sent = || {
        let quoted = pasted_line_text(text);
        if quoted.is_empty() {
            format!("Line {line} of {lines}")
        } else {
            format!("Line {line} of {lines} (“{quoted}”)")
        }
    };
    match answer? {
        PromptWatchResult::Error { line: output } => {
            Some(format!("{} was answered with “{output}”", sent()))
        }
        PromptWatchResult::Failed { message } => Some(format!("{}: {message}.", sent())),
        PromptWatchResult::Prompt => None,
    }
}
