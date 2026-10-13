//! One NETCONF or gNMI tab's backend (port of
//! `server/src/management/management-socket.ts`): it owns the SSH lease
//! (NETCONF's transport, or a gNMI gateway), the protocol client, and every
//! request still in flight.
//!
//! The TypeScript speaks JSON over `/ws/management`; here the same messages
//! travel over channels: [`ManagementSession::start`] takes
//! [`ManagementClientMessage`]s and emits [`ManagementServerMessage`]s.
//! What the session needs from the rest of the app (saved hosts, pinned
//! certificates, the password vault, SSH connections) comes in through
//! [`ManagementEnv`], so this crate depends on nothing but `muxus-core`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD};
use bytes::Bytes;
use muxus_core::js::{js_trim, now_iso};
use muxus_core::management::{
    GnmiEncoding, GnmiNotification, GnmiSessionInfo, GnmiTlsMode, GrpcJson, ManagementError,
    ManagementResult, ManagementSessionInfo, ManagementStreamMessage, ManagementTlsInfo,
    NetconfSessionInfo, PresentedCertificateInfo,
};
use muxus_core::protocol::{
    AuthPromptField, AuthPromptInfo, AuthPromptPurpose, AuthPromptResponse,
    DesktopCertificateChallenge, ExitReason, GnmiProfile, HostKeyChallenge,
    ManagementClientMessage, ManagementProfile, ManagementServerMessage, NetconfProfile,
    RememberPasswordOffer, SshGateway,
};
use muxus_core::validate::Validate;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::dial::{DialError, connect_tcp};
use crate::error::{Error, GrpcError, code, describe_error};
use crate::gnmi::values::pick_encoding;
use crate::gnmi::{
    CallEvent, CallHandle, CallOptions, GnmiClient, GnmiGetRequest, GnmiSetRequest,
    GnmiSubscribeRequest, SubscriptionControl, SubscriptionEvent,
};
use crate::netconf::{BoxedChannel, NetconfClient};
use crate::services::json::{decode_json, encode_json};
use crate::services::registry::{MethodDef, MethodKind, method_def};
use crate::services::transfer::{FileUpload, download_file, list_services};
use crate::tls::{PresentedCertificate, TlsFiles, certificate_challenge, upgrade_gnmi_tls};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LOGIN_ATTEMPTS: u32 = 3;
/// Subscription updates are forwarded in batches at most this often.
const NOTIFICATION_FLUSH: Duration = Duration::from_millis(100);
const NOTIFICATION_BATCH_LIMIT: usize = 2000;
/// Messages of a streaming call, like subscription updates, go out in batches.
const GRPC_BATCH_LIMIT: usize = 500;
/// Size of the base64 chunks a download is sent to the UI in.
const DOWNLOAD_CHUNK_BYTES: usize = 1024 * 1024;

/// A byte stream to a device: a TCP socket, an SSH channel, …
pub trait DeviceStream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> DeviceStream for T {}

pub type BoxedStream = Box<dyn DeviceStream>;

/// An SSH connection the session holds while it runs; dropping the last
/// reference releases it.
pub trait SshLease: Send + Sync + 'static {
    /// A direct-tcpip channel to `host:port`, resolved and dialed on the far side.
    fn open_direct_tcpip<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, Result<BoxedStream, DialError>>;
    /// An SSH subsystem channel (`netconf`).
    fn open_subsystem<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<BoxedChannel, String>>;
    /// `user@host:port` as dialed.
    fn address(&self) -> String;
    /// Resolves when the SSH connection closes, with the reason if there is one.
    fn closed(&self) -> BoxFuture<'static, Option<String>>;
}

/// A password's place in the vault, and how prompts name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordRef {
    pub account: String,
    pub label: String,
}

/// The password vault side of a gNMI login (the TypeScript's
/// `DesktopPasswords`): unlock and create prompts go through `io`.
pub trait PasswordVault: Send + Sync + 'static {
    fn has(&self, account: &str) -> bool;
    /// The saved password, unlocking the vault first when its policy asks for that.
    fn read<'a>(
        &'a self,
        reference: &'a PasswordRef,
        io: &'a SessionIo,
    ) -> BoxFuture<'a, Result<Option<String>, String>>;
    /// Save a password the user asked to remember, once the login has succeeded.
    fn remember<'a>(
        &'a self,
        reference: &'a PasswordRef,
        password: &'a str,
        io: &'a SessionIo,
    ) -> BoxFuture<'a, Result<(), String>>;
}

/// A certificate the user trusted, pinned by fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedIdentity {
    pub host: String,
    pub port: u16,
    pub gateway: String,
    pub fingerprint: String,
    pub subject: String,
}

/// What a session needs from the rest of the app. Every method has a
/// default that works without a store, a vault or SSH.
pub trait ManagementEnv: Send + Sync + 'static {
    /// A saved host's current settings; `None` when it no longer exists.
    fn saved_profile(&self, _profile_id: &str) -> Option<ManagementProfile> {
        None
    }

    /// Note a successful connection for the recent-hosts list.
    fn record_connection(&self, _profile_id: &str) {}

    /// The fingerprint pinned for a device, if any.
    fn trusted_identity(&self, _host: &str, _port: u16, _gateway: &str) -> Option<String> {
        None
    }

    fn trust_identity(&self, _identity: TrustedIdentity) {}

    fn passwords(&self) -> Option<Arc<dyn PasswordVault>> {
        None
    }

    /// A direct TCP connection to a device.
    fn connect_tcp<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, Result<BoxedStream, DialError>> {
        Box::pin(async move { Ok(Box::new(connect_tcp(host, port).await?) as BoxedStream) })
    }

    /// Connect (and authenticate) to the SSH gateway a gNMI host is reached through.
    fn connect_ssh_gateway<'a>(
        &'a self,
        _gateway: &'a SshGateway,
        _io: SessionIo,
    ) -> BoxFuture<'a, Result<Arc<dyn SshLease>, String>> {
        Box::pin(async { Err("SSH gateways are not available.".to_owned()) })
    }

    /// Connect (and authenticate) to a NETCONF server's SSH, through its
    /// gateway as a jump host when it has one.
    fn connect_netconf<'a>(
        &'a self,
        _profile: &'a NetconfProfile,
        _io: SessionIo,
    ) -> BoxFuture<'a, Result<Arc<dyn SshLease>, String>> {
        Box::pin(async { Err("SSH is not available.".to_owned()) })
    }
}

/// The keyring account a remote desktop or gNMI password is stored under
/// (`desktopPasswordAccount`).
pub fn desktop_password_account(
    protocol: &str,
    user: &str,
    host: &str,
    port: u16,
    gateway: &str,
    domain: &str,
) -> String {
    let json = serde_json::to_string(&serde_json::json!([
        protocol,
        user,
        host.to_lowercase(),
        port,
        gateway,
        domain.to_lowercase()
    ]))
    .expect("strings serialize");
    URL_SAFE_NO_PAD.encode(json)
}

/// How prompts name a stored password (`desktopPasswordLabel`).
pub fn desktop_password_label(
    protocol: &str,
    user: &str,
    host: &str,
    port: u16,
    gateway: &str,
    gateway_label: Option<&str>,
    domain: &str,
) -> String {
    let user = if !domain.is_empty() && !user.is_empty() {
        format!("{domain}\\{user}")
    } else {
        user.to_owned()
    };
    let name = if user.is_empty() {
        host.to_owned()
    } else {
        format!("{user}@{host}")
    };
    let route = if gateway.is_empty() {
        String::new()
    } else {
        format!(
            " via {}",
            gateway_label
                .filter(|label| !label.is_empty())
                .unwrap_or(gateway)
        )
    };
    let protocol = if protocol == "gnmi" {
        "gNMI".to_owned()
    } else {
        protocol.to_uppercase()
    };
    format!("{protocol} {name}:{port}{route}")
}

fn gateway_key(gateway: Option<&SshGateway>) -> String {
    match gateway {
        None => String::new(),
        Some(gateway) => match &gateway.profile_id {
            Some(id) => format!("profile:{id}"),
            None => format!("ssh:{}", gateway.target),
        },
    }
}

fn password_ref(profile: &GnmiProfile, username: &str) -> PasswordRef {
    let gateway = gateway_key(profile.ssh_gateway.as_ref());
    PasswordRef {
        account: desktop_password_account(
            "gnmi",
            username,
            &profile.host,
            profile.port,
            &gateway,
            "",
        ),
        label: desktop_password_label(
            "gnmi",
            username,
            &profile.host,
            profile.port,
            &gateway,
            profile
                .ssh_gateway
                .as_ref()
                .map(|gateway| gateway.target.as_str()),
            "",
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ResponseOp {
    Auth,
    HostKey,
    Certificate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Connecting,
    Done,
}

#[derive(Default, Clone)]
struct Running {
    cancel: CancellationToken,
    subscription: Option<SubscriptionControl>,
    /// An open streaming gNOI/gNSI call.
    call: Option<CallHandle>,
    method: Option<&'static MethodDef>,
    upload: Option<Arc<tokio::sync::Mutex<Option<FileUpload>>>>,
    upload_cancel: Option<CallHandle>,
}

impl Running {
    fn persistent(&self) -> bool {
        self.subscription.is_some() || self.call.is_some() || self.upload.is_some()
    }

    fn stop(&self) {
        self.cancel.cancel();
        if let Some(subscription) = &self.subscription {
            subscription.cancel();
        }
        if let Some(call) = &self.call {
            call.cancel(None);
        }
        if let Some(upload) = &self.upload_cancel {
            upload.cancel(None);
        }
    }
}

struct State {
    profile: Option<ManagementProfile>,
    lease: Option<Arc<dyn SshLease>>,
    gnmi: Option<GnmiClient>,
    gnmi_info: Option<GnmiSessionInfo>,
    encoding: GnmiEncoding,
    netconf: Option<Arc<NetconfClient<BoxedChannel>>>,
    running: HashMap<String, Running>,
}

struct Inner {
    env: Arc<dyn ManagementEnv>,
    out: async_channel::Sender<ManagementServerMessage>,
    closed: AtomicBool,
    stop: CancellationToken,
    state: Mutex<State>,
    waiters: Mutex<HashMap<ResponseOp, oneshot::Sender<ManagementClientMessage>>>,
    phase: watch::Sender<Phase>,
}

/// The prompts a connection may need on the way: status lines, logins and
/// host keys go to the UI and wait for its answer.
#[derive(Clone)]
pub struct SessionIo {
    session: Session,
}

impl SessionIo {
    pub fn status(&self, message: &str, transient: bool) {
        self.session.status(message, transient);
    }

    /// Ask the UI; `Err` when the session closed or another prompt replaced this one.
    pub async fn prompt(&self, info: AuthPromptInfo) -> Result<AuthPromptResponse, String> {
        self.session.prompt(info).await
    }

    pub async fn host_key(&self, challenge: HostKeyChallenge) -> Result<bool, String> {
        let reply = self.session.expect(ResponseOp::HostKey)?;
        self.session
            .send(ManagementServerMessage::HostKey(challenge));
        match reply.await {
            Ok(ManagementClientMessage::HostKeyResponse { accept }) => Ok(accept),
            _ => Err("connection closed".into()),
        }
    }
}

/// A running management session.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

/// Entry point: [`ManagementSession::start`].
pub struct ManagementSession;

impl ManagementSession {
    /// Start a session on the current tokio runtime. Send `connect` first;
    /// the session ends when the input closes, the device goes away or no
    /// `connect` arrives within 30 seconds, and then closes its output.
    pub fn start(
        env: Arc<dyn ManagementEnv>,
    ) -> (
        async_channel::Sender<ManagementClientMessage>,
        async_channel::Receiver<ManagementServerMessage>,
    ) {
        let (input_tx, input) = async_channel::unbounded();
        let (out, output) = async_channel::unbounded();
        let session = Session {
            inner: Arc::new(Inner {
                env,
                out,
                closed: AtomicBool::new(false),
                stop: CancellationToken::new(),
                state: Mutex::new(State {
                    profile: None,
                    lease: None,
                    gnmi: None,
                    gnmi_info: None,
                    encoding: GnmiEncoding::JsonIetf,
                    netconf: None,
                    running: HashMap::new(),
                }),
                waiters: Mutex::default(),
                phase: watch::channel(Phase::Idle).0,
            }),
        };
        tokio::spawn(session.run(input));
        (input_tx, output)
    }
}

fn request_id(message: &ManagementClientMessage) -> Option<&str> {
    use ManagementClientMessage as M;
    match message {
        M::GnmiCapabilities { id }
        | M::GnmiGet { id, .. }
        | M::GnmiSet { id, .. }
        | M::GnmiSubscribe { id, .. }
        | M::GnmiPoll { id }
        | M::NetconfRpc { id, .. }
        | M::GrpcCall { id, .. }
        | M::GrpcStream { id, .. }
        | M::GrpcSend { id, .. }
        | M::GrpcCloseSend { id }
        | M::GnoiFileGet { id, .. }
        | M::GnoiFilePut { id, .. }
        | M::GnoiFileChunk { id, .. }
        | M::TlsProbe { id }
        | M::Cancel { id } => Some(id),
        M::Connect { .. }
        | M::AuthResponse(_)
        | M::HostKeyResponse { .. }
        | M::CertificateResponse { .. } => None,
    }
}

fn subsystem_error(message: &str) -> String {
    let lower = message.to_lowercase();
    if lower.contains("unknown") || lower.contains("not supported") || lower.contains("fail") {
        "The SSH server has no NETCONF subsystem. Check that NETCONF is enabled on the device and the port is right (usually 830).".into()
    } else {
        format!("Could not open the NETCONF subsystem: {message}")
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

/// A connect failure as the exit message shows it.
enum ConnectFailure {
    Dial(DialError),
    Message(String),
}

impl From<String> for ConnectFailure {
    fn from(message: String) -> Self {
        ConnectFailure::Message(message)
    }
}

impl From<&str> for ConnectFailure {
    fn from(message: &str) -> Self {
        ConnectFailure::Message(message.to_owned())
    }
}

impl From<DialError> for ConnectFailure {
    fn from(err: DialError) -> Self {
        ConnectFailure::Dial(err)
    }
}

impl From<Error> for ConnectFailure {
    fn from(err: Error) -> Self {
        ConnectFailure::Message(err.to_string())
    }
}

impl ConnectFailure {
    fn message(&self) -> String {
        match self {
            ConnectFailure::Dial(err) => err.user_message(),
            ConnectFailure::Message(message) => message.clone(),
        }
    }
}

impl Session {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock().expect("session state lock")
    }

    fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    fn send(&self, message: ManagementServerMessage) {
        if !self.is_closed() {
            let _ = self.inner.out.try_send(message);
        }
    }

    fn stream(&self, message: ManagementStreamMessage) {
        self.send(ManagementServerMessage::Stream(message));
    }

    fn status(&self, message: &str, transient: bool) {
        self.send(ManagementServerMessage::Status {
            message: message.to_owned(),
            transient: Some(transient),
        });
    }

    fn io(&self) -> SessionIo {
        SessionIo {
            session: self.clone(),
        }
    }

    async fn run(self, input: async_channel::Receiver<ManagementClientMessage>) {
        let connect_deadline = tokio::time::sleep(CONNECT_TIMEOUT);
        tokio::pin!(connect_deadline);
        loop {
            let waiting_for_connect = self.state().profile.is_none();
            tokio::select! {
                message = input.recv() => match message {
                    Ok(message) => self.dispatch(message),
                    Err(_) => break,
                },
                () = &mut connect_deadline, if waiting_for_connect => break,
                () = self.inner.stop.cancelled() => break,
            }
        }
        self.close();
    }

    fn dispatch(&self, mut message: ManagementClientMessage) {
        if let Err(err) = message.validate() {
            if let Some(id) = request_id(&message) {
                self.stream(ManagementStreamMessage::Error {
                    id: id.to_owned(),
                    error: ManagementError {
                        message: err.message,
                        code: None,
                        cancelled: None,
                    },
                    duration_ms: 0,
                });
            }
            return;
        }
        use ManagementClientMessage as M;
        match message {
            M::AuthResponse(_) => self.answer(ResponseOp::Auth, message),
            M::HostKeyResponse { .. } => self.answer(ResponseOp::HostKey, message),
            M::CertificateResponse { .. } => self.answer(ResponseOp::Certificate, message),
            M::Connect { profile } => {
                {
                    let mut state = self.state();
                    if state.profile.is_some() {
                        return;
                    }
                    state.profile = Some(profile.clone());
                }
                self.inner.phase.send_replace(Phase::Connecting);
                let session = self.clone();
                tokio::spawn(async move {
                    if let Err(failure) = session.connect(profile).await {
                        session.fail(&failure.message());
                    }
                    session.inner.phase.send_replace(Phase::Done);
                });
            }
            M::Cancel { id } => {
                if let Some(running) = self.state().running.get(&id) {
                    running.stop();
                }
            }
            M::GnmiPoll { id } => {
                if let Some(subscription) = self
                    .state()
                    .running
                    .get(&id)
                    .and_then(|running| running.subscription.clone())
                {
                    subscription.poll();
                }
            }
            M::GrpcSend { id, message } => self.send_on_call(&id, message),
            M::GrpcCloseSend { id } => {
                if let Some(call) = self
                    .state()
                    .running
                    .get(&id)
                    .and_then(|running| running.call.clone())
                {
                    call.end();
                }
            }
            M::GnoiFileChunk { id, data, last } => self.upload_chunk(&id, data, last == Some(true)),
            request => {
                let session = self.clone();
                tokio::spawn(async move { session.request(request).await });
            }
        }
    }

    fn answer(&self, op: ResponseOp, message: ManagementClientMessage) {
        if let Some(waiter) = self.inner.waiters.lock().expect("waiters lock").remove(&op) {
            let _ = waiter.send(message);
        }
    }

    fn expect(&self, op: ResponseOp) -> Result<oneshot::Receiver<ManagementClientMessage>, String> {
        if self.is_closed() {
            return Err("connection closed".into());
        }
        let (sender, receiver) = oneshot::channel();
        // A newer prompt supersedes the one still waiting.
        self.inner
            .waiters
            .lock()
            .expect("waiters lock")
            .insert(op, sender);
        Ok(receiver)
    }

    async fn prompt(&self, info: AuthPromptInfo) -> Result<AuthPromptResponse, String> {
        let reply = self.expect(ResponseOp::Auth)?;
        self.send(ManagementServerMessage::AuthPrompt(info));
        match reply.await {
            Ok(ManagementClientMessage::AuthResponse(response)) => Ok(AuthPromptResponse {
                answers: response.answers,
                remember_password: response.remember_password,
                skipped: response.skipped,
            }),
            _ => Err("connection closed".into()),
        }
    }

    fn fail(&self, message: &str) {
        if self.is_closed() {
            return;
        }
        let host = self.state().profile.as_ref().map(profile_host);
        tracing::warn!(err = message, host = ?host, "management session failed");
        self.send(ManagementServerMessage::Exit {
            message: Some(message.to_owned()),
            reason: ExitReason::Failed,
        });
        self.close();
    }

    /// The transport went away under a live session.
    fn lost(&self, message: &str) {
        if self.is_closed() {
            return;
        }
        self.send(ManagementServerMessage::Exit {
            message: Some(message.to_owned()),
            reason: ExitReason::Disconnected,
        });
        self.close();
    }

    fn close(&self) {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.inner.stop.cancel();
        self.inner.waiters.lock().expect("waiters lock").clear();
        let (gnmi, netconf, lease, running) = {
            let mut state = self.state();
            (
                state.gnmi.take(),
                state.netconf.take(),
                state.lease.take(),
                std::mem::take(&mut state.running),
            )
        };
        for running in running.values() {
            running.stop();
        }
        if let Some(gnmi) = gnmi {
            gnmi.close();
        }
        if let Some(netconf) = netconf {
            tokio::spawn(async move {
                netconf.close().await;
                drop(lease);
            });
        } else {
            drop(lease);
        }
        self.inner.out.close();
    }

    /// Saved hosts connect with their stored settings, not whatever the UI sent.
    fn resolve_profile(&self, profile: ManagementProfile) -> Result<ManagementProfile, String> {
        let Some(id) = profile_id(&profile) else {
            return Ok(profile);
        };
        match self.inner.env.saved_profile(id) {
            Some(saved) if same_kind(&saved, &profile) => Ok(saved),
            _ => Err("This saved host no longer exists.".into()),
        }
    }

    async fn connect(&self, requested: ManagementProfile) -> Result<(), ConnectFailure> {
        let profile = self.resolve_profile(requested)?;
        self.state().profile = Some(profile.clone());
        match &profile {
            ManagementProfile::Gnmi(gnmi) => self.connect_gnmi(gnmi, &profile).await?,
            ManagementProfile::Netconf(netconf) => self.connect_netconf(netconf, &profile).await?,
        }
        if self.is_closed() {
            return Ok(());
        }
        if let Some(id) = profile_id(&profile) {
            self.inner.env.record_connection(id);
        }
        Ok(())
    }

    fn watch_lease(&self, lease: &Arc<dyn SshLease>, what: String) {
        let closed = lease.closed();
        let session = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                reason = closed => {
                    let reason = reason.filter(|reason| !reason.is_empty());
                    session.lost(&match reason {
                        Some(reason) => format!("{what} disconnected: {reason}"),
                        None => format!("{what} disconnected."),
                    });
                }
                () = session.inner.stop.cancelled() => {}
            }
        });
    }

    // gNMI

    async fn connect_gnmi(
        &self,
        profile: &GnmiProfile,
        full: &ManagementProfile,
    ) -> Result<(), ConnectFailure> {
        if let Some(gateway) = &profile.ssh_gateway {
            self.status(
                &format!("Connecting to the SSH gateway {} …", gateway.target),
                true,
            );
            let lease = self
                .inner
                .env
                .connect_ssh_gateway(gateway, self.io())
                .await?;
            if self.is_closed() {
                return Ok(());
            }
            self.watch_lease(&lease, format!("The SSH gateway {}", gateway.target));
            self.state().lease = Some(lease);
        }

        // Reach the device first: an unreachable host or an untrusted
        // certificate should surface before anyone types a password.
        let (client, tls) = self.open_gnmi(profile).await?;
        if self.is_closed() {
            client.close();
            return Ok(());
        }
        self.state().gnmi = Some(client.clone());
        {
            let closed = client.closed();
            let session = self.clone();
            tokio::spawn(async move {
                tokio::select! {
                    reason = closed => session.lost(&reason),
                    () = session.inner.stop.cancelled() => {}
                }
            });
        }

        let passwords = self.inner.env.passwords();
        let io = self.io();
        let mut username = profile
            .username
            .as_deref()
            .map(js_trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        let mut password: Option<String> = None;
        let mut used_saved = false;
        let mut remember: Option<(PasswordRef, String)> = None;
        if let (Some(name), Some(vault)) = (&username, &passwords) {
            let reference = password_ref(profile, name);
            if vault.has(&reference.account)
                && let Ok(Some(saved)) = vault.read(&reference, &io).await
            {
                password = Some(saved);
                used_saved = true;
                self.status(
                    &format!("Using the saved password for {}.", reference.label),
                    true,
                );
            }
        }
        if password.is_none() {
            let answer = self
                .ask_login(profile, username.as_deref(), false, used_saved)
                .await?;
            username = answer.username;
            password = answer.password;
            used_saved = false;
            remember = answer.remember;
        }
        if self.is_closed() {
            return Ok(());
        }

        let mut attempt = 1;
        let info = loop {
            client.set_credentials(username.as_deref(), password.as_deref());
            self.status("Asking the device for its capabilities …", true);
            match client.capabilities(None).await {
                Ok(capabilities) => {
                    let encoding = pick_encoding(&capabilities.encodings, profile.encoding);
                    self.state().encoding = encoding;
                    // Which gNOI and gNSI services share this server, when it says.
                    let services = list_services(&client).await;
                    break GnmiSessionInfo {
                        address: format!(
                            "{}{}:{}",
                            username
                                .as_deref()
                                .map(|name| format!("{name}@"))
                                .unwrap_or_default(),
                            profile.host,
                            profile.port
                        ),
                        via: profile
                            .ssh_gateway
                            .as_ref()
                            .map(|gateway| gateway.target.clone()),
                        connected_at: now_iso(),
                        version: capabilities.version,
                        encodings: capabilities.encodings,
                        models: capabilities.models,
                        tls: tls.clone(),
                        services,
                    };
                }
                Err(err) => {
                    if err.code != code::UNAUTHENTICATED || attempt >= MAX_LOGIN_ATTEMPTS {
                        return Err(format!("{}: {}", err.code_name(), err.message).into());
                    }
                    let answer = self
                        .ask_login(profile, username.as_deref(), true, used_saved)
                        .await?;
                    username = answer.username;
                    password = answer.password;
                    used_saved = false;
                    remember = answer.remember;
                    if self.is_closed() {
                        return Ok(());
                    }
                    attempt += 1;
                }
            }
        };
        self.state().gnmi_info = Some(info.clone());
        if let (Some((reference, secret)), Some(vault)) = (remember, &passwords)
            && let Err(err) = vault.remember(&reference, &secret, &io).await
        {
            self.status(
                &format!("The password could not be remembered: {err}"),
                false,
            );
        }
        if self.is_closed() {
            return Ok(());
        }
        self.send(ManagementServerMessage::Ready {
            info: ManagementSessionInfo::Gnmi(info),
            profile: full.clone(),
        });
        tracing::info!(host = %profile.host, port = profile.port, "gnmi session established");
        Ok(())
    }

    async fn ask_login(
        &self,
        profile: &GnmiProfile,
        known: Option<&str>,
        rejected: bool,
        used_saved: bool,
    ) -> Result<LoginAnswer, String> {
        let ask_username = known.is_none();
        let reference = known.map(|name| password_ref(profile, name));
        let passwords = self.inner.env.passwords();
        let mut prompts = Vec::new();
        if ask_username {
            prompts.push(AuthPromptField {
                prompt: "User name".into(),
                echo: true,
            });
        }
        prompts.push(AuthPromptField {
            prompt: "Password".into(),
            echo: false,
        });
        let response = self
            .prompt(AuthPromptInfo {
                name: Some("gNMI login".into()),
                host: Some(format!("{}:{}", profile.host, profile.port)),
                purpose: Some(AuthPromptPurpose::Authentication),
                instructions: if rejected {
                    Some(if used_saved {
                        "The saved password was not accepted. Enter the current password.".into()
                    } else {
                        "The device rejected the login.".into()
                    })
                } else if ask_username {
                    Some("gNMI devices take the user name and password with every request.".into())
                } else {
                    None
                },
                prompts,
                remember_password: passwords.as_ref().map(|vault| RememberPasswordOffer {
                    label: reference
                        .as_ref()
                        .map(|reference| reference.label.clone())
                        .unwrap_or_else(|| format!("gNMI {}:{}", profile.host, profile.port)),
                    existing: reference
                        .as_ref()
                        .is_some_and(|reference| vault.has(&reference.account)),
                }),
                skip_label: (ask_username && !rejected).then(|| "Connect without a login".into()),
            })
            .await?;
        if response.skipped == Some(true) {
            return Ok(LoginAnswer {
                username: None,
                password: None,
                remember: None,
            });
        }
        let username = if ask_username {
            response
                .answers
                .first()
                .map(|answer| js_trim(answer).to_owned())
                .filter(|answer| !answer.is_empty())
        } else {
            known.map(str::to_owned)
        };
        let password = response
            .answers
            .get(usize::from(ask_username))
            .cloned()
            .unwrap_or_default();
        let remember = (response.remember_password == Some(true))
            .then_some(())
            .and(username.as_ref())
            .map(|name| (password_ref(profile, name), password.clone()));
        Ok(LoginAnswer {
            username,
            password: Some(password),
            remember,
        })
    }

    async fn device_stream(&self, profile: &GnmiProfile) -> Result<BoxedStream, ConnectFailure> {
        let target = format!("{}:{}", profile.host, profile.port);
        let lease = self.state().lease.clone();
        Ok(match lease {
            Some(lease) => {
                let via = profile
                    .ssh_gateway
                    .as_ref()
                    .map(|gateway| gateway.target.clone())
                    .unwrap_or_default();
                self.status(&format!("Connecting to {target} through {via} …"), true);
                lease.open_direct_tcpip(&profile.host, profile.port).await?
            }
            None => {
                self.status(&format!("Connecting to {target} …"), true);
                self.inner
                    .env
                    .connect_tcp(&profile.host, profile.port)
                    .await?
            }
        })
    }

    async fn open_gnmi(
        &self,
        profile: &GnmiProfile,
    ) -> Result<(GnmiClient, ManagementTlsInfo), ConnectFailure> {
        let stream = self.device_stream(profile).await?;
        let mode = profile.tls.unwrap_or_default();
        let authority = format!(
            "{}:{}",
            profile.tls_server_name.as_deref().unwrap_or(&profile.host),
            profile.port
        );
        let mut tls_info = ManagementTlsInfo {
            mode,
            verified: None,
            pinned: None,
            fingerprint: None,
            subject: None,
            issuer: None,
            valid_to: None,
            protocol: None,
        };
        if mode == GnmiTlsMode::Plaintext {
            let client = GnmiClient::connect(stream, &authority, false)
                .await
                .map_err(|err| ConnectFailure::Message(err.message))?;
            return Ok((client, tls_info));
        }
        self.status("Starting TLS …", true);
        let files = TlsFiles::read(profile).await?;
        let tls = upgrade_gnmi_tls(stream, profile, &files).await?;
        tls_info.protocol = tls.protocol.clone();
        if let Some(certificate) = &tls.certificate {
            tls_info.fingerprint = Some(certificate.fingerprint.clone());
            tls_info.subject = Some(certificate.subject.clone());
            tls_info.issuer = Some(certificate.issuer.clone());
            tls_info.valid_to = Some(certificate.valid_to.clone());
            tls_info.verified = Some(certificate.verification_error.is_none());
        }
        if mode == GnmiTlsMode::Verify {
            let Some(certificate) = &tls.certificate else {
                return Err("The device presented no certificate.".into());
            };
            let (accepted, pinned) = self.accept_certificate(profile, certificate).await;
            if !accepted {
                return Err("The certificate was not trusted.".into());
            }
            tls_info.pinned = Some(pinned);
        }
        if self.is_closed() {
            return Err("connection closed".into());
        }
        let client = GnmiClient::connect(tls.stream, &authority, true)
            .await
            .map_err(|err| ConnectFailure::Message(err.message))?;
        Ok((client, tls_info))
    }

    async fn accept_certificate(
        &self,
        profile: &GnmiProfile,
        certificate: &PresentedCertificate,
    ) -> (bool, bool) {
        let gateway = gateway_key(profile.ssh_gateway.as_ref());
        let pinned = self
            .inner
            .env
            .trusted_identity(&profile.host, profile.port, &gateway);
        let Some(challenge) =
            certificate_challenge(certificate, &profile.host, profile.port, pinned.as_deref())
        else {
            return (true, certificate.verification_error.is_some());
        };
        let accept = self.certificate_verdict(challenge).await;
        if accept {
            self.inner.env.trust_identity(TrustedIdentity {
                host: profile.host.clone(),
                port: profile.port,
                gateway,
                fingerprint: certificate.fingerprint.clone(),
                subject: certificate.subject.clone(),
            });
        }
        (accept, accept)
    }

    async fn certificate_verdict(&self, challenge: DesktopCertificateChallenge) -> bool {
        let Ok(reply) = self.expect(ResponseOp::Certificate) else {
            return false;
        };
        self.send(ManagementServerMessage::Certificate(challenge));
        matches!(
            reply.await,
            Ok(ManagementClientMessage::CertificateResponse { accept: true })
        )
    }

    // NETCONF

    async fn connect_netconf(
        &self,
        profile: &NetconfProfile,
        full: &ManagementProfile,
    ) -> Result<(), ConnectFailure> {
        self.status(
            &format!("Connecting to {}:{} …", profile.host, profile.port),
            true,
        );
        let lease = self.inner.env.connect_netconf(profile, self.io()).await?;
        if self.is_closed() {
            return Ok(());
        }
        self.watch_lease(&lease, "The SSH connection".into());
        self.state().lease = Some(lease.clone());
        self.status("Opening the NETCONF subsystem …", true);
        let channel = lease
            .open_subsystem("netconf")
            .await
            .map_err(|err| subsystem_error(&err))?;
        if self.is_closed() {
            return Ok(());
        }
        let (client, mut notifications) = NetconfClient::open(channel)
            .await
            .map_err(|err| ConnectFailure::Message(err.message))?;
        let client = Arc::new(client);
        if self.is_closed() {
            client.close().await;
            return Ok(());
        }
        self.state().netconf = Some(client.clone());
        {
            let closed = client.closed();
            let session = self.clone();
            tokio::spawn(async move {
                tokio::select! {
                    reason = closed => session.lost(&reason),
                    () = session.inner.stop.cancelled() => {}
                }
            });
        }
        {
            let session = self.clone();
            tokio::spawn(async move {
                while let Some(notification) = notifications.recv().await {
                    session.stream(ManagementStreamMessage::NetconfNotification {
                        xml: notification.xml,
                        event_time: notification.event_time,
                        received_at: now_iso(),
                    });
                }
            });
        }
        let info = NetconfSessionInfo {
            address: lease.address(),
            via: profile
                .ssh_gateway
                .as_ref()
                .map(|gateway| gateway.target.clone()),
            connected_at: now_iso(),
            session_id: client.session_id.clone(),
            base: client.base,
            capabilities: client.capabilities.clone(),
            server_software: None,
        };
        self.send(ManagementServerMessage::Ready {
            info: ManagementSessionInfo::Netconf(info),
            profile: full.clone(),
        });
        tracing::info!(host = %profile.host, port = profile.port, "netconf session established");
        Ok(())
    }

    // Requests

    async fn request(&self, message: ManagementClientMessage) {
        let Some(id) = request_id(&message).map(str::to_owned) else {
            return;
        };
        if self.state().running.contains_key(&id) {
            self.stream(ManagementStreamMessage::Error {
                id,
                error: ManagementError {
                    message: "A request with this id is still running.".into(),
                    code: None,
                    cancelled: None,
                },
                duration_ms: 0,
            });
            return;
        }
        // Requests sent while the session is still connecting wait for it.
        let mut phase = self.inner.phase.subscribe();
        let _ = phase.wait_for(|phase| *phase != Phase::Connecting).await;
        let started = Instant::now();
        let running = Running::default();
        let cancel = running.cancel.clone();
        self.state().running.insert(id.clone(), running);
        let outcome = match message {
            ManagementClientMessage::GnmiSubscribe {
                prefix,
                mode,
                encoding,
                updates_only,
                subscriptions,
                ..
            } => self
                .subscribe(
                    &id,
                    GnmiSubscribeRequest {
                        prefix,
                        mode,
                        updates_only: updates_only == Some(true),
                        subscriptions,
                    },
                    encoding,
                )
                .map(|()| None),
            ManagementClientMessage::GrpcStream {
                method, request, ..
            } => self.open_grpc_stream(&id, &method, request).map(|()| None),
            ManagementClientMessage::GnoiFilePut {
                remote_file,
                permissions,
                ..
            } => self.require_gnmi().map(|client| {
                let upload = FileUpload::new(&client, &remote_file, permissions);
                if let Some(running) = self.state().running.get_mut(&id) {
                    running.upload_cancel = Some(upload.canceller());
                    running.upload = Some(Arc::new(tokio::sync::Mutex::new(Some(upload))));
                }
                self.stream(ManagementStreamMessage::GnoiFileAck {
                    id: id.clone(),
                    bytes: 0,
                });
                None
            }),
            other => self.execute(&id, other, &cancel).await.map(Some),
        };
        match outcome {
            Ok(Some((result, bytes))) => self.stream(ManagementStreamMessage::Result {
                id: id.clone(),
                result,
                duration_ms: elapsed_ms(started),
                bytes,
            }),
            Ok(None) => {}
            Err(err) => {
                self.stream(ManagementStreamMessage::Error {
                    id: id.clone(),
                    error: describe_error(&err),
                    duration_ms: elapsed_ms(started),
                });
                if let Some(running) = self.state().running.get_mut(&id) {
                    running.call = None;
                    running.upload = None;
                    running.subscription = None;
                }
            }
        }
        // Live subscriptions, streams and uploads stay registered until they end.
        let mut state = self.state();
        if state
            .running
            .get(&id)
            .is_some_and(|running| !running.persistent())
        {
            state.running.remove(&id);
        }
    }

    fn require_gnmi(&self) -> Result<GnmiClient, Error> {
        let state = self.state();
        match (&state.gnmi, &state.gnmi_info) {
            (Some(client), Some(_)) => Ok(client.clone()),
            _ => Err(Error::other("This is not a connected gNMI session.")),
        }
    }

    async fn execute(
        &self,
        id: &str,
        message: ManagementClientMessage,
        cancel: &CancellationToken,
    ) -> Result<(ManagementResult, u64), Error> {
        use ManagementClientMessage as M;
        match message {
            M::GrpcCall {
                method, request, ..
            } => {
                let method = allowed_method(&method, &[MethodKind::Unary])?;
                let request = encode_json(
                    &method.request,
                    &serde_json::Value::Object(request.unwrap_or_default()),
                )?;
                let (reply, bytes) = self
                    .require_gnmi()?
                    .unary_call(method.path, Bytes::from(request), Some(cancel))
                    .await?;
                let response = decode_json(&method.response, &reply).map_err(|err| {
                    GrpcError::new(
                        code::INTERNAL,
                        format!("Could not decode the response: {err}"),
                    )
                })?;
                Ok((
                    ManagementResult::GrpcCall {
                        method: method.path.to_owned(),
                        response,
                    },
                    bytes,
                ))
            }
            M::TlsProbe { .. } => {
                let profile = match self.state().profile.clone() {
                    Some(ManagementProfile::Gnmi(profile))
                        if profile.tls.unwrap_or_default() != GnmiTlsMode::Plaintext =>
                    {
                        profile
                    }
                    _ => return Err(Error::other("This session does not use TLS.")),
                };
                let lease = self.state().lease.clone();
                let stream = match lease {
                    Some(lease) => lease.open_direct_tcpip(&profile.host, profile.port).await,
                    None => {
                        self.inner
                            .env
                            .connect_tcp(&profile.host, profile.port)
                            .await
                    }
                }
                .map_err(|err| Error::other(err.user_message()))?;
                let files = TlsFiles::read(&profile).await?;
                let tls = upgrade_gnmi_tls(stream, &profile, &files).await?;
                drop(tls.stream);
                let certificate = tls
                    .certificate
                    .ok_or_else(|| Error::other("The device presented no certificate."))?;
                Ok((
                    ManagementResult::TlsProbe {
                        certificate: PresentedCertificateInfo {
                            fingerprint: certificate.fingerprint,
                            subject: certificate.subject,
                            issuer: certificate.issuer,
                            valid_from: certificate.valid_from,
                            valid_to: certificate.valid_to,
                            verification_error: certificate.verification_error,
                        },
                    },
                    0,
                ))
            }
            M::GnoiFileGet { remote_file, .. } => {
                let client = self.require_gnmi()?;
                let mut pending: Vec<u8> = Vec::new();
                let session = self.clone();
                let id = id.to_owned();
                let flush = |pending: &mut Vec<u8>| {
                    if pending.is_empty() {
                        return;
                    }
                    session.stream(ManagementStreamMessage::GnoiFileData {
                        id: id.clone(),
                        data: BASE64.encode(&pending[..]),
                    });
                    pending.clear();
                };
                let download = download_file(
                    &client,
                    &remote_file,
                    |chunk| {
                        pending.extend_from_slice(&chunk);
                        if pending.len() >= DOWNLOAD_CHUNK_BYTES {
                            flush(&mut pending);
                        }
                    },
                    Some(cancel),
                )
                .await?;
                flush(&mut pending);
                Ok((
                    ManagementResult::GnoiFileGet {
                        size: download.size,
                        verified: download.verified,
                        hash_method: download.hash_method,
                    },
                    download.size,
                ))
            }
            M::GnmiCapabilities { .. } => {
                let capabilities = self.require_gnmi()?.capabilities(Some(cancel)).await?;
                let info = {
                    let mut state = self.state();
                    let info = state
                        .gnmi_info
                        .as_mut()
                        .ok_or_else(|| Error::other("This is not a connected gNMI session."))?;
                    info.version = capabilities.version;
                    info.encodings = capabilities.encodings;
                    info.models = capabilities.models;
                    info.clone()
                };
                Ok((
                    ManagementResult::GnmiCapabilities { info },
                    capabilities.bytes,
                ))
            }
            M::GnmiGet {
                prefix,
                paths,
                data_type,
                encoding,
                depth,
                max_bytes,
                ..
            } => {
                let client = self.require_gnmi()?;
                let encoding = encoding.unwrap_or(self.state().encoding);
                let outcome = client
                    .get(
                        &GnmiGetRequest {
                            prefix,
                            paths,
                            data_type,
                            depth,
                            max_bytes,
                        },
                        encoding,
                        Some(cancel),
                    )
                    .await?;
                Ok((
                    ManagementResult::GnmiGet {
                        notifications: outcome.notifications,
                    },
                    outcome.bytes,
                ))
            }
            M::GnmiSet {
                prefix,
                updates,
                replaces,
                deletes,
                commit,
                ..
            } => {
                let outcome = self
                    .require_gnmi()?
                    .set(
                        &GnmiSetRequest {
                            prefix,
                            updates,
                            replaces,
                            deletes,
                            commit,
                        },
                        Some(cancel),
                    )
                    .await?;
                Ok((
                    ManagementResult::GnmiSet {
                        timestamp: outcome.timestamp,
                        results: outcome.results,
                    },
                    outcome.bytes,
                ))
            }
            M::NetconfRpc {
                xml, timeout_ms, ..
            } => {
                let netconf = self
                    .state()
                    .netconf
                    .clone()
                    .ok_or_else(|| Error::other("This is not a connected NETCONF session."))?;
                let reply = netconf
                    .rpc(&xml, timeout_ms.map(Duration::from_millis), Some(cancel))
                    .await?;
                Ok((
                    ManagementResult::NetconfRpc {
                        xml: reply.xml,
                        message_id: reply.message_id,
                    },
                    reply.bytes,
                ))
            }
            _ => Err(Error::other("Unexpected request.")),
        }
    }

    /// Server-streaming and bidirectional gNOI/gNSI calls: messages go to the UI in batches.
    fn open_grpc_stream(
        &self,
        id: &str,
        method: &str,
        request: Option<GrpcJson>,
    ) -> Result<(), Error> {
        let method = allowed_method(
            method,
            &[
                MethodKind::ServerStream,
                MethodKind::Bidi,
                MethodKind::ClientStream,
            ],
        )?;
        let client = self.require_gnmi()?;
        let first = request
            .map(|request| encode_json(&method.request, &serde_json::Value::Object(request)))
            .transpose()?;
        let mut call = client.open_call(method.path, CallOptions::default());
        if let Some(running) = self.state().running.get_mut(id) {
            running.call = Some(call.handle.clone());
            running.method = Some(method);
        }
        if let Some(first) = first {
            call.handle.write(Bytes::from(first));
        }
        if method.kind == MethodKind::ServerStream {
            call.handle.end();
        }
        let session = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let started = Instant::now();
            let mut batch: Vec<GrpcJson> = Vec::new();
            let mut deadline: Option<tokio::time::Instant> = None;
            let flush = |batch: &mut Vec<GrpcJson>| {
                if !batch.is_empty() {
                    session.stream(ManagementStreamMessage::GrpcMessages {
                        id: id.clone(),
                        messages: std::mem::take(batch),
                    });
                }
            };
            loop {
                let event = match deadline {
                    Some(at) => tokio::select! {
                        event = call.next() => event,
                        () = tokio::time::sleep_until(at) => {
                            flush(&mut batch);
                            deadline = None;
                            continue;
                        }
                    },
                    None => call.next().await,
                };
                match event {
                    Some(CallEvent::Message(reply)) => {
                        match decode_json(&method.response, &reply) {
                            Ok(message) => {
                                batch.push(message);
                                if batch.len() >= GRPC_BATCH_LIMIT {
                                    flush(&mut batch);
                                    deadline = None;
                                } else if deadline.is_none() {
                                    deadline =
                                        Some(tokio::time::Instant::now() + NOTIFICATION_FLUSH);
                                }
                            }
                            Err(err) => call.handle.cancel(Some(GrpcError::new(
                                code::INTERNAL,
                                format!("Could not decode a reply: {err}"),
                            ))),
                        }
                    }
                    ended => {
                        let error = match ended {
                            Some(CallEvent::End(error)) => error,
                            _ => None,
                        };
                        flush(&mut batch);
                        session.state().running.remove(&id);
                        let error = error.flatten_end();
                        session.stream(ManagementStreamMessage::StreamEnd {
                            id: id.clone(),
                            error,
                        });
                        tracing::debug!(
                            method = method.path,
                            duration_ms = elapsed_ms(started),
                            "grpc stream ended"
                        );
                        return;
                    }
                }
            }
        });
        Ok(())
    }

    fn send_on_call(&self, id: &str, payload: GrpcJson) {
        let (call, method) = {
            let state = self.state();
            let Some(running) = state.running.get(id) else {
                return;
            };
            let (Some(call), Some(method)) = (running.call.clone(), running.method) else {
                return;
            };
            (call, method)
        };
        match encode_json(&method.request, &serde_json::Value::Object(payload)) {
            Ok(message) => {
                call.write(Bytes::from(message));
            }
            Err(err) => call.cancel(Some(GrpcError::new(code::INVALID_ARGUMENT, err.0))),
        }
    }

    fn upload_chunk(&self, id: &str, data: String, last: bool) {
        let Some(upload) = self
            .state()
            .running
            .get(id)
            .and_then(|running| running.upload.clone())
        else {
            return;
        };
        let session = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let started = Instant::now();
            let mut slot = upload.lock().await;
            if slot.is_none() {
                return;
            }
            let result: Result<Option<u64>, GrpcError> = 'run: {
                if !data.is_empty() {
                    let bytes = match BASE64.decode(data.as_bytes()) {
                        Ok(bytes) => bytes,
                        Err(err) => {
                            break 'run Err(GrpcError::new(
                                code::INVALID_ARGUMENT,
                                err.to_string(),
                            ));
                        }
                    };
                    if let Some(current) = slot.as_mut()
                        && let Err(err) = current.write(&bytes).await
                    {
                        break 'run Err(err);
                    }
                }
                if !last {
                    break 'run Ok(None);
                }
                match slot.take() {
                    Some(finished) => finished.finish().await.map(Some),
                    None => Ok(None),
                }
            };
            match result {
                Ok(None) => {
                    let bytes = slot.as_ref().map_or(0, |upload| upload.bytes);
                    session.stream(ManagementStreamMessage::GnoiFileAck { id, bytes });
                }
                Ok(Some(size)) => {
                    session.state().running.remove(&id);
                    session.stream(ManagementStreamMessage::Result {
                        id,
                        result: ManagementResult::GnoiFilePut { size },
                        duration_ms: elapsed_ms(started),
                        bytes: size,
                    });
                }
                Err(err) => {
                    if let Some(upload) = slot.take() {
                        upload.cancel();
                    }
                    session.state().running.remove(&id);
                    session.stream(ManagementStreamMessage::Error {
                        id,
                        error: describe_error(&Error::Grpc(err)),
                        duration_ms: 0,
                    });
                }
            }
        });
    }

    fn subscribe(
        &self,
        id: &str,
        request: GnmiSubscribeRequest,
        encoding: Option<GnmiEncoding>,
    ) -> Result<(), Error> {
        let client = self.require_gnmi()?;
        let encoding = encoding.unwrap_or(self.state().encoding);
        let mut stream = client.subscribe(&request, encoding)?;
        if let Some(running) = self.state().running.get_mut(id) {
            running.subscription = Some(stream.control());
        }
        let session = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let mut batch: Vec<GnmiNotification> = Vec::new();
            let mut batch_bytes = 0;
            let mut deadline: Option<tokio::time::Instant> = None;
            let flush = |batch: &mut Vec<GnmiNotification>, bytes: &mut u64| {
                if !batch.is_empty() {
                    session.stream(ManagementStreamMessage::GnmiNotifications {
                        id: id.clone(),
                        notifications: std::mem::take(batch),
                        bytes: std::mem::take(bytes),
                    });
                }
            };
            loop {
                let event = match deadline {
                    Some(at) => tokio::select! {
                        event = stream.next() => event,
                        () = tokio::time::sleep_until(at) => {
                            flush(&mut batch, &mut batch_bytes);
                            deadline = None;
                            continue;
                        }
                    },
                    None => stream.next().await,
                };
                match event {
                    Some(SubscriptionEvent::Notification(notification, bytes)) => {
                        batch.push(notification);
                        batch_bytes += bytes;
                        if batch.len() >= NOTIFICATION_BATCH_LIMIT {
                            flush(&mut batch, &mut batch_bytes);
                            deadline = None;
                        } else if deadline.is_none() {
                            deadline = Some(tokio::time::Instant::now() + NOTIFICATION_FLUSH);
                        }
                    }
                    Some(SubscriptionEvent::Sync) => {
                        flush(&mut batch, &mut batch_bytes);
                        deadline = None;
                        session.stream(ManagementStreamMessage::GnmiSync { id: id.clone() });
                    }
                    ended => {
                        let error = match ended {
                            Some(SubscriptionEvent::End(error)) => error,
                            _ => None,
                        };
                        flush(&mut batch, &mut batch_bytes);
                        session.state().running.remove(&id);
                        let error = error.flatten_end();
                        session.stream(ManagementStreamMessage::StreamEnd {
                            id: id.clone(),
                            error,
                        });
                        return;
                    }
                }
            }
        });
        Ok(())
    }
}

/// How a stream's end is reported: a cancel from this side is not an error.
trait EndError {
    fn flatten_end(self) -> Option<ManagementError>;
}

impl EndError for Option<GrpcError> {
    fn flatten_end(self) -> Option<ManagementError> {
        self.filter(|error| error.code != code::CANCELLED)
            .map(|error| describe_error(&Error::Grpc(error)))
    }
}

struct LoginAnswer {
    username: Option<String>,
    password: Option<String>,
    remember: Option<(PasswordRef, String)>,
}

fn allowed_method(path: &str, kinds: &[MethodKind]) -> Result<&'static MethodDef, Error> {
    let method =
        method_def(path).ok_or_else(|| Error::other(format!("Muxus does not call {path}.")))?;
    if !kinds.contains(&method.kind) {
        return Err(Error::other(format!(
            "{path} is a {} call.",
            method.kind.as_str()
        )));
    }
    Ok(method)
}

fn profile_id(profile: &ManagementProfile) -> Option<&str> {
    match profile {
        ManagementProfile::Gnmi(profile) => profile.profile_id.as_deref(),
        ManagementProfile::Netconf(profile) => profile.profile_id.as_deref(),
    }
}

fn profile_host(profile: &ManagementProfile) -> String {
    match profile {
        ManagementProfile::Gnmi(profile) => profile.host.clone(),
        ManagementProfile::Netconf(profile) => profile.host.clone(),
    }
}

fn same_kind(a: &ManagementProfile, b: &ManagementProfile) -> bool {
    matches!(
        (a, b),
        (ManagementProfile::Gnmi(_), ManagementProfile::Gnmi(_))
            | (ManagementProfile::Netconf(_), ManagementProfile::Netconf(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stored_passwords_like_the_typescript() {
        // Buffer.from(JSON.stringify(["gnmi","admin","leaf1",57400,"",""])).toString("base64url")
        assert_eq!(
            desktop_password_account("gnmi", "admin", "Leaf1", 57400, "", ""),
            URL_SAFE_NO_PAD.encode(r#"["gnmi","admin","leaf1",57400,"",""]"#)
        );
        assert_eq!(
            desktop_password_label(
                "gnmi",
                "admin",
                "leaf1",
                57400,
                "ssh:bastion",
                Some("bastion"),
                ""
            ),
            "gNMI admin@leaf1:57400 via bastion"
        );
        assert_eq!(
            desktop_password_label("rdp", "bob", "pc", 3389, "", None, "CORP"),
            "RDP CORP\\bob@pc:3389"
        );
        assert_eq!(
            subsystem_error("subsystem request failed"),
            "The SSH server has no NETCONF subsystem. Check that NETCONF is enabled on the device and the port is right (usually 830)."
        );
    }
}
