//! The file browser against a live SFTP server, in a headless window.

mod support;

use std::time::Duration;

use gpui::TestAppContext;
use muxus_app::features::sftp::panel::{SftpPanel, SftpPanelOptions};
use support::{Fixture, allow_backend_threads, init_ui, settle, wait_until};

fn seed(root: &std::path::Path) {
    std::fs::write(root.join("top.txt"), "x").unwrap();
    std::fs::create_dir_all(root.join("many")).unwrap();
    for i in 0..400 {
        std::fs::write(root.join(format!("many/file-{i:04}.dat")), "").unwrap();
    }
    std::fs::create_dir_all(root.join("other")).unwrap();
    std::fs::write(root.join("other/inside.txt"), "x").unwrap();
}

/// The shell hands every tab's working directory to its browser on each sync, many times
/// while one listing loads. Only a change may navigate: navigating again cancelled the
/// listing in flight, so a followed folder never showed its files (the old folder's rows
/// stayed on screen), and a folder the user went to was thrown away.
#[gpui::test]
fn following_the_terminal_shows_the_new_folder_while_the_shell_keeps_syncing(
    cx: &mut TestAppContext,
) {
    let fx = Fixture::start(seed);
    allow_backend_threads(cx);
    cx.update(|cx| {
        init_ui(&fx.backend, cx);
    });
    let conn = fx.conn_id.clone();
    let window = cx.add_window(|window, cx| {
        let mut options = SftpPanelOptions::new(conn);
        options.terminal_path = Some("/".into());
        options.follow_terminal_folder = true;
        options.follow_toggle = true;
        SftpPanel::new(options, window, cx)
    });
    let names = |cx: &mut TestAppContext| {
        window
            .update(cx, |panel, _, _| panel.entry_names())
            .unwrap()
    };
    wait_until(cx, Duration::from_secs(20), "the home listing", |cx| {
        names(cx).contains(&"top.txt".to_string())
    });

    // `cd many`: the cwd report arrives, then the shell keeps syncing.
    let sync = |cx: &mut TestAppContext, path: &str| {
        window
            .update(cx, |panel, window, cx| {
                panel.set_terminal_path(Some(path.into()), window, cx);
                panel.set_follow_terminal_folder(true, window, cx);
            })
            .unwrap();
    };
    sync(cx, "/many");
    // The old folder's rows are gone at once.
    assert!(!names(cx).contains(&"top.txt".to_string()));
    wait_until(cx, Duration::from_secs(20), "the followed folder", |cx| {
        sync(cx, "/many");
        names(cx).len() == 400
    });
    assert_eq!(
        window
            .update(cx, |panel, _, _| panel.current_path())
            .unwrap(),
        "/many"
    );

    // The user goes elsewhere; syncs with the unchanged terminal folder leave it there.
    window
        .update(cx, |panel, window, cx| {
            panel.navigate_to("/other".into(), window, cx)
        })
        .unwrap();
    wait_until(cx, Duration::from_secs(20), "the chosen folder", |cx| {
        sync(cx, "/many");
        names(cx) == vec!["inside.txt".to_string()]
    });
    settle(cx, Duration::from_millis(300));
    sync(cx, "/many");
    assert_eq!(names(cx), vec!["inside.txt".to_string()]);
}
