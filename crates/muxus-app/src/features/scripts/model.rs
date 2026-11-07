//! What the Scripts dialog knows without a window (`client/src/scripts/script-jobs.ts` and the
//! checks of `components/ScriptsDialog.tsx`): which hosts can run scripts, the job as the
//! window shows it, and what stops the Run button.

use std::time::Instant;

use muxus_core::api::MAX_SCRIPT_TIMEOUT_SECONDS;
use muxus_core::js::js_trim;
use muxus_session::scripts::{HostRunState, RunMode, ScriptJobEvent, ScriptTarget};

use crate::hosts::ManagedHost;
use crate::hosts::organization::host_display_name;
use crate::hosts::saved::saved_host_display_name;
use crate::hosts::system_openssh::managed_host_uses_system_open_ssh;
use crate::model::{SavedHostSessionProfile, SshProfile};
use crate::util::locale_compare;

/// Output kept per host in the window; the backend keeps its own copy too.
pub const MAX_OUTPUT: usize = 1024 * 1024;

/// What a new script starts with.
pub const NEW_SCRIPT_BODY: &str = "#!/bin/sh\n";

/// Scripts run over SSH: config hosts and SSH hosts saved in Muxus (`isScriptHost`).
pub fn is_script_host(host: &ManagedHost) -> bool {
    match host {
        ManagedHost::Ssh(_) => true,
        ManagedHost::Profile(entry) => matches!(entry.profile, SavedHostSessionProfile::Ssh(_)),
    }
}

/// `scriptTarget`: SSH hosts only. Scripts run over an SSH exec channel of the built-in client,
/// so hosts set to use system OpenSSH are left out too.
pub fn script_target(host: &ManagedHost) -> Option<ScriptTarget> {
    if managed_host_uses_system_open_ssh(host) {
        return None;
    }
    match host {
        ManagedHost::Ssh(entry) => Some(ScriptTarget {
            key: format!("ssh:{}", entry.alias),
            label: host_display_name(entry).to_owned(),
            profile: SshProfile::new(entry.alias.clone()),
        }),
        ManagedHost::Profile(entry) => {
            let SavedHostSessionProfile::Ssh(profile) = &entry.profile else {
                return None;
            };
            Some(ScriptTarget {
                key: format!("profile:{}", entry.id),
                label: saved_host_display_name(entry).to_owned(),
                profile: SshProfile {
                    profile_id: Some(entry.id.clone()),
                    ..SshProfile::new(profile.target.clone())
                },
            })
        }
    }
}

/// Every host scripts can run on, by name (the dialog's `targets`).
pub fn script_targets(hosts: &[ManagedHost]) -> Vec<ScriptTarget> {
    let mut targets: Vec<ScriptTarget> = hosts.iter().filter_map(script_target).collect();
    targets.sort_by(|a, b| locale_compare(&a.label, &b.label));
    targets
}

/// The profile a dial uses, with the keepalive preference (`sshKeepalivePrefField`); a
/// disabled keepalive is left out, so a stale value can never outvote it.
pub fn with_keepalive(mut target: ScriptTarget, keepalive_seconds: Option<f64>) -> ScriptTarget {
    target.profile.keepalive_interval_seconds = keepalive_seconds
        .filter(|seconds| *seconds > 0.)
        .map(|seconds| seconds.round() as u32);
    target
}

/// What opening Scripts for some hosts does (`openScriptsFor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenScripts {
    /// Open with these hosts picked; `notice` says how many were left out.
    Open {
        keys: Vec<String>,
        notice: Option<String>,
    },
    /// None of them can run scripts: say so instead.
    Nothing(String),
}

/// Open Scripts with these hosts picked: one host, a selection or a folder. Hosts that cannot
/// run scripts (Telnet, serial, RDP, system OpenSSH …) are left out. `what` names them for the
/// notice ("This host", "The selection", "“Lab”").
pub fn open_scripts_for(hosts: &[ManagedHost], what: &str) -> OpenScripts {
    let keys: Vec<String> = hosts
        .iter()
        .filter_map(script_target)
        .map(|target| target.key)
        .collect();
    if keys.is_empty() {
        return OpenScripts::Nothing(format!("{what} has no SSH hosts to run a script on."));
    }
    let left = hosts.len() - keys.len();
    let notice = (left > 0).then(|| {
        format!(
            "{left} host{} left out: scripts run only on SSH hosts that use the built-in client.",
            if left == 1 { "" } else { "s" }
        )
    });
    OpenScripts::Open { keys, notice }
}

/// One host of a job.
#[derive(Debug, Clone, PartialEq)]
pub struct HostRun {
    pub key: String,
    pub label: String,
    pub state: HostRunState,
    pub output: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub started_at: Option<Instant>,
    pub ended_at: Option<Instant>,
}

/// A job as the window shows it (`ScriptJob`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptJob {
    pub id: u64,
    pub script_name: String,
    /// The script this job runs, kept so a rerun runs it again unchanged.
    pub body: String,
    pub timeout_seconds: u32,
    pub mode: RunMode,
    pub stop_on_failure: bool,
    pub started_at: Instant,
    pub ended_at: Option<Instant>,
    pub hosts: Vec<HostRun>,
    pub cancelled: bool,
}

/// What a job runs: the dialog's choices.
#[derive(Debug, Clone, PartialEq)]
pub struct JobInput {
    pub script_name: String,
    pub body: String,
    pub targets: Vec<ScriptTarget>,
    pub mode: RunMode,
    pub stop_on_failure: bool,
    pub timeout_seconds: u32,
}

impl ScriptJob {
    pub fn new(id: u64, input: &JobInput, now: Instant) -> Self {
        Self {
            id,
            script_name: input.script_name.clone(),
            body: input.body.clone(),
            timeout_seconds: input.timeout_seconds,
            mode: input.mode,
            stop_on_failure: input.stop_on_failure,
            started_at: now,
            ended_at: None,
            hosts: input
                .targets
                .iter()
                .map(|target| HostRun {
                    key: target.key.clone(),
                    label: target.label.clone(),
                    state: HostRunState::Queued,
                    output: String::new(),
                    exit_code: None,
                    error: None,
                    started_at: None,
                    ended_at: None,
                })
                .collect(),
            cancelled: false,
        }
    }

    /// `jobRunning`.
    pub fn running(&self) -> bool {
        self.ended_at.is_none()
    }

    /// Take in what the backend reported.
    pub fn apply(&mut self, event: &ScriptJobEvent) {
        match event {
            ScriptJobEvent::Host {
                key,
                state,
                exit_code,
                error,
                at,
            } => {
                let Some(host) = self.hosts.iter_mut().find(|host| &host.key == key) else {
                    return;
                };
                host.state = *state;
                if *state == HostRunState::Connecting {
                    host.started_at = Some(*at);
                }
                if exit_code.is_some() {
                    host.exit_code = *exit_code;
                }
                if error.is_some() {
                    host.error = error.clone();
                }
                if !state.is_pending() && host.started_at.is_some() {
                    host.ended_at = Some(*at);
                }
            }
            ScriptJobEvent::Output { key, text } => {
                if let Some(host) = self.hosts.iter_mut().find(|host| &host.key == key) {
                    host.output.push_str(text);
                    if host.output.len() > MAX_OUTPUT {
                        let mut cut = host.output.len() - MAX_OUTPUT;
                        while !host.output.is_char_boundary(cut) {
                            cut += 1;
                        }
                        host.output.drain(..cut);
                    }
                }
            }
            ScriptJobEvent::Finished { at } => self.ended_at = Some(*at),
            ScriptJobEvent::Prompt(_) | ScriptJobEvent::PromptWithdrawn { .. } => {}
        }
    }

    /// Hosts in any of `states`.
    pub fn count(&self, states: &[HostRunState]) -> usize {
        self.hosts
            .iter()
            .filter(|host| states.contains(&host.state))
            .count()
    }

    /// The hosts where the script failed, for "Run again on failed hosts".
    pub fn failed_keys(&self) -> Vec<String> {
        self.hosts
            .iter()
            .filter(|host| host.state == HostRunState::Failed)
            .map(|host| host.key.clone())
            .collect()
    }

    /// The job's own script and choices, on the hosts where it failed (`rerunFailedHosts`).
    pub fn rerun_input(&self, targets: &[ScriptTarget]) -> JobInput {
        let failed = self.failed_keys();
        JobInput {
            script_name: self.script_name.clone(),
            body: self.body.clone(),
            targets: targets
                .iter()
                .filter(|target| failed.contains(&target.key))
                .cloned()
                .collect(),
            mode: self.mode,
            stop_on_failure: self.stop_on_failure,
            timeout_seconds: self.timeout_seconds,
        }
    }
}

/// `modeLabel`.
pub fn mode_label(mode: RunMode) -> String {
    match mode {
        RunMode::Parallel => "All at once".into(),
        RunMode::Sequential => "One by one".into(),
        RunMode::Batches(size) => format!("In batches of {size}"),
    }
}

/// `stateLabel`.
pub fn state_label(state: HostRunState) -> &'static str {
    match state {
        HostRunState::Queued => "waiting",
        HostRunState::Connecting => "connecting",
        HostRunState::Running => "running",
        HostRunState::Completed => "succeeded",
        HostRunState::Failed => "failed",
        HostRunState::Cancelled => "cancelled",
        HostRunState::Skipped => "not run",
    }
}

/// `hostSummary`: the state, a failing exit code and how long it took.
pub fn host_summary(run: &HostRun) -> String {
    let mut parts = vec![state_label(run.state).to_owned()];
    if let Some(code) = run.exit_code
        && run.state != HostRunState::Completed
    {
        parts.push(format!("exit {code}"));
    }
    if let (Some(started), Some(ended)) = (run.started_at, run.ended_at) {
        parts.push(format!(
            "{:.1} s",
            ended.saturating_duration_since(started).as_secs_f64()
        ));
    }
    parts.join(" · ")
}

/// The heading over a host's output.
pub fn host_heading(run: &HostRun) -> String {
    let mut heading = format!("{} — {}", run.label, state_label(run.state));
    if let Some(code) = run.exit_code {
        heading.push_str(&format!(", exit {code}"));
    }
    heading
}

/// The radio choice behind a [`RunMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    Parallel,
    Batches,
    Sequential,
}

/// The batch size field as a number: digits only, at least one.
pub fn batch_size(text: &str) -> Option<usize> {
    js_trim(text)
        .parse::<usize>()
        .ok()
        .filter(|size| *size >= 1)
}

/// The mode the choices make (`mode`): a batch size that is no number is one host.
pub fn run_mode(kind: ModeKind, batch_text: &str) -> RunMode {
    match kind {
        ModeKind::Parallel => RunMode::Parallel,
        ModeKind::Sequential => RunMode::Sequential,
        ModeKind::Batches => RunMode::Batches(batch_size(batch_text).unwrap_or(1)),
    }
}

/// The time limit field (minutes, may be fractional; empty is 0): seconds, or what is wrong.
pub fn timeout_seconds(text: &str) -> Result<u32, String> {
    let text = js_trim(text);
    let minutes = if text.is_empty() {
        0.
    } else {
        text.parse::<f64>().unwrap_or(f64::NAN)
    };
    if !minutes.is_finite() || minutes < 0. {
        return Err("Enter the time limit in minutes, or 0.".into());
    }
    let max_minutes = MAX_SCRIPT_TIMEOUT_SECONDS / 60;
    if minutes > f64::from(max_minutes) {
        return Err(format!(
            "The time limit can be at most {max_minutes} minutes (a week)."
        ));
    }
    Ok((minutes * 60.).round() as u32)
}

/// What keeps the Run button off (`problem`), first problem first.
pub fn run_problem(
    body: &str,
    chosen: usize,
    kind: ModeKind,
    batch_text: &str,
    timeout_text: &str,
    running: bool,
) -> Option<String> {
    if js_trim(body).is_empty() {
        return Some("The script is empty.".into());
    }
    if chosen == 0 {
        return Some("Pick the hosts to run it on.".into());
    }
    if kind == ModeKind::Batches && batch_size(batch_text).is_none() {
        return Some("A batch needs at least one host.".into());
    }
    if let Err(problem) = timeout_seconds(timeout_text) {
        return Some(problem);
    }
    running.then(|| "A script is still running.".into())
}

/// "Run on 3 hosts".
pub fn run_label(count: usize) -> String {
    if count == 0 {
        "Run on hosts".into()
    } else {
        format!("Run on {count} host{}", if count == 1 { "" } else { "s" })
    }
}

/// A script file's name without its extension (`file.name.replace(/\.[^.]+$/, '')`).
pub fn script_name_from_file(file_name: &str) -> String {
    match file_name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < file_name.len() => file_name[..dot].to_owned(),
        _ => file_name.to_owned(),
    }
}

/// "3 lines" under a saved script's name.
pub fn line_count_label(body: &str) -> String {
    format!("{} lines", body.split('\n').count())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::model::{HostMetadata, SavedHostProfile};

    fn ssh(alias: &str, system: bool) -> ManagedHost {
        let mut entry = crate::hosts::test_support::ssh_host(alias, None);
        if system {
            entry.metadata = Some(HostMetadata {
                system_open_ssh: Some(true),
                ..HostMetadata::default()
            });
        }
        ManagedHost::Ssh(Arc::new(entry))
    }

    fn saved(id: &str, profile: serde_json::Value) -> ManagedHost {
        let entry: SavedHostProfile = serde_json::from_value(serde_json::json!({
            "id": id,
            "kind": profile["kind"].clone(),
            "name": id.to_uppercase(),
            "profile": profile,
            "metadata": { "profileId": id, "connectCount": 0 },
            "createdAt": "2026-11-01T00:00:00Z",
            "updatedAt": "2026-11-01T00:00:00Z",
        }))
        .unwrap();
        ManagedHost::Profile(Arc::new(entry))
    }

    #[test]
    fn only_ssh_hosts_on_the_built_in_client_run_scripts() {
        let web = ssh("web", false);
        let target = script_target(&web).unwrap();
        assert_eq!(target.key, "ssh:web");
        assert_eq!(target.profile, SshProfile::new("web"));
        assert!(script_target(&ssh("krb", true)).is_none());
        assert!(is_script_host(&ssh("krb", true)));

        let db = saved(
            "db",
            serde_json::json!({ "kind": "ssh", "target": "ops@db" }),
        );
        let target = script_target(&db).unwrap();
        assert_eq!(target.key, "profile:db");
        assert_eq!(target.label, "DB");
        assert_eq!(target.profile.profile_id.as_deref(), Some("db"));
        assert_eq!(target.profile.target, "ops@db");

        let console = saved(
            "console",
            serde_json::json!({ "kind": "telnet", "host": "10.0.0.1", "port": 23 }),
        );
        assert!(!is_script_host(&console));
        assert!(script_target(&console).is_none());
    }

    #[test]
    fn opening_for_hosts_leaves_out_the_ones_that_cannot_run_scripts() {
        let console = saved(
            "console",
            serde_json::json!({ "kind": "telnet", "host": "10.0.0.1", "port": 23 }),
        );
        assert_eq!(
            open_scripts_for(&[ssh("a", false), console.clone()], "The selection"),
            OpenScripts::Open {
                keys: vec!["ssh:a".into()],
                notice: Some(
                    "1 host left out: scripts run only on SSH hosts that use the built-in client."
                        .into()
                ),
            }
        );
        assert_eq!(
            open_scripts_for(&[ssh("a", false), ssh("b", false)], "x"),
            OpenScripts::Open {
                keys: vec!["ssh:a".into(), "ssh:b".into()],
                notice: None,
            }
        );
        assert_eq!(
            open_scripts_for(&[console, ssh("k", true)], "“Lab”"),
            OpenScripts::Nothing("“Lab” has no SSH hosts to run a script on.".into())
        );
    }

    #[test]
    fn a_disabled_keepalive_is_left_out_of_the_dial() {
        let target = script_target(&ssh("a", false)).unwrap();
        assert_eq!(
            with_keepalive(target.clone(), Some(14.6))
                .profile
                .keepalive_interval_seconds,
            Some(15)
        );
        assert_eq!(
            with_keepalive(target, Some(0.))
                .profile
                .keepalive_interval_seconds,
            None
        );
    }

    fn input(keys: &[&str]) -> JobInput {
        JobInput {
            script_name: "Disk".into(),
            body: "df -h\n".into(),
            targets: keys
                .iter()
                .map(|key| ScriptTarget {
                    key: (*key).into(),
                    label: key.to_uppercase(),
                    profile: SshProfile::new(*key),
                })
                .collect(),
            mode: RunMode::Batches(2),
            stop_on_failure: true,
            timeout_seconds: 60,
        }
    }

    #[test]
    fn a_job_follows_its_hosts_and_reruns_the_failed_ones_with_its_own_script() {
        let start = Instant::now();
        let input = input(&["a", "b", "c"]);
        let mut job = ScriptJob::new(1, &input, start);
        assert!(job.running());
        assert_eq!(job.count(&[HostRunState::Queued]), 3);
        let host =
            |key: &str, state, exit_code, error: Option<&str>, after: u64| ScriptJobEvent::Host {
                key: key.into(),
                state,
                exit_code,
                error: error.map(str::to_owned),
                at: start + Duration::from_millis(after),
            };
        job.apply(&host("a", HostRunState::Connecting, None, None, 0));
        job.apply(&host("a", HostRunState::Running, None, None, 100));
        job.apply(&ScriptJobEvent::Output {
            key: "a".into(),
            text: "disk full\n".into(),
        });
        job.apply(&host("a", HostRunState::Failed, Some(2), None, 1500));
        job.apply(&host("b", HostRunState::Connecting, None, None, 0));
        job.apply(&host("b", HostRunState::Failed, None, Some("refused"), 200));
        job.apply(&host("c", HostRunState::Skipped, None, None, 300));
        job.apply(&ScriptJobEvent::Finished {
            at: start + Duration::from_secs(2),
        });
        assert!(!job.running());
        assert_eq!(job.hosts[0].output, "disk full\n");
        assert_eq!(host_summary(&job.hosts[0]), "failed · exit 2 · 1.5 s");
        assert_eq!(host_heading(&job.hosts[0]), "A — failed, exit 2");
        assert_eq!(host_summary(&job.hosts[1]), "failed · 0.2 s");
        assert_eq!(host_summary(&job.hosts[2]), "not run");
        assert_eq!(job.count(&[HostRunState::Failed]), 2);

        // The editor may hold another script by now; the rerun runs the job's own.
        let rerun = job.rerun_input(&input.targets);
        assert_eq!(rerun.body, "df -h\n");
        assert_eq!(
            rerun
                .targets
                .iter()
                .map(|t| t.key.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(rerun.mode, RunMode::Batches(2));
        assert!(rerun.stop_on_failure);
    }

    #[test]
    fn keeps_the_end_of_a_long_output() {
        let mut job = ScriptJob::new(1, &input(&["a"]), Instant::now());
        let chunk = "é".repeat(MAX_OUTPUT / 2 + 1);
        for _ in 0..2 {
            job.apply(&ScriptJobEvent::Output {
                key: "a".into(),
                text: chunk.clone(),
            });
        }
        assert!(job.hosts[0].output.len() <= MAX_OUTPUT);
        assert!(job.hosts[0].output.chars().all(|c| c == 'é'));
    }

    #[test]
    fn says_what_keeps_the_run_button_off() {
        use ModeKind::*;
        assert_eq!(
            run_problem(" ", 1, Parallel, "5", "30", false).as_deref(),
            Some("The script is empty.")
        );
        assert_eq!(
            run_problem("id", 0, Parallel, "5", "30", false).as_deref(),
            Some("Pick the hosts to run it on.")
        );
        assert_eq!(
            run_problem("id", 2, Batches, "0", "30", false).as_deref(),
            Some("A batch needs at least one host.")
        );
        assert_eq!(
            run_problem("id", 2, Batches, "3", "x", false).as_deref(),
            Some("Enter the time limit in minutes, or 0.")
        );
        assert_eq!(
            run_problem("id", 2, Sequential, "", "10081", false).as_deref(),
            Some("The time limit can be at most 10080 minutes (a week).")
        );
        assert_eq!(
            run_problem("id", 2, Parallel, "", "", true).as_deref(),
            Some("A script is still running.")
        );
        assert_eq!(run_problem("id", 2, Parallel, "", "1.5", false), None);
        assert_eq!(timeout_seconds("1.5"), Ok(90));
        assert_eq!(timeout_seconds(""), Ok(0));
        assert_eq!(run_mode(Batches, "4"), RunMode::Batches(4));
        assert_eq!(run_mode(Batches, ""), RunMode::Batches(1));
        assert_eq!(mode_label(RunMode::Batches(4)), "In batches of 4");
        assert_eq!(run_label(1), "Run on 1 host");
        assert_eq!(run_label(3), "Run on 3 hosts");
    }

    #[test]
    fn names_a_script_after_its_file() {
        assert_eq!(script_name_from_file("disk-check.sh"), "disk-check");
        assert_eq!(script_name_from_file(".profile"), ".profile");
        assert_eq!(script_name_from_file("Makefile"), "Makefile");
        assert_eq!(line_count_label("a\nb\n"), "3 lines");
    }
}
