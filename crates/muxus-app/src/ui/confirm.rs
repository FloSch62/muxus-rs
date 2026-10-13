//! Confirmations: the `confirm` half of `client/src/components/DialogHost.tsx`.
//! Every confirmation has the same anatomy: title, plain-language
//! consequence, Cancel on the left of the committing button. Enter commits,
//! Escape keeps things as they are.

use muxus_ui::gpui_kit::{
    Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, Styled, Window, div, px,
};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::ActiveTheme;
use muxus_ui::widgets::{Button, ButtonColor, Checkbox, Dialog, DialogWidth};

/// What a confirmed request does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmAction {
    CloseTabs(Vec<String>),
    ClosePane(String),
    /// Close after unsaved files were confirmed lost.
    DiscardAndCloseTabs(Vec<String>),
    DiscardAndClosePane(String),
    /// Replace every remote connection.
    ForceReconnectAll,
    /// Replace one tab's connection.
    ForceReconnect(String),
    /// Delete these hosts (by key).
    DeleteHosts(Vec<String>),
    DeleteFolder {
        key: String,
        path: String,
        label: String,
    },
}

/// `confirmAction`'s options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmRequest {
    pub title: String,
    pub description: Option<String>,
    pub confirm_label: String,
    pub destructive: bool,
    /// A "Don't ask again" style checkbox, applied when confirmed.
    pub checkbox: Option<String>,
    pub action: ConfirmAction,
}

/// The answer: confirmed (with the checkbox state) or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmEvent {
    Confirmed { checked: bool },
    Cancelled,
}

pub struct ConfirmDialog {
    pub request: ConfirmRequest,
    checked: bool,
    focus: FocusHandle,
}

impl EventEmitter<ConfirmEvent> for ConfirmDialog {}

impl Focusable for ConfirmDialog {
    fn focus_handle(&self, _: &muxus_ui::gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConfirmDialog {
    pub fn new(request: ConfirmRequest, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            request,
            checked: false,
            focus,
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                cx.stop_propagation();
                cx.emit(ConfirmEvent::Confirmed {
                    checked: self.checked,
                });
            }
            "escape" => {
                cx.stop_propagation();
                cx.emit(ConfirmEvent::Cancelled);
            }
            _ => {}
        }
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let body2 = theme.typography.body2;
        let entity = cx.entity().downgrade();
        let (cancel, confirm, close, toggle) =
            (entity.clone(), entity.clone(), entity.clone(), entity);
        let checked = self.checked;
        let mut dialog = Dialog::new()
            .title(self.request.title.clone())
            .max_width(DialogWidth::Xs)
            .full_width()
            .on_close(move |_, cx| {
                let _ = close.update(cx, |_, cx| cx.emit(ConfirmEvent::Cancelled));
            });
        if let Some(description) = &self.request.description {
            dialog = dialog.content(
                paragraph(description.clone())
                    .size(body2.size)
                    .line_height(body2.line_height)
                    .color(p.text.secondary),
            );
        }
        if let Some(checkbox) = &self.request.checkbox {
            dialog = dialog.content(
                div()
                    .mt(px(8.))
                    .ml(px(-11.))
                    .flex()
                    .items_center()
                    .child(Checkbox::new("confirm-checkbox", checked).small().on_click(
                        move |_, _, cx| {
                            let _ = toggle.update(cx, |this, cx| {
                                this.checked = !this.checked;
                                cx.notify();
                            });
                        },
                    ))
                    .child(
                        label(checkbox.clone())
                            .size(body2.size)
                            .line_height(body2.line_height)
                            .color(p.text.primary),
                    ),
            );
        }
        dialog = dialog
            .action(
                Button::new("confirm-cancel", "Cancel").on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(ConfirmEvent::Cancelled));
                }),
            )
            .action(
                Button::new("confirm-commit", self.request.confirm_label.clone())
                    .contained()
                    .color(if self.request.destructive {
                        ButtonColor::Error
                    } else {
                        ButtonColor::Primary
                    })
                    .on_click(move |_, _, cx| {
                        let _ = confirm.update(cx, |this, cx| {
                            cx.emit(ConfirmEvent::Confirmed {
                                checked: this.checked,
                            })
                        });
                    }),
            );
        div()
            .id("confirm-dialog")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(dialog)
    }
}
