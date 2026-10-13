use std::sync::Mutex as StdMutex;

use super::*;
use crate::testing::agent::{ed25519, start_agent, start_silent_agent};

#[derive(Default)]
struct Recorder(StdMutex<Vec<String>>);

impl AgentHooks for Recorder {
    fn pause_deadline(&self) {
        self.0.lock().unwrap().push("pause".into());
    }
    fn resume_deadline(&self) {
        self.0.lock().unwrap().push("resume".into());
    }
    fn on_waiting(&self, operation: AgentOperation) {
        self.0
            .lock()
            .unwrap()
            .push(format!("waiting {operation:?}"));
    }
    fn on_signature(&self, algorithm: &str) {
        self.0.lock().unwrap().push(format!("signed {algorithm}"));
    }
}

fn agent(
    path: &std::path::Path,
    hooks: Arc<Recorder>,
    timeout: Option<Duration>,
    wait: Option<Duration>,
) -> ResponsiveAgent {
    ResponsiveAgent::new(
        AgentEndpoint::Socket(path.to_path_buf()),
        ResponsiveAgentOptions {
            hooks,
            wait_status: wait,
            operation_timeout: timeout,
            security_key_timeout: timeout,
        },
    )
}

#[tokio::test]
async fn lists_and_signs_through_a_real_agent_pausing_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let key = ed25519();
    let sock = start_agent(dir.path(), std::slice::from_ref(&key)).await;
    let hooks = Arc::new(Recorder::default());
    let agent = agent(&sock, hooks.clone(), Some(Duration::from_secs(5)), None);
    let identities = agent.identities().await.unwrap();
    assert_eq!(identities.len(), 1);
    assert_eq!(
        identity_public_key(&identities[0]).key_data(),
        key.public_key().key_data()
    );
    let signature = agent
        .sign(&identities[0], None, b"payload".to_vec())
        .await
        .unwrap();
    assert!(!signature.is_empty());
    assert_eq!(
        *hooks.0.lock().unwrap(),
        vec!["pause", "resume", "pause", "resume", "signed ssh-ed25519"]
    );
}

#[tokio::test]
async fn times_out_a_silent_agent_and_then_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let sock = start_silent_agent(dir.path()).await;
    let hooks = Arc::new(Recorder::default());
    let agent = agent(
        &sock,
        hooks.clone(),
        Some(Duration::from_millis(300)),
        Some(Duration::from_millis(50)),
    );
    let started = std::time::Instant::now();
    let err = agent.identities().await.unwrap_err();
    assert_eq!(
        err.message,
        "SSH agent did not respond while listing identities"
    );
    assert!(err.unavailable);
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert_eq!(
        *hooks.0.lock().unwrap(),
        vec!["pause", "waiting Identities", "resume"]
    );
    // The latch: no second wait on an agent known to hang.
    let again = std::time::Instant::now();
    assert!(agent.identities().await.is_err());
    assert!(again.elapsed() < Duration::from_millis(100));
}

#[tokio::test]
async fn reports_an_unreachable_socket_as_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.sock");
    let agent = agent(
        &missing,
        Arc::new(Recorder::default()),
        Some(Duration::from_secs(1)),
        None,
    );
    let err = agent.identities().await.unwrap_err();
    assert!(err.unavailable);
    assert!(err.message.starts_with("Failed to connect to SSH agent"));
    let probe = crate::key_scan::probe_agent_keys(Some(missing.to_str().unwrap())).await;
    assert!(!probe.available);
}

#[tokio::test]
async fn probes_agent_keys_with_fingerprints() {
    let dir = tempfile::tempdir().unwrap();
    let mut key = ed25519();
    key.set_comment("agent-key");
    let sock = start_agent(dir.path(), std::slice::from_ref(&key)).await;
    let probe = crate::key_scan::probe_agent_keys(Some(sock.to_str().unwrap())).await;
    assert!(probe.available);
    assert_eq!(probe.keys.len(), 1);
    assert_eq!(probe.keys[0].key_type, "ssh-ed25519");
    assert_eq!(
        probe.keys[0].fingerprint,
        crate::known_hosts::fingerprint_sha256(&crate::security_keys::public_key_blob(
            key.public_key()
        ))
    );
}
