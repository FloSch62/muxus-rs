//! `components/SharedSessionsGroup.tsx`: every session list Muxus follows, in Settings →
//! Backup & data. The same actions sit on each link's folder in the sidebar; this is the
//! overview.

use muxus_core::api::SharedSessionSource;
use muxus_ui::gpui_kit::{
    AnyElement, Context, Focusable, IntoElement, ParentElement, SharedString, Styled, Window, div,
    px,
};
use muxus_ui::label::label;
use muxus_ui::theme::{Status, Theme};
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::{Button, ButtonColor, IconButton};

use super::super::texts;
use super::kit::{BODY2, CAPTION, group, secondary_text, snap, status_text};
use super::passwords::{bordered, empty_list};
use super::window::SettingsWindow;
use crate::features::shared_sessions::dialog::{
    SharedSessionsDialogEvent, open_shared_sessions_dialog,
};
use crate::features::shared_sessions::logic::{
    GROUP_DESCRIPTION, shared_sessions_state, source_caption,
};
use crate::features::shared_sessions::stop::{StopFollowingEvent, open_stop_following};
use crate::features::shared_sessions::store;

/// A shared sessions dialog open over the settings.
pub enum SharedSessionsModal {
    Link(
        muxus_ui::gpui_kit::Entity<crate::features::shared_sessions::dialog::SharedSessionsDialog>,
    ),
    Stop(muxus_ui::gpui_kit::Entity<crate::features::shared_sessions::stop::StopFollowingDialog>),
}

impl SharedSessionsModal {
    pub fn element(&self) -> AnyElement {
        match self {
            SharedSessionsModal::Link(dialog) => dialog.clone().into_any_element(),
            SharedSessionsModal::Stop(dialog) => dialog.clone().into_any_element(),
        }
    }
}

impl SettingsWindow {
    pub(super) fn shared_sessions_group(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let sources = store::listed(cx).to_vec();
        let mut section = group(texts::counted_title("Shared sessions", sources.len()))
            .description(secondary_text(theme, GROUP_DESCRIPTION))
            .action(
                Button::new("shared-sessions-add", "Add link")
                    .start_icon("Add")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_shared_sessions_link(None, window, cx)
                    })),
            )
            .flush();
        if sources.is_empty() {
            section = section.row(empty_list(
                theme,
                if store::loaded(cx) {
                    "No links yet."
                } else {
                    "Loading…"
                },
            ));
        }
        for (index, source) in sources.iter().enumerate() {
            let busy = store::is_reading(&source.id, cx);
            let (tone, said) = if busy {
                (Status::Info, "Reading the list…".to_owned())
            } else {
                shared_sessions_state(source)
            };
            let id = |part: &str| SharedString::from(format!("shared-sessions-{part}-{index}"));
            let (refresh, edit, stop) = (source.clone(), source.clone(), source.clone());
            let refresh_button = if busy {
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .child(CircularProgress::new(id("reading"), 16., p.primary.main))
                    .into_any_element()
            } else {
                IconButton::new(id("refresh"), "Refresh")
                    .icon_size(18.)
                    .tooltip("Refresh now")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        store::refresh_now(&this.backend, &refresh, cx)
                    }))
                    .into_any_element()
            };
            let buttons = div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(2.))
                .child(refresh_button)
                .child(
                    IconButton::new(id("edit"), "EditOutlined")
                        .icon_size(18.)
                        .tooltip("Edit")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_shared_sessions_link(Some(edit.clone()), window, cx)
                        })),
                )
                .child(
                    IconButton::new(id("stop"), "DeleteOutlined")
                        .icon_size(18.)
                        .color(ButtonColor::Error)
                        .tooltip("Stop following")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.stop_following_shared_sessions(stop.clone(), window, cx)
                        })),
                );
            let entry = div()
                .flex()
                .items_center()
                .gap(px(12.))
                .px(px(16.))
                .py(px(8.8))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            label(source.name.clone())
                                .size(BODY2)
                                .weight(550.)
                                .line_height(snap(BODY2 * 1.43))
                                .color(p.text.primary),
                        )
                        .child(
                            label(source_caption(source))
                                .size(CAPTION)
                                .line_height(snap(CAPTION * 1.66))
                                .color(p.text.secondary),
                        )
                        .child(div().mt(px(2.)).child(status_text(theme, tone, said))),
                )
                .child(buttons)
                .into_any_element();
            section = section.row(bordered(theme, index > 0, entry));
        }
        section.render(theme)
    }

    /// Follow a new link, or change `source`. A saved link is read at once.
    pub fn open_shared_sessions_link(
        &mut self,
        source: Option<SharedSessionSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = open_shared_sessions_dialog(self.backend.clone(), source, window, cx);
        cx.subscribe_in(
            &dialog,
            window,
            |this, _, event: &SharedSessionsDialogEvent, window, cx| {
                this.state.shared_sessions = None;
                window.focus(&this.focus_handle(cx), cx);
                if let SharedSessionsDialogEvent::Saved(saved) = event {
                    store::refresh_now(&this.backend, saved, cx);
                }
                cx.notify();
            },
        )
        .detach();
        self.state.shared_sessions = Some(SharedSessionsModal::Link(dialog));
        cx.notify();
    }

    /// Ask before a link is removed, and what happens to its hosts.
    pub fn stop_following_shared_sessions(
        &mut self,
        source: SharedSessionSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = source.id.clone();
        let dialog = open_stop_following(source, window, cx);
        cx.subscribe_in(
            &dialog,
            window,
            move |this, _, event: &StopFollowingEvent, window, cx| {
                this.state.shared_sessions = None;
                window.focus(&this.focus_handle(cx), cx);
                if let StopFollowingEvent::Confirmed { keep_hosts } = event {
                    store::stop_following(&this.backend, &id, *keep_hosts, cx);
                }
                cx.notify();
            },
        )
        .detach();
        self.state.shared_sessions = Some(SharedSessionsModal::Stop(dialog));
        cx.notify();
    }
}
