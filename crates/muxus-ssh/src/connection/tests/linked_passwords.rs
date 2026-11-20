//! Logging in with a password-manager entry: where it stands among the saved passwords
//! (the ladder side of tests/unit/server/password-manager-login.test.ts; the managers
//! themselves are tested in muxus-passwords, and the two together in muxus-session).

use russh::MethodKind;

use super::*;
use crate::connection::io::{ExternalPasswordRef, LinkedPasswords};
use crate::connection::leases::ConnectionLeaseOwner;
use crate::connection::manager::{ConnectOptions, ManagerOptions};
use crate::connection::profile::SshProfile;
use crate::folder_auth::{
    FolderAuthDefaults, FolderPasswordRef, folder_password_account, ssh_password_account,
};
use crate::testing::server::{ServerSpec, TestServer, start};

const PASSWORD: &str = "fallback-password";

/// Password-manager entries by ID. One that is not there cannot be read, which is said
/// through the connection as the real managers do.
#[derive(Default)]
struct FakeManagers {
    entries: Mutex<HashMap<String, String>>,
    /// The entries a login asked for, in order.
    asked: Mutex<Vec<String>>,
    /// The unlock prompt was dismissed.
    cancel: Mutex<bool>,
    /// How long a read takes, as one waiting on an approval would.
    delay: Mutex<Option<Duration>>,
}

impl FakeManagers {
    fn holding(entries: &[(&str, &str)]) -> Arc<Self> {
        let managers = Self::default();
        *managers.entries.lock().unwrap() = entries
            .iter()
            .map(|(id, password)| ((*id).to_owned(), (*password).to_owned()))
            .collect();
        Arc::new(managers)
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl LinkedPasswords for FakeManagers {
    fn password_for<'a>(
        &'a self,
        entry: &'a ExternalPasswordRef,
        io: Arc<dyn ConnectIo>,
    ) -> BoxFuture<'a, Result<Option<String>, Cancelled>> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(entry.entry_id.clone());
            let delay = *self.delay.lock().unwrap();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            if *self.cancel.lock().unwrap() {
                return Err(Cancelled);
            }
            let password = self.entries.lock().unwrap().get(&entry.entry_id).cloned();
            match &password {
                Some(_) => io.status(
                    &format!("Using the password of “{}” from Fake.", entry.title),
                    true,
                ),
                None => io.status(
                    &format!(
                        "Could not read “{}” from Fake: gone. Enter the password.",
                        entry.title
                    ),
                    false,
                ),
            }
            Ok(password)
        })
    }
}

fn entry(id: &str) -> ExternalPasswordRef {
    ExternalPasswordRef {
        source_id: "source-1".into(),
        entry_id: id.into(),
        title: id.into(),
        username: None,
    }
}

async fn server() -> TestServer {
    start(
        ServerSpec::new()
            .password(PASSWORD)
            .methods(&[MethodKind::Password]),
    )
    .await
}

fn lab(sandbox: &Sandbox, port: u16, extra: &[&str]) {
    let port_line = format!("  Port {port}");
    let mut lines = vec![
        "Host lab",
        "  HostName 127.0.0.1",
        "  User tester",
        &port_line,
        "  StrictHostKeyChecking accept-new",
    ];
    lines.extend_from_slice(extra);
    sandbox.write_config(&lines);
}

/// Options where the alias "lab" is linked to `host_entry` and sits in folders pointing at
/// `folder_entries`, nearest first (`None` is a folder without an entry of its own).
fn options(
    sandbox: &Sandbox,
    managers: Option<Arc<FakeManagers>>,
    host_entry: Option<&str>,
    folder_entries: &[Option<&str>],
) -> ManagerOptions {
    let mut options = sandbox.options();
    options.password_managers = managers.map(|managers| managers as Arc<dyn LinkedPasswords>);
    let host_entry = host_entry.map(entry);
    options.password_ref_for_host = Some(Arc::new(move |alias: &str| {
        (alias == "lab").then(|| host_entry.clone()).flatten()
    }));
    let passwords: Vec<FolderPasswordRef> = folder_entries
        .iter()
        .enumerate()
        .map(|(index, folder_entry)| FolderPasswordRef {
            account: folder_password_account(&format!("row-{index}")),
            label: format!("Folder {index}"),
            entry: folder_entry.map(entry),
        })
        .collect();
    if !passwords.is_empty() {
        options.folder_auth = Some(Arc::new(move |alias: &str| {
            (alias == "lab").then(|| FolderAuthDefaults {
                option_lines: Vec::new(),
                passwords: passwords.clone(),
            })
        }));
    }
    options
}

async fn connect(
    options: ManagerOptions,
    io: Arc<TestIo>,
) -> Result<crate::connection::MuxedConnectionLease, crate::connection::ConnectFailure> {
    SshConnectionManager::new(options)
        .connect(
            &SshProfile::new("lab"),
            io,
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
}

/// What the server made of each password it was offered.
fn offered(server: &TestServer) -> Vec<&'static str> {
    server
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

#[tokio::test]
async fn logs_in_with_the_hosts_linked_entry_without_prompting() {
    let server = server().await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let managers = FakeManagers::holding(&[("web1", PASSWORD)]);
    let io = TestIo::accepting("never typed");
    connect(
        options(&sandbox, Some(managers.clone()), Some("web1"), &[]),
        io.clone(),
    )
    .await
    .unwrap();
    assert_eq!(io.prompt_count(), 0);
    assert_eq!(offered(&server), ["accepted"]);
    assert_eq!(managers.asked(), ["web1"]);
    assert!(
        io.statuses()
            .iter()
            .any(|status| status == "Using the password of “web1” from Fake.")
    );
}

#[tokio::test]
async fn prefers_the_hosts_entry_over_its_saved_password_and_the_folders() {
    let server = server().await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let managers = FakeManagers::holding(&[("web1", PASSWORD), ("folder", "folder-password")]);
    let vault = MemoryVault::unlocked();
    vault.store(
        &ssh_password_account("tester", "127.0.0.1", server.port()),
        "saved-host-password",
    );
    let mut options = options(
        &sandbox,
        Some(managers.clone()),
        Some("web1"),
        &[Some("folder")],
    );
    options.vault = Some(vault);
    let io = TestIo::accepting("never typed");
    connect(options, io.clone()).await.unwrap();
    assert_eq!(io.prompt_count(), 0);
    assert_eq!(offered(&server), ["accepted"]);
    // Nothing after the first password that could be read was looked at.
    assert_eq!(managers.asked(), ["web1"]);
}

#[tokio::test]
async fn leaves_the_next_saved_password_its_turn_when_an_entry_cannot_be_read() {
    let server = server().await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    // The host's entry is gone from its manager; its vault password still works.
    let managers = FakeManagers::holding(&[]);
    let vault = MemoryVault::unlocked();
    vault.store(
        &ssh_password_account("tester", "127.0.0.1", server.port()),
        PASSWORD,
    );
    let mut options = options(&sandbox, Some(managers.clone()), Some("gone"), &[]);
    options.vault = Some(vault);
    let io = TestIo::accepting("never typed");
    connect(options, io.clone()).await.unwrap();
    assert_eq!(io.prompt_count(), 0);
    assert_eq!(offered(&server), ["accepted"]);
    assert_eq!(managers.asked(), ["gone"]);
    let statuses = io.statuses();
    let unreadable = statuses
        .iter()
        .position(|status| status.starts_with("Could not read “gone”"))
        .expect("the unreadable entry is said");
    let saved = statuses
        .iter()
        .position(|status| status.starts_with("Using the saved password for "))
        .expect("the saved password is said");
    assert!(unreadable < saved);
}

#[tokio::test]
async fn uses_the_folders_entry_for_a_host_without_one_nearest_folder_first() {
    let server = server().await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let managers = FakeManagers::holding(&[("outer", PASSWORD)]);
    let vault = MemoryVault::unlocked();
    // A folder pointing at an entry uses it, not an older vault copy of its password.
    vault.store(&folder_password_account("row-0"), "stale-vault-copy");
    let mut options = options(
        &sandbox,
        Some(managers.clone()),
        None,
        &[Some("inner"), None, Some("outer")],
    );
    options.vault = Some(vault);
    let io = TestIo::accepting("never typed");
    connect(options, io.clone()).await.unwrap();
    assert_eq!(io.prompt_count(), 0);
    assert_eq!(offered(&server), ["accepted"]);
    // The inner folder's entry could not be read; the middle folder has no password at all.
    assert_eq!(managers.asked(), ["inner", "outer"]);
}

#[tokio::test]
async fn falls_back_to_a_folders_vault_password_after_an_unreadable_entry() {
    let server = server().await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let managers = FakeManagers::holding(&[]);
    let vault = MemoryVault::unlocked();
    vault.store(&folder_password_account("row-1"), PASSWORD);
    let mut options = options(
        &sandbox,
        Some(managers.clone()),
        None,
        &[Some("inner"), None],
    );
    options.vault = Some(vault);
    let io = TestIo::accepting("never typed");
    connect(options, io.clone()).await.unwrap();
    assert_eq!(io.prompt_count(), 0);
    assert_eq!(offered(&server), ["accepted"]);
    assert!(
        io.statuses()
            .iter()
            .any(|status| status == "Using the saved password for Folder 1.")
    );
}

#[tokio::test]
async fn asks_for_the_password_when_the_entry_holds_one_the_server_rejects() {
    let server = server().await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let managers = FakeManagers::holding(&[("router", "router-pw"), ("folder", PASSWORD)]);
    let io = TestIo::accepting(PASSWORD);
    connect(
        options(
            &sandbox,
            Some(managers.clone()),
            Some("router"),
            &[Some("folder")],
        ),
        io.clone(),
    )
    .await
    .unwrap();
    // One saved password is tried; after that the person is asked.
    assert_eq!(managers.asked(), ["router"]);
    assert_eq!(offered(&server), ["rejected", "accepted"]);
    assert_eq!(io.prompt_count(), 1);
    let prompt = io.prompts.lock().unwrap()[0].clone();
    assert_eq!(
        prompt.purpose,
        Some(crate::connection::AuthPromptPurpose::SshPassword)
    );
    assert_eq!(
        prompt.instructions.as_deref(),
        Some("The saved password was unavailable or was not accepted. Enter the current password.")
    );
}

#[tokio::test]
async fn asks_for_the_password_when_no_saved_password_can_be_read() {
    let server = server().await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let managers = FakeManagers::holding(&[]);
    let io = TestIo::accepting(PASSWORD);
    connect(
        options(
            &sandbox,
            Some(managers.clone()),
            Some("gone"),
            &[Some("also-gone")],
        ),
        io.clone(),
    )
    .await
    .unwrap();
    assert_eq!(managers.asked(), ["gone", "also-gone"]);
    assert_eq!(offered(&server), ["accepted"]);
    assert_eq!(io.prompt_count(), 1);
}

#[tokio::test]
async fn gives_the_login_up_when_the_unlock_prompt_is_dismissed() {
    let server = server().await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let managers = FakeManagers::holding(&[("web1", PASSWORD)]);
    *managers.cancel.lock().unwrap() = true;
    let io = TestIo::accepting(PASSWORD);
    let Err(error) = connect(
        options(&sandbox, Some(managers), Some("web1"), &[Some("folder")]),
        io.clone(),
    )
    .await
    else {
        panic!("the login went on after its prompt was dismissed");
    };
    assert_eq!(error.to_string(), "authentication cancelled");
    assert_eq!(io.prompt_count(), 0);
    assert!(offered(&server).is_empty());
}

#[tokio::test]
async fn pauses_the_connect_timeout_while_a_password_manager_is_read() {
    let server = server().await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &["  ConnectTimeout 1"]);
    let managers = FakeManagers::holding(&[("web1", PASSWORD)]);
    // An approval the person takes their time with.
    *managers.delay.lock().unwrap() = Some(Duration::from_millis(1_500));
    let io = TestIo::accepting("never typed");
    connect(
        options(&sandbox, Some(managers), Some("web1"), &[]),
        io.clone(),
    )
    .await
    .unwrap();
    assert_eq!(io.prompt_count(), 0);
}

#[tokio::test]
async fn leaves_linked_entries_alone_without_password_managers() {
    let server = server().await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let vault = MemoryVault::unlocked();
    vault.store(&folder_password_account("row-0"), PASSWORD);
    // A folder with an entry still has its vault password to fall back to.
    let mut options = options(&sandbox, None, Some("web1"), &[Some("folder")]);
    options.vault = Some(vault);
    let io = TestIo::accepting("never typed");
    connect(options, io.clone()).await.unwrap();
    assert_eq!(io.prompt_count(), 0);
    assert_eq!(offered(&server), ["accepted"]);
}
