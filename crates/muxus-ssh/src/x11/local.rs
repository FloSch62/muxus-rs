//! The local X server that forwarded X11 connections land on, and the per-transport forwarding
//! state ([`X11Transport`]).

#[cfg(any(windows, test))]
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use super::X11Error;
#[cfg(any(windows, test))]
use super::display::X11Endpoint;
use super::display::{
    DEFAULT_CONNECT_TIMEOUT, ParsedDisplay, Platform, X11Socket, connect_x11_endpoint,
    parse_display, xauth_target,
};
use super::proxy::splice_x11_connection;
#[cfg(any(windows, test))]
use super::vcxsrv::VCXSRV_EXECUTABLE;
#[cfg(any(windows, test))]
use super::vcxsrv::{BundledXServer, BundledXServerOptions, RunningXServer};
use super::xauthority::{MIT_MAGIC_COOKIE, X11Auth, read_xauth_cookie, xauthority_path};
use crate::local_env::LocalEnv;

/// Reads one environment variable. Injectable so tests never depend on the real environment.
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// An [`EnvLookup`] over this process's environment.
pub fn process_env() -> EnvLookup {
    Arc::new(|name| std::env::var(name).ok())
}

/// How long a macOS `launchctl getenv DISPLAY` answer is reused.
const LAUNCHD_DISPLAY_TTL: Duration = Duration::from_secs(5);
const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(2);

/// Shown when a host explicitly asks for X11 and the server refused `x11-req`.
pub const X11_REFUSED_MESSAGE: &str = "This server refused X11 forwarding. It needs \"X11Forwarding yes\" in sshd_config and xauth installed.";

/// The text an `x11-req` refusal carries (ssh2's wording, kept for log and error parity).
pub const X11_REJECTION_MESSAGE: &str = "Unable to request X11";

/// The error the SSH layer reports when the server answers `x11-req` with failure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, thiserror::Error)]
#[error("Unable to request X11 forwarding")]
pub struct X11RequestRefused;

/// Whether `err` (or anything in its source chain) is the server refusing `x11-req`: a
/// [`X11RequestRefused`], or an error whose message says "Unable to request X11".
pub fn is_x11_rejection(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(err);
    while let Some(err) = current {
        if err.is::<X11RequestRefused>() || err.to_string().contains(X11_REJECTION_MESSAGE) {
            return true;
        }
        current = err.source();
    }
    false
}

pub use muxus_core::api::{X11Defaults, X11SettingsUpdate, X11Source, X11Status};

/// Settings that leave every switch at its platform default.
pub fn no_settings() -> X11SettingsUpdate {
    X11SettingsUpdate {
        enabled: None,
        forward_by_default: None,
        clipboard: false,
    }
}

/// What a new SSH session ended up with for X11, as its connection summary shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum X11SessionState {
    On,
    Off,
    Refused,
    NoServer,
}

/// The parameters of an `x11-req` channel request (RFC 4254 6.3.1), as russh's
/// `Channel::request_x11(want_reply, single_connection, auth_protocol, auth_cookie, screen)`
/// takes them. The cookie is the transport's fake one, never the local X server's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X11Request {
    pub single_connection: bool,
    pub auth_protocol: String,
    /// The fake cookie as lowercase hex, the way OpenSSH sends it.
    pub auth_cookie_hex: String,
    /// The remote DISPLAY becomes localhost:N.<screen>.
    pub screen: u32,
}

/// Identifies the SSH transport a bundled X display belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct X11Domain(u64);

impl X11Domain {
    /// A domain distinct from every other one in this process.
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for X11Domain {
    fn default() -> Self {
        Self::new()
    }
}

/// Settings for [`LocalX11`]. Nothing here is read from the Muxus store; the caller passes it.
#[derive(Clone)]
pub struct LocalX11Options {
    /// `$DISPLAY` to use instead of the environment's.
    pub display: Option<String>,
    /// Xauthority file to read instead of `$XAUTHORITY`, else `~/.Xauthority`.
    pub xauthority: Option<PathBuf>,
    /// Directory of the X server bundled with the Windows app (vcxsrv.exe). Only Windows builds
    /// use it.
    pub bundled_server_directory: Option<PathBuf>,
    /// Reads `DISPLAY`, `XAUTHORITY` and `HOME`; default the process environment.
    pub env: EnvLookup,
    pub platform: Platform,
    /// This machine's host name, for FamilyLocal Xauthority records; default the system's.
    pub hostname: Option<String>,
    /// Test seams for the bundled server.
    #[cfg(any(windows, test))]
    pub bundled: BundledXServerOptions,
}

impl Default for LocalX11Options {
    fn default() -> Self {
        Self {
            display: None,
            xauthority: None,
            bundled_server_directory: None,
            env: process_env(),
            platform: Platform::current(),
            hostname: None,
            #[cfg(any(windows, test))]
            bundled: BundledXServerOptions::default(),
        }
    }
}

impl std::fmt::Debug for LocalX11Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalX11Options")
            .field("display", &self.display)
            .field("xauthority", &self.xauthority)
            .field("bundled_server_directory", &self.bundled_server_directory)
            .field("platform", &self.platform)
            .field("hostname", &self.hostname)
            .finish_non_exhaustive()
    }
}

enum Source {
    #[cfg(any(windows, test))]
    Bundled(PathBuf),
    Display {
        display: String,
        parsed: ParsedDisplay,
    },
    None,
}

impl Source {
    fn kind(&self) -> X11Source {
        match self {
            #[cfg(any(windows, test))]
            Self::Bundled(_) => X11Source::Bundled,
            Self::Display { .. } => X11Source::Display,
            Self::None => X11Source::None,
        }
    }
}

/// A bundled X server serving one SSH transport, and its open X connections.
#[cfg(any(windows, test))]
struct BundledDisplay {
    server: BundledXServer,
    open: Arc<AtomicUsize>,
}

struct Inner {
    display_override: Option<String>,
    xauthority: Option<PathBuf>,
    env: EnvLookup,
    platform: Platform,
    hostname: String,
    #[cfg(any(windows, test))]
    bundled_directory: Option<PathBuf>,
    #[cfg(any(windows, test))]
    bundled_options: BundledXServerOptions,
    #[cfg(any(windows, test))]
    bundled_displays: Mutex<HashMap<X11Domain, BundledDisplay>>,
    launchd_display: Mutex<Option<(Option<String>, Instant)>>,
    /// User choices; unset switches follow the platform default.
    settings: Mutex<X11SettingsUpdate>,
}

/// The local X server that forwarded X11 connections land on.
///
/// On Windows that is the bundled VcXsrv, run as one display per SSH transport: programs from one
/// host cannot see, capture or drive windows from another, and unless the user opts in no
/// display is bridged to the Windows clipboard. That isolation is what makes forwarding safe to
/// turn on by default, like MobaXterm. Elsewhere it is the user's own display from $DISPLAY
/// (XQuartz's launchd socket on macOS); a remote client there can see that whole desktop, so
/// forwarding stays opt-in per host, as with OpenSSH's ForwardX11. On macOS the whole feature
/// starts switched off, since it needs XQuartz first.
///
/// Cheap to clone; clones share all state.
#[derive(Clone)]
pub struct LocalX11 {
    inner: Arc<Inner>,
}

/// An open connection to the local X server and the credentials it expects.
pub struct LocalX11Connection {
    pub socket: X11Socket,
    /// `None` when the Xauthority file has no matching record (the server may need none).
    pub auth: Option<X11Auth>,
    /// Counts this connection as an open window on a bundled display while it lives.
    open: Option<OpenGuard>,
}

impl std::fmt::Debug for LocalX11Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalX11Connection")
            .field("socket", &self.socket)
            .field("auth", &self.auth.as_ref().map(|auth| &auth.name))
            .finish_non_exhaustive()
    }
}

struct OpenGuard(Arc<AtomicUsize>);

impl Drop for OpenGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl LocalX11 {
    pub fn new(options: LocalX11Options) -> Self {
        let hostname = options
            .hostname
            .unwrap_or_else(|| LocalEnv::current().hostname);
        Self {
            inner: Arc::new(Inner {
                display_override: options.display.filter(|display| !display.is_empty()),
                xauthority: options.xauthority,
                env: options.env,
                platform: options.platform,
                hostname,
                #[cfg(any(windows, test))]
                bundled_directory: options.bundled_server_directory,
                #[cfg(any(windows, test))]
                bundled_options: BundledXServerOptions {
                    // Display numbers held by this process's bundled servers.
                    claimed_displays: Some(Arc::default()),
                    ..options.bundled
                },
                #[cfg(any(windows, test))]
                bundled_displays: Mutex::default(),
                launchd_display: Mutex::default(),
                settings: Mutex::new(no_settings()),
            }),
        }
    }

    /// Apply the application's X11 settings. A clipboard change reaches a running bundled
    /// display once it shows no windows, so it never closes them.
    pub fn apply_settings(&self, settings: X11SettingsUpdate) {
        *lock(&self.inner.settings) = settings;
    }

    /// The settings last applied.
    pub fn settings(&self) -> X11SettingsUpdate {
        *lock(&self.inner.settings)
    }

    pub async fn status(&self) -> X11Status {
        let source = self.source().await;
        let settings = self.settings();
        let defaults = X11Defaults {
            enabled: self.enabled_by_default(),
            forward_by_default: source.kind() == X11Source::Bundled,
        };
        X11Status {
            source: source.kind(),
            display: match &source {
                Source::Display { display, .. } => Some(display.clone()),
                _ => None,
            },
            enabled: self.enabled(),
            forward_by_default: settings
                .forward_by_default
                .unwrap_or(defaults.forward_by_default),
            clipboard: settings.clipboard,
            defaults,
        }
    }

    /// Whether X11 forwarding is switched on at all.
    pub fn enabled(&self) -> bool {
        self.settings()
            .enabled
            .unwrap_or_else(|| self.enabled_by_default())
    }

    /// macOS needs XQuartz installed first, so nothing about X11 appears there until the user
    /// turns it on.
    fn enabled_by_default(&self) -> bool {
        self.inner.platform != Platform::MacOs
    }

    /// Whether a session should ask for X11: the host's ForwardX11, else the app default.
    pub async fn wanted(&self, forward_x11: Option<bool>) -> bool {
        if !self.enabled() {
            return false;
        }
        let status = self.status().await;
        if status.source == X11Source::None {
            return false;
        }
        forward_x11.unwrap_or(status.forward_by_default)
    }

    /// Screen number sent in x11-req; remote DISPLAY becomes localhost:N.<screen>.
    pub async fn screen(&self) -> u32 {
        match self.source().await {
            Source::Display { parsed, .. } => parsed.screen,
            _ => 0,
        }
    }

    /// Open a connection to the local X server and the credentials it expects. `domain` (the SSH
    /// transport) selects its own bundled display.
    pub async fn connect(&self, domain: X11Domain) -> Result<LocalX11Connection, X11Error> {
        #[cfg(not(any(windows, test)))]
        let _ = domain;
        match self.source().await {
            Source::Display { parsed, .. } => {
                let socket =
                    connect_x11_endpoint(&parsed.endpoint, DEFAULT_CONNECT_TIMEOUT).await?;
                let target = xauth_target(&parsed.endpoint, socket.peer_ip());
                let file = self
                    .inner
                    .xauthority
                    .clone()
                    .unwrap_or_else(|| xauthority_path(&*self.inner.env));
                let auth =
                    read_xauth_cookie(&parsed.number, &target, &file, &self.inner.hostname).await;
                Ok(LocalX11Connection {
                    socket,
                    auth,
                    open: None,
                })
            }
            Source::None => Err(X11Error::NoServer),
            #[cfg(any(windows, test))]
            Source::Bundled(directory) => {
                let open = self.bundled_display(domain, &directory).1;
                let running = self.bundled_server(domain).await?;
                let endpoint = X11Endpoint::Tcp {
                    host: "127.0.0.1".into(),
                    port: running.port,
                };
                let socket = connect_x11_endpoint(&endpoint, DEFAULT_CONNECT_TIMEOUT).await?;
                open.fetch_add(1, Ordering::SeqCst);
                Ok(LocalX11Connection {
                    socket,
                    auth: Some(running.auth),
                    open: Some(OpenGuard(open)),
                })
            }
        }
    }

    /// The bundled X server for `domain`, started when needed.
    #[cfg(any(windows, test))]
    pub async fn bundled_server(&self, domain: X11Domain) -> Result<RunningXServer, X11Error> {
        let Source::Bundled(directory) = self.source().await else {
            return Err(X11Error::NoBundledServer);
        };
        let (server, open) = self.bundled_display(domain, &directory);
        // A clipboard change restarts the display only while it shows no windows.
        server
            .ensure_running(self.settings().clipboard, open.load(Ordering::SeqCst) == 0)
            .await
    }

    /// Stop the bundled display of an SSH transport that has closed.
    pub fn release(&self, domain: X11Domain) {
        #[cfg(any(windows, test))]
        {
            let bundled = lock(&self.inner.bundled_displays).remove(&domain);
            if let Some(bundled) = bundled {
                bundled.server.close();
            }
        }
        #[cfg(not(any(windows, test)))]
        let _ = domain;
    }

    #[cfg(any(windows, test))]
    fn bundled_display(
        &self,
        domain: X11Domain,
        directory: &std::path::Path,
    ) -> (BundledXServer, Arc<AtomicUsize>) {
        let mut displays = lock(&self.inner.bundled_displays);
        let bundled = displays.entry(domain).or_insert_with(|| BundledDisplay {
            server: BundledXServer::new(directory, self.inner.bundled_options.clone()),
            open: Arc::default(),
        });
        (bundled.server.clone(), bundled.open.clone())
    }

    /// Shown when a host asks for ForwardX11 but there is nothing local to forward to.
    pub fn missing_server_message(&self) -> String {
        let prefix = "X11 forwarding is on for this host, but no local X server was found.";
        match self.inner.platform {
            Platform::MacOs => format!(
                "{prefix} Install XQuartz from https://www.xquartz.org, log out and back in, then reconnect."
            ),
            Platform::Windows => format!(
                "{prefix} This Muxus build has no bundled X server; set DISPLAY to point at your own X server."
            ),
            _ => format!("{prefix} Start Muxus from a graphical session so that DISPLAY is set."),
        }
    }

    /// Stop every bundled display.
    pub fn close(&self) {
        #[cfg(any(windows, test))]
        {
            let displays: Vec<BundledDisplay> = lock(&self.inner.bundled_displays)
                .drain()
                .map(|(_, bundled)| bundled)
                .collect();
            for bundled in displays {
                bundled.server.close();
            }
        }
    }

    /// Per-transport state: one fake cookie and the handler for incoming X11 channels.
    pub fn attach(&self) -> X11Transport {
        let mut cookie = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut cookie);
        X11Transport {
            inner: Arc::new(TransportInner {
                local: self.clone(),
                domain: X11Domain::new(),
                cookie,
                refused_by_server: AtomicBool::new(false),
            }),
        }
    }

    async fn source(&self) -> Source {
        // Windows prefers its dedicated server over a $DISPLAY some other tool left behind.
        #[cfg(any(windows, test))]
        if let Some(directory) = &self.inner.bundled_directory
            && directory.join(VCXSRV_EXECUTABLE).exists()
        {
            return Source::Bundled(directory.clone());
        }
        let Some(display) = self.display_variable().await else {
            return Source::None;
        };
        match parse_display(&display, self.inner.platform) {
            Some(parsed) => Source::Display { display, parsed },
            None => Source::None,
        }
    }

    async fn display_variable(&self) -> Option<String> {
        if let Some(display) = &self.inner.display_override {
            return Some(display.clone());
        }
        if let Some(display) = (self.inner.env)("DISPLAY").filter(|value| !value.is_empty()) {
            return Some(display);
        }
        if self.inner.platform != Platform::MacOs {
            return None;
        }
        // Apps started from Finder may miss XQuartz's DISPLAY, but launchd has it. Ask again
        // after a while, so installing XQuartz needs no restart.
        if let Some((value, checked_at)) = lock(&self.inner.launchd_display).as_ref()
            && checked_at.elapsed() <= LAUNCHD_DISPLAY_TTL
        {
            return value.clone();
        }
        let value = launchd_display().await;
        *lock(&self.inner.launchd_display) = Some((value.clone(), Instant::now()));
        value
    }
}

/// `launchctl getenv DISPLAY`; only macOS has launchd.
async fn launchd_display() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let mut command = tokio::process::Command::new("launchctl");
    command
        .args(["getenv", "DISPLAY"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(LAUNCHCTL_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct TransportInner {
    local: LocalX11,
    domain: X11Domain,
    cookie: [u8; 16],
    refused_by_server: AtomicBool,
}

impl Drop for TransportInner {
    fn drop(&mut self) {
        // Its X clients are gone with the transport; so is their display.
        self.local.release(self.domain);
    }
}

/// X11 forwarding for one SSH transport. The server only ever sees a random fake cookie; the
/// real local credentials never leave this machine.
///
/// Cheap to clone; clones share the cookie and the refusal flag. The transport's bundled display
/// (Windows) stops when the last clone is dropped.
#[derive(Clone)]
pub struct X11Transport {
    inner: Arc<TransportInner>,
}

impl std::fmt::Debug for X11Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Transport")
            .field("domain", &self.inner.domain)
            .field("refused", &self.refused())
            .finish_non_exhaustive()
    }
}

impl X11Transport {
    /// The domain that keys this transport's bundled display.
    pub fn domain(&self) -> X11Domain {
        self.inner.domain
    }

    /// x11-req parameters for a new session channel, or `None` when X11 should not be requested.
    /// `forward_x11` is the host's resolved ForwardX11 (`None` follows the app default).
    pub async fn request(&self, forward_x11: Option<bool>) -> Option<X11Request> {
        if self.refused() || !self.inner.local.wanted(forward_x11).await {
            return None;
        }
        Some(X11Request {
            single_connection: false,
            auth_protocol: MIT_MAGIC_COOKIE.to_string(),
            auth_cookie_hex: hex::encode(self.inner.cookie),
            screen: self.inner.local.screen().await,
        })
    }

    /// The server answered x11-req with failure; stop asking on this transport.
    pub fn mark_refused(&self) {
        self.inner.refused_by_server.store(true, Ordering::SeqCst);
    }

    pub fn refused(&self) -> bool {
        self.inner.refused_by_server.load(Ordering::SeqCst)
    }

    /// What a new session's summary shows for X11. `forwarded` is whether the server accepted
    /// x11-req on that session channel. A reason is given only when the session asked for X11,
    /// mirroring [`status_message`](Self::status_message).
    pub async fn session_state(
        &self,
        forward_x11: Option<bool>,
        forwarded: bool,
    ) -> X11SessionState {
        let local = &self.inner.local;
        if forwarded {
            return X11SessionState::On;
        }
        if !local.enabled() {
            return X11SessionState::Off;
        }
        if local.status().await.source == X11Source::None {
            return if forward_x11 == Some(true) {
                X11SessionState::NoServer
            } else {
                X11SessionState::Off
            };
        }
        if self.refused() && local.wanted(forward_x11).await {
            X11SessionState::Refused
        } else {
            X11SessionState::Off
        }
    }

    /// The status line for a new session, if any. A host that explicitly sets ForwardX11 yes
    /// hears why it did not happen. The default (on only with the bundled Windows X server) stays
    /// silent, as MobaXterm does on servers without X11 forwarding; with X11 switched off in
    /// Settings, hosts' ForwardX11 is ignored silently too.
    pub async fn status_message(&self, forward_x11: Option<bool>) -> Option<String> {
        let local = &self.inner.local;
        if !local.enabled() || forward_x11 != Some(true) {
            return None;
        }
        if local.status().await.source == X11Source::None {
            Some(local.missing_server_message())
        } else if self.refused() {
            Some(X11_REFUSED_MESSAGE.to_string())
        } else {
            None
        }
    }

    /// Connect to the local X server for an incoming `x11` channel. On failure, refuse the
    /// channel (the failure is logged here).
    pub async fn open_local(&self) -> Result<LocalX11Connection, X11Error> {
        self.inner
            .local
            .connect(self.inner.domain)
            .await
            .inspect_err(|err| {
                tracing::warn!(
                    %err,
                    "forwarded X11 connection refused: the local X server is unavailable"
                );
            })
    }

    /// Relay an accepted `x11` channel to `local`: check the fake cookie, swap in the real
    /// credentials and splice both directions until the connection ends. A rejected client is
    /// logged with its originator address.
    pub async fn relay<S>(
        &self,
        local: LocalX11Connection,
        stream: S,
        originator_address: &str,
        originator_port: u32,
    ) -> Result<(), X11Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let LocalX11Connection { socket, auth, open } = local;
        let result = splice_x11_connection(stream, socket, &self.inner.cookie, auth.as_ref()).await;
        drop(open);
        if let Err(X11Error::Rejected(reason)) = &result {
            tracing::warn!(
                from = %format!("{originator_address}:{originator_port}"),
                reason,
                "rejected a forwarded X11 connection"
            );
        }
        result
    }

    /// [`open_local`](Self::open_local) then [`relay`](Self::relay), for a channel that is
    /// already accepted. When the local X server is unavailable the stream is dropped, which
    /// closes the channel.
    pub async fn handle_channel<S>(
        &self,
        stream: S,
        originator_address: &str,
        originator_port: u32,
    ) -> Result<(), X11Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let local = self.open_local().await?;
        self.relay(local, stream, originator_address, originator_port)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x11::proxy::{X11SetupParse, parse_x11_setup};
    use crate::x11::test_support::{FakeChildren, setup_request, write_xauthority};
    use std::path::Path;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn env(vars: &[(&str, &str)]) -> EnvLookup {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        Arc::new(move |name| {
            vars.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        })
    }

    fn local(platform: Platform, vars: &[(&str, &str)]) -> LocalX11 {
        LocalX11::new(LocalX11Options {
            env: env(vars),
            platform,
            hostname: Some("workstation".into()),
            ..LocalX11Options::default()
        })
    }

    fn bundled_dir(dir: &Path) -> PathBuf {
        std::fs::write(dir.join(VCXSRV_EXECUTABLE), b"").unwrap();
        dir.to_path_buf()
    }

    /// A short socket directory: Unix socket paths are limited to about 100 bytes.
    fn socket_dir() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("mx11").tempdir().unwrap()
    }

    #[tokio::test]
    async fn forwards_by_default_only_with_the_bundled_server() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = LocalX11::new(LocalX11Options {
            bundled_server_directory: Some(bundled_dir(dir.path())),
            env: env(&[("DISPLAY", ":0")]),
            platform: Platform::Windows,
            hostname: Some("workstation".into()),
            ..LocalX11Options::default()
        });
        assert_eq!(
            bundled.status().await,
            X11Status {
                source: X11Source::Bundled,
                display: None,
                enabled: true,
                forward_by_default: true,
                clipboard: false,
                defaults: X11Defaults {
                    enabled: true,
                    forward_by_default: true,
                },
            }
        );
        assert!(bundled.wanted(None).await);
        assert!(!bundled.wanted(Some(false)).await);

        let display = local(Platform::Linux, &[("DISPLAY", ":0.1")]);
        let status = display.status().await;
        assert_eq!(status.source, X11Source::Display);
        assert_eq!(status.display.as_deref(), Some(":0.1"));
        assert!(status.enabled);
        assert!(!status.forward_by_default);
        assert!(!display.wanted(None).await);
        assert!(display.wanted(Some(true)).await);
        assert_eq!(display.screen().await, 1);

        let none = LocalX11::new(LocalX11Options {
            bundled_server_directory: Some(dir.path().join("missing")),
            env: env(&[]),
            platform: Platform::Windows,
            hostname: Some("workstation".into()),
            ..LocalX11Options::default()
        });
        let status = none.status().await;
        assert_eq!(status.source, X11Source::None);
        assert!(status.enabled);
        assert!(!status.forward_by_default);
        assert!(!none.wanted(Some(true)).await);
        assert!(
            none.missing_server_message()
                .contains("no bundled X server")
        );
    }

    #[tokio::test]
    async fn starts_switched_off_on_macos_until_the_user_turns_x11_on() {
        let mac = local(
            Platform::MacOs,
            &[("DISPLAY", "/private/tmp/com.apple.launchd.x/org.xquartz:0")],
        );
        let status = mac.status().await;
        assert_eq!(status.source, X11Source::Display);
        assert!(!status.enabled);
        assert_eq!(
            status.defaults,
            X11Defaults {
                enabled: false,
                forward_by_default: false,
            }
        );
        assert!(!mac.enabled());
        // Even an explicit ForwardX11 yes stays quiet while X11 is off.
        assert!(!mac.wanted(Some(true)).await);

        mac.apply_settings(X11SettingsUpdate {
            enabled: Some(true),
            forward_by_default: None,
            clipboard: false,
        });
        assert!(mac.wanted(Some(true)).await);
        assert!(!mac.wanted(None).await);
        mac.apply_settings(X11SettingsUpdate {
            enabled: Some(true),
            forward_by_default: Some(true),
            clipboard: false,
        });
        assert!(mac.wanted(None).await);
        // Unset switches return to their platform default.
        mac.apply_settings(no_settings());
        assert!(!mac.enabled());
    }

    #[tokio::test]
    async fn gives_every_ssh_transport_its_own_bundled_display() {
        let dir = tempfile::tempdir().unwrap();
        let children = FakeChildren::new(&[], &[]);
        let x11 = LocalX11::new(LocalX11Options {
            bundled_server_directory: Some(bundled_dir(dir.path())),
            env: env(&[]),
            platform: Platform::Windows,
            hostname: Some("workstation".into()),
            bundled: children.options(dir.path()),
            ..LocalX11Options::default()
        });
        let (host_a, host_b) = (X11Domain::new(), X11Domain::new());

        // Concurrent first windows from two hosts must not end up on one display.
        let (a, b) = tokio::join!(x11.bundled_server(host_a), x11.bundled_server(host_b));
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_ne!(a.display, b.display);
        assert_ne!(a.auth.data, b.auth.data);
        assert_eq!(x11.bundled_server(host_a).await.unwrap(), a);

        // A closed transport takes its display down; the others keep theirs.
        let first = children
            .displays()
            .iter()
            .position(|arg| *arg == format!(":{}", a.display))
            .unwrap();
        x11.release(host_a);
        assert!(children.get(first).killed());
        assert!(!children.get(1 - first).killed());
        assert_eq!(x11.bundled_server(host_b).await.unwrap(), b);
        x11.close();
        assert!(children.get(1 - first).killed());
    }

    #[tokio::test]
    async fn a_dropped_transport_takes_its_bundled_display_down() {
        let dir = tempfile::tempdir().unwrap();
        let children = FakeChildren::new(&[], &[]);
        let x11 = LocalX11::new(LocalX11Options {
            bundled_server_directory: Some(bundled_dir(dir.path())),
            env: env(&[]),
            platform: Platform::Windows,
            hostname: Some("workstation".into()),
            bundled: children.options(dir.path()),
            ..LocalX11Options::default()
        });
        let transport = x11.attach();
        let clone = transport.clone();
        x11.bundled_server(transport.domain()).await.unwrap();
        drop(transport);
        assert!(!children.get(0).killed());
        drop(clone);
        assert!(children.get(0).killed());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn connects_to_display_with_the_cookie_from_xauthority() {
        let dir = socket_dir();
        let socket = dir.path().join("xsock:3");
        let xauthority = dir.path().join("Xauthority");
        write_xauthority(&xauthority, "3", &[3; 16]);
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let display = format!("{}:3", dir.path().join("xsock").display());
        let x11 = local(
            Platform::Linux,
            &[
                ("DISPLAY", display.as_str()),
                ("XAUTHORITY", xauthority.to_str().unwrap()),
            ],
        );
        let (connected, accepted) = tokio::join!(x11.connect(X11Domain::new()), listener.accept());
        accepted.unwrap();
        let connection = connected.unwrap();
        assert_eq!(connection.auth, Some(X11Auth::magic_cookie(vec![3; 16])));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_explicit_display_and_xauthority_override_the_environment() {
        let dir = socket_dir();
        let server = crate::x11::test_support::FakeXServer::start(dir.path(), "4", b"ok");
        let xauthority = dir.path().join("auth");
        write_xauthority(&xauthority, "4", &[4; 16]);
        let x11 = LocalX11::new(LocalX11Options {
            display: Some(server.display.clone()),
            xauthority: Some(xauthority),
            env: env(&[("DISPLAY", ":99"), ("XAUTHORITY", "/nonexistent")]),
            platform: Platform::Linux,
            hostname: Some("workstation".into()),
            ..LocalX11Options::default()
        });
        assert_eq!(
            x11.status().await.display.as_deref(),
            Some(server.display.as_str())
        );
        let connection = x11.connect(X11Domain::new()).await.unwrap();
        assert_eq!(connection.auth, Some(X11Auth::magic_cookie(vec![4; 16])));
    }

    // Ported from x11-forwarding.test.ts: the parts that do not need an SSH connection. The
    // remote X client writes into an in-memory stream standing in for the forwarded channel.

    #[cfg(unix)]
    #[tokio::test]
    async fn relays_remote_x_clients_to_the_local_display_with_the_real_cookie() {
        const REAL_COOKIE: [u8; 16] = [0x42; 16];
        let dir = socket_dir();
        let x_server = crate::x11::test_support::FakeXServer::start(dir.path(), "7", b"X11 ready");
        let xauthority = dir.path().join("Xauthority");
        write_xauthority(&xauthority, "7", &REAL_COOKIE);
        let x11 = local(
            Platform::Linux,
            &[
                ("DISPLAY", x_server.display.as_str()),
                ("XAUTHORITY", xauthority.to_str().unwrap()),
            ],
        );
        let transport = x11.attach();
        let request = transport.request(Some(true)).await.unwrap();
        assert_eq!(request.auth_protocol, MIT_MAGIC_COOKIE);
        assert!(!request.single_connection);
        assert_eq!(request.screen, 0);
        // The server only ever sees a fake cookie.
        assert_eq!(request.auth_cookie_hex.len(), 32);
        assert!(
            request
                .auth_cookie_hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_ne!(request.auth_cookie_hex, hex::encode(REAL_COOKIE));
        // Every session on the transport presents the same fake cookie.
        assert_eq!(transport.request(None).await, None);
        assert_eq!(
            transport.request(Some(true)).await.unwrap().auth_cookie_hex,
            request.auth_cookie_hex
        );

        // Like sshd hands it to xauth: the x11-req cookie is hex text.
        let (channel, mut remote_client) = tokio::io::duplex(4096);
        let relay = {
            let transport = transport.clone();
            tokio::spawn(async move { transport.handle_channel(channel, "127.0.0.1", 41000).await })
        };
        let fake = hex::decode(&request.auth_cookie_hex).unwrap();
        remote_client
            .write_all(&setup_request(true, &request.auth_protocol, &fake))
            .await
            .unwrap();
        let mut reply = [0u8; 9];
        remote_client.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"X11 ready");
        let setups = x_server.setups.lock().unwrap().clone();
        assert_eq!(setups.len(), 1);
        assert!(matches!(
            parse_x11_setup(&setups[0]),
            X11SetupParse::Complete { auth_name, auth_data, .. }
                if auth_name == MIT_MAGIC_COOKIE && auth_data == REAL_COOKIE
        ));
        assert_eq!(
            transport.session_state(Some(true), true).await,
            X11SessionState::On
        );
        assert_eq!(transport.status_message(Some(true)).await, None);

        drop(remote_client);
        drop(x_server);
        tokio::time::timeout(Duration::from_secs(5), relay)
            .await
            .unwrap()
            .unwrap()
            .ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_a_forwarded_connection_with_another_cookie() {
        let dir = socket_dir();
        let x_server = crate::x11::test_support::FakeXServer::start(dir.path(), "7", b"X11 ready");
        let xauthority = dir.path().join("Xauthority");
        write_xauthority(&xauthority, "7", &[0x42; 16]);
        let x11 = local(
            Platform::Linux,
            &[
                ("DISPLAY", x_server.display.as_str()),
                ("XAUTHORITY", xauthority.to_str().unwrap()),
            ],
        );
        let transport = x11.attach();
        let (channel, mut remote_client) = tokio::io::duplex(4096);
        remote_client
            .write_all(&setup_request(true, MIT_MAGIC_COOKIE, &[0x42; 16]))
            .await
            .unwrap();
        let result = transport.handle_channel(channel, "127.0.0.1", 41000).await;
        assert!(matches!(result, Err(X11Error::Rejected(_))));
        let mut rest = Vec::new();
        remote_client.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        assert!(x_server.setups.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stays_silent_while_x11_is_switched_off_as_on_macos_by_default() {
        let x11 = local(Platform::MacOs, &[("DISPLAY", ":0")]);
        let transport = x11.attach();
        assert_eq!(transport.request(Some(true)).await, None);
        assert_eq!(transport.status_message(Some(true)).await, None);
        assert_eq!(
            transport.session_state(Some(true), false).await,
            X11SessionState::Off
        );
    }

    #[tokio::test]
    async fn keeps_x11_off_by_default_when_forwarding_would_reach_the_user_desktop() {
        let x11 = local(Platform::Linux, &[("DISPLAY", ":0")]);
        let transport = x11.attach();
        assert_eq!(transport.request(None).await, None);
        assert_eq!(transport.status_message(None).await, None);
        assert_eq!(
            transport.session_state(None, false).await,
            X11SessionState::Off
        );
    }

    #[tokio::test]
    async fn names_a_missing_local_x_server_for_a_host_that_asks_for_x11() {
        let x11 = local(Platform::Linux, &[]);
        let transport = x11.attach();
        assert_eq!(transport.request(Some(true)).await, None);
        assert_eq!(
            transport.session_state(Some(true), false).await,
            X11SessionState::NoServer
        );
        assert_eq!(
            transport.session_state(None, false).await,
            X11SessionState::Off
        );
        let message = transport.status_message(Some(true)).await.unwrap();
        assert!(message.contains("no local X server was found"));
        assert!(message.contains("graphical session"));
        // An X11 channel the server opens anyway finds nothing to connect to.
        let (channel, _remote) = tokio::io::duplex(64);
        assert!(matches!(
            transport.handle_channel(channel, "127.0.0.1", 41000).await,
            Err(X11Error::NoServer)
        ));
    }

    #[tokio::test]
    async fn stops_asking_once_the_server_refuses_x11() {
        let x11 = local(Platform::Linux, &[("DISPLAY", ":0")]);
        let transport = x11.attach();
        assert!(transport.request(Some(true)).await.is_some());
        transport.mark_refused();
        assert!(transport.refused());
        // A second session on the shared transport does not repeat the refused request.
        let shared = transport.clone();
        assert_eq!(shared.request(Some(true)).await, None);
        assert_eq!(
            shared.session_state(Some(true), false).await,
            X11SessionState::Refused
        );
        assert_eq!(
            shared.status_message(Some(true)).await.as_deref(),
            Some(X11_REFUSED_MESSAGE)
        );
        // Hosts that did not ask for X11 hear nothing about it.
        assert_eq!(
            shared.session_state(None, false).await,
            X11SessionState::Off
        );
        assert_eq!(shared.status_message(None).await, None);
        // A fresh transport asks again.
        assert!(x11.attach().request(Some(true)).await.is_some());
    }

    #[test]
    fn recognises_a_refused_x11_request() {
        assert!(is_x11_rejection(&X11RequestRefused));
        let wrapped = std::io::Error::other(X11RequestRefused);
        assert!(is_x11_rejection(&wrapped));
        let worded = std::io::Error::other("Unable to request X11 forwarding: failure");
        assert!(is_x11_rejection(&worded));
        assert!(!is_x11_rejection(&std::io::Error::other("channel closed")));
    }

    #[test]
    fn status_and_settings_keep_the_api_json_shape() {
        let status = X11Status {
            source: X11Source::Display,
            display: Some(":0".into()),
            enabled: true,
            forward_by_default: false,
            clipboard: false,
            defaults: X11Defaults {
                enabled: true,
                forward_by_default: false,
            },
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({
                "source": "display",
                "display": ":0",
                "enabled": true,
                "forwardByDefault": false,
                "clipboard": false,
                "defaults": { "enabled": true, "forwardByDefault": false },
            })
        );
        let none = X11Status {
            source: X11Source::None,
            display: None,
            ..status
        };
        assert!(
            serde_json::to_value(&none)
                .unwrap()
                .get("display")
                .is_none()
        );

        let update: X11SettingsUpdate =
            serde_json::from_str(r#"{"enabled":true,"clipboard":true}"#).unwrap();
        assert_eq!(
            update,
            X11SettingsUpdate {
                enabled: Some(true),
                forward_by_default: None,
                clipboard: true,
            }
        );
        assert!(serde_json::from_str::<X11SettingsUpdate>(r#"{"enabled":true}"#).is_err());
        assert_eq!(
            serde_json::to_value(X11SessionState::NoServer).unwrap(),
            serde_json::json!("no-server")
        );
    }

    #[test]
    fn missing_server_messages_name_the_platform_fix() {
        let message = |platform| local(platform, &[]).missing_server_message();
        assert!(message(Platform::MacOs).contains("Install XQuartz"));
        assert!(message(Platform::Windows).contains("no bundled X server"));
        assert!(message(Platform::Linux).contains("graphical session"));
    }
}
