//! What a tree of device data shows (`DataTreeView.tsx` `visibleRows`, and
//! the result trees of `ResultPane.tsx`).

use std::collections::{HashMap, HashSet};

use muxus_core::gnmi_path::{GnmiPath, parse_gnmi_path};
use muxus_core::js::js_trim;
use muxus_core::management::GnmiNotification;
use muxus_mgmt::workbench::data_tree::{
    DataNode, DataNodeKind, KeyRegistry, count_leaves, create_root, display_children, merge_json,
    node_at, node_label,
};

use super::values::value_text;

/// Height of a tree row, in px.
pub const ROW_HEIGHT: f32 = 26.;
/// Indent per level, in px.
pub const INDENT: f32 = 14.;
/// Rows drawn beyond the viewport on each side.
pub const OVERSCAN: usize = 12;

/// One row to draw.
#[derive(Debug, Clone, Copy)]
pub struct TreeRow<'a> {
    pub node: &'a DataNode,
    pub depth: usize,
    pub expandable: bool,
    pub expanded: bool,
    /// The row's parent, for ArrowLeft.
    pub parent_id: Option<&'a str>,
}

pub fn is_branch(node: &DataNode) -> bool {
    matches!(
        node.kind,
        DataNodeKind::Container | DataNodeKind::List | DataNodeKind::Entry | DataNodeKind::Root
    )
}

fn matches_needle(node: &DataNode, needle: &str) -> bool {
    if node_label(node).to_lowercase().contains(needle) {
        return true;
    }
    if node.is_leaf() {
        return value_text(node.value.as_ref())
            .to_lowercase()
            .contains(needle);
    }
    false
}

/// The filter as the tree compares it: trimmed and lowercased.
pub fn needle_of(filter: &str) -> String {
    js_trim(filter).to_lowercase()
}

/// Rows to draw, in order. With a filter, every match shows together with
/// its ancestors, expanded or not.
pub fn visible_rows<'a>(
    root: &'a DataNode,
    expanded: &HashSet<String>,
    filter: &str,
    show_root: bool,
    lazy: bool,
) -> Vec<TreeRow<'a>> {
    struct Walk<'s> {
        needle: String,
        expanded: &'s HashSet<String>,
        lazy: bool,
        keep: HashMap<*const DataNode, bool>,
    }
    impl<'s> Walk<'s> {
        fn subtree_matches(&mut self, node: &DataNode) -> bool {
            let key = node as *const DataNode;
            if let Some(&cached) = self.keep.get(&key) {
                return cached;
            }
            let mut result = matches_needle(node, &self.needle);
            for child in &node.children {
                if self.subtree_matches(child) {
                    result = true;
                }
            }
            self.keep.insert(key, result);
            result
        }

        fn visit<'a>(
            &mut self,
            rows: &mut Vec<TreeRow<'a>>,
            node: &'a DataNode,
            depth: usize,
            parent_id: Option<&'a str>,
        ) {
            let children = display_children(node);
            let expandable =
                is_branch(node) && (!children.is_empty() || (self.lazy && !node.loaded));
            let open = !self.needle.is_empty() || self.expanded.contains(&node.id);
            rows.push(TreeRow {
                node,
                depth,
                expandable,
                expanded: expandable && open,
                parent_id,
            });
            if !open {
                return;
            }
            for child in children {
                if !self.needle.is_empty() && !self.subtree_matches(child) {
                    continue;
                }
                self.visit(rows, child, depth + 1, Some(&node.id));
            }
        }
    }

    let mut walk = Walk {
        needle: needle_of(filter),
        expanded,
        lazy,
        keep: HashMap::new(),
    };
    let mut rows = Vec::new();
    if show_root {
        walk.visit(&mut rows, root, 0, None);
    } else {
        for child in display_children(root) {
            if !walk.needle.is_empty() && !walk.subtree_matches(child) {
                continue;
            }
            walk.visit(&mut rows, child, 0, None);
        }
    }
    rows
}

/// Where a filter match sits in a label: the text before, the match and the rest.
pub fn highlight<'t>(text: &'t str, needle: &str) -> Option<(&'t str, &'t str, &'t str)> {
    if needle.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    // Lowercasing keeps byte offsets for ASCII; anything else is matched as is.
    if lower.len() != text.len() {
        return None;
    }
    let start = lower.find(needle)?;
    let end = start + needle.len();
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return None;
    }
    Some((&text[..start], &text[start..end], &text[end..]))
}

/// The tree of a gNMI Get reply, keys learned along the way.
pub fn tree_from_notifications(
    notifications: &[GnmiNotification],
    registry: &KeyRegistry,
) -> DataNode {
    let mut registry = registry.clone();
    let mut root = create_root(GnmiPath::default());
    for notification in notifications {
        for update in &notification.updates {
            // An unparseable path stays visible in the raw view.
            let Ok(path) = parse_gnmi_path(&update.path) else {
                continue;
            };
            let node = node_at(&mut root, &path, Some(&mut registry));
            merge_json(node, &update.value.value, Some(&mut registry));
        }
    }
    root
}

/// Open the path down to the data and, for small results, everything.
pub fn initial_expansion(root: &DataNode) -> HashSet<String> {
    let mut expanded = HashSet::new();
    let small = count_leaves(root) <= 400;
    fn walk(node: &DataNode, depth: usize, small: bool, expanded: &mut HashSet<String>) {
        if !node.is_leaf() && (small || depth < 2 || node.children.len() == 1) {
            expanded.insert(node.id.clone());
            for child in &node.children {
                walk(child, depth + 1, small, expanded);
            }
        }
    }
    for child in &root.children {
        walk(child, 0, small, &mut expanded);
    }
    expanded
}

/// "Expand all": every node with children.
pub fn expand_all(root: &DataNode) -> HashSet<String> {
    let mut expanded = HashSet::new();
    fn walk(node: &DataNode, expanded: &mut HashSet<String>) {
        if !node.children.is_empty() {
            expanded.insert(node.id.clone());
        }
        for child in &node.children {
            walk(child, expanded);
        }
    }
    walk(root, &mut expanded);
    expanded
}

/// The tooltip of a list entry row: its key leaves, and whether they were guessed.
pub fn entry_title(node: &DataNode) -> Option<String> {
    (node.kind == DataNodeKind::Entry).then(|| {
        let keys: Vec<&str> = node
            .keys
            .iter()
            .flat_map(|keys| keys.iter())
            .map(|(key, _)| key)
            .collect();
        format!(
            "Key: {}{}",
            keys.join(", "),
            if node.guessed_keys == Some(true) {
                " (guessed from the data)"
            } else {
                ""
            }
        )
    })
}

/// Index of the row the keyboard moves to, for the keys the tree handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeKey {
    Down,
    Up,
    Home,
    End,
    PageDown,
    PageUp,
    Right,
    Left,
    Enter,
}

/// What a key press does to the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeKeyAction {
    /// Select (and scroll to) the row at this index.
    Move(usize),
    /// Toggle the row at this index.
    Toggle(usize),
    /// Activate the leaf at this index.
    Activate(usize),
    None,
}

/// `DataTreeView`'s keyboard handling: `selected` is the selected node id,
/// `page` the rows a viewport holds.
pub fn tree_key(
    rows: &[TreeRow<'_>],
    selected: Option<&str>,
    key: TreeKey,
    page: usize,
) -> TreeKeyAction {
    if rows.is_empty() {
        return TreeKeyAction::None;
    }
    let index = selected
        .and_then(|id| rows.iter().position(|row| row.node.id == id))
        .unwrap_or(0);
    let row = &rows[index];
    let clamp = |next: isize| next.clamp(0, rows.len() as isize - 1) as usize;
    match key {
        TreeKey::Down => TreeKeyAction::Move(if selected.is_none() {
            0
        } else {
            clamp(index as isize + 1)
        }),
        TreeKey::Up => TreeKeyAction::Move(clamp(index as isize - 1)),
        TreeKey::Home => TreeKeyAction::Move(0),
        TreeKey::End => TreeKeyAction::Move(rows.len() - 1),
        TreeKey::PageDown => TreeKeyAction::Move(clamp(index as isize + page as isize)),
        TreeKey::PageUp => TreeKeyAction::Move(clamp(index as isize - page as isize)),
        TreeKey::Right => {
            if row.expandable && !row.expanded {
                TreeKeyAction::Toggle(index)
            } else if row.expanded {
                TreeKeyAction::Move(clamp(index as isize + 1))
            } else {
                TreeKeyAction::None
            }
        }
        TreeKey::Left => {
            if row.expanded {
                TreeKeyAction::Toggle(index)
            } else if let Some(parent) = row.parent_id {
                rows.iter()
                    .position(|candidate| candidate.node.id == parent)
                    .map_or(TreeKeyAction::None, TreeKeyAction::Move)
            } else {
                TreeKeyAction::None
            }
        }
        TreeKey::Enter => {
            if row.expandable {
                TreeKeyAction::Toggle(index)
            } else {
                TreeKeyAction::Activate(index)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::management::{GnmiUpdate, GnmiValue, GnmiValueType};
    use serde_json::json;

    fn notification(path: &str, value: serde_json::Value) -> GnmiNotification {
        GnmiNotification {
            timestamp: "1".into(),
            prefix: None,
            atomic: None,
            updates: vec![GnmiUpdate {
                path: path.into(),
                value: GnmiValue {
                    kind: GnmiValueType::JsonIetf,
                    value,
                },
                duplicates: None,
            }],
            deletes: Vec::new(),
        }
    }

    fn sample() -> DataNode {
        tree_from_notifications(
            &[
                notification(
                    "/interface[name=ethernet-1/1]",
                    json!({"admin-state": "enable", "statistics": {"in-octets": "12", "out-octets": "7"}}),
                ),
                notification("/system/name/host-name", json!("leaf1")),
            ],
            &KeyRegistry::default(),
        )
    }

    fn labels(rows: &[TreeRow<'_>]) -> Vec<String> {
        rows.iter()
            .map(|row| format!("{}{}", "  ".repeat(row.depth), node_label(row.node)))
            .collect()
    }

    #[test]
    fn shows_expanded_branches_with_leaves_first() {
        let root = sample();
        let expanded = initial_expansion(&root);
        let rows = visible_rows(&root, &expanded, "", false, false);
        assert_eq!(
            labels(&rows),
            vec![
                "interface",
                "  ethernet-1/1",
                "    admin-state",
                "    statistics",
                "      in-octets",
                "      out-octets",
                "system",
                "  name",
                "    host-name",
            ]
        );
        assert!(rows[0].expandable && rows[0].expanded);
        assert!(!rows[2].expandable);
        assert_eq!(rows[2].parent_id, Some(rows[1].node.id.as_str()));
        let collapsed = visible_rows(&root, &HashSet::new(), "", false, false);
        assert_eq!(labels(&collapsed), vec!["interface", "system"]);
    }

    #[test]
    fn a_filter_shows_matches_with_their_ancestors() {
        let root = sample();
        let rows = visible_rows(&root, &HashSet::new(), "  OUT-oct ", false, false);
        assert_eq!(
            labels(&rows),
            vec![
                "interface",
                "  ethernet-1/1",
                "    statistics",
                "      out-octets"
            ]
        );
        // Values match too.
        let rows = visible_rows(&root, &HashSet::new(), "leaf1", false, false);
        assert_eq!(labels(&rows), vec!["system", "  name", "    host-name"]);
    }

    #[test]
    fn lazy_trees_offer_branches_not_loaded_yet() {
        let mut root = create_root(GnmiPath::default());
        let path = parse_gnmi_path("/system").unwrap();
        node_at(&mut root, &path, None);
        let rows = visible_rows(&root, &HashSet::new(), "", false, true);
        assert!(rows[0].expandable);
        let rows = visible_rows(&root, &HashSet::new(), "", false, false);
        assert!(!rows[0].expandable);
        let rows = visible_rows(&root, &HashSet::from([String::new()]), "", true, false);
        assert_eq!(rows.len(), 2);
        assert_eq!(node_label(rows[0].node), "/");
    }

    #[test]
    fn large_results_open_two_levels() {
        let mut entries = serde_json::Map::new();
        for index in 0..500 {
            entries.insert(format!("leaf{index}"), json!(index));
        }
        let root = tree_from_notifications(
            &[notification("/a/b/c", serde_json::Value::Object(entries))],
            &KeyRegistry::default(),
        );
        let expanded = initial_expansion(&root);
        // a and b open (depth < 2); c holds 500 leaves and stays closed.
        assert!(expanded.contains(&root.children[0].id));
        assert!(expanded.contains(&root.children[0].children[0].id));
        assert!(!expanded.contains(&root.children[0].children[0].children[0].id));
        assert_eq!(expand_all(&root).len(), 4);
    }

    #[test]
    fn highlights_the_first_match() {
        assert_eq!(highlight("in-Octets", "oct"), Some(("in-", "Oct", "ets")));
        assert_eq!(highlight("abc", ""), None);
        assert_eq!(highlight("abc", "x"), None);
    }

    #[test]
    fn moves_through_rows_with_the_keyboard() {
        let root = sample();
        let expanded = initial_expansion(&root);
        let rows = visible_rows(&root, &expanded, "", false, false);
        assert_eq!(
            tree_key(&rows, None, TreeKey::Down, 10),
            TreeKeyAction::Move(0)
        );
        let first = rows[0].node.id.as_str();
        assert_eq!(
            tree_key(&rows, Some(first), TreeKey::Down, 10),
            TreeKeyAction::Move(1)
        );
        assert_eq!(
            tree_key(&rows, Some(first), TreeKey::Up, 10),
            TreeKeyAction::Move(0)
        );
        assert_eq!(
            tree_key(&rows, Some(first), TreeKey::End, 10),
            TreeKeyAction::Move(8)
        );
        assert_eq!(
            tree_key(&rows, Some(first), TreeKey::PageDown, 4),
            TreeKeyAction::Move(4)
        );
        assert_eq!(
            tree_key(&rows, Some(first), TreeKey::Left, 4),
            TreeKeyAction::Toggle(0)
        );
        assert_eq!(
            tree_key(&rows, Some(first), TreeKey::Right, 4),
            TreeKeyAction::Move(1)
        );
        let leaf = rows[2].node.id.as_str();
        assert_eq!(
            tree_key(&rows, Some(leaf), TreeKey::Left, 4),
            TreeKeyAction::Move(1)
        );
        assert_eq!(
            tree_key(&rows, Some(leaf), TreeKey::Enter, 4),
            TreeKeyAction::Activate(2)
        );
        assert_eq!(tree_key(&[], None, TreeKey::Down, 4), TreeKeyAction::None);
    }
}
