//! The file browser and remote editors in the window (`AppShell.tsx`, `Sidebar.tsx`,
//! `file-browser.ts`): the SFTP panel beside an SSH terminal, the browser docked in the
//! sidebar, open files above their terminal, the SFTP window, and files dropped on terminals.

use std::collections::HashMap;

use muxus_app::features::sftp::docked::{DockedEvent, DockedFileBrowser, DockedTab, sidebar_rail};
use muxus_app::features::sftp::editor::workspace::{
    EditorSource, RemoteEditorEvent, RemoteEditorOptions, RemoteEditorWorkspace,
};
use muxus_app::features::sftp::panel::{SftpPanel, SftpPanelEvent, SftpPanelOptions};
use muxus_app::features::sftp::placement::{
    self, BrowserTabFacts, FileBrowserPlacement, FileBrowserPosition, FileBrowserStores,
    SidebarView,
};
use muxus_app::features::sftp::terminal_drop::{
    TerminalDropTab, drop_intent, handle_terminal_drop, terminal_drop_overlay,
    terminal_upload_corner,
};
use muxus_app::features::sftp::window::{SftpWindowLaunch, open_sftp_window};
use muxus_app::keymap::bindings::KeybindingOverrides;
use muxus_app::keymap::hints::chord_label;
use muxus_app::model::SessionProfile;
use muxus_app::state::tabs::{TabPatch, TabStatus, TerminalTab};
use muxus_term::drop_paths::{DroppedPathStyle, ShellQuoting};
use muxus_term::file_drop::DropSessionStatus;
use muxus_ui::gpui_kit::{
    AnyElement, AppContext, Context, Entity, ExternalPaths, FocusHandle, InteractiveElement,
    IntoElement, ParentElement, Styled, Subscription, Window, div,
};

use crate::ui::prefs::Side;
use crate::ui::shell::AppShell;

/// The window's file browsers and editors.
#[derive(Default)]
pub struct SftpState {
    /// The panel beside each tab's terminal, keyed by tab, with the connection it lists.
    panels: HashMap<String, (String, Entity<SftpPanel>, Subscription)>,
    /// Each tab's open files.
    editors: HashMap<String, (Entity<RemoteEditorWorkspace>, Subscription)>,
    /// The browser docked in the sidebar.
    docked: Option<(Entity<DockedFileBrowser>, Subscription)>,
    /// Hosts or files, while the browser is docked in the sidebar.
    pub sidebar_view: SidebarView,
    rail_focus: Option<FocusHandle>,
}

/// The shell as `file-browser.ts` sees it.
struct ShellStores<'a> {
    shell: &'a mut AppShell,
}

impl FileBrowserStores for ShellStores<'_> {
    fn placement(&self) -> FileBrowserPlacement {
        FileBrowserPlacement {
            position: self.shell.prefs.file_browser_position,
            sidebar_collapsed: self.shell.prefs.sidebar_collapsed,
            focus_mode: self.shell.focus_mode,
            sidebar_view: self.shell.sftp.sidebar_view,
        }
    }

    fn tab_sftp_open(&self, tab_id: &str) -> Option<bool> {
        self.shell.tabs.tab(tab_id).map(|tab| tab.sftp_open)
    }

    fn set_tab_sftp_open(&mut self, tab_id: &str, open: bool) {
        self.shell.tabs.update(
            tab_id,
            TabPatch {
                sftp_open: Some(open),
                ..Default::default()
            },
        );
    }

    fn set_position(&mut self, position: FileBrowserPosition) {
        self.shell.prefs.file_browser_position = position;
    }

    fn set_sidebar_collapsed(&mut self, collapsed: bool) {
        self.shell.prefs.sidebar_collapsed = collapsed;
    }

    fn set_sidebar_view(&mut self, view: SidebarView) {
        self.shell.sftp.sidebar_view = view;
    }
}

fn status_name(status: TabStatus) -> &'static str {
    match status {
        TabStatus::Connecting => "connecting",
        TabStatus::Connected => "connected",
        TabStatus::Interrupted => "interrupted",
        TabStatus::Closed => "closed",
        TabStatus::Idle => "idle",
    }
}

fn facts(tab: &TerminalTab) -> BrowserTabFacts {
    BrowserTabFacts {
        profile_kind: tab
            .profile
            .as_ref()
            .map(|profile| muxus_app::model::wire_name(&profile.kind())),
        status: status_name(tab.status).into(),
        sftp_available: tab.sftp_available,
    }
}

impl AppShell {
    fn with_stores(&mut self, change: impl FnOnce(&mut ShellStores<'_>)) {
        change(&mut ShellStores { shell: self });
    }

    fn placement_changed(&mut self, before: (FileBrowserPosition, bool), cx: &mut Context<Self>) {
        if before
            != (
                self.prefs.file_browser_position,
                self.prefs.sidebar_collapsed,
            )
        {
            self.prefs_changed(cx);
        }
        self.tabs_changed(cx);
    }

    /// `showFileBrowser`.
    pub fn show_file_browser(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let before = (
            self.prefs.file_browser_position,
            self.prefs.sidebar_collapsed,
        );
        self.with_stores(|stores| placement::show_file_browser(stores, tab_id));
        self.placement_changed(before, cx);
    }

    /// `toggleFileBrowser`.
    pub fn toggle_file_browser(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let before = (
            self.prefs.file_browser_position,
            self.prefs.sidebar_collapsed,
        );
        self.with_stores(|stores| placement::toggle_file_browser(stores, tab_id));
        self.placement_changed(before, cx);
    }

    /// `moveFileBrowser`.
    pub fn move_file_browser(
        &mut self,
        position: FileBrowserPosition,
        tab_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let before = (
            self.prefs.file_browser_position,
            self.prefs.sidebar_collapsed,
        );
        self.with_stores(|stores| placement::move_file_browser(stores, position, tab_id));
        self.placement_changed(before, cx);
    }

    /// Whether the active tab's browser is on screen (the top bar's button).
    pub fn file_browser_shown(&self, tab_id: &str) -> bool {
        placement::is_file_browser_shown(
            &FileBrowserPlacement {
                position: self.prefs.file_browser_position,
                sidebar_collapsed: self.prefs.sidebar_collapsed,
                focus_mode: self.focus_mode,
                sidebar_view: self.sftp.sidebar_view,
            },
            self.tabs.tab(tab_id).map(|tab| tab.sftp_open),
        )
    }

    /// `openEditor`: a remote (or local) file above the tab's terminal.
    pub fn open_editor(&mut self, tab_id: &str, path: &str, cx: &mut Context<Self>) {
        self.tabs.open_editor(tab_id, path);
        self.tabs.activate(tab_id);
        self.tabs_changed(cx);
    }

    fn open_sftp_window_for(&mut self, tab_id: &str, path: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.tab(tab_id).cloned() else {
            return;
        };
        let Some(conn_id) = tab.conn_id.clone() else {
            return;
        };
        open_sftp_window(
            SftpWindowLaunch {
                conn_id,
                title: tab.title.clone(),
                path,
            },
            cx,
        );
    }

    fn on_panel_event(&mut self, tab_id: &str, event: &SftpPanelEvent, cx: &mut Context<Self>) {
        match event {
            SftpPanelEvent::OpenFile(path) => self.open_editor(tab_id, path, cx),
            SftpPanelEvent::Move => {
                let target = match self.prefs.file_browser_position {
                    FileBrowserPosition::Pane => FileBrowserPosition::Sidebar,
                    FileBrowserPosition::Sidebar => FileBrowserPosition::Pane,
                };
                self.move_file_browser(target, Some(tab_id), cx);
            }
            SftpPanelEvent::OpenInNewWindow(path) => {
                self.open_sftp_window_for(tab_id, path.clone(), cx)
            }
            SftpPanelEvent::FollowTerminalFolder(follow) => {
                self.tabs.update(
                    tab_id,
                    TabPatch {
                        sftp_follow_terminal: Some(Some(*follow)),
                        ..Default::default()
                    },
                );
                self.tabs_changed(cx);
            }
            SftpPanelEvent::WidthChanged(width) => {
                self.prefs.sftp_panel_width = f64::from(*width);
                self.prefs_changed(cx);
            }
        }
    }

    /// Bring the file browsers and editors in line with the tabs.
    pub fn sync_sftp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.data.backend().is_none() {
            return;
        }
        let beside = self.prefs.file_browser_position == FileBrowserPosition::Pane;
        let sidebar_on_left = self.prefs.sidebar_position != Side::Right;
        let width = self.prefs.sftp_panel_width as f32;
        // Panels beside the terminals: the visible tab of each pane that asks for one.
        let mut wanted: HashMap<String, TerminalTab> = HashMap::new();
        if beside {
            for tab in &self.tabs.tabs {
                let visible =
                    muxus_app::state::workspace_layout::find_pane(&self.tabs.root, &tab.pane_id)
                        .and_then(|pane| pane.active_tab_id.as_deref())
                        == Some(tab.id.as_str());
                if visible
                    && tab.sftp_open
                    && tab.sftp_available != Some(false)
                    && tab.conn_id.is_some()
                {
                    wanted.insert(tab.id.clone(), tab.clone());
                }
            }
        }
        self.sftp.panels.retain(|tab_id, (conn, _, _)| {
            wanted
                .get(tab_id)
                .is_some_and(|tab| tab.conn_id.as_deref() == Some(conn.as_str()))
        });
        for (tab_id, tab) in wanted {
            let follow = tab.sftp_follow_terminal != Some(false);
            if let Some((_, panel, _)) = self.sftp.panels.get(&tab_id) {
                let cwd = tab.terminal_cwd.clone();
                let title = tab.title.clone();
                panel.update(cx, |panel, cx| {
                    panel.set_terminal_path(cwd, window, cx);
                    panel.set_follow_terminal_folder(follow, window, cx);
                    panel.set_host_label(Some(title), cx);
                    panel.set_sidebar_on_left(sidebar_on_left, cx);
                });
                continue;
            }
            let conn_id = tab.conn_id.clone().unwrap_or_default();
            let mut options = SftpPanelOptions::new(conn_id.clone());
            options.terminal_path = tab.terminal_cwd.clone();
            options.follow_terminal_folder = follow;
            options.host_label = Some(tab.title.clone());
            options.movable = true;
            options.opens_in_new_window = true;
            options.follow_toggle = true;
            options.width = width;
            options.sidebar_on_left = sidebar_on_left;
            let panel = cx.new(|cx| SftpPanel::new(options, window, cx));
            let id = tab_id.clone();
            let subscription = cx.subscribe(&panel, move |this, _, event: &SftpPanelEvent, cx| {
                this.on_panel_event(&id, event, cx)
            });
            self.sftp
                .panels
                .insert(tab_id, (conn_id, panel, subscription));
        }

        // Editors: every tab with open files keeps its workspace.
        let chord = chord_label("terminal.toggle-editor", &KeybindingOverrides::default());
        let active_pane = self.tabs.active_pane_id.clone();
        let with_files: HashMap<String, TerminalTab> = self
            .tabs
            .tabs
            .iter()
            .filter(|tab| !tab.editor_paths.is_empty() && tab.profile.is_some())
            .map(|tab| (tab.id.clone(), tab.clone()))
            .collect();
        self.sftp
            .editors
            .retain(|tab_id, _| with_files.contains_key(tab_id));
        for (tab_id, tab) in with_files {
            let source = match &tab.profile {
                Some(SessionProfile::Local(_)) => EditorSource::Local,
                _ => EditorSource::Sftp(tab.conn_id.clone()),
            };
            let editor_shown = tab.active_editor_path.is_some() && tab.terminal_shown != Some(true);
            let visible =
                muxus_app::state::workspace_layout::find_pane(&self.tabs.root, &tab.pane_id)
                    .and_then(|pane| pane.active_tab_id.as_deref())
                    == Some(tab.id.as_str());
            let active = visible && tab.pane_id == active_pane && editor_shown;
            if let Some((editor, _)) = self.sftp.editors.get(&tab_id) {
                let editor = editor.clone();
                editor.update(cx, |editor, cx| {
                    if editor.paths() != tab.editor_paths.as_slice()
                        || editor.active_path() != tab.active_editor_path.as_deref()
                    {
                        editor.set_paths(
                            tab.editor_paths.clone(),
                            tab.active_editor_path.clone(),
                            window,
                            cx,
                        );
                    }
                    editor.set_source(source, cx);
                    editor.set_terminal_shown(!editor_shown, window, cx);
                    editor.set_active(active, window, cx);
                });
                continue;
            }
            let mut options = RemoteEditorOptions::new(tab_id.clone(), source);
            options.paths = tab.editor_paths.clone();
            options.active_path = tab.active_editor_path.clone();
            options.active = active;
            options.terminal_shown = !editor_shown;
            options.terminal_entry = true;
            options.terminal_chord = chord.clone();
            let editor = cx.new(|cx| RemoteEditorWorkspace::new(options, window, cx));
            let id = tab_id.clone();
            let subscription =
                cx.subscribe(&editor, move |this, _, event: &RemoteEditorEvent, cx| {
                    match event {
                        RemoteEditorEvent::Activate(path) => this.tabs.activate_editor(&id, path),
                        RemoteEditorEvent::Close(path) => this.tabs.close_editor(&id, path),
                        RemoteEditorEvent::ShowTerminal => {
                            this.tabs.show_terminal(&id);
                        }
                    }
                    this.tabs_changed(cx);
                });
            self.sftp.editors.insert(tab_id, (editor, subscription));
        }

        // The docked browser follows the active tab.
        if !beside {
            let shown = self.sftp.sidebar_view == SidebarView::Files
                && !self.prefs.sidebar_collapsed
                && !self.focus_mode;
            let active = self.tabs.active_tab().map(|tab| DockedTab {
                tab_id: tab.id.clone(),
                conn_id: tab.conn_id.clone(),
                facts: facts(tab),
                title: tab.title.clone(),
                terminal_cwd: tab.terminal_cwd.clone(),
                follow_terminal_folder: tab.sftp_follow_terminal != Some(false),
            });
            let docked = match &self.sftp.docked {
                Some((docked, _)) => docked.clone(),
                None => {
                    let docked = cx.new(|_| DockedFileBrowser::new(shown));
                    let subscription =
                        cx.subscribe(&docked, |this, _, event: &DockedEvent, cx| match event {
                            DockedEvent::Panel { tab_id, event } => {
                                let tab_id = tab_id.clone();
                                this.on_panel_event(&tab_id, event, cx)
                            }
                            DockedEvent::MoveToPane { tab_id } => {
                                let tab_id = tab_id.clone();
                                this.move_file_browser(
                                    FileBrowserPosition::Pane,
                                    tab_id.as_deref(),
                                    cx,
                                )
                            }
                        });
                    self.sftp.docked = Some((docked.clone(), subscription));
                    docked
                }
            };
            docked.update(cx, |docked, cx| {
                docked.set_sidebar_on_left(sidebar_on_left, cx);
                docked.set_shown(shown, window, cx);
                docked.set_active_tab(active, window, cx);
            });
        } else {
            self.sftp.docked = None;
            self.sftp.sidebar_view = SidebarView::Hosts;
        }
    }

    /// A terminal tab's body: its open files above the terminal (the editors' strip stays
    /// while the terminal is in front), with the file browser beside them.
    pub fn render_session_body(
        &mut self,
        tab: &TerminalTab,
        terminal: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editor_shown = tab.active_editor_path.is_some() && tab.terminal_shown != Some(true);
        let mut column = div().flex_1().min_w_0().h_full().flex().flex_col();
        if let Some((editor, _)) = self.sftp.editors.get(&tab.id) {
            column = column.child(
                div()
                    .when_flex(editor_shown)
                    .min_h_0()
                    .child(editor.clone()),
            );
        }
        let group = format!("terminal-drop-{}", tab.id);
        let drop_tab = drop_tab(tab, &self.prefs.local_shell);
        let drop_tab_id = tab.id.clone();
        let intent = drop_intent(&drop_tab, false);
        let terminal_box = div()
            .id(gpui_id(&group))
            .group(group.clone())
            .relative()
            .flex_1()
            .min_h_0()
            .when_hidden(editor_shown)
            .child(terminal)
            .children(terminal_drop_overlay(group.clone(), &intent, cx))
            .children(
                tab.conn_id
                    .as_deref()
                    .and_then(|conn| terminal_upload_corner(conn, cx)),
            )
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                // Typed paths go in like a paste; uploads run on their own. Without the
                // backend (the documentation scenes) there is nothing to drop into.
                if this.data.backend().is_none() {
                    return;
                }
                let mut typed = None;
                handle_terminal_drop(
                    paths,
                    &drop_tab,
                    |text, _, _| typed = Some(text),
                    window,
                    cx,
                );
                if let Some(text) = typed {
                    this.paste_text(&drop_tab_id, &text, cx);
                }
            }));
        column = column.child(terminal_box);
        let mut row = div().size_full().flex().child(column);
        if let Some((_, panel, _)) = self.sftp.panels.get(&tab.id) {
            row = row.child(panel.clone());
        }
        row.into_any_element()
    }

    /// The sidebar's hosts tree, or with the browser docked, the rail and both views.
    pub fn render_sidebar_views(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (Option<AnyElement>, AnyElement) {
        let docked = self.prefs.file_browser_position == FileBrowserPosition::Sidebar;
        let view = if docked {
            self.sftp.sidebar_view
        } else {
            SidebarView::Hosts
        };
        let hosts = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .when_hidden(view != SidebarView::Hosts)
            .child(self.sidebar.clone());
        let mut content = div().flex().flex_col().flex_1().min_h_0().child(hosts);
        if docked && let Some((browser, _)) = &self.sftp.docked {
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .when_hidden(view != SidebarView::Files)
                    .child(browser.clone()),
            );
        }
        let rail = docked.then(|| {
            let focus = self
                .sftp
                .rail_focus
                .get_or_insert_with(|| cx.focus_handle())
                .clone();
            let on_right = self.prefs.sidebar_position == Side::Right;
            let shell = cx.entity().downgrade();
            sidebar_rail(
                view,
                on_right,
                &focus,
                move |view, _, cx| {
                    let _ = shell.update(cx, |this, cx| {
                        this.sftp.sidebar_view = view;
                        cx.notify();
                    });
                },
                cx,
            )
            .into_any_element()
        });
        (rail, content.into_any_element())
    }
}

fn gpui_id(name: &str) -> muxus_ui::gpui_kit::ElementId {
    muxus_ui::gpui_kit::ElementId::Name(name.to_string().into())
}

/// What the drop rules read of a tab.
fn drop_tab(tab: &TerminalTab, local_shell: &str) -> TerminalDropTab {
    let profile_kind = tab
        .profile
        .as_ref()
        .map(|profile| muxus_app::model::wire_name(&profile.kind()))
        .unwrap_or_default();
    let status = match tab.status {
        TabStatus::Connected => DropSessionStatus::Connected,
        TabStatus::Interrupted => DropSessionStatus::Interrupted,
        TabStatus::Connecting | TabStatus::Idle => DropSessionStatus::Connecting,
        TabStatus::Closed => DropSessionStatus::Closed,
    };
    let path_style = match &tab.profile {
        Some(SessionProfile::Local(profile)) => {
            let args: Vec<&str> = profile.args.iter().flatten().map(String::as_str).collect();
            let default_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            muxus_term::drop_paths::local_shell_path_style(
                profile
                    .shell
                    .as_deref()
                    .or(Some(local_shell).filter(|s| *s != "auto")),
                &args,
                &default_shell,
                std::env::consts::OS,
            )
        }
        _ => DroppedPathStyle::new(ShellQuoting::Posix),
    };
    TerminalDropTab {
        profile_kind,
        status,
        conn_id: tab.conn_id.clone(),
        sftp_available: tab.sftp_available,
        cwd: tab.terminal_cwd.clone(),
        title: tab.title.clone(),
        path_style,
    }
}

/// Small layout helpers for the views above.
trait LayoutExt: Styled + Sized {
    fn when_flex(self, grow: bool) -> Self {
        if grow {
            self.flex_1()
        } else {
            self.flex_none()
        }
    }

    fn when_hidden(self, hidden: bool) -> Self {
        if hidden { self.hidden() } else { self }
    }
}

impl<T: Styled> LayoutExt for T {}
