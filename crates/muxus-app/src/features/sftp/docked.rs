//! The file browser docked in the window sidebar (issue #222,
//! `client/src/layout/Sidebar.tsx`): a rail of vertical tabs along the window
//! edge switches the sidebar between the hosts and the active session's
//! files. Behind the hosts tab the browser stays on the session it last
//! showed, so an upload in flight or the folder it was in survives a look at
//! the hosts.

use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, FocusHandle,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::{IconButton, Placement, Tooltip};

use super::panel::{SftpPanel, SftpPanelEvent, SftpPanelOptions};
use super::placement::{BrowserTabFacts, SidebarView, file_browser_unavailable_reason};

/// The rail's width, outside the sidebar's resizable width.
pub const RAIL_WIDTH: f32 = 40.;

const VIEWS: [(SidebarView, &str, &str); 2] = [
    (SidebarView::Hosts, "Hosts", "DnsOutlined"),
    (SidebarView::Files, "File browser", "FolderOutlined"),
];

/// The rail's next view for an arrow, Home or End key; `None` for other keys.
pub fn rail_key_target(view: SidebarView, key: &str) -> Option<SidebarView> {
    let index = VIEWS.iter().position(|(v, ..)| *v == view).unwrap_or(0);
    let count = VIEWS.len();
    let next = match key {
        "down" => (index + 1) % count,
        "up" => (index + count - 1) % count,
        "home" => 0,
        "end" => count - 1,
        _ => return None,
    };
    Some(VIEWS[next].0)
}

type SelectView = std::rc::Rc<dyn Fn(SidebarView, &mut Window, &mut App)>;

/// `SidebarRail`: the vertical tabs. `on_select` switches the sidebar view.
pub fn sidebar_rail(
    view: SidebarView,
    on_right: bool,
    focus: &FocusHandle,
    on_select: impl Fn(SidebarView, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let p = theme.palette;
    let on_select: SelectView = std::rc::Rc::new(on_select);
    let keys = on_select.clone();
    let mut rail = div()
        .id("sidebar-rail")
        .track_focus(focus)
        .key_context("SidebarRail")
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            // Arrow keys move between the tabs and open the one they land on.
            if let Some(target) = rail_key_target(view, event.keystroke.key.as_str()) {
                cx.stop_propagation();
                keys(target, window, cx);
            }
        })
        .flex()
        .flex_col()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .py(px(10.))
        .w(px(RAIL_WIDTH))
        .h_full()
        .bg(p.sidebar)
        .border_color(p.divider);
    rail = if on_right {
        rail.border_l_1()
    } else {
        rail.border_r_1()
    };
    for (entry, title, glyph) in VIEWS {
        let selected = entry == view;
        let select = on_select.clone();
        let hover_bg = if selected {
            p.action.selected
        } else {
            p.action.hover
        };
        let ink = if selected {
            p.text.primary
        } else {
            p.text.secondary
        };
        let anchor = muxus_ui::widgets::AnchorBounds::default();
        let mut tab = div()
            .id(ElementId::Name(format!("sidebar-tab-{title}").into()))
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .size(px(30.))
            .rounded(px(12.))
            .cursor_pointer()
            .text_color(ink)
            .hover(move |s| s.bg(hover_bg))
            .on_click(move |_, window, cx| select(entry, window, cx))
            .child(icon(glyph, 18., ink))
            .child(muxus_ui::widgets::bounds_probe(&anchor))
            .tooltip(Tooltip::anchored(
                title,
                anchor,
                if on_right {
                    Placement::Left
                } else {
                    Placement::Right
                },
            ));
        if selected {
            tab = tab.bg(p.action.selected);
        }
        rail = rail.child(tab);
    }
    rail
}

/// The session the docked browser shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DockedTab {
    pub tab_id: String,
    /// The live connection; `None` until the session is ready.
    pub conn_id: Option<String>,
    pub facts: BrowserTabFacts,
    /// The tab's title.
    pub title: String,
    pub terminal_cwd: Option<String>,
    /// `sftpFollowTerminal !== false`.
    pub follow_terminal_folder: bool,
}

/// What the docked browser asks of the app, with the tab it was showing.
#[derive(Clone, Debug, PartialEq)]
pub enum DockedEvent {
    /// The panel's own events.
    Panel {
        tab_id: String,
        event: SftpPanelEvent,
    },
    /// "Move beside the terminal" without a session to show it for.
    MoveToPane { tab_id: Option<String> },
}

/// `SidebarFileBrowser`.
pub struct DockedFileBrowser {
    shown: bool,
    /// Nothing is listed until the files tab has been opened at least once.
    held: Option<Option<DockedTab>>,
    /// The latest active tab, adopted whenever the browser is on screen.
    active: Option<DockedTab>,
    panel: Option<(String, Entity<SftpPanel>, Subscription)>,
    sidebar_on_left: bool,
}

impl EventEmitter<DockedEvent> for DockedFileBrowser {}

impl DockedFileBrowser {
    pub fn new(shown: bool) -> Self {
        Self {
            shown,
            held: None,
            active: None,
            panel: None,
            sidebar_on_left: true,
        }
    }

    /// The window's active tab changed.
    pub fn set_active_tab(
        &mut self,
        tab: Option<DockedTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active = tab;
        self.sync(window, cx);
    }

    /// The files tab came to the front, or went behind the hosts.
    pub fn set_shown(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.shown = shown;
        self.sync(window, cx);
    }

    pub fn set_sidebar_on_left(&mut self, left: bool, cx: &mut Context<Self>) {
        self.sidebar_on_left = left;
        if let Some((_, panel, _)) = &self.panel {
            panel.update(cx, |panel, cx| panel.set_sidebar_on_left(left, cx));
        }
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown {
            self.held = Some(self.active.clone());
        }
        let tab = self.held.clone().flatten();
        let wanted = tab
            .as_ref()
            .filter(|tab| tab.facts.sftp_available != Some(false))
            .and_then(|tab| tab.conn_id.clone().map(|conn| (conn, tab.clone())));
        match wanted {
            Some((conn, tab)) => {
                let same = self
                    .panel
                    .as_ref()
                    .is_some_and(|(current, ..)| *current == conn);
                if same {
                    if let Some((_, panel, _)) = &self.panel {
                        let shown = self.shown;
                        panel.update(cx, |panel, cx| {
                            panel.set_shown(shown, cx);
                            panel.set_host_label(Some(tab.title.clone()), cx);
                            panel.set_follow_terminal_folder(
                                tab.follow_terminal_folder,
                                window,
                                cx,
                            );
                            panel.set_terminal_path(tab.terminal_cwd.clone(), window, cx);
                        });
                    }
                } else {
                    // A new connection gets a browser of its own (`key={connId}`).
                    let mut options = SftpPanelOptions::new(conn.clone());
                    options.in_sidebar = true;
                    options.shown = self.shown;
                    options.host_label = Some(tab.title.clone());
                    options.terminal_path = tab.terminal_cwd.clone();
                    options.follow_terminal_folder = tab.follow_terminal_folder;
                    options.follow_toggle = true;
                    options.movable = true;
                    options.opens_in_new_window = true;
                    options.sidebar_on_left = self.sidebar_on_left;
                    let panel = cx.new(|cx| SftpPanel::new(options, window, cx));
                    let tab_id = tab.tab_id.clone();
                    let subscription = cx.subscribe(&panel, move |_, _, event, cx| {
                        cx.emit(DockedEvent::Panel {
                            tab_id: tab_id.clone(),
                            event: event.clone(),
                        })
                    });
                    self.panel = Some((conn, panel, subscription));
                }
            }
            None => self.panel = None,
        }
        cx.notify();
    }

    fn render_unavailable(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = theme.palette;
        let subtitle2 = theme.typography.subtitle2;
        let body2 = theme.typography.body2;
        let tab = self.held.clone().flatten();
        let reason = file_browser_unavailable_reason(tab.as_ref().map(|tab| &tab.facts));
        let move_tab = tab
            .as_ref()
            .filter(|tab| tab.facts.profile_kind.as_deref() == Some("ssh"))
            .map(|tab| tab.tab_id.clone());
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(px(10.))
                    .pt(px(8.))
                    .child(
                        div()
                            .mr(px(6.))
                            .child(icon("FolderOpenOutlined", 18., p.primary.main)),
                    )
                    .child(
                        label("File browser")
                            .size(subtitle2.size)
                            .weight(subtitle2.weight)
                            .line_height(subtitle2.line_height)
                            .color(p.text.primary)
                            .grow(),
                    )
                    .child(
                        IconButton::new("docked-move", "VerticalSplitOutlined")
                            .icon_size(17.)
                            .tooltip("Move beside the terminal")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(DockedEvent::MoveToPane {
                                    tab_id: move_tab.clone(),
                                })
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .p(px(24.))
                    .pt(px(40.))
                    .child(icon("FolderOffOutlined", 36., p.text.disabled))
                    .child(
                        paragraph(reason)
                            .size(body2.size)
                            .line_height(body2.line_height)
                            .color(p.text.secondary)
                            .center(),
                    ),
            )
            .into_any_element()
    }
}

impl Render for DockedFileBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let root = div().flex().flex_col().flex_1().min_h_0().h_full();
        if self.held.is_none() {
            return root;
        }
        match &self.panel {
            Some((_, panel, _)) => root.child(panel.clone()),
            None => root.child(self.render_unavailable(&theme, cx)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_keys_move_between_the_rail_tabs() {
        assert_eq!(
            rail_key_target(SidebarView::Hosts, "down"),
            Some(SidebarView::Files)
        );
        assert_eq!(
            rail_key_target(SidebarView::Files, "down"),
            Some(SidebarView::Hosts)
        );
        assert_eq!(
            rail_key_target(SidebarView::Hosts, "up"),
            Some(SidebarView::Files)
        );
        assert_eq!(
            rail_key_target(SidebarView::Files, "home"),
            Some(SidebarView::Hosts)
        );
        assert_eq!(
            rail_key_target(SidebarView::Hosts, "end"),
            Some(SidebarView::Files)
        );
        assert_eq!(rail_key_target(SidebarView::Hosts, "left"), None);
    }
}
