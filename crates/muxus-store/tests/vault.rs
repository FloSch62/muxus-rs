//! Port of tests/unit/server/password-vault.test.ts, the storage parts of
//! vault-secret-input.test.ts, and interoperability with vaults the
//! TypeScript build created.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aes_gcm::aead::{AeadInOut, KeyInit};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use muxus_core::api::PasswordVaultUnlockPolicy::{self, Credential, Never, Startup};
use muxus_store::Database;
use muxus_store::db::{
    CredentialRefInput, EncryptedCredentialInput, EncryptedCredentialSecret, PasswordVaultConfig,
};
use muxus_store::vault::secret_input::{secret_keystrokes, type_vault_secret};
use muxus_store::vault::{
    KeyStoreBackend, KeyStoreUnavailable, MemoryVaultKeyStore, PasswordVault, PasswordVaultKdf,
    SecretInput, VaultError, VaultKeyStore, VaultOptions, ssh_password_account,
};
use serde_json::Value;
use zeroize::Zeroizing;

const MASTER: &str = "master-pass-12";
const NEXT_MASTER: &str = "next-master-12";
const REMOTE_PASSWORD: &str = "remote-password-that-must-not-leak";
const TEST_KDF: PasswordVaultKdf = PasswordVaultKdf {
    cost: 1024,
    block_size: 8,
    parallelism: 1,
};

fn account() -> String {
    ssh_password_account("alice", "router.example", 22)
}

fn setup_with(
    database: Database,
    key_store: Arc<dyn VaultKeyStore>,
) -> (Arc<Database>, PasswordVault) {
    let database = Arc::new(database);
    let vault = PasswordVault::new(
        database.clone(),
        VaultOptions {
            kdf: Some(TEST_KDF),
            key_store: Some(key_store),
        },
    );
    vault.initialize().unwrap();
    (database, vault)
}

fn setup() -> (Arc<Database>, PasswordVault, Arc<MemoryVaultKeyStore>) {
    let key_store = Arc::new(MemoryVaultKeyStore::new());
    let (database, vault) = setup_with(Database::open_in_memory().unwrap(), key_store.clone());
    (database, vault, key_store)
}

fn reveal(value: VaultResultString) -> Option<String> {
    value.unwrap().map(|value| value.to_string())
}

type VaultResultString = Result<Option<Zeroizing<String>>, VaultError>;

/// A memory store that can be made unavailable, like a locked keyring.
#[derive(Default)]
struct ToggleKeyStore {
    inner: MemoryVaultKeyStore,
    unavailable: AtomicBool,
}

impl ToggleKeyStore {
    fn check(&self) -> Result<(), KeyStoreUnavailable> {
        if self.unavailable.load(Ordering::SeqCst) {
            Err(KeyStoreUnavailable::new("locked"))
        } else {
            Ok(())
        }
    }
}

impl VaultKeyStore for ToggleKeyStore {
    fn backend(&self) -> KeyStoreBackend {
        KeyStoreBackend::Memory
    }
    fn get(&self, vault_id: &str) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreUnavailable> {
        self.check()?;
        self.inner.get(vault_id)
    }
    fn set(&self, vault_id: &str, key: &[u8]) -> Result<(), KeyStoreUnavailable> {
        self.check()?;
        self.inner.set(vault_id, key)
    }
    fn delete(&self, vault_id: &str) -> Result<(), KeyStoreUnavailable> {
        self.check()?;
        self.inner.delete(vault_id)
    }
}

#[test]
fn uses_the_os_keyring_policy_while_the_master_password_gates_management() {
    let (database, store, _) = setup();
    store.create(MASTER, Some(Never)).unwrap();
    store
        .remember_ssh_password(&account(), "alice@router.example:22", REMOTE_PASSWORD, None)
        .unwrap();
    let status = store.status().unwrap();
    assert!(status.configured && !status.locked);
    assert_eq!(
        (status.unlock_policy, status.credential_count),
        (Some(Never), 1)
    );
    assert_eq!(
        database
            .password_vault_config()
            .unwrap()
            .unwrap()
            .config
            .key_check
            .unwrap()
            .len(),
        32
    );
    assert_eq!(
        reveal(store.ssh_password(&account(), None)).as_deref(),
        Some(REMOTE_PASSWORD)
    );

    let credential = status.credentials[0].clone();
    assert!(matches!(
        store.reveal_credential(&credential.id, "incorrect-pass"),
        Err(VaultError::InvalidMasterPassword)
    ));
    assert_eq!(
        reveal(store.reveal_credential(&credential.id, MASTER)).as_deref(),
        Some(REMOTE_PASSWORD)
    );
    assert!(
        store
            .update_credential(&credential.id, MASTER, "changed-password")
            .unwrap()
    );
    store.change_master_password(MASTER, NEXT_MASTER).unwrap();
    assert!(matches!(
        store.reveal_credential(&credential.id, MASTER),
        Err(VaultError::InvalidMasterPassword)
    ));
    assert_eq!(
        reveal(store.reveal_credential(&credential.id, NEXT_MASTER)).as_deref(),
        Some("changed-password")
    );
}

#[test]
fn implements_startup_and_per_credential_prompt_policies() {
    let (_, store, _) = setup();
    store.create(MASTER, Some(Startup)).unwrap();
    store
        .remember_ssh_password(&account(), "alice@router.example:22", REMOTE_PASSWORD, None)
        .unwrap();
    store.lock();
    let status = store.status().unwrap();
    assert_eq!((status.unlock_policy, status.locked), (Some(Startup), true));
    assert!(matches!(
        store.ssh_password(&account(), None),
        Err(VaultError::UnlockRequired(Startup))
    ));
    store.unlock_for_session(MASTER).unwrap();
    assert_eq!(
        reveal(store.ssh_password(&account(), None)).as_deref(),
        Some(REMOTE_PASSWORD)
    );

    store.change_unlock_policy(MASTER, Credential).unwrap();
    let status = store.status().unwrap();
    assert_eq!(
        (status.unlock_policy, status.locked),
        (Some(Credential), true)
    );
    assert!(
        store
            .ssh_password(&account(), None)
            .unwrap_err()
            .is_unlock_required()
    );
    assert_eq!(
        reveal(store.ssh_password(&account(), Some(MASTER))).as_deref(),
        Some(REMOTE_PASSWORD)
    );
    assert!(store.status().unwrap().locked);

    store.change_unlock_policy(MASTER, Never).unwrap();
    let status = store.status().unwrap();
    assert_eq!((status.unlock_policy, status.locked), (Some(Never), false));
}

fn seal_for_test(plaintext: &[u8], key: &[u8; 32], aad: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(key).unwrap();
    let nonce = vec![13u8; 12];
    let mut buffer = plaintext.to_vec();
    let tag = cipher
        .encrypt_inout_detached(
            &aes_gcm::Nonce::try_from(nonce.as_slice()).unwrap(),
            aad,
            buffer.as_mut_slice().into(),
        )
        .unwrap();
    (nonce, buffer, tag.to_vec())
}

#[test]
fn unlocks_a_draft_v13_vault_and_upgrades_it_on_master_password_change() {
    let (database, store, _) = setup();
    let legacy_master = "old-pass";
    let vault_key = [11u8; 32];
    let salt = vec![12u8; 16];
    let mut master_key = [0u8; 32];
    scrypt::scrypt(
        legacy_master.as_bytes(),
        &salt,
        &scrypt::Params::new(10, 8, 1).unwrap(),
        &mut master_key,
    )
    .unwrap();
    let (nonce, ciphertext, tag) = seal_for_test(
        &vault_key,
        &master_key,
        b"muxus:password-vault:master-key:v2",
    );
    database
        .create_password_vault_config(&PasswordVaultConfig {
            format_version: 2,
            vault_id: "legacy-vault-id-12345".into(),
            unlock_policy: Startup,
            kdf_algorithm: "scrypt".into(),
            kdf_salt: salt,
            kdf_cost: 1024,
            kdf_block_size: 8,
            kdf_parallelism: 1,
            master_key_nonce: nonce,
            master_key_ciphertext: ciphertext,
            master_key_tag: tag,
            key_check: None,
        })
        .unwrap();
    let reference = database
        .upsert_credential_ref(&CredentialRefInput {
            provider: "muxus-master-vault".into(),
            service: "muxus/ssh-password/v1".into(),
            account: account(),
            label: Some("legacy credential".into()),
            username: None,
        })
        .unwrap();
    let aad = format!("muxus:ssh-password:{}:v1", reference.id);
    let (nonce, ciphertext, auth_tag) =
        seal_for_test(REMOTE_PASSWORD.as_bytes(), &vault_key, aad.as_bytes());
    database
        .upsert_encrypted_credential(&EncryptedCredentialInput {
            reference: CredentialRefInput {
                provider: reference.provider,
                service: reference.service,
                account: reference.account,
                label: reference.label,
                username: None,
            },
            secret: EncryptedCredentialSecret {
                format_version: 1,
                nonce,
                ciphertext,
                auth_tag,
            },
        })
        .unwrap();

    store.unlock_for_session(legacy_master).unwrap();
    assert_eq!(
        reveal(store.ssh_password(&account(), None)).as_deref(),
        Some(REMOTE_PASSWORD)
    );
    store
        .change_master_password(legacy_master, NEXT_MASTER)
        .unwrap();
    assert_eq!(
        database
            .password_vault_config()
            .unwrap()
            .unwrap()
            .config
            .format_version,
        3
    );
    store.lock();
    store.unlock_for_session(NEXT_MASTER).unwrap();
    assert_eq!(
        reveal(store.ssh_password(&account(), None)).as_deref(),
        Some(REMOTE_PASSWORD)
    );
}

#[test]
fn restores_never_prompt_access_after_a_complete_application_restart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("muxus.sqlite3");
    let key_store = Arc::new(MemoryVaultKeyStore::new());
    {
        let (_, first) = setup_with(Database::open(&file).unwrap(), key_store.clone());
        first.create(MASTER, Some(Never)).unwrap();
        first
            .remember_ssh_password(&account(), "alice@router.example:22", REMOTE_PASSWORD, None)
            .unwrap();
        first.dispose();
    }
    let (_, second) = setup_with(Database::open(&file).unwrap(), key_store);
    let status = second.status().unwrap();
    assert_eq!((status.unlock_policy, status.locked), (Some(Never), false));
    assert_eq!(
        reveal(second.ssh_password(&account(), None)).as_deref(),
        Some(REMOTE_PASSWORD)
    );
    assert!(!dir.path().join("muxus-vault-device.key").exists());
}

#[test]
fn repairs_a_missing_os_keyring_entry_with_the_master_password() {
    let (database, store, key_store) = setup();
    store.create(MASTER, Some(Never)).unwrap();
    store
        .remember_ssh_password(&account(), "alice@router.example:22", REMOTE_PASSWORD, None)
        .unwrap();
    let vault_id = database
        .password_vault_config()
        .unwrap()
        .unwrap()
        .config
        .vault_id;
    store.lock();
    key_store.delete(&vault_id).unwrap();
    assert!(matches!(
        store.ssh_password(&account(), None),
        Err(VaultError::AutomaticAccess)
    ));
    assert!(matches!(
        store.repair_automatic_access("incorrect-pass"),
        Err(VaultError::InvalidMasterPassword)
    ));
    store.repair_automatic_access(MASTER).unwrap();
    assert_eq!(
        reveal(store.ssh_password(&account(), None)).as_deref(),
        Some(REMOTE_PASSWORD)
    );
}

#[test]
fn uses_a_successfully_unwrapped_key_when_os_caching_is_unavailable() {
    let key_store = Arc::new(ToggleKeyStore::default());
    let (database, store) = setup_with(Database::open_in_memory().unwrap(), key_store.clone());
    store.create(MASTER, Some(Never)).unwrap();
    let vault_id = database
        .password_vault_config()
        .unwrap()
        .unwrap()
        .config
        .vault_id;
    store
        .remember_ssh_password(&account(), "alice@router.example:22", REMOTE_PASSWORD, None)
        .unwrap();

    store.lock();
    key_store.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(
        reveal(store.ssh_password(&account(), Some(MASTER))).as_deref(),
        Some(REMOTE_PASSWORD)
    );
    let status = store.status().unwrap();
    assert_eq!(
        (
            status.unlock_policy,
            status.locked,
            status.os_key_store_available
        ),
        (Some(Never), false, false)
    );

    store.change_master_password(MASTER, NEXT_MASTER).unwrap();
    store.lock();
    store.change_unlock_policy(NEXT_MASTER, Startup).unwrap();
    let status = store.status().unwrap();
    assert_eq!(
        (
            status.unlock_policy,
            status.locked,
            status.os_key_store_available
        ),
        (Some(Startup), false, false)
    );
    assert_eq!(
        database.pending_password_vault_key_cleanup().unwrap(),
        vec![vault_id.clone()]
    );

    store.dispose();
    key_store.unavailable.store(false, Ordering::SeqCst);
    let vault = PasswordVault::new(
        database.clone(),
        VaultOptions {
            kdf: Some(TEST_KDF),
            key_store: Some(key_store.clone()),
        },
    );
    vault.initialize().unwrap();
    assert!(
        database
            .pending_password_vault_key_cleanup()
            .unwrap()
            .is_empty()
    );
    assert_eq!(key_store.get(&vault_id).unwrap(), None);
    let status = vault.status().unwrap();
    assert_eq!(
        (
            status.unlock_policy,
            status.locked,
            status.os_key_store_available
        ),
        (Some(Startup), true, true)
    );
}

#[test]
fn resets_a_never_policy_vault_even_when_os_key_deletion_fails() {
    let key_store = Arc::new(ToggleKeyStore::default());
    let (_, store) = setup_with(Database::open_in_memory().unwrap(), key_store.clone());
    store.create(MASTER, Some(Never)).unwrap();
    store
        .remember_ssh_password(&account(), "alice@router.example:22", REMOTE_PASSWORD, None)
        .unwrap();
    key_store.unavailable.store(true, Ordering::SeqCst);
    store.delete_all().unwrap();
    let status = store.status().unwrap();
    assert!(!status.configured && status.locked && !status.os_key_store_available);
    assert_eq!(status.credential_count, 0);
}

#[test]
fn rejects_a_stale_32_byte_os_key_and_repairs_it_with_the_master_password() {
    let key_store = Arc::new(MemoryVaultKeyStore::new());
    let (database, first) = setup_with(Database::open_in_memory().unwrap(), key_store.clone());
    first.create(MASTER, Some(Never)).unwrap();
    first
        .remember_ssh_password(&account(), "alice@router.example:22", REMOTE_PASSWORD, None)
        .unwrap();
    let vault_id = database
        .password_vault_config()
        .unwrap()
        .unwrap()
        .config
        .vault_id;
    let correct = key_store.get(&vault_id).unwrap().unwrap();
    first.dispose();
    key_store.set(&vault_id, &[0x5a; 32]).unwrap();

    let vault = PasswordVault::new(
        database,
        VaultOptions {
            kdf: Some(TEST_KDF),
            key_store: Some(key_store.clone()),
        },
    );
    vault.initialize().unwrap();
    let status = vault.status().unwrap();
    assert_eq!((status.unlock_policy, status.locked), (Some(Never), true));
    assert!(matches!(
        vault.ssh_password(&account(), None),
        Err(VaultError::AutomaticAccess)
    ));
    assert_eq!(
        reveal(vault.ssh_password(&account(), Some(MASTER))).as_deref(),
        Some(REMOTE_PASSWORD)
    );
    assert_eq!(key_store.get(&vault_id).unwrap().unwrap(), correct);
}

#[test]
fn does_not_let_obsolete_key_cleanup_failures_block_initialization() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("muxus-vault-device.key")).unwrap();
    let (_, vault) = setup_with(
        Database::open(dir.path().join("muxus.sqlite3")).unwrap(),
        Arc::new(MemoryVaultKeyStore::new()),
    );
    assert!(!vault.status().unwrap().configured);
}

#[test]
fn preserves_kdf_failures_instead_of_reporting_an_incorrect_password() {
    let (database, store, _) = setup();
    database
        .create_password_vault_config(&PasswordVaultConfig {
            format_version: 3,
            vault_id: "invalid-kdf-vault".into(),
            unlock_policy: Startup,
            kdf_algorithm: "scrypt".into(),
            kdf_salt: vec![0; 16],
            kdf_cost: 1025,
            kdf_block_size: 8,
            kdf_parallelism: 1,
            master_key_nonce: vec![0; 12],
            master_key_ciphertext: vec![0; 32],
            master_key_tag: vec![0; 16],
            key_check: None,
        })
        .unwrap();
    let error = store.unlock_for_session(MASTER).unwrap_err();
    assert!(!matches!(error, VaultError::InvalidMasterPassword));
    assert!(error.to_string().to_lowercase().contains("scrypt"));
}

#[test]
fn does_not_create_a_key_file_or_leave_deleted_secrets_in_sqlite_or_wal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("muxus-vault-device.key"), [42u8; 32]).unwrap();
    let key_store = Arc::new(MemoryVaultKeyStore::new());
    let (database, store) = setup_with(
        Database::open(dir.path().join("muxus.sqlite3")).unwrap(),
        key_store.clone(),
    );
    assert!(!dir.path().join("muxus-vault-device.key").exists());
    store.create(MASTER, Some(Never)).unwrap();
    store
        .remember_ssh_password(
            &account(),
            "unique-label-that-must-be-scrubbed",
            REMOTE_PASSWORD,
            None,
        )
        .unwrap();
    let record = database
        .list_encrypted_credentials("muxus-master-vault", "muxus/ssh-password/v1")
        .unwrap()
        .remove(0);
    let vault_id = database
        .password_vault_config()
        .unwrap()
        .unwrap()
        .config
        .vault_id;
    store.delete_all().unwrap();
    assert_eq!(key_store.get(&vault_id).unwrap(), None);
    store.dispose();
    drop(store);
    Arc::try_unwrap(database).unwrap().close().unwrap();

    let mut persisted = Vec::new();
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("muxus.sqlite3")
        {
            persisted.extend(std::fs::read(entry.path()).unwrap());
        }
    }
    let contains = |needle: &[u8]| {
        persisted
            .windows(needle.len())
            .any(|window| window == needle)
    };
    assert!(!contains(b"unique-label-that-must-be-scrubbed"));
    assert!(!contains(&record.ciphertext));
    assert!(!contains(MASTER.as_bytes()));
    assert!(!contains(REMOTE_PASSWORD.as_bytes()));
}

#[test]
fn forgets_individual_credentials_or_resets_the_whole_vault() {
    let (_, store, _) = setup();
    store.create(MASTER, Some(Startup)).unwrap();
    store
        .remember_ssh_password(&account(), "alice@router.example:22", REMOTE_PASSWORD, None)
        .unwrap();
    let credential = store.status().unwrap().credentials.remove(0);
    assert!(store.delete_credential(&credential.id).unwrap());
    assert!(!store.has_ssh_password(&account()).unwrap());
    store.delete_all().unwrap();
    let status = store.status().unwrap();
    assert!(!status.configured && status.locked);
    assert_eq!(status.credential_count, 0);
}

fn secret(name: &str, username: Option<&str>, value: Option<&str>) -> SecretInput {
    SecretInput {
        name: name.into(),
        username: username.map(str::to_owned),
        value: value.map(str::to_owned),
    }
}

#[test]
fn keeps_named_secrets_apart_from_login_passwords_behind_the_master_password() {
    let (_, store, _) = setup();
    store.create(MASTER, Some(Never)).unwrap();
    store
        .remember_ssh_password(&account(), "alice@router.example:22", REMOTE_PASSWORD, None)
        .unwrap();
    assert!(matches!(
        store.create_secret(
            &secret("Core enable", None, Some("enable-secret")),
            "incorrect-pass"
        ),
        Err(VaultError::InvalidMasterPassword)
    ));
    let enable = store
        .create_secret(
            &secret("  Core enable ", None, Some("enable-secret")),
            MASTER,
        )
        .unwrap();
    let sudo = store
        .create_secret(
            &secret("Lab sudo", Some(" admin "), Some("sudo-secret")),
            MASTER,
        )
        .unwrap();
    assert_eq!(
        (enable.name.as_str(), enable.username.as_deref()),
        ("Core enable", None)
    );
    assert_eq!(
        (sudo.name.as_str(), sudo.username.as_deref()),
        ("Lab sudo", Some("admin"))
    );
    let status = store.status().unwrap();
    assert_eq!(status.credential_count, 1);
    assert_eq!(status.credentials[0].label, "alice@router.example:22");
    assert_eq!(
        status
            .secrets
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["Core enable", "Lab sudo"]
    );
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains("enable-secret")
    );

    assert_eq!(
        reveal(store.secret_value(&enable.id, None)).as_deref(),
        Some("enable-secret")
    );
    assert_eq!(reveal(store.secret_value("no-such-secret", None)), None);
    assert_eq!(
        reveal(store.reveal_credential(&sudo.id, MASTER)).as_deref(),
        Some("sudo-secret")
    );
    assert!(matches!(
        store.create_secret(&secret("core ENABLE", None, Some("other")), MASTER),
        Err(VaultError::SecretNameTaken(_))
    ));
    assert!(matches!(
        store.create_secret(&secret("   ", None, Some("other")), MASTER),
        Err(VaultError::InvalidSecret(_))
    ));
    assert!(matches!(
        store.create_secret(&secret("Empty", None, Some("")), MASTER),
        Err(VaultError::InvalidSecret(_))
    ));

    assert!(matches!(
        store.update_secret(
            &enable.id,
            &secret("Edge enable", None, None),
            "incorrect-pass"
        ),
        Err(VaultError::InvalidMasterPassword)
    ));
    store
        .update_secret(
            &enable.id,
            &secret("Edge enable", Some("ops"), None),
            MASTER,
        )
        .unwrap();
    assert_eq!(
        reveal(store.secret_value(&enable.id, None)).as_deref(),
        Some("enable-secret")
    );
    store
        .update_secret(
            &enable.id,
            &secret("Edge enable", None, Some("rotated")),
            MASTER,
        )
        .unwrap();
    assert_eq!(
        reveal(store.secret_value(&enable.id, None)).as_deref(),
        Some("rotated")
    );
    assert_eq!(
        store
            .secrets()
            .unwrap()
            .iter()
            .find(|s| s.id == enable.id)
            .unwrap()
            .name,
        "Edge enable"
    );
    assert!(matches!(
        store.update_secret(&enable.id, &secret("lab sudo", None, None), MASTER),
        Err(VaultError::SecretNameTaken(_))
    ));
    // A saved SSH password is not a named secret.
    let credential = store.status().unwrap().credentials.remove(0);
    assert!(!store.has_secret(&credential.id).unwrap());
    assert_eq!(reveal(store.secret_value(&credential.id, None)), None);

    assert!(store.delete_credential(&enable.id).unwrap());
    assert!(!store.has_secret(&enable.id).unwrap());
    assert_eq!(
        store
            .status()
            .unwrap()
            .secrets
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["Lab sudo"]
    );
}

#[test]
fn asks_for_the_master_password_to_use_a_secret_under_the_stricter_policies() {
    let (_, store, _) = setup();
    store.create(MASTER, Some(Credential)).unwrap();
    let enable = store
        .create_secret(&secret("Enable", None, Some("enable-secret")), MASTER)
        .unwrap();
    assert!(matches!(
        store.secret_value(&enable.id, None),
        Err(VaultError::UnlockRequired(Credential))
    ));
    assert!(matches!(
        store.secret_value(&enable.id, Some("incorrect-pass")),
        Err(VaultError::InvalidMasterPassword)
    ));
    assert_eq!(
        reveal(store.secret_value(&enable.id, Some(MASTER))).as_deref(),
        Some("enable-secret")
    );
    assert!(store.status().unwrap().locked);

    store.change_unlock_policy(MASTER, Startup).unwrap();
    store.lock();
    assert!(
        store
            .secret_value(&enable.id, None)
            .unwrap_err()
            .is_unlock_required()
    );
    assert_eq!(
        reveal(store.secret_value(&enable.id, Some(MASTER))).as_deref(),
        Some("enable-secret")
    );
    // The startup policy keeps the vault unlocked once the master password was given.
    assert_eq!(
        reveal(store.secret_value(&enable.id, None)).as_deref(),
        Some("enable-secret")
    );
}

#[test]
fn accepts_eight_character_master_passwords_and_rejects_seven() {
    let (_, store, _) = setup();
    assert!(matches!(
        store.create("1234567", None),
        Err(VaultError::InvalidMasterPasswordFormat(_))
    ));
    store.create("12345678", None).unwrap();
    assert_eq!(store.status().unwrap().unlock_policy, Some(Never));
}

#[test]
fn types_a_secret_into_every_session_that_takes_it() {
    let (_, store, _) = setup();
    store.create(MASTER, Some(Startup)).unwrap();
    let enable = store
        .create_secret(&secret("Enable", None, Some("enable-Secret-4711")), MASTER)
        .unwrap();
    assert_eq!(secret_keystrokes("pw", true).as_slice(), b"pw\r");
    assert_eq!(secret_keystrokes("pw", false).as_slice(), b"pw");

    let received = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let sink = received.clone();
    type Writer = Box<dyn FnMut(&[u8]) -> bool>;
    let mut writers: Vec<Writer> = vec![
        Box::new(move |bytes: &[u8]| {
            sink.lock().unwrap().push(bytes.to_vec());
            true
        }),
        Box::new(|_: &[u8]| false),
    ];
    let typed = type_vault_secret(&store, &enable.id, &mut writers, true, None).unwrap();
    assert_eq!(typed, Some(1));
    assert_eq!(
        received.lock().unwrap().as_slice(),
        [b"enable-Secret-4711\r".to_vec()]
    );
    assert_eq!(
        type_vault_secret(&store, "gone", &mut writers, true, None).unwrap(),
        None
    );
    store.lock();
    assert!(
        type_vault_secret(&store, &enable.id, &mut writers, false, None)
            .unwrap_err()
            .is_unlock_required()
    );
    assert_eq!(
        type_vault_secret(&store, &enable.id, &mut writers, false, Some(MASTER)).unwrap(),
        Some(1)
    );
}

// ---------------------------------------------------------------------------
// Vaults the TypeScript build created
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/vault")
        .join(name)
}

fn open_ts_vault(
    name: &str,
    dir: &Path,
    key_store: Arc<dyn VaultKeyStore>,
) -> (Arc<Database>, PasswordVault, Value) {
    let compressed = std::fs::read(fixture(&format!("{name}.sqlite3.zst"))).unwrap();
    let path = dir.join("muxus.sqlite3");
    std::fs::write(&path, zstd::decode_all(compressed.as_slice()).unwrap()).unwrap();
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture(&format!("{name}.expected.json"))).unwrap(),
    )
    .unwrap();
    let database = Arc::new(Database::open(&path).unwrap());
    let vault = PasswordVault::new(
        database.clone(),
        VaultOptions {
            kdf: Some(TEST_KDF),
            key_store: Some(key_store),
        },
    );
    vault.initialize().unwrap();
    (database, vault, expected)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap()
}

#[test]
fn opens_a_typescript_startup_vault_with_its_master_password() {
    let dir = tempfile::tempdir().unwrap();
    let (_, vault, expected) = open_ts_vault(
        "ts-vault-startup",
        dir.path(),
        Arc::new(MemoryVaultKeyStore::new()),
    );
    let master = text(&expected, "masterPassword");
    let mut status = serde_json::to_value(vault.status().unwrap()).unwrap();
    // The TypeScript status was taken while unlocked; this one is locked until asked.
    assert_eq!(status["locked"], Value::Bool(true));
    status["locked"] = Value::Bool(false);
    assert_eq!(status, expected["status"]);
    assert_eq!(
        ssh_password_account("alice", "Router.Example", 22),
        text(&expected, "sshAccount")
    );
    assert!(
        vault
            .ssh_password(text(&expected, "sshAccount"), None)
            .unwrap_err()
            .is_unlock_required()
    );
    vault.unlock_for_session(master).unwrap();
    assert_eq!(
        reveal(vault.ssh_password(text(&expected, "sshAccount"), None)).as_deref(),
        Some(text(&expected, "sshPassword"))
    );
    assert_eq!(
        reveal(vault.ssh_password(text(&expected, "folderAccount"), None)).as_deref(),
        Some(text(&expected, "folderPassword"))
    );
    assert_eq!(
        reveal(vault.secret_value(text(&expected, "secretId"), None)).as_deref(),
        Some(text(&expected, "secretValue"))
    );
    assert_eq!(
        reveal(vault.reveal_credential(text(&expected, "secretId"), master)).as_deref(),
        Some(text(&expected, "secretValue"))
    );
    // A value saved by this build reads back too (and stays readable by the TypeScript build).
    vault
        .remember_ssh_password("rust-account", "rust@host:22", "from-rust", None)
        .unwrap();
    vault.lock();
    vault.unlock_for_session(master).unwrap();
    assert_eq!(
        reveal(vault.ssh_password("rust-account", None)).as_deref(),
        Some("from-rust")
    );
}

/// The default KDF (N = 2^17, r = 8) takes a few seconds unoptimized, so this
/// runs on request: `cargo test -p muxus-store --test vault -- --ignored`.
#[test]
#[ignore = "slow: derives a key with the default scrypt cost"]
fn opens_a_typescript_never_vault_through_its_os_key_and_its_master_password() {
    let dir = tempfile::tempdir().unwrap();
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture("ts-vault-never-default-kdf.expected.json")).unwrap(),
    )
    .unwrap();
    let key_store = Arc::new(MemoryVaultKeyStore::new());
    let os_key = STANDARD.decode(text(&expected, "osKey")).unwrap();
    key_store.set(text(&expected, "vaultId"), &os_key).unwrap();
    let (database, vault, expected) =
        open_ts_vault("ts-vault-never-default-kdf", dir.path(), key_store.clone());
    let config = database.password_vault_config().unwrap().unwrap().config;
    assert_eq!(
        (
            config.kdf_cost,
            config.kdf_block_size,
            config.kdf_parallelism
        ),
        (131_072, 8, 1)
    );
    // The OS key the TypeScript build stored unlocks it without a prompt.
    assert!(!vault.status().unwrap().locked);
    assert_eq!(
        reveal(vault.ssh_password(text(&expected, "sshAccount"), None)).as_deref(),
        Some(text(&expected, "sshPassword"))
    );
    // And the master password unwraps the same key through the default KDF.
    assert_eq!(
        reveal(vault.reveal_credential(
            text(&expected, "secretId"),
            text(&expected, "masterPassword")
        ))
        .as_deref(),
        Some(text(&expected, "secretValue"))
    );
}

#[test]
fn a_never_vault_without_its_os_key_asks_for_repair() {
    let dir = tempfile::tempdir().unwrap();
    let (_, vault, expected) = open_ts_vault(
        "ts-vault-never-default-kdf",
        dir.path(),
        Arc::new(MemoryVaultKeyStore::new()),
    );
    let status = vault.status().unwrap();
    assert_eq!(
        (status.unlock_policy, status.locked),
        (Some(PasswordVaultUnlockPolicy::Never), true)
    );
    assert!(matches!(
        vault.ssh_password(text(&expected, "sshAccount"), None),
        Err(VaultError::AutomaticAccess)
    ));
}

/// Writes a vault created by this build to `$MUXUS_EXPORT_VAULT_FIXTURE` so
/// `fixtures/vault/verify.mts` can open it with the TypeScript build.
#[test]
#[ignore = "manual interoperability export"]
fn exports_a_vault_for_the_typescript_build() {
    let Ok(out) = std::env::var("MUXUS_EXPORT_VAULT_FIXTURE") else {
        return;
    };
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("muxus.sqlite3");
    let _ = std::fs::remove_file(&path);
    let (database, vault) = setup_with(
        Database::open(&path).unwrap(),
        Arc::new(MemoryVaultKeyStore::new()),
    );
    vault.create(MASTER, Some(Startup)).unwrap();
    vault
        .remember_ssh_password(
            &account(),
            "alice@router.example:22",
            "pässword-from-rust-🔑",
            None,
        )
        .unwrap();
    let created = vault
        .create_secret(
            &secret("Rust enable", Some("ops"), Some("enable-from-rust")),
            MASTER,
        )
        .unwrap();
    let expected = serde_json::json!({
        "masterPassword": MASTER,
        "sshAccount": account(),
        "sshPassword": "pässword-from-rust-🔑",
        "secretId": created.id,
        "secretValue": "enable-from-rust",
    });
    std::fs::write(
        out.join("expected.json"),
        serde_json::to_string_pretty(&expected).unwrap(),
    )
    .unwrap();
    drop(vault);
    Arc::try_unwrap(database).unwrap().close().unwrap();
}
