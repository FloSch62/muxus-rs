//! Typing a vault secret into a terminal (`client/src/vault-secret-send.ts`
//! and `typeSecretIntoTab` in `vault-secrets.ts`). The backend types the
//! value, so it never reaches the window; while the vault's prompt policy
//! wants the master password, the window asks for it ("Unlock password
//! vault") and the send is retried.
//!
//! [`SecretSender`] is a ready implementation of `CommandHooks::send_secret`
//! (command buttons, the send-secret menu): the window creates one, renders
//! it (it draws the unlock prompt) and registers it with
//! [`install_secret_sender`], handing it how to find a tab's backend
//! terminal ids (the tab first, then the panes multi-execution mirrors it to,
//! as `features::settings::vault_secrets::secret_terminal_ids` computes).

use std::rc::Rc;

use muxus_session::Backend;
use muxus_ui::gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, Styled,
    Window, div,
};
use muxus_ui::theme::Status;

use crate::features::command_buttons::run::{
    CommandHooks, SecretRef, SecretSendOutcome, command_hooks, send_secret_to_terminals,
    set_command_hooks,
};
use crate::features::launcher::kit::show_toast;
use crate::features::sftp::dialogs::{DialogQueue, PromptOptions};

/// The master-password prompt a locked vault answers a send with.
pub fn unlock_prompt(description: String) -> PromptOptions {
    PromptOptions {
        title: "Unlock password vault".into(),
        description: Some(description),
        label: "Master password".into(),
        masked: true,
        confirm_label: "Send".into(),
        ..PromptOptions::default()
    }
}

/// The toast a finished send shows, if any: status and message.
pub fn send_outcome_toast(outcome: &SecretSendOutcome) -> Option<(Status, String)> {
    match outcome {
        SecretSendOutcome::Failed {
            warning, message, ..
        } => Some((
            if *warning {
                Status::Warning
            } else {
                Status::Error
            },
            message.clone(),
        )),
        _ => None,
    }
}

/// Backend terminal ids a secret typed into a tab goes to.
pub type TerminalIdsOf = Rc<dyn Fn(&str, &App) -> Vec<String>>;

/// What the sender tells its window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretSenderEvent {
    /// The vault status changed (unlocked for the session, or a secret was
    /// found missing): read it again.
    VaultChanged,
}

/// Sends secrets for one window and asks for the master password in it.
pub struct SecretSender {
    backend: Backend,
    terminal_ids: TerminalIdsOf,
    dialogs: DialogQueue,
}

impl EventEmitter<SecretSenderEvent> for SecretSender {}

impl SecretSender {
    pub fn new(backend: Backend, terminal_ids: TerminalIdsOf) -> Self {
        Self {
            backend,
            terminal_ids,
            dialogs: DialogQueue::new(),
        }
    }

    /// `typeSecretIntoTab`: false when the tab is not a connected terminal;
    /// anything else is reported by the sender as a toast.
    pub fn send(
        &mut self,
        tab_id: &str,
        secret: &SecretRef,
        enter: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let ids = (self.terminal_ids)(tab_id, cx);
        if ids.is_empty() {
            return false;
        }
        let backend = self.backend.clone();
        let secret = secret.clone();
        let tab = tab_id.to_owned();
        let this = cx.entity().downgrade();
        let window_handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            let asked_password = std::cell::Cell::new(false);
            let outcome = send_secret_to_terminals(&backend, ids, &secret, enter, |description| {
                asked_password.set(true);
                let this = this.clone();
                let mut cx = cx.clone();
                let answer = window_handle
                    .update(&mut cx, |_, window, cx| {
                        this.update(cx, |sender, cx| {
                            sender.dialogs.prompt(
                                unlock_prompt(description),
                                |sender: &mut SecretSender| &mut sender.dialogs,
                                window,
                                cx,
                            )
                        })
                        .ok()
                    })
                    .ok()
                    .flatten();
                async move {
                    match answer {
                        Some(answer) => answer.await,
                        None => None,
                    }
                }
            })
            .await;
            let _ = window_handle.update(cx, |_, window, cx| {
                if let Some((status, message)) = send_outcome_toast(&outcome) {
                    show_toast(status, message, cx);
                }
                let refresh = match &outcome {
                    // A startup-policy vault stays unlocked once the master password was given.
                    SecretSendOutcome::Sent => asked_password.get(),
                    SecretSendOutcome::Failed { refresh_vault, .. } => *refresh_vault,
                    SecretSendOutcome::Cancelled => false,
                };
                if refresh {
                    let _ = this.update(cx, |_, cx| cx.emit(SecretSenderEvent::VaultChanged));
                }
                // Keyboard focus goes back to the terminal either way.
                if let Some(focus) = command_hooks(cx).focus_terminal {
                    focus(&tab, window, cx);
                }
            });
        })
        .detach();
        true
    }
}

impl Render for SecretSender {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let overlay = self
            .dialogs
            .render(|sender: &mut Self| &mut sender.dialogs, window, cx);
        div().size_0().children(overlay)
    }
}

/// Create a sender for a window.
pub fn secret_sender(
    backend: Backend,
    terminal_ids: TerminalIdsOf,
    cx: &mut App,
) -> Entity<SecretSender> {
    cx.new(|_| SecretSender::new(backend, terminal_ids))
}

/// Make `sender` the window's `CommandHooks::send_secret`, keeping the
/// other hooks.
pub fn install_secret_sender(sender: Entity<SecretSender>, cx: &mut App) {
    let mut hooks: CommandHooks = command_hooks(cx);
    hooks.send_secret = Some(Rc::new(move |tab_id, secret, enter, window, cx| {
        sender.update(cx, |sender, cx| {
            sender.send(tab_id, secret, enter, window, cx)
        })
    }));
    set_command_hooks(hooks, cx);
}

#[cfg(test)]
mod tests {
    //! `vault-secrets.test.ts`, "sending a secret", against a sandboxed
    //! backend: a real vault, a real secret and a live local terminal.

    use std::time::Duration;

    use muxus_core::api::{PasswordVaultUnlockPolicy, VaultSecretInput};
    use muxus_core::protocol::{TerminalClientMessage, TerminalServerMessage};
    use muxus_session::{BackendOptions, RuntimeChoice, SessionEvent, SessionHandle};

    use super::*;

    const MASTER: &str = "master-pass-12";

    struct Sandbox {
        runtime: tokio::runtime::Runtime,
        backend: Backend,
        secret: SecretRef,
        _scratch: tempfile::TempDir,
    }

    fn sandbox(policy: PasswordVaultUnlockPolicy) -> Sandbox {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let home = scratch.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut options = BackendOptions::sandbox(scratch.path().join("data"), &home);
        options.runtime = RuntimeChoice::Handle(runtime.handle().clone());
        let backend = runtime.block_on(Backend::start(options)).unwrap();
        runtime
            .block_on(backend.create_vault(MASTER, Some(policy)))
            .unwrap();
        let saved = runtime
            .block_on(backend.create_vault_secret(VaultSecretInput {
                name: "Core enable".into(),
                username: None,
                value: Some("en4ble".into()),
                master_password: MASTER.into(),
            }))
            .unwrap();
        Sandbox {
            runtime,
            backend,
            secret: SecretRef {
                id: saved.secret.id,
                name: saved.secret.name,
            },
            _scratch: scratch,
        }
    }

    impl Sandbox {
        fn terminal(&self) -> SessionHandle {
            let connect: TerminalClientMessage = serde_json::from_value(serde_json::json!({
                "op": "connect",
                "profile": { "kind": "local", "shell": "/bin/sh" },
                "cols": 80,
                "rows": 24
            }))
            .unwrap();
            let session = self.backend.open_session(connect);
            self.runtime.block_on(async {
                loop {
                    let event = tokio::time::timeout(Duration::from_secs(20), session.next_event())
                        .await
                        .unwrap()
                        .unwrap();
                    match event {
                        SessionEvent::Message(TerminalServerMessage::Ready { .. }) => break,
                        SessionEvent::Message(TerminalServerMessage::Exit { message, .. }) => {
                            panic!("the shell ended: {message:?}")
                        }
                        _ => {}
                    }
                }
            });
            session
        }

        fn send(
            &self,
            terminal_ids: Vec<String>,
            answers: &mut Vec<Option<String>>,
            asked: &mut Vec<String>,
        ) -> SecretSendOutcome {
            self.runtime.block_on(send_secret_to_terminals(
                &self.backend,
                terminal_ids,
                &self.secret,
                true,
                |description| {
                    asked.push(description);
                    let answer = if answers.is_empty() {
                        None
                    } else {
                        answers.remove(0)
                    };
                    async move { answer }
                },
            ))
        }
    }

    #[test]
    fn sends_only_the_reference_to_a_connected_terminal() {
        let sandbox = sandbox(PasswordVaultUnlockPolicy::Never);
        let terminal = sandbox.terminal();
        let outcome = sandbox.send(
            vec![terminal.terminal_id().to_owned()],
            &mut Vec::new(),
            &mut Vec::new(),
        );
        assert_eq!(outcome, SecretSendOutcome::Sent);
        terminal.close();
        sandbox.runtime.block_on(sandbox.backend.shutdown());
    }

    #[test]
    fn asks_for_the_master_password_while_the_vault_is_locked_again_when_it_is_wrong() {
        let sandbox = sandbox(PasswordVaultUnlockPolicy::Credential);
        let terminal = sandbox.terminal();
        let mut answers = vec![Some(" wrong-pass ".to_owned()), Some(MASTER.to_owned())];
        let mut asked = Vec::new();
        let outcome = sandbox.send(
            vec![terminal.terminal_id().to_owned()],
            &mut answers,
            &mut asked,
        );
        assert_eq!(outcome, SecretSendOutcome::Sent);
        assert_eq!(
            asked,
            [
                "Enter the master password to send “Core enable”.",
                "The master password is incorrect."
            ]
        );
        let prompt = unlock_prompt(asked[0].clone());
        assert_eq!(prompt.title, "Unlock password vault");
        assert!(prompt.masked);
        assert_eq!(prompt.confirm_label, "Send");
        terminal.close();
        sandbox.runtime.block_on(sandbox.backend.shutdown());
    }

    #[test]
    fn stops_when_the_prompt_is_dismissed_and_reports_a_deleted_secret() {
        let sandbox = sandbox(PasswordVaultUnlockPolicy::Credential);
        let terminal = sandbox.terminal();
        let ids = vec![terminal.terminal_id().to_owned()];
        let mut asked = Vec::new();
        let cancelled = sandbox.send(ids.clone(), &mut vec![None], &mut asked);
        assert_eq!(cancelled, SecretSendOutcome::Cancelled);
        assert_eq!(asked.len(), 1);
        assert_eq!(send_outcome_toast(&cancelled), None);

        let missing = SecretRef {
            id: "deleted-secret".into(),
            name: "Core enable".into(),
        };
        let outcome = sandbox.runtime.block_on(send_secret_to_terminals(
            &sandbox.backend,
            ids,
            &missing,
            true,
            |_| async { Some(MASTER.to_owned()) },
        ));
        assert_eq!(
            send_outcome_toast(&outcome),
            Some((
                Status::Error,
                "“Core enable” is no longer in the password vault.".to_owned()
            ))
        );
        assert!(matches!(
            outcome,
            SecretSendOutcome::Failed {
                refresh_vault: true,
                ..
            }
        ));
        terminal.close();
        sandbox.runtime.block_on(sandbox.backend.shutdown());
    }

    #[test]
    fn warns_when_the_terminal_is_not_connected() {
        let sandbox = sandbox(PasswordVaultUnlockPolicy::Never);
        let outcome = sandbox.send(Vec::new(), &mut Vec::new(), &mut Vec::new());
        assert_eq!(
            send_outcome_toast(&outcome),
            Some((
                Status::Warning,
                "The active terminal is not connected.".to_owned()
            ))
        );
        let gone = sandbox.send(
            vec!["terminal-gone".to_owned()],
            &mut Vec::new(),
            &mut Vec::new(),
        );
        assert_eq!(
            send_outcome_toast(&gone),
            Some((
                Status::Warning,
                "The active terminal is not connected.".to_owned()
            ))
        );
        sandbox.runtime.block_on(sandbox.backend.shutdown());
    }
}
