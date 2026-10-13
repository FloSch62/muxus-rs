//! OpenSSH's client auth order: none → agent (unless IdentitiesOnly) → configured certificates
//! with their matching private keys → identity files (config ones first, else the default id_*
//! files) with passphrase prompts → keyboard-interactive → password. Every attempt happens
//! inside one SSH connection, like the real client.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use russh::client::AuthResult;
use russh::client::{Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg};
use russh::{MethodKind, MethodSet};

use super::chain::ChainHop;
use super::deadline::PausableDeadline;
use super::io::{
    AuthPromptInfo, AuthPromptPurpose, AuthPromptResponse, Cancelled, ConnectIo, PasswordVault,
    PromptField, RememberPasswordOffer, VaultError,
};
use super::transport::ClientHandler;
use crate::certificates::{
    OpenSshCertificate, certificate_algorithms, certificate_matches_key, parse_openssh_certificate,
};
use crate::folder_auth::{ssh_password_account, ssh_password_label};
use crate::key_scan::is_passphrase_error;
use crate::known_hosts::fingerprint_sha256;
use crate::local_env::LocalEnv;
use crate::responsive_agent::{AgentSigner, ResponsiveAgent, identity_blob, identity_type};
use crate::security_keys::{is_security_key_type, public_key_blob, security_key_file};

pub const MAX_KEYBOARD_INTERACTIVE_ATTEMPTS: usize = 3;
pub const MAX_PASSWORD_ATTEMPTS: usize = 3;
pub const MAX_PASSPHRASE_ATTEMPTS: usize = 3;
pub const DEFAULT_IDENTITY_NAMES: &[&str] = &[
    "id_ed25519",
    "id_ecdsa",
    "id_rsa",
    "id_ed25519_sk",
    "id_ecdsa_sk",
];

/// The auth method names the session summary reports.
pub fn method_name(kind: AttemptKind) -> &'static str {
    match kind {
        AttemptKind::None => "none",
        AttemptKind::Agent => "agent",
        AttemptKind::PublicKey => "publickey",
        AttemptKind::KeyboardInteractive => "keyboard-interactive",
        AttemptKind::Password => "password",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptKind {
    None,
    Agent,
    PublicKey,
    KeyboardInteractive,
    Password,
}

impl AttemptKind {
    /// Agent auth is publickey on the wire; servers never advertise "agent".
    fn wire(self) -> MethodKind {
        match self {
            AttemptKind::None => MethodKind::None,
            AttemptKind::Agent | AttemptKind::PublicKey => MethodKind::PublicKey,
            AttemptKind::KeyboardInteractive => MethodKind::KeyboardInteractive,
            AttemptKind::Password => MethodKind::Password,
        }
    }
}

enum Attempt {
    None,
    Agent,
    Certificate {
        file: String,
        certificate: Box<OpenSshCertificate>,
        algorithm: String,
    },
    IdentityFile {
        file: String,
    },
    SecurityKeyCertificate {
        file: String,
        certificate: Box<russh::keys::Certificate>,
        blob: Vec<u8>,
    },
    KeyboardInteractive {
        attempt: usize,
    },
    Password {
        attempt: usize,
    },
}

impl Attempt {
    fn kind(&self) -> AttemptKind {
        match self {
            Attempt::None => AttemptKind::None,
            Attempt::Agent => AttemptKind::Agent,
            Attempt::Certificate { .. }
            | Attempt::IdentityFile { .. }
            | Attempt::SecurityKeyCertificate { .. } => AttemptKind::PublicKey,
            Attempt::KeyboardInteractive { .. } => AttemptKind::KeyboardInteractive,
            Attempt::Password { .. } => AttemptKind::Password,
        }
    }
}

/// What one wire attempt did.
enum Outcome {
    Success,
    Failure {
        remaining: MethodSet,
        partial: bool,
    },
    /// The method had nothing to offer (no key, no agent identity); skip it.
    Unavailable,
}

impl From<AuthResult> for Outcome {
    fn from(result: AuthResult) -> Self {
        match result {
            AuthResult::Success => Outcome::Success,
            AuthResult::Failure {
                remaining_methods,
                partial_success,
            } => Outcome::Failure {
                remaining: remaining_methods,
                partial: partial_success,
            },
        }
    }
}

/// A password that logged in and may be saved once the connection is ready.
#[derive(Debug, Clone)]
pub struct RememberedPasswordCandidate {
    pub account: String,
    pub label: String,
    pub password: String,
}

struct PasswordCredential {
    account: String,
    label: String,
    existing: bool,
}

/// How the login went, for the session summary.
#[derive(Debug, Clone, Default)]
pub struct AuthReport {
    /// Methods the server accepted, in order.
    pub methods: Vec<String>,
    /// Signature algorithm of the key that logged in, when known.
    pub key_algorithm: Option<String>,
    /// A password to offer for saving after the connection is ready.
    pub remember: Option<RememberedPasswordCandidate>,
}

/// Why authentication ended without success.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("authentication cancelled")]
    Cancelled,
    #[error("All configured authentication methods failed")]
    Exhausted {
        security_key_problem: Option<String>,
    },
    #[error("{0}")]
    Transport(String),
}

/// Keyboard-interactive is also used for OTPs and arbitrary challenges. Only treat an
/// unambiguous, single hidden password field as an SSH password.
pub fn is_keyboard_interactive_password_prompt(prompts: &[PromptField]) -> bool {
    if prompts.len() != 1 || prompts[0].echo {
        return false;
    }
    let label = prompts[0].prompt.trim();
    let label = label
        .trim_end()
        .strip_suffix(':')
        .unwrap_or(label)
        .trim()
        .to_lowercase();
    password_for(&label) || possessive_password(&label)
}

/// `^password(?:\s+for\s+.+)?$`
fn password_for(label: &str) -> bool {
    let Some(rest) = label.strip_prefix("password") else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    let after_ws = rest.trim_start();
    if after_ws.len() == rest.len() {
        return false;
    }
    let Some(after_for) = after_ws.strip_prefix("for") else {
        return false;
    };
    let target = after_for.trim_start();
    target.len() < after_for.len() && !target.is_empty()
}

/// `^.+(?:'s|’s)\s+password$`
fn possessive_password(label: &str) -> bool {
    let Some(head) = label.strip_suffix("password") else {
        return false;
    };
    let trimmed = head.trim_end();
    if trimmed.len() == head.len() {
        return false;
    }
    ["'s", "’s"]
        .iter()
        .any(|marker| trimmed.strip_suffix(marker).is_some_and(|h| !h.is_empty()))
}

/// Everything the ladder needs for one hop.
pub(crate) struct AuthContext {
    pub hop: ChainHop,
    pub io: Arc<dyn ConnectIo>,
    pub vault: Option<Arc<dyn PasswordVault>>,
    pub deadline: Arc<PausableDeadline>,
    pub agent: Option<Arc<ResponsiveAgent>>,
    pub env: LocalEnv,
    pub env_lookup: super::transport::EnvLookup,
    /// The OpenSSH programs security key files sign through.
    pub security_key_tools: crate::security_key_agent::OpenSshTools,
}

mod security_keys;

pub(crate) struct AuthLadder {
    cx: AuthContext,
    private_keys: HashMap<String, Option<Arc<PrivateKey>>>,
    agent_identity_prints: Option<HashSet<String>>,
    last_password_candidate: Option<RememberedPasswordCandidate>,
    partial_password_candidate: Option<RememberedPasswordCandidate>,
    saved_password_attempted: bool,
    retry_keyboard_interactive: bool,
    partial_methods: Vec<String>,
    key_algorithm: Option<String>,
    partial_key_algorithm: Option<String>,
    security_key_agents: Vec<Arc<crate::security_key_agent::SecurityKeyFileAgent>>,
    security_key_problem: Option<String>,
    cancelled: bool,
}

impl AuthLadder {
    pub fn new(cx: AuthContext) -> Self {
        Self {
            cx,
            private_keys: HashMap::new(),
            agent_identity_prints: None,
            last_password_candidate: None,
            partial_password_candidate: None,
            saved_password_attempted: false,
            retry_keyboard_interactive: false,
            partial_methods: Vec::new(),
            key_algorithm: None,
            partial_key_algorithm: None,
            cancelled: false,
            security_key_agents: Vec::new(),
            security_key_problem: None,
        }
    }

    fn label(&self) -> String {
        self.cx.hop.display_name().to_string()
    }

    fn status(&self, message: &str) {
        self.cx.io.status(message, false);
    }

    /// Runs a user interaction with the readiness deadline paused.
    async fn interact<T>(&self, work: impl Future<Output = T>) -> T {
        self.cx.deadline.pause();
        let result = work.await;
        self.cx.deadline.resume();
        result
    }

    async fn prompt(&self, info: AuthPromptInfo) -> Result<AuthPromptResponse, Cancelled> {
        self.interact(self.cx.io.prompt(info)).await
    }

    fn build(&self) -> Vec<Attempt> {
        let resolved = &self.cx.hop.resolved;
        let mut attempts = vec![Attempt::None];
        if !resolved.password_only {
            if self.cx.agent.is_some() && !resolved.identities_only {
                attempts.push(Attempt::Agent);
            }
            let files = self.identity_files();
            for file in &resolved.certificate_files {
                if let Some((certificate, blob)) = self.security_key_certificate(file) {
                    attempts.push(Attempt::SecurityKeyCertificate {
                        file: file.clone(),
                        certificate: Box::new(certificate),
                        blob,
                    });
                    continue;
                }
                let Some(certificate) = self.load_certificate(file) else {
                    continue;
                };
                for algorithm in certificate_algorithms(&certificate) {
                    attempts.push(Attempt::Certificate {
                        file: file.clone(),
                        certificate: Box::new(certificate.clone()),
                        algorithm,
                    });
                }
            }
            for file in files {
                attempts.push(Attempt::IdentityFile { file });
            }
        }
        if resolved.kbd_interactive_authentication != Some(false) {
            for attempt in 0..MAX_KEYBOARD_INTERACTIVE_ATTEMPTS {
                attempts.push(Attempt::KeyboardInteractive { attempt });
            }
        }
        if resolved.password_authentication != Some(false) {
            for attempt in 1..=MAX_PASSWORD_ATTEMPTS {
                attempts.push(Attempt::Password { attempt });
            }
        }
        attempts
    }

    fn explicit_identity_files(&self) -> bool {
        !self.cx.hop.resolved.identity_files.is_empty()
    }

    fn identity_files(&self) -> Vec<String> {
        if self.explicit_identity_files() {
            return self.cx.hop.resolved.identity_files.clone();
        }
        let dir = self.cx.env.ssh_dir();
        DEFAULT_IDENTITY_NAMES
            .iter()
            .map(|name| dir.join(name))
            .filter(|p| p.exists())
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    /// Walks the ladder until the server accepts or every method is spent.
    pub async fn run(
        &mut self,
        handle: &mut Handle<ClientHandler>,
    ) -> Result<AuthReport, AuthError> {
        let user = self.cx.hop.user.clone();
        let mut remaining: Option<MethodSet> = None;
        for attempt in self.build() {
            if self.cancelled {
                return Err(AuthError::Cancelled);
            }
            let kind = attempt.kind();
            // The server told us which methods can still succeed; skip the rest.
            if kind != AttemptKind::None
                && let Some(left) = &remaining
                && !left.iter().any(|m| *m == kind.wire())
            {
                tracing::debug!(
                    method = method_name(kind),
                    "ssh auth method not accepted by server; skipping"
                );
                continue;
            }
            self.key_algorithm = None;
            let outcome = match self
                .attempt(handle, &user, &attempt, remaining.as_ref())
                .await
            {
                Ok(outcome) => outcome,
                Err(err) => {
                    if self.cancelled || err == AuthError::Cancelled {
                        return Err(AuthError::Cancelled);
                    }
                    return Err(err);
                }
            };
            match outcome {
                Outcome::Unavailable => {
                    tracing::debug!(
                        method = method_name(kind),
                        "ssh auth method unavailable; skipping"
                    );
                }
                Outcome::Success => {
                    let mut methods = self.partial_methods.clone();
                    methods.push(method_name(kind).to_string());
                    return Ok(AuthReport {
                        methods,
                        key_algorithm: self
                            .key_algorithm
                            .clone()
                            .or_else(|| self.partial_key_algorithm.clone()),
                        remember: self
                            .last_password_candidate
                            .take()
                            .or_else(|| self.partial_password_candidate.take()),
                    });
                }
                Outcome::Failure {
                    remaining: left,
                    partial,
                } => {
                    if let Some(candidate) = self.last_password_candidate.take()
                        && partial
                    {
                        self.partial_password_candidate = Some(candidate);
                    }
                    if partial {
                        self.partial_methods.push(method_name(kind).to_string());
                        if self.key_algorithm.is_some() {
                            self.partial_key_algorithm = self.key_algorithm.clone();
                        }
                    }
                    remaining = Some(left);
                }
            }
        }
        if self.cancelled {
            Err(AuthError::Cancelled)
        } else {
            Err(AuthError::Exhausted {
                security_key_problem: self.security_key_problem.clone(),
            })
        }
    }

    async fn attempt(
        &mut self,
        handle: &mut Handle<ClientHandler>,
        user: &str,
        attempt: &Attempt,
        remaining: Option<&MethodSet>,
    ) -> Result<Outcome, AuthError> {
        let transport = |e: russh::Error| AuthError::Transport(e.to_string());
        match attempt {
            Attempt::None => Ok(handle
                .authenticate_none(user)
                .await
                .map_err(transport)?
                .into()),
            Attempt::Agent => self.agent_attempt(handle, user).await,
            Attempt::Certificate {
                file,
                certificate,
                algorithm,
            } => {
                let Some(key) = self.find_certificate_key(file, certificate).await? else {
                    return Ok(Outcome::Unavailable);
                };
                let hash = if algorithm.starts_with("rsa-sha2-512") {
                    Some(HashAlg::Sha512)
                } else if algorithm.starts_with("rsa-sha2-256") {
                    Some(HashAlg::Sha256)
                } else {
                    None
                };
                self.key_algorithm = Some(algorithm.clone());
                let mut signer = LocalSigner(key);
                let result = handle
                    .authenticate_certificate_with(
                        user,
                        certificate.certificate.clone(),
                        hash,
                        &mut signer,
                    )
                    .await
                    .map_err(|e| AuthError::Transport(e.to_string()))?;
                let _ = remaining;
                Ok(result.into())
            }
            Attempt::IdentityFile { file } => {
                if let Some(public_key) = self.security_key_public(file) {
                    return self
                        .security_key_attempt(handle, user, file, public_key, None)
                        .await;
                }
                let Some(key) = self.load_private_key(file).await? else {
                    return Ok(Outcome::Unavailable);
                };
                let hash = if key.algorithm().is_rsa() {
                    handle
                        .best_supported_rsa_hash()
                        .await
                        .map_err(transport)?
                        .flatten()
                } else {
                    None
                };
                // A key file signs inside the client, which picks the RSA hash itself.
                self.key_algorithm =
                    (!key.algorithm().is_rsa()).then(|| key.algorithm().as_str().to_string());
                let result = handle
                    .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key, hash))
                    .await
                    .map_err(transport)?;
                Ok(result.into())
            }
            Attempt::SecurityKeyCertificate {
                file,
                certificate,
                blob,
            } => {
                self.security_key_certificate_attempt(handle, user, file, certificate, blob)
                    .await
            }
            Attempt::KeyboardInteractive { attempt } => {
                // Arbitrary challenges get one attempt; only a recognized password flow may
                // retry for stale-password replacement.
                if *attempt > 0 && !self.retry_keyboard_interactive {
                    return Ok(Outcome::Unavailable);
                }
                self.retry_keyboard_interactive = false;
                let mut response = handle
                    .authenticate_keyboard_interactive_start(user, None::<String>)
                    .await
                    .map_err(transport)?;
                let mut round = 0;
                loop {
                    match response {
                        KeyboardInteractiveAuthResponse::Success => return Ok(Outcome::Success),
                        KeyboardInteractiveAuthResponse::Failure {
                            remaining_methods,
                            partial_success,
                        } => {
                            return Ok(Outcome::Failure {
                                remaining: remaining_methods,
                                partial: partial_success,
                            });
                        }
                        // A round without prompts (PAM ends a password login with one) is
                        // answered at once, as OpenSSH and ssh2 do: there is nothing to ask.
                        KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. }
                            if prompts.is_empty() =>
                        {
                            response = handle
                                .authenticate_keyboard_interactive_respond(Vec::new())
                                .await
                                .map_err(transport)?;
                        }
                        KeyboardInteractiveAuthResponse::InfoRequest {
                            name,
                            instructions,
                            prompts,
                        } => {
                            round += 1;
                            let fields: Vec<PromptField> = prompts
                                .iter()
                                .map(|p| PromptField {
                                    prompt: p.prompt.clone(),
                                    echo: p.echo,
                                })
                                .collect();
                            let answers = match self
                                .keyboard_interactive_answers(
                                    &name,
                                    &instructions,
                                    &fields,
                                    round == 1,
                                )
                                .await
                            {
                                Ok(answers) => answers,
                                Err(_) => {
                                    self.cancelled = true;
                                    fields.iter().map(|_| String::new()).collect()
                                }
                            };
                            response = handle
                                .authenticate_keyboard_interactive_respond(answers)
                                .await
                                .map_err(transport)?;
                            if self.cancelled {
                                return Err(AuthError::Cancelled);
                            }
                        }
                    }
                }
            }
            Attempt::Password { attempt } => {
                let password = self.password_method(*attempt).await?;
                Ok(handle
                    .authenticate_password(user, password)
                    .await
                    .map_err(transport)?
                    .into())
            }
        }
    }

    async fn agent_attempt(
        &mut self,
        handle: &mut Handle<ClientHandler>,
        user: &str,
    ) -> Result<Outcome, AuthError> {
        let Some(agent) = self.cx.agent.clone() else {
            return Ok(Outcome::Unavailable);
        };
        let identities = match agent.identities().await {
            Ok(identities) => identities,
            Err(err) => {
                // An unreachable agent is an expected fallback when a stale agent socket is
                // inherited, so keep it out of user-facing status.
                if err.message.starts_with("Failed to connect to SSH agent") {
                    tracing::debug!(%err, "ssh-agent unavailable; trying other authentication methods");
                } else {
                    tracing::warn!(%err, "ssh-agent unavailable; trying other authentication methods");
                    self.cx.io.status(
                        &format!(
                            "ssh-agent unavailable ({}) — trying other authentication methods",
                            err.message
                        ),
                        false,
                    );
                }
                return Ok(Outcome::Unavailable);
            }
        };
        self.agent_identity_prints = Some(
            identities
                .iter()
                .map(|i| fingerprint_sha256(&identity_blob(i)))
                .collect(),
        );
        let mut last = Outcome::Unavailable;
        for identity in identities {
            if let Outcome::Failure { remaining, .. } = &last
                && !remaining.contains(&MethodKind::PublicKey)
            {
                break;
            }
            let rsa = identity_type(&identity) == "ssh-rsa"
                || identity_type(&identity).starts_with("ssh-rsa-cert");
            let hash = if rsa {
                handle
                    .best_supported_rsa_hash()
                    .await
                    .map_err(|e| AuthError::Transport(e.to_string()))?
                    .flatten()
            } else {
                None
            };
            let mut signer = AgentSigner::new(&agent);
            let result = match &identity {
                AgentIdentity::PublicKey { key, .. } => {
                    handle
                        .authenticate_publickey_with(user, key.clone(), hash, &mut signer)
                        .await
                }
                AgentIdentity::Certificate { certificate, .. } => {
                    handle
                        .authenticate_certificate_with(user, certificate.clone(), hash, &mut signer)
                        .await
                }
            };
            let type_name = identity_type(&identity);
            if let Some(err) = signer.failure.take() {
                // Like ssh(1), a key the agent could not sign with costs that key, not the login.
                self.report_agent_failure(&err);
                if let Ok(outcome) = result {
                    last = outcome.into();
                }
                if err.unavailable {
                    break;
                }
                continue;
            }
            match result {
                Ok(result) => {
                    self.key_algorithm =
                        Some(crate::security_keys::signature_algorithm(&type_name, hash));
                    let outcome: Outcome = result.into();
                    if matches!(
                        outcome,
                        Outcome::Success | Outcome::Failure { partial: true, .. }
                    ) {
                        return Ok(outcome);
                    }
                    last = outcome;
                }
                Err(err) => return Err(AuthError::Transport(err.to_string())),
            }
        }
        Ok(last)
    }

    fn report_agent_failure(&self, err: &crate::responsive_agent::AgentError) {
        if let Some(key) = &err.security_key {
            let problem = crate::security_keys::describe_security_key_error(
                &crate::security_keys::SecurityKeyError {
                    message: err.message.clone(),
                    key: Some((**key).clone()),
                },
                self.cx.hop.display_name(),
            );
            tracing::warn!(%problem, "security key did not sign");
            self.cx.io.status(
                &format!("{problem} — trying other authentication methods"),
                false,
            );
        } else {
            tracing::warn!(%err, "ssh-agent did not sign; trying other authentication methods");
            self.cx.io.status(
                &format!(
                    "ssh-agent unavailable ({}) — trying other authentication methods",
                    err.message
                ),
                false,
            );
        }
    }

    async fn password_method(&mut self, attempt: usize) -> Result<String, AuthError> {
        let user = self.cx.hop.user.clone();
        let label = self.label();
        let credential = self.password_credential();
        if let Some(saved) = self.available_saved_password(&credential).await? {
            return Ok(saved);
        }
        let response = self
            .prompt(AuthPromptInfo {
                host: Some(label.clone()),
                purpose: Some(AuthPromptPurpose::SshPassword),
                instructions: self.saved_password_attempted.then(|| {
                    "The saved password was unavailable or was not accepted. Enter the current password."
                        .to_string()
                }),
                prompts: vec![PromptField {
                    prompt: if attempt == 1 {
                        format!("{user}@{label}'s password")
                    } else {
                        "Permission denied, please try again. Password".to_string()
                    },
                    echo: false,
                }],
                remember_password: self
                    .cx
                    .vault
                    .as_ref()
                    .map(|_| RememberPasswordOffer { label: credential.label.clone(), existing: credential.existing }),
                ..AuthPromptInfo::default()
            })
            .await
            .map_err(|_| self.cancel())?;
        if response.skipped == Some(true) {
            return Err(self.cancel());
        }
        self.capture_password_candidate(&response, &credential);
        Ok(response.answers.first().cloned().unwrap_or_default())
    }

    fn cancel(&mut self) -> AuthError {
        self.cancelled = true;
        AuthError::Cancelled
    }

    async fn keyboard_interactive_answers(
        &mut self,
        name: &str,
        instructions: &str,
        prompts: &[PromptField],
        first_challenge_round: bool,
    ) -> Result<Vec<String>, Cancelled> {
        let label = self.label();
        let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_string());
        if !first_challenge_round || !is_keyboard_interactive_password_prompt(prompts) {
            // A later round makes the exchange multi-field, so a password offered for saving in
            // an earlier round is no longer a safe candidate.
            self.last_password_candidate = None;
            self.retry_keyboard_interactive = false;
            let response = self
                .prompt(AuthPromptInfo {
                    name: non_empty(name),
                    instructions: non_empty(instructions),
                    host: Some(label),
                    purpose: Some(AuthPromptPurpose::Authentication),
                    prompts: prompts.to_vec(),
                    ..AuthPromptInfo::default()
                })
                .await?;
            return Ok(response.answers);
        }

        self.retry_keyboard_interactive = true;
        let credential = self.password_credential();
        match self.available_saved_password(&credential).await {
            Ok(Some(saved)) => return Ok(vec![saved]),
            Ok(None) => {}
            Err(_) => return Err(Cancelled),
        }
        let retry_instructions = self.saved_password_attempted.then_some(
            "The saved password was unavailable or was not accepted. Enter the current password.",
        );
        let joined: Vec<&str> = retry_instructions
            .into_iter()
            .chain((!instructions.is_empty()).then_some(instructions))
            .collect();
        let response = self
            .prompt(AuthPromptInfo {
                name: non_empty(name),
                instructions: (!joined.is_empty()).then(|| joined.join("\n\n")),
                host: Some(label),
                purpose: Some(AuthPromptPurpose::SshPassword),
                prompts: prompts.to_vec(),
                remember_password: self.cx.vault.as_ref().map(|_| RememberPasswordOffer {
                    label: credential.label.clone(),
                    existing: credential.existing,
                }),
                ..AuthPromptInfo::default()
            })
            .await?;
        self.capture_password_candidate(&response, &credential);
        Ok(response.answers)
    }

    fn password_credential(&self) -> PasswordCredential {
        let hop = &self.cx.hop;
        let account = ssh_password_account(&hop.user, &hop.resolved.hostname, hop.port);
        PasswordCredential {
            existing: self
                .cx
                .vault
                .as_ref()
                .is_some_and(|v| v.has_ssh_password(&account)),
            label: ssh_password_label(&hop.user, &hop.resolved.hostname, hop.port),
            account,
        }
    }

    async fn available_saved_password(
        &mut self,
        credential: &PasswordCredential,
    ) -> Result<Option<String>, AuthError> {
        if self.saved_password_attempted {
            return Ok(None);
        }
        let Some(vault) = self.cx.vault.clone() else {
            return Ok(None);
        };
        // The host's own saved password wins; folder passwords are the shared default the host
        // falls back to, nearest folder first.
        let source = if credential.existing {
            Some((credential.account.clone(), credential.label.clone()))
        } else {
            self.cx
                .hop
                .folder_passwords
                .iter()
                .flatten()
                .find(|folder| vault.has_ssh_password(&folder.account))
                .map(|folder| (folder.account.clone(), folder.label.clone()))
        };
        let Some((account, label)) = source else {
            return Ok(None);
        };
        self.saved_password_attempted = true;
        let saved = self.saved_password(&vault, &account, &label).await?;
        if saved.is_some() {
            self.cx
                .io
                .status(&format!("Using the saved password for {label}."), true);
        }
        Ok(saved)
    }

    fn capture_password_candidate(
        &mut self,
        response: &AuthPromptResponse,
        credential: &PasswordCredential,
    ) {
        self.last_password_candidate =
            (response.remember_password == Some(true)).then(|| RememberedPasswordCandidate {
                account: credential.account.clone(),
                label: credential.label.clone(),
                password: response.answers.first().cloned().unwrap_or_default(),
            });
    }

    async fn saved_password(
        &mut self,
        vault: &Arc<dyn PasswordVault>,
        account: &str,
        label: &str,
    ) -> Result<Option<String>, AuthError> {
        let result = if !vault.status().locked {
            vault.ssh_password(account, None).await.map(Some)
        } else {
            let io = self.cx.io.clone();
            let deadline = self.cx.deadline.clone();
            prompt_for_vault_operation(
                &io,
                &deadline,
                "Unlock password vault",
                &format!("Enter the master password to use the saved password for {label}."),
                "Use another password",
                |master| {
                    let vault = vault.clone();
                    let account = account.to_string();
                    async move { vault.ssh_password(&account, Some(&master)).await }
                },
            )
            .await
        };
        match result {
            Ok(value) => Ok(value),
            Err(VaultError::CredentialVaultCorrupt) => {
                self.status(&format!(
                    "The saved password for {label} is damaged. Enter it again to replace the saved copy."
                ));
                Ok(None)
            }
            Err(VaultError::KeyStoreUnavailable) => {
                self.status(&format!("The OS credential store is unavailable. Enter the current password for {label}."));
                Ok(None)
            }
            Err(VaultError::Other(message)) if message == "authentication cancelled" => {
                Err(self.cancel())
            }
            Err(err) => Err(AuthError::Transport(err.to_string())),
        }
    }

    fn load_certificate(&self, file: &str) -> Option<OpenSshCertificate> {
        let Ok(content) = std::fs::read_to_string(file) else {
            self.status(&format!("certificate file {file} not found — skipping"));
            return None;
        };
        let first = content.split_whitespace().next().unwrap_or("");
        if is_security_key_type(first) {
            self.status(&format!(
                "could not load {}: security key certificates need the user's ssh-agent",
                basename(file)
            ));
            return None;
        }
        match parse_openssh_certificate(&content) {
            Ok(certificate) => Some(certificate),
            Err(message) => {
                self.status(&format!("could not load {}: {message}", basename(file)));
                None
            }
        }
    }

    async fn find_certificate_key(
        &mut self,
        certificate_file: &str,
        certificate: &OpenSshCertificate,
    ) -> Result<Option<Arc<PrivateKey>>, AuthError> {
        for file in self.identity_files() {
            if let Some(key) = self.load_private_key(&file).await?
                && certificate_matches_key(certificate, &key)
            {
                return Ok(Some(key));
            }
        }
        self.status(&format!(
            "certificate {certificate_file} has no matching identity file — skipping"
        ));
        Ok(None)
    }

    /// Read + parse one identity file, prompting for its passphrase when needed.
    async fn load_private_key(&mut self, file: &str) -> Result<Option<Arc<PrivateKey>>, AuthError> {
        if let Some(cached) = self.private_keys.get(file) {
            return Ok(cached.clone());
        }
        let key = self.read_private_key(file).await?;
        self.private_keys.insert(file.to_string(), key.clone());
        Ok(key)
    }

    async fn read_private_key(&mut self, file: &str) -> Result<Option<Arc<PrivateKey>>, AuthError> {
        let explicit = self.explicit_identity_files();
        let Ok(content) = std::fs::read(file) else {
            if explicit {
                self.status(&format!("identity file {file} not found — skipping"));
            }
            return Ok(None);
        };
        // Security key files are signed through an agent, never parsed here.
        if security_key_file(&content).is_some() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&content).into_owned();
        let mut parsed = russh::keys::decode_secret_key(&text, None);
        if let Err(err) = &parsed
            && is_passphrase_error(err)
        {
            // Encrypted. A default (unconfigured) key the agent already holds was covered by the
            // agent attempt: skip it silently. Anything else prompts for its passphrase, like
            // ssh(1).
            if !explicit && self.agent_holds_key(file).await {
                return Ok(None);
            }
            for i in 0..MAX_PASSPHRASE_ATTEMPTS {
                let response = self
                    .prompt(AuthPromptInfo {
                        host: Some(self.label()),
                        purpose: Some(AuthPromptPurpose::Authentication),
                        prompts: vec![PromptField {
                            prompt: format!(
                                "{}Passphrase for {}",
                                if i > 0 {
                                    "Bad passphrase, try again. "
                                } else {
                                    ""
                                },
                                basename(file)
                            ),
                            echo: false,
                        }],
                        ..AuthPromptInfo::default()
                    })
                    .await
                    .map_err(|_| self.cancel())?;
                let passphrase = response.answers.first().cloned().unwrap_or_default();
                if passphrase.is_empty() {
                    return Ok(None); // empty answer = skip this key
                }
                parsed = russh::keys::decode_secret_key(&text, Some(&passphrase));
                if parsed.is_ok() {
                    break;
                }
            }
        }
        match parsed {
            Ok(key) => Ok(Some(Arc::new(key))),
            Err(err) => {
                self.status(&format!("could not load {}: {err}", basename(file)));
                Ok(None)
            }
        }
    }

    /// True when the key's .pub sibling names an identity the agent holds.
    async fn agent_holds_key(&mut self, file: &str) -> bool {
        use base64::Engine;
        let Ok(text) = std::fs::read_to_string(format!("{file}.pub")) else {
            return false; // no readable .pub: prompt rather than guess
        };
        let Some(blob) = text.split_whitespace().nth(1) else {
            return false;
        };
        let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(blob) else {
            return false;
        };
        let fingerprint = fingerprint_sha256(&blob);
        self.agent_holds_fingerprint(&fingerprint).await
    }
}

fn basename(file: &str) -> String {
    Path::new(file)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_string())
}

/// Asks for the master password (three tries) and runs `operation` with it. `Ok(None)` when the
/// user skipped.
pub(crate) async fn prompt_for_vault_operation<T, F, Fut>(
    io: &Arc<dyn ConnectIo>,
    deadline: &PausableDeadline,
    name: &str,
    instructions: &str,
    skip_label: &str,
    operation: F,
) -> Result<Option<T>, VaultError>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<T, VaultError>>,
{
    deadline.pause();
    let result = async {
        let mut error: Option<String> = None;
        for _ in 0..3 {
            let response = io
                .prompt(AuthPromptInfo {
                    name: Some(name.to_string()),
                    purpose: Some(AuthPromptPurpose::VaultUnlock),
                    instructions: Some(match &error {
                        Some(error) => format!("{error}\n\n{instructions}"),
                        None => instructions.to_string(),
                    }),
                    prompts: vec![PromptField {
                        prompt: "Master password".into(),
                        echo: false,
                    }],
                    skip_label: Some(skip_label.to_string()),
                    ..AuthPromptInfo::default()
                })
                .await
                .map_err(|_| VaultError::Other("authentication cancelled".into()))?;
            if response.skipped == Some(true) {
                return Ok(None);
            }
            match operation(response.answers.first().cloned().unwrap_or_default()).await {
                Ok(value) => return Ok(Some(value)),
                Err(
                    VaultError::InvalidMasterPassword(message)
                    | VaultError::InvalidMasterPasswordFormat(message),
                ) => {
                    error = Some(message);
                }
                Err(err) => return Err(err),
            }
        }
        io.status("The password vault remains locked.", false);
        Ok(None)
    }
    .await;
    deadline.resume();
    result
}

/// Saves a password that logged in, creating or unlocking the vault as needed. Runs after the
/// connection is ready, so it never holds readiness on the remember-password dialog.
pub(crate) async fn commit_remembered_password(
    io: Arc<dyn ConnectIo>,
    vault: Arc<dyn PasswordVault>,
    candidate: RememberedPasswordCandidate,
) {
    let deadline = PausableDeadline::new(std::time::Duration::ZERO);
    let result: Result<bool, VaultError> = async {
        if !vault.status().configured
            && !create_vault_for_save(&io, &vault, &candidate.label).await?
        {
            return Ok(false);
        }
        if vault.status().locked {
            let saved = prompt_for_vault_operation(
                &io,
                &deadline,
                "Unlock password vault",
                &format!(
                    "Enter the master password to remember the password for {}.",
                    candidate.label
                ),
                "Not now",
                |master| {
                    let vault = vault.clone();
                    let candidate = candidate.clone();
                    async move {
                        vault
                            .remember_ssh_password(
                                &candidate.account,
                                &candidate.label,
                                &candidate.password,
                                Some(&master),
                            )
                            .await
                    }
                },
            )
            .await?;
            Ok(saved.is_some())
        } else {
            vault
                .remember_ssh_password(
                    &candidate.account,
                    &candidate.label,
                    &candidate.password,
                    None,
                )
                .await?;
            Ok(true)
        }
    }
    .await;
    match result {
        Ok(true) => io.status(
            &format!("Remembered the password for {}.", candidate.label),
            false,
        ),
        Ok(false) => {}
        Err(err) => io.status(
            &format!("Connected, but the password was not remembered: {err}"),
            false,
        ),
    }
}

async fn create_vault_for_save(
    io: &Arc<dyn ConnectIo>,
    vault: &Arc<dyn PasswordVault>,
    label: &str,
) -> Result<bool, VaultError> {
    let response = io
        .prompt(AuthPromptInfo {
            name: Some("Create password vault".into()),
            purpose: Some(AuthPromptPurpose::VaultCreate),
            instructions: Some(
                "Create a master password to protect viewing and editing saved SSH passwords. \
                 By default, Muxus stores the vault key in the operating-system credential store \
                 so saved passwords can be used without another master-password prompt."
                    .into(),
            ),
            prompts: vec![
                PromptField {
                    prompt: "Master password".into(),
                    echo: false,
                },
                PromptField {
                    prompt: "Confirm master password".into(),
                    echo: false,
                },
            ],
            skip_label: Some("Not now".into()),
            ..AuthPromptInfo::default()
        })
        .await
        .map_err(|_| VaultError::Other("authentication cancelled".into()))?;
    if response.skipped == Some(true) {
        return Ok(false);
    }
    let master = response.answers.first().cloned().unwrap_or_default();
    let confirmation = response.answers.get(1).cloned().unwrap_or_default();
    if master != confirmation {
        return Err(VaultError::InvalidMasterPasswordFormat(
            "The master-password confirmation did not match.".into(),
        ));
    }
    match vault.create(&master).await {
        Ok(()) => {}
        Err(VaultError::AlreadyConfigured) => io.status(
            &format!("The vault was created in another session; unlock it to remember the password for {label}."),
            false,
        ),
        Err(err) => return Err(err),
    }
    Ok(true)
}

/// Signs certificate logins with a key file held in memory.
struct LocalSigner(Arc<PrivateKey>);

impl russh::Signer for LocalSigner {
    type Error = LocalSignError;

    fn auth_sign(
        &mut self,
        _key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send {
        let key = self.0.clone();
        async move {
            let signature = sign_with_key(&key, hash_alg, &to_sign)
                .map_err(|e| LocalSignError::Sign(e.to_string()))?;
            // The engine expects the signed data back with the signature appended.
            let mut signed = to_sign;
            signed.extend(crate::security_keys::ssh_string(&signature));
            Ok(signed)
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum LocalSignError {
    #[error("the SSH session closed during authentication")]
    Send(#[from] russh::SendError),
    #[error("could not sign: {0}")]
    Sign(String),
}

/// An SSH signature blob made with a private key (`hash` picks the RSA variant).
pub fn sign_with_key(
    key: &PrivateKey,
    hash: Option<HashAlg>,
    data: &[u8],
) -> Result<Vec<u8>, russh::keys::ssh_key::Error> {
    use russh::keys::signature::Signer;
    use russh::keys::ssh_key::private::KeypairData;
    let signature: russh::keys::ssh_key::Signature = match key.key_data() {
        KeypairData::Rsa(rsa) => Signer::try_sign(&(rsa, hash), data)?,
        keypair => Signer::try_sign(keypair, data)?,
    };
    use russh::keys::ssh_encoding::Encode;
    let mut out = Vec::new();
    signature.encode(&mut out)?;
    Ok(out)
}

/// Identity blob of a public key for fingerprints.
pub fn key_fingerprint(key: &russh::keys::PublicKey) -> String {
    fingerprint_sha256(&public_key_blob(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(prompt: &str, echo: bool) -> Vec<PromptField> {
        vec![PromptField {
            prompt: prompt.into(),
            echo,
        }]
    }

    #[test]
    fn recognizes_only_unambiguous_password_prompts() {
        assert!(is_keyboard_interactive_password_prompt(&field(
            "Password: ",
            false
        )));
        assert!(is_keyboard_interactive_password_prompt(&field(
            "password for alice:",
            false
        )));
        assert!(is_keyboard_interactive_password_prompt(&field(
            "alice@host's password:",
            false
        )));
        assert!(is_keyboard_interactive_password_prompt(&field(
            "alice’s password",
            false
        )));
        assert!(!is_keyboard_interactive_password_prompt(&field(
            "Password: ",
            true
        )));
        assert!(!is_keyboard_interactive_password_prompt(&field(
            "Verification code:",
            false
        )));
        assert!(!is_keyboard_interactive_password_prompt(&field(
            "passwordfor x",
            false
        )));
        assert!(!is_keyboard_interactive_password_prompt(&[
            PromptField {
                prompt: "Password:".into(),
                echo: false
            },
            PromptField {
                prompt: "OTP:".into(),
                echo: false
            },
        ]));
    }
}
