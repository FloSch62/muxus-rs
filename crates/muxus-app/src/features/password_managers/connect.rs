//! `PasswordManagerDialog.tsx`: connect a password manager, or change how a connected one
//! is reached.

use std::rc::Rc;

use muxus_core::api::{
    PASSWORD_MANAGER_KINDS, PasswordManagerConfig, PasswordManagerKind, PasswordManagerSource,
};
use muxus_session::Backend;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, PathPromptOptions, Render,
    SharedString, Styled, Window, div,
};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::form_field::{FieldBody, FormField};
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::typography::{body2, para};
use muxus_ui::widgets::{Alert, Button, IconButton};

use super::logic::{
    incomplete, kind_choice_label, kind_note, kind_program, manager_input, missing_program,
    program_helper,
};
use super::store;
use crate::features::host_editor::view::form::{self, Choice, Form};
use crate::features::host_editor::widgets::inputs::{InputCache, Menus};
use crate::features::host_editor::widgets::shell::{DIALOG_SM, form_dialog_frame};

/// What the dialog tells its owner.
#[derive(Clone, Debug, PartialEq)]
pub enum PasswordManagerDialogEvent {
    Close,
    Saved(Box<PasswordManagerSource>),
}

/// What a browse button picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Browse {
    Database,
    KeyFile,
    Store,
}

impl Browse {
    /// The input the chosen path goes into.
    fn field(self) -> &'static str {
        match self {
            Browse::Database | Browse::Store => PATH,
            Browse::KeyFile => KEY_FILE,
        }
    }

    fn prompt(self) -> &'static str {
        match self {
            Browse::Database => "Use database",
            Browse::KeyFile => "Use key file",
            Browse::Store => "Use folder",
        }
    }
}

const PATH: &str = "password-manager-path";
const KEY_FILE: &str = "password-manager-key-file";
const PROGRAM: &str = "password-manager-program";
const ACCOUNT: &str = "password-manager-account";
const NAME: &str = "password-manager-name";

pub struct PasswordManagerDialog {
    backend: Backend,
    /// The source being edited; `None` connects a new one.
    source: Option<PasswordManagerSource>,
    installed: Vec<PasswordManagerKind>,
    kind: PasswordManagerKind,
    name: String,
    config: PasswordManagerConfig,
    error: Option<String>,
    busy: bool,
    /// The database field still has to take the focus (a new KeePass connection starts in
    /// the one field it cannot do without).
    focus_path: bool,
    inputs: InputCache,
    menus: Menus,
    focus: FocusHandle,
}

impl EventEmitter<PasswordManagerDialogEvent> for PasswordManagerDialog {}

impl Focusable for PasswordManagerDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Form for PasswordManagerDialog {
    fn inputs(&mut self) -> &mut InputCache {
        &mut self.inputs
    }

    fn menus(&mut self) -> &mut Menus {
        &mut self.menus
    }
}

/// Open the dialog for `source`, or to connect a new manager. `installed` are the kinds
/// whose program was found on this computer.
pub fn open_password_manager_dialog(
    backend: Backend,
    source: Option<PasswordManagerSource>,
    installed: Vec<PasswordManagerKind>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<PasswordManagerDialog> {
    cx.new(|cx| {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        PasswordManagerDialog {
            backend,
            kind: source
                .as_ref()
                .map_or(PasswordManagerKind::Keepass, |source| source.kind),
            name: source
                .as_ref()
                .map(|source| source.name.clone())
                .unwrap_or_default(),
            config: source
                .as_ref()
                .map(|source| source.config.clone())
                .unwrap_or_default(),
            focus_path: source.is_none(),
            source,
            installed,
            error: None,
            busy: false,
            inputs: InputCache::default(),
            menus: Menus::default(),
            focus,
        }
    })
}

impl PasswordManagerDialog {
    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(PasswordManagerDialogEvent::Close);
        }
    }

    fn set_kind(&mut self, kind: PasswordManagerKind, cx: &mut Context<Self>) {
        if kind == self.kind {
            return;
        }
        self.kind = kind;
        // Another manager is reached another way: nothing of the last one carries over.
        self.config = PasswordManagerConfig::default();
        for key in [PATH, KEY_FILE, PROGRAM, ACCOUNT] {
            self.inputs.remove(key);
        }
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || incomplete(self.kind, &self.config) {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let backend = self.backend.clone();
        let id = self.source.as_ref().map(|source| source.id.clone());
        let input = manager_input(self.kind, &self.name, &self.config);
        cx.spawn_in(window, async move |this, cx| {
            let result = match id {
                Some(id) => backend.update_password_manager(&id, input).await,
                None => backend.create_password_manager(input).await,
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(saved) => {
                        store::refresh(&this.backend, cx);
                        cx.emit(PasswordManagerDialogEvent::Saved(Box::new(saved)));
                    }
                    Err(error) => {
                        this.error = Some(if error.message.is_empty() {
                            "Could not save the password manager.".to_owned()
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

    fn browse(&mut self, target: Browse, cx: &mut Context<Self>) {
        let chosen = crate::util::file_dialog::pick_paths(
            PathPromptOptions {
                files: target != Browse::Store,
                directories: target == Browse::Store,
                multiple: false,
                prompt: Some(target.prompt().into()),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let Some(path) = chosen.await.and_then(|paths| paths.into_iter().next()) else {
                return;
            };
            let path = path.to_string_lossy().into_owned();
            let _ = this.update(cx, |this, cx| {
                // The field shows what was picked the next time it is drawn.
                this.inputs.remove(target.field());
                match target {
                    Browse::Database | Browse::Store => this.config.path = Some(path),
                    Browse::KeyFile => this.config.key_file = Some(path),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" {
            return;
        }
        cx.stop_propagation();
        if self.menus.open.is_some() {
            self.menus.close();
            cx.notify();
        } else {
            self.close(cx);
        }
    }

    /// One text field of the dialog; Enter in it submits.
    #[allow(clippy::too_many_arguments)]
    fn setting(
        &mut self,
        key: &'static str,
        label: String,
        value: String,
        placeholder: Option<String>,
        helper: Option<String>,
        error: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: fn(&mut Self, String),
    ) -> FormField {
        let input = form::text_input_enter(
            self,
            key,
            window,
            cx,
            value,
            move |this, value, cx| {
                apply(this, value);
                cx.notify();
                None
            },
            |this, window, cx| this.submit(window, cx),
        );
        let mut field = FormField::input(SharedString::from(format!("{key}-field")), &input)
            .label(label)
            .error(error);
        if let Some(placeholder) = placeholder {
            field = field.placeholder(placeholder);
        }
        field.helper_opt(helper)
    }

    /// A field with a browse button beside it.
    fn with_browse(
        field: FormField,
        id: &'static str,
        tip: &'static str,
        target: Browse,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity().downgrade();
        div()
            .flex()
            .items_start()
            .gap(Theme::spacing(1.))
            .child(div().flex_1().min_w_0().child(field))
            .child(
                div().mt(Theme::spacing(0.75)).child(
                    IconButton::new(id, "FolderOpenOutlined")
                        .medium()
                        .icon_size(muxus_ui::icon::SMALL)
                        .tooltip(tip)
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |this, cx| this.browse(target, cx));
                        }),
                ),
            )
            .into_any_element()
    }
}

fn optional(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

impl Render for PasswordManagerDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let kind = self.kind;
        let editing = self.source.is_some();
        let mut stack = div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(2.25))
            .pt(Theme::spacing(1.));

        // A connected manager keeps its kind: its entries are referenced by it.
        stack = stack.child(if editing {
            FormField::new(
                "password-manager-kind-field",
                FieldBody::Value(kind.label().into()),
            )
            .label("Password manager")
            .shrink(true)
            .disabled(true)
            .into_any_element()
        } else {
            let choices = PASSWORD_MANAGER_KINDS
                .into_iter()
                .map(|option| Choice::new(option, kind_choice_label(option, &self.installed)))
                .collect();
            form::select(
                self,
                "password-manager-kind",
                |body| {
                    FormField::new("password-manager-kind-field", body)
                        .label("Password manager")
                        .shrink(true)
                },
                &kind,
                choices,
                Some(FieldBody::Value(kind.label().into())),
                window,
                cx,
                |this, picked, cx| this.set_kind(picked, cx),
            )
        });
        stack = stack.child(para(kind_note(kind), body2(), p.text.secondary));

        if kind == PasswordManagerKind::Keepass {
            let path = self.setting(
                PATH,
                "Database file".into(),
                self.config.path.clone().unwrap_or_default(),
                Some("~/Passwords.kdbx".into()),
                None,
                false,
                window,
                cx,
                |this, value| this.config.path = optional(value),
            );
            stack = stack.child(Self::with_browse(
                path,
                "password-manager-browse-database",
                "Browse for the database",
                Browse::Database,
                cx,
            ));
            if std::mem::take(&mut self.focus_path)
                && let Some(input) = self.inputs.existing(PATH)
            {
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            let key_file = self.setting(
                KEY_FILE,
                "Key file".into(),
                self.config.key_file.clone().unwrap_or_default(),
                None,
                Some("Only when the database is protected with one.".into()),
                false,
                window,
                cx,
                |this, value| this.config.key_file = optional(value),
            );
            stack = stack.child(Self::with_browse(
                key_file,
                "password-manager-browse-key-file",
                "Browse for the key file",
                Browse::KeyFile,
                cx,
            ));
        }

        if kind == PasswordManagerKind::Pass {
            let store = self.setting(
                PATH,
                "Password store folder".into(),
                self.config.path.clone().unwrap_or_default(),
                Some("~/.password-store".into()),
                Some(
                    "Leave empty for the default store. gopass keeps its own under ~/.local/share/gopass/stores/root."
                        .into(),
                ),
                false,
                window,
                cx,
                |this, value| this.config.path = optional(value),
            );
            stack = stack.child(Self::with_browse(
                store,
                "password-manager-browse-store",
                "Browse for the store folder",
                Browse::Store,
                cx,
            ));
        }

        if kind == PasswordManagerKind::Onepassword {
            stack = stack.child(self.setting(
                ACCOUNT,
                "Account".into(),
                self.config.account.clone().unwrap_or_default(),
                Some("my.1password.com".into()),
                Some(
                    "Sign-in address or account ID. Only needed when the app has several accounts."
                        .into(),
                ),
                false,
                window,
                cx,
                |this, value| this.config.account = optional(value),
            ));
        }

        if let Some(program) = kind_program(kind) {
            let typed = self.config.program.clone().unwrap_or_default();
            let missing = missing_program(kind, &self.installed, &typed);
            stack = stack.child(self.setting(
                PROGRAM,
                format!("Path to {program}"),
                typed,
                Some(program.into()),
                Some(program_helper(kind, missing)),
                missing,
                window,
                cx,
                |this, value| this.config.program = optional(value),
            ));
        }

        stack = stack.child(self.setting(
            NAME,
            "Name".into(),
            self.name.clone(),
            Some(kind.label().into()),
            Some("Shown wherever an entry from it is used.".into()),
            false,
            window,
            cx,
            |this, value| this.name = value,
        ));

        if let Some(error) = &self.error {
            stack = stack.child(Alert::new(Status::Error, error.clone()).outlined());
        }

        let (cancel, submit, close) = (
            cx.entity().downgrade(),
            cx.entity().downgrade(),
            cx.entity().downgrade(),
        );
        let actions = vec![
            Button::new("password-manager-cancel", "Cancel")
                .disabled(self.busy)
                .on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |this, cx| this.close(cx));
                })
                .into_any_element(),
            Button::new(
                "password-manager-submit",
                if editing { "Save" } else { "Connect" },
            )
            .contained()
            .disabled(self.busy || incomplete(kind, &self.config))
            .on_click(move |_, window, cx| {
                let _ = submit.update(cx, |this, cx| this.submit(window, cx));
            })
            .into_any_element(),
        ];
        let title = match &self.source {
            Some(source) => SharedString::from(format!("Edit “{}”", source.name)),
            None => "Connect a password manager".into(),
        };
        div()
            .id("password-manager-dialog")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(modal_layer(
                window,
                "password-manager-dialog-backdrop",
                form_dialog_frame(
                    &theme,
                    window,
                    "password-manager-dialog-content",
                    DIALOG_SM,
                    title,
                    6.,
                    stack.into_any_element(),
                    actions,
                ),
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = close.update(cx, |this, cx| this.close(cx));
                })),
            ))
    }
}
