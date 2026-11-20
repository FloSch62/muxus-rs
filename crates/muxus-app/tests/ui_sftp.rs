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

/// Rows select like a file manager: a click selects one, Ctrl/Cmd-click adds one, Shift-click
/// the run from the last click; going to another folder starts over.
#[gpui::test]
fn selects_several_files_and_forgets_them_in_another_folder(cx: &mut TestAppContext) {
    use muxus_app::features::sftp::selection::SftpSelectGesture::{Extend, Only, Toggle};

    let fx = Fixture::start(|root| {
        for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            std::fs::write(root.join(name), name).unwrap();
        }
        std::fs::create_dir_all(root.join("logs")).unwrap();
    });
    allow_backend_threads(cx);
    cx.update(|cx| {
        init_ui(&fx.backend, cx);
    });
    let conn = fx.conn_id.clone();
    let window = cx.add_window(|window, cx| {
        let mut options = SftpPanelOptions::new(conn);
        options.initial_path = "/".into();
        SftpPanel::new(options, window, cx)
    });
    wait_until(cx, Duration::from_secs(20), "the listing", |cx| {
        window
            .update(cx, |panel, _, _| panel.entry_names().len() == 5)
            .unwrap()
    });
    let click = |cx: &mut TestAppContext, name: &str, gesture| {
        window
            .update(cx, |panel, _, cx| panel.select_with(name, gesture, cx))
            .unwrap();
    };
    let selected = |cx: &mut TestAppContext| {
        window
            .update(cx, |panel, _, _| panel.selected_names())
            .unwrap()
    };

    click(cx, "b.txt", Only);
    click(cx, "d.txt", Extend);
    assert_eq!(selected(cx), ["b.txt", "c.txt", "d.txt"]);
    click(cx, "c.txt", Toggle);
    assert_eq!(selected(cx), ["b.txt", "d.txt"]);
    click(cx, "a.txt", Only);
    assert_eq!(selected(cx), ["a.txt"]);

    window
        .update(cx, |panel, window, cx| {
            panel.navigate_to("/logs".into(), window, cx)
        })
        .unwrap();
    assert!(selected(cx).is_empty());
}

fn panel_at_root(
    fx: &Fixture,
    conn: &str,
    cx: &mut TestAppContext,
) -> gpui::WindowHandle<SftpPanel> {
    let _ = fx;
    let conn = conn.to_string();
    cx.add_window(|window, cx| {
        let mut options = SftpPanelOptions::new(conn);
        options.initial_path = "/".into();
        SftpPanel::new(options, window, cx)
    })
}

fn names(window: &gpui::WindowHandle<SftpPanel>, cx: &mut TestAppContext) -> Vec<String> {
    window
        .update(cx, |panel, _, _| panel.entry_names())
        .unwrap()
}

fn set_mtime(path: &std::path::Path, seconds: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

/// The headers sort the listing, a second click reverses it, and the order is kept across
/// folders. The name filter and the hidden-files button take rows off the screen, out of the
/// selection, and say why a folder looks empty.
#[gpui::test]
fn sorts_by_column_filters_by_name_and_hides_dotfiles(cx: &mut TestAppContext) {
    use muxus_app::features::sftp::sort::{SftpSort, SftpSortKey};

    let fx = Fixture::start(|root| {
        for (name, size, mtime) in [
            ("b.log", 300, 3_000),
            ("a.txt", 10, 5_000),
            ("C.bin", 300, 2_000),
            (".bashrc", 20, 4_000),
        ] {
            std::fs::write(root.join(name), vec![b'x'; size]).unwrap();
            set_mtime(&root.join(name), mtime);
        }
        std::fs::create_dir_all(root.join("logs")).unwrap();
        std::fs::create_dir_all(root.join(".ssh")).unwrap();
        std::fs::write(root.join("logs/inside.txt"), "x").unwrap();
    });
    allow_backend_threads(cx);
    cx.update(|cx| {
        init_ui(&fx.backend, cx);
    });
    let window = panel_at_root(&fx, &fx.conn_id, cx);
    wait_until(cx, Duration::from_secs(20), "the listing", |cx| {
        names(&window, cx).len() == 6
    });
    let sort_by = |cx: &mut TestAppContext, key| {
        window
            .update(cx, |panel, _, cx| panel.sort_by(key, cx))
            .unwrap();
    };

    // Folders first, then names A to Z.
    assert_eq!(
        names(&window, cx),
        [".ssh", "logs", ".bashrc", "a.txt", "b.log", "C.bin"]
    );
    // Size starts with the largest; equal sizes go by name, folders stay on top.
    sort_by(cx, SftpSortKey::Size);
    assert_eq!(
        names(&window, cx),
        [".ssh", "logs", "b.log", "C.bin", ".bashrc", "a.txt"]
    );
    sort_by(cx, SftpSortKey::Size);
    assert_eq!(
        names(&window, cx),
        [".ssh", "logs", "a.txt", ".bashrc", "b.log", "C.bin"]
    );
    // Modified starts with the newest.
    sort_by(cx, SftpSortKey::Modified);
    assert_eq!(
        names(&window, cx)[2..],
        ["a.txt", ".bashrc", "b.log", "C.bin"]
    );

    // The dotfiles go, and with them what was selected of them.
    window
        .update(cx, |panel, _, cx| {
            panel.select_with(
                ".bashrc",
                muxus_app::features::sftp::selection::SftpSelectGesture::Only,
                cx,
            );
            panel.select_with(
                "a.txt",
                muxus_app::features::sftp::selection::SftpSelectGesture::Toggle,
                cx,
            );
            assert!(panel.shows_hidden_files(cx));
            panel.set_show_hidden_files(false, cx);
        })
        .unwrap();
    assert_eq!(names(&window, cx), ["logs", "a.txt", "b.log", "C.bin"]);
    assert_eq!(
        window
            .update(cx, |panel, _, _| panel.selected_names())
            .unwrap(),
        ["a.txt"]
    );

    // The filter matches anywhere in the name, ignoring case.
    let filter = |cx: &mut TestAppContext, text: &str| {
        window
            .update(cx, |panel, window, cx| panel.set_filter(text, window, cx))
            .unwrap();
    };
    filter(cx, "B");
    assert_eq!(names(&window, cx), ["b.log", "C.bin"]);
    assert!(
        window
            .update(cx, |panel, _, _| panel.selected_names())
            .unwrap()
            .is_empty()
    );
    let empty_label = |cx: &mut TestAppContext| {
        window
            .update(cx, |panel, _, _| panel.empty_label())
            .unwrap()
    };
    filter(cx, "bash");
    assert!(names(&window, cx).is_empty());
    assert_eq!(empty_label(cx), "Only hidden files match");
    filter(cx, "zzz");
    assert_eq!(empty_label(cx), "No files match");

    // Another folder starts with an empty filter and keeps the sort.
    window
        .update(cx, |panel, window, cx| {
            panel.navigate_to("/logs".into(), window, cx)
        })
        .unwrap();
    wait_until(cx, Duration::from_secs(20), "the other folder", |cx| {
        names(&window, cx) == ["inside.txt"]
    });
    window
        .update(cx, |panel, _, _| {
            assert_eq!(panel.filter(), "");
            assert_eq!(
                panel.sort(),
                SftpSort {
                    key: SftpSortKey::Modified,
                    descending: true
                }
            );
        })
        .unwrap();
}

/// A folder is downloaded whole into the picked folder, after one question with its totals.
#[gpui::test]
fn downloads_a_folder_with_everything_in_it_beside_the_selected_files(cx: &mut TestAppContext) {
    use muxus_app::features::sftp::download::DownloadTarget;

    let fx = Fixture::start(|root| {
        std::fs::write(root.join("notes.txt"), "notes").unwrap();
        std::fs::create_dir_all(root.join("logs/2024")).unwrap();
        std::fs::create_dir_all(root.join("logs/empty")).unwrap();
        std::fs::write(root.join("logs/app.log"), "app").unwrap();
        std::fs::write(root.join("logs/2024/jan.log"), "january").unwrap();
        std::os::unix::fs::symlink("app.log", root.join("logs/current")).unwrap();
    });
    allow_backend_threads(cx);
    let notes = cx.update(|cx| init_ui(&fx.backend, cx));
    let window = panel_at_root(&fx, &fx.conn_id, cx);
    wait_until(cx, Duration::from_secs(20), "the listing", |cx| {
        names(&window, cx).len() == 2
    });
    let picked = fx.scratch.path().join("Downloads");
    std::fs::create_dir_all(picked.join("logs")).unwrap();

    window
        .update(cx, |panel, window, cx| {
            let targets = panel
                .entries()
                .iter()
                .map(|entry| DownloadTarget {
                    path: format!("/{}", entry.name),
                    entry: entry.clone(),
                })
                .collect();
            panel.download_into(targets, picked.clone(), window, cx);
        })
        .unwrap();
    wait_until(cx, Duration::from_secs(20), "the question", |cx| {
        window
            .update(cx, |panel, _, _| panel.question().is_some())
            .unwrap()
    });
    window
        .update(cx, |panel, window, cx| {
            assert_eq!(panel.question(), Some("Download 3 files (15 B)?"));
            panel.answer_question(true, window, cx);
        })
        .unwrap();
    wait_until(cx, Duration::from_secs(20), "the download", |_| {
        !notes.borrow().is_empty()
    });

    assert_eq!(
        *notes.borrow(),
        ["Downloaded 3 files to Downloads; 1 item skipped"]
    );
    // The folder of that name already there is kept; the downloaded one is numbered.
    let read = |path: &str| std::fs::read_to_string(picked.join(path)).unwrap();
    assert_eq!(read("notes.txt"), "notes");
    assert_eq!(read("logs (1)/app.log"), "app");
    assert_eq!(read("logs (1)/2024/jan.log"), "january");
    assert!(picked.join("logs (1)/empty").is_dir());
    assert!(!picked.join("logs (1)/current").exists());
    assert_eq!(std::fs::read_dir(picked.join("logs")).unwrap().count(), 0);
}

/// Bytes the server has answered reads with so far.
fn served(fx: &Fixture) -> u64 {
    fx.sftp.reads().iter().map(|(_, _, len)| *len as u64).sum()
}

/// A download cut off by its connection keeps what it received, and the next download of the
/// file asks the server only for the rest.
#[gpui::test]
fn continues_an_interrupted_download_on_the_next_connection(cx: &mut TestAppContext) {
    use muxus_app::features::sftp::download::DownloadTarget;
    use muxus_app::features::sftp::services::services;

    const SIZE: usize = 3 * 1024 * 1024;
    const CUT: u64 = 1024 * 1024;
    let content: Vec<u8> = (0..SIZE).map(|index| (index % 251) as u8).collect();
    let fx = Fixture::start(|root| {
        std::fs::write(root.join("image.bin"), &content).unwrap();
        set_mtime(&root.join("image.bin"), 1_760_000_000);
    });
    allow_backend_threads(cx);
    let notes = cx.update(|cx| init_ui(&fx.backend, cx));
    let picked = fx.scratch.path().join("Downloads");
    std::fs::create_dir_all(&picked).unwrap();
    let start = |window: &gpui::WindowHandle<SftpPanel>, cx: &mut TestAppContext| {
        let picked = picked.clone();
        window
            .update(cx, |panel, window, cx| {
                let targets = panel
                    .entries()
                    .iter()
                    .map(|entry| DownloadTarget {
                        path: format!("/{}", entry.name),
                        entry: entry.clone(),
                    })
                    .collect();
                panel.download_into(targets, picked, window, cx);
            })
            .unwrap();
    };
    let kept = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            services(cx)
                .downloads
                .borrow()
                .kept("/image.bin")
                .map(|partial| partial.bytes.len() as u64)
        })
    };

    // The link goes quiet after the first megabyte, then the server drops the connection.
    fx.sftp.options.lock().unwrap().stall_reads_from = Some(CUT);
    let window = panel_at_root(&fx, &fx.conn_id, cx);
    wait_until(cx, Duration::from_secs(20), "the listing", |cx| {
        names(&window, cx) == ["image.bin"]
    });
    start(&window, cx);
    // The bar is updated ten times a second at most, so the server says how far it got.
    // Requests overtake each other on their way, so the last ones before the cut may wait
    // behind one that is never answered.
    wait_until(
        cx,
        Duration::from_secs(20),
        "most of the first megabyte",
        |_| served(&fx) >= CUT / 2,
    );
    settle(cx, Duration::from_millis(300));
    fx.runtime.block_on(fx.server.disconnect_all());
    wait_until(cx, Duration::from_secs(20), "the failure", |_| {
        !notes.borrow().is_empty()
    });
    assert!(
        notes.borrow()[0].starts_with("Could not download image.bin: "),
        "{:?}",
        notes.borrow()
    );
    let received = kept(cx).expect("what the download received is kept");
    assert!(
        (CUT / 2..=CUT).contains(&received),
        "{received} of {CUT} bytes"
    );
    assert!(!picked.join("image.bin").exists());
    notes.borrow_mut().clear();

    // The same host again, on a new connection.
    fx.sftp.options.lock().unwrap().stall_reads_from = None;
    let (_session, conn_id) = support::connect(&fx.runtime, &fx.backend, "box");
    assert_ne!(conn_id, fx.conn_id);
    let before = fx.sftp.reads().len();
    let again = panel_at_root(&fx, &conn_id, cx);
    wait_until(cx, Duration::from_secs(20), "the listing again", |cx| {
        names(&again, cx) == ["image.bin"]
    });
    start(&again, cx);
    wait_until(cx, Duration::from_secs(20), "the rest", |_| {
        !notes.borrow().is_empty()
    });

    assert_eq!(*notes.borrow(), ["Downloaded 1 file to Downloads"]);
    assert_eq!(std::fs::read(picked.join("image.bin")).unwrap(), content);
    // Only the rest is read.
    let rest = &fx.sftp.reads()[before..];
    assert_eq!(
        rest.iter().map(|(_, offset, _)| *offset).min(),
        Some(received)
    );
    assert_eq!(kept(cx), None);
}

/// A transfer stops with a word about the lost connection when its session loses it, and
/// without one when Cancel or the closing browser stops it.
#[gpui::test]
fn says_why_a_download_stopped(cx: &mut TestAppContext) {
    use muxus_app::features::sftp::download::DownloadTarget;
    use muxus_app::features::sftp::services::services;

    const CUT: u64 = 256 * 1024;
    let fx = Fixture::start(|root| {
        std::fs::write(root.join("image.bin"), vec![7u8; 1024 * 1024]).unwrap();
        set_mtime(&root.join("image.bin"), 1_760_000_000);
    });
    fx.sftp.options.lock().unwrap().stall_reads_from = Some(CUT);
    allow_backend_threads(cx);
    let notes = cx.update(|cx| init_ui(&fx.backend, cx));
    let picked = fx.scratch.path().join("Downloads");
    std::fs::create_dir_all(&picked).unwrap();
    let stalled = |cx: &mut TestAppContext| {
        let window = panel_at_root(&fx, &fx.conn_id, cx);
        wait_until(cx, Duration::from_secs(20), "the listing", |cx| {
            names(&window, cx) == ["image.bin"]
        });
        let picked = picked.clone();
        window
            .update(cx, |panel, window, cx| {
                let targets = vec![DownloadTarget {
                    path: "/image.bin".into(),
                    entry: panel.entries()[0].clone(),
                }];
                panel.download_into(targets, picked, window, cx);
            })
            .unwrap();
        wait_until(cx, Duration::from_secs(20), "the stall", |_| {
            served(&fx) >= CUT / 2
        });
        // What the server sent is on its way to the window.
        settle(cx, Duration::from_millis(300));
        window
    };
    let kept = |cx: &mut TestAppContext| {
        cx.update(|cx| services(cx).downloads.borrow().kept("/image.bin").is_some())
    };

    // The session lost its connection: the browser says so, and keeps what it received.
    let window = stalled(cx);
    window
        .update(cx, |panel, _, cx| panel.connection_lost(cx))
        .unwrap();
    settle(cx, Duration::from_millis(100));
    assert_eq!(*notes.borrow(), ["Download stopped: connection lost"]);
    assert!(
        window
            .update(cx, |panel, _, _| panel.transfer_state().is_none())
            .unwrap()
    );
    assert!(kept(cx));
    notes.borrow_mut().clear();
}
