//! Live terminal sessions the backend can type into (`server/src/ws/terminal-inputs.ts`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Writes bytes to a session's transport; `false` when it cannot take input yet or any more.
pub type TerminalInputWriter = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;

#[derive(Default)]
struct Inner {
    writers: Mutex<HashMap<String, (u64, TerminalInputWriter)>>,
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
            .is_some_and(|(token, _)| *token == self.token)
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
    pub fn register(&self, terminal_id: &str, writer: TerminalInputWriter) -> InputRegistration {
        let token = self.inner.next.fetch_add(1, Ordering::SeqCst);
        lock(&self.inner.writers).insert(terminal_id.to_string(), (token, writer));
        InputRegistration {
            inner: self.inner.clone(),
            terminal_id: terminal_id.to_string(),
            token,
        }
    }

    pub fn writer(&self, terminal_id: &str) -> Option<TerminalInputWriter> {
        lock(&self.inner.writers)
            .get(terminal_id)
            .map(|(_, writer)| writer.clone())
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
}
