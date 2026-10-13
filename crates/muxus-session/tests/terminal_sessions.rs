//! Terminal sessions through `Backend::open_session`: ports of `terminal-socket-break`,
//! `terminal-socket-dial`, `terminal-socket-paste` and `login-sequence-ssh`, plus end-to-end
//! SSH (against the in-process test server of `muxus_ssh::testing`), local and Telnet sessions.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_core::api::SessionEndStatus;
use muxus_core::protocol::{
    AuthPromptResponse, ExitReason, LoginSequenceState, PasteProgressState, TerminalClientMessage,
    TerminalServerMessage,
};
use muxus_session::{SessionEvent, SessionHandle};
use muxus_ssh::testing::server::{ServerSpec, TestServer, start as start_sshd};
use muxus_store::history::SessionHistoryQuery;
use serde_json::json;
use support::{Sandbox, body, events_until, next_event, output_until, start};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn connect(profile: serde_json::Value) -> TerminalClientMessage {
    body(json!({ "op": "connect", "profile": profile, "cols": 80, "rows": 24 }))
}

fn message(event: &SessionEvent) -> Option<&TerminalServerMessage> {
    match event {
        SessionEvent::Message(message) => Some(message),
        _ => None,
    }
}

fn is_ready(event: &SessionEvent) -> bool {
    matches!(message(event), Some(TerminalServerMessage::Ready { .. }))
}

/// Skip output still in flight until the source window's `transfer-ready`.
async fn until_transfer_ready(handle: &SessionHandle) {
    events_until(handle, |event| {
        matches!(message(event), Some(TerminalServerMessage::TransferReady))
    })
    .await;
}

fn is_exit(event: &SessionEvent) -> bool {
    matches!(message(event), Some(TerminalServerMessage::Exit { .. }))
}

fn exit_of(events: &[SessionEvent]) -> (Option<i32>, Option<String>, ExitReason) {
    events
        .iter()
        .find_map(|event| match message(event) {
            Some(TerminalServerMessage::Exit {
                code,
                message,
                reason,
            }) => Some((*code, message.clone(), *reason)),
            _ => None,
        })
        .expect("an exit")
}

/// A Telnet console on loopback: greets with `router> `, echoes every line, and keeps every
/// byte it received.
struct Console {
    port: u16,
    received: Arc<Mutex<Vec<u8>>>,
}

impl Console {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let received: Arc<Mutex<Vec<u8>>> = Arc::default();
        let sink = received.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let sink = sink.clone();
                tokio::spawn(async move {
                    let _ = socket.write_all(b"router> ").await;
                    let mut buffer = [0u8; 4096];
                    while let Ok(read) = socket.read(&mut buffer).await {
                        if read == 0 {
                            break;
                        }
                        sink.lock().unwrap().extend_from_slice(&buffer[..read]);
                        let printable: Vec<u8> = buffer[..read]
                            .iter()
                            .copied()
                            .filter(|byte| *byte != 0xff && *byte != 0xf3)
                            .collect();
                        if socket.write_all(&printable).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        Self { port, received }
    }

    fn received(&self) -> Vec<u8> {
        self.received.lock().unwrap().clone()
    }

    async fn wait_for(&self, needle: &[u8]) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self
                .received()
                .windows(needle.len())
                .any(|window| window == needle)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the console did not receive {:?}; it got {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&self.received())
            )
        });
    }

    fn profile(&self) -> serde_json::Value {
        json!({ "kind": "telnet", "host": "127.0.0.1", "port": self.port })
    }
}

/// An SSH server that takes the password `pw`, and a sandbox whose ssh_config names it `lab`.
async fn ssh_lab(extra: &[&str]) -> (Sandbox, TestServer) {
    let server = start_sshd(
        ServerSpec::new()
            .password("pw")
            .methods(&[russh::MethodKind::Password]),
    )
    .await;
    let sandbox = start().await;
    let port = format!("  Port {}", server.port());
    let mut lines = vec![
        "Host lab",
        "  HostName 127.0.0.1",
        "  User tester",
        port.as_str(),
    ];
    lines.extend_from_slice(extra);
    sandbox.write_ssh_config(&lines);
    (sandbox, server)
}

/// Answer host-key and password prompts until the session is ready.
async fn sign_in(handle: &SessionHandle) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    loop {
        let event = next_event(handle).await;
        match message(&event) {
            Some(TerminalServerMessage::HostKey(_)) => {
                handle.send(TerminalClientMessage::HostKeyResponse { accept: true })
            }
            Some(TerminalServerMessage::AuthPrompt(_)) => {
                handle.send(TerminalClientMessage::AuthResponse(AuthPromptResponse {
                    answers: vec!["pw".into()],
                    remember_password: None,
                    skipped: None,
                }))
            }
            Some(TerminalServerMessage::Exit { message, .. }) => {
                panic!("the session ended while connecting: {message:?}")
            }
            _ => {}
        }
        let ready = is_ready(&event);
        events.push(event);
        if ready {
            return events;
        }
    }
}

// --- local -------------------------------------------------------------------------------------

#[tokio::test]
async fn a_local_shell_runs_commands_and_reports_its_exit() {
    let sandbox = start().await;
    let handle = sandbox
        .backend
        .open_session(connect(json!({ "kind": "local", "shell": "/bin/sh" })));
    let first = next_event(&handle).await;
    let terminal_id = match message(&first) {
        Some(TerminalServerMessage::Session { terminal_id }) => terminal_id.clone(),
        other => panic!("expected the session id first, got {other:?}"),
    };
    assert!(terminal_id.starts_with("terminal-"));
    assert_eq!(handle.terminal_id(), terminal_id);
    let setup = events_until(&handle, is_ready).await;
    assert!(matches!(
        message(&setup[0]),
        Some(TerminalServerMessage::LoggingState { enabled: false, .. })
    ));
    match message(setup.last().unwrap()) {
        Some(TerminalServerMessage::Ready {
            conn_id, summary, ..
        }) => {
            assert_eq!(conn_id, &format!("local-{}", std::process::id()));
            assert!(summary.is_none());
        }
        _ => unreachable!(),
    }
    assert!(sandbox.backend.terminal_ids().contains(&terminal_id));
    handle.send_data(b"echo hello-$((40+2))\r".to_vec());
    output_until(&handle, "hello-42").await;
    handle.send_data(b"exit 7\r".to_vec());
    let end = events_until(&handle, |event| {
        matches!(event, SessionEvent::Closed { .. })
    })
    .await;
    assert_eq!(exit_of(&end), (Some(7), None, ExitReason::Completed));
    assert!(sandbox.backend.terminal_ids().is_empty());
    sandbox.stop().await;
}

#[tokio::test]
async fn a_first_message_other_than_connect_dial_or_attach_ends_the_handle() {
    let sandbox = start().await;
    let handle = sandbox
        .backend
        .open_session(TerminalClientMessage::Resize { cols: 80, rows: 24 });
    assert_eq!(
        next_event(&handle).await,
        SessionEvent::Closed {
            reason: Some("expected connect or attach".into())
        }
    );
    let gone = sandbox.backend.attach_session("terminal-missing", 80, 24);
    let events = events_until(&gone, |event| matches!(event, SessionEvent::Closed { .. })).await;
    assert_eq!(
        exit_of(&events),
        (
            Some(1),
            Some("The terminal session is no longer available.".into()),
            ExitReason::Disconnected
        )
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn a_failed_setup_is_an_exit_with_reason_failed() {
    let sandbox = start().await;
    let handle = sandbox.backend.open_session(connect(
        json!({ "kind": "ssh", "target": "gone", "profileId": "missing" }),
    ));
    let events = events_until(&handle, |event| {
        matches!(event, SessionEvent::Closed { .. })
    })
    .await;
    assert_eq!(
        exit_of(&events),
        (
            Some(1),
            Some("saved SSH profile \"missing\" was not found".into()),
            ExitReason::Failed
        )
    );
    sandbox.stop().await;
}

// --- telnet ------------------------------------------------------------------------------------

#[tokio::test]
async fn a_telnet_session_exchanges_bytes_and_puts_iac_brk_on_the_wire() {
    let sandbox = start().await;
    let console = Console::start().await;
    let handle = sandbox.backend.open_session(connect(console.profile()));
    let setup = events_until(&handle, is_ready).await;
    assert!(setup.iter().any(|event| matches!(
        message(event),
        Some(TerminalServerMessage::Status { message, transient: Some(true) })
            if message == &format!("Connecting to 127.0.0.1:{} over Telnet …", console.port)
    )));
    match message(setup.last().unwrap()) {
        Some(TerminalServerMessage::Ready { conn_id, .. }) => {
            assert!(conn_id.starts_with("telnet-"))
        }
        _ => unreachable!(),
    }
    output_until(&handle, "router> ").await;
    handle.send_data(b"show version\r".to_vec());
    console.wait_for(b"show version\r").await;
    output_until(&handle, "show version").await;

    handle.send(TerminalClientMessage::SendBreak);
    console.wait_for(&[0xff, 0xf3]).await;
    let answer = events_until(&handle, |event| {
        matches!(
            message(event),
            Some(TerminalServerMessage::BreakResult { .. })
        )
    })
    .await;
    assert_eq!(
        message(answer.last().unwrap()),
        Some(&TerminalServerMessage::BreakResult {
            ok: true,
            message: None
        })
    );
    handle.close();
    sandbox.stop().await;
}

#[tokio::test]
async fn paced_pastes_wait_for_the_telnet_socket_and_report_progress() {
    let sandbox = start().await;
    let console = Console::start().await;
    let handle = sandbox.backend.open_session(connect(console.profile()));
    events_until(&handle, is_ready).await;
    handle.send(body(json!({
        "op": "paste", "text": "interface ge-0/0/1\ndescription uplink\n",
        "bracketed": false, "lineDelayMs": 50, "charDelayMs": 0,
    })));
    let events = events_until(&handle, |event| {
        matches!(
            message(event),
            Some(TerminalServerMessage::PasteProgress {
                state: PasteProgressState::Done,
                ..
            })
        )
    })
    .await;
    assert!(events.iter().any(|event| matches!(
        message(event),
        Some(TerminalServerMessage::PasteProgress {
            state: PasteProgressState::Running,
            line: 2,
            lines: 2,
            ..
        })
    )));
    console
        .wait_for(b"interface ge-0/0/1\r\ndescription uplink\r\n")
        .await;
    handle.close();
    sandbox.stop().await;
}

#[tokio::test]
async fn vault_secrets_are_typed_into_live_sessions_without_reaching_the_logs() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    backend.create_vault("master-pass-12", None).await.unwrap();
    let secret = backend
        .create_vault_secret(body(json!({
            "name": "Enable", "value": "s3cret", "masterPassword": "master-pass-12",
        })))
        .await
        .unwrap()
        .secret;
    let console = Console::start().await;
    let handle = backend.open_session(connect(console.profile()));
    events_until(&handle, is_ready).await;
    let sent = backend
        .send_vault_secret(
            &secret.id,
            body(json!({ "terminalIds": [handle.terminal_id(), "terminal-gone"], "enter": true })),
        )
        .await
        .unwrap();
    assert_eq!(sent.sent, 1);
    console.wait_for(b"s3cret\r").await;
    let unavailable = backend
        .send_vault_secret(
            &secret.id,
            body(json!({ "terminalIds": ["terminal-gone"], "enter": false })),
        )
        .await
        .unwrap_err();
    assert_eq!(unavailable.code.as_deref(), Some("terminal-unavailable"));
    handle.close();
    sandbox.stop().await;
}

#[tokio::test]
async fn session_logging_records_the_session_into_history() {
    let sandbox = start().await;
    let console = Console::start().await;
    let handle = sandbox.backend.open_session(connect(console.profile()));
    events_until(&handle, is_ready).await;
    handle.send(body(json!({ "op": "set-logging", "enabled": true })));
    let state = events_until(&handle, |event| {
        matches!(
            message(event),
            Some(TerminalServerMessage::LoggingState { enabled: true, .. })
        )
    })
    .await;
    let session_id = match message(state.last().unwrap()) {
        Some(TerminalServerMessage::LoggingState { session_id, .. }) => session_id.clone().unwrap(),
        _ => unreachable!(),
    };
    handle.send(body(json!({ "op": "set-title", "title": "Core switch" })));
    handle.send_data(b"show clock\r".to_vec());
    output_until(&handle, "show clock").await;
    handle.close();
    // The record finishes as disconnected (the tab closed) and holds the echoed command.
    let found = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let page = sandbox
                .backend
                .session_history(SessionHistoryQuery {
                    query: Some("clock".into()),
                    ..SessionHistoryQuery::default()
                })
                .await
                .unwrap();
            if let Some(summary) = page.sessions.into_iter().find(|s| s.id == session_id)
                && summary.status != muxus_core::api::SessionLogStatus::Active
            {
                return summary;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(found.title, "Core switch");
    assert_eq!(found.status, SessionEndStatus::Disconnected.into());
    sandbox.stop().await;
}

// --- moving a session between windows ------------------------------------------------------------

#[tokio::test]
async fn handoff_output_is_held_until_attach_and_released_on_cancel() {
    let sandbox = start().await;
    let console = Console::start().await;
    let source = sandbox.backend.open_session(connect(console.profile()));
    events_until(&source, is_ready).await;
    output_until(&source, "router> ").await;

    source.send(TerminalClientMessage::PrepareTransfer);
    until_transfer_ready(&source).await;
    source.send_data(b"held\r".to_vec());
    console.wait_for(b"held\r").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        source.try_next_event(),
        None,
        "output is held while a transfer is prepared"
    );
    source.send(TerminalClientMessage::CancelTransfer);
    output_until(&source, "held").await;

    // Let the rest of the echo arrive before the next handoff starts.
    tokio::time::sleep(Duration::from_millis(100)).await;
    while source.try_next_event().is_some() {}
    source.send(TerminalClientMessage::PrepareTransfer);
    until_transfer_ready(&source).await;
    source.send_data(b"moved\r".to_vec());
    console.wait_for(b"moved\r").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let destination = sandbox
        .backend
        .attach_session(source.terminal_id(), 120, 40);
    assert_eq!(
        next_event(&source).await,
        SessionEvent::Closed {
            reason: Some("terminal transferred".into())
        }
    );
    // The destination first learns the session's state, then gets the held output.
    let replay = events_until(&destination, |event| matches!(event, SessionEvent::Data(_))).await;
    let ops: Vec<&TerminalServerMessage> = replay.iter().filter_map(message).collect();
    assert!(matches!(ops[0], TerminalServerMessage::Session { .. }));
    assert!(
        ops.iter()
            .any(|m| matches!(m, TerminalServerMessage::Ready { .. }))
    );
    assert!(
        ops.iter()
            .any(|m| matches!(m, TerminalServerMessage::LoggingState { .. }))
    );
    assert!(
        !ops.iter()
            .any(|m| matches!(m, TerminalServerMessage::Status { .. }))
    );
    match replay.last().unwrap() {
        SessionEvent::Data(data) => assert!(String::from_utf8_lossy(data).contains("moved")),
        _ => unreachable!(),
    }
    // The old window's input goes nowhere; the new one's reaches the console.
    source.send_data(b"stale\r".to_vec());
    destination.send_data(b"fresh\r".to_vec());
    console.wait_for(b"fresh\r").await;
    assert!(!String::from_utf8_lossy(&console.received()).contains("stale"));
    destination.close();
    sandbox.stop().await;
}

#[tokio::test]
async fn a_session_survives_an_unexpected_window_loss_for_the_grace_period() {
    let sandbox =
        support::start_with(|options| options.reattach_grace = Duration::from_millis(300)).await;
    let console = Console::start().await;
    let first = sandbox.backend.open_session(connect(console.profile()));
    events_until(&first, is_ready).await;
    let terminal_id = first.terminal_id().to_string();
    drop(first);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = sandbox.backend.attach_session(&terminal_id, 80, 24);
    events_until(&second, is_ready).await;
    second.send_data(b"after\r".to_vec());
    console.wait_for(b"after\r").await;

    second.detach();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let late = sandbox.backend.attach_session(&terminal_id, 80, 24);
    let events = events_until(&late, |event| matches!(event, SessionEvent::Closed { .. })).await;
    assert_eq!(exit_of(&events).2, ExitReason::Disconnected);
    sandbox.stop().await;
}

#[tokio::test]
async fn a_closed_session_ends_at_once() {
    let sandbox = start().await;
    let console = Console::start().await;
    let handle = sandbox.backend.open_session(connect(console.profile()));
    events_until(&handle, is_ready).await;
    let terminal_id = handle.terminal_id().to_string();
    handle.close();
    let closed = events_until(&handle, |event| {
        matches!(event, SessionEvent::Closed { .. })
    })
    .await;
    assert_eq!(closed.last(), Some(&SessionEvent::Closed { reason: None }));
    let late = sandbox.backend.attach_session(&terminal_id, 80, 24);
    let events = events_until(&late, |event| matches!(event, SessionEvent::Closed { .. })).await;
    assert!(is_exit(&events[0]));
    sandbox.stop().await;
}

// --- ssh ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_ssh_session_signs_in_exchanges_bytes_and_reports_its_exit() {
    let (sandbox, server) = ssh_lab(&[]).await;
    let handle = sandbox
        .backend
        .open_session(connect(json!({ "kind": "ssh", "target": "lab" })));
    let setup = sign_in(&handle).await;
    assert!(
        setup
            .iter()
            .any(|event| matches!(message(event), Some(TerminalServerMessage::HostKey(_))))
    );
    assert!(
        setup
            .iter()
            .any(|event| matches!(message(event), Some(TerminalServerMessage::AuthPrompt(_))))
    );
    let (conn_id, summary) = match message(setup.last().unwrap()) {
        Some(TerminalServerMessage::Ready {
            conn_id,
            host,
            user,
            sftp_available,
            summary,
        }) => {
            assert_eq!(host.as_deref(), Some("127.0.0.1"));
            assert_eq!(user.as_deref(), Some("tester"));
            assert_eq!(*sftp_available, Some(true));
            (conn_id.clone(), summary.clone().unwrap())
        }
        _ => unreachable!(),
    };
    assert_eq!(summary.user, "tester");
    assert_eq!(summary.auth_methods, vec!["password".to_string()]);
    assert!(summary.forwards.is_empty());
    assert_eq!(
        sandbox
            .backend
            .connections_list()
            .await
            .unwrap()
            .connections[0]
            .id,
        conn_id
    );
    output_until(&handle, "shell ok").await;
    handle.send_data(b"ls\r".to_vec());
    output_until(&handle, "ls").await;
    handle.send(TerminalClientMessage::Resize {
        cols: 120,
        rows: 40,
    });

    // The host stats command runs over the live transport; this server does not run it.
    let stats = sandbox.backend.ssh_host_stats(&conn_id).await.unwrap();
    assert!(matches!(
        stats,
        muxus_core::api::HostStatsResponse::Unsupported { .. }
    ));
    assert_eq!(
        sandbox
            .backend
            .ssh_host_stats("gone")
            .await
            .unwrap_err()
            .status,
        404
    );

    handle.send(TerminalClientMessage::SendBreak);
    let answer = events_until(&handle, |event| {
        matches!(
            message(event),
            Some(TerminalServerMessage::BreakResult { .. })
        )
    })
    .await;
    assert_eq!(
        message(answer.last().unwrap()),
        Some(&TerminalServerMessage::BreakResult {
            ok: false,
            message: Some(
                "Could not send BREAK: the SSH engine cannot send BREAK requests yet".into()
            )
        })
    );

    handle.send_data(b"exit\r".to_vec());
    let end = events_until(&handle, |event| {
        matches!(event, SessionEvent::Closed { .. })
    })
    .await;
    assert_eq!(exit_of(&end), (Some(3), None, ExitReason::Completed));
    assert!(
        server
            .events()
            .contains(&"window-change 120x40".to_string())
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn a_lost_ssh_transport_ends_the_session_as_disconnected() {
    let (sandbox, server) = ssh_lab(&["  StrictHostKeyChecking accept-new"]).await;
    let handle = sandbox
        .backend
        .open_session(connect(json!({ "kind": "ssh", "target": "lab" })));
    sign_in(&handle).await;
    output_until(&handle, "shell ok").await;
    // A paste in flight stops with the connection.
    handle.send(body(json!({
        "op": "paste", "text": "a\nb\nc\nd\n", "bracketed": false, "lineDelayMs": 2000, "charDelayMs": 0,
    })));
    output_until(&handle, "a").await;
    server.disconnect_all().await;
    let end = events_until(&handle, |event| {
        matches!(event, SessionEvent::Closed { .. })
    })
    .await;
    let (_, message, reason) = exit_of(&end);
    assert_eq!(reason, ExitReason::Disconnected);
    assert!(message.is_some());
    sandbox.stop().await;
}

#[tokio::test]
async fn the_ssh_summary_lists_the_config_forwards_that_started() {
    let blocker = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let taken = blocker.local_addr().unwrap().port();
    let free = {
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        probe.local_addr().unwrap().port()
    };
    let forward = format!("  LocalForward {free} 127.0.0.1:9");
    let conflicting = format!("  DynamicForward {taken}");
    let (sandbox, _server) =
        ssh_lab(&["  StrictHostKeyChecking accept-new", &forward, &conflicting]).await;
    let handle = sandbox
        .backend
        .open_session(connect(json!({ "kind": "ssh", "target": "lab" })));
    let setup = sign_in(&handle).await;
    assert!(setup.iter().any(|event| matches!(
        message(event),
        Some(TerminalServerMessage::Status { message, .. }) if message.starts_with(&format!("forward -D {taken} failed:"))
    )));
    let summary = match message(setup.last().unwrap()) {
        Some(TerminalServerMessage::Ready { summary, .. }) => summary.clone().unwrap(),
        _ => unreachable!(),
    };
    assert_eq!(summary.forwards.len(), 1);
    assert_eq!(summary.forwards[0].bind_port, free);
    assert_eq!(summary.forwards[0].target_port, Some(9));
    assert_eq!(sandbox.backend.forwards(None).await.unwrap().len(), 1);
    // The session's forwards stop with the last terminal on the connection.
    handle.close();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !sandbox.backend.forwards(None).await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    drop(blocker);
    sandbox.stop().await;
}

#[tokio::test]
async fn a_dial_announces_readiness_and_hands_the_transport_to_a_forward() {
    let (sandbox, _server) = ssh_lab(&["  StrictHostKeyChecking accept-new"]).await;
    let dial = sandbox.backend.open_session(body(json!({
        "op": "dial", "profile": { "kind": "ssh", "target": "lab" },
    })));
    let setup = sign_in(&dial).await;
    assert!(!setup.iter().any(|event| matches!(
        message(event),
        Some(TerminalServerMessage::LoggingState { .. })
    )));
    let conn_id = match message(setup.last().unwrap()) {
        Some(TerminalServerMessage::Ready {
            conn_id, summary, ..
        }) => {
            assert!(summary.is_none());
            conn_id.clone()
        }
        _ => unreachable!(),
    };
    let bind_port = {
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        probe.local_addr().unwrap().port()
    };
    let forward = sandbox
        .backend
        .start_forward(body(
            json!({ "connId": conn_id, "type": "dynamic", "bindPort": bind_port }),
        ))
        .await
        .unwrap();
    dial.close();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // The forward's own lease keeps the transport.
    let connections = sandbox
        .backend
        .connections_list()
        .await
        .unwrap()
        .connections;
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0].id, conn_id);
    sandbox.backend.stop_forward(&forward.id).await.unwrap();
    sandbox.stop().await;
}

#[tokio::test]
async fn a_folder_login_sequence_runs_over_the_ssh_shell() {
    let (sandbox, _server) = ssh_lab(&["  StrictHostKeyChecking accept-new"]).await;
    let backend = &sandbox.backend;
    backend
        .patch_ssh_host_metadata("lab", body(json!({ "group": "Lab" })))
        .await
        .unwrap();
    backend
        .put_folder_settings(body(json!({
            "path": "Lab",
            "auth": {},
            "loginSequence": { "steps": [
                { "id": "w", "kind": "wait", "pattern": "shell ok", "timeoutSeconds": 5 },
                { "id": "s", "kind": "send", "text": "terminal length 0", "enter": true },
            ] },
        })))
        .await
        .unwrap();
    let handle = backend.open_session(connect(json!({ "kind": "ssh", "target": "lab" })));
    let setup = sign_in(&handle).await;
    // The tab learns a sequence runs before `ready`.
    assert!(setup.iter().any(|event| matches!(
        message(event),
        Some(TerminalServerMessage::LoginSequence {
            state: LoginSequenceState::Running,
            ..
        })
    )));
    let (mut echoed, mut done) = (String::new(), false);
    while !(done && echoed.contains("terminal length 0")) {
        match next_event(&handle).await {
            SessionEvent::Data(data) => echoed.push_str(&String::from_utf8_lossy(&data)),
            SessionEvent::Message(TerminalServerMessage::LoginSequence { state, .. }) => {
                assert_ne!(state, LoginSequenceState::Failed);
                done |= state == LoginSequenceState::Done;
            }
            SessionEvent::Message(_) => {}
            SessionEvent::Closed { reason } => panic!("closed: {reason:?}"),
        }
    }
    handle.close();
    sandbox.stop().await;
}

#[tokio::test]
async fn saved_tunnels_start_with_the_backend_from_a_vault_password() {
    let server = start_sshd(
        ServerSpec::new()
            .password("pw")
            .methods(&[russh::MethodKind::Password]),
    )
    .await;
    // One credential store across the restart, as the OS store would be.
    let keys: Arc<dyn muxus_store::vault::VaultKeyStore> =
        Arc::new(muxus_store::vault::MemoryVaultKeyStore::new());
    let shared_keys = keys.clone();
    let sandbox =
        support::start_with(move |options| options.vault_key_store = Some(shared_keys)).await;
    sandbox.write_ssh_config(&[
        "Host lab",
        "  HostName 127.0.0.1",
        "  User tester",
        &format!("  Port {}", server.port()),
    ]);
    let backend = &sandbox.backend;
    backend.create_vault("master-pass-12", None).await.unwrap();
    let account = muxus_store::vault::ssh_password_account("tester", "127.0.0.1", server.port());
    backend
        .vault()
        .remember_ssh_password(&account, "tester@127.0.0.1", "pw", None)
        .unwrap();
    // Trust the host the way the forwarding panel's interactive dial does.
    let dial = backend.open_session(body(
        json!({ "op": "dial", "profile": { "kind": "ssh", "target": "lab" } }),
    ));
    sign_in(&dial).await;
    dial.close();
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = echo.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 64];
                while let Ok(read) = socket.read(&mut buffer).await {
                    if read == 0 || socket.write_all(&buffer[..read]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    let bind_port = {
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        probe.local_addr().unwrap().port()
    };
    backend
        .save_tunnel(body(json!({
            "target": "lab", "type": "local", "bindPort": bind_port,
            "targetHost": "127.0.0.1", "targetPort": echo_port, "autoStart": true,
        })))
        .await
        .unwrap();
    let Sandbox { dir, backend } = sandbox;
    backend.shutdown().await;

    let sandbox = support::start_in(dir, move |options| options.vault_key_store = Some(keys)).await;
    let active = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let forwards = sandbox.backend.forwards(None).await.unwrap();
            if let Some(forward) = forwards.first()
                && forward.status == muxus_core::api::ForwardStatus::Active
            {
                return forward.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the tunnel starts with the backend");
    assert_eq!(active.bind_port, bind_port);
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", bind_port))
        .await
        .unwrap();
    socket.write_all(b"through").await.unwrap();
    let mut reply = [0u8; 7];
    socket.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"through");
    sandbox.stop().await;
}

#[tokio::test]
async fn a_dial_announces_readiness_only_after_post_auth_password_saving() {
    let (sandbox, _server) = ssh_lab(&["  StrictHostKeyChecking accept-new"]).await;
    let backend = &sandbox.backend;
    let dial = backend.open_session(body(
        json!({ "op": "dial", "profile": { "kind": "ssh", "target": "lab" } }),
    ));
    let mut asked_to_create = false;
    loop {
        let event = next_event(&dial).await;
        match message(&event) {
            Some(TerminalServerMessage::AuthPrompt(info)) => {
                let purpose = info.purpose;
                let answers =
                    if purpose == Some(muxus_core::protocol::AuthPromptPurpose::VaultCreate) {
                        asked_to_create = true;
                        vec!["master-pass-12".to_string(), "master-pass-12".to_string()]
                    } else {
                        vec!["pw".to_string()]
                    };
                dial.send(TerminalClientMessage::AuthResponse(AuthPromptResponse {
                    answers,
                    remember_password: Some(true),
                    skipped: None,
                }));
            }
            Some(TerminalServerMessage::Ready { summary, .. }) => {
                assert!(summary.is_none());
                break;
            }
            Some(TerminalServerMessage::Exit { message, .. }) => panic!("dial failed: {message:?}"),
            _ => {}
        }
    }
    assert!(
        asked_to_create,
        "remembering a password creates the vault first"
    );
    // The password is saved by the time the dial says it is ready.
    assert_eq!(backend.vault_status().await.unwrap().credential_count, 1);

    // A dial session moves between windows like any other.
    let destination = backend.attach_session(dial.terminal_id(), 100, 30);
    assert_eq!(
        next_event(&dial).await,
        SessionEvent::Closed {
            reason: Some("terminal transferred".into())
        }
    );
    events_until(&destination, is_ready).await;
    assert_eq!(
        backend.connections_list().await.unwrap().connections.len(),
        1
    );
    destination.close();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !backend
            .connections_list()
            .await
            .unwrap()
            .connections
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the last lease closes the transport");
    sandbox.stop().await;
}

// --- serial ------------------------------------------------------------------------------------

#[cfg(unix)]
mod serial {
    use std::io::{Read, Write};

    use serialport::{SerialPort, TTYPort};

    use super::*;

    /// Read the far end of the pty until `until` arrived.
    fn read_line_end(port: &mut TTYPort, until: &[u8]) -> Vec<u8> {
        let mut received = Vec::new();
        let mut buffer = [0u8; 256];
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !received.windows(until.len()).any(|window| window == until) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out: {received:?}"
            );
            match port.read(&mut buffer) {
                Ok(read) => received.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(error) => panic!("{error}"),
            }
        }
        received
    }

    #[tokio::test]
    async fn a_serial_session_carries_bytes_paced_pastes_and_break_answers() {
        let sandbox = start().await;
        let (mut device, line) = TTYPort::pair().unwrap();
        let path = line.name().unwrap();
        let handle = sandbox.backend.open_session(connect(json!({
            "kind": "serial", "path": path, "baudRate": 9600,
        })));
        let setup = events_until(&handle, is_ready).await;
        assert!(setup.iter().any(|event| matches!(
            message(event),
            Some(TerminalServerMessage::Status { message, transient: Some(true) })
                if message == &format!("Opening {path} at 9600 baud …")
        )));
        match message(setup.last().unwrap()) {
            Some(TerminalServerMessage::Ready { conn_id, .. }) => {
                assert!(conn_id.starts_with("serial-"))
            }
            _ => unreachable!(),
        }
        device.write_all(b"login: ").unwrap();
        output_until(&handle, "login: ").await;
        handle.send_data(b"admin\r".to_vec());
        let mut device = tokio::task::spawn_blocking(move || {
            read_line_end(&mut device, b"admin\r");
            device
        })
        .await
        .unwrap();

        // A paced paste waits for each line to leave the line before its delay.
        handle.send(body(json!({
            "op": "paste", "text": "conf t\nend\n", "bracketed": false,
            "lineDelayMs": 20, "charDelayMs": 0,
        })));
        let reader = tokio::task::spawn_blocking(move || {
            read_line_end(&mut device, b"conf t\rend\r");
            device
        });
        events_until(&handle, |event| {
            matches!(
                message(event),
                Some(TerminalServerMessage::PasteProgress {
                    state: PasteProgressState::Done,
                    ..
                })
            )
        })
        .await;
        let _device = reader.await.unwrap();

        // A pty takes the break but has no modem lines, so the BREAK is answered as failed.
        handle.send(TerminalClientMessage::SendBreak);
        let answer = events_until(&handle, |event| {
            matches!(
                message(event),
                Some(TerminalServerMessage::BreakResult { .. })
            )
        })
        .await;
        match message(answer.last().unwrap()) {
            Some(TerminalServerMessage::BreakResult { ok, message }) => {
                assert!(!ok);
                assert!(
                    message
                        .as_deref()
                        .unwrap()
                        .starts_with("Could not send BREAK: ")
                );
            }
            _ => unreachable!(),
        }
        handle.close();
        drop(line);
        sandbox.stop().await;
    }
}
