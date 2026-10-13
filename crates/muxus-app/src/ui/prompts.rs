//! Questions a session asks while it connects: interactive authentication
//! (`components/AuthPromptDialog.tsx`), host key trust
//! (`components/HostKeyDialog.tsx`) and certificate trust for remote desktops
//! and gNMI (`components/DesktopCertificateDialog.tsx`). Questions queue per
//! window, oldest first, whichever tab (or tunnel) asked; the answer goes back
//! to whoever asked.

use muxus_app::session::LinkInput;
use muxus_core::protocol::{
    AuthPromptInfo, AuthPromptPurpose, AuthPromptResponse, DesktopCertificateChallenge,
    DesktopIdentity, HostKeyChallenge, TerminalClientMessage, TrustState,
};
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled, Window, div, px,
};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, fonts};
use muxus_ui::widgets::display::Alert;
use muxus_ui::widgets::{Button, ButtonColor, Checkbox, Dialog, DialogWidth, TextField};

use crate::ui::shell::AppShell;

/// One open question.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionPrompt {
    Auth(AuthPromptInfo),
    HostKey(HostKeyChallenge),
    /// A TLS certificate or VNC server key; `gnmi` words the explanation for a device.
    Certificate {
        challenge: DesktopCertificateChallenge,
        gnmi: bool,
    },
}

/// The answer to the question on screen.
#[derive(Clone, Debug, PartialEq)]
pub enum PromptAnswer {
    /// `None` closes the session, as dismissing the dialog does.
    Auth(Option<AuthPromptResponse>),
    HostKey(bool),
    Certificate(bool),
}

/// Where the answer to a question goes when no terminal asked it.
pub enum PromptReply {
    /// A tunnel's dial (`features::forwarding`).
    Tunnel(async_channel::Sender<muxus_app::features::forwarding::start::DialAnswer>),
}

/// The dialog of the oldest question.
pub struct PromptDialog {
    pub tab_id: String,
    prompt: SessionPrompt,
    answers: Vec<Entity<InputState>>,
    remember: bool,
    error: Option<String>,
    focus: FocusHandle,
    _subscriptions: Vec<muxus_ui::gpui_kit::Subscription>,
}

impl EventEmitter<PromptAnswer> for PromptDialog {}

impl Focusable for PromptDialog {
    fn focus_handle(&self, _: &muxus_ui::gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// `label={p.prompt.replace(/:\s*$/, '')}`.
fn field_label(prompt: &str) -> String {
    let trimmed = prompt.trim_end();
    trimmed.strip_suffix(':').unwrap_or(trimmed).to_string()
}

/// Checks before a new vault's master password is sent.
fn vault_create_problem(answers: &[String]) -> Option<&'static str> {
    let first = answers.first().map(String::as_str).unwrap_or("");
    if first.chars().count() < 8 {
        return Some("Use at least 8 characters for the master password.");
    }
    if first.len() > 1024 {
        return Some("The master password is too long.");
    }
    if Some(first) != answers.get(1).map(String::as_str) {
        return Some("The master-password confirmation does not match.");
    }
    None
}

impl PromptDialog {
    pub fn new(
        tab_id: String,
        prompt: SessionPrompt,
        remember_by_default: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut answers = Vec::new();
        let mut subscriptions = Vec::new();
        if let SessionPrompt::Auth(info) = &prompt {
            for field in &info.prompts {
                let masked = !field.echo;
                let input = cx.new(|cx| InputState::new(window, cx).masked(masked));
                subscriptions.push(cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit(cx);
                    }
                }));
                answers.push(input);
            }
        }
        let focus = cx.focus_handle();
        match answers.first() {
            // `autoFocus` on the first field.
            Some(first) => first.update(cx, |input, cx| input.focus(window, cx)),
            None => window.focus(&focus, cx),
        }
        Self {
            tab_id,
            prompt,
            answers,
            remember: remember_by_default,
            error: None,
            focus,
            _subscriptions: subscriptions,
        }
    }

    fn values(&self, cx: &muxus_ui::gpui_kit::App) -> Vec<String> {
        self.answers
            .iter()
            .map(|input| input.read(cx).value().to_string())
            .collect()
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        match &self.prompt {
            SessionPrompt::Auth(info) => {
                let answers = self.values(cx);
                if info.purpose == Some(AuthPromptPurpose::VaultCreate)
                    && let Some(problem) = vault_create_problem(&answers)
                {
                    self.error = Some(problem.into());
                    cx.notify();
                    return;
                }
                let remember = info.remember_password.as_ref().map(|_| self.remember);
                cx.emit(PromptAnswer::Auth(Some(AuthPromptResponse {
                    answers,
                    remember_password: remember,
                    skipped: None,
                })));
            }
            SessionPrompt::HostKey(_) => cx.emit(PromptAnswer::HostKey(true)),
            SessionPrompt::Certificate { .. } => cx.emit(PromptAnswer::Certificate(true)),
        }
    }

    /// Escape, the backdrop, Cancel (or the skip action when offered).
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        match &self.prompt {
            SessionPrompt::Auth(info) => {
                let skipped = info.skip_label.as_ref().map(|_| AuthPromptResponse {
                    answers: Vec::new(),
                    remember_password: None,
                    skipped: Some(true),
                });
                cx.emit(PromptAnswer::Auth(skipped));
            }
            SessionPrompt::HostKey(_) => cx.emit(PromptAnswer::HostKey(false)),
            SessionPrompt::Certificate { .. } => cx.emit(PromptAnswer::Certificate(false)),
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                self.dismiss(cx);
            }
            // The default button answers Enter when no field has the keyboard;
            // a changed host key has no default.
            "enter" if self.focus.is_focused(window) => {
                let mismatch = match &self.prompt {
                    SessionPrompt::HostKey(challenge) => challenge.state == TrustState::Mismatch,
                    SessionPrompt::Certificate { challenge, .. } => {
                        challenge.state == TrustState::Mismatch
                    }
                    SessionPrompt::Auth(_) => false,
                };
                if mismatch {
                    return;
                }
                cx.stop_propagation();
                self.submit(cx);
            }
            _ => {}
        }
    }

    fn render_auth(&self, info: &AuthPromptInfo, cx: &mut Context<Self>) -> Dialog {
        let theme = *cx.theme();
        let p = theme.palette;
        let body2 = theme.typography.body2;
        let caption = theme.typography.caption;
        let entity = cx.entity().downgrade();
        let mut dialog = Dialog::new()
            .title(SharedString::from(
                info.name
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "Authentication".into()),
            ))
            .max_width(DialogWidth::Xs)
            .full_width()
            .on_close({
                let entity = entity.clone();
                move |_, cx| {
                    let _ = entity.update(cx, |this, cx| this.dismiss(cx));
                }
            });
        if let Some(host) = info.host.clone().filter(|host| !host.is_empty()) {
            dialog = dialog.subtitle(host);
        }
        let mut stack = div().mt(px(4.)).flex().flex_col().gap(px(12.));
        if let Some(instructions) = info.instructions.clone().filter(|text| !text.is_empty()) {
            stack = stack.child(
                paragraph(instructions)
                    .size(body2.size)
                    .line_height(body2.line_height)
                    .color(p.text.secondary),
            );
        }
        if let Some(error) = &self.error {
            stack = stack.child(Alert::new(Status::Error, error.clone()));
        }
        for (index, (field, input)) in info.prompts.iter().zip(&self.answers).enumerate() {
            stack = stack.child(
                // The notched label sits above the field's top edge.
                div().pt(px(6.)).child(
                    TextField::new(SharedString::from(format!("auth-prompt-{index}")), input)
                        .label(field_label(&field.prompt))
                        .full_width(),
                ),
            );
        }
        if let Some(offer) = &info.remember_password {
            let checked = self.remember;
            let toggle = entity.clone();
            stack = stack.child(
                div()
                    .flex()
                    .items_center()
                    .ml(px(-11.))
                    .child(
                        Checkbox::new("auth-remember", checked).on_click(move |_, _, cx| {
                            let _ = toggle.update(cx, |this, cx| {
                                this.remember = !this.remember;
                                cx.notify();
                            });
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                label(if offer.existing {
                                    "Update the saved password"
                                } else {
                                    "Remember this password"
                                })
                                .size(body2.size)
                                .line_height(body2.line_height)
                                .color(p.text.primary),
                            )
                            .child(
                                label("Encrypted locally under your password-vault policy.")
                                    .size(caption.size)
                                    .line_height(caption.line_height)
                                    .color(p.text.secondary),
                            ),
                    ),
            );
        }
        dialog = dialog.content(stack);
        let cancel = entity.clone();
        dialog = match info.skip_label.clone() {
            Some(skip) => {
                dialog.action(Button::new("auth-skip", skip).on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |this, cx| this.dismiss(cx));
                }))
            }
            None => dialog.action(Button::new("auth-cancel", "Cancel").on_click(
                move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(PromptAnswer::Auth(None)));
                },
            )),
        };
        let submit = entity;
        dialog.action(
            Button::new(
                "auth-continue",
                if info.purpose == Some(AuthPromptPurpose::VaultCreate) {
                    "Create vault"
                } else {
                    "Continue"
                },
            )
            .contained()
            .on_click(move |_, _, cx| {
                let _ = submit.update(cx, |this, cx| this.submit(cx));
            }),
        )
    }

    fn render_host_key(&self, challenge: &HostKeyChallenge, cx: &mut Context<Self>) -> Dialog {
        let theme = *cx.theme();
        let p = theme.palette;
        let body2 = theme.typography.body2;
        let mismatch = challenge.state == TrustState::Mismatch;
        let entity = cx.entity().downgrade();
        let mono = |text: String| {
            div()
                .font_family(fonts::MONO)
                .text_size(px(12.))
                .line_height(px(12. * 1.5))
                .text_color(p.text.primary)
                .child(text)
        };
        let mut stack = div().mt(px(4.)).flex().flex_col().gap(px(12.));
        if let Some(hop) = &challenge.hop {
            stack = stack.child(Alert::new(
                Status::Info,
                format!("This is the jump host “{hop}” on the way to your target."),
            ));
        }
        let target = format!("{}:{}", challenge.host, challenge.port);
        stack = if mismatch {
            stack.child(Alert::new(
                Status::Error,
                format!(
                    "The identity of {target} has CHANGED. This can mean the server was reinstalled — or that the connection is being intercepted. Only continue if you can explain the change."
                ),
            ))
        } else {
            stack.child(
                paragraph(format!(
                    "First connection to {target}. Verify the fingerprint before trusting it."
                ))
                .size(body2.size)
                .line_height(body2.line_height)
                .color(p.text.primary),
            )
        };
        stack = stack
            .child(
                paragraph(format!("{} key fingerprint:", challenge.key_type))
                    .size(body2.size)
                    .line_height(body2.line_height)
                    .color(p.text.secondary),
            )
            .child(mono(challenge.fingerprint.clone()));
        if let Some(previous) = &challenge.previous {
            stack = stack
                .child(
                    paragraph("Previously recorded fingerprint:")
                        .size(body2.size)
                        .line_height(body2.line_height)
                        .color(p.text.secondary),
                )
                .child(mono(previous.clone()));
        }
        let (close, cancel, accept) = (entity.clone(), entity.clone(), entity);
        Dialog::new()
            .title(if mismatch {
                "Host key changed!"
            } else {
                "Unknown host"
            })
            .max_width(DialogWidth::Sm)
            .full_width()
            .on_close(move |_, cx| {
                let _ = close.update(cx, |_, cx| cx.emit(PromptAnswer::HostKey(false)));
            })
            .content(stack)
            .action(
                Button::new("host-key-cancel", "Cancel").on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(PromptAnswer::HostKey(false)));
                }),
            )
            .action(
                Button::new(
                    "host-key-accept",
                    if mismatch {
                        "Accept new key"
                    } else {
                        "Trust host"
                    },
                )
                .contained()
                .color(if mismatch {
                    ButtonColor::Error
                } else {
                    ButtonColor::Primary
                })
                .on_click(move |_, _, cx| {
                    let _ = accept.update(cx, |_, cx| cx.emit(PromptAnswer::HostKey(true)));
                }),
            )
    }
}

impl PromptDialog {
    fn render_certificate(
        &self,
        challenge: &DesktopCertificateChallenge,
        gnmi: bool,
        cx: &mut Context<Self>,
    ) -> Dialog {
        let theme = *cx.theme();
        let p = theme.palette;
        let body2 = theme.typography.body2;
        let mismatch = challenge.state == TrustState::Mismatch;
        let key = matches!(challenge.identity, DesktopIdentity::RsaKey { .. });
        let (noun, title_noun) = if key {
            ("server key", "Server key")
        } else {
            ("certificate", "Certificate")
        };
        let entity = cx.entity().downgrade();
        let field = |name: &str, value: String, monospace: bool| {
            let value = if value.is_empty() {
                "—".into()
            } else {
                value
            };
            let text = div()
                .text_size(if monospace {
                    px(muxus_ui::widgets::typography::code_size::SMALL)
                } else {
                    px(body2.size)
                })
                .line_height(px(body2.line_height))
                .text_color(p.text.primary)
                .child(value);
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    label(name.to_string())
                        .size(body2.size)
                        .line_height(body2.line_height)
                        .color(p.text.secondary),
                )
                .child(if monospace {
                    text.font_family(fonts::MONO)
                } else {
                    text
                })
        };
        let target = format!("{}:{}", challenge.host, challenge.port);
        let mut stack = div().mt(px(4.)).flex().flex_col().gap(px(12.));
        stack = if mismatch {
            stack.child(Alert::new(
                Status::Error,
                format!(
                    "The {noun} of {target} has CHANGED since you last trusted it. This can mean the server was reinstalled or its {noun} renewed — or that the connection is being intercepted. Only continue if you can explain the change."
                ),
            ))
        } else if key {
            stack.child(
                paragraph(format!(
                    "{target} identified itself with an RSA key Muxus has not seen before. Before sending it your password, compare the signature with the server's (TigerVNC's viewer calls it the fingerprint)."
                ))
                .size(body2.size)
                .line_height(body2.line_height)
                .color(p.text.primary),
            )
        } else {
            let error = match &challenge.identity {
                DesktopIdentity::Certificate {
                    verification_error: Some(error),
                    ..
                } => format!(" ({error})"),
                _ => String::new(),
            };
            let why = if gnmi {
                "Network devices often use self-signed certificates or one from a private lab CA"
            } else {
                "Remote Desktop servers usually use self-signed certificates"
            };
            stack.child(
                paragraph(format!(
                    "{target} presented a certificate Muxus cannot verify{error}. {why}; compare the fingerprint with the server before trusting it."
                ))
                .size(body2.size)
                .line_height(body2.line_height)
                .color(p.text.primary),
            )
        };
        stack = match &challenge.identity {
            DesktopIdentity::RsaKey { bits, signature } => stack
                .child(field("Key", format!("RSA, {bits} bits"), false))
                .child(field("Signature", signature.clone(), true)),
            DesktopIdentity::Certificate {
                subject,
                issuer,
                valid_from,
                valid_to,
                ..
            } => stack
                .child(field("Issued to", subject.clone(), false))
                .child(field("Issued by", issuer.clone(), false))
                .child(field("Valid", format!("{valid_from} – {valid_to}"), false)),
        };
        stack = stack.child(field(
            "SHA-256 fingerprint",
            challenge.fingerprint.clone(),
            true,
        ));
        if let Some(previous) = &challenge.previous {
            stack = stack.child(field(
                "Previously trusted fingerprint",
                previous.clone(),
                true,
            ));
        }
        let (close, cancel, accept) = (entity.clone(), entity.clone(), entity);
        Dialog::new()
            .title(if mismatch {
                format!("{title_noun} changed!")
            } else if key {
                "Unknown server key".to_string()
            } else {
                "Unverified certificate".to_string()
            })
            .max_width(DialogWidth::Sm)
            .full_width()
            .on_close(move |_, cx| {
                let _ = close.update(cx, |_, cx| cx.emit(PromptAnswer::Certificate(false)));
            })
            .content(stack)
            .action(
                Button::new("certificate-cancel", "Cancel").on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(PromptAnswer::Certificate(false)));
                }),
            )
            .action(
                Button::new(
                    "certificate-accept",
                    if mismatch {
                        format!("Trust new {noun}")
                    } else {
                        format!("Trust {noun}")
                    },
                )
                .contained()
                .color(if mismatch {
                    ButtonColor::Error
                } else {
                    ButtonColor::Primary
                })
                .on_click(move |_, _, cx| {
                    let _ = accept.update(cx, |_, cx| cx.emit(PromptAnswer::Certificate(true)));
                }),
            )
    }
}

impl Render for PromptDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog = match self.prompt.clone() {
            SessionPrompt::Auth(info) => self.render_auth(&info, cx),
            SessionPrompt::HostKey(challenge) => self.render_host_key(&challenge, cx),
            SessionPrompt::Certificate { challenge, gnmi } => {
                self.render_certificate(&challenge, gnmi, cx)
            }
        };
        div()
            .id("session-prompt")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key_down))
            .child(dialog)
    }
}

impl AppShell {
    pub fn open_auth_prompt(
        &mut self,
        tab_id: &str,
        prompt: AuthPromptInfo,
        cx: &mut Context<Self>,
    ) {
        self.prompts
            .push((tab_id.to_string(), SessionPrompt::Auth(prompt)));
        cx.notify();
    }

    pub fn open_host_key_prompt(
        &mut self,
        tab_id: &str,
        challenge: HostKeyChallenge,
        cx: &mut Context<Self>,
    ) {
        self.prompts
            .push((tab_id.to_string(), SessionPrompt::HostKey(challenge)));
        cx.notify();
    }

    /// The session ended: its open questions go with it.
    pub fn close_prompts_of(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let before = self.prompts.len();
        self.prompts.retain(|(id, _)| id != tab_id);
        self.prompt_replies.remove(tab_id);
        if self
            .prompt_dialog
            .as_ref()
            .is_some_and(|(dialog, _)| dialog.read(cx).tab_id == tab_id)
        {
            self.prompt_dialog = None;
        }
        if self.prompts.len() != before {
            cx.notify();
        }
    }

    /// Show the oldest question when none is on screen.
    pub fn sync_prompt_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompt_dialog.is_some() || self.confirm.is_some() || self.paste_dialog.is_some() {
            return;
        }
        let Some((tab_id, prompt)) = self.prompts.first().cloned() else {
            return;
        };
        let remember = self.prefs.remember_passwords_by_default;
        self.menu = None;
        let dialog = cx.new(|cx| PromptDialog::new(tab_id, prompt, remember, window, cx));
        let subscription = cx.subscribe_in(&dialog, window, Self::on_prompt_answer);
        self.prompt_dialog = Some((dialog, subscription));
    }

    fn on_prompt_answer(
        &mut self,
        dialog: &Entity<PromptDialog>,
        answer: &PromptAnswer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_id = dialog.read(cx).tab_id.clone();
        self.prompt_dialog = None;
        if !self.prompts.is_empty() {
            self.prompts.remove(0);
        }
        self.route_prompt_answer(&tab_id, answer.clone(), window, cx);
        cx.notify();
    }

    /// Send an answer to whoever asked: a tunnel's dial, a management or remote desktop
    /// view, or a terminal session.
    fn route_prompt_answer(
        &mut self,
        key: &str,
        answer: PromptAnswer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use muxus_app::features::forwarding::start::DialAnswer;
        if let Some(PromptReply::Tunnel(reply)) = self.prompt_replies.remove(key) {
            let answer = match answer {
                PromptAnswer::Auth(response) => DialAnswer::Auth(response),
                PromptAnswer::HostKey(accept) | PromptAnswer::Certificate(accept) => {
                    DialAnswer::HostKey(accept)
                }
            };
            let _ = reply.try_send(answer);
            return;
        }
        if self.answer_view_prompt(key, &answer, cx) {
            return;
        }
        let input = match answer {
            PromptAnswer::Auth(Some(response)) => {
                LinkInput::message(TerminalClientMessage::AuthResponse(response))
            }
            // Dismissing an auth prompt ends the connection attempt.
            PromptAnswer::Auth(None) => LinkInput::Close,
            PromptAnswer::HostKey(accept) | PromptAnswer::Certificate(accept) => {
                LinkInput::message(TerminalClientMessage::HostKeyResponse { accept })
            }
        };
        self.send_to_session(key, input, cx);
        self.focused_tab = None;
        self.focus_terminal(key, window, cx);
    }

    /// Queue a question from something other than a terminal tab.
    pub fn queue_prompt(
        &mut self,
        key: String,
        prompt: SessionPrompt,
        reply: Option<PromptReply>,
        cx: &mut Context<Self>,
    ) {
        if let Some(reply) = reply {
            self.prompt_replies.insert(key.clone(), reply);
        }
        self.prompts.push((key, prompt));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_drop_the_trailing_colon() {
        assert_eq!(field_label("Verification code: "), "Verification code");
        assert_eq!(field_label("Password:"), "Password");
        assert_eq!(field_label("PIN"), "PIN");
    }

    #[test]
    fn checks_a_new_master_password() {
        let answers = |a: &str, b: &str| vec![a.to_string(), b.to_string()];
        assert_eq!(
            vault_create_problem(&answers("short", "short")),
            Some("Use at least 8 characters for the master password.")
        );
        assert_eq!(
            vault_create_problem(&answers("long enough", "different")),
            Some("The master-password confirmation does not match.")
        );
        assert_eq!(
            vault_create_problem(&answers("long enough", "long enough")),
            None
        );
    }
}
