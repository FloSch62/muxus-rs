//! What can be done with a node of device data, for either protocol
//! (`node-actions.tsx`): the drafts each action loads into the editor and
//! the texts it copies.

use std::sync::LazyLock;

use muxus_core::gnmi_path::format_gnmi_path;
use muxus_core::management::{GnmiSetEncoding, GnmiSubscriptionListMode, GnmiSubscriptionMode};
use muxus_core::protocol::GnmiProfile;
use muxus_mgmt::workbench::copy_as::gnmic_command;
use muxus_mgmt::workbench::data_tree::{DataNode, namespace_chain, node_to_json};
use muxus_mgmt::workbench::requests::{
    Datastore, FilterType, GnmiDraft, GnmiOperation, GnmiSetItem, GnmiSetItemOp, NetconfDraft,
    NetconfOperation, default_netconf_draft, new_item_id,
};
use muxus_mgmt::workbench::xml::{
    EditMode, edit_config_for, pretty_xml, subtree_filter_for, xpath_for,
};
use regex::Regex;
use serde_json::Value;

use super::values::value_text;

static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+$").expect("valid regex"));

/// `JSON.stringify(value, null, 2)`.
pub fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// The node and its ancestors, for NETCONF filters.
pub fn chain<'a>(root: &'a DataNode, node_id: &str) -> Vec<&'a DataNode> {
    namespace_chain(root, node_id)
}

/// The subtree filter that selects a node, pretty-printed.
pub fn filter_for(root: &DataNode, node_id: &str) -> String {
    pretty_xml(&subtree_filter_for(&chain(root, node_id), ""))
}

/// Where NETCONF edits go: the candidate when the device has one.
pub fn writable_target(has_candidate: bool) -> Datastore {
    if has_candidate {
        Datastore::Candidate
    } else {
        Datastore::Running
    }
}

/// gNMI "Get": load the node into a Get.
pub fn gnmi_get(current: &GnmiDraft, node: &DataNode) -> GnmiDraft {
    GnmiDraft {
        operation: GnmiOperation::Get,
        paths: vec![format_gnmi_path(&node.path)],
        ..current.clone()
    }
}

/// NETCONF "Get with state": a get filtered on the node.
pub fn netconf_get(current: &NetconfDraft, root: &DataNode, node_id: &str) -> NetconfDraft {
    NetconfDraft {
        operation: NetconfOperation::Get,
        filter_type: FilterType::Subtree,
        filter: filter_for(root, node_id),
        ..current.clone()
    }
}

/// NETCONF "Get configuration": a get-config of running filtered on the node.
pub fn netconf_get_config(current: &NetconfDraft, root: &DataNode, node_id: &str) -> NetconfDraft {
    NetconfDraft {
        operation: NetconfOperation::GetConfig,
        source: Datastore::Running,
        filter_type: FilterType::Subtree,
        filter: filter_for(root, node_id),
        ..current.clone()
    }
}

/// "Watch": stream the node. Counters want samples; a single state leaf reads best on change.
pub fn watch(current: &GnmiDraft, node: &DataNode) -> GnmiDraft {
    let on_change = node.is_leaf()
        && matches!(&node.value, Some(Value::String(text)) if !DIGITS.is_match(text));
    let seconds = current.subscribe.sample_seconds;
    GnmiDraft {
        operation: GnmiOperation::Subscribe,
        paths: vec![format_gnmi_path(&node.path)],
        subscribe: muxus_mgmt::workbench::requests::GnmiSubscribeDraft {
            list_mode: GnmiSubscriptionListMode::Stream,
            mode: if on_change {
                GnmiSubscriptionMode::OnChange
            } else {
                GnmiSubscriptionMode::Sample
            },
            sample_seconds: if seconds > 10. { 1. } else { seconds }.max(1.),
            ..current.subscribe.clone()
        },
        ..current.clone()
    }
}

/// "Set value…" / "Edit as Set…": a Set of the node's current value, ready to edit.
pub fn set_value(current: &GnmiDraft, node: &DataNode) -> GnmiDraft {
    let value = if node.is_leaf() {
        node.value.clone().unwrap_or(Value::Null)
    } else {
        node_to_json(node)
    };
    GnmiDraft {
        operation: GnmiOperation::Set,
        set: vec![GnmiSetItem {
            id: new_item_id(),
            op: GnmiSetItemOp::Update,
            path: format_gnmi_path(&node.path),
            value: pretty_json(&value),
            encoding: GnmiSetEncoding::JsonIetf,
        }],
        ..current.clone()
    }
}

/// gNMI "Delete…": a Set that deletes the node, reviewed before it is sent.
pub fn gnmi_remove(current: &GnmiDraft, node: &DataNode) -> GnmiDraft {
    GnmiDraft {
        operation: GnmiOperation::Set,
        set: vec![GnmiSetItem {
            id: new_item_id(),
            op: GnmiSetItemOp::Delete,
            path: format_gnmi_path(&node.path),
            value: String::new(),
            encoding: GnmiSetEncoding::JsonIetf,
        }],
        ..current.clone()
    }
}

/// NETCONF "Delete…": an edit-config that deletes the node.
pub fn netconf_remove(root: &DataNode, node_id: &str, has_candidate: bool) -> NetconfDraft {
    NetconfDraft {
        operation: NetconfOperation::EditConfig,
        target: writable_target(has_candidate),
        config: edit_config_for(&chain(root, node_id), EditMode::Delete),
        ..default_netconf_draft()
    }
}

/// NETCONF "Edit configuration…": the fetched configuration (or a skeleton) in an edit-config.
pub fn netconf_edit(
    config: String,
    root: &DataNode,
    node_id: &str,
    has_candidate: bool,
) -> NetconfDraft {
    NetconfDraft {
        operation: NetconfOperation::EditConfig,
        target: writable_target(has_candidate),
        config: if config.is_empty() {
            edit_config_for(&chain(root, node_id), EditMode::Edit)
        } else {
            config
        },
        ..default_netconf_draft()
    }
}

/// NETCONF "Use as filter": keep get or get-config, filter on the node.
pub fn use_as_filter(current: &NetconfDraft, root: &DataNode, node_id: &str) -> NetconfDraft {
    NetconfDraft {
        operation: match current.operation {
            NetconfOperation::Get | NetconfOperation::GetConfig => current.operation,
            _ => NetconfOperation::Get,
        },
        filter_type: FilterType::Subtree,
        filter: filter_for(root, node_id),
        ..current.clone()
    }
}

/// "Copy path" (gNMI) or "Copy XPath" (NETCONF): the text and what the toast calls it.
pub fn copy_path(gnmi: bool, root: &DataNode, node: &DataNode) -> (String, &'static str) {
    if gnmi {
        (format_gnmi_path(&node.path), "the path")
    } else {
        (xpath_for(&chain(root, &node.id)), "the XPath")
    }
}

/// "Copy value" / "Copy as JSON".
pub fn copy_value(node: &DataNode) -> String {
    if node.is_leaf() {
        value_text(node.value.as_ref())
    } else {
        pretty_json(&node_to_json(node))
    }
}

/// "Copy gnmic get".
pub fn copy_gnmic(profile: &GnmiProfile, current: &GnmiDraft, node: &DataNode) -> String {
    gnmic_command(profile, Some(&gnmi_get(current, node)), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::gnmi_path::{GnmiPath, parse_gnmi_path};
    use muxus_mgmt::workbench::data_tree::{create_root, merge_json, node_at};
    use muxus_mgmt::workbench::requests::default_gnmi_draft;
    use serde_json::json;

    fn tree() -> DataNode {
        let mut root = create_root(GnmiPath::default());
        let path = parse_gnmi_path("/interface[name=e1]").unwrap();
        let node = node_at(&mut root, &path, None);
        merge_json(
            node,
            &json!({"oper-state": "up", "mtu": 9232, "statistics": {"in-octets": "5"}}),
            None,
        );
        root
    }

    fn find<'a>(node: &'a DataNode, name: &str) -> &'a DataNode {
        fn walk<'a>(node: &'a DataNode, name: &str) -> Option<&'a DataNode> {
            if node.name == name {
                return Some(node);
            }
            node.children.iter().find_map(|child| walk(child, name))
        }
        walk(node, name).expect("node in the tree")
    }

    #[test]
    fn watches_state_on_change_and_counters_by_sample() {
        let root = tree();
        let mut current = default_gnmi_draft();
        current.subscribe.sample_seconds = 30.;
        let state = watch(&current, find(&root, "oper-state"));
        assert_eq!(state.subscribe.mode, GnmiSubscriptionMode::OnChange);
        assert_eq!(state.subscribe.sample_seconds, 1.);
        assert_eq!(state.paths, vec!["/interface[name=e1]/oper-state"]);
        let counter = watch(&default_gnmi_draft(), find(&root, "in-octets"));
        assert_eq!(counter.subscribe.mode, GnmiSubscriptionMode::Sample);
        assert_eq!(counter.subscribe.sample_seconds, 10.);
        let branch = watch(&default_gnmi_draft(), find(&root, "statistics"));
        assert_eq!(branch.subscribe.mode, GnmiSubscriptionMode::Sample);
    }

    #[test]
    fn prepares_sets_and_deletes() {
        let root = tree();
        let set = set_value(&default_gnmi_draft(), find(&root, "mtu"));
        assert_eq!(set.operation, GnmiOperation::Set);
        assert_eq!(set.set[0].value, "9232");
        let statistics = set_value(&default_gnmi_draft(), find(&root, "statistics"));
        assert_eq!(statistics.set[0].value, "{\n  \"in-octets\": \"5\"\n}");
        let remove = gnmi_remove(&default_gnmi_draft(), find(&root, "mtu"));
        assert_eq!(remove.set[0].op, GnmiSetItemOp::Delete);
        assert_eq!(remove.set[0].path, "/interface[name=e1]/mtu");
        assert_eq!(
            copy_value(find(&root, "statistics")),
            "{\n  \"in-octets\": \"5\"\n}"
        );
        assert_eq!(copy_value(find(&root, "mtu")), "9232");
        let (path, what) = copy_path(true, &root, find(&root, "mtu"));
        assert_eq!(
            (path.as_str(), what),
            ("/interface[name=e1]/mtu", "the path")
        );
        let get = gnmi_get(&default_gnmi_draft(), find(&root, "statistics"));
        assert_eq!(get.paths, vec!["/interface[name=e1]/statistics"]);
        assert_eq!(writable_target(false), Datastore::Running);
    }
}
