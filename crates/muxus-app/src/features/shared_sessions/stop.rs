//! `confirmRemoveSharedSessions`: ask before a link is removed, and what happens to its
//! hosts.

use std::rc::Rc;

use muxus_core::api::SharedSessionSource;
use muxus_ui::gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, Styled, Window, div, px,
};
use muxus_ui::theme::ActiveTheme;
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::typography::{body2, para, text};
use muxus_ui::widgets::{Button, ButtonColor, Checkbox};

use super::logic::{keep_hosts_label, stop_following_description, stop_following_title};
use crate::features::host_editor::widgets::shell::{DIALOG_XS, dialog_frame};

/// The answer: stop following (keeping the hosts or not), or leave things as they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopFollowingEvent {
    Cancelled,
    Confirmed { keep_hosts: bool },
}

pub struct StopFollowingDialog {
    source: SharedSessionSource,
    keep_hosts: bool,
    focus: FocusHandle,
}

impl EventEmitter<StopFollowingEvent> for StopFollowingDialog {}

impl Focusable for StopFollowingDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

pub fn open_stop_following(
    source: SharedSessionSource,
    window: &mut Window,
    cx: &mut App,
) -> Entity<StopFollowingDialog> {
    cx.new(|cx| {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        StopFollowingDialog {
            source,
            keep_hosts: false,
            focus,
        }
    })
}

impl StopFollowingDialog {
    pub fn source(&self) -> &SharedSessionSource {
        &self.source
    }

    /// Keep the link's hosts as hosts of the user's own.
    pub fn set_keep_hosts(&mut self, keep: bool, cx: &mut Context<Self>) {
        self.keep_hosts = keep;
        cx.notify();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        cx.emit(StopFollowingEvent::Confirmed {
            keep_hosts: self.keep_hosts,
        });
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                cx.emit(StopFollowingEvent::Cancelled);
            }
            "enter" => {
                cx.stop_propagation();
                self.confirm(cx);
            }
            _ => {}
        }
    }
}

impl Render for StopFollowingDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let mut content = div().flex().flex_col();
        if let Some(description) = stop_following_description(&self.source) {
            content = content.child(para(description, body2(), p.text.secondary));
        }
        if let Some(keep) = keep_hosts_label(&self.source) {
            let toggle = cx.entity().downgrade();
            let checked = self.keep_hosts;
            content = content.child(
                div()
                    .mt(px(8.))
                    .ml(px(-11.))
                    .flex()
                    .items_center()
                    .child(
                        Checkbox::new("stop-following-keep", checked)
                            .small()
                            .on_click(move |_, _, cx| {
                                let _ =
                                    toggle.update(cx, |this, cx| this.set_keep_hosts(!checked, cx));
                            }),
                    )
                    .child(text(keep, body2(), p.text.primary)),
            );
        }
        let (cancel, confirm, close) = (
            cx.entity().downgrade(),
            cx.entity().downgrade(),
            cx.entity().downgrade(),
        );
        let actions = vec![
            Button::new("stop-following-cancel", "Cancel")
                .on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(StopFollowingEvent::Cancelled));
                })
                .into_any_element(),
            Button::new("stop-following-confirm", "Stop following")
                .contained()
                .color(ButtonColor::Error)
                .on_click(move |_, _, cx| {
                    let _ = confirm.update(cx, |this, cx| this.confirm(cx));
                })
                .into_any_element(),
        ];
        div()
            .id("stop-following-dialog")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(modal_layer(
                window,
                "stop-following-backdrop",
                dialog_frame(
                    &theme,
                    window,
                    "stop-following-content",
                    DIALOG_XS,
                    stop_following_title(&self.source),
                    8.,
                    content.into_any_element(),
                    actions,
                    true,
                ),
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = close.update(cx, |_, cx| cx.emit(StopFollowingEvent::Cancelled));
                })),
            ))
    }
}
