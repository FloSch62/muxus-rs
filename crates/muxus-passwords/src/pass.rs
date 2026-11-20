//! The standard Unix password store (`pass.ts`).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use muxus_core::api::{
    PasswordManagerConfig, PasswordManagerEntry, PasswordManagerKind, PasswordManagerState,
};

use crate::cli::{APPROVAL_TIMEOUT, CliOptions, cli_message, resolve_program, run_cli};
use crate::host::{HostEnv, resolve_path};
use crate::provider::{
    PasswordManagerError, PasswordManagerProvider, PasswordManagerResult, ProviderStatus,
    sort_entries, trimmed,
};

const MAX_DEPTH: usize = 12;
const NO_GPG: &str = "GnuPG (gpg) was not found on this computer.";

/// The standard Unix password store (`pass`, and `gopass` with its default GPG backend): a
/// directory of files, each one entry encrypted with GnuPG, the password on its first
/// line. Muxus lists the files and has `gpg` decrypt the one it needs, so the passphrase
/// prompt stays with gpg-agent's pinentry.
pub struct PassProvider {
    store: PathBuf,
    program: Option<String>,
    host: HostEnv,
}

impl PassProvider {
    pub fn new(config: &PasswordManagerConfig, host: &HostEnv) -> Self {
        let configured = trimmed(config.path.as_deref())
            .or_else(|| host.var("PASSWORD_STORE_DIR"))
            .unwrap_or_else(|| {
                host.home()
                    .join(".password-store")
                    .to_string_lossy()
                    .into_owned()
            });
        let store = PathBuf::from(host.expand_home(&configured));
        Self {
            store: resolve_path(Path::new(""), &store),
            program: trimmed(config.program.as_deref()),
            host: host.clone(),
        }
    }

    fn store_missing(&self) -> String {
        format!("The password store {} was not found.", self.store.display())
    }

    fn gpg(&self) -> Option<String> {
        let candidates: Vec<&str> = match &self.program {
            Some(program) => vec![program],
            None => vec!["gpg2", "gpg"],
        };
        candidates
            .into_iter()
            .find(|candidate| resolve_program(&self.host, candidate).is_some())
            .map(str::to_owned)
    }

    /// The entries below `directory`, which is `depth` folders into the store.
    fn walk(
        &self,
        directory: &Path,
        depth: usize,
        entries: &mut Vec<PasswordManagerEntry>,
    ) -> PasswordManagerResult<()> {
        let children = match std::fs::read_dir(directory) {
            Ok(children) => children,
            Err(_) if depth == 0 => {
                return Err(PasswordManagerError::unavailable(self.store_missing()));
            }
            Err(_) => return Ok(()),
        };
        for child in children.flatten() {
            let file_name = child.file_name();
            let name = file_name.to_string_lossy();
            // .git, .gpg-id, .extensions and the like are not entries.
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind) = child.file_type() else {
                continue;
            };
            let full = child.path();
            if kind.is_dir() {
                if depth < MAX_DEPTH {
                    self.walk(&full, depth + 1, entries)?;
                }
            } else if kind.is_file()
                && name.ends_with(".gpg")
                && let Ok(relative) = full.strip_prefix(&self.store)
            {
                let parts: Vec<String> = relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect();
                let joined = parts.join("/");
                let id = &joined[..joined.len() - ".gpg".len()];
                let (group, title) = match id.rsplit_once('/') {
                    Some((group, title)) => (Some(group.to_owned()), title),
                    None => (None, id),
                };
                entries.push(PasswordManagerEntry {
                    id: id.to_owned(),
                    title: title.to_owned(),
                    group,
                    ..Default::default()
                });
            }
        }
        Ok(())
    }
}

#[async_trait]
impl PasswordManagerProvider for PassProvider {
    fn kind(&self) -> PasswordManagerKind {
        PasswordManagerKind::Pass
    }

    async fn status(&self) -> PasswordManagerResult<ProviderStatus> {
        let is_directory = tokio::fs::metadata(&self.store)
            .await
            .is_ok_and(|info| info.is_dir());
        if !is_directory {
            return Ok(ProviderStatus::new(
                PasswordManagerState::Unavailable,
                self.store_missing(),
            ));
        }
        Ok(if self.gpg().is_some() {
            ProviderStatus::ready()
        } else {
            ProviderStatus::new(PasswordManagerState::Unavailable, NO_GPG)
        })
    }

    async fn entries(&self) -> PasswordManagerResult<Vec<PasswordManagerEntry>> {
        let mut entries = Vec::new();
        self.walk(&self.store, 0, &mut entries)?;
        Ok(sort_entries(entries))
    }

    async fn password(&self, entry_id: &str) -> PasswordManagerResult<String> {
        let file = resolve_path(&self.store, Path::new(&format!("{entry_id}.gpg")));
        // An entry names a file inside the store and nothing above it.
        let inside = file != self.store && file.starts_with(&self.store);
        if !inside || entry_id.split('/').any(|part| part.starts_with('.')) {
            return Err(PasswordManagerError::entry_missing(
                "The password store entry reference is not valid.",
            ));
        }
        if tokio::fs::metadata(&file).await.is_err() {
            return Err(PasswordManagerError::entry_missing(format!(
                "“{entry_id}” is no longer in the password store."
            )));
        }
        let Some(gpg) = self.gpg() else {
            return Err(PasswordManagerError::unavailable(NO_GPG));
        };
        // --batch keeps gpg off the terminal; the passphrase still comes from gpg-agent's
        // own pinentry window.
        let file = file.to_string_lossy();
        let result = run_cli(
            &self.host,
            &gpg,
            &["--quiet", "--batch", "--yes", "--decrypt", file.as_ref()],
            CliOptions::timeout(APPROVAL_TIMEOUT),
        )
        .await?;
        if result.code != 0 {
            return Err(PasswordManagerError::failed(format!(
                "gpg could not decrypt “{entry_id}”: {}",
                cli_message(&result, "decryption failed")
            )));
        }
        // The password is the first line; the rest is notes and other fields.
        let first = result.stdout.split('\n').next().unwrap_or("");
        Ok(first.strip_suffix('\r').unwrap_or(first).to_owned())
    }
}
