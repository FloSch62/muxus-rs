//! Shared sessions in the window (`SharedSessionsWindowDialog.tsx` and the sidebar's
//! handlers in `SessionSidebar.tsx`): the dialogs the sidebar and the quick launcher open,
//! and what a folder's menu does with the link the folder follows.

use muxus_app::features::shared_sessions::dialog::{
    SharedSessionsDialogEvent, open_shared_sessions_dialog,
};
use muxus_app::features::shared_sessions::share::{ShareFolderEvent, open_share_folder};
use muxus_app::features::shared_sessions::stop::{StopFollowingEvent, open_stop_following};
use muxus_app::features::shared_sessions::store;
use muxus_core::api::SharedSessionSource;
use muxus_ui::gpui_kit::{Context, Window};

use crate::ui::shell::AppShell;

impl AppShell {
    /// The link with this ID, as last listed.
    fn shared_sessions_link(&self, id: &str, cx: &Context<Self>) -> Option<SharedSessionSource> {
        store::listed(cx)
            .iter()
            .find(|source| source.id == id)
            .cloned()
    }

    /// Follow a new shared sessions link, or change the one with this ID. A saved link is
    /// read at once.
    pub fn open_shared_sessions_link(
        &mut self,
        source: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let source = match source {
            // A link removed meanwhile has nothing to edit.
            Some(id) => match self.shared_sessions_link(&id, cx) {
                Some(source) => Some(source),
                None => return,
            },
            None => None,
        };
        let dialog = open_shared_sessions_dialog(backend, source, window, cx);
        self.mount_overlay(
            "shared-sessions",
            dialog,
            window,
            cx,
            |this, _, event: &SharedSessionsDialogEvent, window, cx| {
                this.close_overlay("shared-sessions", window, cx);
                if let SharedSessionsDialogEvent::Saved(saved) = event
                    && let Some(backend) = this.data.backend()
                {
                    store::refresh_now(&backend, saved, cx);
                }
            },
        );
    }

    /// Write a sidebar folder as a session list for others to follow.
    pub fn open_share_folder(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let dialog = open_share_folder(backend, path, window, cx);
        self.mount_overlay(
            "share-folder",
            dialog,
            window,
            cx,
            |this, _, event: &ShareFolderEvent, window, cx| match event {
                ShareFolderEvent::Close => this.close_overlay("share-folder", window, cx),
            },
        );
    }

    /// Read a link now and say what came of it.
    pub fn refresh_shared_sessions(&mut self, id: &str, cx: &mut Context<Self>) {
        if let (Some(backend), Some(source)) =
            (self.data.backend(), self.shared_sessions_link(id, cx))
        {
            store::refresh_now(&backend, &source, cx);
        }
    }

    /// Ask before a link is removed, and what happens to its hosts.
    pub fn stop_following_shared_sessions(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.shared_sessions_link(id, cx) else {
            return;
        };
        let id = source.id.clone();
        let dialog = open_stop_following(source, window, cx);
        self.mount_overlay(
            "stop-following",
            dialog,
            window,
            cx,
            move |this, _, event: &StopFollowingEvent, window, cx| {
                this.close_overlay("stop-following", window, cx);
                if let StopFollowingEvent::Confirmed { keep_hosts } = event
                    && let Some(backend) = this.data.backend()
                {
                    store::stop_following(&backend, &id, *keep_hosts, cx);
                }
            },
        );
    }
}
