//! Dashlane through its `dcli` program (`dashlane.ts`).

use std::sync::LazyLock;

use async_trait::async_trait;
use muxus_core::api::{
    PasswordManagerConfig, PasswordManagerEntry, PasswordManagerKind, PasswordManagerState,
};
use regex::Regex;

use crate::cli::{
    CliOptions, CliResult, LIST_TIMEOUT, STATUS_TIMEOUT, cli_message, parse_json, run_cli,
    text_field,
};
use crate::host::HostEnv;
use crate::provider::{
    PasswordManagerError, PasswordManagerProvider, PasswordManagerResult, ProviderStatus,
    sort_entries, trimmed, without_line_ending,
};

const SIGNED_OUT: &str = "The Dashlane CLI is not logged in. Run “dcli sync” in a terminal first.";
const LOCKED: &str = "The Dashlane CLI is locked. Run “dcli sync” in a terminal and enter the master password to unlock it.";

static LOGGED_IN_NO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)Logged in:\s*No").expect("a valid pattern"));
static LOCKED_YES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)Locked:\s*Yes").expect("a valid pattern"));
static NEEDS_UNLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("(?i)master password|not logged in|log ?in|device").expect("a valid pattern")
});
static GONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)no matching|not found|no secret").expect("a valid pattern"));
static CREDENTIAL_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("^[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$").expect("a valid pattern")
});

/// Dashlane through its `dcli` program, which reads a local copy of the vault and keeps
/// the master password in the operating system's keychain.
pub struct DashlaneProvider {
    program: String,
    host: HostEnv,
}

impl DashlaneProvider {
    pub fn new(config: &PasswordManagerConfig, host: &HostEnv) -> Self {
        Self {
            program: trimmed(config.program.as_deref()).unwrap_or_else(|| "dcli".to_owned()),
            host: host.clone(),
        }
    }

    async fn run(&self, args: &[&str]) -> PasswordManagerResult<CliResult> {
        let result = run_cli(
            &self.host,
            &self.program,
            args,
            CliOptions::timeout(LIST_TIMEOUT),
        )
        .await?;
        if result.code == 0 {
            return Ok(result);
        }
        let message = cli_message(&result, "The Dashlane CLI reported an error.");
        if NEEDS_UNLOCK.is_match(&message) {
            return Err(PasswordManagerError::signed_out(LOCKED));
        }
        if GONE.is_match(&message) {
            return Err(PasswordManagerError::entry_missing(
                "The credential is no longer in Dashlane.",
            ));
        }
        Err(PasswordManagerError::failed(message))
    }
}

#[async_trait]
impl PasswordManagerProvider for DashlaneProvider {
    fn kind(&self) -> PasswordManagerKind {
        PasswordManagerKind::Dashlane
    }

    async fn status(&self) -> PasswordManagerResult<ProviderStatus> {
        let result = run_cli(
            &self.host,
            &self.program,
            &["status"],
            CliOptions::timeout(STATUS_TIMEOUT),
        )
        .await?;
        // Older versions have no status command; reading an entry then says what is wrong.
        if result.code != 0 {
            return Ok(ProviderStatus::ready());
        }
        if LOGGED_IN_NO.is_match(&result.stdout) {
            return Ok(ProviderStatus::new(
                PasswordManagerState::SignedOut,
                SIGNED_OUT,
            ));
        }
        if LOCKED_YES.is_match(&result.stdout) {
            return Ok(ProviderStatus::new(PasswordManagerState::SignedOut, LOCKED));
        }
        Ok(ProviderStatus::ready())
    }

    async fn sync(&self) -> PasswordManagerResult<()> {
        self.run(&["sync"]).await?;
        Ok(())
    }

    async fn entries(&self) -> PasswordManagerResult<Vec<PasswordManagerEntry>> {
        let result = self.run(&["password", "--output", "json"]).await?;
        // The listing carries every decrypted password; none of them is kept.
        let credentials = parse_json("dcli", &result.stdout, Some("[]"))?;
        let mut entries = Vec::new();
        for credential in credentials
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            // A UUID wrapped in braces.
            let Some(id) = text_field(credential, "id")
                .map(|id| {
                    let id = id.strip_prefix('{').unwrap_or(id);
                    id.strip_suffix('}').unwrap_or(id)
                })
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            let url = text_field(credential, "url");
            entries.push(PasswordManagerEntry {
                id: id.to_owned(),
                title: text_field(credential, "title")
                    .or(url)
                    .unwrap_or("Untitled credential")
                    .to_owned(),
                username: trimmed(text_field(credential, "login"))
                    .or_else(|| trimmed(text_field(credential, "email"))),
                url: url.map(str::to_owned),
                group: None,
            });
        }
        Ok(sort_entries(entries))
    }

    async fn password(&self, entry_id: &str) -> PasswordManagerResult<String> {
        // Anything that is not a UUID is taken for a title, and the first match wins.
        if !CREDENTIAL_ID.is_match(entry_id) {
            return Err(PasswordManagerError::entry_missing(
                "The Dashlane credential reference is not valid.",
            ));
        }
        let reference = format!("dl://{entry_id}/password");
        let result = self.run(&["read", &reference]).await?;
        Ok(without_line_ending(&result.stdout).to_owned())
    }
}
