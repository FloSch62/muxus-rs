//! Terminal tabs: one `TerminalView` per terminal tab, the session behind it,
//! and what the tab shows and does when that session ends. Port of the
//! session half of `client/src/components/TerminalViewImpl.tsx` (the socket
//! effect, `showFinalState`, the reconnect and paste paths) on top of the
//! native view in `muxus-term-view`.
//!
//! Hidden tabs keep their view and session, as the client keeps hidden tabs
//! mounted. The view never talks to the session directly: output is written
//! into it by the session pump below, and its input (keys, pastes, resizes,
//! BREAK) arrives on a transport pair whose other end the input pump forwards
//! to the session, holding keystrokes back until the session is ready.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use muxus_app::data_source::connect_size;
use muxus_app::model::{HostMetadata, ProfileKind, SessionProfile};
use muxus_app::session::diagnostics::{
    diagnose_connection, diagnosis_failed, diagnosis_started, settled_diagnosis,
};
use muxus_app::session::recovery::{
    AUTO_RECONNECT_STABLE_MS, AutoReconnectInput, CONNECTION_INTERRUPTION_GRACE_MS,
    ChannelClose as CloseInfo, TerminalExit as ExitInfo, auto_reconnect_delay_ms,
    auto_reconnect_notice, can_diagnose_connection, connection_failure_reason, is_diagnose_input,
    reattach_command_for, reconnect_prompt, should_delay_connection_lost,
    should_wait_for_terminal_output, terminal_notice,
};
use muxus_app::session::{LinkEvent, LinkInput, LinkWriter};
use muxus_app::state::tabs::{DisconnectReason, TabPatch, TabStatus, TerminalTab};
use muxus_core::paste_pacing::{MAX_PACED_PASTE_LENGTH, PastePacing};
use muxus_core::protocol::{
    ConnectionHealth, ExitReason, LoginSequenceState, PasteProgressState, TerminalClientMessage,
    TerminalServerMessage,
};
use muxus_core::stream::{
    TerminalSize, TransportChannels, TransportEnds, TransportEvent, TransportInput,
};
use muxus_store::prefs::state::{CursorStyle, RightClickAction, TerminalFileLinkActivation};
use muxus_term_view::muxus_term::keywords::resolve_keyword_highlights;
use muxus_term_view::muxus_term::links::{LinkActivation, PathSource, resolve_terminal_file_path};
use muxus_term_view::muxus_term::palette::{
    terminal_color_for_host, terminal_scheme, terminal_scheme_id_for_host,
    theme_with_color_overrides,
};
use muxus_term_view::muxus_term::right_click::RightClickAction as TermRightClick;
use muxus_term_view::muxus_term::scrollback::terminal_history_divider;
use muxus_term_view::muxus_term::terminal::CursorShape;
use muxus_term_view::{TerminalSettings, TerminalView, TerminalViewEvent, new_terminal_view};
use muxus_ui::gpui_kit::{AppContext, ClipboardItem, Context, Entity, Subscription, Task, Window};
use muxus_ui::theme::{ActiveTheme, Status};

use crate::ui::shell::{AppShell, ShellMenu};

/// Type-ahead held for a session that has not finished connecting.
const PENDING_INPUT_LIMIT: usize = 64 * 1024;
/// A tab whose pane never lays out (a background tab) connects at the view's
/// default size after this long; a visible one connects at its first fit.
const FIRST_FIT_WAIT: Duration = Duration::from_millis(250);
/// Between the dials of hidden tabs opened together.
const FIRST_FIT_STAGGER: Duration = Duration::from_millis(20);
/// Most link events, and output bytes, handed to the window in one turn: big
/// enough to keep up with a flood, small enough to keep frames coming.
const EVENT_BATCH: usize = 1024;
const EVENT_BATCH_BYTES: usize = 64 * 1024;
/// The most output handed to the emulator in one update.
const OUTPUT_PIECE: usize = 16 * 1024;
/// How long output may hold the UI thread before frames and input get a turn
/// (xterm.js gives its write buffer 12 ms per frame as well).
const OUTPUT_SLICE: Duration = Duration::from_millis(12);
/// While output streams in (the last update was this recent), less than one
/// piece of it lets the event loop run a turn or two to collect more before it
/// goes in: a flood of short lines becomes far fewer updates, while a
/// keystroke's echo after a pause is shown at once. No timer: waiting would
/// back the session up and slow the flood down.
const OUTPUT_MERGE: Duration = Duration::from_millis(2);
/// Event-loop turns without new output that end the collecting.
const OUTPUT_MERGE_TURNS: u32 = 2;

/// Keystrokes typed before the session is ready, sent the moment it is.
#[derive(Default)]
struct InputGate {
    ready: bool,
    pending: Vec<Vec<u8>>,
    bytes: usize,
}

/// One session generation of a tab: dialled on mount and on every reconnect.
struct LiveSession {
    writer: Arc<dyn LinkWriter>,
    gate: Arc<Mutex<InputGate>>,
    exit: Option<ExitInfo>,
    ready: bool,
    ready_at: Option<Instant>,
    saw_auth_prompt: bool,
    received_output: bool,
    waiting_for_output: bool,
    transport_suspect: bool,
    transient_status: bool,
    /// The login sequence's reattach command waits for it to finish.
    pending_reattach: Option<&'static str>,
    ended: bool,
    _tasks: Vec<Task<()>>,
}

/// A terminal tab's view and the session behind it.
pub struct TabTerminal {
    pub view: Entity<TerminalView>,
    /// The view's transport from the session's side: the session's end is
    /// reported on `events`; the view's input arrives on `input`.
    ends: TransportEnds,
    profile: SessionProfile,
    /// Distinguishes this view and session from the ones a reconnect replaced.
    pub generation: u64,
    seen_reconnect_request: u32,
    seen_search_request: u32,
    awaiting_size: bool,
    /// The stored output is still being replayed: the session dials after it.
    replaying: bool,
    session: Option<LiveSession>,
    /// The ended session offers the diagnose key.
    diagnosable: bool,
    /// Connection checks running for the ended session.
    diagnosis: Option<Task<()>>,
    /// Automatic redials since the last stable connection.
    auto_reconnect_attempts: usize,
    auto_reconnect: Option<Task<()>>,
    interruption: Option<Task<()>>,
    /// A session moved here from another window: the first dial attaches to it.
    attach: Option<String>,
    /// Output arrived since the last stored snapshot (`snapshotDirty`).
    snapshot_dirty: bool,
    /// Counts output, so a save that raced newer output leaves the snapshot dirty.
    snapshot_revision: u64,
    snapshot_saving: bool,
    /// Stores a snapshot once output goes quiet (`TERMINAL_SNAPSHOT_QUIET_MS`).
    snapshot_quiet: Option<Task<()>>,
    _first_fit: Option<Task<()>>,
    _subscription: Subscription,
}

impl TabTerminal {
    /// End the session from this side (the tab closed or was replaced).
    fn shutdown(&mut self, cx: &mut Context<AppShell>) {
        if let Some(session) = self.session.take() {
            cx.background_spawn(session.writer.send(LinkInput::Close))
                .detach();
        }
        self.ends.events.close();
    }
}

/// The parts of a tab the terminal reads while it starts.
#[derive(Clone)]
struct TerminalTabInfo {
    id: String,
    profile: SessionProfile,
    connect_on_mount: bool,
    /// Restored from a workspace: its stored output replays first.
    restored: bool,
    reconnect_request: u32,
    search_request: u32,
}

impl TerminalTabInfo {
    fn of(tab: &TerminalTab) -> Option<Self> {
        let profile = tab.profile.clone().filter(SessionProfile::is_terminal)?;
        Some(Self {
            id: tab.id.clone(),
            profile,
            connect_on_mount: tab.connect_on_mount,
            restored: tab.restored,
            reconnect_request: tab.reconnect_request,
            search_request: tab.search_request,
        })
    }
}

/// `xterm`'s cursor style from the preference.
fn cursor_shape(style: CursorStyle) -> CursorShape {
    match style {
        CursorStyle::Block => CursorShape::Block,
        CursorStyle::Underline => CursorShape::Underline,
        CursorStyle::Bar => CursorShape::Bar,
    }
}

fn right_click(action: RightClickAction) -> TermRightClick {
    match action {
        RightClickAction::CopyPaste => TermRightClick::CopyPaste,
        RightClickAction::Paste => TermRightClick::Paste,
        RightClickAction::Menu => TermRightClick::Menu,
    }
}

fn link_activation(activation: TerminalFileLinkActivation) -> LinkActivation {
    match activation {
        TerminalFileLinkActivation::Direct => LinkActivation::Direct,
        TerminalFileLinkActivation::Alt => LinkActivation::Alt,
        TerminalFileLinkActivation::Ctrl => LinkActivation::Ctrl,
        TerminalFileLinkActivation::Meta => LinkActivation::Meta,
    }
}

/// The custom schemes of the preferences in the terminal crate's shape (both
/// are the same JSON document).
fn custom_schemes(
    prefs: &crate::ui::prefs::Prefs,
) -> Vec<muxus_term_view::muxus_term::palette::TerminalScheme> {
    let custom: Vec<muxus_term_view::muxus_term::custom_schemes::CustomTerminalScheme> =
        serde_json::to_value(&prefs.custom_terminal_schemes)
            .ok()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
    muxus_term_view::muxus_term::custom_schemes::terminal_schemes_from_custom(&custom)
}

impl AppShell {
    /// The Muxus metadata of the host a tab's session belongs to.
    pub fn host_metadata_for(&self, profile: &SessionProfile) -> Option<HostMetadata> {
        if let Some(id) = profile.profile_id() {
            return self
                .catalog
                .profiles
                .iter()
                .find(|candidate| candidate.id == id)
                .map(|entry| entry.metadata.clone());
        }
        let SessionProfile::Ssh(ssh) = profile else {
            return None;
        };
        if ssh.use_config == Some(false) {
            return None;
        }
        self.catalog
            .ssh
            .hosts
            .iter()
            .find(|host| host.aliases.contains(&ssh.target))
            .and_then(|host| host.metadata.clone())
    }

    /// What one tab's terminal reads from the preferences, with its host's
    /// scheme, colors and highlighting applied.
    pub fn terminal_settings(
        &self,
        tab_id: &str,
        profile: &SessionProfile,
        dark: bool,
    ) -> TerminalSettings {
        let prefs = &self.prefs;
        let metadata = self.host_metadata_for(profile);
        let custom = custom_schemes(prefs);
        let application_scheme = if dark {
            prefs.dark_terminal_scheme.as_str()
        } else {
            prefs.light_terminal_scheme.as_str()
        };
        let scheme_id = terminal_scheme_id_for_host(
            application_scheme,
            metadata.as_ref().and_then(|m| m.terminal_scheme.as_deref()),
            &custom,
        );
        let scheme = terminal_scheme(Some(scheme_id), &custom);
        let font_color = terminal_color_for_host(
            &prefs.font_color,
            metadata
                .as_ref()
                .and_then(|m| m.terminal_font_color.as_deref()),
        );
        let background_color = terminal_color_for_host(
            &prefs.background_color,
            metadata
                .as_ref()
                .and_then(|m| m.terminal_background_color.as_deref()),
        );
        let theme = theme_with_color_overrides(&scheme.theme, font_color, background_color);
        let host_rules = metadata
            .as_ref()
            .and_then(|m| m.keyword_highlights.as_ref());
        let profile_rules = host_rules
            .and_then(|config| config.profile_id.as_deref())
            .and_then(|id| {
                prefs
                    .keyword_highlight_profiles
                    .iter()
                    .find(|candidate| candidate.id == id)
            })
            .map(|profile| profile.rules.clone())
            .unwrap_or_default();
        let sftp_available = self.tabs.tab(tab_id).and_then(|tab| tab.sftp_available);
        TerminalSettings {
            font_family: prefs.font_family.clone(),
            font_size: prefs.mono_font_size as f32,
            line_height: prefs.line_height as f32,
            cursor_style: cursor_shape(prefs.cursor_style),
            cursor_blink: prefs.cursor_blink,
            scrollback: prefs.scrollback.max(0.0) as usize,
            theme: theme.into_owned(),
            theme_is_light: scheme.light,
            app_dark: dark,
            copy_on_select: prefs.copy_on_select,
            right_click: right_click(prefs.right_click_action),
            link_activation: link_activation(prefs.terminal_file_link_activation),
            file_links: match profile {
                SessionProfile::Local(_) => true,
                SessionProfile::Ssh(_) => sftp_available == Some(true),
                _ => false,
            },
            allow_osc52_write: prefs.allow_osc52_clipboard_write,
            paste_warn_multiline: prefs.paste_warn_multiline,
            keyword_rules: resolve_keyword_highlights(
                &prefs.keyword_highlights,
                host_rules,
                &profile_rules,
            ),
            ..TerminalSettings::default()
        }
    }

    /// Push the current preferences and theme to every terminal.
    pub fn refresh_terminal_settings(&mut self, cx: &mut Context<Self>) {
        let dark = cx.theme().mode.is_dark();
        let updates: Vec<(Entity<TerminalView>, TerminalSettings)> = self
            .terminals
            .iter()
            .map(|(id, terminal)| {
                (
                    terminal.view.clone(),
                    self.terminal_settings(id, &terminal.profile, dark),
                )
            })
            .collect();
        for (view, settings) in updates {
            view.update(cx, |view, cx| {
                if view.settings() != &settings {
                    view.set_settings(settings, cx);
                }
            });
        }
    }

    /// The terminal of the window's active tab.
    pub fn active_terminal(&self) -> Option<Entity<TerminalView>> {
        let id = self.tabs.active_id.as_ref()?;
        self.terminals.get(id).map(|terminal| terminal.view.clone())
    }

    pub fn terminal_of(&self, tab_id: &str) -> Option<Entity<TerminalView>> {
        self.terminals
            .get(tab_id)
            .map(|terminal| terminal.view.clone())
    }

    /// Give a tab's terminal the keyboard.
    pub fn focus_terminal(&mut self, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.terminal_of(tab_id) {
            let handle = view.read(cx).focus_handle().clone();
            window.focus(&handle, cx);
            self.focused_tab = Some(tab_id.to_string());
        }
    }

    /// Bring the views in line with the tabs: start terminals for new tabs,
    /// end the ones whose tab closed, redial on reconnect requests, open
    /// search on find requests and move the keyboard to the active tab.
    pub fn sync_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let infos: Vec<TerminalTabInfo> = self
            .tabs
            .tabs
            .iter()
            .filter_map(TerminalTabInfo::of)
            .collect();
        let live: HashSet<&str> = infos.iter().map(|info| info.id.as_str()).collect();
        let gone: Vec<String> = self
            .terminals
            .keys()
            .filter(|id| !live.contains(id.as_str()))
            .cloned()
            .collect();
        for id in gone {
            if let Some(mut terminal) = self.terminals.remove(&id) {
                terminal.shutdown(cx);
            }
        }
        for info in &infos {
            match self.terminals.get(&info.id) {
                None => {
                    self.start_terminal(info, None, window, cx);
                    // A tab moved here from another window: its buffer, then its session.
                    if let Some((terminal_id, buffer)) = self.transfers.remove(&info.id)
                        && let Some(terminal) = self.terminals.get_mut(&info.id)
                    {
                        terminal.attach = Some(terminal_id);
                        if !buffer.is_empty() {
                            terminal
                                .view
                                .clone()
                                .update(cx, |view, cx| view.write_output(buffer.as_bytes(), cx));
                        }
                    }
                }
                Some(terminal) if terminal.seen_reconnect_request != info.reconnect_request => {
                    self.restart_terminal(info, window, cx)
                }
                Some(terminal) if terminal.seen_search_request != info.search_request => {
                    let view = terminal.view.clone();
                    if let Some(terminal) = self.terminals.get_mut(&info.id) {
                        terminal.seen_search_request = info.search_request;
                    }
                    view.update(cx, |view, cx| view.open_search(window, cx));
                }
                Some(_) => {}
            }
        }
        // The active tab's terminal takes the keyboard when the tab becomes
        // active, as `term.focus()` on `active` does; menus and dialogs keep it.
        let active = self.tabs.active_id.clone();
        if active != self.focused_tab && self.menu.is_none() && !self.modal_open() {
            match active.filter(|id| self.terminals.contains_key(id)) {
                Some(id) => {
                    self.focused_tab = Some(id.clone());
                    cx.defer_in(window, move |this, window, cx| {
                        this.focus_terminal(&id, window, cx);
                    });
                }
                None => self.focused_tab = self.tabs.active_id.clone(),
            }
        }
    }

    /// Start a tab's terminal. `carry` is the previous generation's buffer
    /// and zoom, replayed above the new session's output.
    fn start_terminal(
        &mut self,
        info: &TerminalTabInfo,
        carry: Option<(String, i32)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dark = cx.theme().mode.is_dark();
        let settings = self.terminal_settings(&info.id, &info.profile, dark);
        let (channels, ends) = TransportChannels::pair(256);
        let view = new_terminal_view(channels, settings, window, cx);
        let replayed = carry.is_some();
        if let Some((buffer, zoom_steps)) = carry {
            view.update(cx, |view, cx| {
                for _ in 0..zoom_steps.unsigned_abs() {
                    if zoom_steps > 0 {
                        view.zoom_in(cx);
                    } else {
                        view.zoom_out(cx);
                    }
                }
                if !buffer.is_empty() {
                    view.write_output(buffer.as_bytes(), cx);
                    view.write_output(terminal_history_divider().as_bytes(), cx);
                }
            });
        }
        // A restored tab replays what it showed before (the stored snapshot) on its first
        // mount, and only then dials or offers to reconnect, so the new output lands below
        // the divider.
        let replay = (!replayed
            && info.restored
            && self.prefs.restore_scrollback
            && self.snapshots_fetched.insert(info.id.clone()))
        .then(|| self.data.backend())
        .flatten()
        .map(|backend| {
            let tab_id = info.id.clone();
            let view = view.downgrade();
            cx.spawn(async move |_, cx| {
                let stored = backend.terminal_snapshot(&tab_id).await.ok().flatten();
                if let Some(stored) = stored {
                    let data = muxus_term::scrollback::restorable_snapshot(
                        &stored.data,
                        Some(stored.format_version),
                    );
                    if !data.is_empty() {
                        let _ = view.update(cx, |view, cx| {
                            view.write_output(data.as_bytes(), cx);
                            view.write_output(terminal_history_divider().as_bytes(), cx);
                        });
                    }
                }
            })
        });
        let tab_id = info.id.clone();
        let subscription = cx.subscribe_in(&view, window, move |this, view, event, window, cx| {
            this.on_terminal_event(&tab_id, view, event, window, cx)
        });
        self.next_generation += 1;
        let generation = self.next_generation;
        let should_connect = info.connect_on_mount || replayed;
        let mut terminal = TabTerminal {
            view: view.clone(),
            ends,
            profile: info.profile.clone(),
            generation,
            seen_reconnect_request: info.reconnect_request,
            seen_search_request: info.search_request,
            awaiting_size: should_connect,
            replaying: replay.is_some(),
            session: None,
            diagnosable: false,
            diagnosis: None,
            auto_reconnect_attempts: 0,
            auto_reconnect: None,
            interruption: None,
            attach: None,
            snapshot_dirty: false,
            snapshot_revision: 0,
            snapshot_saving: false,
            snapshot_quiet: None,
            _first_fit: None,
            _subscription: subscription,
        };
        if should_connect {
            self.tabs.update(
                &info.id,
                TabPatch {
                    status: Some(TabStatus::Connecting),
                    conn_id: Some(None),
                    sftp_available: Some(None),
                    sftp_open: Some(false),
                    terminal_cwd: Some(None),
                    ..Default::default()
                },
            );
            // A pane that is laid out fits at once and connects at its real
            // size; one hidden behind another tab connects at the default.
            // Hidden tabs opened together dial one per frame: starting a local
            // shell forks this process, which holds the UI thread up for a
            // few milliseconds each time.
            let queued = self
                .terminals
                .values()
                .filter(|terminal| terminal.awaiting_size)
                .count() as u32;
            let wait = FIRST_FIT_WAIT + FIRST_FIT_STAGGER * queued;
            let tab_id = info.id.clone();
            terminal._first_fit = Some(cx.spawn(async move |this, cx| {
                if let Some(replay) = replay {
                    replay.await;
                    let _ = this.update(cx, |this, _| {
                        if let Some(terminal) = this.terminals.get_mut(&tab_id) {
                            terminal.replaying = false;
                        }
                    });
                }
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| {
                    let size = this
                        .terminals
                        .get(&tab_id)
                        .filter(|t| t.generation == generation && t.awaiting_size)
                        .map(|t| t.view.read(cx).size());
                    if let Some(size) = size {
                        this.connect_terminal(&tab_id, size, cx);
                    }
                });
            }));
        } else {
            // A restored tab waits for a key press before it dials.
            let notice = |view: &mut TerminalView, cx: &mut Context<TerminalView>| {
                view.write_output(b"\x1b[90m[restored session]\x1b[0m\r\n", cx);
                view.write_output(b"\x1b[1;36mPress any key to reconnect\x1b[0m\r\n", cx);
            };
            match replay {
                Some(replay) => {
                    let view = view.downgrade();
                    cx.spawn(async move |_, cx| {
                        replay.await;
                        let _ = view.update(cx, notice);
                    })
                    .detach();
                }
                None => view.update(cx, notice),
            }
            let _ = terminal.ends.events.try_send(TransportEvent::Exit {
                code: None,
                signal: None,
            });
            self.tabs.update(
                &info.id,
                TabPatch {
                    status: Some(TabStatus::Closed),
                    ..Default::default()
                },
            );
        }
        self.terminals.insert(info.id.clone(), terminal);
    }

    /// `persistSnapshot`: store the tab's recent output so a restored tab can replay it. The
    /// snapshot stays dirty until a save succeeds with nothing newer arrived meanwhile.
    pub fn persist_snapshot(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        if !self.prefs.restore_scrollback {
            return;
        }
        let Some(backend) = self.data.backend() else {
            return;
        };
        let Some(terminal) = self.terminals.get_mut(tab_id) else {
            return;
        };
        if !terminal.snapshot_dirty || terminal.snapshot_saving {
            return;
        }
        let Some(data) = terminal.view.read(cx).snapshot(None) else {
            return;
        };
        let (revision, generation) = (terminal.snapshot_revision, terminal.generation);
        terminal.snapshot_saving = true;
        let tab = tab_id.to_string();
        cx.spawn(async move |this, cx| {
            let saved = backend
                .save_terminal_snapshot(
                    &tab,
                    &data,
                    Some(muxus_term::scrollback::TERMINAL_SNAPSHOT_FORMAT_VERSION),
                )
                .await
                .is_ok();
            let _ = this.update(cx, |this, _| {
                if let Some(terminal) = this.terminals.get_mut(&tab)
                    && terminal.generation == generation
                {
                    terminal.snapshot_saving = false;
                    if saved && terminal.snapshot_revision == revision {
                        terminal.snapshot_dirty = false;
                    }
                }
            });
        })
        .detach();
    }

    /// Every tab's unsaved output (the interval timer).
    pub fn persist_snapshots(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.terminals.keys().cloned().collect();
        for id in ids {
            self.persist_snapshot(&id, cx);
        }
    }

    /// Every tab's unsaved output, written before this returns: the window is closing (the
    /// page's keepalive flush on unload).
    pub fn flush_snapshots(&mut self, cx: &mut Context<Self>) {
        if !self.prefs.restore_scrollback {
            return;
        }
        let Some(backend) = self.data.backend() else {
            return;
        };
        let pending: Vec<(String, String)> = self
            .terminals
            .iter()
            .filter(|(_, terminal)| terminal.snapshot_dirty)
            .filter_map(|(id, terminal)| {
                // In process there is no keepalive body cap: the full ladder applies.
                let data = terminal.view.read(cx).snapshot(None)?;
                Some((id.clone(), data))
            })
            .collect();
        for (id, data) in pending {
            let saved = futures_lite::future::block_on(backend.save_terminal_snapshot(
                &id,
                &data,
                Some(muxus_term::scrollback::TERMINAL_SNAPSHOT_FORMAT_VERSION),
            ));
            if saved.is_ok()
                && let Some(terminal) = self.terminals.get_mut(&id)
            {
                terminal.snapshot_dirty = false;
            }
        }
    }

    /// Replace a tab's terminal with a fresh one that replays the old buffer
    /// (`carryBufferRef`) and dials a new session.
    fn restart_terminal(
        &mut self,
        info: &TerminalTabInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut old) = self.terminals.remove(&info.id) else {
            return;
        };
        let attempts = old.auto_reconnect_attempts;
        let (buffer, zoom_steps) = {
            let view = old.view.read(cx);
            let buffer = if self.prefs.restore_scrollback {
                view.snapshot(None).unwrap_or_default()
            } else {
                String::new()
            };
            let base = view.settings().font_size;
            let zoom = (view.zoom_percent() as f32 / 100.0 * base - base).round() as i32;
            (buffer, zoom)
        };
        old.shutdown(cx);
        drop(old);
        self.start_terminal(info, Some((buffer, zoom_steps)), window, cx);
        if let Some(terminal) = self.terminals.get_mut(&info.id) {
            terminal.auto_reconnect_attempts = attempts;
        }
        if self.tabs.active_id.as_deref() == Some(info.id.as_str()) {
            self.focused_tab = None;
        }
    }

    /// Dial the session for a tab at `size`.
    fn connect_terminal(&mut self, tab_id: &str, size: TerminalSize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.tab(tab_id).cloned() else {
            return;
        };
        let Some(terminal) = self.terminals.get_mut(tab_id) else {
            return;
        };
        if !terminal.awaiting_size || terminal.replaying {
            return;
        }
        terminal.awaiting_size = false;
        let Some(profile) = terminal.profile.as_terminal() else {
            return;
        };
        let profile = match profile {
            muxus_core::protocol::TerminalProfile::Ssh(mut ssh) => {
                // Read at send time, the way dialConnection does.
                ssh.keepalive_interval_seconds = self.prefs.ssh_keepalive_field();
                muxus_core::protocol::TerminalProfile::Ssh(ssh)
            }
            other => other,
        };
        // A tmux/screen reattach brings back its own directory instead.
        let cwd = (terminal.profile.kind() == ProfileKind::Ssh
            && tab.reconnect_request > 0
            && tab.reconnect_mode.is_none())
        .then(|| tab.terminal_cwd.clone())
        .flatten();
        // A tab moved from another window takes over the live session there.
        let connect = match terminal.attach.take() {
            Some(terminal_id) => TerminalClientMessage::Attach {
                terminal_id,
                cols: u32::from(size.cols),
                rows: u32::from(size.rows),
            },
            None => {
                let connect = TerminalClientMessage::Connect {
                    profile,
                    fresh_transport: tab.fresh_transport.clone(),
                    title: Some(tab.title.clone()),
                    cwd,
                    cols: u32::from(size.cols),
                    rows: u32::from(size.rows),
                };
                debug_assert_eq!(connect_size(&connect).cols, size.cols.max(1));
                connect
            }
        };
        let link = self.data.open_terminal(connect);
        let generation = terminal.generation;
        let gate = Arc::new(Mutex::new(InputGate::default()));
        let writer = link.writer.clone();

        // Output and control messages, in order, into the window.
        let mut reader = link.reader;
        let tab = tab_id.to_string();
        let events = cx.spawn(async move |this, cx| {
            let mut slice = Instant::now();
            let mut last_update: Option<Instant> = None;
            let mut queue: VecDeque<LinkEvent> = VecDeque::new();
            while let Some(first) = reader.next_event().await {
                push_split(&mut queue, first);
                if last_update.is_some_and(|at| at.elapsed() < OUTPUT_MERGE) {
                    let mut quiet = 0;
                    while quiet < OUTPUT_MERGE_TURNS && queued_bytes(&queue) < OUTPUT_PIECE {
                        futures_lite::future::yield_now().await;
                        let before = queue.len();
                        while queued_bytes(&queue) < EVENT_BATCH_BYTES {
                            match reader.try_next_event() {
                                Some(event) => push_split(&mut queue, event),
                                None => break,
                            }
                        }
                        quiet = if queue.len() == before { quiet + 1 } else { 0 };
                    }
                }
                let mut bytes = queued_bytes(&queue);
                while queue.len() < EVENT_BATCH && bytes < EVENT_BATCH_BYTES {
                    match reader.try_next_event() {
                        Some(event) => {
                            push_split(&mut queue, event);
                            bytes = queued_bytes(&queue);
                        }
                        None => break,
                    }
                }
                let mut ended = false;
                // One update per piece of output: a flood of short lines costs
                // the emulator a few microseconds per line, so pieces keep each
                // turn on the UI thread short.
                while !queue.is_empty() {
                    let mut part = Vec::new();
                    let mut part_bytes = 0;
                    while let Some(event) = queue.front() {
                        let len = match event {
                            LinkEvent::Data(data) => data.len(),
                            _ => 0,
                        };
                        if !part.is_empty() && part_bytes + len > OUTPUT_PIECE {
                            break;
                        }
                        part_bytes += len;
                        part.extend(queue.pop_front());
                    }
                    let closed = part
                        .iter()
                        .any(|event| matches!(event, LinkEvent::Closed { .. }));
                    let started = Instant::now();
                    let updated = this
                        .update(cx, |this, cx| {
                            this.on_session_events(&tab, generation, part, cx)
                        })
                        .is_ok();
                    last_update = Some(Instant::now());
                    if let Some(perf) = crate::ui::perf::perf() {
                        perf.batch(part_bytes, started.elapsed());
                    }
                    if !updated || closed {
                        ended = true;
                        break;
                    }
                    // A flood keeps the queue full, and a ready channel never
                    // makes this task wait: hand the thread back to the event
                    // loop every few milliseconds so frames are painted and keys
                    // handled.
                    if slice.elapsed() >= OUTPUT_SLICE {
                        cx.background_executor()
                            .timer(Duration::from_millis(1))
                            .await;
                        slice = Instant::now();
                    }
                }
                if ended {
                    break;
                }
            }
        });

        // The view's input, out to the session; keystrokes wait for `ready`.
        let input = terminal.ends.input.clone();
        let input_gate = gate.clone();
        let input_writer = writer.clone();
        let inputs = cx.background_spawn(async move {
            while let Ok(message) = input.recv().await {
                let input = match message {
                    TransportInput::Data(data) => {
                        let held = {
                            let mut gate = input_gate.lock().unwrap_or_else(|p| p.into_inner());
                            if gate.ready {
                                None
                            } else {
                                if gate.bytes + data.len() <= PENDING_INPUT_LIMIT {
                                    gate.bytes += data.len();
                                    gate.pending.push(data.clone());
                                }
                                Some(())
                            }
                        };
                        if held.is_some() {
                            continue;
                        }
                        LinkInput::Data(data)
                    }
                    TransportInput::Resize(size) => {
                        LinkInput::message(TerminalClientMessage::Resize {
                            cols: u32::from(size.cols),
                            rows: u32::from(size.rows),
                        })
                    }
                    TransportInput::Break { .. } => {
                        LinkInput::message(TerminalClientMessage::SendBreak)
                    }
                    TransportInput::Close => LinkInput::Close,
                };
                if !input_writer.send(input).await {
                    break;
                }
            }
        });

        terminal.session = Some(LiveSession {
            writer,
            gate,
            exit: None,
            ready: false,
            ready_at: None,
            saw_auth_prompt: false,
            received_output: false,
            waiting_for_output: false,
            transport_suspect: false,
            transient_status: false,
            pending_reattach: None,
            ended: false,
            _tasks: vec![events, inputs],
        });
        self.tabs.update(
            tab_id,
            TabPatch {
                login_sequence: Some(None),
                ..Default::default()
            },
        );
    }

    /// Send a message to a tab's live session.
    pub fn send_to_session(&self, tab_id: &str, input: LinkInput, cx: &mut Context<Self>) -> bool {
        let Some(session) = self
            .terminals
            .get(tab_id)
            .and_then(|terminal| terminal.session.as_ref())
            .filter(|session| !session.ended)
        else {
            return false;
        };
        cx.background_spawn(session.writer.send(input)).detach();
        true
    }

    fn write_to(&self, tab_id: &str, text: &str, cx: &mut Context<Self>) {
        if let Some(view) = self.terminal_of(tab_id) {
            view.update(cx, |view, cx| view.write_output(text.as_bytes(), cx));
        }
    }

    /// The session pump's batch: output into the view, control messages into the tab.
    fn on_session_events(
        &mut self,
        tab_id: &str,
        generation: u64,
        events: Vec<LinkEvent>,
        cx: &mut Context<Self>,
    ) {
        if self
            .terminals
            .get(tab_id)
            .is_none_or(|terminal| terminal.generation != generation)
        {
            return;
        }
        let mut output: Vec<u8> = Vec::new();
        let mut tabs_dirty = false;
        for event in events {
            match event {
                LinkEvent::Data(data) => {
                    if output.is_empty() {
                        output = data;
                    } else {
                        output.extend_from_slice(&data);
                    }
                }
                other => {
                    if !output.is_empty() {
                        tabs_dirty |= self.on_output(tab_id, std::mem::take(&mut output), cx);
                    }
                    match other {
                        LinkEvent::Message(message) => {
                            tabs_dirty |= self.on_session_message(tab_id, *message, cx)
                        }
                        LinkEvent::Closed { reason } => {
                            self.on_session_closed(tab_id, reason, cx);
                            tabs_dirty = true;
                        }
                        LinkEvent::Data(_) => {}
                    }
                }
            }
        }
        if !output.is_empty() {
            tabs_dirty |= self.on_output(tab_id, output, cx);
        }
        if tabs_dirty {
            self.tabs_changed(cx);
        }
    }

    /// Terminal output. Returns whether the tabs changed.
    fn on_output(&mut self, tab_id: &str, data: Vec<u8>, cx: &mut Context<Self>) -> bool {
        let Some(terminal) = self.terminals.get_mut(tab_id) else {
            return false;
        };
        terminal.snapshot_dirty = true;
        terminal.snapshot_revision += 1;
        // The interval alone would lose a command run seconds before the window closes.
        let quiet_tab = tab_id.to_string();
        terminal.snapshot_quiet = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(
                    muxus_term::scrollback::TERMINAL_SNAPSHOT_QUIET_MS,
                ))
                .await;
            let _ = this.update(cx, |this, cx| this.persist_snapshot(&quiet_tab, cx));
        }));
        let view = terminal.view.clone();
        let mut changed = false;
        let mut clear_transient = false;
        if let Some(session) = terminal.session.as_mut() {
            clear_transient = std::mem::take(&mut session.transient_status);
            session.received_output = true;
            if session.waiting_for_output {
                session.waiting_for_output = false;
                if !session.transport_suspect {
                    self.tabs.update(
                        tab_id,
                        TabPatch {
                            status: Some(TabStatus::Connected),
                            failure_reason: Some(None),
                            disconnect_reason: Some(None),
                            ..Default::default()
                        },
                    );
                    changed = true;
                }
            }
        }
        changed |= self.tabs.notify_output(tab_id);
        view.update(cx, |view, cx| {
            if clear_transient {
                view.write_output(b"\r\x1b[2K", cx);
            }
            view.write_output(&data, cx);
        });
        changed
    }

    fn clear_transient_status(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let cleared = self
            .terminals
            .get_mut(tab_id)
            .and_then(|terminal| terminal.session.as_mut())
            .is_some_and(|session| std::mem::take(&mut session.transient_status));
        if cleared {
            self.write_to(tab_id, "\r\x1b[2K", cx);
        }
    }

    /// One control message. Returns whether the tabs changed.
    fn on_session_message(
        &mut self,
        tab_id: &str,
        message: TerminalServerMessage,
        cx: &mut Context<Self>,
    ) -> bool {
        let kind = match self.terminals.get(tab_id) {
            Some(terminal) => terminal.profile.kind(),
            None => return false,
        };
        match message {
            TerminalServerMessage::Session { terminal_id } => {
                self.tabs.update(
                    tab_id,
                    TabPatch {
                        terminal_id: Some(Some(terminal_id)),
                        ..Default::default()
                    },
                );
                true
            }
            TerminalServerMessage::TransferReady => false,
            TerminalServerMessage::Status { message, transient } => {
                let (ready, suspect) = self
                    .terminals
                    .get(tab_id)
                    .and_then(|t| t.session.as_ref())
                    .map(|s| (s.ready, s.transport_suspect))
                    .unwrap_or_default();
                let mut changed = false;
                if !ready && !suspect {
                    self.tabs.update(
                        tab_id,
                        TabPatch {
                            status: Some(TabStatus::Connecting),
                            conn_id: Some(None),
                            failure_reason: Some(None),
                            disconnect_reason: Some(None),
                            ..Default::default()
                        },
                    );
                    changed = true;
                }
                self.clear_transient_status(tab_id, cx);
                if transient == Some(true) {
                    let cols = self
                        .terminal_of(tab_id)
                        .map(|view| usize::from(view.read(cx).size().cols))
                        .unwrap_or(80);
                    let notice: String = terminal_notice(&message)
                        .chars()
                        .take(cols.saturating_sub(1).max(1))
                        .collect();
                    self.write_to(tab_id, &format!("\r\x1b[90m{notice}\x1b[0m"), cx);
                    if let Some(session) = self
                        .terminals
                        .get_mut(tab_id)
                        .and_then(|t| t.session.as_mut())
                    {
                        session.transient_status = true;
                    }
                } else {
                    self.write_to(tab_id, &format!("\x1b[90m{message}\x1b[0m\r\n"), cx);
                }
                changed
            }
            TerminalServerMessage::ConnectionHealth { state } => {
                let Some(session) = self
                    .terminals
                    .get_mut(tab_id)
                    .and_then(|t| t.session.as_mut())
                else {
                    return false;
                };
                session.transport_suspect = state == ConnectionHealth::Suspect;
                if session.transport_suspect {
                    self.tabs.update(
                        tab_id,
                        TabPatch {
                            status: Some(TabStatus::Interrupted),
                            failure_reason: Some(Some(
                                "The SSH transport is not responding.".into(),
                            )),
                            disconnect_reason: Some(None),
                            ..Default::default()
                        },
                    );
                } else if session.ready {
                    let status = if session.waiting_for_output {
                        TabStatus::Connecting
                    } else {
                        TabStatus::Connected
                    };
                    self.tabs.update(
                        tab_id,
                        TabPatch {
                            status: Some(status),
                            failure_reason: Some(None),
                            disconnect_reason: Some(None),
                            ..Default::default()
                        },
                    );
                }
                true
            }
            TerminalServerMessage::AuthPrompt(prompt) => {
                if let Some(session) = self
                    .terminals
                    .get_mut(tab_id)
                    .and_then(|t| t.session.as_mut())
                {
                    session.saw_auth_prompt = true;
                }
                self.open_auth_prompt(tab_id, prompt, cx);
                false
            }
            TerminalServerMessage::HostKey(challenge) => {
                self.open_host_key_prompt(tab_id, challenge, cx);
                false
            }
            TerminalServerMessage::Ready {
                conn_id,
                sftp_available,
                summary,
                ..
            } => self.on_session_ready(tab_id, kind, conn_id, sftp_available, summary, cx),
            TerminalServerMessage::LoginSequence {
                state,
                step,
                steps,
                detail,
                message,
            } => {
                let was_running = self
                    .tabs
                    .tab(tab_id)
                    .is_some_and(|tab| tab.login_sequence.is_some());
                if state == LoginSequenceState::Running {
                    self.tabs.update(
                        tab_id,
                        TabPatch {
                            login_sequence: Some(Some(
                                muxus_app::state::tabs::LoginSequenceProgress {
                                    step,
                                    steps,
                                    detail,
                                },
                            )),
                            ..Default::default()
                        },
                    );
                    return true;
                }
                self.tabs.update(
                    tab_id,
                    TabPatch {
                        login_sequence: Some(None),
                        ..Default::default()
                    },
                );
                let reattach = self
                    .terminals
                    .get_mut(tab_id)
                    .and_then(|t| t.session.as_mut())
                    .and_then(|s| s.pending_reattach.take());
                if state == LoginSequenceState::Done {
                    if let Some(command) = reattach {
                        self.send_to_session(tab_id, LinkInput::Data(command.into()), cx);
                    }
                    return true;
                }
                if !was_running {
                    return true;
                }
                let notice = if state == LoginSequenceState::Failed {
                    let reason =
                        terminal_notice(&message.unwrap_or_else(|| format!("step {step} failed")));
                    format!(
                        "\x1b[33m[login sequence stopped: {}]\x1b[0m\r\n",
                        reason.strip_suffix('.').unwrap_or(&reason)
                    )
                } else {
                    format!(
                        "\x1b[90m[login sequence cancelled at step {step} of {steps}]\x1b[0m\r\n"
                    )
                };
                self.write_notice_on_own_line(tab_id, &notice, cx);
                true
            }
            TerminalServerMessage::LoggingState {
                enabled,
                session_id,
                paused,
                capture_input,
                warning,
                file_path,
            } => {
                self.tabs.update(
                    tab_id,
                    TabPatch {
                        logging_enabled: Some(Some(enabled)),
                        session_log_id: Some(session_id),
                        logging_warning: Some(warning.clone()),
                        logging_paused: Some(paused),
                        capture_input: Some(capture_input),
                        log_file_path: Some(file_path),
                        ..Default::default()
                    },
                );
                if let Some(warning) = warning {
                    self.show_toast(Status::Warning, warning, cx);
                }
                true
            }
            TerminalServerMessage::BreakResult { ok, message } => {
                if !ok {
                    let notice = format!(
                        "\x1b[33m[{}]\x1b[0m\r\n",
                        terminal_notice(message.as_deref().unwrap_or("BREAK was not sent"))
                    );
                    self.write_notice_on_own_line(tab_id, &notice, cx);
                }
                false
            }
            TerminalServerMessage::Exit {
                code,
                message,
                reason,
            } => {
                if let Some(session) = self
                    .terminals
                    .get_mut(tab_id)
                    .and_then(|t| t.session.as_mut())
                {
                    session.exit = Some(ExitInfo {
                        code,
                        message,
                        reason,
                    });
                }
                false
            }
            TerminalServerMessage::PasteProgress { state, .. } => {
                // The progress overlay is not drawn yet; a finished paste needs nothing.
                let _ = state == PasteProgressState::Running;
                false
            }
        }
    }

    /// Write a notice so it starts on a line of its own.
    fn write_notice_on_own_line(&self, tab_id: &str, notice: &str, cx: &mut Context<Self>) {
        if let Some(view) = self.terminal_of(tab_id) {
            view.update(cx, |view, cx| {
                let at_line_start = view.emulator().cursor().col == 0;
                let text = if at_line_start {
                    notice.to_string()
                } else {
                    format!("\r\n{notice}")
                };
                view.write_output(text.as_bytes(), cx);
            });
        }
    }

    fn on_session_ready(
        &mut self,
        tab_id: &str,
        kind: ProfileKind,
        conn_id: String,
        sftp_available: Option<bool>,
        summary: Option<muxus_core::protocol::SshSessionSummary>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.clear_transient_status(tab_id, cx);
        let show_summary = self.prefs.ssh_session_summary;
        let (pending, waiting, suspect) = {
            let Some(session) = self
                .terminals
                .get_mut(tab_id)
                .and_then(|t| t.session.as_mut())
            else {
                return false;
            };
            session.ready = true;
            session.ready_at = Some(Instant::now());
            session.waiting_for_output =
                should_wait_for_terminal_output(kind, session.received_output);
            let pending = {
                let mut gate = session.gate.lock().unwrap_or_else(|p| p.into_inner());
                gate.ready = true;
                gate.bytes = 0;
                std::mem::take(&mut gate.pending)
            };
            (
                pending,
                session.waiting_for_output,
                session.transport_suspect,
            )
        };
        if let Some(interruption) = self
            .terminals
            .get_mut(tab_id)
            .and_then(|t| t.interruption.take())
        {
            drop(interruption);
        }
        if show_summary && let Some(summary) = summary {
            self.write_to(
                tab_id,
                &muxus_term_view::muxus_term::ssh_summary::format_ssh_session_summary(&summary),
                cx,
            );
        }
        let sftp = (kind == ProfileKind::Ssh).then(|| sftp_available != Some(false));
        let tab = self.tabs.tab(tab_id).cloned();
        self.tabs.update(
            tab_id,
            TabPatch {
                status: Some(if suspect {
                    TabStatus::Interrupted
                } else if waiting {
                    TabStatus::Connecting
                } else {
                    TabStatus::Connected
                }),
                failure_reason: Some(None),
                disconnect_reason: Some(None),
                // Only SSH transport ids key SFTP and forwarding.
                conn_id: Some((kind == ProfileKind::Ssh).then_some(conn_id)),
                sftp_available: Some(sftp),
                sftp_open: (sftp == Some(false)).then_some(false),
                transfer_id: Some(None),
                fresh_transport: Some(None),
                ..Default::default()
            },
        );
        // The replacement connection is live; a reattach goes in now, or
        // once the host's login sequence has logged in.
        if kind == ProfileKind::Ssh
            && let Some(mode) = tab.as_ref().and_then(|tab| tab.reconnect_mode)
        {
            let command = reattach_command_for(mode);
            if tab.is_some_and(|tab| tab.login_sequence.is_some()) {
                if let Some(session) = self
                    .terminals
                    .get_mut(tab_id)
                    .and_then(|t| t.session.as_mut())
                {
                    session.pending_reattach = Some(command);
                }
            } else {
                self.send_to_session(tab_id, LinkInput::Data(command.into()), cx);
            }
        }
        for data in pending {
            self.send_to_session(tab_id, LinkInput::Data(data), cx);
        }
        self.refresh_terminal_settings(cx);
        true
    }

    /// The session's channel closed (`socket.onclose`).
    fn on_session_closed(&mut self, tab_id: &str, reason: Option<String>, cx: &mut Context<Self>) {
        self.clear_transient_status(tab_id, cx);
        self.close_prompts_of(tab_id, cx);
        let Some(terminal) = self.terminals.get_mut(tab_id) else {
            return;
        };
        let kind = terminal.profile.kind();
        let Some(session) = terminal.session.as_mut() else {
            return;
        };
        session.ended = true;
        let exit = session.exit.clone();
        let ready = session.ready;
        let ready_at = session.ready_at;
        let saw_auth_prompt = session.saw_auth_prompt;
        let close = CloseInfo {
            code: 1000,
            reason,
            was_clean: true,
        };
        let failure = connection_failure_reason(exit.as_ref(), &close);
        let reason_kind = exit.as_ref().map(|exit| exit.reason).unwrap_or(if ready {
            ExitReason::Disconnected
        } else {
            ExitReason::Failed
        });
        // The view takes keys as "after the end" from here on.
        let _ = terminal.ends.events.try_send(TransportEvent::Exit {
            code: exit.as_ref().and_then(|exit| exit.code),
            signal: None,
        });
        let generation = terminal.generation;
        if !should_delay_connection_lost(exit.as_ref(), ready) {
            self.show_final_state(
                tab_id,
                kind,
                failure,
                reason_kind,
                ready_at,
                saw_auth_prompt,
                cx,
            );
            return;
        }
        self.tabs.update(
            tab_id,
            TabPatch {
                status: Some(TabStatus::Interrupted),
                conn_id: Some(None),
                terminal_id: Some(None),
                log_file_path: Some(None),
                failure_reason: Some(Some(failure.clone())),
                disconnect_reason: Some(Some(disconnect_reason(reason_kind))),
                login_sequence: Some(None),
                ..Default::default()
            },
        );
        let tab = tab_id.to_string();
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(CONNECTION_INTERRUPTION_GRACE_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                let still = this
                    .terminals
                    .get(&tab)
                    .is_some_and(|t| t.generation == generation)
                    && this
                        .tabs
                        .tab(&tab)
                        .is_some_and(|t| t.status == TabStatus::Interrupted);
                if still {
                    this.show_final_state(
                        &tab,
                        kind,
                        failure,
                        reason_kind,
                        ready_at,
                        saw_auth_prompt,
                        cx,
                    );
                    this.tabs_changed(cx);
                }
            });
        });
        if let Some(terminal) = self.terminals.get_mut(tab_id) {
            terminal.interruption = Some(timer);
        }
    }

    /// `showFinalState`: the notice, the reconnect prompt or countdown, and the closed tab.
    #[allow(clippy::too_many_arguments)]
    fn show_final_state(
        &mut self,
        tab_id: &str,
        kind: ProfileKind,
        failure: String,
        reason_kind: ExitReason,
        ready_at: Option<Instant>,
        saw_auth_prompt: bool,
        cx: &mut Context<Self>,
    ) {
        let enabled = self.prefs.auto_reconnect_remote;
        let Some(terminal) = self.terminals.get_mut(tab_id) else {
            return;
        };
        // A drop after a stable stretch is a fresh incident, not one more
        // failure of the previous redial chain.
        if ready_at
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(AUTO_RECONNECT_STABLE_MS))
        {
            terminal.auto_reconnect_attempts = 0;
        }
        let redial = auto_reconnect_delay_ms(AutoReconnectInput {
            enabled,
            profile_kind: kind,
            reason: reason_kind,
            attempts: terminal.auto_reconnect_attempts,
            saw_auth_prompt,
        });
        let completed = reason_kind == ExitReason::Completed;
        let mut text = format!(
            "\r\n\x1b[{}m[{}: {}]\x1b[0m\r\n",
            if completed { "33" } else { "31" },
            if completed {
                "session ended"
            } else {
                "connection lost"
            },
            terminal_notice(&failure)
        );
        let diagnosable = can_diagnose_connection(kind, reason_kind);
        terminal.diagnosable = diagnosable;
        match redial {
            None => text.push_str(&format!(
                "\x1b[1;36m{}\x1b[0m\r\n",
                reconnect_prompt(diagnosable)
            )),
            Some(delay) => {
                terminal.auto_reconnect_attempts += 1;
                text.push_str(&format!(
                    "\x1b[1;36m{}\x1b[0m\r\n",
                    auto_reconnect_notice(delay, terminal.auto_reconnect_attempts, diagnosable)
                ));
                let tab = tab_id.to_string();
                let generation = terminal.generation;
                terminal.auto_reconnect = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(delay))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if this
                            .terminals
                            .get(&tab)
                            .is_none_or(|t| t.generation != generation)
                        {
                            return;
                        }
                        if !this.prefs.auto_reconnect_remote {
                            if let Some(t) = this.terminals.get_mut(&tab) {
                                t.auto_reconnect_attempts =
                                    t.auto_reconnect_attempts.saturating_sub(1);
                            }
                            return;
                        }
                        this.reconnect_tab(&tab, cx);
                    });
                }));
            }
        }
        let view = terminal.view.clone();
        view.update(cx, |view, cx| view.write_output(text.as_bytes(), cx));
        self.tabs.update(
            tab_id,
            TabPatch {
                status: Some(TabStatus::Closed),
                conn_id: Some(None),
                terminal_id: Some(None),
                log_file_path: Some(None),
                failure_reason: Some(Some(failure)),
                disconnect_reason: Some(Some(disconnect_reason(reason_kind))),
                login_sequence: Some(None),
                ..Default::default()
            },
        );
    }

    /// `reconnectFromTerminalInput`: redial an ended session.
    pub fn reconnect_tab(&mut self, tab_id: &str, cx: &mut Context<Self>) -> bool {
        let Some(tab) = self.tabs.tab(tab_id).cloned() else {
            return false;
        };
        if tab.profile.is_none() || tab.status != TabStatus::Closed {
            return false;
        }
        self.tabs.reconnect(&[tab_id], tab.reconnect_mode);
        self.tabs_changed(cx);
        true
    }

    /// Keys typed into an ended session: D diagnoses where offered, anything
    /// else reconnects.
    fn on_ended_session_input(&mut self, tab_id: &str, data: &[u8], cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.get_mut(tab_id) else {
            return;
        };
        if terminal.diagnosable && is_diagnose_input(data) {
            self.diagnose_connection(tab_id, cx);
            return;
        }
        terminal.auto_reconnect = None;
        self.reconnect_tab(tab_id, cx);
    }

    /// `diagnoseFromTerminalInput`: run the backend's checks against the
    /// session's first hop; the report lands below the failure, followed by
    /// the prompt again.
    fn diagnose_connection(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let backend = self.data.backend();
        let Some(terminal) = self.terminals.get_mut(tab_id) else {
            return;
        };
        if terminal.diagnosis.is_some() {
            return;
        }
        let Some(profile) = terminal.profile.as_terminal().filter(|profile| {
            matches!(
                profile,
                muxus_core::protocol::TerminalProfile::Ssh(_)
                    | muxus_core::protocol::TerminalProfile::Telnet(_)
            )
        }) else {
            return;
        };
        // A pending automatic reconnect waits for the user instead of
        // redialling over the report.
        if terminal.auto_reconnect.take().is_some() {
            terminal.auto_reconnect_attempts = terminal.auto_reconnect_attempts.saturating_sub(1);
        }
        let view = terminal.view.clone();
        view.update(cx, |view, cx| {
            view.write_output(diagnosis_started().as_bytes(), cx)
        });
        let generation = terminal.generation;
        let tab_id = tab_id.to_string();
        terminal.diagnosis = Some(cx.spawn(async move |this, cx| {
            let text = match backend {
                Some(backend) => diagnose_connection(&backend, profile).await,
                None => settled_diagnosis(&diagnosis_failed(
                    "Connection diagnostics need the backend.",
                )),
            };
            let _ = this.update(cx, |this, cx| {
                let Some(terminal) = this
                    .terminals
                    .get_mut(&tab_id)
                    .filter(|terminal| terminal.generation == generation)
                else {
                    return;
                };
                terminal.diagnosis = None;
                terminal
                    .view
                    .clone()
                    .update(cx, |view, cx| view.write_output(text.as_bytes(), cx));
            });
        }));
    }

    fn on_terminal_event(
        &mut self,
        tab_id: &str,
        view: &Entity<TerminalView>,
        event: &TerminalViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Events of a view a reconnect already replaced are stale.
        if self
            .terminals
            .get(tab_id)
            .is_none_or(|terminal| &terminal.view != view)
        {
            return;
        }
        match event {
            TerminalViewEvent::Resized(size) => {
                if self
                    .terminals
                    .get(tab_id)
                    .is_some_and(|terminal| terminal.awaiting_size)
                {
                    self.connect_terminal(tab_id, *size, cx);
                }
            }
            TerminalViewEvent::InputAfterExit(data) => {
                let data = data.clone();
                self.on_ended_session_input(tab_id, &data, cx);
            }
            TerminalViewEvent::ContextMenu {
                position,
                selection,
            } => {
                self.menu = Some(ShellMenu::Terminal {
                    tab_id: tab_id.to_string(),
                    position: *position,
                    selection: selection.clone(),
                });
                cx.notify();
            }
            TerminalViewEvent::ConfirmPaste(text) => {
                let text = text.clone();
                self.open_paste_dialog(tab_id, text, window, cx);
            }
            TerminalViewEvent::ClipboardWithoutText => {
                self.paste_clipboard_image(tab_id, cx);
            }
            TerminalViewEvent::OpenUrl(url) => cx.open_url(url),
            TerminalViewEvent::OpenFile(candidate) => {
                self.open_linked_file(tab_id, candidate, cx);
            }
            TerminalViewEvent::CwdChanged(cwd) => {
                let changed = self
                    .tabs
                    .tab(tab_id)
                    .is_some_and(|tab| tab.terminal_cwd.as_deref() != Some(cwd));
                if changed {
                    self.tabs.update(
                        tab_id,
                        TabPatch {
                            terminal_cwd: Some(Some(cwd.clone())),
                            ..Default::default()
                        },
                    );
                }
            }
            // Typing and pastes reach the other terminals of a multi-execution group.
            TerminalViewEvent::Input(data) => {
                let data = data.clone();
                self.mirror_input(tab_id, &data, cx);
            }
            // The client keeps the tab title the host gave it and has no bell.
            TerminalViewEvent::TitleChanged(_)
            | TerminalViewEvent::Bell
            | TerminalViewEvent::SearchOpened
            | TerminalViewEvent::Exited { .. }
            | TerminalViewEvent::Failed(_) => {}
        }
    }

    /// `openLinkedTerminalFile`: resolve a clicked path and open it in the tab's editor.
    fn open_linked_file(&mut self, tab_id: &str, candidate: &str, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.tab(tab_id).cloned() else {
            return;
        };
        let path = match &tab.profile {
            Some(SessionProfile::Local(_)) => {
                let home = std::env::var("HOME").ok();
                resolve_terminal_file_path(
                    candidate,
                    tab.terminal_cwd.as_deref(),
                    home.as_deref(),
                    PathSource::Local,
                )
            }
            Some(SessionProfile::Ssh(_)) => {
                if tab.sftp_available == Some(false) {
                    self.show_toast(
                        Status::Warning,
                        "SFTP is disabled for this host, so remote files cannot be edited.",
                        cx,
                    );
                    return;
                }
                resolve_terminal_file_path(
                    candidate,
                    tab.terminal_cwd.as_deref(),
                    None,
                    PathSource::Posix,
                )
            }
            _ => return,
        };
        match path {
            Some(path) => self.open_editor(tab_id, &path, cx),
            None => self.show_toast(
                Status::Warning,
                if matches!(tab.profile, Some(SessionProfile::Local(_))) {
                    "The local working directory is unavailable. Click an absolute path instead."
                } else {
                    "The remote working directory is unavailable. Click an absolute path instead."
                },
                cx,
            ),
        }
    }

    /// An image on the clipboard pasted into a local terminal: saved as a
    /// PNG the shell can read, and its path typed in
    /// (`storeLocalTerminalClipboardImage`).
    fn paste_clipboard_image(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let image = item.entries().iter().find_map(|entry| match entry {
            muxus_ui::gpui_kit::ClipboardEntry::Image(image) => Some(image.clone()),
            _ => None,
        });
        let Some(image) = image else {
            return;
        };
        let local = matches!(
            self.tabs.tab(tab_id).and_then(|tab| tab.profile.as_ref()),
            Some(SessionProfile::Local(_))
        );
        if !local {
            self.show_toast(
                Status::Error,
                "Could not paste clipboard content. Image paste over SSH needs SFTP, which is not connected yet.",
                cx,
            );
            return;
        }
        if image.format != muxus_ui::gpui_kit::ImageFormat::Png {
            self.show_toast(
                Status::Error,
                "Could not paste clipboard content. Only PNG images can be pasted.",
                cx,
            );
            return;
        }
        let name = format!(
            "muxus-paste-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default()
        );
        let path = std::env::temp_dir().join(name);
        let written = {
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .and_then(|mut file| file.write_all(&image.bytes))
            }
            #[cfg(not(unix))]
            {
                std::fs::write(&path, &image.bytes)
            }
        };
        match written {
            Ok(()) => {
                let text = path.display().to_string();
                self.paste_text(tab_id, &text, cx);
            }
            Err(err) => self.show_toast(
                Status::Error,
                format!("Could not paste clipboard content. {err}"),
                cx,
            ),
        }
    }

    /// `pasteText`: a paste the user made, previewed when it spans lines.
    pub fn paste_text(&mut self, tab_id: &str, text: &str, cx: &mut Context<Self>) {
        if let Some(view) = self.terminal_of(tab_id) {
            view.update(cx, |view, cx| view.paste(text, cx));
        }
    }

    /// The pacing a paste into this tab uses (`hostPastePacing`).
    pub fn paste_pacing(&self, tab_id: &str) -> PastePacing {
        let metadata = self
            .tabs
            .tab(tab_id)
            .and_then(|tab| tab.profile.as_ref())
            .and_then(|profile| self.host_metadata_for(profile));
        muxus_term_view::muxus_term::paste::host_paste_pacing(
            self.prefs.paste_line_delay_ms.max(0.0) as u32,
            self.prefs.paste_char_delay_ms.max(0.0) as u32,
            metadata.as_ref().and_then(|m| m.paste_line_delay_ms),
            metadata.as_ref().and_then(|m| m.paste_char_delay_ms),
        )
    }

    /// `pasteIntoSession`: send a confirmed paste, paced by the backend when
    /// the host (or the preview) asks for a delay.
    pub fn paste_into_session(
        &mut self,
        tab_id: &str,
        text: &str,
        line_delay_ms: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            return;
        }
        let Some(view) = self.terminal_of(tab_id) else {
            return;
        };
        let mut pacing = self.paste_pacing(tab_id);
        if let Some(delay) = line_delay_ms {
            pacing.line_delay_ms = delay;
        }
        if !pacing.is_paced() {
            view.update(cx, |view, cx| view.paste_confirmed(text, cx));
            return;
        }
        if text.encode_utf16().count() > MAX_PACED_PASTE_LENGTH {
            self.show_toast(
                Status::Error,
                "This paste is too long to send with a delay. Paste it in parts.",
                cx,
            );
            return;
        }
        let bracketed = view.read(cx).emulator().bracketed_paste();
        let sent = self.send_to_session(
            tab_id,
            LinkInput::message(TerminalClientMessage::Paste {
                text: text.to_string(),
                bracketed,
                line_delay_ms: pacing.line_delay_ms,
                char_delay_ms: pacing.char_delay_ms,
            }),
            cx,
        );
        if sent {
            view.update(cx, |view, cx| {
                let _ = view.send_input(Vec::<u8>::new());
                cx.notify();
            });
            self.mirror_paste(tab_id, text, cx);
        } else {
            self.reconnect_tab(tab_id, cx);
        }
    }

    /// Copy text to the clipboard.
    pub fn copy_text(&self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// `sendBreak`: serial, Telnet and SSH sessions that are attached.
    pub fn send_break(&mut self, tab_id: &str, cx: &mut Context<Self>) -> bool {
        let Some(tab) = self.tabs.tab(tab_id) else {
            return false;
        };
        let supports = matches!(
            tab.profile.as_ref().map(SessionProfile::kind),
            Some(ProfileKind::Serial | ProfileKind::Telnet | ProfileKind::Ssh)
        );
        let attached = tab.status == TabStatus::Connected
            || (tab.status == TabStatus::Connecting && tab.conn_id.is_some());
        if !supports || !attached {
            return false;
        }
        self.send_to_session(
            tab_id,
            LinkInput::message(TerminalClientMessage::SendBreak),
            cx,
        )
    }
}

/// Queue an event, cutting large output into pieces of [`OUTPUT_PIECE`].
fn push_split(queue: &mut VecDeque<LinkEvent>, event: LinkEvent) {
    match event {
        LinkEvent::Data(data) if data.len() > OUTPUT_PIECE => {
            queue.extend(
                data.chunks(OUTPUT_PIECE)
                    .map(|piece| LinkEvent::Data(piece.to_vec())),
            );
        }
        other => queue.push_back(other),
    }
}

fn queued_bytes(queue: &VecDeque<LinkEvent>) -> usize {
    queue
        .iter()
        .map(|event| match event {
            LinkEvent::Data(data) => data.len(),
            _ => 0,
        })
        .sum()
}

fn disconnect_reason(reason: ExitReason) -> DisconnectReason {
    match reason {
        ExitReason::Completed => DisconnectReason::Completed,
        ExitReason::Failed => DisconnectReason::Failed,
        ExitReason::Disconnected => DisconnectReason::Disconnected,
    }
}

impl AppShell {
    /// The live session and buffer of a tab, for moving it into another window
    /// (`moveTabToNewWindow`): the terminal is dropped without ending the session, which
    /// waits for the other window to attach (the reattach grace).
    pub fn detach_terminal(
        &mut self,
        tab_id: &str,
        cx: &muxus_ui::gpui_kit::App,
    ) -> Option<(String, String)> {
        let terminal_id = self.tabs.tab(tab_id)?.terminal_id.clone()?;
        let terminal = self.terminals.remove(tab_id)?;
        let buffer = terminal.view.read(cx).snapshot(None).unwrap_or_default();
        // Dropping the link detaches; `shutdown` would end the session.
        drop(terminal);
        Some((terminal_id, buffer))
    }
}
