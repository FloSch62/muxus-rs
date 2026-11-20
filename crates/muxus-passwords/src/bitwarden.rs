//! Bitwarden and Vaultwarden through the `bw` program (`bitwarden.ts`).

use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use muxus_core::api::{
    PasswordManagerConfig, PasswordManagerEntry, PasswordManagerKind, PasswordManagerState,
};
use regex::Regex;
use serde_json::Value;

use crate::cli::{
    CliOptions, CliResult, LIST_TIMEOUT, STATUS_TIMEOUT, cli_message, parse_json, run_cli,
    text_field,
};
use crate::host::HostEnv;
use crate::provider::{
    PasswordManagerError, PasswordManagerProvider, PasswordManagerResult, ProviderStatus,
    sort_entries, trimmed, without_line_ending,
};

/// Bitwarden's item type for a login; notes, cards and identities have no login password.
const LOGIN_ITEM: i64 = 1;
const PASSWORD_VARIABLE: &str = "MUXUS_BW_PASSWORD";
const NOT_LOGGED_IN: &str =
    "The Bitwarden CLI is not logged in. Run “bw login” in a terminal first.";

static WRONG_PASSWORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)invalid master password").expect("a valid pattern"));
static LOGGED_OUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)not logged in").expect("a valid pattern"));
static LOCKED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)vault is locked").expect("a valid pattern"));
static GONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)not found").expect("a valid pattern"));
static ITEM_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("^[0-9a-fA-F-]{36}$").expect("a valid pattern"));

/// Bitwarden (and Vaultwarden) through the `bw` command-line program. `bw` stays signed in
/// between runs but locked: unlocking it with the master password returns a session key,
/// which Muxus keeps in memory and passes to later runs in the environment, never on the
/// command line.
pub struct BitwardenProvider {
    program: String,
    session: Mutex<Option<String>>,
    host: HostEnv,
}

impl BitwardenProvider {
    pub fn new(config: &PasswordManagerConfig, host: &HostEnv) -> Self {
        Self {
            program: trimmed(config.program.as_deref()).unwrap_or_else(|| "bw".to_owned()),
            session: Mutex::new(None),
            host: host.clone(),
        }
    }

    fn session(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_session(&self, session: Option<String>) {
        *self.session.lock().unwrap_or_else(PoisonError::into_inner) = session;
    }

    async fn run(&self, args: &[&str], timeout: Duration) -> PasswordManagerResult<CliResult> {
        let mut args = args.to_vec();
        args.push("--nointeraction");
        let mut options = CliOptions::timeout(timeout);
        // Without a key of its own, Muxus leaves a BW_SESSION it inherited in place.
        if let Some(session) = self.session() {
            options = options.with_env("BW_SESSION", Some(&session));
        }
        run_cli(&self.host, &self.program, &args, options).await
    }

    fn check(&self, result: CliResult) -> PasswordManagerResult<CliResult> {
        if result.code == 0 {
            return Ok(result);
        }
        let message = cli_message(&result, "The Bitwarden CLI reported an error.");
        if LOCKED.is_match(&message) {
            self.set_session(None);
            return Err(PasswordManagerError::locked(
                "The Bitwarden vault is locked.",
            ));
        }
        if LOGGED_OUT.is_match(&message) {
            self.set_session(None);
            return Err(PasswordManagerError::signed_out(NOT_LOGGED_IN));
        }
        if GONE.is_match(&message) {
            return Err(PasswordManagerError::entry_missing(
                "The item is no longer in Bitwarden.",
            ));
        }
        Err(PasswordManagerError::failed(message))
    }
}

#[async_trait]
impl PasswordManagerProvider for BitwardenProvider {
    fn kind(&self) -> PasswordManagerKind {
        PasswordManagerKind::Bitwarden
    }

    async fn status(&self) -> PasswordManagerResult<ProviderStatus> {
        let result = self.run(&["status"], STATUS_TIMEOUT).await?;
        if result.code != 0 {
            return Ok(ProviderStatus::new(
                PasswordManagerState::Unavailable,
                cli_message(&result, "The Bitwarden CLI reported an error."),
            ));
        }
        let parsed = parse_json("bw", &result.stdout, None)?;
        let status = parsed.get("status").and_then(Value::as_str);
        if status == Some("unlocked") {
            return Ok(ProviderStatus::ready());
        }
        // The session key Muxus held is gone, for instance after `bw lock` elsewhere.
        self.set_session(None);
        Ok(if status == Some("locked") {
            ProviderStatus::new(
                PasswordManagerState::Locked,
                "Enter the Bitwarden master password to unlock the vault.",
            )
        } else {
            ProviderStatus::new(PasswordManagerState::SignedOut, NOT_LOGGED_IN)
        })
    }

    fn can_unlock(&self) -> bool {
        true
    }

    async fn unlock(&self, password: &str) -> PasswordManagerResult<()> {
        let result = run_cli(
            &self.host,
            &self.program,
            &[
                "unlock",
                "--raw",
                "--nointeraction",
                "--passwordenv",
                PASSWORD_VARIABLE,
            ],
            CliOptions::timeout(LIST_TIMEOUT)
                .with_env(PASSWORD_VARIABLE, Some(password))
                .with_env("BW_SESSION", None),
        )
        .await?;
        let session = muxus_core::js::js_trim(&result.stdout);
        if result.code != 0 || session.is_empty() {
            let message = cli_message(&result, "The Bitwarden vault could not be unlocked.");
            if WRONG_PASSWORD.is_match(&message) {
                return Err(PasswordManagerError::invalid_unlock(
                    "The Bitwarden master password is wrong.",
                ));
            }
            if LOGGED_OUT.is_match(&message) {
                return Err(PasswordManagerError::signed_out(NOT_LOGGED_IN));
            }
            return Err(PasswordManagerError::failed(message));
        }
        self.set_session(Some(session.to_owned()));
        Ok(())
    }

    async fn lock(&self) -> PasswordManagerResult<()> {
        self.set_session(None);
        // Ends the session for good, so the key Muxus held stops working.
        let _ = self.run(&["lock"], STATUS_TIMEOUT).await;
        Ok(())
    }

    async fn sync(&self) -> PasswordManagerResult<()> {
        self.check(self.run(&["sync"], LIST_TIMEOUT).await?)?;
        Ok(())
    }

    fn dispose(&self) {
        self.set_session(None);
    }

    async fn entries(&self) -> PasswordManagerResult<Vec<PasswordManagerEntry>> {
        let result = self.check(self.run(&["list", "items"], LIST_TIMEOUT).await?)?;
        let items = parse_json("bw", &result.stdout, Some("[]"))?;
        let mut entries = Vec::new();
        for item in items.as_array().map(Vec::as_slice).unwrap_or_default() {
            let login = item.get("login").unwrap_or(&Value::Null);
            // The listing carries decrypted passwords; only their presence is used.
            let (Some(id), Some(_password)) =
                (text_field(item, "id"), text_field(login, "password"))
            else {
                continue;
            };
            if item.get("type").and_then(Value::as_i64) != Some(LOGIN_ITEM) {
                continue;
            }
            let url = login
                .get("uris")
                .and_then(Value::as_array)
                .and_then(|uris| uris.iter().find_map(|entry| text_field(entry, "uri")));
            entries.push(PasswordManagerEntry {
                id: id.to_owned(),
                title: text_field(item, "name")
                    .unwrap_or("Untitled item")
                    .to_owned(),
                username: trimmed(text_field(login, "username")),
                url: url.map(str::to_owned),
                group: None,
            });
        }
        Ok(sort_entries(entries))
    }

    async fn password(&self, entry_id: &str) -> PasswordManagerResult<String> {
        // Anything but an exact ID makes `bw get` search, and guess.
        if !ITEM_ID.is_match(entry_id) {
            return Err(PasswordManagerError::entry_missing(
                "The Bitwarden item reference is not valid.",
            ));
        }
        let result = self.check(
            self.run(&["get", "password", entry_id], LIST_TIMEOUT)
                .await?,
        )?;
        Ok(without_line_ending(&result.stdout).to_owned())
    }
}
