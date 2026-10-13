//! Port of `tests/unit/server/session-log-file.test.ts`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use muxus_core::api::{SessionEndStatus, SessionLogFileSettings, SessionLoggingPolicy};
use muxus_core::protocol::TerminalProfile;
use muxus_session::logging::{
    ManualClock, MemoryHistory, RecorderConfig, SessionLogFile, SessionLoggingPatch,
    SessionRecorder, SystemClock, session_profile_identity, unused_session_log_file_path,
};

/// Writes are asynchronous; wait until the file holds everything expected.
fn file_text(file: &Path, until: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        if text.contains(until) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "{} never contained {until:?}: {text:?}",
            file.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn writes_the_normalized_transcript_and_the_rows_still_on_screen_at_close() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("nested").join("router.log");
    let mut log = SessionLogFile::open(&file, false, false, Arc::new(SystemClock)).unwrap();
    log.system("Logging to file started.");
    log.output(b"\x1b[32mone\x1b[0m\r\ntwo\r\nloading 1%\r\x1b[2Kready\r\n$ ");
    log.close("Logging to file stopped.");
    assert_eq!(
        file_text(&file, "stopped"),
        "Logging to file started.\none\ntwo\nready\n$\nLogging to file stopped.\n"
    );
}

#[test]
fn prefixes_lines_with_the_time_they_last_changed() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("timed.log");
    let clock = ManualClock::at("2026-10-04T10:00:00.000Z");
    let mut log = SessionLogFile::open(&file, false, true, Arc::new(clock.clone())).unwrap();
    log.output(b"first\r\nsec");
    clock.set_iso("2026-10-04T10:00:05.000Z");
    log.output(b"ond\r\n");
    log.close("done");
    assert_eq!(
        file_text(&file, "done"),
        "[2026-10-04T10:00:00.000Z] first\n[2026-10-04T10:00:05.000Z] second\n[2026-10-04T10:00:05.000Z] done\n"
    );
}

#[test]
fn appends_to_a_chosen_file_and_numbers_generated_names_that_exist() {
    let directory = tempfile::tempdir().unwrap();
    let chosen = directory.path().join("chosen.log");
    std::fs::write(&chosen, "earlier session").unwrap();
    let mut appended = SessionLogFile::open(&chosen, true, false, Arc::new(SystemClock)).unwrap();
    appended.close("later session");
    assert_eq!(
        file_text(&chosen, "later"),
        "earlier session\nlater session\n"
    );

    assert_eq!(
        unused_session_log_file_path(&chosen),
        directory.path().join("chosen-1.log")
    );
    let mut generated = SessionLogFile::open(&chosen, false, false, Arc::new(SystemClock)).unwrap();
    assert_eq!(generated.path(), directory.path().join("chosen-1.log"));
    generated.close("new file");
    file_text(generated.path(), "new file");
    assert_eq!(
        std::fs::read_to_string(&chosen).unwrap(),
        "earlier session\nlater session\n"
    );
}

#[test]
fn refuses_relative_paths() {
    let err = SessionLogFile::open(
        Path::new("relative.log"),
        true,
        false,
        Arc::new(SystemClock),
    )
    .unwrap_err();
    assert!(err.to_string().contains("absolute"));
}

#[cfg(unix)]
#[test]
fn keeps_log_files_private() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("private.log");
    let mut log = SessionLogFile::open(&file, false, false, Arc::new(SystemClock)).unwrap();
    log.close("x");
    file_text(&file, "x");
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

struct Fixture {
    directory: tempfile::TempDir,
    history: MemoryHistory,
}

impl Fixture {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
            history: MemoryHistory::new(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn recorder(&self, enabled: bool, log_to_file: bool, title: Option<&str>) -> SessionRecorder {
        let directory = self.directory.path().to_string_lossy().into_owned();
        let profile = TerminalProfile::Ssh(
            serde_json::from_value(serde_json::json!({ "target": "router1" })).unwrap(),
        );
        SessionRecorder::start(RecorderConfig {
            history: Arc::new(self.history.clone()),
            policy: SessionLoggingPolicy {
                profile_key: "*".into(),
                enabled,
                capture_input: false,
                max_part_bytes: 1024 * 1024,
                max_parts: 2,
                log_to_file,
                overridden: false,
            },
            file_settings: Arc::new(move || SessionLogFileSettings {
                directory: Some(directory.clone()),
                filename_pattern: "{host}.log".into(),
                timestamps: false,
            }),
            home: PathBuf::from("/nonexistent/home"),
            identity: session_profile_identity(&profile),
            title: title.map(str::to_string),
            clock: Arc::new(SystemClock),
        })
    }
}

fn patch() -> SessionLoggingPatch {
    SessionLoggingPatch::default()
}

#[test]
fn starts_a_log_file_with_the_session_when_the_policy_asks_for_it() {
    let fixture = Fixture::new();
    let recorder = fixture.recorder(false, true, Some("Core router"));
    let file = fixture.path("router1.log");
    let state = recorder.state();
    assert!(!state.enabled);
    assert_eq!(state.file_path.as_deref(), Some(file.to_str().unwrap()));

    recorder.system("Connecting to router1 …");
    recorder.output(b"show version\r\nSR OS 25.7\r\n");
    recorder.end(SessionEndStatus::Disconnected);

    let text = file_text(&file, "ended");
    let first_line = text.lines().next().unwrap();
    assert!(
        first_line.starts_with("Logging to file started: Core router (router1) at "),
        "{first_line}"
    );
    assert!(first_line.ends_with('.') && !first_line[50..].contains(' '));
    assert!(text.contains("Connecting to router1 …\nshow version\nSR OS 25.7\n"));
    let last_line = text.lines().last().unwrap();
    assert!(
        last_line.starts_with("Logging to file ended (disconnected) at ") && text.ends_with(".\n"),
        "{text}"
    );
    assert_eq!(recorder.state().file_path, None);
}

#[test]
fn pauses_and_records_input_in_the_log_file_without_session_history() {
    let fixture = Fixture::new();
    let recorder = fixture.recorder(false, false, None);
    let file = fixture.path("manual.log");
    let state = recorder.set_state(&SessionLoggingPatch {
        log_to_file: Some(true),
        log_file_path: Some(file.to_string_lossy().into_owned()),
        ..patch()
    });
    assert!(!state.enabled);
    assert_eq!(state.file_path.as_deref(), Some(file.to_str().unwrap()));

    recorder.output(b"visible\r\n");
    recorder.set_state(&SessionLoggingPatch {
        paused: Some(true),
        ..patch()
    });
    recorder.output(b"secret\r\n");
    recorder.set_state(&SessionLoggingPatch {
        paused: Some(false),
        capture_input: Some(true),
        ..patch()
    });
    recorder.input(b"ls\r");
    let state = recorder.set_state(&SessionLoggingPatch {
        log_to_file: Some(false),
        ..patch()
    });
    assert!(!state.paused);
    assert_eq!(state.file_path, None);
    recorder.output(b"after stop\r\n");

    let text = file_text(&file, "stopped");
    assert!(
        text.contains("visible\nSession logging paused.\nSession logging resumed.\n"),
        "{text}"
    );
    assert!(text.contains("Input recording enabled.\nls"), "{text}");
    assert!(!text.contains("secret"));
    assert!(!text.contains("after stop"));
    assert_eq!(recorder.state().session_id, None);
}

#[test]
fn keeps_history_and_the_log_file_apart() {
    let fixture = Fixture::new();
    let recorder = fixture.recorder(true, false, None);
    let file = fixture.path("both.log");
    recorder.set_state(&SessionLoggingPatch {
        log_to_file: Some(true),
        log_file_path: Some(file.to_string_lossy().into_owned()),
        ..patch()
    });
    recorder.output(b"shared output\r\n");
    recorder.set_state(&SessionLoggingPatch {
        paused: Some(true),
        ..patch()
    });
    // Starting history again keeps a paused log file paused.
    let state = recorder.set_state(&SessionLoggingPatch {
        enabled: Some(false),
        ..patch()
    });
    assert!(!state.enabled && state.paused);
    let state = recorder.set_state(&SessionLoggingPatch {
        enabled: Some(true),
        ..patch()
    });
    assert!(state.enabled && state.paused);
    recorder.output(b"hidden\r\n");
    recorder.end(SessionEndStatus::Completed);

    let text = file_text(&file, "ended");
    assert!(text.contains("shared output\n"));
    assert!(!text.contains("Session logging started."));
    assert!(!text.contains("hidden"));
    let transcript = fixture
        .history
        .session(recorder.state().session_id.as_deref().unwrap())
        .unwrap()
        .transcript();
    assert!(transcript.contains("Session logging paused."));
    assert!(!transcript.contains("Logging to file"));
    assert!(!transcript.contains("hidden"));
}

#[test]
fn warns_instead_of_failing_the_session_when_the_file_cannot_be_written() {
    let fixture = Fixture::new();
    let recorder = fixture.recorder(false, false, None);
    let blocked = fixture.path("blocked");
    std::fs::write(&blocked, "").unwrap();
    let state = recorder.set_state(&SessionLoggingPatch {
        log_to_file: Some(true),
        log_file_path: Some(blocked.join("session.log").to_string_lossy().into_owned()),
        ..patch()
    });
    assert_eq!(state.file_path, None);
    assert!(
        state
            .warning
            .as_deref()
            .unwrap()
            .starts_with("Could not write the log file"),
        "{state:?}"
    );
    recorder.output(b"still running\r\n");
    recorder.end(SessionEndStatus::Completed);
}

#[test]
fn numbers_generated_names_instead_of_reusing_a_file() {
    let fixture = Fixture::new();
    let first = fixture.recorder(false, true, None);
    let second = fixture.recorder(false, true, None);
    assert_eq!(
        first.state().file_path.as_deref(),
        Some(fixture.path("router1.log").to_str().unwrap())
    );
    assert_eq!(
        second.state().file_path.as_deref(),
        Some(fixture.path("router1-1.log").to_str().unwrap())
    );
    first.end(SessionEndStatus::Completed);
    second.end(SessionEndStatus::Completed);
    file_text(&fixture.path("router1-1.log"), "ended");
    let mut names: Vec<String> = std::fs::read_dir(fixture.directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["router1-1.log", "router1.log"]);
}
