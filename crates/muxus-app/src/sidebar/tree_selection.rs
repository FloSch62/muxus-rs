//! Multi-selection rules for the host tree, kept free of UI so they can be
//! tested. Only hosts are ever selected; folders are what ranges run through.
//! Port of `client/src/components/sidebar/tree-selection.ts`.
//!
//! A range is file-manager style: Ctrl/Cmd-click sets the anchor and fixes the
//! selection around it (the base), and each Shift-click or Shift+Arrow then
//! re-derives base ∪ anchor…target. Moving the target back shrinks the range
//! again instead of leaving every row it ever touched selected.

use std::collections::BTreeSet;

use crate::hosts::VisibleNode;

/// Selected host keys.
pub type Selection = BTreeSet<String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeSelectionAnchor {
    pub key: String,
    pub base: Selection,
}

/// Add a host to the selection, or take it out; it becomes the new anchor.
pub fn toggle_selection(selection: &Selection, key: &str) -> (Selection, TreeSelectionAnchor) {
    let mut next = selection.clone();
    if !next.remove(key) {
        next.insert(key.to_string());
    }
    let anchor = TreeSelectionAnchor {
        key: key.to_string(),
        base: next.clone(),
    };
    (next, anchor)
}

/// The anchor's base plus every host from the anchor to `key` in visible
/// order. An anchor that is no longer visible, such as one inside a folder
/// collapsed since, starts the range at `key` itself.
pub fn range_selection(rows: &[VisibleNode], anchor: &TreeSelectionAnchor, key: &str) -> Selection {
    let mut next = anchor.base.clone();
    let Some(to) = rows.iter().position(|row| row.key == key) else {
        return next;
    };
    let from = rows
        .iter()
        .position(|row| row.key == anchor.key)
        .unwrap_or(to);
    let (low, high) = if from <= to { (from, to) } else { (to, from) };
    for row in &rows[low..=high] {
        if row.node.is_host() {
            next.insert(row.key.clone());
        }
    }
    next
}

/// Every visible host, for Ctrl/Cmd+A.
pub fn visible_host_keys(rows: &[VisibleNode]) -> Selection {
    rows.iter()
        .filter(|row| row.node.is_host())
        .map(|row| row.key.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::test_support::visible_row;

    fn set(keys: &[&str]) -> Selection {
        keys.iter().map(|k| k.to_string()).collect()
    }

    /// Folders interleave with hosts; a range runs through them and skips them.
    fn rows() -> Vec<VisibleNode> {
        [
            ("prod", false),
            ("a", true),
            ("b", true),
            ("lab", false),
            ("c", true),
            ("d", true),
        ]
        .into_iter()
        .map(|(key, host)| visible_row(key, 0, None, host))
        .collect()
    }

    #[test]
    fn adds_and_removes_a_host_anchoring_the_next_range_on_it() {
        let (selection, anchor) = toggle_selection(&set(&["a"]), "c");
        assert_eq!(selection, set(&["a", "c"]));
        assert_eq!(
            anchor,
            TreeSelectionAnchor {
                key: "c".into(),
                base: set(&["a", "c"])
            }
        );
        assert_eq!(toggle_selection(&selection, "a").0, set(&["c"]));
    }

    #[test]
    fn selects_the_hosts_from_the_anchor_to_the_target_skipping_folders() {
        let anchor = TreeSelectionAnchor {
            key: "a".into(),
            base: set(&["a"]),
        };
        assert_eq!(
            range_selection(&rows(), &anchor, "c"),
            set(&["a", "b", "c"])
        );
    }

    #[test]
    fn works_upwards_and_shrinks_again_when_the_target_moves_back() {
        let anchor = TreeSelectionAnchor {
            key: "d".into(),
            base: set(&["d"]),
        };
        assert_eq!(
            range_selection(&rows(), &anchor, "b"),
            set(&["b", "c", "d"])
        );
        assert_eq!(range_selection(&rows(), &anchor, "c"), set(&["c", "d"]));
    }

    #[test]
    fn keeps_what_was_selected_before_the_anchor_was_set() {
        let anchor = TreeSelectionAnchor {
            key: "c".into(),
            base: set(&["a", "c"]),
        };
        assert_eq!(
            range_selection(&rows(), &anchor, "d"),
            set(&["a", "c", "d"])
        );
    }

    #[test]
    fn starts_at_the_target_when_the_anchor_is_no_longer_visible() {
        let anchor = TreeSelectionAnchor {
            key: "hidden".into(),
            base: set(&["x"]),
        };
        assert_eq!(range_selection(&rows(), &anchor, "b"), set(&["b", "x"]));
    }

    #[test]
    fn collects_only_hosts() {
        assert_eq!(visible_host_keys(&rows()), set(&["a", "b", "c", "d"]));
    }
}
