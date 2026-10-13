//! A readiness deadline that stops counting while the user is busy with a prompt. The SSH
//! library's own timeout includes time spent in UI prompts and cannot be paused, so the dial
//! races this instead.

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

use tokio::sync::Notify;

struct State {
    remaining: Duration,
    started: Instant,
    pause_depth: u32,
    active: bool,
}

pub struct PausableDeadline {
    state: Mutex<State>,
    changed: Notify,
}

impl PausableDeadline {
    /// A zero duration never expires.
    pub fn new(duration: Duration) -> Self {
        Self {
            state: Mutex::new(State {
                remaining: duration,
                started: Instant::now(),
                pause_depth: 0,
                active: !duration.is_zero(),
            }),
            changed: Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn pause(&self) {
        let mut s = self.lock();
        if !s.active {
            return;
        }
        s.pause_depth += 1;
        if s.pause_depth == 1 {
            let elapsed = s.started.elapsed();
            s.remaining = s.remaining.saturating_sub(elapsed);
        }
        drop(s);
        self.changed.notify_waiters();
    }

    pub fn resume(&self) {
        let mut s = self.lock();
        if !s.active || s.pause_depth == 0 {
            return;
        }
        s.pause_depth -= 1;
        if s.pause_depth == 0 {
            s.started = Instant::now();
        }
        drop(s);
        self.changed.notify_waiters();
    }

    pub fn clear(&self) {
        let mut s = self.lock();
        s.active = false;
        s.pause_depth = 0;
        drop(s);
        self.changed.notify_waiters();
    }

    /// Completes when the deadline runs out; never while paused or after `clear`.
    pub async fn expired(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let wait = {
                let s = self.lock();
                if !s.active || s.pause_depth > 0 {
                    None
                } else {
                    Some(s.remaining.saturating_sub(s.started.elapsed()))
                }
            };
            match wait {
                None => notified.await,
                Some(left) if left.is_zero() => {
                    let mut s = self.lock();
                    if s.active && s.pause_depth == 0 {
                        s.active = false;
                        return;
                    }
                }
                Some(left) => {
                    tokio::select! {
                        _ = &mut notified => {}
                        _ = tokio::time::sleep(left) => {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn expires_after_the_running_time_excluding_pauses() {
        let deadline = std::sync::Arc::new(PausableDeadline::new(Duration::from_secs(10)));
        let started = tokio::time::Instant::now();
        let d = deadline.clone();
        let waiter = tokio::spawn(async move {
            d.expired().await;
            tokio::time::Instant::now()
        });
        tokio::time::sleep(Duration::from_secs(4)).await;
        deadline.pause();
        deadline.pause();
        tokio::time::sleep(Duration::from_secs(60)).await;
        deadline.resume();
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!waiter.is_finished());
        deadline.resume();
        let fired = waiter.await.unwrap();
        let ran = fired - started;
        // 4s running + 120s paused + the remaining 6s.
        assert!(
            ran >= Duration::from_secs(130) && ran <= Duration::from_secs(131),
            "{ran:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn never_expires_once_cleared() {
        let deadline = PausableDeadline::new(Duration::from_secs(1));
        deadline.clear();
        let result = tokio::time::timeout(Duration::from_secs(30), deadline.expired()).await;
        assert!(result.is_err());
    }
}
