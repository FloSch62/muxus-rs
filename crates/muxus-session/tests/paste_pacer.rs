//! Port of `tests/unit/server/paste-pacer.test.ts`: paced pastes on the backend's timers.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_core::protocol::PasteProgressState;
use muxus_session::terminal::paste::{PastePacer, PasteProgress, PasteRequest};
use tokio::time::Instant;

type WriteResult = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
type Custom = Arc<dyn Fn(&str) -> Option<WriteResult> + Send + Sync>;

struct Recording {
    pacer: PastePacer,
    writes: Arc<Mutex<Vec<(u64, String)>>>,
    reports: Arc<Mutex<Vec<PasteProgress>>>,
}

impl Recording {
    fn writes(&self) -> Vec<(u64, String)> {
        self.writes.lock().unwrap().clone()
    }

    fn data(&self) -> Vec<String> {
        self.writes().into_iter().map(|(_, data)| data).collect()
    }

    fn reports(&self) -> Vec<PasteProgress> {
        self.reports.lock().unwrap().clone()
    }
}

/// A pacer whose writes are recorded with the paused clock's time.
fn recording_pacer(custom: Option<Custom>) -> Recording {
    let writes: Arc<Mutex<Vec<(u64, String)>>> = Arc::default();
    let reports: Arc<Mutex<Vec<PasteProgress>>> = Arc::default();
    let start = Instant::now();
    let sink = writes.clone();
    let progress = reports.clone();
    let pacer = PastePacer::new(
        Arc::new(move |data: Vec<u8>| {
            let text = String::from_utf8(data).unwrap();
            sink.lock()
                .unwrap()
                .push((start.elapsed().as_millis() as u64, text.clone()));
            match custom.as_ref().and_then(|custom| custom(&text)) {
                Some(future) => future,
                None => Box::pin(async { Ok(()) }),
            }
        }),
        Arc::new(move |report| progress.lock().unwrap().push(report)),
    );
    Recording {
        pacer,
        writes,
        reports,
    }
}

fn paste(text: &str, line_delay_ms: u32, char_delay_ms: u32, bracketed: bool) -> PasteRequest {
    PasteRequest {
        text: text.into(),
        bracketed,
        line_delay_ms,
        char_delay_ms,
    }
}

/// Let every pending timer fire (`vi.runAllTimersAsync`).
async fn run_all_timers() {
    tokio::time::sleep(Duration::from_secs(3600)).await;
}

/// Move the clock (`vi.advanceTimersByTimeAsync`): timers due by then fire and the tasks they
/// wake run before the test looks.
async fn advance(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

fn progress(
    state: PasteProgressState,
    line: u64,
    lines: u64,
    sent: u64,
    total: u64,
) -> PasteProgress {
    PasteProgress {
        state,
        line,
        lines,
        sent,
        total,
    }
}

fn at(ms: u64, data: &str) -> (u64, String) {
    (ms, data.to_string())
}

#[tokio::test(start_paused = true)]
async fn types_a_line_at_a_time_with_the_line_delay_between_them() {
    let r = recording_pacer(None);
    r.pacer.enqueue(paste(
        "interface ge-0/0/1\ndescription uplink\nexit\n",
        250,
        0,
        false,
    ));
    run_all_timers().await;
    assert_eq!(
        r.writes(),
        vec![
            at(0, "interface ge-0/0/1\r"),
            at(250, "description uplink\r"),
            at(500, "exit\r"),
        ]
    );
    let reports = r.reports();
    assert_eq!(
        reports[0],
        progress(PasteProgressState::Running, 2, 3, 19, 43)
    );
    assert_eq!(
        *reports.last().unwrap(),
        progress(PasteProgressState::Done, 3, 3, 43, 43)
    );
}

#[tokio::test(start_paused = true)]
async fn types_a_character_at_a_time_with_a_character_delay() {
    let r = recording_pacer(None);
    r.pacer.enqueue(paste("ab\nc", 100, 10, false));
    run_all_timers().await;
    assert_eq!(
        r.writes(),
        vec![at(0, "a"), at(10, "b"), at(20, "\r"), at(130, "c")]
    );
}

#[tokio::test(start_paused = true)]
async fn encloses_the_whole_paste_in_one_pair_of_bracketed_paste_markers() {
    let r = recording_pacer(None);
    r.pacer
        .enqueue(paste("one\ntwo\nthree\x1b[201~\n", 100, 0, true));
    run_all_timers().await;
    assert_eq!(
        r.data(),
        vec![
            "\x1b[200~one\r".to_string(),
            "two\r".into(),
            "three\u{241b}[201~\r\x1b[201~".into(),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn starts_each_wait_only_once_the_transport_has_taken_the_write() {
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let custom: Custom = Arc::new(move |data: &str| {
        if data != "slow\r" {
            return None;
        }
        let rx = release_rx.lock().unwrap().take()?;
        Some(Box::pin(async move {
            let _ = rx.await;
            Ok(())
        }))
    });
    let r = recording_pacer(Some(custom));
    r.pacer.enqueue(paste("slow\nnext\n", 100, 0, false));
    advance(1000).await;
    assert_eq!(r.data(), vec!["slow\r".to_string()]);
    release_tx.send(()).unwrap();
    advance(99).await;
    assert_eq!(r.writes().len(), 1);
    advance(1).await;
    assert_eq!(r.writes().last().unwrap(), &at(1100, "next\r"));
}

#[tokio::test(start_paused = true)]
async fn queues_a_second_paste_behind_the_first_and_reports_progress_over_both() {
    let r = recording_pacer(None);
    r.pacer.enqueue(paste("a\nb\n", 100, 0, false));
    r.pacer.enqueue(paste("c\nd\n", 300, 0, false));
    run_all_timers().await;
    assert_eq!(
        r.writes(),
        vec![at(0, "a\r"), at(100, "b\r"), at(100, "c\r"), at(400, "d\r")]
    );
    let reports = r.reports();
    assert!(
        reports
            .iter()
            .all(|report| report.lines == 4 && report.total == 8)
    );
    let last = reports.last().unwrap();
    assert_eq!((last.state, last.sent), (PasteProgressState::Done, 8));
}

#[tokio::test(start_paused = true)]
async fn reports_a_long_paste_at_most_every_200_ms() {
    let r = recording_pacer(None);
    r.pacer.enqueue(paste(&"x\n".repeat(100), 10, 0, false));
    run_all_timers().await;
    let reports = r.reports();
    // 990 ms of waits: the first report, one per 200 ms, and the final one.
    assert!(
        reports
            .iter()
            .filter(|report| report.state == PasteProgressState::Running)
            .count()
            <= 6
    );
    let last = reports.last().unwrap();
    assert_eq!(
        (last.state, last.line, last.lines),
        (PasteProgressState::Done, 100, 100)
    );
}

#[tokio::test(start_paused = true)]
async fn sends_nothing_to_report_for_a_paste_that_needs_no_wait() {
    let r = recording_pacer(None);
    r.pacer.enqueue(paste("single line", 500, 0, false));
    run_all_timers().await;
    assert_eq!(r.data(), vec!["single line".to_string()]);
    assert!(r.reports().is_empty());
}

#[tokio::test(start_paused = true)]
async fn stops_on_cancel_closing_the_bracket_so_the_shell_leaves_paste_mode() {
    let r = recording_pacer(None);
    r.pacer.enqueue(paste("a\nb\nc\nd\n", 100, 0, true));
    r.pacer.enqueue(paste("queued\n", 100, 0, false));
    advance(150).await;
    r.pacer.cancel();
    run_all_timers().await;
    assert_eq!(
        r.data(),
        vec!["\x1b[200~a\r".to_string(), "b\r".into(), "\x1b[201~".into()]
    );
    let last = *r.reports().last().unwrap();
    assert_eq!(
        (last.state, last.sent, last.total),
        (PasteProgressState::Cancelled, 4, 15)
    );

    // The next paste starts afresh.
    r.pacer.enqueue(paste("e\nf\n", 100, 0, false));
    run_all_timers().await;
    assert_eq!(r.data()[3..], ["e\r".to_string(), "f\r".to_string()]);
    let last = *r.reports().last().unwrap();
    assert_eq!(
        (last.state, last.lines, last.total),
        (PasteProgressState::Done, 2, 4)
    );
}

#[tokio::test(start_paused = true)]
async fn stops_without_writing_or_reporting_anything_more_once_the_session_is_gone() {
    let r = recording_pacer(None);
    r.pacer.enqueue(paste("a\nb\nc\n", 100, 0, true));
    advance(50).await;
    let reported = r.reports().len();
    r.pacer.close();
    r.pacer.enqueue(paste("late\n", 100, 0, false));
    run_all_timers().await;
    assert_eq!(r.data(), vec!["\x1b[200~a\r".to_string()]);
    assert_eq!(r.reports().len(), reported);
}

#[tokio::test(start_paused = true)]
async fn gives_up_when_the_transport_fails_a_write() {
    let custom: Custom = Arc::new(|data: &str| {
        (data == "b\r")
            .then(|| -> WriteResult { Box::pin(async { Err("port closed".to_string()) }) })
    });
    let r = recording_pacer(Some(custom));
    r.pacer.enqueue(paste("a\nb\nc\n", 100, 0, false));
    run_all_timers().await;
    assert_eq!(r.data(), vec!["a\r".to_string(), "b\r".into()]);
    assert_eq!(
        r.reports().last().unwrap().state,
        PasteProgressState::Cancelled
    );
}
