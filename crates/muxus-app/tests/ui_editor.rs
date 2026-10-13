//! The remote editor workspace in a headless window, on a live SFTP connection.

mod support;

use std::time::Duration;

use gpui::{Focusable, TestAppContext};
use muxus_app::features::sftp::editor::workspace::{
    EditorSource, RemoteEditorOptions, RemoteEditorWorkspace,
};
use muxus_ui::gpui_kit::{
    AppContext, Context, Entity, FocusHandle, IntoElement, ParentElement, Render, Window, div,
};
use support::{Fixture, allow_backend_threads, init_ui, settle, wait_until};

/// The window: the editor workspace and a field elsewhere (the file browser's path, the
/// sidebar search) that can take the keyboard.
struct Scene {
    editor: Entity<RemoteEditorWorkspace>,
    elsewhere: FocusHandle,
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(self.editor.clone())
    }
}

/// The shell hands the workspace its state on every render. The workspace took the
/// keyboard back each time, so whatever the user typed into another field went into the
/// open file instead (and Ctrl+A, Delete wiped it).
#[gpui::test]
fn the_open_file_takes_the_keyboard_once_not_on_every_sync(cx: &mut TestAppContext) {
    let fx = Fixture::start(|root| {
        std::fs::write(root.join("notes.txt"), "keep me\n").unwrap();
    });
    allow_backend_threads(cx);
    cx.update(|cx| {
        init_ui(&fx.backend, cx);
    });
    let conn = fx.conn_id.clone();
    let window = cx.add_window(|window, cx| {
        let mut options = RemoteEditorOptions::new("tab-1", EditorSource::Sftp(Some(conn)));
        options.paths = vec!["/notes.txt".into()];
        options.active_path = Some("/notes.txt".into());
        options.active = true;
        let editor = cx.new(|cx| RemoteEditorWorkspace::new(options, window, cx));
        Scene {
            editor,
            elsewhere: cx.focus_handle(),
        }
    });
    let editor_focused = |cx: &mut TestAppContext| {
        window
            .update(cx, |scene, window, cx| {
                !scene.elsewhere.is_focused(window) && window.focused(cx).is_some()
            })
            .unwrap()
    };
    wait_until(
        cx,
        Duration::from_secs(20),
        "the file to load and take focus",
        |cx| editor_focused(cx),
    );

    // The user clicks another field; the shell keeps syncing.
    window
        .update(cx, |scene, window, cx| window.focus(&scene.elsewhere, cx))
        .unwrap();
    for _ in 0..5 {
        window
            .update(cx, |scene, window, cx| {
                scene.editor.update(cx, |editor, cx| {
                    editor.set_active(true, window, cx);
                    editor.set_terminal_shown(false, window, cx);
                })
            })
            .unwrap();
        settle(cx, Duration::from_millis(20));
    }
    assert!(
        window
            .update(cx, |scene, window, _| scene.elsewhere.is_focused(window))
            .unwrap(),
        "the editor took the keyboard back"
    );

    // Coming back to the front (another tab, then this one) takes it again.
    window
        .update(cx, |scene, window, cx| {
            scene.editor.update(cx, |editor, cx| {
                editor.set_active(false, window, cx);
                editor.set_active(true, window, cx);
            })
        })
        .unwrap();
    settle(cx, Duration::from_millis(20));
    assert!(editor_focused(cx));
    let _ = window.update(cx, |scene, _, cx| scene.editor.focus_handle(cx));
}
