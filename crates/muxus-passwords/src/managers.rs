//! The password managers connected in Settings (`password-managers.ts`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use muxus_core::api::{
    ExternalPasswordRef, PASSWORD_MANAGER_KINDS, PasswordManagerConfig, PasswordManagerEntry,
    PasswordManagerInput, PasswordManagerKind, PasswordManagerSource, PasswordManagerState,
    PasswordManagersResponse,
};
use muxus_core::protocol::{
    AuthPromptField, AuthPromptInfo, AuthPromptPurpose, AuthPromptResponse,
};

use crate::bitwarden::BitwardenProvider;
use crate::cli::program_installed;
use crate::dashlane::DashlaneProvider;
use crate::host::HostEnv;
use crate::keepass::{KeePassProvider, database_name};
use crate::lastpass::LastPassProvider;
use crate::onepassword::OnePasswordProvider;
use crate::pass::PassProvider;
use crate::protonpass::ProtonPassProvider;
use crate::provider::{
    PasswordManagerError, PasswordManagerProblem, PasswordManagerProvider, ProviderStatus, trimmed,
};

const UNLOCK_ATTEMPTS: usize = 3;

/// One connected password manager, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSource {
    pub id: String,
    /// The kind as stored; a newer build may have written one this build does not know.
    pub kind: String,
    pub name: String,
    pub config: PasswordManagerConfig,
    pub created_at: String,
    pub updated_at: String,
}

/// Where the connected password managers are kept: the database, which the session layer
/// stands in for. Errors are the store's own words.
pub trait SourceStore: Send + Sync {
    fn list(&self) -> Result<Vec<StoredSource>, String>;
    fn get(&self, id: &str) -> Result<Option<StoredSource>, String>;
    fn create(
        &self,
        kind: PasswordManagerKind,
        name: &str,
        config: &PasswordManagerConfig,
    ) -> Result<StoredSource, String>;
    fn update(
        &self,
        id: &str,
        name: &str,
        config: &PasswordManagerConfig,
    ) -> Result<Option<StoredSource>, String>;
    fn delete(&self, id: &str) -> Result<bool, String>;
}

/// Why a request about a connected password manager failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PasswordManagersError {
    #[error(transparent)]
    Provider(#[from] PasswordManagerError),
    #[error("The password manager is no longer connected.")]
    SourceMissing,
    /// The database refused; the message is its own.
    #[error("{0}")]
    Store(String),
}

/// The person dismissed a prompt: the login it belonged to is given up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("authentication cancelled")]
pub struct PromptCancelled;

/// The part of a connection's prompt channel reading a password needs.
#[async_trait]
pub trait PasswordManagerIo: Send + Sync {
    /// A line for the terminal; a transient one may be replaced by the next.
    fn status(&self, message: &str, transient: bool);
    async fn prompt(&self, info: AuthPromptInfo) -> Result<AuthPromptResponse, PromptCancelled>;
}

/// Makes the provider of one kind from its settings.
pub type ProviderFactory = Arc<
    dyn Fn(PasswordManagerKind, &PasswordManagerConfig) -> Arc<dyn PasswordManagerProvider>
        + Send
        + Sync,
>;

/// The provider each kind is read through on this computer.
pub fn default_provider(
    host: &HostEnv,
    kind: PasswordManagerKind,
    config: &PasswordManagerConfig,
) -> Arc<dyn PasswordManagerProvider> {
    match kind {
        PasswordManagerKind::Keepass => Arc::new(KeePassProvider::new(config, host)),
        PasswordManagerKind::Onepassword => Arc::new(OnePasswordProvider::new(config, host)),
        PasswordManagerKind::Bitwarden => Arc::new(BitwardenProvider::new(config, host)),
        PasswordManagerKind::Protonpass => Arc::new(ProtonPassProvider::new(config, host)),
        PasswordManagerKind::Lastpass => Arc::new(LastPassProvider::new(config, host)),
        PasswordManagerKind::Dashlane => Arc::new(DashlaneProvider::new(config, host)),
        PasswordManagerKind::Pass => Arc::new(PassProvider::new(config, host)),
    }
}

/// The program each kind runs by default; KeePass reads its file directly.
fn programs(kind: PasswordManagerKind) -> &'static [&'static str] {
    match kind {
        PasswordManagerKind::Keepass => &[],
        PasswordManagerKind::Onepassword => &["op"],
        PasswordManagerKind::Bitwarden => &["bw"],
        PasswordManagerKind::Protonpass => &["pass-cli"],
        PasswordManagerKind::Lastpass => &["lpass"],
        PasswordManagerKind::Dashlane => &["dcli"],
        PasswordManagerKind::Pass => &["gpg2", "gpg"],
    }
}

/// What the unlock prompt asks for.
fn unlock_prompt(kind: PasswordManagerKind) -> Option<&'static str> {
    match kind {
        PasswordManagerKind::Keepass => Some("Database password"),
        PasswordManagerKind::Bitwarden => Some("Bitwarden master password"),
        _ => None,
    }
}

struct Held {
    signature: String,
    provider: Arc<dyn PasswordManagerProvider>,
}

/// The password managers connected in Settings. Each gets one provider that lives as long
/// as its configuration, so an unlocked KeePass database or a Bitwarden session stays
/// unlocked across connections.
pub struct PasswordManagers {
    store: Arc<dyn SourceStore>,
    factory: ProviderFactory,
    host: HostEnv,
    providers: Mutex<HashMap<String, Held>>,
}

impl PasswordManagers {
    /// The managers in `store`, read through the programs and files of this computer.
    pub fn new(store: Arc<dyn SourceStore>) -> Self {
        Self::for_host(store, HostEnv::system())
    }

    /// The managers in `store`, read through the programs and files `host` sees.
    pub fn for_host(store: Arc<dyn SourceStore>, host: HostEnv) -> Self {
        let factory_host = host.clone();
        Self::with_providers(
            store,
            host,
            Arc::new(move |kind, config| default_provider(&factory_host, kind, config)),
        )
    }

    /// The managers in `store`, each read through what `factory` makes for its kind.
    pub fn with_providers(
        store: Arc<dyn SourceStore>,
        host: HostEnv,
        factory: ProviderFactory,
    ) -> Self {
        Self {
            store,
            factory,
            host,
            providers: Mutex::new(HashMap::new()),
        }
    }

    pub async fn list(&self) -> Result<PasswordManagersResponse, PasswordManagersError> {
        let rows = self.store.list().map_err(PasswordManagersError::Store)?;
        // A slow program must not hold up the others: every source is asked at once.
        let describing: Vec<_> = rows
            .into_iter()
            .filter_map(|row| {
                let kind = PasswordManagerKind::parse(&row.kind)?;
                let provider = self.provider(&row);
                Some(tokio::spawn(
                    async move { describe(row, kind, provider).await },
                ))
            })
            .collect();
        let host = self.host.clone();
        let installed = tokio::task::spawn_blocking(move || installed_kinds(&host));
        let mut sources = Vec::with_capacity(describing.len());
        for described in describing {
            if let Ok(source) = described.await {
                sources.push(source);
            }
        }
        Ok(PasswordManagersResponse {
            sources,
            installed: installed.await.unwrap_or_default(),
        })
    }

    pub async fn create(
        &self,
        input: &PasswordManagerInput,
    ) -> Result<PasswordManagerSource, PasswordManagersError> {
        let config = input.config.cleaned();
        let name =
            trimmed(input.name.as_deref()).unwrap_or_else(|| default_name(input.kind, &config));
        let row = self
            .store
            .create(input.kind, &name, &config)
            .map_err(PasswordManagersError::Store)?;
        Ok(self.describe(row, input.kind).await)
    }

    /// Rename a connected manager or change how it is reached; `None` when it is gone.
    pub async fn update(
        &self,
        id: &str,
        input: &PasswordManagerInput,
    ) -> Result<Option<PasswordManagerSource>, PasswordManagersError> {
        let Some(current) = self.store.get(id).map_err(PasswordManagersError::Store)? else {
            return Ok(None);
        };
        let config = input.config.cleaned();
        let name = trimmed(input.name.as_deref()).unwrap_or(current.name);
        let Some(row) = self
            .store
            .update(id, &name, &config)
            .map_err(PasswordManagersError::Store)?
        else {
            return Ok(None);
        };
        let kind = known_kind(&row)?;
        Ok(Some(self.describe(row, kind).await))
    }

    /// Disconnect a manager; whether there was one. Hosts and folders keep their
    /// reference, which reports the missing manager when it is used.
    pub fn remove(&self, id: &str) -> Result<bool, PasswordManagersError> {
        self.forget(id);
        self.store.delete(id).map_err(PasswordManagersError::Store)
    }

    pub async fn unlock(
        &self,
        id: &str,
        password: &str,
    ) -> Result<PasswordManagerSource, PasswordManagersError> {
        let (row, kind, provider) = self.require(id)?;
        if !provider.can_unlock() {
            return Err(PasswordManagerError::failed(format!(
                "{} is unlocked in its own app, not in Muxus.",
                row.name
            ))
            .into());
        }
        provider.unlock(password).await?;
        Ok(describe(row, kind, Ok(provider)).await)
    }

    pub async fn lock(&self, id: &str) -> Result<PasswordManagerSource, PasswordManagersError> {
        let (row, kind, provider) = self.require(id)?;
        provider.lock().await?;
        Ok(describe(row, kind, Ok(provider)).await)
    }

    /// The entries a login can use, after pulling changes from the manager's server
    /// when `sync` asks for it.
    pub async fn entries(
        &self,
        id: &str,
        sync: bool,
    ) -> Result<Vec<PasswordManagerEntry>, PasswordManagersError> {
        let (_, _, provider) = self.require(id)?;
        if sync {
            provider.sync().await?;
        }
        Ok(provider.entries().await?)
    }

    /// The entry's password, or why it cannot be read.
    pub async fn password(
        &self,
        entry: &ExternalPasswordRef,
    ) -> Result<String, PasswordManagersError> {
        let (_, _, provider) = self.require(&entry.source_id)?;
        Ok(provider.password(&entry.entry_id).await?)
    }

    /// The entry's password for a login in progress. A locked manager is unlocked through
    /// the connection's own prompt; anything else that goes wrong is reported there and
    /// gives `None`, so the login falls back to asking for the password.
    pub async fn password_for(
        &self,
        entry: &ExternalPasswordRef,
        io: &dyn PasswordManagerIo,
    ) -> Result<Option<String>, PromptCancelled> {
        let named = if entry.title.is_empty() {
            "the linked entry".to_owned()
        } else {
            format!("“{}”", entry.title)
        };
        let row = match self.store.get(&entry.source_id) {
            Ok(Some(row)) => row,
            Ok(None) => {
                io.status(
                    &format!(
                        "The password manager holding {named} is no longer connected. Enter the password."
                    ),
                    false,
                );
                return Ok(None);
            }
            Err(error) => {
                io.status(
                    &format!("Could not read {named}: {error} Enter the password."),
                    false,
                );
                return Ok(None);
            }
        };
        let name = row.name.clone();
        let unreadable = |error: &PasswordManagerError| {
            io.status(
                &format!(
                    "Could not read {named} from {name}: {} Enter the password.",
                    error.message
                ),
                false,
            );
        };
        let provider = match self.provider(&row) {
            Ok(provider) => provider,
            Err(error) => {
                unreadable(&error);
                return Ok(None);
            }
        };
        let prompt = PasswordManagerKind::parse(&row.kind).and_then(unlock_prompt);
        let mut problem: Option<String> = None;
        for attempt in 0..=UNLOCK_ATTEMPTS {
            let error = match provider.password(&entry.entry_id).await {
                Ok(password) => {
                    io.status(&format!("Using the password of {named} from {name}."), true);
                    return Ok(Some(password));
                }
                Err(error) => error,
            };
            let prompt = match prompt {
                Some(prompt)
                    if error.problem == PasswordManagerProblem::Locked
                        && provider.can_unlock()
                        && attempt < UNLOCK_ATTEMPTS =>
                {
                    prompt
                }
                _ => {
                    unreadable(&error);
                    return Ok(None);
                }
            };
            let instructions = format!("Unlock {name} to use the password of {named}.");
            let response = io
                .prompt(AuthPromptInfo {
                    name: Some(format!("Unlock {name}")),
                    purpose: Some(AuthPromptPurpose::VaultUnlock),
                    instructions: Some(match &problem {
                        Some(problem) => format!("{problem}\n\n{instructions}"),
                        None => instructions,
                    }),
                    prompts: vec![AuthPromptField {
                        prompt: prompt.to_owned(),
                        echo: false,
                    }],
                    skip_label: Some("Use another password".to_owned()),
                    ..AuthPromptInfo::default()
                })
                .await?;
            if response.skipped == Some(true) {
                return Ok(None);
            }
            let answer = response.answers.first().map(String::as_str).unwrap_or("");
            match provider.unlock(answer).await {
                Ok(()) => problem = None,
                Err(error) if error.problem == PasswordManagerProblem::InvalidUnlock => {
                    problem = Some(error.message);
                }
                Err(error) => {
                    io.status(
                        &format!(
                            "{name} could not be unlocked: {} Enter the password.",
                            error.message
                        ),
                        false,
                    );
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }

    /// Drop every provider, and with them what unlocked their managers.
    pub fn dispose(&self) {
        let mut providers = self
            .providers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for held in providers.values() {
            held.provider.dispose();
        }
        providers.clear();
    }

    #[allow(clippy::type_complexity)]
    fn require(
        &self,
        id: &str,
    ) -> Result<
        (
            StoredSource,
            PasswordManagerKind,
            Arc<dyn PasswordManagerProvider>,
        ),
        PasswordManagersError,
    > {
        let row = self
            .store
            .get(id)
            .map_err(PasswordManagersError::Store)?
            .ok_or(PasswordManagersError::SourceMissing)?;
        let kind = known_kind(&row)?;
        let provider = self.provider(&row)?;
        Ok((row, kind, provider))
    }

    /// The source's provider, rebuilt (and so locked again) when its settings changed.
    fn provider(
        &self,
        row: &StoredSource,
    ) -> Result<Arc<dyn PasswordManagerProvider>, PasswordManagerError> {
        let kind = known_kind(row)?;
        let signature = serde_json::json!([row.kind, row.config]).to_string();
        let mut providers = self
            .providers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(held) = providers.get(&row.id) {
            if held.signature == signature {
                return Ok(held.provider.clone());
            }
            held.provider.dispose();
        }
        let provider = (self.factory)(kind, &row.config);
        providers.insert(
            row.id.clone(),
            Held {
                signature,
                provider: provider.clone(),
            },
        );
        Ok(provider)
    }

    fn forget(&self, id: &str) {
        let held = self
            .providers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        if let Some(held) = held {
            held.provider.dispose();
        }
    }

    async fn describe(
        &self,
        row: StoredSource,
        kind: PasswordManagerKind,
    ) -> PasswordManagerSource {
        let provider = self.provider(&row);
        describe(row, kind, provider).await
    }
}

/// The kind a stored source names, when this build has it.
fn known_kind(row: &StoredSource) -> Result<PasswordManagerKind, PasswordManagerError> {
    PasswordManagerKind::parse(&row.kind).ok_or_else(|| {
        PasswordManagerError::failed(format!(
            "Muxus does not know the password manager “{}”.",
            row.kind
        ))
    })
}

/// A stored source with where it stands right now.
async fn describe(
    row: StoredSource,
    kind: PasswordManagerKind,
    provider: Result<Arc<dyn PasswordManagerProvider>, PasswordManagerError>,
) -> PasswordManagerSource {
    let status = match provider {
        Ok(provider) => provider.status().await,
        Err(error) => Err(error),
    };
    let status = status.unwrap_or_else(|error| {
        ProviderStatus::new(PasswordManagerState::Unavailable, error.message)
    });
    PasswordManagerSource {
        id: row.id,
        kind,
        name: row.name,
        config: row.config,
        state: status.state,
        message: status.message.filter(|message| !message.is_empty()),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// Kinds whose command-line program was found on this computer.
fn installed_kinds(host: &HostEnv) -> Vec<PasswordManagerKind> {
    PASSWORD_MANAGER_KINDS
        .into_iter()
        .filter(|kind| {
            let programs = programs(*kind);
            // KeePass needs no program at all.
            programs.is_empty()
                || programs
                    .iter()
                    .any(|program| program_installed(host, program))
        })
        .collect()
}

/// What a newly connected manager is called when no name is given.
fn default_name(kind: PasswordManagerKind, config: &PasswordManagerConfig) -> String {
    // Several KeePass databases are told apart by their file names.
    let file = (kind == PasswordManagerKind::Keepass)
        .then(|| config.path.as_deref().and_then(database_name))
        .flatten();
    match file {
        Some(file) => format!("KeePass – {file}"),
        None => kind.label().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_a_keepass_database_after_its_file() {
        let config = |path: &str| PasswordManagerConfig {
            path: Some(path.to_owned()),
            ..Default::default()
        };
        assert_eq!(
            default_name(PasswordManagerKind::Keepass, &config("~/Secrets/Work.kdbx")),
            "KeePass – Work"
        );
        assert_eq!(
            default_name(
                PasswordManagerKind::Keepass,
                &config("C:\\Users\\me\\Lab.KDBX")
            ),
            "KeePass – Lab"
        );
        assert_eq!(
            default_name(PasswordManagerKind::Keepass, &config("vault.kdb")),
            "KeePass – vault.kdb"
        );
        assert_eq!(
            default_name(PasswordManagerKind::Keepass, &config("~/Secrets/")),
            "KeePass / KeePassXC"
        );
        assert_eq!(
            default_name(
                PasswordManagerKind::Keepass,
                &PasswordManagerConfig::default()
            ),
            "KeePass / KeePassXC"
        );
        assert_eq!(
            default_name(PasswordManagerKind::Bitwarden, &config("ignored.kdbx")),
            "Bitwarden"
        );
    }

    #[test]
    fn keepass_is_always_available_and_programs_are_looked_up() {
        let nowhere = HostEnv::system().with_var("PATH", "");
        assert_eq!(installed_kinds(&nowhere), [PasswordManagerKind::Keepass]);
    }
}
