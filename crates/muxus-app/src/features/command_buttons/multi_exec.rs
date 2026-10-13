//! Mirrored terminal input (port of `client/src/state/multi-exec.ts`, the
//! `toggleMultiExec` action of `session-actions.ts` and the mirroring half of
//! `state/paste-progress.ts`).
//!
//! Two or more selected terminals form a mirrored-input group: what is typed
//! or pasted into one of them goes to the others too. [`MultiExecState`] is
//! the store; the terminal panes route their input through
//! [`MultiExecState::broadcast_input`] and [`MultiExecState::broadcast_paste`]
//! with the window's terminals behind the [`MirrorTargets`] trait.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use muxus_core::api::WorkspaceMultiExecGroup;
use muxus_core::js::js_trim;

use crate::state::tabs::{TabsState, is_multi_exec_target};
use crate::state::workspace_layout::{PaneNode, panes_in_order, visible_tab_ids};

/// The window's terminals as mirrored input reaches them (`TerminalHandle`).
pub trait MirrorTargets {
    /// Write input straight to the session (not through the terminal's own
    /// input handler, so it is not mirrored again). False when the tab has
    /// no live terminal.
    fn send_input(&self, tab_id: &str, data: &[u8]) -> bool;
    /// Paste text, paced by that terminal's own host settings.
    fn send_paste(&self, tab_id: &str, text: &str) -> bool;
    /// Stop a paced paste.
    fn cancel_paste(&self, tab_id: &str);
}

fn unique(tab_ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    tab_ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect()
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

/// `multi-${Date.now().toString(36)}-${random}`.
fn new_group_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    // Six random base-36 digits, taken from a fresh preference id's UUID.
    let id = muxus_store::prefs::command_buttons::new_preference_id("x");
    let hex: String = id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(12)
        .collect();
    let random: String = to_base36(u128::from(u64::from_str_radix(&hex, 16).unwrap_or(0)))
        .chars()
        .take(6)
        .collect();
    format!("multi-{}-{random}", to_base36(millis))
}

/// The multi-execution store (`useMultiExecStore`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MultiExecState {
    /// Two or more selected tabs automatically form a mirrored-input group.
    pub selected_ids: Vec<String>,
    /// The last set that actually mirrored, so the toggle can resume it.
    pub last_mirrored_ids: Vec<String>,
    /// Named target sets persisted as part of the active workspace.
    pub groups: Vec<WorkspaceMultiExecGroup>,
}

impl MultiExecState {
    /// Whether input is being mirrored right now.
    pub fn active(&self) -> bool {
        self.selected_ids.len() >= 2
    }

    /// A selection change that also remembers any set large enough to
    /// mirror, so switching multi-execution off and on again lands on the
    /// same terminals.
    fn select(&mut self, selected_ids: Vec<String>) {
        if selected_ids.len() >= 2 {
            self.last_mirrored_ids = selected_ids.clone();
        }
        self.selected_ids = selected_ids;
    }

    pub fn set_selection(&mut self, tab_ids: impl IntoIterator<Item = String>) {
        self.select(unique(tab_ids));
    }

    pub fn toggle_target(&mut self, tab_id: &str) {
        let next = if self.selected_ids.iter().any(|id| id == tab_id) {
            self.selected_ids
                .iter()
                .filter(|id| *id != tab_id)
                .cloned()
                .collect()
        } else {
            let mut next = self.selected_ids.clone();
            next.push(tab_id.to_owned());
            next
        };
        self.select(next);
    }

    /// Switch mirroring off (always) or back on: the last mirrored set when
    /// at least two of it are still available, else the fallback (the
    /// sessions on screen). False when there was nothing to switch on.
    pub fn toggle_mirroring(
        &mut self,
        available_tab_ids: &[String],
        fallback_tab_ids: &[String],
    ) -> bool {
        // Off is unconditional: whatever is mirroring right now stops, and the
        // set is kept for the next press.
        if self.selected_ids.len() >= 2 {
            self.last_mirrored_ids = std::mem::take(&mut self.selected_ids);
            return true;
        }
        let available: HashSet<&str> = available_tab_ids.iter().map(String::as_str).collect();
        let resumed: Vec<String> = self
            .last_mirrored_ids
            .iter()
            .filter(|id| available.contains(id.as_str()))
            .cloned()
            .collect();
        let selected = if resumed.len() >= 2 {
            resumed
        } else {
            unique(fallback_tab_ids.iter().cloned())
                .into_iter()
                .filter(|id| available.contains(id.as_str()))
                .collect()
        };
        if selected.len() < 2 {
            return false;
        }
        self.select(selected);
        true
    }

    /// Drop targets that are no longer connected. True when the selection changed.
    pub fn reconcile(&mut self, available_tab_ids: &[String]) -> bool {
        let available: HashSet<&str> = available_tab_ids.iter().map(String::as_str).collect();
        let selected: Vec<String> = self
            .selected_ids
            .iter()
            .filter(|id| available.contains(id.as_str()))
            .cloned()
            .collect();
        if selected == self.selected_ids {
            return false;
        }
        self.selected_ids = selected;
        true
    }

    /// Replace the groups (a workspace was opened): the selection ends, and
    /// tab ids from the outgoing workspace can never be resumed.
    pub fn set_groups(&mut self, groups: &[WorkspaceMultiExecGroup]) {
        self.selected_ids.clear();
        self.last_mirrored_ids.clear();
        self.groups = groups
            .iter()
            .map(|group| WorkspaceMultiExecGroup {
                id: group.id.clone(),
                name: group.name.clone(),
                tab_ids: unique(group.tab_ids.iter().cloned()),
            })
            .collect();
    }

    /// Save the selection (or `tab_ids`) under a name; a group of the same
    /// name (any case) is replaced. Returns the group's id, or `None` when
    /// the name is blank or fewer than two terminals were given.
    pub fn save_group(&mut self, name: &str, tab_ids: Option<&[String]>) -> Option<String> {
        let normalized = js_trim(name);
        let targets = unique(tab_ids.unwrap_or(&self.selected_ids).iter().cloned());
        if normalized.is_empty() || targets.len() < 2 {
            return None;
        }
        let existing = self
            .groups
            .iter()
            .position(|group| group.name.to_lowercase() == normalized.to_lowercase());
        let id = existing
            .map(|index| self.groups[index].id.clone())
            .unwrap_or_else(new_group_id);
        let next = WorkspaceMultiExecGroup {
            id: id.clone(),
            name: normalized.to_owned(),
            tab_ids: targets,
        };
        match existing {
            Some(index) => self.groups[index] = next,
            None => self.groups.push(next),
        }
        Some(id)
    }

    pub fn delete_group(&mut self, id: &str) {
        self.groups.retain(|group| group.id != id);
    }

    /// Select a saved group's terminals that are still available.
    pub fn activate_group(&mut self, id: &str, available_tab_ids: &[String]) {
        let Some(group) = self.groups.iter().find(|group| group.id == id) else {
            return;
        };
        let available: HashSet<&str> = available_tab_ids.iter().map(String::as_str).collect();
        let selected = group
            .tab_ids
            .iter()
            .filter(|id| available.contains(id.as_str()))
            .cloned()
            .collect();
        self.select(selected);
    }

    /// The other terminals that input into `source_tab_id` reaches while it
    /// is being mirrored.
    pub fn mirror_tab_ids(&self, source_tab_id: &str) -> Vec<String> {
        if !self.active() || !self.selected_ids.iter().any(|id| id == source_tab_id) {
            return Vec::new();
        }
        self.selected_ids
            .iter()
            .filter(|id| *id != source_tab_id)
            .cloned()
            .collect()
    }

    /// The terminals mirroring input with this one, itself included; empty
    /// when it mirrors nothing.
    pub fn mirrored_tab_ids(&self, tab_id: &str) -> Vec<String> {
        if self.active() && self.selected_ids.iter().any(|id| id == tab_id) {
            self.selected_ids.clone()
        } else {
            Vec::new()
        }
    }

    /// Mirror user input from one selected terminal to every other selected
    /// terminal. Returns how many received it.
    pub fn broadcast_input(
        &self,
        source_tab_id: &str,
        data: &[u8],
        targets: &dyn MirrorTargets,
    ) -> usize {
        self.mirror_tab_ids(source_tab_id)
            .iter()
            .filter(|tab_id| targets.send_input(tab_id, data))
            .count()
    }

    /// Paste into every other mirrored terminal. Each one paces the paste by
    /// its own host's settings, in its own backend session, independently of
    /// the rest.
    pub fn broadcast_paste(
        &self,
        source_tab_id: &str,
        text: &str,
        targets: &dyn MirrorTargets,
    ) -> usize {
        self.mirrored_tab_ids(source_tab_id)
            .iter()
            .filter(|tab_id| *tab_id != source_tab_id && targets.send_paste(tab_id, text))
            .count()
    }

    /// Stop a paced paste (`cancelPaste`). Input is mirrored under
    /// multi-execution, so stopping it in one mirrored terminal stops it in
    /// all of them.
    pub fn cancel_paste(&self, tab_id: &str, targets: &dyn MirrorTargets) {
        let mirrored = self.mirrored_tab_ids(tab_id);
        if mirrored.is_empty() {
            targets.cancel_paste(tab_id);
        } else {
            for id in mirrored {
                targets.cancel_paste(&id);
            }
        }
    }

    /// The workspace's groups for a save: only tabs that still exist, and
    /// only groups that still name two of them (`currentSnapshot`).
    pub fn persistable_groups(&self, tab_ids: &HashSet<&str>) -> Vec<WorkspaceMultiExecGroup> {
        self.groups
            .iter()
            .filter_map(|group| {
                let ids: Vec<String> = unique(group.tab_ids.iter().cloned())
                    .into_iter()
                    .filter(|id| tab_ids.contains(id.as_str()))
                    .collect();
                (ids.len() >= 2).then(|| WorkspaceMultiExecGroup {
                    id: group.id.clone(),
                    name: group.name.clone(),
                    tab_ids: ids,
                })
            })
            .collect()
    }
}

/// Visible panes whose active terminals currently take part in mirrored
/// input (they get the warning-colored border).
pub fn multi_exec_pane_ids(root: &PaneNode, selected_ids: &[String]) -> HashSet<String> {
    if selected_ids.len() < 2 {
        return HashSet::new();
    }
    let selected: HashSet<&str> = selected_ids.iter().map(String::as_str).collect();
    panes_in_order(root)
        .into_iter()
        .filter(|pane| {
            pane.active_tab_id
                .as_deref()
                .is_some_and(|id| selected.contains(id))
        })
        .map(|pane| pane.id.clone())
        .collect()
}

/// The connected terminal tabs input can be mirrored into.
pub fn multi_exec_targets(tabs: &TabsState) -> Vec<String> {
    tabs.tabs
        .iter()
        .filter(|tab| is_multi_exec_target(tab))
        .map(|tab| tab.id.clone())
        .collect()
}

/// `toggleMultiExec`: the keyboard toggle. Returns the info toast to show
/// when there was nothing to switch on.
pub fn toggle_multi_exec(state: &mut MultiExecState, tabs: &TabsState) -> Option<&'static str> {
    let connected = multi_exec_targets(tabs);
    let on_screen = visible_tab_ids(&tabs.root, tabs.zoomed_pane_id.as_deref());
    if state.toggle_mirroring(&connected, &on_screen) {
        return None;
    }
    Some(if connected.len() < 2 {
        "Connect at least two sessions to mirror input."
    } else {
        "Pick the terminals to mirror input into from the multi-exec control."
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use muxus_core::protocol::{LocalProfile, SessionProfile, VncProfile};

    use super::*;
    use crate::state::tabs::{IdGen, TabPatch, TabStatus};
    use crate::state::workspace_layout::PaneDirection;

    #[derive(Default)]
    struct Terminals {
        inputs: RefCell<HashMap<String, Vec<Vec<u8>>>>,
        pastes: RefCell<HashMap<String, Vec<String>>>,
        cancels: RefCell<HashMap<String, usize>>,
        /// Terminals whose paste is refused.
        refuse_paste: Vec<&'static str>,
        /// Only these have a live terminal.
        registered: Vec<&'static str>,
    }

    impl MirrorTargets for Terminals {
        fn send_input(&self, tab_id: &str, data: &[u8]) -> bool {
            if !self.registered.contains(&tab_id) {
                return false;
            }
            self.inputs
                .borrow_mut()
                .entry(tab_id.into())
                .or_default()
                .push(data.to_vec());
            true
        }

        fn send_paste(&self, tab_id: &str, text: &str) -> bool {
            if !self.registered.contains(&tab_id) {
                return false;
            }
            self.pastes
                .borrow_mut()
                .entry(tab_id.into())
                .or_default()
                .push(text.into());
            !self.refuse_paste.contains(&tab_id)
        }

        fn cancel_paste(&self, tab_id: &str) {
            *self.cancels.borrow_mut().entry(tab_id.into()).or_default() += 1;
        }
    }

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn selected(state: &MultiExecState) -> Vec<&str> {
        state.selected_ids.iter().map(String::as_str).collect()
    }

    #[test]
    fn identifies_panes_whose_visible_tabs_participate_in_active_mirrored_input() {
        let mut tabs = TabsState::with_pane("pane-left", IdGen::sequential());
        let a = tabs.open(SessionProfile::Local(LocalProfile::default()), "a");
        let right = tabs.split("pane-left", PaneDirection::Right).unwrap();
        let b = tabs.open(SessionProfile::Local(LocalProfile::default()), "b");
        let panes = |ids: &[&str]| {
            let mut found: Vec<String> = multi_exec_pane_ids(&tabs.root, &self::ids(ids))
                .into_iter()
                .collect();
            found.sort();
            found
        };
        assert!(panes(&[a.as_str()]).is_empty());
        assert_eq!(panes(&[a.as_str(), "tab-c"]), ["pane-left"]);
        let mut both = vec!["pane-left".to_owned(), right.clone()];
        both.sort();
        assert_eq!(panes(&[a.as_str(), b.as_str()]), both);
    }

    #[test]
    fn mirrors_source_input_only_to_the_other_selected_terminals() {
        let terminals = Terminals {
            registered: vec!["tab-a", "tab-b", "tab-c"],
            ..Default::default()
        };
        let mut state = MultiExecState::default();
        state.set_selection(ids(&["tab-a", "tab-b"]));
        assert_eq!(state.broadcast_input("tab-a", b"ls", &terminals), 1);
        let inputs = terminals.inputs.borrow();
        assert!(!inputs.contains_key("tab-a"));
        assert_eq!(inputs["tab-b"], [b"ls".to_vec()]);
        assert!(!inputs.contains_key("tab-c"));
    }

    #[test]
    fn mirrors_complete_commands_typed_into_a_selected_terminal() {
        let terminals = Terminals {
            registered: vec!["tab-a", "tab-b"],
            ..Default::default()
        };
        let mut state = MultiExecState::default();
        state.set_selection(ids(&["tab-a", "tab-b"]));
        assert_eq!(state.broadcast_input("tab-a", b"uptime\r", &terminals), 1);
        assert_eq!(terminals.inputs.borrow()["tab-b"], [b"uptime\r".to_vec()]);
    }

    #[test]
    fn hands_a_paste_to_each_other_mirrored_terminal_to_pace_on_its_own() {
        let terminals = Terminals {
            registered: vec!["tab-a", "tab-b", "tab-c", "tab-d"],
            refuse_paste: vec!["tab-c"],
            ..Default::default()
        };
        let mut state = MultiExecState::default();
        state.set_selection(ids(&["tab-a", "tab-b", "tab-c"]));
        // Raw text, not bytes: each terminal applies its own pacing and
        // bracketed paste mode.
        assert_eq!(
            state.broadcast_paste("tab-a", "conf t\nexit\n", &terminals),
            1
        );
        {
            let pastes = terminals.pastes.borrow();
            assert!(!pastes.contains_key("tab-a"));
            assert_eq!(pastes["tab-b"], ["conf t\nexit\n"]);
            assert_eq!(pastes["tab-c"], ["conf t\nexit\n"]);
            assert!(!pastes.contains_key("tab-d"));
        }
        state.set_selection(ids(&["tab-a"]));
        assert_eq!(state.broadcast_paste("tab-a", "uptime\n", &terminals), 0);
    }

    #[test]
    fn cancels_a_paced_paste_in_every_mirrored_terminal_or_only_in_its_own() {
        let terminals = Terminals::default();
        let mut state = MultiExecState::default();
        let counts = |terminals: &Terminals| {
            let cancels = terminals.cancels.borrow();
            ["tab-a", "tab-b", "tab-c"].map(|id| cancels.get(id).copied().unwrap_or(0))
        };
        state.cancel_paste("tab-c", &terminals);
        assert_eq!(counts(&terminals), [0, 0, 1]);
        state.set_selection(ids(&["tab-a", "tab-b"]));
        state.cancel_paste("tab-b", &terminals);
        assert_eq!(counts(&terminals), [1, 1, 1]);
    }

    #[test]
    fn drops_disconnected_targets_from_the_selection() {
        let mut state = MultiExecState::default();
        state.set_selection(ids(&["tab-a", "tab-b"]));
        assert!(state.reconcile(&ids(&["tab-a"])));
        assert_eq!(selected(&state), ["tab-a"]);
        assert!(!state.reconcile(&ids(&["tab-a"])));
    }

    #[test]
    fn switches_mirroring_off_and_back_on_over_the_same_terminals() {
        let mut state = MultiExecState::default();
        state.set_selection(ids(&["tab-a", "tab-b"]));
        assert!(state.toggle_mirroring(&ids(&["tab-a", "tab-b"]), &[]));
        assert!(state.selected_ids.is_empty());
        assert!(state.toggle_mirroring(&ids(&["tab-a", "tab-b"]), &[]));
        assert_eq!(selected(&state), ["tab-a", "tab-b"]);
    }

    #[test]
    fn resumes_only_the_terminals_still_live_else_the_ones_on_screen() {
        let mut state = MultiExecState::default();
        state.set_selection(ids(&["tab-a", "tab-b"]));
        state.toggle_mirroring(&ids(&["tab-a", "tab-b"]), &[]);
        // "tab-b" went away while mirroring was off, leaving too little to resume.
        state.toggle_mirroring(
            &ids(&["tab-a", "tab-c", "tab-d"]),
            &ids(&["tab-c", "tab-d", "tab-gone"]),
        );
        assert_eq!(selected(&state), ["tab-c", "tab-d"]);
    }

    #[test]
    fn declines_to_mirror_when_fewer_than_two_terminals_are_available() {
        let mut state = MultiExecState::default();
        state.set_selection(ids(&["tab-a", "tab-b"]));
        state.toggle_mirroring(&ids(&["tab-a", "tab-b"]), &[]);
        assert!(!state.toggle_mirroring(&ids(&["tab-a"]), &ids(&["tab-a"])));
        assert!(state.selected_ids.is_empty());
    }

    /// A connected session in the focused pane.
    fn connect_tab(tabs: &mut TabsState, title: &str) -> String {
        let id = tabs.open(SessionProfile::Local(LocalProfile::default()), title);
        tabs.update(
            &id,
            TabPatch {
                status: Some(TabStatus::Connected),
                ..Default::default()
            },
        );
        id
    }

    #[test]
    fn mirrors_the_sessions_on_screen_the_first_time_the_shortcut_is_pressed() {
        let mut tabs = TabsState::with_pane("pane-test", IdGen::sequential());
        let mut state = MultiExecState::default();
        let first = connect_tab(&mut tabs, "edge-1");
        let pane = tabs.active_pane_id.clone();
        tabs.split(&pane, PaneDirection::Right);
        let second = connect_tab(&mut tabs, "edge-2");
        assert_eq!(toggle_multi_exec(&mut state, &tabs), None);
        assert_eq!(selected(&state), [first.as_str(), second.as_str()]);
        assert_eq!(toggle_multi_exec(&mut state, &tabs), None);
        assert!(state.selected_ids.is_empty());
    }

    #[test]
    fn leaves_the_panes_hidden_behind_a_zoomed_one_out_of_the_shortcut() {
        let mut tabs = TabsState::with_pane("pane-test", IdGen::sequential());
        let mut state = MultiExecState::default();
        connect_tab(&mut tabs, "edge-1");
        let pane = tabs.active_pane_id.clone();
        tabs.split(&pane, PaneDirection::Right);
        connect_tab(&mut tabs, "edge-2");
        // A zoomed pane covers the canvas, so the other session is off screen
        // and must not receive a command typed into the one that is visible.
        assert!(tabs.toggle_zoom(None));
        assert_eq!(
            toggle_multi_exec(&mut state, &tabs),
            Some("Pick the terminals to mirror input into from the multi-exec control.")
        );
        assert!(state.selected_ids.is_empty());
    }

    #[test]
    fn leaves_remote_desktops_out_of_mirrored_input() {
        let mut tabs = TabsState::with_pane("pane-test", IdGen::sequential());
        let mut state = MultiExecState::default();
        connect_tab(&mut tabs, "edge-1");
        let pane = tabs.active_pane_id.clone();
        tabs.split(&pane, PaneDirection::Right);
        let desktop = tabs.open(
            SessionProfile::Vnc(VncProfile {
                profile_id: None,
                host: "design-vm".into(),
                port: 5900,
                username: None,
                ssh_gateway: None,
                resize_remote: Some(false),
                view_only: Some(false),
                share_clipboard: Some(true),
            }),
            "design-vm",
        );
        tabs.update(
            &desktop,
            TabPatch {
                status: Some(TabStatus::Connected),
                ..Default::default()
            },
        );
        let toast = toggle_multi_exec(&mut state, &tabs).unwrap();
        assert!(state.selected_ids.is_empty());
        assert!(toast.contains("two sessions"));
    }

    #[test]
    fn says_why_the_shortcut_did_nothing_when_there_is_nothing_to_mirror() {
        let mut tabs = TabsState::with_pane("pane-test", IdGen::sequential());
        let mut state = MultiExecState::default();
        connect_tab(&mut tabs, "edge-1");
        let toast = toggle_multi_exec(&mut state, &tabs).unwrap();
        assert!(state.selected_ids.is_empty());
        assert!(toast.contains("two sessions"));
    }

    #[test]
    fn saves_and_activates_named_workspace_groups() {
        let mut state = MultiExecState::default();
        state.set_selection(ids(&["tab-a", "tab-b"]));
        let id = state.save_group("Routers", None).unwrap();
        assert_eq!(
            state.groups,
            [WorkspaceMultiExecGroup {
                id: id.clone(),
                name: "Routers".into(),
                tab_ids: ids(&["tab-a", "tab-b"]),
            }]
        );
        state.set_selection(Vec::new());
        state.activate_group(&id, &ids(&["tab-b", "tab-c"]));
        assert_eq!(selected(&state), ["tab-b"]);
        // The same name (any case) replaces the group and keeps its id.
        state.set_selection(ids(&["tab-c", "tab-d"]));
        assert_eq!(state.save_group("  routers ", None), Some(id.clone()));
        assert_eq!(state.groups.len(), 1);
        assert_eq!(state.groups[0].name, "routers");
        assert!(id.starts_with("multi-"));
        assert_eq!(state.save_group("   ", None), None);
    }
}
