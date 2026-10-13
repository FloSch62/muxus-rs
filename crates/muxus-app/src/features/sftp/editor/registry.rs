//! `client/src/editor/remote-editor-registry.ts`: the open editor workspaces by
//! the tab (or SFTP window) that holds them, so closing a tab can ask about
//! unsaved files and the close-file chord reaches the file in front first.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::super::dialogs::ConfirmOptions;

/// What a workspace answers for the registry.
pub trait RemoteEditorHandle {
    fn has_dirty(&self) -> bool;
    /// Close the file in front; false when the terminal is in front instead.
    fn close_active(&self) -> bool;
}

thread_local! {
    static HANDLES: RefCell<HashMap<String, Rc<dyn RemoteEditorHandle>>> =
        RefCell::new(HashMap::new());
}

/// Removes its registration when dropped or [`unregister`](Self::unregister)ed,
/// unless a newer one for the same tab replaced it meanwhile.
#[must_use = "dropping the registration unregisters the editor"]
pub struct RemoteEditorRegistration {
    tab_id: String,
    handle: Option<Rc<dyn RemoteEditorHandle>>,
}

impl RemoteEditorRegistration {
    pub fn unregister(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        HANDLES.with(|handles| {
            let mut handles = handles.borrow_mut();
            if handles
                .get(&self.tab_id)
                .is_some_and(|current| Rc::ptr_eq(current, &handle))
            {
                handles.remove(&self.tab_id);
            }
        });
    }
}

impl Drop for RemoteEditorRegistration {
    fn drop(&mut self) {
        self.unregister();
    }
}

pub fn register_remote_editor(
    tab_id: &str,
    handle: Rc<dyn RemoteEditorHandle>,
) -> RemoteEditorRegistration {
    HANDLES.with(|handles| {
        handles
            .borrow_mut()
            .insert(tab_id.to_string(), handle.clone())
    });
    RemoteEditorRegistration {
        tab_id: tab_id.to_string(),
        handle: Some(handle),
    }
}

/// One deliberate confirmation covers every dirty file in the requested tab
/// set: `None` when nothing is dirty (close at once), else the question to ask.
pub fn confirm_discard_remote_editors(tab_ids: &[&str]) -> Option<ConfirmOptions> {
    let dirty = HANDLES.with(|handles| {
        let handles = handles.borrow();
        tab_ids
            .iter()
            .any(|id| handles.get(*id).is_some_and(|handle| handle.has_dirty()))
    });
    dirty.then(|| ConfirmOptions {
        title: "Discard unsaved files?".into(),
        description: Some(
            "One or more files have unsaved changes. Closing them now loses those edits.".into(),
        ),
        confirm_label: "Discard changes".into(),
        destructive: true,
        ..ConfirmOptions::default()
    })
}

/// Route the close-file chord to the active editor tab before the containing
/// terminal or SFTP window is considered for closing.
pub fn request_close_remote_editor(tab_id: &str) -> bool {
    let handle = HANDLES.with(|handles| handles.borrow().get(tab_id).cloned());
    handle.is_some_and(|handle| handle.close_active())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct Handle {
        closes: Cell<usize>,
        answer: bool,
        dirty: bool,
    }

    impl RemoteEditorHandle for Handle {
        fn has_dirty(&self) -> bool {
            self.dirty
        }
        fn close_active(&self) -> bool {
            self.closes.set(self.closes.get() + 1);
            self.answer
        }
    }

    fn handle(answer: bool) -> Rc<Handle> {
        Rc::new(Handle {
            closes: Cell::new(0),
            answer,
            dirty: false,
        })
    }

    #[test]
    fn routes_a_close_request_to_the_registered_editor_and_cleans_up_safely() {
        let editor = handle(true);
        let mut registration = register_remote_editor("tab-1", editor.clone());
        assert!(request_close_remote_editor("tab-1"));
        assert_eq!(editor.closes.get(), 1);
        registration.unregister();
        assert!(!request_close_remote_editor("tab-1"));
    }

    #[test]
    fn does_not_let_an_older_cleanup_remove_a_replacement_handle() {
        let mut old = register_remote_editor("tab-2", handle(true));
        let replacement = handle(true);
        let mut current = register_remote_editor("tab-2", replacement.clone());
        old.unregister();
        assert!(request_close_remote_editor("tab-2"));
        assert_eq!(replacement.closes.get(), 1);
        current.unregister();
    }

    #[test]
    fn lets_the_close_request_through_to_the_tab_while_its_terminal_is_in_front() {
        let _registration = register_remote_editor("tab-3", handle(false));
        assert!(!request_close_remote_editor("tab-3"));
    }

    #[test]
    fn asks_once_for_every_dirty_file_in_the_tabs() {
        let dirty = Rc::new(Handle {
            closes: Cell::new(0),
            answer: true,
            dirty: true,
        });
        let _clean = register_remote_editor("tab-4", handle(true));
        let _dirty = register_remote_editor("tab-5", dirty);
        assert!(confirm_discard_remote_editors(&["tab-4"]).is_none());
        let question = confirm_discard_remote_editors(&["tab-4", "tab-5"]).unwrap();
        assert_eq!(question.title, "Discard unsaved files?");
        assert!(question.destructive);
    }
}
