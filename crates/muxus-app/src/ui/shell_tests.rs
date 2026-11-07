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

/// A paced paste shows its progress on the tab and over the terminal while the backend types
/// it, and cancelling it there stops it.
#[gpui::test]
fn a_paced_paste_shows_its_progress_until_it_is_cancelled(cx: &mut TestAppContext) {
    let fx = Fixture::start(|_| {});
    let window = main_window(&fx, cx);
    let tab_id = window
        .update(cx, |shell, window, cx| {
            let id = shell.tabs.open(muxus_app::model::ssh_profile("box"), "box");
            shell.tabs_changed(cx);
            window.refresh();
            id
        })
        .unwrap();
    wait_until(cx, Duration::from_secs(20), "the session", |cx| {
        window
            .update(cx, |shell, window, _| {
                window.refresh();
                shell
                    .tabs
                    .tab(&tab_id)
                    .is_some_and(|tab| tab.status == muxus_app::state::tabs::TabStatus::Connected)
            })
            .unwrap()
    });
    let progress = |cx: &mut TestAppContext| {
        // The session pump yields to frames on a timer, which only a moving clock fires.
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
        window
            .update(cx, |shell, window, _| {
                // The tab's ring and the overlay draw from it.
                window.refresh();
                shell.paste_progress(&tab_id)
            })
            .unwrap()
    };
    assert_eq!(progress(cx), None);
    // Two seconds after each line: the paste is still running when it is looked at.
    window
        .update(cx, |shell, _, cx| {
            shell.paste_into_session(&tab_id, "true\ntrue\ntrue\n", Some(2_000), true, cx)
        })
        .unwrap();
    wait_until(cx, Duration::from_secs(10), "the progress", |cx| {
        progress(cx).is_some()
    });
    let running = progress(cx).unwrap();
    assert_eq!(running.lines, 3);
    assert!(running.label().starts_with("Pasting line "));
    window
        .update(cx, |shell, _, cx| shell.cancel_paste(&tab_id, cx))
        .unwrap();
    wait_until(cx, Duration::from_secs(10), "the cancel", |cx| {
        progress(cx).is_none()
    });
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

/// Open `count` blank tabs titled "t0", "t1"... in the active pane, laid out once.
fn blank_tabs(
    window: &WindowHandle<AppShell>,
    count: usize,
    cx: &mut TestAppContext,
) -> Vec<String> {
    let ids = window
        .update(cx, |shell, window, cx| {
            let ids: Vec<String> = (0..count)
                .map(|index| {
                    let id = shell.tabs.open_empty();
                    shell.tabs.update(
                        &id,
                        muxus_app::state::tabs::TabPatch {
                            title: Some(format!("t{index}")),
                            ..Default::default()
                        },
                    );
                    id
                })
                .collect();
            shell.tabs_changed(cx);
            window.refresh();
            ids
        })
        .unwrap();
    cx.run_until_parked();
    ids
}

/// The titles of the active pane's tabs, in order.
fn strip_titles(window: &WindowHandle<AppShell>, cx: &mut TestAppContext) -> Vec<String> {
    window
        .update(cx, |shell, _, _| {
            let pane = shell.tabs.active_pane_id.clone();
            shell
                .tabs
                .pane_tabs(&pane)
                .iter()
                .map(|tab| tab.title.clone())
                .collect()
        })
        .unwrap()
}

type PxBounds = muxus_ui::gpui_kit::Bounds<muxus_ui::gpui_kit::Pixels>;

/// Where tab `id` is painted once every slide has arrived.
fn tab_bounds(window: &WindowHandle<AppShell>, id: &str, cx: &mut TestAppContext) -> PxBounds {
    // Tests have no clock to run the slides: land them.
    window
        .update(cx, |shell, window, _| {
            shell.tab_motion.slides.clear();
            window.refresh();
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| shell.geometry.tabs.borrow()[id])
        .unwrap()
}

/// A strip from `first`'s top-left, `width` wide.
fn strip_from(first: PxBounds, width: f32) -> PxBounds {
    muxus_ui::gpui_kit::Bounds {
        origin: first.origin,
        size: muxus_ui::gpui_kit::size(muxus_ui::gpui_kit::px(width), first.size.height),
    }
}

/// `TabStrip.tsx` drags: a tab dragged along its own strip resorts it live (a pinned
/// tab stays among the pinned ones), and a tab from another pane shows where it lands
/// and goes there when dropped.
#[gpui::test]
fn dragged_tabs_resort_live_and_land_where_the_line_shows(cx: &mut TestAppContext) {
    use muxus_app::state::tabs::Edge;
    use muxus_app::state::workspace_layout::PaneDirection;
    use muxus_ui::gpui_kit::{point, px};

    let fx = Fixture::start(|_| {});
    let window = main_window(&fx, cx);
    let ids = blank_tabs(&window, 3, cx);
    let over = |cx: &mut TestAppContext, dragged: &str, x: muxus_ui::gpui_kit::Pixels| {
        let mut first = tab_bounds(&window, &ids[0], cx);
        first.origin.x = px(0.);
        let strip = strip_from(first, 4000.);
        window
            .update(cx, |shell, window, cx| {
                // No slide is settling in a test.
                shell.tab_motion.settle_until = None;
                let pane = shell.tabs.active_pane_id.clone();
                shell.tab_dragged_over(&pane, dragged, point(x, strip.center().y), strip, cx);
                window.refresh();
            })
            .unwrap();
        cx.run_until_parked();
    };
    assert_eq!(strip_titles(&window, cx), ["t0", "t1", "t2"]);
    // "t0" dragged over the right half of "t2" moves behind it, live.
    let third = tab_bounds(&window, &ids[2], cx);
    over(cx, &ids[0], third.right() - px(5.));
    assert_eq!(strip_titles(&window, cx), ["t1", "t2", "t0"]);
    // Past the last tab: to the end of its group.
    let last = tab_bounds(&window, &ids[0], cx);
    over(cx, &ids[1], last.right() + px(40.));
    assert_eq!(strip_titles(&window, cx), ["t2", "t0", "t1"]);

    // A pinned tab never mixes with the others.
    window
        .update(cx, |shell, window, cx| {
            shell.tabs.set_pinned(&ids[2], true);
            shell.tabs_changed(cx);
            window.refresh();
        })
        .unwrap();
    cx.run_until_parked();
    let pinned = tab_bounds(&window, &ids[2], cx);
    over(cx, &ids[1], pinned.left() + px(5.));
    assert_eq!(strip_titles(&window, cx), ["t2", "t0", "t1"]);

    // A second pane: "t1" dragged over its strip shows the line, and the drop lands there.
    let (left_pane, right_pane) = window
        .update(cx, |shell, window, cx| {
            let left = shell.tabs.active_pane_id.clone();
            shell
                .tabs
                .move_tab_to_new_pane(&ids[0], PaneDirection::Right);
            let right = shell.tabs.active_pane_id.clone();
            shell.tabs_changed(cx);
            window.refresh();
            (left, right)
        })
        .unwrap();
    cx.run_until_parked();
    let target = tab_bounds(&window, &ids[0], cx);
    window
        .update(cx, |shell, _, cx| {
            shell.tab_dragged_over(
                &right_pane,
                &ids[1],
                point(target.left() + px(5.), target.center().y),
                strip_from(target, 400.),
                cx,
            );
            assert_eq!(
                shell.tab_motion.drop,
                Some(super::tab_drag::TabDropIndicator {
                    pane_id: right_pane.clone(),
                    target: Some(ids[0].clone()),
                    edge: Edge::Before,
                })
            );
            shell.drop_tab(&ids[1], &right_pane, Some(&ids[0]), Edge::Before, cx);
            assert_eq!(shell.tab_motion.drop, None);
            let titles = |pane: &str| -> Vec<String> {
                shell
                    .tabs
                    .pane_tabs(pane)
                    .iter()
                    .map(|tab| tab.title.clone())
                    .collect()
            };
            assert_eq!(titles(&right_pane), ["t1", "t0"]);
            assert_eq!(titles(&left_pane), ["t2"]);
        })
        .unwrap();
}

/// A tab from another window lands beside the tab it was let go over, keeping its
/// title and flag (`adoptTransferredTab`).
#[gpui::test]
fn a_tab_from_another_window_lands_beside_the_tab_under_the_pointer(cx: &mut TestAppContext) {
    use muxus_ui::gpui_kit::{point, px};

    let fx = Fixture::start(|_| {});
    let window = main_window(&fx, cx);
    let ids = blank_tabs(&window, 2, cx);
    let first = tab_bounds(&window, &ids[0], cx);
    window
        .update(cx, |shell, _, cx| {
            let mut moved = shell.tabs.tab(&ids[1]).unwrap().clone();
            moved.title = "router".into();
            moved.color = Some(muxus_ui::theme::TAB_FLAG_COLORS[0].into());
            shell.adopt_tab(
                moved,
                None,
                point(first.right() - px(4.), first.center().y),
                cx,
            );
        })
        .unwrap();
    assert_eq!(strip_titles(&window, cx), ["t0", "router", "t1"]);
    let adopted = window
        .update(cx, |shell, _, _| shell.tabs.active_tab().cloned())
        .unwrap()
        .unwrap();
    assert_eq!(
        adopted.color.as_deref(),
        Some(muxus_ui::theme::TAB_FLAG_COLORS[0])
    );
}

/// `ToastHost`: errors and warnings can be copied; details unfold below the message and
/// keep the toast up until it is dismissed.
#[gpui::test]
fn toasts_offer_copy_and_details(cx: &mut TestAppContext) {
    use muxus_ui::theme::Status;

    let fx = Fixture::start(|_| {});
    let window = main_window(&fx, cx);
    window
        .update(cx, |shell, _, cx| {
            shell.show_toast_with_details(
                Status::Error,
                "Could not save 3 hosts.",
                Some("router-1: permission denied".into()),
                cx,
            );
            let toast = shell.toast.clone().unwrap();
            assert!(toast.copyable() && toast.sticky());
            assert_eq!(
                toast.copy_text(),
                "Could not save 3 hosts.\n\nrouter-1: permission denied"
            );
            shell.show_toast(Status::Success, "Saved.", cx);
            let toast = shell.toast.clone().unwrap();
            assert!(!toast.copyable() && !toast.sticky() && toast.details.is_none());
        })
        .unwrap();
}
