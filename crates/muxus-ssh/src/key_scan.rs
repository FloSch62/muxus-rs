//! Discover the user's SSH identities for the host editor's key picker: the private keys sitting
//! in ~/.ssh (type/comment from the .pub sibling, encrypted-or-not from a parse probe) and what
//! the agent currently holds, cross-referenced by fingerprint so the UI can badge keys as loaded.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::config::expand_tilde;
use crate::known_hosts::fingerprint_sha256;
use crate::local_env::LocalEnv;
use crate::responsive_agent::{
    AgentEndpoint, NoHooks, ResponsiveAgent, ResponsiveAgentOptions, identity_blob,
    identity_comment, identity_type,
};
use crate::security_keys::{is_security_key_type, public_key_blob, read_openssh_key_file};

const PRIVATE_KEY_HEADERS: &[&str] = &[
    "-----BEGIN OPENSSH PRIVATE KEY-----",
    "-----BEGIN RSA PRIVATE KEY-----",
    "-----BEGIN EC PRIVATE KEY-----",
    "-----BEGIN DSA PRIVATE KEY-----",
    "-----BEGIN ENCRYPTED PRIVATE KEY-----",
    "-----BEGIN PRIVATE KEY-----",
    "PuTTY-User-Key-File-",
];

const MAX_KEY_FILE_BYTES: u64 = 1024 * 1024;

/// Files in ~/.ssh that are definitely not private keys.
fn skip_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.starts_with("config")
        || lower.starts_with("known_hosts")
        || lower.starts_with("authorized_keys")
        || [".pub", ".old", ".bak", ".tmp"]
            .iter()
            .any(|ext| lower.ends_with(ext))
        || lower.contains("muxus")
}

pub use muxus_core::api::{SshAgentKey, SshKeyInfo, SshKeysResponse};

/// The platform's default agent socket: `SSH_AUTH_SOCK`, or OpenSSH's named pipe on Windows.
pub fn agent_socket(env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(sock) = env("SSH_AUTH_SOCK").filter(|s| !s.is_empty()) {
        return Some(sock);
    }
    cfg!(windows).then(|| r"\\.\pipe\openssh-ssh-agent".to_string())
}

/// The process environment, as a lookup.
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The agent socket for one host: the IdentityAgent override when set (`none`, `$VAR`/`${VAR}`,
/// the literal `SSH_AUTH_SOCK`, or a path: 1Password and friends), otherwise the environment
/// default.
pub fn resolve_agent_socket(
    identity_agent: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
    local: &LocalEnv,
) -> Option<String> {
    let Some(identity_agent) = identity_agent else {
        return agent_socket(env);
    };
    if identity_agent.eq_ignore_ascii_case("none") {
        return None;
    }
    if identity_agent == "SSH_AUTH_SOCK" {
        return env("SSH_AUTH_SOCK").filter(|s| !s.is_empty());
    }
    if let Some(rest) = identity_agent.strip_prefix('$') {
        let name = rest
            .strip_prefix('{')
            .and_then(|r| r.strip_suffix('}'))
            .filter(|n| !n.is_empty() && !n.contains(['{', '}']))
            .unwrap_or(rest);
        return env(name).filter(|s| !s.is_empty());
    }
    Some(expand_tilde(identity_agent, local))
}

/// Whether the agent answered, and what it holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentKeyProbe {
    pub available: bool,
    pub keys: Vec<SshAgentKey>,
}

/// Ask an agent for its identities; `available` is false when it does not answer.
pub async fn probe_agent_keys(sock: Option<&str>) -> AgentKeyProbe {
    let Some(sock) = sock else {
        return AgentKeyProbe::default();
    };
    let agent = ResponsiveAgent::new(
        AgentEndpoint::Socket(PathBuf::from(sock)),
        ResponsiveAgentOptions {
            hooks: Arc::new(NoHooks),
            // Discovery only supplies editor badges; it must never hold the form open waiting
            // for an approval that may not be visible.
            operation_timeout: Some(Duration::from_secs(2)),
            wait_status: None,
            security_key_timeout: Some(Duration::from_secs(2)),
        },
    );
    probe_with(&agent).await
}

/// [`probe_agent_keys`] through an existing client.
pub async fn probe_with(agent: &ResponsiveAgent) -> AgentKeyProbe {
    match agent.identities().await {
        Err(_) => AgentKeyProbe::default(),
        Ok(identities) => AgentKeyProbe {
            available: true,
            keys: identities
                .iter()
                .map(|identity| SshAgentKey {
                    key_type: identity_type(identity),
                    comment: Some(identity_comment(identity).to_string()).filter(|c| !c.is_empty()),
                    fingerprint: fingerprint_sha256(&identity_blob(identity)),
                })
                .collect(),
        },
    }
}

/// The key picker's data: agent identities plus the private keys in `dir`.
pub async fn list_ssh_keys(dir: &Path, agent_sock: Option<&str>) -> SshKeysResponse {
    let agent = probe_agent_keys(agent_sock).await;
    let agent_prints: std::collections::HashSet<&str> =
        agent.keys.iter().map(|k| k.fingerprint.as_str()).collect();
    let mut keys = Vec::new();

    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        // No ~/.ssh yet: perfectly fine.
        Err(_) => Vec::new(),
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
    };
    names.sort();

    for name in names {
        if skip_file(&name) {
            continue;
        }
        let file_path = dir.join(&name);
        let Ok(meta) = std::fs::metadata(&file_path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_KEY_FILE_BYTES {
            continue;
        }
        let Ok(content) = std::fs::read(&file_path) else {
            continue;
        };
        let head: String = content.iter().take(64).map(|&b| b as char).collect();
        if !PRIVATE_KEY_HEADERS.iter().any(|h| head.starts_with(h)) {
            continue;
        }

        // A security key file holds only a key handle; its header names the key.
        let header = read_openssh_key_file(&content);
        let security_key = header.filter(|h| is_security_key_type(&h.key_type));
        let text = String::from_utf8_lossy(&content);
        let probe = if security_key.is_some() {
            None
        } else {
            Some(russh::keys::decode_secret_key(&text, None))
        };
        let encrypted = match (&security_key, &probe) {
            (Some(sk), _) => sk.encrypted,
            (None, Some(Err(_))) => true,
            _ => false,
        };
        if let Some(Err(err)) = &probe
            && !is_passphrase_error(err)
        {
            continue; // not actually a usable key
        }
        let parsed = probe.and_then(Result::ok);

        let mut key_type = parsed
            .as_ref()
            .map(|k| k.algorithm().as_str().to_string())
            .or_else(|| security_key.as_ref().map(|s| s.key_type.clone()));
        let mut comment = parsed
            .as_ref()
            .map(|k| k.comment().to_string())
            .filter(|c| !c.is_empty());
        let mut fingerprint = parsed
            .as_ref()
            .map(|k| fingerprint_sha256(&public_key_blob(k.public_key())))
            .or_else(|| {
                security_key
                    .as_ref()
                    .map(|s| fingerprint_sha256(&s.public_blob))
            });

        // The .pub sibling names the algorithm and comment even for encrypted keys.
        if let Ok(pub_text) = std::fs::read_to_string(format!("{}.pub", file_path.display())) {
            let fields: Vec<&str> = pub_text.split_whitespace().collect();
            if fields.len() >= 2 {
                key_type.get_or_insert_with(|| fields[0].to_string());
                if comment.is_none() {
                    comment = Some(fields[2..].join(" ")).filter(|c| !c.is_empty());
                }
                if fingerprint.is_none() {
                    use base64::Engine;
                    if let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(fields[1]) {
                        fingerprint = Some(fingerprint_sha256(&blob));
                    }
                }
            }
        }

        keys.push(SshKeyInfo {
            path: file_path.to_string_lossy().into_owned(),
            name,
            key_type,
            comment,
            encrypted,
            in_agent: fingerprint
                .as_deref()
                .is_some_and(|f| agent_prints.contains(f)),
        });
    }

    SshKeysResponse {
        agent_available: agent.available,
        agent_keys: agent.keys,
        keys,
    }
}

/// Whether a key parse failed only for want of a passphrase.
pub fn is_passphrase_error(err: &russh::keys::Error) -> bool {
    match err {
        russh::keys::Error::KeyIsEncrypted => true,
        other => {
            let text = other.to_string().to_lowercase();
            text.contains("passphrase") || text.contains("encrypted")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local() -> LocalEnv {
        LocalEnv {
            home: "/home/user".into(),
            username: "user".into(),
            hostname: "h".into(),
            uid: 1,
        }
    }

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn falls_back_to_the_environment_default_when_no_identity_agent_is_set() {
        let env = env_of(&[("SSH_AUTH_SOCK", "/run/user/1000/ssh-agent.sock")]);
        assert_eq!(
            resolve_agent_socket(None, &env, &local()).as_deref(),
            Some("/run/user/1000/ssh-agent.sock")
        );
    }

    #[test]
    fn disables_the_agent_for_identity_agent_none() {
        let env = env_of(&[("SSH_AUTH_SOCK", "/run/user/1000/ssh-agent.sock")]);
        assert_eq!(resolve_agent_socket(Some("none"), &env, &local()), None);
    }

    #[test]
    fn uses_a_configured_socket_path() {
        let env = env_of(&[]);
        assert_eq!(
            resolve_agent_socket(Some("/home/user/.1password/agent.sock"), &env, &local())
                .as_deref(),
            Some("/home/user/.1password/agent.sock")
        );
        assert_eq!(
            resolve_agent_socket(Some("~/.1password/agent.sock"), &env, &local()).as_deref(),
            Some("/home/user/.1password/agent.sock")
        );
    }

    #[test]
    fn dereferences_var_braced_var_and_the_literal_ssh_auth_sock() {
        let env = env_of(&[
            ("CUSTOM_SOCK", "/tmp/custom.sock"),
            ("SSH_AUTH_SOCK", "/tmp/default.sock"),
        ]);
        assert_eq!(
            resolve_agent_socket(Some("$CUSTOM_SOCK"), &env, &local()).as_deref(),
            Some("/tmp/custom.sock")
        );
        assert_eq!(
            resolve_agent_socket(Some("${CUSTOM_SOCK}"), &env, &local()).as_deref(),
            Some("/tmp/custom.sock")
        );
        assert_eq!(
            resolve_agent_socket(Some("SSH_AUTH_SOCK"), &env, &local()).as_deref(),
            Some("/tmp/default.sock")
        );
        assert_eq!(
            resolve_agent_socket(Some("$MISSING_VAR"), &env, &local()),
            None
        );
        assert_eq!(
            resolve_agent_socket(Some("${MISSING_VAR}"), &env, &local()),
            None
        );
    }

    #[tokio::test]
    async fn lists_private_keys_with_pub_siblings_and_encryption() {
        let dir = tempfile::tempdir().unwrap();
        let mut rng = russh::keys::key::safe_rng();
        let plain =
            russh::keys::PrivateKey::random(&mut rng, russh::keys::Algorithm::Ed25519).unwrap();
        let mut plain_with_comment = plain.clone();
        plain_with_comment.set_comment("me@laptop");
        std::fs::write(
            dir.path().join("id_ed25519"),
            plain_with_comment
                .to_openssh(russh::keys::ssh_key::LineEnding::LF)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        let locked = plain.encrypt(&mut rng, "secret").unwrap();
        std::fs::write(
            dir.path().join("id_locked"),
            locked
                .to_openssh(russh::keys::ssh_key::LineEnding::LF)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("id_locked.pub"),
            format!(
                "{} locked@laptop\n",
                plain.public_key().to_openssh().unwrap()
            ),
        )
        .unwrap();
        std::fs::write(dir.path().join("config"), "Host *\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();

        let response = list_ssh_keys(dir.path(), None).await;
        assert!(!response.agent_available);
        let names: Vec<&str> = response.keys.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, vec!["id_ed25519", "id_locked"]);
        assert_eq!(response.keys[0].key_type.as_deref(), Some("ssh-ed25519"));
        assert_eq!(response.keys[0].comment.as_deref(), Some("me@laptop"));
        assert!(!response.keys[0].encrypted);
        assert!(response.keys[1].encrypted);
        assert_eq!(response.keys[1].comment.as_deref(), Some("locked@laptop"));
        assert_eq!(response.keys[1].key_type.as_deref(), Some("ssh-ed25519"));
    }
}
