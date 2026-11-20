//! Ports of tests/unit/server/password-manager-routes.test.ts and
//! password-manager-login.test.ts: the routes as `Backend` methods, and SSH logins that
//! read a KeePass entry, against the in-process test server of `muxus_ssh::testing`.

mod support;

use std::path::{Path, PathBuf};

use muxus_core::api::{
    ExternalPasswordRef, OpenSshMetadataPatch, PasswordManagerKind, PasswordManagerSource,
    PasswordManagerState,
};
use muxus_core::js::Patch;
use muxus_core::protocol::{
    AuthPromptInfo, AuthPromptPurpose, AuthPromptResponse, TerminalClientMessage,
    TerminalServerMessage,
};
use muxus_session::{SessionEvent, SessionHandle};
use muxus_ssh::testing::server::{ServerSpec, TestServer, start as start_sshd};
use serde_json::json;
use support::{Sandbox, body, next_event, start};

/// Written by KeePassXC; "Servers/web1" holds the password the test server accepts, and
/// "Core router" holds `router-pw`.
fn database() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../muxus-passwords/tests/fixtures/keepassxc-kdbx3.kdbx")
}
const DATABASE_PASSWORD: &str = "master-pw-1";
const PASSWORD: &str = "hunter2-säcret";

fn keepass(path: &Path) -> serde_json::Value {
    json!({ "kind": "keepass", "config": { "path": path.to_string_lossy() } })
}

// --- routes ----------------------------------------------------------------------------------

#[tokio::test]
async fn connects_a_keepass_database_unlocks_it_and_lists_entries_without_passwords() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let empty = backend.password_managers_list().await.unwrap();
    assert!(empty.sources.is_empty());
    // KeePass needs no program, so it is always offered.
    assert!(empty.installed.contains(&PasswordManagerKind::Keepass));

    let refused = backend
        .create_password_manager(body(json!({ "kind": "keepass", "config": {} })))
        .await
        .unwrap_err();
    assert_eq!(
        (refused.status, refused.message.as_str()),
        (400, "Choose the KeePass database file.")
    );
    let too_long = backend
        .create_password_manager(body(json!({
            "kind": "bitwarden", "name": "x".repeat(201), "config": {},
        })))
        .await
        .unwrap_err();
    assert_eq!(
        (too_long.status, too_long.message.as_str()),
        (400, "A valid password manager is required.")
    );

    let source = backend
        .create_password_manager(body(keepass(&database())))
        .await
        .unwrap();
    assert_eq!(source.kind, PasswordManagerKind::Keepass);
    assert_eq!(source.name, "KeePass – keepassxc-kdbx3");
    assert_eq!(source.state, PasswordManagerState::Locked);

    let locked = backend
        .password_manager_entries(&source.id, false)
        .await
        .unwrap_err();
    assert_eq!(locked.status, 423);
    assert_eq!(locked.code.as_deref(), Some("password-manager-locked"));

    let wrong = backend
        .unlock_password_manager(&source.id, "nope")
        .await
        .unwrap_err();
    assert_eq!(wrong.status, 422);
    assert_eq!(
        wrong.code.as_deref(),
        Some("password-manager-invalid-unlock")
    );
    let unlocked = backend
        .unlock_password_manager(&source.id, DATABASE_PASSWORD)
        .await
        .unwrap();
    assert_eq!(unlocked.state, PasswordManagerState::Ready);

    let entries = backend
        .password_manager_entries(&source.id, false)
        .await
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.title.as_str())
            .collect::<Vec<_>>(),
        ["Core router", "web1"]
    );
    let listed = serde_json::to_string(&entries).unwrap();
    assert!(!listed.contains("hunter2"));
    assert!(!listed.contains("router-pw"));
    // Asking for changes first is the same listing for a file on disk.
    assert_eq!(
        backend
            .password_manager_entries(&source.id, true)
            .await
            .unwrap(),
        entries
    );

    assert_eq!(
        backend
            .lock_password_manager(&source.id)
            .await
            .unwrap()
            .state,
        PasswordManagerState::Locked
    );
    assert!(backend.delete_password_manager(&source.id).await.unwrap());
    assert!(!backend.delete_password_manager(&source.id).await.unwrap());
    let gone = backend
        .password_manager_entries(&source.id, false)
        .await
        .unwrap_err();
    assert_eq!(gone.status, 404);
    assert_eq!(gone.code.as_deref(), Some("password-manager-missing"));
    assert_eq!(gone.message, "The password manager is no longer connected.");
    let gone = backend
        .update_password_manager(&source.id, body(keepass(&database())))
        .await
        .unwrap_err();
    assert_eq!(gone.code.as_deref(), Some("password-manager-missing"));
    sandbox.stop().await;
}

#[tokio::test]
async fn locks_a_source_again_when_its_settings_change() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let source = backend
        .create_password_manager(body(keepass(&database())))
        .await
        .unwrap();
    backend
        .unlock_password_manager(&source.id, DATABASE_PASSWORD)
        .await
        .unwrap();
    let renamed = backend
        .update_password_manager(
            &source.id,
            body(json!({
                "kind": "keepass", "name": "Work",
                "config": { "path": database().to_string_lossy() },
            })),
        )
        .await
        .unwrap();
    assert_eq!(
        (renamed.name.as_str(), renamed.state),
        ("Work", PasswordManagerState::Ready)
    );
    let moved = backend
        .update_password_manager(
            &source.id,
            body(json!({
                "kind": "keepass", "name": "Work",
                "config": { "path": database().to_string_lossy(), "keyFile": "/somewhere/else.keyx" },
            })),
        )
        .await
        .unwrap();
    assert_eq!(moved.state, PasswordManagerState::Locked);
    // The list says the same, and it is kept across a restart of the backend.
    let listed = backend.password_managers_list().await.unwrap();
    assert_eq!(listed.sources, std::slice::from_ref(&moved));
    let Sandbox { dir, backend } = sandbox;
    backend.shutdown().await;
    let again = support::start_in(dir, |_| {}).await;
    let listed = again.backend.password_managers_list().await.unwrap();
    assert_eq!(listed.sources, [moved]);
    again.stop().await;
}

#[tokio::test]
async fn expands_the_home_directory_of_the_backend_in_a_database_path() {
    let sandbox = start().await;
    std::fs::copy(database(), sandbox.home().join("Passwords.kdbx")).unwrap();
    let source = sandbox
        .backend
        .create_password_manager(body(json!({
            "kind": "keepass", "config": { "path": " ~/Passwords.kdbx " },
        })))
        .await
        .unwrap();
    // The path is kept as it was typed, less the blanks around it.
    assert_eq!(source.config.path.as_deref(), Some("~/Passwords.kdbx"));
    assert_eq!(source.name, "KeePass – Passwords");
    assert_eq!(source.state, PasswordManagerState::Locked);
    sandbox.stop().await;
}

#[tokio::test]
async fn stores_a_folder_entry_as_a_reference_and_keeps_the_folder_row_for_it_alone() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let entry = json!({ "sourceId": "source-1", "entryId": "entry-1", "title": "web1", "username": "root" });
    let mut with_password = entry.clone();
    with_password["password"] = json!("must not be stored");
    let saved = backend
        .put_folder_settings(body(json!({
            "path": "Lab", "auth": { "passwordRef": with_password },
        })))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&saved.auth).unwrap(),
        json!({ "passwordRef": entry })
    );
    let too_long = backend
        .put_folder_settings(body(json!({
            "path": "Lab",
            "auth": { "passwordRef": { "sourceId": "s", "entryId": "e".repeat(2049), "title": "" } },
        })))
        .await
        .unwrap_err();
    assert_eq!(too_long.status, 400);
    let cleared = backend
        .put_folder_settings(body(json!({ "path": "Lab", "auth": {} })))
        .await
        .unwrap();
    assert_eq!(cleared, None);
    sandbox.stop().await;
}

#[tokio::test]
async fn links_and_unlinks_a_hosts_entry_through_its_metadata() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    sandbox.write_ssh_config(&["Host lab", "  HostName 127.0.0.1"]);
    let entry = ExternalPasswordRef {
        source_id: "source-1".into(),
        entry_id: "entry-1".into(),
        title: "web1".into(),
        username: Some("root".into()),
    };
    let linked = backend
        .patch_ssh_host_metadata(
            "lab",
            OpenSshMetadataPatch {
                password_ref: Patch::Set(entry.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(linked.password_ref, Some(entry.clone()));
    // Other edits leave the link alone.
    let recolored = backend
        .patch_ssh_host_metadata("lab", body(json!({ "color": "#112233" })))
        .await
        .unwrap();
    assert_eq!(recolored.password_ref, Some(entry));
    let unlinked = backend
        .patch_ssh_host_metadata("lab", body(json!({ "passwordRef": null })))
        .await
        .unwrap();
    assert_eq!(unlinked.password_ref, None);
    // A reference without an entry is not one.
    let refused = backend
        .patch_ssh_host_metadata(
            "lab",
            body(json!({ "passwordRef": { "sourceId": "s", "entryId": "", "title": "" } })),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.status, 400);

    // A saved host keeps its entry the same way.
    let saved = backend
        .save_saved_host(body(json!({
            "name": "Build server",
            "profile": { "kind": "rdp", "host": "win-build", "port": 3389, "username": "ci" },
        })))
        .await
        .unwrap();
    let patched = backend
        .patch_saved_host_metadata(
            &saved.id,
            body(json!({
                "passwordRef": { "sourceId": "source-1", "entryId": "entry-2", "title": "ci" },
            })),
        )
        .await
        .unwrap();
    assert_eq!(
        patched.metadata.password_ref.map(|entry| entry.entry_id),
        Some("entry-2".to_owned())
    );
    sandbox.stop().await;
}

// --- logging in with a password-manager entry -------------------------------------------------

struct Lab {
    sandbox: Sandbox,
    server: TestServer,
    source: PasswordManagerSource,
    /// "Servers/web1", whose password the server accepts.
    web1: ExternalPasswordRef,
    /// "Core router", whose password it rejects.
    router: ExternalPasswordRef,
}

/// A password-only sshd named `lab` in ssh_config and in the folder "Lab", and the KeePass
/// database connected and locked.
async fn lab() -> Lab {
    let server = start_sshd(
        ServerSpec::new()
            .password(PASSWORD)
            .methods(&[russh::MethodKind::Password]),
    )
    .await;
    let sandbox = start().await;
    let port = format!("  Port {}", server.port());
    sandbox.write_ssh_config(&[
        "Host lab",
        "  HostName 127.0.0.1",
        "  User tester",
        port.as_str(),
        "  StrictHostKeyChecking no",
    ]);
    let backend = &sandbox.backend;
    let source = backend
        .create_password_manager(body(keepass(&database())))
        .await
        .unwrap();
    backend
        .unlock_password_manager(&source.id, DATABASE_PASSWORD)
        .await
        .unwrap();
    let entries = backend
        .password_manager_entries(&source.id, false)
        .await
        .unwrap();
    let reference = |title: &str| {
        let entry = entries.iter().find(|entry| entry.title == title).unwrap();
        ExternalPasswordRef {
            source_id: source.id.clone(),
            entry_id: entry.id.clone(),
            title: entry.title.clone(),
            username: entry.username.clone(),
        }
    };
    let (web1, router) = (reference("web1"), reference("Core router"));
    backend.lock_password_manager(&source.id).await.unwrap();
    backend
        .patch_ssh_host_metadata("lab", body(json!({ "group": "Lab" })))
        .await
        .unwrap();
    Lab {
        sandbox,
        server,
        source,
        web1,
        router,
    }
}

impl Lab {
    async fn link_host(&self, entry: &ExternalPasswordRef) {
        self.sandbox
            .backend
            .patch_ssh_host_metadata(
                "lab",
                OpenSshMetadataPatch {
                    password_ref: Patch::Set(entry.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    async fn link_folder(&self, entry: &ExternalPasswordRef) {
        self.sandbox
            .backend
            .put_folder_settings(body(
                json!({ "path": "Lab", "auth": { "passwordRef": entry } }),
            ))
            .await
            .unwrap();
    }

    async fn unlock(&self) {
        self.sandbox
            .backend
            .unlock_password_manager(&self.source.id, DATABASE_PASSWORD)
            .await
            .unwrap();
    }

    /// What the server made of each password it was offered.
    fn offered(&self) -> Vec<&'static str> {
        self.server
            .auth_events()
            .into_iter()
            .filter_map(|event| {
                if event.starts_with("auth password:accepted") {
                    Some("accepted")
                } else if event.starts_with("auth password:rejected") {
                    Some("rejected")
                } else {
                    None
                }
            })
            .collect()
    }
}

/// What a login showed on the way to its shell.
#[derive(Default)]
struct Login {
    prompts: Vec<AuthPromptInfo>,
    statuses: Vec<String>,
}

impl Login {
    fn purposes(&self) -> Vec<Option<AuthPromptPurpose>> {
        self.prompts.iter().map(|prompt| prompt.purpose).collect()
    }

    fn said(&self) -> String {
        self.statuses.join("\n")
    }
}

/// How an unlock prompt is answered.
#[derive(Clone, Copy)]
enum Unlock {
    With(&'static str),
    /// "Use another password".
    Skip,
}

/// Connect to `lab`, answering unlock prompts from `unlock` in order and password prompts
/// with the password the server takes, until the session is ready.
async fn sign_in(lab: &Lab, unlock: &[Unlock]) -> (SessionHandle, Login) {
    let handle = lab.sandbox.backend.open_session(body(json!({
        "op": "connect", "profile": { "kind": "ssh", "target": "lab" }, "cols": 80, "rows": 24,
    })));
    let mut login = Login::default();
    let mut unlock = unlock.iter();
    loop {
        match next_event(&handle).await {
            SessionEvent::Message(TerminalServerMessage::HostKey(_)) => {
                handle.send(TerminalClientMessage::HostKeyResponse { accept: true });
            }
            SessionEvent::Message(TerminalServerMessage::Status { message, .. }) => {
                login.statuses.push(message);
            }
            SessionEvent::Message(TerminalServerMessage::AuthPrompt(info)) => {
                let response = if info.purpose == Some(AuthPromptPurpose::VaultUnlock) {
                    match unlock.next() {
                        Some(Unlock::With(password)) => AuthPromptResponse {
                            answers: vec![(*password).to_owned()],
                            ..Default::default()
                        },
                        Some(Unlock::Skip) => AuthPromptResponse {
                            skipped: Some(true),
                            ..Default::default()
                        },
                        None => panic!("an unlock prompt nobody expected: {info:?}"),
                    }
                } else {
                    AuthPromptResponse {
                        answers: vec![PASSWORD.to_owned(); info.prompts.len()],
                        ..Default::default()
                    }
                };
                login.prompts.push(info);
                handle.send(TerminalClientMessage::AuthResponse(response));
            }
            SessionEvent::Message(TerminalServerMessage::Ready { .. }) => return (handle, login),
            SessionEvent::Message(TerminalServerMessage::Exit { message, .. }) => {
                panic!("the session ended while connecting: {message:?}")
            }
            SessionEvent::Closed { reason } => panic!("closed while connecting: {reason:?}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn uses_the_host_entry_from_an_unlocked_database_without_any_prompt() {
    let lab = lab().await;
    lab.link_host(&lab.web1).await;
    lab.unlock().await;

    let (_handle, login) = sign_in(&lab, &[]).await;
    assert!(login.prompts.is_empty(), "{:?}", login.prompts);
    assert_eq!(lab.offered(), ["accepted"]);
    assert!(login.said().contains("“web1”"), "{}", login.said());
    lab.sandbox.stop().await;
}

#[tokio::test]
async fn asks_for_the_database_password_when_the_database_is_locked_retrying_a_wrong_one() {
    let lab = lab().await;
    lab.link_host(&lab.web1).await;

    let (handle, login) = sign_in(
        &lab,
        &[Unlock::With("not-it"), Unlock::With(DATABASE_PASSWORD)],
    )
    .await;
    assert_eq!(
        login.purposes(),
        [
            Some(AuthPromptPurpose::VaultUnlock),
            Some(AuthPromptPurpose::VaultUnlock)
        ]
    );
    assert_eq!(login.prompts[0].prompts.len(), 1);
    assert_eq!(login.prompts[0].prompts[0].prompt, "Database password");
    assert!(!login.prompts[0].prompts[0].echo);
    assert_eq!(
        login.prompts[0].skip_label.as_deref(),
        Some("Use another password")
    );
    assert!(
        login.prompts[1]
            .instructions
            .as_deref()
            .unwrap()
            .contains("The database password is wrong.")
    );
    assert_eq!(lab.offered(), ["accepted"]);

    // The database stays unlocked for the next connection.
    drop(handle);
    lab.sandbox.backend.connections().close_all();
    let (_again, login) = sign_in(&lab, &[]).await;
    assert!(login.prompts.is_empty(), "{:?}", login.prompts);
    lab.sandbox.stop().await;
}

#[tokio::test]
async fn falls_back_to_asking_for_the_password_when_unlocking_is_skipped() {
    let lab = lab().await;
    lab.link_host(&lab.web1).await;

    let (_handle, login) = sign_in(&lab, &[Unlock::Skip]).await;
    assert_eq!(
        login.purposes(),
        [
            Some(AuthPromptPurpose::VaultUnlock),
            Some(AuthPromptPurpose::SshPassword)
        ]
    );
    assert_eq!(lab.offered(), ["accepted"]);
    lab.sandbox.stop().await;
}

#[tokio::test]
async fn uses_the_folder_entry_for_a_host_without_one_of_its_own() {
    let lab = lab().await;
    lab.link_folder(&lab.web1).await;
    lab.unlock().await;

    let (_handle, login) = sign_in(&lab, &[]).await;
    assert!(login.prompts.is_empty(), "{:?}", login.prompts);
    assert_eq!(lab.offered(), ["accepted"]);
    lab.sandbox.stop().await;
}

#[tokio::test]
async fn prefers_the_host_entry_over_the_folder_entry() {
    let lab = lab().await;
    lab.unlock().await;
    lab.link_folder(&lab.router).await;
    lab.link_host(&lab.web1).await;

    let (_handle, login) = sign_in(&lab, &[]).await;
    assert!(login.prompts.is_empty(), "{:?}", login.prompts);
    assert_eq!(lab.offered(), ["accepted"]);
    lab.sandbox.stop().await;
}

#[tokio::test]
async fn reports_an_entry_or_a_password_manager_that_is_gone_and_asks_instead() {
    let lab = lab().await;
    lab.unlock().await;
    lab.link_host(&ExternalPasswordRef {
        entry_id: "AAAAAAAAAAAAAAAAAAAAAA==".into(),
        ..lab.web1.clone()
    })
    .await;
    let (handle, missing_entry) = sign_in(&lab, &[]).await;
    assert_eq!(
        missing_entry.purposes(),
        [Some(AuthPromptPurpose::SshPassword)]
    );
    assert!(
        missing_entry
            .said()
            .contains("no longer in the KeePass database"),
        "{}",
        missing_entry.said()
    );

    drop(handle);
    lab.sandbox.backend.connections().close_all();
    lab.link_host(&lab.web1).await;
    assert!(
        lab.sandbox
            .backend
            .delete_password_manager(&lab.source.id)
            .await
            .unwrap()
    );
    let (_handle, missing_source) = sign_in(&lab, &[]).await;
    assert_eq!(
        missing_source.purposes(),
        [Some(AuthPromptPurpose::SshPassword)]
    );
    assert!(
        missing_source.said().contains("no longer connected"),
        "{}",
        missing_source.said()
    );
    assert_eq!(lab.offered(), ["accepted", "accepted"]);
    lab.sandbox.stop().await;
}

#[tokio::test]
async fn asks_for_the_password_when_the_entry_holds_one_the_server_rejects() {
    let lab = lab().await;
    lab.unlock().await;
    lab.link_host(&lab.router).await;

    let (_handle, login) = sign_in(&lab, &[]).await;
    assert_eq!(login.purposes(), [Some(AuthPromptPurpose::SshPassword)]);
    assert_eq!(lab.offered(), ["rejected", "accepted"]);
    lab.sandbox.stop().await;
}
