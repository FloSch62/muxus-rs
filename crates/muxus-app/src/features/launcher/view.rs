//! The quick launcher dialog (`client/src/components/QuickLauncherDialog.tsx`).
//!
//! One search box over everything the window can open: tabs and their open
//! files, saved hosts, ad-hoc `user@host` targets, workspaces, saved
//! commands, local shells, tunnels, retained session history, app actions
//! and the keymap. Arrow keys move from the box into the results, Enter runs
//! the selected one, Escape closes.
//!
//! What a result does to the window (activate a tab, connect a host, open a
//! dialog, run a key command) is reported as a [`LauncherEvent`] for the app
//! to carry out. Workspaces open through the window's [`Workspaces`] runtime,
//! saved commands run through the command hooks
//! ([`crate::features::command_buttons::run`]), and stopping a tunnel goes
//! straight to the backend.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use muxus_core::api::{
    ForwardInfo, ForwardStatus, PasswordVaultStatus, SavedHostProfile, SessionLogSummary,
    SshHostEntry, TunnelRecord, WorkspaceSummary,
};
use muxus_session::Backend;
use muxus_store::history::SessionHistoryQuery;
use muxus_store::prefs::PrefsState;
use muxus_store::prefs::command_buttons::CommandButton;
use muxus_store::prefs::state::LocalShellProfileConfig;
use muxus_ui::gpui_kit::base::input::{Input, InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    Hsla, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div, px,
};
use muxus_ui::icon::{MEDIUM, SMALL, icon};
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts};
use muxus_ui::widgets::dialog::{dialog_paper, modal_at_top};
use muxus_ui::widgets::display::divider;
use muxus_ui::widgets::list::{ListItemButton, list_item_icon, list_item_text, pitch_correction};
use muxus_ui::widgets::progress::{CircularProgress, linear_progress_indeterminate};
use muxus_ui::widgets::text_field::style_input;
use muxus_ui::widgets::{Alert, Button, ButtonColor};

use super::catalog::{
    BreakState, CatalogInput, LauncherAction, LauncherPayload, LauncherResult,
    build_action_results, build_catalog_results, build_query_results,
};
use super::kit::{chord_chip, kbd, show_error_toast, show_toast, text, text_weight};
use super::search::{RESULT_LIMIT, select_quick_launcher_items};
use crate::features::command_buttons::palette::command_button_ink;
use crate::features::command_buttons::run::run_command_button;
use crate::features::settings::SettingsPrefs;
use crate::features::settings::vault_secrets::find_vault_secret;
use crate::features::workspaces::model::Workspaces;
use crate::hosts::managed::ManagedHost;
use crate::keymap::bindings::KeybindingOverrides;
use crate::keymap::chords::IS_MAC;
use crate::state::tabs::{TabStatus, TerminalTab};

/// History search waits for typing to pause this long.
const HISTORY_DEBOUNCE: Duration = Duration::from_millis(250);

/// What the launcher reads about the window it was opened in.
#[derive(Clone, Default)]
pub struct LauncherContext {
    pub tabs: Vec<TerminalTab>,
    pub active_id: Option<String>,
    /// The command group the bar shows (its commands lead the empty launcher).
    pub shown_command_group: String,
    /// Installed WSL distributions offered next to the saved local shells.
    pub wsl_shell_profiles: Vec<LocalShellProfileConfig>,
    /// The window's workspace runtime.
    pub workspaces: Option<Entity<Workspaces>>,
}

/// What the launcher asks the window to do. Every event but
/// [`LauncherEvent::Close`] is followed by `Close` unless noted.
#[derive(Clone, Debug, PartialEq)]
pub enum LauncherEvent {
    /// Remove the launcher (the window owns the entity).
    Close,
    /// Activate a tab; `reconnect` asks its ended session to reconnect.
    ActivateTab { tab_id: String, reconnect: bool },
    /// Show an editor tab of a session (`activateEditor`).
    ActivateEditor { tab_id: String, path: String },
    /// `connectManagedHost`.
    ConnectHost(ManagedHost),
    /// `connectTarget`: an ad-hoc SSH target.
    ConnectTarget(String),
    /// `openLocalShellProfile`.
    OpenLocalShell(LocalShellProfileConfig),
    /// Open session history at a search and a session (`openHistory`).
    OpenHistory { query: String, selected_id: String },
    /// One of the app actions.
    Action(LauncherAction),
    /// Run a keyboard command by id (`command.run()`).
    RunKeyCommand(String),
    /// Start a saved tunnel (dialing it may ask for credentials).
    StartTunnel(TunnelRecord),
}

pub struct QuickLauncher {
    backend: Backend,
    context: LauncherContext,
    input: Entity<InputState>,
    list_focus: FocusHandle,
    scroll: ScrollHandle,
    query: String,
    /// The trimmed query history searched for (after the debounce).
    history_query: String,
    ssh_hosts: Option<Vec<SshHostEntry>>,
    saved_hosts: Option<Vec<SavedHostProfile>>,
    tunnels: Option<Vec<TunnelRecord>>,
    forwards: Vec<ForwardInfo>,
    vault: Option<PasswordVaultStatus>,
    history: Vec<SessionLogSummary>,
    history_loading: bool,
    history_seq: u64,
    catalog: Vec<LauncherResult>,
    query_results: Vec<LauncherResult>,
    actions: Vec<LauncherResult>,
    /// The ranked results on screen (indices into `all()`).
    results: Vec<LauncherResult>,
    selected: usize,
    pending_workspace: Option<WorkspaceSummary>,
    busy_result: Option<String>,
    debounce: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LauncherEvent> for QuickLauncher {}

impl Focusable for QuickLauncher {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx).clone()
    }
}

/// Open the launcher over the window: the app mounts the returned view on
/// top of its root and removes it on [`LauncherEvent::Close`].
pub fn open_quick_launcher(
    backend: Backend,
    context: LauncherContext,
    window: &mut Window,
    cx: &mut App,
) -> Entity<QuickLauncher> {
    cx.new(|cx| QuickLauncher::new(backend, context, window, cx))
}

fn prefs_snapshot(cx: &App) -> PrefsState {
    SettingsPrefs::global(cx)
        .map(|prefs| prefs.read(cx).prefs().clone())
        .unwrap_or_default()
}

fn overrides(prefs: &PrefsState) -> KeybindingOverrides {
    prefs
        .keybindings
        .iter()
        .map(|(id, chords)| (id.clone(), chords.clone()))
        .collect()
}

impl QuickLauncher {
    pub fn new(
        backend: Backend,
        context: LauncherContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search hosts, tabs, workspaces, commands, tunnels, history…")
        });
        let mut subscriptions = vec![cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.query_changed(cx);
            }
        })];
        if let Some(prefs) = SettingsPrefs::global(cx) {
            subscriptions.push(cx.observe(&prefs, |this, _, cx| this.rebuild(cx)));
        }
        if let Some(workspaces) = &context.workspaces {
            subscriptions.push(cx.observe(workspaces, |this, _, cx| this.rebuild(cx)));
        }
        input.update(cx, |state, cx| state.focus(window, cx));
        let mut launcher = Self {
            backend,
            context,
            input,
            list_focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            query: String::new(),
            history_query: String::new(),
            ssh_hosts: None,
            saved_hosts: None,
            tunnels: None,
            forwards: Vec::new(),
            vault: None,
            history: Vec::new(),
            history_loading: false,
            history_seq: 0,
            catalog: Vec::new(),
            query_results: Vec::new(),
            actions: Vec::new(),
            results: Vec::new(),
            selected: 0,
            pending_workspace: None,
            busy_result: None,
            debounce: None,
            _subscriptions: subscriptions,
        };
        launcher.load(cx);
        launcher.rebuild(cx);
        launcher
    }

    /// The window's tabs or workspace changed while the launcher is open.
    pub fn set_context(&mut self, context: LauncherContext, cx: &mut Context<Self>) {
        self.context = context;
        self.rebuild(cx);
    }

    /// Type `query` into the search box (the window can open the launcher
    /// pre-filled; the examples use it).
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let value = query.to_owned();
        self.input
            .update(cx, |state, cx| state.set_value(value, window, cx));
        self.query_changed(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let ssh = backend.ssh_config().await;
            let _ = this.update(cx, |this, cx| {
                this.ssh_hosts = Some(ssh.map(|config| config.hosts).unwrap_or_default());
                this.rebuild(cx);
            });
            let saved = backend.saved_hosts().await;
            let _ = this.update(cx, |this, cx| {
                this.saved_hosts = Some(saved.map(|saved| saved.profiles).unwrap_or_default());
                this.rebuild(cx);
            });
            let tunnels = backend.tunnels().await;
            let forwards = backend.forwards(None).await;
            let _ = this.update(cx, |this, cx| {
                this.tunnels = Some(tunnels.map(|tunnels| tunnels.tunnels).unwrap_or_default());
                this.forwards = forwards.unwrap_or_default();
                this.rebuild(cx);
            });
            // Secret names and missing secrets need the vault; plain commands do not.
            let wants_vault = this
                .update(cx, |_, cx| {
                    prefs_snapshot(cx)
                        .command_buttons
                        .iter()
                        .any(|button| button.secret_id.is_some())
                })
                .unwrap_or(false);
            if wants_vault {
                let vault = backend.vault_status().await.ok();
                let _ = this.update(cx, |this, cx| {
                    this.vault = vault;
                    this.rebuild(cx);
                });
            }
        })
        .detach();
    }

    fn loading(&self) -> bool {
        self.ssh_hosts.is_none()
            || self.saved_hosts.is_none()
            || self.tunnels.is_none()
            || self.history_loading
    }

    fn active_tab(&self) -> Option<&TerminalTab> {
        let id = self.context.active_id.as_deref()?;
        self.context.tabs.iter().find(|tab| tab.id == id)
    }

    fn active_connected(&self) -> bool {
        self.active_tab()
            .is_some_and(|tab| tab.profile.is_some() && tab.status == TabStatus::Connected)
    }

    fn live_count(&self) -> usize {
        self.context
            .tabs
            .iter()
            .filter(|tab| {
                tab.profile.is_some()
                    && matches!(tab.status, TabStatus::Connected | TabStatus::Connecting)
            })
            .count()
    }

    fn workspace_busy(&self, cx: &App) -> bool {
        self.context
            .workspaces
            .as_ref()
            .is_some_and(|workspaces| workspaces.read(cx).state().busy)
    }

    /// Rebuild the catalog and the action list, then rank again.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let prefs = prefs_snapshot(cx);
        let empty_counts = HashMap::new();
        let workspace_state = self
            .context
            .workspaces
            .as_ref()
            .map(|workspaces| workspaces.read(cx).state().clone());
        let active_connected = self.active_connected();
        let ssh_hosts = self.ssh_hosts.clone().unwrap_or_default();
        let saved_hosts = self.saved_hosts.clone().unwrap_or_default();
        let tunnels = self.tunnels.clone().unwrap_or_default();
        self.catalog = build_catalog_results(CatalogInput {
            tabs: &self.context.tabs,
            active_id: self.context.active_id.as_deref(),
            ssh_hosts: &ssh_hosts,
            saved_hosts: &saved_hosts,
            workspaces: workspace_state
                .as_ref()
                .map(|state| state.workspaces.as_slice())
                .unwrap_or(&[]),
            active_workspace_id: workspace_state
                .as_ref()
                .and_then(|state| state.active_id.as_deref()),
            open_workspace_window_counts: workspace_state
                .as_ref()
                .map(|state| &state.open_window_counts)
                .unwrap_or(&empty_counts),
            commands: &prefs.command_buttons,
            command_groups: &prefs.command_button_groups,
            shown_command_group: &self.context.shown_command_group,
            vault: self.vault.as_ref(),
            local_shell_profiles: &prefs.local_shell_profiles,
            wsl_shell_profiles: &self.context.wsl_shell_profiles,
            tunnels: &tunnels,
            forwards: &self.forwards,
            active_connected,
        });
        let active = self.active_tab();
        let active_ssh = active.is_some_and(|tab| {
            tab.status == TabStatus::Connected
                && matches!(
                    tab.profile,
                    Some(muxus_core::protocol::SessionProfile::Ssh(_))
                )
                && tab.conn_id.is_some()
                && tab.sftp_available != Some(false)
        });
        self.actions = build_action_results(
            active_connected,
            active_ssh,
            BreakState::of(active),
            &overrides(&prefs),
        );
        self.rebuild_query(cx);
    }

    fn rebuild_query(&mut self, cx: &mut Context<Self>) {
        self.query_results = build_query_results(
            self.ssh_hosts.as_deref().unwrap_or(&[]),
            self.saved_hosts.as_deref().unwrap_or(&[]),
            &self.history,
            &self.history_query,
            &self.query,
        );
        self.rank(cx);
    }

    fn rank(&mut self, cx: &mut Context<Self>) {
        let all: Vec<&LauncherResult> = self
            .catalog
            .iter()
            .chain(self.query_results.iter())
            .chain(self.actions.iter())
            .collect();
        let owned: Vec<LauncherResult> = all.into_iter().cloned().collect();
        self.results = select_quick_launcher_items(&owned, &self.query, RESULT_LIMIT)
            .into_iter()
            .cloned()
            .collect();
        if self.selected >= self.results.len() {
            self.selected = self.results.len().saturating_sub(1);
        }
        cx.notify();
    }

    fn query_changed(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        if query == self.query {
            return;
        }
        self.query = query;
        self.selected = 0;
        self.scroll.scroll_to_item(0);
        self.rebuild_query(cx);
        let trimmed = muxus_core::js::js_trim(&self.query).to_owned();
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HISTORY_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.search_history(trimmed, cx));
        }));
    }

    fn search_history(&mut self, query: String, cx: &mut Context<Self>) {
        self.history_query = query.clone();
        self.history_seq += 1;
        let seq = self.history_seq;
        if query.chars().count() < 2 {
            self.history.clear();
            self.history_loading = false;
            self.rebuild_query(cx);
            return;
        }
        self.history_loading = true;
        cx.notify();
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend
                .session_history(SessionHistoryQuery {
                    query: Some(query),
                    ..SessionHistoryQuery::default()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.history_seq != seq {
                    return;
                }
                this.history_loading = false;
                this.history = result.map(|response| response.sessions).unwrap_or_default();
                this.rebuild_query(cx);
            });
        })
        .detach();
    }

    fn result_disabled(&self, result: &LauncherResult, cx: &App) -> bool {
        result.disabled_reason.is_some() || self.busy_result.is_some() || self.workspace_busy(cx)
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(LauncherEvent::Close);
    }

    fn request_close(&mut self, cx: &mut Context<Self>) {
        if self.busy_result.is_none() && !self.workspace_busy(cx) {
            self.close(cx);
        }
    }

    fn perform_workspace_open(
        &mut self,
        workspace: WorkspaceSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = window;
        let Some(workspaces) = self.context.workspaces.clone() else {
            return;
        };
        self.busy_result = Some(format!("workspace:{}", workspace.id));
        cx.notify();
        let task = workspaces.update(cx, |workspaces, cx| workspaces.open(&workspace.id, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy_result = None;
                this.pending_workspace = None;
                match result {
                    Ok(opened) => {
                        show_toast(
                            Status::Success,
                            format!("Opened workspace “{}”.", opened.name),
                            cx,
                        );
                        this.close(cx);
                    }
                    Err(error) => show_error_toast(error, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn stop_tunnel(&mut self, tunnel: TunnelRecord, running: ForwardInfo, cx: &mut Context<Self>) {
        self.busy_result = Some(format!("tunnel:{}", tunnel.id));
        cx.notify();
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.stop_forward(&running.id).await;
            let _ = this.update(cx, |this, cx| {
                this.busy_result = None;
                let name = tunnel.name.clone().unwrap_or_else(|| tunnel.target.clone());
                match result {
                    Ok(()) => {
                        show_toast(Status::Success, format!("Stopped tunnel “{name}”."), cx);
                        this.close(cx);
                    }
                    Err(error) => show_error_toast(error.message, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Run a result (`execute`).
    fn execute(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(result) = self.results.get(index).cloned() else {
            return;
        };
        if self.result_disabled(&result, cx) {
            return;
        }
        match result.payload {
            LauncherPayload::Tab { tab_id, reconnect } => {
                self.close(cx);
                cx.emit(LauncherEvent::ActivateTab { tab_id, reconnect });
            }
            LauncherPayload::Editor { tab_id, path } => {
                self.close(cx);
                cx.emit(LauncherEvent::ActivateEditor { tab_id, path });
            }
            LauncherPayload::Host { host, .. } => {
                self.close(cx);
                cx.emit(LauncherEvent::ConnectHost(host));
            }
            LauncherPayload::QuickConnect { target } => {
                self.close(cx);
                cx.emit(LauncherEvent::ConnectTarget(target));
            }
            LauncherPayload::Workspace(workspace) => {
                let active_id = self
                    .context
                    .workspaces
                    .as_ref()
                    .and_then(|workspaces| workspaces.read(cx).state().active_id.clone());
                let focused = self.context.workspaces.as_ref().is_some_and(|workspaces| {
                    workspaces.read(cx).focus_open_workspace(&workspace.id)
                });
                if active_id.as_deref() == Some(workspace.id.as_str()) {
                    self.close(cx);
                } else if focused {
                    show_toast(
                        Status::Info,
                        format!("Switched to the window showing “{}”.", workspace.name),
                        cx,
                    );
                    self.close(cx);
                } else if self.live_count() > 0 {
                    self.pending_workspace = Some(workspace);
                    cx.notify();
                } else {
                    self.perform_workspace_open(workspace, window, cx);
                }
            }
            LauncherPayload::Command(command) => self.run_command(&command, window, cx),
            LauncherPayload::Tunnel { tunnel, running } => match running {
                // A tunnel that gave up reconnecting starts again, with its prompts.
                Some(running) if running.status != ForwardStatus::Error => {
                    self.stop_tunnel(tunnel, running, cx)
                }
                _ => {
                    self.close(cx);
                    cx.emit(LauncherEvent::StartTunnel(tunnel));
                }
            },
            LauncherPayload::History {
                session,
                history_query,
            } => {
                self.close(cx);
                cx.emit(LauncherEvent::OpenHistory {
                    query: history_query,
                    selected_id: session.id,
                });
            }
            LauncherPayload::LocalShell { profile, .. } => {
                self.close(cx);
                cx.emit(LauncherEvent::OpenLocalShell(profile));
            }
            LauncherPayload::Action(action) => {
                self.close(cx);
                cx.emit(LauncherEvent::Action(action));
            }
            LauncherPayload::Keymap { command_id, .. } => {
                self.close(cx);
                cx.emit(LauncherEvent::RunKeyCommand(command_id));
            }
        }
    }

    fn run_command(
        &mut self,
        command: &CommandButton,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let secret_name = command
            .secret_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .and_then(|id| find_vault_secret(self.vault.as_ref(), id))
            .map(|secret| secret.name.clone());
        let tab_id = self.context.active_id.clone();
        let sent = run_command_button(
            tab_id.as_deref(),
            command,
            secret_name.as_deref(),
            window,
            cx,
        );
        if sent {
            self.close(cx);
        } else {
            show_toast(Status::Warning, "The active terminal is not connected.", cx);
        }
    }

    fn first_enabled(&self, cx: &App) -> Option<usize> {
        self.results
            .iter()
            .position(|result| !self.result_disabled(result, cx))
    }

    fn last_enabled(&self, cx: &App) -> Option<usize> {
        self.results
            .iter()
            .rposition(|result| !self.result_disabled(result, cx))
    }

    fn focus_result(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(result) = self.results.get(index) else {
            return;
        };
        if self.result_disabled(result, cx) {
            return;
        }
        self.selected = index;
        self.scroll.scroll_to_item(index);
        window.focus(&self.list_focus, cx);
        cx.notify();
    }

    fn focus_result_from_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected_ok = self
            .results
            .get(self.selected)
            .is_some_and(|result| !self.result_disabled(result, cx));
        if selected_ok {
            self.focus_result(self.selected, window, cx);
        } else if let Some(first) = self.first_enabled(cx) {
            self.focus_result(first, window, cx);
        }
    }

    fn focus_adjacent(&mut self, direction: isize, window: &mut Window, cx: &mut Context<Self>) {
        let mut candidate = self.selected as isize + direction;
        while candidate >= 0 && (candidate as usize) < self.results.len() {
            if !self.result_disabled(&self.results[candidate as usize], cx) {
                self.focus_result(candidate as usize, window, cx);
                return;
            }
            candidate += direction;
        }
        if direction < 0 {
            self.input.update(cx, |state, cx| state.focus(window, cx));
            cx.notify();
        }
    }

    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.pending_workspace.take().is_some() {
            cx.notify();
        } else {
            self.request_close(cx);
        }
    }

    fn on_search_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "down" => {
                cx.stop_propagation();
                self.focus_result_from_search(window, cx);
            }
            "enter" => {
                cx.stop_propagation();
                match self.pending_workspace.clone() {
                    Some(workspace) => self.perform_workspace_open(workspace, window, cx),
                    None => self.execute(self.selected, window, cx),
                }
            }
            "escape" => {
                cx.stop_propagation();
                self.escape(cx);
            }
            _ => {}
        }
    }

    fn on_list_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "down" => self.focus_adjacent(1, window, cx),
            "up" => self.focus_adjacent(-1, window, cx),
            "home" => {
                if let Some(first) = self.first_enabled(cx) {
                    self.focus_result(first, window, cx);
                }
            }
            "end" => {
                if let Some(last) = self.last_enabled(cx) {
                    self.focus_result(last, window, cx);
                }
            }
            "enter" => self.execute(self.selected, window, cx),
            "escape" => self.escape(cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

/// The list icon's ink: its own, or the default (`action.active`,
/// `text.primary` on the selected row).
enum Ink {
    Default,
    Color(Hsla),
}

/// `ResultIcon`: the icon and its ink.
fn result_icon(result: &LauncherResult, theme: &Theme) -> (&'static str, Ink) {
    let p = &theme.palette;
    match &result.payload {
        LauncherPayload::Tab {
            reconnect: true, ..
        } => ("PlayArrowOutlined", Ink::Color(p.warning.main)),
        LauncherPayload::Tab { .. } => (
            "TerminalOutlined",
            Ink::Color(if result.item.detail.starts_with("Connected") {
                p.success.main
            } else {
                p.primary.main
            }),
        ),
        LauncherPayload::Editor { .. } => ("CodeOutlined", Ink::Color(p.primary.main)),
        LauncherPayload::Host { .. } | LauncherPayload::QuickConnect { .. } => {
            ("DnsOutlined", Ink::Color(p.primary.main))
        }
        LauncherPayload::Workspace(_) => ("WorkspacesOutlined", Ink::Default),
        LauncherPayload::Command(command) => {
            let name = if command.secret_id.is_some() {
                "KeyOutlined"
            } else {
                "BoltOutlined"
            };
            match command.color {
                Some(color) => (name, Ink::Color(command_button_ink(color, theme))),
                None => (name, Ink::Color(p.warning.main)),
            }
        }
        LauncherPayload::Tunnel { running, .. } => match running {
            Some(running) if running.status != ForwardStatus::Error => {
                ("StopOutlined", Ink::Color(p.success.main))
            }
            _ => ("SwapHorizOutlined", Ink::Default),
        },
        LauncherPayload::History { .. } => ("HistoryOutlined", Ink::Default),
        LauncherPayload::LocalShell { .. } => ("TerminalOutlined", Ink::Color(p.primary.main)),
        LauncherPayload::Keymap { .. } => ("KeyboardOutlined", Ink::Default),
        LauncherPayload::Action(action) => (
            match action {
                LauncherAction::Settings => "SettingsOutlined",
                LauncherAction::Workspaces => "WorkspacesOutlined",
                LauncherAction::History => "HistoryOutlined",
                LauncherAction::Commands => "BoltOutlined",
                LauncherAction::Tunnels => "SwapHorizOutlined",
                LauncherAction::Sftp => "FolderOutlined",
                LauncherAction::NewHost => "DnsOutlined",
                LauncherAction::NewLocal | LauncherAction::FindTerminal => "TerminalOutlined",
            },
            Ink::Default,
        ),
    }
}

/// `HOTKEY_MOD_LABEL` + K.
fn launcher_chord() -> &'static str {
    if IS_MAC { "⌘K" } else { "Ctrl+K" }
}

/// One result row: MUI's dense `ListItemButton` with `mx: 0.75, py: 0.7,
/// borderRadius: 1`, a 38px icon column and the kind tag at the end.
fn result_row(
    launcher: &QuickLauncher,
    index: usize,
    result: &LauncherResult,
    theme: &Theme,
    cx: &mut Context<QuickLauncher>,
) -> ListItemButton {
    let p = &theme.palette;
    let t = &theme.typography;
    let disabled = launcher.result_disabled(result, cx);
    let selected = index == launcher.selected;
    let (name, ink) = result_icon(result, theme);
    let ink = match ink {
        Ink::Color(color) => color,
        Ink::Default if selected => p.text.primary,
        Ink::Default => p.action.active,
    };
    let tag_gap = Theme::spacing(1.5);
    let trailing: AnyElement = if launcher.busy_result.as_deref() == Some(result.id()) {
        div()
            .ml(Theme::spacing(1.))
            .child(CircularProgress::new(
                ("launcher-busy", index),
                16.,
                p.primary.main,
            ))
            .into_any_element()
    } else if let LauncherPayload::Keymap { chord, .. } = &result.payload {
        match chord {
            Some(chord) => div()
                .ml(tag_gap)
                .child(kbd(chord.clone(), theme, p.text.secondary))
                .into_any_element(),
            None => div().into_any_element(),
        }
    } else {
        div()
            .ml(tag_gap)
            .child(text(result.kind_label(), t.tag, p.text.disabled).no_truncate())
            .into_any_element()
    };
    let hover = cx.entity().downgrade();
    // MUI: 1 + 5.6 + 6 + 2 × 18.59 + 6 + 5.6 per row; GPUI lays it out at 61.
    let chrome_pitch = 1. + 2. * Theme::spacing_css(0.7) + 12. + 2. * t.body2.line_height;
    ListItemButton::new(("quick-launcher-result", index))
        .selected(selected)
        .disabled(disabled)
        .radius(theme.radius)
        .padding(Theme::spacing_css(2.), Theme::spacing_css(0.7))
        .extra_bottom(pitch_correction(index, chrome_pitch, chrome_pitch.floor()))
        .no_hover()
        .on_hover(move |_, cx| {
            let _ = hover.update(cx, |this, cx| {
                if this.selected != index {
                    this.selected = index;
                    cx.notify();
                }
            });
        })
        .on_click(cx.listener(
            move |this: &mut QuickLauncher, _: &ClickEvent, window, cx| {
                this.execute(index, window, cx)
            },
        ))
        .child(list_item_icon(38., icon(name, SMALL, ink)))
        .child(list_item_text(
            text_weight(result.item.label.clone(), t.body2, 600., p.text.primary),
            Some(text(result.secondary().to_owned(), t.body2, p.text.secondary).into_any_element()),
        ))
        .child(trailing)
}

impl Render for QuickLauncher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let t = theme.typography;
        style_input(&self.input, &theme, cx);
        let viewport = window.viewport_size();
        let (vw, vh) = (
            muxus_ui::zoom::unzoom(viewport.width),
            muxus_ui::zoom::unzoom(viewport.height),
        );
        // `top: { xs: 16, sm: '10vh' }`, `maxHeight: min(720px, 80vh)`, maxWidth sm.
        let max_height = 720f32.min(vh * 0.8);
        let top = if vw < 600. {
            Theme::spacing_css(2.)
        } else {
            vh * 0.1
        };
        let width = 600f32.min(vw - 64.);
        let loading = self.loading();

        // The search field: a standard input without underline, `px: 2,
        // py: 1.4`, 17px type, the small input's 1px/5px input padding.
        let search_type = t.search_input;
        let end: AnyElement = if loading {
            CircularProgress::new("launcher-loading", 18., p.primary.main).into_any_element()
        } else {
            chord_chip(launcher_chord(), &theme).into_any_element()
        };
        let search = div()
            .flex()
            .flex_none()
            .items_center()
            .px(Theme::spacing(2.))
            .py(Theme::spacing(1.4))
            .capture_key_down(cx.listener(Self::on_search_key))
            .child(div().flex().flex_none().mr(Theme::spacing(1.)).child(icon(
                "Search",
                MEDIUM,
                p.primary.main,
            )))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pt(px(1.))
                    .pb(px(5.))
                    .h(px(search_type.line_height + 6.))
                    .font_family(fonts::UI)
                    .text_size(px(search_type.size))
                    .line_height(px(search_type.line_height))
                    .text_color(p.text.primary)
                    .child(Input::new(&self.input)),
            )
            .child(div().flex().flex_none().ml(Theme::spacing(1.)).child(end));

        let separator: AnyElement = if loading {
            linear_progress_indeterminate(&theme, 2.).into_any_element()
        } else {
            divider(&theme).into_any_element()
        };

        let mut list = div()
            .id("quick-launcher-results")
            .track_focus(&self.list_focus)
            .on_key_down(cx.listener(Self::on_list_key))
            .flex()
            .flex_col()
            .max_h(px(max_height
                - if self.pending_workspace.is_some() {
                    190.
                } else {
                    110.
                }))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .pt(Theme::spacing(0.75))
            .pb(Theme::spacing(0.75) + px(1.));
        let results = self.results.clone();
        for (index, result) in results.iter().enumerate() {
            list = list.child(result_row(self, index, result, &theme, cx));
        }
        if self.results.is_empty() && !loading {
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .py(Theme::spacing(5.))
                    .px(Theme::spacing(3.))
                    .child(div().mb(Theme::spacing(1.)).child(icon(
                        "Search",
                        t.px_to_rem(34.),
                        p.text.disabled,
                    )))
                    .child(
                        text(
                            format!(
                                "No launcher results match “{}”.",
                                muxus_core::js::js_trim(&self.query)
                            ),
                            t.body2,
                            p.text.secondary,
                        )
                        .no_truncate(),
                    ),
            );
        }

        let mut paper = dialog_paper(&theme)
            .w(px(width))
            .max_h(px(max_height))
            .overflow_hidden()
            .child(search)
            .child(separator);
        if let Some(workspace) = &self.pending_workspace {
            let live = self.live_count();
            let message = format!(
                "Open “{}”? This ends {live} live or connecting session{}.",
                workspace.name,
                if live == 1 { "" } else { "s" }
            );
            let (cancel, open) = (cx.entity().downgrade(), cx.entity().downgrade());
            let workspace = workspace.clone();
            let disabled = self.busy_result.is_some() || self.workspace_busy(cx);
            paper = paper.child(
                div()
                    .mx(Theme::spacing(1.5))
                    .mt(Theme::spacing(1.25))
                    .child(
                        Alert::new(Status::Warning, message).action(
                            div()
                                .flex()
                                .gap(Theme::spacing(0.5))
                                .child(
                                    Button::new("launcher-cancel-open", "Cancel")
                                        .color(ButtonColor::Inherit)
                                        .on_click(move |_, _, cx| {
                                            let _ = cancel.update(cx, |this, cx| {
                                                this.pending_workspace = None;
                                                cx.notify();
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("launcher-confirm-open", "Open")
                                        .contained()
                                        .color(ButtonColor::Warning)
                                        .disabled(disabled)
                                        .on_click(move |_, window, cx| {
                                            let workspace = workspace.clone();
                                            let _ = open.update(cx, |this, cx| {
                                                this.perform_workspace_open(workspace, window, cx)
                                            });
                                        }),
                                ),
                        ),
                    ),
            );
        }
        let count = self.results.len();
        let footer = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::spacing(2.))
            .px(Theme::spacing(2.))
            .py(Theme::spacing(0.8))
            .child(key_hint("↑↓", "navigate", &theme))
            .child(key_hint("↵", "open", &theme))
            .child(key_hint("esc", "close", &theme))
            .child(
                text(
                    format!(
                        "{count}{} result{}",
                        if count == RESULT_LIMIT { "+" } else { "" },
                        if count == 1 { "" } else { "s" }
                    ),
                    t.caption,
                    p.text.secondary,
                )
                .no_truncate(),
            );
        paper = paper
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h(px(180.))
                    .overflow_hidden()
                    .child(list),
            )
            .child(divider(&theme))
            .child(footer);

        let close = cx.entity().downgrade();
        div()
            .id("quick-launcher")
            .absolute()
            .inset_0()
            .child(modal_at_top(
                paper,
                Some(Rc::new(move |_, cx: &mut App| {
                    let _ = close.update(cx, |this, cx| this.request_close(cx));
                })),
                top,
            ))
    }
}

/// `KeyHint`: a chord in the mono tier, then what it does.
fn key_hint(keys: &str, what: &str, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(Theme::spacing(0.5))
        .child(kbd(
            SharedString::from(keys.to_owned()),
            theme,
            theme.palette.text.primary,
        ))
        .child(
            text(
                what.to_owned(),
                theme.typography.caption,
                theme.palette.text.secondary,
            )
            .no_truncate(),
        )
}
