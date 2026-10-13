//! The window: `client/src/layout/AppShell.tsx`. Top bar, the optional
//! command bar, the sidebar beside the pane canvas, and the status bar.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use muxus_app::data_source::DataSource;
use muxus_app::features::settings::{PrefsChanged, SettingsPrefs};
use muxus_app::hosts::ManagedHost;
use muxus_app::model::Catalog;
use muxus_app::sidebar::folder_prefs::FolderPrefs;
use muxus_app::sidebar::live_counts::{LiveStatus, live_host_counts};
use muxus_app::sidebar_width::clamp_sidebar_width;
use muxus_app::state::session_actions::{LaunchGuard, connect_managed_host, connect_target};
use muxus_app::state::tabs::{TabStatus, TabsState};
use muxus_ui::gpui_kit::{
    AnyWindowHandle, AppContext, Context, Entity, InteractiveElement, IntoElement, MouseButton,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, Styled, Subscription,
    Window, WindowAppearance, div,
};
use muxus_ui::theme::{ActiveTheme, Mode, Status, Theme};

use crate::ui::confirm::ConfirmDialog;
use crate::ui::paste_dialog::PasteDialog;
use crate::ui::prefs::{Prefs, Side, ThemeMode};
use crate::ui::prompts::{PromptDialog, SessionPrompt};
use crate::ui::sidebar::{SessionSidebar, SidebarEvent};
use crate::ui::terminal::TabTerminal;

/// Menus owned by the window chrome (the sidebar owns its own).
#[derive(Clone, Debug)]
pub enum ShellMenu {
    Appearance {
        position: Point<Pixels>,
    },
    TerminalActions {
        position: Point<Pixels>,
    },
    Tab {
        tab_id: String,
        position: Point<Pixels>,
    },
    Pane {
        pane_id: String,
        position: Point<Pixels>,
    },
    AllTabs {
        pane_id: String,
        position: Point<Pixels>,
    },
    StatusBar {
        position: Point<Pixels>,
    },
    /// The open sessions of a host (`HostSessionsMenu.tsx`).
    HostSessions {
        host: Box<ManagedHost>,
        position: Point<Pixels>,
    },
    /// A terminal's right-click menu, with the selection at the click.
    Terminal {
        tab_id: String,
        position: Point<Pixels>,
        selection: String,
    },
}

/// Painted bounds of the controls menus open from.
#[derive(Default)]
pub struct Anchors {
    pub appearance: muxus_ui::widgets::AnchorBounds,
    pub terminal: muxus_ui::widgets::AnchorBounds,
    pub status_items: muxus_ui::widgets::AnchorBounds,
}

/// A sidebar resize in progress.
#[derive(Clone, Copy, Debug)]
pub struct SidebarDrag {
    pub start_x: f32,
    pub start_width: f32,
}

pub struct AppShell {
    pub prefs: Prefs,
    pub tabs: TabsState,
    pub catalog: Catalog,
    pub sidebar: Entity<SessionSidebar>,
    pub launch_guard: LaunchGuard,
    pub menu: Option<ShellMenu>,
    pub focus_mode: bool,
    pub workspace_name: String,
    pub sidebar_drag: Option<SidebarDrag>,
    /// Forwards the backend reports (the top bar badge). The backend is not
    /// wired yet; the demo scenes set the count the docs screenshots show.
    /// The top bar's forwarding badge and tooltip, live from the backend.
    pub forwards: muxus_app::features::forwarding::model::ForwardsSummary,
    pub toast: Option<crate::ui::toast::Toast>,
    pub anchors: Anchors,
    pub toast_seq: u64,
    /// The tab-switch modifier (Alt) is held: tab numbers show in place of icons.
    pub alt_held: bool,
    pub split_drag: Option<crate::ui::panes::SplitDrag>,
    pub geometry: std::rc::Rc<crate::ui::panes::PaneGeometry>,
    /// The pane whose strip the pointer last focused, for strip focus styling.
    pub window_active: bool,
    /// Where hosts come from and sessions go.
    pub data: Arc<dyn DataSource>,
    /// The terminal of every terminal tab, open or hidden.
    pub terminals: HashMap<String, TabTerminal>,
    pub next_generation: u64,
    /// The tab whose terminal was last given the keyboard.
    pub focused_tab: Option<String>,
    pub paste_dialog: Option<(Entity<PasteDialog>, Subscription)>,
    pub confirm: Option<(Entity<ConfirmDialog>, Subscription)>,
    /// Questions sessions asked, per tab, oldest first.
    pub prompts: Vec<(String, SessionPrompt)>,
    /// Where answers go for questions no terminal asked (tunnel dials).
    pub prompt_replies: HashMap<String, crate::ui::prompts::PromptReply>,
    /// The dialog of the oldest question.
    pub prompt_dialog: Option<(Entity<PromptDialog>, Subscription)>,
    /// The pending write of changed preferences.
    prefs_save: Option<muxus_ui::gpui_kit::Task<()>>,
    /// The status bar's readings of the active session's host.
    pub host_stats: crate::ui::status_bar::HostStatsPoller,
    /// The window's features (`ui::window_features`).
    pub features: crate::ui::window_features::WindowFeatures,
    /// The workspace runtime of this window.
    pub workspaces: Option<Entity<muxus_app::features::workspaces::Workspaces>>,
    /// Mirrored input (multi-execution).
    pub multi_exec: Entity<muxus_app::features::command_buttons::MultiExec>,
    /// Numbers questions that have no tab of their own.
    pub prompt_seq: u64,
    /// Frames of the window are shown (not minimized, covered or on a sleeping display).
    pub window_visible: bool,
    /// Sessions moved here from another window, by tab: (terminal id, buffer).
    pub transfers: HashMap<String, (String, String)>,
    /// Restored tabs whose stored output was already fetched (`snapshotFetchedRef`).
    pub snapshots_fetched: std::collections::HashSet<String>,
    /// Each pane's tab row scroll position.
    pub tab_scroll: HashMap<String, muxus_ui::gpui_kit::ScrollHandle>,
    /// File browsers and editors (`ui::sftp`).
    pub sftp: crate::ui::sftp::SftpState,
    /// Remote desktop and management tabs' views.
    pub session_views: crate::ui::session_views::SessionViews,
    /// Feature dialogs drawn over the window (`ui::overlays`).
    pub overlays: Vec<crate::ui::overlays::Overlay>,
    /// This shell's window, for routing app-wide requests to it.
    pub window_handle: Option<AnyWindowHandle>,
    _subscriptions: Vec<Subscription>,
}

impl AppShell {
    pub fn new(
        data: Arc<dyn DataSource>,
        catalog: Catalog,
        folders: FolderPrefs,
        prefs: Prefs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sidebar = cx.new(|cx| SessionSidebar::new(catalog.clone(), folders, window, cx));
        sidebar.update(cx, |sidebar, _| {
            sidebar.set_open_gesture(prefs.sidebar_open_gesture);
            sidebar.set_on_right(prefs.sidebar_position == Side::Right);
        });
        let mut subscriptions = vec![
            cx.subscribe_in(&sidebar, window, Self::on_sidebar_event),
            cx.observe_window_appearance(window, |this, window, cx| {
                if this.prefs.theme_mode == ThemeMode::Os {
                    this.apply_theme(window, cx);
                }
            }),
            // The status bar's host statistics pause while no frame of the window is shown
            // (`document.visibilityState === 'hidden'`), and resume when it shows again.
            cx.observe_window_visibility(window, |this, visibility, _, cx| {
                let visible = matches!(visibility, muxus_ui::gpui_kit::WindowVisibility::Visible);
                if this.window_visible != visible {
                    this.window_visible = visible;
                    if visible {
                        this.host_stats.key = None;
                    }
                    cx.notify();
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                this.window_active = window.is_window_active();
                cx.notify();
            }),
            // Folder collapse, order and styles live in the preferences.
            cx.observe(&sidebar, |this, sidebar, cx| {
                let folders = sidebar.read(cx).folders.clone();
                if this.folder_prefs_differ(&folders) {
                    this.prefs.sidebar_collapsed_folders = folders.collapsed;
                    this.prefs.sidebar_folder_styles = folders.styles;
                    this.prefs.sidebar_folder_order = folders.order;
                    this.prefs.sidebar_empty_folders = folders.empty_folders;
                    this.prefs_changed(cx);
                }
            }),
        ];
        // Settings, other windows and restored backups change the shared preferences; this
        // window applies them live (theme, fonts, schemes, sidebar, status bar).
        if let Some(shared) = SettingsPrefs::global(cx) {
            subscriptions.push(cx.subscribe_in(
                &shared,
                window,
                |this, _, event: &PrefsChanged, window, cx| {
                    if this.prefs != event.current {
                        this.apply_prefs(event.current.clone(), window, cx);
                    }
                },
            ));
        }
        crate::ui::features::register_shell(cx.entity().downgrade(), cx);
        let mut shell = Self {
            prefs,
            tabs: TabsState::default(),
            catalog,
            sidebar,
            launch_guard: LaunchGuard::default(),
            menu: None,
            focus_mode: false,
            workspace_name: "Unsaved workspace".into(),
            sidebar_drag: None,
            alt_held: false,
            toast: None,
            anchors: Anchors::default(),
            toast_seq: 0,
            forwards: Default::default(),
            split_drag: None,
            geometry: Default::default(),
            window_active: true,
            data,
            terminals: HashMap::new(),
            next_generation: 0,
            focused_tab: None,
            paste_dialog: None,
            confirm: None,
            prompts: Vec::new(),
            prompt_dialog: None,
            prompt_replies: HashMap::new(),
            prefs_save: None,
            host_stats: Default::default(),
            overlays: Vec::new(),
            session_views: HashMap::new(),
            sftp: Default::default(),
            tab_scroll: HashMap::new(),
            transfers: HashMap::new(),
            snapshots_fetched: Default::default(),
            window_visible: true,
            features: Default::default(),
            workspaces: None,
            multi_exec: cx.new(|_| muxus_app::features::command_buttons::MultiExec::new()),
            prompt_seq: 0,
            window_handle: Some(window.window_handle()),
            _subscriptions: subscriptions,
        };
        shell.apply_theme(window, cx);
        shell.load_catalog(cx);
        if crate::ui::perf::perf().is_some() {
            // The heartbeat: a timer on the UI thread that notes how late it wakes.
            cx.spawn(async move |_, cx| {
                const BEAT: std::time::Duration = std::time::Duration::from_millis(4);
                loop {
                    if !crate::ui::perf::perf().is_some_and(|perf| perf.beating()) {
                        cx.background_executor()
                            .timer(std::time::Duration::from_secs(1))
                            .await;
                        continue;
                    }
                    let due = std::time::Instant::now() + BEAT;
                    cx.background_executor().timer(BEAT).await;
                    if let Some(perf) = crate::ui::perf::perf() {
                        perf.heartbeat(std::time::Instant::now().saturating_duration_since(due));
                    }
                }
            })
            .detach();
            cx.spawn(async move |_, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(1))
                        .await;
                    if let Some(perf) = crate::ui::perf::perf() {
                        perf.tick();
                    }
                }
            })
            .detach();
        }
        shell
    }

    fn folder_prefs_differ(&self, folders: &FolderPrefs) -> bool {
        self.prefs.sidebar_collapsed_folders != folders.collapsed
            || self.prefs.sidebar_folder_styles != folders.styles
            || self.prefs.sidebar_folder_order != folders.order
            || self.prefs.sidebar_empty_folders != folders.empty_folders
    }

    /// The preferences changed: store them (debounced, the way the client's
    /// persisted store writes `client-state.json`) and apply what terminals read.
    pub fn prefs_changed(&mut self, cx: &mut Context<Self>) {
        // With the shared preferences installed, they write `client-state.json` and tell the
        // other windows; this window already shows the change.
        if let Some(shared) = SettingsPrefs::global(cx) {
            let prefs = self.prefs.clone();
            shared.update(cx, |shared, cx| {
                shared.update(cx, |current| *current = prefs)
            });
            self.refresh_terminal_settings(cx);
            cx.notify();
            return;
        }
        let save = self.data.save_prefs(self.prefs.clone());
        self.prefs_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(300))
                .await;
            if let Err(message) = save.await {
                let _ = this.update(cx, |this, cx| {
                    this.show_toast(
                        Status::Error,
                        format!("Could not save the preferences: {message}"),
                        cx,
                    )
                });
            }
        }));
        self.refresh_terminal_settings(cx);
        cx.notify();
    }

    /// Replace the preferences (another window or the settings changed them).
    pub fn apply_prefs(&mut self, prefs: Prefs, window: &mut Window, cx: &mut Context<Self>) {
        self.prefs = prefs;
        let folders = FolderPrefs {
            collapsed: self.prefs.sidebar_collapsed_folders.clone(),
            styles: self.prefs.sidebar_folder_styles.clone(),
            order: self.prefs.sidebar_folder_order.clone(),
            empty_folders: self.prefs.sidebar_empty_folders.clone(),
        };
        let gesture = self.prefs.sidebar_open_gesture;
        let on_right = self.prefs.sidebar_position == Side::Right;
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.folders = folders;
            sidebar.set_open_gesture(gesture);
            sidebar.set_on_right(on_right);
            sidebar.rebuild();
            cx.notify();
        });
        self.apply_theme(window, cx);
        self.refresh_terminal_settings(cx);
        cx.notify();
    }

    /// Read the host catalog from the data source and show it.
    pub fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let load = self.data.load_catalog();
        cx.spawn(async move |this, cx| {
            let result = load.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(catalog) => this.set_catalog(catalog, cx),
                Err(message) => this.show_toast(
                    Status::Error,
                    format!("Could not load the hosts: {message}"),
                    cx,
                ),
            });
        })
        .detach();
    }

    pub fn set_catalog(&mut self, catalog: Catalog, cx: &mut Context<Self>) {
        if self.catalog == catalog {
            return;
        }
        self.catalog = catalog.clone();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_catalog(catalog, cx));
        // Host schemes and highlighting follow the catalog.
        self.refresh_terminal_settings(cx);
        cx.notify();
    }

    /// The light/dark mode `themeMode` resolves to in this window.
    pub fn resolved_mode(&self, window: &Window) -> Mode {
        match self.prefs.theme_mode {
            ThemeMode::Light => Mode::Light,
            ThemeMode::Dark => Mode::Dark,
            ThemeMode::Os => match window.appearance() {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Mode::Dark,
                _ => Mode::Light,
            },
        }
    }

    pub fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = self.resolved_mode(window);
        if cx.theme().mode != mode {
            Theme::install(mode, cx);
            // Terminals follow the light or dark scheme preference.
            self.refresh_terminal_settings(cx);
        }
    }

    pub fn set_theme_mode(&mut self, mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        self.prefs.theme_mode = mode;
        self.apply_theme(window, cx);
        self.prefs_changed(cx);
    }

    fn on_sidebar_event(
        &mut self,
        _: &Entity<SessionSidebar>,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        match event {
            SidebarEvent::OpenHost {
                host,
                new_session,
                repeat,
                anchor,
            } => self.open_host(host, *new_session || *repeat, *anchor, cx),
            SidebarEvent::OpenLocalTerminal => self.open_local(None, cx),
            SidebarEvent::QuickConnect(target) => {
                connect_target(
                    &mut self.tabs,
                    &mut self.launch_guard,
                    target,
                    target,
                    None,
                    now,
                );
            }
            SidebarEvent::AddHost { prefill } => self.add_host(prefill.clone(), window, cx),
            SidebarEvent::Copied(text) => {
                self.show_toast(Status::Success, format!("Copied \"{text}\""), cx)
            }
            SidebarEvent::MoveSidebar => {
                self.prefs.sidebar_position = if self.prefs.sidebar_position == Side::Right {
                    Side::Left
                } else {
                    Side::Right
                };
                let on_right = self.prefs.sidebar_position == Side::Right;
                self.sidebar
                    .update(cx, |sidebar, _| sidebar.set_on_right(on_right));
                self.prefs_changed(cx);
            }
            SidebarEvent::Action(action) => self.on_sidebar_action(action, window, cx),
        }
        self.tabs_changed(cx);
    }

    /// `activateHost`: a host that already has tabs in this window lists them first, so a
    /// click lands on the session you had instead of stacking up duplicates. A middle-click,
    /// a repeat click and any host with nothing open connect straight away.
    pub fn open_host(
        &mut self,
        host: &ManagedHost,
        connect_now: bool,
        anchor: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let key = host.key();
        let has_sessions = !muxus_app::features::launcher::sessions::host_session_tabs(
            &self.tabs.root,
            &self.tabs.tabs,
            &key,
        )
        .is_empty();
        match anchor {
            Some(position) if has_sessions && !connect_now => {
                self.menu = Some(ShellMenu::HostSessions {
                    host: Box::new(host.clone()),
                    position,
                });
                cx.notify();
            }
            _ => {
                connect_managed_host(
                    &mut self.tabs,
                    &mut self.launch_guard,
                    host,
                    None,
                    Instant::now(),
                );
                self.tabs_changed(cx);
            }
        }
    }

    /// Push everything derived from the tabs to the views that show it.
    pub fn tabs_changed(&mut self, cx: &mut Context<Self>) {
        let counts = live_host_counts(self.tabs.tabs.iter().map(|tab| {
            let status = match tab.status {
                TabStatus::Connected => LiveStatus::Connected,
                TabStatus::Connecting => LiveStatus::Connecting,
                _ => LiveStatus::Other,
            };
            (tab.profile.as_ref(), status)
        }));
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_live(counts, cx));
        if self.workspaces.is_none()
            && self.workspace_name == "Unsaved workspace"
            && !self.tabs.tabs.is_empty()
        {
            self.workspace_name = "Workspace 1".into();
        }
        self.publish_layout(cx);
        cx.notify();
    }

    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    fn show_sidebar(&self) -> bool {
        !self.focus_mode && !self.prefs.sidebar_collapsed
    }

    fn on_drag_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.sidebar_drag else {
            return;
        };
        if !event.dragging() {
            return;
        }
        let delta = muxus_ui::zoom::unzoom(event.position.x) - drag.start_x;
        let direction = if self.prefs.sidebar_position == Side::Right {
            -1.
        } else {
            1.
        };
        let container = muxus_ui::zoom::unzoom(window.viewport_size().width);
        self.prefs.sidebar_width = clamp_sidebar_width(
            (drag.start_width + delta * direction) as f64,
            container as f64,
        );
        cx.notify();
    }

    fn on_drag_end(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_drag.take().is_some() {
            self.prefs_changed(cx);
        }
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        muxus_ui::zoom::apply_to_window(window);
        self.sync_feature_state(cx);
        self.sync_terminals(window, cx);
        self.sync_session_views(window, cx);
        self.sync_sftp(window, cx);
        self.sync_prompt_dialog(window, cx);
        let theme = *cx.theme();
        let p = theme.palette;
        let show_sidebar = self.show_sidebar();
        let on_right = self.prefs.sidebar_position == Side::Right;
        let command_bar_bottom = self.command_bar_at_bottom(cx);

        let mut middle = div().flex().flex_1().min_h_0();
        let sidebar = show_sidebar.then(|| self.render_sidebar(window, cx));
        let mut canvas = div().flex_1().min_w_0().min_h_0().flex().child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(self.render_pane_canvas(window, cx)),
        );
        if self.features.forwarding_open
            && let Some(panel) = self.features.forwarding.clone()
        {
            canvas = canvas.child(panel);
        }
        middle = if on_right {
            middle.child(canvas).children(sidebar)
        } else {
            middle.children(sidebar).child(canvas)
        };

        let mut root = div()
            .id("app-shell")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(p.background_default)
            .text_color(p.text.primary)
            .font_family(muxus_ui::theme::fonts::UI)
            // App shortcuts run before the terminal sees the key, so a bound
            // chord never reaches the shell (`installShortcuts`, capture phase).
            .capture_key_down(cx.listener(Self::on_key_down))
            .on_modifiers_changed(cx.listener(
                |this, event: &muxus_ui::gpui_kit::ModifiersChangedEvent, _, cx| {
                    if this.alt_held != event.modifiers.alt {
                        this.alt_held = event.modifiers.alt;
                        cx.notify();
                    }
                },
            ))
            .on_mouse_move(cx.listener(Self::on_drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_drag_end))
            .child(self.render_top_bar(window, cx));
        if !self.focus_mode && !command_bar_bottom {
            root = root.children(self.render_action_bar(cx));
        }
        root = root.child(middle);
        if !self.focus_mode && command_bar_bottom {
            root = root.children(self.render_action_bar(cx));
        }
        if !self.focus_mode && self.prefs.show_status_bar {
            root = root.child(self.render_status_bar(window, cx));
        }
        if let Some(menu) = self.menu.clone() {
            root = root.child(self.render_shell_menu(menu, window, cx));
        }
        if let Some((dialog, _)) = &self.paste_dialog {
            root = root.child(dialog.clone());
        }
        if let Some((dialog, _)) = &self.confirm {
            root = root.child(dialog.clone());
        }
        if let Some((dialog, _)) = &self.prompt_dialog {
            root = root.child(dialog.clone());
        }
        root = root.children(self.feature_layers());
        root = root.children(self.overlays.iter().map(|overlay| overlay.view.clone()));
        if let Some(toast) = self.render_toast(window, cx) {
            root = root.child(toast);
        }
        if let Some(perf) = crate::ui::perf::perf() {
            root = root.child(
                muxus_ui::gpui_kit::canvas(|_, _, _| {}, move |_, _, _, _| perf.frame())
                    .absolute()
                    .size_0(),
            );
        }
        muxus_ui::widgets::window_frame::decorate_window(root, window, cx)
    }
}
