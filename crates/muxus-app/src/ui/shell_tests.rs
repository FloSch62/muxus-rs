//! The main window's shell in a headless window against a live SSH server: what only the
//! assembled window does (its top bar, its terminals' stored output, its dialogs).

#[path = "../../tests/support/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use gpui::{TestAppContext, WindowHandle};
use muxus_app::data_source::BackendSource;
use muxus_app::features::settings::SettingsPrefs;
use muxus_app::sidebar::folder_prefs::FolderPrefs;
use muxus_ui::gpui_kit::AppContext as _;
use support::{Fixture, allow_backend_threads, wait_until};

use super::shell::AppShell;

/// A main window on the fixture's backend, as `ui::run` opens one.
fn main_window(fx: &Fixture, cx: &mut TestAppContext) -> WindowHandle<AppShell> {
    allow_backend_threads(cx);
    let backend = fx.backend.clone();
    cx.update(|cx| {
        muxus_ui::init(muxus_ui::theme::Mode::Light, cx);
        SettingsPrefs::init(backend.clone(), Default::default(), cx);
        super::features::install(&backend, cx);
    });
    let backend = fx.backend.clone();
    cx.add_window(move |window, cx| {
        let mut shell = AppShell::new(
            Arc::new(BackendSource::new(backend)),
            muxus_app::model::Catalog::default(),
            FolderPrefs::default(),
            Default::default(),
            window,
            cx,
        );
        shell.install_window_features(None, window, cx);
        shell
    })
}

/// The top bar's forwarding badge counted forwards only when the launcher started one: a
/// forward started anywhere else (the forwarding panel, a host's ssh-config lines) never
/// showed. It follows the backend's forward list now.
#[gpui::test]
fn the_forwarding_badge_follows_every_forward(cx: &mut TestAppContext) {
    let fx = Fixture::start(|_| {});
    let window = main_window(&fx, cx);
    let forwards = |cx: &mut TestAppContext| {
        window
            .update(cx, |shell, _, _| shell.forwards.total)
            .unwrap()
    };
    assert_eq!(forwards(cx), 0);
    let bind = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let request: muxus_core::api::ForwardRequest = serde_json::from_value(serde_json::json!({
        "connId": fx.conn_id,
        "type": "dynamic",
        "bindPort": bind,
    }))
    .unwrap();
    let started = fx
        .runtime
        .block_on(fx.backend.start_forward(request))
        .expect("the forward starts");
    wait_until(cx, Duration::from_secs(10), "the badge", |cx| {
        forwards(cx) == 1
    });
    let tooltip = window
        .update(cx, |shell, _, _| shell.forwards.tooltip())
        .unwrap();
    assert_eq!(tooltip, "Port forwarding & tunnels — 1 active");
    fx.runtime
        .block_on(fx.backend.stop_forward(&started.id))
        .unwrap();
    wait_until(cx, Duration::from_secs(10), "the badge to go", |cx| {
        forwards(cx) == 0
    });
}

/// Muxus-rs never stored a terminal's output: a restored tab came back empty. Output is
/// stored while it arrives (and before the window closes) and replays when the tab is
/// restored.
#[gpui::test]
fn terminal_output_is_stored_for_a_restore(cx: &mut TestAppContext) {
    let fx = Fixture::start(|_| {});
    let window = main_window(&fx, cx);
    let tab_id = window
        .update(cx, |shell, window, cx| {
            shell.prefs.restore_scrollback = true;
            let id = shell.tabs.open(muxus_app::model::ssh_profile("box"), "box");
            shell.tabs_changed(cx);
            window.refresh();
            id
        })
        .unwrap();
    let connected = |cx: &mut TestAppContext| {
        window
            .update(cx, |shell, window, cx| {
                window.refresh();
                let _ = cx;
                shell
                    .tabs
                    .tab(&tab_id)
                    .is_some_and(|tab| tab.status == muxus_app::state::tabs::TabStatus::Connected)
            })
            .unwrap()
    };
    wait_until(cx, Duration::from_secs(20), "the session", |cx| {
        connected(cx)
    });
    // The test server's shell greets; that output is the tab's.
    wait_until(cx, Duration::from_secs(10), "some output", |cx| {
        window
            .update(cx, |shell, _, cx| {
                shell
                    .terminals
                    .get(&tab_id)
                    .is_some_and(|terminal| terminal.view.read(cx).snapshot(None).is_some())
            })
            .unwrap()
    });
    // Closing the window stores it before it goes.
    cx.update_window(window.into(), |_, window, cx| {
        muxus_ui::widgets::window_frame::request_close(window, cx)
    })
    .unwrap();
    let stored = fx
        .runtime
        .block_on(fx.backend.terminal_snapshot(&tab_id))
        .unwrap();
    assert!(stored.is_some_and(|snapshot| !snapshot.data.is_empty()));
}

/// A tab restored from a workspace replays its stored output first, then offers to
/// reconnect below it.
#[gpui::test]
fn a_restored_tab_replays_its_stored_output(cx: &mut TestAppContext) {
    let fx = Fixture::start(|_| {});
    let window = main_window(&fx, cx);
    let tab_id = window
        .update(cx, |shell, _, _| {
            shell.prefs.restore_scrollback = true;
            let id = shell.tabs.open(muxus_app::model::ssh_profile("box"), "box");
            if let Some(tab) = shell.tabs.tabs.iter_mut().find(|tab| tab.id == id) {
                tab.restored = true;
                tab.connect_on_mount = false;
            }
            id
        })
        .unwrap();
    fx.runtime
        .block_on(fx.backend.save_terminal_snapshot(
            &tab_id,
            "remembered output\r\n",
            Some(muxus_term::scrollback::TERMINAL_SNAPSHOT_FORMAT_VERSION),
        ))
        .unwrap();
    window
        .update(cx, |shell, window, cx| {
            shell.tabs_changed(cx);
            window.refresh();
        })
        .unwrap();
    wait_until(cx, Duration::from_secs(10), "the replay", |cx| {
        window
            .update(cx, |shell, window, cx| {
                window.refresh();
                shell.terminals.get(&tab_id).is_some_and(|terminal| {
                    let text = terminal.view.read(cx).emulator().buffer_text();
                    text.contains("remembered output") && text.contains("Press any key")
                })
            })
            .unwrap()
    });
    let text = window
        .update(cx, |shell, _, cx| {
            shell.terminals[&tab_id]
                .view
                .read(cx)
                .emulator()
                .buffer_text()
        })
        .unwrap();
    let replay = text.find("remembered output").unwrap();
    let notice = text.find("[restored session]").unwrap();
    assert!(replay < notice, "{text}");
}

/// A program reporting with OSC 7501 marks its tab: work in progress until the next prompt,
/// a finished result until the tab is looked at.
#[gpui::test]
fn programs_mark_their_tab_until_it_is_looked_at(cx: &mut TestAppContext) {
    use muxus_term::program_status::ProgramState;

    let fx = Fixture::start(|_| {});
    let window = main_window(&fx, cx);
    let build = window
        .update(cx, |shell, window, cx| {
            let build = shell
                .tabs
                .open(muxus_app::model::ssh_profile("box"), "build");
            shell.tabs_changed(cx);
            window.refresh();
            build
        })
        .unwrap();
    wait_until(cx, Duration::from_secs(20), "the session", |cx| {
        window
            .update(cx, |shell, window, _| {
                window.refresh();
                shell
                    .tabs
                    .tab(&build)
                    .is_some_and(|tab| tab.status == muxus_app::state::tabs::TabStatus::Connected)
            })
            .unwrap()
    });
    // The user is in another window.
    window
        .update(cx, |shell, _, _| shell.window_active = false)
        .unwrap();
    let output = |cx: &mut TestAppContext, data: &str| {
        window
            .update(cx, |shell, window, cx| {
                let view = shell.terminals[&build].view.clone();
                view.update(cx, |view, cx| view.write_output(data.as_bytes(), cx));
                window.refresh();
            })
            .unwrap();
        cx.run_until_parked();
    };
    let shown = |cx: &mut TestAppContext| {
        window
            .update(cx, |shell, _, _| {
                shell
                    .program_status
                    .summary(&build)
                    .map(|record| (record.state, record.app))
            })
            .unwrap()
    };

    output(cx, "\x1b]7501;state=working:app=make\x07");
    assert_eq!(
        shown(cx),
        Some((ProgramState::Working, Some("make".into())))
    );
    // A new prompt ends the work in progress.
    output(cx, "\x1b]133;A\x07");
    assert_eq!(shown(cx), None);

    // The build finishes while nobody looks: the result stays, past the next prompt.
    output(cx, "\x1b]7501;state=error:app=make\x07\x1b]133;A\x07");
    assert_eq!(shown(cx), Some((ProgramState::Error, Some("make".into()))));
    // Coming back to the window with the tab active is looking at it.
    window
        .update(cx, |shell, window, _| {
            shell.window_active = true;
            window.refresh();
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(shown(cx), None);
    // A result that finishes in the tab being watched never shows.
    output(cx, "\x1b]7501;state=done:id=again\x07");
    assert_eq!(shown(cx), None);

    // Turned off, reports are not taken in.
    window
        .update(cx, |shell, _, cx| {
            shell.prefs.show_program_status = false;
            shell.refresh_terminal_settings(cx);
        })
        .unwrap();
    output(cx, "\x1b]7501;state=blocked:id=x\x07");
    assert_eq!(shown(cx), None);
}
