//! `components/PasswordVaultSection.tsx` with its master-password and
//! saved-password dialogs, and `components/VaultSecretDialog.tsx`.

use std::rc::Rc;

use muxus_core::api::{
    PasswordVaultCredential, PasswordVaultSecret, PasswordVaultStatus, PasswordVaultUnlockPolicy,
    VaultSecretInput,
};
use muxus_session::ApiError;
use muxus_session::login_sequence::editor::{LoginSequenceOwner, login_sequences_using_secret};
use muxus_store::prefs::command_buttons::command_button_label;
use muxus_ui::gpui_kit::base::input::InputState;
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement, SharedString, Styled, Window,
    div, px,
};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::{Alert, Button, ButtonColor, IconButton};

use super::super::texts;
use super::super::vault_secrets::{command_buttons_using_secret, quoted_list};
use super::fields::field;
use super::kit::{
    BODY2, CAPTION, DialogParts, alert_text, dialog, dialog_title, group, outlined_alert, page,
    row, secondary_text, skeleton, snap, status_text,
};
use super::state::SectionDialog;
use super::window::{Load, SettingsWindow};

const DESCRIPTION: &str = "An optional vault for SSH passwords and other secrets, protected by a master password. Saved values stay on this machine and are never part of a backup.";

/// What the master-password dialog does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MasterMode {
    Create,
    Change,
    Repair,
    Unlock,
    Policy,
}

impl MasterMode {
    fn title(self) -> &'static str {
        match self {
            MasterMode::Create => "Create password vault",
            MasterMode::Repair => "Restore OS credential-store access",
            MasterMode::Unlock => "Unlock password vault",
            MasterMode::Policy => "Master-password prompt policy",
            MasterMode::Change => "Change master password",
        }
    }

    fn button(self) -> &'static str {
        match self {
            MasterMode::Create => "Create vault",
            MasterMode::Repair => "Restore",
            MasterMode::Unlock => "Unlock",
            MasterMode::Policy => "Save policy",
            MasterMode::Change => "Change password",
        }
    }

    fn saved_message(self) -> &'static str {
        match self {
            MasterMode::Create => "Password vault created.",
            MasterMode::Repair => "OS credential-store access restored.",
            MasterMode::Unlock => "Password vault unlocked.",
            MasterMode::Policy => "Master-password prompt policy changed.",
            MasterMode::Change => "Master password changed.",
        }
    }
}

/// `MasterPasswordDialog`.
pub struct MasterDialog {
    pub mode: MasterMode,
    pub unlock_policy: PasswordVaultUnlockPolicy,
    pub error: Option<String>,
    pub saving: bool,
}

/// `EditSavedPasswordDialog`.
pub struct EditPasswordDialog {
    pub credential: PasswordVaultCredential,
    pub revealed: bool,
    pub visible: bool,
    pub error: Option<String>,
    pub saving: bool,
}

/// `VaultSecretDialog`: add a secret, or view and edit one.
pub struct SecretDialog {
    pub secret: Option<PasswordVaultSecret>,
    pub revealed: bool,
    pub visible: bool,
    pub saved_value: Option<String>,
    pub error: Option<String>,
    pub busy: bool,
}

/// The checks the master-password dialog runs before it asks the backend.
pub fn master_password_problem(
    mode: MasterMode,
    proposed: &str,
    confirmation: &str,
) -> Option<&'static str> {
    if !matches!(mode, MasterMode::Create | MasterMode::Change) {
        return None;
    }
    if proposed.chars().count() < 8 {
        return Some("Use at least 8 characters for the master password.");
    }
    if proposed.len() > 1024 {
        return Some("The master password is too long.");
    }
    if proposed != confirmation {
        return Some("The master-password confirmation does not match.");
    }
    None
}

/// The checks `VaultSecretDialog` runs before saving.
pub fn secret_problem(name: &str, value: &str, master: &str) -> Option<&'static str> {
    if muxus_core::js::js_trim(name).is_empty() {
        return Some("Give the secret a name.");
    }
    if value.is_empty() {
        return Some("Enter the secret to save.");
    }
    if value.len() > 8192 {
        return Some("The secret is too long to save.");
    }
    if master.is_empty() {
        return Some("Enter the master password.");
    }
    None
}

/// `new Date(updatedAt).toLocaleString()` in the en-US style.
fn local_time(value: &str) -> String {
    muxus_core::js::parse_timestamp(value)
        .map(|at| {
            at.with_timezone(&chrono::Local)
                .format("%-m/%-d/%Y, %-I:%M:%S %p")
                .to_string()
        })
        .unwrap_or_else(|| value.to_string())
}

impl SettingsWindow {
    pub fn passwords(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let status = match &self.data.vault {
            Load::Failed(error) => {
                return page(
                    &theme,
                    "Passwords",
                    Some(secondary_text(&theme, DESCRIPTION)),
                    vec![
                        outlined_alert(
                            &theme,
                            Status::Error,
                            alert_text(&theme, Status::Error, error.clone()),
                        )
                        .into_any_element(),
                    ],
                )
                .into_any_element();
            }
            Load::Pending => {
                return page(
                    &theme,
                    "Passwords",
                    Some(secondary_text(&theme, DESCRIPTION)),
                    vec![
                        skeleton(&theme, 72.).into_any_element(),
                        skeleton(&theme, 120.).into_any_element(),
                    ],
                )
                .into_any_element();
            }
            Load::Ready(status) => status.clone(),
        };
        let prefs = self.prefs(cx).clone();
        let (tone, text) = texts::vault_status(&status);
        let action: Option<AnyElement> = if !status.configured {
            Some(
                Button::new("vault-create", "Create password vault")
                    .contained()
                    .start_icon("PasswordOutlined")
                    .on_click(
                        cx.listener(|this, _, w, cx| this.open_master(MasterMode::Create, w, cx)),
                    )
                    .into_any_element(),
            )
        } else if status.locked && status.unlock_policy == Some(PasswordVaultUnlockPolicy::Never) {
            Some(
                Button::new("vault-repair", "Restore OS access")
                    .contained()
                    .on_click(
                        cx.listener(|this, _, w, cx| this.open_master(MasterMode::Repair, w, cx)),
                    )
                    .into_any_element(),
            )
        } else if status.locked && status.unlock_policy == Some(PasswordVaultUnlockPolicy::Startup)
        {
            Some(
                Button::new("vault-unlock", "Unlock now")
                    .contained()
                    .on_click(
                        cx.listener(|this, _, w, cx| this.open_master(MasterMode::Unlock, w, cx)),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };
        let mut vault_row =
            row(&theme, "Password vault").description(status_text(&theme, tone, text));
        if let Some(action) = action {
            vault_row = vault_row.control(action);
        }
        let mut vault_group = group("Vault")
            .row(vault_row.render())
            .row(self.switch_row(
                &theme,
                "settings-remember-passwords",
                "Remember passwords by default",
                Some(if status.configured {
                    "Password prompts start with \u{201c}Remember this password\u{201d} selected. Clear it for a login that should not be saved."
                } else {
                    "Password prompts start with \u{201c}Remember this password\u{201d} selected. The first saved password creates the vault."
                }),
                prefs.remember_passwords_by_default,
                |p, value| p.remember_passwords_by_default = value,
            ));
        if status.configured {
            let policy = status
                .unlock_policy
                .unwrap_or(PasswordVaultUnlockPolicy::Startup);
            vault_group = vault_group
                .row(
                    row(&theme, "Ask for the master password")
                        .description_text(&theme, texts::unlock_policy_label(policy))
                        .control(
                            Button::new("vault-policy", "Change prompt policy")
                                .outlined()
                                .on_click(cx.listener(|this, _, w, cx| this.open_master(MasterMode::Policy, w, cx))),
                        )
                        .render(),
                )
                .row(
                    row(&theme, "Master password")
                        .description_text(&theme, "Always required to view or edit saved values. It cannot be recovered.")
                        .control(
                            Button::new("vault-change", "Change master password")
                                .outlined()
                                .on_click(cx.listener(|this, _, w, cx| this.open_master(MasterMode::Change, w, cx))),
                        )
                        .render(),
                )
                .row(
                    row(&theme, "Delete the vault")
                        .description_text(&theme, "Forgets every saved password and secret; hosts, keys and other settings stay. Needs no master password, so a forgotten one can still be removed.")
                        .control(
                            Button::new("vault-delete", "Delete vault")
                                .outlined()
                                .color(ButtonColor::Error)
                                .disabled(self.state.vault_busy)
                                .on_click(cx.listener(|this, _, _, cx| this.remove_vault(cx))),
                        )
                        .render(),
                );
        }
        let mut groups = vec![vault_group.render(&theme)];
        if status.configured {
            groups.push(self.secrets_group(&theme, &status, &prefs, cx));
            groups.push(self.credentials_group(&theme, &status, cx));
        }
        groups.push(
            div()
                .mt(px(-12.))
                .child(super::kit::caption(
                    &theme,
                    "\u{201c}Never\u{201d} stores the vault key in the operating-system credential store; the other policies keep no usable vault key on disk. Two-factor codes and private-key passphrases are never remembered.",
                    1.66,
                ))
                .into_any_element(),
        );
        page(
            &theme,
            "Passwords",
            Some(secondary_text(&theme, DESCRIPTION)),
            groups,
        )
        .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn list_entry(
        &self,
        theme: &Theme,
        id: SharedString,
        name: String,
        meta: String,
        edit_tip: &'static str,
        delete_tip: &'static str,
        on_edit: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        on_delete: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        div()
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
                        label(name)
                            .size(BODY2)
                            .weight(550.)
                            .line_height(snap(BODY2 * 1.43))
                            .color(p.text.primary),
                    )
                    .child(
                        label(meta)
                            .size(CAPTION)
                            .line_height(snap(CAPTION * 1.66))
                            .color(p.text.secondary),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(2.))
                    .child(
                        IconButton::new(SharedString::from(format!("{id}-edit")), "EditOutlined")
                            .icon_size(18.)
                            .tooltip(edit_tip)
                            .on_click(
                                cx.listener(move |this, _, window, cx| on_edit(this, window, cx)),
                            ),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("{id}-delete")),
                            "DeleteOutlined",
                        )
                        .icon_size(18.)
                        .color(ButtonColor::Error)
                        .tooltip(delete_tip)
                        .on_click(cx.listener(move |this, _, _, cx| on_delete(this, cx))),
                    ),
            )
            .into_any_element()
    }

    fn secrets_group(
        &mut self,
        theme: &Theme,
        status: &PasswordVaultStatus,
        prefs: &muxus_store::prefs::PrefsState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut section = group(texts::counted_title("Secrets", status.secrets.len()))
            .description(secondary_text(
                theme,
                "Passwords and PINs that are not a login, such as enable or sudo passwords. A command button or Send secret\u{2026} types one into the focused session without showing it, and it stays out of session history and log files.",
            ))
            .action(
                Button::new("vault-add-secret", "Add secret")
                    .start_icon("Add")
                    .on_click(cx.listener(|this, _, window, cx| this.open_secret(None, window, cx))),
            )
            .flush();
        if status.secrets.is_empty() {
            section = section.row(empty_list(theme, "None yet."));
        } else {
            for (index, secret) in status.secrets.iter().enumerate() {
                let users = command_buttons_using_secret(&prefs.command_buttons, &secret.id).len();
                let mut meta: Vec<String> = Vec::new();
                if let Some(username) = &secret.username {
                    meta.push(format!("User {username}"));
                }
                if users > 0 {
                    meta.push(format!(
                        "Used by {users} command button{}",
                        if users == 1 { "" } else { "s" }
                    ));
                }
                meta.push(format!("Updated {}", local_time(&secret.updated_at)));
                let (edit, delete) = (secret.clone(), secret.clone());
                let entry = self.list_entry(
                    theme,
                    SharedString::from(format!("vault-secret-{}", secret.id)),
                    secret.name.clone(),
                    meta.join(" \u{b7} "),
                    "View or edit secret",
                    "Delete secret",
                    move |this, window, cx| this.open_secret(Some(edit.clone()), window, cx),
                    move |this, cx| this.forget_secret(delete.clone(), cx),
                    cx,
                );
                section = section.row(bordered(theme, index > 0, entry));
            }
        }
        section.render(theme)
    }

    fn credentials_group(
        &mut self,
        theme: &Theme,
        status: &PasswordVaultStatus,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut section = group(texts::counted_title(
            "Saved SSH passwords",
            status.credentials.len(),
        ))
        .flush();
        if status.credentials.is_empty() {
            section = section.row(empty_list(
                theme,
                "None yet. Select \u{201c}Remember this password\u{201d} the next time SSH asks for one.",
            ));
        } else {
            for (index, credential) in status.credentials.iter().enumerate() {
                let (edit, forget) = (credential.clone(), credential.clone());
                let entry = self.list_entry(
                    theme,
                    SharedString::from(format!("vault-credential-{}", credential.id)),
                    credential.label.clone(),
                    format!("Updated {}", local_time(&credential.updated_at)),
                    "View or edit password",
                    "Forget password",
                    move |this, window, cx| this.open_edit_password(edit.clone(), window, cx),
                    move |this, cx| this.forget_credential(forget.clone(), cx),
                    cx,
                );
                section = section.row(bordered(theme, index > 0, entry));
            }
        }
        section.render(theme)
    }

    fn vault_ready(&mut self, status: PasswordVaultStatus, cx: &mut Context<Self>) {
        self.data.vault = Load::Ready(status);
        cx.notify();
    }

    fn remove_vault(&mut self, cx: &mut Context<Self>) {
        let secret_ids: Vec<String> = self
            .data
            .vault
            .ready()
            .map(|status| {
                status
                    .secrets
                    .iter()
                    .map(|secret| secret.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let buttons = self
            .prefs(cx)
            .command_buttons
            .iter()
            .filter(|button| {
                button
                    .secret_id
                    .as_ref()
                    .is_some_and(|id| secret_ids.contains(id))
            })
            .count();
        let tail = match buttons {
            0 => String::new(),
            1 => " A saved command types one of these secrets and will show it as missing.".into(),
            n => format!(
                " {n} saved commands type one of these secrets and will show it as missing."
            ),
        };
        self.confirm(
            "Delete the password vault?",
            format!("Every saved SSH password and secret will be securely removed from the active database. Muxus also removes the OS credential-store copy when that store is available. No master password is required, so deletion remains possible if it is forgotten. Existing backups or filesystem snapshots are not affected. Connection profiles and SSH keys are not affected.{tail}"),
            "Delete vault",
            true,
            |this, _, cx| {
                this.state.vault_busy = true;
                let backend = this.backend.clone();
                this.fetch(cx, async move { backend.reset_vault().await }, |this, result, cx| {
                    this.state.vault_busy = false;
                    match result {
                        Ok(status) => {
                            this.vault_ready(status, cx);
                            this.show_toast(Status::Success, "Password vault deleted.", cx);
                        }
                        Err(error) => this.show_toast(Status::Error, error.to_string(), cx),
                    }
                });
            },
            cx,
        );
    }

    fn forget_credential(&mut self, credential: PasswordVaultCredential, cx: &mut Context<Self>) {
        self.confirm(
            "Forget saved password?",
            format!(
                "Muxus will ask for the password to {} next time.",
                credential.label
            ),
            "Forget password",
            true,
            move |this, _, cx| {
                let backend = this.backend.clone();
                let (id, label_) = (credential.id.clone(), credential.label.clone());
                this.fetch(
                    cx,
                    async move {
                        backend.delete_vault_credential(&id).await?;
                        backend.vault_status().await
                    },
                    move |this, result, cx| match result {
                        Ok(status) => {
                            this.vault_ready(status, cx);
                            this.show_toast(
                                Status::Success,
                                format!("Forgot the password for {label_}."),
                                cx,
                            );
                        }
                        Err(error) => this.show_toast(Status::Error, error.to_string(), cx),
                    },
                );
            },
            cx,
        );
    }

    fn forget_secret(&mut self, secret: PasswordVaultSecret, cx: &mut Context<Self>) {
        let buttons: Vec<String> =
            command_buttons_using_secret(&self.prefs(cx).command_buttons, &secret.id)
                .into_iter()
                .map(command_button_label)
                .collect();
        let backend = self.backend.clone();
        self.fetch(
            cx,
            async move { Ok(login_sequence_owners(&backend).await.unwrap_or_default()) },
            move |this, owners, cx| {
                let owners = owners.unwrap_or_default();
                let sequences = login_sequences_using_secret(&owners, &secret.id);
                let mut parts: Vec<String> = Vec::new();
                if !buttons.is_empty() {
                    let (who, theirs) = if buttons.len() == 1 {
                        ("A saved command still types".to_string(), "Its button")
                    } else {
                        (
                            format!("{} saved commands still type", buttons.len()),
                            "Their buttons",
                        )
                    };
                    parts.push(format!(
                        "{who} it: {}. {theirs} will show that the secret is missing.",
                        quoted_list(&buttons)
                    ));
                } else if sequences.is_empty() {
                    parts.push("The secret is removed from the password vault.".into());
                }
                if !sequences.is_empty() {
                    parts.push(format!(
                        "The login sequence of {} types it too and will stop at that step.",
                        quoted_list(&sequences)
                    ));
                }
                let secret = secret.clone();
                this.confirm(
                    format!("Delete \u{201c}{}\u{201d}?", secret.name),
                    parts.join(" "),
                    "Delete secret",
                    true,
                    move |this, _, cx| {
                        let backend = this.backend.clone();
                        let (id, name) = (secret.id.clone(), secret.name.clone());
                        this.fetch(
                            cx,
                            async move {
                                backend.delete_vault_credential(&id).await?;
                                backend.vault_status().await
                            },
                            move |this, result, cx| match result {
                                Ok(status) => {
                                    this.vault_ready(status, cx);
                                    this.show_toast(
                                        Status::Success,
                                        format!("Deleted \u{201c}{name}\u{201d}."),
                                        cx,
                                    );
                                }
                                Err(error) => this.show_toast(Status::Error, error.to_string(), cx),
                            },
                        );
                    },
                    cx,
                );
            },
        );
    }

    fn masked_input(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        self.input(key, String::new, None, true, window, cx, |_, _, _, _| {})
    }

    fn open_master(&mut self, mode: MasterMode, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_inputs("master-");
        let policy = self
            .data
            .vault
            .ready()
            .and_then(|status| status.unlock_policy)
            .unwrap_or(PasswordVaultUnlockPolicy::Startup);
        let first = self.masked_input("master-current", window, cx);
        let _ = self.masked_input("master-next", window, cx);
        let _ = self.masked_input("master-confirm", window, cx);
        // Enter submits from the field that ends the form.
        let submit = |this: &mut Self, _: String, window: &mut Window, cx: &mut Context<Self>| {
            this.submit_master(window, cx)
        };
        self.on_input_enter("master-confirm", submit);
        if matches!(mode, MasterMode::Repair | MasterMode::Unlock) {
            self.on_input_enter("master-current", submit);
        } else {
            self.on_input_enter("master-current", |_, _, _, _| {});
        }
        self.state.dialog = Some(SectionDialog::Master(MasterDialog {
            mode,
            unlock_policy: policy,
            error: None,
            saving: false,
        }));
        first.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn submit_master(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(SectionDialog::Master(dialog_)) = &self.state.dialog else {
            return;
        };
        let (mode, policy) = (dialog_.mode, dialog_.unlock_policy);
        let current = self.input_value("master-current", cx).unwrap_or_default();
        let next = self.input_value("master-next", cx).unwrap_or_default();
        let confirmation = self.input_value("master-confirm", cx).unwrap_or_default();
        let proposed = if mode == MasterMode::Create {
            &current
        } else {
            &next
        };
        if let Some(problem) = master_password_problem(mode, proposed, &confirmation) {
            self.master_error(Some(problem.to_string()), cx);
            return;
        }
        if let Some(SectionDialog::Master(dialog_)) = &mut self.state.dialog {
            dialog_.saving = true;
            dialog_.error = None;
        }
        let backend = self.backend.clone();
        self.fetch(
            cx,
            async move {
                match mode {
                    MasterMode::Create => backend.create_vault(&current, Some(policy)).await,
                    MasterMode::Repair => backend.repair_vault_automatic_access(&current).await,
                    MasterMode::Unlock => backend.unlock_vault(&current).await,
                    MasterMode::Policy => {
                        backend.change_vault_unlock_policy(&current, policy).await
                    }
                    MasterMode::Change => {
                        backend.change_vault_master_password(&current, &next).await
                    }
                }
            },
            move |this, result, cx| match result {
                Ok(status) => {
                    this.drop_inputs("master-");
                    this.state.dialog = None;
                    this.vault_ready(status, cx);
                    this.show_toast(Status::Success, mode.saved_message(), cx);
                }
                Err(error) => {
                    if let Some(SectionDialog::Master(dialog_)) = &mut this.state.dialog {
                        dialog_.saving = false;
                    }
                    this.master_error(Some(error.to_string()), cx);
                }
            },
        );
    }

    fn master_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        if let Some(SectionDialog::Master(dialog_)) = &mut self.state.dialog {
            dialog_.error = error;
        }
        cx.notify();
    }

    fn render_master(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some(SectionDialog::Master(dialog_)) = &self.state.dialog else {
            return None;
        };
        let (mode, policy, error, saving) = (
            dialog_.mode,
            dialog_.unlock_policy,
            dialog_.error.clone(),
            dialog_.saving,
        );
        let p = theme.palette;
        let paper = p.background_paper;
        let current = self.masked_input("master-current", window, cx);
        let next = self.masked_input("master-next", window, cx);
        let confirm = self.masked_input("master-confirm", window, cx);
        let creating = mode == MasterMode::Create;
        let changing = mode == MasterMode::Change;
        let mut body = div().flex().flex_col().gap(px(16.)).mt(px(4.));
        let intro = match mode {
            MasterMode::Create => Some(
                "Use at least 8 characters. This password is always required to view or edit saved values.",
            ),
            MasterMode::Repair => Some(
                "Enter the master password to restore the vault key in the operating-system credential store.",
            ),
            MasterMode::Unlock => {
                Some("The decrypted vault key remains in memory until Muxus exits.")
            }
            _ => None,
        };
        if let Some(intro) = intro {
            body = body.child(secondary_text(theme, intro));
        }
        if let Some(error) = error {
            body = body.child(Alert::new(Status::Error, error));
        }
        body = body.child(
            field("master-current-field", &current)
                .label(
                    if changing || mode == MasterMode::Policy {
                        "Current master password"
                    } else {
                        "Master password"
                    },
                    paper,
                )
                .render(theme, window, cx),
        );
        if creating || mode == MasterMode::Policy {
            let options: Vec<(String, String)> = texts::UNLOCK_POLICY_OPTIONS
                .iter()
                .map(|(policy, text)| (policy_id(*policy).to_string(), text.to_string()))
                .collect();
            let select = self.labeled_select(
                theme,
                "master-policy",
                "Ask for master password",
                options,
                policy_id(policy),
                cx,
                |this, value, _, cx| {
                    if let Some(SectionDialog::Master(dialog_)) = &mut this.state.dialog {
                        dialog_.unlock_policy = match value.as_str() {
                            "never" => PasswordVaultUnlockPolicy::Never,
                            "credential" => PasswordVaultUnlockPolicy::Credential,
                            _ => PasswordVaultUnlockPolicy::Startup,
                        };
                    }
                    cx.notify();
                },
            );
            body = body.child(select);
        }
        if changing {
            body = body.child(
                field("master-next-field", &next)
                    .label("New master password", paper)
                    .render(theme, window, cx),
            );
        }
        if creating || changing {
            body = body.child(
                field("master-confirm-field", &confirm)
                    .label("Confirm master password", paper)
                    .render(theme, window, cx),
            );
        }
        let close = self.close_dialog_handler(saving, cx);
        Some(dialog(
            theme,
            DialogParts {
                max_width: 444.,
                title: dialog_title(theme, mode.title()),
                content: body.into_any_element(),
                dividers: false,
                actions: vec![
                    Button::new("master-cancel", "Cancel")
                        .disabled(saving)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.drop_inputs("master-");
                            this.state.dialog = None;
                            cx.notify();
                        }))
                        .into_any_element(),
                    Button::new(
                        "master-submit",
                        if saving {
                            "Working\u{2026}"
                        } else {
                            mode.button()
                        },
                    )
                    .contained()
                    .disabled(saving)
                    .on_click(cx.listener(|this, _, window, cx| this.submit_master(window, cx)))
                    .into_any_element(),
                ],
                actions_padding: (8., 8.),
            },
            close,
        ))
    }

    fn close_dialog_handler(
        &self,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> Option<super::kit::Handler> {
        if busy {
            return None;
        }
        let weak = cx.entity().downgrade();
        Some(Rc::new(move |_, cx: &mut App| {
            let _ = weak.update(cx, |this, cx| {
                this.drop_inputs("master-");
                this.drop_inputs("edit-");
                this.drop_inputs("secret-");
                this.state.dialog = None;
                cx.notify();
            });
        }))
    }

    /// A select with a floating label (`TextField select`).
    #[allow(clippy::too_many_arguments)]
    pub fn labeled_select(
        &mut self,
        theme: &Theme,
        key: &'static str,
        title: &'static str,
        options: Vec<(String, String)>,
        value: &str,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let p = theme.palette;
        let select = self.row_select(theme, key, 0., options, value, false, cx, on_pick);
        div()
            .relative()
            .w_full()
            .child(div().w_full().child(select))
            .child(
                div()
                    .absolute()
                    .top(px(-9.))
                    .left(px(9.))
                    .px(px(5.))
                    .bg(p.background_paper)
                    .child(
                        label(title)
                            .size(super::kit::BODY1 * 0.75)
                            .line_height(snap(super::kit::BODY1 * 1.4375 * 0.75))
                            .color(p.text.secondary)
                            .no_truncate(),
                    ),
            )
            .into_any_element()
    }

    fn open_edit_password(
        &mut self,
        credential: PasswordVaultCredential,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.drop_inputs("edit-");
        let master = self.masked_input("edit-master", window, cx);
        let _ = self.masked_input("edit-password", window, cx);
        self.on_input_enter("edit-master", |this, _, window, cx| {
            this.submit_edit_password(window, cx)
        });
        self.on_input_enter("edit-password", |this, _, window, cx| {
            this.submit_edit_password(window, cx)
        });
        self.state.dialog = Some(SectionDialog::EditPassword(EditPasswordDialog {
            credential,
            revealed: false,
            visible: false,
            error: None,
            saving: false,
        }));
        master.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn submit_edit_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(SectionDialog::EditPassword(dialog_)) = &mut self.state.dialog else {
            return;
        };
        let (id, revealed) = (dialog_.credential.id.clone(), dialog_.revealed);
        let master = self.input_value("edit-master", cx).unwrap_or_default();
        let password = self.input_value("edit-password", cx).unwrap_or_default();
        let set_error = |this: &mut Self, error: Option<String>, saving: bool| {
            if let Some(SectionDialog::EditPassword(dialog_)) = &mut this.state.dialog {
                dialog_.error = error;
                dialog_.saving = saving;
            }
        };
        if !revealed {
            // Legacy vaults accepted eight-character passwords; the backend applies the minimum.
            if master.is_empty() {
                set_error(self, Some("Enter the master password.".into()), false);
                cx.notify();
                return;
            }
            set_error(self, None, true);
            let backend = self.backend.clone();
            self.fetch(
                cx,
                async move { backend.reveal_vault_credential(&id, &master).await },
                move |this, result, cx| {
                    match result {
                        Ok(value) => {
                            if let Some(SectionDialog::EditPassword(dialog_)) =
                                &mut this.state.dialog
                            {
                                dialog_.revealed = true;
                                dialog_.saving = false;
                            }
                            this.pending_reveal = Some(("edit-password".into(), value));
                        }
                        Err(error) => {
                            if let Some(SectionDialog::EditPassword(dialog_)) =
                                &mut this.state.dialog
                            {
                                dialog_.saving = false;
                                dialog_.error = Some(error.to_string());
                            }
                        }
                    }
                    cx.notify();
                },
            );
            let _ = window;
            return;
        }
        if password.len() > 8192 {
            set_error(
                self,
                Some("The SSH password is too long to save.".into()),
                false,
            );
            cx.notify();
            return;
        }
        set_error(self, None, true);
        let backend = self.backend.clone();
        let label_ = dialog_label(&self.state.dialog);
        self.fetch(
            cx,
            async move {
                backend
                    .update_vault_credential(&id, &master, &password)
                    .await
            },
            move |this, result, cx| match result {
                Ok(status) => {
                    this.drop_inputs("edit-");
                    this.state.dialog = None;
                    this.vault_ready(status, cx);
                    this.show_toast(
                        Status::Success,
                        format!("Updated the password for {label_}."),
                        cx,
                    );
                }
                Err(error) => {
                    if let Some(SectionDialog::EditPassword(dialog_)) = &mut this.state.dialog {
                        dialog_.saving = false;
                        dialog_.error = Some(error.to_string());
                    }
                    cx.notify();
                }
            },
        );
    }

    fn render_edit_password(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some(SectionDialog::EditPassword(dialog_)) = &self.state.dialog else {
            return None;
        };
        let (label_, revealed, visible, error, saving) = (
            dialog_.credential.label.clone(),
            dialog_.revealed,
            dialog_.visible,
            dialog_.error.clone(),
            dialog_.saving,
        );
        let paper = theme.palette.background_paper;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .mt(px(4.))
            .child(secondary_text(theme, label_));
        if let Some(error) = error {
            body = body.child(Alert::new(Status::Error, error));
        }
        if revealed {
            let state = self.masked_input("edit-password", window, cx);
            body = body.child(
                field("edit-password-field", &state)
                    .label("SSH password", paper)
                    .end(
                        self.visibility_button("edit-visible", visible, cx, |this, cx| {
                            if let Some(SectionDialog::EditPassword(dialog_)) =
                                &mut this.state.dialog
                            {
                                dialog_.visible = !dialog_.visible;
                            }
                            this.set_masked("edit-password", cx);
                        }),
                    )
                    .render(theme, window, cx),
            );
        } else {
            let state = self.masked_input("edit-master", window, cx);
            body = body.child(
                field("edit-master-field", &state)
                    .label("Master password", paper)
                    .render(theme, window, cx),
            );
        }
        let close = self.close_dialog_handler(saving, cx);
        Some(dialog(
            theme,
            DialogParts {
                max_width: 444.,
                title: dialog_title(theme, "View or edit saved password"),
                content: body.into_any_element(),
                dividers: false,
                actions: vec![
                    Button::new("edit-cancel", "Cancel")
                        .disabled(saving)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.drop_inputs("edit-");
                            this.state.dialog = None;
                            cx.notify();
                        }))
                        .into_any_element(),
                    Button::new(
                        "edit-submit",
                        if saving {
                            "Working\u{2026}"
                        } else if revealed {
                            "Save password"
                        } else {
                            "Continue"
                        },
                    )
                    .contained()
                    .disabled(saving)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.submit_edit_password(window, cx)),
                    )
                    .into_any_element(),
                ],
                actions_padding: (8., 8.),
            },
            close,
        ))
    }

    fn visibility_button(
        &self,
        id: &'static str,
        visible: bool,
        cx: &mut Context<Self>,
        toggle: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        IconButton::new(
            id,
            if visible {
                "VisibilityOffOutlined"
            } else {
                "VisibilityOutlined"
            },
        )
        .medium()
        .tooltip(if visible { "Hide" } else { "Show" })
        .on_click(cx.listener(move |this, _, _, cx| {
            toggle(this, cx);
            cx.notify();
        }))
        .into_any_element()
    }

    fn open_secret(
        &mut self,
        secret: Option<PasswordVaultSecret>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.drop_inputs("secret-");
        let name = secret.as_ref().map(|s| s.name.clone()).unwrap_or_default();
        let username = secret
            .as_ref()
            .and_then(|s| s.username.clone())
            .unwrap_or_default();
        let name_input = self.input(
            "secret-name",
            || name.clone(),
            None,
            false,
            window,
            cx,
            |_, _, _, _| {},
        );
        let _ = self.input(
            "secret-user",
            || username.clone(),
            None,
            false,
            window,
            cx,
            |_, _, _, _| {},
        );
        let _ = self.masked_input("secret-value", window, cx);
        let master = self.masked_input("secret-master", window, cx);
        self.on_input_enter("secret-master", |this, _, window, cx| {
            this.submit_secret(window, cx)
        });
        let editing = secret.is_some();
        self.state.dialog = Some(SectionDialog::Secret(SecretDialog {
            secret,
            revealed: !editing,
            visible: false,
            saved_value: None,
            error: None,
            busy: false,
        }));
        // Start typing in the first field: the name, or the master password before an edit.
        if editing {
            master.update(cx, |state, cx| state.focus(window, cx));
        } else {
            name_input.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    fn submit_secret(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(SectionDialog::Secret(dialog_)) = &self.state.dialog else {
            return;
        };
        let (secret, revealed, saved_value) = (
            dialog_.secret.clone(),
            dialog_.revealed,
            dialog_.saved_value.clone(),
        );
        let master = self.input_value("secret-master", cx).unwrap_or_default();
        let set = |this: &mut Self, error: Option<String>, busy: bool| {
            if let Some(SectionDialog::Secret(dialog_)) = &mut this.state.dialog {
                dialog_.error = error;
                dialog_.busy = busy;
            }
        };
        if !revealed {
            let Some(secret) = secret else {
                return;
            };
            if master.is_empty() {
                set(self, Some("Enter the master password.".into()), false);
                cx.notify();
                return;
            }
            set(self, None, true);
            let backend = self.backend.clone();
            let id = secret.id.clone();
            self.fetch(
                cx,
                async move { backend.reveal_vault_credential(&id, &master).await },
                |this, result, cx| {
                    match result {
                        Ok(value) => {
                            if let Some(SectionDialog::Secret(dialog_)) = &mut this.state.dialog {
                                dialog_.revealed = true;
                                dialog_.busy = false;
                                dialog_.saved_value = Some(value.clone());
                            }
                            this.pending_reveal = Some(("secret-value".into(), value));
                        }
                        Err(error) => {
                            if let Some(SectionDialog::Secret(dialog_)) = &mut this.state.dialog {
                                dialog_.busy = false;
                                dialog_.error = Some(error.to_string());
                            }
                        }
                    }
                    cx.notify();
                },
            );
            let _ = window;
            return;
        }
        let name = self.input_value("secret-name", cx).unwrap_or_default();
        let username = self.input_value("secret-user", cx).unwrap_or_default();
        let value = self.input_value("secret-value", cx).unwrap_or_default();
        if let Some(problem) = secret_problem(&name, &value, &master) {
            set(self, Some(problem.to_string()), false);
            cx.notify();
            return;
        }
        set(self, None, true);
        let trimmed_user = muxus_core::js::js_trim(&username).to_string();
        let input = VaultSecretInput {
            name: muxus_core::js::js_trim(&name).to_string(),
            username: (!trimmed_user.is_empty()).then_some(trimmed_user),
            value: (Some(&value) != saved_value.as_ref()).then_some(value),
            master_password: master,
        };
        let backend = self.backend.clone();
        let editing = secret.is_some();
        self.fetch(
            cx,
            async move {
                match secret {
                    Some(secret) => backend.update_vault_secret(&secret.id, input).await,
                    None => backend.create_vault_secret(input).await,
                }
            },
            move |this, result, cx| match result {
                Ok(saved) => {
                    this.drop_inputs("secret-");
                    this.state.dialog = None;
                    let name = saved.secret.name.clone();
                    this.vault_ready(saved.status, cx);
                    this.show_toast(
                        Status::Success,
                        if editing {
                            format!("Updated \u{201c}{name}\u{201d}.")
                        } else {
                            format!("Saved \u{201c}{name}\u{201d}.")
                        },
                        cx,
                    );
                }
                Err(error) => {
                    if let Some(SectionDialog::Secret(dialog_)) = &mut this.state.dialog {
                        dialog_.busy = false;
                        dialog_.error = Some(error.to_string());
                    }
                    cx.notify();
                }
            },
        );
    }

    fn render_secret(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some(SectionDialog::Secret(dialog_)) = &self.state.dialog else {
            return None;
        };
        let editing = dialog_.secret.is_some();
        let (revealed, visible, error, busy) = (
            dialog_.revealed,
            dialog_.visible,
            dialog_.error.clone(),
            dialog_.busy,
        );
        let intro = if editing && !revealed {
            format!(
                "Enter the master password to see or change \u{201c}{}\u{201d}.",
                dialog_
                    .secret
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default()
            )
        } else {
            "A password or PIN to type into sessions, such as an enable or sudo password. Command buttons and Send secret\u{2026} type it without showing it.".into()
        };
        let paper = theme.palette.background_paper;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .mt(px(4.))
            .child(secondary_text(theme, intro));
        if let Some(error) = error {
            body = body.child(Alert::new(Status::Error, error));
        }
        if revealed {
            let name = self.input(
                "secret-name",
                String::new,
                None,
                false,
                window,
                cx,
                |_, _, _, _| {},
            );
            let user = self.input(
                "secret-user",
                String::new,
                None,
                false,
                window,
                cx,
                |_, _, _, _| {},
            );
            let value = self.masked_input("secret-value", window, cx);
            body = body
                .child(
                    field("secret-name-field", &name)
                        .label("Name", paper)
                        .render(theme, window, cx),
                )
                .child(
                    field("secret-user-field", &user)
                        .label("User name (optional)", paper)
                        .render(theme, window, cx),
                )
                .child(
                    field("secret-value-field", &value)
                        .label("Secret", paper)
                        .end(
                            self.visibility_button("secret-visible", visible, cx, |this, cx| {
                                if let Some(SectionDialog::Secret(dialog_)) = &mut this.state.dialog
                                {
                                    dialog_.visible = !dialog_.visible;
                                }
                                this.set_masked("secret-value", cx);
                            }),
                        )
                        .render(theme, window, cx),
                );
        }
        if !(revealed && editing) {
            let master = self.masked_input("secret-master", window, cx);
            body = body.child(
                field("secret-master-field", &master)
                    .label("Master password", paper)
                    .render(theme, window, cx),
            );
        }
        let close = self.close_dialog_handler(busy, cx);
        Some(dialog(
            theme,
            DialogParts {
                max_width: 444.,
                title: dialog_title(
                    theme,
                    if editing {
                        "View or edit secret"
                    } else {
                        "Add secret"
                    },
                ),
                content: body.into_any_element(),
                dividers: false,
                actions: vec![
                    Button::new("secret-cancel", "Cancel")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.drop_inputs("secret-");
                            this.state.dialog = None;
                            cx.notify();
                        }))
                        .into_any_element(),
                    Button::new(
                        "secret-submit",
                        if busy {
                            "Working\u{2026}"
                        } else if revealed {
                            "Save secret"
                        } else {
                            "Continue"
                        },
                    )
                    .contained()
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.submit_secret(window, cx)))
                    .into_any_element(),
                ],
                actions_padding: (8., 8.),
            },
            close,
        ))
    }

    /// Show or hide the typed value of a masked input (the eye button).
    fn set_masked(&mut self, key: &str, cx: &mut Context<Self>) {
        let visible = match &self.state.dialog {
            Some(SectionDialog::EditPassword(dialog_)) => dialog_.visible,
            Some(SectionDialog::Secret(dialog_)) => dialog_.visible,
            _ => false,
        };
        self.pending_mask = Some((key.to_string(), !visible));
        cx.notify();
    }

    /// The section dialogs, the restore review and the menus they open.
    pub fn render_dialogs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // Values revealed by the backend and mask changes land in their inputs
        // here, where the window is at hand.
        if let Some((key, value)) = self.pending_reveal.take() {
            self.set_input_value(&key, &value, window, cx);
        }
        if let Some((key, masked)) = self.pending_mask.take()
            && let Some(state) = self.input_entity(&key)
        {
            state.update(cx, |state, cx| state.set_masked(masked, window, cx));
        }
        let theme = *cx.theme();
        self.render_master(&theme, window, cx)
            .or_else(|| self.render_edit_password(&theme, window, cx))
            .or_else(|| self.render_secret(&theme, window, cx))
            .or_else(|| self.render_restore_review(&theme, cx))
    }
}

fn policy_id(policy: PasswordVaultUnlockPolicy) -> &'static str {
    match policy {
        PasswordVaultUnlockPolicy::Never => "never",
        PasswordVaultUnlockPolicy::Startup => "startup",
        PasswordVaultUnlockPolicy::Credential => "credential",
    }
}

fn dialog_label(dialog_: &Option<SectionDialog>) -> String {
    match dialog_ {
        Some(SectionDialog::EditPassword(dialog_)) => dialog_.credential.label.clone(),
        _ => String::new(),
    }
}

fn empty_list(theme: &Theme, text: &str) -> AnyElement {
    div()
        .px(px(16.))
        .py(px(20.))
        .child(
            paragraph(text.to_string())
                .size(BODY2)
                .line_height(snap(BODY2 * 1.43))
                .color(theme.palette.text.secondary)
                .center(),
        )
        .into_any_element()
}

fn bordered(theme: &Theme, border: bool, element: AnyElement) -> AnyElement {
    div()
        .when(border, |d| {
            d.border_t_1().border_color(theme.palette.divider)
        })
        .child(element)
        .into_any_element()
}

/// The hosts and folders with a login sequence (`fetchLoginSequenceOwners`).
async fn login_sequence_owners(
    backend: &muxus_session::Backend,
) -> Result<Vec<LoginSequenceOwner>, ApiError> {
    let config = backend.ssh_config().await?;
    let saved = backend.saved_hosts().await?;
    let folders = backend.folder_settings().await?;
    let mut owners: Vec<LoginSequenceOwner> = config
        .hosts
        .iter()
        .map(|host| LoginSequenceOwner {
            label: host
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.display_name.clone())
                .unwrap_or_else(|| host.alias.clone()),
            login_sequence: host
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.login_sequence.clone()),
        })
        .collect();
    owners.extend(saved.profiles.iter().map(|profile| LoginSequenceOwner {
        label: profile.name.clone(),
        login_sequence: profile.metadata.login_sequence.clone(),
    }));
    owners.extend(folders.folders.iter().map(|folder| LoginSequenceOwner {
        label: folder.path.clone(),
        login_sequence: folder.login_sequence.clone(),
    }));
    Ok(owners)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_password_checks_follow_the_dialog() {
        assert_eq!(
            master_password_problem(MasterMode::Create, "short", "short"),
            Some("Use at least 8 characters for the master password.")
        );
        assert_eq!(
            master_password_problem(MasterMode::Change, "long enough", "different"),
            Some("The master-password confirmation does not match.")
        );
        assert_eq!(master_password_problem(MasterMode::Unlock, "x", ""), None);
        assert_eq!(
            master_password_problem(MasterMode::Create, "long enough", "long enough"),
            None
        );
        // Characters count, not bytes.
        assert_eq!(
            master_password_problem(MasterMode::Create, "äöüäöüäö", "äöüäöüäö"),
            None
        );
    }

    #[test]
    fn secret_checks_follow_the_dialog() {
        assert_eq!(
            secret_problem(" ", "x", "m"),
            Some("Give the secret a name.")
        );
        assert_eq!(
            secret_problem("a", "", "m"),
            Some("Enter the secret to save.")
        );
        assert_eq!(
            secret_problem("a", "x", ""),
            Some("Enter the master password.")
        );
        assert_eq!(secret_problem("a", "x", "m"), None);
    }
}
