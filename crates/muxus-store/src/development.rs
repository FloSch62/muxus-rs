//! Development builds' own data (port of `electron/src/development-database.ts`).
//!
//! A development build keeps its data in `<userData>-development` so source
//! runs never touch the installed app's data. On each start it refreshes its
//! database from the installed one, then isolates the copy: session history
//! goes to development storage, and the vault gets its own id (vault ids
//! double as OS credential-store account names), with an automatic vault's
//! key copied under the new id.

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::db::{DbError, DbResult};
use crate::paths::DATABASE_FILENAME;
use crate::vault::VaultKeyStore;

const DEVELOPMENT_VAULT_PREFIX: &str = "development:";

pub use crate::paths::development_user_data_path;

/// What a seed did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevelopmentDatabaseSeed {
    /// The installed database existed and was copied.
    pub database_copied: bool,
    pub automatic_vault_key: AutomaticVaultKey,
}

/// What happened to an automatic ("never" policy) vault's OS key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomaticVaultKey {
    NotNeeded,
    Copied,
    Missing,
    Unavailable,
}

struct VaultKeyRemap {
    source_vault_id: String,
    development_vault_id: String,
    automatic: bool,
}

/// The development vault id for an installed vault id.
pub fn development_vault_id(installed_vault_id: &str) -> String {
    let digest = Sha256::digest(installed_vault_id.as_bytes());
    format!(
        "{DEVELOPMENT_VAULT_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(digest)
    )
}

/// Refresh the development database from the installed application database.
/// SQLite's backup API is used, since copying the main file directly can omit
/// committed transactions which still live in its WAL file. Must run before
/// the development database is opened by anything else.
pub fn seed_development_database(
    installed_user_data: &Path,
    development_user_data: &Path,
    key_store: Option<&dyn VaultKeyStore>,
) -> DbResult<DevelopmentDatabaseSeed> {
    let source_path = installed_user_data.join(DATABASE_FILENAME);
    let destination_path = development_user_data.join(DATABASE_FILENAME);
    if absolute(&source_path) == absolute(&destination_path) {
        return Err(DbError::Invalid(
            "development database path must differ from the installed database path".into(),
        ));
    }
    std::fs::create_dir_all(development_user_data)?;
    if !source_path.exists() {
        let remap = if destination_path.exists() {
            isolate_development_data(&destination_path, false)?
        } else {
            None
        };
        return Ok(DevelopmentDatabaseSeed {
            database_copied: false,
            automatic_vault_key: copy_automatic_vault_key(remap.as_ref(), key_store),
        });
    }

    let temporary = tempdir_in(development_user_data)?;
    let temporary_database = temporary.join(DATABASE_FILENAME);
    let result = (|| {
        {
            let source =
                Connection::open_with_flags(&source_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            source.busy_timeout(std::time::Duration::from_secs(5))?;
            let mut destination = Connection::open(&temporary_database)?;
            let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
            backup.run_to_completion(i32::MAX, std::time::Duration::ZERO, None)?;
        }
        let remap = isolate_development_data(&temporary_database, true)?;
        let automatic_vault_key = copy_automatic_vault_key(remap.as_ref(), key_store);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &temporary_database,
                std::fs::Permissions::from_mode(0o600),
            );
        }
        // Discard sidecars from the previous seed before atomically replacing
        // its main database file.
        let _ = std::fs::remove_file(sidecar(&destination_path, "-wal"));
        let _ = std::fs::remove_file(sidecar(&destination_path, "-shm"));
        std::fs::rename(&temporary_database, &destination_path)?;
        Ok(DevelopmentDatabaseSeed {
            database_copied: true,
            automatic_vault_key,
        })
    })();
    let _ = std::fs::remove_dir_all(&temporary);
    result
}

fn absolute(path: &Path) -> std::path::PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn tempdir_in(parent: &Path) -> DbResult<std::path::PathBuf> {
    for _ in 0..16 {
        let candidate = parent.join(format!(".database-seed-{}", crate::ids::nanoid()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(DbError::Invalid(
        "could not create a temporary seed directory".into(),
    ))
}

fn table_exists(conn: &Connection, table: &str) -> DbResult<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn isolate_development_data(
    database: &Path,
    copied_from_installed_app: bool,
) -> DbResult<Option<VaultKeyRemap>> {
    let mut conn = Connection::open(database)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // Ensure the changes below live in the main file before it is renamed.
    conn.execute_batch("PRAGMA journal_mode = DELETE;")?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if table_exists(&tx, "session_history_settings")? {
        tx.execute_batch("UPDATE session_history_settings SET storage_location = NULL;")?;
    }
    if table_exists(&tx, "password_vault_key_cleanup")? {
        if copied_from_installed_app {
            tx.execute_batch("DELETE FROM password_vault_key_cleanup;")?;
        } else {
            tx.execute(
                "DELETE FROM password_vault_key_cleanup WHERE vault_id NOT LIKE ?",
                [format!("{DEVELOPMENT_VAULT_PREFIX}%")],
            )?;
        }
    }
    let mut remap = None;
    if table_exists(&tx, "password_vault")? {
        let vault: Option<(String, String)> = tx
            .query_row(
                "SELECT vault_id, unlock_policy FROM password_vault WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((source_vault_id, unlock_policy)) = vault
            && (copied_from_installed_app || !source_vault_id.starts_with(DEVELOPMENT_VAULT_PREFIX))
        {
            let next = development_vault_id(&source_vault_id);
            tx.execute(
                "UPDATE password_vault SET vault_id = ? WHERE singleton = 1",
                params![next],
            )?;
            remap = Some(VaultKeyRemap {
                source_vault_id,
                development_vault_id: next,
                automatic: unlock_policy == "never",
            });
        }
    }
    tx.commit()?;
    Ok(remap)
}

fn copy_automatic_vault_key(
    remap: Option<&VaultKeyRemap>,
    key_store: Option<&dyn VaultKeyStore>,
) -> AutomaticVaultKey {
    let (Some(remap), Some(key_store)) = (remap.filter(|remap| remap.automatic), key_store) else {
        return AutomaticVaultKey::NotNeeded;
    };
    match key_store.get(&remap.source_vault_id) {
        Ok(Some(key)) => match key_store.set(&remap.development_vault_id, &key) {
            Ok(()) => AutomaticVaultKey::Copied,
            Err(_) => AutomaticVaultKey::Unavailable,
        },
        Ok(None) => AutomaticVaultKey::Missing,
        Err(_) => AutomaticVaultKey::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::vault::MemoryVaultKeyStore;

    fn directories() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("Muxus");
        let development = development_user_data_path(&installed);
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::create_dir_all(&development).unwrap();
        (dir, installed, development)
    }

    fn open(directory: &Path) -> Connection {
        Connection::open(directory.join(DATABASE_FILENAME)).unwrap()
    }

    fn values(conn: &Connection, sql: &str) -> Vec<Option<String>> {
        let mut statement = conn.prepare(sql).unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn uses_a_sibling_user_data_directory() {
        assert_eq!(
            development_user_data_path(Path::new("/profiles/Muxus")),
            PathBuf::from("/profiles/Muxus-development")
        );
    }

    #[test]
    fn copies_committed_wal_data_from_the_installed_database() {
        let (_dir, installed, development) = directories();
        let source = open(&installed);
        source
            .execute_batch(
                "PRAGMA journal_mode = WAL;
      CREATE TABLE example(value TEXT NOT NULL);
      INSERT INTO example VALUES ('installed');",
            )
            .unwrap();
        let seed = seed_development_database(&installed, &development, None).unwrap();
        drop(source);
        assert!(seed.database_copied);
        let copy = open(&development);
        assert_eq!(
            values(&copy, "SELECT value FROM example"),
            [Some("installed".into())]
        );
        assert_eq!(values(&copy, "PRAGMA integrity_check"), [Some("ok".into())]);
    }

    #[test]
    fn refreshes_an_existing_development_database_on_every_seed() {
        let (_dir, installed, development) = directories();
        open(&installed)
            .execute_batch(
                "CREATE TABLE example(value TEXT NOT NULL); INSERT INTO example VALUES ('first');",
            )
            .unwrap();
        seed_development_database(&installed, &development, None).unwrap();
        open(&development)
            .execute_batch("INSERT INTO example VALUES ('development-only');")
            .unwrap();
        open(&installed)
            .execute_batch("DELETE FROM example; INSERT INTO example VALUES ('second');")
            .unwrap();
        seed_development_database(&installed, &development, None).unwrap();
        assert_eq!(
            values(&open(&development), "SELECT value FROM example"),
            [Some("second".into())]
        );
    }

    #[test]
    fn keeps_the_development_database_when_no_installed_database_exists() {
        let (_dir, installed, development) = directories();
        open(&development)
            .execute_batch(
                "CREATE TABLE example(value TEXT NOT NULL); INSERT INTO example VALUES ('kept');",
            )
            .unwrap();
        let seed = seed_development_database(&installed, &development, None).unwrap();
        assert!(!seed.database_copied);
        assert_eq!(
            values(&open(&development), "SELECT value FROM example"),
            [Some("kept".into())]
        );
    }

    #[test]
    fn forces_copied_session_history_into_development_storage() {
        let (_dir, installed, development) = directories();
        open(&installed)
            .execute_batch(
                "CREATE TABLE session_history_settings(singleton INTEGER PRIMARY KEY, storage_location TEXT);
      INSERT INTO session_history_settings VALUES (1, '/production/history');",
            )
            .unwrap();
        seed_development_database(&installed, &development, None).unwrap();
        assert_eq!(
            values(
                &open(&development),
                "SELECT storage_location FROM session_history_settings"
            ),
            [None]
        );
    }

    #[test]
    fn namespaces_automatic_vault_keys_and_drops_copied_cleanup_work() {
        let (_dir, installed, development) = directories();
        let key_store = MemoryVaultKeyStore::new();
        key_store.set("production-vault-id", &[0x5a; 32]).unwrap();
        open(&installed)
            .execute_batch(
                "CREATE TABLE password_vault(singleton INTEGER PRIMARY KEY, vault_id TEXT NOT NULL, unlock_policy TEXT NOT NULL);
      CREATE TABLE password_vault_key_cleanup(vault_id TEXT PRIMARY KEY);
      INSERT INTO password_vault VALUES (1, 'production-vault-id', 'never');
      INSERT INTO password_vault_key_cleanup VALUES ('production-old-vault');",
            )
            .unwrap();
        let seed = seed_development_database(&installed, &development, Some(&key_store)).unwrap();
        assert_eq!(
            seed,
            DevelopmentDatabaseSeed {
                database_copied: true,
                automatic_vault_key: AutomaticVaultKey::Copied
            }
        );
        let namespaced = development_vault_id("production-vault-id");
        assert!(namespaced.starts_with("development:"));
        let copy = open(&development);
        assert_eq!(
            values(&copy, "SELECT vault_id FROM password_vault"),
            [Some(namespaced.clone())]
        );
        assert!(values(&copy, "SELECT vault_id FROM password_vault_key_cleanup").is_empty());
        assert_eq!(
            key_store
                .get("production-vault-id")
                .unwrap()
                .unwrap()
                .as_slice(),
            &[0x5a; 32]
        );
        assert_eq!(
            key_store.get(&namespaced).unwrap().unwrap().as_slice(),
            &[0x5a; 32]
        );
    }

    #[test]
    fn matches_the_typescript_development_vault_id() {
        // createHash('sha256').update('production-vault-id').digest('base64url')
        assert_eq!(
            development_vault_id("production-vault-id"),
            "development:RYD2KeKbzmj-TtNUHBJUV7jyKt9uBSZPrhiY3JKOPC4"
        );
    }
}
