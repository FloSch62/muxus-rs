//! Plain-text session log files (`server/src/session-logging/session-log-file.ts`).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use chrono::NaiveDateTime;
use muxus_core::api::{
    SessionLineTimestamp, SessionLogFileNameValues, SessionLogFileSettings, session_log_file_name,
};

use super::clock::Clock;
use super::normalizer::TerminalTextNormalizer;

const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
const FLUSH_CHARS: usize = 64 * 1024;
const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;
const MAX_NAME_ATTEMPTS: u32 = 999;

/// Where log files go when no folder is configured: `~/Documents/Muxus/Logs`.
pub fn default_session_log_directory(home: &Path) -> PathBuf {
    home.join("Documents").join("Muxus").join("Logs")
}

/// The configured folder, or the default one below `home`.
pub fn session_log_directory(settings: &SessionLogFileSettings, home: &Path) -> PathBuf {
    settings
        .directory
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_session_log_directory(home))
}

/// The file a log started at `started_at` would be named, from the folder and name pattern.
pub fn session_log_file_path(
    settings: &SessionLogFileSettings,
    home: &Path,
    host: &str,
    title: &str,
    kind: &str,
    started_at: NaiveDateTime,
) -> PathBuf {
    let values = SessionLogFileNameValues {
        host: host.to_string(),
        title: title.to_string(),
        kind: kind.to_string(),
        started_at,
    };
    let mut path = session_log_directory(settings, home);
    for segment in session_log_file_name(&settings.filename_pattern, &values) {
        path.push(segment);
    }
    path
}

/// `file` itself, or `file-1`, `file-2` ... when it already exists.
pub fn unused_session_log_file_path(file: &Path) -> PathBuf {
    let mut candidate = file.to_path_buf();
    let mut attempt = 1;
    while candidate.exists() && attempt <= MAX_NAME_ATTEMPTS {
        candidate = numbered_path(file, attempt);
        attempt += 1;
    }
    candidate
}

/// `name-<number>.ext`, numbering before the extension as `path.extname` splits it.
fn numbered_path(file: &Path, number: u32) -> PathBuf {
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension_at = name.rfind('.').filter(|&index| index > 0);
    let numbered = match extension_at {
        Some(index) => format!("{}-{number}{}", &name[..index], &name[index..]),
        None => format!("{name}-{number}"),
    };
    file.with_file_name(numbered)
}

fn open_append(file: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    restrict(&mut options);
    options.open(file)
}

fn open_new(file: &Path) -> io::Result<(PathBuf, File)> {
    let mut attempt = 0;
    loop {
        let candidate = if attempt == 0 {
            file.to_path_buf()
        } else {
            numbered_path(file, attempt)
        };
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        restrict(&mut options);
        match options.open(&candidate) {
            Ok(opened) => return Ok((candidate, opened)),
            Err(err)
                if err.kind() == io::ErrorKind::AlreadyExists && attempt < MAX_NAME_ATTEMPTS =>
            {
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn restrict(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn restrict(_options: &mut OpenOptions) {}

type FailureListener = Box<dyn FnOnce(String) + Send>;

#[derive(Default)]
struct Queue {
    pending: String,
    first_pending_at: Option<Instant>,
    closing: bool,
    /// Why writing stopped; reported once by the writer thread.
    failure: Option<String>,
    failed: bool,
    listener: Option<FailureListener>,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A plain-text transcript written while the session runs. It is normalized
/// like the clean log in session history, but streamed to a file of the
/// user's choosing so it is complete the moment the session ends, and
/// readable if Muxus never gets to end it. Disk writes happen on a thread of
/// their own, at most a quarter second after the text arrives.
pub struct SessionLogFile {
    path: PathBuf,
    timestamps: bool,
    clock: Arc<dyn Clock>,
    input_normalizer: TerminalTextNormalizer,
    output_normalizer: TerminalTextNormalizer,
    line_start: bool,
    closed: bool,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for SessionLogFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionLogFile")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl SessionLogFile {
    /// Open `file` for writing. A file the user picked (`append`) is appended
    /// to; a generated name never reuses an existing file and gets a number instead.
    pub fn open(
        file: &Path,
        append: bool,
        timestamps: bool,
        clock: Arc<dyn Clock>,
    ) -> io::Result<Self> {
        if !file.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the log file path must be absolute",
            ));
        }
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let (path, handle) = if append {
            (file.to_path_buf(), open_append(file)?)
        } else {
            open_new(file)?
        };
        let mut queue = Queue::default();
        // Keep an earlier log in the same file on its own lines.
        if append && handle.metadata()?.len() > 0 {
            queue.pending.push('\n');
            queue.first_pending_at = Some(Instant::now());
        }
        let shared = Arc::new(Shared {
            queue: Mutex::new(queue),
            wake: Condvar::new(),
        });
        let writer_shared = shared.clone();
        thread::Builder::new()
            .name("muxus-log-file".into())
            .spawn(move || write_loop(&writer_shared, handle))?;
        Ok(Self {
            path,
            timestamps,
            clock,
            input_normalizer: TerminalTextNormalizer::new(0),
            output_normalizer: TerminalTextNormalizer::new(4),
            line_start: true,
            closed: false,
            shared,
        })
    }

    /// The file being written (numbered when the requested name was taken).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Called once if a write fails, even after close, on the writer thread;
    /// the file is closed by then.
    pub fn on_failure(&self, listener: impl FnOnce(String) + Send + 'static) {
        self.shared.lock().listener = Some(Box::new(listener));
    }

    pub fn input(&mut self, data: &[u8]) {
        if self.closed {
            return;
        }
        self.drain_output();
        let now = self.clock.now_iso();
        let text = self.input_normalizer.write(data, &now);
        let stamps = self.input_normalizer.take_line_timestamps();
        self.write(&text, &stamps);
    }

    pub fn output(&mut self, data: &[u8]) {
        if self.closed {
            return;
        }
        self.drain_input();
        let now = self.clock.now_iso();
        let text = self.output_normalizer.write(data, &now);
        let stamps = self.output_normalizer.take_line_timestamps();
        self.write(&text, &stamps);
    }

    /// A status line of Muxus' own, always on a line of its own.
    pub fn system(&mut self, message: &str) {
        if self.closed {
            return;
        }
        self.drain_input();
        self.drain_output();
        if !self.line_start {
            self.write("\n", &[]);
        }
        self.write(&format!("{message}\n"), &[]);
    }

    /// Write the rows still on screen and the closing line, then close the file.
    pub fn close(&mut self, message: &str) {
        if self.closed {
            return;
        }
        let text = self.input_normalizer.finish();
        let stamps = self.input_normalizer.take_line_timestamps();
        self.write(&text, &stamps);
        let text = self.output_normalizer.finish();
        let stamps = self.output_normalizer.take_line_timestamps();
        self.write(&text, &stamps);
        self.system(message);
        self.closed = true;
        let mut queue = self.shared.lock();
        queue.closing = true;
        self.shared.wake.notify_all();
    }

    fn drain_input(&mut self) {
        let text = self.input_normalizer.drain();
        let stamps = self.input_normalizer.take_line_timestamps();
        self.write(&text, &stamps);
    }

    fn drain_output(&mut self) {
        let text = self.output_normalizer.drain();
        let stamps = self.output_normalizer.take_line_timestamps();
        self.write(&text, &stamps);
    }

    /// Same line prefixes as the timestamped clean log in session history.
    fn write(&mut self, text: &str, line_timestamps: &[SessionLineTimestamp]) {
        if text.is_empty() {
            return;
        }
        let mut chunk = String::with_capacity(text.len() + 32);
        if self.timestamps {
            let mut stamp = self.clock.now_iso();
            let mut stamp_index = 0;
            let (mut start, mut start_units) = (0usize, 0u64);
            while start < text.len() {
                while let Some(next) = line_timestamps
                    .get(stamp_index)
                    .filter(|next| next.offset <= start_units)
                {
                    stamp.clone_from(&next.recorded_at);
                    stamp_index += 1;
                }
                let (end, newline) = match text[start..].find('\n') {
                    Some(index) => (start + index + 1, true),
                    None => (text.len(), false),
                };
                if self.line_start {
                    chunk.push('[');
                    chunk.push_str(&stamp);
                    chunk.push_str("] ");
                }
                let piece = &text[start..end];
                chunk.push_str(piece);
                self.line_start = newline;
                start_units += piece.encode_utf16().count() as u64;
                start = end;
            }
        } else {
            chunk.push_str(text);
        }
        self.line_start = text.ends_with('\n');

        let mut queue = self.shared.lock();
        if queue.failed {
            return;
        }
        queue.pending.push_str(&chunk);
        queue.first_pending_at.get_or_insert_with(Instant::now);
        if queue.pending.len() > MAX_QUEUED_BYTES {
            // A stalled disk must not grow memory without bound.
            queue.failed = true;
            queue.pending.clear();
            queue.failure = Some("the disk is not keeping up".into());
            self.closed = true;
            self.shared.wake.notify_all();
        } else if queue.pending.len() >= FLUSH_CHARS {
            self.shared.wake.notify_all();
        }
    }
}

impl Drop for SessionLogFile {
    fn drop(&mut self) {
        // Whatever was written reaches the disk even without a closing line.
        let mut queue = self.shared.lock();
        queue.closing = true;
        self.shared.wake.notify_all();
    }
}

fn write_loop(shared: &Shared, mut file: File) {
    loop {
        let (chunk, closing) = {
            let mut queue = shared.lock();
            loop {
                if queue.failed {
                    break;
                }
                let due = queue
                    .first_pending_at
                    .is_some_and(|at| at.elapsed() >= FLUSH_INTERVAL);
                if queue.closing || queue.pending.len() >= FLUSH_CHARS || due {
                    break;
                }
                let wait = queue.first_pending_at.map_or(FLUSH_INTERVAL, |at| {
                    FLUSH_INTERVAL.saturating_sub(at.elapsed())
                });
                queue = shared
                    .wake
                    .wait_timeout(queue, wait)
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .0;
            }
            if queue.failed {
                let failure = queue.failure.take();
                let listener = queue.listener.take();
                drop(queue);
                if let (Some(failure), Some(listener)) = (failure, listener) {
                    listener(failure);
                }
                return;
            }
            queue.first_pending_at = None;
            (std::mem::take(&mut queue.pending), queue.closing)
        };
        if !chunk.is_empty()
            && let Err(err) = file.write_all(chunk.as_bytes())
        {
            let listener = {
                let mut queue = shared.lock();
                queue.failed = true;
                queue.pending.clear();
                queue.listener.take()
            };
            if let Some(listener) = listener {
                listener(err.to_string());
            }
            return;
        }
        if closing && shared.lock().pending.is_empty() {
            let _ = file.flush();
            return;
        }
    }
}
