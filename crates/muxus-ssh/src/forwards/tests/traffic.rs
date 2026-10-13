//! Traffic through each kind of forward, the requests the manager refuses, the published list
//! and the JSON the forwarding panel sees.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::*;
use crate::types::{ConfigForward, ConfigForwardExt};

async fn lab_with_manager() -> (Lab, crate::connection::MuxedConnectionLease, ForwardManager) {
    let lab = Lab::keyed().await;
    let first = lab.dial_db().await;
    let manager = ForwardManager::new(lab.manager.clone(), ForwardManagerOptions::default());
    (lab, first, manager)
}

/// Greets a SOCKS5 forward and asks it to CONNECT; returns the socket and the reply code.
async fn socks_connect(port: u16, address: &[u8], target_port: u16) -> (TcpStream, u8) {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    socket.write_all(&[5, 1, 0]).await.unwrap();
    let mut choice = [0u8; 2];
    socket.read_exact(&mut choice).await.unwrap();
    assert_eq!(choice, [5, 0]);
    let mut request = vec![5, 1, 0];
    request.extend_from_slice(address);
    request.extend_from_slice(&target_port.to_be_bytes());
    socket.write_all(&request).await.unwrap();
    let mut reply = [0u8; 10];
    socket.read_exact(&mut reply).await.unwrap();
    (socket, reply[1])
}

async fn round_trip(socket: &mut TcpStream, text: &str) -> String {
    socket.write_all(text.as_bytes()).await.unwrap();
    let mut reply = vec![0u8; text.len()];
    socket.read_exact(&mut reply).await.unwrap();
    String::from_utf8(reply).unwrap()
}

#[tokio::test]
async fn a_local_forward_carries_traffic_to_its_target() {
    let (lab, first, manager) = lab_with_manager().await;
    let echo = echo_server().await;
    let port = free_port();

    let info = manager
        .start(ForwardRequest::to(
            &first.connection().id,
            ForwardType::Local,
            port,
            "127.0.0.1",
            echo.port,
        ))
        .await
        .unwrap();

    assert_eq!(info.status, ForwardStatus::Active);
    assert_eq!(info.origin, ForwardOrigin::Manual);
    assert_eq!(echo_through(port, "hello").await.unwrap(), "hello");
    assert!(
        lab.server
            .events()
            .contains(&format!("direct-tcpip 127.0.0.1:{}", echo.port))
    );

    manager.stop(&info.id);
    settle().await;
    assert!(echo_through(port, "x").await.is_err());
}

#[tokio::test]
async fn a_remote_forward_routes_server_connections_to_the_local_target() {
    let (lab, first, manager) = lab_with_manager().await;
    let echo = echo_server().await;
    let port = free_port();

    let info = manager
        .start(ForwardRequest::to(
            &first.connection().id,
            ForwardType::Remote,
            port,
            "127.0.0.1",
            echo.port,
        ))
        .await
        .unwrap();

    assert!(
        lab.server
            .events()
            .contains(&format!("tcpip-forward 127.0.0.1:{port} -> {port}"))
    );
    // The in-process server listens on the remote port of the same machine.
    assert_eq!(
        echo_through(port, "from the server").await.unwrap(),
        "from the server"
    );

    manager.stop(&info.id);
    settle().await;
    assert!(echo_through(port, "x").await.is_err());
}

#[tokio::test]
async fn a_dynamic_forward_speaks_socks5() {
    let (_lab, first, manager) = lab_with_manager().await;
    let echo = echo_server().await;
    let port = free_port();
    manager
        .start(ForwardRequest::dynamic(&first.connection().id, port))
        .await
        .unwrap();

    let (mut socket, code) = socks_connect(port, &[1, 127, 0, 0, 1], echo.port).await;
    assert_eq!(code, 0);
    assert_eq!(round_trip(&mut socket, "by address").await, "by address");

    let mut domain = vec![3, 9];
    domain.extend_from_slice(b"localhost");
    let (mut socket, code) = socks_connect(port, &domain, echo.port).await;
    assert_eq!(code, 0);
    assert_eq!(round_trip(&mut socket, "by name").await, "by name");

    // Nothing listens there: the server refuses the channel, and the client hears so.
    let (_socket, code) = socks_connect(port, &[1, 127, 0, 0, 1], free_port()).await;
    assert_eq!(code, 5);
    manager.stop_all();
}

#[tokio::test]
async fn config_forwards_start_from_the_connection_rules() {
    let (lab, first, manager) = lab_with_manager().await;
    let echo = echo_server().await;
    let rule = ConfigForward::to(ForwardType::Local, free_port(), "127.0.0.1", echo.port);
    let conn_id = first.connection().id.clone();

    let started = manager
        .start_config(ForwardRequest::from_config(&conn_id, &rule))
        .await
        .unwrap();
    assert!(started.started);
    assert_eq!(started.info.origin, ForwardOrigin::Config);
    assert_eq!(started.info.lifecycle, ForwardLifecycle::Session);
    assert_eq!(
        echo_through(rule.bind_port, "config").await.unwrap(),
        "config"
    );

    // A second session on the shared transport finds the rule running.
    let again = manager
        .start_config(ForwardRequest::from_config(&conn_id, &rule))
        .await
        .unwrap();
    assert!(!again.started);
    assert_eq!(again.info.id, started.info.id);

    manager.stop_session_for_connection(&conn_id);
    assert!(manager.list(None).is_empty());
    assert_eq!(lab.forward_leases(&conn_id), 0);
}

#[tokio::test]
async fn refuses_requests_it_cannot_serve() {
    let (_lab, first, manager) = lab_with_manager().await;
    let conn_id = first.connection().id.clone();

    let missing = manager
        .start(ForwardRequest::dynamic("no-such-connection", 0))
        .await
        .unwrap_err();
    assert_eq!(
        (missing.status, missing.message.as_str()),
        (404, "connection not found")
    );

    let mut no_target = ForwardRequest::dynamic(&conn_id, 0);
    no_target.kind = ForwardType::Local;
    let invalid = manager.start(no_target).await.unwrap_err();
    assert_eq!(
        (invalid.status, invalid.message.as_str()),
        (
            400,
            "targetHost and targetPort are required for local/remote forwards"
        )
    );

    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let busy = manager
        .start(ForwardRequest::dynamic(&conn_id, port))
        .await
        .unwrap_err();
    assert_eq!(busy.status, 400);
    assert!(
        busy.message
            .starts_with(&format!("could not listen on 127.0.0.1:{port}: ")),
        "{}",
        busy.message
    );

    let remote = manager
        .start(ForwardRequest::to(
            &conn_id,
            ForwardType::Remote,
            port,
            "127.0.0.1",
            9,
        ))
        .await
        .unwrap_err();
    assert_eq!(remote.status, 400);
    assert!(
        remote.message.starts_with("remote bind failed: "),
        "{}",
        remote.message
    );

    assert!(manager.list(None).is_empty());
}

#[tokio::test]
async fn publishes_every_change_to_the_list() {
    let (_lab, first, manager) = lab_with_manager().await;
    let mut changes = manager.subscribe();
    assert!(changes.borrow_and_update().is_empty());

    let info = manager
        .start(ForwardRequest::dynamic(&first.connection().id, 0))
        .await
        .unwrap();
    assert!(changes.has_changed().unwrap());
    assert_eq!(*changes.borrow_and_update(), vec![info.clone()]);

    let saved = manager.assign_tunnel(&info.id, "tunnel-9").unwrap();
    assert_eq!(*changes.borrow_and_update(), vec![saved]);

    manager.stop(&info.id);
    assert!(changes.borrow_and_update().is_empty());
}

#[tokio::test]
async fn lists_forwards_by_connection() {
    let lab = Lab::keyed().await;
    let first = lab.dial_db().await;
    // A dedicated second transport to the same host.
    let second = lab
        .manager
        .connect(
            &SshProfile::new("db"),
            crate::connection::tests::TestIo::accepting(PASSWORD),
            crate::connection::ConnectionLeaseOwner::Dial,
            crate::connection::ConnectOptions {
                dedicated_transport: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let manager = ForwardManager::new(lab.manager.clone(), ForwardManagerOptions::default());
    let a = manager
        .start(ForwardRequest::dynamic(&first.connection().id, 0))
        .await
        .unwrap();
    let b = manager
        .start(ForwardRequest::dynamic(&second.connection().id, 0))
        .await
        .unwrap();

    assert_eq!(manager.list(None), vec![a.clone(), b.clone()]);
    assert_eq!(manager.list(Some("")), vec![a.clone(), b.clone()]);
    assert_eq!(manager.list(Some(&first.connection().id)), vec![a]);
    assert_eq!(manager.list(Some(&second.connection().id)), vec![b]);
    manager.stop_all();
}

#[test]
fn serializes_like_the_typescript_dtos() {
    let info = ForwardInfo {
        id: "Ab3_xY-z".into(),
        conn_id: "c1".into(),
        kind: ForwardType::Dynamic,
        bind_port: 1080,
        target_host: None,
        target_port: None,
        origin: ForwardOrigin::Manual,
        lifecycle: ForwardLifecycle::Independent,
        status: ForwardStatus::Reconnecting,
        error: Some("The SSH connection was lost.".into()),
        tunnel_id: Some("t1".into()),
    };
    assert_eq!(
        serde_json::to_value(&info).unwrap(),
        serde_json::json!({
            "id": "Ab3_xY-z",
            "connId": "c1",
            "type": "dynamic",
            "bindPort": 1080,
            "origin": "manual",
            "lifecycle": "independent",
            "status": "reconnecting",
            "error": "The SSH connection was lost.",
            "tunnelId": "t1",
        })
    );

    let request: ForwardRequest = serde_json::from_value(serde_json::json!({
        "connId": "c1", "type": "local", "bindPort": 8080, "targetHost": "db", "targetPort": 5432,
    }))
    .unwrap();
    assert_eq!(
        request,
        ForwardRequest::to("c1", ForwardType::Local, 8080, "db", 5432)
    );

    let tunnel = serde_json::json!({
        "id": "t1",
        "target": "db.internal",
        "sshOptions": { "user": "deploy", "proxyJump": ["bastion"] },
        "type": "remote",
        "bindPort": 9000,
        "targetHost": "localhost",
        "targetPort": 3000,
        "autoStart": true,
        "autoReconnect": false,
        "createdAt": "2026-10-08T00:00:00Z",
        "updatedAt": "2026-10-08T00:00:00Z",
    });
    let record: TunnelRecord = serde_json::from_value(tunnel.clone()).unwrap();
    assert_eq!(
        record.ssh_options.as_ref().and_then(|o| o.user.as_deref()),
        Some("deploy")
    );
    assert_eq!(serde_json::to_value(&record).unwrap(), tunnel);
    // Tunnel-owned settings are a closed set.
    assert!(
        serde_json::from_value::<TunnelSshOptions>(serde_json::json!({ "password": "x" })).is_err()
    );
}

#[test]
fn builds_the_dial_of_a_saved_tunnel() {
    let alias = saved_tunnel();
    assert_eq!(tunnel_profile(&alias, None), SshProfile::new("db"));
    let owned = TunnelRecord {
        target: "10.0.0.5".into(),
        ssh_options: Some(TunnelSshOptions {
            user: Some("deploy".into()),
            port: Some(2222),
            identity_files: Some(vec!["~/.ssh/deploy".into()]),
            identities_only: Some(true),
            forward_agent: Some(false),
            proxy_jump: Some(vec!["bastion".into()]),
            password_only: Some(false),
        }),
        ..saved_tunnel()
    };
    assert_eq!(
        tunnel_profile(&owned, Some(15)),
        SshProfile {
            use_config: Some(false),
            keepalive_interval_seconds: Some(15),
            user: Some("deploy".into()),
            port: Some(2222),
            identity_files: Some(vec!["~/.ssh/deploy".into()]),
            identities_only: Some(true),
            forward_agent: Some(false),
            proxy_jump: Some(vec!["bastion".into()]),
            password_only: Some(false),
            ..SshProfile::new("10.0.0.5")
        }
    );
    // An empty object still means a tunnel-owned profile.
    let bare = TunnelRecord {
        ssh_options: Some(TunnelSshOptions::default()),
        ..saved_tunnel()
    };
    assert_eq!(tunnel_profile(&bare, None).use_config, Some(false));
}
