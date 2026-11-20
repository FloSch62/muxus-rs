//! 1Password through its `op` program (`onepassword.ts`).

use std::sync::LazyLock;

use async_trait::async_trait;
use muxus_core::api::{
    PasswordManagerConfig, PasswordManagerEntry, PasswordManagerKind, PasswordManagerState,
};
use regex::Regex;
use serde_json::Value;

use crate::cli::{
    APPROVAL_TIMEOUT, CliOptions, CliResult, STATUS_TIMEOUT, cli_message, parse_json, run_cli,
    text_field,
};
use crate::host::HostEnv;
use crate::provider::{
    PasswordManagerError, PasswordManagerProvider, PasswordManagerResult, ProviderStatus,
    non_empty, sort_entries, trimmed,
};

/// Item categories that carry a login password in a field called `password`.
const CATEGORIES: &str = "Login,Password,Server,Database";

static GONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)isn't an item|not found|no item").expect("a valid pattern"));
static SIGNED_OUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("(?i)not (currently )?signed in|no accounts configured|sign ?in")
        .expect("a valid pattern")
});
static ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("^[a-zA-Z0-9]+$").expect("a valid pattern"));

/// 1Password through its `op` command-line program. With the 1Password app's CLI
/// integration turned on, `op` asks the app to approve each session, with Touch ID,
/// Windows Hello or the system password; a service-account token in the environment works
/// as well. Muxus never sees the account password.
pub struct OnePasswordProvider {
    program: String,
    account: Option<String>,
    host: HostEnv,
}

impl OnePasswordProvider {
    pub fn new(config: &PasswordManagerConfig, host: &HostEnv) -> Self {
        Self {
            program: trimmed(config.program.as_deref()).unwrap_or_else(|| "op".to_owned()),
            account: trimmed(config.account.as_deref()),
            host: host.clone(),
        }
    }

    async fn run(&self, args: &[&str]) -> PasswordManagerResult<CliResult> {
        let mut args = args.to_vec();
        if let Some(account) = &self.account {
            args.extend(["--account", account]);
        }
        let result = run_cli(
            &self.host,
            &self.program,
            &args,
            CliOptions::timeout(APPROVAL_TIMEOUT),
        )
        .await?;
        if result.code == 0 {
            return Ok(result);
        }
        let message = cli_message(&result, "The 1Password CLI reported an error.");
        if GONE.is_match(&message) {
            return Err(PasswordManagerError::entry_missing(
                "The item is no longer in 1Password.",
            ));
        }
        if SIGNED_OUT.is_match(&message) {
            return Err(PasswordManagerError::signed_out(message));
        }
        Err(PasswordManagerError::failed(message))
    }
}

#[async_trait]
impl PasswordManagerProvider for OnePasswordProvider {
    fn kind(&self) -> PasswordManagerKind {
        PasswordManagerKind::Onepassword
    }

    async fn status(&self) -> PasswordManagerResult<ProviderStatus> {
        if self.host.var("OP_SERVICE_ACCOUNT_TOKEN").is_some() {
            return Ok(ProviderStatus::ready());
        }
        // Listing accounts never asks for approval, unlike reading from one.
        let result = run_cli(
            &self.host,
            &self.program,
            &["account", "list", "--format", "json"],
            CliOptions::timeout(STATUS_TIMEOUT),
        )
        .await?;
        if result.code != 0 {
            return Ok(ProviderStatus::new(
                PasswordManagerState::SignedOut,
                cli_message(&result, "The 1Password CLI is not set up."),
            ));
        }
        let accounts = parse_json("op", &result.stdout, Some("[]"))?;
        Ok(
            if accounts
                .as_array()
                .is_some_and(|accounts| !accounts.is_empty())
            {
                ProviderStatus::ready()
            } else {
                ProviderStatus::new(
                    PasswordManagerState::SignedOut,
                    "No 1Password account is available to the CLI. Turn on “Integrate with 1Password CLI” in the 1Password app, under Settings → Developer.",
                )
            },
        )
    }

    async fn entries(&self) -> PasswordManagerResult<Vec<PasswordManagerEntry>> {
        let result = self
            .run(&[
                "item",
                "list",
                "--categories",
                CATEGORIES,
                "--format",
                "json",
            ])
            .await?;
        let items = parse_json("op", &result.stdout, Some("[]"))?;
        let mut entries = Vec::new();
        for item in items.as_array().map(Vec::as_slice).unwrap_or_default() {
            let vault = item.get("vault").unwrap_or(&Value::Null);
            let (Some(id), Some(vault_id)) = (text_field(item, "id"), text_field(vault, "id"))
            else {
                continue;
            };
            // Items without a user name show a dash here.
            let username = trimmed(text_field(item, "additional_information"))
                .filter(|username| username != "—" && username != "-");
            let urls = item
                .get("urls")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let url = urls
                .iter()
                .find(|url| js_truthy(url.get("primary")))
                .or_else(|| urls.first())
                .and_then(|url| text_field(url, "href"));
            entries.push(PasswordManagerEntry {
                // The vault is part of the address: `op read` needs both.
                id: format!("{vault_id}/{id}"),
                title: text_field(item, "title")
                    .unwrap_or("Untitled item")
                    .to_owned(),
                username,
                url: non_empty(url),
                group: non_empty(text_field(vault, "name")),
            });
        }
        Ok(sort_entries(entries))
    }

    async fn password(&self, entry_id: &str) -> PasswordManagerResult<String> {
        let parts: Vec<&str> = entry_id.split('/').collect();
        let [vault, item] = parts.as_slice() else {
            return Err(invalid_reference());
        };
        if !ID.is_match(vault) || !ID.is_match(item) {
            return Err(invalid_reference());
        }
        // IDs rather than names: a name may hold characters a reference cannot.
        let reference = format!("op://{vault}/{item}/password");
        let result = self.run(&["read", "--no-newline", &reference]).await?;
        Ok(result.stdout)
    }
}

fn invalid_reference() -> PasswordManagerError {
    PasswordManagerError::entry_missing("The 1Password item reference is not valid.")
}

/// Whether JavaScript would take a JSON value for true.
fn js_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|number| number != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}
