//! The application database (port of `server/src/persistence/database.ts`).
//!
//! One SQLite file, `muxus.sqlite3`, shared with the TypeScript build: the
//! same schema (created by the same 29 migrations, whose SQL text is copied
//! verbatim so a fresh file is identical), the same pragmas, the same row
//! semantics and JSON columns. A database written by a newer build is refused
//! with [`DbError::NewerSchema`], exactly as the TypeScript refuses it, so this
//! build never runs a migration Muxus does not know.
//!
//! [`Database`] is `Send + Sync` and synchronous; it serializes access through
//! one connection behind a mutex, like the single `DatabaseSync` of the
//! TypeScript server. Callers on an async runtime run its methods on a
//! blocking thread.

mod credentials;
mod folders;
mod hosts;
mod logging;
mod migrate;
mod migration_sql;
mod secrets;
mod tunnels;
mod workspaces;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, TransactionBehavior};

pub use credentials::{
    CredentialRefInput, CredentialRefRecord, EncryptedCredentialInput, EncryptedCredentialRecord,
    EncryptedCredentialSecret, PasswordVaultConfig, PasswordVaultConfigRecord,
};
pub use folders::{FolderMove, FolderSettingsRow, merge_folder_auth};
pub use hosts::{
    DesktopIdentity, HostLoginSequence, HostRef, NativeConnectionInput, NativeConnectionKind,
};
pub use logging::DEFAULT_SESSION_LOGGING_POLICY;
pub use migrate::{AppliedMigration, SUPPORTED_SCHEMA_VERSION};
pub use secrets::assert_secret_free;
pub use workspaces::{DEFAULT_SNAPSHOT_GRACE_SECONDS, StoredWorkspace, WorkspaceInput};

/// Everything that can go wrong in the database layer. The messages match the
/// TypeScript's, since routes show them to the user.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The file was written by a newer Muxus; nothing was changed.
    #[error("database schema {found} is newer than this Muxus build supports ({supported})")]
    NewerSchema { found: i64, supported: i64 },
    /// A required value was empty (`<name> is required`) or a value was out of range.
    #[error("{0}")]
    Invalid(String),
    /// A secret was about to be stored outside the vault (`assertSecretFree`).
    #[error("{0}")]
    Secret(String),
    /// The record an operation needs does not exist (`saved host not found`).
    #[error("{0}")]
    NotFound(String),
    /// `workspace "<id>" is locked; use an explicit save to overwrite it`.
    #[error("workspace \"{0}\" is locked; use an explicit save to overwrite it")]
    WorkspaceLocked(String),
}

pub type DbResult<T> = Result<T, DbError>;

/// The application database.
#[derive(Debug)]
pub struct Database {
    conn: Mutex<Connection>,
    /// `None` for an in-memory database (`:memory:` in the TypeScript).
    filename: Option<PathBuf>,
}

impl Database {
    /// Open (creating when missing) the database file and bring its schema up
    /// to date. The parent folder is created with mode 0700 and the file is
    /// made 0600 where the platform allows it.
    pub fn open(path: impl AsRef<Path>) -> DbResult<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            create_private_dir(parent)?;
        }
        let conn = Connection::open(path)?;
        restrict_file_mode(path);
        Self::init(conn, Some(path.to_path_buf()))
    }

    /// A private in-memory database (tests, ephemeral sessions).
    pub fn open_in_memory() -> DbResult<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(conn: Connection, filename: Option<PathBuf>) -> DbResult<Self> {
        conn.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000; PRAGMA secure_delete = ON;",
        )?;
        if filename.is_some() {
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        }
        migrate::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            filename,
        })
    }

    /// The database file, or `None` for an in-memory database.
    pub fn filename(&self) -> Option<&Path> {
        self.filename.as_deref()
    }

    /// Close the connection (also happens on drop).
    pub fn close(self) -> DbResult<()> {
        let conn = self
            .conn
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.close().map_err(|(_, error)| DbError::Sqlite(error))
    }

    /// Every migration recorded in the file, oldest first.
    pub fn applied_migrations(&self) -> DbResult<Vec<AppliedMigration>> {
        migrate::applied_migrations(&self.lock())
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Run `f` inside `BEGIN IMMEDIATE … COMMIT`, rolling back on error.
    pub(crate) fn immediate<T>(&self, f: impl FnOnce(&Connection) -> DbResult<T>) -> DbResult<T> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    /// Push deleted secrets out of the WAL (and, when compacting, out of free
    /// pages) right away instead of at the next checkpoint.
    pub(crate) fn flush_sensitive_pages(&self, conn: &Connection, compact: bool) -> DbResult<()> {
        if self.filename.is_none() {
            return Ok(());
        }
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        if compact {
            conn.execute_batch("VACUUM")?;
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        }
        Ok(())
    }
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

fn restrict_file_mode(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Permissions may be controlled by the platform/filesystem.
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// `requireNonEmpty`: `<name> is required` for a blank value.
pub(crate) fn require_non_empty(value: &str, name: &str) -> DbResult<()> {
    if muxus_core::js::js_trim(value).is_empty() {
        return Err(DbError::Invalid(format!("{name} is required")));
    }
    Ok(())
}

/// `optionalString`: a non-empty text column, else `None`.
pub(crate) fn optional_string(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// The trimmed value, or `None` when blank (`value?.trim() || null`).
pub(crate) fn trimmed_or_none(value: Option<&str>) -> Option<String> {
    value
        .map(muxus_core::js::js_trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
