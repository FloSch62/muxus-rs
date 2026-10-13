//! One tree for everything a device returns: gNMI notifications (paths plus
//! JSON values) and NETCONF `<data>` (XML) both land here, so the explorer,
//! the result views and "copy path" work the same for either protocol
//! (port of `client/src/management/data-tree.ts`).
//!
//! YANG list keys are not in the data itself. They are learned from keyed
//! paths the device sends (subscriptions, keyed requests) and otherwise
//! guessed from the entries' leaves.

use std::collections::{HashMap, HashSet};

use muxus_core::gnmi_path::{
    GnmiKeys, GnmiPath, GnmiPathElem, format_gnmi_elem, format_gnmi_path, strip_module_prefix,
};
use serde_json::{Map, Number, Value};

use super::xml::XmlElement;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataNodeKind {
    Root,
    Container,
    List,
    Entry,
    Leaf,
    LeafList,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DataNode {
    /// Unique within its tree: the node's path with module prefixes dropped.
    pub id: String,
    /// Element name as the device wrote it (module-qualified when it was).
    pub name: String,
    pub kind: DataNodeKind,
    pub path: GnmiPath,
    /// List entries: their key leaves.
    pub keys: Option<GnmiKeys>,
    /// The keys were guessed from the leaves rather than learned from the device.
    pub guessed_keys: Option<bool>,
    /// Leaves and leaf-lists.
    pub value: Option<Value>,
    /// XML namespace of the element (NETCONF).
    pub namespace: Option<String>,
    pub children: Vec<DataNode>,
    /// Explorer: children have been fetched from the device.
    pub loaded: bool,
}

impl DataNode {
    pub fn is_leaf(&self) -> bool {
        matches!(self.kind, DataNodeKind::Leaf | DataNodeKind::LeafList)
    }
}

/// The well-known key leaf names, in the order a guess prefers them.
pub const KEY_NAMES: [&str; 21] = [
    "name",
    "index",
    "id",
    "key",
    "sequence-id",
    "sequence",
    "ip-prefix",
    "prefix",
    "address",
    "ip-address",
    "neighbor-address",
    "peer-address",
    "interface-name",
    "interface",
    "vlan-id",
    "mac-address",
    "mac",
    "type",
    "priority",
    "number",
    "identifier",
];

/// Key leaf names per schema path (`/interface/subinterface`), learned from keyed paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyRegistry {
    keys: HashMap<String, Vec<String>>,
}

impl KeyRegistry {
    pub fn learn(&mut self, path: &GnmiPath) {
        let mut schema = String::new();
        for elem in &path.elems {
            schema.push('/');
            schema.push_str(strip_module_prefix(&elem.name));
            if let Some(keys) = elem.keys.as_ref().filter(|keys| !keys.is_empty()) {
                self.keys.insert(
                    schema.clone(),
                    keys.iter().map(|(key, _)| key.to_owned()).collect(),
                );
            }
        }
    }

    pub fn lookup(&self, schema_path: &str) -> Option<&[String]> {
        self.keys.get(schema_path).map(Vec::as_slice)
    }
}

fn is_plain_object(value: &Value) -> bool {
    value.is_object()
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

/// `String(number)` in JavaScript.
pub fn js_number_text(number: &Number) -> String {
    if let Some(integer) = number.as_i64() {
        return integer.to_string();
    }
    if let Some(integer) = number.as_u64() {
        return integer.to_string();
    }
    let value = number.as_f64().unwrap_or(f64::NAN);
    if value.fract() == 0.0 && value.abs() < 1e21 {
        return format!("{value:.0}");
    }
    let magnitude = value.abs();
    if magnitude >= 1e21 || (magnitude != 0.0 && magnitude < 1e-6) {
        let text = format!("{value:e}");
        return match text.split_once('e') {
            Some((mantissa, exponent)) if !exponent.starts_with('-') => {
                format!("{mantissa}e+{exponent}")
            }
            _ => text,
        };
    }
    value.to_string()
}

/// `String(value)` for what JSON holds; arrays join with commas.
pub fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(scalar_text).collect::<Vec<_>>().join(","),
        Value::Number(number) => js_number_text(number),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => "null".into(),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// Key leaves that tell `entries` apart, preferring well-known key names.
pub fn guess_list_keys(entries: &[&Map<String, Value>]) -> Vec<String> {
    let Some(first) = entries.first() else {
        return Vec::new();
    };
    let scalar_fields: Vec<&String> = first
        .keys()
        .filter(|field| {
            entries
                .iter()
                .all(|entry| entry.get(field.as_str()).is_some_and(is_scalar))
        })
        .collect();
    let unique = |fields: &[&String]| {
        let mut seen = HashSet::new();
        entries.iter().all(|entry| {
            let signature = fields
                .iter()
                .map(|field| {
                    entry
                        .get(field.as_str())
                        .map(scalar_text)
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("\u{0}");
            seen.insert(signature)
        })
    };
    let preference = |field: &str| {
        KEY_NAMES
            .iter()
            .position(|name| *name == strip_module_prefix(field))
            .unwrap_or(KEY_NAMES.len())
    };
    let mut preferred: Vec<&String> = scalar_fields
        .iter()
        .copied()
        .filter(|field| preference(field) < KEY_NAMES.len())
        .collect();
    preferred.sort_by_key(|field| preference(field));
    // The most key-like leaf is most likely (part of) the key: prefer it alone,
    // then paired with another, before settling for a less key-like leaf.
    if let Some((first, rest)) = preferred.split_first() {
        if unique(&[first]) {
            return vec![(*first).clone()];
        }
        for second in rest {
            if unique(&[first, second]) {
                return vec![(*first).clone(), (*second).clone()];
            }
        }
        for field in rest {
            if unique(&[field]) {
                return vec![(*field).clone()];
            }
        }
        for i in 0..rest.len() {
            for j in i + 1..rest.len() {
                if unique(&[rest[i], rest[j]]) {
                    return vec![rest[i].clone(), rest[j].clone()];
                }
            }
        }
    }
    for field in &scalar_fields {
        if unique(&[field]) {
            return vec![(*field).clone()];
        }
    }
    // A single entry, or no distinguishing leaf: take the most key-like one.
    if let Some(first) = preferred.first() {
        return vec![(*first).clone()];
    }
    scalar_fields
        .first()
        .map(|field| vec![(*field).clone()])
        .unwrap_or_default()
}

fn elem_identity(elem: &GnmiPathElem) -> String {
    format_gnmi_elem(&GnmiPathElem {
        name: strip_module_prefix(&elem.name).to_owned(),
        keys: elem.keys.clone(),
    })
}

fn module_of(name: &str) -> Option<&str> {
    match name.find(':') {
        Some(colon) if colon > 0 => Some(&name[..colon]),
        _ => None,
    }
}

/// The path element for a child the device named `name` below `parent`.
/// RFC 7951 qualifies every member at the top of a JSON value, but a path
/// needs the module only where it changes: SR OS rejects a qualified name
/// anywhere in a path, SR Linux takes either. The prefix stays on the first
/// element, which names the model, and where it differs from the module the
/// parent's path names; below a path that names none, it goes.
fn elem_name(parent: &DataNode, name: &str) -> String {
    let elems = &parent.path.elems;
    let Some(module) = module_of(name) else {
        return name.to_owned();
    };
    if elems.is_empty() {
        return name.to_owned();
    }
    for elem in elems.iter().rev() {
        if let Some(parent_module) = module_of(&elem.name) {
            return if parent_module == module {
                strip_module_prefix(name).to_owned()
            } else {
                name.to_owned()
            };
        }
    }
    strip_module_prefix(name).to_owned()
}

fn child_id(parent: &DataNode, elem: &GnmiPathElem) -> String {
    format!(
        "{}/{}",
        if parent.kind == DataNodeKind::Root {
            ""
        } else {
            &parent.id
        },
        elem_identity(elem)
    )
}

fn child_path(parent: &GnmiPath, elem: GnmiPathElem) -> GnmiPath {
    let mut path = parent.clone();
    path.elems.push(elem);
    path
}

pub fn create_root(path: GnmiPath) -> DataNode {
    DataNode {
        id: String::new(),
        name: String::new(),
        kind: DataNodeKind::Root,
        path,
        keys: None,
        guessed_keys: None,
        value: None,
        namespace: None,
        children: Vec::new(),
        loaded: false,
    }
}

fn new_node(id: String, name: &str, kind: DataNodeKind, path: GnmiPath) -> DataNode {
    DataNode {
        id,
        name: name.to_owned(),
        kind,
        path,
        keys: None,
        guessed_keys: None,
        value: None,
        namespace: None,
        children: Vec::new(),
        loaded: false,
    }
}

fn find_child_index(parent: &DataNode, id: &str) -> Option<usize> {
    parent.children.iter().position(|child| child.id == id)
}

fn add_child(parent: &mut DataNode, child: DataNode) -> usize {
    parent.children.push(child);
    parent.children.len() - 1
}

/// The list node `name` under `parent`, created on first use.
fn list_node(parent: &mut DataNode, name: &str) -> usize {
    let elem = GnmiPathElem::new(elem_name(parent, name));
    let id = child_id(parent, &elem);
    if let Some(index) = find_child_index(parent, &id) {
        parent.children[index].kind = DataNodeKind::List;
        return index;
    }
    let path = child_path(&parent.path, elem);
    add_child(parent, new_node(id, name, DataNodeKind::List, path))
}

fn entry_node(list: &mut DataNode, keys: GnmiKeys, guessed: bool) -> usize {
    let mut parent_path = list.path.clone();
    let last = parent_path
        .elems
        .pop()
        .map(|elem| elem.name)
        .unwrap_or_default();
    let elem = GnmiPathElem::with_keys(last, keys.clone());
    let base = list.id.rfind('/').map_or("", |slash| &list.id[..slash]);
    let id = format!("{base}/{}", elem_identity(&elem));
    if let Some(index) = find_child_index(list, &id) {
        if !guessed {
            list.children[index].guessed_keys = Some(false);
        }
        return index;
    }
    let mut entry = new_node(
        id,
        &list.name.clone(),
        DataNodeKind::Entry,
        child_path(&parent_path, elem),
    );
    entry.keys = Some(keys);
    if guessed {
        entry.guessed_keys = Some(true);
    }
    add_child(list, entry)
}

fn container_node(parent: &mut DataNode, name: &str) -> usize {
    let elem = GnmiPathElem::new(elem_name(parent, name));
    let id = child_id(parent, &elem);
    if let Some(index) = find_child_index(parent, &id) {
        return index;
    }
    let path = child_path(&parent.path, elem);
    add_child(parent, new_node(id, name, DataNodeKind::Container, path))
}

/// The schema node a data node stands for: its path with the keys dropped.
pub fn schema_path_of(node: &DataNode) -> String {
    format!(
        "/{}",
        node.path
            .elems
            .iter()
            .map(|elem| strip_module_prefix(&elem.name))
            .collect::<Vec<_>>()
            .join("/")
    )
}

/// Walk (and create) the nodes for `path` below `root`.
pub fn node_at<'a>(
    root: &'a mut DataNode,
    path: &GnmiPath,
    registry: Option<&mut KeyRegistry>,
) -> &'a mut DataNode {
    if let Some(registry) = registry {
        registry.learn(path);
    }
    let skip = root.path.elems.len();
    let mut node = root;
    for elem in path.elems.iter().skip(skip) {
        match elem.keys.as_ref().filter(|keys| !keys.is_empty()) {
            Some(keys) => {
                let list = list_node(node, &elem.name);
                let list = &mut node.children[list];
                let entry = entry_node(list, keys.clone(), false);
                node = &mut list.children[entry];
            }
            None => {
                let index = container_node(node, &elem.name);
                node = &mut node.children[index];
            }
        }
    }
    node
}

fn set_leaf(parent: &mut DataNode, name: &str, value: Value) {
    let elem = GnmiPathElem::new(elem_name(parent, name));
    let id = child_id(parent, &elem);
    let kind = if value.is_array() {
        DataNodeKind::LeafList
    } else {
        DataNodeKind::Leaf
    };
    if let Some(index) = find_child_index(parent, &id) {
        let existing = &mut parent.children[index];
        existing.kind = kind;
        existing.value = Some(value);
        existing.children = Vec::new();
        return;
    }
    let path = child_path(&parent.path, elem);
    let mut leaf = new_node(id, name, kind, path);
    leaf.value = Some(value);
    add_child(parent, leaf);
}

/// Merge a JSON value (RFC 7951 or plain JSON) into `node`.
pub fn merge_json(node: &mut DataNode, value: &Value, mut registry: Option<&mut KeyRegistry>) {
    if node.kind == DataNodeKind::List
        && let Value::Array(entries) = value
    {
        merge_list_entries(node, entries, registry);
        return;
    }
    let Value::Object(object) = value else {
        // A scalar at a path: the node itself is a leaf.
        node.kind = if value.is_array() {
            DataNodeKind::LeafList
        } else {
            DataNodeKind::Leaf
        };
        node.value = Some(value.clone());
        return;
    };
    if node.is_leaf() {
        node.kind = DataNodeKind::Container;
        node.value = None;
    }
    for (name, child) in object {
        match child {
            Value::Array(items) if !items.is_empty() && items.iter().all(is_plain_object) => {
                let list = list_node(node, name);
                merge_list_entries(&mut node.children[list], items, registry.as_deref_mut());
            }
            Value::Object(_) => {
                let index = container_node(node, name);
                merge_json(&mut node.children[index], child, registry.as_deref_mut());
            }
            _ => set_leaf(node, name, child.clone()),
        }
    }
}

fn merge_list_entries(
    list: &mut DataNode,
    entries: &[Value],
    mut registry: Option<&mut KeyRegistry>,
) {
    let objects: Vec<&Map<String, Value>> = entries.iter().filter_map(Value::as_object).collect();
    let learned = registry
        .as_deref()
        .and_then(|registry| registry.lookup(&schema_path_of(list)))
        .map(<[String]>::to_vec);
    let guessed = learned.is_none();
    let key_names = learned.unwrap_or_else(|| guess_list_keys(&objects));
    for entry in objects {
        let mut keys = GnmiKeys::default();
        for key in &key_names {
            // JSON may qualify the key leaf with its module; the path never does.
            let field = if entry.contains_key(key) {
                Some(key.as_str())
            } else {
                entry
                    .keys()
                    .find(|name| strip_module_prefix(name) == key)
                    .map(String::as_str)
            };
            keys.insert(
                strip_module_prefix(key),
                field
                    .and_then(|field| entry.get(field))
                    .map(scalar_text)
                    .unwrap_or_default(),
            );
        }
        let index = entry_node(list, keys, guessed);
        merge_json(
            &mut list.children[index],
            &Value::Object(entry.clone()),
            registry.as_deref_mut(),
        );
    }
}

/// Sort for display: leaves first (they describe the node), then containers and lists by name.
pub fn display_children(node: &DataNode) -> Vec<&DataNode> {
    if node.kind == DataNodeKind::List {
        return node.children.iter().collect();
    }
    let leaves = node.children.iter().filter(|child| child.is_leaf());
    let branches = node.children.iter().filter(|child| !child.is_leaf());
    leaves.chain(branches).collect()
}

pub fn node_label(node: &DataNode) -> String {
    if node.kind == DataNodeKind::Entry {
        let values: Vec<&str> = node
            .keys
            .iter()
            .flat_map(|keys| keys.iter())
            .map(|(_, value)| value)
            .collect();
        return if values.is_empty() {
            strip_module_prefix(&node.name).to_owned()
        } else {
            values.join(" · ")
        };
    }
    let label = strip_module_prefix(&node.name);
    if label.is_empty() {
        "/".into()
    } else {
        label.to_owned()
    }
}

pub fn node_path_text(node: &DataNode) -> String {
    format_gnmi_path(&node.path)
}

/// A leaf of a tree, for the table view.
#[derive(Debug, Clone, PartialEq)]
pub struct LeafRow {
    pub path: String,
    pub node_id: String,
    pub value: Option<Value>,
}

pub fn collect_leaves(node: &DataNode, limit: usize) -> Vec<LeafRow> {
    let mut out = Vec::new();
    collect_leaves_into(node, &mut out, limit);
    out
}

fn collect_leaves_into(node: &DataNode, out: &mut Vec<LeafRow>, limit: usize) {
    if out.len() >= limit {
        return;
    }
    if node.is_leaf() {
        out.push(LeafRow {
            path: node_path_text(node),
            node_id: node.id.clone(),
            value: node.value.clone(),
        });
        return;
    }
    for child in display_children(node) {
        collect_leaves_into(child, out, limit);
        if out.len() >= limit {
            break;
        }
    }
}

pub fn count_leaves(node: &DataNode) -> usize {
    if node.is_leaf() {
        return 1;
    }
    node.children.iter().map(count_leaves).sum()
}

/// The tree as plain JSON, for the raw view and the clipboard.
pub fn node_to_json(node: &DataNode) -> Value {
    match node.kind {
        DataNodeKind::Leaf | DataNodeKind::LeafList => node.value.clone().unwrap_or(Value::Null),
        DataNodeKind::List => Value::Array(node.children.iter().map(node_to_json).collect()),
        _ => Value::Object(
            node.children
                .iter()
                .map(|child| (child.name.clone(), node_to_json(child)))
                .collect(),
        ),
    }
}

pub fn find_node<'a>(root: &'a DataNode, id: &str) -> Option<&'a DataNode> {
    if root.id == id {
        return Some(root);
    }
    root.children
        .iter()
        .filter(|child| descends(child, id))
        .find_map(|child| find_node(child, id))
}

pub fn find_node_mut<'a>(root: &'a mut DataNode, id: &str) -> Option<&'a mut DataNode> {
    if root.id == id {
        return Some(root);
    }
    let index = root
        .children
        .iter()
        .position(|child| descends(child, id) && find_node(child, id).is_some())?;
    find_node_mut(&mut root.children[index], id)
}

fn descends(child: &DataNode, id: &str) -> bool {
    id == child.id
        || (id.starts_with(&child.id) && id[child.id.len()..].starts_with('/'))
        || child.kind == DataNodeKind::List
}

// XML (NETCONF)

/// Leaves of an XML list entry that most likely form its key.
fn guess_element_keys(siblings: &[&XmlElement]) -> Vec<String> {
    let entries: Vec<Map<String, Value>> = siblings
        .iter()
        .map(|sibling| {
            sibling
                .elements()
                .filter(|child| !child.has_elements())
                .map(|child| {
                    (
                        child.local_name().to_owned(),
                        Value::String(child.text_content().trim().to_owned()),
                    )
                })
                .collect()
        })
        .collect();
    let refs: Vec<&Map<String, Value>> = entries.iter().collect();
    guess_list_keys(&refs)
}

fn text_of(element: &XmlElement) -> Value {
    Value::String(muxus_core::js::js_trim(&element.text_content()).to_owned())
}

/// Merge the element children of `container` (an XML element such as
/// `<data>`) into `node`. Repeated siblings become list entries; a lone
/// element with a well-known key leaf (`name`, `index`, …) is treated as a
/// list entry too, which is what it almost always is.
pub fn merge_xml(node: &mut DataNode, container: &XmlElement) {
    let mut groups: Vec<(String, Vec<&XmlElement>)> = Vec::new();
    for child in container.elements() {
        let key = format!(
            "{} {}",
            child.namespace.as_deref().unwrap_or_default(),
            child.local_name()
        );
        match groups.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, group)) => group.push(child),
            None => groups.push((key, vec![child])),
        }
    }
    for (_, elements) in groups {
        let first = elements[0];
        let name = first.local_name();
        if elements.len() == 1 && !first.has_elements() {
            set_leaf(node, name, text_of(first));
            tag_namespace(node, name, first.namespace.as_deref());
            continue;
        }
        if elements.iter().all(|element| !element.has_elements()) {
            set_leaf(
                node,
                name,
                Value::Array(elements.iter().map(|element| text_of(element)).collect()),
            );
            tag_namespace(node, name, first.namespace.as_deref());
            continue;
        }
        let key_names = guess_element_keys(&elements);
        let looks_like_entry = elements.len() > 1
            || key_names.iter().any(|key| {
                KEY_NAMES[..6].contains(&key.as_str()) && first.count_descendants_named(key) > 0
            });
        if !looks_like_entry {
            let index = container_node(node, name);
            let child = &mut node.children[index];
            if child.namespace.is_none() {
                child.namespace = first.namespace.clone();
            }
            merge_xml(child, first);
            continue;
        }
        let list_index = list_node(node, name);
        let list = &mut node.children[list_index];
        if list.namespace.is_none() {
            list.namespace = first.namespace.clone();
        }
        for element in elements {
            let mut keys = GnmiKeys::default();
            for key in &key_names {
                let leaf = element
                    .elements()
                    .find(|child| child.local_name() == key && !child.has_elements());
                keys.insert(
                    key.as_str(),
                    leaf.map(|leaf| leaf.text_content().trim().to_owned())
                        .unwrap_or_default(),
                );
            }
            let index = entry_node(list, keys, true);
            let entry = &mut list.children[index];
            if entry.namespace.is_none() {
                entry.namespace = element.namespace.clone();
            }
            merge_xml(entry, element);
        }
    }
}

fn tag_namespace(parent: &mut DataNode, name: &str, namespace: Option<&str>) {
    let Some(namespace) = namespace else {
        return;
    };
    let id = child_id(parent, &GnmiPathElem::new(name));
    if let Some(index) = find_child_index(parent, &id)
        && parent.children[index].namespace.is_none()
    {
        parent.children[index].namespace = Some(namespace.to_owned());
    }
}

/// The nodes from the top of `root` down to the node with id `target`.
pub fn namespace_chain<'a>(root: &'a DataNode, target: &str) -> Vec<&'a DataNode> {
    fn visit<'a>(node: &'a DataNode, target: &str, chain: &mut Vec<&'a DataNode>) -> bool {
        for child in &node.children {
            chain.push(child);
            if child.id == target || visit(child, target, chain) {
                return true;
            }
            chain.pop();
        }
        false
    }
    let mut chain = Vec::new();
    if root.id != target {
        visit(root, target, &mut chain);
    }
    chain
}

#[cfg(test)]
mod tests {
    use muxus_core::gnmi_path::parse_gnmi_path;
    use serde_json::json;

    use super::*;
    use crate::workbench::xml::{
        EditMode, edit_config_for, parse_document, subtree_filter_for, xpath_for,
    };

    fn find<'a>(root: &'a DataNode, label: &str) -> &'a DataNode {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node_label(node) == label {
                return node;
            }
            stack.extend(node.children.iter());
        }
        panic!("no node {label}");
    }

    fn leaf_paths(node: &DataNode) -> Vec<String> {
        collect_leaves(node, usize::MAX)
            .into_iter()
            .map(|leaf| leaf.path)
            .collect()
    }

    #[test]
    fn builds_lists_from_json_with_guessed_keys_and_from_keyed_paths_with_learned_keys() {
        let mut registry = KeyRegistry::default();
        let mut root = create_root(GnmiPath::default());
        merge_json(
            &mut root,
            &json!({
                "srl_nokia-interfaces:interface": [
                    {"name": "ethernet-1/1", "admin-state": "enable", "statistics": {"in-octets": "10"}},
                    {"name": "mgmt0", "admin-state": "enable"}
                ]
            }),
            Some(&mut registry),
        );
        let entry = find(&root, "ethernet-1/1");
        assert_eq!(entry.kind, DataNodeKind::Entry);
        assert_eq!(entry.guessed_keys, Some(true));
        assert_eq!(
            entry.path.elems[0],
            GnmiPathElem::with_keys(
                "srl_nokia-interfaces:interface",
                [("name", "ethernet-1/1")].into_iter().collect()
            )
        );
        // The same entry addressed by a path from the device lines up, now with known keys.
        let statistics = node_at(
            &mut root,
            &parse_gnmi_path("/interface[name=ethernet-1/1]/statistics").unwrap(),
            Some(&mut registry),
        );
        merge_json(statistics, &json!({"out-octets": "5"}), Some(&mut registry));
        let entry = find(&root, "ethernet-1/1");
        assert_eq!(
            leaf_paths(entry),
            vec![
                "/srl_nokia-interfaces:interface[name=ethernet-1/1]/name",
                "/srl_nokia-interfaces:interface[name=ethernet-1/1]/admin-state",
                "/srl_nokia-interfaces:interface[name=ethernet-1/1]/statistics/in-octets",
                "/srl_nokia-interfaces:interface[name=ethernet-1/1]/statistics/out-octets",
            ]
        );
        assert_eq!(entry.guessed_keys, Some(false));
        assert_eq!(
            registry.lookup("/interface"),
            Some(&["name".to_owned()][..])
        );
        let json = node_to_json(&root);
        assert_eq!(
            json["srl_nokia-interfaces:interface"][0]["name"],
            "ethernet-1/1"
        );
        assert_eq!(json["srl_nokia-interfaces:interface"][1]["name"], "mgmt0");
    }

    #[test]
    fn names_a_module_in_paths_only_where_it_changes() {
        // SR OS writes its paths bare and rejects a module name anywhere in one.
        let mut sros = create_root(GnmiPath::default());
        merge_json(
            node_at(&mut sros, &parse_gnmi_path("/state").unwrap(), None),
            &json!({"nokia-state:system": {"oper-name": "sr1"}, "nokia-li-state:li": {"mirror-dest": 0}}),
            None,
        );
        assert_eq!(
            leaf_paths(&sros),
            vec!["/state/system/oper-name", "/state/li/mirror-dest"]
        );
        // SR Linux names the module; an augmenting one keeps its prefix, the same one goes.
        let mut srl = create_root(GnmiPath::default());
        merge_json(
            node_at(
                &mut srl,
                &parse_gnmi_path("/srl_nokia-system:system").unwrap(),
                None,
            ),
            &json!({"srl_nokia-system-name:name": {"host-name": "srl1"}, "srl_nokia-system:features": ["a"]}),
            None,
        );
        assert_eq!(
            leaf_paths(&srl),
            vec![
                "/srl_nokia-system:system/features",
                "/srl_nokia-system:system/srl_nokia-system-name:name/host-name",
            ]
        );
    }

    #[test]
    fn guesses_compound_keys_when_one_leaf_is_not_unique() {
        let a = json!({"name": "cpm", "type": "ipv4", "entries": 3});
        let b = json!({"name": "cpm", "type": "ipv6", "entries": 3});
        assert_eq!(
            guess_list_keys(&[a.as_object().unwrap(), b.as_object().unwrap()]),
            vec!["name", "type"]
        );
        let a = json!({"sequence-id": 10, "action": "accept"});
        let b = json!({"sequence-id": 20, "action": "accept"});
        assert_eq!(
            guess_list_keys(&[a.as_object().unwrap(), b.as_object().unwrap()]),
            vec!["sequence-id"]
        );
    }

    #[test]
    fn turns_netconf_data_into_the_same_tree_and_writes_filters_that_select_a_node() {
        let document = parse_document(
            "<data xmlns=\"urn:ietf:params:xml:ns:netconf:base:1.0\">\
             <interface xmlns=\"urn:srl:if\"><name>ethernet-1/1</name><description>up</description>\
             <subinterface><index>0</index><vlan xmlns=\"urn:srl:vlan\"><id>10</id></vlan></subinterface></interface>\
             <interface xmlns=\"urn:srl:if\"><name>mgmt0</name></interface>\
             <system xmlns=\"urn:srl:sys\"><dns><server>1.1.1.1</server><server>8.8.8.8</server></dns></system>\
             </data>",
        )
        .unwrap();
        let mut root = create_root(GnmiPath::default());
        merge_xml(&mut root, &document.root);
        let subinterface = find(&root, "0").id.clone();
        let chain = namespace_chain(&root, &subinterface);
        assert_eq!(
            subtree_filter_for(&chain, ""),
            "<interface xmlns=\"urn:srl:if\"><name>ethernet-1/1</name><subinterface><index>0</index></subinterface></interface>"
        );
        assert_eq!(
            xpath_for(&chain),
            "/interface[name='ethernet-1/1']/subinterface[index='0']"
        );
        let vlan = find(&root, "vlan").id.clone();
        assert!(
            subtree_filter_for(&namespace_chain(&root, &vlan), "")
                .contains("<vlan xmlns=\"urn:srl:vlan\"/>")
        );
        assert_eq!(find(&root, "server").kind, DataNodeKind::LeafList);
        let mgmt0 = find(&root, "mgmt0").id.clone();
        assert!(
            edit_config_for(&namespace_chain(&root, &mgmt0), EditMode::Delete)
                .contains("nc:operation=\"delete\"")
        );
        let description = find(&root, "description").id.clone();
        let edit = edit_config_for(&namespace_chain(&root, &description), EditMode::Edit);
        assert!(edit.contains("<description>up</description>"), "{edit}");
        assert!(edit.contains("<name>ethernet-1/1</name>"), "{edit}");
    }

    #[test]
    fn finds_nodes_by_id_through_lists() {
        let mut root = create_root(GnmiPath::default());
        merge_json(
            &mut root,
            &json!({"interface": [{"name": "e1", "mtu": 1500}]}),
            None,
        );
        let id = find(&root, "mtu").id.clone();
        assert_eq!(id, "/interface[name=e1]/mtu");
        assert_eq!(find_node(&root, &id).unwrap().value, Some(json!(1500)));
        find_node_mut(&mut root, &id).unwrap().loaded = true;
        assert!(find_node(&root, &id).unwrap().loaded);
        assert_eq!(count_leaves(&root), 2);
        assert_eq!(js_number_text(&Number::from_f64(1e21).unwrap()), "1e+21");
        assert_eq!(js_number_text(&Number::from_f64(0.5).unwrap()), "0.5");
    }
}
