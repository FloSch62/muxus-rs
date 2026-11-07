//! Port of `tests/unit/server/paste-pacer.test.ts`: paced pastes on the backend's timers.
//!
//! The TypeScript checks output on the event-loop turn after it arrives, which its fake timers
//! place 1 ms later; here the paste reading the answer wakes at the instant the output arrives,
//! so a line sent on the prompt goes out 1 ms sooner than in the TypeScript expectations.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use muxus_core::api::TerminalEnterKey;
use muxus_core::paste_pacing::{
    DEFAULT_PASTE_ERROR_PATTERN, DEFAULT_PASTE_PROMPT_PATTERN, PastePromptWait,
};
use muxus_core::protocol::PasteProgressState;
use muxus_session::terminal::paste::{PastePacer, PasteProgress, PasteRequest};
use tokio::time::Instant;

type WriteResult = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
/// Sees each write (and the pacer, to answer it); may hold the write with its own future.
type Custom = Arc<dyn Fn(&str, &PastePacer) -> Option<WriteResult> + Send + Sync>;

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

    fn times(&self) -> Vec<u64> {
        self.writes().into_iter().map(|(at, _)| at).collect()
    }

    fn reports(&self) -> Vec<PasteProgress> {
        self.reports.lock().unwrap().clone()
    }

    fn last(&self) -> PasteProgress {
        self.reports().last().cloned().expect("a report")
    }
}

/// A pacer whose writes are recorded with the paused clock's time.
fn recording_pacer(custom: Option<Custom>) -> Recording {
    let writes: Arc<Mutex<Vec<(u64, String)>>> = Arc::default();
    let reports: Arc<Mutex<Vec<PasteProgress>>> = Arc::default();
    let start = Instant::now();
    let sink = writes.clone();
    let progress = reports.clone();
    let handle: Arc<OnceLock<PastePacer>> = Arc::default();
    let own = handle.clone();
    let pacer = PastePacer::new(
        Arc::new(move |data: Vec<u8>| {
            let text = String::from_utf8(data).unwrap();
            sink.lock()
                .unwrap()
                .push((start.elapsed().as_millis() as u64, text.clone()));
            let pacer = own.get().expect("the pacer").clone();
            match custom.as_ref().and_then(|custom| custom(&text, &pacer)) {
                Some(future) => future,
                None => Box::pin(async { Ok(()) }),
            }
        }),
        Arc::new(move |report| progress.lock().unwrap().push(report)),
    );
    let _ = handle.set(pacer.clone());
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
        ..PasteRequest::default()
    }
}

/// Output the session shows `after_ms` from now.
fn output_later(pacer: &PastePacer, after_ms: u64, chunk: String) {
    let pacer = pacer.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(after_ms)).await;
        pacer.output(chunk.as_bytes());
    });
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
        message: None,
        stop_id: None,
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
    assert_eq!(r.last(), progress(PasteProgressState::Done, 3, 3, 43, 43));
}

#[tokio::test(start_paused = true)]
async fn reports_running_from_the_first_write_until_the_queue_is_done_or_cancelled() {
    let r = recording_pacer(None);
    assert!(!r.pacer.running());
    r.pacer.enqueue(paste("one\ntwo\n", 250, 0, false));
    assert!(r.pacer.running());
    run_all_timers().await;
    assert!(!r.pacer.running());

    r.pacer.enqueue(paste("one\ntwo\n", 250, 0, false));
    r.pacer.cancel();
    run_all_timers().await;
    assert!(!r.pacer.running());
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
async fn types_line_breaks_as_the_hosts_enter_key_a_line_at_a_time_as_with_cr() {
    let r = recording_pacer(None);
    r.pacer.enqueue(PasteRequest {
        enter_key: Some(TerminalEnterKey::Crlf),
        ..paste("one\ntwo\n", 100, 0, false)
    });
    run_all_timers().await;
    assert_eq!(r.writes(), vec![at(0, "one\r\n"), at(100, "two\r\n")]);
    let last = r.last();
    assert_eq!(
        (last.state, last.line, last.lines),
        (PasteProgressState::Done, 2, 2)
    );
}

#[tokio::test(start_paused = true)]
async fn sends_an_lf_enter_key_as_one_keystroke_with_a_character_delay() {
    let r = recording_pacer(None);
    r.pacer.enqueue(PasteRequest {
        enter_key: Some(TerminalEnterKey::Lf),
        ..paste("a\r\nb", 100, 10, false)
    });
    run_all_timers().await;
    assert_eq!(r.writes(), vec![at(0, "a"), at(10, "\n"), at(120, "b")]);
}

#[tokio::test(start_paused = true)]
async fn keeps_cr_inside_a_bracketed_paste_whatever_the_enter_key() {
    let r = recording_pacer(None);
    r.pacer.enqueue(PasteRequest {
        enter_key: Some(TerminalEnterKey::Lf),
        ..paste("one\ntwo", 100, 0, true)
    });
    run_all_timers().await;
    assert_eq!(
        r.data(),
        vec!["\x1b[200~one\r".to_string(), "two\x1b[201~".into()]
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
    let custom: Custom = Arc::new(move |data: &str, _| {
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
    let last = r.last();
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
    let last = r.last();
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
    let last = r.last();
    assert_eq!(
        (last.state, last.sent, last.total),
        (PasteProgressState::Cancelled, 4, 15)
    );

    // The next paste starts afresh.
    r.pacer.enqueue(paste("e\nf\n", 100, 0, false));
    run_all_timers().await;
    assert_eq!(r.data()[3..], ["e\r".to_string(), "f\r".to_string()]);
    let last = r.last();
    assert_eq!(
        (last.state, last.lines, last.total),
        (PasteProgressState::Done, 2, 4)
    );
}

#[tokio::test(start_paused = true)]
async fn keeps_a_paste_sent_in_the_same_tick_as_a_cancel() {
    let r = recording_pacer(None);
    r.pacer.enqueue(paste("a\nb\nc\n", 100, 0, true));
    advance(150).await;
    // Frames that arrive together are handled back to back.
    r.pacer.cancel();
    r.pacer.enqueue(paste("d\ne\n", 100, 0, false));
    run_all_timers().await;
    assert_eq!(
        r.data(),
        vec![
            "\x1b[200~a\r".to_string(),
            "b\r".into(),
            "\x1b[201~".into(),
            "d\r".into(),
            "e\r".into(),
        ]
    );
    let cancelled = r
        .reports()
        .into_iter()
        .find(|report| report.state == PasteProgressState::Cancelled)
        .expect("a cancelled report");
    assert_eq!(
        (cancelled.lines, cancelled.sent, cancelled.total),
        (3, 4, 6)
    );
    let last = r.last();
    assert_eq!(
        (last.state, last.lines, last.sent, last.total),
        (PasteProgressState::Done, 2, 4, 4)
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
    let custom: Custom = Arc::new(|data: &str, _| {
        (data == "b\r")
            .then(|| -> WriteResult { Box::pin(async { Err("port closed".to_string()) }) })
    });
    let r = recording_pacer(Some(custom));
    r.pacer.enqueue(paste("a\nb\nc\n", 100, 0, false));
    run_all_timers().await;
    assert_eq!(r.data(), vec!["a\r".to_string(), "b\r".into()]);
    assert_eq!(r.last().state, PasteProgressState::Cancelled);
}

// --- pastes that wait for the prompt ---

const PROMPT: &str = "R1(config)#";
const REJECTED: &str = "% Invalid input detected at '^' marker.";

fn prompted(timeout_ms: u32) -> PastePromptWait {
    PastePromptWait {
        prompt_pattern: DEFAULT_PASTE_PROMPT_PATTERN.into(),
        timeout_ms,
        error_pattern: DEFAULT_PASTE_ERROR_PATTERN.into(),
    }
}

fn waiting(
    text: &str,
    line_delay_ms: u32,
    char_delay_ms: u32,
    prompt: PastePromptWait,
) -> PasteRequest {
    PasteRequest {
        prompt: Some(prompt),
        ..paste(text, line_delay_ms, char_delay_ms, false)
    }
}

type Answer = Arc<dyn Fn(&str) -> Option<Vec<String>> + Send + Sync>;

/// A pacer typing into a fake IOS console: each line is echoed, then answered after
/// `answer_ms` with the prompt, or rejected first when it starts with "bogus". `answer`
/// replaces that reply, in chunks.
fn console_pacer(answer_ms: u64, enter: &'static str, answer: Option<Answer>) -> Recording {
    let custom: Custom = Arc::new(move |text: &str, pacer: &PastePacer| {
        let line = text.strip_suffix(enter)?;
        let reply = answer
            .as_ref()
            .and_then(|answer| answer(line))
            .unwrap_or_else(|| {
                vec![
                    format!("{line}\r\n"),
                    if line.starts_with("bogus") {
                        format!("        ^\r\n{REJECTED}\r\n\r\n{PROMPT}")
                    } else {
                        PROMPT.to_string()
                    },
                ]
            });
        for (index, chunk) in reply.into_iter().enumerate() {
            output_later(pacer, answer_ms * (index as u64 + 1), chunk);
        }
        None
    });
    recording_pacer(Some(custom))
}

fn console() -> Recording {
    console_pacer(30, "\r", None)
}

fn answering(answer: impl Fn(&str) -> Option<Vec<String>> + Send + Sync + 'static) -> Recording {
    console_pacer(30, "\r", Some(Arc::new(answer)))
}

fn stopped(r: &Recording) -> (u64, String) {
    let last = r.last();
    assert_eq!(last.state, PasteProgressState::Stopped, "{last:?}");
    (last.line, last.message.unwrap_or_default())
}

#[tokio::test(start_paused = true)]
async fn sends_each_line_once_the_prompt_is_back_instead_of_after_the_line_delay() {
    let r = console();
    r.pacer.enqueue(waiting(
        "interface ge-0/0/1\ndescription uplink\nexit\n",
        5000,
        0,
        prompted(1000),
    ));
    run_all_timers().await;
    assert_eq!(
        r.writes(),
        vec![
            at(0, "interface ge-0/0/1\r"),
            at(60, "description uplink\r"),
            at(120, "exit\r"),
        ]
    );
    // The last line waits for its answer too, so an error there is still caught.
    assert_eq!(r.last(), progress(PasteProgressState::Done, 3, 3, 43, 43));
}

#[tokio::test(start_paused = true)]
async fn stops_at_the_first_rejected_line_and_reports_it_with_the_output_dropping_queued_pastes() {
    let r = console();
    r.pacer.enqueue(waiting(
        "interface ge-0/0/1\nbogus command\nshutdown\n",
        0,
        0,
        prompted(1000),
    ));
    r.pacer.enqueue(paste("queued\n", 100, 0, false));
    r.pacer.enqueue(paste("queued too\n", 100, 0, false));
    run_all_timers().await;
    assert_eq!(
        r.data(),
        vec!["interface ge-0/0/1\r".to_string(), "bogus command\r".into()]
    );
    // Progress counts every queued line; the message counts within the paste that stopped.
    assert_eq!(
        r.last(),
        PasteProgress {
            state: PasteProgressState::Stopped,
            line: 2,
            lines: 5,
            sent: 33,
            total: 60,
            message: Some(format!(
                "Line 2 of 3 (“bogus command”) was answered with “{REJECTED}”"
            )),
            stop_id: Some(1),
        }
    );

    // The next paste starts afresh, and a later stop gets a number of its own.
    r.pacer.enqueue(paste("next\n", 100, 0, false));
    run_all_timers().await;
    assert_eq!(r.data().last().unwrap(), "next\r");
    r.pacer
        .enqueue(waiting("bogus again\n", 0, 0, prompted(1000)));
    run_all_timers().await;
    let last = r.last();
    assert_eq!(last.state, PasteProgressState::Stopped);
    assert_eq!(
        last.message.as_deref(),
        Some(format!("Line 1 of 1 (“bogus again”) was answered with “{REJECTED}”").as_str())
    );
    assert_eq!(last.stop_id, Some(2));
}

#[tokio::test(start_paused = true)]
async fn stops_at_a_rejection_of_the_last_line_which_waits_for_its_answer_too() {
    let r = console();
    r.pacer.enqueue(waiting(
        "interface ge-0/0/1\nbogus command\n",
        0,
        0,
        prompted(1000),
    ));
    run_all_timers().await;
    assert_eq!(r.writes().len(), 2);
    assert_eq!(
        stopped(&r),
        (
            2,
            format!("Line 2 of 2 (“bogus command”) was answered with “{REJECTED}”")
        )
    );
}

#[tokio::test(start_paused = true)]
async fn waits_for_the_prompt_and_stops_at_a_rejected_line_with_the_hosts_lf_enter_key() {
    let r = console_pacer(30, "\n", None);
    r.pacer.enqueue(PasteRequest {
        enter_key: Some(TerminalEnterKey::Lf),
        ..waiting(
            "interface ge-0/0/1\nbogus command\nshutdown\n",
            5000,
            0,
            prompted(1000),
        )
    });
    r.pacer.enqueue(PasteRequest {
        enter_key: Some(TerminalEnterKey::Lf),
        ..paste("queued\n", 100, 0, false)
    });
    run_all_timers().await;
    // Each line ends in LF and goes once the prompt is back, not after the line delay.
    assert_eq!(
        r.writes(),
        vec![at(0, "interface ge-0/0/1\n"), at(60, "bogus command\n")]
    );
    let last = r.last();
    assert_eq!((last.lines, last.stop_id), (4, Some(1)));
    assert_eq!(
        stopped(&r),
        (
            2,
            format!("Line 2 of 3 (“bogus command”) was answered with “{REJECTED}”")
        )
    );
}

#[tokio::test(start_paused = true)]
async fn falls_back_to_the_line_delay_for_a_line_whose_prompt_does_not_come_in_time() {
    let quiet = answering(|line| Some(vec![format!("{line}\r\n")]));
    quiet
        .pacer
        .enqueue(waiting("one\ntwo\nthree\n", 0, 0, prompted(500)));
    run_all_timers().await;
    assert_eq!(quiet.times(), vec![0, 500, 1000]);
    assert_eq!(quiet.last().state, PasteProgressState::Done);

    // A line delay longer than the timeout still holds; the next line waits for the prompt
    // again.
    let slow = answering(|line| (line == "one").then(|| vec![format!("{line}\r\n")]));
    slow.pacer
        .enqueue(waiting("one\ntwo\nthree\n", 800, 0, prompted(500)));
    run_all_timers().await;
    assert_eq!(slow.times(), vec![0, 800, 860]);
}

#[tokio::test(start_paused = true)]
async fn still_stops_at_a_rejection_that_comes_while_a_line_falls_back_to_the_line_delay() {
    // Every answer comes after the timeout: the echo at 400 ms, the rest at 800 ms.
    let r = console_pacer(400, "\r", None);
    r.pacer
        .enqueue(waiting("one\nbogus\nthree\n", 1500, 0, prompted(300)));
    run_all_timers().await;
    assert_eq!(r.writes(), vec![at(0, "one\r"), at(1500, "bogus\r")]);
    assert_eq!(
        stopped(&r),
        (
            2,
            format!("Line 2 of 3 (“bogus”) was answered with “{REJECTED}”")
        )
    );
}

#[tokio::test(start_paused = true)]
async fn still_stops_at_a_rejection_that_comes_while_the_next_line_is_typed_a_character_at_a_time()
{
    let line_ends = Arc::new(Mutex::new(0));
    let custom: Custom = Arc::new(move |text: &str, pacer: &PastePacer| {
        if text != "\r" {
            return None;
        }
        let mut ends = line_ends.lock().unwrap();
        *ends += 1;
        // The second line is rejected only once the next one is being typed.
        let (reply, after) = if *ends == 2 {
            (format!("\r\n{REJECTED}\r\n"), 1515)
        } else {
            (format!("\r\n{PROMPT}"), 20)
        };
        output_later(pacer, after, reply);
        None
    });
    let r = recording_pacer(Some(custom));
    r.pacer
        .enqueue(waiting("a\nb\ncd\n", 1500, 10, prompted(300)));
    run_all_timers().await;
    assert_eq!(
        r.writes(),
        vec![
            at(0, "a"),
            at(10, "\r"),
            at(30, "b"),
            at(40, "\r"),
            at(1550, "c")
        ]
    );
    // The line it answers, not the one being typed.
    assert_eq!(
        stopped(&r),
        (
            2,
            format!("Line 2 of 3 (“b”) was answered with “{REJECTED}”")
        )
    );
}

#[tokio::test(start_paused = true)]
async fn reads_the_prompt_and_errors_through_escape_sequences_and_chunk_boundaries() {
    let r = answering(|line| {
        Some(if line == "show clock" {
            vec![
                format!("{line}\r"),
                "\n\x1b[1mR1(con".into(),
                "fig)\x1b[0".into(),
                "m#".into(),
            ]
        } else {
            vec![
                format!("{line}\r\n% Inv"),
                "alid input detected\x1b[K\r".into(),
                format!("\n{PROMPT}"),
            ]
        })
    });
    r.pacer
        .enqueue(waiting("show clock\nbogus\nexit\n", 0, 0, prompted(1000)));
    run_all_timers().await;
    assert_eq!(r.writes(), vec![at(0, "show clock\r"), at(120, "bogus\r")]);
    assert_eq!(
        stopped(&r),
        (
            2,
            "Line 2 of 3 (“bogus”) was answered with “% Invalid input detected”".into()
        )
    );
}

#[tokio::test(start_paused = true)]
async fn takes_neither_the_echo_of_a_line_ending_in_hash_nor_earlier_output_for_the_answer() {
    let r = answering(|line| Some(vec![line.to_string(), "\r\n".into(), PROMPT.into()]));
    // Rejected before the paste started: not an answer to anything it sends.
    r.pacer.output(format!("{REJECTED}\r\n").as_bytes());
    r.pacer
        .enqueue(waiting("banner motd #\nexit\n", 0, 0, prompted(1000)));
    run_all_timers().await;
    assert_eq!(r.writes(), vec![at(0, "banner motd #\r"), at(90, "exit\r")]);
    assert_eq!(r.last().state, PasteProgressState::Done);
}

#[tokio::test(start_paused = true)]
async fn waits_for_the_prompt_after_the_line_break_of_a_paste_typed_a_character_at_a_time() {
    let r = console();
    r.pacer.enqueue(waiting("ab\nc", 1000, 10, prompted(1000)));
    run_all_timers().await;
    assert_eq!(
        r.writes(),
        vec![at(0, "a"), at(10, "b"), at(20, "\r"), at(80, "c")]
    );
}

#[tokio::test(start_paused = true)]
async fn keeps_the_delays_for_a_bracketed_paste_which_shows_no_prompt_after_each_line() {
    let r = answering(|line| Some(vec![format!("{line}\r\n{REJECTED}\r\n")]));
    r.pacer.enqueue(PasteRequest {
        bracketed: true,
        ..waiting("bogus\nexit\n", 100, 0, prompted(1000))
    });
    run_all_timers().await;
    assert_eq!(
        r.writes(),
        vec![at(0, "\x1b[200~bogus\r"), at(100, "exit\r\x1b[201~")]
    );
    assert_eq!(r.last().state, PasteProgressState::Done);
}

// The TypeScript pattern `^(a+)+$` backtracks for ever in V8; here it runs on a linear-time
// engine. A lookaround needs the backtracking engine, which gives up at its effort limit.
#[tokio::test(start_paused = true)]
async fn stops_the_paste_not_the_backend_when_a_pattern_backtracks_too_long() {
    let r = answering(|line| Some(vec![format!("{line}\r\n{}!", "a".repeat(32))]));
    r.pacer.enqueue(waiting(
        "one\ntwo\n",
        0,
        0,
        PastePromptWait {
            prompt_pattern: "^((?=a)a+)+$".into(),
            ..prompted(1000)
        },
    ));
    run_all_timers().await;
    assert_eq!(r.data(), vec!["one\r".to_string()]);
    assert_eq!(
        stopped(&r),
        (
            1,
            "Line 1 of 2 (“one”): the prompt pattern took too long to match.".into()
        )
    );
}

#[tokio::test(start_paused = true)]
async fn matches_the_prompt_only_while_it_waits_for_it_not_on_the_echo_of_a_line_being_typed() {
    // The device echoes each character, and answers a line break with its prompt.
    let custom: Custom = Arc::new(|text: &str, pacer: &PastePacer| {
        let reply = if text == "\r" {
            "\r\nR1#".to_string()
        } else {
            text.to_string()
        };
        output_later(pacer, 1, reply);
        None
    });
    let r = recording_pacer(Some(custom));
    let long = "a".repeat(40);
    r.pacer.enqueue(waiting(
        &format!("interface Gi0/1\ndescription {long}\nexit\n"),
        0,
        5,
        PastePromptWait {
            prompt_pattern: "(\\w+\\s?)+#\\s*$".into(),
            ..prompted(1000)
        },
    ));
    run_all_timers().await;
    assert_eq!(
        r.data().concat(),
        format!("interface Gi0/1\rdescription {long}\rexit\r")
    );
    // Each line went on the prompt, not after the timeout.
    assert!(r.writes().last().unwrap().0 < 1000);
    assert_eq!(r.last().state, PasteProgressState::Done);
}

#[tokio::test(start_paused = true)]
async fn sends_nothing_of_a_paste_whose_pattern_does_not_compile() {
    let r = console();
    r.pacer.enqueue(waiting(
        "one\n",
        0,
        0,
        PastePromptWait {
            error_pattern: "([".into(),
            ..prompted(1000)
        },
    ));
    run_all_timers().await;
    assert!(r.writes().is_empty());
    let last = r.last();
    assert_eq!(
        (last.state, last.line, last.stop_id),
        (PasteProgressState::Stopped, 0, Some(1))
    );
    assert!(
        last.message
            .unwrap()
            .contains("The prompt or error pattern cannot be used")
    );
}

#[tokio::test(start_paused = true)]
async fn ends_a_prompt_wait_and_a_read_of_the_answer_at_once_on_cancel_or_close() {
    for close in [false, true] {
        let end = |pacer: &PastePacer| {
            if close {
                pacer.close();
            } else {
                pacer.cancel();
            }
        };
        let reported: Vec<PasteProgressState> = if close {
            vec![]
        } else {
            vec![PasteProgressState::Cancelled]
        };
        // A prompt that never comes, with a line delay longer than the timeout to fall back to.
        let waiting_for_prompt = answering(|line| Some(vec![format!("{line}\r\n")]));
        waiting_for_prompt
            .pacer
            .enqueue(waiting("one\ntwo\n", 5000, 0, prompted(5000)));
        advance(100).await;
        let before = waiting_for_prompt.reports().len();
        end(&waiting_for_prompt.pacer);
        advance(0).await;
        assert_eq!(waiting_for_prompt.data(), vec!["one\r".to_string()]);
        let states: Vec<_> = waiting_for_prompt.reports()[before..]
            .iter()
            .map(|report| report.state)
            .collect();
        assert_eq!(states, reported);
        assert!(!waiting_for_prompt.pacer.running());

        // The answer to the first line is read while the second is typed a character at a time.
        let typing = console();
        typing
            .pacer
            .enqueue(waiting("a\nbc\n", 0, 1000, prompted(5000)));
        advance(1500).await;
        assert_eq!(
            typing.data(),
            vec!["a".to_string(), "\r".into(), "b".into()]
        );
        let before = typing.reports().len();
        end(&typing.pacer);
        advance(0).await;
        let states: Vec<_> = typing.reports()[before..]
            .iter()
            .map(|report| report.state)
            .collect();
        assert_eq!(states, reported);
        assert!(!typing.pacer.running());
    }
}

#[tokio::test(start_paused = true)]
async fn reads_an_answer_that_came_while_the_write_before_it_was_still_pending() {
    /// The transport holds the writes `held` picks for 50 ms; the device rejects the line
    /// meanwhile.
    fn holding(held: fn(&str) -> bool) -> Recording {
        let custom: Custom = Arc::new(move |text: &str, pacer: &PastePacer| {
            if !held(text) {
                return None;
            }
            output_later(pacer, 10, format!("\r\n{REJECTED}\r\n"));
            Some(Box::pin(async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(())
            }))
        });
        recording_pacer(Some(custom))
    }

    // Line by line: the wait for the prompt starts once the line has been written.
    let lines = holding(|_| true);
    lines
        .pacer
        .enqueue(waiting("bogus\nexit\n", 0, 0, prompted(1000)));
    run_all_timers().await;
    assert_eq!(lines.data(), vec!["bogus\r".to_string()]);
    assert_eq!(stopped(&lines).0, 1);

    // A character at a time: line one has no answer within the timeout, and its rejection
    // comes while the next character is being written.
    let chars = holding(|text| text == "x");
    chars
        .pacer
        .enqueue(waiting("a\nxy\n", 0, 20, prompted(300)));
    run_all_timers().await;
    assert_eq!(chars.data(), vec!["a".to_string(), "\r".into(), "x".into()]);
    assert_eq!(
        stopped(&chars),
        (
            1,
            format!("Line 1 of 2 (“a”) was answered with “{REJECTED}”")
        )
    );
}

#[tokio::test(start_paused = true)]
async fn reads_no_output_while_no_paste_waits_for_the_prompt() {
    let r = console();
    r.pacer.enqueue(paste("bogus\nexit\n", 100, 0, false));
    run_all_timers().await;
    assert_eq!(r.times(), vec![0, 100]);
}
