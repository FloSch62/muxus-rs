//! ssh-agents for tests: a real one (russh's agent server), one that never answers, and one
//! holding security keys that signs like a touched authenticator.

use std::path::{Path, PathBuf};

use russh::keys::PrivateKey;
use russh::keys::agent::client::AgentClient;

/// Starts an ssh-agent holding `keys` (a Unix socket in `dir`, a named pipe on Windows).
pub async fn start_agent(dir: &Path, keys: &[PrivateKey]) -> PathBuf {
    let (path, listener) = listen(dir, "agent");
    let connections = futures::stream::unfold(listener, |mut listener| async move {
        Some((listener.accept().await, listener))
    });
    tokio::spawn(async move {
        let _ = russh::keys::agent::server::serve(Box::pin(connections), ()).await;
    });
    #[cfg(unix)]
    let mut client = AgentClient::connect_uds(&path).await.unwrap();
    #[cfg(windows)]
    let mut client = AgentClient::connect_named_pipe(&path).await.unwrap();
    for key in keys {
        client.add_identity(key, &[]).await.unwrap();
    }
    path
}

/// An agent that accepts connections and reads requests but never replies.
pub async fn start_silent_agent(dir: &Path) -> PathBuf {
    let (path, mut listener) = listen(dir, "silent");
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok(stream) = listener.accept().await {
            held.push(stream);
        }
    });
    path
}

/// A fresh Ed25519 key.
pub fn ed25519() -> PrivateKey {
    PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        russh::keys::Algorithm::Ed25519,
    )
    .unwrap()
}

/// Requests a scripted agent received.
#[derive(Default)]
pub struct AgentCounters {
    pub identities: std::sync::atomic::AtomicUsize,
    pub signs: std::sync::atomic::AtomicUsize,
}

/// An agent that lists `identity` and never answers a signature request (or never answers at
/// all with `hang_identities`).
pub async fn start_hanging_agent(
    dir: &Path,
    identity: Option<russh::keys::PublicKey>,
    hang_identities: bool,
) -> (PathBuf, std::sync::Arc<AgentCounters>) {
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (path, mut listener) = listen(dir, "hang");
    let counters = std::sync::Arc::new(AgentCounters::default());
    let shared = counters.clone();
    tokio::spawn(async move {
        while let Ok(mut stream) = listener.accept().await {
            let counters = shared.clone();
            let identity = identity.clone();
            tokio::spawn(async move {
                let mut held = Vec::new();
                loop {
                    let mut len = [0u8; 4];
                    if stream.read_exact(&mut len).await.is_err() {
                        return;
                    }
                    let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
                    if stream.read_exact(&mut body).await.is_err() {
                        return;
                    }
                    match body.first() {
                        Some(11) => {
                            counters.identities.fetch_add(1, Ordering::SeqCst);
                            if hang_identities {
                                held.push(body);
                                continue;
                            }
                            let mut reply = vec![12u8];
                            match &identity {
                                Some(key) => {
                                    reply.extend_from_slice(&1u32.to_be_bytes());
                                    let blob = crate::security_keys::public_key_blob(key);
                                    reply.extend(crate::security_keys::ssh_string(&blob));
                                    reply.extend(crate::security_keys::ssh_string(b"hanging"));
                                }
                                None => reply.extend_from_slice(&0u32.to_be_bytes()),
                            }
                            let mut frame = (reply.len() as u32).to_be_bytes().to_vec();
                            frame.extend(reply);
                            let _ = stream.write_all(&frame).await;
                        }
                        Some(13) => {
                            counters.signs.fetch_add(1, Ordering::SeqCst);
                            held.push(body);
                        }
                        _ => {
                            let _ = stream.write_all(&[0, 0, 0, 1, 5]).await;
                        }
                    }
                }
            });
        }
    });
    (path, counters)
}

/// A FIDO key the way an authenticator holds it: an Ed25519 or P-256 key bound to an
/// application.
pub struct SoftSecurityKey {
    private: PrivateKey,
    pub public: russh::keys::PublicKey,
    pub application: String,
}

impl SoftSecurityKey {
    /// An `sk-ssh-ed25519@openssh.com` key for "ssh:".
    pub fn ed25519() -> Self {
        Self::new(ed25519(), "sk-ssh-ed25519@openssh.com")
    }

    /// An `sk-ecdsa-sha2-nistp256@openssh.com` key for "ssh:".
    pub fn ecdsa() -> Self {
        let private = PrivateKey::random(
            &mut russh::keys::key::safe_rng(),
            russh::keys::Algorithm::Ecdsa {
                curve: russh::keys::EcdsaCurve::NistP256,
            },
        )
        .unwrap();
        Self::new(private, "sk-ecdsa-sha2-nistp256@openssh.com")
    }

    fn new(private: PrivateKey, sk_type: &str) -> Self {
        use crate::security_keys::{public_key_blob, read_field, ssh_string};
        let application = "ssh:".to_string();
        let blob = public_key_blob(private.public_key());
        let (_, after_type) = read_field(&blob, 0).unwrap();
        let mut sk = ssh_string(sk_type.as_bytes());
        sk.extend_from_slice(&blob[after_type..]);
        sk.extend(ssh_string(application.as_bytes()));
        Self {
            private,
            public: russh::keys::PublicKey::from_bytes(&sk).unwrap(),
            application,
        }
    }

    pub fn key_type(&self) -> String {
        self.public.algorithm().as_str().to_string()
    }

    pub fn blob(&self) -> Vec<u8> {
        crate::security_keys::public_key_blob(&self.public)
    }

    /// A user certificate for this key, signed by a throwaway CA.
    pub fn certificate(&self) -> russh::keys::Certificate {
        use russh::keys::ssh_key::certificate::{Builder, CertType};
        let mut builder = Builder::new_with_random_nonce(
            &mut russh::keys::key::safe_rng(),
            self.public.key_data().clone(),
            0,
            u64::MAX - 1,
        )
        .unwrap();
        builder.cert_type(CertType::User).unwrap();
        builder.valid_principal("tester").unwrap();
        builder.sign(&ed25519()).unwrap()
    }

    /// What an agent returns for a security key (PROTOCOL.u2f): the signature over the
    /// authenticator's message, then the flags and the counter.
    pub fn sign(&self, data: &[u8], flags: u8, counter: u32) -> Vec<u8> {
        use russh::keys::signature::Signer;
        use sha2::{Digest, Sha256};

        use crate::security_keys::ssh_string;
        let mut message = Sha256::digest(self.application.as_bytes()).to_vec();
        message.push(flags);
        message.extend_from_slice(&counter.to_be_bytes());
        message.extend_from_slice(&Sha256::digest(data));
        // Ed25519 signs the message itself; ECDSA signs its SHA-256 and encodes (r, s) as
        // mpints, as OpenSSH's sk signatures do.
        let signature: russh::keys::ssh_key::Signature = self.private.try_sign(&message).unwrap();
        let mut blob = ssh_string(self.key_type().as_bytes());
        blob.extend(ssh_string(signature.as_bytes()));
        blob.push(flags);
        blob.extend_from_slice(&counter.to_be_bytes());
        blob
    }
}

/// One identity a security key agent lists; `key` signs for it.
pub struct SkAgentIdentity {
    pub blob: Vec<u8>,
    pub comment: String,
    pub key: Option<std::sync::Arc<SoftSecurityKey>>,
}

/// How a security key agent answers signature requests.
#[derive(Clone, Default)]
pub struct SkAgentBehaviour {
    /// Answer every signature request with SSH_AGENT_FAILURE (not touched in time).
    pub refuse: bool,
    /// Wait this long before answering, like a key waiting for its touch.
    pub delay: Option<std::time::Duration>,
    /// Authenticator flags (default 0x01, user present).
    pub flags: Option<u8>,
    pub counter: Option<u32>,
}

/// A minimal ssh-agent holding security keys, signing like a touched authenticator.
pub async fn start_sk_agent(
    dir: &Path,
    identities: Vec<SkAgentIdentity>,
    behaviour: SkAgentBehaviour,
) -> (PathBuf, std::sync::Arc<AgentCounters>) {
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncReadExt;

    use crate::security_keys::{read_field, ssh_string};
    let (path, mut listener) = listen(dir, "sk");
    let counters = std::sync::Arc::new(AgentCounters::default());
    let shared = counters.clone();
    let identities = std::sync::Arc::new(identities);
    tokio::spawn(async move {
        while let Ok(stream) = listener.accept().await {
            let counters = shared.clone();
            let identities = identities.clone();
            let behaviour = behaviour.clone();
            tokio::spawn(async move {
                let (mut reader, writer) = tokio::io::split(stream);
                let writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer));
                loop {
                    let mut len = [0u8; 4];
                    if reader.read_exact(&mut len).await.is_err() {
                        return;
                    }
                    let mut message = vec![0u8; u32::from_be_bytes(len) as usize];
                    if reader.read_exact(&mut message).await.is_err() {
                        return;
                    }
                    match message.first() {
                        Some(11) => {
                            counters.identities.fetch_add(1, Ordering::SeqCst);
                            let mut body = vec![12u8];
                            body.extend_from_slice(&(identities.len() as u32).to_be_bytes());
                            for identity in identities.iter() {
                                body.extend(ssh_string(&identity.blob));
                                body.extend(ssh_string(identity.comment.as_bytes()));
                            }
                            reply(&writer, body).await;
                        }
                        Some(13) => {
                            counters.signs.fetch_add(1, Ordering::SeqCst);
                            let (key_blob, next) = read_field(&message, 1).unwrap();
                            let (data, _) = read_field(&message, next).unwrap();
                            let key = identities
                                .iter()
                                .find(|identity| identity.blob == key_blob)
                                .and_then(|identity| identity.key.clone());
                            let body = match key {
                                Some(key) if !behaviour.refuse => {
                                    let mut body = vec![14u8];
                                    body.extend(ssh_string(&key.sign(
                                        data,
                                        behaviour.flags.unwrap_or(0x01),
                                        behaviour.counter.unwrap_or(0x0102_0304),
                                    )));
                                    body
                                }
                                _ => vec![5u8],
                            };
                            let writer = writer.clone();
                            let delay = behaviour.delay;
                            tokio::spawn(async move {
                                if let Some(delay) = delay {
                                    tokio::time::sleep(delay).await;
                                }
                                reply(&writer, body).await;
                            });
                        }
                        _ => reply(&writer, vec![5u8]).await,
                    }
                }
            });
        }
    });
    (path, counters)
}

/// Writes one agent reply frame.
async fn reply(writer: &tokio::sync::Mutex<tokio::io::WriteHalf<AgentStream>>, body: Vec<u8>) {
    use tokio::io::AsyncWriteExt;
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend(body);
    let _ = writer.lock().await.write_all(&frame).await;
}

#[cfg(unix)]
type AgentStream = tokio::net::UnixStream;
#[cfg(windows)]
type AgentStream = tokio::net::windows::named_pipe::NamedPipeServer;

/// Where a test agent takes connections: a Unix socket in `dir`, or on Windows a named pipe,
/// the way the client reaches agents there.
struct AgentListener {
    #[cfg(unix)]
    socket: tokio::net::UnixListener,
    #[cfg(windows)]
    path: PathBuf,
    #[cfg(windows)]
    next: AgentStream,
}

#[cfg(unix)]
fn listen(dir: &Path, name: &str) -> (PathBuf, AgentListener) {
    let path = dir.join(format!("{name}-{}.sock", rand::random::<u32>()));
    let socket = tokio::net::UnixListener::bind(&path).unwrap();
    (path, AgentListener { socket })
}

#[cfg(windows)]
fn listen(_dir: &Path, name: &str) -> (PathBuf, AgentListener) {
    use tokio::net::windows::named_pipe::ServerOptions;
    let path = PathBuf::from(format!(
        r"\\.\pipe\muxus-test-{name}-{}",
        rand::random::<u32>()
    ));
    let next = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&path)
        .unwrap();
    let listener = AgentListener {
        path: path.clone(),
        next,
    };
    (path, listener)
}

impl AgentListener {
    #[cfg(unix)]
    async fn accept(&mut self) -> std::io::Result<AgentStream> {
        Ok(self.socket.accept().await?.0)
    }

    /// A pipe instance serves one client: the next one is created before this one is handed
    /// out, so a client never finds the pipe missing.
    #[cfg(windows)]
    async fn accept(&mut self) -> std::io::Result<AgentStream> {
        use tokio::net::windows::named_pipe::ServerOptions;
        self.next.connect().await?;
        let next = ServerOptions::new().create(&self.path)?;
        Ok(std::mem::replace(&mut self.next, next))
    }
}
