//! Port of tests/unit/server/password-managers-keepass.test.ts.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use muxus_core::api::{PasswordManagerConfig, PasswordManagerState};
use muxus_passwords::{
    HostEnv, KeePassProvider, PasswordManagerProblem, PasswordManagerProvider, ProviderStatus,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// KDBX 3.1 (AES-KDF), written by KeePassXC 2.7.6.
fn keepassxc() -> PathBuf {
    fixture("keepassxc-kdbx3.kdbx")
}

/// KDBX 4 (Argon2id), checked to open in KeePassXC 2.7.6.
fn kdbx4() -> PathBuf {
    fixture("kdbx4-argon2id.kdbx")
}

fn provider(file: &Path) -> KeePassProvider {
    KeePassProvider::new(
        &PasswordManagerConfig {
            path: Some(file.to_string_lossy().into_owned()),
            ..Default::default()
        },
        &HostEnv::system(),
    )
}

async fn state(provider: &KeePassProvider) -> PasswordManagerState {
    provider.status().await.unwrap().state
}

/// Mark a file as saved `seconds` from now, as a later save by KeePass would.
fn touch(file: &Path, seconds: u64) {
    std::fs::File::options()
        .write(true)
        .open(file)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(seconds))
        .unwrap();
}

#[tokio::test]
async fn reads_a_database_written_by_keepassxc() {
    let provider = provider(&keepassxc());
    assert_eq!(state(&provider).await, PasswordManagerState::Locked);
    provider.unlock("master-pw-1").await.unwrap();
    assert_eq!(provider.status().await.unwrap(), ProviderStatus::ready());

    let entries = provider.entries().await.unwrap();
    let listed: Vec<_> = entries
        .iter()
        .map(|entry| {
            (
                entry.title.as_str(),
                entry.username.as_deref(),
                entry.url.as_deref(),
                entry.group.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("Core router", Some("admin"), None, None),
            (
                "web1",
                Some("root"),
                Some("ssh://web1.example.com"),
                Some("Servers")
            ),
        ]
    );
    let web1 = entries.iter().find(|entry| entry.title == "web1").unwrap();
    assert_eq!(provider.password(&web1.id).await.unwrap(), "hunter2-säcret");
}

#[tokio::test]
async fn reads_a_kdbx_4_database_protected_with_argon2id() {
    let provider = provider(&kdbx4());
    provider.unlock("kdbx4-master").await.unwrap();
    let entries = provider.entries().await.unwrap();
    // The recycle bin and the entry without a password are left out.
    let listed: Vec<_> = entries
        .iter()
        .map(|entry| {
            (
                entry.group.as_deref(),
                entry.title.as_str(),
                entry.username.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            (Some("Lab"), "sw2 (same login)", Some("netops")),
            (Some("Lab / Core"), "sw1", Some("netops")),
        ]
    );
    let (sw2, sw1) = (&entries[0], &entries[1]);
    assert_eq!(
        provider.password(&sw1.id).await.unwrap(),
        "p4ss \"quoted\" & ünïcode"
    );
    // A field reference resolves to the entry it points at.
    assert_eq!(
        provider.password(&sw2.id).await.unwrap(),
        "p4ss \"quoted\" & ünïcode"
    );
}

#[tokio::test]
async fn names_entries_by_the_uuid_the_typescript_build_stored() {
    // A host linked in the Electron build keeps this reference: the entry's UUID in base64.
    let provider = provider(&kdbx4());
    provider.unlock("kdbx4-master").await.unwrap();
    let entries = provider.entries().await.unwrap();
    for entry in &entries {
        assert_eq!(entry.id.len(), 24, "{}", entry.id);
        assert!(entry.id.ends_with("=="), "{}", entry.id);
    }
    assert!(
        entries
            .iter()
            .any(|entry| entry.id == "J/3PdeqpbaiDMsRLdzIWyA=="),
        "{entries:?}"
    );
}

#[tokio::test]
async fn rejects_a_wrong_password_and_stays_locked() {
    let provider = provider(&kdbx4());
    let error = provider.unlock("not-the-password").await.unwrap_err();
    assert_eq!(error.problem, PasswordManagerProblem::InvalidUnlock);
    assert_eq!(error.message, "The database password is wrong.");
    assert_eq!(
        provider.entries().await.unwrap_err().problem,
        PasswordManagerProblem::Locked
    );
    assert_eq!(state(&provider).await, PasswordManagerState::Locked);
}

#[tokio::test]
async fn locks_again_on_request() {
    let provider = provider(&kdbx4());
    provider.unlock("kdbx4-master").await.unwrap();
    provider.lock().await.unwrap();
    assert_eq!(
        provider
            .password("J/3PdeqpbaiDMsRLdzIWyA==")
            .await
            .unwrap_err()
            .problem,
        PasswordManagerProblem::Locked
    );
}

#[tokio::test]
async fn reports_an_entry_that_is_gone_and_a_file_that_is_missing_or_not_a_database() {
    let tmp = tempfile::tempdir().unwrap();
    let opened = provider(&kdbx4());
    opened.unlock("kdbx4-master").await.unwrap();
    assert_eq!(
        opened
            .password("AAAAAAAAAAAAAAAAAAAAAA==")
            .await
            .unwrap_err()
            .problem,
        PasswordManagerProblem::EntryMissing
    );

    let missing = provider(&tmp.path().join("nowhere.kdbx"));
    assert_eq!(state(&missing).await, PasswordManagerState::Unavailable);
    assert_eq!(
        missing.unlock("x").await.unwrap_err().problem,
        PasswordManagerProblem::Unavailable
    );

    let text = tmp.path().join("notes.kdbx");
    std::fs::write(
        &text,
        "just some text, long enough to have a header to misread",
    )
    .unwrap();
    let error = provider(&text).unlock("x").await.unwrap_err();
    assert_eq!(error.problem, PasswordManagerProblem::Failed);
    assert_eq!(error.message, "notes.kdbx is not a KeePass database.");

    let unset = KeePassProvider::new(&PasswordManagerConfig::default(), &HostEnv::system());
    assert_eq!(
        unset.status().await.unwrap(),
        ProviderStatus::new(
            PasswordManagerState::Unavailable,
            "No database file is set."
        )
    );
}

#[tokio::test]
async fn picks_up_a_database_saved_again_by_keepass_without_asking_for_the_password() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("live.kdbx");
    std::fs::copy(keepassxc(), &file).unwrap();
    let provider = provider(&file);
    provider.unlock("master-pw-1").await.unwrap();
    assert_eq!(provider.entries().await.unwrap().len(), 2);

    // The same password still opens the replaced file...
    std::fs::copy(keepassxc(), &file).unwrap();
    touch(&file, 5);
    assert_eq!(provider.entries().await.unwrap().len(), 2);

    // ...but a database whose password changed has to be unlocked again.
    std::fs::copy(kdbx4(), &file).unwrap();
    touch(&file, 10);
    let error = provider.entries().await.unwrap_err();
    assert_eq!(error.problem, PasswordManagerProblem::Locked);
    assert_eq!(error.message, "The KeePass database password has changed.");
    assert_eq!(state(&provider).await, PasswordManagerState::Locked);
}

#[tokio::test]
async fn expands_the_home_directory_in_the_database_and_key_file_paths() {
    let home = tempfile::tempdir().unwrap();
    std::fs::copy(keepassxc(), home.path().join("Passwords.kdbx")).unwrap();
    let host = HostEnv::system().with_home(home.path());
    let provider = KeePassProvider::new(
        &PasswordManagerConfig {
            path: Some("  ~/Passwords.kdbx ".into()),
            ..Default::default()
        },
        &host,
    );
    provider.unlock("master-pw-1").await.unwrap();
    assert_eq!(provider.entries().await.unwrap().len(), 2);

    // A key file that is not there is said so, before the database is tried.
    let keyed = KeePassProvider::new(
        &PasswordManagerConfig {
            path: Some("~/Passwords.kdbx".into()),
            key_file: Some("~/missing.keyx".into()),
            ..Default::default()
        },
        &host,
    );
    let error = keyed.unlock("master-pw-1").await.unwrap_err();
    assert_eq!(error.problem, PasswordManagerProblem::Unavailable);
    assert_eq!(
        error.message,
        format!(
            "The key file {} could not be read.",
            home.path().join("missing.keyx").display()
        )
    );
}
