//! The connected password managers: what `PasswordManagers` does with the sources a store
//! keeps, and how a login reads a linked entry (the parts of
//! tests/unit/server/password-manager-login.test.ts and password-manager-routes.test.ts
//! that need no SSH server and no routes).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use muxus_core::api::{
    ExternalPasswordRef, PasswordManagerConfig, PasswordManagerInput, PasswordManagerKind,
    PasswordManagerState,
};
use muxus_core::protocol::{
    AuthPromptField, AuthPromptInfo, AuthPromptPurpose, AuthPromptResponse,
};
use muxus_passwords::{
    HostEnv, PasswordManagerIo, PasswordManagerProblem, PasswordManagers, PasswordManagersError,
    PromptCancelled, SourceStore, StoredSource, default_provider,
};

/// Written by KeePassXC; "Servers/web1" holds `PASSWORD`.
fn database() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/keepassxc-kdbx3.kdbx")
}
const DATABASE_PASSWORD: &str = "master-pw-1";
const PASSWORD: &str = "hunter2-säcret";

/// Sources kept in memory, the way the database keeps them.
#[derive(Default)]
struct MemoryStore {
    rows: Mutex<Vec<StoredSource>>,
    next: Mutex<u32>,
}

impl SourceStore for MemoryStore {
    fn list(&self) -> Result<Vec<StoredSource>, String> {
        let mut rows = self.rows.lock().unwrap().clone();
        rows.sort_by_key(|row| row.name.to_lowercase());
        Ok(rows)
    }

    fn get(&self, id: &str) -> Result<Option<StoredSource>, String> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|row| row.id == id)
            .cloned())
    }

    fn create(
        &self,
        kind: PasswordManagerKind,
        name: &str,
        config: &PasswordManagerConfig,
    ) -> Result<StoredSource, String> {
        if name.trim().is_empty() {
            return Err("name is required".into());
        }
        let mut next = self.next.lock().unwrap();
        *next += 1;
        let row = StoredSource {
            id: format!("source-{next}"),
            kind: kind.as_str().to_owned(),
            name: name.trim().to_owned(),
            config: config.clone(),
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: "2026-01-01 00:00:00".into(),
        };
        self.rows.lock().unwrap().push(row.clone());
        Ok(row)
    }

    fn update(
        &self,
        id: &str,
        name: &str,
        config: &PasswordManagerConfig,
    ) -> Result<Option<StoredSource>, String> {
        let mut rows = self.rows.lock().unwrap();
        let Some(row) = rows.iter_mut().find(|row| row.id == id) else {
            return Ok(None);
        };
        row.name = name.trim().to_owned();
        row.config = config.clone();
        row.updated_at = "2026-01-02 00:00:00".into();
        Ok(Some(row.clone()))
    }

    fn delete(&self, id: &str) -> Result<bool, String> {
        let mut rows = self.rows.lock().unwrap();
        let before = rows.len();
        rows.retain(|row| row.id != id);
        Ok(rows.len() < before)
    }
}

/// Answers unlock prompts with the database passwords listed, in order; `None` skips, and
/// running out of answers dismisses the prompt.
#[derive(Default)]
struct TestIo {
    unlock: Mutex<VecDeque<Option<String>>>,
    prompts: Mutex<Vec<AuthPromptInfo>>,
    statuses: Mutex<Vec<(String, bool)>>,
}

impl TestIo {
    fn answering(unlock: &[Option<&str>]) -> Self {
        Self {
            unlock: Mutex::new(
                unlock
                    .iter()
                    .map(|answer| answer.map(str::to_owned))
                    .collect(),
            ),
            ..Self::default()
        }
    }

    fn prompts(&self) -> Vec<AuthPromptInfo> {
        self.prompts.lock().unwrap().clone()
    }

    fn said(&self) -> String {
        self.statuses
            .lock()
            .unwrap()
            .iter()
            .map(|(message, _)| message.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait]
impl PasswordManagerIo for TestIo {
    fn status(&self, message: &str, transient: bool) {
        self.statuses
            .lock()
            .unwrap()
            .push((message.to_owned(), transient));
    }

    async fn prompt(&self, info: AuthPromptInfo) -> Result<AuthPromptResponse, PromptCancelled> {
        self.prompts.lock().unwrap().push(info);
        match self.unlock.lock().unwrap().pop_front() {
            None => Err(PromptCancelled),
            Some(None) => Ok(AuthPromptResponse {
                skipped: Some(true),
                ..Default::default()
            }),
            Some(Some(answer)) => Ok(AuthPromptResponse {
                answers: vec![answer],
                ..Default::default()
            }),
        }
    }
}

struct Setup {
    managers: PasswordManagers,
    store: Arc<MemoryStore>,
    source_id: String,
    web1: ExternalPasswordRef,
}

fn managers(store: Arc<MemoryStore>) -> PasswordManagers {
    // No programs on the PATH: what is installed must not depend on this computer.
    let host = HostEnv::system().with_var("PATH", "");
    let provider_host = host.clone();
    PasswordManagers::with_providers(
        store,
        host,
        Arc::new(move |kind, config| default_provider(&provider_host, kind, config)),
    )
}

fn keepass(path: &Path) -> PasswordManagerInput {
    PasswordManagerInput {
        kind: PasswordManagerKind::Keepass,
        name: None,
        config: PasswordManagerConfig {
            path: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        },
    }
}

/// A connected, locked KeePass database and the reference to its "web1" entry.
async fn setup() -> Setup {
    let store = Arc::new(MemoryStore::default());
    let managers = managers(store.clone());
    let source = managers.create(&keepass(&database())).await.unwrap();
    managers
        .unlock(&source.id, DATABASE_PASSWORD)
        .await
        .unwrap();
    let entry = managers
        .entries(&source.id, false)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.title == "web1")
        .unwrap();
    managers.lock(&source.id).await.unwrap();
    Setup {
        managers,
        store,
        web1: ExternalPasswordRef {
            source_id: source.id.clone(),
            entry_id: entry.id,
            title: entry.title,
            username: entry.username,
        },
        source_id: source.id,
    }
}

#[tokio::test]
async fn reads_the_entry_from_an_unlocked_database_without_any_prompt() {
    let Setup {
        managers,
        source_id,
        web1,
        ..
    } = setup().await;
    managers
        .unlock(&source_id, DATABASE_PASSWORD)
        .await
        .unwrap();
    let io = TestIo::default();
    assert_eq!(
        managers.password_for(&web1, &io).await.unwrap().as_deref(),
        Some(PASSWORD)
    );
    assert!(io.prompts().is_empty());
    assert_eq!(
        *io.statuses.lock().unwrap(),
        [(
            "Using the password of “web1” from KeePass – keepassxc-kdbx3.".to_owned(),
            true
        )]
    );
    // The route-side read gives the same.
    assert_eq!(managers.password(&web1).await.unwrap(), PASSWORD);
}

#[tokio::test]
async fn asks_for_the_database_password_when_the_database_is_locked_retrying_a_wrong_one() {
    let Setup { managers, web1, .. } = setup().await;
    let io = TestIo::answering(&[Some("not-it"), Some(DATABASE_PASSWORD)]);
    assert_eq!(
        managers.password_for(&web1, &io).await.unwrap().as_deref(),
        Some(PASSWORD)
    );
    let prompts = io.prompts();
    assert_eq!(prompts.len(), 2);
    assert_eq!(
        prompts[0],
        AuthPromptInfo {
            name: Some("Unlock KeePass – keepassxc-kdbx3".into()),
            purpose: Some(AuthPromptPurpose::VaultUnlock),
            instructions: Some(
                "Unlock KeePass – keepassxc-kdbx3 to use the password of “web1”.".into()
            ),
            prompts: vec![AuthPromptField {
                prompt: "Database password".into(),
                echo: false
            }],
            skip_label: Some("Use another password".into()),
            ..Default::default()
        }
    );
    assert_eq!(
        prompts[1].instructions.as_deref(),
        Some(
            "The database password is wrong.\n\nUnlock KeePass – keepassxc-kdbx3 to use the password of “web1”."
        )
    );

    // The database stays unlocked for the next login.
    let again = TestIo::default();
    assert_eq!(
        managers
            .password_for(&web1, &again)
            .await
            .unwrap()
            .as_deref(),
        Some(PASSWORD)
    );
    assert!(again.prompts().is_empty());
}

#[tokio::test]
async fn gives_no_password_when_unlocking_is_skipped_and_gives_up_the_login_when_it_is_dismissed() {
    let Setup { managers, web1, .. } = setup().await;
    let skipped = TestIo::answering(&[None]);
    assert_eq!(managers.password_for(&web1, &skipped).await, Ok(None));
    assert_eq!(skipped.prompts().len(), 1);
    // Nothing is said: the login goes on to ask for the password itself.
    assert_eq!(skipped.said(), "");

    let dismissed = TestIo::default();
    assert_eq!(
        managers.password_for(&web1, &dismissed).await,
        Err(PromptCancelled)
    );
}

#[tokio::test]
async fn stops_asking_after_three_wrong_database_passwords() {
    let Setup { managers, web1, .. } = setup().await;
    let io = TestIo::answering(&[Some("a"), Some("b"), Some("c"), Some(DATABASE_PASSWORD)]);
    assert_eq!(managers.password_for(&web1, &io).await, Ok(None));
    assert_eq!(io.prompts().len(), 3);
    assert_eq!(
        io.said(),
        "Could not read “web1” from KeePass – keepassxc-kdbx3: The KeePass database is locked. Enter the password."
    );
}

#[tokio::test]
async fn reports_an_entry_or_a_password_manager_that_is_gone() {
    let Setup {
        managers,
        source_id,
        web1,
        ..
    } = setup().await;
    managers
        .unlock(&source_id, DATABASE_PASSWORD)
        .await
        .unwrap();
    let missing_entry = TestIo::default();
    let gone = ExternalPasswordRef {
        entry_id: "AAAAAAAAAAAAAAAAAAAAAA==".into(),
        ..web1.clone()
    };
    assert_eq!(managers.password_for(&gone, &missing_entry).await, Ok(None));
    assert_eq!(
        missing_entry.said(),
        "Could not read “web1” from KeePass – keepassxc-kdbx3: The entry is no longer in the KeePass database. Enter the password."
    );
    assert!(matches!(
        managers.password(&gone).await,
        Err(PasswordManagersError::Provider(error)) if error.problem == PasswordManagerProblem::EntryMissing
    ));

    assert!(managers.remove(&source_id).unwrap());
    assert!(!managers.remove(&source_id).unwrap());
    let missing_source = TestIo::default();
    // An entry picked before titles were kept is named in general words.
    let untitled = ExternalPasswordRef {
        title: String::new(),
        ..web1.clone()
    };
    assert_eq!(
        managers.password_for(&untitled, &missing_source).await,
        Ok(None)
    );
    assert_eq!(
        missing_source.said(),
        "The password manager holding the linked entry is no longer connected. Enter the password."
    );
    assert_eq!(
        managers.password(&web1).await,
        Err(PasswordManagersError::SourceMissing)
    );
    assert_eq!(
        managers.entries(&source_id, false).await,
        Err(PasswordManagersError::SourceMissing)
    );
    assert_eq!(
        managers
            .unlock(&source_id, "x")
            .await
            .unwrap_err()
            .to_string(),
        "The password manager is no longer connected."
    );
}

#[tokio::test]
async fn lists_the_connected_managers_with_where_each_stands() {
    let Setup {
        managers,
        source_id,
        ..
    } = setup().await;
    let tmp = tempfile::tempdir().unwrap();
    managers
        .create(&PasswordManagerInput {
            name: Some("  Old database ".into()),
            ..keepass(&tmp.path().join("gone.kdbx"))
        })
        .await
        .unwrap();
    let bitwarden = managers
        .create(&PasswordManagerInput {
            kind: PasswordManagerKind::Bitwarden,
            name: None,
            config: PasswordManagerConfig {
                program: Some("   ".into()),
                account: Some(String::new()),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    // Empty settings are not kept, and a program that is not there is said so.
    assert_eq!(bitwarden.name, "Bitwarden");
    assert_eq!(bitwarden.config, PasswordManagerConfig::default());
    assert_eq!(bitwarden.state, PasswordManagerState::Unavailable);
    assert_eq!(
        bitwarden.message.as_deref(),
        Some("bw was not found on this computer.")
    );

    let listed = managers.list().await.unwrap();
    let summary: Vec<_> = listed
        .sources
        .iter()
        .map(|source| (source.name.as_str(), source.kind, source.state))
        .collect();
    assert_eq!(
        summary,
        [
            (
                "Bitwarden",
                PasswordManagerKind::Bitwarden,
                PasswordManagerState::Unavailable
            ),
            (
                "KeePass – keepassxc-kdbx3",
                PasswordManagerKind::Keepass,
                PasswordManagerState::Locked
            ),
            (
                "Old database",
                PasswordManagerKind::Keepass,
                PasswordManagerState::Unavailable
            ),
        ]
    );
    assert_eq!(
        listed.sources[1].message.as_deref(),
        Some("Enter the database password to unlock it.")
    );
    // KeePass needs no program; nothing else is on this PATH.
    assert_eq!(listed.installed, [PasswordManagerKind::Keepass]);

    let unlocked = managers
        .unlock(&source_id, DATABASE_PASSWORD)
        .await
        .unwrap();
    assert_eq!(unlocked.state, PasswordManagerState::Ready);
    assert_eq!(unlocked.message, None);
    let locked = managers.lock(&source_id).await.unwrap();
    assert_eq!(locked.state, PasswordManagerState::Locked);

    // Only KeePass and Bitwarden are unlocked from Muxus.
    let onepassword = managers
        .create(&PasswordManagerInput {
            kind: PasswordManagerKind::Onepassword,
            name: Some("Team".into()),
            config: PasswordManagerConfig::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        managers
            .unlock(&onepassword.id, "x")
            .await
            .unwrap_err()
            .to_string(),
        "Team is unlocked in its own app, not in Muxus."
    );
    // A wrong database password is reported as such.
    assert!(matches!(
        managers.unlock(&source_id, "wrong").await,
        Err(PasswordManagersError::Provider(error)) if error.problem == PasswordManagerProblem::InvalidUnlock
    ));
}

#[tokio::test]
async fn keeps_a_manager_unlocked_until_how_it_is_reached_changes() {
    let Setup {
        managers,
        source_id,
        web1,
        ..
    } = setup().await;
    managers
        .unlock(&source_id, DATABASE_PASSWORD)
        .await
        .unwrap();

    // A new name is the same database: it stays open.
    let renamed = managers
        .update(
            &source_id,
            &PasswordManagerInput {
                name: Some(" Work ".into()),
                ..keepass(&database())
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(renamed.name, "Work");
    assert_eq!(renamed.state, PasswordManagerState::Ready);
    // No name keeps the one it has.
    let kept = managers
        .update(&source_id, &keepass(&database()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.name, "Work");
    assert_eq!(managers.password(&web1).await.unwrap(), PASSWORD);

    // Another file is another database, and locked.
    let tmp = tempfile::tempdir().unwrap();
    let copy = tmp.path().join("copy.kdbx");
    std::fs::copy(database(), &copy).unwrap();
    let moved = managers
        .update(&source_id, &keepass(&copy))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.state, PasswordManagerState::Locked);
    assert!(matches!(
        managers.password(&web1).await,
        Err(PasswordManagersError::Provider(error)) if error.problem == PasswordManagerProblem::Locked
    ));

    assert_eq!(
        managers
            .update("missing", &keepass(&database()))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn forgets_what_unlocked_the_managers_when_disposed() {
    let Setup {
        managers,
        source_id,
        web1,
        ..
    } = setup().await;
    managers
        .unlock(&source_id, DATABASE_PASSWORD)
        .await
        .unwrap();
    managers.dispose();
    assert!(matches!(
        managers.password(&web1).await,
        Err(PasswordManagersError::Provider(error)) if error.problem == PasswordManagerProblem::Locked
    ));
}

#[tokio::test]
async fn leaves_out_a_kind_a_newer_build_stored_and_says_so_when_it_is_used() {
    let Setup {
        managers, store, ..
    } = setup().await;
    store.rows.lock().unwrap().push(StoredSource {
        id: "future".into(),
        kind: "enpass".into(),
        name: "Enpass".into(),
        config: PasswordManagerConfig::default(),
        created_at: "2026-01-01 00:00:00".into(),
        updated_at: "2026-01-01 00:00:00".into(),
    });
    let listed = managers.list().await.unwrap();
    assert!(listed.sources.iter().all(|source| source.id != "future"));

    let entry = ExternalPasswordRef {
        source_id: "future".into(),
        entry_id: "x".into(),
        title: "router".into(),
        username: None,
    };
    let io = TestIo::default();
    assert_eq!(managers.password_for(&entry, &io).await, Ok(None));
    assert_eq!(
        io.said(),
        "Could not read “router” from Enpass: Muxus does not know the password manager “enpass”. Enter the password."
    );
    assert_eq!(
        managers
            .entries("future", false)
            .await
            .unwrap_err()
            .to_string(),
        "Muxus does not know the password manager “enpass”."
    );
}
