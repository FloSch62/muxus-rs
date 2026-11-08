//! The host tree's keyboard: `useTreeKeyboard.ts` and the keys `HostTree.tsx`,
//! `HostRow.tsx` and `FolderRow.tsx` claim (Ctrl/Cmd+A, Shift+Arrow ranges,
//! Shift+Space, Alt+Arrow reordering, Enter and Space on a row). Every
//! decision comes from `muxus_app::sidebar::tree_navigation`; this owns the
//! focus effects and the type-ahead buffer.

use std::time::{Duration, Instant};

use muxus_app::hosts::RowNode;
use muxus_app::sidebar::row_clicks::HostActivation;
use muxus_app::sidebar::tree_navigation::{
    TreeNavAction, TreeNavKey, tree_nav_action, type_ahead_index,
};
use muxus_app::sidebar::tree_selection::{TreeSelectionAnchor, range_selection, visible_host_keys};
use muxus_ui::gpui_kit::{Context, KeyDownEvent, Window};

use crate::ui::sidebar::{SessionSidebar, SidebarAction, SidebarEvent};

/// Long enough to type a word, short enough that a pause starts a new search.
const TYPE_AHEAD_RESET: Duration = Duration::from_millis(700);

/// What has been typed at the tree, and when the last key came.
#[derive(Clone, Debug, Default)]
pub struct TypeAhead {
    text: String,
    at: Option<Instant>,
}

impl TypeAhead {
    /// The buffer after typing `character` at `now`: a pause starts afresh.
    pub fn push(&mut self, character: &str, now: Instant) -> &str {
        let fresh = self
            .at
            .is_none_or(|at| now.saturating_duration_since(at) > TYPE_AHEAD_RESET);
        if fresh {
            self.text.clear();
        }
        self.text.push_str(character);
        self.at = Some(now);
        &self.text
    }
}

impl SessionSidebar {
    pub(crate) fn on_tree_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let command = modifiers.control || modifiers.platform;
        let index = self
            .focused_key
            .as_ref()
            .and_then(|key| self.rows.iter().position(|row| &row.key == key))
            .unwrap_or(0);
        if command && !modifiers.alt && !modifiers.shift && keystroke.key == "a" {
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
            // Alt+Arrow is the reorder shortcut and belongs to the row (`HostRow` and
            // `FolderRow` claim it before the tree sees it).
            if modifiers.alt {
                let delta = match nav {
                    TreeNavKey::ArrowUp => -1,
                    TreeNavKey::ArrowDown => 1,
                    _ => return,
                };
                if self.move_row(index, delta, cx) {
                    cx.stop_propagation();
                }
                return;
            }
            if command {
                return;
            }
            // Still swallowed when nothing moves: an arrow key must never scroll the
            // sidebar out from under the row that has focus.
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
            "enter" | "space" => {
                cx.stop_propagation();
                let Some(row) = self.rows.get(index).cloned() else {
                    return;
                };
                match &row.node {
                    RowNode::Host { host, .. } => {
                        // Shift+Space toggles the selection; otherwise the row clicks itself
                        // (a keyboard click: no count, the modifiers held).
                        let toggle_only = keystroke.key == "space"
                            && modifiers.shift
                            && !command
                            && !modifiers.alt;
                        let activation = if toggle_only {
                            HostActivation {
                                toggle_selection: true,
                                ..Default::default()
                            }
                        } else {
                            HostActivation {
                                new_session: modifiers.shift,
                                repeat: false,
                                clicks: Some(0),
                                toggle_selection: command,
                                extend_selection: modifiers.shift,
                            }
                        };
                        self.activate_host(host.clone(), activation, cx);
                    }
                    _ => {
                        let expanded = self.is_expanded(&row.key);
                        self.set_expanded(&row.key, !expanded, cx);
                    }
                }
            }
            "escape" => {
                cx.stop_propagation();
                // Escape lets go of a selection before it leaves the tree.
                if !self.selection.is_empty() {
                    self.selection.clear();
                    self.anchor = None;
                    cx.notify();
                } else {
                    let handle = self.search_focus(cx);
                    window.focus(&handle, cx);
                }
            }
            _ => self.type_ahead_key(event, index, window, cx),
        }
    }

    /// Type-ahead: single printable characters only, so shortcuts still work.
    fn type_ahead_key(
        &mut self,
        event: &KeyDownEvent,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt {
            return;
        }
        let Some(character) = keystroke
            .key_char
            .as_deref()
            .filter(|text| text.chars().count() == 1)
        else {
            return;
        };
        let text = self.type_ahead.push(character, Instant::now()).to_string();
        let labels: Vec<&str> = self.rows.iter().map(|row| row.node.label()).collect();
        if let Some(found) = type_ahead_index(&labels, index, &text) {
            cx.stop_propagation();
            self.focus_index(found, window, cx);
        }
    }

    /// Alt+Arrow: a host swaps with its neighbour among its siblings, a folder with
    /// the folder next to it. Only against the full, unfiltered list; whether the
    /// row took the key.
    fn move_row(&mut self, index: usize, delta: isize, cx: &mut Context<Self>) -> bool {
        if !self.reorder_enabled() {
            return false;
        }
        let Some(row) = self.rows.get(index) else {
            return false;
        };
        let key = row.key.clone();
        match row.node {
            RowNode::Host { .. } => {
                cx.emit(SidebarEvent::Action(SidebarAction::MoveHost { key, delta }));
                true
            }
            RowNode::Folder { .. } => {
                self.move_folder(&key, delta, cx);
                true
            }
            // ssh_config file groups have no order of their own.
            RowNode::File { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pause_starts_a_new_search() {
        let start = Instant::now();
        let mut buffer = TypeAhead::default();
        assert_eq!(buffer.push("w", start), "w");
        assert_eq!(buffer.push("e", start + Duration::from_millis(300)), "we");
        // Each key restarts the window.
        assert_eq!(buffer.push("b", start + Duration::from_millis(900)), "web");
        assert_eq!(buffer.push("d", start + Duration::from_millis(1700)), "d");
    }
}
