//! `components/ToastHost.tsx`: the single snackbar for notifications, a
//! filled `Alert` centred 24px above the window's bottom edge. Errors wait to
//! be dismissed; everything else hides after five seconds.

use std::time::Duration;

use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, deferred, div,
    px,
};
use muxus_ui::theme::Status;
use muxus_ui::widgets::layer::TOAST_LAYER;
use muxus_ui::widgets::{Alert, ButtonColor, IconButton};

use crate::ui::shell::AppShell;

#[derive(Clone, Debug)]
pub struct Toast {
    /// Distinguishes consecutive toasts so the auto-hide timer restarts.
    pub id: u64,
    pub status: Status,
    pub message: SharedString,
}

impl AppShell {
    /// `showToast`.
    pub fn show_toast(
        &mut self,
        status: Status,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.toast_seq += 1;
        let id = self.toast_seq;
        self.toast = Some(Toast {
            id,
            status,
            message: message.into(),
        });
        if status != Status::Error {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let _ = this.update(cx, |this, cx| {
                    if this.toast.as_ref().is_some_and(|toast| toast.id == id) {
                        this.toast = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub fn render_toast(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let toast = self.toast.clone()?;
        let snackbar = div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(24.))
            .flex()
            .justify_center()
            .child(
                Alert::new(toast.status, toast.message)
                    .filled()
                    .max_width(560.)
                    .action(
                        IconButton::new("close-toast", "Close")
                            .color(ButtonColor::Inherit)
                            .tooltip("Close notification")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toast = None;
                                cx.notify();
                            })),
                    ),
            );
        // Above dialogs and menus, as MUI stacks its snackbar.
        Some(
            deferred(snackbar)
                .with_priority(TOAST_LAYER)
                .into_any_element(),
        )
    }
}
