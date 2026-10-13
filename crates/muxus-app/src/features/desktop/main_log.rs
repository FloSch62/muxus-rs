//! Desktop shell diagnostics (`electron/src/main-log.ts`). Every entry goes two places: the
//! in-process log buffer behind the log viewer (visible once a window is up) and a plain-text
//! file, `<data folder>/logs/main.log`: the only trace left when the app never gets that far.
//! The file is rotated to `main.old.log` past 1 MiB when the app starts.

use std::fmt::Display;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use muxus_core::api::AppLogLevel;
use serde_json::{Map, Value};

pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

static LOG_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The levels the shell logs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainLogLevel {
    Info,
    Warn,
    Error,
}

impl MainLogLevel {
    fn label(self) -> &'static str {
        match self {
            MainLogLevel::Info => "INFO",
            MainLogLevel::Warn => "WARN",
            MainLogLevel::Error => "ERROR",
        }
    }

    fn app_level(self) -> AppLogLevel {
        match self {
            MainLogLevel::Info => AppLogLevel::Info,
            MainLogLevel::Warn => AppLogLevel::Warn,
            MainLogLevel::Error => AppLogLevel::Error,
        }
    }
}

/// Start the log file in `<user_data>/logs`, rotating a large one. Never fails: without a
/// file the in-memory mirror still works.
pub fn init_main_log(user_data: &Path) -> Option<PathBuf> {
    let dir = user_data.join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join("main.log");
    if std::fs::metadata(&file).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&file, dir.join("main.old.log"));
    }
    *LOG_FILE.lock().unwrap_or_else(|p| p.into_inner()) = Some(file.clone());
    Some(file)
}

/// Where the log file is, once [`init_main_log`] ran.
pub fn main_log_path() -> Option<PathBuf> {
    LOG_FILE.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// One file line: ISO time, level, message, and the error indented below it.
pub fn format_line(time: &str, level: MainLogLevel, msg: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) => format!(
            "{time} {} {msg}\n  {}\n",
            level.label(),
            detail.replace('\n', "\n  ")
        ),
        None => format!("{time} {} {msg}\n", level.label()),
    }
}

fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// Log a shell milestone or failure.
pub fn main_log(level: MainLogLevel, msg: &str, err: Option<&dyn Display>) {
    let detail = err.map(|err| err.to_string());
    let context = detail.as_ref().map(|detail| {
        let mut context = Map::new();
        context.insert("err".into(), Value::String(detail.clone()));
        context
    });
    muxus_session::logging::app_log::append_app_log(level.app_level(), msg, context);
    let Some(file) = main_log_path() else { return };
    let line = format_line(&now_iso(), level, msg, detail.as_deref());
    // Never let logging take the app down.
    if let Ok(mut out) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
    {
        let _ = out.write_all(line.as_bytes());
    }
}

/// Last-resort capture: a panic is written to the log before the default report runs.
pub fn install_crash_capture() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic without a message".into());
        let location = info
            .location()
            .map(|location| format!(" at {}:{}", location.file(), location.line()))
            .unwrap_or_default();
        let thread = std::thread::current();
        let detail = format!(
            "{payload}{location}\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        main_log(
            MainLogLevel::Error,
            &format!("panic in thread {}", thread.name().unwrap_or("<unnamed>")),
            Some(&detail),
        );
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_lines_with_indented_details() {
        assert_eq!(
            format_line(
                "2026-10-12T10:00:00.000Z",
                MainLogLevel::Info,
                "starting",
                None
            ),
            "2026-10-12T10:00:00.000Z INFO starting\n"
        );
        assert_eq!(
            format_line(
                "t",
                MainLogLevel::Error,
                "the backend failed to start",
                Some("Error: locked\nat open")
            ),
            "t ERROR the backend failed to start\n  Error: locked\n  at open\n"
        );
    }

    #[test]
    fn writes_the_file_and_rotates_a_large_one() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("main.log"),
            vec![b'x'; MAX_LOG_BYTES as usize + 1],
        )
        .unwrap();
        let file = init_main_log(dir.path()).unwrap();
        assert_eq!(file, logs.join("main.log"));
        assert!(logs.join("main.old.log").exists());
        main_log(
            MainLogLevel::Warn,
            "could not import the login shell environment",
            Some(&"timeout"),
        );
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.ends_with(" WARN could not import the login shell environment\n  timeout\n"),
            "{text}"
        );
        let entries = muxus_session::logging::app_log::app_log_entries();
        assert!(entries.iter().any(|entry| entry.msg
            == "could not import the login shell environment"
            && entry.context.as_ref().and_then(|c| c.get("err"))
                == Some(&Value::String("timeout".into()))));
    }
}
