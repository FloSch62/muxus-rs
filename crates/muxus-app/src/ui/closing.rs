//! Closing tabs and panes: `requestCloseTabs`, `requestClosePane` and the
//! preference-gated "this ends a live session" question of
//! `client/src/session-actions.ts`.

use muxus_app::state::tabs::TabStatus;
use muxus_app::state::workspace_layout::PaneNode;
use muxus_ui::gpui_kit::{AppContext, Context, Entity, Window};

use crate::ui::confirm::{ConfirmAction, ConfirmDialog, ConfirmEvent, ConfirmRequest};
use crate::ui::shell::AppShell;

impl AppShell {
    /// The confirmation for closing `targets`, when one of them is live and
    /// the preference asks (`confirmEndingLiveSessions`).
    fn ending_live_sessions(&self, targets: &[String], title: &str) -> Option<ConfirmRequest> {
        let live: Vec<&str> = self
            .tabs
            .tabs
            .iter()
            .filter(|tab| targets.contains(&tab.id) && tab.status == TabStatus::Connected)
            .map(|tab| tab.title.as_str())
            .collect();
        if live.is_empty() || !self.prefs.confirm_close_connected {
            return None;
        }
        Some(ConfirmRequest {
            title: title.to_string(),
            description: Some(if live.len() == 1 {
                format!("“{}” has a live session — closing ends it.", live[0])
            } else {
                format!(
                    "{} tabs have live sessions — closing ends them.",
                    live.len()
                )
            }),
            confirm_label: "Close".into(),
            destructive: true,
            checkbox: Some("Don’t ask again".into()),
            action: ConfirmAction::CloseTabs(targets.to_vec()),
        })
    }

    /// Close tabs, asking first when any of them still has a live session.
    pub fn request_close_tabs(
        &mut self,
        ids: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets: Vec<String> = ids
            .into_iter()
            .filter(|id| self.tabs.tab(id).is_some())
            .collect();
        if targets.is_empty() {
            return;
        }
        let title = if targets.len() == 1 {
            "Close this tab?".to_string()
        } else {
            format!("Close {} tabs?", targets.len())
        };
        match self.ending_live_sessions(&targets, &title) {
            Some(request) => self.open_confirm(request, window, cx),
            None => self.close_tabs_after_editors(targets, None, window, cx),
        }
    }

    /// `confirmDiscardRemoteEditors`: one question covers every unsaved file in the tabs
    /// about to close (or the pane), then they close.
    pub fn close_tabs_after_editors(
        &mut self,
        targets: Vec<String>,
        pane: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<&str> = targets.iter().map(String::as_str).collect();
        match muxus_app::features::sftp::editor::registry::confirm_discard_remote_editors(&ids) {
            Some(options) => self.open_confirm(
                ConfirmRequest {
                    title: options.title,
                    description: options.description,
                    confirm_label: options.confirm_label,
                    destructive: options.destructive,
                    checkbox: None,
                    action: match pane {
                        Some(pane) => ConfirmAction::DiscardAndClosePane(pane),
                        None => ConfirmAction::DiscardAndCloseTabs(targets),
                    },
                },
                window,
                cx,
            ),
            None => match pane {
                Some(pane) => {
                    self.tabs.close_pane(&pane);
                    self.tabs_changed(cx);
                }
                None => self.close_tabs_now(&targets, cx),
            },
        }
    }

    /// Close a split pane and every tab in it, with the same question.
    pub fn request_close_pane(
        &mut self,
        pane_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.tabs.root, PaneNode::Pane(_)) {
            return;
        }
        let targets: Vec<String> = self
            .tabs
            .tabs
            .iter()
            .filter(|tab| tab.pane_id == pane_id)
            .map(|tab| tab.id.clone())
            .collect();
        match self.ending_live_sessions(&targets, "Close this pane?") {
            Some(mut request) => {
                request.action = ConfirmAction::ClosePane(pane_id.to_string());
                self.open_confirm(request, window, cx);
            }
            None => self.close_tabs_after_editors(targets, Some(pane_id.to_string()), window, cx),
        }
    }

    pub fn close_tabs_now(&mut self, ids: &[String], cx: &mut Context<Self>) {
        for id in ids {
            self.tabs.close(id);
        }
        self.tabs_changed(cx);
    }

    pub fn open_confirm(
        &mut self,
        request: ConfirmRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        let dialog = cx.new(|cx| ConfirmDialog::new(request, window, cx));
        let subscription = cx.subscribe_in(&dialog, window, Self::on_confirm_event);
        self.confirm = Some((dialog, subscription));
        cx.notify();
    }

    fn on_confirm_event(
        &mut self,
        dialog: &Entity<ConfirmDialog>,
        event: &ConfirmEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = dialog.read(cx).request.clone();
        self.confirm = None;
        // The keyboard goes back to the active terminal.
        self.focused_tab = None;
        if let ConfirmEvent::Confirmed { checked } = event {
            if *checked && request.checkbox.is_some() {
                self.prefs.confirm_close_connected = false;
                self.prefs_changed(cx);
            }
            match request.action {
                ConfirmAction::CloseTabs(ids) => {
                    self.close_tabs_after_editors(ids, None, window, cx)
                }
                ConfirmAction::ClosePane(pane) => {
                    let ids = self
                        .tabs
                        .tabs
                        .iter()
                        .filter(|tab| tab.pane_id == pane)
                        .map(|tab| tab.id.clone())
                        .collect();
                    self.close_tabs_after_editors(ids, Some(pane), window, cx)
                }
                ConfirmAction::DiscardAndCloseTabs(ids) => self.close_tabs_now(&ids, cx),
                ConfirmAction::DiscardAndClosePane(pane) => {
                    self.tabs.close_pane(&pane);
                    self.tabs_changed(cx);
                }
                ConfirmAction::DeleteHosts(keys) => self.delete_hosts_now(&keys, cx),
                ConfirmAction::ForceReconnect(id) => {
                    self.tabs.force_reconnect(&id);
                    self.tabs_changed(cx);
                }
                ConfirmAction::ForceReconnectAll => {
                    let count = self
                        .tabs
                        .tabs
                        .iter()
                        .filter(|tab| muxus_app::state::tabs::is_remote_session_tab(tab))
                        .count();
                    self.tabs.force_reconnect_all();
                    self.show_toast(
                        muxus_ui::theme::Status::Info,
                        format!(
                            "Reconnecting {count} remote session{}…",
                            if count == 1 { "" } else { "s" }
                        ),
                        cx,
                    );
                    self.tabs_changed(cx);
                }
                ConfirmAction::DeleteFolder { key, path, label } => {
                    self.delete_folder_now(&key, &path, &label, cx)
                }
            }
        }
        cx.notify();
    }
}
