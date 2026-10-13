//! Tabs whose session is not a terminal: remote desktops (`RemoteDesktopView.tsx`) and
//! management workbenches (`ManagementView.tsx`). Each tab gets its feature view; its status,
//! questions and toasts come back to the window as they do for terminals.

use std::collections::{HashMap, HashSet};

use muxus_app::features::management::{
    ManagementPrompt, ManagementView, ManagementViewEvent, TabStatus as ManagementStatus,
    open_management_view,
};
use muxus_app::features::remote_desktop::view::{
    DesktopPrompt, RemoteDesktopView, RemoteDesktopViewEvent, TabStatus as DesktopStatus,
    open_remote_desktop_view,
};
use muxus_app::model::SessionProfile;
use muxus_app::state::tabs::{DisconnectReason, TabPatch, TabStatus};
use muxus_core::protocol::{DesktopProfile, ExitReason, ManagementProfile};
use muxus_ui::gpui_kit::{AnyView, Context, Entity, Subscription, Window};

use crate::ui::prompts::{PromptAnswer, SessionPrompt};
use crate::ui::shell::AppShell;

/// The view of one non-terminal tab.
pub enum SessionView {
    Management(Entity<ManagementView>),
    Desktop(Entity<RemoteDesktopView>),
}

/// A tab's view and what the window remembers about it.
pub struct TabView {
    pub view: SessionView,
    seen_reconnect_request: u32,
    title: String,
    active: bool,
    _subscription: Subscription,
}

impl TabView {
    pub fn any_view(&self) -> AnyView {
        match &self.view {
            SessionView::Management(view) => view.clone().into(),
            SessionView::Desktop(view) => view.clone().into(),
        }
    }
}

fn exit_reason(reason: ExitReason) -> DisconnectReason {
    match reason {
        ExitReason::Completed => DisconnectReason::Completed,
        ExitReason::Failed => DisconnectReason::Failed,
        ExitReason::Disconnected => DisconnectReason::Disconnected,
    }
}

fn status_patch(
    status: TabStatus,
    failure_reason: Option<String>,
    disconnect_reason: Option<ExitReason>,
) -> TabPatch {
    TabPatch {
        status: Some(status),
        failure_reason: Some(failure_reason),
        disconnect_reason: Some(disconnect_reason.map(exit_reason)),
        ..Default::default()
    }
}

impl AppShell {
    /// Start the views of new remote desktop and management tabs, drop the ones whose tab
    /// closed, and pass on reconnects, titles and which tab is in front.
    pub fn sync_session_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let wanted: Vec<(String, String, SessionProfile, u32)> = self
            .tabs
            .tabs
            .iter()
            .filter_map(|tab| {
                let profile = tab.profile.clone()?;
                matches!(
                    profile,
                    SessionProfile::Rdp(_)
                        | SessionProfile::Vnc(_)
                        | SessionProfile::Gnmi(_)
                        | SessionProfile::Netconf(_)
                )
                .then(|| {
                    (
                        tab.id.clone(),
                        tab.title.clone(),
                        profile,
                        tab.reconnect_request,
                    )
                })
            })
            .collect();
        let live: HashSet<&str> = wanted.iter().map(|(id, ..)| id.as_str()).collect();
        let gone: Vec<String> = self
            .session_views
            .keys()
            .filter(|id| !live.contains(id.as_str()))
            .cloned()
            .collect();
        for id in gone {
            if let Some(view) = self.session_views.remove(&id) {
                match view.view {
                    SessionView::Management(view) => {
                        view.update(cx, |view, cx| view.disconnect(cx))
                    }
                    SessionView::Desktop(view) => view.update(cx, |view, cx| view.disconnect(cx)),
                }
            }
            self.close_prompts_of(&id, cx);
        }
        let auto_reconnect = self.prefs.auto_reconnect_remote;
        let active = self.tabs.active_id.clone();
        for (id, title, profile, reconnect_request) in wanted {
            let is_active = active.as_deref() == Some(id.as_str());
            if let Some(existing) = self.session_views.get_mut(&id) {
                let reconnect = existing.seen_reconnect_request != reconnect_request;
                existing.seen_reconnect_request = reconnect_request;
                let retitle = existing.title != title;
                existing.title = title.clone();
                let activate = existing.active != is_active;
                existing.active = is_active;
                match &existing.view {
                    SessionView::Management(view) => {
                        let view = view.clone();
                        view.update(cx, |view, cx| {
                            view.set_auto_reconnect(auto_reconnect);
                            if retitle {
                                view.set_title(title.clone(), cx);
                            }
                            if activate {
                                view.set_active(is_active, cx);
                            }
                            if reconnect {
                                view.reconnect(window, cx);
                            }
                        });
                    }
                    SessionView::Desktop(view) => {
                        let view = view.clone();
                        view.update(cx, |view, cx| {
                            view.set_auto_reconnect(auto_reconnect);
                            if retitle {
                                view.set_title(title.clone(), cx);
                            }
                            if activate {
                                view.set_active(is_active, window, cx);
                            }
                            if reconnect {
                                view.reconnect(window, cx);
                            }
                        });
                    }
                }
                continue;
            }
            let tab_id = id.clone();
            let (view, subscription) = match profile {
                SessionProfile::Rdp(profile) => self.open_desktop_view(
                    &tab_id,
                    DesktopProfile::Rdp(profile),
                    &title,
                    &backend,
                    window,
                    cx,
                ),
                SessionProfile::Vnc(profile) => self.open_desktop_view(
                    &tab_id,
                    DesktopProfile::Vnc(profile),
                    &title,
                    &backend,
                    window,
                    cx,
                ),
                SessionProfile::Gnmi(profile) => self.open_management(
                    &tab_id,
                    ManagementProfile::Gnmi(profile),
                    &title,
                    &backend,
                    window,
                    cx,
                ),
                SessionProfile::Netconf(profile) => self.open_management(
                    &tab_id,
                    ManagementProfile::Netconf(profile),
                    &title,
                    &backend,
                    window,
                    cx,
                ),
                _ => continue,
            };
            match &view {
                SessionView::Management(view) => {
                    view.update(cx, |view, cx| {
                        view.set_auto_reconnect(auto_reconnect);
                        view.set_active(is_active, cx);
                    });
                }
                SessionView::Desktop(view) => {
                    view.update(cx, |view, cx| {
                        view.set_auto_reconnect(auto_reconnect);
                        view.set_active(is_active, window, cx);
                    });
                }
            }
            self.session_views.insert(
                id,
                TabView {
                    view,
                    seen_reconnect_request: reconnect_request,
                    title,
                    active: is_active,
                    _subscription: subscription,
                },
            );
        }
    }

    fn open_desktop_view(
        &mut self,
        tab_id: &str,
        profile: DesktopProfile,
        title: &str,
        backend: &muxus_session::Backend,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (SessionView, Subscription) {
        let view = open_remote_desktop_view(
            backend.clone(),
            profile,
            title.to_string(),
            true,
            window,
            cx,
        );
        let tab_id = tab_id.to_string();
        let subscription = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &RemoteDesktopViewEvent, _, cx| match event {
                RemoteDesktopViewEvent::Status {
                    status,
                    failure_reason,
                    disconnect_reason,
                } => {
                    let status = match status {
                        DesktopStatus::Connecting => TabStatus::Connecting,
                        DesktopStatus::Connected => TabStatus::Connected,
                        DesktopStatus::Closed => TabStatus::Closed,
                    };
                    this.tabs.update(
                        &tab_id,
                        status_patch(status, failure_reason.clone(), *disconnect_reason),
                    );
                    this.tabs_changed(cx);
                }
                RemoteDesktopViewEvent::Prompt(prompt) => {
                    let prompt = match prompt.clone() {
                        DesktopPrompt::Auth(info) => SessionPrompt::Auth(info),
                        DesktopPrompt::HostKey(challenge) => SessionPrompt::HostKey(challenge),
                        DesktopPrompt::Certificate(challenge) => SessionPrompt::Certificate {
                            challenge,
                            gnmi: false,
                        },
                    };
                    this.queue_prompt(tab_id.clone(), prompt, None, cx);
                }
                RemoteDesktopViewEvent::PromptsClosed => this.close_prompts_of(&tab_id, cx),
                RemoteDesktopViewEvent::Toast { status, message } => {
                    this.show_toast(*status, message.clone(), cx)
                }
            },
        );
        (SessionView::Desktop(view), subscription)
    }

    fn open_management(
        &mut self,
        tab_id: &str,
        profile: ManagementProfile,
        title: &str,
        backend: &muxus_session::Backend,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (SessionView, Subscription) {
        let view = open_management_view(
            backend.clone(),
            profile,
            title.to_string(),
            true,
            window,
            cx,
        );
        let tab_id = tab_id.to_string();
        let subscription = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &ManagementViewEvent, _, cx| match event {
                ManagementViewEvent::Status {
                    status,
                    failure_reason,
                    disconnect_reason,
                } => {
                    let status = match status {
                        ManagementStatus::Connecting => TabStatus::Connecting,
                        ManagementStatus::Connected => TabStatus::Connected,
                        ManagementStatus::Closed => TabStatus::Closed,
                    };
                    this.tabs.update(
                        &tab_id,
                        status_patch(status, failure_reason.clone(), *disconnect_reason),
                    );
                    this.tabs_changed(cx);
                }
                ManagementViewEvent::Prompt(prompt) => {
                    let prompt = match prompt.clone() {
                        ManagementPrompt::Auth(info) => SessionPrompt::Auth(info),
                        ManagementPrompt::HostKey(challenge) => SessionPrompt::HostKey(challenge),
                        ManagementPrompt::Certificate(challenge) => SessionPrompt::Certificate {
                            challenge,
                            gnmi: true,
                        },
                    };
                    this.queue_prompt(tab_id.clone(), prompt, None, cx);
                }
                ManagementViewEvent::PromptsClosed => this.close_prompts_of(&tab_id, cx),
                ManagementViewEvent::Toast { status, message } => {
                    this.show_toast(*status, message.clone(), cx)
                }
            },
        );
        (SessionView::Management(view), subscription)
    }

    /// Answer a question a remote desktop or management view asked; `false` when `key`
    /// is not such a tab.
    pub fn answer_view_prompt(
        &mut self,
        key: &str,
        answer: &PromptAnswer,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(view) = self.session_views.get(key) else {
            return false;
        };
        match (&view.view, answer.clone()) {
            (SessionView::Management(view), answer) => view.update(cx, |view, cx| match answer {
                PromptAnswer::Auth(response) => view.answer_auth(response, cx),
                PromptAnswer::HostKey(accept) => view.answer_host_key(accept, cx),
                PromptAnswer::Certificate(accept) => view.answer_certificate(accept, cx),
            }),
            (SessionView::Desktop(view), answer) => view.update(cx, |view, cx| match answer {
                PromptAnswer::Auth(response) => view.answer_auth(response, cx),
                PromptAnswer::HostKey(accept) => view.answer_host_key(accept, cx),
                PromptAnswer::Certificate(accept) => view.answer_certificate(accept, cx),
            }),
        }
        true
    }
}

/// Views keyed by tab id.
pub type SessionViews = HashMap<String, TabView>;
