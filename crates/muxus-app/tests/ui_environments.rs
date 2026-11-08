//! Containers and VMs: the window's hub and the panel against a live SSH connection, in a
//! headless window. The test server answers commands without a shell, so it is a host that
//! "runs no commands": automatic discovery leaves it alone, an explicit reading does not.

mod support;

use std::time::Duration;

use gpui::TestAppContext;
use muxus_app::features::environments::hub::{EnvironmentsHub, HubInput, PanelScope};
use muxus_app::features::environments::model::{EnvironmentStatus, LOCAL_ENVIRONMENT_KEY, LiveTab};
use muxus_app::features::environments::panel::EnvironmentsPanel;
use muxus_app::model::ssh_profile;
use muxus_ui::gpui_kit::AppContext;
use support::{Fixture, allow_backend_threads, init_ui, wait_until};

#[gpui::test]
fn reads_connected_hosts_that_run_commands_and_the_panel_reads_the_rest(cx: &mut TestAppContext) {
    let fx = Fixture::start(|_| {});
    allow_backend_threads(cx);
    cx.update(|cx| {
        init_ui(&fx.backend, cx);
    });
    let config = fx
        .runtime
        .block_on(fx.backend.ssh_config())
        .expect("ssh_config reads");
    let backend = fx.backend.clone();
    let hub = cx.update(|cx| cx.new(|_| EnvironmentsHub::new(backend)));
    hub.update(cx, |hub, cx| {
        hub.set_catalog(config.hosts.clone(), Vec::new(), cx);
        hub.sync(
            HubInput {
                tabs: vec![LiveTab {
                    profile: Some(ssh_profile("box")),
                    connected: true,
                    conn_id: Some(fx.conn_id.clone()),
                    runs_commands: true,
                }],
                discover: true,
                refresh_seconds: 0.,
                keepalive_seconds: 0.,
                visible: true,
            },
            cx,
        );
    });

    // This computer is read at start; the connected host is asked only whether it runs
    // commands, which the test server never showed.
    wait_until(cx, Duration::from_secs(30), "both hosts to be read", |cx| {
        hub.read_with(cx, |hub, _| {
            let local = hub.entry(LOCAL_ENVIRONMENT_KEY).map(|entry| entry.status);
            let remote = hub.entry("ssh:box").map(|entry| entry.status);
            local.is_some_and(|status| status != EnvironmentStatus::Loading)
                && remote == Some(EnvironmentStatus::Unsupported)
        })
    });

    // The panel on that host reads it on request, over the same connection.
    let window = cx.add_window({
        let hub = hub.clone();
        move |window, cx| EnvironmentsPanel::new(hub, window, cx)
    });
    hub.update(cx, |hub, cx| {
        hub.set_panel(PanelScope::Host("ssh:box".into()), cx)
    });
    wait_until(cx, Duration::from_secs(30), "the explicit reading", |cx| {
        hub.read_with(cx, |hub, _| {
            hub.entry("ssh:box")
                .is_some_and(|entry| entry.status == EnvironmentStatus::Ready && !entry.automatic)
        })
    });
    window
        .update(cx, |_, window, _| window.refresh())
        .expect("the panel draws");
    cx.run_until_parked();

    // The tab disconnects: the host is forgotten once the panel no longer shows it.
    hub.update(cx, |hub, cx| {
        hub.sync(
            HubInput {
                tabs: Vec::new(),
                discover: true,
                refresh_seconds: 0.,
                keepalive_seconds: 0.,
                visible: true,
            },
            cx,
        );
        hub.set_panel(PanelScope::Closed, cx);
    });
    cx.run_until_parked();
    hub.read_with(cx, |hub, _| {
        assert!(hub.entry("ssh:box").is_none());
        assert!(hub.entry(LOCAL_ENVIRONMENT_KEY).is_some());
    });
}
