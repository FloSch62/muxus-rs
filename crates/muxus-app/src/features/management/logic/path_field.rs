//! What a gNMI path field completes from the device's own data
//! (`PathField.tsx`): children, list entries with their keys, and leaf
//! values as a preview.

use muxus_core::gnmi_path::{
    GnmiPath, GnmiPathElem, format_gnmi_elem, parse_gnmi_path, strip_module_prefix,
};
use muxus_core::js::js_trim;
use muxus_mgmt::workbench::data_tree::{DataNode, DataNodeKind};

use super::values::value_text;

/// The most suggestions a list shows.
const MAX_SUGGESTIONS: usize = 200;

/// One completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Text replacing the segment being typed.
    pub insert: String,
    pub node_id: String,
    pub kind: DataNodeKind,
    /// Continue typing below it after accepting.
    pub branch: bool,
    /// Leaves: the value, as a preview.
    pub preview: Option<String>,
    /// Lists: how many entries the explorer knows.
    pub entries: Option<usize>,
}

/// The completions for a path text, and the level they come from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Suggestions {
    pub suggestions: Vec<Suggestion>,
    /// The explorer node being completed below, when it is known.
    pub parent: Option<String>,
    /// The parsed parent path, when it parses.
    pub elems: Option<Vec<GnmiPathElem>>,
}

/// Split at the last `/` outside a key predicate: the parent path and the segment being typed.
pub fn split_last_segment(text: &str) -> (&str, &str) {
    let mut depth = 0usize;
    let mut cut: Option<usize> = None;
    let mut chars = text.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            '/' if depth == 0 => cut = Some(index),
            _ => {}
        }
    }
    match cut {
        Some(cut) => (&text[..cut], &text[cut + 1..]),
        None => ("", text),
    }
}

fn find_child<'a>(node: &'a DataNode, elem: &GnmiPathElem) -> Option<&'a DataNode> {
    let name = strip_module_prefix(&elem.name);
    let child = node.children.iter().find(|candidate| {
        candidate.kind != DataNodeKind::Entry && strip_module_prefix(&candidate.name) == name
    })?;
    let Some(keys) = &elem.keys else {
        return Some(child);
    };
    child.children.iter().find(|entry| {
        entry.kind == DataNodeKind::Entry
            && keys.iter().all(|(key, value)| {
                entry
                    .keys
                    .as_ref()
                    .and_then(|keys| keys.get(key))
                    .is_some_and(|candidate| candidate == value)
            })
    })
}

/// The explorer node a path text leads to, if it was loaded.
fn resolve<'a>(
    root: &'a DataNode,
    parent_text: &str,
) -> (Option<&'a DataNode>, Option<Vec<GnmiPathElem>>) {
    if js_trim(parent_text).is_empty() {
        return (Some(root), Some(Vec::new()));
    }
    match parse_gnmi_path(parent_text) {
        Ok(path) => {
            let mut node = Some(root);
            for elem in &path.elems {
                node = node.and_then(|node| find_child(node, elem));
                if node.is_none() {
                    break;
                }
            }
            (node, Some(path.elems))
        }
        Err(_) => (None, None),
    }
}

fn suggestion(node: &DataNode, insert: String, branch: bool) -> Suggestion {
    Suggestion {
        insert,
        node_id: node.id.clone(),
        kind: node.kind,
        branch,
        preview: node.is_leaf().then(|| value_text(node.value.as_ref())),
        entries: (node.kind == DataNodeKind::List).then_some(node.children.len()),
    }
}

/// What to offer for `text` from the explorer tree under `root`.
pub fn suggestions_for(root: &DataNode, text: &str) -> Suggestions {
    let (parent_text, segment) = split_last_segment(text);
    let (node, elems) = resolve(root, parent_text);
    let Some(node) = node else {
        return Suggestions {
            elems,
            ..Suggestions::default()
        };
    };
    if let Some(bracket) = segment.find('[') {
        // Typing a key predicate: offer the list's entries.
        let name = &segment[..bracket];
        let list = node.children.iter().find(|child| {
            child.kind == DataNodeKind::List
                && strip_module_prefix(&child.name) == strip_module_prefix(name)
        });
        let Some(list) = list else {
            return Suggestions {
                suggestions: Vec::new(),
                parent: Some(node.id.clone()),
                elems,
            };
        };
        let typed = segment.to_lowercase();
        let suggestions = list
            .children
            .iter()
            .map(|entry| {
                let insert = format_gnmi_elem(&GnmiPathElem {
                    name: strip_module_prefix(&entry.name).to_owned(),
                    keys: entry.keys.clone(),
                });
                suggestion(entry, insert, true)
            })
            .filter(|candidate| {
                candidate.insert.to_lowercase().starts_with(&typed) || typed.ends_with('[')
            })
            .take(MAX_SUGGESTIONS)
            .collect();
        return Suggestions {
            suggestions,
            parent: Some(node.id.clone()),
            elems,
        };
    }
    let typed = segment.to_lowercase();
    // Branches first: completing a path is mostly about the way down.
    let children: Vec<&DataNode> = node
        .children
        .iter()
        .filter(|child| child.kind != DataNodeKind::Entry && !child.is_leaf())
        .chain(node.children.iter().filter(|child| child.is_leaf()))
        .collect();
    let lower = |child: &DataNode| strip_module_prefix(&child.name).to_lowercase();
    let starts = children
        .iter()
        .filter(|child| lower(child).starts_with(&typed));
    let contains = children.iter().filter(|child| {
        !typed.is_empty() && !lower(child).starts_with(&typed) && lower(child).contains(&typed)
    });
    let suggestions = starts
        .chain(contains)
        .take(MAX_SUGGESTIONS)
        .map(|child| {
            suggestion(
                child,
                strip_module_prefix(&child.name).to_owned(),
                !child.is_leaf(),
            )
        })
        .collect();
    Suggestions {
        suggestions,
        parent: Some(node.id.clone()),
        elems,
    }
}

/// The field's text after accepting `suggestion`.
pub fn accept_suggestion(value: &str, suggestion: &Suggestion) -> String {
    let (parent, _) = split_last_segment(value);
    format!(
        "{parent}/{}{}",
        suggestion.insert,
        if suggestion.branch { "/" } else { "" }
    )
}

/// The parsed parent path of `elems`, for loading a level the explorer has not fetched.
pub fn parent_path(elems: &[GnmiPathElem]) -> GnmiPath {
    GnmiPath {
        origin: None,
        elems: elems.to_vec(),
    }
}

/// The syntax error a path field shows under itself while focused.
pub fn syntax_error(value: &str) -> Option<String> {
    if js_trim(value).is_empty() {
        return None;
    }
    parse_gnmi_path(value).err().map(|err| err.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_mgmt::workbench::data_tree::{KeyRegistry, create_root, merge_json, node_at};
    use serde_json::json;

    fn explorer() -> DataNode {
        let mut root = create_root(GnmiPath::default());
        let mut registry = KeyRegistry::default();
        for (path, value) in [
            (
                "/srl_nokia-interfaces:interface[name=ethernet-1/1]",
                json!({"admin-state": "enable", "statistics": {"in-octets": "1"}}),
            ),
            (
                "/srl_nokia-interfaces:interface[name=mgmt0]",
                json!({"admin-state": "enable"}),
            ),
            ("/system/information", json!({"version": "v26.3.1"})),
            ("/acl", json!({"capture-filter": {}})),
        ] {
            let parsed = parse_gnmi_path(path).unwrap();
            let node = node_at(&mut root, &parsed, Some(&mut registry));
            merge_json(node, &value, Some(&mut registry));
        }
        root
    }

    fn inserts(suggestions: &Suggestions) -> Vec<&str> {
        suggestions
            .suggestions
            .iter()
            .map(|suggestion| suggestion.insert.as_str())
            .collect()
    }

    #[test]
    fn splits_outside_key_predicates() {
        assert_eq!(
            split_last_segment("/interface[name=e1/1]/sta"),
            ("/interface[name=e1/1]", "sta")
        );
        assert_eq!(
            split_last_segment("/interface[name=e1/"),
            ("", "interface[name=e1/")
        );
        assert_eq!(split_last_segment("sys"), ("", "sys"));
        assert_eq!(split_last_segment("/a\\/b/c"), ("/a\\/b", "c"));
    }

    #[test]
    fn offers_branches_first_then_leaves_then_partial_matches() {
        let root = explorer();
        let top = suggestions_for(&root, "/");
        assert_eq!(inserts(&top), vec!["interface", "system", "acl"]);
        assert_eq!(top.parent.as_deref(), Some(""));
        let typed = suggestions_for(&root, "/s");
        assert_eq!(inserts(&typed), vec!["system"]);
        let inner = suggestions_for(&root, "/system/information/");
        assert_eq!(inserts(&inner), vec!["version"]);
        assert_eq!(inner.suggestions[0].preview.as_deref(), Some("v26.3.1"));
        assert!(!inner.suggestions[0].branch);
        // Contains-matches come after prefix matches.
        let contains = suggestions_for(&root, "/c");
        assert_eq!(inserts(&contains), vec!["interface", "acl"]);
        let contains = suggestions_for(&root, "/acl/filter");
        assert_eq!(inserts(&contains), vec!["capture-filter"]);
    }

    #[test]
    fn offers_list_entries_with_their_keys() {
        let root = explorer();
        let entries = suggestions_for(&root, "/interface[");
        assert_eq!(
            inserts(&entries),
            vec!["interface[name=ethernet-1/1]", "interface[name=mgmt0]"]
        );
        let narrowed = suggestions_for(&root, "/interface[name=m");
        assert_eq!(inserts(&narrowed), vec!["interface[name=mgmt0]"]);
        let below = suggestions_for(&root, "/interface[name=ethernet-1/1]/st");
        assert_eq!(inserts(&below), vec!["statistics", "admin-state"]);
        let list = &suggestions_for(&root, "/int").suggestions[0];
        assert_eq!(list.entries, Some(2));
        assert_eq!(accept_suggestion("/int", list), "/interface/");
        assert_eq!(
            accept_suggestion("/interface[", &entries.suggestions[1]),
            "/interface[name=mgmt0]/"
        );
    }

    #[test]
    fn unknown_levels_report_the_parsed_path() {
        let root = explorer();
        let unknown = suggestions_for(&root, "/network-instance[name=default]/proto");
        assert!(unknown.suggestions.is_empty());
        assert_eq!(unknown.parent, None);
        assert_eq!(unknown.elems.as_ref().map(Vec::len), Some(1));
        let broken = suggestions_for(&root, "/x[=1]/y");
        assert_eq!(broken, Suggestions::default());
        assert_eq!(syntax_error("  "), None);
        assert!(syntax_error("/a[b").is_some());
    }
}
