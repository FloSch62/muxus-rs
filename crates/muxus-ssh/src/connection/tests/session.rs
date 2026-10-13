//! Port of tests/unit/server/connection-session.test.ts (session settings, host keys, summary).

use super::*;
use crate::connection::leases::ConnectionLeaseOwner;
use crate::connection::manager::ConnectOptions;
use crate::connection::profile::SshProfile;
use crate::testing::server::{ServerSpec, start};

const PASSWORD: &str = "secret";

fn host_lines(port: u16, extra: &[&str]) -> Vec<String> {
    let mut lines = vec![
        "Host lab".to_string(),
        "  HostName 127.0.0.1".to_string(),
        "  User tester".to_string(),
        format!("  Port {port}"),
    ];
    lines.extend(extra.iter().map(|s| s.to_string()));
    lines
}

fn write(sandbox: &Sandbox, lines: &[String]) {
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    sandbox.write_config(&refs);
}

#[tokio::test]
async fn sends_set_env_and_runs_remote_command_without_a_tty_by_default() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(
            server.port(),
            &[
                "  StrictHostKeyChecking accept-new",
                "  SetEnv FOO=bar",
                "  RemoteCommand echo hello",
            ],
        ),
    );
    let manager = sandbox.manager();
    let io = TestIo::accepting(PASSWORD);
    let mut shell = manager
        .connect_shell(
            &SshProfile::new("lab"),
            io.clone(),
            80,
            24,
            "xterm-256color",
            None,
            None,
        )
        .await
        .unwrap();
    let output = read_until(&mut shell.channel, "ran: echo hello").await;
    assert!(output.contains("ran: echo hello"));
    let log = server.log.lock().unwrap();
    assert_eq!(log.env, vec![("FOO".to_string(), "bar".to_string())]);
    assert_eq!(log.execs, vec!["echo hello".to_string()]);
    assert!(log.ptys.is_empty());
}

#[tokio::test]
async fn request_tty_yes_allocates_a_tty_for_a_remote_command() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(
            server.port(),
            &[
                "  StrictHostKeyChecking accept-new",
                "  RemoteCommand top",
                "  RequestTTY yes",
            ],
        ),
    );
    let shell = sandbox
        .manager()
        .connect_shell(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            100,
            30,
            "xterm-256color",
            None,
            None,
        )
        .await
        .unwrap();
    drop(shell);
    let log = server.log.lock().unwrap();
    assert_eq!(log.ptys.len(), 1);
    assert_eq!((log.ptys[0].1, log.ptys[0].2), (100, 30));
    assert_eq!(log.execs, vec!["top".to_string()]);
}

#[tokio::test]
async fn opens_an_interactive_shell_with_del_as_the_erase_character() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(server.port(), &["  StrictHostKeyChecking accept-new"]),
    );
    let io = TestIo::accepting(PASSWORD);
    let mut shell = sandbox
        .manager()
        .connect_shell(
            &SshProfile::new("lab"),
            io,
            132,
            42,
            "xterm-256color",
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        read_until(&mut shell.channel, "shell ok")
            .await
            .contains("shell ok")
    );
    let log = server.log.lock().unwrap();
    // The shell-integration probe ran first; the server is no bash/zsh, so a plain shell opened.
    assert!(log.execs[0].contains("__MUXUS_SHELL__"));
    let (term, cols, rows, modes) = log.ptys.last().unwrap().clone();
    assert_eq!((term.as_str(), cols, rows), ("xterm-256color", 132, 42));
    assert!(modes.contains(&(russh::Pty::VERASE, 0x7f)));
    assert!(log.events.contains(&"shell".to_string()));
}

#[tokio::test]
async fn strict_host_key_checking_accept_new_records_into_user_known_hosts_file_without_prompting()
{
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    let custom = sandbox.dir.path().join("custom_known_hosts");
    write(
        &sandbox,
        &host_lines(
            server.port(),
            &[
                "  StrictHostKeyChecking accept-new",
                &format!("  UserKnownHostsFile {}", custom.display()),
            ],
        ),
    );
    let io = TestIo::accepting(PASSWORD);
    let lease = sandbox
        .manager()
        .connect(
            &SshProfile::new("lab"),
            io.clone(),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap();
    assert!(io.host_keys.lock().unwrap().is_empty());
    let text = std::fs::read_to_string(&custom).unwrap();
    assert!(text.contains(&format!("[127.0.0.1]:{} ssh-ed25519", server.port())));
    assert!(!sandbox.known_hosts.exists());
    assert!(
        io.statuses()
            .iter()
            .any(|s| s.starts_with("Automatically accepted the new host key for 127.0.0.1"))
    );
    drop(lease);
}

#[cfg(unix)]
#[tokio::test]
async fn connects_without_persisting_when_user_known_hosts_file_is_the_null_device() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(server.port(), &["  UserKnownHostsFile /dev/null"]),
    );
    let io = TestIo::accepting(PASSWORD);
    let manager = sandbox.manager();
    let first = manager
        .connect(
            &SshProfile::new("lab"),
            io.clone(),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(io.host_keys.lock().unwrap().len(), 1);
    drop(first);
    assert!(!sandbox.known_hosts.exists());
}

#[tokio::test]
async fn asks_about_a_new_host_key_and_remembers_an_accepted_one() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(&sandbox, &host_lines(server.port(), &[]));
    let io = TestIo::accepting(PASSWORD);
    let manager = sandbox.manager();
    let lease = manager
        .connect(
            &SshProfile::new("lab"),
            io.clone(),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap();
    {
        let challenges = io.host_keys.lock().unwrap();
        assert_eq!(challenges.len(), 1);
        assert_eq!(challenges[0].state, crate::connection::HostKeyState::New);
        assert_eq!(challenges[0].key_type, "ssh-ed25519");
        assert_eq!(
            challenges[0].fingerprint,
            crate::known_hosts::fingerprint_sha256(&crate::security_keys::public_key_blob(
                &server.host_key
            ))
        );
        assert_eq!(challenges[0].port, server.port());
    }
    lease.connection().close();
    drop(lease);
    // Known now: the next dial asks nothing.
    let io2 = TestIo::accepting(PASSWORD);
    let again = manager
        .connect(
            &SshProfile::new("lab"),
            io2.clone(),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap();
    assert!(io2.host_keys.lock().unwrap().is_empty());
    drop(again);
}

#[tokio::test]
async fn fails_promptly_when_the_host_key_interaction_rejects() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(&sandbox, &host_lines(server.port(), &[]));
    let io = TestIo::accepting(PASSWORD);
    *io.accept_host_keys.lock().unwrap() = false;
    let started = std::time::Instant::now();
    let err = sandbox
        .manager()
        .connect(
            &SshProfile::new("lab"),
            io.clone(),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        err.to_string().contains("Host key verification failed"),
        "{err}"
    );
    assert_eq!(io.prompt_count(), 0);
}

#[tokio::test]
async fn refuses_a_changed_host_key_under_strict_checking() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    let other = crate::testing::agent::ed25519();
    let blob = crate::security_keys::public_key_blob(other.public_key());
    KnownHostsStore::new(vec![sandbox.known_hosts.clone()], vec![])
        .record("127.0.0.1", server.port(), &blob)
        .unwrap();
    write(
        &sandbox,
        &host_lines(server.port(), &["  StrictHostKeyChecking yes"]),
    );
    let io = TestIo::accepting(PASSWORD);
    let err = sandbox
        .manager()
        .connect(
            &SshProfile::new("lab"),
            io.clone(),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Host key verification failed"));
    assert!(
        io.statuses()
            .iter()
            .any(|s| s.starts_with("HOST KEY CHANGED for 127.0.0.1"))
    );
}

#[tokio::test]
async fn password_authentication_no_never_prompts_for_or_offers_a_password() {
    let server = start(
        ServerSpec::new()
            .password(PASSWORD)
            .methods(&[russh::MethodKind::Password]),
    )
    .await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(
            server.port(),
            &[
                "  StrictHostKeyChecking accept-new",
                "  PasswordAuthentication no",
                "  KbdInteractiveAuthentication no",
            ],
        ),
    );
    let io = TestIo::accepting(PASSWORD);
    let err = sandbox
        .manager()
        .connect(
            &SshProfile::new("lab"),
            io.clone(),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("authentication to 127.0.0.1:{} failed", server.port())
    );
    assert_eq!(io.prompt_count(), 0);
    assert!(server.auth_events().is_empty());
}

#[tokio::test]
async fn describes_a_direct_password_login_and_a_second_tab_on_its_connection() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(server.port(), &["  StrictHostKeyChecking accept-new"]),
    );
    let manager = sandbox.manager();
    let io = TestIo::accepting(PASSWORD);
    let first = manager
        .connect_shell(
            &SshProfile::new("lab"),
            io.clone(),
            80,
            24,
            "xterm",
            None,
            None,
        )
        .await
        .unwrap();
    let s = &first.summary;
    assert_eq!(
        (s.user.as_str(), s.host.as_str(), s.port),
        ("tester", "127.0.0.1", server.port())
    );
    assert!(s.jump_hosts.is_empty());
    assert!(!s.proxy_command);
    assert!(!s.shared);
    assert_eq!(s.auth_methods, vec!["password".to_string()]);
    assert_eq!(s.compression, muxus_core::protocol::CompressionState::Off);
    assert!(
        s.server_software
            .as_deref()
            .is_some_and(|v| v.starts_with("russh")),
        "{:?}",
        s.server_software
    );
    assert!(s.kex.is_some() && s.cipher.is_some());
    assert_eq!(s.x11, muxus_core::protocol::X11State::Off);
    assert_eq!(
        s.agent_forwarding,
        crate::connection::transport::AgentForwarding::Off
    );
    let second = manager
        .connect_shell(&SshProfile::new("lab"), io, 80, 24, "xterm", None, None)
        .await
        .unwrap();
    assert!(second.summary.shared);
    assert_eq!(server.connections(), 1);
}

#[tokio::test]
async fn reports_compression_the_server_agreed_to() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(
            server.port(),
            &["  StrictHostKeyChecking accept-new", "  Compression yes"],
        ),
    );
    let shell = sandbox
        .manager()
        .connect_shell(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        shell.summary.compression,
        muxus_core::protocol::CompressionState::On
    );
}

#[tokio::test]
async fn says_when_agent_forwarding_is_asked_for_but_no_agent_is_reachable() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(
            server.port(),
            &["  StrictHostKeyChecking accept-new", "  ForwardAgent yes"],
        ),
    );
    let shell = sandbox
        .manager()
        .connect_shell(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        shell.summary.agent_forwarding,
        crate::connection::transport::AgentForwarding::NoAgent
    );
}

#[tokio::test]
async fn times_out_a_silent_server_with_connect_timeout() {
    // A listener that accepts and never speaks SSH.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            held.push(s);
        }
    });
    let sandbox = Sandbox::new();
    write(&sandbox, &host_lines(port, &["  ConnectTimeout 1"]));
    let err = sandbox
        .manager()
        .connect(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("connection to 127.0.0.1:{port} timed out")
    );
}

#[tokio::test]
async fn reports_a_refused_connection() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let sandbox = Sandbox::new();
    write(&sandbox, &host_lines(port, &[]));
    let err = sandbox
        .manager()
        .connect(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("connection refused by 127.0.0.1:{port} — is sshd running?")
    );
}

// --- host-key-pin.test.ts ---

fn pinned(fingerprint: &str) -> SshProfile {
    SshProfile {
        host_key_fingerprint: Some(fingerprint.to_string()),
        ..SshProfile::new("lab")
    }
}

#[tokio::test]
async fn refuses_a_server_whose_key_does_not_match_the_link_before_any_known_hosts_prompt() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(server.port(), &["  StrictHostKeyChecking no"]),
    );
    let other = crate::testing::agent::ed25519();
    let wrong = crate::known_hosts::fingerprint_sha256(&crate::security_keys::public_key_blob(
        other.public_key(),
    ));
    let io = TestIo::accepting(PASSWORD);
    let err = sandbox
        .manager()
        .connect(
            &pinned(&wrong),
            io.clone(),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Host key verification failed"));
    assert!(io.host_keys.lock().unwrap().is_empty());
    assert!(
        io.statuses()
            .iter()
            .any(|s| s.starts_with("HOST KEY MISMATCH for 127.0.0.1: the link expects"))
    );
    assert!(!sandbox.known_hosts.exists());
}

#[tokio::test]
async fn still_asks_before_trusting_a_new_host_whose_key_matches_the_link() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(&sandbox, &host_lines(server.port(), &[]));
    let blob = crate::security_keys::public_key_blob(&server.host_key);
    let right = format!(
        "ssh-ed25519 {}",
        crate::known_hosts::fingerprint_sha256(&blob)
    );
    let io = TestIo::accepting(PASSWORD);
    let manager = sandbox.manager();
    let lease = manager
        .connect(
            &pinned(&right),
            io.clone(),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(io.host_keys.lock().unwrap().len(), 1);
    assert!(
        io.statuses()
            .iter()
            .any(|s| s
                == "The ssh-ed25519 host key of 127.0.0.1 matches the fingerprint in the link.")
    );
    // A pinned target never shares a transport whose key it has not checked.
    let plain = manager
        .connect(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap();
    assert_ne!(plain.connection().id, lease.connection().id);
}

// --- ProxyCommand transports ---

#[cfg(unix)]
#[tokio::test]
async fn dials_through_a_proxy_command() {
    if !std::path::Path::new("/bin/bash").exists() {
        return;
    }
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(
            server.port(),
            &[
                "  StrictHostKeyChecking accept-new",
                "  ProxyCommand /bin/bash -c 'exec 3<>/dev/tcp/%h/%p; cat <&3 & exec cat >&3'",
            ],
        ),
    );
    let shell = sandbox
        .manager()
        .connect_shell(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm",
            None,
            None,
        )
        .await
        .unwrap();
    assert!(shell.summary.proxy_command);
    assert_eq!(server.connections(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn reports_a_failing_proxy_command_with_its_output() {
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(2, &["  ProxyCommand echo no route to %h >&2; exit 7"]),
    );
    let err = sandbox
        .manager()
        .connect(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("ProxyCommand failed with exit code 7"),
        "{err}"
    );
    assert!(err.contains("no route to 127.0.0.1"), "{err}");
}

// --- x11-forwarding.test.ts (SSH parts) ---

#[cfg(unix)]
#[tokio::test]
async fn forwards_x11_to_the_local_server_with_the_real_cookie() {
    use crate::x11::test_support::{FakeXServer, write_xauthority};
    use crate::x11::{LocalX11, LocalX11Options, Platform};
    let sandbox = Sandbox::new();
    let x_server = FakeXServer::start(sandbox.dir.path(), "7", b"X11 ready");
    let xauthority = sandbox.dir.path().join("Xauthority");
    write_xauthority(&xauthority, "7", &[0x42; 16]);
    let local = Arc::new(LocalX11::new(LocalX11Options {
        display: Some(x_server.display.clone()),
        xauthority: Some(xauthority),
        env: Arc::new(|_: &str| None),
        platform: Platform::Linux,
        ..LocalX11Options::default()
    }));
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    write(
        &sandbox,
        &host_lines(
            server.port(),
            &["  StrictHostKeyChecking accept-new", "  ForwardX11 yes"],
        ),
    );
    let mut options = sandbox.options();
    options.x11 = Some(local);
    let shell = SshConnectionManager::new(options)
        .connect_shell(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(shell.summary.x11, muxus_core::protocol::X11State::On);
    let reply = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(event) = server
                .events()
                .into_iter()
                .find(|e| e.starts_with("x11 reply"))
            {
                return event;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(reply, "x11 reply X11 ready");
    let x11_req = server
        .events()
        .into_iter()
        .find(|e| e.starts_with("x11 "))
        .unwrap();
    assert!(
        x11_req.starts_with("x11 false MIT-MAGIC-COOKIE-1 "),
        "{x11_req}"
    );
    // The local X server got the real cookie, never the fake one sshd saw.
    let setups = x_server.setups.lock().unwrap().clone();
    assert_eq!(setups.len(), 1);
    assert!(setups[0].windows(16).any(|w| w == [0x42; 16]));
}

#[cfg(unix)]
#[tokio::test]
async fn tells_a_forward_x11_host_that_the_server_refused() {
    use crate::x11::test_support::FakeXServer;
    use crate::x11::{LocalX11, LocalX11Options, Platform};
    let sandbox = Sandbox::new();
    let x_server = FakeXServer::start(sandbox.dir.path(), "8", b"X11 ready");
    let local = Arc::new(LocalX11::new(LocalX11Options {
        display: Some(x_server.display.clone()),
        env: Arc::new(|_: &str| None),
        platform: Platform::Linux,
        ..LocalX11Options::default()
    }));
    let mut spec = ServerSpec::new().password(PASSWORD);
    spec.reject_x11 = true;
    let server = start(spec).await;
    write(
        &sandbox,
        &host_lines(
            server.port(),
            &["  StrictHostKeyChecking accept-new", "  ForwardX11 yes"],
        ),
    );
    let mut options = sandbox.options();
    options.x11 = Some(local);
    let manager = SshConnectionManager::new(options);
    let io = TestIo::accepting(PASSWORD);
    let first = manager
        .connect_shell(
            &SshProfile::new("lab"),
            io.clone(),
            80,
            24,
            "xterm",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(first.summary.x11, muxus_core::protocol::X11State::Refused);
    assert!(
        io.statuses()
            .iter()
            .any(|s| s.starts_with("This server refused X11 forwarding."))
    );
    // A refusal stops later requests on the transport.
    let _second = manager
        .connect_shell(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        server
            .events()
            .iter()
            .filter(|e| e.starts_with("x11 "))
            .count(),
        1
    );
}

#[tokio::test]
async fn runs_a_command_and_collects_its_output_and_exit_status() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &host_lines(server.port(), &["  StrictHostKeyChecking accept-new"]),
    );
    let lease = sandbox
        .manager()
        .connect(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            ConnectionLeaseOwner::Stats,
            ConnectOptions::default(),
        )
        .await
        .unwrap();
    let out = lease
        .connection()
        .exec("uname -a", Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ran: uname -a\n");
    assert_eq!(out.exit_status, Some(0));
    assert!(!out.timed_out);
}
