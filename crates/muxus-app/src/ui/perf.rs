//! Performance log for `hack/perf.sh` (`MUXUS_PERF_LOG=<file>`).
//!
//! Once a second the log gets one line: frames painted, the largest and the
//! 95th-percentile gap between frames, terminal output bytes handled, the
//! longest single output batch on the UI thread, the longest the UI thread was
//! busy in one go (a 4 ms heartbeat's lateness, measured for two seconds while
//! tabs open), and the resident memory.
//! Scenes add `mark` lines around what they measure. Nothing is recorded
//! without the variable.

use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Window {
    frames: Vec<Instant>,
    bytes: u64,
    batches: u32,
    max_batch: Duration,
    /// The longest the UI thread kept a 4 ms heartbeat waiting: how long it was busy
    /// in one go. Frame gaps also count idle time, when nothing needed painting.
    max_block: Duration,
}

pub struct PerfLog {
    /// The heartbeat runs only for the two seconds after the `one-tab` mark, while the
    /// tabs open, so it never adds to the idle, flood or idle-tabs CPU figures.
    beat_until: Mutex<Option<Instant>>,
    file: Mutex<File>,
    start: Instant,
    window: Mutex<Window>,
    last_frame: Mutex<Option<Instant>>,
}

static PERF: OnceLock<Option<PerfLog>> = OnceLock::new();

/// The log, when `MUXUS_PERF_LOG` names a file.
pub fn perf() -> Option<&'static PerfLog> {
    PERF.get_or_init(|| {
        let path = std::env::var_os("MUXUS_PERF_LOG")?;
        let file = File::create(path).ok()?;
        Some(PerfLog {
            beat_until: Mutex::new(None),
            file: Mutex::new(file),
            start: Instant::now(),
            window: Mutex::default(),
            last_frame: Mutex::new(None),
        })
    })
    .as_ref()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Resident memory of this process, in KiB (Linux).
pub fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

impl PerfLog {
    fn write_line(&self, line: &str) {
        let elapsed = self.start.elapsed().as_secs_f64();
        let mut file = lock(&self.file);
        let _ = writeln!(file, "{elapsed:.3} {line}");
        let _ = file.flush();
    }

    /// A frame was painted.
    pub fn frame(&self) {
        let now = Instant::now();
        let mut last = lock(&self.last_frame);
        if *last == Some(now) {
            return;
        }
        *last = Some(now);
        lock(&self.window).frames.push(now);
    }

    /// One batch of terminal output went through the UI thread.
    pub fn batch(&self, bytes: usize, took: Duration) {
        let mut window = lock(&self.window);
        window.bytes += bytes as u64;
        window.batches += 1;
        window.max_batch = window.max_batch.max(took);
    }

    /// The UI heartbeat woke `late` after it was due.
    pub fn heartbeat(&self, late: Duration) {
        let mut window = lock(&self.window);
        window.max_block = window.max_block.max(late);
    }

    /// Whether the heartbeat should run now.
    pub fn beating(&self) -> bool {
        lock(&self.beat_until).is_some_and(|until| Instant::now() < until)
    }

    pub fn mark(&self, label: &str) {
        if label == "one-tab" {
            *lock(&self.beat_until) = Some(Instant::now() + Duration::from_secs(2));
        }
        self.write_line(&format!("mark {label} rss_kib={}", rss_kib().unwrap_or(0)));
    }

    pub fn note(&self, text: &str) {
        for line in text.lines() {
            self.write_line(&format!("note {line}"));
        }
    }

    /// Close the one-second window and log it.
    pub fn tick(&self) {
        let window = std::mem::take(&mut *lock(&self.window));
        let mut gaps: Vec<f64> = window
            .frames
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).as_secs_f64() * 1000.0)
            .collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        let max_gap = gaps.last().copied().unwrap_or(0.0);
        let p95 = if gaps.is_empty() {
            0.0
        } else {
            gaps[((gaps.len() as f64 * 0.95).ceil() as usize).min(gaps.len()) - 1]
        };
        self.write_line(&format!(
            "tick frames={} max_gap_ms={max_gap:.1} p95_gap_ms={p95:.1} bytes={} batches={} max_batch_ms={:.1} max_block_ms={:.1} rss_kib={}",
            window.frames.len(),
            window.bytes,
            window.batches,
            window.max_batch.as_secs_f64() * 1000.0,
            window.max_block.as_secs_f64() * 1000.0,
            rss_kib().unwrap_or(0),
        ));
    }
}
