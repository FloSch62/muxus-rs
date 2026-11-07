//! Scripts running on live SSH connections (`server/src/scripts/script-runs.ts`): each run is
//! one exec channel on a connection it holds a lease on. Its output and end are read back by
//! offset ([`ScriptRuns::status`]); [`ScriptRuns::watch`] says when there is more to read.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use muxus_core::api::{ScriptRunState, ScriptRunStatus};
use muxus_ssh::connection::session::{SessionRequest, open_session};
use muxus_ssh::connection::{ConnectionLeaseOwner, SshConnectionManager};
use russh::{ChannelMsg, Sig};
use tokio::sync::{mpsc, watch};

use super::wrapper::{find_job_marker, kill_job_command, script_command};
use crate::error::{ApiError, ApiResult};
use crate::logging::utf8::Utf8Stream;

/// Output kept per run; older output is dropped from the start.
const MAX_OUTPUT: usize = 1024 * 1024;
/// Finished runs stay readable this long, so a slow reader still sees the end.
const KEEP_FINISHED: Duration = Duration::from_secs(30 * 60);
const MAX_RUNS: usize = 1000;
/// Stderr held back while waiting for the job marker, at most.
const MAX_HELD_STDERR: usize = 64 * 1024;
/// How long stopping waits for the kill command over the connection.
const KILL_WAIT: Duration = Duration::from_secs(5);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What a run has said so far.
struct RunData {
    state: ScriptRunState,
    exit_code: Option<i32>,
    error: Option<String>,
    /// Output kept, starting `base` bytes into the whole output.
    output: String,
    base: u64,
    finished_at: Option<Instant>,
}

struct Run {
    id: String,
    data: Mutex<RunData>,
    /// Bumped on every change, for [`ScriptRuns::watch`].
    changed: watch::Sender<u64>,
    /// Tells the run's task to stop the script (the state is already settled).
    stop: mpsc::UnboundedSender<()>,
}

impl Run {
    fn notify(&self) {
        self.changed.send_modify(|version| *version += 1);
    }

    fn append(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        {
            let mut data = lock(&self.data);
            data.output.push_str(text);
            if data.output.len() > MAX_OUTPUT {
                let mut drop = data.output.len() - MAX_OUTPUT;
                while !data.output.is_char_boundary(drop) {
                    drop += 1;
                }
                data.output.drain(..drop);
                data.base += drop as u64;
            }
        }
        self.notify();
    }

    /// End the run as `state`, once: later calls change nothing.
    fn settle(&self, state: ScriptRunState, exit_code: Option<i32>, error: Option<String>) -> bool {
        {
            let mut data = lock(&self.data);
            if data.state != ScriptRunState::Running {
                return false;
            }
            data.state = state;
            if exit_code.is_some() {
                data.exit_code = exit_code;
            }
            if error.is_some() {
                data.error = error;
            }
            data.finished_at = Some(Instant::now());
        }
        self.notify();
        true
    }

    fn stop(&self, state: ScriptRunState, error: Option<String>) {
        if self.settle(state, None, error) {
            let _ = self.stop.send(());
        }
    }
}

/// Scripts running on live SSH connections.
#[derive(Clone)]
pub struct ScriptRuns {
    connections: SshConnectionManager,
    runs: Arc<Mutex<HashMap<String, Arc<Run>>>>,
}

impl ScriptRuns {
    pub fn new(connections: SshConnectionManager) -> Self {
        Self {
            connections,
            runs: Arc::default(),
        }
    }

    /// Start `body` on the connection `conn_id`; `timeout_seconds` of 0 lets it run. Must be
    /// called on the backend runtime.
    pub async fn start(
        &self,
        conn_id: &str,
        body: &str,
        timeout_seconds: u32,
    ) -> ApiResult<String> {
        self.prune();
        let Some(lease) = self
            .connections
            .acquire(conn_id, ConnectionLeaseOwner::Script)
        else {
            return Err(ApiError::not_found(
                "The SSH connection is gone. Run the script again.",
            ));
        };
        let conn = lease.connection().clone();
        // A host that refuses at once (cmd.exe has no sh) can send its exit status with the
        // exec reply; the session keeps it among its pending messages.
        let session = open_session(
            conn.handle(),
            &SessionRequest {
                command: Some(script_command()),
                ..SessionRequest::default()
            },
        )
        .await
        .map_err(|error| {
            ApiError::new(502, format!("The host would not start the script: {error}"))
        })?;

        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
        let run = Arc::new(Run {
            id: nanoid(),
            data: Mutex::new(RunData {
                state: ScriptRunState::Running,
                exit_code: None,
                error: None,
                output: String::new(),
                base: 0,
                finished_at: None,
            }),
            changed: watch::channel(0).0,
            stop: stop_tx,
        });
        lock(&self.runs).insert(run.id.clone(), run.clone());
        let mut body = body.to_owned();
        if !body.ends_with('\n') {
            body.push('\n');
        }
        let id = run.id.clone();
        tokio::spawn(drive(
            run,
            session,
            conn,
            lease,
            body,
            timeout_seconds,
            stop_rx,
        ));
        Ok(id)
    }

    /// The run's state and its output from `since` (a byte offset into the whole output) on.
    pub fn status(&self, id: &str, since: u64) -> ApiResult<ScriptRunStatus> {
        let run = self.get(id)?;
        let data = lock(&run.data);
        let end = data.base + data.output.len() as u64;
        let mut from = (since.min(end).max(data.base) - data.base) as usize;
        while !data.output.is_char_boundary(from) {
            from += 1;
        }
        Ok(ScriptRunStatus {
            id: run.id.clone(),
            state: data.state,
            exit_code: data.exit_code,
            output: data.output[from..].to_owned(),
            offset: end,
            truncated: since < data.base,
            error: data.error.clone(),
        })
    }

    /// Changes whenever the run has more to read.
    pub fn watch(&self, id: &str) -> ApiResult<watch::Receiver<u64>> {
        Ok(self.get(id)?.changed.subscribe())
    }

    pub fn cancel(&self, id: &str) {
        let run = lock(&self.runs).get(id).cloned();
        if let Some(run) = run {
            run.stop(ScriptRunState::Cancelled, None);
        }
    }

    pub fn stop_all(&self) {
        let runs: Vec<Arc<Run>> = lock(&self.runs).values().cloned().collect();
        for run in runs {
            run.stop(ScriptRunState::Cancelled, None);
        }
    }

    fn get(&self, id: &str) -> ApiResult<Arc<Run>> {
        lock(&self.runs)
            .get(id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("This run is no longer known."))
    }

    fn prune(&self) {
        let now = Instant::now();
        let mut runs = lock(&self.runs);
        runs.retain(|_, run| {
            lock(&run.data)
                .finished_at
                .is_none_or(|finished| now.duration_since(finished) <= KEEP_FINISHED)
        });
        if runs.len() < MAX_RUNS {
            return;
        }
        let mut finished: Vec<(Instant, String)> = runs
            .values()
            .filter_map(|run| Some((lock(&run.data).finished_at?, run.id.clone())))
            .collect();
        finished.sort();
        let excess = runs.len() - MAX_RUNS + 1;
        for (_, id) in finished.into_iter().take(excess) {
            runs.remove(&id);
        }
    }
}

/// How the script's channel ended.
enum Exit {
    Code(u32),
    Signal(String),
}

fn signal_name(signal: &Sig) -> String {
    match signal {
        Sig::Custom(name) => name.clone(),
        other => format!("{other:?}"),
    }
}

/// `formatDuration`.
fn format_duration(seconds: u32) -> String {
    if seconds.is_multiple_of(60) {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds} s")
    }
}

/// Output as it arrives: stdout and stderr decoded apart, stderr held until the job marker
/// is seen and taken out of it.
struct Streams {
    stdout: Utf8Stream,
    stderr: Utf8Stream,
    held: String,
    /// The run's process group; `Some(0)` when the marker never came in time.
    job: Option<u32>,
}

impl Streams {
    fn take_stderr(&mut self, run: &Run, text: &str) {
        if self.job.is_some() {
            run.append(text);
            return;
        }
        self.held.push_str(text);
        if let Some((start, len, group)) = find_job_marker(&self.held) {
            self.job = Some(group);
            let mut rest = std::mem::take(&mut self.held);
            rest.replace_range(start..start + len, "");
            run.append(&rest);
        } else if self.held.len() > MAX_HELD_STDERR {
            run.append(&std::mem::take(&mut self.held));
            self.job = Some(0);
        }
    }

    /// Everything still decoding or held back.
    fn flush(&mut self, run: &Run) {
        run.append(&self.stdout.finish());
        let tail = self.stderr.finish();
        self.take_stderr(run, &tail);
        run.append(&std::mem::take(&mut self.held));
    }
}

/// The run's task: feeds the script in, reads its output and ends it.
async fn drive(
    run: Arc<Run>,
    session: muxus_ssh::connection::SessionChannel,
    conn: Arc<muxus_ssh::connection::ManagedConnection>,
    lease: muxus_ssh::connection::manager::ConnectionLease,
    body: String,
    timeout_seconds: u32,
    mut stop: mpsc::UnboundedReceiver<()>,
) {
    let muxus_ssh::connection::SessionChannel {
        channel, pending, ..
    } = session;
    let (mut reader, writer) = channel.split();
    let writer = Arc::new(writer);
    let feed = {
        let writer = writer.clone();
        tokio::spawn(async move {
            let _ = writer.data(body.as_bytes()).await;
            let _ = writer.eof().await;
        })
    };
    let mut streams = Streams {
        stdout: Utf8Stream::new(),
        stderr: Utf8Stream::new(),
        held: String::new(),
        job: None,
    };
    let mut exit: Option<Exit> = None;
    let mut pending = pending;
    let deadline = (timeout_seconds > 0)
        .then(|| tokio::time::Instant::now() + Duration::from_secs(u64::from(timeout_seconds)));
    let timer = async {
        match deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(timer);
    let lost = conn.closed();
    tokio::pin!(lost);

    // Exit status can arrive before the last output; the run ends on close.
    let stopped = loop {
        let message = if let Some(message) = pending.pop_front() {
            Some(message)
        } else {
            tokio::select! {
                message = reader.wait() => message,
                _ = &mut timer => {
                    streams.flush(&run);
                    run.settle(
                        ScriptRunState::TimedOut,
                        None,
                        Some(format!("Stopped after {}.", format_duration(timeout_seconds))),
                    );
                    break true;
                }
                _ = stop.recv() => {
                    streams.flush(&run);
                    break true;
                }
                reason = &mut lost => {
                    streams.flush(&run);
                    run.settle(
                        ScriptRunState::Failed,
                        None,
                        Some(match reason {
                            Some(reason) if !reason.is_empty() => {
                                format!("The SSH connection was lost: {reason}")
                            }
                            _ => "The SSH connection was lost.".into(),
                        }),
                    );
                    break false;
                }
            }
        };
        match message {
            Some(ChannelMsg::Data { data }) => {
                let text = streams.stdout.decode(&data);
                run.append(&text);
            }
            Some(ChannelMsg::ExtendedData { data, .. }) => {
                let text = streams.stderr.decode(&data);
                streams.take_stderr(&run, &text);
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                exit.get_or_insert(Exit::Code(exit_status));
            }
            Some(ChannelMsg::ExitSignal { signal_name: s, .. }) => {
                exit.get_or_insert(Exit::Signal(signal_name(&s)));
            }
            Some(ChannelMsg::Close) | None => {
                streams.flush(&run);
                finish_on_close(&run, exit.take(), streams.job);
                break false;
            }
            Some(_) => {}
        }
    };
    feed.abort();
    if stopped {
        // Closing the channel leaves a background script running; stop its process group
        // over the same connection, then let go of it.
        if let Some(group) = streams.job.filter(|group| *group > 1) {
            let _ = conn.exec(&kill_job_command(group), KILL_WAIT).await;
        }
        // Not every server takes signals.
        let _ = writer.signal(Sig::TERM).await;
        let _ = writer.close().await;
    }
    drop(lease);
}

/// The channel closed: how the run went, from its exit status.
fn finish_on_close(run: &Run, exit: Option<Exit>, job: Option<u32>) {
    match exit {
        None => {
            run.settle(
                ScriptRunState::Failed,
                None,
                Some("The connection closed before the script finished.".into()),
            );
        }
        Some(Exit::Code(code)) => {
            let empty = lock(&run.data).output.is_empty();
            let error = if code == 125 && empty {
                Some("The host could not store the script in a temporary file.".into())
            } else if code != 0 && code != 125 && job.is_none() {
                // No marker: the wrapper itself never ran, e.g. on a Windows host whose shell
                // is cmd.exe, whatever language it answers in.
                Some(
                    "The script never started: this host has no POSIX shell (sh). Scripts run on Linux and Unix hosts."
                        .into(),
                )
            } else {
                None
            };
            let state = if code == 0 {
                ScriptRunState::Completed
            } else {
                ScriptRunState::Failed
            };
            run.settle(state, Some(code as i32), error);
        }
        Some(Exit::Signal(signal)) => {
            run.settle(
                ScriptRunState::Failed,
                None,
                Some(format!("Stopped by signal {signal}.")),
            );
        }
    }
}

/// A short random id (`nanoid(10)`).
fn nanoid() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..10].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_time_limit() {
        assert_eq!(format_duration(60), "1 min");
        assert_eq!(format_duration(1800), "30 min");
        assert_eq!(format_duration(90), "90 s");
    }
}
