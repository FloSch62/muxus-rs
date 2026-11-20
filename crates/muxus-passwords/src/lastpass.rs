//! LastPass through the `lpass` program (`lastpass.ts`).

use std::sync::LazyLock;

use async_trait::async_trait;
use muxus_core::api::{
    PasswordManagerConfig, PasswordManagerEntry, PasswordManagerKind, PasswordManagerState,
};
use regex::Regex;

use crate::cli::{CliOptions, CliResult, LIST_TIMEOUT, STATUS_TIMEOUT, cli_message, run_cli};
use crate::host::HostEnv;
use crate::provider::{
    PasswordManagerError, PasswordManagerProvider, PasswordManagerResult, ProviderStatus,
    non_empty, sort_entries, trimmed, without_line_ending,
};

const SIGNED_OUT: &str =
    "The LastPass CLI is not logged in. Run “lpass login <email>” in a terminal first.";
/// Between the fields of one listed entry; a character no name or URL holds.
const FIELD: char = '\u{1f}';
/// ID, name, user name, URL and folder of each entry.
const LIST_FORMAT: &str = "--format=%ai\u{1f}%an\u{1f}%au\u{1f}%al\u{1f}%ag";

static LOGGED_OUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("(?i)could not find decryption key|not logged in|log ?in").expect("a valid pattern")
});
static GONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)could not find specified account").expect("a valid pattern"));

fn is_entry_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
}

/// LastPass through the `lpass` program. Its own agent keeps the vault key after
/// `lpass login`, so Muxus only ever asks it for one entry at a time.
pub struct LastPassProvider {
    program: String,
    host: HostEnv,
}

impl LastPassProvider {
    pub fn new(config: &PasswordManagerConfig, host: &HostEnv) -> Self {
        Self {
            program: trimmed(config.program.as_deref()).unwrap_or_else(|| "lpass".to_owned()),
            host: host.clone(),
        }
    }

    fn options(timeout: std::time::Duration) -> CliOptions {
        // Without a terminal, a pinentry program could open a window nobody expects, or
        // wait on one that never shows; failing at once is the better answer.
        CliOptions::timeout(timeout).with_env("LPASS_DISABLE_PINENTRY", Some("1"))
    }

    async fn run(&self, args: &[&str]) -> PasswordManagerResult<CliResult> {
        let result = run_cli(&self.host, &self.program, args, Self::options(LIST_TIMEOUT)).await?;
        if result.code == 0 {
            return Ok(result);
        }
        let message = cli_message(&result, "The LastPass CLI reported an error.");
        if LOGGED_OUT.is_match(&message) {
            return Err(PasswordManagerError::signed_out(SIGNED_OUT));
        }
        if GONE.is_match(&message) {
            return Err(PasswordManagerError::entry_missing(
                "The entry is no longer in LastPass.",
            ));
        }
        Err(PasswordManagerError::failed(message))
    }
}

#[async_trait]
impl PasswordManagerProvider for LastPassProvider {
    fn kind(&self) -> PasswordManagerKind {
        PasswordManagerKind::Lastpass
    }

    async fn status(&self) -> PasswordManagerResult<ProviderStatus> {
        // Exits 0 while logged in and 1 otherwise.
        let result = run_cli(
            &self.host,
            &self.program,
            &["status", "--quiet", "--color=never"],
            Self::options(STATUS_TIMEOUT),
        )
        .await?;
        Ok(if result.code == 0 {
            ProviderStatus::ready()
        } else {
            ProviderStatus::new(PasswordManagerState::SignedOut, SIGNED_OUT)
        })
    }

    async fn sync(&self) -> PasswordManagerResult<()> {
        self.run(&["sync", "--color=never"]).await?;
        Ok(())
    }

    async fn entries(&self) -> PasswordManagerResult<Vec<PasswordManagerEntry>> {
        let result = self
            .run(&["ls", "--sync=auto", "--color=never", LIST_FORMAT])
            .await?;
        let mut entries = Vec::new();
        for line in result.stdout.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let mut fields = line.split(FIELD);
            let mut next = || fields.next().unwrap_or("");
            let (id, name, username, url, group) = (next(), next(), next(), next(), next());
            // Folders are listed as entries at "http://group", secure notes at "http://sn".
            if !is_entry_id(id) || url == "http://group" || url == "http://sn" {
                continue;
            }
            entries.push(PasswordManagerEntry {
                id: id.to_owned(),
                title: non_empty(Some(name)).unwrap_or_else(|| "Untitled entry".to_owned()),
                username: non_empty(Some(username)),
                url: non_empty(Some(url)).filter(|url| url != "http://"),
                group: non_empty(Some(group)),
            });
        }
        Ok(sort_entries(entries))
    }

    async fn password(&self, entry_id: &str) -> PasswordManagerResult<String> {
        // The numeric ID names exactly one entry; a name could match several.
        if !is_entry_id(entry_id) {
            return Err(PasswordManagerError::entry_missing(
                "The LastPass entry reference is not valid.",
            ));
        }
        let result = self
            .run(&[
                "show",
                "--sync=auto",
                "--color=never",
                "--password",
                entry_id,
            ])
            .await?;
        Ok(without_line_ending(&result.stdout).to_owned())
    }
}
