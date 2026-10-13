//! The X server bundled with the Windows app (VcXsrv).
//!
//! [`BundledXServer`] exists only on Windows (and in this crate's tests, which drive it through
//! its seams). The cookie and probe helpers are plain tokio and compile everywhere.

use std::time::Duration;

use rand::RngCore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::proxy::rewrite_x11_setup;
use super::xauthority::X11Auth;

#[cfg(any(windows, test))]
pub use bundled::*;

pub const VCXSRV_EXECUTABLE: &str = "vcxsrv.exe";

/// A random MIT-MAGIC-COOKIE-1 that VcXsrv can actually read back. Its LoadAuthorization opens
/// the Xauthority file with fopen(..., "r"), which on Windows is text mode: a 0x1A byte ends the
/// file early and "\r\n" shrinks to "\n". A cookie with either reads as zero entries, and the X
/// server then enables local access, letting any local client in without a cookie. Drawing from
/// the other 254 byte values keeps about 128 bits of entropy.
pub fn text_safe_cookie(length: usize) -> Vec<u8> {
    let mut cookie = Vec::with_capacity(length);
    let mut random = vec![0u8; length];
    while cookie.len() < length {
        rand::rngs::OsRng.fill_bytes(&mut random);
        for &byte in &random {
            if byte == 0x1a || byte == 0x0d {
                continue;
            }
            cookie.push(byte);
            if cookie.len() == length {
                break;
            }
        }
    }
    cookie
}

/// The result of [`probe_x11_server`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XServerProbe {
    /// An X server accepted the cookie.
    Ours,
    /// An X server refused the cookie.
    Foreign,
    /// Nothing answered.
    Down,
}

/// Little-endian X11 protocol 11.0 connection setup header.
const SETUP_HEADER: [u8; 12] = [0x6c, 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PORT_CHECK_TIMEOUT: Duration = Duration::from_secs(1);

/// Attempt an X11 connection setup on 127.0.0.1:port with `auth`.
pub async fn probe_x11_server(port: u16, auth: &X11Auth) -> XServerProbe {
    let attempt = async {
        let mut socket =
            tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(("127.0.0.1", port)))
                .await
                .ok()?
                .ok()?;
        let _ = socket.set_nodelay(true);
        let setup = rewrite_x11_setup(&SETUP_HEADER, true, Some(auth));
        tokio::time::timeout(PROBE_TIMEOUT, socket.write_all(&setup))
            .await
            .ok()?
            .ok()?;
        let mut status = [0u8; 1];
        let read = tokio::time::timeout(PROBE_TIMEOUT, socket.read(&mut status))
            .await
            .ok()?
            .ok()?;
        // Status byte: 1 Success; 0 Failed and 2 Authenticate mean the cookie was refused.
        (read == 1).then_some(if status[0] == 1 {
            XServerProbe::Ours
        } else {
            XServerProbe::Foreign
        })
    };
    attempt.await.unwrap_or(XServerProbe::Down)
}

/// True when something accepts connections on 127.0.0.1:port.
pub async fn loopback_port_in_use(port: u16) -> bool {
    matches!(
        tokio::time::timeout(PORT_CHECK_TIMEOUT, TcpStream::connect(("127.0.0.1", port))).await,
        Ok(Ok(_))
    )
}

#[cfg(any(windows, test))]
mod bundled {
    use std::collections::HashSet;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, Weak};
    use std::time::{Duration, Instant};

    use tokio::sync::{oneshot, watch};

    use super::super::display::X11_TCP_PORT_BASE;
    use super::super::xauthority::{FAMILY_WILD, X11Auth, XauthEntry, serialize_xauthority};
    use super::super::{BoxFuture, X11Error};
    use super::{VCXSRV_EXECUTABLE, XServerProbe, text_safe_cookie};

    /// Display numbers tried in order; :0 is left to any X server the user runs.
    const FIRST_DISPLAY: u16 = 10;
    const LAST_DISPLAY: u16 = 99;
    const START_TIMEOUT: Duration = Duration::from_secs(15);
    const READY_POLL: Duration = Duration::from_millis(100);
    const MAX_START_ATTEMPTS: usize = 3;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RunningXServer {
        pub display: u16,
        pub port: u16,
        pub auth: X11Auth,
        /// Whether VcXsrv bridges X selections to the Windows clipboard.
        pub clipboard: bool,
    }

    /// Display numbers held by the bundled servers of one process, shared between them.
    pub type ClaimedDisplays = Arc<Mutex<HashSet<u16>>>;

    /// A started X server process as [`BundledXServer`] supervises it: a way to kill it and the
    /// news of its exit.
    pub struct XServerProcess {
        kill: Box<dyn FnOnce() + Send>,
        exited: oneshot::Receiver<String>,
    }

    impl XServerProcess {
        /// `kill` asks the process to stop; `exited` receives a description of how it ended
        /// ("exited with code 1"). A dropped sender counts as an exit.
        pub fn new(
            kill: impl FnOnce() + Send + 'static,
            exited: oneshot::Receiver<String>,
        ) -> Self {
            Self {
                kill: Box::new(kill),
                exited,
            }
        }
    }

    /// Starts the X server: `(executable, args, working directory)`.
    pub type SpawnServer =
        Arc<dyn Fn(&Path, &[String], &Path) -> io::Result<XServerProcess> + Send + Sync>;
    /// Whether something already listens on a loopback TCP port.
    pub type PortInUse = Arc<dyn Fn(u16) -> BoxFuture<bool> + Send + Sync>;
    /// An authenticated connection setup against a loopback port.
    pub type ProbeServer = Arc<dyn Fn(u16, X11Auth) -> BoxFuture<XServerProbe> + Send + Sync>;

    /// Test seams; production spawns VcXsrv and probes loopback TCP.
    #[derive(Clone, Default)]
    pub struct BundledXServerOptions {
        pub spawn_server: Option<SpawnServer>,
        pub port_in_use: Option<PortInUse>,
        pub probe_server: Option<ProbeServer>,
        /// Where the private Xauthority file goes; default the system temp directory.
        pub auth_directory: Option<PathBuf>,
        /// Display numbers held by sibling servers in this process, shared between them.
        pub claimed_displays: Option<ClaimedDisplays>,
    }

    impl std::fmt::Debug for BundledXServerOptions {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("BundledXServerOptions")
                .field("spawn_server", &self.spawn_server.is_some())
                .field("port_in_use", &self.port_in_use.is_some())
                .field("probe_server", &self.probe_server.is_some())
                .field("auth_directory", &self.auth_directory)
                .finish_non_exhaustive()
        }
    }

    type StartResult = Option<Result<RunningXServer, X11Error>>;

    struct ChildSlot {
        id: u64,
        kill: Option<Box<dyn FnOnce() + Send>>,
    }

    impl ChildSlot {
        fn kill(&mut self) {
            if let Some(kill) = self.kill.take() {
                kill();
            }
        }
    }

    #[derive(Default)]
    struct State {
        /// The start in progress (or finished, while it stays current), shared by its callers.
        starting: Option<(u64, watch::Receiver<StartResult>)>,
        running: Option<RunningXServer>,
        child: Option<ChildSlot>,
        auth_file: Option<PathBuf>,
        /// Display number this server holds in the shared claim set.
        claimed: Option<u16>,
        closed: bool,
        next_id: u64,
    }

    struct Inner {
        directory: PathBuf,
        options: BundledXServerOptions,
        claimed_displays: ClaimedDisplays,
        state: Mutex<State>,
    }

    /// One VcXsrv display from the build shipped with the Windows app, started on first use.
    /// LocalX11 runs one per SSH transport.
    ///
    /// It gets its own display number and a fresh MIT-MAGIC-COOKIE-1 in a private auth file, so
    /// only Muxus (and nothing else on the machine) can open windows on it, and it stops with its
    /// transport or with Muxus.
    ///
    /// Clipboard integration is off unless asked for: with it, any forwarding server could read
    /// and replace the Windows clipboard.
    ///
    /// Cheap to clone; clones share the server. Dropping the last clone kills the process but
    /// leaves the auth file; call [`close`](Self::close) to clean up.
    #[derive(Clone)]
    pub struct BundledXServer {
        inner: Arc<Inner>,
    }

    impl BundledXServer {
        pub fn new(directory: impl Into<PathBuf>, options: BundledXServerOptions) -> Self {
            let claimed_displays = options.claimed_displays.clone().unwrap_or_default();
            Self {
                inner: Arc::new(Inner {
                    directory: directory.into(),
                    options,
                    claimed_displays,
                    state: Mutex::new(State::default()),
                }),
            }
        }

        pub fn executable(&self) -> PathBuf {
            self.inner.executable()
        }

        /// Start the server if it is not running; concurrent callers share one start. A running
        /// server with a different clipboard mode is restarted only when `idle` (no forwarded
        /// connections would lose their windows).
        pub async fn ensure_running(
            &self,
            clipboard: bool,
            idle: bool,
        ) -> Result<RunningXServer, X11Error> {
            let mut outcome = {
                let mut state = self.inner.lock();
                if state.closed {
                    return Err(X11Error::ShuttingDown);
                }
                if state
                    .running
                    .as_ref()
                    .is_some_and(|running| running.clipboard != clipboard)
                    && idle
                {
                    tracing::info!(
                        clipboard,
                        "restarting the bundled X server for the clipboard setting"
                    );
                    stop(&self.inner.claimed_displays, &mut state);
                }
                match &state.starting {
                    Some((_, outcome)) => outcome.clone(),
                    None => self.begin_start(&mut state, clipboard),
                }
            };
            match outcome.wait_for(Option::is_some).await {
                Ok(result) => result.clone().unwrap_or(Err(X11Error::ShuttingDown)),
                Err(_) => Err(X11Error::StartFailed("the start was abandoned".into())),
            }
        }

        /// Run a start on its own task, so a caller that goes away cannot cut it short.
        fn begin_start(&self, state: &mut State, clipboard: bool) -> watch::Receiver<StartResult> {
            state.next_id += 1;
            let generation = state.next_id;
            let (publish, outcome) = watch::channel(None);
            state.starting = Some((generation, outcome.clone()));
            let inner = self.inner.clone();
            tokio::spawn(async move {
                let mut result = inner.start(clipboard).await;
                {
                    let mut state = inner.lock();
                    let current = state
                        .starting
                        .as_ref()
                        .is_some_and(|(id, _)| *id == generation);
                    match &result {
                        Ok(running) if current => state.running = Some(running.clone()),
                        Err(_) if current => state.starting = None,
                        Ok(_) if state.closed => result = Err(X11Error::ShuttingDown),
                        _ => {}
                    }
                }
                let _ = publish.send(Some(result));
            });
            outcome
        }

        /// Stop the server for good and remove its auth file.
        pub fn close(&self) {
            let auth_file = {
                let mut state = self.inner.lock();
                state.closed = true;
                stop(&self.inner.claimed_displays, &mut state);
                state.auth_file.take()
            };
            if let Some(file) = auth_file {
                let _ = std::fs::remove_file(file);
            }
        }
    }

    fn stop(claimed: &ClaimedDisplays, state: &mut State) {
        state.starting = None;
        state.running = None;
        if let Some(mut child) = state.child.take() {
            child.kill();
        }
        unclaim(claimed, state);
    }

    fn unclaim(claimed: &ClaimedDisplays, state: &mut State) {
        if let Some(display) = state.claimed.take() {
            claimed.lock().unwrap().remove(&display);
        }
    }

    impl Inner {
        fn lock(&self) -> std::sync::MutexGuard<'_, State> {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        fn executable(&self) -> PathBuf {
            self.directory.join(VCXSRV_EXECUTABLE)
        }

        async fn start(self: &Arc<Self>, clipboard: bool) -> Result<RunningXServer, X11Error> {
            let mut first = FIRST_DISPLAY;
            for _ in 0..MAX_START_ATTEMPTS {
                let mut display = None;
                for candidate in first..=LAST_DISPLAY {
                    let taken = self.claimed_displays.lock().unwrap().contains(&candidate);
                    if taken || self.port_in_use(X11_TCP_PORT_BASE + candidate).await {
                        continue;
                    }
                    // A sibling may have claimed it while the port was probed.
                    if !self.claimed_displays.lock().unwrap().insert(candidate) {
                        continue;
                    }
                    self.lock().claimed = Some(candidate);
                    display = Some(candidate);
                    break;
                }
                let Some(display) = display else {
                    return Err(X11Error::NoFreeDisplay);
                };
                match self.launch(display, clipboard).await {
                    Ok(Some(running)) => return Ok(running),
                    Ok(None) => {
                        // Another X server took the display between the scan and VcXsrv's bind.
                        unclaim(&self.claimed_displays, &mut self.lock());
                        first = display + 1;
                    }
                    Err(err) => {
                        unclaim(&self.claimed_displays, &mut self.lock());
                        return Err(err);
                    }
                }
            }
            Err(X11Error::DisplaysTaken)
        }

        async fn port_in_use(&self, port: u16) -> bool {
            match &self.options.port_in_use {
                Some(check) => check(port).await,
                None => super::loopback_port_in_use(port).await,
            }
        }

        async fn probe(&self, port: u16, auth: &X11Auth) -> XServerProbe {
            match &self.options.probe_server {
                Some(probe) => probe(port, auth.clone()).await,
                None => super::probe_x11_server(port, auth).await,
            }
        }

        /// Start VcXsrv on `display_number`; `None` when a different server answers there.
        async fn launch(
            self: &Arc<Self>,
            display_number: u16,
            clipboard: bool,
        ) -> Result<Option<RunningXServer>, X11Error> {
            let auth = X11Auth::magic_cookie(text_safe_cookie(16));
            let auth_directory = self
                .options
                .auth_directory
                .clone()
                .unwrap_or_else(std::env::temp_dir);
            let auth_file = auth_directory.join(format!(
                "muxus-x11-{}-{display_number}.Xauthority",
                std::process::id()
            ));
            {
                let mut state = self.lock();
                if state.closed {
                    return Err(X11Error::ShuttingDown);
                }
                if let Some(previous) = state.auth_file.take()
                    && previous != auth_file
                {
                    let _ = std::fs::remove_file(previous);
                }
            }
            let record = serialize_xauthority(&[XauthEntry {
                family: FAMILY_WILD,
                address: Vec::new(),
                number: display_number.to_string(),
                name: auth.name.clone(),
                data: auth.data.clone(),
            }])?;
            write_private_file(&auth_file, &record)?;
            self.lock().auth_file = Some(auth_file.clone());

            let args: Vec<String> = vec![
                format!(":{display_number}"),
                "-multiwindow".into(),
                if clipboard {
                    "-clipboard"
                } else {
                    "-noclipboard"
                }
                .into(),
                "-wgl".into(),
                "-auth".into(),
                auth_file.to_string_lossy().into_owned(),
                // A race for the display number must not pop up a modal error box.
                "-silent-dup-error".into(),
                "-notrayicon".into(),
            ];
            let spawned = match &self.options.spawn_server {
                Some(spawn) => spawn(&self.executable(), &args, &self.directory),
                None => spawn_vcxsrv(&self.executable(), &args, &self.directory),
            };
            let process = spawned.map_err(|err| X11Error::StartFailed(err.to_string()))?;
            let XServerProcess { kill, exited } = process;
            let ready = Arc::new(AtomicBool::new(false));
            let exit_reason: Arc<Mutex<Option<String>>> = Arc::default();
            let id = {
                let mut state = self.lock();
                if state.closed {
                    kill();
                    return Err(X11Error::ShuttingDown);
                }
                state.next_id += 1;
                let id = state.next_id;
                state.child = Some(ChildSlot {
                    id,
                    kill: Some(kill),
                });
                id
            };
            self.watch_exit(
                id,
                display_number,
                exited,
                ready.clone(),
                exit_reason.clone(),
            );

            // Readiness is a connection setup with this server's cookie: only the VcXsrv just
            // started can accept it, whatever else listens on the port.
            let port = X11_TCP_PORT_BASE + display_number;
            let deadline = Instant::now() + START_TIMEOUT;
            loop {
                match self.probe(port, &auth).await {
                    XServerProbe::Ours => break,
                    XServerProbe::Foreign => {
                        let mut state = self.lock();
                        if let Some(mut child) = state.child.take_if(|child| child.id == id) {
                            child.kill();
                        }
                        tracing::warn!(
                            display = u32::from(display_number),
                            "another X server took the display; trying the next one"
                        );
                        return Ok(None);
                    }
                    XServerProbe::Down => {}
                }
                // close() already killed the child.
                let closed = self.lock().closed;
                if closed {
                    return Err(X11Error::ShuttingDown);
                }
                if let Some(reason) = exit_reason.lock().unwrap().clone() {
                    return Err(X11Error::StartFailed(reason));
                }
                if Instant::now() > deadline {
                    if let Some(child) = self.lock().child.as_mut().filter(|child| child.id == id) {
                        child.kill();
                    }
                    return Err(X11Error::StartTimeout);
                }
                tokio::time::sleep(READY_POLL).await;
            }
            ready.store(true, Ordering::SeqCst);
            tracing::info!(
                display = u32::from(display_number),
                clipboard,
                "bundled X server started"
            );
            Ok(Some(RunningXServer {
                display: display_number,
                port,
                auth,
                clipboard,
            }))
        }

        /// Follow a child's exit: a server that stops after it was ready lets a later X11 channel
        /// start a fresh one; a start in progress reports the failure itself.
        fn watch_exit(
            self: &Arc<Self>,
            id: u64,
            display_number: u16,
            exited: oneshot::Receiver<String>,
            ready: Arc<AtomicBool>,
            exit_reason: Arc<Mutex<Option<String>>>,
        ) {
            let inner: Weak<Self> = Arc::downgrade(self);
            tokio::spawn(async move {
                let reason = exited.await.unwrap_or_else(|_| "exited".to_string());
                exit_reason.lock().unwrap().get_or_insert(reason.clone());
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                let mut state = inner.lock();
                if state.child.as_ref().is_none_or(|child| child.id != id) {
                    return;
                }
                state.child = None;
                if ready.load(Ordering::SeqCst) {
                    state.starting = None;
                    state.running = None;
                    unclaim(&inner.claimed_displays, &mut state);
                }
                if !state.closed {
                    tracing::warn!(
                        display = u32::from(display_number),
                        reason,
                        "bundled X server stopped"
                    );
                }
            });
        }
    }

    /// The auth file holds the server's only credential: readable by this user alone.
    fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)?.write_all(contents)
    }

    /// Spawn VcXsrv hidden, with no console window, and follow it until it exits or is killed.
    fn spawn_vcxsrv(executable: &Path, args: &[String], cwd: &Path) -> io::Result<XServerProcess> {
        let mut command = tokio::process::Command::new(executable);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn()?;
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let (exit_tx, exit_rx) = oneshot::channel();
        tokio::spawn(async move {
            // A dropped kill handle counts as a kill: the server never outlives its owner.
            let status = tokio::select! {
                status = child.wait() => status,
                _ = kill_rx => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            let reason = match status {
                Ok(status) => match status.code() {
                    Some(code) => format!("exited with code {code}"),
                    None => format!("exited with {status}"),
                },
                Err(err) => err.to_string(),
            };
            let _ = exit_tx.send(reason);
        });
        Ok(XServerProcess::new(
            move || {
                let _ = kill_tx.send(());
            },
            exit_rx,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x11::X11Error;
    use crate::x11::proxy::{X11SetupParse, parse_x11_setup};
    use crate::x11::test_support::FakeChildren;
    use crate::x11::xauthority::{FAMILY_WILD, MIT_MAGIC_COOKIE, XauthEntry, parse_xauthority};
    use std::collections::HashSet;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    #[test]
    fn cookies_never_contain_bytes_vcxsrv_mangles_when_it_reads_the_xauthority_file_in_text_mode() {
        let mut seen = HashSet::new();
        for _ in 0..4000 {
            let cookie = text_safe_cookie(16);
            assert_eq!(cookie.len(), 16);
            // 0x1A ends a text-mode read and \r\n shrinks; either leaves VcXsrv with no cookie,
            // which makes it accept local clients without one.
            assert!(!cookie.contains(&0x1a));
            assert!(!cookie.contains(&0x0d));
            seen.extend(cookie);
        }
        assert_eq!(seen.len(), 254);
    }

    /// `busy` ports are taken before the scan; `raced` ports are grabbed by some other X server
    /// between the scan and VcXsrv binding them.
    fn fixture(dir: &Path, busy: &[u16], raced: &[u16]) -> (BundledXServer, FakeChildren) {
        std::fs::write(dir.join(VCXSRV_EXECUTABLE), b"").unwrap();
        let children = FakeChildren::new(busy, raced);
        let server = BundledXServer::new(dir, children.options(dir));
        (server, children)
    }

    #[tokio::test]
    async fn starts_one_server_on_the_first_free_display_with_a_private_cookie() {
        let dir = tempfile::tempdir().unwrap();
        let (server, children) = fixture(dir.path(), &[6010, 6011], &[]);
        let (first, second) = tokio::join!(
            server.ensure_running(false, true),
            server.ensure_running(false, true)
        );
        let (first, second) = (first.unwrap(), second.unwrap());
        assert_eq!(first, second);
        assert_eq!(children.count(), 1);
        assert_eq!(first.display, 12);
        assert_eq!(first.port, 6012);
        let spawn = children.get(0);
        assert_eq!(spawn.executable, dir.path().join(VCXSRV_EXECUTABLE));
        for arg in [":12", "-multiwindow", "-noclipboard", "-silent-dup-error"] {
            assert!(spawn.args.iter().any(|a| a == arg), "missing {arg}");
        }
        assert!(!first.clipboard);
        let auth_index = spawn.args.iter().position(|a| a == "-auth").unwrap();
        let auth_file = std::path::PathBuf::from(&spawn.args[auth_index + 1]);
        assert_eq!(
            parse_xauthority(&std::fs::read(&auth_file).unwrap()),
            vec![XauthEntry {
                family: FAMILY_WILD,
                address: Vec::new(),
                number: "12".into(),
                name: MIT_MAGIC_COOKIE.into(),
                data: first.auth.data.clone(),
            }]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&auth_file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        server.close();
        assert!(spawn.killed());
        assert!(!auth_file.exists());
        assert!(matches!(
            server.ensure_running(false, true).await,
            Err(X11Error::ShuttingDown)
        ));
    }

    #[tokio::test]
    async fn starts_a_fresh_server_after_the_previous_one_exits() {
        let dir = tempfile::tempdir().unwrap();
        let (server, children) = fixture(dir.path(), &[], &[]);
        let first = server.ensure_running(false, true).await.unwrap();
        children.get(0).exit("exited with code 0");
        children.settle().await;
        let second = server.ensure_running(false, true).await.unwrap();
        assert_eq!(children.count(), 2);
        assert_ne!(second.auth.data, first.auth.data);
        server.close();
    }

    #[tokio::test]
    async fn switches_clipboard_sharing_only_while_no_forwarded_windows_are_open() {
        let dir = tempfile::tempdir().unwrap();
        let (server, children) = fixture(dir.path(), &[], &[]);
        let plain = server.ensure_running(false, true).await.unwrap();

        // Windows are open: keep the running server rather than closing them.
        assert_eq!(server.ensure_running(true, false).await.unwrap(), plain);
        assert_eq!(children.count(), 1);

        let shared = server.ensure_running(true, true).await.unwrap();
        assert_eq!(children.count(), 2);
        assert!(children.get(0).killed());
        assert!(children.get(1).args.iter().any(|a| a == "-clipboard"));
        assert!(shared.clipboard);
        assert_eq!(server.ensure_running(true, true).await.unwrap(), shared);
        server.close();
    }

    #[tokio::test]
    async fn probes_readiness_with_an_authenticated_x11_connection_setup() {
        let cookie = X11Auth::magic_cookie(vec![0x11; 16]);
        // A stand-in X server that answers Success (1) only for this cookie.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = cookie.data.clone();
        let x_server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 1024];
                let read = socket.read(&mut buf).await.unwrap();
                let ok = matches!(
                    parse_x11_setup(&buf[..read]),
                    X11SetupParse::Complete { auth_data, .. } if auth_data == expected
                );
                socket.write_all(&[u8::from(ok)]).await.unwrap();
            }
        });
        assert_eq!(probe_x11_server(port, &cookie).await, XServerProbe::Ours);
        let wrong = X11Auth::magic_cookie(vec![0; 16]);
        assert_eq!(probe_x11_server(port, &wrong).await, XServerProbe::Foreign);
        x_server.abort();
        let _ = x_server.await;
        assert_eq!(probe_x11_server(port, &cookie).await, XServerProbe::Down);
    }

    #[tokio::test]
    async fn checks_loopback_ports() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(loopback_port_in_use(port).await);
        drop(listener);
        assert!(!loopback_port_in_use(port).await);
    }

    #[tokio::test]
    async fn moves_to_the_next_display_when_another_x_server_wins_the_race_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let (server, children) = fixture(dir.path(), &[], &[6010]);
        let running = server.ensure_running(false, true).await.unwrap();
        assert_eq!(running.display, 11);
        assert_eq!(children.displays(), vec![":10", ":11"]);
        assert!(children.get(0).killed());
        // The loser's exit must not reset the start that is still in progress.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(server.ensure_running(false, true).await.unwrap(), running);
        assert_eq!(children.count(), 2);
        server.close();
    }

    #[tokio::test]
    async fn reports_a_server_that_exits_during_startup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(VCXSRV_EXECUTABLE), b"").unwrap();
        let options = BundledXServerOptions {
            auth_directory: Some(dir.path().to_path_buf()),
            port_in_use: Some(Arc::new(|_| Box::pin(async { false }))),
            probe_server: Some(Arc::new(|_, _| Box::pin(async { XServerProbe::Down }))),
            spawn_server: Some(Arc::new(|_, _, _| {
                let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    let _ = exit_tx.send("exited with code 1".to_string());
                });
                Ok(XServerProcess::new(|| {}, exit_rx))
            })),
            claimed_displays: None,
        };
        let server = BundledXServer::new(dir.path(), options);
        let err = server.ensure_running(false, true).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "the bundled X server failed to start (exited with code 1)"
        );
        server.close();
    }

    #[tokio::test]
    async fn reports_a_server_that_cannot_be_spawned_and_frees_its_display() {
        let dir = tempfile::tempdir().unwrap();
        let claimed: ClaimedDisplays = Arc::new(Mutex::new(HashSet::new()));
        let options = BundledXServerOptions {
            auth_directory: Some(dir.path().to_path_buf()),
            port_in_use: Some(Arc::new(|_| Box::pin(async { false }))),
            spawn_server: Some(Arc::new(|_, _, _| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such file",
                ))
            })),
            claimed_displays: Some(claimed.clone()),
            ..BundledXServerOptions::default()
        };
        let server = BundledXServer::new(dir.path(), options);
        let err = server.ensure_running(false, true).await.unwrap_err();
        assert!(matches!(err, X11Error::StartFailed(_)));
        assert!(claimed.lock().unwrap().is_empty());
        server.close();
    }
}
