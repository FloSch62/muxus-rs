//! ssh-agents for tests: a real one (russh's agent server) and one that never answers.

use std::path::{Path, PathBuf};

use russh::keys::PrivateKey;
use russh::keys::agent::client::AgentClient;

/// Starts an ssh-agent on a Unix socket in `dir` holding `keys`.
pub async fn start_agent(dir: &Path, keys: &[PrivateKey]) -> PathBuf {
    let path = dir.join(format!("agent-{}.sock", rand::random::<u32>()));
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let stream = tokio_stream::wrappers::UnixListenerStream::new(listener);
    tokio::spawn(async move {
        let _ = russh::keys::agent::server::serve(stream, ()).await;
    });
    let mut client = AgentClient::connect_uds(&path).await.unwrap();
    for key in keys {
        client.add_identity(key, &[]).await.unwrap();
    }
    path
}

/// An agent that accepts connections and reads requests but never replies.
pub async fn start_silent_agent(dir: &Path) -> PathBuf {
    let path = dir.join(format!("silent-{}.sock", rand::random::<u32>()));
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
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
    let path = dir.join(format!("hang-{}.sock", rand::random::<u32>()));
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let counters = std::sync::Arc::new(AgentCounters::default());
    let shared = counters.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
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
