//! `PasswordManagerUnlockForm.tsx`: unlock a KeePass database or a Bitwarden vault. The
//! password goes to the backend, which keeps the manager unlocked in memory until it is
//! locked or Muxus quits; it is never stored. Also the small dialog Settings wraps it in.

use std::rc::Rc;

use muxus_core::api::PasswordManagerSource;
use muxus_session::Backend;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled, Subscription, Window,
    div,
};
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::Button;
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::layer::modal_layer;

use super::logic::{unlock_helper, unlock_password_label, unlock_password_optional};
use super::store;
use crate::features::host_editor::widgets::shell::{DIALOG_XS, dialog_frame};

/// The manager is unlocked; the backend's new description of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Unlocked(pub Box<PasswordManagerSource>);

pub struct PasswordManagerUnlockForm {
    backend: Backend,
    source: PasswordManagerSource,
    password: Entity<InputState>,
    error: Option<String>,
    busy: bool,
    _events: Vec<Subscription>,
}

impl EventEmitter<Unlocked> for PasswordManagerUnlockForm {}

impl PasswordManagerUnlockForm {
    pub fn new(
        backend: Backend,
        source: PasswordManagerSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let events =
            vec![cx.subscribe_in(
                &password,
                window,
                |this, _, event, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.submit(window, cx),
                    InputEvent::Change => {
                        // A new attempt starts without the last one's complaint.
                        this.error = None;
                        cx.notify();
                    }
                    _ => {}
                },
            )];
        Self {
            backend,
            source,
            password,
            error: None,
            busy: false,
            _events: events,
        }
    }

    pub fn source_id(&self) -> &str {
        &self.source.id
    }

    /// What the last attempt to unlock said, when it failed.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Type `password` and submit it.
    pub fn unlock_with(&mut self, password: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.password.update(cx, |state, cx| {
            state.set_value(password.to_owned(), window, cx)
        });
        self.submit(window, cx);
    }

    /// The password field, for the owner to start typing in.
    pub fn input(&self) -> Entity<InputState> {
        self.password.clone()
    }

    fn typed(&self, cx: &App) -> String {
        self.password.read(cx).value().to_string()
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let password = self.typed(cx);
        if self.busy || (password.is_empty() && !unlock_password_optional(&self.source)) {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let backend = self.backend.clone();
        let id = self.source.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = backend.unlock_password_manager(&id, &password).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(source) => {
                        this.password
                            .update(cx, |state, cx| state.set_value("", window, cx));
                        store::refresh(&this.backend, cx);
                        cx.emit(Unlocked(Box::new(source)));
                    }
                    Err(error) => {
                        this.error = Some(if error.message.is_empty() {
                            "Could not unlock.".to_owned()
                        } else {
                            error.message
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for PasswordManagerUnlockForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let optional = unlock_password_optional(&self.source);
        let empty = self.typed(cx).is_empty();
        let helper = self
            .error
            .clone()
            .unwrap_or_else(|| unlock_helper(&self.source));
        let entity = cx.entity().downgrade();
        div()
            .flex()
            .items_start()
            .gap(Theme::spacing(1.))
            .child(
                div().flex_1().min_w_0().child(
                    FormField::input("password-manager-unlock-password", &self.password)
                        .label(unlock_password_label(self.source.kind))
                        .error(self.error.is_some())
                        .helper(helper),
                ),
            )
            .child(
                div().mt(Theme::spacing(0.25)).flex_none().child(
                    Button::new("password-manager-unlock-submit", "Unlock")
                        .contained()
                        .disabled(self.busy || (empty && !optional))
                        .on_click(move |_, window, cx| {
                            let _ = entity.update(cx, |this, cx| this.submit(window, cx));
                        }),
                ),
            )
    }
}

/// What the unlock dialog tells its owner.
#[derive(Clone, Debug, PartialEq)]
pub enum UnlockDialogEvent {
    Close,
    Unlocked,
}

/// Settings → Passwords → Unlock: the form in a dialog of its own.
pub struct PasswordManagerUnlockDialog {
    name: String,
    form: Entity<PasswordManagerUnlockForm>,
    focus: FocusHandle,
    _events: Vec<Subscription>,
}

impl EventEmitter<UnlockDialogEvent> for PasswordManagerUnlockDialog {}

impl Focusable for PasswordManagerUnlockDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

pub fn open_unlock_dialog(
    backend: Backend,
    source: PasswordManagerSource,
    window: &mut Window,
    cx: &mut App,
) -> Entity<PasswordManagerUnlockDialog> {
    cx.new(|cx| {
        let name = source.name.clone();
        let form = cx.new(|cx| PasswordManagerUnlockForm::new(backend, source, window, cx));
        let input = form.read(cx).input();
        input.update(cx, |state, cx| state.focus(window, cx));
        let events = vec![cx.subscribe(&form, |_, _, _: &Unlocked, cx| {
            cx.emit(UnlockDialogEvent::Unlocked);
        })];
        PasswordManagerUnlockDialog {
            name,
            form,
            focus: cx.focus_handle(),
            _events: events,
        }
    })
}

impl PasswordManagerUnlockDialog {
    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            cx.emit(UnlockDialogEvent::Close);
        }
    }
}

impl Render for PasswordManagerUnlockDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let (cancel, close) = (cx.entity().downgrade(), cx.entity().downgrade());
        let content = div()
            .pt(Theme::spacing(1.))
            .child(self.form.clone())
            .into_any_element();
        let actions = vec![
            Button::new("password-manager-unlock-cancel", "Cancel")
                .on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(UnlockDialogEvent::Close));
                })
                .into_any_element(),
        ];
        div()
            .id("password-manager-unlock")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(modal_layer(
                window,
                "password-manager-unlock-backdrop",
                dialog_frame(
                    &theme,
                    window,
                    "password-manager-unlock-content",
                    DIALOG_XS,
                    SharedString::from(format!("Unlock “{}”", self.name)),
                    6.,
                    content,
                    actions,
                    true,
                ),
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = close.update(cx, |_, cx| cx.emit(UnlockDialogEvent::Close));
                })),
            ))
    }
}
