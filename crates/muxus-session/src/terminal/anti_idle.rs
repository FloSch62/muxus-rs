//! A host's anti-idle text, typed into an idle session (`server/src/ws/anti-idle.ts`).

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use muxus_core::api::{AntiIdle, TerminalEnterKey};
use muxus_core::paste_pacing::with_enter_key;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// How soon a wait that ended while something else was typing looks again.
const BUSY_RECHECK: Duration = Duration::from_millis(1_000);

/// Where the timer types, and what it asks the session.
#[derive(Clone)]
pub struct AntiIdleIo {
    /// Type into the session the way typed input travels.
    pub write: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
    /// A paced paste or a login sequence is typing into the session.
    pub busy: Arc<dyn Fn() -> bool + Send + Sync>,
    /// What Enter sends on the host, read at each send.
    pub enter_key: Arc<dyn Fn() -> TerminalEnterKey + Send + Sync>,
}

/// What a setting types: its text as is, then the host's Enter only when asked for.
pub fn anti_idle_input(setting: &AntiIdle, enter_key: TerminalEnterKey) -> Vec<u8> {
    if setting.enter {
        format!("{}{}", setting.text, with_enter_key("\r", enter_key)).into_bytes()
    } else {
        setting.text.clone().into_bytes()
    }
}

struct State {
    last_input_at: Instant,
    held: bool,
    start_on_input: bool,
    task: Option<JoinHandle<()>>,
    closed: bool,
}

struct Inner {
    setting: AntiIdle,
    interval: Duration,
    io: AntiIdleIo,
    state: Mutex<State>,
}

/// Types a host's anti-idle text into one session each time it goes a full interval without
/// input. Any input starts the wait over. While a paced paste or a login sequence runs the wait
/// is held, and a full interval starts once it is done, so the text never lands inside either.
/// Time is monotonic, so a wall-clock change cannot delay or hasten a send. Must be used inside
/// a tokio runtime.
#[derive(Clone)]
pub struct AntiIdleTimer {
    inner: Arc<Inner>,
}

impl AntiIdleTimer {
    pub fn new(setting: AntiIdle, io: AntiIdleIo) -> Self {
        Self {
            inner: Arc::new(Inner {
                interval: Duration::from_secs(setting.interval_seconds.into()),
                setting,
                io,
                state: Mutex::new(State {
                    last_input_at: Instant::now(),
                    held: false,
                    start_on_input: false,
                    task: None,
                    closed: false,
                }),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn start(&self) {
        let mut state = self.state();
        if state.closed || state.task.is_some() {
            return;
        }
        state.start_on_input = false;
        state.last_input_at = Instant::now();
        let timer = self.clone();
        state.task = Some(tokio::spawn(async move { timer.run().await }));
    }

    /// Start the first wait at the session's first input rather than now, for sessions that log
    /// in after they open: one left at a login prompt must not get the text as a username.
    pub fn start_after_first_input(&self) {
        let mut state = self.state();
        if state.closed || state.task.is_some() {
            return;
        }
        state.start_on_input = true;
    }

    /// Input reached the session.
    pub fn input(&self) {
        let start = {
            let mut state = self.state();
            if !state.start_on_input {
                state.last_input_at = Instant::now();
            }
            state.start_on_input
        };
        if start {
            self.start();
        }
    }

    /// The session is gone.
    pub fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        if let Some(task) = state.task.take() {
            task.abort();
        }
    }

    async fn run(self) {
        let interval = self.inner.interval;
        let mut wait = interval;
        loop {
            tokio::time::sleep(wait).await;
            if self.state().closed {
                return;
            }
            if (self.inner.io.busy)() {
                self.state().held = true;
                wait = BUSY_RECHECK;
                continue;
            }
            let now = Instant::now();
            {
                let mut state = self.state();
                if state.closed {
                    return;
                }
                // The first look that finds the hold over starts a full wait, never a shorter one.
                if state.held {
                    state.held = false;
                    state.last_input_at = now;
                }
                // Input only moves the mark; the timer catches up here rather than per keystroke.
                let idle = now.duration_since(state.last_input_at);
                if idle < interval {
                    wait = interval - idle;
                    continue;
                }
                state.last_input_at = now;
            }
            let enter_key = (self.inner.io.enter_key)();
            (self.inner.io.write)(anti_idle_input(&self.inner.setting, enter_key));
            wait = interval;
        }
    }
}

/// Port of `tests/unit/server/anti-idle.test.ts` (the timer and the input; the metadata schema
/// half lives with the route schemas in muxus-core).
#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    fn setting(text: &str, enter: bool) -> AntiIdle {
        AntiIdle {
            interval_seconds: 10,
            text: text.into(),
            enter,
        }
    }

    struct Recording {
        timer: AntiIdleTimer,
        typed: Arc<Mutex<Vec<(u64, String)>>>,
        busy: Arc<AtomicBool>,
    }

    impl Recording {
        fn typed(&self) -> Vec<(u64, String)> {
            self.typed.lock().unwrap().clone()
        }
    }

    fn recording(start: bool) -> Recording {
        let typed: Arc<Mutex<Vec<(u64, String)>>> = Arc::default();
        let busy = Arc::new(AtomicBool::new(false));
        let began = Instant::now();
        let sink = typed.clone();
        let is_busy = busy.clone();
        let timer = AntiIdleTimer::new(
            setting(" ", false),
            AntiIdleIo {
                write: Arc::new(move |data| {
                    sink.lock().unwrap().push((
                        began.elapsed().as_millis() as u64,
                        String::from_utf8(data).unwrap(),
                    ))
                }),
                busy: Arc::new(move || is_busy.load(Ordering::SeqCst)),
                enter_key: Arc::new(|| TerminalEnterKey::Cr),
            },
        );
        if start {
            timer.start();
        }
        Recording { timer, typed, busy }
    }

    /// `vi.advanceTimersByTimeAsync`.
    async fn advance(ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
    }

    fn at(ms: u64) -> (u64, String) {
        (ms, " ".into())
    }

    #[test]
    fn types_the_text_as_is_and_presses_enter_only_when_asked_to() {
        let cr = TerminalEnterKey::Cr;
        assert_eq!(anti_idle_input(&setting(" ", false), cr), b" ");
        assert_eq!(
            anti_idle_input(&setting("show clock", true), cr),
            b"show clock\r"
        );
        assert_eq!(anti_idle_input(&setting("", true), cr), b"\r");
    }

    #[test]
    fn presses_the_hosts_enter_key() {
        let on = setting("show clock", true);
        assert_eq!(
            anti_idle_input(&on, TerminalEnterKey::Crlf),
            b"show clock\r\n"
        );
        assert_eq!(anti_idle_input(&on, TerminalEnterKey::Lf), b"show clock\n");
        assert_eq!(
            anti_idle_input(&setting("show clock", false), TerminalEnterKey::Lf),
            b"show clock"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn types_after_each_full_interval_without_input() {
        let r = recording(true);
        advance(9_999).await;
        assert_eq!(r.typed(), vec![]);
        advance(20_001).await;
        assert_eq!(r.typed(), vec![at(10_000), at(20_000), at(30_000)]);
        r.timer.close();
    }

    #[tokio::test(start_paused = true)]
    async fn starts_the_wait_over_on_any_input() {
        let r = recording(true);
        advance(6_000).await;
        r.timer.input();
        advance(6_000).await;
        r.timer.input();
        advance(9_999).await;
        assert_eq!(r.typed(), vec![]);
        advance(1).await;
        assert_eq!(r.typed(), vec![at(22_000)]);
        r.timer.close();
    }

    #[tokio::test(start_paused = true)]
    async fn holds_while_something_else_types_then_waits_a_full_interval() {
        let r = recording(true);
        r.busy.store(true, Ordering::SeqCst);
        advance(60_000).await;
        assert_eq!(r.typed(), vec![]);
        r.busy.store(false, Ordering::SeqCst);
        advance(10_000).await;
        assert_eq!(r.typed(), vec![]);
        advance(1_000).await;
        // A full interval from the first look that found it done, at 61 s.
        assert_eq!(r.typed(), vec![at(71_000)]);
        r.timer.close();
    }

    #[tokio::test(start_paused = true)]
    async fn never_sends_sooner_than_a_full_interval_after_a_hold_that_ends_between_looks() {
        let r = recording(true);
        r.busy.store(true, Ordering::SeqCst);
        advance(60_500).await;
        r.busy.store(false, Ordering::SeqCst);
        advance(9_999).await;
        assert_eq!(r.typed(), vec![]);
        advance(1_000).await;
        assert_eq!(r.typed(), vec![at(71_000)]);
        r.timer.close();
    }

    #[tokio::test(start_paused = true)]
    async fn can_wait_for_the_first_input_before_it_starts_counting() {
        let r = recording(false);
        r.timer.start_after_first_input();
        advance(60_000).await;
        assert_eq!(r.typed(), vec![]);
        assert!(r.timer.state().task.is_none());

        r.timer.input();
        advance(9_999).await;
        assert_eq!(r.typed(), vec![]);
        advance(1).await;
        assert_eq!(r.typed(), vec![at(70_000)]);
        r.timer.close();
    }

    #[tokio::test(start_paused = true)]
    async fn stops_for_good_once_closed() {
        let r = recording(true);
        advance(10_000).await;
        r.timer.close();
        r.timer.start();
        advance(60_000).await;
        assert_eq!(r.typed().len(), 1);
        assert!(r.timer.state().task.is_none());
    }
}
