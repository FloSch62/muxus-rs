//! ssh-agent requests over connections we own, so an agent that accepts a connection but never
//! replies cannot trap the authentication state machine forever. Agent interaction is user time
//! (approval prompts, security key touches), not network dial time: every request pauses the
//! dial deadline while it runs.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::keys::HashAlg;
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::ssh_key::PublicKey;

use crate::security_keys::{is_security_key_type, signature_algorithm};

pub const DEFAULT_AGENT_WAIT_STATUS: Duration = Duration::from_secs(2);
pub const DEFAULT_AGENT_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
/// A security key waits for a touch (and maybe its PIN), which is user time.
pub const DEFAULT_SECURITY_KEY_TIMEOUT: Duration = Duration::from_secs(120);

/// Which agent request is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentOperation {
    Identities,
    Sign,
}

/// Callbacks around agent requests; every method has a no-op default.
pub trait AgentHooks: Send + Sync {
    /// Agent interaction is user time (approval/touch), not network dial time.
    fn pause_deadline(&self) {}
    fn resume_deadline(&self) {}
    /// Called once when an operation takes long enough to need user action.
    fn on_waiting(&self, _operation: AgentOperation) {}
    /// Called as a signature with a security key starts; it waits for a touch.
    fn on_security_key(&self, _key: &PublicKey) {}
    /// Called with the signature algorithm of every signature the agent made.
    fn on_signature(&self, _algorithm: &str) {}
}

/// Hooks that do nothing.
pub struct NoHooks;
impl AgentHooks for NoHooks {}

/// How long to wait, and whom to tell.
#[derive(Clone)]
pub struct ResponsiveAgentOptions {
    pub hooks: Arc<dyn AgentHooks>,
    /// When to call `on_waiting`; `None` never does.
    pub wait_status: Option<Duration>,
    /// Bound for one request; `None` waits as long as the agent does.
    pub operation_timeout: Option<Duration>,
    /// Bound for a security key signature; `None` waits as long as the agent does.
    pub security_key_timeout: Option<Duration>,
}

impl Default for ResponsiveAgentOptions {
    fn default() -> Self {
        Self {
            hooks: Arc::new(NoHooks),
            wait_status: Some(DEFAULT_AGENT_WAIT_STATUS),
            operation_timeout: Some(DEFAULT_AGENT_OPERATION_TIMEOUT),
            security_key_timeout: Some(DEFAULT_SECURITY_KEY_TIMEOUT),
        }
    }
}

/// A failed agent request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct AgentError {
    pub message: String,
    /// The agent cannot be reached or did not answer; later requests fail fast.
    pub unavailable: bool,
    /// Set when a security key did not sign: the message is then the agent's own reason.
    pub security_key: Option<Box<PublicKey>>,
}

impl AgentError {
    fn new(message: impl Into<String>, unavailable: bool) -> Self {
        Self {
            message: message.into(),
            unavailable,
            security_key: None,
        }
    }
}

type BoxStream = Box<dyn AgentStream + Send + Unpin>;
type ConnectFuture = Pin<Box<dyn Future<Output = std::io::Result<BoxStream>> + Send>>;

/// Where the agent listens.
#[derive(Clone)]
pub enum AgentEndpoint {
    /// A Unix socket path, or a Windows named pipe (`\\.\pipe\...`).
    Socket(PathBuf),
    /// A custom connector (tests, embedded agents).
    Custom(Arc<dyn Fn() -> ConnectFuture + Send + Sync>),
}

impl std::fmt::Debug for AgentEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentEndpoint::Socket(path) => write!(f, "Socket({})", path.display()),
            AgentEndpoint::Custom(_) => f.write_str("Custom"),
        }
    }
}

impl AgentEndpoint {
    async fn connect(&self) -> std::io::Result<BoxStream> {
        match self {
            AgentEndpoint::Custom(connect) => connect().await,
            AgentEndpoint::Socket(path) => connect_socket(path).await,
        }
    }
}

#[cfg(unix)]
async fn connect_socket(path: &std::path::Path) -> std::io::Result<BoxStream> {
    Ok(Box::new(tokio::net::UnixStream::connect(path).await?))
}

#[cfg(windows)]
async fn connect_socket(path: &std::path::Path) -> std::io::Result<BoxStream> {
    let pipe = tokio::net::windows::named_pipe::ClientOptions::new().open(path)?;
    Ok(Box::new(pipe))
}

/// An ssh-agent client that never hangs a login.
pub struct ResponsiveAgent {
    endpoint: AgentEndpoint,
    options: ResponsiveAgentOptions,
    unavailable: Mutex<Option<AgentError>>,
}

impl ResponsiveAgent {
    pub fn new(endpoint: AgentEndpoint, options: ResponsiveAgentOptions) -> Self {
        Self {
            endpoint,
            options,
            unavailable: Mutex::new(None),
        }
    }

    pub fn endpoint(&self) -> &AgentEndpoint {
        &self.endpoint
    }

    /// The identities the agent holds.
    pub async fn identities(&self) -> Result<Vec<AgentIdentity>, AgentError> {
        self.request(AgentOperation::Identities, None, |mut client| async move {
            client.request_identities().await
        })
        .await
    }

    /// Signs `data` with `identity` (`hash` picks rsa-sha2-256/512 for RSA keys).
    pub async fn sign(
        &self,
        identity: &AgentIdentity,
        hash: Option<HashAlg>,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, AgentError> {
        let key = identity_public_key(identity);
        let security_key = is_security_key_type(key.algorithm().as_str());
        if security_key {
            self.options.hooks.on_security_key(&key);
        }
        let limits = security_key.then_some((None, self.options.security_key_timeout));
        let owned = identity.clone();
        let result = self
            .request(AgentOperation::Sign, limits, move |mut client| async move {
                client.sign_request(&owned, hash, data).await
            })
            .await;
        match result {
            Ok(signature) => {
                let algorithm = signature_algorithm(key.algorithm().as_str(), hash);
                self.options.hooks.on_signature(&algorithm);
                Ok(signature)
            }
            Err(mut err) => {
                if security_key && err.security_key.is_none() {
                    err.security_key = Some(Box::new(key));
                }
                Err(err)
            }
        }
    }

    async fn request<T, F, Fut>(
        &self,
        operation: AgentOperation,
        limits: Option<(Option<Duration>, Option<Duration>)>,
        send: F,
    ) -> Result<T, AgentError>
    where
        F: FnOnce(AgentClient<BoxStream>) -> Fut + Send,
        Fut: Future<Output = Result<T, russh::keys::Error>> + Send,
    {
        if let Some(err) = self.unavailable.lock().ok().and_then(|g| g.clone()) {
            return Err(err);
        }
        let (wait_status, timeout) = match limits {
            Some(limits) => limits,
            None => (self.options.wait_status, self.options.operation_timeout),
        };
        let hooks = self.options.hooks.clone();
        hooks.pause_deadline();

        let work = async {
            let stream = self
                .endpoint
                .connect()
                .await
                .map_err(|e| AgentError::new(connect_message(&e), true))?;
            let client = AgentClient::connect(stream);
            send(client).await.map_err(|e| request_error(e, operation))
        };
        let bounded = async {
            match timeout {
                Some(limit) => tokio::time::timeout(limit, work).await.unwrap_or_else(|_| {
                    Err(AgentError::new(
                        format!(
                            "SSH agent did not respond while {}",
                            match operation {
                                AgentOperation::Identities => "listing identities",
                                AgentOperation::Sign => "signing",
                            }
                        ),
                        true,
                    ))
                }),
                None => work.await,
            }
        };
        tokio::pin!(bounded);
        let result = match wait_status {
            Some(delay) => {
                tokio::select! {
                    result = &mut bounded => result,
                    _ = tokio::time::sleep(delay) => {
                        hooks.on_waiting(operation);
                        bounded.await
                    }
                }
            }
            None => bounded.await,
        };
        if let Err(err) = &result
            && err.unavailable
            && let Ok(mut latch) = self.unavailable.lock()
        {
            *latch = Some(err.clone());
        }
        hooks.resume_deadline();
        result
    }
}

fn connect_message(err: &std::io::Error) -> String {
    format!("Failed to connect to SSH agent: {err}")
}

fn request_error(err: russh::keys::Error, operation: AgentOperation) -> AgentError {
    match err {
        // Same wording as the TypeScript agent client, which the error descriptions match on.
        russh::keys::Error::AgentFailure => AgentError::new(
            "Agent responded with failure",
            operation == AgentOperation::Identities,
        ),
        russh::keys::Error::IO(io) => AgentError::new(
            format!("SSH agent connection closed before replying: {io}"),
            true,
        ),
        other => AgentError::new(other.to_string(), operation == AgentOperation::Identities),
    }
}

/// The plain public key behind an agent identity (a certificate's certified key).
pub fn identity_public_key(identity: &AgentIdentity) -> PublicKey {
    match identity {
        AgentIdentity::PublicKey { key, .. } => key.clone(),
        AgentIdentity::Certificate { certificate, .. } => {
            PublicKey::new(certificate.public_key().clone(), certificate.comment())
        }
    }
}

/// The wire blob of an agent identity (certificates keep their full blob).
pub fn identity_blob(identity: &AgentIdentity) -> Vec<u8> {
    use russh::keys::ssh_encoding::Encode;
    let mut out = Vec::new();
    let _ = match identity {
        AgentIdentity::PublicKey { key, .. } => key.key_data().encode(&mut out),
        AgentIdentity::Certificate { certificate, .. } => certificate.encode(&mut out),
    };
    out
}

/// The comment an agent identity carries.
pub fn identity_comment(identity: &AgentIdentity) -> &str {
    match identity {
        AgentIdentity::PublicKey { comment, .. } | AgentIdentity::Certificate { comment, .. } => {
            comment
        }
    }
}

/// The key type of an agent identity as the server sees it.
pub fn identity_type(identity: &AgentIdentity) -> String {
    match identity {
        AgentIdentity::PublicKey { key, .. } => key.algorithm().as_str().to_string(),
        AgentIdentity::Certificate { certificate, .. } => {
            certificate.algorithm().to_certificate_type()
        }
    }
}

/// Failure while authenticating through the agent.
#[derive(Debug, thiserror::Error)]
pub enum AgentAuthError {
    #[error("the SSH session closed during authentication")]
    Send(#[from] russh::SendError),
    #[error(transparent)]
    Agent(#[from] AgentError),
}

/// Signs login challenges through a [`ResponsiveAgent`].
///
/// The SSH engine waits for a signature once the server accepted a key, so a failed agent
/// signature must still answer: the signer then sends a signature the server rejects, the attempt
/// fails like a wrong key, and the agent's error is kept in `failure` for the caller to report.
pub struct AgentSigner<'a> {
    pub agent: &'a ResponsiveAgent,
    pub failure: Option<AgentError>,
}

impl<'a> AgentSigner<'a> {
    pub fn new(agent: &'a ResponsiveAgent) -> Self {
        Self {
            agent,
            failure: None,
        }
    }
}

impl russh::Signer for AgentSigner<'_> {
    type Error = AgentAuthError;

    fn auth_sign(
        &mut self,
        key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send {
        let agent = self.agent;
        let key = key.clone();
        let failure = &mut self.failure;
        async move {
            match agent.sign(&key, hash_alg, to_sign.clone()).await {
                Ok(signed) => Ok(signed),
                Err(err) => {
                    *failure = Some(err);
                    Ok(unusable_signature(to_sign, &identity_type(&key)))
                }
            }
        }
    }
}

/// `data` followed by a well-formed signature no server accepts. It has to parse as the key
/// type's signature (security keys add flags and a counter), or a strict server drops the whole
/// connection instead of failing just this attempt.
pub fn unusable_signature(mut data: Vec<u8>, key_type: &str) -> Vec<u8> {
    use crate::security_keys::ssh_string;
    let base = key_type
        .strip_suffix("-cert-v01@openssh.com")
        .map(|b| {
            if b.starts_with("sk-") {
                format!("{b}@openssh.com")
            } else {
                b.to_string()
            }
        })
        .unwrap_or_else(|| key_type.to_string());
    let algorithm = if base.starts_with("ssh-rsa") {
        "rsa-sha2-256".to_string()
    } else {
        base
    };
    let mut signature = ssh_string(algorithm.as_bytes());
    if algorithm.contains("ecdsa") {
        let mut pair = ssh_string(&[1]);
        pair.extend(ssh_string(&[1]));
        signature.extend(ssh_string(&pair));
    } else {
        signature.extend(ssh_string(&[0u8; 64]));
    }
    if algorithm.starts_with("sk-") {
        signature.push(0x01); // user present
        signature.extend_from_slice(&0u32.to_be_bytes());
    }
    data.extend(ssh_string(&signature));
    data
}

#[cfg(test)]
mod tests;
