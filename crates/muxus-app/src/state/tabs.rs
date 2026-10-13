//! Tabs and panes of one window (port of `client/src/state/tabs.ts`).
//!
//! The zustand store becomes [`TabsState`]: its fields are the store's state
//! and its methods the store's actions, with the same names in snake_case.
//! Hidden tabs stay in `tabs`; panes only point at the one they show.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use super::workspace_layout::{
    Branch, PaneDirection, PaneLeaf, PaneNode, RestoreWorkspaceOptions, SplitDirection,
    WorkspaceLayout, equalize_splits, find_pane, first_pane, neighbor_pane_id, panes_in_order,
    parent_split, places_pane_first, remove_pane, restore_workspace, set_active_tab, split_axis,
    update_pane, update_split_ratio,
};
use crate::model::{ProfileKind, SessionProfile};

/// Keyboard resize step, as a fraction of the split's own extent.
pub const PANE_RESIZE_STEP: f64 = 0.04;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TabStatus {
    Connecting,
    Connected,
    Interrupted,
    Closed,
    /// A blank tab offering the session chooser (`EmptyTab`).
    Idle,
}

/// Multiplexer to attach after a replacement SSH shell is ready
/// (`ReattachMode` in `client/src/connection-recovery.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReattachMode {
    Tmux,
    Screen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisconnectReason {
    Completed,
    Failed,
    Disconnected,
}

/// The host's login sequence while it runs: the current step and what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginSequenceProgress {
    pub step: u32,
    pub steps: u32,
    pub detail: String,
}

/// Which side of a drop target a tab lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Edge {
    Before,
    After,
}

/// One tab: a session (`SessionTab`) or the blank session chooser (`EmptyTab`,
/// `profile: None` with status [`TabStatus::Idle`]).
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalTab {
    pub id: String,
    pub pane_id: String,
    pub title: String,
    pub profile: Option<SessionProfile>,
    pub status: TabStatus,
    /// Whether this tab should establish its session when the terminal view mounts.
    pub connect_on_mount: bool,
    /// Live SSH connection id from `ready`; absent for local, Telnet, and serial tabs.
    pub conn_id: Option<String>,
    /// Stable server-side identity for renderer reattachment, including during setup.
    pub terminal_id: Option<String>,
    /// Cross-window transfer awaiting acknowledgement from the source window.
    pub transfer_id: Option<String>,
    /// SFTP file panel visible for this tab.
    pub sftp_open: bool,
    /// Server-advertised capability for the current SSH transport.
    pub sftp_available: Option<bool>,
    /// Most recent working directory reported by a live local or remote shell.
    pub terminal_cwd: Option<String>,
    /// Explicit opt-out from following the terminal in the attached SFTP panel.
    pub sftp_follow_terminal: Option<bool>,
    /// Monotonic UI signal consumed by the mounted terminal search bar.
    pub search_request: u32,
    /// User-set color flag marking the tab.
    pub color: Option<String>,
    /// Pinned tabs stay grouped at the start of their pane.
    pub pinned: bool,
    /// Files open in the editor workspace attached to this session.
    pub editor_paths: Vec<String>,
    pub active_editor_path: Option<String>,
    /// The terminal is in front of its open files, which stay loaded behind it.
    pub terminal_shown: Option<bool>,
    /// Durable server-side history state for this live tab.
    pub logging_enabled: Option<bool>,
    pub session_log_id: Option<String>,
    pub logging_warning: Option<String>,
    pub logging_paused: bool,
    pub capture_input: bool,
    /// Plain-text log file the live session is written to.
    pub log_file_path: Option<String>,
    /// Monotonic signal used to reconnect one or many mounted terminal views.
    pub reconnect_request: u32,
    /// Optional multiplexer to attach after the replacement SSH shell is ready.
    pub reconnect_mode: Option<ReattachMode>,
    /// Replacement-group token: the next SSH connection must not multiplex onto
    /// an established transport, but connects sharing the token (one force
    /// reconnect gesture) share one replacement connection. Kept across failed
    /// retries; cleared when a connection succeeds.
    pub fresh_transport: Option<String>,
    /// Most recent connection/end reason, shown without requiring terminal scrollback.
    pub failure_reason: Option<String>,
    pub disconnect_reason: Option<DisconnectReason>,
    /// Tab came from a persisted workspace layout, so stored scrollback may exist.
    pub restored: bool,
    /// The host's login sequence while it runs: the current step and what it does.
    pub login_sequence: Option<LoginSequenceProgress>,
}

impl TerminalTab {
    /// A tab in its freshly opened state: a session dialing, or a blank chooser.
    fn fresh(id: String, pane_id: String, title: String, profile: Option<SessionProfile>) -> Self {
        let blank = profile.is_none();
        TerminalTab {
            id,
            pane_id,
            title,
            profile,
            status: if blank {
                TabStatus::Idle
            } else {
                TabStatus::Connecting
            },
            connect_on_mount: !blank,
            conn_id: None,
            terminal_id: None,
            transfer_id: None,
            sftp_open: false,
            sftp_available: None,
            terminal_cwd: None,
            sftp_follow_terminal: None,
            search_request: 0,
            color: None,
            pinned: false,
            editor_paths: Vec::new(),
            active_editor_path: None,
            terminal_shown: None,
            logging_enabled: None,
            session_log_id: None,
            logging_warning: None,
            logging_paused: false,
            capture_input: false,
            log_file_path: None,
            reconnect_request: 0,
            reconnect_mode: None,
            fresh_transport: None,
            failure_reason: None,
            disconnect_reason: None,
            restored: false,
            login_sequence: None,
        }
    }

    /// The blank session chooser, rather than a session.
    pub fn is_empty(&self) -> bool {
        self.profile.is_none()
    }

    /// Shared patch for dialing a replacement session into an existing tab. An
    /// unfulfilled fresh-transport request survives retries (the ready handler
    /// clears it on success), so a failed forced dial never falls back silently
    /// onto the very transport it was meant to replace.
    fn apply_reconnect(&mut self, reattach: Option<ReattachMode>, fresh_transport: Option<String>) {
        let ssh = self
            .profile
            .as_ref()
            .is_some_and(|profile| profile.kind() == ProfileKind::Ssh);
        self.status = TabStatus::Connecting;
        self.connect_on_mount = true;
        self.conn_id = None;
        self.terminal_id = None;
        self.transfer_id = None;
        self.reconnect_request += 1;
        self.reconnect_mode = if ssh { reattach } else { None };
        self.fresh_transport = fresh_transport.or(self.fresh_transport.take());
        self.failure_reason = None;
        self.disconnect_reason = None;
    }

    /// Reset the per-session fields when a new session takes over the tab.
    fn reset_session(&mut self) {
        self.conn_id = None;
        self.sftp_open = false;
        self.sftp_available = None;
        self.terminal_cwd = None;
        self.search_request = 0;
        self.editor_paths = Vec::new();
        self.active_editor_path = None;
        self.terminal_shown = None;
        self.logging_enabled = None;
        self.session_log_id = None;
        self.logging_warning = None;
        self.logging_paused = false;
        self.capture_input = false;
        self.log_file_path = None;
        self.reconnect_request = 0;
    }
}

/// `TabUpdate`: a partial update of a tab. `None` leaves a field alone;
/// `Some(None)` clears an optional one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabPatch {
    pub title: Option<String>,
    pub status: Option<TabStatus>,
    pub conn_id: Option<Option<String>>,
    pub terminal_id: Option<Option<String>>,
    pub transfer_id: Option<Option<String>>,
    pub sftp_open: Option<bool>,
    pub sftp_available: Option<Option<bool>>,
    pub terminal_cwd: Option<Option<String>>,
    pub sftp_follow_terminal: Option<Option<bool>>,
    pub color: Option<Option<String>>,
    pub logging_enabled: Option<Option<bool>>,
    pub session_log_id: Option<Option<String>>,
    pub logging_warning: Option<Option<String>>,
    pub logging_paused: Option<bool>,
    pub capture_input: Option<bool>,
    pub log_file_path: Option<Option<String>>,
    pub failure_reason: Option<Option<String>>,
    pub disconnect_reason: Option<Option<DisconnectReason>>,
    pub fresh_transport: Option<Option<String>>,
    pub login_sequence: Option<Option<LoginSequenceProgress>>,
}

impl TabPatch {
    fn apply(self, tab: &mut TerminalTab) {
        // A blank chooser has no session, so the session-only fields of a
        // patch are dropped rather than applied to it.
        let session = tab.profile.is_some();
        macro_rules! set {
            ($field:ident) => {
                if let Some(value) = self.$field {
                    tab.$field = value;
                }
            };
        }
        macro_rules! set_session {
            ($field:ident) => {
                if session && let Some(value) = self.$field {
                    tab.$field = value;
                }
            };
        }
        set!(title);
        set_session!(status);
        set_session!(conn_id);
        set!(terminal_id);
        set!(transfer_id);
        set_session!(sftp_open);
        set_session!(sftp_available);
        set_session!(terminal_cwd);
        set_session!(sftp_follow_terminal);
        set!(color);
        set!(logging_enabled);
        set!(session_log_id);
        set!(logging_warning);
        set!(logging_paused);
        set!(capture_input);
        set!(log_file_path);
        set!(failure_reason);
        set!(disconnect_reason);
        set!(fresh_transport);
        set!(login_sequence);
    }
}

/// One session of a launched set.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSetEntry {
    pub profile: SessionProfile,
    pub title: String,
    pub color: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionSetLayout {
    Tabs,
    Columns,
    Rows,
    Grid,
}

/// Generates tab, pane and split ids: `<prefix>-<stamp>-<n>`, where the stamp
/// is the creation time in base 36, as the TypeScript does. Tests use
/// [`IdGen::sequential`] for predictable ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdGen {
    stamp: Option<String>,
    next: u64,
}

impl IdGen {
    /// Ids stamped with the current time, unique across windows and restarts.
    pub fn timestamped() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or_default();
        IdGen {
            stamp: Some(to_base36(millis)),
            next: 1,
        }
    }

    /// `<prefix>-1`, `<prefix>-2`, …
    pub fn sequential() -> Self {
        IdGen {
            stamp: None,
            next: 1,
        }
    }

    pub fn next(&mut self, prefix: &str) -> String {
        let n = self.next;
        self.next += 1;
        match &self.stamp {
            Some(stamp) => format!("{prefix}-{stamp}-{n}"),
            None => format!("{prefix}-{n}"),
        }
    }
}

impl Default for IdGen {
    fn default() -> Self {
        IdGen::timestamped()
    }
}

fn to_base36(mut value: u128) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// The tabs and pane canvas of one window.
#[derive(Clone, Debug, PartialEq)]
pub struct TabsState {
    /// Every tab, grouped by pane in strip order.
    pub tabs: Vec<TerminalTab>,
    /// Terminal tabs with output that arrived while they were not visible.
    pub unread_output_ids: HashSet<String>,
    pub root: PaneNode,
    pub active_pane_id: String,
    pub active_id: Option<String>,
    /// Pane temporarily filling the canvas; siblings stay mounted underneath.
    pub zoomed_pane_id: Option<String>,
    pub ids: IdGen,
}

impl Default for TabsState {
    fn default() -> Self {
        TabsState::new(IdGen::timestamped())
    }
}

/// Session tabs that dial a transport: everything but local shells.
pub fn is_remote_session_tab(tab: &TerminalTab) -> bool {
    tab.profile
        .as_ref()
        .is_some_and(|profile| profile.kind() != ProfileKind::Local)
}

/// Connected terminal sessions, the tabs mirrored input can reach. Remote
/// desktops and management sessions are connected too, but have no terminal
/// to type into.
pub fn is_multi_exec_target(tab: &TerminalTab) -> bool {
    tab.status == TabStatus::Connected
        && tab
            .profile
            .as_ref()
            .is_some_and(SessionProfile::is_terminal)
}

/// Serial consoles own the device exclusively, so a second session on the same
/// port would only fail; everything else can be dialed again
/// (`canReopen` in `client/src/session-actions.ts`).
pub fn can_reopen(profile: &SessionProfile) -> bool {
    profile.kind() != ProfileKind::Serial
}

/// Whether a terminal, rather than one of its editors, is visible on the canvas.
fn terminal_is_visible(tab: &TerminalTab, root: &PaneNode, zoomed_pane_id: Option<&str>) -> bool {
    (tab.active_editor_path.is_none() || tab.terminal_shown == Some(true))
        && zoomed_pane_id.is_none_or(|zoomed| zoomed == tab.pane_id)
        && find_pane(root, &tab.pane_id).and_then(|pane| pane.active_tab_id.as_deref())
            == Some(tab.id.as_str())
}

/// Drop notifications for every terminal currently visible on the canvas.
fn clear_visible_output(
    unread: &mut HashSet<String>,
    tabs: &[TerminalTab],
    root: &PaneNode,
    zoomed_pane_id: Option<&str>,
) {
    if unread.is_empty() {
        return;
    }
    for tab in tabs {
        if terminal_is_visible(tab, root, zoomed_pane_id) {
            unread.remove(&tab.id);
        }
    }
}

/// Drop an emptied pane and hand focus to the sibling that takes over its
/// space, so closing the last tab of a split behaves like closing the pane.
/// Returns `None` when the pane is the whole canvas: that one stays.
fn collapse_pane(root: &PaneNode, pane_id: &str) -> Option<(PaneNode, PaneLeaf)> {
    if root.is_pane() {
        return None;
    }
    let sibling = parent_split(root, pane_id)
        .map(|(split, branch)| split.children[branch.other().index()].clone());
    let next = remove_pane(root, pane_id)?;
    let focus = first_pane(sibling.as_ref().unwrap_or(&next)).clone();
    Some((next, focus))
}

/// Replace one pane's ordered tabs without disturbing the other panes' slots.
fn replace_pane_tabs(
    tabs: &[TerminalTab],
    pane_id: &str,
    pane_tabs: Vec<TerminalTab>,
) -> Vec<TerminalTab> {
    let mut replacements = pane_tabs.into_iter();
    tabs.iter()
        .map(|tab| {
            if tab.pane_id == pane_id {
                replacements.next().unwrap_or_else(|| tab.clone())
            } else {
                tab.clone()
            }
        })
        .collect()
}

/// Tabs in deterministic window-wide order: panes first, then each strip left-to-right.
pub fn tabs_in_order<'a>(root: &PaneNode, tabs: &'a [TerminalTab]) -> Vec<&'a TerminalTab> {
    let mut by_pane: HashMap<&str, Vec<&TerminalTab>> = HashMap::new();
    for tab in tabs {
        by_pane.entry(tab.pane_id.as_str()).or_default().push(tab);
    }
    panes_in_order(root)
        .into_iter()
        .flat_map(|pane| by_pane.remove(pane.id.as_str()).unwrap_or_default())
        .collect()
}

/// Browser-style bulk close target: unpinned tabs after one tab in its strip.
pub fn closable_tab_ids_to_right<'a>(
    pane_tabs: impl IntoIterator<Item = &'a TerminalTab>,
    tab_id: &str,
) -> Vec<String> {
    let pane_tabs: Vec<&TerminalTab> = pane_tabs.into_iter().collect();
    let Some(index) = pane_tabs.iter().position(|tab| tab.id == tab_id) else {
        return Vec::new();
    };
    pane_tabs[index + 1..]
        .iter()
        .filter(|tab| !tab.pinned)
        .map(|tab| tab.id.clone())
        .collect()
}

/// Insert a tab at an exact target, while keeping each pane's pinned group first.
pub fn insert_into_pane(
    tabs: &[TerminalTab],
    tab: &TerminalTab,
    pane_id: &str,
    target_id: Option<&str>,
    edge: Edge,
) -> Vec<TerminalTab> {
    let mut next = tabs.to_vec();
    let pane_tabs: Vec<&TerminalTab> = tabs
        .iter()
        .filter(|candidate| candidate.pane_id == pane_id)
        .collect();
    let target_index = pane_tabs.iter().position(|candidate| {
        Some(candidate.id.as_str()) == target_id && candidate.pinned == tab.pinned
    });
    let first_unpinned = pane_tabs.iter().position(|candidate| !candidate.pinned);
    let pane_index = match target_index {
        Some(index) => index + usize::from(edge == Edge::After),
        None if tab.pinned => first_unpinned.unwrap_or(pane_tabs.len()),
        None => pane_tabs.len(),
    };
    let anchor = pane_tabs.get(pane_index);
    let previous = pane_index
        .checked_sub(1)
        .and_then(|index| pane_tabs.get(index));
    let index = if let Some(anchor) = anchor {
        tabs.iter()
            .position(|candidate| candidate.id == anchor.id)
            .unwrap_or(tabs.len())
    } else if let Some(previous) = previous {
        tabs.iter()
            .position(|candidate| candidate.id == previous.id)
            .map_or(tabs.len(), |index| index + 1)
    } else {
        tabs.len()
    };
    let mut moved = tab.clone();
    moved.pane_id = pane_id.to_owned();
    next.insert(index, moved);
    next
}

impl TabsState {
    /// An empty window: one pane with a generated id and no tabs.
    pub fn new(mut ids: IdGen) -> Self {
        let pane_id = ids.next("pane");
        TabsState::with_pane(pane_id, ids)
    }

    /// An empty window whose single pane has a known id.
    pub fn with_pane(pane_id: impl Into<String>, ids: IdGen) -> Self {
        let pane_id = pane_id.into();
        TabsState {
            tabs: Vec::new(),
            unread_output_ids: HashSet::new(),
            root: PaneNode::pane(pane_id.clone(), None),
            active_pane_id: pane_id,
            active_id: None,
            zoomed_pane_id: None,
            ids,
        }
    }

    pub fn tab(&self, id: &str) -> Option<&TerminalTab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn active_tab(&self) -> Option<&TerminalTab> {
        self.active_id.as_deref().and_then(|id| self.tab(id))
    }

    /// The tabs of one pane, in strip order.
    pub fn pane_tabs(&self, pane_id: &str) -> Vec<&TerminalTab> {
        self.tabs
            .iter()
            .filter(|tab| tab.pane_id == pane_id)
            .collect()
    }

    fn tab_mut(&mut self, id: &str) -> Option<&mut TerminalTab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    /// The pane new tabs go to: the focused one, or the first if it is gone.
    fn target_pane_id(&self) -> String {
        find_pane(&self.root, &self.active_pane_id)
            .unwrap_or_else(|| first_pane(&self.root))
            .id
            .clone()
    }

    fn push_and_focus(&mut self, tab: TerminalTab) -> String {
        let id = tab.id.clone();
        let pane_id = tab.pane_id.clone();
        self.tabs.push(tab);
        self.root = set_active_tab(&self.root, &pane_id, Some(id.clone()));
        self.active_pane_id = pane_id;
        self.active_id = Some(id.clone());
        id
    }

    fn clear_visible_output(&mut self) {
        clear_visible_output(
            &mut self.unread_output_ids,
            &self.tabs,
            &self.root,
            self.zoomed_pane_id.as_deref(),
        );
    }

    /// Open a session in the focused pane and show it.
    pub fn open(&mut self, profile: SessionProfile, title: impl Into<String>) -> String {
        let id = self.ids.next("tab");
        let pane_id = self.target_pane_id();
        self.push_and_focus(TerminalTab::fresh(id, pane_id, title.into(), Some(profile)))
    }

    /// Open the blank session chooser in the focused pane.
    pub fn open_empty(&mut self) -> String {
        let id = self.ids.next("tab");
        let pane_id = self.target_pane_id();
        self.push_and_focus(TerminalTab::fresh(id, pane_id, "New tab".into(), None))
    }

    /// Turn a blank chooser into a session; false when `id` is not a chooser.
    pub fn replace_empty(
        &mut self,
        id: &str,
        profile: SessionProfile,
        title: impl Into<String>,
    ) -> bool {
        let Some(tab) = self.tab_mut(id).filter(|tab| tab.profile.is_none()) else {
            return false;
        };
        tab.title = title.into();
        tab.profile = Some(profile);
        tab.status = TabStatus::Connecting;
        tab.connect_on_mount = true;
        tab.reset_session();
        true
    }

    pub fn close(&mut self, id: &str) {
        let Some(closing) = self.tab(id).cloned() else {
            return;
        };
        let pane_tabs = self.pane_tabs(&closing.pane_id);
        let index = pane_tabs.iter().position(|tab| tab.id == id).unwrap_or(0);
        let others: Vec<&&TerminalTab> = pane_tabs.iter().filter(|tab| tab.id != id).collect();
        let replacement = pane_tabs
            .len()
            .checked_sub(2)
            .and_then(|last| others.get(index.min(last)))
            .map(|tab| tab.id.clone());
        let tabs: Vec<TerminalTab> = self
            .tabs
            .iter()
            .filter(|tab| tab.id != id)
            .cloned()
            .collect();
        self.unread_output_ids.remove(id);
        // The last tab of a split pane takes the pane with it, the way closing
        // the last program in a tmux pane does.
        let collapsed = if replacement.is_none() {
            collapse_pane(&self.root, &closing.pane_id)
        } else {
            None
        };
        if let Some((root, focus)) = collapsed {
            let focused = self.active_pane_id == closing.pane_id;
            if self.zoomed_pane_id.as_deref() == Some(closing.pane_id.as_str()) {
                self.zoomed_pane_id = None;
            }
            self.tabs = tabs;
            self.root = root;
            self.clear_visible_output();
            if focused {
                self.active_pane_id = focus.id;
                self.active_id = focus.active_tab_id;
            }
            return;
        }
        let pane_active_id = match find_pane(&self.root, &closing.pane_id) {
            Some(pane) if pane.active_tab_id.as_deref() == Some(id) => replacement,
            Some(pane) => pane.active_tab_id.clone(),
            None => None,
        };
        self.root = set_active_tab(&self.root, &closing.pane_id, pane_active_id.clone());
        self.tabs = tabs;
        self.clear_visible_output();
        if self.active_id.as_deref() == Some(id) {
            self.active_id = pane_active_id;
        }
    }

    pub fn activate(&mut self, id: &str) {
        let Some(pane_id) = self.tab(id).map(|tab| tab.pane_id.clone()) else {
            return;
        };
        self.root = set_active_tab(&self.root, &pane_id, Some(id.to_owned()));
        if self.zoomed_pane_id.as_deref() != Some(pane_id.as_str()) {
            self.zoomed_pane_id = None;
        }
        self.clear_visible_output();
        self.active_pane_id = pane_id;
        self.active_id = Some(id.to_owned());
    }

    pub fn focus_pane(&mut self, pane_id: &str) {
        let Some(pane) = find_pane(&self.root, pane_id).cloned() else {
            return;
        };
        if self.zoomed_pane_id.as_deref() != Some(pane_id) {
            self.zoomed_pane_id = None;
        }
        self.clear_visible_output();
        self.active_pane_id = pane_id.to_owned();
        self.active_id = pane.active_tab_id;
    }

    /// Step to the next (or previous) tab of the focused pane.
    pub fn cycle(&mut self, backwards: bool) {
        let pane_tabs = self.pane_tabs(&self.active_pane_id);
        if pane_tabs.len() < 2 {
            return;
        }
        let len = pane_tabs.len() as isize;
        let index = pane_tabs
            .iter()
            .position(|tab| Some(&tab.id) == self.active_id.as_ref())
            .map_or(-1, |index| index as isize);
        let next = (index + if backwards { -1 } else { 1 } + len).rem_euclid(len) as usize;
        let active_id = pane_tabs[next].id.clone();
        self.unread_output_ids.remove(&active_id);
        self.root = set_active_tab(&self.root, &self.active_pane_id, Some(active_id.clone()));
        self.active_id = Some(active_id);
    }

    /// Activate the nth tab across the whole window in pane/strip order.
    pub fn activate_tab_index(&mut self, index: usize) -> bool {
        let Some(target) = tabs_in_order(&self.root, &self.tabs)
            .get(index)
            .map(|tab| tab.id.clone())
        else {
            return false;
        };
        if Some(&target) != self.active_id.as_ref() {
            self.activate(&target);
        }
        true
    }

    /// Reorder the active tab inside its own pane.
    pub fn move_tab_within_pane(&mut self, offset: isize) -> bool {
        let pane_tabs = self.pane_tabs(&self.active_pane_id);
        let Some(position) = pane_tabs
            .iter()
            .position(|tab| Some(&tab.id) == self.active_id.as_ref())
        else {
            return false;
        };
        let Some(swap_with) = position
            .checked_add_signed(offset)
            .and_then(|index| pane_tabs.get(index))
        else {
            return false;
        };
        if swap_with.pinned != pane_tabs[position].pinned {
            return false;
        }
        let (id, target) = (pane_tabs[position].id.clone(), swap_with.id.clone());
        self.reorder_tab(
            &id,
            &target,
            if offset < 0 {
                Edge::Before
            } else {
                Edge::After
            },
        )
    }

    /// Place one tab before or after another tab in the same pane and pin group.
    pub fn reorder_tab(&mut self, id: &str, target_id: &str, edge: Edge) -> bool {
        let (Some(tab), Some(target)) = (self.tab(id), self.tab(target_id)) else {
            return false;
        };
        if tab.id == target.id || tab.pane_id != target.pane_id || tab.pinned != target.pinned {
            return false;
        }
        let pane_id = target.pane_id.clone();
        self.move_tab_to_pane(id, &pane_id, Some(target_id), edge)
    }

    /// Move a tab to an exact position in another split pane.
    pub fn move_tab_to_pane(
        &mut self,
        id: &str,
        pane_id: &str,
        target_id: Option<&str>,
        edge: Edge,
    ) -> bool {
        let Some(tab) = self.tab(id).cloned() else {
            return false;
        };
        if find_pane(&self.root, pane_id).is_none() || target_id == Some(id) {
            return false;
        }
        if let Some(target_id) = target_id
            && !self
                .tabs
                .iter()
                .any(|candidate| candidate.id == target_id && candidate.pane_id == pane_id)
        {
            return false;
        }
        let source_id = tab.pane_id.clone();
        let source_index = self
            .pane_tabs(&source_id)
            .iter()
            .position(|candidate| candidate.id == id);
        let others: Vec<TerminalTab> = self
            .tabs
            .iter()
            .filter(|candidate| candidate.id != id)
            .cloned()
            .collect();
        let tabs = insert_into_pane(&others, &tab, pane_id, target_id, edge);
        let unchanged =
            self.tabs.len() == tabs.len()
                && self.tabs.iter().zip(&tabs).all(|(before, after)| {
                    before.id == after.id && before.pane_id == after.pane_id
                });
        if unchanged {
            return false;
        }
        if source_id == pane_id {
            self.tabs = tabs;
            return true;
        }

        let remaining: Vec<&TerminalTab> = tabs
            .iter()
            .filter(|candidate| candidate.pane_id == source_id)
            .collect();
        let previous_source_active_id =
            find_pane(&self.root, &source_id).and_then(|pane| pane.active_tab_id.clone());
        let source_active_id = if previous_source_active_id.as_deref() == Some(id) {
            remaining
                .len()
                .checked_sub(1)
                .and_then(|last| remaining.get(source_index.unwrap_or(0).min(last)))
                .map(|candidate| candidate.id.clone())
        } else {
            previous_source_active_id
        };
        let mut root = set_active_tab(&self.root, &source_id, source_active_id);
        root = set_active_tab(&root, pane_id, Some(id.to_owned()));
        let mut zoomed_pane_id = self.zoomed_pane_id.clone();
        if remaining.is_empty()
            && let Some((collapsed, _)) = collapse_pane(&root, &source_id)
        {
            root = collapsed;
            if zoomed_pane_id.as_deref() == Some(source_id.as_str()) {
                zoomed_pane_id = None;
            }
        }
        self.tabs = tabs;
        self.root = root;
        self.zoomed_pane_id = zoomed_pane_id.filter(|zoomed| zoomed == pane_id);
        self.clear_visible_output();
        self.active_pane_id = pane_id.to_owned();
        self.active_id = Some(id.to_owned());
        true
    }

    /// Pin or unpin a tab and move it to the corresponding strip boundary.
    pub fn set_pinned(&mut self, id: &str, pinned: bool) -> bool {
        let Some(tab) = self.tab(id).cloned() else {
            return false;
        };
        if tab.pinned == pinned {
            return false;
        }
        let mut reordered: Vec<TerminalTab> = self
            .tabs
            .iter()
            .filter(|candidate| candidate.pane_id == tab.pane_id && candidate.id != id)
            .cloned()
            .collect();
        let index = reordered
            .iter()
            .position(|candidate| !candidate.pinned)
            .unwrap_or(reordered.len());
        reordered.insert(
            index,
            TerminalTab {
                pinned,
                ..tab.clone()
            },
        );
        self.tabs = replace_pane_tabs(&self.tabs, &tab.pane_id, reordered);
        true
    }

    /// Split a pane in place; returns the new, focused pane.
    pub fn split(&mut self, pane_id: &str, direction: PaneDirection) -> Option<String> {
        find_pane(&self.root, pane_id)?;
        let pane_id_new = self.ids.next("pane");
        let split_id = self.ids.next("split");
        let new_pane = PaneNode::pane(pane_id_new.clone(), None);
        self.root = update_pane(&self.root, pane_id, |existing| {
            let existing = PaneNode::Pane(existing.clone());
            let (first, second) = if places_pane_first(direction) {
                (new_pane, existing)
            } else {
                (existing, new_pane)
            };
            PaneNode::split(split_id, split_axis(direction), 0.5, first, second)
        });
        self.zoomed_pane_id = None;
        self.clear_visible_output();
        self.active_pane_id = pane_id_new.clone();
        self.active_id = None;
        Some(pane_id_new)
    }

    /// Split the focused pane and, when `inherit_session` (the
    /// `splitInheritsSession` preference) is on, carry the current session into
    /// it: the tmux reflex of "same box, one more pane". Panes with nothing to
    /// copy open the session chooser instead (`splitActivePane` in
    /// `client/src/session-actions.ts`).
    pub fn split_active_pane(&mut self, direction: PaneDirection, inherit_session: bool) -> bool {
        let source = self.active_tab().cloned();
        let pane_id = self.active_pane_id.clone();
        if self.split(&pane_id, direction).is_none() {
            return false;
        }
        if inherit_session
            && let Some(source) = source
            && let Some(profile) = source.profile.filter(can_reopen)
        {
            let id = self.open(profile, source.title);
            if let Some(color) = source.color {
                self.update(
                    &id,
                    TabPatch {
                        color: Some(Some(color)),
                        ..TabPatch::default()
                    },
                );
            }
        }
        true
    }

    /// Open a second session of a tab beside it (`duplicateTab` in
    /// `client/src/session-actions.ts`).
    pub fn duplicate_tab(&mut self, tab_id: &str) -> bool {
        let Some(tab) = self.tab(tab_id).cloned() else {
            return false;
        };
        let Some(profile) = tab.profile else {
            return false;
        };
        let id = self.open(profile, tab.title);
        if let Some(color) = tab.color {
            self.update(
                &id,
                TabPatch {
                    color: Some(Some(color)),
                    ..TabPatch::default()
                },
            );
        }
        true
    }

    /// Close a pane and every tab in it; the last pane of the canvas stays.
    pub fn close_pane(&mut self, pane_id: &str) {
        let Some((root, focus)) = collapse_pane(&self.root, pane_id) else {
            return;
        };
        let focused = self.active_pane_id == pane_id;
        if self.zoomed_pane_id.as_deref() == Some(pane_id) {
            self.zoomed_pane_id = None;
        }
        let closed: HashSet<String> = self
            .pane_tabs(pane_id)
            .iter()
            .map(|tab| tab.id.clone())
            .collect();
        self.unread_output_ids.retain(|id| !closed.contains(id));
        self.tabs.retain(|tab| tab.pane_id != pane_id);
        self.root = root;
        self.clear_visible_output();
        if focused {
            self.active_pane_id = focus.id;
            self.active_id = focus.active_tab_id;
        }
    }

    /// Focus the pane that borders the active one in a direction.
    pub fn focus_pane_direction(&mut self, direction: PaneDirection) -> bool {
        let Some(target) = neighbor_pane_id(&self.root, &self.active_pane_id, direction) else {
            return false;
        };
        self.focus_pane(&target);
        true
    }

    /// Focus the next/previous pane in reading order.
    pub fn cycle_pane(&mut self, backwards: bool) -> bool {
        let panes = panes_in_order(&self.root);
        if panes.len() < 2 {
            return false;
        }
        let len = panes.len() as isize;
        let index = panes
            .iter()
            .position(|pane| pane.id == self.active_pane_id)
            .map_or(-1, |index| index as isize);
        let next = panes[(index + if backwards { -1 } else { 1 } + len).rem_euclid(len) as usize]
            .id
            .clone();
        self.focus_pane(&next);
        true
    }

    /// Move the active tab to the bordering pane, splitting off a new one when there is none.
    pub fn move_tab_to_direction(&mut self, direction: PaneDirection) -> bool {
        let Some(tab) = self.active_tab().cloned() else {
            return false;
        };
        let source_count = self.pane_tabs(&tab.pane_id).len();
        let neighbor = neighbor_pane_id(&self.root, &tab.pane_id, direction);
        // Splitting off the only tab of a pane would just move the pane around.
        if neighbor.is_none() && source_count < 2 {
            return false;
        }
        let target = match neighbor {
            Some(target) => Some(target),
            None => self.split(&tab.pane_id, direction),
        };
        match target {
            Some(target) => self.move_tab_to_pane(&tab.id, &target, None, Edge::After),
            None => false,
        }
    }

    /// Split a specific tab into a newly created neighbouring pane.
    pub fn move_tab_to_new_pane(&mut self, id: &str, direction: PaneDirection) -> bool {
        let Some(pane_id) = self.tab(id).map(|tab| tab.pane_id.clone()) else {
            return false;
        };
        // Splitting away the only tab would leave an empty source pane and merely
        // relocate the session, rather than turning a tabbed pair into two panes.
        if self.pane_tabs(&pane_id).len() < 2 {
            return false;
        }
        match self.split(&pane_id, direction) {
            Some(target) => self.move_tab_to_pane(id, &target, None, Edge::After),
            None => false,
        }
    }

    /// Fill the canvas with one pane (tmux-style zoom), or restore the layout.
    pub fn toggle_zoom(&mut self, pane_id: Option<&str>) -> bool {
        let target = pane_id.unwrap_or(&self.active_pane_id).to_owned();
        if self.root.is_pane() && self.zoomed_pane_id.is_none() {
            return false;
        }
        let Some(pane) = find_pane(&self.root, &target).cloned() else {
            return false;
        };
        self.zoomed_pane_id = if self.zoomed_pane_id.as_deref() == Some(target.as_str()) {
            None
        } else {
            Some(target.clone())
        };
        self.clear_visible_output();
        self.active_pane_id = target;
        self.active_id = pane.active_tab_id;
        true
    }

    pub fn resize_split(&mut self, split_id: &str, ratio: f64) {
        self.root = update_split_ratio(&self.root, split_id, ratio);
    }

    /// Grow or shrink the focused pane inside its nearest split.
    pub fn resize_active_pane(&mut self, delta: f64) -> bool {
        let Some((split, branch)) = parent_split(&self.root, &self.active_pane_id) else {
            return false;
        };
        let ratio = split.ratio
            + if branch == Branch::First {
                delta
            } else {
                -delta
            };
        let split_id = split.id.clone();
        self.root = update_split_ratio(&self.root, &split_id, ratio);
        true
    }

    /// Give every pane an equal share of the canvas.
    pub fn equalize_panes(&mut self) -> bool {
        if self.root.is_pane() {
            return false;
        }
        self.root = equalize_splits(&self.root);
        true
    }

    pub fn request_search(&mut self) {
        if let Some(id) = self.active_id.clone()
            && let Some(tab) = self.tab_mut(&id)
        {
            tab.search_request += 1;
        }
    }

    pub fn open_editor(&mut self, tab_id: &str, path: &str) {
        if let Some(tab) = self.tab_mut(tab_id).filter(|tab| tab.profile.is_some()) {
            if !tab.editor_paths.iter().any(|candidate| candidate == path) {
                tab.editor_paths.push(path.to_owned());
            }
            tab.active_editor_path = Some(path.to_owned());
            tab.terminal_shown = Some(false);
        }
    }

    pub fn activate_editor(&mut self, tab_id: &str, path: &str) {
        if let Some(tab) = self
            .tab_mut(tab_id)
            .filter(|tab| tab.editor_paths.iter().any(|candidate| candidate == path))
        {
            tab.active_editor_path = Some(path.to_owned());
            tab.terminal_shown = Some(false);
        }
    }

    pub fn close_editor(&mut self, tab_id: &str, path: &str) {
        if let Some(tab) = self.tab_mut(tab_id)
            && let Some(index) = tab
                .editor_paths
                .iter()
                .position(|candidate| candidate == path)
        {
            tab.editor_paths.remove(index);
            if tab.active_editor_path.as_deref() == Some(path) {
                tab.active_editor_path = tab
                    .editor_paths
                    .len()
                    .checked_sub(1)
                    .map(|last| tab.editor_paths[index.min(last)].clone());
            }
            tab.terminal_shown = if tab.editor_paths.is_empty() {
                Some(false)
            } else {
                tab.terminal_shown
            };
        }
        self.clear_visible_output();
    }

    /// Bring a session's terminal in front of its open files without closing
    /// them. Returns false, changing nothing, when no file is in front.
    pub fn show_terminal(&mut self, tab_id: &str) -> bool {
        let Some(tab) = self
            .tab_mut(tab_id)
            .filter(|tab| tab.active_editor_path.is_some() && tab.terminal_shown != Some(true))
        else {
            return false;
        };
        tab.terminal_shown = Some(true);
        self.clear_visible_output();
        true
    }

    /// Switch the active session between its terminal and its open files
    /// (the `terminal.toggle-editor` command). False when it has no files open.
    pub fn toggle_editor(&mut self) -> bool {
        let Some(tab) = self.active_tab().cloned() else {
            return false;
        };
        let Some(path) = tab.active_editor_path else {
            return false;
        };
        if tab.terminal_shown == Some(true) {
            self.activate_editor(&tab.id, &path);
        } else {
            self.show_terminal(&tab.id);
        }
        true
    }

    /// Replace the canvas with a saved workspace layout.
    pub fn restore(&mut self, layout: &WorkspaceLayout, options: RestoreWorkspaceOptions) {
        if layout.root.is_none() {
            let pane_id = self.ids.next("pane");
            self.tabs = Vec::new();
            self.unread_output_ids = HashSet::new();
            self.root = PaneNode::pane(pane_id.clone(), None);
            self.active_pane_id = pane_id;
            self.active_id = None;
            self.zoomed_pane_id = None;
            return;
        }
        let Some(restored) = restore_workspace(layout, options) else {
            return;
        };
        self.root = restored.root;
        self.active_pane_id = restored.active_pane_id;
        self.active_id = restored.active_id;
        self.zoomed_pane_id = None;
        self.unread_output_ids = HashSet::new();
        self.tabs = restored
            .tabs
            .into_iter()
            .map(|tab| {
                let mut restored_tab =
                    TerminalTab::fresh(tab.id, tab.pane_id, tab.title, Some(tab.profile));
                restored_tab.color = tab.color;
                restored_tab.pinned = tab.pinned;
                restored_tab.restored = true;
                restored_tab.connect_on_mount = tab.connect_on_mount;
                restored_tab.status = if tab.connect_on_mount {
                    TabStatus::Connecting
                } else {
                    TabStatus::Closed
                };
                restored_tab
            })
            .collect();
    }

    /// Replace the pane canvas with a freshly connected, arranged session set.
    pub fn launch_set(
        &mut self,
        entries: &[SessionSetEntry],
        layout: SessionSetLayout,
    ) -> Vec<String> {
        if entries.is_empty() {
            return Vec::new();
        }
        let ids: Vec<String> = entries.iter().map(|_| self.ids.next("tab")).collect();
        let pane_ids: Vec<String> = if layout == SessionSetLayout::Tabs {
            vec![self.ids.next("pane")]
        } else {
            entries.iter().map(|_| self.ids.next("pane")).collect()
        };
        let tabs: Vec<TerminalTab> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let pane_id = if layout == SessionSetLayout::Tabs {
                    pane_ids[0].clone()
                } else {
                    pane_ids[index].clone()
                };
                let mut tab = TerminalTab::fresh(
                    ids[index].clone(),
                    pane_id,
                    entry.title.clone(),
                    Some(entry.profile.clone()),
                );
                tab.color = entry.color.clone();
                tab
            })
            .collect();
        let leaves: Vec<PaneNode> = pane_ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let active = if layout == SessionSetLayout::Tabs {
                    ids.last()
                } else {
                    ids.get(index)
                };
                PaneNode::pane(id.clone(), active.map(String::as_str))
            })
            .collect();
        let root = if layout == SessionSetLayout::Tabs {
            leaves
                .into_iter()
                .next()
                .unwrap_or_else(|| PaneNode::pane(self.ids.next("pane"), None))
        } else {
            let direction = if layout == SessionSetLayout::Rows {
                SplitDirection::Vertical
            } else {
                SplitDirection::Horizontal
            };
            build_session_set_tree(
                &mut self.ids,
                leaves,
                direction,
                layout == SessionSetLayout::Grid,
                0,
            )
        };
        let active_pane = first_pane(&root).clone();
        self.tabs = tabs;
        self.root = root;
        self.unread_output_ids = HashSet::new();
        self.active_pane_id = active_pane.id;
        self.active_id = active_pane.active_tab_id;
        self.zoomed_pane_id = None;
        ids
    }

    /// Start fresh connections for selected ended/restored sessions.
    pub fn reconnect(&mut self, tab_ids: &[&str], reattach: Option<ReattachMode>) {
        let requested: HashSet<&str> = tab_ids.iter().copied().collect();
        for tab in &mut self.tabs {
            if tab.profile.is_some()
                && tab.status == TabStatus::Closed
                && requested.contains(tab.id.as_str())
            {
                tab.apply_reconnect(reattach, None);
            }
        }
    }

    /// Reconnect every ended/restored session in the current workspace.
    pub fn reconnect_all(&mut self, reattach: Option<ReattachMode>) {
        for tab in &mut self.tabs {
            if tab.profile.is_some() && tab.status == TabStatus::Closed {
                tab.apply_reconnect(reattach, None);
            }
        }
    }

    /// Replace every remote session in the current workspace, including live ones.
    pub fn force_reconnect_all(&mut self) {
        // One token for the whole gesture: every SSH tab skips established
        // transports, while the window still shares one replacement per host.
        let fresh_transport = self.ids.next("fresh");
        for tab in &mut self.tabs {
            if is_remote_session_tab(tab) {
                let ssh = tab
                    .profile
                    .as_ref()
                    .is_some_and(|profile| profile.kind() == ProfileKind::Ssh);
                tab.apply_reconnect(None, ssh.then(|| fresh_transport.clone()));
            }
        }
    }

    /// Replace one remote session's connection, even while it is live.
    pub fn force_reconnect(&mut self, id: &str) {
        // A token of its own: this tab dials a replacement transport while its
        // siblings keep the connection they are on.
        let fresh_transport = self.ids.next("fresh");
        if let Some(tab) = self.tab_mut(id).filter(|tab| is_remote_session_tab(tab)) {
            let ssh = tab
                .profile
                .as_ref()
                .is_some_and(|profile| profile.kind() == ProfileKind::Ssh);
            tab.apply_reconnect(None, ssh.then_some(fresh_transport));
        }
    }

    /// Record terminal output, notifying only while the tab is hidden. Returns
    /// whether the unread set changed.
    pub fn notify_output(&mut self, id: &str) -> bool {
        let Some(tab) = self.tab(id).filter(|tab| tab.profile.is_some()) else {
            return false;
        };
        let visible = terminal_is_visible(tab, &self.root, self.zoomed_pane_id.as_deref());
        let unread = self.unread_output_ids.contains(id);
        if unread == !visible {
            return false;
        }
        if visible {
            self.unread_output_ids.remove(id);
        } else {
            self.unread_output_ids.insert(id.to_owned());
        }
        true
    }

    pub fn update(&mut self, id: &str, patch: TabPatch) {
        if let Some(tab) = self.tab_mut(id) {
            patch.apply(tab);
        }
    }
}

fn build_session_set_tree(
    ids: &mut IdGen,
    mut panes: Vec<PaneNode>,
    direction: SplitDirection,
    grid: bool,
    depth: usize,
) -> PaneNode {
    if panes.len() == 1 {
        return panes.remove(0);
    }
    let total = panes.len();
    let midpoint = total.div_ceil(2);
    let second = panes.split_off(midpoint);
    let first = panes;
    let split_direction = if grid && depth % 2 == 1 {
        match direction {
            SplitDirection::Horizontal => SplitDirection::Vertical,
            SplitDirection::Vertical => SplitDirection::Horizontal,
        }
    } else {
        direction
    };
    let id = ids.next("split");
    let ratio = first.len() as f64 / total as f64;
    let first = build_session_set_tree(ids, first, direction, grid, depth + 1);
    let second = build_session_set_tree(ids, second, direction, grid, depth + 1);
    PaneNode::split(id, split_direction, ratio, first, second)
}

#[cfg(test)]
mod tests {
    use super::super::workspace_layout::{WorkspaceNode, WorkspaceTabLayout};
    use super::*;

    fn store() -> TabsState {
        TabsState::with_pane("pane-test", IdGen::sequential())
    }

    fn local() -> SessionProfile {
        crate::model::local_profile()
    }

    fn ssh(target: &str) -> SessionProfile {
        crate::model::ssh_profile(target)
    }

    fn set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    fn ids(state: &TabsState) -> Vec<String> {
        state.tabs.iter().map(|tab| tab.id.clone()).collect()
    }

    fn tab_panes(state: &TabsState) -> Vec<(String, String)> {
        state
            .tabs
            .iter()
            .map(|tab| (tab.id.clone(), tab.pane_id.clone()))
            .collect()
    }

    fn split_of(node: &PaneNode) -> &super::super::workspace_layout::SplitNode {
        node.as_split().expect("a split")
    }

    fn active_of(node: &PaneNode) -> Option<&str> {
        node.as_pane()
            .and_then(|pane| pane.active_tab_id.as_deref())
    }

    // terminal output notifications

    #[test]
    fn marks_output_on_a_hidden_tab_and_clears_it_when_the_tab_is_activated() {
        let mut state = store();
        let background = state.open(local(), "Background");
        let active = state.open(local(), "Active");
        state.notify_output(&background);
        state.notify_output(&active);
        assert_eq!(state.unread_output_ids, set(&[&background]));
        state.activate(&background);
        assert!(state.unread_output_ids.is_empty());
    }

    #[test]
    fn does_not_update_state_again_for_subsequent_output_while_a_notification_is_unread() {
        let mut state = store();
        let background = state.open(local(), "Background");
        state.open(local(), "Active");
        assert!(state.notify_output(&background));
        let before = state.clone();
        assert!(!state.notify_output(&background));
        assert_eq!(state, before);
    }

    #[test]
    fn clears_notifications_when_cycling_to_or_closing_a_background_tab() {
        let mut state = store();
        let first = state.open(local(), "First");
        let second = state.open(local(), "Second");
        state.notify_output(&first);
        state.cycle(true);
        assert!(state.unread_output_ids.is_empty());
        state.notify_output(&second);
        state.close(&second);
        assert!(state.unread_output_ids.is_empty());
    }

    #[test]
    fn treats_the_selected_tab_in_every_split_pane_as_visible() {
        let mut state = store();
        let left = state.open(local(), "Left");
        state.split("pane-test", PaneDirection::Right);
        state.open(local(), "Right");
        state.notify_output(&left);
        assert!(state.unread_output_ids.is_empty());
    }

    #[test]
    fn notifies_for_panes_hidden_by_zoom_and_clears_when_they_become_visible_again() {
        let mut state = store();
        let left = state.open(local(), "Left");
        state.split("pane-test", PaneDirection::Right);
        state.open(local(), "Right");
        state.toggle_zoom(None);
        state.notify_output(&left);
        assert_eq!(state.unread_output_ids, set(&[&left]));
        state.toggle_zoom(None);
        assert!(state.unread_output_ids.is_empty());
    }

    #[test]
    fn treats_a_terminal_covered_by_its_remote_editor_as_hidden() {
        let mut state = store();
        let id = state.open(ssh("router"), "Router");
        state.open_editor(&id, "/etc/hosts");
        state.notify_output(&id);
        assert_eq!(state.unread_output_ids, set(&[&id]));
        state.close_editor(&id, "/etc/hosts");
        assert!(state.unread_output_ids.is_empty());
    }

    #[test]
    fn preserves_unread_output_for_the_selected_tab_in_a_zoom_hidden_pane() {
        let mut state = store();
        let closing = state.open(local(), "Closing");
        let unread = state.open(local(), "Unread");
        state.split("pane-test", PaneDirection::Right);
        state.open(local(), "Zoomed");
        state.toggle_zoom(None);
        state.notify_output(&unread);
        state.close(&closing);
        assert_eq!(state.unread_output_ids, set(&[&unread]));
    }

    // blank session tabs

    #[test]
    fn opens_an_idle_chooser_instead_of_starting_a_local_process() {
        let mut state = store();
        let id = state.open_empty();
        assert_eq!(state.active_id.as_deref(), Some(id.as_str()));
        assert_eq!(state.tabs.len(), 1);
        let tab = &state.tabs[0];
        assert_eq!(tab.id, id);
        assert_eq!(tab.title, "New tab");
        assert!(tab.profile.is_none());
        assert_eq!(tab.status, TabStatus::Idle);
        assert!(!tab.connect_on_mount);
    }

    #[test]
    fn replaces_the_chooser_when_a_local_session_is_selected() {
        let mut state = store();
        let id = state.open_empty();
        assert!(state.replace_empty(&id, local(), "Local"));
        let tab = &state.tabs[0];
        assert_eq!(
            (tab.id.as_str(), tab.title.as_str()),
            (id.as_str(), "Local")
        );
        assert_eq!(
            tab.profile.as_ref().map(SessionProfile::kind),
            Some(ProfileKind::Local)
        );
        assert_eq!(tab.status, TabStatus::Connecting);
        assert!(tab.connect_on_mount);
    }

    #[test]
    fn replaces_the_chooser_when_an_ssh_target_is_selected() {
        let mut state = store();
        let id = state.open_empty();
        assert!(state.replace_empty(&id, ssh("edge-router"), "Edge router"));
        let tab = &state.tabs[0];
        assert_eq!(tab.title, "Edge router");
        assert_eq!(tab.profile, Some(ssh("edge-router")));
        assert_eq!(tab.status, TabStatus::Connecting);
        // A session tab is no longer a chooser, so it cannot be replaced again.
        assert!(!state.replace_empty(&id, local(), "Local"));
    }

    // workspace restoration

    fn terminal(id: &str, title: &str, profile: SessionProfile) -> WorkspaceTabLayout {
        WorkspaceTabLayout::Terminal {
            id: id.into(),
            title: title.into(),
            profile: Box::new(profile),
            cwd_hint: None,
            color: None,
            pinned: None,
            offer_reconnect: true,
        }
    }

    fn single_pane(tabs: Vec<WorkspaceTabLayout>, active: Option<&str>) -> WorkspaceLayout {
        WorkspaceLayout {
            root: Some(WorkspaceNode::Pane {
                id: "pane-restored".into(),
                tabs,
                active_tab_id: active.map(str::to_owned),
            }),
            active_pane_id: active.map(|_| "pane-restored".into()),
        }
    }

    #[test]
    fn starts_restored_local_shells_without_reconnecting_remote_sessions() {
        let mut state = store();
        let local_in = SessionProfile::Local(crate::model::LocalProfile {
            cwd: Some("/srv/app".into()),
            ..Default::default()
        });
        state.restore(
            &single_pane(
                vec![
                    terminal("local-tab", "Local", local_in),
                    terminal("ssh-tab", "Router", ssh("router")),
                ],
                Some("local-tab"),
            ),
            RestoreWorkspaceOptions::default(),
        );
        let tabs: Vec<_> = state
            .tabs
            .iter()
            .map(|tab| {
                (
                    tab.id.as_str(),
                    tab.status,
                    tab.connect_on_mount,
                    tab.restored,
                )
            })
            .collect();
        assert_eq!(
            tabs,
            vec![
                ("local-tab", TabStatus::Connecting, true, true),
                ("ssh-tab", TabStatus::Closed, false, true),
            ]
        );
    }

    #[test]
    fn dials_restored_remote_sessions_when_the_restore_opts_in() {
        let mut state = store();
        state.restore(
            &single_pane(
                vec![terminal("ssh-tab", "Router", ssh("router"))],
                Some("ssh-tab"),
            ),
            RestoreWorkspaceOptions {
                connect_remote: true,
            },
        );
        assert_eq!(state.tabs[0].status, TabStatus::Connecting);
        assert!(state.tabs[0].connect_on_mount);
    }

    #[test]
    fn reconnects_only_selected_ended_sessions() {
        let mut state = store();
        state.restore(
            &single_pane(
                vec![
                    terminal("ssh-a", "A", ssh("a")),
                    terminal("ssh-b", "B", ssh("b")),
                ],
                None,
            ),
            RestoreWorkspaceOptions::default(),
        );
        state.reconnect(&["ssh-b"], None);
        let tabs: Vec<_> = state
            .tabs
            .iter()
            .map(|tab| (tab.id.as_str(), tab.status, tab.reconnect_request))
            .collect();
        assert_eq!(
            tabs,
            vec![
                ("ssh-a", TabStatus::Closed, 0),
                ("ssh-b", TabStatus::Connecting, 1)
            ]
        );
    }

    #[test]
    fn reconnects_every_ended_workspace_session_with_optional_ssh_reattachment() {
        let mut state = store();
        state.restore(
            &single_pane(
                vec![
                    terminal("ssh-a", "A", ssh("a")),
                    terminal("telnet-b", "B", crate::model::telnet_profile("b", 23)),
                ],
                None,
            ),
            RestoreWorkspaceOptions::default(),
        );
        state.reconnect_all(Some(ReattachMode::Tmux));
        let tabs: Vec<_> = state
            .tabs
            .iter()
            .map(|tab| {
                (
                    tab.id.as_str(),
                    tab.status,
                    tab.reconnect_request,
                    tab.reconnect_mode,
                )
            })
            .collect();
        assert_eq!(
            tabs,
            vec![
                ("ssh-a", TabStatus::Connecting, 1, Some(ReattachMode::Tmux)),
                ("telnet-b", TabStatus::Connecting, 1, None),
            ]
        );
    }

    #[test]
    fn force_reconnects_every_remote_session_without_replacing_local_shells() {
        let mut state = store();
        let local_id = state.open(local(), "Local");
        let ssh_id = state.open(ssh("router"), "Router");
        let telnet_id = state.open(crate::model::telnet_profile("console", 23), "Console");
        state.update(
            &ssh_id,
            TabPatch {
                status: Some(TabStatus::Connected),
                conn_id: Some(Some("old-transport".into())),
                ..TabPatch::default()
            },
        );
        state.update(
            &telnet_id,
            TabPatch {
                status: Some(TabStatus::Interrupted),
                failure_reason: Some(Some("not responding".into())),
                disconnect_reason: Some(Some(DisconnectReason::Disconnected)),
                ..TabPatch::default()
            },
        );

        state.force_reconnect_all();

        let local_tab = state.tab(&local_id).unwrap();
        assert_eq!(
            (local_tab.status, local_tab.reconnect_request),
            (TabStatus::Connecting, 0)
        );
        let ssh_tab = state.tab(&ssh_id).unwrap();
        assert_eq!(ssh_tab.status, TabStatus::Connecting);
        assert_eq!(ssh_tab.conn_id, None);
        assert_eq!(ssh_tab.reconnect_request, 1);
        assert!(ssh_tab.fresh_transport.is_some());
        assert_eq!(ssh_tab.failure_reason, None);
        let telnet_tab = state.tab(&telnet_id).unwrap();
        assert_eq!(telnet_tab.status, TabStatus::Connecting);
        assert_eq!(telnet_tab.reconnect_request, 1);
        assert_eq!(telnet_tab.fresh_transport, None);
        assert_eq!(telnet_tab.failure_reason, None);
        assert_eq!(telnet_tab.disconnect_reason, None);
    }

    #[test]
    fn force_reconnects_one_session_while_its_siblings_keep_their_transport() {
        let mut state = store();
        let ssh_id = state.open(ssh("router"), "Router");
        let sibling_id = state.open(ssh("router"), "Router 2");
        for id in [&ssh_id, &sibling_id] {
            state.update(
                id,
                TabPatch {
                    status: Some(TabStatus::Connected),
                    conn_id: Some(Some("shared".into())),
                    ..TabPatch::default()
                },
            );
        }

        state.force_reconnect(&ssh_id);

        let tab = state.tab(&ssh_id).unwrap();
        assert_eq!(
            (tab.status, tab.conn_id.as_deref(), tab.reconnect_request),
            (TabStatus::Connecting, None, 1)
        );
        assert!(tab.fresh_transport.is_some());
        let sibling = state.tab(&sibling_id).unwrap();
        assert_eq!(
            (
                sibling.status,
                sibling.conn_id.as_deref(),
                sibling.reconnect_request
            ),
            (TabStatus::Connected, Some("shared"), 0)
        );
        assert_eq!(sibling.fresh_transport, None);
    }

    #[test]
    fn keeps_an_unfulfilled_fresh_transport_request_across_retries_until_success() {
        let mut state = store();
        let ssh_id = state.open(ssh("router"), "Router");
        state.update(
            &ssh_id,
            TabPatch {
                status: Some(TabStatus::Connected),
                ..TabPatch::default()
            },
        );
        state.force_reconnect_all();
        let token = state.tabs[0].fresh_transport.clone();
        assert!(token.is_some());

        // A failed forced dial retries through reconnect() and must stay fresh:
        // never silently multiplex onto the transport it was meant to replace.
        state.update(
            &ssh_id,
            TabPatch {
                status: Some(TabStatus::Closed),
                ..TabPatch::default()
            },
        );
        state.reconnect(&[&ssh_id], None);
        assert_eq!(state.tabs[0].fresh_transport, token);

        // Success clears it, the way the terminal ready handler does.
        state.update(
            &ssh_id,
            TabPatch {
                status: Some(TabStatus::Connected),
                fresh_transport: Some(None),
                ..TabPatch::default()
            },
        );
        assert_eq!(state.tabs[0].fresh_transport, None);
    }

    #[test]
    fn clears_stale_failure_details_when_manually_reconnecting() {
        let mut state = store();
        state.restore(
            &single_pane(vec![terminal("ssh-a", "A", ssh("a"))], None),
            RestoreWorkspaceOptions::default(),
        );
        state.update(
            "ssh-a",
            TabPatch {
                failure_reason: Some(Some("network unreachable".into())),
                disconnect_reason: Some(Some(DisconnectReason::Failed)),
                ..TabPatch::default()
            },
        );
        state.reconnect(&["ssh-a"], Some(ReattachMode::Screen));
        let tab = &state.tabs[0];
        assert_eq!(tab.status, TabStatus::Connecting);
        assert_eq!(tab.reconnect_mode, Some(ReattachMode::Screen));
        assert_eq!(tab.failure_reason, None);
        assert_eq!(tab.disconnect_reason, None);
    }

    #[test]
    fn clears_the_canvas_when_opening_an_empty_workspace() {
        let mut state = store();
        state.open(ssh("router"), "Router");
        state.restore(
            &WorkspaceLayout {
                root: None,
                active_pane_id: None,
            },
            RestoreWorkspaceOptions::default(),
        );
        assert!(state.tabs.is_empty());
        assert!(state.root.is_pane());
        assert_eq!(active_of(&state.root), None);
        assert_eq!(state.active_id, None);
    }

    // session-set layouts

    fn entries() -> Vec<SessionSetEntry> {
        ["a", "b", "c", "d"]
            .iter()
            .map(|target| SessionSetEntry {
                profile: ssh(target),
                title: target.to_uppercase(),
                color: None,
            })
            .collect()
    }

    #[test]
    fn launches_a_group_as_tabs_in_one_pane() {
        let mut state = store();
        state.launch_set(&entries(), SessionSetLayout::Tabs);
        assert!(state.root.is_pane());
        let panes: HashSet<_> = state.tabs.iter().map(|tab| tab.pane_id.clone()).collect();
        assert_eq!(panes.len(), 1);
        assert!(
            state
                .tabs
                .iter()
                .all(|tab| tab.status == TabStatus::Connecting)
        );
    }

    #[test]
    fn launches_a_group_as_an_even_grid() {
        let mut state = store();
        state.launch_set(&entries(), SessionSetLayout::Grid);
        let root = split_of(&state.root);
        assert_eq!(root.direction, SplitDirection::Horizontal);
        assert_eq!(
            split_of(&root.children[0]).direction,
            SplitDirection::Vertical
        );
        assert_eq!(
            split_of(&root.children[1]).direction,
            SplitDirection::Vertical
        );
        let panes: HashSet<_> = state.tabs.iter().map(|tab| tab.pane_id.clone()).collect();
        assert_eq!(panes.len(), 4);
    }

    // remote editor tabs

    #[test]
    fn opens_activates_and_closes_remote_files_without_disturbing_the_terminal_tab() {
        let mut state = store();
        let id = state.open(ssh("edge-router"), "Edge router");
        state.open_editor(&id, "/etc/hosts");
        state.open_editor(&id, "/etc/ssh/sshd_config");
        assert_eq!(
            state.tabs[0].editor_paths,
            vec!["/etc/hosts", "/etc/ssh/sshd_config"]
        );
        assert_eq!(
            state.tabs[0].active_editor_path.as_deref(),
            Some("/etc/ssh/sshd_config")
        );

        state.activate_editor(&id, "/etc/hosts");
        state.close_editor(&id, "/etc/hosts");
        assert_eq!(state.tabs[0].profile, Some(ssh("edge-router")));
        assert_eq!(state.tabs[0].editor_paths, vec!["/etc/ssh/sshd_config"]);
        assert_eq!(
            state.tabs[0].active_editor_path.as_deref(),
            Some("/etc/ssh/sshd_config")
        );

        state.close_editor(&id, "/etc/ssh/sshd_config");
        assert!(state.tabs[0].editor_paths.is_empty());
        assert_eq!(state.tabs[0].active_editor_path, None);
    }

    #[test]
    fn brings_the_terminal_in_front_of_open_files_and_back_without_closing_them() {
        let mut state = store();
        let id = state.open(ssh("edge-router"), "Edge router");
        state.open_editor(&id, "/etc/hosts");
        state.open_editor(&id, "/etc/motd");

        state.show_terminal(&id);
        assert_eq!(state.tabs[0].editor_paths, vec!["/etc/hosts", "/etc/motd"]);
        assert_eq!(
            state.tabs[0].active_editor_path.as_deref(),
            Some("/etc/motd")
        );
        assert_eq!(state.tabs[0].terminal_shown, Some(true));

        // Closing a file from the strip leaves the terminal in front.
        state.close_editor(&id, "/etc/motd");
        assert_eq!(state.tabs[0].editor_paths, vec!["/etc/hosts"]);
        assert_eq!(
            state.tabs[0].active_editor_path.as_deref(),
            Some("/etc/hosts")
        );
        assert_eq!(state.tabs[0].terminal_shown, Some(true));

        state.activate_editor(&id, "/etc/hosts");
        assert_eq!(state.tabs[0].terminal_shown, Some(false));

        state.show_terminal(&id);
        state.open_editor(&id, "/etc/resolv.conf");
        assert_eq!(
            state.tabs[0].active_editor_path.as_deref(),
            Some("/etc/resolv.conf")
        );
        assert_eq!(state.tabs[0].terminal_shown, Some(false));

        state.show_terminal(&id);
        state.close_editor(&id, "/etc/hosts");
        state.close_editor(&id, "/etc/resolv.conf");
        assert!(state.tabs[0].editor_paths.is_empty());
        assert_eq!(state.tabs[0].terminal_shown, Some(false));
    }

    #[test]
    fn ignores_a_terminal_request_when_no_file_is_in_front() {
        let mut state = store();
        let id = state.open(local(), "Local");
        let before = state.clone();
        assert!(!state.show_terminal(&id));
        assert_eq!(state, before);
    }

    #[test]
    fn marks_terminal_output_unread_behind_a_file_and_clears_it_once_the_terminal_is_shown() {
        let mut state = store();
        let id = state.open(local(), "Local");
        state.open_editor(&id, "/tmp/notes.txt");
        state.notify_output(&id);
        assert_eq!(state.unread_output_ids, set(&[&id]));
        state.show_terminal(&id);
        assert!(state.unread_output_ids.is_empty());
        state.notify_output(&id);
        assert!(state.unread_output_ids.is_empty());
    }

    /// From keymap.test.ts: Ctrl+` switches between the terminal and open files.
    #[test]
    fn switches_between_the_terminal_and_open_files() {
        let mut state = store();
        let id = state.open(local(), "Local");
        // Without open files the key stays with the shell.
        assert!(!state.toggle_editor());
        state.open_editor(&id, "/etc/hosts");
        assert!(state.toggle_editor());
        assert_eq!(
            state.tabs[0].active_editor_path.as_deref(),
            Some("/etc/hosts")
        );
        assert_eq!(state.tabs[0].terminal_shown, Some(true));
        assert!(state.toggle_editor());
        assert_eq!(state.tabs[0].terminal_shown, Some(false));
    }

    // pane closing

    #[test]
    fn closes_a_non_empty_pane_and_its_tabs() {
        let mut state = store();
        let surviving = state.open(local(), "Local");
        state.split("pane-test", PaneDirection::Right);
        let closing_pane = state.active_pane_id.clone();
        let closing_tab = state.open(ssh("router"), "Router");

        state.close_pane(&closing_pane);

        assert_eq!(state.root.id(), "pane-test");
        assert!(state.root.is_pane());
        assert_eq!(ids(&state), vec![surviving.clone()]);
        assert!(state.tab(&closing_tab).is_none());
        assert_eq!(state.active_pane_id, "pane-test");
        assert_eq!(state.active_id, Some(surviving));
    }

    // directional splits

    #[test]
    fn places_the_new_pane_on_the_side_that_was_asked_for() {
        let mut state = store();
        state.open(local(), "Local");
        let created = state.split("pane-test", PaneDirection::Left).unwrap();
        let root = split_of(&state.root);
        assert_eq!(root.direction, SplitDirection::Horizontal);
        assert_eq!(root.children[0].id(), created);
        assert_eq!(root.children[1].id(), "pane-test");
        assert_eq!(state.active_pane_id, created);
        assert_eq!(state.active_id, None);
    }

    #[test]
    fn stacks_vertically_when_splitting_up_or_down() {
        let mut state = store();
        let created = state.split("pane-test", PaneDirection::Down).unwrap();
        let root = split_of(&state.root);
        assert_eq!(root.direction, SplitDirection::Vertical);
        assert_eq!(root.children[0].id(), "pane-test");
        assert_eq!(root.children[1].id(), created);
    }

    #[test]
    fn carries_the_current_session_and_its_color_into_the_new_pane() {
        let mut state = store();
        let id = state.open(ssh("router"), "Router");
        state.update(
            &id,
            TabPatch {
                color: Some(Some("#ef5350".into())),
                ..TabPatch::default()
            },
        );

        assert!(state.split_active_pane(PaneDirection::Right, true));

        assert_eq!(split_of(&state.root).direction, SplitDirection::Horizontal);
        assert_eq!(state.tabs.len(), 2);
        let copy = &state.tabs[1];
        assert_eq!(copy.profile, Some(ssh("router")));
        assert_eq!(copy.title, "Router");
        assert_eq!(copy.color.as_deref(), Some("#ef5350"));
        assert_eq!(copy.pane_id, state.active_pane_id);
        assert_eq!(copy.status, TabStatus::Connecting);
        assert_eq!(state.active_id.as_ref(), Some(&copy.id));
    }

    #[test]
    fn leaves_the_session_chooser_for_serial_consoles_and_when_opted_out() {
        let mut state = store();
        state.open(
            SessionProfile::Serial(crate::model::SerialProfile::new("/dev/ttyUSB0")),
            "Console",
        );
        state.split_active_pane(PaneDirection::Right, true);
        assert_eq!(state.tabs.len(), 1);

        state.open(local(), "Local");
        state.split_active_pane(PaneDirection::Down, false);
        assert_eq!(state.tabs.len(), 2);
        assert_eq!(state.active_id, None);
    }

    // keyboard pane focus

    #[test]
    fn moves_focus_to_the_pane_on_that_side_and_reports_when_there_is_none() {
        let mut state = store();
        let local_id = state.open(local(), "Local");
        let right_pane = state.split("pane-test", PaneDirection::Right).unwrap();
        let remote_id = state.open(ssh("router"), "Router");

        assert!(state.focus_pane_direction(PaneDirection::Left));
        assert_eq!(
            (state.active_pane_id.as_str(), state.active_id.as_deref()),
            ("pane-test", Some(local_id.as_str()))
        );
        assert!(state.focus_pane_direction(PaneDirection::Right));
        assert_eq!(
            (state.active_pane_id.as_str(), state.active_id.as_deref()),
            (right_pane.as_str(), Some(remote_id.as_str()))
        );
        // Nothing further right: the key is left to the terminal.
        assert!(!state.focus_pane_direction(PaneDirection::Right));
        assert!(!state.focus_pane_direction(PaneDirection::Up));
    }

    #[test]
    fn cycles_panes_in_reading_order() {
        let mut state = store();
        let right_pane = state.split("pane-test", PaneDirection::Right).unwrap();
        state.focus_pane("pane-test");
        assert!(state.cycle_pane(false));
        assert_eq!(state.active_pane_id, right_pane);
        assert!(state.cycle_pane(false));
        assert_eq!(state.active_pane_id, "pane-test");
    }

    // moving tabs between panes

    #[test]
    fn drops_a_background_tab_at_an_exact_position_in_another_pane() {
        let mut state = store();
        let moving = state.open(local(), "Moving");
        let staying = state.open(local(), "Staying");
        let right_pane = state.split("pane-test", PaneDirection::Right).unwrap();
        let first_right = state.open(local(), "Right one");
        let second_right = state.open(local(), "Right two");

        assert!(state.move_tab_to_pane(&moving, &right_pane, Some(&second_right), Edge::Before));

        let right: Vec<_> = state
            .pane_tabs(&right_pane)
            .iter()
            .map(|tab| tab.id.clone())
            .collect();
        assert_eq!(right, vec![first_right, moving.clone(), second_right]);
        assert_eq!(
            find_pane(&state.root, "pane-test")
                .unwrap()
                .active_tab_id
                .as_deref(),
            Some(staying.as_str())
        );
        assert_eq!(state.active_pane_id, right_pane);
        assert_eq!(state.active_id, Some(moving));
    }

    #[test]
    fn collapses_a_split_after_its_last_tab_is_dropped_into_its_sibling() {
        let mut state = store();
        let left = state.open(local(), "Left");
        let right_pane = state.split("pane-test", PaneDirection::Right).unwrap();
        let moving = state.open(local(), "Right");

        assert!(state.move_tab_to_pane(&moving, "pane-test", Some(&left), Edge::After));

        assert_eq!(state.root.id(), "pane-test");
        assert_eq!(active_of(&state.root), Some(moving.as_str()));
        assert_eq!(
            tab_panes(&state),
            vec![(left, "pane-test".into()), (moving, "pane-test".into())]
        );
        assert_eq!(state.active_pane_id, "pane-test");
        assert_ne!(right_pane, "pane-test");
    }

    #[test]
    fn sends_the_active_tab_to_the_neighbouring_pane() {
        let mut state = store();
        let stay = state.open(local(), "Local");
        state.split("pane-test", PaneDirection::Right);
        let moving = state.open(ssh("router"), "Router");

        assert!(state.move_tab_to_direction(PaneDirection::Left));

        // The emptied pane collapses, leaving both tabs in the surviving pane.
        assert_eq!(state.root.id(), "pane-test");
        assert!(state.root.is_pane());
        assert_eq!(
            tab_panes(&state),
            vec![
                (stay, "pane-test".into()),
                (moving.clone(), "pane-test".into())
            ]
        );
        assert_eq!(state.active_pane_id, "pane-test");
        assert_eq!(state.active_id, Some(moving));
    }

    #[test]
    fn splits_off_a_new_pane_when_there_is_none_in_that_direction() {
        let mut state = store();
        let first = state.open(local(), "Local");
        let second = state.open(ssh("router"), "Router");

        assert!(state.move_tab_to_direction(PaneDirection::Down));

        let root = split_of(&state.root);
        assert_eq!(root.direction, SplitDirection::Vertical);
        assert_eq!(root.children[0].id(), "pane-test");
        assert_eq!(active_of(&root.children[0]), Some(first.as_str()));
        assert_eq!(active_of(&root.children[1]), Some(second.as_str()));
        assert_eq!(state.active_id.as_ref(), Some(&second));
        assert_eq!(state.tab(&second).unwrap().pane_id, state.active_pane_id);
    }

    #[test]
    fn splits_a_specific_background_tab_into_a_new_right_pane() {
        let mut state = store();
        let moving = state.open(local(), "Move me");
        let staying = state.open(ssh("router"), "Stay here");

        assert!(state.move_tab_to_new_pane(&moving, PaneDirection::Right));

        let root = split_of(&state.root);
        assert_eq!(root.direction, SplitDirection::Horizontal);
        assert_eq!(root.children[0].id(), "pane-test");
        assert_eq!(active_of(&root.children[0]), Some(staying.as_str()));
        assert_eq!(active_of(&root.children[1]), Some(moving.as_str()));
        assert_eq!(state.tab(&moving).unwrap().pane_id, state.active_pane_id);
        assert_eq!(state.active_id, Some(moving));
    }

    #[test]
    fn splits_a_specific_tab_into_a_new_lower_pane() {
        let mut state = store();
        let staying = state.open(local(), "Stay here");
        let moving = state.open(ssh("router"), "Move me");

        assert!(state.move_tab_to_new_pane(&moving, PaneDirection::Down));

        let root = split_of(&state.root);
        assert_eq!(root.direction, SplitDirection::Vertical);
        assert_eq!(root.children[0].id(), "pane-test");
        assert_eq!(active_of(&root.children[0]), Some(staying.as_str()));
        assert_eq!(active_of(&root.children[1]), Some(moving.as_str()));
    }

    #[test]
    fn does_not_move_the_only_tab_into_a_new_pane() {
        let mut state = store();
        let id = state.open(local(), "Only tab");
        assert!(!state.move_tab_to_new_pane(&id, PaneDirection::Right));
        assert_eq!(state.root.id(), "pane-test");
        assert!(state.root.is_pane());
    }

    #[test]
    fn declines_to_move_the_only_tab_of_a_pane_into_a_new_pane() {
        let mut state = store();
        state.open(local(), "Local");
        assert!(!state.move_tab_to_direction(PaneDirection::Right));
        assert!(state.root.is_pane());
    }

    #[test]
    fn reorders_tabs_inside_the_strip() {
        let mut state = store();
        let first = state.open(local(), "One");
        let second = state.open(local(), "Two");
        assert!(state.move_tab_within_pane(-1));
        assert_eq!(ids(&state), vec![second, first]);
        assert!(!state.move_tab_within_pane(-1));
    }

    #[test]
    fn reorders_a_dragged_tab_relative_to_its_drop_target() {
        let mut state = store();
        let first = state.open(local(), "One");
        let second = state.open(local(), "Two");
        let third = state.open(local(), "Three");
        assert!(state.reorder_tab(&first, &third, Edge::After));
        assert_eq!(ids(&state), vec![second, third.clone(), first.clone()]);
        assert!(!state.reorder_tab(&first, &third, Edge::After));
    }

    #[test]
    fn groups_pinned_tabs_first_and_reorders_only_within_the_same_pin_group() {
        let mut state = store();
        let first = state.open(local(), "One");
        let second = state.open(local(), "Two");
        let third = state.open(local(), "Three");

        assert!(state.set_pinned(&second, true));
        assert!(state.set_pinned(&third, true));
        assert_eq!(
            ids(&state),
            vec![second.clone(), third.clone(), first.clone()]
        );
        assert!(!state.reorder_tab(&first, &second, Edge::Before));
        assert!(state.reorder_tab(&third, &second, Edge::Before));
        assert!(state.set_pinned(&third, false));
        let pins: Vec<_> = state
            .tabs
            .iter()
            .map(|tab| (tab.id.clone(), tab.pinned))
            .collect();
        assert_eq!(pins, vec![(second, true), (third, false), (first, false)]);
    }

    #[test]
    fn selects_only_unpinned_tabs_to_the_right_for_browser_style_bulk_close() {
        let mut state = store();
        let pinned_source = state.open(local(), "Pinned source");
        let pinned_neighbor = state.open(local(), "Pinned neighbor");
        let first = state.open(local(), "One");
        let second = state.open(local(), "Two");
        state.set_pinned(&pinned_source, true);
        state.set_pinned(&pinned_neighbor, true);
        let tabs = &state.tabs;

        assert_eq!(
            closable_tab_ids_to_right(tabs, &pinned_source),
            vec![first.clone(), second.clone()]
        );
        assert_eq!(
            closable_tab_ids_to_right(tabs, &first),
            vec![second.clone()]
        );
        assert!(closable_tab_ids_to_right(tabs, &second).is_empty());
        assert!(closable_tab_ids_to_right(tabs, "missing").is_empty());
    }

    #[test]
    fn keeps_a_pinned_tab_ahead_of_unpinned_tabs_when_moving_it_to_another_pane() {
        let mut state = store();
        state.open(local(), "Stay");
        let pinned = state.open(local(), "Pinned");
        state.set_pinned(&pinned, true);
        let right_pane = state.split("pane-test", PaneDirection::Right).unwrap();
        let first_right = state.open(ssh("one"), "One");
        let second_right = state.open(ssh("two"), "Two");
        state.activate(&pinned);

        assert!(state.move_tab_to_direction(PaneDirection::Right));
        let right: Vec<_> = state
            .pane_tabs(&right_pane)
            .iter()
            .map(|tab| (tab.id.clone(), tab.pinned))
            .collect();
        assert_eq!(
            right,
            vec![(pinned, true), (first_right, false), (second_right, false)]
        );
    }

    #[test]
    fn activates_tabs_by_their_window_wide_pane_and_strip_order() {
        let mut state = store();
        let first = state.open(local(), "Left one");
        state.open(local(), "Left two");
        let right_pane = state.split("pane-test", PaneDirection::Right).unwrap();
        let third = state.open(local(), "Right one");
        state.open(local(), "Right two");

        assert!(state.activate_tab_index(0));
        assert_eq!(
            (state.active_pane_id.as_str(), state.active_id.as_deref()),
            ("pane-test", Some(first.as_str()))
        );
        assert!(state.activate_tab_index(2));
        assert_eq!(
            (state.active_pane_id.as_str(), state.active_id.as_deref()),
            (right_pane.as_str(), Some(third.as_str()))
        );
        assert!(!state.activate_tab_index(4));
    }

    // pane zoom

    #[test]
    fn fills_the_canvas_with_one_pane_and_restores_it_on_the_next_press() {
        let mut state = store();
        state.open(local(), "Local");
        let right_pane = state.split("pane-test", PaneDirection::Right).unwrap();
        assert!(state.toggle_zoom(None));
        assert_eq!(state.zoomed_pane_id, Some(right_pane));
        assert!(state.toggle_zoom(None));
        assert_eq!(state.zoomed_pane_id, None);
    }

    #[test]
    fn does_not_zoom_a_canvas_that_has_a_single_pane() {
        let mut state = store();
        assert!(!state.toggle_zoom(None));
        assert_eq!(state.zoomed_pane_id, None);
    }

    #[test]
    fn drops_the_zoom_as_soon_as_another_pane_takes_focus() {
        let mut state = store();
        state.split("pane-test", PaneDirection::Right);
        state.toggle_zoom(None);
        state.focus_pane("pane-test");
        assert_eq!(state.zoomed_pane_id, None);
    }

    #[test]
    fn drops_the_zoom_when_the_zoomed_pane_closes() {
        let mut state = store();
        state.open(local(), "Local");
        let right_pane = state.split("pane-test", PaneDirection::Right).unwrap();
        state.toggle_zoom(None);
        state.close_pane(&right_pane);
        assert_eq!(state.zoomed_pane_id, None);
        assert_eq!(state.active_pane_id, "pane-test");
    }

    // pane and tab symbiosis

    #[test]
    fn closes_the_pane_along_with_its_last_tab() {
        let mut state = store();
        let kept = state.open(local(), "Local");
        state.split("pane-test", PaneDirection::Right);
        let closing = state.open(ssh("router"), "Router");
        state.close(&closing);
        assert_eq!(state.root.id(), "pane-test");
        assert!(state.root.is_pane());
        assert_eq!(state.active_pane_id, "pane-test");
        assert_eq!(state.active_id, Some(kept));
    }

    #[test]
    fn keeps_the_canvas_when_the_last_tab_of_the_only_pane_closes() {
        let mut state = store();
        let id = state.open(local(), "Local");
        state.close(&id);
        assert_eq!(state.root, PaneNode::pane("pane-test", None));
        assert!(state.tabs.is_empty());
        assert_eq!(state.active_id, None);
    }

    #[test]
    fn resizes_the_focused_pane_inside_its_own_split_and_evens_panes_out_again() {
        let mut state = store();
        state.split("pane-test", PaneDirection::Right);
        state.focus_pane("pane-test");

        assert!(state.resize_active_pane(0.1));
        assert_eq!(split_of(&state.root).ratio, 0.6);

        // The second child grows in the other direction.
        assert!(state.cycle_pane(false));
        assert!(state.resize_active_pane(0.1));
        assert_eq!(split_of(&state.root).ratio, 0.5);

        state.resize_active_pane(0.2);
        assert!(state.equalize_panes());
        assert_eq!(split_of(&state.root).ratio, 0.5);
    }

    #[test]
    fn closing_a_middle_tab_shows_its_right_neighbour() {
        let mut state = store();
        let first = state.open(local(), "One");
        let second = state.open(local(), "Two");
        let third = state.open(local(), "Three");
        state.activate(&second);
        state.close(&second);
        assert_eq!(state.active_id, Some(third.clone()));
        state.close(&third);
        assert_eq!(state.active_id, Some(first));
    }

    #[test]
    fn timestamped_ids_are_unique_and_prefixed() {
        let mut ids = IdGen::timestamped();
        let first = ids.next("tab");
        let second = ids.next("tab");
        assert!(first.starts_with("tab-") && first.ends_with("-1"));
        assert_ne!(first, second);
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
    }
}
