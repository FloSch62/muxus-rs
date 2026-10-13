//! Port of tests/unit/server/tunnel-reconnect.test.ts: tunnel auto-reconnect over a real SSH
//! transport.

use super::*;

/// Servers, managers and a saved tunnel, with nothing dialed yet.
struct Prepared {
    lab: Lab,
    _echo: Echo,
    forwards: ForwardManager,
    tunnel: TunnelRecord,
}

async fn prepare(lab: Lab) -> Prepared {
    let echo = echo_server().await;
    let tunnel = TunnelRecord {
        id: "tunnel-1".into(),
        name: None,
        target: "127.0.0.1".into(),
        ssh_options: Some(TunnelSshOptions {
            user: Some("tester".into()),
            port: Some(lab.relay.port()),
            ..TunnelSshOptions::default()
        }),
        kind: ForwardType::Local,
        bind_port: free_port(),
        target_host: Some("127.0.0.1".into()),
        target_port: Some(echo.port),
        auto_start: true,
        auto_reconnect: true,
        created_at: "2026-10-08T00:00:00Z".into(),
        updated_at: "2026-10-08T00:00:00Z".into(),
    };
    let saved = tunnel.clone();
    let options = ForwardManagerOptions {
        tunnel: Some(Arc::new(move |id: &str| {
            (id == saved.id).then(|| saved.clone())
        })),
        reconnect_delays: vec![ms(200)],
    };
    let forwards = ForwardManager::new(lab.manager.clone(), options);
    Prepared {
        lab,
        _echo: echo,
        forwards,
        tunnel,
    }
}

impl Prepared {
    /// What the forwarding panel does first: dial with prompts (and trust the host key).
    async fn dial_interactively(&self) -> crate::connection::MuxedConnectionLease {
        self.lab.dial(&tunnel_profile(&self.tunnel, None)).await
    }

    /// A running tunnel, started the way the forwarding panel starts one.
    async fn start(&self) -> ForwardInfo {
        let dial = self.dial_interactively().await;
        let started = self
            .forwards
            .start(ForwardRequest {
                conn_id: dial.connection().id.clone(),
                kind: self.tunnel.kind,
                bind_port: self.tunnel.bind_port,
                target_host: self.tunnel.target_host.clone(),
                target_port: self.tunnel.target_port,
                tunnel_id: Some(self.tunnel.id.clone()),
            })
            .await
            .unwrap();
        dial.release();
        started
    }
}

impl Drop for Prepared {
    fn drop(&mut self) {
        self.forwards.stop_all();
        self.lab.manager.close_all();
    }
}

#[tokio::test]
async fn starts_with_muxus_once_the_host_is_trusted() {
    let t = prepare(Lab::keyed().await).await;

    // An unknown host key is never accepted on the user's behalf.
    t.forwards.autostart(std::slice::from_ref(&t.tunnel));
    assert_eq!(t.forwards.list(None)[0].status, ForwardStatus::Starting);
    let list = wait_for_status(&t.forwards, ForwardStatus::Error).await;
    assert!(
        list[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("Sign-in needed")),
        "{:?}",
        list[0].error
    );

    t.dial_interactively().await.release();
    t.forwards.stop(&list[0].id);

    t.forwards.autostart(std::slice::from_ref(&t.tunnel));
    wait_for_status(&t.forwards, ForwardStatus::Active).await;
    assert_eq!(
        echo_through(t.tunnel.bind_port, "started").await.unwrap(),
        "started"
    );
    // The refused first attempt, the trusting dial and the autostart.
    assert_eq!(t.lab.server.connections(), 3);
}

#[tokio::test]
async fn dials_again_and_carries_traffic_on_the_new_connection() {
    let t = prepare(Lab::keyed().await).await;
    let started = t.start().await;
    let first_conn_id = started.conn_id.clone();
    assert_eq!(
        echo_through(t.tunnel.bind_port, "before").await.unwrap(),
        "before"
    );

    t.lab.relay.cut();
    wait_for_status(&t.forwards, ForwardStatus::Reconnecting).await;
    let list = wait_for_status(&t.forwards, ForwardStatus::Active).await;

    assert_eq!(list[0].id, started.id);
    assert_eq!(list[0].tunnel_id.as_deref(), Some("tunnel-1"));
    assert_ne!(list[0].conn_id, first_conn_id);
    assert_eq!(t.lab.server.connections(), 2);
    assert_eq!(t.lab.relay.accepted(), 2);
    assert_eq!(
        echo_through(t.tunnel.bind_port, "after").await.unwrap(),
        "after"
    );
}

#[tokio::test]
async fn stops_and_asks_for_a_start_when_only_a_password_prompt_would_get_back_in() {
    let t = prepare(Lab::password_only().await).await;
    t.start().await;
    assert_eq!(
        echo_through(t.tunnel.bind_port, "before").await.unwrap(),
        "before"
    );

    t.lab.relay.cut();
    let list = wait_for_status(&t.forwards, ForwardStatus::Error).await;
    assert!(
        list[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("Sign-in needed")),
        "{:?}",
        list[0].error
    );
    // The listener is down while the tunnel waits for the user.
    let refused = echo_through(t.tunnel.bind_port, "x").await.unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::ConnectionRefused);
}
