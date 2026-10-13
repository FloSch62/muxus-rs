//! One RDP or VNC tab on the backend (`DesktopSession` in `desktop-socket.ts`, and the
//! `startRdp`/`startVnc`/`finish` half of `RemoteDesktopViewImpl.tsx`). It owns the optional
//! SSH gateway lease, the credentials in use and the desktop connection.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use muxus_core::api::SavedHostProfileInput;
use muxus_core::js::js_trim;
use muxus_core::protocol::{
    AuthPromptInfo, AuthPromptResponse, DesktopCertificateChallenge, DesktopCredentialType,
    DesktopCredentials, DesktopProfile, ExitReason, HostKeyChallenge, RdpProfile,
    RememberPasswordOffer, SavedHostSessionProfile, SshGateway, SshProfile, VncProfile,
};
use muxus_mgmt::dial::{DialError, DialFailure};
use muxus_rdp::passwords::{
    DesktopPasswords, DesktopPrompts, LogonAnswer, LogonPrompt, VaultPasswordRef,
};
use muxus_rdp::{PresentedCertificate, RdpError, RdpFailureKind, SocketFailure};
use muxus_ssh::connection::{
    BoxFuture, Cancelled, ConnectIo, ConnectOptions, ConnectionLeaseOwner, MuxedConnectionLease,
    SshConnectionManager,
};
use muxus_store::Database;
use muxus_store::vault::{
    DesktopPasswordProtocol, DesktopPasswordTarget, PasswordVault, desktop_password_account,
    desktop_password_label,
};
use muxus_vnc::{VncFailureKind, VncServerKey};
use tokio::sync::{oneshot, watch};

use super::vault::StoreDesktopVault;
use super::{
    DesktopConnection, DesktopCursor, DesktopEvent, DesktopInput, DesktopOptions, PixelOrder,
};

/// How long opening a `direct-tcpip` channel through the gateway may take
/// (`TCP_CONNECT_TIMEOUT_MS`).
const GATEWAY_CHANNEL_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a session that ended through a gateway waits to learn whether the gateway went
/// first.
const GATEWAY_CLOSE_GRACE: Duration = Duration::from_millis(250);

/// Development sandboxes give their invented desktop hosts loopback servers, the way
/// `hack/demo-resolve.mjs` did for the TypeScript server: a JSON object of host name → port.
pub const DEMO_HOST_MAP_ENV: &str = "MUXUS_DEMO_HOSTMAP";

/// The prompt the session waits on was withdrawn: the session closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Closed;

/// Answers the UI owes, one per kind (a newer question of a kind supersedes the older one).
#[derive(Default)]
struct Waiters {
    auth: Option<oneshot::Sender<AuthPromptResponse>>,
    host_key: Option<oneshot::Sender<bool>>,
    certificate: Option<oneshot::Sender<bool>>,
}

/// A password typed with "remember" ticked, held until the login works.
#[derive(Debug, Clone)]
struct RememberCandidate {
    reference: VaultPasswordRef,
    password: String,
    /// A user name typed at the prompt, kept on the saved host afterwards.
    username: Option<String>,
}

#[derive(Default)]
struct State {
    profile: Option<DesktopProfile>,
    /// The RDP logon in use.
    credentials: DesktopCredentials,
    /// The password in use came from the vault (so a rejection means it is stale).
    used_saved_password: bool,
    /// A VNC server rejected the last credentials; ask instead of using the vault.
    vnc_rejected: bool,
    remember: Option<RememberCandidate>,
    recorded: bool,
    /// Why the attempt in flight has to end, decided inside a handler (a cancelled logon).
    abort: Option<String>,
    connection: Option<DesktopConnection>,
}

pub(super) struct Session {
    database: Arc<Database>,
    vault: StoreDesktopVault,
    connections: SshConnectionManager,
    events: async_channel::Sender<DesktopEvent>,
    options: DesktopOptions,
    waiters: Mutex<Waiters>,
    state: Mutex<State>,
    closed: AtomicBool,
    closed_signal: watch::Sender<bool>,
}

/// An SSH gateway lease, released when the session lets go of it.
struct GatewayLease {
    lease: MuxedConnectionLease,
    target: String,
}

impl Drop for GatewayLease {
    fn drop(&mut self) {
        self.lease.release();
    }
}

fn profile_host(profile: &DesktopProfile) -> (&str, u16) {
    match profile {
        DesktopProfile::Rdp(p) => (&p.host, p.port),
        DesktopProfile::Vnc(p) => (&p.host, p.port),
    }
}

fn profile_gateway(profile: &DesktopProfile) -> Option<&SshGateway> {
    match profile {
        DesktopProfile::Rdp(p) => p.ssh_gateway.as_ref(),
        DesktopProfile::Vnc(p) => p.ssh_gateway.as_ref(),
    }
}

fn profile_id(profile: &DesktopProfile) -> Option<&str> {
    match profile {
        DesktopProfile::Rdp(p) => p.profile_id.as_deref(),
        DesktopProfile::Vnc(p) => p.profile_id.as_deref(),
    }
}

fn kind_label(profile: &DesktopProfile) -> &'static str {
    match profile {
        DesktopProfile::Rdp(_) => "RDP",
        DesktopProfile::Vnc(_) => "VNC",
    }
}

/// A user name as typed: trimmed, `None` when empty (`username?.trim() || undefined`).
fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(js_trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The loopback port a development sandbox serves `host` on, if any.
fn demo_port(host: &str) -> Option<u16> {
    let map = std::env::var(DEMO_HOST_MAP_ENV).ok()?;
    let map: serde_json::Value = serde_json::from_str(&map).ok()?;
    map.get(host)?
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
}

/// A gateway's channel-open failure the way the RDP client explains socket errors.
fn rdp_dial_message(error: &DialError) -> String {
    let error = match error.failure {
        DialFailure::Refused => RdpError::Socket(SocketFailure::Refused),
        DialFailure::TimedOut => RdpError::Socket(SocketFailure::TimedOut),
        DialFailure::NotFound => RdpError::Socket(SocketFailure::HostNotFound),
        DialFailure::Unreachable => RdpError::Socket(SocketFailure::HostUnreachable),
        DialFailure::Denied | DialFailure::Other => RdpError::ConnectFailed,
    };
    error.describe().message
}

impl Session {
    pub(super) fn new(
        database: Arc<Database>,
        vault: Arc<PasswordVault>,
        connections: SshConnectionManager,
        events: async_channel::Sender<DesktopEvent>,
        options: DesktopOptions,
    ) -> Arc<Self> {
        Arc::new(Self {
            database,
            vault: StoreDesktopVault(vault),
            connections,
            events,
            options,
            waiters: Mutex::new(Waiters::default()),
            state: Mutex::new(State::default()),
            closed: AtomicBool::new(false),
            closed_signal: watch::channel(false).0,
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn waiters(&self) -> MutexGuard<'_, Waiters> {
        self.waiters.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(super) fn emit(&self, event: DesktopEvent) {
        // An unbounded queue: `try_send` only fails once the session is over.
        let _ = self.events.try_send(event);
    }

    pub(super) fn status(&self, message: &str, transient: bool) {
        self.emit(DesktopEvent::Status {
            message: message.to_owned(),
            transient,
        });
    }

    /// The profile being dialled (set before anything asks for it).
    /// Pretend `profile` was resolved (tests that drive a handler without a connection).
    #[cfg(test)]
    pub(super) fn set_profile(&self, profile: DesktopProfile) {
        self.state().profile = Some(profile);
    }

    fn profile(&self) -> DesktopProfile {
        self.state()
            .profile
            .clone()
            .expect("the profile is resolved before the session asks for it")
    }

    // --- prompts ---------------------------------------------------------------------------

    pub(super) async fn prompt(&self, info: AuthPromptInfo) -> Result<AuthPromptResponse, Closed> {
        if self.is_closed() {
            return Err(Closed);
        }
        let (tx, rx) = oneshot::channel();
        self.waiters().auth = Some(tx);
        self.emit(DesktopEvent::AuthPrompt(info));
        rx.await.map_err(|_| Closed)
    }

    async fn ask_host_key(&self, challenge: HostKeyChallenge) -> Result<bool, Closed> {
        if self.is_closed() {
            return Err(Closed);
        }
        let (tx, rx) = oneshot::channel();
        self.waiters().host_key = Some(tx);
        self.emit(DesktopEvent::HostKey(challenge));
        rx.await.map_err(|_| Closed)
    }

    pub(super) async fn ask_certificate(
        &self,
        challenge: DesktopCertificateChallenge,
    ) -> Result<bool, Closed> {
        if self.is_closed() {
            return Err(Closed);
        }
        let (tx, rx) = oneshot::channel();
        self.waiters().certificate = Some(tx);
        self.emit(DesktopEvent::Certificate(challenge));
        rx.await.map_err(|_| Closed)
    }

    /// Route one of the UI's answers to the question waiting for it.
    pub(super) fn answer(&self, input: DesktopInput) {
        let mut waiters = self.waiters();
        match input {
            DesktopInput::AuthResponse(response) => {
                if let Some(waiter) = waiters.auth.take() {
                    let _ = waiter.send(response);
                }
            }
            DesktopInput::HostKeyResponse { accept } => {
                if let Some(waiter) = waiters.host_key.take() {
                    let _ = waiter.send(accept);
                }
            }
            DesktopInput::CertificateResponse { accept } => {
                if let Some(waiter) = waiters.certificate.take() {
                    let _ = waiter.send(accept);
                }
            }
            DesktopInput::Close => {
                drop(waiters);
                self.close();
            }
        }
    }

    /// The tab went away: withdraw every question and stop.
    pub(super) fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.waiters() = Waiters::default();
        self.closed_signal.send_replace(true);
    }

    // --- the session -----------------------------------------------------------------------

    /// Run the session to its end: [`DesktopEvent::Exit`] is the last event.
    pub(super) async fn run(
        self: Arc<Self>,
        requested: DesktopProfile,
        inputs: async_channel::Receiver<DesktopInput>,
    ) {
        let router = {
            let session = Arc::downgrade(&self);
            tokio::spawn(async move {
                while let Ok(input) = inputs.recv().await {
                    let Some(session) = session.upgrade() else {
                        return;
                    };
                    let close = input == DesktopInput::Close;
                    session.answer(input);
                    if close {
                        return;
                    }
                }
                // The handle was dropped: the tab is gone.
                if let Some(session) = session.upgrade() {
                    session.close();
                }
            })
        };
        let mut closed = self.closed_signal.subscribe();
        let outcome = tokio::select! {
            biased;
            _ = closed.wait_for(|closed| *closed) => None,
            outcome = self.drive(requested) => Some(outcome),
        };
        // Closing withdraws the open questions, which can end the attempt in the same instant:
        // the tab closing is what happened.
        let (message, reason) = match outcome {
            Some(outcome) if !self.is_closed() => outcome,
            _ => {
                let connected = self.state().connection.is_some();
                (
                    "The connection closed.".to_owned(),
                    if connected {
                        ExitReason::Disconnected
                    } else {
                        ExitReason::Failed
                    },
                )
            }
        };
        // Whatever ended the session, the desktop goes with it.
        if let Some(connection) = self.state().connection.take() {
            connection.shutdown();
        }
        self.close();
        {
            let mut state = self.state();
            state.remember = None;
            state.credentials = DesktopCredentials::default();
        }
        if reason == ExitReason::Failed {
            tracing::warn!(%message, "remote desktop session failed");
        } else {
            tracing::info!(%message, ?reason, "remote desktop session ended");
        }
        self.emit(DesktopEvent::Exit { message, reason });
        self.events.close();
        router.abort();
    }

    /// Dial, sign in and forward the desktop's events until it ends.
    async fn drive(self: &Arc<Self>, requested: DesktopProfile) -> (String, ExitReason) {
        let profile = match self.resolve_profile(requested) {
            Ok(profile) => profile,
            Err(message) => return (message, ExitReason::Failed),
        };
        self.state().profile = Some(profile.clone());
        self.emit(DesktopEvent::Dialing(profile.clone()));
        let gateway = match profile_gateway(&profile) {
            Some(gateway) => match self.dial_gateway(gateway).await {
                Ok(lease) => Some(lease),
                Err(message) => return (message, ExitReason::Failed),
            },
            None => None,
        };
        let gateway_closed = async {
            match &gateway {
                Some(lease) => {
                    let connection = lease.lease.connection().clone();
                    connection.closed().await
                }
                None => std::future::pending().await,
            }
        };
        let gateway_message = |reason: Option<String>| {
            let target = gateway
                .as_ref()
                .map(|lease| lease.target.as_str())
                .unwrap_or_default();
            match reason {
                Some(reason) if !reason.is_empty() => {
                    format!("The SSH gateway {target} disconnected: {reason}")
                }
                _ => format!("The SSH gateway {target} disconnected."),
            }
        };
        let outcome = tokio::select! {
            outcome = self.connect_and_run(&profile, gateway.as_ref()) => outcome,
            reason = gateway_closed => return (gateway_message(reason), ExitReason::Disconnected),
        };
        // A tunnel ends with its gateway: say so rather than how the desktop saw it go.
        if let Some(lease) = &gateway {
            let connection = lease.lease.connection().clone();
            if let Ok(reason) = tokio::time::timeout(GATEWAY_CLOSE_GRACE, connection.closed()).await
            {
                return (gateway_message(reason), ExitReason::Disconnected);
            }
        }
        outcome
    }

    /// Saved hosts connect with their stored settings, not whatever the tab holds.
    fn resolve_profile(&self, profile: DesktopProfile) -> Result<DesktopProfile, String> {
        let Some(id) = profile_id(&profile).map(str::to_owned) else {
            return Ok(profile);
        };
        let missing = || "This saved host no longer exists.".to_owned();
        let saved = self
            .database
            .saved_host_profile(&id)
            .ok()
            .flatten()
            .ok_or_else(missing)?;
        match (saved.profile, &profile) {
            (SavedHostSessionProfile::Rdp(saved), DesktopProfile::Rdp(_)) => {
                Ok(DesktopProfile::Rdp(RdpProfile {
                    profile_id: Some(id),
                    ..saved
                }))
            }
            (SavedHostSessionProfile::Vnc(saved), DesktopProfile::Vnc(_)) => {
                Ok(DesktopProfile::Vnc(VncProfile {
                    profile_id: Some(id),
                    ..saved
                }))
            }
            _ => Err(missing()),
        }
    }

    async fn dial_gateway(self: &Arc<Self>, gateway: &SshGateway) -> Result<GatewayLease, String> {
        let ssh = SshProfile {
            profile_id: gateway.profile_id.clone(),
            ..SshProfile::new(gateway.target.clone())
        };
        self.status(
            &format!("Connecting to the SSH gateway {} …", gateway.target),
            true,
        );
        let io: Arc<dyn ConnectIo> = Arc::new(GatewayIo(Arc::downgrade(self)));
        let lease = self
            .connections
            .connect(
                &ssh,
                io,
                ConnectionLeaseOwner::Desktop,
                ConnectOptions::default(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let lease = GatewayLease {
            lease,
            target: gateway.target.clone(),
        };
        lease.lease.connection().wait_for_post_auth().await;
        Ok(lease)
    }

    /// `profile:<id>` for a saved SSH gateway, `ssh:<alias>` for an ssh_config one, `""` for a
    /// direct connection: pinned identities and saved passwords are kept apart per route.
    fn gateway_key(&self) -> String {
        let profile = self.state().profile.clone();
        match profile.as_ref().and_then(profile_gateway) {
            Some(SshGateway {
                profile_id: Some(id),
                ..
            }) => format!("profile:{id}"),
            Some(gateway) => format!("ssh:{}", gateway.target),
            None => String::new(),
        }
    }

    fn password_ref(&self, username: &str) -> VaultPasswordRef {
        let profile = self.profile();
        let (host, port) = profile_host(&profile);
        let target = DesktopPasswordTarget {
            protocol: match profile {
                DesktopProfile::Rdp(_) => DesktopPasswordProtocol::Rdp,
                DesktopProfile::Vnc(_) => DesktopPasswordProtocol::Vnc,
            },
            user: username.to_owned(),
            host: host.to_owned(),
            port,
            gateway: self.gateway_key(),
            gateway_label: profile_gateway(&profile).map(|gateway| gateway.target.clone()),
            domain: match &profile {
                DesktopProfile::Rdp(p) => p.domain.clone().unwrap_or_default(),
                DesktopProfile::Vnc(_) => String::new(),
            },
        };
        VaultPasswordRef {
            account: desktop_password_account(&target),
            label: desktop_password_label(&target),
        }
    }

    /// The stream to the desktop: the gateway's `direct-tcpip` channel, a development
    /// sandbox's loopback server, or `None` to dial directly. `Err` carries the failure
    /// sentence for `protocol`.
    async fn open_tunnel(
        &self,
        profile: &DesktopProfile,
        gateway: Option<&GatewayLease>,
    ) -> Result<Option<Box<dyn muxus_rdp::DesktopStream>>, DialError> {
        let (host, port) = profile_host(profile);
        let target = format!("{host}:{port}");
        if let Some(gateway) = gateway {
            self.status(
                &format!("Connecting to {target} through {} …", gateway.target),
                true,
            );
            let open =
                gateway
                    .lease
                    .connection()
                    .open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0);
            let channel = match tokio::time::timeout(GATEWAY_CHANNEL_TIMEOUT, open).await {
                Ok(Ok(channel)) => channel,
                Ok(Err(error)) => {
                    let prohibited = matches!(
                        error,
                        russh::Error::ChannelOpenFailure(
                            russh::ChannelOpenFailure::AdministrativelyProhibited
                        )
                    );
                    return Err(DialError::from_channel_open(&error.to_string(), prohibited));
                }
                Err(_) => {
                    return Err(DialError::new(
                        DialFailure::TimedOut,
                        format!("Timed out connecting to {target} through the SSH gateway"),
                    ));
                }
            };
            return Ok(Some(Box::new(channel.into_stream())));
        }
        if let Some(port) = demo_port(host) {
            self.status(&format!("Connecting to {target} …"), true);
            let stream = muxus_mgmt::dial::connect_tcp("127.0.0.1", port).await?;
            return Ok(Some(Box::new(stream)));
        }
        Ok(None)
    }

    async fn connect_and_run(
        self: &Arc<Self>,
        profile: &DesktopProfile,
        gateway: Option<&GatewayLease>,
    ) -> (String, ExitReason) {
        let connection = match profile {
            DesktopProfile::Rdp(rdp) => self.connect_rdp(profile, rdp, gateway).await,
            DesktopProfile::Vnc(vnc) => self.connect_vnc(profile, vnc, gateway).await,
        };
        let connection = match connection {
            Ok(connection) => connection,
            Err(message) => return (message, ExitReason::Failed),
        };
        match profile {
            DesktopProfile::Rdp(rdp) => {
                tracing::info!(host = %rdp.host, port = rdp.port, "rdp session established")
            }
            DesktopProfile::Vnc(vnc) => {
                tracing::info!(host = %vnc.host, port = vnc.port, "vnc session established")
            }
        }
        self.state().connection = Some(connection.clone());
        self.emit(DesktopEvent::Connected(connection.clone()));
        // Remembering a password may ask for the vault while the desktop is already up.
        tokio::spawn(self.clone().login_succeeded());
        match connection {
            DesktopConnection::Rdp(session) => self.forward_rdp(session).await,
            DesktopConnection::Vnc(session) => self.forward_vnc(session).await,
        }
    }

    fn rdp_options(&self, profile: &RdpProfile) -> muxus_rdp::RdpConnectOptions {
        let credentials = self.state().credentials.clone();
        muxus_rdp::RdpConnectOptions {
            username: credentials.username.unwrap_or_default(),
            password: credentials.password.unwrap_or_default(),
            domain: credentials.domain,
            width: self.options.width,
            height: self.options.height,
            share_clipboard: profile.share_clipboard != Some(false),
            pixel_order: match self.options.pixel_order {
                PixelOrder::Rgba => muxus_rdp::PixelOrder::Rgba,
                PixelOrder::Bgra => muxus_rdp::PixelOrder::Bgra,
            },
            ..muxus_rdp::RdpConnectOptions::new(profile.host.clone(), profile.port)
        }
    }

    async fn connect_rdp(
        self: &Arc<Self>,
        profile: &DesktopProfile,
        rdp: &RdpProfile,
        gateway: Option<&GatewayLease>,
    ) -> Result<DesktopConnection, String> {
        let mut rejected = false;
        loop {
            self.state().remember = None;
            self.load_rdp_credentials(rdp, rejected).await?;
            self.status("Negotiating the Remote Desktop session …", true);
            let transport = match self.open_tunnel(profile, gateway).await {
                Ok(Some(stream)) => muxus_rdp::RdpTransport::Tunnel(stream),
                Ok(None) => muxus_rdp::RdpTransport::Direct,
                Err(error) => return Err(rdp_dial_message(&error)),
            };
            let handler = Arc::new(RdpCallbacks(Arc::downgrade(self)));
            match muxus_rdp::connect(self.rdp_options(rdp), transport, handler).await {
                Ok(session) => return Ok(DesktopConnection::Rdp(session)),
                Err(failure) => {
                    if let Some(message) = self.state().abort.take() {
                        return Err(message);
                    }
                    if failure.kind == RdpFailureKind::Credentials {
                        self.status(&failure.message, true);
                        rejected = true;
                        continue;
                    }
                    return Err(failure.message);
                }
            }
        }
    }

    async fn connect_vnc(
        self: &Arc<Self>,
        profile: &DesktopProfile,
        vnc: &VncProfile,
        gateway: Option<&GatewayLease>,
    ) -> Result<DesktopConnection, String> {
        loop {
            self.state().remember = None;
            self.status("Opening the VNC session …", true);
            let transport = match self.open_tunnel(profile, gateway).await {
                Ok(Some(stream)) => muxus_vnc::VncTransport::Tunnel(Box::new(stream)),
                Ok(None) => muxus_vnc::VncTransport::Direct,
                Err(error) => return Err(error.user_message()),
            };
            let options = muxus_vnc::VncConnectOptions {
                view_only: vnc.view_only == Some(true),
                resize_remote: vnc.resize_remote == Some(true),
                share_clipboard: vnc.share_clipboard != Some(false),
                width: self.options.width,
                height: self.options.height,
                pixel_order: match self.options.pixel_order {
                    PixelOrder::Rgba => muxus_vnc::PixelOrder::Rgba,
                    PixelOrder::Bgra => muxus_vnc::PixelOrder::Bgra,
                },
                ..muxus_vnc::VncConnectOptions::new(vnc.host.clone(), vnc.port)
            };
            let handler = Arc::new(VncCallbacks(Arc::downgrade(self)));
            match muxus_vnc::connect(options, transport, handler).await {
                Ok(session) => return Ok(DesktopConnection::Vnc(session)),
                Err(failure) => {
                    if let Some(message) = self.state().abort.take() {
                        return Err(message);
                    }
                    if failure.kind == VncFailureKind::Credentials {
                        self.status(&failure.message, true);
                        self.state().vnc_rejected = true;
                        continue;
                    }
                    return Err(failure.message);
                }
            }
        }
    }

    // --- credentials -----------------------------------------------------------------------

    /// RDP needs the whole logon before it connects: NLA runs CredSSP in the client, and
    /// TLS-only servers take it in the Client Info PDU.
    async fn load_rdp_credentials(
        self: &Arc<Self>,
        profile: &RdpProfile,
        rejected: bool,
    ) -> Result<(), String> {
        let username = trimmed(profile.username.as_deref());
        let domain = profile.domain.clone().filter(|domain| !domain.is_empty());
        if !rejected && let Some(user) = &username {
            let reference = self.password_ref(user);
            let prompts = Prompts(self.clone());
            let passwords = DesktopPasswords::new(Some(&self.vault), &prompts);
            let saved = passwords
                .read(&reference)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(password) = saved {
                self.status(
                    &format!("Using the saved password for {}.", reference.label),
                    true,
                );
                let mut state = self.state();
                state.credentials = DesktopCredentials {
                    username: Some(user.clone()),
                    password: Some(password),
                    domain,
                };
                state.used_saved_password = true;
                return Ok(());
            }
        }
        let answer = self
            .ask_login(
                username.clone(),
                username.is_none(),
                rejected,
                LogonPrompt::RDP,
            )
            .await?;
        self.state().credentials = DesktopCredentials {
            username: Some(answer.username),
            password: Some(answer.password),
            domain,
        };
        Ok(())
    }

    /// Ask for a password, and for the user name too when the host does not store one. A
    /// password marked to remember is held until the login works.
    async fn ask_login(
        self: &Arc<Self>,
        username: Option<String>,
        ask_username: bool,
        rejected: bool,
        name: &str,
    ) -> Result<LogonAnswer, String> {
        let profile = self.profile();
        let (host, port) = profile_host(&profile);
        let known = (!ask_username).then(|| self.password_ref(username.as_deref().unwrap_or("")));
        let prompts = Prompts(self.clone());
        let passwords = DesktopPasswords::new(Some(&self.vault), &prompts);
        let remember = passwords.available().then(|| RememberPasswordOffer {
            label: known
                .as_ref()
                .map(|known| known.label.clone())
                .unwrap_or_else(|| format!("{} {host}:{port}", kind_label(&profile))),
            existing: known.as_ref().is_some_and(|known| passwords.has(known)),
        });
        let prompt = LogonPrompt {
            name: name.to_owned(),
            host: format!("{host}:{port}"),
            username: if ask_username {
                None
            } else {
                username.filter(|user| !user.is_empty())
            },
            ask_username,
            rejected,
            used_saved_password: self.state().used_saved_password,
            remember,
        };
        let response = self
            .prompt(prompt.info())
            .await
            .map_err(|_| "connection closed".to_owned())?;
        let answer = prompt.answer(&response)?;
        let candidate = answer.remember.then(|| RememberCandidate {
            reference: self.password_ref(&answer.username),
            password: answer.password.clone(),
            username: (ask_username && !answer.username.is_empty())
                .then(|| answer.username.clone()),
        });
        let mut state = self.state();
        state.used_saved_password = false;
        state.remember = candidate;
        Ok(answer)
    }

    /// A VNC server asked for credentials in the middle of the RFB security handshake.
    async fn provide_vnc_credentials(
        self: &Arc<Self>,
        types: Vec<DesktopCredentialType>,
    ) -> Result<DesktopCredentials, String> {
        let profile = self.profile();
        let DesktopProfile::Vnc(vnc) = &profile else {
            return Err("connection closed".into());
        };
        let username = trimmed(vnc.username.as_deref());
        let needs_user = types.contains(&DesktopCredentialType::Username);
        let rejected = self.state().vnc_rejected;
        if !rejected && (username.is_some() || !needs_user) {
            let reference = self.password_ref(if needs_user {
                username.as_deref().unwrap_or_default()
            } else {
                ""
            });
            let prompts = Prompts(self.clone());
            let passwords = DesktopPasswords::new(Some(&self.vault), &prompts);
            let saved = passwords
                .read(&reference)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(password) = saved {
                self.status(
                    &format!("Using the saved password for {}.", reference.label),
                    true,
                );
                self.state().used_saved_password = true;
                return Ok(DesktopCredentials {
                    username,
                    password: Some(password),
                    domain: None,
                });
            }
        }
        let answer = self
            .ask_login(
                if needs_user {
                    username.clone()
                } else {
                    Some(String::new())
                },
                needs_user && username.is_none(),
                rejected,
                LogonPrompt::VNC,
            )
            .await?;
        self.state().vnc_rejected = false;
        Ok(DesktopCredentials {
            username: needs_user.then_some(answer.username),
            password: Some(answer.password),
            domain: None,
        })
    }

    /// The desktop accepted the login: record the recent connection and save a password
    /// marked to remember.
    async fn login_succeeded(self: Arc<Self>) {
        let (profile, candidate, record) = {
            let mut state = self.state();
            let Some(profile) = state.profile.clone() else {
                return;
            };
            let record = !state.recorded && profile_id(&profile).is_some();
            if record {
                state.recorded = true;
            }
            (profile, state.remember.take(), record)
        };
        let id = profile_id(&profile).map(str::to_owned);
        if record
            && let Some(id) = &id
            && let Err(error) = self.database.record_saved_host_connection(id)
        {
            tracing::warn!(%error, "could not record recent connection");
        }
        let Some(candidate) = candidate else {
            return;
        };
        // The desktop is already up: failing to save must never tear it down.
        if let Err(error) = self.remember(&candidate, id.as_deref()).await {
            if self.is_closed() {
                return;
            }
            tracing::warn!(%error, "could not remember the desktop password");
            self.status(
                &format!("The password could not be remembered: {error}"),
                false,
            );
        }
    }

    async fn remember(
        self: &Arc<Self>,
        candidate: &RememberCandidate,
        id: Option<&str>,
    ) -> Result<(), String> {
        let prompts = Prompts(self.clone());
        let passwords = DesktopPasswords::new(Some(&self.vault), &prompts);
        passwords
            .remember(&candidate.reference, &candidate.password)
            .await
            .map_err(|error| error.to_string())?;
        // A name typed at the prompt is what the saved password is filed under; keep it on
        // the host so the next connect finds the password.
        let (Some(username), Some(id)) = (&candidate.username, id) else {
            return Ok(());
        };
        let Some(saved) = self
            .database
            .saved_host_profile(id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        let profile = match saved.profile {
            SavedHostSessionProfile::Rdp(profile)
                if profile.username.as_deref().unwrap_or("").is_empty() =>
            {
                SavedHostSessionProfile::Rdp(RdpProfile {
                    profile_id: None,
                    username: Some(username.clone()),
                    ..profile
                })
            }
            SavedHostSessionProfile::Vnc(profile)
                if profile.username.as_deref().unwrap_or("").is_empty() =>
            {
                SavedHostSessionProfile::Vnc(VncProfile {
                    profile_id: None,
                    username: Some(username.clone()),
                    ..profile
                })
            }
            _ => return Ok(()),
        };
        self.database
            .save_saved_host_profile(&SavedHostProfileInput {
                id: Some(saved.id),
                name: saved.name,
                profile,
            })
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    // --- server identities -----------------------------------------------------------------

    fn pinned(&self, host: &str, port: u16, gateway: &str) -> Option<String> {
        self.database
            .trusted_desktop_identity(host, port, gateway)
            .ok()
            .flatten()
            .map(|identity| identity.fingerprint)
    }

    fn pin(&self, host: &str, port: u16, gateway: &str, fingerprint: &str, subject: &str) {
        if let Err(error) =
            self.database
                .trust_desktop_identity(host, port, gateway, fingerprint, subject)
        {
            tracing::warn!(%error, "could not pin the remote desktop identity");
        }
    }

    /// The RDP server's certificate, before any credentials go out: one that verifies, or the
    /// one pinned before, passes; anything else is the user's call, and an accepted one is
    /// pinned.
    pub(super) async fn accept_certificate(&self, certificate: PresentedCertificate) -> bool {
        let profile = self.profile();
        let (host, port) = profile_host(&profile);
        let gateway = self.gateway_key();
        let pinned = self.pinned(host, port, &gateway);
        let Some(challenge) =
            muxus_rdp::certificate_challenge(&certificate, host, port, pinned.as_deref())
        else {
            return true;
        };
        let accept = self.ask_certificate(challenge).await.unwrap_or(false);
        if accept {
            self.pin(
                host,
                port,
                &gateway,
                &certificate.fingerprint,
                &certificate.subject,
            );
        }
        accept
    }

    /// A VNC server's RSA-AES key, before any credentials are sent. VNC keys are almost never
    /// certified, so first contact and changes are the user's call, as for RDP.
    pub(super) async fn accept_server_key(&self, key: VncServerKey) -> bool {
        let profile = self.profile();
        if !matches!(profile, DesktopProfile::Vnc(_)) {
            return false;
        }
        let (host, port) = profile_host(&profile);
        let gateway = self.gateway_key();
        let pinned = self.pinned(host, port, &gateway);
        let Some(challenge) = muxus_vnc::server_key_challenge(&key, host, port, pinned.as_deref())
        else {
            return true;
        };
        let accept = self.ask_certificate(challenge).await.unwrap_or(false);
        if accept {
            self.pin(
                host,
                port,
                &gateway,
                &key.fingerprint,
                &muxus_vnc::server_key_subject(&key),
            );
        }
        accept
    }

    // --- the running desktop ---------------------------------------------------------------

    async fn forward_rdp(&self, session: muxus_rdp::RdpSession) -> (String, ExitReason) {
        use muxus_rdp::{CursorShape, RdpEvent, SessionEnd};
        let events = session.events();
        while let Ok(event) = events.recv().await {
            self.emit(match event {
                RdpEvent::Resized { width, height } => DesktopEvent::Resized { width, height },
                RdpEvent::Frame => DesktopEvent::Frame,
                RdpEvent::Cursor(shape) => DesktopEvent::Cursor(match shape {
                    CursorShape::Default => DesktopCursor::Default,
                    CursorShape::Hidden => DesktopCursor::Hidden,
                    CursorShape::Image {
                        width,
                        height,
                        hotspot_x,
                        hotspot_y,
                        rgba,
                    } => DesktopCursor::Image {
                        width,
                        height,
                        hotspot_x,
                        hotspot_y,
                        rgba,
                    },
                }),
                RdpEvent::PointerPosition { x, y } => DesktopEvent::PointerPosition { x, y },
                RdpEvent::RemoteClipboard(text) => DesktopEvent::RemoteClipboard(text),
                RdpEvent::Ended(end) => {
                    return match end {
                        SessionEnd::Completed { .. } => (end.message(), ExitReason::Completed),
                        SessionEnd::Failed(failure) => (failure.message, ExitReason::Disconnected),
                        SessionEnd::Closed => (end.message(), ExitReason::Disconnected),
                    };
                }
            });
        }
        ("The connection closed.".into(), ExitReason::Disconnected)
    }

    async fn forward_vnc(&self, session: muxus_vnc::VncSession) -> (String, ExitReason) {
        use muxus_vnc::{CursorShape, SessionEnd, VncEvent};
        let events = session.events();
        while let Ok(event) = events.recv().await {
            self.emit(match event {
                VncEvent::Resized { width, height } => DesktopEvent::Resized { width, height },
                VncEvent::Frame => DesktopEvent::Frame,
                VncEvent::Cursor(shape) => DesktopEvent::Cursor(match shape {
                    CursorShape::Hidden => DesktopCursor::Hidden,
                    CursorShape::Image {
                        width,
                        height,
                        hotspot_x,
                        hotspot_y,
                        rgba,
                    } => DesktopCursor::Image {
                        width,
                        height,
                        hotspot_x,
                        hotspot_y,
                        rgba,
                    },
                }),
                VncEvent::RemoteClipboard(text) => DesktopEvent::RemoteClipboard(text),
                VncEvent::DesktopName(name) => DesktopEvent::DesktopName(name),
                VncEvent::Bell => DesktopEvent::Bell,
                // Muxus offers no power menu (the TypeScript view never asked for XVP).
                VncEvent::PowerCapability => continue,
                VncEvent::Ended(end) => {
                    let reason = match end {
                        SessionEnd::Completed => ExitReason::Completed,
                        SessionEnd::Lost { .. } | SessionEnd::Closed => ExitReason::Disconnected,
                    };
                    return (end.message(), reason);
                }
            });
        }
        ("The connection closed.".into(), ExitReason::Disconnected)
    }
}

/// Status lines and prompts for [`DesktopPasswords`].
struct Prompts(Arc<Session>);

impl DesktopPrompts for Prompts {
    fn status(&self, message: &str, transient: bool) {
        self.0.status(message, transient);
    }

    fn prompt(&self, info: AuthPromptInfo) -> muxus_rdp::BoxFuture<'static, AuthPromptResponse> {
        let session = self.0.clone();
        Box::pin(async move {
            // A withdrawn prompt reads as skipped; the closing session ends the attempt anyway.
            session
                .prompt(info)
                .await
                .unwrap_or_else(|_| AuthPromptResponse {
                    skipped: Some(true),
                    ..AuthPromptResponse::default()
                })
        })
    }
}

/// The SSH gateway's prompts, over the desktop session.
struct GatewayIo(Weak<Session>);

impl ConnectIo for GatewayIo {
    fn status(&self, message: &str, transient: bool) {
        if let Some(session) = self.0.upgrade() {
            session.status(message, transient);
        }
    }

    fn prompt(&self, info: AuthPromptInfo) -> BoxFuture<'_, Result<AuthPromptResponse, Cancelled>> {
        Box::pin(async move {
            let session = self.0.upgrade().ok_or(Cancelled)?;
            session.prompt(info).await.map_err(|_| Cancelled)
        })
    }

    fn host_key(&self, challenge: HostKeyChallenge) -> BoxFuture<'_, Result<bool, Cancelled>> {
        Box::pin(async move {
            let session = self.0.upgrade().ok_or(Cancelled)?;
            session.ask_host_key(challenge).await.map_err(|_| Cancelled)
        })
    }
}

/// The RDP client's questions.
struct RdpCallbacks(Weak<Session>);

impl muxus_rdp::RdpHandler for RdpCallbacks {
    fn accept_certificate(
        &self,
        certificate: PresentedCertificate,
    ) -> muxus_rdp::BoxFuture<'static, bool> {
        let session = self.0.upgrade();
        Box::pin(async move {
            match session {
                Some(session) => session.accept_certificate(certificate).await,
                None => false,
            }
        })
    }

    fn status(&self, message: &str) {
        if let Some(session) = self.0.upgrade() {
            session.status(message, true);
        }
    }
}

/// The VNC client's questions.
struct VncCallbacks(Weak<Session>);

impl muxus_vnc::VncHandler for VncCallbacks {
    fn credentials(
        &self,
        types: Vec<DesktopCredentialType>,
    ) -> muxus_vnc::BoxFuture<'static, Option<DesktopCredentials>> {
        let session = self.0.upgrade();
        Box::pin(async move {
            let session = session?;
            match session.provide_vnc_credentials(types).await {
                Ok(credentials) => Some(credentials),
                Err(message) => {
                    session.state().abort = Some(message);
                    None
                }
            }
        })
    }

    fn verify_server_key(&self, key: VncServerKey) -> muxus_vnc::BoxFuture<'static, bool> {
        let session = self.0.upgrade();
        Box::pin(async move {
            match session {
                Some(session) => session.accept_server_key(key).await,
                None => false,
            }
        })
    }

    fn status(&self, message: &str) {
        if let Some(session) = self.0.upgrade() {
            session.status(message, true);
        }
    }
}
