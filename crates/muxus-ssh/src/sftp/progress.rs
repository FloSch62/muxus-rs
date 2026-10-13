//! Byte progress for transfers (port of `progressReporter` in `client/src/api/transfers.ts`) and
//! the cancel signal transfers check (the `AbortSignal` of the TypeScript client).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

/// Bytes moved so far in one transfer (`ByteProgress`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByteProgress {
    pub loaded: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub bytes_per_second: f64,
}

/// What an upload reports while it runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UploadStep {
    /// Bytes the server acknowledged, smoothed and throttled.
    Progress(ByteProgress),
    /// Every byte arrived; the file is being moved into place (`onUploadComplete`).
    Finalizing,
}

const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
const EMIT_INTERVAL: Duration = Duration::from_millis(100);

/// Smooths and throttles noisy progress. Ten updates per second look continuous without
/// re-rendering the file table for every chunk.
#[derive(Debug, Clone)]
pub struct ProgressMeter {
    sampled_at: Instant,
    sampled_bytes: u64,
    emitted_at: Option<Instant>,
    smoothed_speed: f64,
}

impl Default for ProgressMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressMeter {
    pub fn new() -> Self {
        Self::started_at(Instant::now())
    }

    pub fn started_at(now: Instant) -> Self {
        Self {
            sampled_at: now,
            sampled_bytes: 0,
            emitted_at: None,
            smoothed_speed: 0.0,
        }
    }

    /// The progress to show now, or `None` while throttled. `force` always emits (start, end).
    pub fn report(&mut self, loaded: u64, total: Option<u64>, force: bool) -> Option<ByteProgress> {
        self.report_at(Instant::now(), loaded, total, force)
    }

    pub fn report_at(
        &mut self,
        now: Instant,
        loaded: u64,
        total: Option<u64>,
        force: bool,
    ) -> Option<ByteProgress> {
        let sample = now.saturating_duration_since(self.sampled_at);
        if sample >= SAMPLE_INTERVAL || force {
            let instant_speed = if sample > Duration::ZERO {
                (loaded as f64 - self.sampled_bytes as f64) / sample.as_secs_f64()
            } else {
                self.smoothed_speed
            };
            self.smoothed_speed = if self.smoothed_speed > 0.0 {
                self.smoothed_speed * 0.72 + instant_speed.max(0.0) * 0.28
            } else {
                instant_speed.max(0.0)
            };
            self.sampled_at = now;
            self.sampled_bytes = loaded;
        }
        let due = self
            .emitted_at
            .is_none_or(|at| now.saturating_duration_since(at) >= EMIT_INTERVAL);
        if force || due || loaded == 0 {
            self.emitted_at = Some(now);
            return Some(ByteProgress {
                loaded,
                total,
                bytes_per_second: self.smoothed_speed,
            });
        }
        None
    }
}

/// A cancel request shared between the caller and a running transfer.
#[derive(Debug, Clone)]
pub struct CancelSignal {
    sender: Arc<watch::Sender<bool>>,
}

impl Default for CancelSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelSignal {
    pub fn new() -> Self {
        Self {
            sender: Arc::new(watch::channel(false).0),
        }
    }

    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }

    /// Completes once [`cancel`](Self::cancel) was called.
    pub async fn cancelled(&self) {
        let mut receiver = self.sender.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttles_to_ten_updates_a_second_and_smooths_the_speed() {
        let start = Instant::now();
        let mut meter = ProgressMeter::started_at(start);
        let first = meter.report_at(start, 0, Some(1000), true).unwrap();
        assert_eq!(first.loaded, 0);
        assert_eq!(first.bytes_per_second, 0.0);

        // Within 100 ms of the last update nothing is emitted unless forced.
        assert!(
            meter
                .report_at(start + Duration::from_millis(50), 10, Some(1000), false)
                .is_none()
        );
        let sampled = meter
            .report_at(start + Duration::from_millis(250), 250, Some(1000), false)
            .unwrap();
        assert_eq!(sampled.bytes_per_second, 1000.0);

        // The next sample blends 28 % of the new speed into the old one.
        let blended = meter
            .report_at(start + Duration::from_millis(500), 1000, Some(1000), true)
            .unwrap();
        assert!((blended.bytes_per_second - (1000.0 * 0.72 + 3000.0 * 0.28)).abs() < 1e-6);
    }

    #[tokio::test]
    async fn a_cancel_signal_wakes_every_waiter() {
        let signal = CancelSignal::new();
        let waiter = tokio::spawn({
            let signal = signal.clone();
            async move { signal.cancelled().await }
        });
        assert!(!signal.is_cancelled());
        signal.cancel();
        waiter.await.unwrap();
        assert!(signal.is_cancelled());
        signal.cancelled().await;
    }
}
