//! A host set to "Use system OpenSSH" through `Backend::open_session` (port of the system
//! OpenSSH case of `terminal-socket-dial.test.ts`): the terminal is the computer's `ssh` under
//! a PTY, and the file browser and the status bar share its login.

mod support;

use std::time::Duration;

use muxus_core::api::{AntiIdle, HostStatsResponse, OpenSshMetadataPatch};
use muxus_core::js::Patch;
use muxus_core::protocol::{ExitReason, PasteProgressState, TerminalServerMessage};
use muxus_session::SessionEvent;
use muxus_ssh::testing::server::{ServerSpec, start as start_sshd};
use serde_json::json;
use support::{body, events_until, output_until, start_in};

/// The `ssh` program, when this computer has one that runs.
fn ssh_runs() -> bool {
    !cfg!(windows)
        && std::process::Command::new("ssh")
            .arg("-V")
            .output()
            .is_ok_and(|out| out.status.success())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_a_host_set_to_system_openssh_through_the_ssh_program() {
    if !ssh_runs() {
        return;
    }
    let mut spec = ServerSpec::new();
    spec.accept_none = true;
    spec.greeting = "hello from the shell\n".into();
    let files = tempfile::tempdir().unwrap();
    spec.sftp_root = Some(files.path().to_path_buf());
    let server = start_sshd(spec).await;

    // The `ssh` program reads the sandbox's config, never the real one.
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("home").join(".ssh").join("config");
    let args = vec!["-F".to_string(), config.display().to_string()];
    let sandbox = start_in(dir, |options| options.system_openssh_args = args).await;
    let port = format!("  Port {}", server.port());
    let no_key = format!("  IdentityFile {}", sandbox.home().join("no-key").display());
    sandbox.write_ssh_config(&[
        "Host krb",
        "  HostName 127.0.0.1",
        "  User tester",
        port.as_str(),
        "  StrictHostKeyChecking no",
        "  UserKnownHostsFile /dev/null",
        "  GlobalKnownHostsFile /dev/null",
        "  IdentityAgent none",
        "  IdentitiesOnly yes",
        no_key.as_str(),
        "  GSSAPIAuthentication no",
        "  LogLevel ERROR",
    ]);
    let backend = &sandbox.backend;
    backend
        .patch_ssh_host_metadata(
            "krb",
            OpenSshMetadataPatch {
                system_open_ssh: Some(true),
                anti_idle: Patch::Set(AntiIdle {
                    interval_seconds: 10,
                    text: "still here".into(),
                    enter: true,
                }),
                ..OpenSshMetadataPatch::default()
            },
        )
        .await
        .unwrap();

    let handle = backend.open_session(body(json!({
        "op": "connect",
        "profile": { "kind": "ssh", "target": "krb" },
        "cols": 100,
        "rows": 30,
    })));
    let events = events_until(&handle, |event| {
        matches!(
            event,
            SessionEvent::Message(TerminalServerMessage::Ready { .. })
                | SessionEvent::Message(TerminalServerMessage::Exit { .. })
        )
    })
    .await;
    let Some(SessionEvent::Message(TerminalServerMessage::Ready {
        conn_id,
        sftp_available,
        system_open_ssh,
        summary,
        ..
    })) = events.last().cloned()
    else {
        panic!("no ready: {events:?}");
    };
    assert!(conn_id.starts_with("system-ssh-"), "{conn_id}");
    assert_eq!(sftp_available, Some(true));
    assert_eq!(system_open_ssh, Some(true));
    assert!(summary.is_none());
    output_until(&handle, "hello from the shell").await;
    // A pty was asked for, so the fake shell echoes what is typed.
    handle.send_data(b"typed text\r".to_vec());
    output_until(&handle, "typed text").await;

    // A paste that waits for the prompt reads the answer to each line. The echoing shell shows
    // no prompt, so the empty line after each echo stands in for one; the echo of the second
    // line is the error that stops the paste.
    handle.send(body(json!({
        "op": "paste", "text": "first\nbogus\nthird\n",
        "bracketed": false, "lineDelayMs": 0, "charDelayMs": 0, "enterKey": "lf",
        "prompt": { "promptPattern": "^$", "timeoutMs": 3000, "errorPattern": "^bogus" },
    })));
    let pasted = events_until(&handle, |event| {
        matches!(
            event,
            SessionEvent::Message(TerminalServerMessage::PasteProgress {
                state: PasteProgressState::Stopped | PasteProgressState::Done,
                ..
            })
        )
    })
    .await;
    assert!(
        matches!(
            pasted.last(),
            Some(SessionEvent::Message(
                TerminalServerMessage::PasteProgress {
                    state: PasteProgressState::Stopped,
                    line: 2,
                    ..
                }
            ))
        ),
        "{pasted:?}"
    );

    // The host's anti-idle text is typed a full interval (10 s) after the last input, so the
    // wait outlasts the helpers' 10 s for one event.
    let anti_idle = tokio::time::timeout(Duration::from_secs(25), async {
        let mut text = String::new();
        while let Some(event) = handle.next_event().await {
            if let SessionEvent::Data(data) = event {
                text.push_str(&String::from_utf8_lossy(&data));
                if text.contains("still here") {
                    return true;
                }
            }
        }
        false
    })
    .await;
    assert_eq!(anti_idle, Ok(true), "no anti-idle text");

    // The file browser shares the terminal's login.
    let home = backend.sftp_home(&conn_id).await.unwrap();
    assert!(!home.path.is_empty());
    // The fake host answers commands without the probe's sum: no statistics, no extra channel
    // on the terminal's connection.
    assert!(matches!(
        backend.ssh_host_stats(&conn_id).await.unwrap(),
        HostStatsResponse::Unsupported { .. }
    ));
    // Muxus cannot open a connection of its own to the host.
    let dial = backend.open_session(body(json!({
        "op": "dial",
        "profile": { "kind": "ssh", "target": "krb" },
    })));
    let dialed = events_until(&dial, |event| {
        matches!(
            event,
            SessionEvent::Message(TerminalServerMessage::Exit { .. })
        )
    })
    .await;
    assert!(
        matches!(
            dialed.last(),
            Some(SessionEvent::Message(TerminalServerMessage::Exit { message: Some(message), reason: ExitReason::Failed, .. }))
                if message.contains("uses system OpenSSH")
        ),
        "{dialed:?}"
    );

    // The fake shell ends with status 3 on `exit`.
    handle.send_data(b"exit\r".to_vec());
    let ended = events_until(&handle, |event| {
        matches!(
            event,
            SessionEvent::Message(TerminalServerMessage::Exit { .. })
        )
    })
    .await;
    assert!(
        matches!(
            ended.last(),
            Some(SessionEvent::Message(TerminalServerMessage::Exit {
                code: Some(3),
                reason: ExitReason::Completed,
                ..
            }))
        ),
        "{ended:?}"
    );
    sandbox.stop().await;
}
