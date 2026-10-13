//! A backend in scratch folders: its own data folder and home, an in-memory vault key store, no
//! SSH agent. Nothing a test does reaches the real user environment.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use muxus_session::{Backend, BackendOptions, RuntimeChoice, SessionEvent, SessionHandle};
use serde::de::DeserializeOwned;
use tempfile::TempDir;

pub struct Sandbox {
    pub dir: TempDir,
    pub backend: Backend,
}

impl Sandbox {
    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    /// Write `~/.ssh/config`.
    pub fn write_ssh_config(&self, lines: &[&str]) {
        let ssh = self.home().join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::write(ssh.join("config"), format!("{}\n", lines.join("\n"))).unwrap();
    }

    pub fn ssh_config(&self) -> String {
        std::fs::read_to_string(self.home().join(".ssh").join("config")).unwrap()
    }

    pub async fn stop(self) {
        self.backend.shutdown().await;
    }
}

pub fn options(dir: &Path) -> BackendOptions {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut options = BackendOptions::sandbox(dir.join("data"), home);
    options.runtime = RuntimeChoice::Own { worker_threads: 2 };
    options
}

pub async fn start() -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    start_in(dir, |_| {}).await
}

pub async fn start_with(configure: impl FnOnce(&mut BackendOptions)) -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    start_in(dir, configure).await
}

pub async fn start_in(dir: TempDir, configure: impl FnOnce(&mut BackendOptions)) -> Sandbox {
    let mut options = options(dir.path());
    configure(&mut options);
    let backend = Backend::start(options).await.expect("the backend starts");
    Sandbox { dir, backend }
}

/// A request body as the UI would build it from JSON.
pub fn body<T: DeserializeOwned>(value: serde_json::Value) -> T {
    serde_json::from_value(value).expect("a well-formed body")
}

/// The next event within a few seconds.
pub async fn next_event(handle: &SessionHandle) -> SessionEvent {
    tokio::time::timeout(Duration::from_secs(10), handle.next_event())
        .await
        .expect("an event in time")
        .expect("the session is still open")
}

/// Events until `stop` matches one (included), with terminal output collected as text.
pub async fn events_until(
    handle: &SessionHandle,
    mut stop: impl FnMut(&SessionEvent) -> bool,
) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    loop {
        let event = next_event(handle).await;
        let done = stop(&event);
        events.push(event);
        if done {
            return events;
        }
    }
}

/// Output text until it contains `needle`.
pub async fn output_until(handle: &SessionHandle, needle: &str) -> String {
    let mut text = String::new();
    loop {
        match next_event(handle).await {
            SessionEvent::Data(data) => {
                text.push_str(&String::from_utf8_lossy(&data));
                if text.contains(needle) {
                    return text;
                }
            }
            SessionEvent::Closed { reason } => {
                panic!("closed ({reason:?}) before {needle:?}; got {text:?}")
            }
            SessionEvent::Message(_) => {}
        }
    }
}
