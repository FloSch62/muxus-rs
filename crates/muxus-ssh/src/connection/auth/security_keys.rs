//! Security key IdentityFiles and CertificateFiles in the auth ladder: they sign through a
//! private ssh-agent (see [`crate::security_key_agent`]), offered to the server as publickey
//! auth.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use russh::client::Handle;
use russh::keys::agent::AgentIdentity;
use russh::keys::{Certificate, HashAlg, PublicKey};

use super::{AuthError, AuthLadder, Outcome, basename};
use crate::connection::deadline::PausableDeadline;
use crate::connection::io::{AuthPromptInfo, AuthPromptPurpose, ConnectIo, PromptField};
use crate::connection::transport::ClientHandler;
use crate::known_hosts::fingerprint_sha256;
use crate::responsive_agent::{
    AgentHooks, ResponsiveAgentOptions, identity_type, unusable_signature,
};
use crate::security_key_agent::{
    AskCancelled, AskFuture, SecurityKeyAsk, SecurityKeyFileAgent, SecurityKeyFileOptions,
};
use crate::security_keys::{
    SecurityKeyError, SecurityKeyQuestion, SecurityKeyQuestionKind, describe_security_key_error,
    is_security_key_type, public_key_blob, security_key_file, security_key_label,
    security_key_provider,
};

impl AuthLadder {
    /// The public key of a security key IdentityFile, `None` for any other file.
    pub(super) fn security_key_public(&self, file: &str) -> Option<PublicKey> {
        std::fs::read(file)
            .ok()
            .and_then(|content| security_key_file(&content))
    }

    /// A security key CertificateFile and its blob, `None` for any other file.
    pub(super) fn security_key_certificate(&self, file: &str) -> Option<(Certificate, Vec<u8>)> {
        use base64::Engine;
        let text = std::fs::read_to_string(file).ok()?;
        let mut fields = text.split_whitespace();
        let key_type = fields.next()?;
        if !is_security_key_type(key_type) || !key_type.contains("-cert-") {
            return None;
        }
        let blob = base64::engine::general_purpose::STANDARD
            .decode(fields.next()?)
            .ok()?;
        let certificate = Certificate::from_openssh(text.trim()).ok()?;
        Some((certificate, blob))
    }

    /// A security key CertificateFile, signed by its matching IdentityFile.
    pub(super) async fn security_key_certificate_attempt(
        &mut self,
        handle: &mut Handle<ClientHandler>,
        user: &str,
        file: &str,
        certificate: &Certificate,
        blob: &[u8],
    ) -> Result<Outcome, AuthError> {
        let plain = crate::security_keys::plain_key_blob(blob);
        for identity in self.identity_files() {
            if let Some(public_key) = self.security_key_public(&identity)
                && public_key_blob(&public_key) == plain
            {
                return self
                    .security_key_attempt(
                        handle,
                        user,
                        &identity,
                        public_key,
                        Some(certificate.clone()),
                    )
                    .await;
            }
        }
        self.status(&format!(
            "certificate {file} has no matching identity file — skipping"
        ));
        Ok(Outcome::Unavailable)
    }

    /// Signs with a security key IdentityFile through its own private agent.
    pub(super) async fn security_key_attempt(
        &mut self,
        handle: &mut Handle<ClientHandler>,
        user: &str,
        file: &str,
        public_key: PublicKey,
        certificate: Option<Certificate>,
    ) -> Result<Outcome, AuthError> {
        // The login agent already offered this identity, and signs it the same way.
        let offered = match &certificate {
            Some(certificate) => {
                let mut blob = Vec::new();
                let _ = russh::keys::ssh_encoding::Encode::encode(certificate, &mut blob);
                blob
            }
            None => public_key_blob(&public_key),
        };
        if self
            .agent_holds_fingerprint(&fingerprint_sha256(&offered))
            .await
        {
            return Ok(Outcome::Unavailable);
        }
        let label = self.label();
        let cancelled = Arc::new(AtomicBool::new(false));
        let ask = Arc::new(SecurityKeyPrompt {
            io: self.cx.io.clone(),
            deadline: self.cx.deadline.clone(),
            host: label.clone(),
            name: basename(file),
            cancelled: cancelled.clone(),
        });
        let mut options = SecurityKeyFileOptions::new(file, public_key, ask);
        options.identity = certificate
            .clone()
            .map(|certificate| AgentIdentity::Certificate {
                certificate,
                comment: String::new(),
            });
        options.provider = security_key_provider(
            self.cx.hop.resolved.security_key_provider.as_deref(),
            &*self.cx.env_lookup,
            &self.cx.env,
        );
        options.agent_options = ResponsiveAgentOptions {
            hooks: Arc::new(SecurityKeyHooks {
                deadline: self.cx.deadline.clone(),
                io: self.cx.io.clone(),
                hop: label,
            }),
            ..ResponsiveAgentOptions::default()
        };
        options.tools = self.cx.security_key_tools.clone();
        let agent = Arc::new(SecurityKeyFileAgent::new(options));
        self.security_key_agents.push(agent.clone());
        let mut signer = SecurityKeyFileSigner {
            agent: &agent,
            failure: None,
        };
        let result = match certificate {
            Some(certificate) => {
                handle
                    .authenticate_certificate_with(user, certificate, None, &mut signer)
                    .await
            }
            None => {
                let key = agent.public_key().clone();
                handle
                    .authenticate_publickey_with(user, key, None, &mut signer)
                    .await
            }
        };
        if cancelled.load(Ordering::SeqCst) {
            return Err(self.cancel());
        }
        if let Some(err) = signer.failure.take() {
            let problem = describe_security_key_error(&err, self.cx.hop.display_name());
            tracing::warn!(%problem, "security key did not sign");
            self.cx.io.status(
                &format!("{problem} — trying other authentication methods"),
                false,
            );
            self.security_key_problem = Some(problem);
        }
        match result {
            Ok(result) => {
                let outcome: Outcome = result.into();
                if matches!(
                    outcome,
                    Outcome::Success | Outcome::Failure { partial: true, .. }
                ) {
                    self.key_algorithm = agent.identities().first().map(identity_type);
                }
                Ok(outcome)
            }
            Err(err) => Err(AuthError::Transport(err.to_string())),
        }
    }

    pub(super) async fn agent_holds_fingerprint(&mut self, fingerprint: &str) -> bool {
        if self.agent_identity_prints.is_none() {
            let Some(agent) = &self.cx.agent else {
                return false;
            };
            let probe = crate::key_scan::probe_with(agent).await;
            self.agent_identity_prints =
                Some(probe.keys.into_iter().map(|k| k.fingerprint).collect());
        }
        self.agent_identity_prints
            .as_ref()
            .is_some_and(|prints| prints.contains(fingerprint))
    }
}

/// Asks for a security key file's passphrase or the security key's PIN with the readiness
/// deadline paused.
struct SecurityKeyPrompt {
    io: Arc<dyn ConnectIo>,
    deadline: Arc<PausableDeadline>,
    host: String,
    name: String,
    cancelled: Arc<AtomicBool>,
}

impl SecurityKeyAsk for SecurityKeyPrompt {
    fn ask(&self, question: SecurityKeyQuestion) -> AskFuture<'_> {
        Box::pin(async move {
            let prompt = match question.kind {
                SecurityKeyQuestionKind::Passphrase => format!(
                    "{}Passphrase for {}",
                    if question.retry {
                        "Bad passphrase, try again. "
                    } else {
                        ""
                    },
                    self.name
                ),
                SecurityKeyQuestionKind::Pin => format!("PIN for the security key {}", self.name),
                SecurityKeyQuestionKind::Other => question.text.clone(),
            };
            self.deadline.pause();
            let response = self
                .io
                .prompt(AuthPromptInfo {
                    host: Some(self.host.clone()),
                    purpose: Some(AuthPromptPurpose::Authentication),
                    instructions: (question.kind == SecurityKeyQuestionKind::Pin
                        && question.presence)
                        .then(|| "Touch the security key after entering its PIN.".to_string()),
                    prompts: vec![PromptField {
                        prompt,
                        echo: false,
                    }],
                    ..AuthPromptInfo::default()
                })
                .await;
            self.deadline.resume();
            match response {
                Ok(response) => Ok(response.answers.first().cloned().filter(|a| !a.is_empty())),
                Err(_) => {
                    self.cancelled.store(true, Ordering::SeqCst);
                    Err(AskCancelled)
                }
            }
        })
    }
}

/// Deadline and touch hooks for a security key file's private agent.
struct SecurityKeyHooks {
    deadline: Arc<PausableDeadline>,
    io: Arc<dyn ConnectIo>,
    hop: String,
}

impl AgentHooks for SecurityKeyHooks {
    fn pause_deadline(&self) {
        self.deadline.pause();
    }
    fn resume_deadline(&self) {
        self.deadline.resume();
    }
    fn on_security_key(&self, key: &PublicKey) {
        let label = security_key_label(key.algorithm().as_str(), &public_key_blob(key));
        self.io.status(
            &format!(
                "Touch your security key to log in to {} ({label})…",
                self.hop
            ),
            true,
        );
    }
}

/// Signs through a security key file's private agent. A failure still answers, with a signature
/// the server rejects, so the login moves on (see `AgentSigner`).
struct SecurityKeyFileSigner<'a> {
    agent: &'a SecurityKeyFileAgent,
    failure: Option<SecurityKeyError>,
}

#[derive(Debug, thiserror::Error)]
#[error("the SSH session closed during authentication")]
struct SignerClosed(#[from] russh::SendError);

impl russh::Signer for SecurityKeyFileSigner<'_> {
    type Error = SignerClosed;

    fn auth_sign(
        &mut self,
        key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send {
        let agent = self.agent;
        let failure = &mut self.failure;
        let key_type = identity_type(key);
        async move {
            match agent.sign(hash_alg, to_sign.clone()).await {
                Ok(signed) => Ok(signed),
                Err(err) => {
                    *failure = Some(err);
                    Ok(unusable_signature(to_sign, &key_type))
                }
            }
        }
    }
}
