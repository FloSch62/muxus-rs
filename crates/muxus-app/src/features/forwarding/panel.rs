//! `components/ForwardingPanel.tsx`: the global port forwarding side panel.
//! Saved tunnels start and stop independently of terminals (each running
//! tunnel holds its own transport lease); below them, the live forwards of
//! every open SSH connection, which can be saved as a tunnel or into the
//! host's ssh config.
//!
//! The window mounts the panel on the right of its canvas while forwarding is
//! toggled on and drops it on [`ForwardingPanelEvent::Close`]. Sign-in
//! questions of a tunnel's dial arrive as [`ForwardingPanelEvent::Prompt`];
//! the window shows its auth or host-key dialog and sends the answer back on
//! the event's channel (a dropped channel cancels the dial).

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use muxus_core::api::{
    ConnectionInfo, ForwardInfo, ForwardOrigin, ForwardStatus, ForwardType, SshHostEntry,
    TunnelRecord,
};
use muxus_session::Backend;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, FocusHandle, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, div, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::label::{Label, label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts, layout, sizes, weight};
use muxus_ui::widgets::dialog::{
    DialogWidth, dialog_actions, dialog_content, dialog_paper, dialog_title,
};
use muxus_ui::widgets::display::divider;
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::layer::{DIALOG_LAYER, modal_layer, window_layer};
use muxus_ui::widgets::menu::{self, MenuItem, menu_divider};
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::toggle_button::{ToggleButton, ToggleButtonGroup};
use muxus_ui::widgets::tooltip::{AnchorBounds, Tooltip};
use muxus_ui::widgets::{Button, ButtonColor, Chip, IconButton};

use super::model::{
    ADHOC_CAPTION, NO_CONNECTIONS, NO_TUNNELS, SAVED_AS_TUNNEL, SAVED_TO_CONFIG, TunnelAction,
    TunnelDot, TunnelStatusLine, TunnelSwitch, adhoc_forward_request, bookmark_request,
    connection_address, delete_confirmation, forward_flag, host_entry_for, needs_start,
    running_by_tunnel, save_as_tunnel_input, session_forward_text, session_forwards_by_conn,
    stop_forward_tooltip, toggled_input, tunnel_action, tunnel_dot, tunnel_rule_line,
    tunnel_status_line, tunnel_title, tunnel_up_message,
};
use super::start::{DialAnswer, DialPrompt, start_tunnel};
use crate::features::host_editor::dialogs::tunnel::{
    TunnelEditor, TunnelEditorEvent, open_tunnel_editor,
};
use crate::features::host_editor::logic::forwards::ForwardRuleDraft;
use crate::features::host_editor::logic::js::digits_only;
use crate::features::host_editor::logic::tunnel::TunnelEditorState;
use crate::features::host_editor::widgets::visuals::diagram;
use crate::features::launcher::kit::{show_error_toast, show_toast};
use crate::features::sftp::dialogs::{ConfirmOptions, DialogQueue};

/// How often the live lists are read again (`refetchInterval`).
const POLL: Duration = Duration::from_secs(5);
/// ssh_config is re-read less often (`useSshConfig`).
const CONFIG_POLL: Duration = Duration::from_secs(15);

/// What the panel tells its window.
pub enum ForwardingPanelEvent {
    /// The close button (`setForwardingOpen(false)`).
    Close,
    /// A tunnel's dial asks a sign-in question; answer on `reply`.
    Prompt {
        prompt: DialPrompt,
        reply: async_channel::Sender<DialAnswer>,
    },
}

/// The "Forward on <target>" dialog's state.
struct AdhocForward {
    conn: ConnectionInfo,
    rule: ForwardRuleDraft,
    bind: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    busy: bool,
    /// The dialog takes the keyboard (no field has `autoFocus`), for Escape.
    focus: FocusHandle,
    _events: Vec<Subscription>,
}

pub struct ForwardingPanel {
    backend: Backend,
    forwards: Vec<ForwardInfo>,
    connections: Vec<ConnectionInfo>,
    tunnels: Vec<TunnelRecord>,
    hosts: Vec<SshHostEntry>,
    keepalive: Option<f64>,
    /// Tunnel id → progress message while a start (dial + forward) is in flight.
    starting: HashMap<String, String>,
    save_pending: bool,
    menu: Option<TunnelRecord>,
    menu_anchors: HashMap<String, AnchorBounds>,
    editor: Option<(Entity<TunnelEditor>, Subscription)>,
    adhoc: Option<AdhocForward>,
    dialogs: DialogQueue,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<ForwardingPanelEvent> for ForwardingPanel {}

impl ForwardingPanel {
    pub fn new(backend: Backend, cx: &mut Context<Self>) -> Self {
        let mut tasks = Vec::new();
        // Live forward changes as the backend publishes them.
        let mut watch = backend.subscribe_forwards();
        tasks.push(cx.spawn(async move |this, cx| {
            loop {
                let forwards = watch.borrow_and_update().clone();
                if this
                    .update(cx, |this, cx| {
                        if this.forwards != forwards {
                            this.forwards = forwards;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                if watch.changed().await.is_err() {
                    break;
                }
            }
        }));
        let poll_backend = backend.clone();
        tasks.push(cx.spawn(async move |this, cx| {
            loop {
                let connections = poll_backend.connections_list().await;
                let forwards = poll_backend.forwards(None).await;
                let alive = this
                    .update(cx, |this, cx| {
                        if let Ok(response) = connections {
                            this.connections = response.connections;
                        }
                        if let Ok(forwards) = forwards {
                            this.forwards = forwards;
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
                cx.background_executor().timer(POLL).await;
            }
        }));
        let config_backend = backend.clone();
        tasks.push(cx.spawn(async move |this, cx| {
            if let Ok(prefs) = config_backend.prefs().await {
                let _ = this.update(cx, |this, _| {
                    this.keepalive = Some(prefs.ssh_keepalive_interval_seconds);
                });
            }
            loop {
                let config = config_backend.ssh_config().await;
                let alive = this
                    .update(cx, |this, cx| {
                        if let Ok(config) = config {
                            this.hosts = config.hosts;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
                cx.background_executor().timer(CONFIG_POLL).await;
            }
        }));
        let mut panel = Self {
            backend,
            forwards: Vec::new(),
            connections: Vec::new(),
            tunnels: Vec::new(),
            hosts: Vec::new(),
            keepalive: None,
            starting: HashMap::new(),
            save_pending: false,
            menu: None,
            menu_anchors: HashMap::new(),
            editor: None,
            adhoc: None,
            dialogs: DialogQueue::new(),
            _tasks: tasks,
        };
        panel.load_tunnels(cx);
        panel
    }

    fn load_tunnels(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(response) = backend.tunnels().await {
                let _ = this.update(cx, |this, cx| {
                    this.tunnels = response.tunnels;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Read forwards, connections and tunnels again (`invalidate`).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let forwards = backend.forwards(None).await;
            let connections = backend.connections_list().await;
            let tunnels = backend.tunnels().await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(forwards) = forwards {
                    this.forwards = forwards;
                }
                if let Ok(response) = connections {
                    this.connections = response.connections;
                }
                if let Ok(response) = tunnels {
                    this.tunnels = response.tunnels;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn ask(
        &mut self,
        prompt: DialPrompt,
        cx: &mut Context<Self>,
    ) -> async_channel::Receiver<DialAnswer> {
        let (reply, answer) = async_channel::bounded(1);
        cx.emit(ForwardingPanelEvent::Prompt { prompt, reply });
        answer
    }

    /// Start a tunnel: progress in its row, prompts through the window, a
    /// toast when it is up.
    pub fn start(&mut self, tunnel: TunnelRecord, cx: &mut Context<Self>) {
        self.starting
            .insert(tunnel.id.clone(), "Starting\u{2026}".into());
        cx.notify();
        let backend = self.backend.clone();
        let keepalive = self.keepalive;
        cx.spawn(async move |this, cx| {
            let status_this = this.clone();
            let status_cx = cx.clone();
            let ask_this = this.clone();
            let ask_cx = cx.clone();
            let id = tunnel.id.clone();
            let result = start_tunnel(
                &backend,
                &tunnel,
                keepalive,
                |message| {
                    let id = id.clone();
                    let mut cx = status_cx.clone();
                    let _ = status_this.update(&mut cx, |this, cx| {
                        if let Some(progress) = this.starting.get_mut(&id) {
                            *progress = message;
                            cx.notify();
                        }
                    });
                },
                |prompt| {
                    let mut cx = ask_cx.clone();
                    let answer = ask_this
                        .update(&mut cx, |this, cx| this.ask(prompt.clone(), cx))
                        .ok();
                    let is_auth = matches!(prompt, DialPrompt::Auth(_));
                    async move {
                        let cancelled = if is_auth {
                            DialAnswer::Auth(None)
                        } else {
                            DialAnswer::HostKey(false)
                        };
                        match answer {
                            Some(answer) => answer.recv().await.unwrap_or(cancelled),
                            None => cancelled,
                        }
                    }
                },
            )
            .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => show_toast(Status::Success, tunnel_up_message(&tunnel), cx),
                    Err(message) => show_error_toast(message, cx),
                }
                this.starting.remove(&tunnel.id);
                this.refresh(cx);
            });
        })
        .detach();
    }

    fn stop(&mut self, forward_id: String, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.stop_forward(&forward_id).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.refresh(cx),
                Err(error) => show_error_toast(error.message, cx),
            });
        })
        .detach();
    }

    fn toggle_switch(
        &mut self,
        tunnel: TunnelRecord,
        switch: TunnelSwitch,
        cx: &mut Context<Self>,
    ) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.save_tunnel(toggled_input(&tunnel, switch)).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => this.refresh(cx),
                Err(error) => show_error_toast(error.message, cx),
            });
        })
        .detach();
    }

    fn delete(&mut self, tunnel: TunnelRecord, window: &mut Window, cx: &mut Context<Self>) {
        let running = running_by_tunnel(&self.forwards).get(&tunnel.id).cloned();
        let (title, description) = delete_confirmation(&tunnel, running.is_some());
        let answer = self.dialogs.confirm(
            ConfirmOptions {
                title,
                description: Some(description),
                confirm_label: "Delete".into(),
                destructive: true,
                ..ConfirmOptions::default()
            },
            window,
            cx,
        );
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            if !answer.await.confirmed {
                return;
            }
            let result = async {
                if let Some(running) = running {
                    backend.stop_forward(&running.id).await?;
                }
                backend.delete_tunnel(&tunnel.id).await.map(|_| ())
            }
            .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.refresh(cx),
                Err(error) => show_error_toast(error.message, cx),
            });
        })
        .detach();
    }

    fn save_as_tunnel(&mut self, forward: ForwardInfo, cx: &mut Context<Self>) {
        let Some(conn) = self
            .connections
            .iter()
            .find(|conn| conn.id == forward.conn_id)
            .cloned()
        else {
            show_error_toast("connection is gone", cx);
            return;
        };
        self.save_pending = true;
        cx.notify();
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = async {
                let record = backend
                    .save_tunnel(save_as_tunnel_input(&conn, &forward))
                    .await?;
                backend.assign_forward_tunnel(&forward.id, &record.id).await
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.save_pending = false;
                match result {
                    Ok(_) => {
                        show_toast(Status::Success, SAVED_AS_TUNNEL, cx);
                        this.refresh(cx);
                    }
                    Err(error) => show_error_toast(error.message, cx),
                }
            });
        })
        .detach();
    }

    fn bookmark(&mut self, conn: &ConnectionInfo, forward: &ForwardInfo, cx: &mut Context<Self>) {
        let Some(entry) = host_entry_for(&self.hosts, conn) else {
            return;
        };
        let request = bookmark_request(entry, forward);
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.upsert_ssh_host(request).await;
            let config = backend.ssh_config().await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => show_toast(Status::Success, SAVED_TO_CONFIG, cx),
                    Err(error) => show_error_toast(error.message, cx),
                }
                if let Ok(config) = config {
                    this.hosts = config.hosts;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open the tunnel editor for a new tunnel or an existing one.
    pub fn edit_tunnel(
        &mut self,
        tunnel: Option<TunnelRecord>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = open_tunnel_editor(
            window,
            cx,
            self.backend.clone(),
            TunnelEditorState {
                tunnel,
                prefill_target: None,
            },
        );
        let subscription = cx.subscribe(&editor, |this, _, event: &TunnelEditorEvent, cx| {
            match event {
                TunnelEditorEvent::Close => this.editor = None,
                TunnelEditorEvent::Saved { record, start } => {
                    this.editor = None;
                    this.on_tunnel_saved((**record).clone(), *start, cx);
                }
                TunnelEditorEvent::Toast { status, message } => {
                    show_toast(*status, message.clone(), cx)
                }
            }
            cx.notify();
        });
        self.editor = Some((editor, subscription));
        cx.notify();
    }

    /// `onTunnelSaved`: Save & start (or restart a running one).
    fn on_tunnel_saved(&mut self, record: TunnelRecord, start: bool, cx: &mut Context<Self>) {
        self.refresh(cx);
        if !start {
            return;
        }
        let running = running_by_tunnel(&self.forwards).get(&record.id).cloned();
        match running {
            // Save & restart: replace the live forward with the new definition.
            Some(running) => {
                let backend = self.backend.clone();
                cx.spawn(async move |this, cx| {
                    let _ = backend.stop_forward(&running.id).await;
                    let _ = this.update(cx, |this, cx| this.start(record, cx));
                })
                .detach();
            }
            None => self.start(record, cx),
        }
    }

    fn open_adhoc(&mut self, conn: ConnectionInfo, window: &mut Window, cx: &mut Context<Self>) {
        let rule = ForwardRuleDraft::default();
        let bind = cx.new(|cx| InputState::new(window, cx));
        let host = cx.new(|cx| InputState::new(window, cx).default_value(rule.target_host.clone()));
        let port = cx.new(|cx| InputState::new(window, cx));
        let events = vec![
            cx.subscribe_in(&bind, window, |this, input, event, window, cx| {
                this.on_adhoc_input(AdhocField::Bind, input, event, window, cx)
            }),
            cx.subscribe_in(&host, window, |this, input, event, window, cx| {
                this.on_adhoc_input(AdhocField::Host, input, event, window, cx)
            }),
            cx.subscribe_in(&port, window, |this, input, event, window, cx| {
                this.on_adhoc_input(AdhocField::Port, input, event, window, cx)
            }),
        ];
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.adhoc = Some(AdhocForward {
            conn,
            rule,
            bind,
            host,
            port,
            busy: false,
            focus,
            _events: events,
        });
        cx.notify();
    }

    fn on_adhoc_input(
        &mut self,
        field: AdhocField,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let value = input.read(cx).value().to_string();
                let Some(adhoc) = &mut self.adhoc else {
                    return;
                };
                match field {
                    AdhocField::Host => adhoc.rule.target_host = value,
                    AdhocField::Bind | AdhocField::Port => {
                        // Ports take digits only.
                        let digits = digits_only(&value);
                        if digits != value {
                            input.update(cx, |state, cx| {
                                state.set_value(digits.clone(), window, cx)
                            });
                        }
                        if field == AdhocField::Bind {
                            adhoc.rule.bind_port = digits;
                        } else {
                            adhoc.rule.target_port = digits;
                        }
                    }
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } if field != AdhocField::Host => {
                self.submit_adhoc(window, cx)
            }
            _ => {}
        }
    }

    fn submit_adhoc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(adhoc) = &mut self.adhoc else {
            return;
        };
        if adhoc.busy {
            return;
        }
        let Some(rule) = adhoc.rule.rule() else {
            return;
        };
        adhoc.busy = true;
        adhoc.rule.after_submit();
        adhoc
            .bind
            .update(cx, |state, cx| state.set_value("", window, cx));
        adhoc
            .port
            .update(cx, |state, cx| state.set_value("", window, cx));
        let request = adhoc_forward_request(&adhoc.conn.id, &rule);
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.start_forward(request).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        this.adhoc = None;
                        this.refresh(cx);
                    }
                    Err(error) => {
                        if let Some(adhoc) = &mut this.adhoc {
                            adhoc.busy = false;
                        }
                        show_error_toast(error.message, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Open a tunnel's options menu (the documentation scene does this
    /// instead of a click).
    pub fn show_menu(&mut self, tunnel_id: &str, cx: &mut Context<Self>) {
        self.menu = self
            .tunnels
            .iter()
            .find(|tunnel| tunnel.id == tunnel_id)
            .cloned();
        cx.notify();
    }

    /// Open "Forward on <target>" for a live connection.
    pub fn show_adhoc(&mut self, conn_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conn) = self
            .connections
            .iter()
            .find(|conn| conn.id == conn_id)
            .cloned()
        {
            self.open_adhoc(conn, window, cx);
        }
    }

    pub fn tunnels(&self) -> &[TunnelRecord] {
        &self.tunnels
    }

    pub fn forwards(&self) -> &[ForwardInfo] {
        &self.forwards
    }

    /// The tunnel editor on screen.
    pub fn editor(&self) -> Option<Entity<TunnelEditor>> {
        self.editor.as_ref().map(|(editor, _)| editor.clone())
    }

    pub fn connections(&self) -> &[ConnectionInfo] {
        &self.connections
    }

    /// Test and scene access: the lists as the panel shows them.
    pub fn set_lists(
        &mut self,
        tunnels: Vec<TunnelRecord>,
        forwards: Vec<ForwardInfo>,
        connections: Vec<ConnectionInfo>,
        cx: &mut Context<Self>,
    ) {
        self.tunnels = tunnels;
        self.forwards = forwards;
        self.connections = connections;
        cx.notify();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AdhocField {
    Bind,
    Host,
    Port,
}

/// Geometry `ForwardingPanel.tsx` sets in px: the status dots, the flag
/// chips of session forwards and the "config" chip.
const TUNNEL_DOT: f32 = 8.;
const CONNECTION_DOT: f32 = 7.;
const FLAG_CHIP_WIDTH: f32 = 42.;
const FLAG_CHIP_HEIGHT: f32 = 20.;
const CONFIG_CHIP_HEIGHT: f32 = 18.;
const SPINNER_SIZE: f32 = 18.;
/// The connection rows' rule column starts this far in (`pl: 3`).
const NESTED_INSET: f32 = 3.;

/// `SectionLabel`: dense, semibold, upper case, tracked.
fn section_label(theme: &Theme, content: &str) -> impl IntoElement {
    let size = sizes::TEXT_DENSE;
    div().px(Theme::spacing(1.5)).pb(Theme::spacing(0.5)).child(
        label(content.to_uppercase())
            .size(size)
            .weight(weight::SEMIBOLD)
            .line_height(size * 1.5)
            .tracking(sizes::OVERLINE_TRACKING)
            .color(theme.palette.text.secondary),
    )
}

fn dot(size: f32, color: Hsla) -> impl IntoElement {
    div().size(px(size)).flex_none().rounded_full().bg(color)
}

/// `variant="caption"` at the dense size (`fontSize: 11`), optionally monospace.
fn dense_caption(
    content: impl Into<SharedString>,
    mono: bool,
    color: Hsla,
    theme: &Theme,
) -> Label {
    let size = sizes::TEXT_DENSE;
    let line_height = size * theme.typography.caption.line_height / theme.typography.caption.size;
    let text = label(content)
        .size(size)
        .weight(theme.typography.caption.weight)
        .line_height(line_height)
        .color(color);
    if mono { text.family(fonts::MONO) } else { text }
}

/// A small icon with a tooltip (the autostart / reconnect marks).
fn marked_icon(
    id: &str,
    glyph: &'static str,
    size: f32,
    tip: &'static str,
    color: Hsla,
) -> impl IntoElement {
    div()
        .id(ElementId::Name(id.to_string().into()))
        .flex()
        .flex_none()
        .child(icon(glyph, size, color))
        .tooltip(Tooltip::text(tip))
}

impl ForwardingPanel {
    fn render_tunnel(
        &self,
        theme: &Theme,
        tunnel: &TunnelRecord,
        running: Option<&ForwardInfo>,
        any_starting: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let typography = theme.typography;
        let busy = self.starting.get(&tunnel.id);
        let dot_color = match tunnel_dot(running) {
            TunnelDot::Idle => p.text.disabled,
            TunnelDot::Active => theme.status_text_color(Status::Success),
            TunnelDot::Pending => theme.status_text_color(Status::Warning),
            TunnelDot::Error => theme.status_text_color(Status::Error),
        };
        let id = tunnel.id.clone();
        let mut name_row = div()
            .flex()
            .items_center()
            .gap(Theme::spacing(0.5))
            .min_w_0()
            .child(
                label(tunnel_title(tunnel))
                    .variant(typography.body2)
                    .weight(weight::SEMIBOLD)
                    .color(p.text.primary),
            );
        if tunnel.auto_start {
            name_row = name_row.child(marked_icon(
                &format!("tunnel-autostart-{id}"),
                "PowerSettingsNew",
                sizes::ICON_TINY,
                "Starts with Muxus",
                p.text.secondary,
            ));
        }
        if tunnel.auto_reconnect {
            name_row = name_row.child(marked_icon(
                &format!("tunnel-reconnect-{id}"),
                "Autorenew",
                sizes::ICON_COMPACT,
                "Reconnects automatically",
                p.text.secondary,
            ));
        }
        let mut info = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(name_row)
            .child(dense_caption(
                tunnel_rule_line(tunnel, busy.map(String::as_str)),
                true,
                p.text.secondary,
                theme,
            ))
            .child(dense_caption(
                super::model::describe_tunnel_connection(tunnel),
                false,
                p.text.disabled,
                theme,
            ));
        let caption = typography.caption;
        match tunnel_status_line(running) {
            Some(TunnelStatusLine::Starting) => {
                info = info.child(
                    label("Starting\u{2026}")
                        .variant(caption)
                        .color(p.text.secondary),
                );
            }
            Some(TunnelStatusLine::Reconnecting { text, tooltip }) => {
                info = info.child(
                    div()
                        .id(ElementId::Name(format!("tunnel-status-{id}").into()))
                        .child(
                            label(text)
                                .variant(caption)
                                .color(theme.status_text_color(Status::Warning)),
                        )
                        .tooltip(Tooltip::text(tooltip)),
                );
            }
            Some(TunnelStatusLine::Failed(error)) => {
                info = info.child(
                    div()
                        .id(ElementId::Name(format!("tunnel-status-{id}").into()))
                        .child(label(error.clone()).variant(caption).color(p.error.main))
                        .tooltip(Tooltip::text(error)),
                );
            }
            None => {}
        }
        let entity = cx.entity().downgrade();
        let action: AnyElement = match tunnel_action(tunnel, running, busy.is_some(), any_starting)
        {
            TunnelAction::Busy => div()
                .mx(Theme::spacing(0.75))
                .child(CircularProgress::new(
                    SharedString::from(format!("tunnel-spinner-{id}")),
                    SPINNER_SIZE,
                    p.primary.main,
                ))
                .into_any_element(),
            TunnelAction::Stop { tooltip } => {
                let forward_id = running
                    .map(|forward| forward.id.clone())
                    .unwrap_or_default();
                let entity = entity.clone();
                IconButton::new(SharedString::from(format!("tunnel-stop-{id}")), "Stop")
                    .tooltip(tooltip)
                    .on_click(move |_, _, cx| {
                        let forward_id = forward_id.clone();
                        let _ = entity.update(cx, |this, cx| this.stop(forward_id, cx));
                    })
                    .into_any_element()
            }
            TunnelAction::Start { tooltip, disabled } => {
                let tunnel = tunnel.clone();
                let entity = entity.clone();
                IconButton::new(
                    SharedString::from(format!("tunnel-start-{id}")),
                    "PlayArrow",
                )
                .color(ButtonColor::Primary)
                .disabled(disabled)
                .tooltip(tooltip)
                .on_click(move |_, _, cx| {
                    let tunnel = tunnel.clone();
                    let _ = entity.update(cx, |this, cx| this.start(tunnel, cx));
                })
                .into_any_element()
            }
        };
        let group: SharedString = format!("tunnel-row-{id}").into();
        let mut row = div()
            .id(ElementId::Name(group.clone()))
            .group(group.clone())
            .flex()
            .items_center()
            .gap(Theme::spacing(1.))
            .px(Theme::spacing(1.5))
            .py(Theme::spacing(0.75))
            .child(dot(TUNNEL_DOT, dot_color))
            .child(info)
            .child(action);
        if needs_start(running) {
            let forward_id = running
                .map(|forward| forward.id.clone())
                .unwrap_or_default();
            let entity = entity.clone();
            row = row.child(
                IconButton::new(SharedString::from(format!("tunnel-dismiss-{id}")), "Close")
                    .icon_size(sizes::ICON_DENSE)
                    .tooltip("Dismiss")
                    .on_click(move |_, _, cx| {
                        let forward_id = forward_id.clone();
                        let _ = entity.update(cx, |this, cx| this.stop(forward_id, cx));
                    }),
            );
        }
        let anchor = self.menu_anchors.get(&id).cloned().unwrap_or_default();
        let tunnel_for_menu = tunnel.clone();
        let options = IconButton::new(
            SharedString::from(format!("tunnel-options-{id}")),
            "MoreVert",
        )
        .icon_size(sizes::ICON_DENSE)
        .track_bounds(&anchor)
        .on_click(move |_, _, cx| {
            let tunnel = tunnel_for_menu.clone();
            let _ = entity.update(cx, |this, cx| {
                this.menu = Some(tunnel);
                cx.notify();
            });
        });
        // The options button shows while the row is hovered (`opacity: { md: 0 }`).
        let options_slot = div()
            .flex()
            .child(options)
            .opacity(0.)
            .group_hover(group, |s| s.opacity(1.));
        row.child(options_slot).into_any_element()
    }

    fn render_connection(
        &self,
        theme: &Theme,
        conn: &ConnectionInfo,
        forwards: &[ForwardInfo],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let typography = theme.typography;
        let has_host_entry = host_entry_for(&self.hosts, conn).is_some();
        let entity = cx.entity().downgrade();
        let add_conn = conn.clone();
        let add = entity.clone();
        let header = div()
            .flex()
            .items_center()
            .gap(Theme::spacing(0.75))
            .px(Theme::spacing(1.5))
            .child(dot(
                CONNECTION_DOT,
                theme.status_text_color(Status::Success),
            ))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_baseline()
                    .overflow_hidden()
                    .child(
                        label(conn.target.clone())
                            .variant(typography.body2)
                            .weight(weight::SEMIBOLD)
                            .color(p.text.primary)
                            .no_truncate(),
                    )
                    .child(
                        div().ml(Theme::spacing(0.75)).min_w_0().child(
                            label(connection_address(conn))
                                .variant(typography.caption)
                                .color(p.text.secondary),
                        ),
                    ),
            )
            .child(
                IconButton::new(SharedString::from(format!("conn-add-{}", conn.id)), "Add")
                    .icon_size(sizes::ICON_DENSE)
                    .tooltip("Start a forward on this connection")
                    .on_click(move |_, window, cx| {
                        let conn = add_conn.clone();
                        let _ = add.update(cx, |this, cx| this.open_adhoc(conn, window, cx));
                    }),
            );
        let mut group = div()
            .flex()
            .flex_col()
            .pb(Theme::spacing(0.5))
            .child(header);
        for forward in forwards {
            let fid = forward.id.clone();
            let mut flag = Chip::new(forward_flag(forward.kind))
                .mono()
                .width(FLAG_CHIP_WIDTH)
                .height(FLAG_CHIP_HEIGHT)
                .font_size(sizes::TEXT_DENSE);
            if forward.status == ForwardStatus::Error {
                flag = flag.palette(p.error);
            }
            let mut row = div()
                .flex()
                .items_center()
                .gap(Theme::spacing(1.))
                .pl(Theme::spacing(NESTED_INSET))
                .pr(Theme::spacing(1.5))
                .py(Theme::spacing(0.25))
                .child(flag)
                .child(div().flex_1().min_w_0().child(dense_caption(
                    session_forward_text(forward),
                    true,
                    p.text.primary,
                    theme,
                )));
            if forward.origin == ForwardOrigin::Config {
                row = row.child(
                    div()
                        .id(ElementId::Name(format!("forward-config-{fid}").into()))
                        .flex_none()
                        .child(
                            Chip::new("config")
                                .outlined()
                                .height(CONFIG_CHIP_HEIGHT)
                                .font_size(sizes::TEXT_MICRO),
                        )
                        .tooltip(Tooltip::text(
                            "Declared in ssh config — starts and stops with this terminal",
                        )),
                );
            } else {
                let save = entity.clone();
                let save_forward = forward.clone();
                row = row.child(
                    IconButton::new(
                        SharedString::from(format!("forward-save-{fid}")),
                        "SaveOutlined",
                    )
                    .icon_size(sizes::ICON_DENSE)
                    .disabled(self.save_pending)
                    .tooltip("Save as tunnel — start it later without a terminal")
                    .on_click(move |_, _, cx| {
                        let forward = save_forward.clone();
                        let _ = save.update(cx, |this, cx| this.save_as_tunnel(forward, cx));
                    }),
                );
                if has_host_entry {
                    let bookmark = entity.clone();
                    let (bconn, bforward) = (conn.clone(), forward.clone());
                    row = row.child(
                        IconButton::new(
                            SharedString::from(format!("forward-bookmark-{fid}")),
                            "BookmarkAddOutlined",
                        )
                        .icon_size(sizes::ICON_DENSE)
                        .tooltip(format!(
                            "Save to {}'s ssh config — starts on every connect",
                            conn.metadata_alias.clone().unwrap_or_default()
                        ))
                        .on_click(move |_, _, cx| {
                            let _ = bookmark
                                .update(cx, |this, cx| this.bookmark(&bconn, &bforward, cx));
                        }),
                    );
                }
            }
            let stop = entity.clone();
            let stop_id = forward.id.clone();
            row = row.child(
                IconButton::new(SharedString::from(format!("forward-stop-{fid}")), "Stop")
                    .icon_size(sizes::ICON_DENSE)
                    .tooltip(stop_forward_tooltip(forward))
                    .on_click(move |_, _, cx| {
                        let id = stop_id.clone();
                        let _ = stop.update(cx, |this, cx| this.stop(id, cx));
                    }),
            );
            group = group.child(row);
        }
        if forwards.is_empty() {
            group = group.child(
                div().pl(Theme::spacing(NESTED_INSET)).child(
                    label("No session forwards.")
                        .variant(typography.caption)
                        .color(p.text.secondary),
                ),
            );
        }
        group.into_any_element()
    }

    fn render_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tunnel = self.menu.clone()?;
        let anchor = self
            .menu_anchors
            .get(&tunnel.id)
            .cloned()
            .unwrap_or_default();
        let position = menu::below(&anchor)?;
        let p = theme.palette;
        let entity = cx.entity().downgrade();
        let mut content = menu::paper(theme);
        for switch in TunnelSwitch::ALL {
            let on = switch.is_on(&tunnel);
            let (entity, tunnel) = (entity.clone(), tunnel.clone());
            content = content.child(
                MenuItem::new(
                    SharedString::from(format!("tunnel-switch-{switch:?}")),
                    switch.label(),
                )
                .leading(icon(
                    if on {
                        "CheckBox"
                    } else {
                        "CheckBoxOutlineBlank"
                    },
                    SMALL,
                    if on { p.primary.main } else { p.action.active },
                ))
                .on_click(move |_, _, cx| {
                    let tunnel = tunnel.clone();
                    let _ = entity.update(cx, |this, cx| {
                        this.menu = None;
                        this.toggle_switch(tunnel, switch, cx);
                    });
                }),
            );
        }
        content = content.child(menu_divider(theme));
        let (edit, edit_tunnel) = (entity.clone(), tunnel.clone());
        content = content.child(
            MenuItem::new("tunnel-edit", "Edit tunnel")
                .icon("EditOutlined")
                .on_click(move |_, window, cx| {
                    let tunnel = edit_tunnel.clone();
                    let _ = edit.update(cx, |this, cx| {
                        this.menu = None;
                        this.edit_tunnel(Some(tunnel), window, cx);
                    });
                }),
        );
        let (delete, delete_tunnel) = (entity.clone(), tunnel);
        content = content.child(
            MenuItem::new("tunnel-delete", "Delete tunnel")
                .icon("DeleteOutlined")
                .destructive()
                .on_click(move |_, window, cx| {
                    let tunnel = delete_tunnel.clone();
                    let _ = delete.update(cx, |this, cx| {
                        this.menu = None;
                        this.delete(tunnel, window, cx);
                    });
                }),
        );
        let dismiss = entity;
        Some(
            menu::popup(
                position,
                move |_, cx| {
                    let _ = dismiss.update(cx, |this, cx| {
                        this.menu = None;
                        cx.notify();
                    });
                },
                content,
            )
            .into_any_element(),
        )
    }

    /// "Forward on <target>": `ForwardRuleForm` with Start, on the live connection.
    fn render_adhoc(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let adhoc = self.adhoc.as_ref()?;
        let p = theme.palette;
        let typography = theme.typography;
        let kind = adhoc.rule.kind;
        let entity = cx.entity().downgrade();
        let toggle = |id: &'static str, value: ForwardType, content: &'static str| {
            let entity = entity.clone();
            ToggleButton::new(id, content, kind == value).on_click(move |_, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    if let Some(adhoc) = &mut this.adhoc {
                        adhoc.rule.kind = value;
                    }
                    cx.notify();
                });
            })
        };
        let group = ToggleButtonGroup::new(vec![
            toggle("adhoc-local", ForwardType::Local, "Local (-L)"),
            toggle("adhoc-remote", ForwardType::Remote, "Remote (-R)"),
            toggle("adhoc-dynamic", ForwardType::Dynamic, "SOCKS proxy (-D)"),
        ]);
        let rule = &adhoc.rule;
        let mut fields = div().flex().gap(Theme::spacing(1.5)).child(
            FormField::input("adhoc-bind", &adhoc.bind)
                .label(rule.bind_label())
                .width(PORT_FIELD_WIDTH),
        );
        if kind != ForwardType::Dynamic {
            fields = fields
                .child(FormField::input("adhoc-host", &adhoc.host).label("Target host"))
                .child(
                    FormField::input("adhoc-port", &adhoc.port)
                        .label("Target port")
                        .width(PORT_FIELD_WIDTH),
                );
        }
        let submit = entity.clone();
        fields = fields.child(
            div().flex().items_center().child(
                Button::new("adhoc-start", "Start")
                    .outlined()
                    .start_icon("Add")
                    .disabled(!rule.valid() || adhoc.busy)
                    .on_click(move |_, window, cx| {
                        let _ = submit.update(cx, |this, cx| this.submit_adhoc(window, cx));
                    }),
            ),
        );
        let server = adhoc.conn.target.clone();
        let form = div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(2.))
            .mt(Theme::spacing(0.5))
            .child(group)
            .child(diagram(
                theme,
                kind,
                &rule.bind_port,
                &rule.target_host,
                &rule.target_port,
                &server,
            ))
            .child(fields);
        let title = dialog_title(theme)
            .flex_col()
            .items_start()
            .child(
                label(format!("Forward on {}", adhoc.conn.target))
                    .variant(typography.h6)
                    .color(p.text.primary),
            )
            .child(
                label(ADHOC_CAPTION)
                    .variant(typography.caption)
                    .color(p.text.secondary),
            );
        let cancel = entity.clone();
        let width = DialogWidth::Sm.px().min(
            muxus_ui::zoom::unzoom(window.viewport_size().width) - 2. * Theme::spacing_css(4.),
        );
        let paper = dialog_paper(theme)
            .w(px(width))
            .child(title)
            .child(dialog_content(theme, true, false).child(form))
            .child(
                dialog_actions().child(Button::new("adhoc-cancel", "Cancel").on_click(
                    move |_, _, cx| {
                        let _ = cancel.update(cx, |this, cx| {
                            this.adhoc = None;
                            cx.notify();
                        });
                    },
                )),
            );
        let close = entity;
        let escape = close.clone();
        Some(
            div()
                .id("adhoc-forward-keys")
                .track_focus(&adhoc.focus)
                .capture_key_down(move |event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        let _ = escape.update(cx, |this, cx| {
                            this.adhoc = None;
                            cx.notify();
                        });
                    }
                })
                .child(modal_layer(
                    window,
                    "adhoc-forward",
                    paper,
                    Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                        let _ = close.update(cx, |this, cx| {
                            this.adhoc = None;
                            cx.notify();
                        });
                    })),
                ))
                .into_any_element(),
        )
    }
}

/// The port fields' width in the rule form (`sx={{ width: 140 }}`).
const PORT_FIELD_WIDTH: f32 = 140.;

impl Render for ForwardingPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let typography = theme.typography;
        let running = running_by_tunnel(&self.forwards);
        let by_conn = session_forwards_by_conn(&self.forwards, &self.tunnels);
        let any_starting = !self.starting.is_empty();
        for tunnel in &self.tunnels {
            self.menu_anchors.entry(tunnel.id.clone()).or_default();
        }

        let entity = cx.entity().downgrade();
        let (add, close) = (entity.clone(), entity.clone());
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::spacing(1.))
            .px(Theme::spacing(1.5))
            .py(Theme::spacing(1.))
            .border_b_1()
            .border_color(p.divider)
            .child(icon("SwapHorizOutlined", SMALL, p.primary.main))
            .child(
                div().flex_1().min_w_0().child(
                    label("Port forwarding")
                        .variant(typography.subtitle2)
                        .weight(weight::BOLD)
                        .color(p.text.primary),
                ),
            )
            .child(
                Button::new("forwarding-add-tunnel", "Tunnel")
                    .contained()
                    .start_icon("Add")
                    .on_click(move |_, window, cx| {
                        let _ = add.update(cx, |this, cx| this.edit_tunnel(None, window, cx));
                    }),
            )
            .child(
                IconButton::new("forwarding-close", "Close")
                    .tooltip("Close forwarding panel")
                    .on_click(move |_, _, cx| {
                        let _ = close.update(cx, |_, cx| cx.emit(ForwardingPanelEvent::Close));
                    }),
            );

        let empty_note = |text: &'static str| {
            div().px(Theme::spacing(1.5)).pb(Theme::spacing(1.)).child(
                paragraph(text)
                    .variant(typography.body2)
                    .color(p.text.secondary),
            )
        };
        let mut body = div()
            .id("forwarding-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .py(Theme::spacing(1.))
            .child(section_label(&theme, "Persistent tunnels"));
        if self.tunnels.is_empty() {
            body = body.child(empty_note(NO_TUNNELS));
        }
        for tunnel in self.tunnels.clone() {
            let row =
                self.render_tunnel(&theme, &tunnel, running.get(&tunnel.id), any_starting, cx);
            body = body.child(row);
        }
        body = body
            .child(div().my(Theme::spacing(1.)).child(divider(&theme)))
            .child(section_label(&theme, "Connections"));
        if self.connections.is_empty() {
            body = body.child(empty_note(NO_CONNECTIONS));
        }
        for conn in self.connections.clone() {
            let forwards = by_conn.get(&conn.id).cloned().unwrap_or_default();
            let group = self.render_connection(&theme, &conn, &forwards, cx);
            body = body.child(group);
        }

        let menu = self.render_menu(&theme, cx);
        let adhoc = self.render_adhoc(&theme, window, cx);
        let editor = self
            .editor
            .as_ref()
            .map(|(editor, _)| window_layer(window, DIALOG_LAYER, editor.clone()));
        let dialogs = self
            .dialogs
            .render(|this: &mut Self| &mut this.dialogs, window, cx);
        div()
            .id("forwarding-panel")
            .w(px(layout::FORWARDING_PANEL_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(p.sidebar)
            .border_l_1()
            .border_color(p.divider)
            .font_family(fonts::UI)
            .child(header)
            .child(body)
            .children(menu)
            .children(adhoc)
            .children(editor)
            .children(dialogs)
    }
}

/// Create the panel for a window.
pub fn forwarding_panel(backend: Backend, cx: &mut App) -> Entity<ForwardingPanel> {
    cx.new(|cx| ForwardingPanel::new(backend, cx))
}
