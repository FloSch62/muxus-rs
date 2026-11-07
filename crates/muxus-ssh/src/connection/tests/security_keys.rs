//! Port of the "security keys in the ssh-agent" cases of security-keys.test.ts: an agent that
//! holds FIDO keys and signs like a touched authenticator, against the in-process server, which
//! checks security key signatures the way sshd does.

use std::sync::atomic::Ordering;

use russh::MethodKind;

use super::*;
use crate::connection::leases::ConnectionLeaseOwner;
use crate::connection::manager::ConnectOptions;
use crate::connection::profile::SshProfile;
use crate::key_scan::probe_agent_keys;
use crate::responsive_agent::{
    AgentEndpoint, ResponsiveAgent, ResponsiveAgentOptions, identity_comment, identity_type,
};
use crate::security_keys::ssh_string;
use crate::testing::agent::{SkAgentBehaviour, SkAgentIdentity, SoftSecurityKey, start_sk_agent};
use crate::testing::server::{ServerSpec, TestServer, start};

const PASSWORD: &str = "fallback-password";
const ED25519_SK: &str = "sk-ssh-ed25519@openssh.com";
const ECDSA_SK: &str = "sk-ecdsa-sha2-nistp256@openssh.com";
const ED25519_SK_CERT: &str = "sk-ssh-ed25519-cert-v01@openssh.com";

fn identity(blob: Vec<u8>, comment: &str, key: Option<&Arc<SoftSecurityKey>>) -> SkAgentIdentity {
    SkAgentIdentity {
        blob,
        comment: comment.into(),
        key: key.cloned(),
    }
}

/// sshd stand-in accepting one security key (its signature checked) or, unless `password` is
/// false, the password.
async fn server_for(key: &SoftSecurityKey, password: bool) -> TestServer {
    let mut spec = ServerSpec::new().password(PASSWORD);
    if !password {
        spec = spec.methods(&[MethodKind::PublicKey]);
    }
    spec.authorized_keys = vec![key.public.clone()];
    start(spec).await
}

/// A sandbox naming the server `lab`, with `agent` as SSH_AUTH_SOCK and the agent bounds the
/// TypeScript test uses.
fn manager_for(
    sandbox: &Sandbox,
    port: u16,
    agent: &std::path::Path,
    extra: &[&str],
) -> SshConnectionManager {
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
    let mut options = sandbox.options();
    let socket = agent.to_string_lossy().into_owned();
    options.env_lookup =
        Arc::new(move |name: &str| (name == "SSH_AUTH_SOCK").then(|| socket.clone()));
    options.agent_operation_timeout = Duration::from_millis(100);
    options.agent_wait_status = Some(Duration::from_millis(10));
    SshConnectionManager::new(options)
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

#[tokio::test]
async fn lists_security_key_identities_certificates_included() {
    let ed = SoftSecurityKey::ed25519();
    let ecdsa = SoftSecurityKey::ecdsa();
    let sandbox = Sandbox::new();
    let mut unknown = ssh_string(b"unknown-type@example.com");
    unknown.extend(ssh_string(b"x"));
    let (agent, _) = start_sk_agent(
        sandbox.dir.path(),
        vec![
            identity(ed.blob(), "ed", None),
            identity(ed.certificate().to_bytes().unwrap(), "ed-cert", None),
            identity(ecdsa.blob(), "ecdsa", None),
            identity(unknown, "unknown", None),
        ],
        SkAgentBehaviour::default(),
    )
    .await;

    let responsive = ResponsiveAgent::new(
        AgentEndpoint::Socket(agent.clone()),
        ResponsiveAgentOptions {
            wait_status: None,
            ..ResponsiveAgentOptions::default()
        },
    );
    let keys: Vec<(String, String)> = responsive
        .identities()
        .await
        .unwrap()
        .iter()
        .map(|identity| {
            (
                identity_type(identity),
                identity_comment(identity).to_string(),
            )
        })
        .collect();
    assert_eq!(
        keys,
        vec![
            (ED25519_SK.to_string(), "ed".to_string()),
            (ED25519_SK_CERT.to_string(), "ed-cert".to_string()),
            (ECDSA_SK.to_string(), "ecdsa".to_string()),
        ]
    );
    let probe = probe_agent_keys(Some(&agent.to_string_lossy())).await;
    assert!(probe.available);
    assert_eq!(
        probe
            .keys
            .iter()
            .map(|key| key.key_type.as_str())
            .collect::<Vec<_>>(),
        vec![ED25519_SK, ED25519_SK_CERT, ECDSA_SK]
    );
}

async fn logs_in_with_an_agent_held_key(key: SoftSecurityKey) {
    let key = Arc::new(key);
    let sandbox = Sandbox::new();
    let (agent, counters) = start_sk_agent(
        sandbox.dir.path(),
        vec![identity(key.blob(), "sk", Some(&key))],
        SkAgentBehaviour {
            flags: Some(0x05),
            counter: Some(4242),
            ..SkAgentBehaviour::default()
        },
    )
    .await;
    let server = server_for(&key, true).await;
    let manager = manager_for(&sandbox, server.port(), &agent, &[]);
    let io = TestIo::accepting(PASSWORD);
    let shell = manager
        .connect_shell(
            &SshProfile::new("lab"),
            io.clone(),
            80,
            24,
            "xterm",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(shell.summary.auth_methods, vec!["agent".to_string()]);
    assert_eq!(
        shell.summary.auth_key_algorithm.as_deref(),
        Some(key.key_type().as_str())
    );
    // The server checked the signature over the authenticator's flags and counter, so both
    // went through untouched.
    assert_eq!(counters.signs.load(Ordering::SeqCst), 1);
    assert_eq!(
        server.auth_events(),
        vec![format!("auth publickey:accepted {}", key.key_type())]
    );
    let label = if key.key_type() == ED25519_SK {
        "ED25519-SK"
    } else {
        "ECDSA-SK"
    };
    let touch = format!("Touch your security key to log in to lab ({label} SHA256:");
    assert!(
        io.statuses().iter().any(|s| s.starts_with(&touch)),
        "{:?}",
        io.statuses()
    );
    assert_eq!(io.prompt_count(), 0);
}

#[tokio::test]
async fn logs_in_with_an_agent_held_ed25519_sk_key_passing_flags_and_counter_through() {
    logs_in_with_an_agent_held_key(SoftSecurityKey::ed25519()).await;
}

#[tokio::test]
async fn logs_in_with_an_agent_held_ecdsa_sk_key_passing_flags_and_counter_through() {
    logs_in_with_an_agent_held_key(SoftSecurityKey::ecdsa()).await;
}

#[tokio::test]
async fn logs_in_with_a_security_key_certificate_from_the_agent() {
    let key = Arc::new(SoftSecurityKey::ed25519());
    let certificate = key.certificate();
    let sandbox = Sandbox::new();
    let (agent, _) = start_sk_agent(
        sandbox.dir.path(),
        vec![identity(
            certificate.to_bytes().unwrap(),
            "cert",
            Some(&key),
        )],
        SkAgentBehaviour::default(),
    )
    .await;
    let server = server_for(&key, true).await;
    let manager = manager_for(&sandbox, server.port(), &agent, &[]);
    let shell = manager
        .connect_shell(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        shell.summary.auth_key_algorithm.as_deref(),
        Some(ED25519_SK_CERT)
    );
    assert_eq!(server.auth_events(), vec!["auth certificate:accepted"]);
}

#[tokio::test]
async fn waits_for_the_touch_beyond_connect_timeout_and_the_agent_response_bound() {
    let key = Arc::new(SoftSecurityKey::ed25519());
    let sandbox = Sandbox::new();
    // Longer than both ConnectTimeout (1 s) and the agent bound (100 ms).
    let (agent, _) = start_sk_agent(
        sandbox.dir.path(),
        vec![identity(key.blob(), "sk", Some(&key))],
        SkAgentBehaviour {
            delay: Some(Duration::from_millis(1_300)),
            ..SkAgentBehaviour::default()
        },
    )
    .await;
    let server = server_for(&key, true).await;
    let manager = manager_for(&sandbox, server.port(), &agent, &["  ConnectTimeout 1"]);
    let io = TestIo::accepting(PASSWORD);
    let lease = connect(&manager, io.clone()).await.unwrap();
    assert_eq!(
        lease.connection().facts.handshake.auth_methods,
        vec!["agent".to_string()]
    );
    assert!(
        server
            .auth_events()
            .iter()
            .any(|e| e.starts_with("auth publickey:accepted"))
    );
    // The touch prompt replaces the signing wait notice. Listing identities keeps its own
    // notice, which a slow machine can reach within the test's 10 ms.
    assert!(
        !io.statuses()
            .iter()
            .any(|s| s.contains("Waiting for the SSH agent to approve signing")),
        "{:?}",
        io.statuses()
    );
}

#[tokio::test]
async fn says_why_the_security_key_did_not_sign_instead_of_a_bare_authentication_failure() {
    let key = Arc::new(SoftSecurityKey::ecdsa());
    let sandbox = Sandbox::new();
    let (agent, counters) = start_sk_agent(
        sandbox.dir.path(),
        vec![identity(key.blob(), "sk", Some(&key))],
        SkAgentBehaviour {
            refuse: true,
            ..SkAgentBehaviour::default()
        },
    )
    .await;
    let server = server_for(&key, false).await;
    let manager = manager_for(&sandbox, server.port(), &agent, &[]);
    let io = TestIo::accepting(PASSWORD);
    let Err(err) = connect(&manager, io.clone()).await else {
        panic!("the login should fail");
    };
    let message = err.to_string();
    let prefix = format!(
        "authentication to 127.0.0.1:{} failed: the security key ECDSA-SK SHA256:",
        server.port()
    );
    assert!(message.starts_with(&prefix), "{message}");
    assert!(
        message.contains(" did not sign the login to lab: it was not touched in time"),
        "{message}"
    );
    assert_eq!(counters.signs.load(Ordering::SeqCst), 1);
    let statuses = io.statuses();
    assert!(
        statuses
            .iter()
            .any(|s| s.contains("did not sign the login to lab")
                && s.ends_with(" — trying other authentication methods")),
        "{statuses:?}"
    );
    assert!(
        !statuses.iter().any(|s| s.contains("ssh-agent unavailable")),
        "{statuses:?}"
    );
}
