//! How a script is spread over its hosts (`client/src/scripts/run-plan.ts`).

use std::future::Future;

use futures_util::future::join_all;

/// How a script is spread over its hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Every host starts together.
    Parallel,
    /// `size` hosts at a time; the next batch starts when the whole batch is done.
    Batches(usize),
    /// Each host after the one before has finished.
    Sequential,
}

/// Where one host of a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostRunState {
    Queued,
    Connecting,
    Running,
    Completed,
    Failed,
    Cancelled,
    Skipped,
}

impl HostRunState {
    /// Waiting, connecting or running: the host has not ended yet.
    pub fn is_pending(self) -> bool {
        matches!(
            self,
            HostRunState::Queued | HostRunState::Connecting | HostRunState::Running
        )
    }
}

/// How one host's run ended (`HostRunOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOutcome {
    Completed,
    Failed,
    Cancelled,
}

impl From<HostOutcome> for HostRunState {
    fn from(outcome: HostOutcome) -> Self {
        match outcome {
            HostOutcome::Completed => HostRunState::Completed,
            HostOutcome::Failed => HostRunState::Failed,
            HostOutcome::Cancelled => HostRunState::Cancelled,
        }
    }
}

/// Hosts in the order they start, grouped into the waves that run together.
pub fn run_waves<T: Clone>(hosts: &[T], mode: RunMode) -> Vec<Vec<T>> {
    if hosts.is_empty() {
        return Vec::new();
    }
    let size = match mode {
        RunMode::Parallel => hosts.len(),
        RunMode::Sequential => 1,
        RunMode::Batches(size) => size.max(1),
    };
    hosts.chunks(size).map(<[T]>::to_vec).collect()
}

/// Run `run_one` over the hosts wave by wave. With `stop_on_failure`, a failed host lets its
/// own wave finish and marks every later host skipped. A cancelled job starts nothing more.
pub async fn run_job<T, F, Fut>(
    hosts: &[T],
    mode: RunMode,
    stop_on_failure: bool,
    cancelled: impl Fn() -> bool,
    run_one: F,
    mut skip: impl FnMut(&T),
) where
    T: Clone,
    F: Fn(T) -> Fut,
    Fut: Future<Output = HostOutcome>,
{
    let waves = run_waves(hosts, mode);
    for (index, wave) in waves.iter().enumerate() {
        if cancelled() {
            waves[index..].iter().flatten().for_each(&mut skip);
            return;
        }
        let outcomes = join_all(wave.iter().cloned().map(&run_one)).await;
        if stop_on_failure && outcomes.contains(&HostOutcome::Failed) {
            waves[index + 1..].iter().flatten().for_each(&mut skip);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::time::Duration;

    use super::*;

    const HOSTS: [&str; 5] = ["a", "b", "c", "d", "e"];

    #[test]
    fn runs_everything_together_in_batches_or_one_by_one() {
        assert_eq!(run_waves(&HOSTS, RunMode::Parallel), vec![HOSTS.to_vec()]);
        assert_eq!(
            run_waves(&HOSTS, RunMode::Batches(2)),
            vec![vec!["a", "b"], vec!["c", "d"], vec!["e"]]
        );
        assert_eq!(
            run_waves(&HOSTS, RunMode::Sequential),
            vec![vec!["a"], vec!["b"], vec!["c"], vec!["d"], vec!["e"]]
        );
        assert!(run_waves::<&str>(&[], RunMode::Parallel).is_empty());
        // A batch of nothing still runs one host at a time.
        assert_eq!(run_waves(&HOSTS, RunMode::Batches(0)).len(), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn starts_a_batch_only_when_the_one_before_has_finished() {
        let events = RefCell::new(Vec::new());
        run_job(
            &HOSTS,
            RunMode::Batches(2),
            false,
            || false,
            |host| {
                let events = &events;
                async move {
                    events.borrow_mut().push(format!("start {host}"));
                    let wait = if host == "a" { 20 } else { 1 };
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                    events.borrow_mut().push(format!("end {host}"));
                    HostOutcome::Completed
                }
            },
            |_| {},
        )
        .await;
        let events = events.into_inner();
        let at = |event: &str| events.iter().position(|e| e == event).unwrap();
        assert!(at("start c") > at("end a"));
        assert!(at("start c") > at("end b"));
    }

    #[tokio::test]
    async fn skips_the_rest_after_a_failure_when_asked_to() {
        let ran = RefCell::new(Vec::new());
        let mut skipped = Vec::new();
        run_job(
            &HOSTS,
            RunMode::Sequential,
            true,
            || false,
            |host| {
                ran.borrow_mut().push(host);
                async move {
                    if host == "b" {
                        HostOutcome::Failed
                    } else {
                        HostOutcome::Completed
                    }
                }
            },
            |host| skipped.push(*host),
        )
        .await;
        assert_eq!(ran.into_inner(), vec!["a", "b"]);
        assert_eq!(skipped, vec!["c", "d", "e"]);
    }

    #[tokio::test]
    async fn keeps_going_after_a_failure_otherwise_and_stops_starting_hosts_once_cancelled() {
        let ran = RefCell::new(Vec::new());
        let cancel = Cell::new(false);
        let mut skipped = Vec::new();
        run_job(
            &HOSTS,
            RunMode::Sequential,
            false,
            || cancel.get(),
            |host| {
                ran.borrow_mut().push(host);
                if host == "c" {
                    cancel.set(true);
                }
                async move {
                    if host == "a" {
                        HostOutcome::Failed
                    } else {
                        HostOutcome::Completed
                    }
                }
            },
            |host| skipped.push(*host),
        )
        .await;
        assert_eq!(ran.into_inner(), vec!["a", "b", "c"]);
        assert_eq!(skipped, vec!["d", "e"]);
    }
}
