//! Where the window gets its hosts and sessions from.
//!
//! The views only talk to a [`DataSource`]: the host catalog comes from
//! [`DataSource::load_catalog`] and every terminal session from
//! [`DataSource::open_terminal`], which takes the protocol's `connect`
//! message and hands back a [`SessionLink`]. [`BackendSource`] is the app's:
//! `muxus_session::Backend` routes and session handles. [`LocalSource`] opens
//! local shells on its own (no hosts, no remote sessions);
//! [`crate::model::demo_shell::DemoSource`] serves the documentation's demo
//! catalog with local stand-ins for its hosts.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use muxus_core::protocol::{LocalProfile, TerminalClientMessage, TerminalProfile};
use muxus_core::stream::TerminalSize;
use muxus_local::{LocalEnvironment, spawn_local_pty};

use muxus_session::{Backend, SessionEvent, SessionHandle};
use muxus_store::prefs::PrefsState;

use crate::model::Catalog;
use crate::session::{BoxFuture, LinkEvent, LinkInput, LinkReader, LinkWriter, SessionLink};

/// The window's backend: hosts in, sessions out.
pub trait DataSource: Send + Sync + 'static {
    /// Both host sources: the ssh_config entries and the hosts Muxus saved.
    fn load_catalog(&self) -> BoxFuture<'static, Result<Catalog, String>>;

    /// Start a terminal session. `connect` is the protocol's `connect`
    /// message (profile, size, title, working directory to restore).
    fn open_terminal(&self, connect: TerminalClientMessage) -> SessionLink;

    /// Whether sessions of this kind can be opened at all (the empty-pane
    /// cards and menus grey out the rest).
    fn supports(&self, profile: &TerminalProfile) -> bool;

    /// The stored preferences, when this source keeps any.
    fn load_prefs(&self) -> BoxFuture<'static, Result<Option<PrefsState>, String>> {
        Box::pin(async { Ok(None) })
    }

    /// Store the preferences (a no-op for sources without storage).
    fn save_prefs(&self, _prefs: PrefsState) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    /// The backend behind this source, for the routes the window calls
    /// directly (workspaces, host statistics, diagnostics, metadata).
    fn backend(&self) -> Option<Backend> {
        None
    }
}

/// Everything from the backend: hosts, sessions of every kind, preferences.
#[derive(Clone)]
pub struct BackendSource {
    backend: Backend,
}

impl BackendSource {
    pub fn new(backend: Backend) -> Self {
        Self { backend }
    }
}

impl DataSource for BackendSource {
    fn load_catalog(&self) -> BoxFuture<'static, Result<Catalog, String>> {
        let backend = self.backend.clone();
        Box::pin(async move {
            let ssh = backend.ssh_config().await.map_err(|err| err.to_string())?;
            let saved = backend.saved_hosts().await.map_err(|err| err.to_string())?;
            Ok(Catalog {
                ssh,
                profiles: saved.profiles,
            })
        })
    }

    fn open_terminal(&self, connect: TerminalClientMessage) -> SessionLink {
        session_link(self.backend.open_session(connect))
    }

    fn supports(&self, _profile: &TerminalProfile) -> bool {
        true
    }

    fn load_prefs(&self) -> BoxFuture<'static, Result<Option<PrefsState>, String>> {
        let backend = self.backend.clone();
        Box::pin(async move {
            backend
                .prefs()
                .await
                .map(Some)
                .map_err(|err| err.to_string())
        })
    }

    fn save_prefs(&self, prefs: PrefsState) -> BoxFuture<'static, Result<(), String>> {
        let backend = self.backend.clone();
        Box::pin(async move {
            backend
                .set_prefs(prefs)
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn backend(&self) -> Option<Backend> {
        Some(self.backend.clone())
    }
}

/// A backend session as a link: its events and inputs map one to one.
pub fn session_link(handle: SessionHandle) -> SessionLink {
    let handle = Arc::new(handle);
    SessionLink {
        reader: Box::new(HandleReader(handle.clone())),
        writer: Arc::new(HandleWriter(handle)),
    }
}

struct HandleReader(Arc<SessionHandle>);

fn link_event(event: SessionEvent) -> LinkEvent {
    match event {
        SessionEvent::Data(data) => LinkEvent::Data(data),
        SessionEvent::Message(message) => LinkEvent::message(message),
        SessionEvent::Closed { reason } => LinkEvent::Closed { reason },
    }
}

impl LinkReader for HandleReader {
    fn next_event(&mut self) -> BoxFuture<'_, Option<LinkEvent>> {
        Box::pin(async move { self.0.next_event().await.map(link_event) })
    }

    fn try_next_event(&mut self) -> Option<LinkEvent> {
        self.0.try_next_event().map(link_event)
    }
}

struct HandleWriter(Arc<SessionHandle>);

impl LinkWriter for HandleWriter {
    fn send(&self, input: LinkInput) -> BoxFuture<'static, bool> {
        // A session handle's input never blocks.
        match input {
            LinkInput::Data(data) => self.0.send_data(data),
            LinkInput::Message(message) => self.0.send(*message),
            LinkInput::Close => self.0.close(),
        }
        Box::pin(async { true })
    }
}

/// The size a `connect` message asks for.
pub fn connect_size(connect: &TerminalClientMessage) -> TerminalSize {
    match connect {
        TerminalClientMessage::Connect { cols, rows, .. }
        | TerminalClientMessage::Attach { cols, rows, .. } => TerminalSize {
            cols: (*cols).clamp(1, u32::from(u16::MAX)) as u16,
            rows: (*rows).clamp(1, u32::from(u16::MAX)) as u16,
            pixel_width: 0,
            pixel_height: 0,
        },
        _ => TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        },
    }
}

/// The environment local shells start from: this process's, with the login
/// shell's `PATH` once it is known (`importLoginShellEnvironment`). GUI
/// launches do not inherit shell startup files, so the login shell is asked
/// once, in the background, when the app starts.
#[derive(Clone, Default)]
pub struct LoginEnvironment {
    state: Arc<LoginState>,
}

#[derive(Default)]
struct LoginState {
    resolved: OnceLock<HashMap<String, String>>,
    pending: Mutex<Option<std::thread::JoinHandle<HashMap<String, String>>>>,
}

impl LoginEnvironment {
    /// Start reading the login shell's environment in the background.
    pub fn import_in_background() -> Self {
        let handle = std::thread::Builder::new()
            .name("muxus-login-env".into())
            .spawn(|| {
                let platform = muxus_local::Platform::current();
                let mut vars: HashMap<String, String> = std::env::vars().collect();
                muxus_local::login_env::import_login_shell_environment(
                    platform,
                    &mut vars,
                    || muxus_local::login_env::shell_env(None),
                    None,
                );
                vars
            })
            .ok();
        Self {
            state: Arc::new(LoginState {
                resolved: OnceLock::new(),
                pending: Mutex::new(handle),
            }),
        }
    }

    /// This process's environment as it is, without asking a login shell.
    pub fn current() -> Self {
        let env = Self::default();
        let _ = env.state.resolved.set(std::env::vars().collect());
        env
    }

    /// The variables local shells inherit; waits for the login shell the
    /// first time if it has not answered yet (it is capped at 10 s).
    pub fn vars(&self) -> HashMap<String, String> {
        if let Some(vars) = self.state.resolved.get() {
            return vars.clone();
        }
        let handle = self
            .state
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let vars = handle
            .and_then(|handle| handle.join().ok())
            .unwrap_or_else(|| std::env::vars().collect());
        self.state.resolved.get_or_init(|| vars).clone()
    }
}

/// Open a local shell for `profile` as a session.
pub fn open_local_shell(
    profile: &LocalProfile,
    size: TerminalSize,
    environment: &LocalEnvironment,
) -> SessionLink {
    match spawn_local_pty(profile, size, environment) {
        Ok(pty) => {
            let conn_id = format!("local-{}", pty.process_id.unwrap_or_else(std::process::id));
            SessionLink::from_transport(pty.channels, conn_id)
        }
        Err(err) => SessionLink::failed(format!("Could not start the local shell: {err}")),
    }
}

/// Local shells only: what the app can do before the backend is wired in.
/// The catalog is empty and remote sessions report that they need the backend.
#[derive(Clone)]
pub struct LocalSource {
    login: LoginEnvironment,
}

impl LocalSource {
    pub fn new(login: LoginEnvironment) -> Self {
        Self { login }
    }

    fn environment(&self) -> LocalEnvironment {
        LocalEnvironment {
            vars: self.login.vars(),
            ..LocalEnvironment::current()
        }
    }
}

impl DataSource for LocalSource {
    fn load_catalog(&self) -> BoxFuture<'static, Result<Catalog, String>> {
        Box::pin(async { Ok(Catalog::default()) })
    }

    fn open_terminal(&self, connect: TerminalClientMessage) -> SessionLink {
        let size = connect_size(&connect);
        match connect {
            TerminalClientMessage::Connect {
                profile: TerminalProfile::Local(profile),
                ..
            } => open_local_shell(&profile, size, &self.environment()),
            TerminalClientMessage::Connect { .. } => SessionLink::failed(
                "Remote sessions are not available in this build yet: the backend is not connected.",
            ),
            _ => SessionLink::failed("This session cannot be reattached."),
        }
    }

    fn supports(&self, profile: &TerminalProfile) -> bool {
        matches!(profile, TerminalProfile::Local(_))
    }
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use muxus_core::protocol::{ExitReason, TerminalServerMessage};

    use super::*;
    use crate::session::LinkEvent;

    fn connect(profile: TerminalProfile) -> TerminalClientMessage {
        TerminalClientMessage::Connect {
            profile,
            fresh_transport: None,
            title: None,
            cwd: None,
            cols: 100,
            rows: 30,
        }
    }

    #[test]
    fn reads_the_size_a_connect_asks_for() {
        let size = connect_size(&connect(TerminalProfile::Local(LocalProfile::default())));
        assert_eq!((size.cols, size.rows), (100, 30));
    }

    #[test]
    fn refuses_remote_sessions_without_the_backend() {
        let source = LocalSource::new(LoginEnvironment::current());
        let mut link = source.open_terminal(connect(TerminalProfile::Ssh(
            muxus_core::protocol::SshProfile::new("router"),
        )));
        let event = block_on(link.reader.next_event());
        assert!(matches!(
            event,
            Some(LinkEvent::Message(message))
                if matches!(*message, TerminalServerMessage::Exit {
                    reason: ExitReason::Failed,
                    ..
                })
        ));
        assert!(!source.supports(&TerminalProfile::Ssh(
            muxus_core::protocol::SshProfile::new("router")
        )));
    }

    /// A real shell on a pty, in a scratch home without ZDOTDIR.
    #[cfg(unix)]
    #[test]
    fn runs_a_local_shell_and_reports_its_exit() {
        let home = tempfile::tempdir().unwrap();
        let mut vars: HashMap<String, String> = std::env::vars()
            .filter(|(key, _)| key != "ZDOTDIR")
            .collect();
        vars.insert("HOME".into(), home.path().display().to_string());
        let environment = LocalEnvironment {
            vars,
            integration_root: None,
            platform: muxus_local::Platform::current(),
        };
        let profile = LocalProfile {
            shell: Some("/bin/sh".into()),
            args: Some(vec!["-c".into(), "printf ready; exit 4".into()]),
            ..Default::default()
        };
        let mut link = open_local_shell(
            &profile,
            TerminalSize {
                cols: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            },
            &environment,
        );
        let mut output = Vec::new();
        let mut exit = None;
        block_on(async {
            while let Some(event) = link.reader.next_event().await {
                match event {
                    LinkEvent::Data(data) => output.extend(data),
                    LinkEvent::Message(message) => {
                        if let TerminalServerMessage::Exit { code, reason, .. } = *message {
                            exit = Some((code, reason));
                        }
                    }
                    LinkEvent::Closed { .. } => break,
                }
            }
        });
        assert!(String::from_utf8_lossy(&output).contains("ready"));
        assert_eq!(exit, Some((Some(4), ExitReason::Completed)));
    }
}
