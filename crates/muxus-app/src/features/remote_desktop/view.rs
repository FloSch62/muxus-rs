//! An RDP or VNC tab (`RemoteDesktopViewImpl.tsx`): the remote picture fitted into the pane,
//! the hover toolbar, and the connecting and ended screens. The session runs on the backend
//! ([`Backend::open_desktop_session`]); this view draws what it reports and feeds it input.

use std::time::{Duration, Instant};

use muxus_core::protocol::{
    AuthPromptInfo, AuthPromptResponse, DesktopCertificateChallenge, DesktopProfile, ExitReason,
    HostKeyChallenge, ProfileKind,
};
use muxus_session::Backend;
use muxus_session::desktop::{
    DesktopConnection, DesktopCursor, DesktopEvent, DesktopHandle, DesktopOptions,
    FALLBACK_DESKTOP_SIZE, MouseButton, PixelOrder, RESIZE_SETTLE_MS, ViewRect, WheelDelta,
    WheelUnit, desktop_point, fit_rect,
};
use muxus_session::recovery::{
    AUTO_RECONNECT_DELAYS_MS, AUTO_RECONNECT_STABLE_MS, AutoReconnectInput, auto_reconnect_delay_ms,
};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase,
    Entity, EventEmitter, FocusHandle, Focusable, HitboxBehavior, InteractiveElement, IntoElement,
    KeyDownEvent, KeyUpEvent, ModifiersChangedEvent, MouseButton as GpuiButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, NavigationDirection, ParentElement, Pixels, Render, ScrollDelta,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window,
    canvas, div, px, relative,
};
use muxus_ui::icon::icon;
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::button::{Button, ButtonColor, IconButton, elevation};
use muxus_ui::widgets::progress::CircularProgress;

use super::KEY_CONTEXT;
use super::keys::KeyTranslator;
use super::picture::{Picture, PictureSnapshot};

/// The toolbar's distance from the top of the pane (`top: 6`), and the height of the strip
/// along the top edge that brings it back (`height: 6`).
const TOOLBAR_TOP: f32 = 6.;
const TOOLBAR_STRIP: f32 = 6.;
/// The ended screen's message is at most this wide (`maxWidth: 520`).
const MESSAGE_MAX_WIDTH: f32 = 520.;
/// `<CircularProgress size={28} />` and the ended screen's icon (`fontSize: 40`).
const SPINNER_SIZE: f32 = 28.;
const ENDED_ICON_SIZE: f32 = 40.;
/// Opacity of the ended screen's icon and of the redial countdown.
const ENDED_ICON_OPACITY: f32 = 0.6;
const REDIAL_OPACITY: f32 = 0.75;
/// How long the toolbar stays in view after connecting, so it can be found.
const TOOLBAR_INTRO: Duration = Duration::from_millis(2500);
/// How long a new tab waits for its first frame before connecting without a pane size.
const UNMEASURED_CONNECT: Duration = Duration::from_millis(250);
/// The toolbar's slide and fade (`transition: transform 160ms ease, opacity 160ms ease`).
const TOOLBAR_TRANSITION: Duration = Duration::from_millis(160);
/// The toolbar Paper's height: a small IconButton plus 2px padding above and below.
const TOOLBAR_HEIGHT: f32 = 28.571 + 4.;
/// Pixels a GPUI wheel line is worth: a notch is three lines, and 120 pixels is one notch of
/// the RDP wheel (`WHEEL_DELTA`).
const PIXELS_PER_LINE: f64 = 40.0;

/// Where the connection stands (`Phase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
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

/// A question the session asks. The app shows its shared dialogs (`AuthPromptDialog`,
/// `HostKeyDialog`, `DesktopCertificateDialog`) and answers through
/// [`RemoteDesktopView::answer_auth`], [`RemoteDesktopView::answer_host_key`] and
/// [`RemoteDesktopView::answer_certificate`].
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopPrompt {
    Auth(AuthPromptInfo),
    HostKey(HostKeyChallenge),
    Certificate(DesktopCertificateChallenge),
}

/// What the view tells its tab and the window.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteDesktopViewEvent {
    /// The tab's status changed (`updateTab`).
    Status {
        status: TabStatus,
        failure_reason: Option<String>,
        disconnect_reason: Option<ExitReason>,
    },
    /// Show this prompt.
    Prompt(DesktopPrompt),
    /// The open prompts were withdrawn (the session ended).
    PromptsClosed,
    /// A toast (`showToast`): lasting notes once the desktop is up.
    Toast { status: Status, message: String },
}

/// A scheduled automatic reconnect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Redial {
    pub delay_ms: u64,
    pub attempt: usize,
}

/// The toolbar's slide-in state.
#[derive(Debug, Clone, Copy)]
struct Toolbar {
    visible: bool,
    /// When `visible` last changed, for the transition.
    changed: Instant,
    /// Kept in view regardless of the pointer (screenshots).
    pinned: bool,
}

impl Toolbar {
    /// How far in view: 0 hidden, 1 shown.
    fn shown(&self) -> f32 {
        let progress =
            (self.changed.elapsed().as_secs_f32() / TOOLBAR_TRANSITION.as_secs_f32()).min(1.);
        // CSS `ease` is close enough to a smoothstep for 160 ms.
        let eased = progress * progress * (3. - 2. * progress);
        if self.visible { eased } else { 1. - eased }
    }

    fn animating(&self) -> bool {
        self.changed.elapsed() < TOOLBAR_TRANSITION
    }
}

pub struct RemoteDesktopView {
    backend: Backend,
    /// The tab's profile.
    profile: DesktopProfile,
    /// What the backend dialled: a saved host's current settings.
    dialed: Option<DesktopProfile>,
    title: SharedString,
    active: bool,
    auto_reconnect: bool,
    phase: Phase,
    status_text: Option<String>,
    end_message: Option<String>,
    handle: Option<DesktopHandle>,
    connection: Option<DesktopConnection>,
    generation: u64,
    finished: bool,
    connected_at: Option<Instant>,
    saw_auth_prompt: bool,
    user_disconnect: bool,
    redial: Option<Redial>,
    redial_attempts: usize,
    prompts: Vec<DesktopPrompt>,
    /// Connect once the pane has been measured (RDP asks for a desktop that size).
    connect_when_measured: bool,
    focus: FocusHandle,
    picture: Picture,
    cursor: DesktopCursor,
    /// The pane, as last laid out.
    viewport: Bounds<Pixels>,
    /// Where the desktop is drawn, in window coordinates.
    view_rect: ViewRect,
    keys: KeyTranslator,
    pressed: Vec<MouseButton>,
    /// The pointer is over the pane.
    pointer_inside: bool,
    toolbar: Toolbar,
    _events: Option<Task<()>>,
    _redial: Option<Task<()>>,
    _mount: Option<Task<()>>,
    _resize: Option<Task<()>>,
    _toolbar: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RemoteDesktopViewEvent> for RemoteDesktopView {}

impl Focusable for RemoteDesktopView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Open the view for an RDP or VNC host. With `connect`, the session starts right away
/// (`tab.connectOnMount`); otherwise the view waits for Connect.
pub fn open_remote_desktop_view(
    backend: Backend,
    profile: DesktopProfile,
    title: impl Into<SharedString>,
    connect: bool,
    window: &mut Window,
    cx: &mut App,
) -> Entity<RemoteDesktopView> {
    let title = title.into();
    cx.new(|cx| RemoteDesktopView::new(backend, profile, title, connect, window, cx))
}

fn host_port(profile: &DesktopProfile) -> (&str, u16) {
    match profile {
        DesktopProfile::Rdp(p) => (&p.host, p.port),
        DesktopProfile::Vnc(p) => (&p.host, p.port),
    }
}

fn mouse_button(button: GpuiButton) -> MouseButton {
    match button {
        GpuiButton::Left => MouseButton::Left,
        GpuiButton::Right => MouseButton::Right,
        GpuiButton::Middle => MouseButton::Middle,
        GpuiButton::Navigate(NavigationDirection::Back) => MouseButton::Back,
        GpuiButton::Navigate(NavigationDirection::Forward) => MouseButton::Forward,
    }
}

/// Where the desktop is drawn inside a pane: RDP centres a whole-pixel box (`fitRect`); noVNC
/// scales its canvas by the exact ratio and centres it, fractions included
/// (`Display.autoscale`).
pub fn desktop_box(
    rdp: bool,
    width: f64,
    height: f64,
    desktop_width: f64,
    desktop_height: f64,
) -> ViewRect {
    if rdp {
        return fit_rect(width, height, desktop_width, desktop_height);
    }
    if desktop_width <= 0.0 || desktop_height <= 0.0 || width <= 0.0 || height <= 0.0 {
        return ViewRect {
            left: 0.0,
            top: 0.0,
            width,
            height,
        };
    }
    let scale = if desktop_width / desktop_height >= width / height {
        width / desktop_width
    } else {
        height / desktop_height
    };
    let (scaled_width, scaled_height) = (desktop_width * scale, desktop_height * scale);
    ViewRect {
        left: (width - scaled_width) / 2.0,
        top: (height - scaled_height) / 2.0,
        width: scaled_width,
        height: scaled_height,
    }
}

/// A pointer position on the desktop: IronRDP rounds (`desktopPoint`), noVNC truncates.
pub fn desktop_position(
    rdp: bool,
    view: ViewRect,
    width: u16,
    height: u16,
    x: f64,
    y: f64,
) -> (u16, u16) {
    if rdp {
        return desktop_point(view, width, height, x, y);
    }
    let view = muxus_session::desktop::vnc::ViewRect {
        left: view.left,
        top: view.top,
        width: view.width,
        height: view.height,
    };
    muxus_session::desktop::vnc::desktop_point(view, width, height, x, y)
}

/// A GPUI wheel event in DOM terms: `deltaY` grows toward the user, in pixels.
pub fn wheel_delta(delta: ScrollDelta) -> WheelDelta {
    let (x, y) = match delta {
        ScrollDelta::Pixels(delta) => {
            (f64::from(f32::from(delta.x)), f64::from(f32::from(delta.y)))
        }
        ScrollDelta::Lines(lines) => (
            f64::from(lines.x) * PIXELS_PER_LINE,
            f64::from(lines.y) * PIXELS_PER_LINE,
        ),
    };
    // GPUI counts scrolling toward the user as negative, the DOM as positive.
    WheelDelta {
        delta_x: -x,
        delta_y: -y,
        unit: WheelUnit::Pixel,
    }
}

impl RemoteDesktopView {
    fn new(
        backend: Backend,
        profile: DesktopProfile,
        title: SharedString,
        connect: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus(&focus, window, |this, window, cx| {
                this.keys.sync(window.modifiers(), window.capslock().on);
                this.push_clipboard(cx);
            }),
            cx.on_blur(&focus, window, |this, _, _| this.release_all()),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    if this.active && this.phase == Phase::Connected {
                        this.push_clipboard(cx);
                    }
                } else {
                    this.release_all();
                }
            }),
            cx.on_release(|this, cx| {
                for image in this.picture.clear() {
                    cx.drop_image(image, None);
                }
            }),
        ];
        let mut view = Self {
            backend,
            profile,
            dialed: None,
            title,
            active: true,
            auto_reconnect: true,
            phase: Phase::Idle,
            status_text: None,
            end_message: None,
            handle: None,
            connection: None,
            generation: 0,
            finished: false,
            connected_at: None,
            saw_auth_prompt: false,
            user_disconnect: false,
            redial: None,
            redial_attempts: 0,
            prompts: Vec::new(),
            connect_when_measured: false,
            focus,
            picture: Picture::default(),
            cursor: DesktopCursor::Default,
            viewport: Bounds::default(),
            view_rect: ViewRect::default(),
            keys: KeyTranslator::new(),
            pressed: Vec::new(),
            pointer_inside: false,
            toolbar: Toolbar {
                visible: false,
                changed: Instant::now() - TOOLBAR_TRANSITION,
                pinned: false,
            },
            _events: None,
            _redial: None,
            _mount: None,
            _resize: None,
            _toolbar: None,
            _subscriptions: subscriptions,
        };
        if connect {
            // The tab's first frame measures the pane; RDP asks for a desktop that size. A tab
            // that is not drawn (a background tab) connects anyway, at the fallback size.
            view.phase = Phase::Connecting;
            view.connect_when_measured = true;
            view._mount = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(UNMEASURED_CONNECT).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.connect_when_measured && this.handle.is_none() {
                        this.connect(window, cx);
                    }
                });
            }));
        }
        view
    }

    // --- public API ------------------------------------------------------------------------

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The profile the session dials: a saved host's current settings once known.
    pub fn current_profile(&self) -> &DesktopProfile {
        self.dialed.as_ref().unwrap_or(&self.profile)
    }

    /// The pointer the server last asked for. GPUI 0.3.9 can neither show an image cursor
    /// nor hide the pointer, so the view keeps the system arrow over the desktop.
    pub fn cursor(&self) -> &DesktopCursor {
        &self.cursor
    }

    pub fn title(&self) -> &SharedString {
        &self.title
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

    /// Whether this tab is the one shown in its pane. A hidden desktop lets go of every key;
    /// a shown, connected one takes focus and the local clipboard.
    pub fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        if !active {
            self.release_all();
        } else if self.phase == Phase::Connected {
            self.focus_desktop(window, cx);
        }
        cx.notify();
    }

    /// The open prompts, oldest first.
    pub fn prompts(&self) -> &[DesktopPrompt] {
        &self.prompts
    }

    /// Connect, or reconnect (the Connect/Reconnect buttons and the tab's Reconnect action).
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
            connection.shutdown();
        }
        match &self.handle {
            Some(handle) => handle.close(),
            // Waiting to connect: there is nothing to close but the wait.
            None if self.connect_when_measured => {
                self.connect_when_measured = false;
                self.finish(
                    "The connection closed.".into(),
                    ExitReason::Failed,
                    None,
                    cx,
                );
            }
            None => {}
        }
        cx.notify();
    }

    /// Answer the auth prompt; `None` dismisses it, which closes the session.
    pub fn answer_auth(&mut self, response: Option<AuthPromptResponse>, cx: &mut Context<Self>) {
        self.take_prompt(|prompt| matches!(prompt, DesktopPrompt::Auth(_)));
        if let Some(handle) = &self.handle {
            handle.answer_auth(response);
        }
        cx.notify();
    }

    pub fn answer_host_key(&mut self, accept: bool, cx: &mut Context<Self>) {
        self.take_prompt(|prompt| matches!(prompt, DesktopPrompt::HostKey(_)));
        if let Some(handle) = &self.handle {
            handle.answer_host_key(accept);
        }
        cx.notify();
    }

    pub fn answer_certificate(&mut self, accept: bool, cx: &mut Context<Self>) {
        self.take_prompt(|prompt| matches!(prompt, DesktopPrompt::Certificate(_)));
        // Declining is the user's decision; redialling would only ask again.
        if !accept {
            self.user_disconnect = true;
        }
        if let Some(handle) = &self.handle {
            handle.answer_certificate(accept);
        }
        cx.notify();
    }

    /// Keep the toolbar in view (documentation screenshots).
    pub fn pin_toolbar(&mut self, pinned: bool, cx: &mut Context<Self>) {
        self.toolbar.pinned = pinned;
        self.set_toolbar(pinned, cx);
    }

    // --- connection lifecycle --------------------------------------------------------------

    fn take_prompt(&mut self, which: impl Fn(&DesktopPrompt) -> bool) {
        if let Some(index) = self.prompts.iter().position(which) {
            self.prompts.remove(index);
        }
    }

    fn emit_status(
        &self,
        status: TabStatus,
        failure_reason: Option<String>,
        disconnect_reason: Option<ExitReason>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(RemoteDesktopViewEvent::Status {
            status,
            failure_reason,
            disconnect_reason,
        });
    }

    /// Start a fresh session (`generation + 1`).
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.handle.take() {
            handle.close();
        }
        self.teardown(window, cx);
        self.generation += 1;
        let generation = self.generation;
        self.connect_when_measured = false;
        self.finished = false;
        self.connected_at = None;
        self.saw_auth_prompt = false;
        self.user_disconnect = false;
        self.phase = Phase::Connecting;
        self.status_text = None;
        self.end_message = None;
        // A pending redial (possibly the caller) becomes a no-op.
        self.redial = None;
        self.emit_status(TabStatus::Connecting, None, None, cx);

        let size = self.viewport.size;
        let (width, height) = if f32::from(size.width) >= 200. && f32::from(size.height) >= 200. {
            (
                f64::from(f32::from(size.width)),
                f64::from(f32::from(size.height)),
            )
        } else {
            (
                f64::from(FALLBACK_DESKTOP_SIZE.0),
                f64::from(FALLBACK_DESKTOP_SIZE.1),
            )
        };
        let handle = self.backend.open_desktop_session(
            self.profile.clone(),
            DesktopOptions {
                width,
                height,
                pixel_order: PixelOrder::Bgra,
            },
        );
        let events = handle.events();
        self.handle = Some(handle);
        self._events = Some(cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let keep = this
                    .update_in(cx, |this, window, cx| {
                        if this.generation == generation {
                            this.on_event(event, window, cx);
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

    /// Drop the picture and the connection of the last session.
    fn teardown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connection = None;
        self.pressed.clear();
        self.cursor = DesktopCursor::Default;
        for image in self.picture.clear() {
            cx.drop_image(image, Some(window));
        }
    }

    fn on_event(&mut self, event: DesktopEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            DesktopEvent::Status { message, transient } => {
                // Once the desktop is up the overlay is gone; lasting notes become toasts.
                if self.connected_at.is_some() && !self.finished && !transient {
                    cx.emit(RemoteDesktopViewEvent::Toast {
                        status: Status::Info,
                        message,
                    });
                } else {
                    self.status_text = Some(message);
                }
            }
            DesktopEvent::AuthPrompt(info) => {
                self.saw_auth_prompt = true;
                self.prompt(DesktopPrompt::Auth(info), cx);
            }
            DesktopEvent::HostKey(challenge) => self.prompt(DesktopPrompt::HostKey(challenge), cx),
            DesktopEvent::Certificate(challenge) => {
                self.prompt(DesktopPrompt::Certificate(challenge), cx);
            }
            DesktopEvent::Dialing(profile) => self.dialed = Some(profile),
            DesktopEvent::Connected(connection) => {
                let (width, height) = connection.desktop_size();
                self.picture.resize(u32::from(width), u32::from(height));
                self.connection = Some(connection);
                self.connected_at = Some(Instant::now());
                self.phase = Phase::Connected;
                self.status_text = None;
                self.emit_status(TabStatus::Connected, None, None, cx);
                if self.active {
                    self.focus_desktop(window, cx);
                }
                self.show_toolbar_briefly(cx);
                self.schedule_resize(cx);
            }
            // The old tiles leave the atlas with the next frame, after the one that drew them.
            DesktopEvent::Resized { width, height } => {
                self.picture.resize(u32::from(width), u32::from(height));
            }
            DesktopEvent::Frame => self.draw_frame(window, cx),
            DesktopEvent::Cursor(cursor) => self.cursor = cursor,
            // A local pointer cannot be moved for the user; the browser ignored it too.
            DesktopEvent::PointerPosition { .. } => {}
            DesktopEvent::RemoteClipboard(text) => {
                if self.share_clipboard() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            DesktopEvent::DesktopName(_) | DesktopEvent::Bell => {}
            DesktopEvent::Exit { message, reason } => {
                self.handle = None;
                self.finish(message, reason, Some(window), cx);
                self.teardown(window, cx);
            }
        }
        cx.notify();
    }

    fn prompt(&mut self, prompt: DesktopPrompt, cx: &mut Context<Self>) {
        self.prompts.push(prompt.clone());
        cx.emit(RemoteDesktopViewEvent::Prompt(prompt));
    }

    /// The session ended (`finish`).
    fn finish(
        &mut self,
        message: String,
        reason: ExitReason,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.prompts.is_empty() {
            self.prompts.clear();
            cx.emit(RemoteDesktopViewEvent::PromptsClosed);
        }
        self.phase = Phase::Ended;
        self.end_message = Some(message.clone());
        // A drop after a stable stretch starts a fresh redial chain.
        if let Some(at) = self.connected_at
            && at.elapsed() >= Duration::from_millis(AUTO_RECONNECT_STABLE_MS)
        {
            self.redial_attempts = 0;
        }
        let delay = auto_reconnect_delay_ms(AutoReconnectInput {
            enabled: self.auto_reconnect && !self.user_disconnect,
            profile_kind: match self.profile {
                DesktopProfile::Rdp(_) => ProfileKind::Rdp,
                DesktopProfile::Vnc(_) => ProfileKind::Vnc,
            },
            reason,
            attempts: self.redial_attempts,
            saw_auth_prompt: self.saw_auth_prompt,
        });
        if let Some(delay_ms) = delay
            && let Some(window) = window
        {
            self.redial_attempts += 1;
            self.schedule_redial(delay_ms, window, cx);
        }
        self.emit_status(
            TabStatus::Closed,
            (reason != ExitReason::Completed).then_some(message),
            Some(reason),
            cx,
        );
    }

    fn schedule_redial(&mut self, delay_ms: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.redial = Some(Redial {
            delay_ms,
            attempt: self.redial_attempts,
        });
        let generation = self.generation;
        self._redial = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(delay_ms))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation == generation && this.redial.take().is_some() {
                    if this.auto_reconnect {
                        this.connect(window, cx);
                    }
                    cx.notify();
                }
            });
        }));
    }

    // --- the picture -----------------------------------------------------------------------

    /// Images replaced by an earlier update leave the atlas once a newer frame drew over them.
    fn retire_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for image in self.picture.take_retired() {
            cx.drop_image(image, Some(window));
        }
    }

    fn draw_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connection.is_none() {
            return;
        }
        self.retire_images(window, cx);
        let Some(connection) = &self.connection else {
            return;
        };
        let dirty = connection.take_dirty();
        if dirty.is_empty() {
            return;
        }
        let picture = &mut self.picture;
        connection.read_pixels(|frame| picture.update(frame, &dirty));
    }

    /// The pane changed size: after a settle, RDP asks the server for a matching desktop and
    /// VNC does when remote resizing is on.
    fn schedule_resize(&mut self, cx: &mut Context<Self>) {
        let size = self.viewport.size;
        let (width, height) = (f32::from(size.width), f32::from(size.height));
        if self.phase != Phase::Connected || width < 200. || height < 200. {
            return;
        }
        self._resize = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(RESIZE_SETTLE_MS))
                .await;
            let _ = this.update(cx, |this, _| {
                if let Some(connection) = &this.connection {
                    connection.resize(f64::from(width), f64::from(height));
                }
            });
        }));
    }

    /// The pane as laid out this frame.
    fn measured(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let resized = bounds.size != self.viewport.size;
        self.viewport = bounds;
        if self.connect_when_measured {
            self.connect(window, cx);
        } else if resized {
            self.schedule_resize(cx);
        }
    }

    // --- input -----------------------------------------------------------------------------

    fn share_clipboard(&self) -> bool {
        let share = match self.current_profile() {
            DesktopProfile::Rdp(p) => p.share_clipboard,
            DesktopProfile::Vnc(p) => p.share_clipboard,
        };
        share != Some(false)
    }

    fn view_only(&self) -> bool {
        matches!(self.current_profile(), DesktopProfile::Vnc(p) if p.view_only == Some(true))
    }

    /// Offer the local clipboard to the remote side when the desktop takes focus.
    fn push_clipboard(&self, cx: &mut Context<Self>) {
        if !self.share_clipboard() {
            return;
        }
        let Some(connection) = &self.connection else {
            return;
        };
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            connection.send_clipboard_text(&text);
        }
    }

    fn focus_desktop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus.is_focused(window) {
            self.push_clipboard(cx);
        } else {
            window.focus(&self.focus, cx);
        }
    }

    fn release_all(&mut self) {
        self.pressed.clear();
        if let Some(connection) = &self.connection {
            connection.release_all();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = &self.connection else {
            return;
        };
        let key = self.keys.key_down(
            &event.keystroke,
            event.is_held,
            window.modifiers(),
            window.capslock().on,
        );
        if connection.key(&key) {
            cx.stop_propagation();
        }
    }

    fn on_key_up(&mut self, event: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = &self.connection else {
            return;
        };
        let key = self
            .keys
            .key_up(&event.keystroke, window.modifiers(), window.capslock().on);
        if connection.key(&key) {
            cx.stop_propagation();
        }
    }

    fn on_modifiers_changed(&mut self, event: &ModifiersChangedEvent) {
        let events = self
            .keys
            .modifiers_changed(event.modifiers, event.capslock.on);
        if let Some(connection) = &self.connection {
            for key in &events {
                connection.key(key);
            }
        }
    }

    fn desktop_position(&self, position: gpui_point::Point) -> Option<(u16, u16)> {
        let connection = self.connection.as_ref()?;
        let (width, height) = connection.desktop_size();
        let (x, y) = (
            f64::from(f32::from(position.x)),
            f64::from(f32::from(position.y)),
        );
        Some(desktop_position(
            connection.is_rdp(),
            self.view_rect,
            width,
            height,
            x,
            y,
        ))
    }

    fn inside_desktop(&self, position: gpui_point::Point) -> bool {
        let (x, y) = (
            f64::from(f32::from(position.x)),
            f64::from(f32::from(position.y)),
        );
        let view = self.view_rect;
        x >= view.left && y >= view.top && x < view.left + view.width && y < view.top + view.height
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        hovered: bool,
        inside: bool,
        cx: &mut Context<Self>,
    ) {
        // Leaving the pane hides the toolbar (`onMouseLeave` of the root box; its children,
        // the toolbar and the strip above the desktop, count as inside).
        if !inside && std::mem::replace(&mut self.pointer_inside, false) {
            self.set_toolbar(false, cx);
        }
        self.pointer_inside = inside;
        if !hovered && self.pressed.is_empty() {
            return;
        }
        if self.view_only() {
            return;
        }
        if let (Some(connection), Some((x, y))) =
            (&self.connection, self.desktop_position(event.position))
        {
            connection.pointer_move(x, y);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.connection.is_none() || !self.inside_desktop(event.position) {
            return;
        }
        self.focus_desktop(window, cx);
        if self.view_only() {
            return;
        }
        let button = mouse_button(event.button);
        if let (Some(connection), Some((x, y))) =
            (&self.connection, self.desktop_position(event.position))
        {
            if !self.pressed.contains(&button) {
                self.pressed.push(button);
            }
            connection.pointer_button(x, y, button, true);
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, hovered: bool) {
        let button = mouse_button(event.button);
        // A drag that left the desktop still releases there, instead of sticking.
        let held = self.pressed.contains(&button);
        self.pressed.retain(|pressed| *pressed != button);
        if !(held || hovered && self.inside_desktop(event.position)) || self.view_only() {
            return;
        }
        if let (Some(connection), Some((x, y))) =
            (&self.connection, self.desktop_position(event.position))
        {
            connection.pointer_button(x, y, button, false);
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent) {
        if self.view_only() {
            return;
        }
        if let (Some(connection), Some((x, y))) =
            (&self.connection, self.desktop_position(event.position))
        {
            connection.wheel(x, y, wheel_delta(event.delta));
        }
    }

    // --- the toolbar -----------------------------------------------------------------------

    fn set_toolbar(&mut self, visible: bool, cx: &mut Context<Self>) {
        let visible = visible || self.toolbar.pinned;
        if self.toolbar.visible == visible {
            return;
        }
        let shown = self.toolbar.shown();
        self.toolbar.visible = visible;
        // Start the transition from where it is now.
        let remaining = if visible { shown } else { 1. - shown };
        self.toolbar.changed = Instant::now() - TOOLBAR_TRANSITION.mul_f32(remaining.clamp(0., 1.));
        cx.notify();
    }

    fn show_toolbar_briefly(&mut self, cx: &mut Context<Self>) {
        self.set_toolbar(true, cx);
        let generation = self.generation;
        self._toolbar = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOOLBAR_INTRO).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.set_toolbar(false, cx);
                }
            });
        }));
    }

    fn render_toolbar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let shown = self.toolbar.shown();
        if self.toolbar.animating() {
            window.request_animation_frame();
        }
        if shown <= 0. {
            return None;
        }
        let theme = cx.theme();
        let typo = theme.typography;
        let p = &theme.palette;
        let profile = self.current_profile();
        let (host, port) = host_port(profile);
        let kind = match profile {
            DesktopProfile::Rdp(_) => "RDP",
            DesktopProfile::Vnc(_) => "VNC",
        };
        let rdp = matches!(self.profile, DesktopProfile::Rdp(_));
        // Only a toolbar at rest takes the pointer (`pointerEvents` follows the target state):
        // one sliding in under a still pointer must not count as entered and left.
        let interactive = self.toolbar.visible && !self.toolbar.animating();
        let view = cx.entity();
        let mut paper = div()
            .id("remote-desktop-toolbar")
            .flex()
            .items_center()
            .gap(Theme::spacing(0.25))
            .px(Theme::spacing(0.5))
            .py(Theme::spacing(0.25))
            .rounded_full()
            .bg(p.background_paper)
            .shadow(elevation(4, theme))
            .child(
                div().px(Theme::spacing(1.)).child(
                    label(format!("{kind} · {host}:{port}"))
                        .size(typo.caption.size)
                        .weight(typo.caption.weight)
                        .line_height(typo.caption.line_height)
                        // `color="textSecondary"` is no colour MUI 9 knows, so the caption
                        // inherits the Paper's primary text colour.
                        .color(p.text.primary)
                        .no_truncate(),
                ),
            );
        if !self.view_only() {
            let view = view.clone();
            paper = paper.child(
                IconButton::new("desktop-ctrl-alt-del", "KeyboardOutlined")
                    .tooltip("Send Ctrl+Alt+Del")
                    .on_click(move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            if let Some(connection) = &this.connection {
                                connection.send_ctrl_alt_del();
                            }
                            this.focus_desktop(window, cx);
                        });
                    }),
            );
        }
        if rdp {
            let view = view.clone();
            paper = paper.child(
                IconButton::new("desktop-windows-key", "WindowOutlined")
                    .tooltip("Send the Windows key")
                    .on_click(move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            if let Some(connection) = &this.connection {
                                connection.send_windows_key();
                            }
                            this.focus_desktop(window, cx);
                        });
                    }),
            );
        }
        let reconnect = view.clone();
        let disconnect = view.clone();
        paper = paper
            .child(
                IconButton::new("desktop-reconnect", "Refresh")
                    .tooltip("Reconnect")
                    .on_click(move |_, window, cx| {
                        reconnect.update(cx, |this, cx| this.reconnect(window, cx));
                    }),
            )
            .child(
                IconButton::new("desktop-disconnect", "LinkOffOutlined")
                    .tooltip("Disconnect")
                    .on_click(move |_, _, cx| {
                        disconnect.update(cx, |this, cx| this.disconnect(cx));
                    }),
            );
        if interactive {
            let view = view.clone();
            paper = paper.occlude().on_hover(move |hovered, _, cx| {
                view.update(cx, |this, cx| this.set_toolbar(*hovered, cx));
            });
        }
        let offset = (1. - shown) * (TOOLBAR_HEIGHT + 8.);
        Some(
            div()
                .absolute()
                .top(px(TOOLBAR_TOP - offset))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .opacity(shown)
                .child(paper)
                .into_any_element(),
        )
    }

    // --- the screens -----------------------------------------------------------------------

    fn render_overlay(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let typo = theme.typography;
        let ink = theme.components.remote_desktop_ink;
        let profile = self.current_profile();
        let (host, port) = host_port(profile);
        let address = format!("{host}:{port}");
        let kind = match profile {
            DesktopProfile::Rdp(_) => "RDP",
            DesktopProfile::Vnc(_) => "VNC",
        };
        // Body2, wrapped and centred within 520px (`maxWidth: 520`, `textAlign: center`).
        let body = |text: String| {
            div().max_w(px(MESSAGE_MAX_WIDTH)).child(
                paragraph(text)
                    .size(typo.body2.size)
                    .weight(typo.body2.weight)
                    .line_height(typo.body2.line_height)
                    .color(ink)
                    .center(),
            )
        };
        let view = cx.entity();
        let mut stack = div()
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(Theme::spacing(2.))
            .px(Theme::spacing(3.))
            .text_color(ink);
        if self.phase == Phase::Connecting {
            stack = stack
                .child(CircularProgress::new(
                    "desktop-connecting",
                    SPINNER_SIZE,
                    ink,
                ))
                .child(body(
                    self.status_text
                        .clone()
                        .unwrap_or_else(|| format!("Connecting to {address} …")),
                ))
                .child(
                    Button::new("desktop-cancel", "Cancel")
                        .color(ButtonColor::Inherit)
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.disconnect(cx));
                        }),
                );
        } else {
            let text = if self.phase == Phase::Idle {
                format!("{kind} · {address}")
            } else {
                self.end_message
                    .clone()
                    .unwrap_or_else(|| "Disconnected.".into())
            };
            stack = stack
                .child(icon(
                    "DesktopAccessDisabledOutlined",
                    ENDED_ICON_SIZE,
                    alpha(ink, ENDED_ICON_OPACITY),
                ))
                .child(body(text));
            if let Some(redial) = self.redial {
                stack = stack.child(
                    label(format!(
                        "Reconnecting in {} s (attempt {} of {})",
                        (redial.delay_ms as f64 / 1000.).round(),
                        redial.attempt,
                        AUTO_RECONNECT_DELAYS_MS.len()
                    ))
                    .size(typo.caption.size)
                    .weight(typo.caption.weight)
                    .line_height(typo.caption.line_height)
                    .tracking(typo.caption.letter_spacing)
                    .color(alpha(ink, REDIAL_OPACITY))
                    .no_truncate(),
                );
            }
            let label = if self.phase == Phase::Idle {
                "Connect"
            } else if self.redial.is_some() {
                "Reconnect now"
            } else {
                "Reconnect"
            };
            stack = stack.child(
                Button::new("desktop-reconnect-now", label)
                    .contained()
                    .start_icon("Refresh")
                    .on_click(move |_, window, cx| {
                        view.update(cx, |this, cx| this.reconnect(window, cx));
                    }),
            );
        }
        stack.into_any_element()
    }

    /// The desktop: the picture, and the pointer and wheel listeners over it.
    fn render_desktop(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let snapshot = if self.phase == Phase::Connected {
            self.picture.snapshot()
        } else {
            PictureSnapshot::default()
        };
        let desktop = self
            .connection
            .as_ref()
            .map(DesktopConnection::desktop_size)
            .unwrap_or_default();
        let rdp = matches!(self.profile, DesktopProfile::Rdp(_));
        let measured_view = view.clone();
        canvas(
            move |bounds, window, cx| {
                let size = bounds.size;
                let fitted = desktop_box(
                    rdp,
                    f64::from(f32::from(size.width)),
                    f64::from(f32::from(size.height)),
                    f64::from(desktop.0),
                    f64::from(desktop.1),
                );
                let view_rect = ViewRect {
                    left: fitted.left + f64::from(f32::from(bounds.origin.x)),
                    top: fitted.top + f64::from(f32::from(bounds.origin.y)),
                    ..fitted
                };
                measured_view.update(cx, |this, _| this.view_rect = view_rect);
                if size != measured_view.read(cx).viewport.size
                    || measured_view.read(cx).connect_when_measured
                {
                    let view = measured_view.clone();
                    window.defer(cx, move |window, cx| {
                        view.update(cx, |this, cx| this.measured(bounds, window, cx));
                    });
                }
                (
                    window.insert_hitbox(bounds, HitboxBehavior::Normal),
                    view_rect,
                )
            },
            move |pane, (hitbox, view_rect), window, _| {
                snapshot.paint(view_rect, window);
                window.set_cursor_style(CursorStyle::Arrow, &hitbox);
                let target = view.clone();
                let over = hitbox.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        let hovered = over.is_hovered(window);
                        let inside = pane.contains(&event.position);
                        target.update(cx, |this, cx| {
                            this.on_mouse_move(event, hovered, inside, cx)
                        });
                    }
                });
                let target = view.clone();
                let over = hitbox.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble && over.is_hovered(window) {
                        target.update(cx, |this, cx| this.on_mouse_down(event, window, cx));
                    }
                });
                let target = view.clone();
                let over = hitbox.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        let hovered = over.is_hovered(window);
                        target.update(cx, |this, _| this.on_mouse_up(event, hovered));
                    }
                });
                let target = view.clone();
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                        target.update(cx, |this, _| this.on_scroll(event));
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }
}

/// Window coordinates of a pointer event.
mod gpui_point {
    pub type Point = muxus_ui::gpui_kit::Point<muxus_ui::gpui_kit::Pixels>;
}

impl Render for RemoteDesktopView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = self.phase == Phase::Connected;
        let mut root = div()
            .id("remote-desktop")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().components.remote_desktop_bg)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, _| {
                this.on_modifiers_changed(event);
            }))
            .child(self.render_desktop(cx));
        if connected {
            // A strip along the top edge brings the toolbar back.
            root = root.child(
                div()
                    .id("remote-desktop-toolbar-strip")
                    .absolute()
                    .top_0()
                    .left(relative(0.3))
                    .right(relative(0.3))
                    .h(px(TOOLBAR_STRIP))
                    .occlude()
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.set_toolbar(true, cx);
                        }
                    })),
            );
            if let Some(toolbar) = self.render_toolbar(window, cx) {
                root = root.child(toolbar);
            }
        } else {
            root = root.child(self.render_overlay(cx));
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use muxus_ui::gpui_kit::point;

    use super::*;

    #[test]
    fn turns_gpui_wheel_events_into_dom_deltas() {
        // One notch toward the user: GPUI reports three lines up (negative).
        let notch = wheel_delta(ScrollDelta::Lines(point(0., -3.)));
        assert_eq!(notch.delta_y, 120.);
        assert_eq!(notch.delta_x, 0.);
        assert_eq!(notch.unit, WheelUnit::Pixel);
        let touchpad = wheel_delta(ScrollDelta::Pixels(point(px(4.), px(10.))));
        assert_eq!((touchpad.delta_x, touchpad.delta_y), (-4., -10.));
    }

    #[test]
    fn fits_the_desktop_like_each_client() {
        // The documentation's VNC desktop in its pane: noVNC scales by the exact ratio.
        let vnc = desktop_box(false, 1170., 787., 1132., 786.);
        assert!((vnc.width - 1133.44).abs() < 0.01, "{vnc:?}");
        assert!((vnc.left - 18.28).abs() < 0.01, "{vnc:?}");
        assert_eq!((vnc.top, vnc.height), (0., 787.));
        // RDP draws a whole-pixel box.
        let rdp = desktop_box(true, 1170., 787., 1132., 786.);
        assert_eq!(
            (rdp.left, rdp.top, rdp.width, rdp.height),
            (18., 0., 1133., 787.)
        );
        assert_eq!(
            desktop_position(false, vnc, 1132, 786, vnc.left + 10.9, 5.9),
            (10, 5)
        );
        assert_eq!(
            desktop_position(true, rdp, 1132, 786, 18. + 10.6, 5.6),
            (11, 6)
        );
        // Outside the box clamps to the edge.
        assert_eq!(desktop_position(true, rdp, 1132, 786, 0., 9999.), (0, 785));
    }

    #[test]
    fn maps_every_mouse_button() {
        assert_eq!(mouse_button(GpuiButton::Left), MouseButton::Left);
        assert_eq!(mouse_button(GpuiButton::Middle), MouseButton::Middle);
        assert_eq!(mouse_button(GpuiButton::Right), MouseButton::Right);
        assert_eq!(
            mouse_button(GpuiButton::Navigate(NavigationDirection::Back)),
            MouseButton::Back
        );
        assert_eq!(
            mouse_button(GpuiButton::Navigate(NavigationDirection::Forward)),
            MouseButton::Forward
        );
    }

    #[test]
    fn eases_the_toolbar_in_and_out() {
        let shown = Toolbar {
            visible: true,
            changed: Instant::now() - TOOLBAR_TRANSITION,
            pinned: false,
        };
        assert_eq!(shown.shown(), 1.);
        assert!(!shown.animating());
        let hidden = Toolbar {
            visible: false,
            ..shown
        };
        assert_eq!(hidden.shown(), 0.);
        let starting = Toolbar {
            visible: true,
            changed: Instant::now(),
            pinned: false,
        };
        assert!(starting.shown() < 0.5);
        assert!(starting.animating());
    }
}
