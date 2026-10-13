//! `components/VaultSecretDialog.tsx`: add a named secret, or view and edit
//! one. Both take the master password, like every other way to see or change
//! a saved value; editing asks for it first and then shows the current value.
//!
//! This is the stand-alone dialog `VaultSecretField` opens (the command
//! button editor, the login sequence editor). The settings window draws the
//! same dialog inside its Passwords page; both share the checks in
//! `features::settings::view::passwords::secret_problem`.

use muxus_core::api::{PasswordVaultSecret, VaultSecretInput, VaultSecretSaveResult};
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
use muxus_ui::widgets::{Alert, Button, IconButton};

use crate::features::settings::view::passwords::secret_problem;

/// `NAME_MAX`, `USERNAME_MAX`: the inputs stop accepting characters there.
pub const NAME_MAX: usize = 80;
pub const USERNAME_MAX: usize = 256;

/// The dialog's description line.
pub fn secret_dialog_description(editing_name: Option<&str>, revealed: bool) -> String {
    match editing_name {
        Some(name) if !revealed => format!("Enter the master password to see or change “{name}”."),
        _ => "A password or PIN to type into sessions, such as an enable or sudo password. Command buttons and Send secret… type it without showing it.".into(),
    }
}

/// The submit button's label.
pub fn secret_submit_label(busy: bool, revealed: bool) -> &'static str {
    if busy {
        "Working…"
    } else if revealed {
        "Save secret"
    } else {
        "Continue"
    }
}

/// What a save sends: the trimmed name, the user name when one is given, the
/// value only when it changed.
pub fn secret_input(
    name: &str,
    username: &str,
    value: &str,
    saved_value: Option<&str>,
    master_password: &str,
) -> VaultSecretInput {
    let username = muxus_core::js::js_trim(username);
    VaultSecretInput {
        name: muxus_core::js::js_trim(name).to_owned(),
        username: (!username.is_empty()).then(|| username.to_owned()),
        value: (Some(value) != saved_value).then(|| value.to_owned()),
        master_password: master_password.to_owned(),
    }
}

/// What the dialog tells its owner.
#[derive(Clone, Debug, PartialEq)]
pub enum VaultSecretEvent {
    Close,
    Saved(Box<VaultSecretSaveResult>),
}

pub struct VaultSecretDialog {
    backend: Backend,
    secret: Option<PasswordVaultSecret>,
    revealed: bool,
    name: Entity<InputState>,
    username: Entity<InputState>,
    value: Entity<InputState>,
    master: Entity<InputState>,
    saved_value: Option<String>,
    visible: bool,
    error: Option<String>,
    busy: bool,
    focus: FocusHandle,
    _events: Vec<Subscription>,
}

impl EventEmitter<VaultSecretEvent> for VaultSecretDialog {}

impl Focusable for VaultSecretDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Keep at most `max` characters, as `maxLength` does.
fn clip(value: &str, max: usize) -> Option<String> {
    (value.chars().count() > max).then(|| value.chars().take(max).collect())
}

impl VaultSecretDialog {
    /// `secret` edits an existing secret; `None` adds one named `initial_name`.
    pub fn new(
        backend: Backend,
        secret: Option<PasswordVaultSecret>,
        initial_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editing = secret.is_some();
        let name_value = secret
            .as_ref()
            .map_or_else(|| initial_name.to_owned(), |secret| secret.name.clone());
        let username_value = secret
            .as_ref()
            .and_then(|secret| secret.username.clone())
            .unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).default_value(name_value));
        let username = cx.new(|cx| InputState::new(window, cx).default_value(username_value));
        let value = cx.new(|cx| InputState::new(window, cx).masked(true));
        let master = cx.new(|cx| InputState::new(window, cx).masked(true));
        let mut events = Vec::new();
        for (input, max) in [
            (&name, Some(NAME_MAX)),
            (&username, Some(USERNAME_MAX)),
            (&value, None),
        ] {
            events.push(
                cx.subscribe_in(input, window, move |_, input, event, window, cx| {
                    if let InputEvent::Change = event {
                        if let Some(max) = max
                            && let Some(clipped) = clip(&input.read(cx).value(), max)
                        {
                            input.update(cx, |state, cx| state.set_value(clipped, window, cx));
                        }
                        cx.notify();
                    }
                }),
            );
        }
        events.push(
            cx.subscribe_in(&master, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
        );
        let mut dialog = Self {
            backend,
            secret,
            revealed: !editing,
            name,
            username,
            value,
            master,
            saved_value: None,
            visible: false,
            error: None,
            busy: false,
            focus: cx.focus_handle(),
            _events: events,
        };
        dialog.focus_first(window, cx);
        dialog
    }

    /// Start typing in the first field: the name, or the master password before an edit.
    fn focus_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = if self.revealed {
            &self.name
        } else {
            &self.master
        };
        first.update(cx, |state, cx| state.focus(window, cx));
    }

    fn text(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().to_string()
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(VaultSecretEvent::Close);
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.revealed {
            self.save(window, cx);
        } else {
            self.reveal(window, cx);
        }
    }

    fn reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(secret) = self.secret.clone() else {
            return;
        };
        let master = Self::text(&self.master, cx);
        if master.is_empty() {
            self.error = Some("Enter the master password.".into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let backend = self.backend.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = backend.reveal_vault_credential(&secret.id, &master).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(value) => {
                        this.value
                            .update(cx, |state, cx| state.set_value(value.clone(), window, cx));
                        this.saved_value = Some(value);
                        this.revealed = true;
                        this.focus_first(window, cx);
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (name, username, value, master) = (
            Self::text(&self.name, cx),
            Self::text(&self.username, cx),
            Self::text(&self.value, cx),
            Self::text(&self.master, cx),
        );
        if let Some(problem) = secret_problem(&name, &value, &master) {
            self.error = Some(problem.into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let input = secret_input(
            &name,
            &username,
            &value,
            self.saved_value.as_deref(),
            &master,
        );
        let backend = self.backend.clone();
        let id = self.secret.as_ref().map(|secret| secret.id.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = match id {
                Some(id) => backend.update_vault_secret(&id, input).await,
                None => backend.create_vault_secret(input).await,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(result) => {
                        this.master
                            .update(cx, |state, cx| state.set_value("", window, cx));
                        this.value
                            .update(cx, |state, cx| state.set_value("", window, cx));
                        this.saved_value = None;
                        cx.emit(VaultSecretEvent::Saved(Box::new(result)));
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_visible(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = !self.visible;
        let masked = !self.visible;
        self.value
            .update(cx, |state, cx| state.set_masked(masked, window, cx));
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.close(cx);
        }
    }
}

impl Render for VaultSecretDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let typography = theme.typography;
        let editing = self.secret.is_some();
        let entity = cx.entity().downgrade();
        let mut stack = div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(2.))
            .mt(Theme::spacing(0.5))
            .child(
                paragraph(secret_dialog_description(
                    self.secret.as_ref().map(|secret| secret.name.as_str()),
                    self.revealed,
                ))
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
        if self.revealed {
            let toggle = entity.clone();
            stack = stack
                .child(FormField::input("secret-name", &self.name).label("Name"))
                .child(
                    FormField::input("secret-username", &self.username)
                        .label("User name (optional)"),
                )
                .child(
                    FormField::input("secret-value", &self.value)
                        .label("Secret")
                        .end(
                            IconButton::new(
                                "secret-visibility",
                                if self.visible {
                                    "VisibilityOffOutlined"
                                } else {
                                    "VisibilityOutlined"
                                },
                            )
                            .medium()
                            .edge_end()
                            .tooltip(if self.visible {
                                "Hide secret"
                            } else {
                                "Show secret"
                            })
                            .on_click(move |_, window, cx| {
                                let _ =
                                    toggle.update(cx, |this, cx| this.toggle_visible(window, cx));
                            }),
                        ),
                );
        }
        if !(self.revealed && editing) {
            stack = stack
                .child(FormField::input("secret-master", &self.master).label("Master password"));
        }
        let (cancel, submit) = (entity.clone(), entity.clone());
        let width = DialogWidth::Xs.px().min(
            muxus_ui::zoom::unzoom(window.viewport_size().width) - 2. * Theme::spacing_css(4.),
        );
        let paper = dialog_paper(&theme)
            .w(px(width))
            .child(
                dialog_title(&theme).child(
                    label(if editing {
                        "View or edit secret"
                    } else {
                        "Add secret"
                    })
                    .variant(typography.h6)
                    .color(p.text.primary),
                ),
            )
            .child(dialog_content(&theme, true, false).child(stack))
            .child(
                dialog_actions()
                    .child(
                        Button::new("secret-cancel", "Cancel")
                            .disabled(self.busy)
                            .on_click(move |_, _, cx| {
                                let _ = cancel.update(cx, |this, cx| this.close(cx));
                            }),
                    )
                    .child(
                        Button::new(
                            "secret-submit",
                            secret_submit_label(self.busy, self.revealed),
                        )
                        .contained()
                        .disabled(self.busy)
                        .on_click(move |_, window, cx| {
                            let _ = submit.update(cx, |this, cx| this.submit(window, cx));
                        }),
                    ),
            );
        let backdrop = entity;
        div()
            .id("vault-secret-dialog")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(modal_layer(
                window,
                "vault-secret-backdrop",
                paper,
                Some(std::rc::Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = backdrop.update(cx, |this, cx| this.close(cx));
                })),
            ))
    }
}

/// Open the dialog; the owner drops it on [`VaultSecretEvent::Close`] or
/// [`VaultSecretEvent::Saved`].
pub fn open_vault_secret_dialog(
    backend: Backend,
    secret: Option<PasswordVaultSecret>,
    initial_name: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<VaultSecretDialog> {
    cx.new(|cx| VaultSecretDialog::new(backend, secret, initial_name, window, cx))
}

/// What to do with a secret created through [`request_new_secret`].
pub type SecretCreated = Box<dyn FnOnce(VaultSecretSaveResult, &mut App)>;

type NewSecretHandler = std::rc::Rc<dyn Fn(SecretCreated, &mut App)>;

/// The window's way of showing this dialog for a field ("New secret…").
struct NewSecretHook(NewSecretHandler);

impl muxus_ui::gpui_kit::Global for NewSecretHook {}

/// Register how "New secret…" opens the dialog (the app mounts it in the active window).
pub fn set_new_secret_hook(hook: impl Fn(SecretCreated, &mut App) + 'static, cx: &mut App) {
    cx.set_global(NewSecretHook(std::rc::Rc::new(hook)));
}

/// `VaultSecretField`'s "New secret…": open the dialog; `on_created` gets the saved secret
/// and the vault's new status. False when no window can show it.
pub fn request_new_secret(on_created: SecretCreated, cx: &mut App) -> bool {
    let Some(hook) = cx.try_global::<NewSecretHook>().map(|hook| hook.0.clone()) else {
        return false;
    };
    hook(on_created, cx);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_each_step() {
        assert_eq!(
            secret_dialog_description(Some("Core enable"), false),
            "Enter the master password to see or change “Core enable”."
        );
        assert!(
            secret_dialog_description(Some("Core enable"), true).starts_with("A password or PIN")
        );
        assert!(secret_dialog_description(None, true).starts_with("A password or PIN"));
        assert_eq!(secret_submit_label(true, true), "Working…");
        assert_eq!(secret_submit_label(false, true), "Save secret");
        assert_eq!(secret_submit_label(false, false), "Continue");
    }

    #[test]
    fn sends_the_value_only_when_it_changed() {
        let unchanged = secret_input(" Core enable ", "  ", "s3cret", Some("s3cret"), "master-pw");
        assert_eq!(unchanged.name, "Core enable");
        assert_eq!(unchanged.username, None);
        assert_eq!(unchanged.value, None);
        let changed = secret_input("Lab sudo", " admin ", "new", Some("old"), "master-pw");
        assert_eq!(changed.username.as_deref(), Some("admin"));
        assert_eq!(changed.value.as_deref(), Some("new"));
        let added = secret_input("Lab sudo", "", "new", None, "master-pw");
        assert_eq!(added.value.as_deref(), Some("new"));
        assert_eq!(clip("abcdef", 4).as_deref(), Some("abcd"));
        assert_eq!(clip("abc", 4), None);
    }
}
