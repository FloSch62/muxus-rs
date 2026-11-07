//! Fallback jump routes and SSH gateways (ported from `tests/unit/server/jump-fallbacks.test.ts`)
//! and hosts set to system OpenSSH (`connection-mux.test.ts`).

use std::collections::HashMap;

use muxus_core::protocol::SshGateway;
use russh::MethodKind;

use super::*;
use crate::connection::chain::ChainHop;
use crate::connection::gateways::{connect_gateway, gateway_candidates};
use crate::connection::leases::ConnectionLeaseOwner;
use crate::connection::manager::{ConnectOptions, ShellTransport, is_route_failure};
use crate::connection::profile::SshProfile;
use crate::connection::transport::{ConnectError, ConnectErrorKind};
use crate::testing::server::{ServerSpec, TestServer, start};
use crate::types::saved_host_hop;

const PASSWORD: &str = "secret";

fn spec() -> ServerSpec {
    ServerSpec::new()
        .password(PASSWORD)
        .methods(&[MethodKind::Password])
}

async fn closed_port() -> u16 {
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    probe.local_addr().unwrap().port()
}

/// Two bastions and an app host whose ssh_config ProxyJump is `primary`.
fn write_config(sandbox: &Sandbox, primary: u16, secondary: u16, app: u16, trust_new_keys: bool) {
    let host = |alias: &str, port: u16, extra: &[&str]| {
        let mut lines = vec![
            format!("Host {alias}"),
            "  HostName 127.0.0.1".into(),
            format!("  Port {port}"),
            "  User tester".into(),
            "  ConnectTimeout 3".into(),
        ];
        if trust_new_keys {
            lines.push("  StrictHostKeyChecking accept-new".into());
        }
        lines.extend(extra.iter().map(|s| s.to_string()));
        lines
    };
    let blocks = [
        vec!["Host *".to_string(), "  IdentityAgent none".into()],
        host("primary", primary, &[]),
        host("secondary", secondary, &[]),
        host("app", app, &["  ProxyJump primary"]),
    ];
    let lines: Vec<&str> = blocks.iter().flatten().map(String::as_str).collect();
    sandbox.write_config(&lines);
}

fn manager_with(sandbox: &Sandbox, fallbacks: &[(&str, Vec<Vec<&str>>)]) -> SshConnectionManager {
    let mut options = sandbox.options();
    let routes: HashMap<String, Vec<Vec<String>>> = fallbacks
        .iter()
        .map(|(alias, routes)| {
            (
                alias.to_string(),
                routes
                    .iter()
                    .map(|route| route.iter().map(|hop| hop.to_string()).collect())
                    .collect(),
            )
        })
        .collect();
    options.jump_fallbacks_for_host = Some(Arc::new(move |alias: &str| {
        routes.get(alias).cloned().unwrap_or_default()
    }));
    options.jump_hop_idle = Duration::from_millis(50);
    SshConnectionManager::new(options)
}

fn forwards(server: &TestServer) -> usize {
    server
        .events()
        .iter()
        .filter(|e| e.starts_with("direct-tcpip"))
        .count()
}

fn auths(server: &TestServer) -> usize {
    server
        .auth_events()
        .iter()
        .filter(|e| e.contains("accepted"))
        .count()
}

async fn shell(
    manager: &SshConnectionManager,
    profile: SshProfile,
    io: Arc<TestIo>,
) -> Result<crate::connection::TerminalShell, crate::connection::ConnectFailure> {
    manager
        .connect_shell(&profile, io, 80, 24, "xterm-256color", None, None)
        .await
}

#[tokio::test]
async fn goes_through_the_fallback_jump_host_when_the_first_cannot_be_reached() {
    let secondary = start(spec()).await;
    let app = start(spec()).await;
    let sandbox = Sandbox::new();
    write_config(
        &sandbox,
        closed_port().await,
        secondary.port(),
        app.port(),
        true,
    );
    let manager = manager_with(&sandbox, &[("app", vec![vec!["secondary"]])]);
    let io = TestIo::accepting(PASSWORD);

    let shell = shell(&manager, SshProfile::new("app"), io.clone())
        .await
        .unwrap();

    assert_eq!(shell.transport, ShellTransport::New);
    assert_eq!((auths(&secondary), forwards(&secondary)), (1, 1));
    assert_eq!(auths(&app), 1);
    assert!(
        io.statuses()
            .iter()
            .any(|line| line.starts_with("Route via primary failed")
                && line.contains("trying via secondary")),
        "{:?}",
        io.statuses()
    );
}

#[tokio::test]
async fn goes_through_the_fallback_when_the_first_jump_host_cannot_reach_the_target() {
    let mut refusing = spec();
    refusing.refuse_direct_tcpip = true;
    let primary = start(refusing).await;
    let secondary = start(spec()).await;
    let app = start(spec()).await;
    let sandbox = Sandbox::new();
    write_config(&sandbox, primary.port(), secondary.port(), app.port(), true);
    let manager = manager_with(&sandbox, &[("app", vec![vec!["secondary"]])]);

    shell(
        &manager,
        SshProfile::new("app"),
        TestIo::accepting(PASSWORD),
    )
    .await
    .unwrap();

    assert_eq!(auths(&primary), 1);
    assert_eq!(forwards(&secondary), 1);
    assert_eq!(auths(&app), 1);
}

#[tokio::test]
async fn takes_fallback_routes_from_a_profile_as_well() {
    let secondary = start(spec()).await;
    let app = start(spec()).await;
    let sandbox = Sandbox::new();
    write_config(
        &sandbox,
        closed_port().await,
        secondary.port(),
        app.port(),
        true,
    );
    let manager = sandbox.manager();
    let profile = SshProfile {
        proxy_jump_fallbacks: Some(vec![vec!["secondary".into()]]),
        ..SshProfile::new("app")
    };

    shell(&manager, profile, TestIo::accepting(PASSWORD))
        .await
        .unwrap();

    assert_eq!(forwards(&secondary), 1);
}

#[tokio::test]
async fn does_not_try_another_route_when_the_target_refuses_the_login() {
    let primary = start(spec()).await;
    let secondary = start(spec()).await;
    let app = start(
        ServerSpec::new()
            .password("another")
            .methods(&[MethodKind::Password]),
    )
    .await;
    let sandbox = Sandbox::new();
    write_config(&sandbox, primary.port(), secondary.port(), app.port(), true);
    let manager = manager_with(&sandbox, &[("app", vec![vec!["secondary"]])]);

    assert!(
        shell(
            &manager,
            SshProfile::new("app"),
            TestIo::accepting(PASSWORD)
        )
        .await
        .is_err()
    );

    assert!(forwards(&primary) > 0);
    assert_eq!(secondary.connections(), 0);
}

#[tokio::test]
async fn rides_a_live_fallback_route_instead_of_dialing_the_dead_first_route_again() {
    let secondary = start(spec()).await;
    let app = start(spec()).await;
    let sandbox = Sandbox::new();
    write_config(
        &sandbox,
        closed_port().await,
        secondary.port(),
        app.port(),
        true,
    );
    let manager = manager_with(&sandbox, &[("app", vec![vec!["secondary"]])]);

    let first = shell(
        &manager,
        SshProfile::new("app"),
        TestIo::accepting(PASSWORD),
    )
    .await
    .unwrap();
    let second = manager
        .connect(
            &SshProfile::new("app"),
            TestIo::accepting(PASSWORD),
            ConnectionLeaseOwner::Forward,
            ConnectOptions::default(),
        )
        .await
        .unwrap();

    assert_eq!(second.connection().id, first.lease.connection().id);
    assert_eq!(app.connections(), 1);
    second.release();
}

#[tokio::test]
async fn skips_a_route_that_cannot_be_planned_and_tries_the_next() {
    let secondary = start(spec()).await;
    let app = start(spec()).await;
    let sandbox = Sandbox::new();
    write_config(
        &sandbox,
        closed_port().await,
        secondary.port(),
        app.port(),
        true,
    );
    // The middle route names a saved jump host that no longer exists.
    let deleted = saved_host_hop("deleted-host");
    let manager = manager_with(
        &sandbox,
        &[("app", vec![vec![deleted.as_str()], vec!["secondary"]])],
    );
    let io = TestIo::accepting(PASSWORD);

    shell(&manager, SshProfile::new("app"), io.clone())
        .await
        .unwrap();

    assert_eq!(forwards(&secondary), 1);
    assert_eq!(
        io.statuses()
            .iter()
            .filter(|line| line.contains(" failed (") && line.contains("trying via"))
            .count(),
        2,
        "{:?}",
        io.statuses()
    );
}

#[tokio::test]
async fn stops_instead_of_routing_around_a_rejected_jump_host_key() {
    let primary = start(spec()).await;
    let secondary = start(spec()).await;
    let app = start(spec()).await;
    let sandbox = Sandbox::new();
    write_config(
        &sandbox,
        primary.port(),
        secondary.port(),
        app.port(),
        false,
    );
    let manager = manager_with(&sandbox, &[("app", vec![vec!["secondary"]])]);
    let io = TestIo::accepting(PASSWORD);
    *io.accept_host_keys.lock().unwrap() = false;

    let err = shell(&manager, SshProfile::new("app"), io)
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(err.to_lowercase().contains("host key"), "{err}");
    assert_eq!(secondary.connections(), 0);
}

#[tokio::test]
async fn keeps_a_route_the_caller_chose_instead_of_the_alias_fallbacks() {
    let secondary = start(spec()).await;
    let app = start(spec()).await;
    let sandbox = Sandbox::new();
    write_config(
        &sandbox,
        closed_port().await,
        secondary.port(),
        app.port(),
        true,
    );
    let manager = manager_with(&sandbox, &[("app", vec![vec!["secondary"]])]);
    let profile = SshProfile {
        proxy_jump: Some(vec!["primary".into()]),
        ..SshProfile::new("app")
    };

    let err = manager
        .connect(
            &profile,
            TestIo::accepting(PASSWORD),
            ConnectionLeaseOwner::Management,
            ConnectOptions::default(),
        )
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(err.contains("refused"), "{err}");
    assert_eq!(secondary.connections(), 0);
}

#[tokio::test]
async fn tries_the_route_that_worked_last_first_next_time() {
    let secondary = start(spec()).await;
    let app = start(spec()).await;
    let sandbox = Sandbox::new();
    write_config(
        &sandbox,
        closed_port().await,
        secondary.port(),
        app.port(),
        true,
    );
    let manager = manager_with(&sandbox, &[("app", vec![vec!["secondary"]])]);
    let first = manager
        .connect(
            &SshProfile::new("app"),
            TestIo::accepting(PASSWORD),
            ConnectionLeaseOwner::Forward,
            ConnectOptions::default(),
        )
        .await
        .unwrap();
    first.release();
    manager.close_all();

    let io = TestIo::accepting(PASSWORD);
    let again = manager
        .connect(
            &SshProfile::new("app"),
            io.clone(),
            ConnectionLeaseOwner::Forward,
            ConnectOptions::default(),
        )
        .await
        .unwrap();

    assert!(
        !io.statuses()
            .iter()
            .any(|line| line.contains("Route via primary failed"))
    );
    assert_eq!(forwards(&secondary), 2);
    again.release();
}

#[tokio::test]
async fn reports_the_last_failure_when_every_route_fails() {
    let app = start(spec()).await;
    let sandbox = Sandbox::new();
    write_config(
        &sandbox,
        closed_port().await,
        closed_port().await,
        app.port(),
        true,
    );
    let manager = manager_with(&sandbox, &[("app", vec![vec!["secondary"]])]);

    let err = shell(
        &manager,
        SshProfile::new("app"),
        TestIo::accepting(PASSWORD),
    )
    .await
    .err()
    .unwrap()
    .to_string();

    assert!(err.contains("refused"), "{err}");
    assert_eq!(app.connections(), 0);
}

// --- isRouteFailure ---

fn error(kind: ConnectErrorKind, message: &str) -> ConnectError {
    ConnectError::new(kind, message)
}

#[test]
fn counts_failures_on_a_jump_host_or_its_channel() {
    let auth = error(
        ConnectErrorKind::Authentication,
        "authentication to x failed",
    );
    assert!(is_route_failure(&auth, 2, 0, false));
    let unreachable = error(
        ConnectErrorKind::ChannelRefused,
        "jump host could not reach",
    );
    assert!(is_route_failure(&unreachable, 2, 1, true));
}

#[test]
fn leaves_the_target_login_a_cancelled_prompt_and_a_host_key_problem_alone() {
    let auth = error(
        ConnectErrorKind::Authentication,
        "authentication to x failed",
    );
    assert!(!is_route_failure(&auth, 2, 1, false));
    let cancelled = error(ConnectErrorKind::Cancelled, "authentication cancelled");
    assert!(!is_route_failure(&cancelled, 2, 0, false));
    let host_key = error(ConnectErrorKind::HostKey, "host key verification failed");
    assert!(!is_route_failure(&host_key, 2, 0, false));
}

#[test]
fn counts_a_direct_connection_that_never_got_through() {
    let timeout = error(ConnectErrorKind::Timeout, "connection to x:22 timed out");
    assert!(is_route_failure(&timeout, 1, 0, false));
    let auth = error(
        ConnectErrorKind::Authentication,
        "authentication to x failed",
    );
    assert!(!is_route_failure(&auth, 1, 0, false));
    let _ = std::marker::PhantomData::<ChainHop>;
}

// --- SSH gateway fallbacks ---

fn gateway(target: &str) -> SshGateway {
    SshGateway {
        target: target.into(),
        profile_id: None,
    }
}

fn gateways_config(sandbox: &Sandbox, hosts: &[(&str, u16)]) {
    let mut lines: Vec<String> = Vec::new();
    for (alias, port) in hosts {
        lines.extend([
            format!("Host {alias}"),
            "  HostName 127.0.0.1".into(),
            format!("  Port {port}"),
            "  User tester".into(),
            "  ConnectTimeout 3".into(),
            "  StrictHostKeyChecking accept-new".into(),
        ]);
    }
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    sandbox.write_config(&lines);
}

#[test]
fn lists_the_first_gateway_then_its_fallbacks() {
    assert!(gateway_candidates(None, None).is_empty());
    assert_eq!(
        gateway_candidates(Some(&gateway("a")), Some(&[gateway("b")])),
        vec![gateway("a"), gateway("b")]
    );
}

#[tokio::test]
async fn uses_the_first_gateway_that_answers() {
    let b = start(spec()).await;
    let sandbox = Sandbox::new();
    gateways_config(&sandbox, &[("a", closed_port().await), ("b", b.port())]);
    let manager = sandbox.manager();
    let io = TestIo::accepting(PASSWORD);

    let (lease, used) = connect_gateway(
        &manager,
        &[gateway("a"), gateway("b")],
        io.clone(),
        ConnectionLeaseOwner::Desktop,
        &|| false,
    )
    .await
    .unwrap();

    assert_eq!(used.target, "b");
    assert_eq!(lease.connection().port, b.port());
    assert!(
        io.statuses()
            .iter()
            .any(|line| line.starts_with("The SSH gateway a failed (")
                && line.ends_with("trying b …"))
    );
}

#[tokio::test]
async fn stops_at_a_cancelled_sign_in() {
    let a = start(spec()).await;
    let b = start(spec()).await;
    let sandbox = Sandbox::new();
    gateways_config(&sandbox, &[("a", a.port()), ("b", b.port())]);
    let manager = sandbox.manager();
    let io = Arc::new(TestIo::default());
    *io.accept_host_keys.lock().unwrap() = true;

    let err = connect_gateway(
        &manager,
        &[gateway("a"), gateway("b")],
        io,
        ConnectionLeaseOwner::Desktop,
        &|| false,
    )
    .await
    .err()
    .unwrap();

    assert!(err.contains("cancelled"), "{err}");
    assert_eq!(b.connections(), 0);
}

#[tokio::test]
async fn stops_at_a_rejected_gateway_host_key() {
    let a = start(spec()).await;
    let b = start(spec()).await;
    let sandbox = Sandbox::new();
    sandbox.write_config(&[
        "Host a",
        "  HostName 127.0.0.1",
        &format!("  Port {}", a.port()),
        "Host b",
        "  HostName 127.0.0.1",
        &format!("  Port {}", b.port()),
    ]);
    let manager = sandbox.manager();
    let io = TestIo::accepting(PASSWORD);
    *io.accept_host_keys.lock().unwrap() = false;

    let err = connect_gateway(
        &manager,
        &[gateway("a"), gateway("b")],
        io,
        ConnectionLeaseOwner::Desktop,
        &|| false,
    )
    .await
    .err()
    .unwrap();

    assert!(err.to_lowercase().contains("host key"), "{err}");
    assert_eq!(b.connections(), 0);
}

// --- hosts set to system OpenSSH ---

#[tokio::test]
async fn hosts_set_to_system_openssh_are_not_dialed_by_the_built_in_client() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&["Host krb", "  HostName 127.0.0.1"]);
    let mut options = sandbox.options();
    options.system_open_ssh_for_host = Some(Arc::new(|alias: &str| alias == "krb"));
    let manager = SshConnectionManager::new(options);
    let profile = SshProfile::new("krb");

    assert!(manager.uses_system_open_ssh(&profile));
    assert!(!manager.uses_system_open_ssh(&SshProfile::new("other")));
    let err = manager
        .connect(
            &profile,
            TestIo::accepting(PASSWORD),
            ConnectionLeaseOwner::Forward,
            ConnectOptions::default(),
        )
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("uses system OpenSSH"), "{err}");
}
