//! Port of `tests/unit/server/session-recorder.test.ts`. The history store
//! is the in-memory sink; the TypeScript test used the real store.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use muxus_core::api::{
    SessionEndStatus, SessionLineTimestamp, SessionLogDirection, SessionLogFileSettings,
    SessionLogStatus, SessionLoggingPolicy,
};
use muxus_core::protocol::{
    LocalProfile, SerialProfile, SshProfile, TelnetProfile, TerminalProfile,
};
use muxus_session::logging::{
    Clock, ManualClock, MemoryHistory, RecorderConfig, SessionLoggingPatch, SessionLoggingState,
    SessionRecorder, SystemClock, TerminalTextNormalizer, session_profile_identity,
};

fn policy(enabled: bool, capture_input: bool) -> SessionLoggingPolicy {
    SessionLoggingPolicy {
        profile_key: "*".into(),
        enabled,
        capture_input,
        max_part_bytes: 5 * 1024 * 1024,
        max_parts: 10,
        log_to_file: false,
        overridden: false,
    }
}

fn ssh(target: &str) -> TerminalProfile {
    TerminalProfile::Ssh(serde_json::from_value(serde_json::json!({ "target": target })).unwrap())
}

fn recorder(
    history: &MemoryHistory,
    policy: SessionLoggingPolicy,
    profile: &TerminalProfile,
    title: Option<&str>,
    clock: Arc<dyn Clock>,
) -> SessionRecorder {
    SessionRecorder::start(RecorderConfig {
        history: Arc::new(history.clone()),
        policy,
        file_settings: Arc::new(|| SessionLogFileSettings {
            directory: Some("/nonexistent/muxus-test".into()),
            filename_pattern: "{host}.log".into(),
            timestamps: false,
        }),
        home: PathBuf::from("/nonexistent/home"),
        identity: session_profile_identity(profile),
        title: title.map(str::to_string),
        clock,
    })
}

fn enable() -> SessionLoggingPatch {
    SessionLoggingPatch {
        enabled: Some(true),
        ..SessionLoggingPatch::default()
    }
}

fn disable() -> SessionLoggingPatch {
    SessionLoggingPatch {
        enabled: Some(false),
        ..SessionLoggingPatch::default()
    }
}

fn stamp(offset: u64, recorded_at: &str) -> SessionLineTimestamp {
    SessionLineTimestamp {
        offset,
        recorded_at: recorded_at.into(),
    }
}

#[test]
fn persists_line_times_through_delayed_snapshots_and_coalesced_batches() {
    let history = MemoryHistory::new();
    let clock = ManualClock::at("2026-09-24T09:59:00.000Z");
    let recorder = recorder(
        &history,
        policy(false, false),
        &TerminalProfile::Local(LocalProfile::default()),
        None,
        Arc::new(clock.clone()),
    );
    recorder.set_state(&enable());
    clock.set_iso("2026-09-24T10:00:00.000Z");
    recorder.output(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n");
    clock.set_iso("2026-09-24T10:00:05.000Z");
    recorder.output(b"six\r\n");
    clock.set_iso("2026-09-24T11:00:00.000Z");
    recorder.end(SessionEndStatus::Completed);

    let session = history
        .session(recorder.state().session_id.as_deref().unwrap())
        .unwrap();
    let output: Vec<_> = session
        .events
        .iter()
        .filter(|event| event.direction == SessionLogDirection::Output)
        .collect();
    assert_eq!(
        output
            .iter()
            .map(|event| event.text.as_str())
            .collect::<String>(),
        "one\ntwo\nthree\nfour\nfive\nsix\n"
    );
    let times: Vec<&str> = output
        .iter()
        .flat_map(|event| event.line_timestamps.as_ref().unwrap())
        .map(|stamp| stamp.recorded_at.as_str())
        .collect();
    let mut expected = vec!["2026-09-24T10:00:00.000Z"; 5];
    expected.push("2026-09-24T10:00:05.000Z");
    assert_eq!(times, expected);
    let offsets: Vec<u64> = output[0]
        .line_timestamps
        .as_ref()
        .unwrap()
        .iter()
        .map(|stamp| stamp.offset)
        .collect();
    assert_eq!(offsets, vec![0, 4]);
}

#[test]
fn keeps_receive_times_across_redraws_split_utf8_and_delayed_drains() {
    let mut normalizer = TerminalTextNormalizer::default();
    normalizer.write(b"prompt> ", "2026-09-24T10:00:00.000Z");
    normalizer.write(b"status\r\nloading 1%", "2026-09-24T10:00:02.000Z");
    normalizer.write(b"\r\x1b[2Kready\r\n", "2026-09-24T10:00:03.000Z");
    let unicode = "✓".as_bytes();
    normalizer.write(&unicode[..1], "2026-09-24T10:00:04.000Z");
    normalizer.write(&unicode[1..], "2026-09-24T10:00:05.000Z");
    assert_eq!(normalizer.finish(), "prompt> status\nready\n✓");
    assert_eq!(
        normalizer.take_line_timestamps(),
        vec![
            stamp(0, "2026-09-24T10:00:02.000Z"),
            stamp(15, "2026-09-24T10:00:03.000Z"),
            stamp(21, "2026-09-24T10:00:05.000Z"),
        ]
    );
}

#[test]
fn does_not_create_a_session_log_until_logging_is_explicitly_enabled() {
    let history = MemoryHistory::new();
    let recorder = recorder(
        &history,
        policy(false, false),
        &ssh("production"),
        Some("Production"),
        Arc::new(SystemClock),
    );
    recorder.output(b"must not be retained\r\n");
    recorder.end(SessionEndStatus::Completed);
    assert_eq!(
        recorder.state(),
        SessionLoggingState {
            enabled: false,
            paused: false,
            capture_input: false,
            ..SessionLoggingState::default()
        }
    );
    assert!(history.sessions().is_empty());
}

#[test]
fn starts_and_stops_distinct_history_sessions_without_interrupting_the_terminal() {
    let history = MemoryHistory::new();
    let recorder = recorder(
        &history,
        policy(false, false),
        &ssh("production"),
        Some("Production"),
        Arc::new(SystemClock),
    );
    recorder.output(b"before logging\r\n");
    recorder.set_state(&enable());
    let first = recorder.state().session_id.unwrap();
    recorder.output(b"first recording\r\n");
    recorder.set_state(&disable());
    recorder.output(b"between recordings\r\n");
    recorder.set_state(&enable());
    let second = recorder.state().session_id.unwrap();
    recorder.output(b"second recording\r\n");
    recorder.end(SessionEndStatus::Completed);

    assert_ne!(first, second);
    let first_transcript = history.session(&first).unwrap().transcript();
    let second_transcript = history.session(&second).unwrap().transcript();
    assert!(first_transcript.contains("first recording\n"));
    assert!(first_transcript.contains("Session logging stopped."));
    assert!(!first_transcript.contains("before logging"));
    assert!(!first_transcript.contains("between recordings"));
    assert!(second_transcript.contains("second recording\n"));
    assert!(!second_transcript.contains("between recordings"));
    assert_eq!(
        history.session(&first).unwrap().status,
        SessionLogStatus::Completed
    );
}

#[test]
fn renames_the_active_record_when_the_tab_is_renamed() {
    let history = MemoryHistory::new();
    let recorder = recorder(
        &history,
        policy(false, false),
        &ssh("production"),
        Some("Production"),
        Arc::new(SystemClock),
    );
    recorder.set_state(&enable());
    let first = recorder.state().session_id.unwrap();
    recorder.set_title("Pre-change");
    assert_eq!(history.session(&first).unwrap().title, "Pre-change");

    // A stopped record keeps its title; the next one starts with the new one.
    recorder.set_state(&disable());
    recorder.set_title("Post-change");
    recorder.set_state(&enable());
    let second = recorder.state().session_id.unwrap();
    assert_eq!(history.session(&first).unwrap().title, "Pre-change");
    assert_eq!(history.session(&second).unwrap().title, "Post-change");

    // A blank title falls back to the host, and nothing changes after the end.
    recorder.set_title("   ");
    assert_eq!(history.session(&second).unwrap().title, "production");
    recorder.end(SessionEndStatus::Completed);
    recorder.set_title("After the session");
    assert_eq!(history.session(&second).unwrap().title, "production");
}

#[test]
fn suppresses_input_by_default_and_honors_pause_resume_at_runtime() {
    let history = MemoryHistory::new();
    let recorder = recorder(
        &history,
        policy(true, false),
        &ssh("production"),
        Some("Production"),
        Arc::new(SystemClock),
    );
    recorder.input(b"secret-token");
    recorder.output(b"\x1b[31mvisible error\x1b[0m\r\n");
    recorder.set_state(&SessionLoggingPatch {
        paused: Some(true),
        ..SessionLoggingPatch::default()
    });
    recorder.output(b"not retained");
    recorder.set_state(&SessionLoggingPatch {
        paused: Some(false),
        capture_input: Some(true),
        ..SessionLoggingPatch::default()
    });
    recorder.input(b"safe-command\n");
    recorder.end(SessionEndStatus::Completed);

    let session = history
        .session(recorder.state().session_id.as_deref().unwrap())
        .unwrap();
    let transcript = session.transcript();
    assert!(transcript.contains("visible error\n"));
    assert!(transcript.contains("Session logging paused."));
    assert!(transcript.contains("Session logging resumed."));
    assert!(transcript.contains("safe-command\n"));
    assert!(!transcript.contains("secret-token"));
    assert!(!transcript.contains("not retained"));
    assert_eq!(session.status, SessionLogStatus::Completed);
    assert!(session.capture_input);
    // Raw bytes keep the colours for exact export.
    let raw: Vec<u8> = session
        .events
        .iter()
        .flat_map(|event| event.raw.clone())
        .collect();
    assert!(String::from_utf8_lossy(&raw).contains("\x1b[31mvisible error"));
}

#[test]
fn suspends_logging_when_the_history_queue_is_full_and_says_so() {
    let history = MemoryHistory::new();
    let recorder = recorder(
        &history,
        policy(true, false),
        &ssh("production"),
        None,
        Arc::new(SystemClock),
    );
    let seen: Arc<Mutex<Vec<SessionLoggingState>>> = Arc::default();
    let sink = seen.clone();
    recorder.on_state_change(move |state| sink.lock().unwrap().push(state));
    history.refuse_appends(true);
    recorder.system("Connected.");
    let state = recorder.state();
    assert!(!state.enabled);
    assert_eq!(
        state.warning.as_deref(),
        Some("Session logging suspended: the history write queue is full.")
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(
        history
            .session(state.session_id.as_deref().unwrap())
            .unwrap()
            .status,
        SessionLogStatus::Failed
    );
}

#[test]
fn suspends_logging_when_the_store_reports_a_failure_later() {
    let history = MemoryHistory::new();
    let recorder = recorder(
        &history,
        policy(true, false),
        &ssh("production"),
        None,
        Arc::new(SystemClock),
    );
    let seen: Arc<Mutex<Vec<SessionLoggingState>>> = Arc::default();
    let sink = seen.clone();
    recorder.on_state_change(move |state| sink.lock().unwrap().push(state));
    let id = recorder.state().session_id.unwrap();
    history.fail(&id, "disk full");
    assert_eq!(recorder.state().warning.as_deref(), Some("disk full"));
    assert_eq!(
        seen.lock().unwrap().last().unwrap().warning.as_deref(),
        Some("disk full")
    );
    // A failure after the record finished is not news.
    recorder.set_state(&enable());
    let next = recorder.state().session_id.unwrap();
    history.fail(&id, "late");
    assert_eq!(recorder.state().session_id.as_deref(), Some(next.as_str()));
    assert!(recorder.state().enabled);
}

#[test]
fn assigns_stable_policy_keys_to_saved_and_ad_hoc_hosts() {
    assert_eq!(
        session_profile_identity(&ssh("edge")).profile_key,
        "ssh:edge"
    );
    let saved_ssh: SshProfile = serde_json::from_value(serde_json::json!({
        "profileId": "saved-ssh-1", "target": "edge.example.test", "useConfig": false
    }))
    .unwrap();
    assert_eq!(
        session_profile_identity(&TerminalProfile::Ssh(saved_ssh)).profile_key,
        "profile:saved-ssh-1"
    );
    let telnet = TelnetProfile {
        profile_id: Some("saved-1".into()),
        host: "router".into(),
        port: 23,
    };
    let identity = session_profile_identity(&TerminalProfile::Telnet(telnet));
    assert_eq!(
        (identity.profile_key.as_str(), identity.host.as_str()),
        ("profile:saved-1", "router:23")
    );
    let serial = SerialProfile::new("/dev/ttyUSB0");
    assert_eq!(
        session_profile_identity(&TerminalProfile::Serial(serial)).profile_key,
        "serial:/dev/ttyUSB0"
    );
    let local = session_profile_identity(&TerminalProfile::Local(LocalProfile::default()));
    assert_eq!(
        (local.profile_key.as_str(), local.host.as_str()),
        ("local", "Local shell")
    );
}

#[test]
fn strips_split_ansi_and_osc_payloads_while_preserving_readable_line_breaks() {
    let mut normalizer = TerminalTextNormalizer::default();
    let now = "2026-01-01T00:00:00.000Z";
    let chunks = [
        normalizer.write(b"one\r\n\x1b[31", now),
        normalizer.write(b"mred\x1b[0m \x1b]0;sec", now),
        normalizer.write(b"ret title\x07two\r\nthree", now),
        normalizer.finish(),
    ];
    assert_eq!(chunks.concat(), "one\nred two\nthree");
}

#[test]
fn keeps_only_the_visible_zsh_command_when_prompts_and_partial_line_markers_redraw() {
    let mut normalizer = TerminalTextNormalizer::default();
    let now = "2026-01-01T00:00:00.000Z";
    let chunks = [
        normalizer.write("~          14:10:57\r\n❯ lll".as_bytes(), now),
        normalizer.write("\r\x1b[K❯ ll\r\ntotal 271M\r\n".as_bytes(), now),
        normalizer.write(b"%                 \r", now),
        normalizer.write("~          14:10:59\r\n❯ ppwd".as_bytes(), now),
        normalizer.write("\r\x1b[K❯ pwd\r\n/home/flschwar\r\n".as_bytes(), now),
        normalizer.finish(),
    ];
    let transcript = chunks.concat();
    assert_eq!(
        transcript,
        "~          14:10:57\n❯ ll\ntotal 271M\n~          14:10:59\n❯ pwd\n/home/flschwar\n"
    );
    assert!(!transcript.contains("lll"));
    assert!(!transcript.contains("ppwd"));
    assert!(!transcript.contains('%'));
}

#[test]
fn reconciles_carriage_return_progress_updates_instead_of_duplicating_them() {
    let mut normalizer = TerminalTextNormalizer::default();
    normalizer.write(
        b"Downloading 10%\rDownloading 100%\r\n",
        "2026-01-01T00:00:00.000Z",
    );
    assert_eq!(normalizer.finish(), "Downloading 100%\n");
}

#[test]
fn applies_cursor_and_erase_operations() {
    let now = "2026-01-01T00:00:00.000Z";
    let mut normalizer = TerminalTextNormalizer::default();
    normalizer.write(b"abcdef\x1b[3D\x1b[1PX\x1b[2@\x1b[1G>\r\n", now);
    normalizer.write(b"tab\there\r\n", now);
    normalizer.write(b"\x1b[?25lhidden cursor\x1b[?25h\x1b7 saved\x1b8!\r\n", now);
    normalizer.write(b"gone\x1b[2K\r\n", now);
    assert_eq!(
        normalizer.finish(),
        ">bcX  f\ntab     here\nhidden cursor!saved\n"
    );
    // A clear commits the screen and starts a fresh one.
    let mut cleared = TerminalTextNormalizer::default();
    assert_eq!(
        cleared.write(b"old\r\nscreen\x1b[2J\x1b[Hnew", now),
        "old\nscreen"
    );
    assert_eq!(cleared.finish(), "new");
    // Absurd cursor moves are clamped instead of allocating without end.
    let mut huge = TerminalTextNormalizer::default();
    let committed = huge.write(b"x\x1b[999999999999B", now);
    assert_eq!(committed.matches('\n').count(), 65_535 - 4);
    assert_eq!(huge.finish(), "");
}

#[test]
fn counts_line_offsets_in_utf16_code_units() {
    let now = "2026-01-01T00:00:00.000Z";
    let mut normalizer = TerminalTextNormalizer::new(0);
    assert_eq!(normalizer.write("😀\r\nb\r\n".as_bytes(), now), "😀\nb\n");
    let offsets: Vec<u64> = normalizer
        .take_line_timestamps()
        .iter()
        .map(|stamp| stamp.offset)
        .collect();
    assert_eq!(offsets, vec![0, 3]);
}

#[tokio::test]
async fn flushes_coalesced_output_after_a_quarter_second_inside_a_runtime() {
    let history = MemoryHistory::new();
    let recorder = recorder(
        &history,
        policy(true, false),
        &ssh("production"),
        None,
        Arc::new(SystemClock),
    );
    let id = recorder.state().session_id.unwrap();
    for line in 0..10 {
        recorder.output(format!("line {line}\r\n").as_bytes());
    }
    let output_events = || {
        history
            .session(&id)
            .unwrap()
            .events
            .iter()
            .filter(|event| event.direction == SessionLogDirection::Output)
            .count()
    };
    assert_eq!(output_events(), 0);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    // One coalesced batch, written without any further call.
    assert_eq!(output_events(), 1);
    recorder.end(SessionEndStatus::Completed);
}
