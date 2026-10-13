//! Running a saved command in a terminal (`runCommandButton`,
//! `activateCommandButton` and `typeSecretIntoTab` in
//! `client/src/vault-secrets.ts`, the sender in `vault-secret-send.ts`) and
//! the words the bar uses to describe a button.
//!
//! The terminals belong to the window, so the window registers
//! [`CommandHooks`]: how to type into a tab's terminal, give it focus and
//! type a vault secret into it. [`send_secret_to_terminals`] is the sender
//! loop the secret hook can use.

use std::future::Future;
use std::rc::Rc;

use muxus_core::api::SendVaultSecretRequest;
use muxus_session::Backend;
use muxus_store::prefs::command_buttons::{
    CommandButton, command_button_input, command_button_label, is_secret_command_button,
};
use muxus_ui::gpui_kit::{App, Global, Pixels, Point, Window};

/// The secret a button types: its id and the name it is known by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretRef {
    pub id: String,
    pub name: String,
}

type SendInput = Rc<dyn Fn(&str, &str, &mut Window, &mut App) -> bool>;
type FocusTerminal = Rc<dyn Fn(&str, &mut Window, &mut App)>;
type SendSecret = Rc<dyn Fn(&str, &SecretRef, bool, &mut Window, &mut App) -> bool>;
type CursorAnchor = Rc<dyn Fn(&str, &mut Window, &mut App) -> Option<Point<Pixels>>>;

/// What running a command needs from the window's terminals.
#[derive(Clone, Default)]
pub struct CommandHooks {
    /// `terminal.sendInput(text)` for a tab (multi-execution mirrors it as it
    /// mirrors typing). False when the tab has no connected terminal.
    pub send_input: Option<SendInput>,
    /// `terminal.focus()`.
    pub focus_terminal: Option<FocusTerminal>,
    /// `typeSecretIntoTab`: hand the secret to the backend to type into the
    /// tab and the panes mirroring it (asking for the master password while
    /// the vault wants one). False when the tab is not a connected terminal.
    pub send_secret: Option<SendSecret>,
    /// Where the terminal's caret is (`cursorAnchorPosition`), for the
    /// keyboard menu; the menu centres itself without one.
    pub cursor_anchor: Option<CursorAnchor>,
}

impl Global for CommandHooks {}

/// Register the window's terminals (replacing earlier hooks).
pub fn set_command_hooks(hooks: CommandHooks, cx: &mut App) {
    cx.set_global(hooks);
}

pub fn command_hooks(cx: &App) -> CommandHooks {
    cx.try_global::<CommandHooks>().cloned().unwrap_or_default()
}

/// Send a saved command and return keyboard focus to its terminal
/// (`activateCommandButton`).
pub fn activate_command_button(
    tab_id: &str,
    button: &CommandButton,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let hooks = command_hooks(cx);
    let Some(send) = hooks.send_input else {
        return false;
    };
    let sent = send(tab_id, &command_button_input(button), window, cx);
    if let Some(focus) = hooks.focus_terminal {
        focus(tab_id, window, cx);
    }
    sent
}

/// Run a saved command in a tab (`runCommandButton`): its text through the
/// terminal, or its secret through the backend. False when the tab is not a
/// connected terminal.
pub fn run_command_button(
    tab_id: Option<&str>,
    button: &CommandButton,
    secret_name: Option<&str>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let Some(tab_id) = tab_id else {
        return false;
    };
    if !is_secret_command_button(button) {
        return activate_command_button(tab_id, button, window, cx);
    }
    let Some(send) = command_hooks(cx).send_secret else {
        return false;
    };
    let secret = SecretRef {
        id: button.secret_id.clone().unwrap_or_default(),
        name: secret_name
            .map(str::to_owned)
            .unwrap_or_else(|| command_button_label(button)),
    };
    send(tab_id, &secret, button.send_enter, window, cx)
}

/// Give a tab's terminal the keyboard back (after a menu closes).
pub fn focus_terminal(tab_id: Option<&str>, window: &mut Window, cx: &mut App) {
    if let (Some(tab_id), Some(focus)) = (tab_id, command_hooks(cx).focus_terminal) {
        focus(tab_id, window, cx);
    }
}

/// How a secret send ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretSendOutcome {
    Sent,
    /// The user dismissed the master password prompt.
    Cancelled,
    /// A warning or error to show as a toast, and whether the vault status
    /// should be read again.
    Failed {
        warning: bool,
        message: String,
        refresh_vault: bool,
    },
}

/// Codes a master password can resolve.
const UNLOCK_CODES: [&str; 3] = [
    "vault-locked",
    "vault-automatic-access-unavailable",
    "invalid-master-password",
];

/// `sendSecretToTab`: ask the backend to type `secret` into the terminals
/// (the tab's first, then its mirrors), asking `ask_password` for the master
/// password while the vault's prompt policy wants one. `ask_password` gets
/// the prompt's description and answers `None` when dismissed.
pub async fn send_secret_to_terminals<F, Fut>(
    backend: &Backend,
    terminal_ids: Vec<String>,
    secret: &SecretRef,
    enter: bool,
    mut ask_password: F,
) -> SecretSendOutcome
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Option<String>>,
{
    if terminal_ids.is_empty() {
        return SecretSendOutcome::Failed {
            warning: true,
            message: "The active terminal is not connected.".into(),
            refresh_vault: false,
        };
    }
    let mut master_password: Option<String> = None;
    let mut problem: Option<String> = None;
    loop {
        let result = backend
            .send_vault_secret(
                &secret.id,
                SendVaultSecretRequest {
                    terminal_ids: terminal_ids.clone(),
                    enter,
                    master_password: master_password.clone(),
                },
            )
            .await;
        let error = match result {
            Ok(_) => return SecretSendOutcome::Sent,
            Err(error) => error,
        };
        let code = error.code.as_deref().unwrap_or_default();
        if UNLOCK_CODES.contains(&code) {
            if code == "invalid-master-password" {
                problem = Some("The master password is incorrect.".into());
            }
            let description = problem
                .clone()
                .unwrap_or_else(|| format!("Enter the master password to send “{}”.", secret.name));
            match ask_password(description).await {
                Some(answer) => {
                    master_password = Some(answer);
                    continue;
                }
                None => return SecretSendOutcome::Cancelled,
            }
        }
        return match code {
            "vault-secret-missing" => SecretSendOutcome::Failed {
                warning: false,
                message: format!("“{}” is no longer in the password vault.", secret.name),
                refresh_vault: true,
            },
            "terminal-unavailable" => SecretSendOutcome::Failed {
                warning: true,
                message: "The active terminal is not connected.".into(),
                refresh_vault: false,
            },
            _ => SecretSendOutcome::Failed {
                warning: false,
                message: error.message,
                refresh_vault: false,
            },
        };
    }
}

/// What a bar button does, in its tooltip (`buttonTooltip`). A secret
/// button names its secret, never its value.
pub fn button_tooltip(button: &CommandButton, secret_name: Option<&str>, missing: bool) -> String {
    let Some(secret_id) = &button.secret_id else {
        let command = if button.command.is_empty() {
            "No command"
        } else {
            button.command.as_str()
        };
        return format!(
            "{command}{}",
            if button.send_enter {
                " · runs immediately"
            } else {
                " · inserts only"
            }
        );
    };
    if missing {
        return "Its secret was deleted from the password vault".into();
    }
    if secret_id.is_empty() {
        return "No secret chosen".into();
    }
    match secret_name {
        None => "Types a secret from the password vault".into(),
        Some(name) => format!(
            "Types the secret “{name}”{}",
            if button.send_enter {
                " and presses Enter"
            } else {
                ""
            }
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(command: &str, send_enter: bool, secret_id: Option<&str>) -> CommandButton {
        CommandButton {
            id: "a".into(),
            label: "A".into(),
            command: command.into(),
            send_enter,
            group_id: None,
            color: None,
            secret_id: secret_id.map(Into::into),
        }
    }

    #[test]
    fn describes_what_a_button_sends() {
        assert_eq!(
            button_tooltip(&button("df -h", true, None), None, false),
            "df -h · runs immediately"
        );
        assert_eq!(
            button_tooltip(&button("", false, None), None, false),
            "No command · inserts only"
        );
        assert_eq!(
            button_tooltip(&button("", true, Some("s1")), Some("Core enable"), false),
            "Types the secret “Core enable” and presses Enter"
        );
        assert_eq!(
            button_tooltip(&button("", true, Some("s1")), None, true),
            "Its secret was deleted from the password vault"
        );
        assert_eq!(
            button_tooltip(&button("", true, Some("")), None, false),
            "No secret chosen"
        );
        assert_eq!(
            button_tooltip(&button("", false, Some("s1")), None, false),
            "Types a secret from the password vault"
        );
    }
}
