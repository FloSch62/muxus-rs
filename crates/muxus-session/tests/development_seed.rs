//! A development build's data folder is a copy of the installed app's data, taken while the
//! installed app keeps running (`electron/src/development-database.ts`).

use std::sync::Arc;

use futures_lite::future::block_on;
use muxus_core::api::PasswordVaultUnlockPolicy;
use muxus_session::data_dir::{DEV_VAR, DataLocation, DataSource};
use muxus_session::{Backend, BackendOptions};
use muxus_store::paths::{Environment, Platform};
use muxus_store::vault::{MemoryVaultKeyStore, VaultKeyStore};

#[test]
fn a_development_folder_is_seeded_from_a_running_installed_app() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let config = home.join(".config");
    std::fs::create_dir_all(&config).unwrap();
    let keys = Arc::new(MemoryVaultKeyStore::new());

    // The installed app: its own backend on `$XDG_CONFIG_HOME/Muxus`, still running while the
    // development build starts, so its latest writes sit in the WAL.
    let installed_dir = config.join("Muxus");
    let mut installed_options = BackendOptions::sandbox(&installed_dir, &home);
    installed_options.vault_key_store = Some(keys.clone());
    let installed = Backend::start_blocking(installed_options).unwrap();
    block_on(installed.create_vault(
        "correct horse battery",
        Some(PasswordVaultUnlockPolicy::Never),
    ))
    .unwrap();
    block_on(installed.put_folder_password("Lab", "lab-secret", None)).unwrap();
    let mut prefs = block_on(installed.prefs()).unwrap();
    prefs.sidebar_width = 321.;
    block_on(installed.set_prefs(prefs)).unwrap();
    assert!(installed_dir.join("muxus.sqlite3-wal").exists());

    let env = Environment::new(Platform::Linux, &home)
        .with_var("XDG_CONFIG_HOME", &config)
        .with_var(DEV_VAR, "1");
    let location = DataLocation::resolve(&env, false);
    assert_eq!(location.source, DataSource::Development);
    assert_eq!(
        location.paths.user_data,
        config.join("Muxus-rs-development")
    );
    let mut options = BackendOptions::sandbox(&location.paths.user_data, &home);
    options.data = location.clone();
    options.vault_key_store = Some(keys.clone());
    let development = Backend::start_blocking(options).unwrap();

    // Hosts, folder passwords and preferences came over; the automatic vault opens with the
    // key copied under the development vault id.
    let status = block_on(development.vault_status()).unwrap();
    assert!(status.configured);
    assert!(!status.locked, "the copied automatic key unlocks the vault");
    assert_eq!(status.credential_count, 1);
    let id = &status.credentials[0].id;
    assert_eq!(
        block_on(development.reveal_vault_credential(id, "correct horse battery")).unwrap(),
        "lab-secret"
    );
    assert_eq!(block_on(development.prefs()).unwrap().sidebar_width, 321.);
    let installed_status = block_on(installed.vault_status()).unwrap();
    assert!(!installed_status.locked);

    // The installed key entry is left as it was; the copy lives under its own account.
    let installed_vault_id = rusqlite_vault_id(&installed_dir.join("muxus.sqlite3"));
    let development_vault_id = rusqlite_vault_id(&location.paths.user_data.join("muxus.sqlite3"));
    assert!(development_vault_id.starts_with("development:"));
    assert_ne!(installed_vault_id, development_vault_id);
    assert!(keys.get(&installed_vault_id).unwrap().is_some());
    assert!(keys.get(&development_vault_id).unwrap().is_some());

    // Changes on either side stay there.
    block_on(development.delete_folder_password("Lab")).unwrap();
    assert_eq!(
        block_on(installed.vault_status()).unwrap().credential_count,
        1
    );
    block_on(development.shutdown());
    block_on(installed.shutdown());
}

fn rusqlite_vault_id(database: &std::path::Path) -> String {
    let db =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    db.query_row(
        "SELECT vault_id FROM password_vault WHERE singleton = 1",
        [],
        |row| row.get(0),
    )
    .unwrap()
}
