//! A NETCONF or gNMI tab (`ManagementViewImpl.tsx`). The connection lives on
//! the backend; the workbench (drafts, results, history, explorer) belongs to
//! the tab and outlives reconnects, so a dropped session costs nothing but a
//! click.

use std::sync::Arc;
use std::time::{Duration, Instant};

use muxus_core::management::{ManagementProtocol, ManagementSessionInfo};
use muxus_core::protocol::{
    AuthPromptInfo, AuthPromptResponse, DesktopCertificateChallenge, ExitReason, HostKeyChallenge,
    ManagementClientMessage, ManagementProfile, ProfileKind,
};
use muxus_mgmt::workbench::connection::{ConnectionEvent, ManagementConnection};
use muxus_mgmt::workbench::controller::WorkbenchController;
use muxus_mgmt::workbench::store::{ReceivedNotification, RunStatus, WorkbenchStore};
use muxus_session::Backend;
use muxus_session::recovery::{
    AUTO_RECONNECT_DELAYS_MS, AUTO_RECONNECT_STABLE_MS, AutoReconnectInput, auto_reconnect_delay_ms,
};
use muxus_ui::gpui_kit::{
    App, AppContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Task, Window, div,
};
use muxus_ui::theme::{ActiveTheme, Status};

use super::fields::Fields;
use super::ui::Ui;

pub type Controller = WorkbenchController<ManagementConnection>;

/// Where the connection stands (`SessionPhase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Idle,
    Connecting,
    Connected,
    Ended,
}

/// The tab status the view reports (`SessionTab.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabStatus {
    Connecting,
    Connected,
    Closed,
}

/// A question the session asks while it connects. The app shows its shared
/// dialogs for these and answers through [`ManagementView::answer_auth`],
/// [`ManagementView::answer_host_key`] and [`ManagementView::answer_certificate`].
#[derive(Debug, Clone, PartialEq)]
pub enum ManagementPrompt {
    Auth(AuthPromptInfo),
    HostKey(HostKeyChallenge),
    Certificate(DesktopCertificateChallenge),
}

/// What the view tells its tab and the window.
#[derive(Debug, Clone, PartialEq)]
pub enum ManagementViewEvent {
    /// The tab's status changed (`updateTab`).
    Status {
        status: TabStatus,
        failure_reason: Option<String>,
        disconnect_reason: Option<ExitReason>,
    },
    /// Show this prompt (`AuthPromptDialog`, `HostKeyDialog`, `DesktopCertificateDialog`).
    Prompt(ManagementPrompt),
    /// The open prompts were withdrawn (the session closed).
    PromptsClosed,
    /// A toast (`showToast`).
    Toast { status: Status, message: String },
}

/// A scheduled automatic reconnect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Redial {
    pub delay_ms: u64,
    pub attempt: usize,
    pub deadline: Instant,
}

/// The session behind the workbench, while it is up.
pub(super) struct LiveSession {
    pub connection: ManagementConnection,
    pub info: ManagementSessionInfo,
    pub profile: ManagementProfile,
}

pub struct ManagementView {
    pub(super) backend: Backend,
    pub(super) profile: ManagementProfile,
    pub(super) title: SharedString,
    pub(super) active: bool,
    auto_reconnect: bool,
    pub(super) store: WorkbenchStore,
    pub(super) phase: SessionPhase,
    pub(super) status_text: Option<String>,
    pub(super) end_message: Option<String>,
    pub(super) session: Option<LiveSession>,
    /// What the last session advertised, kept across drops so the workbench stays usable.
    pub(super) last_info: Option<(ManagementSessionInfo, ManagementProfile)>,
    pub(super) controller: Arc<Controller>,
    generation: u64,
    pub(super) redial: Option<Redial>,
    redial_attempts: usize,
    user_disconnect: bool,
    connected_at: Option<Instant>,
    saw_auth_prompt: bool,
    connection: Option<ManagementConnection>,
    pub(super) prompts: Vec<ManagementPrompt>,
    pub(super) ui: Ui,
    pub(super) fields: Fields,
    pub(super) focus: FocusHandle,
    _watch: Task<()>,
    _events: Option<Task<()>>,
    _redial: Option<Task<()>>,
    _tick: Option<Task<()>>,
    _keys: Option<muxus_ui::gpui_kit::Subscription>,
}

impl EventEmitter<ManagementViewEvent> for ManagementView {}

impl Focusable for ManagementView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Open the workbench for a NETCONF or gNMI host. With `connect`, the session
/// starts right away (`tab.connectOnMount`); otherwise the view waits for
/// Connect.
pub fn open_management_view(
    backend: Backend,
    profile: ManagementProfile,
    title: impl Into<SharedString>,
    connect: bool,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ManagementView> {
    let title = title.into();
    cx.new(|cx| ManagementView::new(backend, profile, title, connect, window, cx))
}

fn protocol_of(profile: &ManagementProfile) -> ManagementProtocol {
    match profile {
        ManagementProfile::Gnmi(_) => ManagementProtocol::Gnmi,
        ManagementProfile::Netconf(_) => ManagementProtocol::Netconf,
    }
}

impl ManagementView {
    fn new(
        backend: Backend,
        profile: ManagementProfile,
        title: SharedString,
        connect: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = WorkbenchStore::new(protocol_of(&profile));
        let controller = WorkbenchController::new(
            store.clone(),
            None,
            profile.clone(),
            None,
            backend.runtime().clone(),
        );
        let watch = Self::watch_store(&store, cx);
        let mut view = Self {
            backend,
            profile,
            title,
            active: true,
            auto_reconnect: true,
            store,
            phase: SessionPhase::Idle,
            status_text: None,
            end_message: None,
            session: None,
            last_info: None,
            controller,
            generation: 0,
            redial: None,
            redial_attempts: 0,
            user_disconnect: false,
            connected_at: None,
            saw_auth_prompt: false,
            connection: None,
            prompts: Vec::new(),
            ui: Ui::default(),
            fields: Fields::default(),
            focus: cx.focus_handle(),
            _watch: watch,
            _events: None,
            _redial: None,
            _tick: None,
            _keys: None,
        };
        view.start_ticker(cx);
        let entity = cx.weak_entity();
        view._keys = Some(cx.intercept_keystrokes(move |event, window, cx| {
            let used = entity
                .update(cx, |this, cx| this.intercept_key(event, window, cx))
                .unwrap_or(false);
            if used {
                cx.stop_propagation();
            }
        }));
        if connect {
            view.connect(window, cx);
        }
        view
    }

    /// Re-render on every store change, at most once a frame's worth.
    fn watch_store(store: &WorkbenchStore, cx: &mut Context<Self>) -> Task<()> {
        let mut changes = store.watch();
        cx.spawn(async move |this, cx| {
            while changes.changed().await.is_ok() {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                changes.borrow_and_update();
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
    }

    /// A once-a-second tick for countdowns, ages and the elapsed time of rotations.
    fn start_ticker(&mut self, cx: &mut Context<Self>) {
        self._tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let keep = this
                    .update(cx, |this, cx| {
                        this.tools_tick(cx);
                        if this.needs_tick() {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !keep {
                    break;
                }
            }
        }));
    }

    fn needs_tick(&self) -> bool {
        self.redial.is_some()
            || self.store.read(|state| {
                state.confirm_deadline.is_some()
                    || state
                        .runs
                        .iter()
                        .any(|run| run.status == RunStatus::Streaming)
                    || !state.tools.is_empty()
            })
    }

    pub(super) fn runtime(&self) -> &tokio::runtime::Handle {
        self.backend.runtime()
    }

    /// Bumped by every connect, so per-session effects run once per session.
    pub(super) fn session_generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn protocol(&self) -> ManagementProtocol {
        protocol_of(&self.profile)
    }

    pub(super) fn is_gnmi(&self) -> bool {
        self.protocol() == ManagementProtocol::Gnmi
    }

    /// The session is up: requests can run.
    pub(super) fn connected(&self) -> bool {
        self.phase == SessionPhase::Connected && self.controller.connected()
    }

    /// The profile as dialed (saved hosts resolve to their stored settings).
    pub(super) fn dialed_profile(&self) -> &ManagementProfile {
        self.session
            .as_ref()
            .map(|session| &session.profile)
            .or(self.last_info.as_ref().map(|(_, profile)| profile))
            .unwrap_or(&self.profile)
    }

    // Public API

    /// Whether this tab is the one shown in its pane.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            cx.notify();
        }
    }

    /// The tab title (the host's display name).
    pub fn set_title(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.title = title.into();
        cx.notify();
    }

    /// The `autoReconnectRemote` preference.
    pub fn set_auto_reconnect(&mut self, enabled: bool) {
        self.auto_reconnect = enabled;
    }

    /// The open prompts, oldest first.
    pub fn prompts(&self) -> &[ManagementPrompt] {
        &self.prompts
    }

    /// Reconnect (the tab's Reconnect action, or the buttons in the view).
    pub fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.redial_attempts = 0;
        self.connect(window, cx);
    }

    /// Close the session on purpose; no automatic reconnect follows.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.user_disconnect = true;
        self.redial = None;
        self._redial = None;
        if let Some(connection) = &self.connection {
            connection.close();
        }
        cx.notify();
    }

    /// Answer the auth prompt; `None` dismisses it and closes the session.
    pub fn answer_auth(&mut self, response: Option<AuthPromptResponse>, cx: &mut Context<Self>) {
        self.take_prompt(|prompt| matches!(prompt, ManagementPrompt::Auth(_)));
        match response {
            None => {
                if let Some(connection) = &self.connection {
                    connection.close();
                }
            }
            Some(response) => self.send(ManagementClientMessage::AuthResponse(response)),
        }
        cx.notify();
    }

    pub fn answer_host_key(&mut self, accept: bool, cx: &mut Context<Self>) {
        self.take_prompt(|prompt| matches!(prompt, ManagementPrompt::HostKey(_)));
        self.send(ManagementClientMessage::HostKeyResponse { accept });
        cx.notify();
    }

    pub fn answer_certificate(&mut self, accept: bool, cx: &mut Context<Self>) {
        self.take_prompt(|prompt| matches!(prompt, ManagementPrompt::Certificate(_)));
        if !accept {
            self.user_disconnect = true;
        }
        self.send(ManagementClientMessage::CertificateResponse { accept });
        cx.notify();
    }

    fn take_prompt(&mut self, which: impl Fn(&ManagementPrompt) -> bool) {
        if let Some(index) = self.prompts.iter().position(which) {
            self.prompts.remove(index);
        }
    }

    fn send(&self, message: ManagementClientMessage) {
        if let Some(connection) = &self.connection {
            connection.send(message);
        }
    }

    pub(super) fn toast(
        &mut self,
        status: Status,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(ManagementViewEvent::Toast {
            status,
            message: message.into(),
        });
    }

    /// Copy `text` and say what was copied.
    pub(super) fn copy(&mut self, text: String, what: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.toast(Status::Success, format!("Copied {what}."), cx);
    }

    /// Ask where to save `text`, then write it (`saveTextFile`).
    pub(super) fn save_file(&mut self, name: String, text: String, cx: &mut Context<Self>) {
        let directory = dirs_downloads();
        let chosen = crate::util::file_dialog::ask_save_path(&directory, &name, cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = chosen.await.path().map(std::path::Path::to_path_buf) else {
                return;
            };
            if let Err(error) = std::fs::write(&path, text) {
                let _ = this.update(cx, |this, cx| {
                    this.toast(Status::Error, format!("Could not save: {error}"), cx);
                });
            }
        })
        .detach();
    }

    // Connection lifecycle

    /// Start a fresh session (`generation + 1`).
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(previous) = self.connection.take() {
            previous.close();
        }
        self.generation += 1;
        let generation = self.generation;
        self.connected_at = None;
        self.saw_auth_prompt = false;
        self.user_disconnect = false;
        self.phase = SessionPhase::Connecting;
        self.status_text = None;
        self.end_message = None;
        self.redial = None;
        self._redial = None;
        cx.emit(ManagementViewEvent::Status {
            status: TabStatus::Connecting,
            failure_reason: None,
            disconnect_reason: None,
        });

        let channels = self.backend.open_management_session();
        let connection = ManagementConnection::new(channels.input, self.profile.clone());
        let (events_tx, events) = async_channel::unbounded();
        self.runtime()
            .spawn(connection.clone().dispatch(channels.output, events_tx));
        self.connection = Some(connection.clone());
        self._events = Some(cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let keep = this
                    .update_in(cx, |this, window, cx| {
                        if this.generation == generation {
                            this.on_connection_event(event, &connection, window, cx);
                        }
                    })
                    .is_ok();
                if !keep {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent,
        connection: &ManagementConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ConnectionEvent::Status { message, .. } => {
                // A login banner (SSH servers send several lines) is kept for the
                // Device tab instead of standing in for the connection status.
                if message.split('\n').count() > 2 {
                    self.store.update(|state| state.banner = Some(message));
                } else {
                    self.status_text = Some(message);
                }
            }
            ConnectionEvent::AuthPrompt(info) => {
                self.saw_auth_prompt = true;
                self.prompt(ManagementPrompt::Auth(info), cx);
            }
            ConnectionEvent::HostKey(challenge) => {
                self.prompt(ManagementPrompt::HostKey(challenge), cx);
            }
            ConnectionEvent::Certificate(challenge) => {
                self.prompt(ManagementPrompt::Certificate(challenge), cx);
            }
            ConnectionEvent::Ready { info, profile } => {
                self.connected_at = Some(Instant::now());
                self.session = Some(LiveSession {
                    connection: connection.clone(),
                    info: info.clone(),
                    profile: profile.clone(),
                });
                self.last_info = Some((info, profile));
                self.phase = SessionPhase::Connected;
                self.status_text = None;
                self.rebuild_controller();
                cx.emit(ManagementViewEvent::Status {
                    status: TabStatus::Connected,
                    failure_reason: None,
                    disconnect_reason: None,
                });
                self.on_connected(window, cx);
            }
            ConnectionEvent::NetconfNotification {
                xml,
                event_time,
                received_at,
            } => {
                self.store.update(|state| {
                    state.notifications.insert(
                        0,
                        ReceivedNotification {
                            xml,
                            event_time,
                            received_at,
                        },
                    );
                    state.notifications.truncate(500);
                });
            }
            ConnectionEvent::Closed { exit } => self.on_closed(exit, window, cx),
        }
        cx.notify();
    }

    fn prompt(&mut self, prompt: ManagementPrompt, cx: &mut Context<Self>) {
        self.prompts.push(prompt.clone());
        cx.emit(ManagementViewEvent::Prompt(prompt));
    }

    fn on_closed(
        &mut self,
        exit: Option<(Option<String>, ExitReason)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connection = None;
        if !self.prompts.is_empty() {
            self.prompts.clear();
            cx.emit(ManagementViewEvent::PromptsClosed);
        }
        self.session = None;
        self.rebuild_controller();
        let ever_connected = self.connected_at.is_some();
        let reason = exit
            .as_ref()
            .map(|(_, reason)| *reason)
            .unwrap_or(if ever_connected {
                ExitReason::Disconnected
            } else {
                ExitReason::Failed
            });
        let message = exit
            .and_then(|(message, _)| message)
            .unwrap_or_else(|| "The connection closed.".into());
        self.phase = SessionPhase::Ended;
        self.end_message = Some(message.clone());
        if let Some(at) = self.connected_at
            && at.elapsed() >= Duration::from_millis(AUTO_RECONNECT_STABLE_MS)
        {
            self.redial_attempts = 0;
        }
        let delay = auto_reconnect_delay_ms(AutoReconnectInput {
            enabled: self.auto_reconnect && !self.user_disconnect,
            profile_kind: match self.profile {
                ManagementProfile::Gnmi(_) => ProfileKind::Gnmi,
                ManagementProfile::Netconf(_) => ProfileKind::Netconf,
            },
            reason,
            attempts: self.redial_attempts,
            saw_auth_prompt: self.saw_auth_prompt,
        });
        if let Some(delay_ms) = delay {
            self.redial_attempts += 1;
            self.schedule_redial(delay_ms, window, cx);
        }
        cx.emit(ManagementViewEvent::Status {
            status: TabStatus::Closed,
            failure_reason: (reason != ExitReason::Completed).then_some(message),
            disconnect_reason: Some(reason),
        });
    }

    fn schedule_redial(&mut self, delay_ms: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.redial = Some(Redial {
            delay_ms,
            attempt: self.redial_attempts,
            deadline: Instant::now() + Duration::from_millis(delay_ms),
        });
        self._redial = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(delay_ms))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.redial = None;
                if this.auto_reconnect {
                    this.connect(window, cx);
                }
                cx.notify();
            });
        }));
    }

    /// The controller follows the session, as `useMemo` rebuilt it.
    fn rebuild_controller(&mut self) {
        self.controller.detach();
        let (connection, info) = match &self.session {
            Some(session) => (
                Some(Arc::new(session.connection.clone())),
                Some(session.info.clone()),
            ),
            None => (None, self.last_info.as_ref().map(|(info, _)| info.clone())),
        };
        self.controller = WorkbenchController::new(
            self.store.clone(),
            connection,
            self.dialed_profile().clone(),
            info,
            self.runtime().clone(),
        );
    }

    /// Work the workbench does once a session is up.
    fn on_connected(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // The candidate may already hold someone's uncommitted changes.
        if self.controller.netconf_info().is_some() {
            self.check_candidate(cx);
        }
        self.load_explorer_root();
        self.refresh_library(cx);
    }

    /// The explorer's top level, once connected.
    pub(super) fn load_explorer_root(&mut self) {
        let needs = self.store.read(|state| {
            !state.explorer.root.loaded
                && !state.explorer.loading.contains("")
                && !state.explorer.errors.contains_key("")
        });
        if needs && self.connected() {
            self.controller.load_node_later("");
        }
    }

    /// `controller.checkCandidate()` without waiting for it.
    pub(super) fn check_candidate(&mut self, _cx: &mut Context<Self>) {
        let controller = self.controller.clone();
        self.runtime().spawn(async move {
            controller.check_candidate().await;
        });
    }

    /// Run a future on the backend runtime and get its result on the UI side.
    pub(super) fn on_runtime<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> impl Future<Output = Option<T>> + 'static {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.runtime().spawn(async move {
            let _ = tx.send(future.await);
        });
        async move { rx.await.ok() }
    }

    pub(super) fn redial_text(&self) -> Option<String> {
        self.redial.map(|redial| {
            format!(
                "Reconnecting in {} s (attempt {} of {})",
                (redial.delay_ms as f64 / 1000.).round(),
                redial.attempt,
                AUTO_RECONNECT_DELAYS_MS.len()
            )
        })
    }

    /// Seconds left before the automatic reconnect, rounded up.
    pub(super) fn redial_remaining(&self) -> Option<u64> {
        self.redial.map(|redial| {
            let left = redial.deadline.saturating_duration_since(Instant::now());
            (left.as_millis() as f64 / 1000.).ceil() as u64
        })
    }
}

fn dirs_downloads() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(|home| std::path::PathBuf::from(home).join("Downloads"))
        .filter(|path| path.is_dir())
        .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
}

impl Render for ManagementView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let content = if self.last_info.is_some() {
            self.render_workbench(window, cx).into_any_element()
        } else {
            self.render_connect_screen(window, cx).into_any_element()
        };
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(theme.palette.background_default)
            .font_family(muxus_ui::theme::fonts::UI)
            .text_color(theme.palette.text.primary)
            .child(content)
            .children(self.render_overlays(window, cx))
    }
}
