//! Arrow-key semantics for a flattened tree, per the WAI-ARIA tree pattern.
//! Kept pure so the behaviour is testable. Port of
//! `client/src/components/sidebar/tree-navigation.ts`.

use crate::hosts::VisibleNode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeNavKey {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeNavAction {
    Focus { index: usize },
    Expand { key: String },
    Collapse { key: String },
    None,
}

pub fn tree_nav_action(nodes: &[VisibleNode], index: usize, key: TreeNavKey) -> TreeNavAction {
    if nodes.is_empty() {
        return TreeNavAction::None;
    }
    let current = nodes.get(index);
    match key {
        TreeNavKey::ArrowDown if index + 1 < nodes.len() => {
            TreeNavAction::Focus { index: index + 1 }
        }
        TreeNavKey::ArrowUp if index > 0 => TreeNavAction::Focus { index: index - 1 },
        TreeNavKey::ArrowDown | TreeNavKey::ArrowUp => TreeNavAction::None,
        TreeNavKey::Home => TreeNavAction::Focus { index: 0 },
        TreeNavKey::End => TreeNavAction::Focus {
            index: nodes.len() - 1,
        },
        TreeNavKey::ArrowRight => {
            let Some(current) = current else {
                return TreeNavAction::None;
            };
            match current.expanded {
                None => TreeNavAction::None,
                Some(false) => TreeNavAction::Expand {
                    key: current.key.clone(),
                },
                // An expanded container's first child is always the next visible row.
                Some(true) => match nodes.get(index + 1) {
                    Some(child) if child.depth > current.depth => {
                        TreeNavAction::Focus { index: index + 1 }
                    }
                    _ => TreeNavAction::None,
                },
            }
        }
        TreeNavKey::ArrowLeft => {
            let Some(current) = current else {
                return TreeNavAction::None;
            };
            if current.expanded == Some(true) {
                return TreeNavAction::Collapse {
                    key: current.key.clone(),
                };
            }
            match parent_index(nodes, index) {
                Some(parent) => TreeNavAction::Focus { index: parent },
                None => TreeNavAction::None,
            }
        }
    }
}

/// Nearest preceding row one level shallower, or `None` at the top level.
pub fn parent_index(nodes: &[VisibleNode], index: usize) -> Option<usize> {
    let depth = nodes.get(index)?.depth;
    if depth == 0 {
        return None;
    }
    (0..index).rev().find(|&i| nodes[i].depth < depth)
}

/// Type-ahead: the next row after `from` whose label starts with `buffer`,
/// wrapping around. Returns `None` when nothing matches so the caller can leave
/// focus alone rather than jumping to the top.
pub fn type_ahead_index<S: AsRef<str>>(labels: &[S], from: usize, buffer: &str) -> Option<usize> {
    let needle = buffer.to_lowercase();
    if needle.is_empty() || labels.is_empty() {
        return None;
    }
    for offset in 1..=labels.len() {
        let index = (from + offset) % labels.len();
        if labels[index].as_ref().to_lowercase().starts_with(&needle) {
            return Some(index);
        }
    }
    // Repeating the same letter cycles; a longer buffer should still match the
    // row already focused rather than reporting no match at all.
    let focused_matches = labels
        .get(from)
        .is_some_and(|label| label.as_ref().to_lowercase().starts_with(&needle));
    (needle.chars().count() > 1 && focused_matches).then_some(from)
}

/// Where focus should land after the visible rows change. Keeping the same key
/// wins; otherwise the row that took its place, clamped to the list.
pub fn focus_after_change(
    nodes: &[VisibleNode],
    previous_key: Option<&str>,
    previous_index: usize,
) -> Option<String> {
    if nodes.is_empty() {
        return None;
    }
    if let Some(key) = previous_key
        && nodes.iter().any(|node| node.key == key)
    {
        return Some(key.to_string());
    }
    let clamped = previous_index.min(nodes.len() - 1);
    Some(nodes[clamped].key.clone())
}

#[cfg(test)]
mod tests {
    use super::TreeNavKey::*;
    use super::*;
    use crate::hosts::test_support::visible_row;

    fn row(key: &str, depth: usize, expanded: Option<bool>) -> VisibleNode {
        visible_row(key, depth, expanded, true)
    }

    ///  0 prod            folder, expanded
    ///  1   eu            folder, expanded
    ///  2     host-a
    ///  3   us            folder, collapsed
    ///  4 lab             folder, expanded
    ///  5   host-b
    fn nodes() -> Vec<VisibleNode> {
        vec![
            row("prod", 0, Some(true)),
            row("eu", 1, Some(true)),
            row("host-a", 2, None),
            row("us", 1, Some(false)),
            row("lab", 0, Some(true)),
            row("host-b", 1, None),
        ]
    }

    fn focus(index: usize) -> TreeNavAction {
        TreeNavAction::Focus { index }
    }

    #[test]
    fn steps_down_and_up_through_visible_rows_and_stops_at_the_ends() {
        let n = nodes();
        assert_eq!(tree_nav_action(&n, 0, ArrowDown), focus(1));
        assert_eq!(tree_nav_action(&n, 5, ArrowDown), TreeNavAction::None);
        assert_eq!(tree_nav_action(&n, 3, ArrowUp), focus(2));
        assert_eq!(tree_nav_action(&n, 0, ArrowUp), TreeNavAction::None);
    }

    #[test]
    fn jumps_to_the_first_and_last_visible_rows() {
        let n = nodes();
        assert_eq!(tree_nav_action(&n, 3, Home), focus(0));
        assert_eq!(tree_nav_action(&n, 3, End), focus(5));
    }

    #[test]
    fn expands_a_collapsed_folder_then_steps_into_it() {
        let n = nodes();
        assert_eq!(
            tree_nav_action(&n, 3, ArrowRight),
            TreeNavAction::Expand { key: "us".into() }
        );
        assert_eq!(tree_nav_action(&n, 1, ArrowRight), focus(2));
    }

    #[test]
    fn does_nothing_on_a_host_or_an_expanded_but_childless_folder() {
        assert_eq!(
            tree_nav_action(&nodes(), 2, ArrowRight),
            TreeNavAction::None
        );
        let childless = [row("empty", 0, Some(true)), row("sibling", 0, Some(true))];
        assert_eq!(
            tree_nav_action(&childless, 0, ArrowRight),
            TreeNavAction::None
        );
    }

    #[test]
    fn collapses_an_expanded_folder_otherwise_walks_to_the_parent() {
        let n = nodes();
        assert_eq!(
            tree_nav_action(&n, 1, ArrowLeft),
            TreeNavAction::Collapse { key: "eu".into() }
        );
        assert_eq!(tree_nav_action(&n, 2, ArrowLeft), focus(1));
        // A collapsed folder steps out rather than collapsing again.
        assert_eq!(tree_nav_action(&n, 3, ArrowLeft), focus(0));
        assert_eq!(
            tree_nav_action(&n, 0, ArrowLeft),
            TreeNavAction::Collapse { key: "prod".into() }
        );
    }

    #[test]
    fn has_nothing_to_do_in_an_empty_tree() {
        assert_eq!(tree_nav_action(&[], 0, ArrowDown), TreeNavAction::None);
    }

    #[test]
    fn finds_the_nearest_shallower_row_above() {
        let n = nodes();
        assert_eq!(parent_index(&n, 2), Some(1));
        assert_eq!(parent_index(&n, 3), Some(0));
        assert_eq!(parent_index(&n, 5), Some(4));
    }

    #[test]
    fn reports_no_parent_at_the_top_level() {
        let n = nodes();
        assert_eq!(parent_index(&n, 0), None);
        assert_eq!(parent_index(&n, 4), None);
    }

    const LABELS: [&str; 4] = ["Production", "edge-1", "edge-2", "Lab"];

    #[test]
    fn finds_the_next_prefix_match_after_the_current_row() {
        assert_eq!(type_ahead_index(&LABELS, 0, "e"), Some(1));
        assert_eq!(type_ahead_index(&LABELS, 1, "e"), Some(2));
    }

    #[test]
    fn wraps_around_the_end_of_the_list() {
        assert_eq!(type_ahead_index(&LABELS, 2, "e"), Some(1));
        assert_eq!(type_ahead_index(&LABELS, 3, "p"), Some(0));
    }

    #[test]
    fn ignores_case() {
        assert_eq!(type_ahead_index(&LABELS, 3, "PROD"), Some(0));
    }

    #[test]
    fn reports_no_match_rather_than_moving_focus() {
        assert_eq!(type_ahead_index(&LABELS, 0, "zz"), None);
        assert_eq!(type_ahead_index(&LABELS, 0, ""), None);
    }

    #[test]
    fn keeps_the_focused_row_when_a_growing_buffer_still_matches_it() {
        // Typing "e", "d", "g" must not hop away from the row it already selected.
        assert_eq!(type_ahead_index(&LABELS, 1, "edg"), Some(2));
        assert_eq!(type_ahead_index(&LABELS, 1, "edge-1"), Some(1));
    }

    #[test]
    fn keeps_the_focused_row_when_it_survives() {
        assert_eq!(
            focus_after_change(&nodes(), Some("host-a"), 2).as_deref(),
            Some("host-a")
        );
    }

    #[test]
    fn falls_back_to_whatever_took_its_place() {
        assert_eq!(
            focus_after_change(&nodes(), Some("deleted"), 3).as_deref(),
            Some("us")
        );
    }

    #[test]
    fn clamps_past_the_end_of_a_shorter_list() {
        assert_eq!(
            focus_after_change(&nodes()[..2], Some("deleted"), 5).as_deref(),
            Some("eu")
        );
    }

    #[test]
    fn reports_nothing_to_focus_in_an_empty_tree() {
        assert_eq!(focus_after_change(&[], Some("gone"), 0), None);
    }
}
