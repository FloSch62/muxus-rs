//! Live terminal sessions the backend can type into (`server/src/ws/terminal-inputs.ts`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use muxus_core::api::TerminalEnterKey;

/// Writes bytes to a session's transport; `false` when it cannot take input yet or any more.
pub type TerminalInputWriter = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;
/// What Enter sends on a session's host, read each time text is typed.
pub type TerminalEnterKeyReader = Arc<dyn Fn() -> TerminalEnterKey + Send + Sync>;

/// A session to type into, with what Enter sends on its host.
#[derive(Clone)]
pub struct TerminalInputTarget {
    pub write: TerminalInputWriter,
    pub enter_key: TerminalEnterKey,
}

struct Input {
    token: u64,
    write: TerminalInputWriter,
    enter_key: TerminalEnterKeyReader,
}

#[derive(Default)]
struct Inner {
    writers: Mutex<HashMap<String, Input>>,
    next: AtomicU64,
}

/// Live terminal sessions the backend can type into, by terminal id. A writer sends bytes
/// straight to the session's transport, the way keystrokes travel, but never through session
/// logging: nothing written here reaches session history or a log file unless the remote side
/// echoes it.
#[derive(Default, Clone)]
pub struct TerminalInputs {
    inner: Arc<Inner>,
}

/// Removes a writer when dropped, unless another one replaced it meanwhile.
pub struct InputRegistration {
    inner: Arc<Inner>,
    terminal_id: String,
    token: u64,
}

impl Drop for InputRegistration {
    fn drop(&mut self) {
        let mut writers = lock(&self.inner.writers);
        if writers
            .get(&self.terminal_id)
            .is_some_and(|input| input.token == self.token)
        {
            writers.remove(&self.terminal_id);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl TerminalInputs {
    /// A session whose host presses Enter as CR.
    pub fn register(&self, terminal_id: &str, writer: TerminalInputWriter) -> InputRegistration {
        self.register_with_enter_key(terminal_id, writer, Arc::new(|| TerminalEnterKey::Cr))
    }

    /// `enter_key` is read each time text is typed, so a change to the host reaches open
    /// sessions.
    pub fn register_with_enter_key(
        &self,
        terminal_id: &str,
        writer: TerminalInputWriter,
        enter_key: TerminalEnterKeyReader,
    ) -> InputRegistration {
        let token = self.inner.next.fetch_add(1, Ordering::SeqCst);
        lock(&self.inner.writers).insert(
            terminal_id.to_string(),
            Input {
                token,
                write: writer,
                enter_key,
            },
        );
        InputRegistration {
            inner: self.inner.clone(),
            terminal_id: terminal_id.to_string(),
            token,
        }
    }

    pub fn writer(&self, terminal_id: &str) -> Option<TerminalInputWriter> {
        lock(&self.inner.writers)
            .get(terminal_id)
            .map(|input| input.write.clone())
    }

    /// The session's writer and its host's Enter key as it is now.
    pub fn target(&self, terminal_id: &str) -> Option<TerminalInputTarget> {
        let (write, enter_key) = {
            let writers = lock(&self.inner.writers);
            let input = writers.get(terminal_id)?;
            (input.write.clone(), input.enter_key.clone())
        };
        Some(TerminalInputTarget {
            write,
            enter_key: enter_key(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registration_removes_only_its_own_writer() {
        let inputs = TerminalInputs::default();
        let first = inputs.register("t", Arc::new(|_: &[u8]| true));
        let second = inputs.register("t", Arc::new(|_: &[u8]| false));
        drop(first);
        let writer = inputs.writer("t").expect("the newer writer stays");
        assert!(!writer(b"x"));
        drop(second);
        assert!(inputs.writer("t").is_none());
    }

    #[test]
    fn a_target_carries_the_hosts_enter_key_as_it_is_when_typed() {
        let inputs = TerminalInputs::default();
        let key = Arc::new(Mutex::new(TerminalEnterKey::Lf));
        let current = key.clone();
        let _registration = inputs.register_with_enter_key(
            "t",
            Arc::new(|_: &[u8]| true),
            Arc::new(move || *current.lock().unwrap()),
        );
        assert_eq!(inputs.target("t").unwrap().enter_key, TerminalEnterKey::Lf);
        *key.lock().unwrap() = TerminalEnterKey::Crlf;
        assert_eq!(
            inputs.target("t").unwrap().enter_key,
            TerminalEnterKey::Crlf
        );
        assert!(inputs.target("gone").is_none());
        let _plain = inputs.register("cr", Arc::new(|_: &[u8]| true));
        assert_eq!(inputs.target("cr").unwrap().enter_key, TerminalEnterKey::Cr);
    }
}
