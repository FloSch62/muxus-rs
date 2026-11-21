//! The shared sessions views against a sandboxed backend: following a list from a file,
//! writing a folder as a list another Muxus follows, and stopping to follow.

mod support;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use gpui::{AppContext, Entity, IntoElement, Render, TestAppContext, Window};
use muxus_app::features::shared_sessions::dialog::{
    SharedSessionsDialog, SharedSessionsDialogEvent, open_shared_sessions_dialog,
};
use muxus_app::features::shared_sessions::share::{ShareFolderDialog, open_share_folder};
use muxus_app::features::shared_sessions::stop::{
    StopFollowingDialog, StopFollowingEvent, open_stop_following,
};
use muxus_app::features::shared_sessions::store;
use muxus_core::api::{SavedHostProfile, SharedSessionSource};
use muxus_session::{Backend, BackendOptions, RuntimeChoice};
use serde_json::{Value, json};
use support::{allow_backend_threads, wait_until};

const WAIT: Duration = Duration::from_secs(20);

/// A sandboxed backend on its own runtime.
struct Sandbox {
    backend: Backend,
    runtime: tokio::runtime::Runtime,
    scratch: tempfile::TempDir,
}

impl Sandbox {
    fn start() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let home = scratch.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut options = BackendOptions::sandbox(scratch.path().join("data"), &home);
        options.runtime = RuntimeChoice::Handle(runtime.handle().clone());
        let backend = runtime
            .block_on(Backend::start(options))
            .expect("the backend starts");
        Self {
            backend,
            runtime,
            scratch,
        }
    }

    fn save_host(&self, id: &str, profile: Value, metadata: Value) {
        self.runtime.block_on(async {
            self.backend
                .save_saved_host(
                    serde_json::from_value(json!({ "id": id, "name": id, "profile": profile }))
                        .unwrap(),
                )
                .await
                .unwrap();
            self.backend
                .patch_saved_host_metadata(id, serde_json::from_value(metadata).unwrap())
                .await
                .unwrap();
        });
    }

    fn hosts(&self) -> Vec<SavedHostProfile> {
        self.runtime
            .block_on(self.backend.saved_hosts())
            .unwrap()
            .profiles
    }

    fn links(&self) -> Vec<SharedSessionSource> {
        self.runtime
            .block_on(self.backend.shared_session_sources())
            .unwrap()
            .sources
    }

    fn stop(self) {
        self.runtime.block_on(self.backend.shutdown());
    }
}

/// A window whose content is one dialog.
struct Stage<V: Render> {
    dialog: Entity<V>,
}

impl<V: Render> Render for Stage<V> {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        self.dialog.clone()
    }
}

fn stage<V: Render>(
    cx: &mut TestAppContext,
    open: impl FnOnce(&mut Window, &mut gpui::App) -> Entity<V>,
) -> Entity<V> {
    cx.update(|cx| muxus_ui::init(muxus_ui::theme::Mode::Light, cx));
    let window = cx.add_window(|window, cx| Stage {
        dialog: open(window, cx),
    });
    window
        .update(cx, |stage, _, _| stage.dialog.clone())
        .unwrap()
}

/// Everything `dialog` emits from now on.
fn events<V, E>(dialog: &Entity<V>, cx: &mut TestAppContext) -> Rc<RefCell<Vec<E>>>
where
    V: gpui::EventEmitter<E> + 'static,
    E: Clone + 'static,
{
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    cx.update(|cx| {
        cx.subscribe(dialog, move |_, event: &E, _| {
            sink.borrow_mut().push(event.clone())
        })
        .detach();
    });
    seen
}

fn write_list(directory: &Path) -> std::path::PathBuf {
    let file = directory.join("lab.muxus-sessions.json");
    std::fs::write(
        &file,
        json!({
            "format": "muxus-shared-sessions",
            "version": 1,
            "hosts": [
                {
                    "id": "node0", "name": "node0", "folder": "clab",
                    "profile": { "kind": "ssh", "target": "node0.lab.example.net", "user": "alice" },
                },
                {
                    "id": "console", "name": "console",
                    "profile": { "kind": "telnet", "host": "console.lab.example.net", "port": 2001 },
                },
            ],
        })
        .to_string(),
    )
    .unwrap();
    file
}

#[gpui::test]
fn follows_a_list_from_a_file_into_a_folder_named_like_the_link(cx: &mut TestAppContext) {
    allow_backend_threads(cx);
    let sandbox = Sandbox::start();
    let file = write_list(sandbox.scratch.path());
    let backend = sandbox.backend.clone();
    let dialog: Entity<SharedSessionsDialog> = stage(cx, |window, cx| {
        open_shared_sessions_dialog(backend, None, window, cx)
    });
    let seen = events::<_, SharedSessionsDialogEvent>(&dialog, cx);

    // Nothing to add until there is a link, a name and a folder.
    assert!(!dialog.read_with(cx, |dialog, _| dialog.ready()));
    dialog.update(cx, |dialog, cx| {
        dialog.set_link(file.to_string_lossy(), cx);
        dialog.set_name("Lab hosts", cx);
    });
    cx.run_until_parked();
    // The folder follows the name until one is chosen.
    let folder = |cx: &mut TestAppContext| {
        dialog.read_with(cx, |dialog, _| {
            (dialog.effective_folder(), dialog.follows_name())
        })
    };
    assert_eq!(folder(cx), ("Lab hosts".to_owned(), true));
    dialog.update(cx, |dialog, cx| dialog.set_name("  Team lab ", cx));
    cx.run_until_parked();
    assert_eq!(folder(cx), ("Team lab".to_owned(), true));
    dialog.update(cx, |dialog, cx| dialog.set_folder("Teams/Lab", cx));
    cx.run_until_parked();
    dialog.update(cx, |dialog, cx| dialog.set_name("Lab hosts", cx));
    cx.run_until_parked();
    assert_eq!(folder(cx), ("Teams/Lab".to_owned(), false));
    assert!(dialog.read_with(cx, |dialog, _| dialog.ready()));
    assert_eq!(
        serde_json::to_value(dialog.read_with(cx, |dialog, _| dialog.input())).unwrap(),
        json!({
            "name": "Lab hosts", "url": file.to_string_lossy(), "folder": "Teams/Lab",
            "refreshMinutes": 60,
        })
    );

    cx.update_window(cx.windows()[0], |_, window, cx| {
        dialog.update(cx, |dialog, cx| dialog.submit(window, cx));
    })
    .unwrap();
    wait_until(cx, WAIT, "the link to be saved", |_| {
        !seen.borrow().is_empty()
    });
    let saved = match seen.borrow().as_slice() {
        [SharedSessionsDialogEvent::Saved(saved)] => (**saved).clone(),
        other => panic!("one saved link: {other:?}"),
    };
    assert_eq!(saved.folder, "Teams/Lab");
    assert_eq!(saved.refresh_minutes, 60);
    assert_eq!(saved.host_count, 0);

    // Whoever opened the dialog reads the link at once.
    let backend = sandbox.backend.clone();
    cx.update(|cx| store::refresh_now(&backend, &saved, cx));
    wait_until(cx, WAIT, "the hosts of the list", |cx| {
        cx.update(|cx| {
            store::listed(cx)
                .first()
                .is_some_and(|link| link.host_count == 2)
        })
    });
    cx.update(|cx| {
        assert!(store::applied(cx) > 0);
        assert!(!store::is_reading(&saved.id, cx));
        let link = store::link_for_folder("folder:teams/lab", cx).expect("the folder's link");
        assert_eq!(link.id, saved.id);
        assert_eq!(store::link_name(&saved.id, cx), Some("Lab hosts"));
        assert!(store::link_for_folder("folder:teams", cx).is_none());
    });
    let mut placed: Vec<(String, Option<String>)> = sandbox
        .hosts()
        .into_iter()
        .map(|host| (host.name, host.metadata.group))
        .collect();
    placed.sort();
    assert_eq!(
        placed,
        [
            ("console".to_owned(), Some("Teams/Lab".to_owned())),
            ("node0".to_owned(), Some("Teams/Lab/clab".to_owned())),
        ]
    );
    sandbox.stop();
}

#[gpui::test]
fn says_why_a_link_cannot_be_followed_and_stays_open(cx: &mut TestAppContext) {
    allow_backend_threads(cx);
    let sandbox = Sandbox::start();
    let backend = sandbox.backend.clone();
    let dialog: Entity<SharedSessionsDialog> = stage(cx, |window, cx| {
        open_shared_sessions_dialog(backend, None, window, cx)
    });
    let seen = events::<_, SharedSessionsDialogEvent>(&dialog, cx);
    dialog.update(cx, |dialog, cx| {
        dialog.set_link("lists/lab.json", cx);
        dialog.set_name("Lab", cx);
    });

    cx.update_window(cx.windows()[0], |_, window, cx| {
        dialog.update(cx, |dialog, cx| dialog.submit(window, cx));
    })
    .unwrap();

    wait_until(cx, WAIT, "the refusal", |cx| {
        dialog.read_with(cx, |dialog, _| dialog.error().is_some())
    });
    assert_eq!(
        dialog.read_with(cx, |dialog, _| dialog.error().map(str::to_owned)),
        Some(
            "Enter the full link, starting with https://, or the full path of the file.".to_owned()
        )
    );
    assert!(seen.borrow().is_empty());
    assert!(sandbox.links().is_empty());
    sandbox.stop();
}

#[gpui::test]
fn shares_a_folder_as_a_list_another_muxus_follows(cx: &mut TestAppContext) {
    allow_backend_threads(cx);
    let sandbox = Sandbox::start();
    sandbox.save_host(
        "bastion",
        json!({ "kind": "ssh", "target": "bastion.example.net", "useConfig": false, "user": "jump" }),
        json!({ "group": "Infra" }),
    );
    sandbox.save_host(
        "leaf",
        json!({
            "kind": "ssh", "target": "leaf-01.lab.example.net", "useConfig": false,
            "user": "admin", "identityFiles": ["~/.ssh/lab"],
            "proxyJump": ["muxus-host:bastion"],
        }),
        json!({ "group": "Lab/Fabric", "color": "#a855f7" }),
    );
    sandbox.save_host(
        "console",
        json!({ "kind": "telnet", "host": "console.lab.example.net", "port": 2001 }),
        json!({ "group": "Lab" }),
    );
    sandbox.save_host(
        "other",
        json!({ "kind": "ssh", "target": "other.example.net", "useConfig": false }),
        json!({ "group": "Production" }),
    );
    let backend = sandbox.backend.clone();
    let dialog: Entity<ShareFolderDialog> = stage(cx, |window, cx| {
        open_share_folder(backend, "Lab".into(), window, cx)
    });

    wait_until(cx, WAIT, "the folder's hosts", |cx| {
        dialog.read_with(cx, |dialog, _| dialog.shared().host_count == 2)
    });
    let shared = dialog.read_with(cx, |dialog, _| dialog.shared());
    assert_eq!(shared.jump_host_count, 1);
    assert_eq!(
        dialog.read_with(cx, |dialog, _| dialog.filename()),
        "lab.muxus-sessions.json"
    );
    let document = |cx: &mut TestAppContext| -> Value {
        serde_json::from_str(&dialog.read_with(cx, |dialog, _| dialog.file_text())).unwrap()
    };
    let written = document(cx);
    assert_eq!(written["format"], "muxus-shared-sessions");
    assert_eq!(written["version"], 1);
    assert_eq!(written["name"], "Lab");
    let host = |document: &Value, id: &str| -> Value {
        document["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|host| host["id"] == id)
            .unwrap_or_else(|| panic!("no host {id}"))
            .clone()
    };
    // User names are in by default, key paths are not, and the jump host comes along.
    let leaf = host(&written, "leaf");
    assert_eq!(leaf["folder"], "Fabric");
    assert_eq!(leaf["color"], "#a855f7");
    assert_eq!(leaf["profile"]["user"], "admin");
    assert_eq!(leaf["profile"].get("identityFiles"), None);
    assert_eq!(leaf["profile"]["proxyJump"], json!(["muxus-host:bastion"]));
    assert_eq!(host(&written, "bastion")["folder"], "Jump hosts");
    assert_eq!(written["hosts"].as_array().unwrap().len(), 3);

    dialog.update(cx, |dialog, cx| {
        dialog.set_include_users(false, cx);
        dialog.set_include_key_paths(true, cx);
    });
    let personal = document(cx);
    assert_eq!(host(&personal, "leaf")["profile"].get("user"), None);
    assert_eq!(
        host(&personal, "leaf")["profile"]["identityFiles"],
        json!(["~/.ssh/lab"])
    );

    // Another Muxus follows the file as it was first written.
    let file = sandbox.scratch.path().join("lab.muxus-sessions.json");
    std::fs::write(
        &file,
        serde_json::to_string_pretty(&written).unwrap() + "\n",
    )
    .unwrap();
    let follower = Sandbox::start();
    let result = follower
        .runtime
        .block_on(async {
            let link = follower
                .backend
                .save_shared_session_source(
                    serde_json::from_value(json!({
                        "name": "Team lab", "url": file.to_string_lossy(),
                        "folder": "Team lab", "refreshMinutes": 60,
                    }))
                    .unwrap(),
                )
                .await?;
            follower.backend.sync_shared_sessions(&link.id, true).await
        })
        .unwrap()
        .expect("the link is read");
    assert_eq!(result.added, 3);
    let hosts = follower.hosts();
    let named = |name: &str| hosts.iter().find(|host| host.name == name).unwrap();
    assert_eq!(
        named("leaf").metadata.group.as_deref(),
        Some("Team lab/Fabric")
    );
    assert_eq!(named("console").metadata.group.as_deref(), Some("Team lab"));
    assert_eq!(
        named("bastion").metadata.group.as_deref(),
        Some("Team lab/Jump hosts")
    );
    // The hop names the jump host the list brought.
    assert_eq!(
        serde_json::to_value(&named("leaf").profile).unwrap()["proxyJump"],
        json!([format!("muxus-host:{}", named("bastion").id)])
    );
    follower.stop();
    sandbox.stop();
}

#[gpui::test]
fn stops_following_and_keeps_the_hosts_when_asked_to(cx: &mut TestAppContext) {
    allow_backend_threads(cx);
    let sandbox = Sandbox::start();
    let file = write_list(sandbox.scratch.path());
    let link = sandbox
        .runtime
        .block_on(async {
            let link = sandbox
                .backend
                .save_shared_session_source(
                    serde_json::from_value(json!({
                        "name": "Lab hosts", "url": file.to_string_lossy(),
                        "folder": "Lab", "refreshMinutes": 0,
                    }))
                    .unwrap(),
                )
                .await?;
            sandbox.backend.sync_shared_sessions(&link.id, true).await?;
            Ok::<_, muxus_session::ApiError>(link)
        })
        .unwrap();
    let link = sandbox
        .links()
        .into_iter()
        .find(|source| source.id == link.id)
        .unwrap();
    assert_eq!(link.host_count, 2);
    let asked = link.clone();
    let dialog: Entity<StopFollowingDialog> =
        stage(cx, |window, cx| open_stop_following(asked, window, cx));
    let seen = events::<_, StopFollowingEvent>(&dialog, cx);

    dialog.update(cx, |dialog, cx| {
        assert_eq!(dialog.source().name, "Lab hosts");
        dialog.set_keep_hosts(true, cx);
        dialog.confirm(cx);
    });
    cx.run_until_parked();
    assert_eq!(
        seen.borrow().as_slice(),
        [StopFollowingEvent::Confirmed { keep_hosts: true }]
    );

    let backend = sandbox.backend.clone();
    cx.update(|cx| store::stop_following(&backend, &link.id, true, cx));
    wait_until(cx, WAIT, "the link to be gone", |cx| {
        cx.update(|cx| store::loaded(cx) && store::listed(cx).is_empty())
    });
    assert!(sandbox.links().is_empty());
    let hosts = sandbox.hosts();
    assert_eq!(hosts.len(), 2);
    assert!(
        hosts
            .iter()
            .all(|host| host.metadata.shared_source_id.is_none())
    );
    sandbox.stop();
}
