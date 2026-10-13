//! Where the file browser sits and how wide it is: `client/src/file-browser.ts`,
//! `sftp-panel-width.ts` and `sftp-panel-state.ts`.
//!
//! The file browser either opens beside an SSH terminal, one tab at a time, or
//! sits behind a tab in the window sidebar and follows the active session.
//! Every way of opening it goes through here, so each behaves the same
//! wherever the browser is docked. The state these rules read and write
//! (preferences, the sidebar view, each tab's `sftpOpen`) belongs to the app;
//! it reaches them through [`FileBrowserStores`].

/// `fileBrowserPosition` preference: beside each tab's terminal (`Pane`) or docked in the
/// window sidebar behind the rail's file tab (`Sidebar`).
pub use muxus_store::prefs::state::FileBrowserPosition;

/// `SidebarView` of `state/ui.ts`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarView {
    #[default]
    Hosts,
    Files,
}

/// `FileBrowserPlacement`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileBrowserPlacement {
    pub position: FileBrowserPosition,
    pub sidebar_collapsed: bool,
    pub focus_mode: bool,
    pub sidebar_view: SidebarView,
}

/// Whether the browser for a tab is on screen. `tab_sftp_open` is the tab's
/// `sftpOpen`, `None` without a tab.
pub fn is_file_browser_shown(
    placement: &FileBrowserPlacement,
    tab_sftp_open: Option<bool>,
) -> bool {
    if placement.position == FileBrowserPosition::Pane {
        return tab_sftp_open == Some(true);
    }
    !placement.sidebar_collapsed
        && !placement.focus_mode
        && placement.sidebar_view == SidebarView::Files
}

/// The app state the placement rules read and change.
pub trait FileBrowserStores {
    fn placement(&self) -> FileBrowserPlacement;
    fn tab_sftp_open(&self, tab_id: &str) -> Option<bool>;
    fn set_tab_sftp_open(&mut self, tab_id: &str, open: bool);
    fn set_position(&mut self, position: FileBrowserPosition);
    fn set_sidebar_collapsed(&mut self, collapsed: bool);
    fn set_sidebar_view(&mut self, view: SidebarView);
}

/// Bring the browser for a tab on screen. The caller activates the tab.
pub fn show_file_browser(stores: &mut impl FileBrowserStores, tab_id: &str) {
    let placement = stores.placement();
    if placement.position == FileBrowserPosition::Pane {
        stores.set_tab_sftp_open(tab_id, true);
        return;
    }
    if placement.sidebar_collapsed {
        stores.set_sidebar_collapsed(false);
    }
    stores.set_sidebar_view(SidebarView::Files);
}

/// Show the browser for a tab, or put it away. Docked, that brings the hosts back.
pub fn toggle_file_browser(stores: &mut impl FileBrowserStores, tab_id: &str) {
    let placement = stores.placement();
    if !is_file_browser_shown(&placement, stores.tab_sftp_open(tab_id)) {
        show_file_browser(stores, tab_id);
    } else if placement.position == FileBrowserPosition::Pane {
        stores.set_tab_sftp_open(tab_id, false);
    } else {
        stores.set_sidebar_view(SidebarView::Hosts);
    }
}

/// Dock the browser elsewhere and keep the tab's browser in view there.
pub fn move_file_browser(
    stores: &mut impl FileBrowserStores,
    position: FileBrowserPosition,
    tab_id: Option<&str>,
) {
    stores.set_position(position);
    if let Some(tab_id) = tab_id {
        show_file_browser(stores, tab_id);
    }
}

/// What the docked browser knows about the active tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrowserTabFacts {
    /// The profile's kind (`ssh`, `local`, …); `None` for an empty tab.
    pub profile_kind: Option<String>,
    /// `connecting`, `connected`, `interrupted`, `closed`, `idle`.
    pub status: String,
    pub sftp_available: Option<bool>,
}

/// Why the sidebar's browser has no files to show for the active tab.
pub fn file_browser_unavailable_reason(tab: Option<&BrowserTabFacts>) -> &'static str {
    let Some(kind) = tab.and_then(|tab| tab.profile_kind.as_deref()) else {
        return "Open an SSH session to browse its files.";
    };
    let tab = tab.expect("a tab with a profile");
    if kind != "ssh" {
        return "Only SSH sessions have a file browser.";
    }
    if tab.sftp_available == Some(false) {
        return "SFTP is turned off for this host.";
    }
    if tab.status == "connecting" {
        return "Waiting for the session to connect…";
    }
    "The session is not connected."
}

pub const DEFAULT_SFTP_PANEL_WIDTH: f32 = 380.;
pub const MIN_SFTP_PANEL_WIDTH: f32 = 280.;
pub const MIN_SFTP_WORKSPACE_WIDTH: f32 = 240.;

/// Keep the browser useful without letting it take more than 70% of its pane
/// or squeeze the terminal/editor below its practical minimum.
pub fn max_sftp_panel_width(container_width: f32) -> f32 {
    MIN_SFTP_PANEL_WIDTH.max(
        (container_width * 0.7)
            .min(container_width - MIN_SFTP_WORKSPACE_WIDTH)
            .floor(),
    )
}

/// `clampSftpPanelWidth`: rounded (half up, as `Math.round`) and clamped.
pub fn clamp_sftp_panel_width(width: f32, container_width: f32) -> f32 {
    MIN_SFTP_PANEL_WIDTH.max(max_sftp_panel_width(container_width).min((width + 0.5).floor()))
}

/// Choose the terminal directory only while the attached browser is following it.
pub fn initial_sftp_path(
    initial_path: &str,
    terminal_path: Option<&str>,
    follow_terminal_folder: bool,
) -> String {
    match terminal_path {
        Some(path) if follow_terminal_folder && !path.is_empty() => path.to_string(),
        _ => initial_path.to_string(),
    }
}

/// A panel narrower than this hides the modification times and the drag hint
/// (`@container (max-width: 359px)`), as the browser docked in the sidebar is.
pub const NARROW_PANEL_WIDTH: f32 = 360.;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct Stores {
        position: FileBrowserPosition,
        sidebar_collapsed: bool,
        focus_mode: bool,
        sidebar_view: SidebarView,
        tabs: HashMap<String, bool>,
    }

    impl FileBrowserStores for Stores {
        fn placement(&self) -> FileBrowserPlacement {
            FileBrowserPlacement {
                position: self.position,
                sidebar_collapsed: self.sidebar_collapsed,
                focus_mode: self.focus_mode,
                sidebar_view: self.sidebar_view,
            }
        }
        fn tab_sftp_open(&self, tab_id: &str) -> Option<bool> {
            self.tabs.get(tab_id).copied()
        }
        fn set_tab_sftp_open(&mut self, tab_id: &str, open: bool) {
            if let Some(tab) = self.tabs.get_mut(tab_id) {
                *tab = open;
            }
        }
        fn set_position(&mut self, position: FileBrowserPosition) {
            self.position = position;
        }
        fn set_sidebar_collapsed(&mut self, collapsed: bool) {
            self.sidebar_collapsed = collapsed;
        }
        fn set_sidebar_view(&mut self, view: SidebarView) {
            self.sidebar_view = view;
        }
    }

    fn with_ssh_tab(stores: &mut Stores) -> String {
        stores.tabs.insert("tab-1".into(), false);
        "tab-1".into()
    }

    const DOCKED: FileBrowserPlacement = FileBrowserPlacement {
        position: FileBrowserPosition::Sidebar,
        sidebar_collapsed: false,
        focus_mode: false,
        sidebar_view: SidebarView::Files,
    };

    #[test]
    fn follows_the_tab_beside_the_terminal() {
        let placement = FileBrowserPlacement {
            position: FileBrowserPosition::Pane,
            ..DOCKED
        };
        assert!(is_file_browser_shown(&placement, Some(true)));
        assert!(!is_file_browser_shown(&placement, Some(false)));
        assert!(!is_file_browser_shown(&placement, None));
    }

    #[test]
    fn follows_the_sidebar_tab_when_docked_there_whatever_the_tab_says() {
        assert!(is_file_browser_shown(&DOCKED, Some(false)));
        let hosts = FileBrowserPlacement {
            sidebar_view: SidebarView::Hosts,
            ..DOCKED
        };
        assert!(!is_file_browser_shown(&hosts, Some(true)));
    }

    #[test]
    fn is_hidden_with_the_sidebar() {
        let collapsed = FileBrowserPlacement {
            sidebar_collapsed: true,
            ..DOCKED
        };
        assert!(!is_file_browser_shown(&collapsed, None));
        let focus = FileBrowserPlacement {
            focus_mode: true,
            ..DOCKED
        };
        assert!(!is_file_browser_shown(&focus, None));
    }

    #[test]
    fn opens_beside_the_terminal_of_that_tab() {
        let mut stores = Stores::default();
        let id = with_ssh_tab(&mut stores);
        show_file_browser(&mut stores, &id);
        assert_eq!(stores.tab_sftp_open(&id), Some(true));
        assert_eq!(stores.sidebar_view, SidebarView::Hosts);
    }

    #[test]
    fn brings_the_sidebar_and_its_file_browser_tab_forward_when_docked_there() {
        let mut stores = Stores {
            position: FileBrowserPosition::Sidebar,
            sidebar_collapsed: true,
            ..Stores::default()
        };
        let id = with_ssh_tab(&mut stores);
        show_file_browser(&mut stores, &id);
        assert!(!stores.sidebar_collapsed);
        assert_eq!(stores.sidebar_view, SidebarView::Files);
        assert_eq!(stores.tab_sftp_open(&id), Some(false));
    }

    #[test]
    fn closes_the_browser_beside_the_terminal() {
        let mut stores = Stores::default();
        let id = with_ssh_tab(&mut stores);
        toggle_file_browser(&mut stores, &id);
        assert_eq!(stores.tab_sftp_open(&id), Some(true));
        toggle_file_browser(&mut stores, &id);
        assert_eq!(stores.tab_sftp_open(&id), Some(false));
    }

    #[test]
    fn switches_the_docked_sidebar_back_to_the_hosts_and_keeps_it_open() {
        let mut stores = Stores {
            position: FileBrowserPosition::Sidebar,
            ..Stores::default()
        };
        let id = with_ssh_tab(&mut stores);
        toggle_file_browser(&mut stores, &id);
        assert_eq!(stores.sidebar_view, SidebarView::Files);
        toggle_file_browser(&mut stores, &id);
        assert_eq!(stores.sidebar_view, SidebarView::Hosts);
        assert!(!stores.sidebar_collapsed);
    }

    #[test]
    fn opens_the_docked_browser_when_the_sidebar_is_hidden_on_its_file_tab() {
        let mut stores = Stores {
            position: FileBrowserPosition::Sidebar,
            sidebar_collapsed: true,
            sidebar_view: SidebarView::Files,
            ..Stores::default()
        };
        let id = with_ssh_tab(&mut stores);
        toggle_file_browser(&mut stores, &id);
        assert!(!stores.sidebar_collapsed);
        assert_eq!(stores.sidebar_view, SidebarView::Files);
    }

    #[test]
    fn docks_it_in_the_sidebar_with_the_file_browser_tab_in_front() {
        let mut stores = Stores::default();
        let id = with_ssh_tab(&mut stores);
        stores.set_tab_sftp_open(&id, true);
        move_file_browser(&mut stores, FileBrowserPosition::Sidebar, Some(&id));
        assert_eq!(stores.position, FileBrowserPosition::Sidebar);
        assert_eq!(stores.sidebar_view, SidebarView::Files);
    }

    #[test]
    fn opens_it_beside_the_terminal_of_the_tab_it_was_showing() {
        let mut stores = Stores {
            position: FileBrowserPosition::Sidebar,
            sidebar_view: SidebarView::Files,
            ..Stores::default()
        };
        let id = with_ssh_tab(&mut stores);
        move_file_browser(&mut stores, FileBrowserPosition::Pane, Some(&id));
        assert_eq!(stores.position, FileBrowserPosition::Pane);
        assert_eq!(stores.tab_sftp_open(&id), Some(true));
    }

    #[test]
    fn moving_without_a_session_only_changes_where_it_docks() {
        let mut stores = Stores {
            position: FileBrowserPosition::Sidebar,
            ..Stores::default()
        };
        move_file_browser(&mut stores, FileBrowserPosition::Pane, None);
        assert_eq!(stores.position, FileBrowserPosition::Pane);
        assert!(stores.tabs.is_empty());
    }

    #[test]
    fn says_why_for_each_kind_of_tab() {
        let facts = |kind: Option<&str>, status: &str, sftp: Option<bool>| BrowserTabFacts {
            profile_kind: kind.map(Into::into),
            status: status.into(),
            sftp_available: sftp,
        };
        assert_eq!(
            file_browser_unavailable_reason(None),
            "Open an SSH session to browse its files."
        );
        assert_eq!(
            file_browser_unavailable_reason(Some(&facts(None, "idle", None))),
            "Open an SSH session to browse its files."
        );
        assert_eq!(
            file_browser_unavailable_reason(Some(&facts(Some("local"), "connected", None))),
            "Only SSH sessions have a file browser."
        );
        assert_eq!(
            file_browser_unavailable_reason(Some(&facts(Some("ssh"), "connected", Some(false)))),
            "SFTP is turned off for this host."
        );
        assert_eq!(
            file_browser_unavailable_reason(Some(&facts(Some("ssh"), "connecting", None))),
            "Waiting for the session to connect…"
        );
        assert_eq!(
            file_browser_unavailable_reason(Some(&facts(Some("ssh"), "closed", None))),
            "The session is not connected."
        );
    }

    #[test]
    fn keeps_enough_room_for_both_the_browser_and_workspace() {
        assert_eq!(max_sftp_panel_width(1_200.), 840.);
        assert_eq!(max_sftp_panel_width(700.), 460.);
        assert_eq!(max_sftp_panel_width(400.), MIN_SFTP_PANEL_WIDTH);
    }

    #[test]
    fn rounds_and_clamps_dragged_widths() {
        assert_eq!(clamp_sftp_panel_width(100., 1_200.), MIN_SFTP_PANEL_WIDTH);
        assert_eq!(clamp_sftp_panel_width(441.6, 1_200.), 442.);
        assert_eq!(clamp_sftp_panel_width(1_000., 1_200.), 840.);
    }

    #[test]
    fn starts_at_the_terminal_directory_only_while_following_is_enabled() {
        assert_eq!(
            initial_sftp_path(".", Some("/srv/project"), true),
            "/srv/project"
        );
        assert_eq!(initial_sftp_path(".", Some("/srv/project"), false), ".");
    }

    #[test]
    fn falls_back_to_the_requested_initial_path_before_a_terminal_directory_is_reported() {
        assert_eq!(initial_sftp_path("/uploads", None, true), "/uploads");
    }
}
