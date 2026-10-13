//! Starting a saved tunnel (`startTunnel` in `client/src/api/tunnels.ts` and
//! `dialConnection` in `api/dial.ts`): ride an existing live connection to
//! its target when one exists, otherwise dial a shell-less transport, with
//! the same interactive sign-in round trips as a terminal. The forward holds
//! its own lease on the transport, so closing terminals never tears a running
//! tunnel down.

use std::future::Future;

use muxus_core::api::{ForwardInfo, TunnelRecord};
use muxus_core::protocol::{
    AuthPromptInfo, AuthPromptResponse, HostKeyChallenge, TerminalClientMessage,
    TerminalServerMessage,
};
use muxus_session::{Backend, SessionEvent};

use super::model::{reusable_connection, tunnel_forward_request};

/// A question the dial asks while it signs in.
#[derive(Clone, Debug, PartialEq)]
pub enum DialPrompt {
    Auth(AuthPromptInfo),
    HostKey(HostKeyChallenge),
}

/// The answer to a [`DialPrompt`].
#[derive(Clone, Debug, PartialEq)]
pub enum DialAnswer {
    /// `None` cancels the connection attempt.
    Auth(Option<AuthPromptResponse>),
    HostKey(bool),
}

/// The dial message for a tunnel: its target, its own SSH options when it
/// carries them (then without ssh_config), and the keepalive preference.
pub fn dial_message(
    tunnel: &TunnelRecord,
    keepalive_interval_seconds: Option<f64>,
) -> Result<TerminalClientMessage, String> {
    let mut profile = serde_json::Map::new();
    profile.insert("kind".into(), "ssh".into());
    profile.insert("target".into(), tunnel.target.clone().into());
    if let Some(options) = &tunnel.ssh_options {
        profile.insert("useConfig".into(), false.into());
        if let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(options) {
            profile.extend(fields);
        }
    }
    // A disabled keepalive is left out, so a stale value can never outvote it.
    if let Some(interval) = keepalive_interval_seconds.filter(|interval| *interval > 0.) {
        profile.insert(
            "keepaliveIntervalSeconds".into(),
            (interval.round() as u64).into(),
        );
    }
    serde_json::from_value(serde_json::json!({ "op": "dial", "profile": profile }))
        .map_err(|error| error.to_string())
}

/// Start `tunnel`. `on_status` hears the dial's progress lines; `ask` answers
/// its sign-in questions. Errors carry the message the panel shows.
pub async fn start_tunnel<S, A, F>(
    backend: &Backend,
    tunnel: &TunnelRecord,
    keepalive_interval_seconds: Option<f64>,
    mut on_status: S,
    mut ask: A,
) -> Result<ForwardInfo, String>
where
    S: FnMut(String),
    A: FnMut(DialPrompt) -> F,
    F: Future<Output = DialAnswer>,
{
    let connections = backend
        .connections_list()
        .await
        .map_err(|error| error.message)?
        .connections;
    if let Some(existing) = reusable_connection(tunnel, &connections) {
        match backend
            .start_forward(tunnel_forward_request(&existing.id, tunnel))
            .await
        {
            Ok(forward) => return Ok(forward),
            // The listed connection can die between poll and start: fall
            // through to a fresh dial only for that case.
            Err(error) if error.status == 404 => {}
            Err(error) => return Err(error.message),
        }
    }
    let handle = backend.open_session(dial_message(tunnel, keepalive_interval_seconds)?);
    let mut last_status = String::new();
    let conn_id = loop {
        let Some(event) = handle.next_event().await else {
            return Err(fallback(
                &last_status,
                "connection closed before it was ready",
            ));
        };
        match event {
            SessionEvent::Message(TerminalServerMessage::Status { message, .. }) => {
                last_status = message.clone();
                on_status(message);
            }
            SessionEvent::Message(TerminalServerMessage::AuthPrompt(info)) => {
                match ask(DialPrompt::Auth(info)).await {
                    DialAnswer::Auth(Some(response)) => {
                        handle.send(TerminalClientMessage::AuthResponse(response))
                    }
                    _ => {
                        handle.close();
                        return Err(fallback(
                            &last_status,
                            "connection closed before it was ready",
                        ));
                    }
                }
            }
            SessionEvent::Message(TerminalServerMessage::HostKey(challenge)) => {
                let accept = matches!(
                    ask(DialPrompt::HostKey(challenge)).await,
                    DialAnswer::HostKey(true)
                );
                handle.send(TerminalClientMessage::HostKeyResponse { accept });
            }
            SessionEvent::Message(TerminalServerMessage::Ready { conn_id, .. }) => break conn_id,
            SessionEvent::Message(TerminalServerMessage::Exit { message, .. }) => {
                let message = message.filter(|message| !message.is_empty());
                return Err(message.unwrap_or_else(|| fallback(&last_status, "connection failed")));
            }
            SessionEvent::Closed { .. } => {
                return Err(fallback(
                    &last_status,
                    "connection closed before it was ready",
                ));
            }
            _ => {}
        }
    };
    // The dial lease is released right after; the forward now holds its own.
    let started = backend
        .start_forward(tunnel_forward_request(&conn_id, tunnel))
        .await
        .map_err(|error| error.message);
    handle.close();
    started
}

fn fallback(last_status: &str, otherwise: &str) -> String {
    if last_status.is_empty() {
        otherwise.to_owned()
    } else {
        last_status.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use muxus_core::api::{ForwardStatus, ForwardType, TunnelInput};
    use muxus_session::{BackendOptions, RuntimeChoice, SessionHandle};
    use muxus_ssh::testing::server::{ServerSpec, TestServer, start as start_sshd};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    struct Lab {
        runtime: tokio::runtime::Runtime,
        backend: Backend,
        server: TestServer,
        echo_port: u16,
        _scratch: tempfile::TempDir,
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// A password-only SSH server known as `lab` in the sandbox's ssh_config,
    /// and an echo service the tunnels forward to.
    fn lab() -> Lab {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(start_sshd(
            ServerSpec::new()
                .password("pw")
                .methods(&[russh::MethodKind::Password]),
        ));
        let echo = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        runtime.spawn(async move {
            while let Ok((mut socket, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let mut buffer = [0u8; 256];
                    while let Ok(read) = socket.read(&mut buffer).await {
                        if read == 0 || socket.write_all(&buffer[..read]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let home = scratch.path().join("home");
        write_config(&home, server.port());
        let mut options = BackendOptions::sandbox(scratch.path().join("data"), &home);
        options.runtime = RuntimeChoice::Handle(runtime.handle().clone());
        let backend = runtime.block_on(Backend::start(options)).unwrap();
        Lab {
            runtime,
            backend,
            server,
            echo_port,
            _scratch: scratch,
        }
    }

    fn write_config(home: &Path, port: u16) {
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(
            home.join(".ssh/config"),
            format!("Host lab\n  HostName 127.0.0.1\n  User ops\n  Port {port}\n"),
        )
        .unwrap();
    }

    impl Lab {
        fn tunnel(&self, bind_port: u16) -> TunnelRecord {
            self.runtime
                .block_on(self.backend.save_tunnel(TunnelInput {
                    id: None,
                    name: Some("Echo".into()),
                    target: "lab".into(),
                    ssh_options: None,
                    kind: ForwardType::Local,
                    bind_port,
                    target_host: Some("127.0.0.1".into()),
                    target_port: Some(self.echo_port),
                    auto_start: None,
                    auto_reconnect: None,
                }))
                .unwrap()
        }

        /// Echo a line through a forwarded port.
        fn echo_through(&self, port: u16) -> String {
            self.runtime.block_on(async {
                let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap();
                socket.write_all(b"ping").await.unwrap();
                let mut buffer = [0u8; 4];
                tokio::time::timeout(Duration::from_secs(10), socket.read_exact(&mut buffer))
                    .await
                    .unwrap()
                    .unwrap();
                String::from_utf8_lossy(&buffer).into_owned()
            })
        }

        /// A terminal on `lab`, signed in.
        fn terminal(&self) -> SessionHandle {
            let connect: TerminalClientMessage = serde_json::from_value(serde_json::json!({
                "op": "connect",
                "profile": { "kind": "ssh", "target": "lab" },
                "cols": 80,
                "rows": 24
            }))
            .unwrap();
            let session = self.backend.open_session(connect);
            self.runtime.block_on(async {
                loop {
                    let event = tokio::time::timeout(Duration::from_secs(20), session.next_event())
                        .await
                        .unwrap()
                        .unwrap();
                    match event {
                        SessionEvent::Message(TerminalServerMessage::HostKey(_)) => {
                            session.send(TerminalClientMessage::HostKeyResponse { accept: true })
                        }
                        SessionEvent::Message(TerminalServerMessage::AuthPrompt(_)) => session
                            .send(TerminalClientMessage::AuthResponse(AuthPromptResponse {
                                answers: vec!["pw".into()],
                                remember_password: None,
                                skipped: None,
                            })),
                        SessionEvent::Message(TerminalServerMessage::Ready { .. }) => break,
                        SessionEvent::Message(TerminalServerMessage::Exit { message, .. }) => {
                            panic!("terminal ended: {message:?}")
                        }
                        _ => {}
                    }
                }
            });
            session
        }
    }

    #[test]
    fn builds_the_dial_message_of_a_tunnel() {
        let tunnel = TunnelRecord {
            id: "t".into(),
            name: None,
            target: "db.internal".into(),
            ssh_options: Some(muxus_core::api::TunnelSshOptions {
                user: Some("ops".into()),
                port: Some(2222),
                ..Default::default()
            }),
            kind: ForwardType::Dynamic,
            bind_port: 1080,
            target_host: None,
            target_port: None,
            auto_start: false,
            auto_reconnect: false,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let message = serde_json::to_value(dial_message(&tunnel, Some(30.)).unwrap()).unwrap();
        assert_eq!(message["op"], "dial");
        assert_eq!(message["profile"]["target"], "db.internal");
        assert_eq!(message["profile"]["useConfig"], false);
        assert_eq!(message["profile"]["user"], "ops");
        assert_eq!(message["profile"]["port"], 2222);
        assert_eq!(message["profile"]["keepaliveIntervalSeconds"], 30);
        let config_backed = TunnelRecord {
            ssh_options: None,
            ..tunnel
        };
        let message =
            serde_json::to_value(dial_message(&config_backed, Some(0.)).unwrap()).unwrap();
        assert!(message["profile"].get("useConfig").is_none());
        assert!(message["profile"].get("keepaliveIntervalSeconds").is_none());
    }

    #[test]
    fn dials_a_tunnel_and_signs_in_through_the_prompts() {
        let lab = lab();
        let port = free_port();
        let tunnel = lab.tunnel(port);
        let mut statuses = Vec::new();
        let mut asked = Vec::new();
        let forward = lab
            .runtime
            .block_on(start_tunnel(
                &lab.backend,
                &tunnel,
                None,
                |status| statuses.push(status),
                |prompt| {
                    let answer = match &prompt {
                        DialPrompt::HostKey(_) => DialAnswer::HostKey(true),
                        DialPrompt::Auth(_) => DialAnswer::Auth(Some(AuthPromptResponse {
                            answers: vec!["pw".into()],
                            remember_password: None,
                            skipped: None,
                        })),
                    };
                    asked.push(matches!(prompt, DialPrompt::Auth(_)));
                    async move { answer }
                },
            ))
            .unwrap();
        assert_eq!(forward.tunnel_id.as_deref(), Some(tunnel.id.as_str()));
        assert_eq!(forward.status, ForwardStatus::Active);
        assert_eq!(asked, [false, true], "host key first, then the password");
        assert!(!statuses.is_empty());
        assert_eq!(lab.echo_through(port), "ping");
        // The dial lease went away; the forward keeps the transport.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(lab.echo_through(port), "ping");
        lab.runtime.block_on(lab.backend.shutdown());
    }

    #[test]
    fn a_dismissed_password_prompt_cancels_the_start() {
        let lab = lab();
        let tunnel = lab.tunnel(free_port());
        let result = lab.runtime.block_on(start_tunnel(
            &lab.backend,
            &tunnel,
            None,
            |_| {},
            |prompt| {
                let answer = match prompt {
                    DialPrompt::HostKey(_) => DialAnswer::HostKey(true),
                    DialPrompt::Auth(_) => DialAnswer::Auth(None),
                };
                async move { answer }
            },
        ));
        let error = result.unwrap_err();
        assert!(!error.is_empty());
        let forwards = lab.runtime.block_on(lab.backend.forwards(None)).unwrap();
        assert!(forwards.is_empty(), "{forwards:?}");
        lab.runtime.block_on(lab.backend.shutdown());
    }

    #[test]
    fn rides_a_live_connection_to_a_config_target() {
        let lab = lab();
        let _terminal = lab.terminal();
        assert_eq!(lab.server.connections(), 1);
        let port = free_port();
        let tunnel = lab.tunnel(port);
        let forward = lab
            .runtime
            .block_on(start_tunnel(
                &lab.backend,
                &tunnel,
                None,
                |_| {},
                |_| async { panic!("a live connection needs no sign-in") },
            ))
            .unwrap();
        assert_eq!(forward.status, ForwardStatus::Active);
        assert_eq!(lab.echo_through(port), "ping");
        assert_eq!(lab.server.connections(), 1, "no second dial");
        lab.runtime.block_on(lab.backend.shutdown());
    }
}
