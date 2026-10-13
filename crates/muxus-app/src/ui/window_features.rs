//! The feature state each main window owns (`AppShell.tsx` and the stores it reads): the
//! workspace runtime and its dialog, command buttons, the action bar and multi-execution,
//! the forwarding panel, the vault's startup unlock and secret sender, the update notice,
//! session history and the window's question host.

use std::cell::RefCell;
use std::rc::Rc;

use muxus_app::features::command_buttons::multi_exec::MultiExecState;
use muxus_app::features::command_buttons::{
    ActionBar, ActionBarEvent, CommandButtons, CommandButtonsEvent, CommandHooks, CommandMenuEvent,
    MultiExecControl, action_bar, multi_exec_control, open_command_buttons, open_command_menu,
    set_command_hooks,
};
use muxus_app::features::forwarding::model::ForwardsSummary;
use muxus_app::features::forwarding::panel::{
    ForwardingPanel, ForwardingPanelEvent, forwarding_panel,
};
use muxus_app::features::history::view::{HistoryEvent, open_history};
use muxus_app::features::notices::update_view::{
    UpdateNoticeEvent, UpdateNotification, update_notification,
};
use muxus_app::features::settings::{SettingsSection, SettingsTarget};
use muxus_app::features::sftp::dialogs::DialogHost;
use muxus_app::features::vault::send::{
    SecretSender, SecretSenderEvent, install_secret_sender, secret_sender,
};
use muxus_app::features::vault::send_menu::{SendSecretMenuEvent, open_send_secret_menu};
use muxus_app::features::vault::startup_unlock::{
    VaultStartupEvent, VaultStartupUnlock, vault_startup_unlock,
};
use muxus_app::features::workspaces::layout::{layout_from_core, window_snapshot};
use muxus_app::features::workspaces::{
    WorkspaceBus, WorkspaceDialogContext, WorkspaceDialogEvent, WorkspaceInitialSelection,
    WorkspaceLayoutHost, Workspaces, open_workspaces,
};
use muxus_app::session::LinkInput;
use muxus_app::state::tabs::{TabStatus, TabsState};
use muxus_app::state::workspace_layout::{PaneNode, RestoreWorkspaceOptions};
use muxus_core::api::{WorkspaceLayoutV1, WorkspaceMultiExecGroup};
use muxus_store::prefs::state::CommandBarPosition;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, Global, IntoElement, Pixels, Point, Subscription,
    Task, WeakEntity, Window,
};
use muxus_ui::theme::Status;

use crate::ui::shell::AppShell;

/// One bus for every window of the process (`BroadcastChannel('muxus-workspaces-v1')`).
struct GlobalWorkspaceBus(WorkspaceBus);

impl Global for GlobalWorkspaceBus {}

fn workspace_bus(cx: &mut App) -> WorkspaceBus {
    if let Some(bus) = cx.try_global::<GlobalWorkspaceBus>() {
        return bus.0.clone();
    }
    let bus = WorkspaceBus::default();
    cx.set_global(GlobalWorkspaceBus(bus.clone()));
    bus
}

/// What the workspace runtime reads of the window, kept current by the window itself so the
/// runtime never has to read the shell while the shell is being updated.
#[derive(Default)]
pub struct LayoutMirror {
    tabs: TabsState,
    multi_exec: MultiExecState,
    connect_remote: bool,
}

/// The window as the workspace runtime sees it.
struct ShellLayoutHost {
    shell: WeakEntity<AppShell>,
    mirror: Rc<RefCell<LayoutMirror>>,
}

impl WorkspaceLayoutHost for ShellLayoutHost {
    fn snapshot(&self, _: &App) -> muxus_app::features::workspaces::layout::WorkspaceSnapshot {
        let mirror = self.mirror.borrow();
        window_snapshot(&mirror.tabs, &mirror.multi_exec)
    }

    fn is_blank(&self, _: &App) -> bool {
        let mirror = self.mirror.borrow();
        mirror.tabs.tabs.is_empty() && matches!(mirror.tabs.root, PaneNode::Pane(_))
    }

    fn restore(
        &self,
        layout: &WorkspaceLayoutV1,
        groups: &[WorkspaceMultiExecGroup],
        connect_remote: bool,
        cx: &mut App,
    ) {
        let layout = layout_from_core(layout);
        let groups = groups.to_vec();
        let _ = self.shell.update(cx, |shell, cx| {
            shell
                .tabs
                .restore(&layout, RestoreWorkspaceOptions { connect_remote });
            shell.multi_exec.update(cx, |multi, cx| {
                multi.update(cx, |state| state.set_groups(&groups))
            });
            shell.publish_layout(cx);
            shell.tabs_changed(cx);
        });
    }

    fn active_workspace_changed(
        &self,
        _id: Option<&str>,
        name: &str,
        _clear_reload_launch: bool,
        cx: &mut App,
    ) {
        let name = name.to_string();
        let _ = self.shell.update(cx, |shell, cx| {
            shell.workspace_name = name;
            cx.notify();
        });
    }

    fn focus_window(&self, cx: &mut App) {
        if let Some(shell) = self.shell.upgrade() {
            crate::ui::features::focus_shell_window(&shell, cx);
        }
    }

    fn connect_remote_on_restore(&self, _: &App) -> bool {
        self.mirror.borrow().connect_remote
    }
}

/// The window's feature state.
pub struct WindowFeatures {
    pub layout_mirror: Rc<RefCell<LayoutMirror>>,
    pub command_buttons: Option<Entity<CommandButtons>>,
    pub action_bar: Option<Entity<ActionBar>>,
    pub multi_exec_control: Option<Entity<MultiExecControl>>,
    pub forwarding: Option<Entity<ForwardingPanel>>,
    pub forwarding_open: bool,
    pub dialog_host: Option<Entity<DialogHost>>,
    pub update_notice: Option<Entity<UpdateNotification>>,
    pub secret_sender: Option<Entity<SecretSender>>,
    pub vault_unlock: Option<Entity<VaultStartupUnlock>>,
    /// The vault's startup question is on screen: it has the keyboard.
    pub vault_blocking: bool,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl Default for WindowFeatures {
    fn default() -> Self {
        Self {
            layout_mirror: Rc::new(RefCell::new(LayoutMirror::default())),
            command_buttons: None,
            action_bar: None,
            multi_exec_control: None,
            forwarding: None,
            forwarding_open: false,
            dialog_host: None,
            update_notice: None,
            secret_sender: None,
            vault_unlock: None,
            vault_blocking: false,
            _subscriptions: Vec::new(),
            _tasks: Vec::new(),
        }
    }
}

impl AppShell {
    /// Create the window's feature state (once, right after the shell). `initial` is the
    /// workspace a new window starts with.
    pub fn install_window_features(
        &mut self,
        initial: Option<WorkspaceInitialSelection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let mut subscriptions = Vec::new();
        self.watch_forwards(backend.clone(), cx);

        // Command buttons, the bar and mirrored input.
        let model = cx.new(|cx| CommandButtons::new(backend.clone(), cx));
        let bar = action_bar(model.clone(), cx);
        subscriptions.push(cx.subscribe_in(
            &bar,
            window,
            |this, _, event: &ActionBarEvent, window, cx| match event {
                ActionBarEvent::ManageCommands => this.open_command_buttons(window, cx),
            },
        ));
        let control = multi_exec_control(self.multi_exec.clone(), window, cx);
        subscriptions.push(cx.observe(&self.multi_exec, |this, _, cx| {
            this.publish_layout(cx);
            cx.notify();
        }));
        self.features.command_buttons = Some(model);
        self.features.action_bar = Some(bar);
        self.features.multi_exec_control = Some(control);

        // The window's own question host (SFTP and local-copy questions, update restarts).
        let host = cx.new(|_| DialogHost::new());
        if crate::ui::features::shells(cx).len() <= 1 {
            muxus_app::features::sftp::local_open::install_dialog_host(host.clone(), cx);
        }
        self.features.dialog_host = Some(host);

        // Secrets typed into terminals, asking for the master password here.
        let shell = cx.entity().downgrade();
        let sender = secret_sender(
            backend.clone(),
            Rc::new(move |tab_id: &str, cx: &App| {
                shell
                    .upgrade()
                    .map(|shell| shell.read(cx).secret_terminal_ids(tab_id, cx))
                    .unwrap_or_default()
            }),
            cx,
        );
        subscriptions.push(cx.subscribe_in(
            &sender,
            window,
            |this, _, event: &SecretSenderEvent, _, cx| match event {
                SecretSenderEvent::VaultChanged => {
                    if let Some(model) = &this.features.command_buttons {
                        model.update(cx, |model, cx| model.refresh_vault(cx));
                    }
                }
            },
        ));
        install_secret_sender(sender.clone(), cx);
        self.features.secret_sender = Some(sender);
        install_command_hooks(cx);

        // The update notice (release manifest, or the in-app updater when it runs).
        let notice = update_notification(backend.clone(), self.prefs.notify_on_new_version, cx);
        subscriptions.push(cx.subscribe_in(
            &notice,
            window,
            |this, _, event: &UpdateNoticeEvent, _, cx| match event {
                UpdateNoticeEvent::Muted => {
                    this.prefs.notify_on_new_version = false;
                    this.prefs_changed(cx);
                }
            },
        ));
        self.features.update_notice = Some(notice);

        // Workspaces: restored once the vault no longer needs its startup password.
        let mirror = self.features.layout_mirror.clone();
        let layout_host = Rc::new(ShellLayoutHost {
            shell: cx.entity().downgrade(),
            mirror,
        });
        let bus = workspace_bus(cx);
        let workspaces = cx.new(|_| Workspaces::new(backend.clone(), &bus, layout_host, initial));
        subscriptions.push(cx.observe(&workspaces, |this, workspaces, cx| {
            let name = workspaces.read(cx).state().active_name.clone();
            if !name.is_empty() && this.workspace_name != name {
                this.workspace_name = name;
                cx.notify();
            }
        }));
        self.workspaces = Some(workspaces);
        self.publish_layout(cx);
        let unlock = vault_startup_unlock(backend, window, cx);
        subscriptions.push(cx.subscribe_in(
            &unlock,
            window,
            |this, _, event: &VaultStartupEvent, _, cx| match event {
                VaultStartupEvent::Ready => {
                    if let Some(workspaces) = this.workspaces.clone() {
                        cx.defer(move |cx| {
                            workspaces.update(cx, |workspaces, cx| workspaces.start(cx))
                        });
                    }
                }
                VaultStartupEvent::Unlocked(_) => {
                    if let Some(model) = &this.features.command_buttons {
                        model.update(cx, |model, cx| model.refresh_vault(cx));
                    }
                }
            },
        ));
        self.features.vault_unlock = Some(unlock);

        // The window coming forward tells the other windows (workspace presence).
        subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active()
                && let Some(workspaces) = &this.workspaces
            {
                workspaces.update(cx, |workspaces, _| workspaces.window_focused());
            }
        }));
        // Closing the window saves its layout and its terminals' output first.
        let closing = cx.entity().downgrade();
        muxus_ui::widgets::window_frame::on_close_request(window, cx, move |_, cx| {
            if let Some(shell) = closing.upgrade() {
                shell.update(cx, |shell, cx| shell.flush_snapshots(cx));
                let workspaces = shell.read(cx).workspaces.clone();
                if let Some(workspaces) = workspaces {
                    workspaces.update(cx, |workspaces, cx| workspaces.stop(cx));
                }
            }
            true
        });
        // Output is stored every few seconds too (`TERMINAL_SNAPSHOT_INTERVAL_MS`).
        self.features._tasks.push(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(
                        muxus_term::scrollback::TERMINAL_SNAPSHOT_INTERVAL_MS,
                    ))
                    .await;
                if this
                    .update(cx, |this, cx| this.persist_snapshots(cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        self.features._subscriptions = subscriptions;
    }

    /// The terminal ids a secret goes to: the tab's session and every terminal mirroring it.
    fn secret_terminal_ids(&self, tab_id: &str, cx: &App) -> Vec<String> {
        let mirrored = self.multi_exec.read(cx).state().mirrored_tab_ids(tab_id);
        let tabs: Vec<String> = if mirrored.is_empty() {
            vec![tab_id.to_string()]
        } else {
            mirrored
        };
        tabs.iter()
            .filter_map(|id| self.tabs.tab(id))
            .filter(|tab| tab.status == TabStatus::Connected)
            .filter_map(|tab| tab.terminal_id.clone())
            .collect()
    }

    /// Keep what the workspace runtime and the command buttons read in step with the tabs,
    /// and let the runtime autosave a changed layout.
    pub fn publish_layout(&mut self, cx: &mut Context<Self>) {
        let multi_exec = self.multi_exec.read(cx).state().clone();
        let changed = {
            let mut mirror = self.features.layout_mirror.borrow_mut();
            let changed = mirror.tabs.tabs != self.tabs.tabs
                || mirror.tabs.root != self.tabs.root
                || mirror.tabs.active_pane_id != self.tabs.active_pane_id
                || mirror.tabs.active_id != self.tabs.active_id
                || mirror.multi_exec != multi_exec;
            if changed {
                mirror.tabs = self.tabs.clone();
                mirror.multi_exec = multi_exec;
            }
            mirror.connect_remote = self.prefs.auto_reconnect_remote;
            changed
        };
        if !changed {
            return;
        }
        if let Some(model) = &self.features.command_buttons {
            let tabs = self.tabs.clone();
            model.update(cx, |model, cx| model.set_tabs(&tabs, cx));
        }
        let tabs = self.tabs.clone();
        self.multi_exec
            .update(cx, |multi, cx| multi.set_tabs(&tabs, cx));
        if let Some(control) = &self.features.multi_exec_control {
            control.update(cx, |control, cx| control.set_tabs(&tabs, cx));
        }
        if let Some(workspaces) = self.workspaces.clone() {
            cx.defer(move |cx| {
                workspaces.update(cx, |workspaces, cx| workspaces.layout_changed(cx))
            });
        }
    }

    /// The command group the bar shows (the launcher lists its commands first).
    pub fn shown_command_group(&self, cx: &App) -> String {
        self.features
            .command_buttons
            .as_ref()
            .map(|model| model.read(cx).active_group(cx).shown_id)
            .unwrap_or_default()
    }

    /// `useMultiExecStore.getState().setGroups([])` after a launched set replaced the layout.
    pub fn clear_multi_exec_groups(&mut self, cx: &mut Context<Self>) {
        self.multi_exec.update(cx, |multi, cx| {
            multi.update(cx, |state| state.set_groups(&[]))
        });
    }

    /// Mirror typed input to the terminals of the multi-execution group.
    pub fn mirror_input(&mut self, source_tab_id: &str, data: &[u8], cx: &mut Context<Self>) {
        let targets = self
            .multi_exec
            .read(cx)
            .state()
            .mirror_tab_ids(source_tab_id);
        for target in targets {
            self.send_to_session(&target, LinkInput::Data(data.to_vec()), cx);
        }
    }

    /// Mirror a paste: each terminal paces it by its own host's settings.
    pub fn mirror_paste(&mut self, source_tab_id: &str, text: &str, cx: &mut Context<Self>) {
        let targets: Vec<String> = self
            .multi_exec
            .read(cx)
            .state()
            .mirrored_tab_ids(source_tab_id)
            .into_iter()
            .filter(|id| id != source_tab_id)
            .collect();
        for target in targets {
            self.paste_into_session(&target, text, None, cx);
        }
    }

    /// `toggleMultiExec` (the keyboard command).
    pub fn toggle_multi_exec(&mut self, cx: &mut Context<Self>) {
        let tabs = self.tabs.clone();
        self.multi_exec
            .update(cx, |multi, cx| multi.toggle(&tabs, cx));
    }

    /// The workspace dialog.
    pub fn open_workspaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspaces) = self.workspaces.clone() else {
            return;
        };
        let dialog = open_workspaces(
            workspaces,
            WorkspaceDialogContext {
                tabs: self.tabs.tabs.clone(),
            },
            window,
            cx,
        );
        self.mount_overlay(
            "workspaces",
            dialog,
            window,
            cx,
            |this, _, event: &WorkspaceDialogEvent, window, cx| match event.clone() {
                WorkspaceDialogEvent::Close => this.close_overlay("workspaces", window, cx),
                WorkspaceDialogEvent::NewWindow { name } => {
                    this.close_overlay("workspaces", window, cx);
                    crate::ui::open_main_window(
                        crate::ui::MainWindowLaunch::Workspace { id: None, name },
                        cx,
                    );
                }
                WorkspaceDialogEvent::OpenInNewWindow { id, name } => {
                    this.close_overlay("workspaces", window, cx);
                    crate::ui::open_main_window(
                        crate::ui::MainWindowLaunch::Workspace { id: Some(id), name },
                        cx,
                    );
                }
                WorkspaceDialogEvent::Reconnect(ids) => {
                    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
                    this.tabs.reconnect(&ids, None);
                    this.tabs_changed(cx);
                }
                WorkspaceDialogEvent::ReconnectAll => {
                    this.tabs.reconnect_all(None);
                    this.tabs_changed(cx);
                }
                WorkspaceDialogEvent::ForceReconnectAll => {
                    this.close_overlay("workspaces", window, cx);
                    this.run_command("terminal.force-reconnect-all", window, cx);
                }
            },
        );
    }

    /// Show a saved workspace in this window (a launch or another window asked).
    pub fn open_workspace_by_id(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        let Some(workspaces) = self.workspaces.clone() else {
            return;
        };
        if workspaces.read(cx).state().active_id.as_deref() == Some(id) {
            return;
        }
        if workspaces.read(cx).focus_open_workspace(id) {
            self.show_toast(
                Status::Info,
                format!("Switched to the window showing “{name}”."),
                cx,
            );
            return;
        }
        let id = id.to_string();
        cx.defer(move |cx| {
            workspaces.update(cx, |workspaces, cx| workspaces.open(&id, cx).detach());
        });
    }

    /// Session history (`openHistory`).
    pub fn open_history(
        &mut self,
        query: &str,
        selected_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let dialog = open_history(backend, query, selected_id, window, cx);
        self.mount_overlay(
            "history",
            dialog,
            window,
            cx,
            |this, _, event: &HistoryEvent, window, cx| match event {
                HistoryEvent::Close => this.close_overlay("history", window, cx),
            },
        );
    }

    /// The command buttons editor.
    pub fn open_command_buttons(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.features.command_buttons.clone() else {
            return;
        };
        let dialog = open_command_buttons(model, window, cx);
        self.mount_overlay(
            "command-buttons",
            dialog,
            window,
            cx,
            |this, _, event: &CommandButtonsEvent, window, cx| match event {
                CommandButtonsEvent::Close => this.close_overlay("command-buttons", window, cx),
                CommandButtonsEvent::OpenPasswordSettings => this.open_settings(
                    Some(SettingsTarget::section(SettingsSection::Passwords)),
                    cx,
                ),
            },
        );
    }

    /// The command menu at the caret (`terminal.command-menu`).
    pub fn open_command_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(model) = self.features.command_buttons.clone() else {
            return false;
        };
        let menu = open_command_menu(model, window, cx);
        self.mount_overlay(
            "command-menu",
            menu,
            window,
            cx,
            |this, _, event: &CommandMenuEvent, window, cx| match event {
                CommandMenuEvent::Close => this.close_overlay("command-menu", window, cx),
                CommandMenuEvent::ManageCommands => {
                    this.close_overlay("command-menu", window, cx);
                    this.open_command_buttons(window, cx);
                }
            },
        );
        true
    }

    /// The send-secret menu (`terminal.send-secret`).
    pub fn open_send_secret_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(backend) = self.data.backend() else {
            return false;
        };
        let tab = self.tabs.active_tab().cloned();
        let connected = tab
            .as_ref()
            .is_some_and(|tab| tab.status == TabStatus::Connected && tab.terminal_id.is_some());
        let tab_id = tab.map(|tab| tab.id);
        let position = tab_id
            .as_deref()
            .and_then(|id| self.cursor_anchor(id, cx))
            .unwrap_or_else(|| crate::ui::features::window_center(window));
        let menu = open_send_secret_menu(backend, tab_id, connected, position, window, cx);
        self.mount_overlay(
            "send-secret",
            menu,
            window,
            cx,
            |this, _, event: &SendSecretMenuEvent, window, cx| match event {
                SendSecretMenuEvent::Close => this.close_overlay("send-secret", window, cx),
                SendSecretMenuEvent::ManageSecrets => {
                    this.close_overlay("send-secret", window, cx);
                    this.open_settings(
                        Some(SettingsTarget::section(SettingsSection::Passwords)),
                        cx,
                    );
                }
            },
        );
        true
    }

    /// The terminal caret, in window coordinates.
    pub fn cursor_anchor(&self, tab_id: &str, cx: &mut App) -> Option<Point<Pixels>> {
        let view = self.terminal_of(tab_id)?;
        view.read(cx).cursor_anchor()
    }

    /// The forwarding panel beside the canvas.
    pub fn set_forwarding_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.features.forwarding_open = open;
        if open && self.features.forwarding.is_none() {
            let Some(backend) = self.data.backend() else {
                return;
            };
            let panel = forwarding_panel(backend, cx);
            let subscription =
                cx.subscribe(
                    &panel,
                    |this, _, event: &ForwardingPanelEvent, cx| match event {
                        ForwardingPanelEvent::Close => this.set_forwarding_open(false, cx),
                        ForwardingPanelEvent::Prompt { prompt, reply } => {
                            use muxus_app::features::forwarding::start::DialPrompt;
                            let prompt = match prompt.clone() {
                                DialPrompt::Auth(info) => {
                                    crate::ui::prompts::SessionPrompt::Auth(info)
                                }
                                DialPrompt::HostKey(challenge) => {
                                    crate::ui::prompts::SessionPrompt::HostKey(challenge)
                                }
                            };
                            this.prompt_seq += 1;
                            let key = format!("tunnel-prompt:{}", this.prompt_seq);
                            this.queue_prompt(
                                key,
                                prompt,
                                Some(crate::ui::prompts::PromptReply::Tunnel(reply.clone())),
                                cx,
                            );
                        }
                    },
                );
            self.features._subscriptions.push(subscription);
            self.features.forwarding = Some(panel);
        }
        cx.notify();
    }

    /// Forwards changed elsewhere (a tunnel started from the launcher).
    pub fn refresh_forwards(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = &self.features.forwarding {
            panel.update(cx, |panel, cx| panel.refresh(cx));
        }
    }

    /// The top bar's badge follows every forward on every connection as the backend
    /// publishes them (the client polls `/api/forwards` every 5 s): tunnels started from
    /// the panel, the launcher, a host's menu or a terminal's `*Forward` lines alike.
    fn watch_forwards(&mut self, backend: muxus_session::Backend, cx: &mut Context<Self>) {
        let mut watch = backend.subscribe_forwards();
        self.features._tasks.push(cx.spawn(async move |this, cx| {
            loop {
                let summary = ForwardsSummary::of(&watch.borrow_and_update());
                let alive = this
                    .update(cx, |this, cx| {
                        if this.forwards != summary {
                            this.forwards = summary;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive || watch.changed().await.is_err() {
                    break;
                }
            }
        }));
    }

    /// The command bar where the preferences put it, when there is anything to show.
    pub fn render_action_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let bar = self.features.action_bar.clone()?;
        ActionBar::visible(cx).then(|| bar.into_any_element())
    }

    /// Whether the command bar sits below the canvas.
    pub fn command_bar_at_bottom(&self, cx: &App) -> bool {
        self.features.action_bar.is_some() && ActionBar::position(cx) == CommandBarPosition::Bottom
    }

    /// Note whether the vault's startup question is open (it keeps the keyboard then).
    pub fn sync_feature_state(&mut self, cx: &App) {
        self.features.vault_blocking = self
            .features
            .vault_unlock
            .as_ref()
            .is_some_and(|unlock| !unlock.read(cx).ready());
    }

    /// The window-wide layers: the question host, the secret sender's prompt, the vault's
    /// startup unlock and the update notice.
    pub fn feature_layers(&self) -> Vec<AnyElement> {
        let mut layers = Vec::new();
        if let Some(host) = &self.features.dialog_host {
            layers.push(host.clone().into_any_element());
        }
        if let Some(sender) = &self.features.secret_sender {
            layers.push(sender.clone().into_any_element());
        }
        if let Some(unlock) = &self.features.vault_unlock {
            layers.push(unlock.clone().into_any_element());
        }
        if let Some(notice) = &self.features.update_notice {
            layers.push(notice.clone().into_any_element());
        }
        layers
    }
}

/// How saved commands and secrets reach this process's terminals: through the shell of the
/// window they were run in.
fn install_command_hooks(cx: &mut App) {
    let mut hooks: CommandHooks = muxus_app::features::command_buttons::run::command_hooks(cx);
    hooks.send_input = Some(Rc::new(|tab_id, text, window, cx| {
        let Some(shell) = crate::ui::features::shell_for_window(window, cx) else {
            return false;
        };
        let tab_id = tab_id.to_string();
        let text = text.to_string();
        shell.update(cx, |shell, cx| {
            let connected = shell
                .tabs
                .tab(&tab_id)
                .is_some_and(|tab| tab.status == TabStatus::Connected);
            if !connected {
                return false;
            }
            let sent =
                shell.send_to_session(&tab_id, LinkInput::Data(text.clone().into_bytes()), cx);
            if sent {
                shell.mirror_input(&tab_id, text.as_bytes(), cx);
            }
            sent
        })
    }));
    hooks.focus_terminal = Some(Rc::new(|tab_id, window, cx| {
        if let Some(shell) = crate::ui::features::shell_for_window(window, cx) {
            let tab_id = tab_id.to_string();
            shell.update(cx, |shell, cx| {
                shell.focused_tab = None;
                shell.focus_terminal(&tab_id, window, cx);
            });
        }
    }));
    hooks.cursor_anchor = Some(Rc::new(|tab_id, window, cx| {
        let shell = crate::ui::features::shell_for_window(window, cx)?;
        let view = shell.read(cx).terminal_of(tab_id)?;
        view.read(cx).cursor_anchor()
    }));
    set_command_hooks(hooks, cx);
}
