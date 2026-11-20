//! The password-manager views against a sandboxed backend and the KeePass database
//! KeePassXC wrote: the entry picker unlocks it, lists and searches its entries and hands
//! back a reference, never a password.

mod support;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::TestAppContext;
use muxus_app::features::password_managers::picker::{
    PasswordEntryPicker, PasswordEntryPickerEvent,
};
use muxus_app::features::password_managers::store;
use muxus_core::api::{ExternalPasswordRef, PasswordManagerSource, PasswordManagerState};
use muxus_session::{Backend, BackendOptions, RuntimeChoice};
use support::{allow_backend_threads, wait_until};

const WAIT: Duration = Duration::from_secs(20);

fn database() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../muxus-passwords/tests/fixtures/keepassxc-kdbx3.kdbx")
}

/// A sandboxed backend on its own runtime.
struct Sandbox {
    backend: Backend,
    runtime: tokio::runtime::Runtime,
    _scratch: tempfile::TempDir,
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
            _scratch: scratch,
        }
    }

    /// Connect the KeePass database; it starts locked.
    fn connect_database(&self, name: &str) -> PasswordManagerSource {
        self.runtime
            .block_on(
                self.backend.create_password_manager(
                    serde_json::from_value(serde_json::json!({
                        "kind": "keepass", "name": name,
                        "config": { "path": database().to_string_lossy() },
                    }))
                    .unwrap(),
                ),
            )
            .expect("the database connects")
    }
}

fn open_picker(
    sandbox: &Sandbox,
    value: Option<ExternalPasswordRef>,
    suggestion: Option<&str>,
    cx: &mut TestAppContext,
) -> (
    gpui::WindowHandle<PasswordEntryPicker>,
    Rc<RefCell<Vec<PasswordEntryPickerEvent>>>,
) {
    cx.update(|cx| muxus_ui::init(muxus_ui::theme::Mode::Light, cx));
    let backend = sandbox.backend.clone();
    let suggestion = suggestion.map(str::to_owned);
    let window = cx
        .add_window(|window, cx| PasswordEntryPicker::new(backend, value, suggestion, window, cx));
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    window
        .update(cx, |_, _, cx| {
            let picker = cx.entity();
            cx.subscribe(&picker, move |_, _, event: &PasswordEntryPickerEvent, _| {
                seen.borrow_mut().push(event.clone());
            })
            .detach();
        })
        .unwrap();
    (window, events)
}

fn titles(
    window: &gpui::WindowHandle<PasswordEntryPicker>,
    cx: &mut TestAppContext,
) -> Vec<String> {
    window
        .update(cx, |picker, _, cx| picker.entry_titles(cx))
        .unwrap()
}

#[gpui::test]
fn unlocks_a_database_lists_its_entries_and_hands_back_a_reference(cx: &mut TestAppContext) {
    allow_backend_threads(cx);
    let sandbox = Sandbox::start();
    let source = sandbox.connect_database("Work");
    assert_eq!(source.state, PasswordManagerState::Locked);
    let (window, events) = open_picker(&sandbox, None, Some("web1"), cx);

    // The database is locked: the picker offers to unlock it right there.
    wait_until(cx, WAIT, "the unlock form", |cx| {
        window
            .update(cx, |picker, _, _| picker.unlock_form().is_some())
            .unwrap()
    });
    assert!(titles(&window, cx).is_empty());
    let form = window
        .update(cx, |picker, _, _| picker.unlock_form().unwrap())
        .unwrap();
    window
        .update(cx, |_, window, cx| {
            form.update(cx, |form, cx| {
                form.unlock_with("not-the-password", window, cx)
            });
        })
        .unwrap();
    wait_until(cx, WAIT, "the wrong password to be refused", |cx| {
        form.read_with(cx, |form, _| form.error().is_some())
    });
    assert_eq!(
        form.read_with(cx, |form, _| form.error().map(str::to_owned)),
        Some("The database password is wrong.".to_owned())
    );

    window
        .update(cx, |_, window, cx| {
            form.update(cx, |form, cx| form.unlock_with("master-pw-1", window, cx));
        })
        .unwrap();
    wait_until(cx, WAIT, "the entries", |cx| {
        !titles(&window, cx).is_empty()
    });
    assert_eq!(titles(&window, cx), ["Core router", "web1"]);
    // Unlocked, there is nothing left to unlock.
    assert!(
        window
            .update(cx, |picker, _, _| picker.unlock_form().is_none())
            .unwrap()
    );
    // The list every view shares says so too.
    cx.update(|cx| {
        let listed = store::listed_sources(cx).expect("the managers are listed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].state, PasswordManagerState::Ready);
    });

    // What the entry is for is offered as a search while nothing is typed.
    let suggested = window
        .update(cx, |picker, _, cx| picker.suggested_search(cx))
        .unwrap();
    assert_eq!(suggested.as_deref(), Some("web1"));
    let search = |cx: &mut TestAppContext, text: &str| {
        window
            .update(cx, |picker, window, cx| picker.search_for(text, window, cx))
            .unwrap();
        cx.run_until_parked();
    };
    search(cx, "ADMIN");
    assert_eq!(titles(&window, cx), ["Core router"]);
    assert_eq!(
        window
            .update(cx, |picker, _, cx| picker.suggested_search(cx))
            .unwrap(),
        None
    );
    search(cx, "servers root");
    assert_eq!(titles(&window, cx), ["web1"]);
    search(cx, "nothing like it");
    assert!(titles(&window, cx).is_empty());
    // An entry the search hides cannot be picked.
    assert!(
        !window
            .update(cx, |picker, _, cx| picker.pick_titled("web1", cx))
            .unwrap()
    );
    assert!(events.borrow().is_empty());

    search(cx, "");
    assert!(
        window
            .update(cx, |picker, _, cx| picker.pick_titled("web1", cx))
            .unwrap()
    );
    cx.run_until_parked();
    let picked = events.borrow().clone();
    let [PasswordEntryPickerEvent::Picked(entry)] = picked.as_slice() else {
        panic!("one pick: {picked:?}");
    };
    assert_eq!(entry.source_id, source.id);
    assert_eq!(entry.title, "web1");
    assert_eq!(entry.username.as_deref(), Some("root"));
    // The reference names the entry; the password stays in the database.
    let said = serde_json::to_string(entry).unwrap();
    assert!(!said.contains("hunter2"), "{said}");
    // The backend reads the very entry the reference names.
    assert_eq!(
        sandbox
            .runtime
            .block_on(sandbox.backend.password_managers().password(entry))
            .unwrap(),
        "hunter2-säcret"
    );
    sandbox.runtime.block_on(sandbox.backend.shutdown());
}

#[gpui::test]
fn starts_in_the_manager_of_the_linked_entry_and_follows_it_when_it_locks(cx: &mut TestAppContext) {
    allow_backend_threads(cx);
    let sandbox = Sandbox::start();
    let first = sandbox.connect_database("Archive");
    let second = sandbox.connect_database("Work");
    // "Archive" sorts first, so it is the one shown without a linked entry.
    sandbox
        .runtime
        .block_on(async {
            sandbox
                .backend
                .unlock_password_manager(&first.id, "master-pw-1")
                .await?;
            sandbox
                .backend
                .unlock_password_manager(&second.id, "master-pw-1")
                .await
        })
        .unwrap();
    let linked = ExternalPasswordRef {
        source_id: second.id.clone(),
        entry_id: "anything".into(),
        title: "web1".into(),
        username: None,
    };
    let (window, events) = open_picker(&sandbox, Some(linked), None, cx);
    wait_until(cx, WAIT, "the entries", |cx| {
        !titles(&window, cx).is_empty()
    });
    assert!(
        window
            .update(cx, |picker, _, cx| picker.pick_titled("Core router", cx))
            .unwrap()
    );
    cx.run_until_parked();
    let picked = events.borrow().clone();
    let [PasswordEntryPickerEvent::Picked(entry)] = picked.as_slice() else {
        panic!("one pick: {picked:?}");
    };
    assert_eq!(entry.source_id, second.id, "picked from the linked manager");

    // Locked elsewhere (Settings, another window): the list goes and the unlock form comes.
    sandbox
        .runtime
        .block_on(sandbox.backend.lock_password_manager(&second.id))
        .unwrap();
    cx.update(|cx| store::refresh(&sandbox.backend, cx));
    wait_until(cx, WAIT, "the unlock form", |cx| {
        window
            .update(cx, |picker, _, _| picker.unlock_form().is_some())
            .unwrap()
    });
    assert!(titles(&window, cx).is_empty());
    sandbox.runtime.block_on(sandbox.backend.shutdown());
}
