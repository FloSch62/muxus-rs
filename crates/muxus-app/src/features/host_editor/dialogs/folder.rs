//! The folder dialog (`sidebar/FolderDialog.tsx`): create, rename,
//! re-parent and style a sidebar folder, give it shared SSH credentials and
//! a login sequence, or pick the folder hosts move to.
//!
//! Folder looks and empty folders are preferences the app owns: a submit
//! hands them back as [`FolderPrefsChange`]s in [`FolderDialogEvent::Submitted`];
//! host moves and folder settings are written through the backend here.

use std::rc::Rc;

use muxus_core::api::{FolderSettingsRecord, PasswordVaultStatus, SshKeysResponse};
use muxus_session::Backend;
use muxus_ui::color::try_hex;
use muxus_ui::gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::{ActiveTheme, Status};
use muxus_ui::widgets::dialog::modal;
use muxus_ui::widgets::{IconButton, Tooltip};

use crate::features::host_editor::logic::folder::{
    FolderDialogState, FolderDraft, FolderPrefsChange, VaultState, affected_hosts,
    folder_settings_for_path, inherited_sequence, merging, name_helper_text, parent_helper_text,
    password_helper_text, password_needs_master, plan_submit, port_invalid, rename_note, renaming,
    source_path, submit_blocked, submit_label, target_path, title,
};
use crate::features::host_editor::services::{self, EditorData};
use crate::features::host_editor::view::form::{self, AutocompleteHandlers, Form, Suggestion};
use crate::features::host_editor::widgets::inputs::{InputCache, Menus};
use crate::features::host_editor::widgets::shell::{DIALOG_SM, DIALOG_XS, form_dialog_frame};
use crate::features::host_editor::widgets::visuals::host_color_picker;
use crate::hosts::managed::ManagedHost;
use crate::hosts::tree::{
    folder_parent_path, is_descendant_path, is_same_path, sanitize_folder_name,
};
use crate::sidebar::folder_icons::{FOLDER_ICONS, folder_icon_name};
use crate::sidebar::folder_prefs::FolderPrefs;
use muxus_ui::theme::Theme;
use muxus_ui::widgets::Button;
use muxus_ui::widgets::display::divider;
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::typography::VariantExt;
use muxus_ui::widgets::typography::weight;
use muxus_ui::widgets::typography::{body2, caption, inline_caption, para, text};

#[derive(Debug, Clone, PartialEq)]
pub enum FolderDialogEvent {
    Close,
    /// The submit's preference edits, for the owner of the folder prefs to
    /// apply (in order); the backend writes are done or under way.
    Submitted {
        prefs: Vec<FolderPrefsChange>,
        toast: Option<String>,
    },
    Toast {
        status: Status,
        message: String,
    },
}

pub struct FolderDialog {
    backend: Backend,
    state: FolderDialogState,
    prefs: FolderPrefs,
    draft: FolderDraft,
    data: EditorData,
    record: Option<FolderSettingsRecord>,
    vault: Option<PasswordVaultStatus>,
    keys: Option<SshKeysResponse>,
    inputs: InputCache,
    menus: Menus,
    busy: bool,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<FolderDialogEvent> for FolderDialog {}

impl Focusable for FolderDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Form for FolderDialog {
    fn inputs(&mut self) -> &mut InputCache {
        &mut self.inputs
    }

    fn menus(&mut self) -> &mut Menus {
        &mut self.menus
    }
}

/// Open the folder dialog. `prefs` is the app's current folder preferences
/// (styles and empty folders), read for the folder's look and the picker.
pub fn open_folder_dialog(
    window: &mut Window,
    cx: &mut App,
    backend: Backend,
    state: FolderDialogState,
    prefs: FolderPrefs,
) -> Entity<FolderDialog> {
    cx.new(|cx: &mut Context<FolderDialog>| {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let draft = FolderDraft::new(&state, &prefs, None);
        let moving = matches!(state, FolderDialogState::MoveHosts { .. });
        let mut dialog = FolderDialog {
            backend: backend.clone(),
            state,
            prefs,
            draft,
            data: EditorData::default(),
            record: None,
            vault: None,
            keys: None,
            inputs: InputCache::default(),
            menus: Menus::default(),
            busy: false,
            focus,
            _tasks: Vec::new(),
        };
        let task = cx.spawn(async move |this, cx| {
            let data = services::load_editor_data(&backend).await;
            let keys = if moving {
                None
            } else {
                backend.ssh_keys(None).await.ok()
            };
            let _ = this.update(cx, |this, cx| {
                let source = source_path(&this.state).to_owned();
                this.record = (!source.is_empty())
                    .then(|| folder_settings_for_path(&data.folders, &source).cloned())
                    .flatten();
                if matches!(this.state, FolderDialogState::Edit { .. }) {
                    // The settings arrive after the dialog opened: seed the credential fields now.
                    let record = this.record.clone();
                    this.draft.seed_credentials(record.as_ref());
                    this.inputs.clear();
                }
                this.vault = data.vault.clone();
                this.keys = keys;
                this.data = data;
                cx.notify();
            });
        });
        dialog._tasks.push(task);
        dialog
    })
}

impl FolderDialog {
    fn hosts(&self) -> Vec<ManagedHost> {
        let mut hosts: Vec<ManagedHost> = self
            .data
            .ssh_config
            .as_ref()
            .map(|c| c.hosts.iter().cloned().map(ManagedHost::ssh).collect())
            .unwrap_or_default();
        hosts.extend(
            self.data
                .saved_hosts
                .iter()
                .cloned()
                .map(ManagedHost::profile),
        );
        hosts
    }

    fn vault_state(&self) -> VaultState {
        VaultState {
            configured: self.vault.as_ref().is_some_and(|v| v.configured),
            locked: self.vault.as_ref().is_some_and(|v| v.locked),
        }
    }

    fn known_paths(&self) -> Vec<String> {
        let mut prefs_data = self.data.clone();
        prefs_data.prefs.sidebar_empty_folders = self.prefs.empty_folders.clone();
        prefs_data.known_folders()
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let hosts = self.hosts();
        let Some(submit) = plan_submit(
            &self.state,
            &self.draft,
            &hosts,
            self.record.as_ref(),
            self.vault_state(),
        ) else {
            return;
        };
        self.busy = true;
        cx.emit(FolderDialogEvent::Submitted {
            prefs: submit.prefs.clone(),
            toast: submit.toast.clone(),
        });
        let backend = self.backend.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = services::apply_folder_submit(&backend, &submit).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                if let Err(message) = result {
                    cx.emit(FolderDialogEvent::Toast {
                        status: Status::Error,
                        message: format!("Could not save the folder credentials: {message}"),
                    });
                }
                cx.emit(FolderDialogEvent::Close);
            });
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                if self.menus.open.is_some() {
                    self.menus.close();
                    cx.notify();
                } else {
                    cx.emit(FolderDialogEvent::Close);
                }
            }
            "enter" if self.menus.open.is_none() => {
                cx.stop_propagation();
                self.submit(cx);
            }
            _ => {}
        }
    }
}

impl Render for FolderDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let moving = matches!(self.state, FolderDialogState::MoveHosts { .. });
        let source = source_path(&self.state).to_owned();
        let target = target_path(&self.state, &self.draft);
        let mut fields = div().flex().flex_col().gap(Theme::spacing(2.25));

        if !moving {
            let name = form::text_input(
                self,
                "folder-name",
                window,
                cx,
                self.draft.name.clone(),
                |this, v, cx| {
                    this.draft.name = v;
                    cx.notify();
                    None
                },
            );
            fields = fields.child(
                FormField::input("folder-name-field", &name)
                    .label("Folder name")
                    .helper(name_helper_text(&self.draft.name)),
            );
        }
        let (error, helper) = parent_helper_text(&self.state, &self.draft);
        let known: Vec<String> = self
            .known_paths()
            .into_iter()
            .filter(|path| {
                source.is_empty()
                    || !(is_same_path(path, &source) || is_descendant_path(path, &source))
            })
            .collect();
        let parent = self.draft.parent.clone();
        fields = fields.child(form::folder_field(
            self,
            "folder-parent",
            if moving { "Folder" } else { "Inside folder" },
            &parent,
            known,
            Some(helper),
            None,
            error,
            window,
            cx,
            |this, parent, cx| {
                this.draft.parent = parent;
                cx.notify();
            },
        ));

        if !moving {
            let entity = cx.entity().downgrade();
            fields = fields.child(host_color_picker(
                &theme,
                "folder-color",
                "Color",
                self.draft.color.as_deref(),
                false,
                30.,
                Rc::new(move |color, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.draft.color = color;
                        cx.notify();
                    });
                }),
            ));
            let accent = self.draft.color.as_deref().and_then(try_hex);
            let mut icons = div().flex().flex_wrap().gap(Theme::spacing(0.75));
            for entry in FOLDER_ICONS {
                let selected = self.draft.icon.as_deref().unwrap_or("folder") == entry.id;
                let entity = cx.entity().downgrade();
                icons = icons.child(
                    div()
                        .id(SharedString::from(format!("folder-icon-{}", entry.id)))
                        .size(px(30.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(theme.radius))
                        .border_1()
                        .border_color(if selected { p.primary.main } else { p.divider })
                        .cursor_pointer()
                        .tooltip(Tooltip::text(entry.label))
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                this.draft.icon =
                                    (entry.id != "folder").then(|| entry.id.to_owned());
                                cx.notify();
                            });
                        })
                        .child(icon(
                            entry.icon,
                            17.,
                            if selected {
                                accent.unwrap_or(p.primary.main)
                            } else {
                                p.text.secondary
                            },
                        )),
                );
            }
            fields = fields.child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().mb(Theme::spacing(1.)).child(text(
                        "Icon",
                        body2().weight(weight::SEMIBOLD),
                        p.text.primary,
                    )))
                    .child(icons),
            );
            let parent_preview = folder_parent_path(&target);
            let name = sanitize_folder_name(&self.draft.name);
            let mut preview_text = div().flex().flex_col().min_w_0().child(text(
                if name.is_empty() {
                    "Folder".to_owned()
                } else {
                    name
                },
                body2().weight(weight::SEMIBOLD),
                p.text.primary,
            ));
            if !parent_preview.is_empty() {
                preview_text = preview_text.child(text(
                    format!(
                        "in {}",
                        parent_preview.split('/').collect::<Vec<_>>().join(" / ")
                    ),
                    caption(),
                    p.text.secondary,
                ));
            }
            fields = fields.child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::spacing(1.25))
                    .p(Theme::spacing(1.25))
                    .border_1()
                    .border_color(p.divider)
                    .rounded(px(theme.radius * 1.5))
                    .bg(p.sidebar)
                    .child(icon(
                        folder_icon_name(self.draft.icon.as_deref(), true),
                        18.,
                        accent.unwrap_or(p.text.secondary),
                    ))
                    .child(preview_text),
            );

            fields = fields
                .child(div().mt(Theme::spacing(0.5)).child(divider(&theme)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(para("Shared SSH credentials", body2().weight(weight::SEMIBOLD), p.text.primary))
                        .child(inline_caption(
                            "Hosts in this folder use these unless they set their own. Anything in your ssh config still wins, and the nearest folder beats its parents.",
                            p.text.secondary,
                        )),
                );
            let user = form::text_input(
                self,
                "folder-user",
                window,
                cx,
                self.draft.auth_user.clone(),
                |this, v, cx| {
                    this.draft.auth_user = v;
                    cx.notify();
                    None
                },
            );
            let port = form::text_input(
                self,
                "folder-port",
                window,
                cx,
                self.draft.auth_port.clone(),
                |this, v, cx| {
                    this.draft.auth_port = v;
                    cx.notify();
                    None
                },
            );
            fields = fields.child(
                div()
                    .flex()
                    .gap(Theme::spacing(1.5))
                    .child(
                        FormField::input("folder-user-field", &user)
                            .label("Username")
                            .placeholder("from ssh config"),
                    )
                    .child(
                        FormField::input("folder-port-field", &port)
                            .label("Port")
                            .placeholder("22")
                            .error(port_invalid(&self.draft))
                            .width(130.),
                    ),
            );
            let keys: Vec<Suggestion> = self
                .keys
                .as_ref()
                .map(|k| {
                    k.keys
                        .iter()
                        .map(|key| {
                            Suggestion::new(key.path.clone(), key.name.clone())
                                .detail(key.path.clone())
                        })
                        .collect()
                })
                .unwrap_or_default();
            let key_helper = (!self.draft.auth_key.trim().is_empty())
                .then_some("Hosts without their own key log in with exactly this key.");
            let key_field = form::autocomplete(
                self,
                "folder-key",
                self.draft.auth_key.clone(),
                keys,
                move |input| {
                    FormField::input("folder-key-field", &input)
                        .label("Private key")
                        .placeholder("pick from ~/.ssh or type a path")
                        .helper_opt(key_helper)
                },
                AutocompleteHandlers {
                    on_input: Rc::new(|this: &mut FolderDialog, v, cx| {
                        this.draft.auth_key = v;
                        cx.notify();
                        None
                    }),
                    on_pick: Rc::new(|this: &mut FolderDialog, path, window, cx| {
                        this.inputs.set("folder-key", &path, window, cx);
                        this.draft.auth_key = path;
                        cx.notify();
                    }),
                    on_enter: None,
                },
                window,
                cx,
            );
            let browse = cx.entity().downgrade();
            fields = fields.child(
                div()
                    .flex()
                    .items_start()
                    .gap(Theme::spacing(1.))
                    .child(div().flex_1().min_w_0().child(key_field))
                    .child(
                        div().mt(Theme::spacing(0.75)).child(
                            IconButton::new("folder-key-browse", "FolderOpenOutlined")
                                .medium()
                                .icon_size(muxus_ui::icon::SMALL)
                                .tooltip("Browse for a key file")
                                .on_click(move |_, _, cx| {
                                    let chosen = crate::util::file_dialog::pick_paths(
                                        PathPromptOptions {
                                            files: true,
                                            directories: false,
                                            multiple: false,
                                            prompt: None,
                                        },
                                        cx,
                                    );
                                    let browse = browse.clone();
                                    cx.spawn(async move |cx| {
                                        if let Some(paths) = chosen.await
                                            && let Some(path) = paths.first()
                                        {
                                            let path = path.to_string_lossy().to_string();
                                            let _ = browse.update_in(cx, |this, window, cx| {
                                                this.inputs.set("folder-key", &path, window, cx);
                                                this.draft.auth_key = path;
                                                cx.notify();
                                            });
                                        }
                                    })
                                    .detach();
                                }),
                        ),
                    ),
            );
            let vault = self.vault_state();
            let has_password = self.record.as_ref().is_some_and(|r| r.has_password);
            let password = form::password_input(
                self,
                "folder-password",
                window,
                cx,
                String::new(),
                |this, v, cx| {
                    if !v.is_empty() {
                        this.draft.remove_password = false;
                    }
                    this.draft.auth_password = v;
                    cx.notify();
                    None
                },
            );
            let mut password_field = FormField::input("folder-password-field", &password)
                .label("Shared password")
                .disabled(!vault.configured)
                .helper(password_helper_text(&self.draft, vault, has_password));
            if has_password && !self.draft.remove_password {
                password_field = password_field.placeholder("•••••••• (saved)");
            }
            if has_password && self.draft.auth_password.is_empty() {
                let remove = self.draft.remove_password;
                let entity = cx.entity().downgrade();
                let tip = if remove {
                    "Keep the saved password"
                } else {
                    "Remove the saved password"
                };
                password_field = password_field.end(
                    IconButton::new(
                        "folder-password-remove",
                        if remove {
                            "UndoOutlined"
                        } else {
                            "DeleteOutlined"
                        },
                    )
                    .tooltip(tip)
                    .on_click(move |_, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.draft.remove_password = !this.draft.remove_password;
                            cx.notify();
                        });
                    }),
                );
            }
            fields = fields.child(password_field);
            if password_needs_master(&self.draft, vault) {
                let master = form::password_input(
                    self,
                    "folder-master",
                    window,
                    cx,
                    String::new(),
                    |this, v, cx| {
                        this.draft.master_password = v;
                        cx.notify();
                        None
                    },
                );
                fields = fields.child(
                    FormField::input("folder-master-field", &master)
                        .label("Vault master password")
                        .helper("The password vault is locked — its master password is needed to store this."),
                );
            }
            fields = fields
                .child(div().mt(Theme::spacing(0.5)).child(divider(&theme)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(para("Login sequence", body2().weight(weight::SEMIBOLD), p.text.primary))
                        .child(inline_caption(
                            "Runs after every SSH, Telnet or serial session to a host in this folder connects, unless the host sets its own. The nearest folder beats its parents.",
                            p.text.secondary,
                        )),
                );
            let inherited = inherited_sequence(&self.data.folders, &target);
            let sequence = self.draft.login_sequence.clone();
            let vault_status = self.vault.clone();
            fields = fields.child(
                crate::features::host_editor::view::shared::login_sequence_editor(
                    self,
                    "folder-login",
                    &sequence,
                    inherited,
                    "Use the parent folder's sequence",
                    "No parent folder sets a login sequence, so none runs.",
                    vault_status.as_ref(),
                    window,
                    cx,
                    |this, draft, cx| {
                        this.draft.login_sequence = draft;
                        cx.notify();
                    },
                ),
            );
            if let Some(problem) = muxus_session::login_sequence::editor::login_sequence_problem(
                &self.draft.login_sequence,
            ) {
                fields = fields.child(text(problem, caption(), p.warning.main));
            }
        }
        if renaming(&self.state, &self.draft) {
            let affected = affected_hosts(&self.state, &self.draft, &self.hosts()).len();
            let merge = merging(&self.state, &self.draft, &self.known_paths());
            if let Some(note) = rename_note(&target, affected, merge) {
                fields = fields.child(text(note, caption(), p.text.secondary));
            }
        }
        let content = div()
            .flex()
            .flex_col()
            .child(div().mb(Theme::spacing(2.5)).child(para(
                "Folders are local to Muxus. They never change your ssh config.",
                body2(),
                p.text.secondary,
            )))
            .child(fields);
        let blocked = submit_blocked(&self.state, &self.draft, self.vault_state()) || self.busy;
        let entity = cx.entity().downgrade();
        let (cancel, save, close) = (entity.clone(), entity.clone(), entity);
        let actions = vec![
            Button::new("folder-cancel", "Cancel")
                .on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(FolderDialogEvent::Close));
                })
                .into_any_element(),
            Button::new("folder-submit", submit_label(&self.state))
                .contained()
                .disabled(blocked)
                .on_click(move |_, _, cx| {
                    let _ = save.update(cx, |this, cx| this.submit(cx));
                })
                .into_any_element(),
        ];
        div()
            .id("folder-dialog")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(modal(
                form_dialog_frame(
                    &theme,
                    window,
                    "folder-content",
                    if moving { DIALOG_XS } else { DIALOG_SM },
                    title(&self.state),
                    6.,
                    content.into_any_element(),
                    actions,
                ),
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = close.update(cx, |_, cx| cx.emit(FolderDialogEvent::Close));
                })),
            ))
    }
}
