//! The hosts sidebar: `client/src/components/SessionSidebar.tsx` with the
//! tree of `components/sidebar/{HostTree,HostRow,FolderRow}.tsx`.
//!
//! One flattened row array (`flatten_visible_tree`) backs rendering, arrow
//! keys and the match highlight, so they cannot disagree about what is on
//! screen. Only the rows near the viewport are built; spacers of the exact
//! row pitch stand in for the rest (`tree_window`), so a catalog of
//! thousands of hosts renders like a screenful.

use std::collections::{BTreeMap, BTreeSet};

use muxus_app::hosts::tree::{BuildOptions, build_host_tree, flatten_visible_tree};
use muxus_app::hosts::{
    HostTree, ManagedHost, RowNode, VisibleNode, best_managed_host_match, group_managed_hosts,
    managed_host_address,
};
use muxus_app::model::Catalog;
use muxus_app::sidebar::folder_icons::folder_icon_name;
use muxus_app::sidebar::folder_prefs::FolderPrefs;
use muxus_app::sidebar::host_details::host_detail_lines;
use muxus_app::sidebar::kind_icon::host_kind_icon_name;
use muxus_app::sidebar::live_counts::LiveCounts;
use muxus_app::sidebar::row_clicks::{
    HostActivation, HostClickOutcome, SidebarOpenGesture, click_opens_row, host_click_outcome,
};
use muxus_app::sidebar::row_style::{
    TREE_ROW_GAP, TREE_ROW_HEIGHT, TREE_ROW_PITCH, indent_guides, indent_px,
};
use muxus_app::sidebar::tree_navigation::{
    TreeNavAction, TreeNavKey, focus_after_change, tree_nav_action,
};
use muxus_app::sidebar::tree_selection::{
    Selection, TreeSelectionAnchor, range_selection, toggle_selection, visible_host_keys,
};
use muxus_app::sidebar::tree_window::{
    RowSegment, RowWindow, TREE_OVERSCAN, next_row_window, rendered_row_indices, row_segments,
    visible_row_span,
};
use muxus_app::state::session_actions::is_quick_connect_target;
use muxus_ui::color::try_hex;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, Pixels, Point, Render, ScrollHandle, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Transformation, Window, div, px, radians,
};
use muxus_ui::icon::icon;
use muxus_ui::label::{Label, label, paragraph};
use muxus_ui::text::TextStyleExt;
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::{
    AnchorBounds, Button, IconButton, Placement, TextField, Tooltip, bounds_probe, scrollbar,
};

use crate::ui::sidebar_menus::SidebarMenu;

/// A right-click handler on a fixed row.
type ContextHandler = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>;

/// What the sidebar asks the window to do.
#[derive(Clone, Debug)]
pub enum SidebarEvent {
    /// Click or Enter on a host. `new_session` for a middle-click; `repeat` for the second
    /// click of a double-click; `anchor` where a click landed (the sessions menu opens there).
    OpenHost {
        host: ManagedHost,
        new_session: bool,
        repeat: bool,
        anchor: Option<Point<Pixels>>,
    },
    OpenLocalTerminal,
    QuickConnect(String),
    AddHost {
        prefill: Option<String>,
    },
    /// A host's connection command went to the clipboard.
    Copied(String),
    /// Dock the sidebar on the other side of the window.
    MoveSidebar,
    /// A host or folder action the window carries out (dialogs, store writes).
    Action(SidebarAction),
}

/// What the sidebar's menus ask the window to do (`SessionSidebar.tsx`'s handlers).
#[derive(Clone, Debug)]
pub enum SidebarAction {
    OpenInNewWindow(ManagedHost),
    OpenSftp(ManagedHost),
    /// Swap a host with its neighbour among its siblings.
    MoveHost {
        key: String,
        delta: isize,
    },
    /// Pick a folder for these hosts.
    MoveHosts(Vec<ManagedHost>),
    Organize(ManagedHost),
    /// One host opens its editor; several open the bulk editor.
    EditHosts(Vec<ManagedHost>),
    Duplicate(ManagedHost),
    /// "Add gNMI/NETCONF host for it…".
    NewHost(muxus_app::features::host_editor::view::NewHost),
    DeleteHosts(Vec<ManagedHost>),
    LaunchHosts {
        label: String,
        hosts: Vec<ManagedHost>,
    },
    /// Write these hosts' order alphabetically.
    SortHosts(Vec<ManagedHost>),
    /// Hosts dragged into a folder or to a new place among their siblings.
    DropHosts(crate::ui::sidebar_dnd::HostDrop),
    /// A folder dragged into another folder or beside one.
    DropFolder(crate::ui::sidebar_dnd::FolderDrop),
    NewFolder {
        parent: Option<String>,
    },
    EditFolder {
        path: String,
    },
    DeleteFolder {
        key: String,
        path: String,
        label: String,
    },
    LocalShellSettings,
}

pub struct SessionSidebar {
    pub(crate) catalog: Catalog,
    pub folders: FolderPrefs,
    pub(crate) search: Entity<InputState>,
    pub(crate) filter: String,
    /// Folders collapsed during a search; discarded when the query changes.
    search_collapsed: BTreeSet<String>,
    /// Hosts picked for a bulk action. Kept while searching, so a selection can span queries.
    pub(crate) selection: Selection,
    anchor: Option<TreeSelectionAnchor>,
    focused_key: Option<String>,
    last_index: usize,
    live: BTreeMap<String, LiveCounts>,
    scroll: ScrollHandle,
    row_window: RowWindow,
    open_gesture: SidebarOpenGesture,
    pub(crate) on_right: bool,
    tree_focus: FocusHandle,
    pub(crate) menu: Option<SidebarMenu>,
    pub(crate) tree: HostTree,
    /// Where the latest host click landed, for the sessions menu.
    pub(crate) click_position: Option<Point<Pixels>>,
    /// The drag in progress (`ui::sidebar_dnd`).
    pub(crate) drag: crate::ui::sidebar_dnd::TreeDrag,
    pub(crate) rows: Vec<VisibleNode>,
    match_key: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SidebarEvent> for SessionSidebar {}

impl SessionSidebar {
    pub fn new(
        catalog: Catalog,
        folders: FolderPrefs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search or user@host ⏎"));
        let subscription = cx.subscribe_in(&search, window, Self::on_search_event);
        let mut sidebar = Self {
            catalog,
            folders,
            search,
            filter: String::new(),
            search_collapsed: BTreeSet::new(),
            selection: Selection::new(),
            anchor: None,
            focused_key: None,
            last_index: 0,
            live: BTreeMap::new(),
            scroll: ScrollHandle::new(),
            // Enough rows for a tall window before the viewport has been measured.
            row_window: RowWindow {
                start: 0,
                end: (1080. / TREE_ROW_PITCH).ceil() as usize + TREE_OVERSCAN,
            },
            open_gesture: SidebarOpenGesture::Click,
            on_right: false,
            tree_focus: cx.focus_handle(),
            menu: None,
            tree: HostTree::default(),
            drag: Default::default(),
            click_position: None,
            rows: Vec::new(),
            match_key: None,
            _subscriptions: vec![subscription],
        };
        sidebar.rebuild();
        sidebar
    }

    /// Live session counts per host key, for the row dots.
    pub fn set_live(&mut self, live: BTreeMap<String, LiveCounts>, cx: &mut Context<Self>) {
        if self.live != live {
            self.live = live;
            cx.notify();
        }
    }

    /// Replace the host catalog (the backend reloaded it).
    pub fn set_catalog(&mut self, catalog: Catalog, cx: &mut Context<Self>) {
        self.catalog = catalog;
        self.rebuild();
        cx.notify();
    }

    /// Whether the search field has focus (shortcuts stay out of text fields).
    pub fn search_focused(&self, window: &Window, cx: &App) -> bool {
        self.search.read(cx).focus_handle(cx).is_focused(window)
    }

    pub fn set_open_gesture(&mut self, gesture: SidebarOpenGesture) {
        self.open_gesture = gesture;
    }

    pub fn set_on_right(&mut self, on_right: bool) {
        self.on_right = on_right;
    }

    fn normalized_filter(&self) -> String {
        self.filter.trim().to_lowercase()
    }

    pub(crate) fn filtering(&self) -> bool {
        !self.normalized_filter().is_empty()
    }

    /// Reordering writes the order to the store: only against the full,
    /// unfiltered list (`reorderEnabled`).
    pub(crate) fn reorder_enabled(&self) -> bool {
        !self.filtering()
    }

    /// Collapse or expand without rebuilding (callers rebuild once after a batch).
    pub(crate) fn set_collapsed_quietly(&mut self, key: &str, collapsed: bool) {
        if self.filtering() {
            if collapsed {
                self.search_collapsed.insert(key.to_string());
            } else {
                self.search_collapsed.remove(key);
            }
        } else {
            self.folders.set_collapsed(key, collapsed);
        }
    }

    fn is_expanded(&self, key: &str) -> bool {
        if self.filtering() {
            !self.search_collapsed.contains(key)
        } else {
            self.folders.is_expanded(key)
        }
    }

    /// Regroup, rebuild the tree and re-flatten after any input changed.
    pub(crate) fn rebuild(&mut self) {
        let needle = self.normalized_filter();
        let ssh = &self.catalog.ssh;
        let groups = group_managed_hosts(
            &ssh.hosts,
            &self.catalog.profiles,
            &ssh.files,
            Some(&ssh.path),
            &needle,
        );
        let known = self.folders.empty_folders.clone();
        let folders = &self.folders;
        let order = |key: &str| folders.folder_order(key);
        self.tree = build_host_tree(
            &groups,
            &BuildOptions {
                known_folders: &known,
                folder_order: Some(&order),
            },
        );
        let filtering = !needle.is_empty();
        let search_collapsed = &self.search_collapsed;
        let expanded = |key: &str| {
            if filtering {
                !search_collapsed.contains(key)
            } else {
                folders.is_expanded(key)
            }
        };
        let color = |key: &str| folders.folder_color(key);
        self.rows = flatten_visible_tree(&self.tree, &expanded, &color);
        // The host Enter would connect, scored against every host as typed.
        let all = self.all_hosts();
        self.match_key = best_managed_host_match(&all, &needle).map(ManagedHost::key);
        // Deleting a host or collapsing its parent must not strand the tab stop.
        if let Some(current) = self.focused_key.clone() {
            self.focused_key = focus_after_change(&self.rows, Some(&current), self.last_index);
        }
    }

    pub(crate) fn all_hosts(&self) -> Vec<ManagedHost> {
        let ssh = &self.catalog.ssh;
        let groups = group_managed_hosts(
            &ssh.hosts,
            &self.catalog.profiles,
            &ssh.files,
            Some(&ssh.path),
            "",
        );
        groups.into_iter().flat_map(|group| group.hosts).collect()
    }

    fn on_search_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let text = self.search.read(cx).value().to_string();
                if text != self.filter {
                    self.filter = text;
                    self.search_collapsed.clear();
                    self.rebuild();
                    self.scroll_to_match();
                    cx.notify();
                }
            }
            InputEvent::PressEnter { .. } => self.on_enter(window, cx),
            _ => {}
        }
    }

    fn set_filter(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |state, cx| {
            state.set_value(text.to_string(), window, cx)
        });
        self.filter = text.to_string();
        self.search_collapsed.clear();
        self.rebuild();
        cx.notify();
    }

    /// Enter in the search box: the best match, else a quick-connect target.
    fn on_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let filter = self.filter.trim().to_string();
        if let Some(key) = &self.match_key
            && let Some(host) = self.all_hosts().into_iter().find(|host| &host.key() == key)
        {
            cx.emit(SidebarEvent::OpenHost {
                host,
                new_session: false,
                repeat: false,
                anchor: None,
            });
        } else if self.quick_connectable() {
            cx.emit(SidebarEvent::QuickConnect(filter));
        } else {
            return;
        }
        self.set_filter("", window, cx);
    }

    fn quick_connectable(&self) -> bool {
        self.filtering() && self.match_key.is_none() && is_quick_connect_target(&self.filter)
    }

    fn set_expanded(&mut self, key: &str, expanded: bool, cx: &mut Context<Self>) {
        if self.filtering() {
            if expanded {
                self.search_collapsed.remove(key);
            } else {
                self.search_collapsed.insert(key.to_string());
            }
        } else {
            self.folders.set_collapsed(key, !expanded);
        }
        self.rebuild();
        cx.notify();
    }

    fn scroll_to_match(&mut self) {
        if let Some(key) = &self.match_key
            && let Some(index) = self.rows.iter().position(|row| &row.key == key)
        {
            self.scroll_to_index(index);
        }
    }

    /// Scroll just enough to show a row, mounted or not.
    fn scroll_to_index(&self, index: usize) {
        let fixed = self.fixed_row_count() as f32 * TREE_ROW_PITCH;
        let top = fixed + index as f32 * TREE_ROW_PITCH + TREE_ROW_GAP;
        let offset = -muxus_ui::zoom::unzoom(self.scroll.offset().y);
        let viewport = muxus_ui::zoom::unzoom(self.scroll.bounds().size.height);
        if viewport <= 0. {
            return;
        }
        if top < offset {
            self.scroll
                .set_offset(muxus_ui::gpui_kit::point(px(0.), px(-top)));
        } else if top + TREE_ROW_HEIGHT > offset + viewport {
            self.scroll.set_offset(muxus_ui::gpui_kit::point(
                px(0.),
                px(-(top + TREE_ROW_HEIGHT - viewport)),
            ));
        }
    }

    fn fixed_row_count(&self) -> usize {
        1 + usize::from(self.quick_connectable())
    }

    /// Re-measure which rows the viewport shows; a no-op while the window still covers them.
    fn sync_window(&mut self) -> bool {
        let fixed = self.fixed_row_count() as f32 * TREE_ROW_PITCH;
        let scrolled_past = -muxus_ui::zoom::unzoom(self.scroll.offset().y) - fixed;
        let viewport = muxus_ui::zoom::unzoom(self.scroll.bounds().size.height);
        if viewport <= 0. {
            return false;
        }
        let visible = visible_row_span(
            scrolled_past as f64,
            viewport as f64,
            TREE_ROW_PITCH as f64,
            self.rows.len(),
        );
        let next = next_row_window(self.row_window, visible, TREE_OVERSCAN);
        let changed = next != self.row_window;
        self.row_window = next;
        changed
    }

    /// Selection gestures never connect: Ctrl-click toggles a host, and
    /// Shift-click selects the range from the last host clicked.
    fn activate_host(
        &mut self,
        host: ManagedHost,
        gesture: HostActivation,
        cx: &mut Context<Self>,
    ) {
        let key = host.key();
        match host_click_outcome(gesture, self.open_gesture, !self.selection.is_empty()) {
            HostClickOutcome::Toggle => {
                let (selection, anchor) = toggle_selection(&self.selection, &key);
                self.selection = selection;
                self.anchor = Some(anchor);
            }
            HostClickOutcome::Extend => {
                let anchor = self
                    .anchor
                    .get_or_insert_with(|| TreeSelectionAnchor {
                        key: key.clone(),
                        base: self.selection.clone(),
                    })
                    .clone();
                self.selection = range_selection(&self.rows, &anchor, &key);
            }
            HostClickOutcome::Select => {
                self.anchor = Some(TreeSelectionAnchor {
                    key: key.clone(),
                    base: Selection::new(),
                });
                self.selection = Selection::from([key]);
            }
            HostClickOutcome::Open => {
                if self.open_gesture == SidebarOpenGesture::Click && gesture.clicks.unwrap_or(0) > 0
                {
                    self.anchor = Some(TreeSelectionAnchor {
                        key,
                        base: self.selection.clone(),
                    });
                }
                cx.emit(SidebarEvent::OpenHost {
                    host,
                    new_session: gesture.new_session,
                    repeat: gesture.repeat,
                    anchor: self.click_position.take(),
                });
            }
            HostClickOutcome::Ignore => {}
        }
        if self.selection.is_empty() {
            self.anchor = None;
        }
        cx.notify();
    }

    fn focus_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.get(index) {
            self.focused_key = Some(row.key.clone());
            self.last_index = index;
            self.scroll_to_index(index);
            window.focus(&self.tree_focus, cx);
            cx.notify();
        }
    }

    fn on_tree_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let index = self
            .focused_key
            .as_ref()
            .and_then(|key| self.rows.iter().position(|row| &row.key == key))
            .unwrap_or(0);
        if (modifiers.control || modifiers.platform)
            && !modifiers.alt
            && !modifiers.shift
            && keystroke.key == "a"
        {
            self.anchor = None;
            self.selection = visible_host_keys(&self.rows);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let nav = match keystroke.key.as_str() {
            "up" => Some(TreeNavKey::ArrowUp),
            "down" => Some(TreeNavKey::ArrowDown),
            "left" => Some(TreeNavKey::ArrowLeft),
            "right" => Some(TreeNavKey::ArrowRight),
            "home" => Some(TreeNavKey::Home),
            "end" => Some(TreeNavKey::End),
            _ => None,
        };
        if let Some(nav) = nav {
            cx.stop_propagation();
            if modifiers.shift && matches!(nav, TreeNavKey::ArrowUp | TreeNavKey::ArrowDown) {
                // Shift+Arrow moves like an arrow and drags the range along with it.
                let to = if nav == TreeNavKey::ArrowUp {
                    index.checked_sub(1)
                } else {
                    Some(index + 1)
                };
                if let (Some(from), Some(to)) =
                    (self.rows.get(index), to.and_then(|to| self.rows.get(to)))
                {
                    let anchor = self
                        .anchor
                        .get_or_insert_with(|| TreeSelectionAnchor {
                            key: from.key.clone(),
                            base: self.selection.clone(),
                        })
                        .clone();
                    let to_key = to.key.clone();
                    self.selection = range_selection(&self.rows, &anchor, &to_key);
                    let to_index = self
                        .rows
                        .iter()
                        .position(|row| row.key == to_key)
                        .unwrap_or(index);
                    self.focus_index(to_index, window, cx);
                }
                return;
            }
            match tree_nav_action(&self.rows, index, nav) {
                TreeNavAction::Focus { index } => self.focus_index(index, window, cx),
                TreeNavAction::Expand { key } => self.set_expanded(&key, true, cx),
                TreeNavAction::Collapse { key } => self.set_expanded(&key, false, cx),
                TreeNavAction::None => {}
            }
            return;
        }
        match keystroke.key.as_str() {
            "enter" => {
                cx.stop_propagation();
                if let Some(row) = self.rows.get(index).cloned() {
                    match &row.node {
                        RowNode::Host { host, .. } => self.activate_host(
                            host.clone(),
                            HostActivation {
                                new_session: false,
                                repeat: false,
                                ..Default::default()
                            },
                            cx,
                        ),
                        _ => {
                            let expanded = self.is_expanded(&row.key);
                            self.set_expanded(&row.key, !expanded, cx);
                        }
                    }
                }
            }
            "escape" => {
                cx.stop_propagation();
                if !self.selection.is_empty() {
                    self.selection.clear();
                    self.anchor = None;
                    cx.notify();
                } else {
                    let handle = self.search.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }
}

impl Focusable for SessionSidebar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.tree_focus.clone()
    }
}

impl Render for SessionSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.settle_drag(cx);
        self.sync_window();
        let theme = *cx.theme();
        let p = theme.palette;
        let search_focus = self.search.read(cx).focus_handle(cx);

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            .pt(px(10.))
            .pb(px(6.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => {
                                this.set_filter("", window, cx);
                                cx.stop_propagation();
                            }
                            "down" => {
                                // Hand off to the tree rather than moving the text cursor.
                                cx.stop_propagation();
                                this.focus_index(0, window, cx);
                            }
                            _ => {}
                        }
                    }))
                    .child(
                        TextField::new("sidebar-search", &self.search)
                            .start_icon("Search")
                            .full_width(),
                    ),
            )
            .child(
                IconButton::new("add-host", "Add")
                    .tooltip("Add host")
                    .on_click(
                        cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::AddHost { prefill: None })),
                    ),
            );

        let mut list = div()
            .flex()
            .flex_col()
            .pt(px(TREE_ROW_GAP))
            .gap(px(TREE_ROW_GAP));
        list = list.child(self.fixed_row(
            "local-terminal",
            icon("Terminal", 16., p.text.secondary),
            "Local terminal".into(),
            None,
            cx.listener(|this, event: &muxus_ui::gpui_kit::ClickEvent, _, cx| {
                if click_opens_row(event.click_count() as u32, this.open_gesture) {
                    cx.emit(SidebarEvent::OpenLocalTerminal);
                }
            }),
            Some(Box::new(cx.listener(
                |this, event: &MouseDownEvent, _, cx| {
                    this.menu = Some(SidebarMenu::LocalShell {
                        position: event.position,
                    });
                    cx.stop_propagation();
                    cx.notify();
                },
            ))),
            &theme,
        ));
        if self.quick_connectable() {
            let target = self.filter.trim().to_string();
            list = list.child(self.fixed_row(
                "quick-connect",
                icon("Bolt", 16., p.primary.main),
                format!("Connect to {target}").into(),
                Some(p.primary.main),
                cx.listener(
                    move |this, event: &muxus_ui::gpui_kit::ClickEvent, window, cx| {
                        if click_opens_row(event.click_count() as u32, this.open_gesture) {
                            cx.emit(SidebarEvent::QuickConnect(target.clone()));
                            this.set_filter("", window, cx);
                        }
                    },
                ),
                None,
                &theme,
            ));
        }

        let focused_index = self
            .focused_key
            .as_ref()
            .and_then(|key| self.rows.iter().position(|row| &row.key == key));
        let pinned = focused_index.or(if self.rows.is_empty() { None } else { Some(0) });
        let segments = row_segments(
            &rendered_row_indices(self.row_window, self.rows.len(), pinned),
            self.rows.len(),
        );
        let mut tree = div()
            .id("host-tree")
            .track_focus(&self.tree_focus)
            .on_key_down(cx.listener(Self::on_tree_key))
            .flex()
            .flex_col()
            .gap(px(TREE_ROW_GAP));
        for segment in segments {
            tree = match segment {
                RowSegment::Row { index } => tree.child(self.render_row(index, &theme, window, cx)),
                // A spacer stands in for rows exactly: n rows and the n-1 gaps between them.
                RowSegment::Gap { rows } => tree.child(
                    div()
                        .flex_none()
                        .h(px(rows as f32 * TREE_ROW_PITCH - TREE_ROW_GAP)),
                ),
            };
        }
        let mut scroll_content = div().flex().flex_col().pb(px(8.)).child(list.child(tree));

        let empty = self.catalog.ssh.hosts.is_empty() && self.catalog.profiles.is_empty();
        if empty {
            scroll_content = scroll_content.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .p(px(24.))
                    .child(icon("DnsOutlined", 36., p.text.disabled))
                    .child(
                        paragraph("No saved hosts yet.")
                            .size(theme.typography.body2.size)
                            .line_height(theme.typography.body2.line_height)
                            .color(p.text.secondary)
                            .center(),
                    )
                    .child(
                        Button::new("add-first-host", "Add your first host")
                            .outlined()
                            .start_icon("Add")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(SidebarEvent::AddHost { prefill: None })
                            })),
                    ),
            );
        } else if self.tree.hosts.is_empty() {
            // Nothing matched is the moment you are most likely to want the host
            // you just typed, so offer to save it rather than only saying no.
            let typed = self.filter.trim().to_string();
            scroll_content = scroll_content.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .p(px(16.))
                    .child(
                        paragraph("No hosts match.")
                            .size(theme.typography.body2.size)
                            .line_height(theme.typography.body2.line_height)
                            .color(p.text.secondary)
                            .center(),
                    )
                    .child(
                        Button::new("add-typed-host", format!("Add “{typed}”"))
                            .outlined()
                            .start_icon("Add")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(SidebarEvent::AddHost {
                                    prefill: Some(typed.clone()),
                                })
                            })),
                    ),
            );
        }

        let scroller = div()
            .id("sidebar-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                if this.sync_window() {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.menu = Some(SidebarMenu::Panel {
                        position: event.position,
                    });
                    cx.notify();
                }),
            )
            .child(scroll_content);

        let mut root = div()
            .id("session-sidebar")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(header)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(scroller)
                    .child(scrollbar::vertical(&self.scroll, &theme)),
            );
        let _ = search_focus;
        if let Some(menu) = self.menu.clone() {
            root = root.child(self.render_menu(menu, &theme, cx));
        }
        root
    }
}

impl SessionSidebar {
    /// The fixed rows above the tree share the tree rows' exact geometry.
    #[allow(clippy::too_many_arguments)]
    fn fixed_row(
        &self,
        id: &'static str,
        leading: impl IntoElement,
        label: SharedString,
        ink: Option<Hsla>,
        on_click: impl Fn(&muxus_ui::gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
        on_context: Option<ContextHandler>,
        theme: &Theme,
    ) -> AnyElement {
        let mut row = row_shell(id.into(), 0, None, theme)
            .gap(px(6.))
            .cursor_pointer()
            .child(leading)
            .child(label_text(
                label,
                ink.unwrap_or(theme.palette.sidebar_ink),
                450.,
            ))
            .on_click(on_click);
        if let Some(handler) = on_context {
            row = row.on_mouse_down(MouseButton::Right, move |event, window, cx| {
                handler(event, window, cx)
            });
        }
        row.into_any_element()
    }

    fn render_row(
        &self,
        index: usize,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = &self.rows[index];
        let p = theme.palette;
        let focused = self.focused_key.as_deref() == Some(row.key.as_str())
            && self.tree_focus.is_focused(window);
        let mut shell = row_shell(
            ElementId::Name(row.key.clone().into()),
            row.depth,
            row.rail_color.as_deref(),
            theme,
        );
        shell = self.with_drag(shell, index, theme, cx);
        if focused {
            // MUI's focus-visible ring on a ListItemButton is the focus overlay.
            shell = shell.bg(p.action.focus);
        }
        match &row.node {
            RowNode::Host { host, .. } => {
                let key = row.key.clone();
                let selected = self.selection.contains(&key);
                let is_match = self.match_key.as_deref() == Some(key.as_str());
                let color = host
                    .metadata()
                    .and_then(|m| m.color.as_deref())
                    .and_then(try_hex);
                let live = self.live.get(&key).copied().unwrap_or_default();
                let title: SharedString = host.display_name().to_string().into();
                let group_name: SharedString = format!("host-row-{key}").into();
                let mut icon_box = div().relative().flex().flex_none().child(icon(
                    host_kind_icon_name(host.kind()),
                    16.,
                    color.unwrap_or(p.text.secondary),
                ));
                if live.connected + live.connecting > 0 {
                    let dot = if live.connected > 0 {
                        p.success.main
                    } else {
                        p.warning.main
                    };
                    // 7px dot at right -3 / bottom -2 of the icon, inside a 2px
                    // ring in the sidebar color (MUI's box-shadow ring).
                    icon_box = icon_box.child(
                        div()
                            .absolute()
                            .left(px(10.))
                            .top(px(9.))
                            .size(px(11.))
                            .rounded(px(5.5))
                            .bg(p.sidebar)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(div().size(px(7.)).rounded(px(3.5)).bg(dot)),
                    );
                }
                let mut label_row = div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .min_w_0()
                    .child(label_text(title.clone(), p.sidebar_ink, 450.));
                if live.connected > 1 {
                    label_row = label_row.child(
                        div()
                            .flex_none()
                            .ui_font(10., 400.)
                            .text_color(p.success.main)
                            .child(format!("×{}", live.connected)),
                    );
                }
                let host_for_click = host.clone();
                let host_for_middle = host.clone();
                let host_for_menu = host.clone();
                let host_for_button = host.clone();
                let mut el = shell
                    .group(group_name.clone())
                    .gap(px(6.))
                    .cursor_pointer()
                    .when_bg(selected, p.action.selected)
                    .when_bg(is_match, p.action.selected)
                    .child(icon_box)
                    .child(div().flex_1().min_w_0().child(label_row));
                if is_match {
                    el = el.child(
                        div()
                            .flex_none()
                            .ui_font(11., 400.)
                            .text_color(p.text.secondary)
                            .child("⏎"),
                    );
                }
                let lines = hover_card_lines(host, &self.catalog.profiles);
                // Hover cards open toward the panes, wherever the sidebar docks.
                let placement = if self.on_right {
                    Placement::Left
                } else {
                    Placement::Right
                };
                let row_anchor = AnchorBounds::default();
                let on_right = self.on_right;
                let menu_closed = self.menu.is_none();
                el.child(
                    div()
                        .flex_none()
                        .mr(px(-3.))
                        .invisible()
                        .group_hover(group_name, |s| s.visible())
                        .child(
                            IconButton::new(
                                ElementId::Name(format!("host-menu-{key}").into()),
                                "MoreVert",
                            )
                            .icon_size(15.)
                            .padding(1.)
                            .on_click(cx.listener(
                                move |this, event: &muxus_ui::gpui_kit::ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    let position = event.mouse_position().unwrap_or_default();
                                    this.menu = Some(SidebarMenu::Host {
                                        host: host_for_button.clone(),
                                        position,
                                    });
                                    cx.notify();
                                },
                            )),
                        ),
                )
                .child(bounds_probe(&row_anchor))
                // The card would cover an open menu; MUI's modal menu takes
                // the hover away from the row.
                .when(menu_closed, |el| {
                    el.tooltip(Tooltip::card(
                        title,
                        lines,
                        Some(row_anchor.clone()),
                        placement,
                    ))
                    .tooltip_show_delay(std::time::Duration::from_millis(400))
                })
                .on_click(cx.listener(
                    move |this, event: &muxus_ui::gpui_kit::ClickEvent, _, cx| {
                        let modifiers = event.modifiers();
                        let clicks = event.click_count() as u32;
                        // The sessions menu opens beside the row, toward the panes
                        // (`anchorOrigin: top/toward`), its top on the row's top.
                        this.click_position = row_anchor
                            .get()
                            .map(|row| {
                                if on_right {
                                    row.origin
                                } else {
                                    muxus_ui::gpui_kit::point(row.right(), row.top())
                                }
                            })
                            .or_else(|| event.mouse_position());
                        this.activate_host(
                            host_for_click.clone(),
                            HostActivation {
                                new_session: modifiers.shift,
                                repeat: clicks > 1,
                                clicks: Some(clicks),
                                toggle_selection: modifiers.control || modifiers.platform,
                                extend_selection: modifiers.shift,
                            },
                            cx,
                        );
                    },
                ))
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _, _, cx| {
                        this.activate_host(
                            host_for_middle.clone(),
                            HostActivation {
                                new_session: true,
                                ..Default::default()
                            },
                            cx,
                        );
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.menu = Some(SidebarMenu::Host {
                            host: host_for_menu.clone(),
                            position: event.position,
                        });
                        cx.notify();
                    }),
                )
                .into_any_element()
            }
            RowNode::Folder {
                label,
                descendant_host_count,
                ..
            }
            | RowNode::File {
                label,
                descendant_host_count,
                ..
            } => {
                let is_folder = matches!(row.node, RowNode::Folder { .. });
                let key = row.key.clone();
                let expanded = row.expanded.unwrap_or(false);
                let color = if is_folder {
                    self.folders.folder_color(&key).as_deref().and_then(try_hex)
                } else {
                    None
                };
                let icon_id = if is_folder {
                    self.folders.folder_icon(&key).map(str::to_string)
                } else {
                    Some("server".into())
                };
                let count = *descendant_host_count;
                let group_name: SharedString = format!("folder-row-{key}").into();
                let chevron = icon("ChevronRight", 16., p.text.disabled).with_transformation(
                    Transformation::rotate(radians(if expanded {
                        std::f32::consts::FRAC_PI_2
                    } else {
                        0.
                    })),
                );
                let toggle_key = key.clone();
                let menu_key = key.clone();
                let button_key = key.clone();
                let mut el = shell
                    .group(group_name.clone())
                    .gap(px(4.))
                    .cursor_pointer()
                    .child(chevron)
                    .child(icon(
                        folder_icon_name(icon_id.as_deref(), expanded),
                        16.,
                        color.unwrap_or(p.text.secondary),
                    ))
                    .child(label_text(label.clone().into(), p.sidebar_ink, 550.).grow())
                    .child(
                        div()
                            .flex_none()
                            .invisible()
                            .group_hover(group_name.clone(), |s| s.visible())
                            .child(
                                IconButton::new(
                                    ElementId::Name(format!("folder-launch-{key}").into()),
                                    "PlayArrowOutlined",
                                )
                                .icon_size(14.)
                                .padding(1.)
                                .disabled(count == 0)
                                .tooltip(if count > 0 {
                                    format!("Launch all {count} hosts…")
                                } else {
                                    String::new()
                                })
                                .on_click(cx.listener({
                                    let key = key.clone();
                                    let label = label.clone();
                                    move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        let hosts = this
                                            .tree
                                            .find_container(&key)
                                            .map(|container| {
                                                muxus_app::hosts::tree::collect_hosts(
                                                    &this.tree, container,
                                                )
                                            })
                                            .unwrap_or_default();
                                        cx.emit(SidebarEvent::Action(SidebarAction::LaunchHosts {
                                            label: label.clone(),
                                            hosts,
                                        }));
                                    }
                                })),
                            ),
                    );
                if is_folder {
                    el = el.child(
                        div()
                            .flex_none()
                            .invisible()
                            .group_hover(group_name, |s| s.visible())
                            .child(
                                IconButton::new(
                                    ElementId::Name(format!("folder-menu-{key}").into()),
                                    "MoreVert",
                                )
                                .icon_size(14.)
                                .padding(1.)
                                .on_click(cx.listener(
                                    move |this, event: &muxus_ui::gpui_kit::ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.menu = Some(SidebarMenu::Folder {
                                            key: button_key.clone(),
                                            position: event.mouse_position().unwrap_or_default(),
                                        });
                                        cx.notify();
                                    },
                                )),
                            ),
                    );
                }
                el = el.child(
                    div().flex_none().min_w(px(14.)).flex().justify_end().child(
                        muxus_ui::label::label(count.to_string())
                            .size(11.)
                            .line_height(16.5)
                            .color(p.text.disabled)
                            .no_truncate(),
                    ),
                );
                let mut el = el.on_click(cx.listener(
                    move |this, event: &muxus_ui::gpui_kit::ClickEvent, _, cx| {
                        // Where a double-click opens hosts it is a habit, and must not
                        // toggle a folder open and straight back shut.
                        if this.open_gesture == SidebarOpenGesture::DoubleClick
                            && event.click_count() > 1
                        {
                            return;
                        }
                        let expanded = this.is_expanded(&toggle_key);
                        this.set_expanded(&toggle_key, !expanded, cx);
                    },
                ));
                if is_folder {
                    el = el.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.menu = Some(SidebarMenu::Folder {
                                key: menu_key.clone(),
                                position: event.position,
                            });
                            cx.notify();
                        }),
                    );
                }
                el.into_any_element()
            }
        }
    }
}

/// `treeRowSx`: the shared geometry of every tree row (26px, 1px gaps, 6px
/// corners, 8px + 14px-per-level indent, indent guides).
fn row_shell(
    id: ElementId,
    depth: usize,
    rail_color: Option<&str>,
    theme: &Theme,
) -> muxus_ui::gpui_kit::Stateful<muxus_ui::gpui_kit::Div> {
    let p = theme.palette;
    let hover = theme.components.list_hover;
    let mut row = div()
        .id(id)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .mx(px(6.))
        .h(px(TREE_ROW_HEIGHT))
        .py(px(2.))
        .pr(px(4.))
        .pl(px(indent_px(depth)))
        .rounded(px(6.))
        .hover(move |s| s.bg(hover));
    for guide in indent_guides(depth, rail_color) {
        let color = guide
            .color
            .as_deref()
            .and_then(try_hex)
            .unwrap_or(p.divider);
        row = row.child(
            div()
                .absolute()
                .top(px(0.))
                .bottom(px(0.))
                .left(px(guide.x))
                .w(px(guide.width))
                .bg(color),
        );
    }
    row
}

/// `treeLabelSx`: 13px, weight 450 (folders 550), -0.1px tracking, sidebar ink, one line.
fn label_text(text: SharedString, ink: Hsla, weight: f32) -> Label {
    // Electron draws these rows' text one device pixel below the line
    // model's baseline; the nudge follows it.
    label(text)
        .size(13.)
        .weight(weight)
        .line_height(19.5)
        .tracking(-0.1)
        .color(ink)
        .nudge(0.5)
}

/// The host hover card: folder path, address and the reference details.
fn hover_card_lines(
    host: &ManagedHost,
    saved: &[muxus_app::model::SavedHostProfile],
) -> Vec<SharedString> {
    let mut lines: Vec<SharedString> = Vec::new();
    let address = managed_host_address(host);
    if address != host.display_name() {
        lines.push(address.into());
    }
    for line in host_detail_lines(host, Some(saved)) {
        lines.push(line.into());
    }
    let path = muxus_app::hosts::tree::folder_segments(host.group());
    if !path.is_empty() {
        lines.insert(0, path.join(" / ").into());
    }
    lines
}

trait WhenBg: Sized {
    fn when_bg(self, condition: bool, color: Hsla) -> Self;
}

impl WhenBg for muxus_ui::gpui_kit::Stateful<muxus_ui::gpui_kit::Div> {
    fn when_bg(self, condition: bool, color: Hsla) -> Self {
        if condition { self.bg(color) } else { self }
    }
}
