//! One terminal session (`handleSession` and `attachTerminalTransport` in
//! `server/src/ws/terminal-socket.ts`).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use muxus_core::api::{
    ConfigForward, DEFAULT_SESSION_LOG_FILE_PATTERN, SessionEndStatus, SessionLogFileSettings,
    TerminalBackspaceKey, TerminalEnterKey,
};
use muxus_core::protocol::{
    ConnectionHealth, DEFAULT_BREAK_DURATION_MS, ExitReason, LocalProfile, PasteProgressState,
    SerialProfile, SshProfile, TelnetProfile, TerminalClientMessage, TerminalProfile,
    TerminalServerMessage,
};
use muxus_core::stream::{TerminalSize, TransportChannels, TransportEvent};
use muxus_ssh::connection::health::TransportHealth;
use muxus_ssh::connection::{ConnectOptions, ConnectionLeaseOwner, MuxedConnectionLease};
use muxus_ssh::forwards::{ForwardRequest, ForwardRequestExt};
use muxus_store::db::HostRef;

use super::anti_idle::{AntiIdleIo, AntiIdleTimer};
use super::connect_io::SessionConnectIo;
use super::control::ControlChannel;
use super::facade::{Facade, Frame};
use super::link::{TransportLink, break_result};
use super::paste::{PastePacer, PasteProgress, PasteRequest};
use super::{SessionHandle, TerminalEnterKeyReader, TerminalInputWriter};
use crate::Backend;
use crate::adapters::{StoreLoginSequences, StoreVault};
use crate::backend::{Shared, history_sink};
use crate::logging::{
    RecorderConfig, SessionLoggingPatch, SessionLoggingState, SessionRecorder, SystemClock,
    session_profile_identity,
};
use crate::login_sequence::{
    LoginSequenceHost, LoginSequenceSession, LoginSequenceSessionOptions, resolve_login_sequence,
};

type Writer = Arc<dyn Fn(Vec<u8>) + Send + Sync>;
type PasteWriter =
    Arc<dyn Fn(Vec<u8>) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;
type LoggingControl = Arc<dyn Fn(&TerminalClientMessage) -> bool + Send + Sync>;

/// A short random id in the alphabet the TypeScript build's nanoid uses.
pub(crate) fn short_id(length: usize) -> String {
    let mut id = muxus_store::ids::nanoid();
    id.truncate(length.min(id.len()));
    id
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Default)]
struct Io {
    /// Writes terminal input to the transport once one is attached.
    writer: Option<Writer>,
    /// Writes a paced paste step, settling once the transport took it.
    paste_writer: Option<PasteWriter>,
    recorder: Option<SessionRecorder>,
    login_sequence: Option<Arc<LoginSequenceSession>>,
    /// Paced pastes; a paste waiting for the prompt reads the session's output.
    pacer: Option<PastePacer>,
    /// The host's anti-idle text, typed here rather than by the tab so multi-exec never
    /// mirrors it.
    anti_idle: Option<AntiIdleTimer>,
}

struct Session {
    shared: Arc<Shared>,
    facade: Arc<Facade>,
    control: Arc<ControlChannel>,
    terminal_id: String,
    io: Mutex<Io>,
    /// The host whose Backspace and Enter keys the session uses (`keysHost`).
    keys_host: Mutex<Option<LoginSequenceHost>>,
}

/// Start a session for a `connect` or `dial` message.
pub(crate) fn start(backend: &Backend, first: TerminalClientMessage) -> SessionHandle {
    let shared = backend.shared.clone();
    let terminal_id = format!("terminal-{}", short_id(16));
    let (facade, handle) = Facade::new(
        terminal_id.clone(),
        shared.config.reattach_grace,
        shared.runtime.clone(),
        &shared.sessions,
    );
    let control = Arc::new(ControlChannel::default());
    let session = Arc::new(Session {
        shared: shared.clone(),
        facade: facade.clone(),
        control: control.clone(),
        terminal_id: terminal_id.clone(),
        io: Mutex::new(Io::default()),
        keys_host: Mutex::new(None),
    });
    // The facade's listener keeps the session alive until the facade finishes (which drops the
    // listener), the way the socket's listeners kept the TypeScript session's closures alive.
    let listener = session.clone();
    facade.set_listener(move |frame| listener.on_frame(frame));
    {
        let control = control.clone();
        facade.on_close(move || control.close());
    }
    facade.send_message(TerminalServerMessage::Session {
        terminal_id: terminal_id.clone(),
    });
    shared.runtime.spawn(async move {
        if let Err(message) = session.handle(first).await {
            tracing::warn!(error = %message, "terminal session failed");
            session.facade.send_message(TerminalServerMessage::Exit {
                code: Some(1),
                message: Some(message),
                reason: ExitReason::Failed,
            });
            session.facade.close();
        }
    });
    handle
}

fn clamp(value: u32) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn logging_state_message(state: SessionLoggingState) -> TerminalServerMessage {
    TerminalServerMessage::LoggingState {
        enabled: state.enabled,
        session_id: state.session_id,
        paused: state.paused,
        capture_input: state.capture_input,
        warning: state.warning,
        file_path: state.file_path,
    }
}

/// Ends a session once, with the exit the window shows.
struct Finisher {
    done: AtomicBool,
    facade: Arc<Facade>,
    recorder: SessionRecorder,
    exit_code: Mutex<Option<i32>>,
    /// What the log calls the session (`ssh web-01`, `local shell`, `telnet:host:23`).
    label: String,
}

impl Finisher {
    fn new(facade: Arc<Facade>, recorder: SessionRecorder, label: String) -> Arc<Self> {
        Arc::new(Self {
            done: AtomicBool::new(false),
            facade,
            recorder,
            exit_code: Mutex::new(None),
            label,
        })
    }

    fn finish(&self, reason: ExitReason, message: Option<String>) {
        if self.done.swap(true, Ordering::SeqCst) {
            return;
        }
        let session = self.label.as_str();
        let reason_text = message.as_deref().unwrap_or("");
        if self.facade.is_closed() {
            // The window closed the tab: whatever the transport reports after that is the
            // close itself.
            tracing::info!(session, "terminal session closed");
        } else {
            self.log_end(session, reason, reason_text);
        }
        self.recorder.end(match reason {
            ExitReason::Completed => SessionEndStatus::Completed,
            ExitReason::Failed => SessionEndStatus::Failed,
            ExitReason::Disconnected => SessionEndStatus::Disconnected,
        });
        self.facade.send_message(TerminalServerMessage::Exit {
            code: *lock(&self.exit_code),
            message,
            reason,
        });
        self.facade.close();
    }

    fn log_end(&self, session: &str, reason: ExitReason, reason_text: &str) {
        match reason {
            ExitReason::Completed => {
                let code = *lock(&self.exit_code);
                tracing::info!(session, code = ?code, "terminal session ended");
            }
            ExitReason::Failed => {
                tracing::warn!(session, err = reason_text, "terminal session failed")
            }
            ExitReason::Disconnected => {
                tracing::warn!(
                    session,
                    reason = reason_text,
                    "terminal session disconnected"
                )
            }
        }
    }
}

impl Session {
    fn on_frame(&self, frame: Frame) {
        match frame {
            Frame::Data(data) => {
                let (writer, recorder) = {
                    let io = lock(&self.io);
                    (io.writer.clone(), io.recorder.clone())
                };
                if let Some(writer) = writer {
                    if let Some(recorder) = recorder {
                        recorder.input(&data);
                    }
                    writer(data);
                }
                self.anti_idle_input();
            }
            Frame::Message(message) => self.control.push(message),
        }
    }

    fn writer(&self) -> Option<Writer> {
        lock(&self.io).writer.clone()
    }

    /// Input reached the session: the anti-idle wait starts over.
    fn anti_idle_input(&self) {
        let anti_idle = lock(&self.io).anti_idle.clone();
        if let Some(anti_idle) = anti_idle {
            anti_idle.input();
        }
    }

    /// What Backspace and Enter send on the session's host (`hostKeys`), read when they are
    /// needed so a change to the host reaches open sessions.
    fn host_keys(&self) -> (Option<TerminalBackspaceKey>, Option<TerminalEnterKey>) {
        let Some(host) = lock(&self.keys_host).clone() else {
            return (None, None);
        };
        let host = match &host {
            LoginSequenceHost::Alias(alias) => HostRef::Alias(alias),
            LoginSequenceHost::Profile(id) => HostRef::ProfileId(id),
        };
        self.shared
            .database
            .host_input_keys(host)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "could not load the host keys");
                (None, None)
            })
    }

    /// The host's Enter key, read at each use, for the backend's own typing.
    fn enter_key_reader(self: &Arc<Self>) -> TerminalEnterKeyReader {
        let session = Arc::downgrade(self);
        Arc::new(move || {
            session
                .upgrade()
                .and_then(|session| session.host_keys().1)
                .unwrap_or_default()
        })
    }

    /// A host's anti-idle text is typed here rather than by the tab, so multi-exec never
    /// mirrors it; it is recorded like typed input. SSH, Telnet and serial only. Telnet and
    /// serial log in inside the session, so there the first wait starts at the first input,
    /// never at a login prompt nobody has answered.
    async fn start_anti_idle(
        self: &Arc<Self>,
        host: Option<LoginSequenceHost>,
        recorder: &SessionRecorder,
        after_first_input: bool,
    ) {
        let Some(host) = host else {
            return;
        };
        let setting = self
            .db(move |db| {
                db.host_anti_idle(match &host {
                    LoginSequenceHost::Alias(alias) => HostRef::Alias(alias),
                    LoginSequenceHost::Profile(id) => HostRef::ProfileId(id),
                })
            })
            .await;
        let setting = match setting {
            Ok(Some(setting)) => setting,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "could not load the anti-idle setting");
                return;
            }
        };
        if self.facade.is_closed() {
            return;
        }
        let write = {
            let session = Arc::downgrade(self);
            let recorder = recorder.clone();
            Arc::new(move |data: Vec<u8>| {
                let Some(writer) = session.upgrade().and_then(|session| session.writer()) else {
                    return;
                };
                recorder.input(&data);
                writer(data);
            })
        };
        let busy = {
            let session = Arc::downgrade(self);
            Arc::new(move || {
                session.upgrade().is_some_and(|session| {
                    let io = lock(&session.io);
                    io.pacer.as_ref().is_some_and(PastePacer::running)
                        || io
                            .login_sequence
                            .as_ref()
                            .is_some_and(|login| login.running())
                })
            })
        };
        let timer = AntiIdleTimer::new(
            setting,
            AntiIdleIo {
                write,
                busy,
                enter_key: self.enter_key_reader(),
            },
        );
        if after_first_input {
            timer.start_after_first_input();
        } else {
            timer.start();
        }
        lock(&self.io).anti_idle = Some(timer.clone());
        self.facade.on_close(move || timer.close());
    }

    fn set_link_writers(&self, link: &Arc<TransportLink>, drained_paste: bool) {
        let input = link.clone();
        let paste = link.clone();
        let mut io = lock(&self.io);
        io.writer = Some(Arc::new(move |data| {
            input.write(data);
        }));
        io.paste_writer = Some(if drained_paste {
            Arc::new(move |data| Box::pin(paste.write_drained(data)))
        } else {
            // A local pty takes a write at once (`pty.write`).
            Arc::new(move |data| {
                paste.write(data);
                Box::pin(async { Ok(()) })
            })
        });
    }

    async fn db<T: Send + 'static>(
        &self,
        work: impl FnOnce(&muxus_store::Database) -> muxus_store::db::DbResult<T> + Send + 'static,
    ) -> Result<T, String> {
        let database = self.shared.database.clone();
        tokio::task::spawn_blocking(move || work(&database))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
    }

    async fn handle(self: &Arc<Self>, first: TerminalClientMessage) -> Result<(), String> {
        match first {
            TerminalClientMessage::Dial { profile } => self.dial(profile).await,
            TerminalClientMessage::Connect {
                profile,
                fresh_transport,
                title,
                cwd,
                cols,
                rows,
            } => {
                self.connect(profile, fresh_transport, title, cwd, cols, rows)
                    .await
            }
            other => Err(format!(
                "expected connect or dial, got {}",
                serde_json::to_value(&other)
                    .ok()
                    .and_then(|value| value.get("op")?.as_str().map(str::to_string))
                    .unwrap_or_default()
            )),
        }
    }

    /// Shell-less transport (`ssh -N`): the session holds a dial lease so the window can start
    /// forwards on the connId; once those hold their own leases the window closes the session
    /// and the transport lives on with them.
    async fn dial(self: &Arc<Self>, profile: SshProfile) -> Result<(), String> {
        let shared = &self.shared;
        let io = Arc::new(SessionConnectIo {
            facade: self.facade.clone(),
            control: self.control.clone(),
            recorder: None,
        });
        let profile = shared
            .connections
            .resolve_profile(&profile)
            .map_err(|error| error.to_string())?;
        let lease = shared
            .connections
            .connect(
                &profile,
                io,
                ConnectionLeaseOwner::Dial,
                ConnectOptions::default(),
            )
            .await
            .map_err(|error| error.to_string())?;
        if self.facade.is_closed() {
            return Ok(());
        }
        let conn = lease.connection().clone();
        let lease = Arc::new(Mutex::new(Some(lease)));
        {
            let lease = lease.clone();
            self.facade.on_close(move || drop(lock(&lease).take()));
        }
        let watcher = {
            let facade = self.facade.clone();
            let conn = conn.clone();
            tokio::spawn(async move {
                let reason = conn.closed().await;
                if !facade.is_closed() {
                    tracing::warn!(
                        conn_id = %conn.id,
                        host = %conn.host,
                        reason = reason.as_deref().unwrap_or("closed unexpectedly"),
                        "ssh transport closed"
                    );
                }
                facade.send_message(TerminalServerMessage::Exit {
                    code: None,
                    message: Some(
                        reason.unwrap_or_else(|| "The SSH transport closed unexpectedly.".into()),
                    ),
                    reason: ExitReason::Disconnected,
                });
                facade.close();
            })
        };
        self.facade.on_close(move || watcher.abort());
        tracing::info!(target = %profile.target, host = %conn.host, user = %conn.user, conn_id = %conn.id, "ssh transport dialed");
        if let Some(alias) = conn.metadata_alias.clone()
            && let Err(error) = self
                .db(move |db| db.record_open_ssh_connection(&alias).map(drop))
                .await
        {
            tracing::warn!(%error, "could not record recent connection");
        }
        // A dial client closes this session as soon as it has handed the connection to a
        // forward. Finish any post-auth vault prompt first so closing the dial lease cannot
        // reject the prompt before the password is saved.
        conn.wait_for_post_auth().await;
        if self.facade.is_closed() {
            drop(lock(&lease).take());
            return Ok(());
        }
        self.facade.send_message(TerminalServerMessage::Ready {
            conn_id: conn.id.clone(),
            host: Some(conn.host.clone()),
            user: Some(conn.user.clone()),
            sftp_available: Some(conn.sftp_available),
            system_open_ssh: None,
            summary: None,
        });
        Ok(())
    }

    async fn start_recorder(
        &self,
        profile: &TerminalProfile,
        title: Option<String>,
    ) -> Result<SessionRecorder, String> {
        let identity = session_profile_identity(profile);
        let key = identity.profile_key.clone();
        let policy = self.db(move |db| db.session_logging_policy(&key)).await?;
        let database = self.shared.database.clone();
        Ok(SessionRecorder::start(RecorderConfig {
            history: history_sink(&self.shared),
            policy,
            file_settings: Arc::new(move || {
                database
                    .session_log_file_settings()
                    .unwrap_or_else(|_| SessionLogFileSettings {
                        directory: None,
                        filename_pattern: DEFAULT_SESSION_LOG_FILE_PATTERN.to_string(),
                        timestamps: false,
                    })
            }),
            home: self.shared.config.home.clone(),
            identity,
            title,
            clock: Arc::new(SystemClock),
        }))
    }

    async fn connect(
        self: &Arc<Self>,
        profile: TerminalProfile,
        fresh_transport: Option<String>,
        title: Option<String>,
        cwd: Option<String>,
        cols: u32,
        rows: u32,
    ) -> Result<(), String> {
        let shared = &self.shared;
        // Input the backend types for the user, such as a vault secret, goes to the transport
        // directly and never through the recorder.
        let inputs_writer: TerminalInputWriter = {
            let session = Arc::downgrade(self);
            Arc::new(move |data: &[u8]| {
                let Some(session) = session.upgrade() else {
                    return false;
                };
                if session.facade.is_closed() {
                    return false;
                }
                match session.writer() {
                    Some(writer) => {
                        writer(data.to_vec());
                        session.anti_idle_input();
                        true
                    }
                    None => false,
                }
            })
        };

        let profile = match profile {
            TerminalProfile::Ssh(ssh) => TerminalProfile::Ssh(
                shared
                    .connections
                    .resolve_profile(&ssh)
                    .map_err(|error| error.to_string())?,
            ),
            other => other,
        };
        // The host the window reads the tab's keys from: its saved host, or the host list's
        // first entry naming the alias it dialed.
        *lock(&self.keys_host) = match &profile {
            TerminalProfile::Local(_) => None,
            TerminalProfile::Serial(serial) => {
                serial.profile_id.clone().map(LoginSequenceHost::Profile)
            }
            TerminalProfile::Telnet(telnet) => {
                telnet.profile_id.clone().map(LoginSequenceHost::Profile)
            }
            TerminalProfile::Ssh(ssh) => match &ssh.profile_id {
                Some(id) => Some(LoginSequenceHost::Profile(id.clone())),
                None => shared
                    .connections
                    .listed_metadata_alias_for(ssh)
                    .map(LoginSequenceHost::Alias),
            },
        };
        // Its Enter is the host's.
        let registration = shared.terminal_inputs.register_with_enter_key(
            &self.terminal_id,
            inputs_writer,
            self.enter_key_reader(),
        );
        self.facade.on_close(move || drop(registration));
        let recorder = self.start_recorder(&profile, title).await?;
        lock(&self.io).recorder = Some(recorder.clone());
        {
            let facade = self.facade.clone();
            recorder
                .on_state_change(move |state| facade.send_message(logging_state_message(state)));
        }
        let handle_logging: LoggingControl = {
            let recorder = recorder.clone();
            let facade = self.facade.clone();
            Arc::new(move |message: &TerminalClientMessage| match message {
                TerminalClientMessage::SetTitle { title } => {
                    recorder.set_title(title);
                    true
                }
                TerminalClientMessage::SetLogging {
                    enabled,
                    paused,
                    capture_input,
                    log_to_file,
                    log_file_path,
                } => {
                    let state = recorder.set_state(&SessionLoggingPatch {
                        enabled: *enabled,
                        paused: *paused,
                        capture_input: *capture_input,
                        log_to_file: *log_to_file,
                        log_file_path: log_file_path.clone(),
                    });
                    facade.send_message(logging_state_message(state));
                    true
                }
                _ => false,
            })
        };
        // Pasted input the backend paces; each kind of session sets how it is written.
        let pacer = {
            let session: Weak<Session> = Arc::downgrade(self);
            let input_recorder = recorder.clone();
            let facade = self.facade.clone();
            PastePacer::new(
                Arc::new(move |data: Vec<u8>| {
                    let writer = session
                        .upgrade()
                        .and_then(|session| lock(&session.io).paste_writer.clone());
                    match writer {
                        Some(writer) => {
                            input_recorder.input(&data);
                            if let Some(session) = session.upgrade() {
                                session.anti_idle_input();
                            }
                            writer(data)
                        }
                        None => Box::pin(async { Ok(()) }),
                    }
                }),
                {
                    let recorder = recorder.clone();
                    Arc::new(move |progress: PasteProgress| {
                        if progress.state == PasteProgressState::Stopped {
                            recorder.system(&format!(
                                "Paste stopped: {}",
                                progress.message.as_deref().unwrap_or_default()
                            ));
                        }
                        facade.send_message(TerminalServerMessage::PasteProgress {
                            state: progress.state,
                            line: progress.line,
                            lines: progress.lines,
                            sent: progress.sent,
                            total: progress.total,
                            message: progress.message,
                            stop_id: progress.stop_id,
                        });
                    })
                },
            )
        };
        lock(&self.io).pacer = Some(pacer.clone());
        // Closing the tab or losing the connection ends the session and the paste.
        {
            let pacer = pacer.clone();
            self.facade.on_close(move || pacer.close());
        }
        {
            let handle_logging = handle_logging.clone();
            let session = Arc::downgrade(self);
            self.control.set_intercept(move |message| {
                if handle_logging(message) {
                    return true;
                }
                match message {
                    TerminalClientMessage::Paste {
                        text,
                        bracketed,
                        line_delay_ms,
                        char_delay_ms,
                        prompt,
                        enter_key,
                    } => {
                        pacer.enqueue(PasteRequest {
                            text: text.clone(),
                            bracketed: *bracketed,
                            line_delay_ms: *line_delay_ms,
                            char_delay_ms: *char_delay_ms,
                            prompt: prompt.clone(),
                            enter_key: *enter_key,
                        });
                        return true;
                    }
                    TerminalClientMessage::PasteCancel => {
                        pacer.cancel();
                        return true;
                    }
                    _ => {}
                }
                let login = session
                    .upgrade()
                    .and_then(|session| lock(&session.io).login_sequence.clone());
                login.is_some_and(|login| login.handle_control(message))
            });
        }
        self.facade
            .send_message(logging_state_message(recorder.state()));
        {
            let recorder = recorder.clone();
            self.facade
                .on_close(move || recorder.end(SessionEndStatus::Disconnected));
        }

        match profile {
            TerminalProfile::Local(local) => {
                self.local(local, cols, rows, recorder, handle_logging)
            }
            TerminalProfile::Serial(serial) => self.serial(serial, recorder, handle_logging).await,
            TerminalProfile::Telnet(telnet) => {
                self.telnet(telnet, cols, rows, recorder, handle_logging)
                    .await
            }
            TerminalProfile::Ssh(ssh) => {
                self.ssh(
                    ssh,
                    fresh_transport,
                    cwd,
                    cols,
                    rows,
                    recorder,
                    handle_logging,
                )
                .await
            }
        }
    }

    /// A host's login sequence sees output from the moment the transport is attached. It
    /// belongs to this connect: a reconnect runs it again, a window reattaching to the live
    /// session does not.
    fn prepare_login_sequence(
        self: &Arc<Self>,
        host: Option<LoginSequenceHost>,
        recorder: &SessionRecorder,
    ) -> Option<Arc<LoginSequenceSession>> {
        let write = self.shared.terminal_inputs.writer(&self.terminal_id)?;
        let target = host?;
        let sequence =
            resolve_login_sequence(&StoreLoginSequences(self.shared.database.clone()), &target)?;
        let facade = self.facade.clone();
        let session = Arc::new(LoginSequenceSession::new(LoginSequenceSessionOptions {
            sequence,
            vault: Arc::new(StoreVault(self.shared.vault.clone())),
            write,
            recorder: Arc::new(recorder.clone()),
            send: Arc::new(move |message| facade.send_message(message)),
            enter_key: Some(self.enter_key_reader()),
        }));
        {
            let session = session.clone();
            self.facade.on_close(move || session.close());
        }
        lock(&self.io).login_sequence = Some(session.clone());
        Some(session)
    }

    fn local(
        self: &Arc<Self>,
        profile: LocalProfile,
        cols: u32,
        rows: u32,
        recorder: SessionRecorder,
        handle_logging: LoggingControl,
    ) -> Result<(), String> {
        let environment = self.shared.local_environment();
        let pty = muxus_local::spawn_local_pty(
            &profile,
            TerminalSize {
                cols: clamp(cols),
                rows: clamp(rows),
                pixel_width: 0,
                pixel_height: 0,
            },
            &environment,
        )
        .map_err(|error| error.to_string())?;
        let (link, events) = TransportLink::new(pty.channels);
        self.set_link_writers(&link, false);
        {
            let link = link.clone();
            self.control.set_on_message(move |message| {
                if handle_logging(&message) {
                    return;
                }
                if let TerminalClientMessage::Resize { cols, rows } = message {
                    link.resize(cols, rows);
                }
            });
        }
        let finisher = Finisher::new(
            self.facade.clone(),
            recorder.clone(),
            "local shell".to_string(),
        );
        let pump = {
            let facade = self.facade.clone();
            async move {
                while let Ok(event) = events.recv().await {
                    match event {
                        TransportEvent::Data(data) => {
                            recorder.output(&data);
                            facade.send_data(data);
                            facade.wait_for_room().await;
                        }
                        TransportEvent::Exit { code, .. } => {
                            *lock(&finisher.exit_code) = code;
                            finisher.finish(ExitReason::Completed, None);
                            return;
                        }
                        TransportEvent::Error(message) => {
                            finisher.finish(ExitReason::Failed, Some(message));
                            return;
                        }
                    }
                }
            }
        };
        {
            let link = link.clone();
            self.facade.on_close(move || link.close());
        }
        self.facade.send_message(TerminalServerMessage::Ready {
            conn_id: format!("local-{}", std::process::id()),
            host: None,
            user: None,
            sftp_available: None,
            system_open_ssh: None,
            summary: None,
        });
        // Output flows once `ready` is out, as the socket's listeners fired only afterwards.
        tokio::spawn(pump);
        Ok(())
    }

    async fn serial(
        self: &Arc<Self>,
        profile: SerialProfile,
        recorder: SessionRecorder,
        handle_logging: LoggingControl,
    ) -> Result<(), String> {
        self.status(
            &recorder,
            &format!("Opening {} at {} baud …", profile.path, profile.baud_rate),
        );
        let channels = muxus_serial::connect(&profile, None)
            .await
            .map_err(|error| error.0)?;
        if self.facade.is_closed() {
            let _ = channels
                .input
                .try_send(muxus_core::stream::TransportInput::Close);
            return Ok(());
        }
        let saved_host = profile.profile_id.clone().map(LoginSequenceHost::Profile);
        let login = self.prepare_login_sequence(saved_host.clone(), &recorder);
        self.attach_transport(
            channels,
            format!("serial-{}", short_id(10)),
            recorder.clone(),
            handle_logging,
            login,
        );
        self.start_anti_idle(saved_host, &recorder, true).await;
        tracing::info!(path = %profile.path, baud_rate = profile.baud_rate, "serial session established");
        self.record_saved_host(profile.profile_id).await;
        Ok(())
    }

    async fn telnet(
        self: &Arc<Self>,
        profile: TelnetProfile,
        cols: u32,
        rows: u32,
        recorder: SessionRecorder,
        handle_logging: LoggingControl,
    ) -> Result<(), String> {
        self.status(
            &recorder,
            &format!(
                "Connecting to {}:{} over Telnet …",
                profile.host, profile.port
            ),
        );
        let channels = muxus_telnet::connect(
            &profile,
            TerminalSize {
                cols: clamp(cols),
                rows: clamp(rows),
                pixel_width: 0,
                pixel_height: 0,
            },
        )
        .await
        .map_err(|error| error.0)?;
        if self.facade.is_closed() {
            let _ = channels
                .input
                .try_send(muxus_core::stream::TransportInput::Close);
            return Ok(());
        }
        let saved_host = profile.profile_id.clone().map(LoginSequenceHost::Profile);
        let login = self.prepare_login_sequence(saved_host.clone(), &recorder);
        self.attach_transport(
            channels,
            format!("telnet-{}", short_id(10)),
            recorder.clone(),
            handle_logging,
            login,
        );
        self.start_anti_idle(saved_host, &recorder, true).await;
        tracing::info!(host = %profile.host, port = profile.port, "telnet session established");
        self.record_saved_host(profile.profile_id).await;
        Ok(())
    }

    /// A transient status line, also written to the session's logs.
    fn status(&self, recorder: &SessionRecorder, message: &str) {
        recorder.system(message);
        self.facade.send_message(TerminalServerMessage::Status {
            message: message.to_string(),
            transient: Some(true),
        });
    }

    async fn record_saved_host(&self, profile_id: Option<String>) {
        let Some(id) = profile_id else {
            return;
        };
        let log_id = id.clone();
        if let Err(error) = self
            .db(move |db| db.record_saved_host_connection(&id))
            .await
        {
            tracing::warn!(%error, profile_id = %log_id, "could not record recent connection");
        }
    }

    /// Answer every `send-break` with the transport's outcome (`answerBreak`).
    fn answer_breaks(&self, link: &Arc<TransportLink>) {
        if !link.feedback.reports_breaks() {
            return;
        }
        let feedback = link.feedback.clone();
        let facade = self.facade.clone();
        let task = tokio::spawn(async move {
            while let Some(outcome) = feedback.break_outcome().await {
                let (ok, message) = break_result(Some(outcome));
                facade.send_message(TerminalServerMessage::BreakResult { ok, message });
            }
        });
        self.facade.on_close(move || task.abort());
    }

    /// Attach a byte transport (serial, Telnet) with shared flow control.
    fn attach_transport(
        self: &Arc<Self>,
        channels: TransportChannels,
        conn_id: String,
        recorder: SessionRecorder,
        handle_logging: LoggingControl,
        login: Option<Arc<LoginSequenceSession>>,
    ) {
        let (link, events) = TransportLink::new(channels);
        self.set_link_writers(&link, true);
        {
            let link = link.clone();
            self.control.set_on_message(move |message| {
                if handle_logging(&message) {
                    return;
                }
                match message {
                    TerminalClientMessage::Resize { cols, rows } => link.resize(cols, rows),
                    TerminalClientMessage::SendBreak => link.send_break(DEFAULT_BREAK_DURATION_MS),
                    _ => {}
                }
            });
        }
        self.answer_breaks(&link);
        let finisher = Finisher::new(self.facade.clone(), recorder.clone(), conn_id.clone());
        let pacer = lock(&self.io).pacer.clone();
        let pump = {
            let facade = self.facade.clone();
            let login = login.clone();
            async move {
                while let Ok(event) = events.recv().await {
                    match event {
                        TransportEvent::Data(data) => {
                            recorder.output(&data);
                            if let Some(login) = &login {
                                login.output(&data);
                            }
                            if let Some(pacer) = &pacer {
                                pacer.output(&data);
                            }
                            facade.send_data(data);
                            facade.wait_for_room().await;
                        }
                        TransportEvent::Exit { .. } => {
                            finisher.finish(ExitReason::Completed, None);
                            return;
                        }
                        TransportEvent::Error(message) => {
                            finisher.finish(ExitReason::Failed, Some(message));
                            return;
                        }
                    }
                }
            }
        };
        {
            let link = link.clone();
            self.facade.on_close(move || link.close());
        }
        if let Some(login) = &login {
            login.start();
        }
        self.facade.send_message(TerminalServerMessage::Ready {
            conn_id,
            host: None,
            user: None,
            sftp_available: None,
            system_open_ssh: None,
            summary: None,
        });
        tokio::spawn(pump);
    }

    #[allow(clippy::too_many_arguments)]
    async fn ssh(
        self: &Arc<Self>,
        profile: SshProfile,
        fresh_transport: Option<String>,
        cwd: Option<String>,
        cols: u32,
        rows: u32,
        recorder: SessionRecorder,
        handle_logging: LoggingControl,
    ) -> Result<(), String> {
        let shared = self.shared.clone();
        let io = Arc::new(SessionConnectIo {
            facade: self.facade.clone(),
            control: self.control.clone(),
            recorder: Some(recorder.clone()),
        });
        // The pty advertises the Backspace the window sends as its erase character.
        let backspace_key = self.host_keys().0.unwrap_or_default();
        let shell = shared
            .connections
            .connect_shell_for_keys(
                &profile,
                io,
                cols,
                rows,
                muxus_local::DEFAULT_TERM,
                fresh_transport,
                cwd.as_deref(),
                backspace_key,
            )
            .await
            .map_err(|error| error.to_string())?;
        let muxus_ssh::connection::TerminalShell {
            lease,
            channel,
            transport,
            mut summary,
        } = shell;
        let conn = lease.connection().clone();
        let (channels, ends) = TransportChannels::pair(muxus_ssh::shell::DEFAULT_CHANNEL_CAPACITY);
        tokio::spawn(muxus_ssh::shell::pump(channel, ends));
        let (link, events) = TransportLink::new(channels);

        // Session forwards belong to the terminal sessions using this transport; stop them when
        // the last terminal/dial lease leaves. Saved/manual tunnels are marked independent and
        // keep their own leases.
        let lease: Arc<Mutex<Option<MuxedConnectionLease>>> = Arc::new(Mutex::new(Some(lease)));
        let release_session = {
            let link = link.clone();
            let lease = lease.clone();
            let shared = shared.clone();
            let conn_id = conn.id.clone();
            move || {
                // On a shared transport the connection (and the remote shell with it) outlives
                // this tab, so the channel must be closed explicitly.
                link.close();
                drop(lock(&lease).take());
                if shared.connections.lease_count(
                    &conn_id,
                    Some(&[ConnectionLeaseOwner::Terminal, ConnectionLeaseOwner::Dial]),
                ) == 0
                {
                    shared.forwards.stop_session_for_connection(&conn_id);
                }
            }
        };
        if self.facade.is_closed() {
            release_session();
            return Ok(());
        }
        self.facade.on_close(release_session);
        {
            let facade = self.facade.clone();
            let mut health = conn.subscribe_health();
            let task = tokio::spawn(async move {
                let state = |health: TransportHealth| match health {
                    TransportHealth::Healthy => ConnectionHealth::Healthy,
                    TransportHealth::Suspect => ConnectionHealth::Suspect,
                };
                if *health.borrow_and_update() == TransportHealth::Suspect {
                    facade.send_message(TerminalServerMessage::ConnectionHealth {
                        state: ConnectionHealth::Suspect,
                    });
                }
                while health.changed().await.is_ok() {
                    let current = *health.borrow_and_update();
                    facade.send_message(TerminalServerMessage::ConnectionHealth {
                        state: state(current),
                    });
                }
            });
            self.facade.on_close(move || task.abort());
        }

        // Every multiplexed alias contributes its resolved *Forward rules to the shared
        // transport. start_config deduplicates rules already started by a sibling session and
        // collapses concurrent attempts for the same listener.
        let mut config_forward_ids: Vec<String> = Vec::new();
        let mut running_forwards: Vec<ConfigForward> = Vec::new();
        for forward in conn.config_forwards() {
            if self.facade.is_closed() {
                break;
            }
            match shared
                .forwards
                .start_config(ForwardRequest::from_config(conn.id.clone(), &forward))
                .await
            {
                Ok(started) => {
                    if started.started {
                        config_forward_ids.push(started.info.id.clone());
                    }
                    running_forwards.push(forward);
                }
                Err(error) => {
                    let letter = match forward.kind {
                        muxus_core::api::ForwardType::Local => "L",
                        muxus_core::api::ForwardType::Remote => "R",
                        muxus_core::api::ForwardType::Dynamic => "D",
                    };
                    self.facade.send_message(TerminalServerMessage::Status {
                        message: format!(
                            "forward -{letter} {} failed: {}",
                            forward.bind_port, error.message
                        ),
                        transient: None,
                    });
                }
            }
        }
        if self.facade.is_closed() {
            // The tab disappeared before setup reached `ready`; do not leave half-created
            // config forwards running with no visible owning session. release_session already
            // ran via the close handler.
            if shared.connections.lease_count(
                &conn.id,
                Some(&[ConnectionLeaseOwner::Terminal, ConnectionLeaseOwner::Dial]),
            ) == 0
            {
                for id in &config_forward_ids {
                    shared.forwards.stop(id);
                }
            }
            return Ok(());
        }

        // The paste waits once the channel window has taken the data.
        self.set_link_writers(&link, true);
        {
            let link = link.clone();
            self.control.set_on_message(move |message| {
                if handle_logging(&message) {
                    return;
                }
                match message {
                    TerminalClientMessage::Resize { cols, rows } => link.resize(cols, rows),
                    TerminalClientMessage::SendBreak => link.send_break(DEFAULT_BREAK_DURATION_MS),
                    _ => {}
                }
            });
        }
        self.answer_breaks(&link);
        let login_host = match &profile.profile_id {
            Some(id) => Some(LoginSequenceHost::Profile(id.clone())),
            None => shared
                .connections
                .metadata_alias_for(&profile)
                .map(LoginSequenceHost::Alias),
        };
        let login = self.prepare_login_sequence(login_host.clone(), &recorder);

        let finisher = Finisher::new(
            self.facade.clone(),
            recorder.clone(),
            format!("ssh {}", profile.target),
        );
        let pacer = lock(&self.io).pacer.clone();
        let anti_idle_recorder = recorder.clone();
        let pump = {
            let facade = self.facade.clone();
            let login = login.clone();
            let finisher = finisher.clone();
            async move {
                while let Ok(event) = events.recv().await {
                    match event {
                        TransportEvent::Data(data) => {
                            recorder.output(&data);
                            if let Some(login) = &login {
                                login.output(&data);
                            }
                            if let Some(pacer) = &pacer {
                                pacer.output(&data);
                            }
                            facade.send_data(data);
                            facade.wait_for_room().await;
                        }
                        TransportEvent::Exit { code, signal } => {
                            if code.is_some() || signal.is_some() {
                                *lock(&finisher.exit_code) = code;
                                finisher.finish(ExitReason::Completed, None);
                            } else {
                                finisher.finish(
                                    ExitReason::Disconnected,
                                    Some("The SSH channel closed without an exit status.".into()),
                                );
                            }
                            return;
                        }
                        TransportEvent::Error(message) => {
                            finisher.finish(ExitReason::Failed, Some(message));
                            return;
                        }
                    }
                }
            }
        };
        {
            let conn = conn.clone();
            let finisher = finisher.clone();
            let task = tokio::spawn(async move {
                let reason = conn.closed().await;
                finisher.finish(
                    ExitReason::Disconnected,
                    Some(reason.unwrap_or_else(|| "The SSH transport closed unexpectedly.".into())),
                );
            });
            self.facade.on_close(move || task.abort());
        }

        tracing::info!(target = %profile.target, host = %conn.host, user = %conn.user, conn_id = %conn.id, ?transport, "ssh session established");
        self.record_saved_host(profile.profile_id.clone()).await;
        if let Some(alias) = conn.metadata_alias.clone()
            && let Err(error) = self
                .db(move |db| db.record_open_ssh_connection(&alias).map(drop))
                .await
        {
            tracing::warn!(%error, "could not record recent connection");
        }
        // Started before `ready`, so the tab learns a sequence is running before it would type
        // anything of its own (a tmux reattach waits for the sequence).
        if let Some(login) = &login {
            login.start();
        }
        self.start_anti_idle(login_host, &anti_idle_recorder, false)
            .await;
        summary.forwards = running_forwards;
        self.facade.send_message(TerminalServerMessage::Ready {
            conn_id: conn.id.clone(),
            host: Some(conn.host.clone()),
            user: Some(conn.user.clone()),
            sftp_available: Some(conn.sftp_available),
            system_open_ssh: None,
            summary: Some(summary),
        });
        tokio::spawn(pump);
        Ok(())
    }
}
