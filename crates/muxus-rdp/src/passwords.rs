//! The password-vault side of an RDP or VNC logon, with the same unlock, create and "remember"
//! prompts an SSH password gets. Passwords are stored under the vault's login-password service
//! alongside SSH passwords.
//!
//! Port of `server/src/remote-desktop/desktop-passwords.ts`. It is protocol-neutral: the session
//! layer drives it for VNC logons too. The vault and the prompts come in through traits so this
//! crate stays free of the store and the UI.

use muxus_core::protocol::{
    AuthPromptField, AuthPromptInfo, AuthPromptPurpose, AuthPromptResponse, RememberPasswordOffer,
};

use crate::session::BoxFuture;

/// Where a desktop password is filed in the vault, and how the user sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultPasswordRef {
    pub account: String,
    pub label: String,
}

/// The vault failures a desktop logon reacts to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VaultFailure {
    /// The master password was wrong.
    #[error("{0}")]
    InvalidMasterPassword(String),
    /// The master password is unusable (too short, confirmation mismatch).
    #[error("{0}")]
    InvalidMasterPasswordFormat(String),
    /// The saved password could not be decrypted.
    #[error("A saved password could not be decrypted.")]
    CredentialCorrupt,
    /// The OS credential store holding the vault key is unavailable.
    #[error("The OS credential store is unavailable.")]
    KeyStoreUnavailable,
    #[error("Password vault is already configured.")]
    AlreadyConfigured,
    #[error("{0}")]
    Other(String),
}

/// The vault operations a desktop logon needs (the session layer implements this over the
/// store's password vault, with its SSH-password service).
pub trait DesktopVault: Send + Sync {
    fn has_password(&self, account: &str) -> bool;
    /// A vault exists.
    fn configured(&self) -> bool;
    /// Using a saved password needs the master password right now.
    fn locked(&self) -> bool;
    fn password(
        &self,
        account: &str,
        master_password: Option<&str>,
    ) -> Result<Option<String>, VaultFailure>;
    fn remember(
        &self,
        account: &str,
        label: &str,
        password: &str,
        master_password: Option<&str>,
    ) -> Result<(), VaultFailure>;
    /// Create the vault with the default unlock policy.
    fn create(&self, master_password: &str) -> Result<(), VaultFailure>;
}

/// The user-facing side: status lines and prompts (the tab's auth-prompt dialog).
pub trait DesktopPrompts: Send + Sync {
    fn status(&self, message: &str, transient: bool);
    fn prompt(&self, info: AuthPromptInfo) -> BoxFuture<'static, AuthPromptResponse>;
}

/// Saved desktop passwords: read before a logon, remembered after one succeeded.
pub struct DesktopPasswords<'a> {
    vault: Option<&'a dyn DesktopVault>,
    io: &'a dyn DesktopPrompts,
}

enum Attempt<T> {
    Ok(T),
    Skipped,
}

fn master_password_field() -> AuthPromptField {
    AuthPromptField {
        prompt: "Master password".into(),
        echo: false,
    }
}

impl<'a> DesktopPasswords<'a> {
    pub fn new(vault: Option<&'a dyn DesktopVault>, io: &'a dyn DesktopPrompts) -> Self {
        Self { vault, io }
    }

    /// Whether a vault is there to save passwords in.
    pub fn available(&self) -> bool {
        self.vault.is_some()
    }

    pub fn has(&self, password: &VaultPasswordRef) -> bool {
        self.vault
            .is_some_and(|vault| vault.has_password(&password.account))
    }

    /// The saved password, unlocking the vault first when its policy asks for that.
    pub async fn read(&self, password: &VaultPasswordRef) -> Result<Option<String>, VaultFailure> {
        let Some(vault) = self.vault else {
            return Ok(None);
        };
        if !vault.has_password(&password.account) {
            return Ok(None);
        }
        let result = if vault.locked() {
            self.with_master_password(
                "Unlock password vault",
                &format!(
                    "Enter the master password to use the saved password for {}.",
                    password.label
                ),
                "Use another password",
                |master| vault.password(&password.account, Some(master)),
            )
            .await
            .map(|attempt| match attempt {
                Attempt::Ok(value) => value,
                Attempt::Skipped => None,
            })
        } else {
            vault.password(&password.account, None)
        };
        match result {
            Err(VaultFailure::CredentialCorrupt) => {
                self.io.status(
                    &format!(
                        "The saved password for {} is damaged. Enter it again to replace the saved copy.",
                        password.label
                    ),
                    false,
                );
                Ok(None)
            }
            Err(VaultFailure::KeyStoreUnavailable) => {
                self.io.status(
                    &format!(
                        "The OS credential store is unavailable. Enter the password for {}.",
                        password.label
                    ),
                    false,
                );
                Ok(None)
            }
            other => other,
        }
    }

    /// Save a password the user asked to remember, once the login has succeeded.
    pub async fn remember(
        &self,
        reference: &VaultPasswordRef,
        password: &str,
    ) -> Result<(), VaultFailure> {
        let Some(vault) = self.vault else {
            return Ok(());
        };
        if !vault.configured() && !self.create_vault(vault).await? {
            return Ok(());
        }
        if vault.locked() {
            let attempt = self
                .with_master_password(
                    "Unlock password vault",
                    &format!(
                        "Enter the master password to remember the password for {}.",
                        reference.label
                    ),
                    "Not now",
                    |master| {
                        vault.remember(&reference.account, &reference.label, password, Some(master))
                    },
                )
                .await?;
            if matches!(attempt, Attempt::Skipped) {
                return Ok(());
            }
        } else {
            vault.remember(&reference.account, &reference.label, password, None)?;
        }
        self.io.status(
            &format!("Remembered the password for {}.", reference.label),
            false,
        );
        Ok(())
    }

    async fn create_vault(&self, vault: &dyn DesktopVault) -> Result<bool, VaultFailure> {
        let response = self
            .io
            .prompt(AuthPromptInfo {
                name: Some("Create password vault".into()),
                purpose: Some(AuthPromptPurpose::VaultCreate),
                instructions: Some(
                    "Create a master password to protect viewing and editing saved passwords. \
                     By default, Muxus stores the vault key in the operating-system credential store \
                     so saved passwords can be used without another master-password prompt."
                        .into(),
                ),
                prompts: vec![
                    master_password_field(),
                    AuthPromptField {
                        prompt: "Confirm master password".into(),
                        echo: false,
                    },
                ],
                skip_label: Some("Not now".into()),
                ..AuthPromptInfo::default()
            })
            .await;
        if response.skipped == Some(true) {
            return Ok(false);
        }
        let master = response.answers.first().cloned().unwrap_or_default();
        let confirmation = response.answers.get(1).cloned().unwrap_or_default();
        if master != confirmation {
            return Err(VaultFailure::InvalidMasterPasswordFormat(
                "The master-password confirmation did not match.".into(),
            ));
        }
        match vault.create(&master) {
            Ok(()) | Err(VaultFailure::AlreadyConfigured) => Ok(true),
            Err(err) => Err(err),
        }
    }

    async fn with_master_password<T>(
        &self,
        name: &str,
        instructions: &str,
        skip_label: &str,
        operation: impl Fn(&str) -> Result<T, VaultFailure>,
    ) -> Result<Attempt<T>, VaultFailure> {
        let mut error: Option<String> = None;
        for _ in 0..3 {
            let response = self
                .io
                .prompt(AuthPromptInfo {
                    name: Some(name.into()),
                    purpose: Some(AuthPromptPurpose::VaultUnlock),
                    instructions: Some(match &error {
                        Some(error) => format!("{error}\n\n{instructions}"),
                        None => instructions.to_owned(),
                    }),
                    prompts: vec![master_password_field()],
                    skip_label: Some(skip_label.into()),
                    ..AuthPromptInfo::default()
                })
                .await;
            if response.skipped == Some(true) {
                return Ok(Attempt::Skipped);
            }
            let master = response.answers.first().cloned().unwrap_or_default();
            match operation(&master) {
                Ok(value) => return Ok(Attempt::Ok(value)),
                Err(
                    VaultFailure::InvalidMasterPassword(message)
                    | VaultFailure::InvalidMasterPasswordFormat(message),
                ) => error = Some(message),
                Err(err) => return Err(err),
            }
        }
        self.io.status("The password vault remains locked.", false);
        Ok(Attempt::Skipped)
    }
}

/// The logon prompt of a desktop tab (`askLogin` in `desktop-socket.ts`): a password, and the
/// user name too when the host does not store one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogonPrompt {
    /// `Remote Desktop logon` or `VNC authentication`.
    pub name: String,
    /// `host:port`, shown as the asking host.
    pub host: String,
    /// The known user name; the vault files the password under it (`None` when none applies).
    pub username: Option<String>,
    pub ask_username: bool,
    /// The server refused the last credentials.
    pub rejected: bool,
    /// Those credentials came from the vault.
    pub used_saved_password: bool,
    /// The "remember" offer, when a vault is available.
    pub remember: Option<RememberPasswordOffer>,
}

/// What the user typed at a [`LogonPrompt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogonAnswer {
    pub username: String,
    pub password: String,
    pub remember: bool,
}

impl LogonPrompt {
    /// The RDP logon name.
    pub const RDP: &'static str = "Remote Desktop logon";
    /// The VNC credentials name.
    pub const VNC: &'static str = "VNC authentication";

    pub fn info(&self) -> AuthPromptInfo {
        let password_only = !self.ask_username && self.username.is_none();
        let instructions = self.rejected.then(|| {
            if self.used_saved_password {
                "The saved password was not accepted. Enter the current password."
            } else if password_only {
                "The password was not accepted."
            } else {
                "The user name or password was not accepted."
            }
            .to_owned()
        });
        let mut prompts = Vec::new();
        if self.ask_username {
            prompts.push(AuthPromptField {
                prompt: "User name".into(),
                echo: true,
            });
        }
        prompts.push(AuthPromptField {
            prompt: "Password".into(),
            echo: false,
        });
        AuthPromptInfo {
            name: Some(self.name.clone()),
            host: Some(self.host.clone()),
            purpose: Some(AuthPromptPurpose::Authentication),
            instructions,
            prompts,
            remember_password: self.remember.clone(),
            ..AuthPromptInfo::default()
        }
    }

    /// The credentials from the user's answer; `Err` with the session's closing message when
    /// the prompt was cancelled.
    pub fn answer(&self, response: &AuthPromptResponse) -> Result<LogonAnswer, String> {
        if response.skipped == Some(true) {
            return Err("Logon cancelled.".into());
        }
        let username = if self.ask_username {
            response
                .answers
                .first()
                .map(|name| name.trim().to_owned())
                .unwrap_or_default()
        } else {
            self.username.clone().unwrap_or_default()
        };
        let password = response
            .answers
            .get(usize::from(self.ask_username))
            .cloned()
            .unwrap_or_default();
        Ok(LogonAnswer {
            username,
            password,
            remember: response.remember_password == Some(true),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeVault {
        configured: Mutex<bool>,
        locked: bool,
        master: String,
        saved: Mutex<Vec<(String, String, String)>>,
        corrupt: bool,
    }

    impl DesktopVault for FakeVault {
        fn has_password(&self, account: &str) -> bool {
            self.saved
                .lock()
                .unwrap()
                .iter()
                .any(|(a, _, _)| a == account)
        }

        fn configured(&self) -> bool {
            *self.configured.lock().unwrap()
        }

        fn locked(&self) -> bool {
            self.locked
        }

        fn password(
            &self,
            account: &str,
            master: Option<&str>,
        ) -> Result<Option<String>, VaultFailure> {
            if self.locked && master != Some(self.master.as_str()) {
                return Err(VaultFailure::InvalidMasterPassword(
                    "The master password is incorrect.".into(),
                ));
            }
            if self.corrupt {
                return Err(VaultFailure::CredentialCorrupt);
            }
            Ok(self
                .saved
                .lock()
                .unwrap()
                .iter()
                .find(|(a, _, _)| a == account)
                .map(|(_, _, p)| p.clone()))
        }

        fn remember(
            &self,
            account: &str,
            label: &str,
            password: &str,
            master: Option<&str>,
        ) -> Result<(), VaultFailure> {
            if self.locked && master != Some(self.master.as_str()) {
                return Err(VaultFailure::InvalidMasterPassword(
                    "The master password is incorrect.".into(),
                ));
            }
            self.saved
                .lock()
                .unwrap()
                .push((account.into(), label.into(), password.into()));
            Ok(())
        }

        fn create(&self, _master: &str) -> Result<(), VaultFailure> {
            *self.configured.lock().unwrap() = true;
            Ok(())
        }
    }

    #[derive(Default)]
    struct Script {
        answers: Mutex<Vec<AuthPromptResponse>>,
        prompts: Mutex<Vec<AuthPromptInfo>>,
        statuses: Mutex<Vec<String>>,
    }

    impl Script {
        fn answering(answers: &[&[&str]]) -> Self {
            Self {
                answers: Mutex::new(
                    answers
                        .iter()
                        .map(|answers| AuthPromptResponse {
                            answers: answers.iter().map(|a| a.to_string()).collect(),
                            ..AuthPromptResponse::default()
                        })
                        .collect(),
                ),
                ..Self::default()
            }
        }
    }

    impl DesktopPrompts for Script {
        fn status(&self, message: &str, _transient: bool) {
            self.statuses.lock().unwrap().push(message.into());
        }

        fn prompt(&self, info: AuthPromptInfo) -> BoxFuture<'static, AuthPromptResponse> {
            self.prompts.lock().unwrap().push(info);
            let mut answers = self.answers.lock().unwrap();
            let answer = if answers.is_empty() {
                AuthPromptResponse {
                    skipped: Some(true),
                    ..AuthPromptResponse::default()
                }
            } else {
                answers.remove(0)
            };
            Box::pin(async move { answer })
        }
    }

    fn reference() -> VaultPasswordRef {
        VaultPasswordRef {
            account: "rdp-account".into(),
            label: "RDP admin@win:3389".into(),
        }
    }

    fn vault_with_password(locked: bool) -> FakeVault {
        let vault = FakeVault {
            locked,
            master: "master-password-1".into(),
            ..FakeVault::default()
        };
        *vault.configured.lock().unwrap() = true;
        vault
            .saved
            .lock()
            .unwrap()
            .push(("rdp-account".into(), "label".into(), "saved".into()));
        vault
    }

    #[tokio::test]
    async fn reads_an_unlocked_password_without_asking() {
        let vault = vault_with_password(false);
        let io = Script::default();
        let passwords = DesktopPasswords::new(Some(&vault), &io);
        assert!(passwords.available() && passwords.has(&reference()));
        assert_eq!(passwords.read(&reference()).await, Ok(Some("saved".into())));
        assert!(io.prompts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unlocks_with_the_master_password_and_repeats_the_error() {
        let vault = vault_with_password(true);
        let io = Script::answering(&[&["wrong"], &["master-password-1"]]);
        let passwords = DesktopPasswords::new(Some(&vault), &io);
        assert_eq!(passwords.read(&reference()).await, Ok(Some("saved".into())));
        let prompts = io.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0].purpose, Some(AuthPromptPurpose::VaultUnlock));
        assert_eq!(
            prompts[0].skip_label.as_deref(),
            Some("Use another password")
        );
        assert_eq!(
            prompts[1].instructions.as_deref(),
            Some(
                "The master password is incorrect.\n\nEnter the master password to use the saved password for RDP admin@win:3389."
            )
        );
    }

    #[tokio::test]
    async fn gives_up_after_three_wrong_master_passwords() {
        let vault = vault_with_password(true);
        let io = Script::answering(&[&["a"], &["b"], &["c"]]);
        let passwords = DesktopPasswords::new(Some(&vault), &io);
        assert_eq!(passwords.read(&reference()).await, Ok(None));
        assert_eq!(
            io.statuses.lock().unwrap().as_slice(),
            ["The password vault remains locked."]
        );
    }

    #[tokio::test]
    async fn asks_for_the_password_again_when_the_saved_copy_is_damaged() {
        let vault = FakeVault {
            corrupt: true,
            ..vault_with_password(false)
        };
        let io = Script::default();
        let passwords = DesktopPasswords::new(Some(&vault), &io);
        assert_eq!(passwords.read(&reference()).await, Ok(None));
        assert_eq!(
            io.statuses.lock().unwrap().as_slice(),
            [
                "The saved password for RDP admin@win:3389 is damaged. Enter it again to replace the saved copy."
            ]
        );
    }

    #[tokio::test]
    async fn offers_to_create_a_vault_and_refuses_a_mismatched_confirmation() {
        let vault = FakeVault::default();
        let io = Script::answering(&[&["master-password-1", "something-else"]]);
        let passwords = DesktopPasswords::new(Some(&vault), &io);
        assert_eq!(
            passwords.remember(&reference(), "secret").await,
            Err(VaultFailure::InvalidMasterPasswordFormat(
                "The master-password confirmation did not match.".into()
            ))
        );
        assert_eq!(
            io.prompts.lock().unwrap()[0].purpose,
            Some(AuthPromptPurpose::VaultCreate)
        );
        assert!(vault.saved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn creates_the_vault_then_remembers() {
        let vault = FakeVault::default();
        let io = Script::answering(&[&["master-password-1", "master-password-1"]]);
        let passwords = DesktopPasswords::new(Some(&vault), &io);
        passwords.remember(&reference(), "secret").await.unwrap();
        assert_eq!(
            vault.saved.lock().unwrap().as_slice(),
            [(
                "rdp-account".to_string(),
                "RDP admin@win:3389".to_string(),
                "secret".to_string()
            )]
        );
        assert_eq!(
            io.statuses.lock().unwrap().as_slice(),
            ["Remembered the password for RDP admin@win:3389."]
        );
    }

    #[tokio::test]
    async fn not_now_skips_remembering_in_a_locked_vault() {
        let vault = vault_with_password(true);
        let io = Script::default();
        let passwords = DesktopPasswords::new(Some(&vault), &io);
        passwords.remember(&reference(), "new").await.unwrap();
        assert_eq!(
            io.prompts.lock().unwrap()[0].skip_label.as_deref(),
            Some("Not now")
        );
        assert!(io.statuses.lock().unwrap().is_empty());
        let without_vault = DesktopPasswords::new(None, &io);
        assert!(!without_vault.available());
        assert_eq!(without_vault.read(&reference()).await, Ok(None));
    }

    #[test]
    fn asks_for_vnc_credentials_the_way_the_backend_did() {
        let prompt = LogonPrompt {
            name: LogonPrompt::VNC.into(),
            host: "127.0.0.1:5900".into(),
            username: None,
            ask_username: false,
            rejected: false,
            used_saved_password: false,
            remember: Some(RememberPasswordOffer {
                label: "VNC 127.0.0.1:5900".into(),
                existing: false,
            }),
        };
        let info = prompt.info();
        assert_eq!(info.name.as_deref(), Some("VNC authentication"));
        assert_eq!(info.host.as_deref(), Some("127.0.0.1:5900"));
        assert_eq!(
            info.prompts,
            [AuthPromptField {
                prompt: "Password".into(),
                echo: false
            }]
        );
        assert_eq!(info.instructions, None);
        let answer = prompt
            .answer(&AuthPromptResponse {
                answers: vec!["secret".into()],
                remember_password: Some(true),
                skipped: None,
            })
            .unwrap();
        assert_eq!(
            answer,
            LogonAnswer {
                username: String::new(),
                password: "secret".into(),
                remember: true
            }
        );
        assert_eq!(
            prompt.answer(&AuthPromptResponse {
                skipped: Some(true),
                ..AuthPromptResponse::default()
            }),
            Err("Logon cancelled.".into())
        );
    }

    #[test]
    fn asks_for_the_user_name_and_explains_a_rejection() {
        let prompt = LogonPrompt {
            name: LogonPrompt::RDP.into(),
            host: "win:3389".into(),
            username: None,
            ask_username: true,
            rejected: true,
            used_saved_password: false,
            remember: None,
        };
        let info = prompt.info();
        assert_eq!(info.prompts.len(), 2);
        assert!(info.prompts[0].echo);
        assert_eq!(
            info.instructions.as_deref(),
            Some("The user name or password was not accepted.")
        );
        let answer = prompt
            .answer(&AuthPromptResponse {
                answers: vec![" alice ".into(), "pw".into()],
                ..AuthPromptResponse::default()
            })
            .unwrap();
        assert_eq!(
            (answer.username.as_str(), answer.password.as_str()),
            ("alice", "pw")
        );
        let saved = LogonPrompt {
            ask_username: false,
            username: Some("alice".into()),
            used_saved_password: true,
            ..prompt.clone()
        };
        assert_eq!(
            saved.info().instructions.as_deref(),
            Some("The saved password was not accepted. Enter the current password.")
        );
        let password_only = LogonPrompt {
            ask_username: false,
            username: None,
            ..prompt
        };
        assert_eq!(
            password_only.info().instructions.as_deref(),
            Some("The password was not accepted.")
        );
    }
}
