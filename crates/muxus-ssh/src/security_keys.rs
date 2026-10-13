//! FIDO2/U2F security keys (`ssh-keygen -t ed25519-sk`/`ecdsa-sk`): the private key never leaves
//! the authenticator, so every signature comes from OpenSSH code that can talk to it: the user's
//! ssh-agent, or for a key file a private ssh-agent that Muxus runs just for the login.

use russh::keys::HashAlg;
use russh::keys::ssh_key::PublicKey;

use crate::known_hosts::fingerprint_sha256;
use crate::local_env::LocalEnv;

/// `(base, certificate)` for a security key type name.
fn split_security_key_type(key_type: &str) -> Option<(&str, bool)> {
    let rest = key_type.strip_suffix("@openssh.com")?;
    let (base, cert) = match rest.strip_suffix("-cert-v01") {
        Some(base) => (base, true),
        None => (rest, false),
    };
    matches!(base, "sk-ssh-ed25519" | "sk-ecdsa-sha2-nistp256").then_some((base, cert))
}

pub fn is_security_key_type(key_type: &str) -> bool {
    split_security_key_type(key_type).is_some()
}

/// OpenSSH's short name for the key type, e.g. ED25519-SK or ECDSA-SK-CERT.
pub fn security_key_type_name(key_type: &str) -> String {
    match split_security_key_type(key_type) {
        None => key_type.to_string(),
        Some((base, cert)) => format!(
            "{}{}",
            if base == "sk-ssh-ed25519" {
                "ED25519-SK"
            } else {
                "ECDSA-SK"
            },
            if cert { "-CERT" } else { "" }
        ),
    }
}

/// One SSH wire string at `offset`: its value and the offset after it.
pub(crate) fn read_field(data: &[u8], offset: usize) -> Option<(&[u8], usize)> {
    let len = data.get(offset..offset + 4)?;
    let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
    let end = offset.checked_add(4)?.checked_add(len)?;
    Some((data.get(offset + 4..end)?, end))
}

pub(crate) fn ssh_string(value: &[u8]) -> Vec<u8> {
    let mut out = (value.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(value);
    out
}

/// The plain key blob of a security key or of its certificate, as ssh-add -l fingerprints it.
pub fn plain_key_blob(blob: &[u8]) -> Vec<u8> {
    let Some((type_bytes, after_type)) = read_field(blob, 0) else {
        return blob.to_vec();
    };
    let key_type = String::from_utf8_lossy(type_bytes);
    let Some((base, true)) = split_security_key_type(&key_type) else {
        return blob.to_vec();
    };
    let Some((_, mut offset)) = read_field(blob, after_type) else {
        return blob.to_vec();
    };
    let count = if base == "sk-ssh-ed25519" { 2 } else { 3 };
    let mut out = ssh_string(format!("{base}@openssh.com").as_bytes());
    for _ in 0..count {
        let Some((value, next)) = read_field(blob, offset) else {
            return blob.to_vec();
        };
        out.extend(ssh_string(value));
        offset = next;
    }
    out
}

/// The wire blob of a public key.
pub fn public_key_blob(key: &PublicKey) -> Vec<u8> {
    use russh::keys::ssh_encoding::Encode;
    let mut out = Vec::new();
    let _ = key.key_data().encode(&mut out);
    out
}

/// "ED25519-SK SHA256:...", the way OpenSSH names a key when it asks for a touch.
pub fn security_key_label(key_type: &str, blob: &[u8]) -> String {
    format!(
        "{} {}",
        security_key_type_name(key_type),
        fingerprint_sha256(&plain_key_blob(blob))
    )
}

/// The algorithm a signature was made with, as the server sees it.
pub fn signature_algorithm(key_type: &str, hash: Option<HashAlg>) -> String {
    if key_type != "ssh-rsa" {
        return key_type.to_string();
    }
    match hash {
        Some(HashAlg::Sha256) => "rsa-sha2-256".to_string(),
        Some(HashAlg::Sha512) => "rsa-sha2-512".to_string(),
        _ => key_type.to_string(),
    }
}

/// The unencrypted header of an OpenSSH private key file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSshKeyFile {
    /// Key type of the public half, e.g. sk-ssh-ed25519@openssh.com.
    pub key_type: String,
    pub public_blob: Vec<u8>,
    pub encrypted: bool,
}

const OPENSSH_BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
const OPENSSH_END: &str = "-----END OPENSSH PRIVATE KEY-----";
const OPENSSH_KEY_MAGIC: &[u8] = b"openssh-key-v1\0";

/// The unencrypted header of an OpenSSH private key file: its public key and whether a
/// passphrase protects the private part. Readable without the passphrase, which is how a security
/// key file is recognized up front.
pub fn read_openssh_key_file(content: &[u8]) -> Option<OpenSshKeyFile> {
    use base64::Engine;
    let text = String::from_utf8_lossy(content);
    let start = text.find(OPENSSH_BEGIN)? + OPENSSH_BEGIN.len();
    let end = start + text[start..].find(OPENSSH_END)?;
    let body: String = text[start..end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if !body
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    {
        return None;
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(body.trim_end_matches('='))
        .or_else(|_| {
            base64::engine::general_purpose::STANDARD_NO_PAD.decode(body.trim_end_matches('='))
        })
        .ok()?;
    if !data.starts_with(OPENSSH_KEY_MAGIC) {
        return None;
    }
    let (cipher, next) = read_field(&data, OPENSSH_KEY_MAGIC.len())?;
    let (_kdf, next) = read_field(&data, next)?;
    let (_kdf_options, next) = read_field(&data, next)?;
    let count = data.get(next..next + 4)?;
    if u32::from_be_bytes([count[0], count[1], count[2], count[3]]) < 1 {
        return None;
    }
    let (public_blob, _) = read_field(&data, next + 4)?;
    let (key_type, _) = read_field(public_blob, 0)?;
    Some(OpenSshKeyFile {
        key_type: key_type.iter().map(|&b| b as char).collect(),
        public_blob: public_blob.to_vec(),
        encrypted: cipher != b"none",
    })
}

/// The public key of a security key file, or `None` for any other file.
pub fn security_key_file(content: &[u8]) -> Option<PublicKey> {
    let header = read_openssh_key_file(content)?;
    if !is_security_key_type(&header.key_type) {
        return None;
    }
    PublicKey::from_bytes(&header.public_blob).ok()
}

/// SecurityKeyProvider the way ssh(1) resolves it: the configured value, else $SSH_SK_PROVIDER,
/// else OpenSSH's built-in FIDO support (`None`).
pub fn security_key_provider(
    configured: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
    local: &LocalEnv,
) -> Option<String> {
    let mut value = configured.unwrap_or("$SSH_SK_PROVIDER").to_string();
    if let Some(name) = value.strip_prefix('$') {
        value = env(name).unwrap_or_default();
    }
    let value = crate::config::expand_tilde(&value, local);
    (!value.is_empty() && !value.eq_ignore_ascii_case("internal")).then_some(value)
}

/// What OpenSSH asks through SSH_ASKPASS while loading or using a security key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityKeyQuestion {
    pub kind: SecurityKeyQuestionKind,
    /// OpenSSH rejected the previous passphrase.
    pub retry: bool,
    /// The authenticator also needs a touch once the PIN is in.
    pub presence: bool,
    /// OpenSSH's own prompt.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityKeyQuestionKind {
    Passphrase,
    Pin,
    Other,
}

pub fn classify_askpass_prompt(text: &str) -> SecurityKeyQuestion {
    // `/:\s*$/`: a trailing colon and the whitespace after it.
    let trimmed = text.trim_end().strip_suffix(':').unwrap_or(text);
    let trimmed = trimmed.to_string();
    let question = |kind, retry, presence| SecurityKeyQuestion {
        kind,
        retry,
        presence,
        text: trimmed.clone(),
    };
    if let Some(rest) = trimmed.strip_prefix("Enter PIN") {
        if rest.starts_with(" for ") {
            return question(SecurityKeyQuestionKind::Pin, false, false);
        }
        if rest.starts_with(" and confirm user presence for ") {
            return question(SecurityKeyQuestionKind::Pin, false, true);
        }
    }
    if trimmed.starts_with("Enter passphrase for ") {
        return question(SecurityKeyQuestionKind::Passphrase, false, false);
    }
    if trimmed.starts_with("Bad passphrase, try again for ") {
        return question(SecurityKeyQuestionKind::Passphrase, true, false);
    }
    question(SecurityKeyQuestionKind::Other, false, false)
}

/// A security key that could not sign. With `key` set, the message is the agent's own reason;
/// otherwise it already tells the user what to do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SecurityKeyError {
    pub message: String,
    pub key: Option<PublicKey>,
}

/// What went wrong with a security key during the login to `hop`, for the user.
pub fn describe_security_key_error(err: &SecurityKeyError, hop: &str) -> String {
    let Some(key) = &err.key else {
        return err.message.clone();
    };
    let reason = if err.message == "Agent responded with failure" {
        "it was not touched in time, its PIN was not accepted, or it is not plugged in".to_string()
    } else if err.message.contains("did not respond") {
        "the SSH agent did not answer in time".to_string()
    } else {
        err.message.clone()
    };
    format!(
        "the security key {} did not sign the login to {hop}: {reason}",
        security_key_label(key.algorithm().as_str(), &public_key_blob(key))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_security_key_types() {
        assert!(is_security_key_type("sk-ssh-ed25519@openssh.com"));
        assert!(is_security_key_type(
            "sk-ecdsa-sha2-nistp256-cert-v01@openssh.com"
        ));
        assert!(!is_security_key_type("ssh-ed25519"));
        assert_eq!(
            security_key_type_name("sk-ssh-ed25519@openssh.com"),
            "ED25519-SK"
        );
        assert_eq!(
            security_key_type_name("sk-ecdsa-sha2-nistp256-cert-v01@openssh.com"),
            "ECDSA-SK-CERT"
        );
        assert_eq!(security_key_type_name("ssh-rsa"), "ssh-rsa");
    }

    #[test]
    fn classifies_openssh_askpass_prompts() {
        let pin =
            classify_askpass_prompt("Enter PIN for ED25519-SK key /home/u/.ssh/id_ed25519_sk: ");
        assert_eq!(pin.kind, SecurityKeyQuestionKind::Pin);
        assert!(!pin.presence);
        assert_eq!(
            pin.text,
            "Enter PIN for ED25519-SK key /home/u/.ssh/id_ed25519_sk"
        );
        let both =
            classify_askpass_prompt("Enter PIN and confirm user presence for ED25519-SK key x:");
        assert_eq!(both.kind, SecurityKeyQuestionKind::Pin);
        assert!(both.presence);
        let pass = classify_askpass_prompt("Enter passphrase for /home/u/.ssh/id_ed25519_sk:");
        assert_eq!(
            (pass.kind, pass.retry),
            (SecurityKeyQuestionKind::Passphrase, false)
        );
        let retry =
            classify_askpass_prompt("Bad passphrase, try again for /home/u/.ssh/id_ed25519_sk: ");
        assert_eq!(
            (retry.kind, retry.retry),
            (SecurityKeyQuestionKind::Passphrase, true)
        );
        let other = classify_askpass_prompt("Confirm user presence for key ED25519-SK SHA256:abc");
        assert_eq!(other.kind, SecurityKeyQuestionKind::Other);
    }

    #[test]
    fn resolves_security_key_provider_like_ssh() {
        let local = LocalEnv {
            home: "/home/u".into(),
            username: "u".into(),
            hostname: "h".into(),
            uid: 1,
        };
        let env = |name: &str| (name == "SSH_SK_PROVIDER").then(|| "/usr/lib/libsk.so".to_string());
        let none = |_: &str| None;
        assert_eq!(
            security_key_provider(None, &env, &local).as_deref(),
            Some("/usr/lib/libsk.so")
        );
        assert_eq!(security_key_provider(None, &none, &local), None);
        assert_eq!(security_key_provider(Some("internal"), &env, &local), None);
        assert_eq!(
            security_key_provider(Some("~/lib/sk.so"), &env, &local).as_deref(),
            Some("/home/u/lib/sk.so")
        );
        assert_eq!(
            security_key_provider(Some("$SSH_SK_PROVIDER"), &env, &local).as_deref(),
            Some("/usr/lib/libsk.so")
        );
    }

    #[test]
    fn signature_algorithms_follow_the_rsa_hash() {
        assert_eq!(
            signature_algorithm("ssh-rsa", Some(HashAlg::Sha512)),
            "rsa-sha2-512"
        );
        assert_eq!(
            signature_algorithm("ssh-rsa", Some(HashAlg::Sha256)),
            "rsa-sha2-256"
        );
        assert_eq!(signature_algorithm("ssh-rsa", None), "ssh-rsa");
        assert_eq!(
            signature_algorithm("ssh-ed25519", Some(HashAlg::Sha512)),
            "ssh-ed25519"
        );
    }

    #[test]
    fn reads_the_public_half_of_an_openssh_key_file_without_its_passphrase() {
        let key = russh::keys::PrivateKey::random(
            &mut russh::keys::key::safe_rng(),
            russh::keys::Algorithm::Ed25519,
        )
        .unwrap();
        let pem = key
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .unwrap();
        let header = read_openssh_key_file(pem.as_bytes()).unwrap();
        assert_eq!(header.key_type, "ssh-ed25519");
        assert!(!header.encrypted);
        assert_eq!(header.public_blob, public_key_blob(key.public_key()));
        assert!(security_key_file(pem.as_bytes()).is_none());
        assert!(read_openssh_key_file(b"not a key").is_none());
    }
}
