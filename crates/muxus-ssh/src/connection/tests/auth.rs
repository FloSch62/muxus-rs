//! Port of agent-auth.test.ts, folder-password-auth.test.ts and the password-memory cases of
//! connection-session.test.ts.

use std::sync::atomic::Ordering;

use russh::MethodKind;
use russh::keys::ssh_key::LineEnding;

use super::*;
use crate::connection::leases::ConnectionLeaseOwner;
use crate::connection::manager::{ConnectOptions, ManagerOptions};
use crate::connection::profile::SshProfile;
use crate::folder_auth::{
    FolderAuthDefaults, FolderPasswordRef, folder_password_account, ssh_password_account,
};
use crate::testing::agent::{ed25519, start_agent, start_hanging_agent};
use crate::testing::server::{KbdRound, ServerSpec, start};

const PASSWORD: &str = "fallback-password";
const PASSPHRASE: &str = "key-passphrase";

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

fn with_agent(options: &mut ManagerOptions, socket: &std::path::Path) {
    let socket = socket.to_string_lossy().into_owned();
    options.env_lookup =
        Arc::new(move |name: &str| (name == "SSH_AUTH_SOCK").then(|| socket.clone()));
}

async fn connect(
    manager: &SshConnectionManager,
    io: Arc<TestIo>,
) -> Result<crate::connection::MuxedConnectionLease, crate::connection::ConnectFailure> {
    manager
        .connect(
            &SshProfile::new("lab"),
            io,
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
}

fn write_key(path: &std::path::Path, key: &russh::keys::PrivateKey, passphrase: Option<&str>) {
    let key = match passphrase {
        Some(p) => key.encrypt(&mut russh::keys::key::safe_rng(), p).unwrap(),
        None => key.clone(),
    };
    std::fs::write(path, key.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
}

#[tokio::test]
async fn authenticates_with_a_key_held_only_by_the_agent_without_any_prompt() {
    let key = ed25519();
    let mut spec = ServerSpec::new().password(PASSWORD);
    spec.authorized_keys = vec![key.public_key().clone()];
    let server = start(spec).await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let sock = start_agent(sandbox.dir.path(), std::slice::from_ref(&key)).await;
    let mut options = sandbox.options();
    with_agent(&mut options, &sock);
    let io = TestIo::accepting(PASSWORD);
    let lease = connect(&SshConnectionManager::new(options), io.clone())
        .await
        .unwrap();
    assert_eq!(io.prompt_count(), 0);
    assert_eq!(
        lease.connection().facts.handshake.auth_methods,
        vec!["agent".to_string()]
    );
    assert_eq!(
        lease
            .connection()
            .facts
            .handshake
            .auth_key_algorithm
            .as_deref(),
        Some("ssh-ed25519")
    );
    assert!(
        server
            .auth_events()
            .iter()
            .any(|e| e.starts_with("auth publickey:accepted"))
    );
}

#[tokio::test]
async fn silently_falls_back_when_the_agent_socket_is_unreachable() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let mut options = sandbox.options();
    with_agent(&mut options, &sandbox.dir.path().join("gone.sock"));
    let io = TestIo::accepting(PASSWORD);
    connect(&SshConnectionManager::new(options), io.clone())
        .await
        .unwrap();
    assert!(
        !io.statuses()
            .iter()
            .any(|s| s.contains("ssh-agent unavailable"))
    );
    assert_eq!(io.prompt_count(), 1);
}

#[tokio::test]
async fn does_not_contact_the_agent_for_identities_only_hosts() {
    let key = ed25519();
    let mut spec = ServerSpec::new().password(PASSWORD);
    spec.authorized_keys = vec![key.public_key().clone()];
    let server = start(spec).await;
    let sandbox = Sandbox::new();
    let key_file = sandbox.dir.path().join("agent-key");
    write_key(&key_file, &key, None);
    let identity_line = format!("  IdentityFile {}", key_file.display());
    lab(
        &sandbox,
        server.port(),
        &[&identity_line, "  IdentitiesOnly yes"],
    );
    let (sock, counters) =
        start_hanging_agent(sandbox.dir.path(), Some(key.public_key().clone()), false).await;
    let mut options = sandbox.options();
    with_agent(&mut options, &sock);
    let io = TestIo::accepting(PASSWORD);
    let lease = connect(&SshConnectionManager::new(options), io)
        .await
        .unwrap();
    assert_eq!(counters.identities.load(Ordering::SeqCst), 0);
    assert_eq!(counters.signs.load(Ordering::SeqCst), 0);
    assert_eq!(
        lease.connection().facts.handshake.auth_methods,
        vec!["publickey".to_string()]
    );
}

#[tokio::test]
async fn times_out_a_silent_identity_listing_and_falls_through_to_the_key_file() {
    let key = ed25519();
    let mut spec = ServerSpec::new().password(PASSWORD);
    spec.authorized_keys = vec![key.public_key().clone()];
    let server = start(spec).await;
    let sandbox = Sandbox::new();
    let key_file = sandbox.dir.path().join("agent-key");
    write_key(&key_file, &key, None);
    let identity_line = format!("  IdentityFile {}", key_file.display());
    lab(
        &sandbox,
        server.port(),
        &[&identity_line, "  ConnectTimeout 1"],
    );
    let (sock, counters) = start_hanging_agent(sandbox.dir.path(), None, true).await;
    let mut options = sandbox.options();
    with_agent(&mut options, &sock);
    // Deliberately longer than ConnectTimeout: agent approval time must pause the network
    // deadline, then fall through cleanly.
    options.agent_operation_timeout = Duration::from_millis(1_200);
    options.agent_wait_status = Some(Duration::from_millis(20));
    let io = TestIo::accepting(PASSWORD);
    let started = std::time::Instant::now();
    let lease = connect(&SshConnectionManager::new(options), io.clone())
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(1_100), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5));
    assert_eq!(counters.identities.load(Ordering::SeqCst), 1);
    assert!(
        io.statuses()
            .iter()
            .any(|s| s.contains("Waiting for the SSH agent"))
    );
    assert!(io.statuses().iter().any(|s| s.contains("did not respond")));
    assert_eq!(
        lease.connection().facts.handshake.auth_methods,
        vec!["publickey".to_string()]
    );
}

#[tokio::test]
async fn times_out_a_silent_agent_signature_and_falls_through_to_the_key_file() {
    let key = ed25519();
    let mut spec = ServerSpec::new().password(PASSWORD);
    spec.authorized_keys = vec![key.public_key().clone()];
    let server = start(spec).await;
    let sandbox = Sandbox::new();
    let key_file = sandbox.dir.path().join("agent-key");
    write_key(&key_file, &key, None);
    let identity_line = format!("  IdentityFile {}", key_file.display());
    lab(&sandbox, server.port(), &[&identity_line]);
    let (sock, counters) =
        start_hanging_agent(sandbox.dir.path(), Some(key.public_key().clone()), false).await;
    let mut options = sandbox.options();
    with_agent(&mut options, &sock);
    options.agent_operation_timeout = Duration::from_millis(100);
    options.agent_wait_status = Some(Duration::from_millis(10));
    let io = TestIo::accepting(PASSWORD);
    let lease = connect(&SshConnectionManager::new(options), io.clone())
        .await
        .unwrap();
    assert_eq!(counters.identities.load(Ordering::SeqCst), 1);
    assert_eq!(counters.signs.load(Ordering::SeqCst), 1);
    assert!(io.statuses().iter().any(|s| s.contains("approve signing")));
    assert!(io.statuses().iter().any(|s| s.contains("did not respond")));
    assert_eq!(
        lease.connection().facts.handshake.auth_methods,
        vec!["publickey".to_string()]
    );
}

#[tokio::test]
async fn prompts_for_the_passphrase_of_a_default_key_the_agent_does_not_hold() {
    let key = ed25519();
    let mut spec = ServerSpec::new().password(PASSWORD);
    spec.authorized_keys = vec![key.public_key().clone()];
    let server = start(spec).await;
    let sandbox = Sandbox::new();
    write_key(
        &sandbox.env.ssh_dir().join("id_ed25519"),
        &key,
        Some(PASSPHRASE),
    );
    lab(&sandbox, server.port(), &[]);
    let io = TestIo::accepting(PASSPHRASE);
    let lease = connect(&sandbox.manager(), io.clone()).await.unwrap();
    let prompts = io.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].prompts[0].prompt, "Passphrase for id_ed25519");
    assert_eq!(
        lease.connection().facts.handshake.auth_methods,
        vec!["publickey".to_string()]
    );
}

#[tokio::test]
async fn never_asks_for_the_passphrase_of_a_default_key_the_agent_already_holds() {
    let key = ed25519();
    // The server rejects every public key, so the ladder walks past both the agent and the
    // identity files before landing on the password.
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    let disk = sandbox.env.ssh_dir().join("id_ed25519");
    write_key(&disk, &key, Some(PASSPHRASE));
    std::fs::write(
        sandbox.env.ssh_dir().join("id_ed25519.pub"),
        key.public_key().to_openssh().unwrap(),
    )
    .unwrap();
    lab(&sandbox, server.port(), &[]);
    let sock = start_agent(sandbox.dir.path(), std::slice::from_ref(&key)).await;
    let mut options = sandbox.options();
    with_agent(&mut options, &sock);
    let io = TestIo::accepting(PASSWORD);
    connect(&SshConnectionManager::new(options), io.clone())
        .await
        .unwrap();
    let prompts = io.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].prompts[0].prompt.ends_with("'s password"));
    assert!(
        server
            .auth_events()
            .iter()
            .any(|e| e.starts_with("auth password:accepted"))
    );
}

#[tokio::test]
async fn tries_a_rejected_passphrase_again_then_skips_the_key_on_an_empty_answer() {
    let key = ed25519();
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    let key_file = sandbox.dir.path().join("locked");
    write_key(&key_file, &key, Some(PASSPHRASE));
    let identity_line = format!("  IdentityFile {}", key_file.display());
    lab(&sandbox, server.port(), &[&identity_line]);
    let io = TestIo::accepting(PASSWORD);
    io.answers.lock().unwrap().extend([
        crate::connection::io::answers(["wrong"]),
        crate::connection::io::answers([""]),
    ]);
    connect(&sandbox.manager(), io.clone()).await.unwrap();
    let prompts: Vec<String> = io
        .prompts
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.prompts[0].prompt.clone())
        .collect();
    assert_eq!(prompts[0], "Passphrase for locked");
    assert_eq!(
        prompts[1],
        "Bad passphrase, try again. Passphrase for locked"
    );
    assert!(prompts[2].ends_with("'s password"));
}

#[tokio::test]
async fn logs_in_with_a_certificate_and_its_identity_file() {
    use russh::keys::ssh_key::certificate::{Builder, CertType};
    let user_key = ed25519();
    let ca = ed25519();
    let mut builder = Builder::new_with_random_nonce(
        &mut russh::keys::key::safe_rng(),
        user_key.public_key().key_data().clone(),
        0,
        u64::MAX - 1,
    )
    .unwrap();
    builder.cert_type(CertType::User).unwrap();
    builder.valid_principal("tester").unwrap();
    let certificate = builder.sign(&ca).unwrap();
    let mut spec = ServerSpec::new().password(PASSWORD);
    spec.authorized_keys = vec![user_key.public_key().clone()];
    let server = start(spec).await;
    let sandbox = Sandbox::new();
    let key_file = sandbox.dir.path().join("id_cert");
    write_key(&key_file, &user_key, None);
    let cert_file = sandbox.dir.path().join("id_cert-cert.pub");
    std::fs::write(&cert_file, certificate.to_openssh().unwrap()).unwrap();
    let id_line = format!("  IdentityFile {}", key_file.display());
    let cert_line = format!("  CertificateFile {}", cert_file.display());
    lab(&sandbox, server.port(), &[&id_line, &cert_line]);
    let io = TestIo::accepting(PASSWORD);
    let lease = connect(&sandbox.manager(), io.clone()).await.unwrap();
    assert_eq!(io.prompt_count(), 0);
    assert!(
        server
            .auth_events()
            .iter()
            .any(|e| e == "auth certificate:accepted"),
        "{:?}",
        server.auth_events()
    );
    assert_eq!(
        lease
            .connection()
            .facts
            .handshake
            .auth_key_algorithm
            .as_deref(),
        Some("ssh-ed25519-cert-v01@openssh.com")
    );
}

#[tokio::test]
async fn lists_every_method_of_a_multi_factor_login_in_order() {
    let key = ed25519();
    let mut spec = ServerSpec::new().password(PASSWORD);
    spec.authorized_keys = vec![key.public_key().clone()];
    spec.require_all = vec![MethodKind::PublicKey, MethodKind::Password];
    let server = start(spec).await;
    let sandbox = Sandbox::new();
    let key_file = sandbox.dir.path().join("mfa");
    write_key(&key_file, &key, None);
    let identity_line = format!("  IdentityFile {}", key_file.display());
    lab(&sandbox, server.port(), &[&identity_line]);
    let lease = connect(&sandbox.manager(), TestIo::accepting(PASSWORD))
        .await
        .unwrap();
    assert_eq!(
        lease.connection().facts.handshake.auth_methods,
        vec!["publickey".to_string(), "password".to_string()]
    );
}

// --- keyboard-interactive and remembered passwords ---

fn password_round(answer: &str) -> KbdRound {
    KbdRound {
        name: String::new(),
        instructions: String::new(),
        prompts: vec![("Password: ".into(), false)],
        answers: vec![answer.into()],
    }
}

#[tokio::test]
async fn remembers_and_reuses_a_keyboard_interactive_password_prompt() {
    let mut spec = ServerSpec::new().methods(&[MethodKind::KeyboardInteractive]);
    spec.kbd_rounds = vec![password_round(PASSWORD)];
    let server = start(spec).await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let vault = MemoryVault::unlocked();
    let mut options = sandbox.options();
    options.vault = Some(vault.clone());
    let manager = SshConnectionManager::new(options);
    let io = TestIo::accepting(PASSWORD);
    io.answers
        .lock()
        .unwrap()
        .push_back(crate::connection::AuthPromptResponse {
            answers: vec![PASSWORD.into()],
            remember_password: Some(true),
            skipped: None,
        });
    let lease = connect(&manager, io.clone()).await.unwrap();
    lease.connection().wait_for_post_auth().await;
    let prompt = io.prompts.lock().unwrap()[0].clone();
    assert_eq!(
        prompt.purpose,
        Some(crate::connection::AuthPromptPurpose::SshPassword)
    );
    assert_eq!(
        prompt.remember_password.as_ref().unwrap().label,
        format!("tester@127.0.0.1:{}", server.port())
    );
    let account = ssh_password_account("tester", "127.0.0.1", server.port());
    assert_eq!(
        vault
            .passwords
            .lock()
            .unwrap()
            .get(&account)
            .map(String::as_str),
        Some(PASSWORD)
    );
    assert!(
        io.statuses()
            .iter()
            .any(|s| s.starts_with("Remembered the password for tester@127.0.0.1"))
    );
    lease.connection().close();
    drop(lease);

    // A fresh connection uses the saved password without asking.
    let io2 = TestIo::accepting("unused");
    let fresh = manager
        .connect(
            &SshProfile::new("lab"),
            io2.clone(),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions {
                dedicated_transport: true,
                ..ConnectOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(io2.prompt_count(), 0);
    assert!(
        io2.statuses()
            .iter()
            .any(|s| s.starts_with("Using the saved password for"))
    );
    drop(fresh);
}

/// OpenSSH with PAM ends a keyboard-interactive password login with a round that asks
/// nothing. It used to open an empty sign-in dialog after the password; it is answered
/// without asking.
#[tokio::test]
async fn answers_a_keyboard_interactive_round_without_prompts_by_itself() {
    let mut spec = ServerSpec::new().methods(&[MethodKind::KeyboardInteractive]);
    spec.kbd_rounds = vec![
        password_round(PASSWORD),
        KbdRound {
            name: String::new(),
            instructions: String::new(),
            prompts: Vec::new(),
            answers: Vec::new(),
        },
    ];
    let server = start(spec).await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let manager = SshConnectionManager::new(sandbox.options());
    let io = TestIo::accepting(PASSWORD);
    let lease = connect(&manager, io.clone()).await.unwrap();
    lease.connection().wait_for_post_auth().await;
    assert_eq!(io.prompt_count(), 1, "{:?}", io.prompts.lock().unwrap());
    assert!(
        server
            .auth_events()
            .iter()
            .any(|event| event == "auth keyboard-interactive:passed round=1")
    );
    drop(lease);
}

#[tokio::test]
async fn does_not_retry_an_unrecognized_keyboard_interactive_challenge_before_password_auth() {
    let mut spec = ServerSpec::new()
        .password(PASSWORD)
        .methods(&[MethodKind::KeyboardInteractive, MethodKind::Password]);
    spec.kbd_rounds = vec![KbdRound {
        name: "OTP".into(),
        instructions: "Enter the code".into(),
        prompts: vec![("Verification code: ".into(), true)],
        answers: vec!["123456".into()],
    }];
    let server = start(spec).await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let io = TestIo::accepting(PASSWORD);
    connect(&sandbox.manager(), io.clone()).await.unwrap();
    let prompts = io.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 2);
    assert_eq!(
        prompts[0].purpose,
        Some(crate::connection::AuthPromptPurpose::Authentication)
    );
    assert_eq!(prompts[0].name.as_deref(), Some("OTP"));
    assert!(prompts[1].prompts[0].prompt.ends_with("'s password"));
}

#[tokio::test]
async fn retries_a_rejected_password_with_the_permission_denied_prompt() {
    let server = start(
        ServerSpec::new()
            .password(PASSWORD)
            .methods(&[MethodKind::Password]),
    )
    .await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let io = TestIo::accepting(PASSWORD);
    io.answers
        .lock()
        .unwrap()
        .push_back(crate::connection::io::answers(["nope"]));
    connect(&sandbox.manager(), io.clone()).await.unwrap();
    let prompts: Vec<String> = io
        .prompts
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.prompts[0].prompt.clone())
        .collect();
    assert_eq!(
        prompts,
        vec![
            "tester@lab's password".to_string(),
            "Permission denied, please try again. Password".to_string()
        ]
    );
}

#[tokio::test]
async fn cancels_the_login_when_the_user_skips_the_password_prompt() {
    let server = start(
        ServerSpec::new()
            .password(PASSWORD)
            .methods(&[MethodKind::Password]),
    )
    .await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let io = TestIo::accepting(PASSWORD);
    io.answers
        .lock()
        .unwrap()
        .push_back(crate::connection::AuthPromptResponse {
            skipped: Some(true),
            ..Default::default()
        });
    let err = connect(&sandbox.manager(), io).await.unwrap_err();
    assert_eq!(err.to_string(), "authentication cancelled");
}

#[tokio::test]
async fn falls_back_to_a_manual_password_when_the_keyring_is_unavailable() {
    let server = start(
        ServerSpec::new()
            .password(PASSWORD)
            .methods(&[MethodKind::Password]),
    )
    .await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let vault = MemoryVault::unlocked();
    vault.store(
        &ssh_password_account("tester", "127.0.0.1", server.port()),
        "stale",
    );
    *vault.fail_reads.lock().unwrap() = Some(crate::connection::VaultError::KeyStoreUnavailable);
    let mut options = sandbox.options();
    options.vault = Some(vault);
    let io = TestIo::accepting(PASSWORD);
    connect(&SshConnectionManager::new(options), io.clone())
        .await
        .unwrap();
    assert!(
        io.statuses()
            .iter()
            .any(|s| s.starts_with("The OS credential store is unavailable"))
    );
    assert_eq!(io.prompt_count(), 1);
}

// --- folder-password-auth.test.ts ---

fn folder_options(sandbox: &Sandbox, vault: Arc<MemoryVault>) -> ManagerOptions {
    let mut options = sandbox.options();
    options.vault = Some(vault);
    options.folder_auth = Some(Arc::new(|alias: &str| {
        (alias == "lab").then(|| FolderAuthDefaults {
            option_lines: Vec::new(),
            passwords: vec![FolderPasswordRef {
                account: folder_password_account("row-lab"),
                label: "Folder Lab".into(),
            }],
        })
    }));
    options
}

#[tokio::test]
async fn logs_in_with_the_folder_password_without_prompting() {
    let server = start(
        ServerSpec::new()
            .password(PASSWORD)
            .methods(&[MethodKind::Password]),
    )
    .await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let vault = MemoryVault::unlocked();
    vault.store(&folder_password_account("row-lab"), PASSWORD);
    let io = TestIo::accepting(PASSWORD);
    connect(
        &SshConnectionManager::new(folder_options(&sandbox, vault)),
        io.clone(),
    )
    .await
    .unwrap();
    assert_eq!(io.prompt_count(), 0);
    let passwords: Vec<String> = server
        .auth_events()
        .into_iter()
        .filter(|e| e.starts_with("auth password"))
        .collect();
    assert_eq!(
        passwords,
        vec!["auth password:accepted user=tester".to_string()]
    );
}

#[tokio::test]
async fn prefers_the_host_saved_password_over_the_folder_password() {
    let server = start(
        ServerSpec::new()
            .password(PASSWORD)
            .methods(&[MethodKind::Password]),
    )
    .await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let vault = MemoryVault::unlocked();
    vault.store(&folder_password_account("row-lab"), "wrong-folder-password");
    vault.store(
        &ssh_password_account("tester", "127.0.0.1", server.port()),
        PASSWORD,
    );
    let io = TestIo::accepting(PASSWORD);
    connect(
        &SshConnectionManager::new(folder_options(&sandbox, vault)),
        io.clone(),
    )
    .await
    .unwrap();
    assert_eq!(io.prompt_count(), 0);
    let passwords: Vec<String> = server
        .auth_events()
        .into_iter()
        .filter(|e| e.starts_with("auth password"))
        .collect();
    // The wrong folder password was never offered.
    assert_eq!(
        passwords,
        vec!["auth password:accepted user=tester".to_string()]
    );
}

#[tokio::test]
async fn falls_back_to_prompting_when_the_folder_password_is_rejected() {
    let server = start(
        ServerSpec::new()
            .password(PASSWORD)
            .methods(&[MethodKind::Password]),
    )
    .await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &[]);
    let vault = MemoryVault::unlocked();
    vault.store(&folder_password_account("row-lab"), "stale-folder-password");
    let io = TestIo::accepting(PASSWORD);
    connect(
        &SshConnectionManager::new(folder_options(&sandbox, vault)),
        io.clone(),
    )
    .await
    .unwrap();
    assert_eq!(io.prompt_count(), 1);
    let passwords: Vec<String> = server
        .auth_events()
        .into_iter()
        .filter(|e| e.starts_with("auth password"))
        .collect();
    assert_eq!(
        passwords,
        vec![
            "auth password:rejected user=tester".to_string(),
            "auth password:accepted user=tester".to_string()
        ]
    );
}

#[tokio::test]
async fn pauses_connect_timeout_while_the_user_unlocks_a_saved_password() {
    let server = start(
        ServerSpec::new()
            .password(PASSWORD)
            .methods(&[MethodKind::Password]),
    )
    .await;
    let sandbox = Sandbox::new();
    lab(&sandbox, server.port(), &["  ConnectTimeout 1"]);
    let vault = MemoryVault::unlocked();
    *vault.locked.lock().unwrap() = true;
    *vault.master.lock().unwrap() = "master".into();
    vault.store(
        &ssh_password_account("tester", "127.0.0.1", server.port()),
        PASSWORD,
    );
    let mut options = sandbox.options();
    options.vault = Some(vault);
    let io = TestIo::accepting("master");
    *io.prompt_delay.lock().unwrap() = Some(Duration::from_millis(1_500));
    connect(&SshConnectionManager::new(options), io.clone())
        .await
        .unwrap();
    let prompt = io.prompts.lock().unwrap()[0].clone();
    assert_eq!(
        prompt.purpose,
        Some(crate::connection::AuthPromptPurpose::VaultUnlock)
    );
}

// --- security-keys.test.ts (connection parts) ---

#[cfg(unix)]
#[tokio::test]
async fn explains_what_to_do_when_openssh_cannot_be_found_then_falls_back() {
    use crate::security_key_agent::tests::fakes::{ed25519_sk, sk_key_file};
    let sk = ed25519_sk();
    // Nothing starts until the server accepts the key.
    let mut spec = ServerSpec::new().password(PASSWORD);
    spec.authorized_keys = vec![sk.clone()];
    let server = start(spec).await;
    let sandbox = Sandbox::new();
    let key_file = sandbox.dir.path().join("id_ed25519_sk");
    std::fs::write(&key_file, sk_key_file(&sk, "none")).unwrap();
    let identity_line = format!("  IdentityFile {}", key_file.display());
    lab(&sandbox, server.port(), &[&identity_line]);
    let mut options = sandbox.options();
    options.security_key_tools = crate::security_key_agent::OpenSshTools {
        ssh_agent: sandbox.dir.path().join("missing-ssh-agent"),
        ssh_add: sandbox.dir.path().join("missing-ssh-add"),
    };
    let io = TestIo::accepting(PASSWORD);
    connect(&SshConnectionManager::new(options), io.clone())
        .await
        .unwrap();
    let statuses = io.statuses();
    let note = statuses
        .iter()
        .find(|s| s.contains("is a security key (ED25519-SK)"))
        .expect("security key note");
    assert!(note.contains("which was not found"), "{note}");
    assert!(
        note.ends_with("— trying other authentication methods"),
        "{note}"
    );
    assert!(
        server
            .auth_events()
            .iter()
            .any(|e| e.starts_with("auth password:accepted"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn says_why_a_certificate_file_has_no_security_key_identity() {
    use crate::security_key_agent::tests::fakes::ed25519_sk;
    use russh::keys::ssh_key::certificate::{Builder, CertType};
    let ca = ed25519();
    let mut builder = Builder::new_with_random_nonce(
        &mut russh::keys::key::safe_rng(),
        ed25519_sk().key_data().clone(),
        0,
        u64::MAX - 1,
    )
    .unwrap();
    builder.cert_type(CertType::User).unwrap();
    builder.valid_principal("tester").unwrap();
    let certificate = builder.sign(&ca).unwrap();
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    let cert_file = sandbox.dir.path().join("id_sk-cert.pub");
    std::fs::write(&cert_file, certificate.to_openssh().unwrap()).unwrap();
    let cert_line = format!("  CertificateFile {}", cert_file.display());
    lab(&sandbox, server.port(), &[&cert_line]);
    let io = TestIo::accepting(PASSWORD);
    connect(&sandbox.manager(), io.clone()).await.unwrap();
    assert!(
        io.statuses().iter().any(|s| s
            == &format!(
                "certificate {} has no matching identity file — skipping",
                cert_file.display()
            )),
        "{:?}",
        io.statuses()
    );
}
