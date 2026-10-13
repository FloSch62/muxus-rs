//! Port of connection-mux.test.ts and jump-hops.test.ts.

use crate::types::ConfigForwardExt;
use russh::MethodKind;

use super::*;
use crate::connection::leases::ConnectionLeaseOwner;
use crate::connection::manager::{ConnectOptions, ShellTransport};
use crate::connection::profile::SshProfile;
use crate::testing::server::{ServerSpec, start};
use crate::types::{ConfigForward, ForwardType};

const PASSWORD: &str = "secret";

fn spec() -> ServerSpec {
    ServerSpec::new()
        .password(PASSWORD)
        .methods(&[MethodKind::Password])
}

fn host(alias: &str, port: u16, extra: &[&str]) -> Vec<String> {
    let mut lines = vec![
        format!("Host {alias}"),
        "  HostName 127.0.0.1".into(),
        "  User tester".into(),
        format!("  Port {port}"),
        "  StrictHostKeyChecking accept-new".into(),
    ];
    lines.extend(extra.iter().map(|s| s.to_string()));
    lines
}

fn write(sandbox: &Sandbox, blocks: &[Vec<String>]) {
    let lines: Vec<&str> = blocks.iter().flatten().map(String::as_str).collect();
    sandbox.write_config(&lines);
}

async fn shell(manager: &SshConnectionManager, target: &str) -> crate::connection::TerminalShell {
    manager
        .connect_shell(
            &SshProfile::new(target),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm-256color",
            None,
            None,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn multiplexes_a_second_terminal_onto_the_existing_transport() {
    let server = start(spec()).await;
    let sandbox = Sandbox::new();
    write(&sandbox, &[host("lab", server.port(), &[])]);
    let manager = sandbox.manager();
    let first = shell(&manager, "lab").await;
    let io = TestIo::accepting(PASSWORD);
    let second = manager
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
    assert_eq!(first.transport, ShellTransport::New);
    assert_eq!(second.transport, ShellTransport::Shared);
    assert_eq!(second.lease.connection().id, first.lease.connection().id);
    assert_eq!(server.connections(), 1);
    assert_eq!(manager.lease_count(&first.lease.connection().id, None), 2);
    assert!(io.statuses().iter().any(|s| s.contains("(multiplexed)")));
    assert_eq!(io.prompt_count(), 0);
}

#[tokio::test]
async fn adds_config_forwards_requested_by_another_alias_on_a_shared_transport() {
    let server = start(spec()).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &[
            host("web", server.port(), &["  LocalForward 18080 localhost:80"]),
            host(
                "web-db",
                server.port(),
                &["  LocalForward 15432 localhost:5432"],
            ),
        ],
    );
    let manager = sandbox.manager();
    let first = shell(&manager, "web").await;
    let second = shell(&manager, "web-db").await;
    assert_eq!(second.transport, ShellTransport::Shared);
    assert_eq!(
        first.lease.connection().config_forwards(),
        vec![
            ConfigForward::to(ForwardType::Local, 18080, "localhost", 80),
            ConfigForward::to(ForwardType::Local, 15432, "localhost", 5432),
        ]
    );
}

#[tokio::test]
async fn collapses_concurrent_dials_into_one_connection_and_one_auth_round_trip() {
    let server = start(spec()).await;
    let sandbox = Sandbox::new();
    write(&sandbox, &[host("lab", server.port(), &[])]);
    let manager = sandbox.manager();
    let profile = SshProfile::new("lab");
    let dial = |io: Arc<TestIo>| {
        let manager = manager.clone();
        let profile = profile.clone();
        async move {
            manager
                .connect(
                    &profile,
                    io,
                    ConnectionLeaseOwner::Terminal,
                    ConnectOptions::default(),
                )
                .await
        }
    };
    let (a, b, c) = tokio::join!(
        dial(TestIo::accepting(PASSWORD)),
        dial(TestIo::accepting(PASSWORD)),
        dial(TestIo::accepting(PASSWORD))
    );
    let (a, b, c) = (a.unwrap(), b.unwrap(), c.unwrap());
    assert_eq!(server.connections(), 1);
    assert_eq!(a.connection().id, b.connection().id);
    assert_eq!(b.connection().id, c.connection().id);
    assert_eq!(
        server
            .auth_events()
            .iter()
            .filter(|e| e.contains("accepted"))
            .count(),
        1
    );
}

#[tokio::test]
async fn falls_back_to_a_dedicated_transport_when_the_shared_one_refuses_another_session() {
    let mut s = spec();
    s.max_sessions = Some(1);
    let server = start(s).await;
    let sandbox = Sandbox::new();
    write(&sandbox, &[host("lab", server.port(), &[])]);
    let manager = sandbox.manager();
    let first = shell(&manager, "lab").await;
    let io = TestIo::accepting(PASSWORD);
    let second = manager
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
    assert_eq!(second.transport, ShellTransport::Overflow);
    assert_ne!(second.lease.connection().id, first.lease.connection().id);
    assert_eq!(server.connections(), 2);
    assert!(
        io.statuses()
            .iter()
            .any(|s| s.contains("refused another session"))
    );
}

#[tokio::test]
async fn honors_the_fresh_transport_escape_hatch_and_retires_the_replaced_transport() {
    let server = start(spec()).await;
    let sandbox = Sandbox::new();
    write(&sandbox, &[host("lab", server.port(), &[])]);
    let manager = sandbox.manager();
    let old = shell(&manager, "lab").await;
    let fresh = manager
        .connect_shell(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm",
            Some("gesture-1".into()),
            None,
        )
        .await
        .unwrap();
    assert_ne!(fresh.lease.connection().id, old.lease.connection().id);
    assert!(old.lease.connection().superseded());
    // Same gesture: shares the replacement. A plain request also lands on it.
    let sibling = manager
        .connect_shell(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm",
            Some("gesture-1".into()),
            None,
        )
        .await
        .unwrap();
    assert_eq!(sibling.lease.connection().id, fresh.lease.connection().id);
    let plain = shell(&manager, "lab").await;
    assert_eq!(plain.lease.connection().id, fresh.lease.connection().id);
    assert_eq!(server.connections(), 2);
    let listed: Vec<String> = manager.list().into_iter().map(|c| c.id).collect();
    assert_eq!(listed, vec![fresh.lease.connection().id.clone()]);
}

#[tokio::test]
async fn closes_a_transport_after_its_last_lease() {
    let server = start(spec()).await;
    let sandbox = Sandbox::new();
    write(&sandbox, &[host("lab", server.port(), &[])]);
    let manager = sandbox.manager();
    let first = shell(&manager, "lab").await;
    let second = shell(&manager, "lab").await;
    let conn = first.lease.connection().clone();
    drop(first);
    assert!(!conn.is_closed());
    drop(second);
    tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .unwrap();
    assert!(manager.list().is_empty());
}

#[tokio::test]
async fn opens_a_plain_shell_in_console_mode_when_the_host_rejects_pty_req() {
    let mut s = spec();
    s.reject_pty = true;
    let server = start(s).await;
    let sandbox = Sandbox::new();
    write(&sandbox, &[host("console", server.port(), &[])]);
    let mut options = sandbox.options();
    options.console_compatibility_for_host = Some(Arc::new(|alias: &str| alias == "console"));
    let manager = SshConnectionManager::new(options);
    let shell = shell(&manager, "console").await;
    assert!(!shell.lease.connection().sftp_available);
    let log = server.log.lock().unwrap();
    assert!(log.execs.is_empty());
    assert_eq!(log.ptys.len(), 1);
    assert_eq!(log.events.iter().filter(|e| *e == "shell").count(), 1);
    assert!(!log.events.iter().any(|e| e.starts_with("subsystem")));
    drop(log);
    let err = futures_lite_block(shell.lease.connection().sftp());
    assert_eq!(
        err.err().as_deref(),
        Some("SFTP is disabled for this host.")
    );
}

/// Polls a future that completes without awaiting anything external.
fn futures_lite_block<F: Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(out) = future.as_mut().poll(&mut cx) {
            return out;
        }
    }
}

#[tokio::test]
async fn allocates_a_pty_in_console_mode_for_hosts_that_require_one() {
    let mut s = spec();
    s.require_pty = true;
    let server = start(s).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &[host(
            "console",
            server.port(),
            &["  SetEnv TERM=xterm-256color"],
        )],
    );
    let mut options = sandbox.options();
    options.console_compatibility_for_host = Some(Arc::new(|alias: &str| alias == "console"));
    let manager = SshConnectionManager::new(options);
    let shell = shell(&manager, "console").await;
    assert!(!shell.lease.connection().sftp_available);
    let log = server.log.lock().unwrap();
    // Console mode sends no env requests: appliances reject an env-before-pty order with a
    // protocol-error disconnect.
    assert!(log.env.is_empty());
    assert!(log.execs.is_empty());
    assert_eq!(log.ptys.len(), 1);
}

#[tokio::test]
async fn still_applies_set_env_for_hosts_outside_console_mode() {
    let server = start(spec()).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &[host(
            "lab",
            server.port(),
            &["  SetEnv MUXUS_TEST=from-config"],
        )],
    );
    let _shell = shell(&sandbox.manager(), "lab").await;
    assert!(
        server
            .log
            .lock()
            .unwrap()
            .env
            .contains(&("MUXUS_TEST".to_string(), "from-config".to_string()))
    );
}

#[tokio::test]
async fn automatically_redials_in_plain_console_mode_when_integration_drops_the_transport() {
    let mut s = spec();
    s.disconnect_on_exec = true;
    let server = start(s).await;
    let sandbox = Sandbox::new();
    write(&sandbox, &[host("console", server.port(), &[])]);
    let manager = sandbox.manager();
    let first = shell(&manager, "console").await;
    assert_eq!(first.transport, ShellTransport::New);
    assert!(!first.lease.connection().sftp_available);
    assert_eq!(server.connections(), 2);
    assert_eq!(server.log.lock().unwrap().execs.len(), 1);
    // The inference is remembered for this manager lifetime, so subsequent sessions reuse the
    // compatible transport without probing again.
    let second = shell(&manager, "console").await;
    assert_eq!(second.transport, ShellTransport::Shared);
    assert_eq!(second.lease.connection().id, first.lease.connection().id);
    assert_eq!(server.connections(), 2);
    assert_eq!(server.log.lock().unwrap().execs.len(), 1);
}

#[tokio::test]
async fn does_not_share_transports_between_disable_sftp_and_console_modes() {
    let server = start(spec()).await;
    let sandbox = Sandbox::new();
    sandbox.write_config(&[
        "Host legacy console",
        "  HostName 127.0.0.1",
        "  User tester",
        &format!("  Port {}", server.port()),
        "  StrictHostKeyChecking accept-new",
        "  SetEnv MUXUS_TEST=from-config",
    ]);
    let mut options = sandbox.options();
    options.disable_sftp_for_host = Some(Arc::new(|alias: &str| alias == "legacy"));
    options.console_compatibility_for_host = Some(Arc::new(|alias: &str| alias == "console"));
    let manager = SshConnectionManager::new(options);
    let legacy = shell(&manager, "legacy").await;
    let console = shell(&manager, "console").await;
    assert_ne!(console.lease.connection().id, legacy.lease.connection().id);
    assert_eq!(server.connections(), 2);
    assert_eq!(server.log.lock().unwrap().env.len(), 1);
}

// --- jump-hops.test.ts ---

#[tokio::test]
async fn names_the_jump_hosts_in_the_session_summary() {
    let target = start(spec()).await;
    let jump = start(spec()).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &[
            host("app", target.port(), &["  ProxyJump bastion"]),
            host("bastion", jump.port(), &[]),
        ],
    );
    let shell = shell(&sandbox.manager(), "app").await;
    assert_eq!(shell.summary.jump_hosts, vec!["bastion".to_string()]);
    assert!(
        jump.events()
            .iter()
            .any(|e| e == &format!("direct-tcpip 127.0.0.1:{}", target.port()))
    );
    assert_eq!(target.connections(), 1);
}

#[tokio::test]
async fn duplicates_a_session_on_a_host_that_refuses_a_second_channel_without_a_second_jump_login()
{
    let mut t = spec();
    t.max_sessions = Some(1);
    let target = start(t).await;
    let jump = start(spec()).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &[
            host("app", target.port(), &["  ProxyJump bastion"]),
            host("bastion", jump.port(), &[]),
        ],
    );
    let manager = sandbox.manager();
    let first = shell(&manager, "app").await;
    let second = shell(&manager, "app").await;
    assert_eq!(second.transport, ShellTransport::Overflow);
    assert_eq!(target.connections(), 2);
    assert_eq!(jump.connections(), 1);
    assert_eq!(
        jump.auth_events()
            .iter()
            .filter(|e| e.contains("accepted"))
            .count(),
        1
    );
    drop(first);
}

#[tokio::test]
async fn reaches_another_host_behind_the_same_jump_host_over_its_open_connection() {
    let one = start(spec()).await;
    let two = start(spec()).await;
    let jump = start(spec()).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &[
            host("one", one.port(), &["  ProxyJump bastion"]),
            host("two", two.port(), &["  ProxyJump bastion"]),
            host("bastion", jump.port(), &[]),
        ],
    );
    let manager = sandbox.manager();
    let _a = shell(&manager, "one").await;
    let io = TestIo::accepting(PASSWORD);
    let _b = manager
        .connect_shell(
            &SshProfile::new("two"),
            io.clone(),
            80,
            24,
            "xterm",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(jump.connections(), 1);
    assert!(
        io.statuses()
            .iter()
            .any(|s| s.contains("via bastion over its open connection"))
    );
}

#[tokio::test]
async fn reports_a_target_the_jump_host_cannot_reach_without_logging_in_again() {
    let jump = start(spec()).await;
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = closed.local_addr().unwrap().port();
    drop(closed);
    let target = start(spec()).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &[
            host("app", target.port(), &["  ProxyJump bastion"]),
            host("dead", dead_port, &["  ProxyJump bastion"]),
            host("bastion", jump.port(), &[]),
        ],
    );
    let manager = sandbox.manager();
    let _app = shell(&manager, "app").await;
    let err = manager
        .connect(
            &SshProfile::new("dead"),
            TestIo::accepting(PASSWORD),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with(&format!("jump host could not reach 127.0.0.1:{dead_port}")),
        "{err}"
    );
    assert_eq!(jump.connections(), 1);
}

#[tokio::test]
async fn closes_the_jump_connection_only_after_the_last_chain_through_it() {
    let target = start(spec()).await;
    let jump = start(spec()).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &[
            host("app", target.port(), &["  ProxyJump bastion"]),
            host("bastion", jump.port(), &[]),
        ],
    );
    let mut options = sandbox.options();
    options.jump_hop_idle = Duration::from_millis(50);
    let manager = SshConnectionManager::new(options);
    let first = shell(&manager, "app").await;
    let second = manager
        .connect_shell(
            &SshProfile::new("app"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm",
            Some("g".into()),
            None,
        )
        .await
        .unwrap();
    let conn = first.lease.connection().clone();
    drop(first);
    tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .unwrap();
    // The second chain still rides its own jump route.
    assert!(!second.lease.connection().is_closed());
    let last = second.lease.connection().clone();
    drop(second);
    tokio::time::timeout(Duration::from_secs(5), last.closed())
        .await
        .unwrap();
}

#[tokio::test]
async fn ends_every_chain_through_a_jump_host_that_disconnects() {
    let one = start(spec()).await;
    let two = start(spec()).await;
    let jump = start(spec()).await;
    let sandbox = Sandbox::new();
    write(
        &sandbox,
        &[
            host("one", one.port(), &["  ProxyJump bastion"]),
            host("two", two.port(), &["  ProxyJump bastion"]),
            host("bastion", jump.port(), &[]),
        ],
    );
    let manager = sandbox.manager();
    let a = shell(&manager, "one").await;
    let b = shell(&manager, "two").await;
    let (ca, cb) = (a.lease.connection().clone(), b.lease.connection().clone());
    jump.disconnect_all().await;
    let reason_a = tokio::time::timeout(Duration::from_secs(10), ca.closed())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), cb.closed())
        .await
        .unwrap();
    assert_eq!(
        reason_a.as_deref(),
        Some("SSH jump host bastion disconnected.")
    );
    assert!(manager.list().is_empty());
}
