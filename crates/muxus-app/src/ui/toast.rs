//! `components/ToastHost.tsx`: the single snackbar for notifications, a
//! filled `Alert` centred 24px above the window's bottom edge. Errors and
//! warnings offer Copy (the message and its details); a toast with details
//! offers Details, which unfolds them below the message. Errors, and a toast
//! whose details are open, wait to be dismissed; everything else hides after
//! five seconds.

use std::time::Duration;

use muxus_ui::gpui_kit::{
    AnyElement, ClipboardItem, Context, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, deferred, div, px,
};
use muxus_ui::theme::{Status, fonts};
use muxus_ui::widgets::layer::TOAST_LAYER;
use muxus_ui::widgets::{Alert, Button, ButtonColor, IconButton};

use crate::ui::shell::AppShell;

/// How long a toast that does not wait to be dismissed stays up.
const AUTO_HIDE: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct Toast {
    /// Distinguishes consecutive toasts so the auto-hide timer restarts.
    pub id: u64,
    pub status: Status,
    pub message: SharedString,
    /// Full error text behind the Details expander and the Copy button.
    pub details: Option<SharedString>,
    /// Details unfolded (the toast then waits to be dismissed).
    pub expanded: bool,
    /// Copy succeeded: the button says "Copied".
    pub copied: bool,
}

impl Toast {
    /// Errors and warnings can be copied.
    pub fn copyable(&self) -> bool {
        matches!(self.status, Status::Error | Status::Warning)
    }

    /// Errors carry Details/Copy actions, so they wait to be dismissed rather
    /// than vanishing while they are being read; so does a toast being read.
    pub fn sticky(&self) -> bool {
        self.status == Status::Error || self.expanded
    }

    /// What Copy puts on the clipboard: the message, then the details.
    pub fn copy_text(&self) -> String {
        match &self.details {
            Some(details) if !details.is_empty() => format!("{}\n\n{details}", self.message),
            _ => self.message.to_string(),
        }
    }
}

impl AppShell {
    /// `showToast(severity, message)`.
    pub fn show_toast(
        &mut self,
        status: Status,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.show_toast_with_details(status, message, None, cx);
    }

    /// `showToast(severity, message, details)`.
    pub fn show_toast_with_details(
        &mut self,
        status: Status,
        message: impl Into<SharedString>,
        details: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.toast_seq += 1;
        let id = self.toast_seq;
        self.toast = Some(Toast {
            id,
            status,
            message: message.into(),
            details: details.filter(|details| !details.is_empty()),
            expanded: false,
            copied: false,
        });
        self.schedule_toast_hide(id, cx);
        cx.notify();
    }

    /// Hide toast `id` after five seconds unless it waits to be dismissed by then.
    fn schedule_toast_hide(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.toast.as_ref().is_none_or(Toast::sticky) {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTO_HIDE).await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .toast
                    .as_ref()
                    .is_some_and(|toast| toast.id == id && !toast.sticky())
                {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn render_toast(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let toast = self.toast.clone()?;
        let mut actions = div().flex().items_center();
        if toast.copyable() {
            let text = toast.copy_text();
            actions = actions.child(
                Button::new("copy-toast", if toast.copied { "Copied" } else { "Copy" })
                    .color(ButtonColor::Inherit)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                        if let Some(toast) = this.toast.as_mut() {
                            toast.copied = true;
                        }
                        cx.notify();
                    })),
            );
        }
        if toast.details.is_some() {
            actions = actions.child(
                Button::new(
                    "details-toast",
                    if toast.expanded { "Less" } else { "Details" },
                )
                .color(ButtonColor::Inherit)
                .on_click(cx.listener(|this, _, _, cx| {
                    let Some(toast) = this.toast.as_mut() else {
                        return;
                    };
                    toast.expanded = !toast.expanded;
                    // Folded again, it hides on its own as before.
                    let id = toast.id;
                    this.schedule_toast_hide(id, cx);
                    cx.notify();
                })),
            );
        }
        actions = actions.child(
            IconButton::new("close-toast", "Close")
                .color(ButtonColor::Inherit)
                .tooltip("Close notification")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toast = None;
                    cx.notify();
                })),
        );
        let mut alert = Alert::new(toast.status, toast.message.clone())
            .filled()
            .max_width(560.)
            .action(actions);
        if toast.expanded
            && let Some(details) = toast.details.clone()
        {
            alert = alert.content(
                div().child(toast.message.clone()).child(
                    div()
                        .id("toast-details")
                        .mt(px(8.))
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .font_family(fonts::MONO)
                        .text_size(px(11.))
                        .line_height(px(16.5))
                        .font_weight(muxus_ui::gpui_kit::FontWeight(400.))
                        .child(details),
                ),
            );
        }
        let snackbar = div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(24.))
            .flex()
            .justify_center()
            .child(alert);
        // Above dialogs and menus, as MUI stacks its snackbar.
        Some(
            deferred(snackbar)
                .with_priority(TOAST_LAYER)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(status: Status, details: Option<&str>) -> Toast {
        Toast {
            id: 1,
            status,
            message: "Could not save the host.".into(),
            details: details.map(SharedString::from),
            expanded: false,
            copied: false,
        }
    }

    #[test]
    fn errors_and_warnings_copy_the_message_and_its_details() {
        assert!(toast(Status::Error, None).copyable());
        assert!(toast(Status::Warning, None).copyable());
        assert!(!toast(Status::Info, None).copyable());
        assert!(!toast(Status::Success, Some("x")).copyable());
        assert_eq!(
            toast(Status::Error, Some("HTTP 500\n{}")).copy_text(),
            "Could not save the host.\n\nHTTP 500\n{}"
        );
        assert_eq!(
            toast(Status::Error, None).copy_text(),
            "Could not save the host."
        );
    }

    #[test]
    fn errors_and_open_details_wait_to_be_dismissed() {
        assert!(toast(Status::Error, None).sticky());
        let mut warning = toast(Status::Warning, Some("stack"));
        assert!(!warning.sticky());
        warning.expanded = true;
        assert!(warning.sticky());
    }
}
