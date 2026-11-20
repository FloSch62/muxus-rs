//! Proton Pass through its `pass-cli` program (`protonpass.ts`).

use std::sync::LazyLock;

use async_trait::async_trait;
use muxus_core::api::{
    PasswordManagerConfig, PasswordManagerEntry, PasswordManagerKind, PasswordManagerState,
};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use regex::Regex;
use serde_json::Value;

use crate::cli::{
    APPROVAL_TIMEOUT, CliOptions, CliResult, LIST_TIMEOUT, STATUS_TIMEOUT, cli_message, parse_json,
    run_cli,
};
use crate::host::HostEnv;
use crate::provider::{
    PasswordManagerError, PasswordManagerProvider, PasswordManagerResult, ProviderStatus,
    sort_entries, trimmed, without_line_ending,
};

const SIGNED_OUT: &str =
    "The Proton Pass CLI is not logged in. Run “pass-cli login” in a terminal first.";

static LOGGED_OUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("(?i)not logged in|log ?in|session|unauthori[sz]ed").expect("a valid pattern")
});
static GONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)not found").expect("a valid pattern"));
static REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("^(share|vault):([^/]+)/([^/]+)$").expect("a valid pattern"));

/// What `encodeURIComponent` leaves as it is.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

struct Vault {
    name: String,
    share_id: Option<String>,
}

/// Proton Pass through its `pass-cli` program, which keeps its own login session. Items
/// are listed one vault at a time, because an item ID is only unique together with its
/// vault's share ID.
pub struct ProtonPassProvider {
    program: String,
    host: HostEnv,
}

impl ProtonPassProvider {
    pub fn new(config: &PasswordManagerConfig, host: &HostEnv) -> Self {
        Self {
            program: trimmed(config.program.as_deref()).unwrap_or_else(|| "pass-cli".to_owned()),
            host: host.clone(),
        }
    }

    async fn run(
        &self,
        args: &[&str],
        timeout: std::time::Duration,
    ) -> PasswordManagerResult<CliResult> {
        let result = run_cli(
            &self.host,
            &self.program,
            args,
            CliOptions::timeout(timeout),
        )
        .await?;
        if result.code == 0 {
            return Ok(result);
        }
        let message = cli_message(&result, "The Proton Pass CLI reported an error.");
        if LOGGED_OUT.is_match(&message) {
            return Err(PasswordManagerError::signed_out(SIGNED_OUT));
        }
        if GONE.is_match(&message) {
            return Err(PasswordManagerError::entry_missing(
                "The item is no longer in Proton Pass.",
            ));
        }
        Err(PasswordManagerError::failed(message))
    }
}

#[async_trait]
impl PasswordManagerProvider for ProtonPassProvider {
    fn kind(&self) -> PasswordManagerKind {
        PasswordManagerKind::Protonpass
    }

    async fn status(&self) -> PasswordManagerResult<ProviderStatus> {
        let result = run_cli(
            &self.host,
            &self.program,
            &["info"],
            CliOptions::timeout(STATUS_TIMEOUT),
        )
        .await?;
        Ok(if result.code == 0 {
            ProviderStatus::ready()
        } else {
            ProviderStatus::new(PasswordManagerState::SignedOut, SIGNED_OUT)
        })
    }

    async fn entries(&self) -> PasswordManagerResult<Vec<PasswordManagerEntry>> {
        let listed = self
            .run(&["vault", "list", "--output", "json"], LIST_TIMEOUT)
            .await?;
        let mut entries = Vec::new();
        for vault in vaults_of(&parse_json("pass-cli", &listed.stdout, Some("{}"))?) {
            let scope = match &vault.share_id {
                Some(share_id) => ["--share-id", share_id.as_str()],
                None => ["--vault-name", vault.name.as_str()],
            };
            let result = self
                .run(
                    &[
                        "item",
                        "list",
                        scope[0],
                        scope[1],
                        "--filter-type",
                        "login",
                        "--filter-state",
                        "active",
                        "--output",
                        "json",
                    ],
                    LIST_TIMEOUT,
                )
                .await?;
            let parsed = parse_json("pass-cli", &result.stdout, Some("{}"))?;
            for item in list_of(&parsed, "items") {
                if !item.is_object() {
                    continue;
                }
                let Some(id) = text(item.get("id")).or_else(|| text(item.get("item_id"))) else {
                    continue;
                };
                let share_id = text(item.get("share_id")).or_else(|| vault.share_id.clone());
                let content = item.get("content").filter(|content| content.is_object());
                let login = content
                    .and_then(|content| content.get("content"))
                    .and_then(|content| content.get("Login"))
                    .filter(|login| login.is_object());
                let field = |name: &str| login.and_then(|login| text(login.get(name)));
                let url = login
                    .and_then(|login| login.get("urls"))
                    .and_then(Value::as_array)
                    .and_then(|urls| urls.iter().find_map(|url| text(Some(url))));
                entries.push(PasswordManagerEntry {
                    id: match share_id {
                        Some(share_id) => format!("share:{share_id}/{id}"),
                        None => format!(
                            "vault:{}/{id}",
                            utf8_percent_encode(&vault.name, URI_COMPONENT)
                        ),
                    },
                    title: content
                        .and_then(|content| text(content.get("title")))
                        .or_else(|| text(item.get("title")))
                        .unwrap_or_else(|| "Untitled item".to_owned()),
                    username: field("username").or_else(|| field("email")),
                    url,
                    group: (!vault.name.is_empty()).then(|| vault.name.clone()),
                });
            }
        }
        Ok(sort_entries(entries))
    }

    async fn password(&self, entry_id: &str) -> PasswordManagerResult<String> {
        let invalid =
            || PasswordManagerError::entry_missing("The Proton Pass item reference is not valid.");
        let found = REFERENCE.captures(entry_id).ok_or_else(invalid)?;
        let (scope, container, item) = (&found[1], &found[2], &found[3]);
        let container = if scope == "share" {
            container.to_owned()
        } else {
            percent_decode_str(container)
                .decode_utf8()
                .map_err(|_| invalid())?
                .into_owned()
        };
        let result = self
            .run(
                &[
                    "item",
                    "view",
                    if scope == "share" {
                        "--share-id"
                    } else {
                        "--vault-name"
                    },
                    &container,
                    "--item-id",
                    item,
                    "--field",
                    "password",
                ],
                APPROVAL_TIMEOUT,
            )
            .await?;
        Ok(without_line_ending(&result.stdout).to_owned())
    }
}

fn vaults_of(parsed: &Value) -> Vec<Vault> {
    let mut vaults = Vec::new();
    for vault in list_of(parsed, "vaults") {
        if !vault.is_object() {
            continue;
        }
        let name = text(vault.get("name")).or_else(|| {
            vault
                .get("content")
                .filter(|content| content.is_object())
                .and_then(|content| text(content.get("name")))
        });
        let share_id = text(vault.get("share_id"));
        if name.is_some() || share_id.is_some() {
            vaults.push(Vault {
                name: name.unwrap_or_default(),
                share_id,
            });
        }
    }
    vaults
}

/// A listing that is the array itself, or an object holding it under `key`.
fn list_of<'a>(parsed: &'a Value, key: &str) -> &'a [Value] {
    parsed
        .as_array()
        .or_else(|| parsed.get(key).and_then(Value::as_array))
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Text that is not blank, trimmed.
fn text(value: Option<&Value>) -> Option<String> {
    trimmed(value.and_then(Value::as_str))
}
