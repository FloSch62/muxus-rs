//! Scripts on many hosts: the script library routes, runs on a live connection (port of
//! `tests/unit/server/script-runs.test.ts`, against the in-process test sshd running each exec
//! with the local `sh`) and jobs over several hosts with their sign-in questions.

#![cfg(unix)]

mod support;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use muxus_core::api::{SavedScriptInput, ScriptRunRequest, ScriptRunState, ScriptRunStatus};
use muxus_core::protocol::{
    AuthPromptResponse, SshProfile, TerminalClientMessage, TerminalServerMessage,
};
use muxus_session::scripts::{
    HostRunState, RunMode, ScriptJobEvent, ScriptJobHandle, ScriptJobRequest, ScriptPrompt,
    ScriptPromptAnswer, ScriptTarget, script_command,
};
use muxus_session::{Backend, SessionEvent, SessionHandle};
use muxus_ssh::connection::ConnectionLeaseOwner;
use muxus_ssh::testing::server::{ServerSpec, TestServer, start as start_sshd};
use serde_json::json;
use support::{Sandbox, body, next_event, start};

/// A server whose exec runs the command with the local sh, stdin and all.
fn shell_spec() -> ServerSpec {
    let mut spec = ServerSpec::new()
        .password("pw")
        .methods(&[russh::MethodKind::Password]);
    spec.exec_shell = true;
    spec
}

/// A sandbox whose ssh_config names each server `lab`, `lab2` …
async fn lab(servers: &[&TestServer]) -> Sandbox {
    let sandbox = start().await;
    let mut lines = Vec::new();
    for (index, server) in servers.iter().enumerate() {
        let name = if index == 0 {
            "lab".to_owned()
        } else {
            format!("lab{}", index + 1)
        };
        lines.push(format!("Host {name}"));
        lines.push("  HostName 127.0.0.1".into());
        lines.push("  User tester".into());
        lines.push(format!("  Port {}", server.port()));
    }
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    sandbox.write_ssh_config(&lines);
    sandbox
}

/// Dial `lab`, trusting its key and signing in with `pw`; the dial and the connection id.
async fn connected(backend: &Backend) -> (SessionHandle, String) {
    let dial = backend.open_session(body(
        json!({ "op": "dial", "profile": { "kind": "ssh", "target": "lab" } }),
    ));
    loop {
        match next_event(&dial).await {
            SessionEvent::Message(TerminalServerMessage::HostKey(_)) => {
                dial.send(TerminalClientMessage::HostKeyResponse { accept: true })
            }
            SessionEvent::Message(TerminalServerMessage::AuthPrompt(_)) => {
                dial.send(TerminalClientMessage::AuthResponse(password()))
            }
            SessionEvent::Message(TerminalServerMessage::Ready { conn_id, .. }) => {
                return (dial, conn_id);
            }
            SessionEvent::Message(TerminalServerMessage::Exit { message, .. }) => {
                panic!("the dial failed: {message:?}")
            }
            _ => {}
        }
    }
}

fn password() -> AuthPromptResponse {
    AuthPromptResponse {
        answers: vec!["pw".into()],
        remember_password: None,
        skipped: None,
    }
}

async fn run(backend: &Backend, conn_id: &str, script: &str, timeout: Option<u32>) -> String {
    backend
        .start_script_run(ScriptRunRequest {
            conn_id: conn_id.into(),
            body: script.into(),
            timeout_seconds: timeout,
        })
        .await
        .expect("the run starts")
}

async fn finished(backend: &Backend, id: &str) -> ScriptRunStatus {
    for _ in 0..400 {
        let status = backend.script_run_status(id, 0).await.unwrap();
        if status.state != ScriptRunState::Running {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the run did not finish");
}

/// Script leases on the connection, once they have settled.
async fn script_leases(backend: &Backend, conn_id: &str) -> usize {
    for _ in 0..200 {
        let count = backend
            .connections()
            .lease_count(conn_id, Some(&[ConnectionLeaseOwner::Script]));
        if count == 0 {
            return 0;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    backend
        .connections()
        .lease_count(conn_id, Some(&[ConnectionLeaseOwner::Script]))
}

// --- the library -------------------------------------------------------------------------------

#[tokio::test]
async fn keeps_scripts_by_name_and_replaces_them_by_id() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let saved = backend
        .save_script(SavedScriptInput {
            id: None,
            name: "  Uptime ".into(),
            body: "uptime\n".into(),
        })
        .await
        .unwrap();
    assert_eq!(saved.name, "Uptime");
    backend
        .save_script(SavedScriptInput {
            id: None,
            name: "disk".into(),
            body: "df -h\n".into(),
        })
        .await
        .unwrap();
    let replaced = backend
        .save_script(SavedScriptInput {
            id: Some(saved.id.clone()),
            name: "Uptime".into(),
            body: "uptime -p\n".into(),
        })
        .await
        .unwrap();
    assert_eq!(replaced.id, saved.id);
    let names: Vec<String> = backend
        .scripts()
        .await
        .unwrap()
        .scripts
        .into_iter()
        .map(|script| format!("{}: {}", script.name, script.body.trim()))
        .collect();
    assert_eq!(names, ["disk: df -h", "Uptime: uptime -p"]);
    let refused = backend
        .save_script(SavedScriptInput {
            id: None,
            name: " ".into(),
            body: String::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(refused.status, 400);
    assert!(backend.delete_script(&saved.id).await.unwrap());
    assert!(!backend.delete_script(&saved.id).await.unwrap());
    sandbox.stop().await;
}

// --- runs over SSH exec ------------------------------------------------------------------------

#[tokio::test]
async fn runs_a_script_with_sh_and_reports_output_and_exit_code() {
    let server = start_sshd(shell_spec()).await;
    let sandbox = lab(&[&server]).await;
    let backend = &sandbox.backend;
    let (_dial, conn_id) = connected(backend).await;
    let id = run(
        backend,
        &conn_id,
        "echo \"hello $((1 + 2))\"\necho oops >&2\nexit 3",
        None,
    )
    .await;
    let status = finished(backend, &id).await;
    assert_eq!(status.state, ScriptRunState::Failed);
    assert_eq!(status.exit_code, Some(3));
    assert!(status.output.contains("hello 3"), "{status:?}");
    assert!(status.output.contains("oops"), "{status:?}");
    assert_eq!(script_leases(backend, &conn_id).await, 0);
    // The script itself never appears in the command line.
    let last = server.log.lock().unwrap().execs.last().cloned();
    assert_eq!(last, Some(script_command()));
    sandbox.stop().await;
}

#[tokio::test]
async fn honours_a_shebang_line_and_gives_the_script_an_empty_stdin() {
    let server = start_sshd(shell_spec()).await;
    let sandbox = lab(&[&server]).await;
    let backend = &sandbox.backend;
    let (_dial, conn_id) = connected(backend).await;
    let id = run(
        backend,
        &conn_id,
        "#!/bin/sh -e\nread line || echo \"stdin is empty\"\nfalse\necho \"not reached\"\n",
        None,
    )
    .await;
    let status = finished(backend, &id).await;
    assert!(status.output.contains("stdin is empty"), "{status:?}");
    assert!(!status.output.contains("not reached"), "{status:?}");
    assert_eq!(status.state, ScriptRunState::Failed);
    sandbox.stop().await;
}

#[tokio::test]
async fn succeeds_on_exit_0_and_returns_only_new_output_when_read_from_an_offset() {
    let server = start_sshd(shell_spec()).await;
    let sandbox = lab(&[&server]).await;
    let backend = &sandbox.backend;
    let (_dial, conn_id) = connected(backend).await;
    let id = run(backend, &conn_id, "printf one\nprintf two", None).await;
    let status = finished(backend, &id).await;
    assert_eq!(status.state, ScriptRunState::Completed);
    assert_eq!(status.exit_code, Some(0));
    assert_eq!(status.output, "onetwo");
    assert_eq!(
        backend.script_run_status(&id, 3).await.unwrap().output,
        "two"
    );
    assert_eq!(
        backend
            .script_run_status(&id, status.offset)
            .await
            .unwrap()
            .output,
        ""
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn stops_a_run_that_outlives_its_timeout_and_one_that_is_cancelled() {
    let server = start_sshd(shell_spec()).await;
    let sandbox = lab(&[&server]).await;
    let backend = &sandbox.backend;
    let (_dial, conn_id) = connected(backend).await;
    let slow = run(backend, &conn_id, "sleep 30", Some(1)).await;
    let status = finished(backend, &slow).await;
    assert_eq!(status.state, ScriptRunState::TimedOut);
    assert_eq!(status.error.as_deref(), Some("Stopped after 1 s."));
    let other = run(backend, &conn_id, "sleep 30", None).await;
    backend.cancel_script_run(&other).await.unwrap();
    assert_eq!(
        backend.script_run_status(&other, 0).await.unwrap().state,
        ScriptRunState::Cancelled
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn stops_the_script_itself_on_cancel_and_timeout_and_removes_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = shell_spec();
    spec.exec_env = vec![("TMPDIR".into(), format!("{}/", dir.path().display()))];
    let server = start_sshd(spec).await;
    let sandbox = lab(&[&server]).await;
    let backend = &sandbox.backend;
    let (_dial, conn_id) = connected(backend).await;
    // A script that would leave a trace if it ran on after the stop.
    let marker = dir.path().join("still-running");
    let cancelled = run(
        backend,
        &conn_id,
        &format!("sleep 1.5; touch {}", marker.display()),
        None,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    backend.cancel_script_run(&cancelled).await.unwrap();
    let timed_out = run(
        backend,
        &conn_id,
        &format!("sleep 2.5; touch {}", marker.display()),
        Some(1),
    )
    .await;
    finished(backend, &timed_out).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;

    assert!(!marker.exists());
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(script_leases(backend, &conn_id).await, 0);
    sandbox.stop().await;
}

#[tokio::test]
async fn says_so_when_the_host_has_no_posix_shell_to_start_the_script() {
    // As cmd.exe would: the wrapper never runs, so no marker arrives.
    let mut spec = ServerSpec::new()
        .password("pw")
        .methods(&[russh::MethodKind::Password]);
    spec.exec_handler = Some(Arc::new(|_, _| {
        Some((
            "'sh' n'est pas reconnu en tant que commande interne\r\n".into(),
            1,
        ))
    }));
    let server = start_sshd(spec).await;
    let sandbox = lab(&[&server]).await;
    let backend = &sandbox.backend;
    let (_dial, conn_id) = connected(backend).await;
    let id = run(backend, &conn_id, "uname -a", None).await;
    let status = finished(backend, &id).await;
    assert_eq!(status.state, ScriptRunState::Failed);
    assert!(
        status
            .error
            .as_deref()
            .unwrap_or("")
            .contains("no POSIX shell"),
        "{status:?}"
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn still_runs_when_tmpdir_points_nowhere() {
    let mut spec = shell_spec();
    spec.exec_env = vec![("TMPDIR".into(), "/nonexistent/muxus-test".into())];
    let server = start_sshd(spec).await;
    let sandbox = lab(&[&server]).await;
    let backend = &sandbox.backend;
    let (_dial, conn_id) = connected(backend).await;
    let id = run(backend, &conn_id, "echo fine", None).await;
    let status = finished(backend, &id).await;
    assert_eq!(status.state, ScriptRunState::Completed, "{status:?}");
    assert_eq!(status.output, "fine\n");
    sandbox.stop().await;
}

#[tokio::test]
async fn keeps_its_own_job_marker_out_of_the_output() {
    let server = start_sshd(shell_spec()).await;
    let sandbox = lab(&[&server]).await;
    let backend = &sandbox.backend;
    let (_dial, conn_id) = connected(backend).await;
    let id = run(backend, &conn_id, "echo out\necho err >&2", None).await;
    let status = finished(backend, &id).await;
    assert!(status.output.contains("out"));
    assert!(status.output.contains("err"));
    assert!(!status.output.contains("muxus-job"), "{status:?}");
    sandbox.stop().await;
}

#[tokio::test]
async fn refuses_a_connection_that_is_gone() {
    let sandbox = start().await;
    let error = sandbox
        .backend
        .start_script_run(ScriptRunRequest {
            conn_id: "nope".into(),
            body: "true".into(),
            timeout_seconds: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error.status, 404);
    assert!(error.message.contains("gone"), "{error:?}");
    sandbox.stop().await;
}

// --- jobs --------------------------------------------------------------------------------------

fn target(alias: &str) -> ScriptTarget {
    ScriptTarget {
        key: format!("ssh:{alias}"),
        label: alias.to_uppercase(),
        profile: SshProfile::new(alias),
    }
}

/// What a job reported, per host.
#[derive(Default, Debug)]
struct Seen {
    states: HashMap<String, Vec<HostRunState>>,
    exit_codes: HashMap<String, i32>,
    errors: HashMap<String, String>,
    output: HashMap<String, String>,
    prompts: Vec<(String, String)>,
    /// Questions left unanswered: like the window's queue, they hold on to their reply.
    waiting: Vec<async_channel::Sender<ScriptPromptAnswer>>,
    withdrawn: usize,
}

/// Read a job to its end, answering its questions with `answer`.
async fn follow(
    job: &ScriptJobHandle,
    mut answer: impl FnMut(&ScriptPrompt, &mut Seen) -> Option<ScriptPromptAnswer>,
) -> Seen {
    let mut seen = Seen::default();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), job.next_event())
            .await
            .expect("a job event in time")
            .expect("the job reports until it finishes");
        match event {
            ScriptJobEvent::Host {
                key,
                state,
                exit_code,
                error,
                ..
            } => {
                seen.states.entry(key.clone()).or_default().push(state);
                if let Some(code) = exit_code {
                    seen.exit_codes.insert(key.clone(), code);
                }
                if let Some(error) = error {
                    seen.errors.insert(key, error);
                }
            }
            ScriptJobEvent::Output { key, text } => {
                seen.output.entry(key).or_default().push_str(&text)
            }
            ScriptJobEvent::Prompt(request) => {
                let name = match &request.prompt {
                    ScriptPrompt::Auth(info) => info.name.clone().unwrap_or_default(),
                    ScriptPrompt::HostKey(_) => "host key".into(),
                };
                seen.prompts.push((request.host.clone(), name));
                match answer(&request.prompt, &mut seen) {
                    Some(reply) => {
                        let _ = request.reply.try_send(reply);
                    }
                    None => seen.waiting.push(request.reply),
                }
            }
            ScriptJobEvent::PromptWithdrawn { .. } => seen.withdrawn += 1,
            ScriptJobEvent::Finished { .. } => return seen,
        }
    }
}

fn sign_in(prompt: &ScriptPrompt, _: &mut Seen) -> Option<ScriptPromptAnswer> {
    Some(match prompt {
        ScriptPrompt::Auth(_) => ScriptPromptAnswer::Auth(Some(password())),
        ScriptPrompt::HostKey(_) => ScriptPromptAnswer::HostKey(true),
    })
}

#[tokio::test]
async fn a_job_signs_in_to_each_host_and_streams_what_each_printed() {
    let one = start_sshd(shell_spec()).await;
    let two = start_sshd(shell_spec()).await;
    let sandbox = lab(&[&one, &two]).await;
    let job = sandbox
        .backend
        .start_script_job(ScriptJobRequest {
            body: "echo \"on $(echo host)\"\nexit 0\n".into(),
            targets: vec![target("lab"), target("lab2")],
            mode: RunMode::Parallel,
            stop_on_failure: false,
            timeout_seconds: 0,
        })
        .unwrap();
    let seen = follow(&job, sign_in).await;
    for key in ["ssh:lab", "ssh:lab2"] {
        assert_eq!(
            seen.states[key],
            [
                HostRunState::Connecting,
                HostRunState::Running,
                HostRunState::Completed
            ],
            "{seen:?}"
        );
        assert_eq!(seen.output[key], "on host\n");
        assert_eq!(seen.exit_codes[key], 0);
    }
    // Each question says which host asks.
    assert!(
        seen.prompts
            .contains(&("ssh:lab2".into(), "Sign in to LAB2".into())),
        "{:?}",
        seen.prompts
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn one_by_one_a_failed_host_stops_the_rest_when_asked_to() {
    let server = start_sshd(shell_spec()).await;
    let sandbox = lab(&[&server]).await;
    sandbox.write_ssh_config(&[
        "Host lab lab2",
        "  HostName 127.0.0.1",
        "  User tester",
        &format!("  Port {}", server.port()),
        "Host nowhere",
        "  HostName 127.0.0.1",
        "  Port 1",
    ]);
    let job = sandbox
        .backend
        .start_script_job(ScriptJobRequest {
            body: "exit 4".into(),
            targets: vec![target("nowhere"), target("lab"), target("lab2")],
            mode: RunMode::Sequential,
            stop_on_failure: true,
            timeout_seconds: 0,
        })
        .unwrap();
    let seen = follow(&job, sign_in).await;
    assert_eq!(
        seen.states["ssh:nowhere"],
        [HostRunState::Connecting, HostRunState::Failed]
    );
    assert!(!seen.errors["ssh:nowhere"].is_empty());
    assert_eq!(seen.states["ssh:lab"], [HostRunState::Skipped]);
    assert_eq!(seen.states["ssh:lab2"], [HostRunState::Skipped]);

    // Without stopping, every host runs; a script's own failure is the host's.
    let job = sandbox
        .backend
        .start_script_job(ScriptJobRequest {
            body: "exit 4".into(),
            targets: vec![target("nowhere"), target("lab")],
            mode: RunMode::Batches(1),
            stop_on_failure: false,
            timeout_seconds: 0,
        })
        .unwrap();
    let seen = follow(&job, sign_in).await;
    assert_eq!(seen.states["ssh:lab"].last(), Some(&HostRunState::Failed));
    assert_eq!(seen.exit_codes["ssh:lab"], 4);
    sandbox.stop().await;
}

#[tokio::test]
async fn cancelling_takes_back_waiting_questions_and_stops_running_scripts() {
    let server = start_sshd(shell_spec()).await;
    let sandbox = lab(&[&server]).await;
    let backend = &sandbox.backend;
    // `lab` is connected already: its run starts without a question. `lab2` is the same server
    // under another name, so it asks again.
    sandbox.write_ssh_config(&[
        "Host lab",
        "  HostName 127.0.0.1",
        "  User tester",
        &format!("  Port {}", server.port()),
        "Host lab2",
        "  HostName localhost",
        "  User other",
        &format!("  Port {}", server.port()),
    ]);
    let (_dial, _) = connected(backend).await;
    let job = Arc::new(
        backend
            .start_script_job(ScriptJobRequest {
                body: "echo started\nsleep 30".into(),
                targets: vec![target("lab"), target("lab2")],
                mode: RunMode::Parallel,
                stop_on_failure: false,
                timeout_seconds: 0,
            })
            .unwrap(),
    );
    let canceller = job.clone();
    let seen = follow(&job, move |prompt, _| {
        if let ScriptPrompt::HostKey(_) = prompt {
            return Some(ScriptPromptAnswer::HostKey(true));
        }
        // The password question stays unanswered; `lab` runs meanwhile.
        let job = canceller.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            job.cancel();
        });
        None
    })
    .await;
    assert!(seen.prompts.iter().any(|(host, _)| host == "ssh:lab2"));
    assert_eq!(seen.output["ssh:lab"], "started\n");
    assert_eq!(seen.withdrawn, 1, "{seen:?}");
    assert_eq!(
        seen.states["ssh:lab"].last(),
        Some(&HostRunState::Cancelled),
        "{seen:?}"
    );
    assert_eq!(
        seen.states["ssh:lab2"].last(),
        Some(&HostRunState::Cancelled),
        "{seen:?}"
    );
    assert!(!seen.errors.contains_key("ssh:lab2"));
    sandbox.stop().await;
}

#[tokio::test]
async fn refuses_an_empty_script_or_a_time_limit_past_a_week() {
    let sandbox = start().await;
    let request = |body: &str, timeout_seconds| ScriptJobRequest {
        body: body.into(),
        targets: vec![target("lab")],
        mode: RunMode::Parallel,
        stop_on_failure: false,
        timeout_seconds,
    };
    assert!(sandbox.backend.start_script_job(request("", 0)).is_err());
    assert!(
        sandbox
            .backend
            .start_script_job(request("true", 8 * 24 * 3600))
            .is_err()
    );
    sandbox.stop().await;
}
