//! Where the backend keeps its data (port of the data-folder choice in `electron/src/main.ts`
//! and `electron/src/development-database.ts`).
//!
//! - A release build uses the installed Muxus data (`AppPaths::installed`: Electron's
//!   `userData`, `$XDG_CONFIG_HOME/Muxus` on Linux), so both builds share hosts, history and the
//!   vault.
//! - A debug build, or any run with `MUXUS_DEV=1`, uses its own folder,
//!   `<appData>/Muxus-rs-development`, refreshed from the installed data on every start the way
//!   the Electron development build refreshes `Muxus-development`: the database is copied with
//!   SQLite's backup API, its vault gets a development id, an automatic vault's key is copied
//!   under that id, and session history stays in the development folder. The preferences
//!   (`client-state.json`) are refreshed with it, so the sidebar's folder order, colors and
//!   collapsed folders match the hosts they describe.
//! - `MUXUS_DATA_DIR=<dir>` puts the data in `<dir>` and nothing is seeded. Tests and sandboxes
//!   use it (together with a scratch `HOME`), so they never touch a real folder.
//!
//! `MUXUS_HISTORY_PATH` still forces the session history root, as for the TypeScript server.

use std::path::{Path, PathBuf};

use muxus_store::development::{AutomaticVaultKey, DevelopmentDatabaseSeed};
use muxus_store::paths::{AppPaths, Environment, app_data_dir, server_history_override};
use muxus_store::vault::VaultKeyStore;

/// The development data folder's name inside the per-user application data folder.
pub const DEVELOPMENT_FOLDER_NAME: &str = "Muxus-rs-development";
/// Puts the data folder somewhere else; nothing is seeded.
pub const DATA_DIR_VAR: &str = "MUXUS_DATA_DIR";
/// `1` (or `true`) makes a release build use the development folder.
pub const DEV_VAR: &str = "MUXUS_DEV";

/// How the data folder was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataSource {
    /// The installed Muxus data, shared with the TypeScript build.
    Installed,
    /// The development folder, seeded from the installed data.
    Development,
    /// An explicit folder (`MUXUS_DATA_DIR`).
    Override,
}

/// The data folder to use and how it is prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataLocation {
    pub paths: AppPaths,
    pub source: DataSource,
    /// The installed data folder to seed a development folder from.
    pub seed_from: Option<PathBuf>,
    /// A forced session history root (`MUXUS_HISTORY_PATH`, or the development folder's own).
    pub history_root: Option<PathBuf>,
}

fn flag(env: &Environment, key: &str) -> bool {
    env.var(key)
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes"))
}

impl DataLocation {
    /// The data folder for this environment. `debug_build` is `cfg!(debug_assertions)` for the
    /// running binary.
    pub fn resolve(env: &Environment, debug_build: bool) -> Self {
        let forced_history = server_history_override(env);
        if let Some(dir) = env.var(DATA_DIR_VAR) {
            return Self {
                paths: AppPaths::at(PathBuf::from(dir)),
                source: DataSource::Override,
                seed_from: None,
                history_root: forced_history,
            };
        }
        if debug_build || flag(env, DEV_VAR) {
            let paths = AppPaths {
                user_data: app_data_dir(env).join(DEVELOPMENT_FOLDER_NAME),
                development: true,
            };
            let history_root = forced_history.or_else(|| paths.forced_history_root());
            return Self {
                seed_from: Some(AppPaths::installed(env).user_data),
                paths,
                source: DataSource::Development,
                history_root,
            };
        }
        Self {
            paths: AppPaths::installed(env),
            source: DataSource::Installed,
            seed_from: None,
            history_root: forced_history,
        }
    }

    /// For the running process: its environment and build profile.
    pub fn from_process() -> Self {
        Self::resolve(&Environment::from_process(), cfg!(debug_assertions))
    }

    /// Create the data folder (0700) without seeding it, so the shell's log file and the
    /// single-instance lock can be set up before the backend starts.
    pub fn create_folder(&self) -> std::io::Result<()> {
        create_private_dir(&self.paths.user_data)
    }

    /// Create the folder (0700) and, for a development folder, refresh it from the installed
    /// data. Must run before anything opens the database, and only while this process holds
    /// the folder's single-instance lock.
    pub fn prepare(
        &self,
        key_store: Option<&dyn VaultKeyStore>,
    ) -> Result<Option<DevelopmentDatabaseSeed>, String> {
        create_private_dir(&self.paths.user_data).map_err(|error| error.to_string())?;
        let Some(installed) = &self.seed_from else {
            return Ok(None);
        };
        if let Err(error) = seed_client_state(installed, &self.paths.user_data) {
            tracing::warn!("could not copy the installed app's preferences: {error}");
        }
        let seed = muxus_store::development::seed_development_database(
            installed,
            &self.paths.user_data,
            key_store,
        )
        .map_err(|error| error.to_string())?;
        if seed.database_copied {
            tracing::info!("refreshed the development database from the installed app");
        } else {
            tracing::info!("installed app database not found; using the development database");
        }
        if matches!(
            seed.automatic_vault_key,
            AutomaticVaultKey::Missing | AutomaticVaultKey::Unavailable
        ) {
            tracing::warn!(
                "automatic password-vault access could not be copied; the development vault may require repair with its master password"
            );
        }
        Ok(Some(seed))
    }
}

/// Refresh `client-state.json` from the installed folder. The file is written whole by the
/// other app (temp file, then rename), so a plain read is a consistent snapshot; the copy is
/// renamed into place for the same reason. Without an installed file the development
/// folder keeps its own.
fn seed_client_state(installed: &Path, development: &Path) -> std::io::Result<bool> {
    let source = installed.join(muxus_store::paths::CLIENT_STATE_FILENAME);
    let contents = match std::fs::read(&source) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    // A half-written or foreign file must not replace working preferences.
    if serde_json::from_slice::<serde_json::Value>(&contents).is_err() {
        return Ok(false);
    }
    let destination = development.join(muxus_store::paths::CLIENT_STATE_FILENAME);
    let temporary = development.join(format!(
        ".{}.seed-{}",
        muxus_store::paths::CLIENT_STATE_FILENAME,
        std::process::id()
    ));
    std::fs::write(&temporary, &contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&temporary, &destination)?;
    Ok(true)
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use muxus_store::paths::Platform;

    use super::*;

    fn linux() -> Environment {
        Environment::new(Platform::Linux, "/home/u").with_var("XDG_CONFIG_HOME", "/home/u/.cfg")
    }

    #[test]
    fn release_builds_share_the_installed_data() {
        let location = DataLocation::resolve(&linux(), false);
        assert_eq!(location.source, DataSource::Installed);
        assert_eq!(
            location.paths.database(),
            PathBuf::from("/home/u/.cfg/Muxus/muxus.sqlite3")
        );
        assert_eq!(location.seed_from, None);
        assert_eq!(location.history_root, None);
    }

    #[test]
    fn debug_builds_and_muxus_dev_use_a_seeded_development_folder() {
        for (env, debug) in [(linux(), true), (linux().with_var(DEV_VAR, "1"), false)] {
            let location = DataLocation::resolve(&env, debug);
            assert_eq!(location.source, DataSource::Development);
            assert_eq!(
                location.paths.user_data,
                PathBuf::from("/home/u/.cfg/Muxus-rs-development")
            );
            assert_eq!(
                location.seed_from,
                Some(PathBuf::from("/home/u/.cfg/Muxus"))
            );
            assert_eq!(
                location.history_root,
                Some(PathBuf::from("/home/u/.cfg/Muxus-rs-development/history"))
            );
        }
        let off = DataLocation::resolve(&linux().with_var(DEV_VAR, "0"), false);
        assert_eq!(off.source, DataSource::Installed);
    }

    #[test]
    fn an_explicit_data_dir_wins_and_is_never_seeded() {
        let env = linux()
            .with_var(DATA_DIR_VAR, "/scratch/data")
            .with_var("MUXUS_HISTORY_PATH", "/scratch/history");
        let location = DataLocation::resolve(&env, true);
        assert_eq!(location.source, DataSource::Override);
        assert_eq!(
            location.paths.database(),
            PathBuf::from("/scratch/data/muxus.sqlite3")
        );
        assert_eq!(location.seed_from, None);
        assert_eq!(
            location.history_root,
            Some(PathBuf::from("/scratch/history"))
        );
    }

    #[test]
    fn preparing_a_development_folder_copies_the_installed_database() {
        let scratch = tempfile::tempdir().unwrap();
        let env = Environment::new(Platform::Linux, scratch.path())
            .with_var("XDG_CONFIG_HOME", scratch.path().join("config"));
        let installed = AppPaths::installed(&env);
        std::fs::create_dir_all(&installed.user_data).unwrap();
        {
            let db = muxus_store::Database::open(installed.database()).unwrap();
            db.record_open_ssh_connection("seeded-host").unwrap();
        }
        let location = DataLocation::resolve(&env, true);
        let keys = muxus_store::vault::MemoryVaultKeyStore::new();
        let seed = location.prepare(Some(&keys)).unwrap().unwrap();
        assert!(seed.database_copied);
        let copy = muxus_store::Database::open(location.paths.database()).unwrap();
        let metadata = copy.open_ssh_metadata(&["seeded-host"]).unwrap();
        assert!(metadata.iter().any(|(alias, _)| alias == "seeded-host"));
    }
}
