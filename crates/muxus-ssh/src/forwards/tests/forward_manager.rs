//! Port of tests/unit/server/forward-manager.test.ts.

use super::*;

/// A manager over the lab whose tunnel dials the test scripts.
fn scripted(
    lab: &Lab,
    tunnels: &SavedTunnels,
    delays: &[Duration],
) -> (Arc<ScriptedConnections>, ForwardManager) {
    let connections = ScriptedConnections::new(lab.manager.clone());
    let manager = ForwardManager::with_connections(connections.clone(), tunnels.options(delays));
    (connections, manager)
}

fn tunnel_request(conn_id: &str) -> ForwardRequest {
    ForwardRequest::dynamic(conn_id, 0).with_tunnel("tunnel-1")
}

// ForwardManager lifecycle

#[tokio::test]
async fn stops_terminal_owned_forwards_but_preserves_explicit_tunnels() {
    let lab = Lab::keyed().await;
    let first = lab.dial_db().await;
    let conn_id = first.connection().id.clone();
    let manager = ForwardManager::new(lab.manager.clone(), ForwardManagerOptions::default());

    let session = manager
        .start(ForwardRequest::dynamic(&conn_id, 0))
        .await
        .unwrap();
    let tunnel = manager.start(tunnel_request(&conn_id)).await.unwrap();

    assert_eq!(session.lifecycle, ForwardLifecycle::Session);
    assert_eq!(tunnel.lifecycle, ForwardLifecycle::Independent);
    assert_eq!(lab.forward_leases(&conn_id), 2);

    manager.stop_session_for_connection(&conn_id);

    assert_eq!(manager.list(None), vec![tunnel]);
    assert_eq!(lab.forward_leases(&conn_id), 1);
    manager.stop_all();
}

#[tokio::test]
async fn promotes_a_running_session_forward_when_it_is_saved_as_a_tunnel() {
    let lab = Lab::keyed().await;
    let first = lab.dial_db().await;
    let conn_id = first.connection().id.clone();
    let manager = ForwardManager::new(lab.manager.clone(), ForwardManagerOptions::default());
    let session = manager
        .start(ForwardRequest::dynamic(&conn_id, 0))
        .await
        .unwrap();

    manager.assign_tunnel(&session.id, "tunnel-1");
    manager.stop_session_for_connection(&conn_id);

    let list = manager.list(None);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, session.id);
    assert_eq!(list[0].tunnel_id.as_deref(), Some("tunnel-1"));
    assert_eq!(list[0].lifecycle, ForwardLifecycle::Independent);
    assert_eq!(lab.forward_leases(&conn_id), 1);
    manager.stop_all();
}

#[tokio::test]
async fn deduplicates_concurrent_config_forwards_on_a_multiplexed_connection() {
    let lab = Lab::keyed().await;
    let first = lab.dial_db().await;
    let conn_id = first.connection().id.clone();
    let manager = ForwardManager::new(lab.manager.clone(), ForwardManagerOptions::default());
    let request = ForwardRequest::dynamic(&conn_id, 0);

    let (a, b) = tokio::join!(
        manager.start_config(request.clone()),
        manager.start_config(request)
    );
    let (a, b) = (a.unwrap(), b.unwrap());

    assert_ne!(a.started, b.started);
    assert_eq!(a.info.id, b.info.id);
    assert_eq!(a.info.origin, ForwardOrigin::Config);
    assert_eq!(manager.list(None), vec![a.info.clone()]);
    assert_eq!(lab.forward_leases(&conn_id), 1);

    manager.stop(&a.info.id);
    assert_eq!(lab.forward_leases(&conn_id), 0);
}

#[tokio::test]
async fn config_starts_waiting_on_a_failed_start_fail_with_it() {
    let lab = Lab::keyed().await;
    let first = lab.dial_db().await;
    let conn_id = first.connection().id.clone();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let manager = ForwardManager::new(lab.manager.clone(), ForwardManagerOptions::default());
    let request = ForwardRequest::dynamic(&conn_id, port);

    let (a, b) = tokio::join!(
        manager.start_config(request.clone()),
        manager.start_config(request.clone())
    );
    let (a, b) = (a.unwrap_err(), b.unwrap_err());
    assert_eq!(a, b);
    assert_eq!(a.status, 400);
    assert!(
        a.message
            .starts_with(&format!("could not listen on 127.0.0.1:{port}: ")),
        "{}",
        a.message
    );
    assert!(manager.list(None).is_empty());

    // Nothing is left pending: once the port is free the rule starts.
    drop(taken);
    let started = manager.start_config(request).await.unwrap();
    assert!(started.started);
    manager.stop_all();
}

// ForwardManager tunnel reconnect

#[tokio::test]
async fn redials_a_dropped_tunnel_and_restarts_it_under_the_same_id() {
    let lab = Lab::keyed().await;
    let tunnels = SavedTunnels::new(vec![saved_tunnel()]);
    let (connections, manager) = scripted(&lab, &tunnels, &[ms(300)]);
    let first = lab.dial_db().await;
    let first_id = first.connection().id.clone();

    let started = manager.start(tunnel_request(&first_id)).await.unwrap();
    lab.drop_connection(&first, Some("Connection reset by peer"));

    let list = wait_for_status(&manager, ForwardStatus::Reconnecting).await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, started.id);
    assert_eq!(list[0].error.as_deref(), Some("Connection reset by peer"));
    assert_eq!(lab.forward_leases(&first_id), 0);

    let list = wait_for_status(&manager, ForwardStatus::Active).await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, started.id);
    assert_eq!(list[0].tunnel_id.as_deref(), Some("tunnel-1"));
    assert_eq!(list[0].error, None);
    let second_id = list[0].conn_id.clone();
    assert_ne!(second_id, first_id);
    assert_eq!(
        connections.profiles(),
        vec![SshProfile {
            keepalive_interval_seconds: Some(25),
            ..SshProfile::new("db")
        }]
    );
    assert_eq!(lab.forward_leases(&second_id), 1);

    manager.stop(&started.id);
    assert_eq!(lab.forward_leases(&second_id), 0);
}

#[tokio::test]
async fn redials_with_the_saved_tunnel_owned_ssh_profile() {
    let lab = Lab::keyed().await;
    let tunnel = TunnelRecord {
        target: "db.internal".into(),
        ssh_options: Some(TunnelSshOptions {
            user: Some("deploy".into()),
            proxy_jump: Some(vec!["bastion".into()]),
            ..TunnelSshOptions::default()
        }),
        ..saved_tunnel()
    };
    let tunnels = SavedTunnels::new(vec![tunnel]);
    let (connections, manager) =
        scripted(&lab, &tunnels, &[Duration::ZERO, Duration::from_secs(60)]);
    connections.then(Dial::Fail("connect ECONNREFUSED"));
    let first = lab.dial_db().await;

    manager
        .start(tunnel_request(&first.connection().id))
        .await
        .unwrap();
    lab.drop_connection(&first, None);

    wait_for(&manager, "a failed redial", |list| {
        list.first().and_then(|f| f.error.as_deref()) == Some("connect ECONNREFUSED")
    })
    .await;
    assert_eq!(
        connections.profiles()[0],
        SshProfile {
            use_config: Some(false),
            user: Some("deploy".into()),
            proxy_jump: Some(vec!["bastion".into()]),
            keepalive_interval_seconds: Some(25),
            ..SshProfile::new("db.internal")
        }
    );
    manager.stop_all();
}

#[tokio::test]
async fn stops_a_dropped_tunnel_that_does_not_reconnect_as_before() {
    let lab = Lab::keyed().await;
    let tunnels = SavedTunnels::new(vec![TunnelRecord {
        auto_reconnect: false,
        ..saved_tunnel()
    }]);
    let (connections, manager) = scripted(&lab, &tunnels, &[Duration::ZERO]);
    let first = lab.dial_db().await;
    let conn_id = first.connection().id.clone();

    manager.start(tunnel_request(&conn_id)).await.unwrap();
    let session = manager
        .start(ForwardRequest::dynamic(&conn_id, 0))
        .await
        .unwrap();
    assert_eq!(session.lifecycle, ForwardLifecycle::Session);
    lab.drop_connection(&first, None);

    wait_for(&manager, "an empty list", |list| list.is_empty()).await;
    settle().await;
    assert!(manager.list(None).is_empty());
    assert_eq!(connections.calls(), 0);
}

#[tokio::test]
async fn keeps_retrying_while_the_host_is_unreachable() {
    let lab = Lab::keyed().await;
    let tunnels = SavedTunnels::new(vec![saved_tunnel()]);
    let (connections, manager) = scripted(&lab, &tunnels, &[Duration::ZERO]);
    connections.script([
        Dial::Fail("connect ECONNREFUSED"),
        Dial::Fail("connect ETIMEDOUT"),
    ]);
    let first = lab.dial_db().await;

    manager
        .start(tunnel_request(&first.connection().id))
        .await
        .unwrap();
    let first_id = first.connection().id.clone();
    lab.drop_connection(&first, None);

    wait_for(&manager, "a forward on a new connection", |list| {
        list.first()
            .is_some_and(|f| f.status == ForwardStatus::Active && f.conn_id != first_id)
    })
    .await;
    assert_eq!(connections.calls(), 3);
    manager.stop_all();
}

#[tokio::test]
async fn gives_up_without_answering_when_the_dial_needs_the_user() {
    let lab = Lab::keyed().await;
    let tunnels = SavedTunnels::new(vec![saved_tunnel()]);
    let (connections, manager) = scripted(&lab, &tunnels, &[Duration::ZERO]);
    connections.script([Dial::Prompt]);
    let first = lab.dial_db().await;

    manager
        .start(tunnel_request(&first.connection().id))
        .await
        .unwrap();
    lab.drop_connection(&first, None);

    let list = wait_for_status(&manager, ForwardStatus::Error).await;
    assert!(
        list[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("Sign-in needed")),
        "{:?}",
        list[0].error
    );
    assert_eq!(connections.calls(), 1);
}

#[tokio::test]
async fn refuses_unknown_host_keys_while_redialing() {
    let lab = Lab::keyed().await;
    let tunnels = SavedTunnels::new(vec![saved_tunnel()]);
    let (connections, manager) = scripted(&lab, &tunnels, &[Duration::ZERO]);
    connections.script([Dial::HostKey]);
    let first = lab.dial_db().await;

    manager
        .start(tunnel_request(&first.connection().id))
        .await
        .unwrap();
    lab.drop_connection(&first, None);

    wait_for_status(&manager, ForwardStatus::Error).await;
    assert_eq!(connections.host_key_answers(), vec![false]);
}

#[tokio::test]
async fn backs_off_between_attempts_and_cancels_a_waiting_redial_on_stop() {
    let lab = Lab::keyed().await;
    let tunnels = SavedTunnels::new(vec![saved_tunnel()]);
    let (connections, manager) = scripted(&lab, &tunnels, &[ms(1_000), ms(5_000)]);
    connections.then(Dial::Fail("connect ENETUNREACH"));
    let first = lab.dial_db().await;

    let started = manager
        .start(tunnel_request(&first.connection().id))
        .await
        .unwrap();
    lab.drop_connection(&first, None);
    wait_for_status(&manager, ForwardStatus::Reconnecting).await;
    // From here on time only moves when the test says so.
    tokio::time::pause();

    advance(ms(700)).await;
    assert_eq!(connections.calls(), 0);
    advance(ms(500)).await;
    assert_eq!(connections.calls(), 1);
    let list = manager.list(None);
    assert_eq!(list[0].status, ForwardStatus::Reconnecting);
    assert_eq!(list[0].error.as_deref(), Some("connect ENETUNREACH"));
    // Timers fire on whole milliseconds, so each wait gets a little slack.
    advance(ms(4_900)).await;
    assert_eq!(connections.calls(), 1);
    advance(ms(200)).await;
    assert_eq!(connections.calls(), 2);
    // The last wait repeats.
    advance(ms(5_100)).await;
    assert_eq!(connections.calls(), 3);

    manager.stop(&started.id);
    advance(ms(60_000)).await;
    assert_eq!(connections.calls(), 3);
    assert!(manager.list(None).is_empty());
}

#[tokio::test]
async fn starts_over_at_the_shortest_wait_after_a_stable_connection() {
    let lab = Lab::keyed().await;
    let tunnels = SavedTunnels::new(vec![saved_tunnel()]);
    let (connections, manager) = scripted(&lab, &tunnels, &[ms(1_000), ms(5_000)]);
    connections.script([Dial::Fail("connect ENETUNREACH")]);
    let first = lab.dial_db().await;

    manager
        .start(tunnel_request(&first.connection().id))
        .await
        .unwrap();
    lab.drop_connection(&first, None);
    wait_for_status(&manager, ForwardStatus::Reconnecting).await;
    // One failure, then a connection that stays up past the stable mark.
    let list = wait_for_status(&manager, ForwardStatus::Active).await;
    assert_eq!(connections.calls(), 2);
    let second = lab
        .manager
        .acquire(
            &list[0].conn_id,
            crate::connection::ConnectionLeaseOwner::Dial,
        )
        .unwrap();
    tokio::time::pause();
    advance(TUNNEL_RECONNECT_STABLE).await;
    tokio::time::resume();

    connections.then(Dial::Fail("connect ENETUNREACH"));
    second
        .connection()
        .transport
        .set_close_reason("gone".into());
    lab.relay.cut();
    wait_for_status(&manager, ForwardStatus::Reconnecting).await;
    tokio::time::pause();
    // Back at the first wait, not the second.
    advance(ms(1_200)).await;
    assert_eq!(connections.calls(), 3);
    manager.stop_all();
}

#[tokio::test]
async fn drops_a_waiting_redial_once_auto_reconnect_is_switched_off() {
    let lab = Lab::keyed().await;
    let tunnels = SavedTunnels::new(vec![saved_tunnel()]);
    let (connections, manager) = scripted(&lab, &tunnels, &[Duration::from_secs(60)]);
    let first = lab.dial_db().await;

    manager
        .start(tunnel_request(&first.connection().id))
        .await
        .unwrap();
    lab.drop_connection(&first, None);
    wait_for_status(&manager, ForwardStatus::Reconnecting).await;
    assert_eq!(manager.list(None).len(), 1);

    tunnels.set(vec![TunnelRecord {
        auto_reconnect: false,
        ..saved_tunnel()
    }]);
    manager.tunnel_changed("tunnel-1");

    assert!(manager.list(None).is_empty());
    tokio::time::pause();
    advance(Duration::from_secs(60)).await;
    assert_eq!(connections.calls(), 0);
}

#[tokio::test]
async fn lets_a_manual_start_take_over_from_a_waiting_redial() {
    let lab = Lab::keyed().await;
    let tunnels = SavedTunnels::new(vec![saved_tunnel()]);
    let (connections, manager) = scripted(&lab, &tunnels, &[Duration::from_secs(60)]);
    let first = lab.dial_db().await;

    let dropped = manager
        .start(tunnel_request(&first.connection().id))
        .await
        .unwrap();
    lab.drop_connection(&first, None);
    wait_for_status(&manager, ForwardStatus::Reconnecting).await;
    let second = lab.dial_db().await;
    let restarted = manager
        .start(tunnel_request(&second.connection().id))
        .await
        .unwrap();

    let list = manager.list(None);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, restarted.id);
    assert_eq!(list[0].status, ForwardStatus::Active);
    assert_ne!(restarted.id, dropped.id);
    tokio::time::pause();
    advance(Duration::from_secs(60)).await;
    assert_eq!(connections.calls(), 0);
    manager.stop_all();
}

#[tokio::test]
async fn releases_a_remote_forward_whose_connection_is_already_gone() {
    let lab = Lab::keyed().await;
    let first = lab.dial_db().await;
    let conn_id = first.connection().id.clone();
    let manager = ForwardManager::new(lab.manager.clone(), ForwardManagerOptions::default());

    let request = ForwardRequest::to(
        &conn_id,
        ForwardType::Remote,
        free_port(),
        "localhost",
        3000,
    );
    manager.start(request).await.unwrap();
    assert_eq!(lab.forward_leases(&conn_id), 1);
    lab.drop_connection(&first, None);

    wait_for(&manager, "an empty list", |list| list.is_empty()).await;
    assert_eq!(lab.forward_leases(&conn_id), 0);
}

// ForwardManager tunnel autostart

#[tokio::test]
async fn starts_only_the_tunnels_marked_to_start_with_muxus() {
    let lab = Lab::keyed().await;
    lab.trust();
    let tunnels = vec![
        TunnelRecord {
            id: "db".into(),
            target: "db".into(),
            auto_start: true,
            auto_reconnect: false,
            ..saved_tunnel()
        },
        TunnelRecord {
            id: "manual".into(),
            target: "web".into(),
            auto_start: false,
            ..saved_tunnel()
        },
    ];
    let saved = SavedTunnels::new(tunnels.clone());
    let (connections, manager) = scripted(&lab, &saved, &[Duration::ZERO]);

    manager.autostart(&tunnels);

    let list = manager.list(None);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].tunnel_id.as_deref(), Some("db"));
    assert_eq!(list[0].status, ForwardStatus::Starting);
    assert_eq!(list[0].conn_id, "");
    assert_eq!(list[0].lifecycle, ForwardLifecycle::Independent);
    let list = wait_for_status(&manager, ForwardStatus::Active).await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].tunnel_id.as_deref(), Some("db"));
    assert!(lab.manager.list().iter().any(|c| c.id == list[0].conn_id));
    // No window has told the manager its keepalive preference yet.
    assert_eq!(
        connections.profiles(),
        vec![SshProfile {
            keepalive_interval_seconds: Some(30),
            ..SshProfile::new("db")
        }]
    );

    // A second pass never doubles a tunnel that is already up.
    manager.autostart(&tunnels);
    assert_eq!(manager.list(None).len(), 1);
    manager.stop_all();
}

#[tokio::test]
async fn shows_why_a_start_failed_when_the_tunnel_does_not_reconnect() {
    let lab = Lab::keyed().await;
    let tunnels = vec![TunnelRecord {
        auto_start: true,
        auto_reconnect: false,
        ..saved_tunnel()
    }];
    let (connections, manager) =
        scripted(&lab, &SavedTunnels::new(tunnels.clone()), &[Duration::ZERO]);
    connections.then(Dial::Fail("connect ENETUNREACH"));

    manager.autostart(&tunnels);

    let list = wait_for_status(&manager, ForwardStatus::Error).await;
    assert_eq!(list[0].error.as_deref(), Some("connect ENETUNREACH"));
    assert_eq!(connections.calls(), 1);
}

#[tokio::test]
async fn keeps_trying_a_failed_start_when_the_tunnel_reconnects() {
    let lab = Lab::keyed().await;
    lab.trust();
    let tunnels = vec![TunnelRecord {
        auto_start: true,
        auto_reconnect: true,
        ..saved_tunnel()
    }];
    let (connections, manager) =
        scripted(&lab, &SavedTunnels::new(tunnels.clone()), &[Duration::ZERO]);
    connections.script([Dial::Fail("connect ENETUNREACH")]);

    manager.autostart(&tunnels);

    wait_for_status(&manager, ForwardStatus::Active).await;
    assert_eq!(connections.calls(), 2);
    manager.stop_all();
}

#[tokio::test]
async fn waits_for_the_user_when_the_start_needs_a_prompt() {
    let lab = Lab::keyed().await;
    let tunnels = vec![TunnelRecord {
        auto_start: true,
        auto_reconnect: true,
        ..saved_tunnel()
    }];
    let (connections, manager) =
        scripted(&lab, &SavedTunnels::new(tunnels.clone()), &[Duration::ZERO]);
    connections.script([Dial::Prompt]);

    manager.autostart(&tunnels);

    let list = wait_for_status(&manager, ForwardStatus::Error).await;
    assert!(
        list[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("Sign-in needed")),
        "{:?}",
        list[0].error
    );
    assert_eq!(connections.calls(), 1);
}

#[tokio::test]
async fn lets_saving_or_deleting_the_tunnel_drop_a_start_it_no_longer_wants() {
    let lab = Lab::keyed().await;
    let tunnel = TunnelRecord {
        auto_start: true,
        auto_reconnect: true,
        ..saved_tunnel()
    };
    let saved = SavedTunnels::new(vec![tunnel.clone()]);
    let (connections, manager) = scripted(&lab, &saved, &[Duration::from_secs(60)]);
    connections.then(Dial::Fail("connect ETIMEDOUT"));

    manager.autostart(std::slice::from_ref(&tunnel));
    wait_for_status(&manager, ForwardStatus::Reconnecting).await;

    // Switching autostart off leaves a redial the tunnel still wants.
    saved.set(vec![TunnelRecord {
        auto_start: false,
        ..tunnel.clone()
    }]);
    manager.tunnel_changed(&tunnel.id);
    assert_eq!(manager.list(None).len(), 1);

    saved.set(Vec::new());
    manager.tunnel_changed(&tunnel.id);
    assert!(manager.list(None).is_empty());
    tokio::time::pause();
    advance(Duration::from_secs(60)).await;
    assert_eq!(connections.calls(), 1);
}
