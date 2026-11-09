//! The dialog loads the hosts and folder settings after it opens. A save before they arrived
//! planned against no hosts: a rename moved the folder but left its hosts in the old one.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use futures_lite::future::block_on;
use gpui::TestAppContext;
use muxus_core::api::OpenSshMetadataPatch;
use muxus_core::js::Patch;
use muxus_session::BackendOptions;

use super::*;

/// The window the dialog opens in; it draws nothing itself.
struct Opener;

impl Render for Opener {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn group_of(backend: &Backend, alias: &str) -> Option<String> {
    let config = block_on(backend.ssh_config()).unwrap();
    config
        .hosts
        .iter()
        .find(|host| host.alias == alias)
        .and_then(|host| host.metadata.as_ref())
        .and_then(|metadata| metadata.group.clone())
}

#[gpui::test]
fn a_rename_waits_for_the_folders_hosts(cx: &mut TestAppContext) {
    let data = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
    std::fs::write(
        home.path().join(".ssh").join("config"),
        "Host web\n  HostName web.example\n",
    )
    .unwrap();
    let backend =
        Backend::start_blocking(BackendOptions::sandbox(data.path(), home.path())).unwrap();
    block_on(backend.patch_ssh_host_metadata(
        "web",
        OpenSshMetadataPatch {
            group: Patch::Set("Lab".into()),
            ..OpenSshMetadataPatch::default()
        },
    ))
    .unwrap();
    // The dialog's data and its writes come from the backend's own threads.
    cx.executor().allow_parking();

    let events: Rc<RefCell<Vec<FolderDialogEvent>>> = Rc::default();
    let window = cx.add_window(|_, _| Opener);
    let dialog = window
        .update(cx, |_, window, cx| {
            let dialog = open_folder_dialog(
                window,
                cx,
                backend.clone(),
                FolderDialogState::Edit { path: "Lab".into() },
                FolderPrefs::default(),
            );
            let sink = events.clone();
            cx.subscribe(&dialog, move |_, _, event: &FolderDialogEvent, _| {
                sink.borrow_mut().push(event.clone())
            })
            .detach();
            // Rename the folder and save at once, before the hosts have been read.
            dialog.update(cx, |dialog, cx| {
                assert!(!dialog.loaded);
                dialog.draft.name = "Prod".into();
                dialog.submit(cx);
                assert!(!dialog.busy);
            });
            dialog
        })
        .unwrap();
    cx.run_until_parked();
    assert!(events.borrow().is_empty(), "saved before the data loaded");

    let deadline = Instant::now() + Duration::from_secs(20);
    while !dialog.read_with(cx, |dialog, _| dialog.loaded) {
        assert!(Instant::now() < deadline, "timed out waiting for the data");
        std::thread::sleep(Duration::from_millis(10));
        cx.run_until_parked();
    }
    // The form kept what was typed while the data loaded.
    dialog.update(cx, |dialog, cx| dialog.submit(cx));
    while !events.borrow().contains(&FolderDialogEvent::Close) {
        assert!(Instant::now() < deadline, "timed out waiting for the save");
        std::thread::sleep(Duration::from_millis(10));
        cx.run_until_parked();
    }
    let prefs = match &events.borrow()[0] {
        FolderDialogEvent::Submitted { prefs, .. } => prefs.clone(),
        other => panic!("expected the submit first, got {other:?}"),
    };
    assert!(prefs.contains(&FolderPrefsChange::Rename {
        from: "Lab".into(),
        to: "Prod".into(),
    }));
    // The host went along: the new folder is not kept as an empty one.
    assert!(!prefs.contains(&FolderPrefsChange::AddEmptyFolder("Prod".into())));
    assert_eq!(group_of(&backend, "web").as_deref(), Some("Prod"));
}
