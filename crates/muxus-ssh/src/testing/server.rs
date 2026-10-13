//! An in-process SSH server on 127.0.0.1:0 for client tests: configurable auth (password,
//! public keys, keyboard-interactive rounds, multi-factor), shell/exec with pty, env recording,
//! direct-tcpip to loopback targets, tcpip-forward, and an event log the tests assert on.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use russh::keys::{PrivateKey, PublicKey};
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet, Preferred, Pty};

/// One keyboard-interactive round: prompts and the answers that pass it.
#[derive(Clone, Debug)]
pub struct KbdRound {
    pub name: String,
    pub instructions: String,
    pub prompts: Vec<(String, bool)>,
    pub answers: Vec<String>,
}

/// What the server accepts and how it behaves.
#[derive(Clone)]
pub struct ServerSpec {
    pub host_key: PrivateKey,
    pub preferred: Preferred,
    pub methods: Vec<MethodKind>,
    pub password: Option<String>,
    pub authorized_keys: Vec<PublicKey>,
    pub kbd_rounds: Vec<KbdRound>,
    /// Methods required in order (multi-factor); each must pass before the next is offered.
    pub require_all: Vec<MethodKind>,
    pub banner: Option<String>,
    pub greeting: String,
    pub max_sessions: Option<usize>,
    pub reject_pty: bool,
    pub require_pty: bool,
    pub reject_x11: bool,
    /// Close the whole connection when an exec request arrives (a console server that dislikes
    /// the shell-integration probe).
    pub disconnect_on_exec: bool,
    /// Never answer exec requests in time (the probe times out).
    pub exec_output: Option<String>,
    /// Serve the `sftp` subsystem from this directory.
    pub sftp_root: Option<std::path::PathBuf>,
}

impl Default for ServerSpec {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerSpec {
    pub fn new() -> Self {
        Self {
            host_key: PrivateKey::random(
                &mut russh::keys::key::safe_rng(),
                russh::keys::Algorithm::Ed25519,
            )
            .unwrap(),
            preferred: Preferred::default(),
            methods: vec![
                MethodKind::Password,
                MethodKind::PublicKey,
                MethodKind::KeyboardInteractive,
            ],
            password: None,
            authorized_keys: Vec::new(),
            kbd_rounds: Vec::new(),
            require_all: Vec::new(),
            banner: None,
            greeting: "shell ok\n".into(),
            max_sessions: None,
            reject_pty: false,
            require_pty: false,
            reject_x11: false,
            disconnect_on_exec: false,
            exec_output: None,
            sftp_root: None,
        }
    }

    pub fn password(mut self, password: &str) -> Self {
        self.password = Some(password.into());
        self
    }

    pub fn methods(mut self, methods: &[MethodKind]) -> Self {
        self.methods = methods.to_vec();
        self
    }
}

/// A pty request: term, columns, rows and modes.
pub type PtyRecord = (String, u32, u32, Vec<(Pty, u32)>);

/// The server's view of what happened, for assertions.
#[derive(Default)]
pub struct ServerLog {
    pub events: Vec<String>,
    pub connections: usize,
    pub env: Vec<(String, String)>,
    pub ptys: Vec<PtyRecord>,
    pub execs: Vec<String>,
    /// Live sessions, for dropping them all at once.
    pub sessions: Vec<russh::server::Handle>,
}

pub struct TestServer {
    pub addr: SocketAddr,
    pub log: Arc<Mutex<ServerLog>>,
    pub host_key: PublicKey,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn events(&self) -> Vec<String> {
        self.log.lock().unwrap().events.clone()
    }

    pub fn connections(&self) -> usize {
        self.log.lock().unwrap().connections
    }

    /// Auth attempts as `method:result`.
    pub fn auth_events(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter(|e| e.starts_with("auth "))
            .collect()
    }
}

impl TestServer {
    /// Drops every live connection, the way a rebooted host would.
    pub async fn disconnect_all(&self) {
        let sessions: Vec<russh::server::Handle> =
            std::mem::take(&mut self.log.lock().unwrap().sessions);
        for session in sessions {
            let _ = session
                .disconnect(
                    russh::Disconnect::ByApplication,
                    "going down".into(),
                    "en".into(),
                )
                .await;
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts a server on 127.0.0.1:0.
pub async fn start(spec: ServerSpec) -> TestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(ServerLog::default()));
    let host_key = spec.host_key.public_key().clone();
    let config = Arc::new(russh::server::Config {
        keys: vec![spec.host_key.clone()],
        preferred: spec.preferred.clone(),
        methods: MethodSet::from(spec.methods.as_slice()),
        auth_rejection_time: std::time::Duration::from_millis(0),
        auth_rejection_time_initial: Some(std::time::Duration::from_millis(0)),
        max_auth_attempts: 30,
        ..russh::server::Config::default()
    });
    let spec = Arc::new(spec);
    let server_log = log.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            server_log.lock().unwrap().connections += 1;
            let handler = ServerHandler {
                spec: spec.clone(),
                log: server_log.clone(),
                passed: Vec::new(),
                kbd_round: 0,
                sessions: 0,
                channels: HashMap::new(),
                ptys: HashMap::new(),
                x11: HashMap::new(),
            };
            let config = config.clone();
            let log_for_session = server_log.clone();
            tokio::spawn(async move {
                if let Ok(session) = russh::server::run_stream(config, stream, handler).await {
                    log_for_session
                        .lock()
                        .unwrap()
                        .sessions
                        .push(session.handle());
                    let _ = session.await;
                }
            });
        }
    });
    TestServer {
        addr,
        log,
        host_key,
        task,
    }
}

struct ServerHandler {
    spec: Arc<ServerSpec>,
    log: Arc<Mutex<ServerLog>>,
    passed: Vec<MethodKind>,
    kbd_round: usize,
    sessions: usize,
    channels: HashMap<ChannelId, Channel<Msg>>,
    ptys: HashMap<ChannelId, bool>,
    /// The fake X11 cookie (hex) a session channel asked to forward with.
    x11: HashMap<ChannelId, String>,
}

impl ServerHandler {
    fn event(&self, event: String) {
        self.log.lock().unwrap().events.push(event);
    }

    /// Accept now, or ask for the next required factor.
    fn passed(&mut self, method: MethodKind) -> Auth {
        if !self.passed.contains(&method) {
            self.passed.push(method);
        }
        let missing: Vec<MethodKind> = self
            .spec
            .require_all
            .iter()
            .filter(|m| !self.passed.contains(m))
            .cloned()
            .collect();
        if missing.is_empty() {
            Auth::Accept
        } else {
            Auth::Reject {
                proceed_with_methods: Some(MethodSet::from(missing.as_slice())),
                partial_success: true,
            }
        }
    }

    fn reject(&self) -> Auth {
        let remaining: Vec<MethodKind> = if self.spec.require_all.is_empty() {
            self.spec.methods.clone()
        } else {
            self.spec
                .require_all
                .iter()
                .filter(|m| !self.passed.contains(m))
                .cloned()
                .collect()
        };
        Auth::Reject {
            proceed_with_methods: Some(MethodSet::from(remaining.as_slice())),
            partial_success: false,
        }
    }
}

impl Handler for ServerHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(self.reject())
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let ok = self.spec.password.as_deref() == Some(password);
        self.event(format!(
            "auth password:{} user={user}",
            if ok { "accepted" } else { "rejected" }
        ));
        Ok(if ok {
            self.passed(MethodKind::Password)
        } else {
            self.reject()
        })
    }

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        let known = self
            .spec
            .authorized_keys
            .iter()
            .any(|k| k.key_data() == key.key_data());
        Ok(if known { Auth::Accept } else { self.reject() })
    }

    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let known = self
            .spec
            .authorized_keys
            .iter()
            .any(|k| k.key_data() == key.key_data());
        self.event(format!(
            "auth publickey:{} {}",
            if known { "accepted" } else { "rejected" },
            key.algorithm().as_str()
        ));
        Ok(if known {
            self.passed(MethodKind::PublicKey)
        } else {
            self.reject()
        })
    }

    async fn auth_openssh_certificate(
        &mut self,
        _user: &str,
        certificate: &russh::keys::Certificate,
    ) -> Result<Auth, Self::Error> {
        let known = self
            .spec
            .authorized_keys
            .iter()
            .any(|k| k.key_data() == certificate.public_key());
        self.event(format!(
            "auth certificate:{}",
            if known { "accepted" } else { "rejected" }
        ));
        Ok(if known {
            self.passed(MethodKind::PublicKey)
        } else {
            self.reject()
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<russh::server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        if self.spec.kbd_rounds.is_empty() {
            return Ok(self.reject());
        }
        if let Some(response) = response {
            let answers: Vec<String> = response
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .collect();
            let round = &self.spec.kbd_rounds[self.kbd_round];
            if answers != round.answers {
                self.event(format!(
                    "auth keyboard-interactive:rejected round={}",
                    self.kbd_round
                ));
                self.kbd_round = 0;
                return Ok(self.reject());
            }
            self.event(format!(
                "auth keyboard-interactive:passed round={}",
                self.kbd_round
            ));
            self.kbd_round += 1;
            if self.kbd_round >= self.spec.kbd_rounds.len() {
                self.kbd_round = 0;
                return Ok(self.passed(MethodKind::KeyboardInteractive));
            }
        }
        let round = &self.spec.kbd_rounds[self.kbd_round];
        Ok(Auth::Partial {
            name: round.name.clone().into(),
            instructions: round.instructions.clone().into(),
            prompts: round
                .prompts
                .iter()
                .map(|(p, echo)| (p.clone().into(), *echo))
                .collect::<Vec<_>>()
                .into(),
        })
    }

    async fn authentication_banner(&mut self) -> Result<Option<String>, Self::Error> {
        Ok(self.spec.banner.clone())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self
            .spec
            .max_sessions
            .is_some_and(|max| self.sessions >= max)
        {
            self.event("session refused".into());
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        }
        self.sessions += 1;
        self.event("session".into());
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channels.remove(&channel).is_some() {
            self.sessions = self.sessions.saturating_sub(1);
        }
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.log
            .lock()
            .unwrap()
            .env
            .push((name.into(), value.into()));
        self.event(format!("env {name}={value}"));
        let _ = session.channel_success(channel);
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _pix_width: u32,
        _pix_height: u32,
        modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.event(format!("pty {term} {cols}x{rows}"));
        self.log
            .lock()
            .unwrap()
            .ptys
            .push((term.into(), cols, rows, modes.to_vec()));
        if self.spec.reject_pty {
            session.channel_failure(channel)?;
        } else {
            self.ptys.insert(channel, true);
            session.channel_success(channel)?;
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        cols: u32,
        rows: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.event(format!("window-change {cols}x{rows}"));
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // An echoing shell: what the client types comes back. Only session channels echo;
        // forwarded channels carry their own streams.
        if !self.ptys.contains_key(&channel) && !self.channels.contains_key(&channel) {
            return Ok(());
        }
        session.data(channel, data.to_vec())?;
        if data == b"exit\r" {
            session.exit_status_request(channel, 3)?;
            session.eof(channel)?;
            session.close(channel)?;
        }
        Ok(())
    }

    async fn x11_request(
        &mut self,
        channel: ChannelId,
        single_connection: bool,
        x11_auth_protocol: &str,
        x11_auth_cookie: &str,
        x11_screen_number: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.event(format!(
            "x11 {single_connection} {x11_auth_protocol} {x11_auth_cookie} {x11_screen_number}"
        ));
        if self.spec.reject_x11 {
            session.channel_failure(channel)?
        } else {
            self.x11.insert(channel, x11_auth_cookie.to_string());
            session.channel_success(channel)?
        }
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.event("shell".into());
        if self.spec.require_pty && !self.ptys.contains_key(&channel) {
            session.channel_failure(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        session.data(channel, self.spec.greeting.as_bytes().to_vec())?;
        if let Some(cookie) = self.x11.get(&channel).cloned() {
            // An X client on the server: connect back through the forwarded display with the
            // cookie sshd handed it, and report what the local X server answered.
            let handle = session.handle();
            let log = self.log.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let Ok(channel) = handle.channel_open_x11("127.0.0.1", 46000).await else {
                    return;
                };
                let mut stream = channel.into_stream();
                let cookie = hex::decode(cookie).unwrap_or_default();
                let setup = x11_setup_request(&cookie);
                if stream.write_all(&setup).await.is_err() {
                    return;
                }
                let mut reply = vec![0u8; 64];
                let n = stream.read(&mut reply).await.unwrap_or(0);
                log.lock().unwrap().events.push(format!(
                    "x11 reply {}",
                    String::from_utf8_lossy(&reply[..n])
                ));
            });
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        self.event(format!("exec {command}"));
        self.log.lock().unwrap().execs.push(command.clone());
        if self.spec.disconnect_on_exec {
            session.disconnect(russh::Disconnect::ProtocolError, "exec not supported", "en")?;
            return Ok(());
        }
        session.channel_success(channel)?;
        let output = self
            .spec
            .exec_output
            .clone()
            .unwrap_or_else(|| format!("ran: {command}\n"));
        session.data(channel, output.into_bytes())?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        // The channel is gone as far as session limits go.
        if self.channels.remove(&channel).is_some() {
            self.sessions = self.sessions.saturating_sub(1);
        }
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.event(format!("subsystem {name}"));
        if name == "sftp"
            && let Some(root) = self.spec.sftp_root.clone()
            && let Some(ch) = self.channels.remove(&channel)
        {
            session.channel_success(channel)?;
            tokio::spawn(super::sftp_server::serve(ch.into_stream(), root));
            return Ok(());
        }
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.event(format!("direct-tcpip {host_to_connect}:{port_to_connect}"));
        let target = format!("{host_to_connect}:{port_to_connect}");
        match tokio::net::TcpStream::connect(&target).await {
            Ok(mut tcp) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
                });
            }
            Err(_) => reply.reject(russh::ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", *port as u16)).await {
            Ok(listener) => listener,
            Err(_) => return Ok(false),
        };
        let bound = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        self.event(format!("tcpip-forward {address}:{} -> {bound}", *port));
        if *port == 0 {
            *port = u32::from(bound);
        }
        let handle = session.handle();
        let connected_address = address.to_string();
        let connected_port = *port;
        tokio::spawn(async move {
            while let Ok((mut tcp, peer)) = listener.accept().await {
                let handle = handle.clone();
                let connected_address = connected_address.clone();
                tokio::spawn(async move {
                    let Ok(channel) = handle
                        .channel_open_forwarded_tcpip(
                            connected_address,
                            connected_port,
                            peer.ip().to_string(),
                            u32::from(peer.port()),
                        )
                        .await
                    else {
                        return;
                    };
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
                });
            }
        });
        Ok(true)
    }
}

/// A little-endian X11 connection setup carrying an MIT-MAGIC-COOKIE-1, as a client sends it.
fn x11_setup_request(cookie: &[u8]) -> Vec<u8> {
    let mut header = vec![b'l', 0, 0, 0, 0, 0];
    header[2..4].copy_from_slice(&11u16.to_le_bytes());
    header.resize(12, 0);
    crate::x11::proxy::rewrite_x11_setup(
        &header,
        true,
        Some(&crate::x11::xauthority::X11Auth {
            name: "MIT-MAGIC-COOKIE-1".to_string(),
            data: cookie.to_vec(),
        }),
    )
}
