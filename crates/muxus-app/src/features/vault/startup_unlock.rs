//! `components/PasswordVaultStartupUnlock.tsx`: resolve the startup vault
//! policy before persisted sessions are restored. While a startup-policy
//! vault is locked the window holds its workspace restore and shows this
//! dialog; once it is unlocked (or the user chooses "Not now") the dialog
//! reports [`VaultStartupEvent::Ready`] and the restore goes ahead, so the
//! restored sessions and this prompt never both ask for the master password.

use crate::features::settings::vault_startup::should_delay_workspace_restore_for_vault;
use muxus_core::api::PasswordVaultStatus;
use muxus_session::Backend;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, Styled, Subscription, Window, div, px,
};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::dialog::{
    DialogWidth, dialog_actions, dialog_content, dialog_paper, dialog_title,
};
use muxus_ui::widgets::display::{AlertVariant, alert_ink};
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::{Alert, Button};

pub const STARTUP_UNLOCK_TEXT: &str =
    "Enter the master password once to use saved SSH passwords until Muxus exits.";

/// What the dialog tells its window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VaultStartupEvent {
    /// Restore the workspace now (sent once).
    Ready,
    /// The vault was unlocked: its status changed.
    Unlocked(Box<PasswordVaultStatus>),
}

pub struct VaultStartupUnlock {
    backend: Backend,
    status: Option<PasswordVaultStatus>,
    /// A status handed in by [`Self::set_status`] wins over the backend's.
    pinned: bool,
    pending: bool,
    failed: bool,
    dismissed: bool,
    ready_sent: bool,
    password: Entity<InputState>,
    error: Option<String>,
    busy: bool,
    focus: FocusHandle,
    _events: Subscription,
}

impl EventEmitter<VaultStartupEvent> for VaultStartupUnlock {}

impl Focusable for VaultStartupUnlock {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl VaultStartupUnlock {
    pub fn new(backend: Backend, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let events = cx.subscribe_in(
            &password,
            window,
            |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.unlock(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        );
        // The field has no `autoFocus`: the dialog itself takes the keyboard.
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let status_backend = backend.clone();
        cx.spawn(async move |this, cx| {
            let result = status_backend.vault_status().await;
            let _ = this.update(cx, |this, cx| {
                if this.pinned {
                    return;
                }
                this.pending = false;
                match result {
                    Ok(status) => this.status = Some(status),
                    Err(_) => this.failed = true,
                }
                this.check_ready(cx);
                cx.notify();
            });
        })
        .detach();
        Self {
            backend,
            status: None,
            pinned: false,
            pending: true,
            failed: false,
            dismissed: false,
            ready_sent: false,
            password,
            error: None,
            busy: false,
            focus,
            _events: events,
        }
    }

    /// Whether the restore may go ahead.
    pub fn ready(&self) -> bool {
        self.dismissed
            || !should_delay_workspace_restore_for_vault(
                self.status.as_ref(),
                self.pending,
                self.failed,
            )
    }

    fn check_ready(&mut self, cx: &mut Context<Self>) {
        if self.ready() && !self.ready_sent {
            self.ready_sent = true;
            cx.emit(VaultStartupEvent::Ready);
        }
    }

    /// Show a status without asking the backend (the documentation scene).
    pub fn set_status(&mut self, status: PasswordVaultStatus, cx: &mut Context<Self>) {
        self.pinned = true;
        self.pending = false;
        self.status = Some(status);
        self.check_ready(cx);
        cx.notify();
    }

    fn unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let password = self.password.read(cx).value().to_string();
        if self.busy || password.is_empty() {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let backend = self.backend.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = backend.unlock_vault(&password).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(status) => {
                        this.password
                            .update(cx, |state, cx| state.set_value("", window, cx));
                        this.status = Some(status.clone());
                        cx.emit(VaultStartupEvent::Unlocked(Box::new(status)));
                        this.check_ready(cx);
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.password
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.error = None;
        self.dismissed = true;
        self.check_ready(cx);
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.dismiss(window, cx);
        }
    }
}

impl Render for VaultStartupUnlock {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.ready() || self.status.is_none() {
            return div().into_any_element();
        }
        let theme = *cx.theme();
        let p = theme.palette;
        let typography = theme.typography;
        let entity = cx.entity().downgrade();
        let mut stack = div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(2.))
            .mt(Theme::spacing(0.5))
            .child(
                paragraph(STARTUP_UNLOCK_TEXT)
                    .variant(typography.body2)
                    .color(p.text.secondary),
            );
        if let Some(error) = &self.error {
            stack = stack.child(
                Alert::new(Status::Error, "").content(
                    paragraph(error.clone())
                        .variant(typography.body2)
                        .color(alert_ink(&theme, Status::Error, AlertVariant::Standard)),
                ),
            );
        }
        stack = stack.child(
            FormField::input("vault-startup-password", &self.password).label("Master password"),
        );
        let empty = self.password.read(cx).value().is_empty();
        let (later, unlock) = (entity.clone(), entity.clone());
        let width = DialogWidth::Xs.px().min(
            muxus_ui::zoom::unzoom(window.viewport_size().width) - 2. * Theme::spacing_css(4.),
        );
        let paper = dialog_paper(&theme)
            .w(px(width))
            .child(
                dialog_title(&theme).child(
                    label("Unlock password vault")
                        .variant(typography.h6)
                        .color(p.text.primary),
                ),
            )
            .child(dialog_content(&theme, true, false).child(stack))
            .child(
                dialog_actions()
                    .child(
                        Button::new("vault-startup-later", "Not now")
                            .disabled(self.busy)
                            .on_click(move |_, window, cx| {
                                let _ = later.update(cx, |this, cx| this.dismiss(window, cx));
                            }),
                    )
                    .child(
                        Button::new(
                            "vault-startup-unlock",
                            if self.busy { "Unlocking…" } else { "Unlock" },
                        )
                        .contained()
                        .disabled(self.busy || empty)
                        .on_click(move |_, window, cx| {
                            let _ = unlock.update(cx, |this, cx| this.unlock(window, cx));
                        }),
                    ),
            );
        let backdrop = entity;
        div()
            .id("vault-startup-unlock")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(modal_layer(
                window,
                "vault-startup-backdrop",
                paper,
                Some(std::rc::Rc::new(
                    move |window: &mut Window, cx: &mut App| {
                        let _ = backdrop.update(cx, |this, cx| this.dismiss(window, cx));
                    },
                )),
            ))
            .into_any_element()
    }
}

/// Mount the startup check for a window.
pub fn vault_startup_unlock(
    backend: Backend,
    window: &mut Window,
    cx: &mut App,
) -> Entity<VaultStartupUnlock> {
    cx.new(|cx| VaultStartupUnlock::new(backend, window, cx))
}
