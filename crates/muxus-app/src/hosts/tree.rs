//! Folders are not a separate record type: a host's Muxus group string *is* its
//! folder path, with `/` between levels. Nesting therefore needs no schema
//! change, no new endpoint, and no migration: `Production/EU/Edge` is just a
//! group name that this module knows how to read as three levels.
//!
//! Port of `client/src/host-tree.ts`. The TypeScript tree shares node objects
//! between `roots` and `foldersByKey`; here folders live in one arena
//! (`HostTree::folders`) and both refer to them by index.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::hosts::managed::{ManagedHost, ManagedHostGroup, known_host_groups, managed_host_key};
use crate::hosts::organization::GroupKind;
use crate::model::{SavedHostProfile, SshHostEntry};
use crate::util::locale_compare;
use muxus_core::folder_paths;

pub const FOLDER_SEPARATOR: &str = "/";

/// Deeper than this and the path stops being a useful organizational tool.
pub const MAX_FOLDER_DEPTH: usize = 8;

/// Mirrors the server's `group` cap so the UI can refuse before the round trip.
pub const MAX_GROUP_PATH: usize = 300;

/// How far rows keep indenting; deeper folders render at this level.
pub const MAX_INDENT_DEPTH: usize = 5;

/// Stands in for "no parent" when keying the top level's manual order.
pub const TREE_ROOT_KEY: &str = "root";

/// Index of a folder in `HostTree::folders`.
pub type FolderId = usize;

#[derive(Clone, Debug, PartialEq)]
pub struct FolderNode {
    /// `folder:production/eu`: lowercased, so casing edits keep their identity.
    pub key: String,
    /// Canonical display path, e.g. `Production/EU`.
    pub path: String,
    /// Last segment only.
    pub label: String,
    pub depth: usize,
    pub parent_key: Option<String>,
    pub children: Vec<TreeChild>,
    pub descendant_host_count: usize,
    /// No host anywhere below: the folder exists only because prefs remember it.
    pub empty: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostNode {
    pub key: String,
    pub host: ManagedHost,
    pub depth: usize,
    pub parent_key: String,
}

/// A group that came from an ssh_config file rather than a Muxus group. These
/// stay flat and read-only: their membership is decided by the config file, not
/// by anything the user can drag. Always at depth 0.
#[derive(Clone, Debug, PartialEq)]
pub struct FileGroupNode {
    pub key: String,
    pub label: String,
    pub tooltip: Option<String>,
    pub children: Vec<HostNode>,
    pub descendant_host_count: usize,
}

/// A folder's child: a nested folder or a host.
#[derive(Clone, Debug, PartialEq)]
pub enum TreeChild {
    Folder(FolderId),
    Host(HostNode),
}

/// A node that holds hosts: a folder or an ssh_config file group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerRef {
    Folder(FolderId),
    /// Index into `HostTree::file_groups`.
    File(usize),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostTree {
    /// Custom folders first, ssh_config file groups pinned last.
    pub roots: Vec<ContainerRef>,
    pub folders: Vec<FolderNode>,
    pub file_groups: Vec<FileGroupNode>,
    pub folders_by_key: HashMap<String, FolderId>,
    /// Every host in the tree, in render order.
    pub hosts: Vec<ManagedHost>,
}

impl HostTree {
    pub fn folder(&self, id: FolderId) -> &FolderNode {
        &self.folders[id]
    }

    pub fn folder_by_key(&self, key: &str) -> Option<&FolderNode> {
        self.folders_by_key.get(key).map(|&id| &self.folders[id])
    }

    /// A folder (by key) or a top-level file group (by key).
    pub fn find_container(&self, key: &str) -> Option<ContainerRef> {
        if let Some(&id) = self.folders_by_key.get(key) {
            return Some(ContainerRef::Folder(id));
        }
        self.roots
            .iter()
            .copied()
            .find(|root| matches!(root, ContainerRef::File(_)) && self.container_key(*root) == key)
    }

    pub fn container_key(&self, container: ContainerRef) -> &str {
        match container {
            ContainerRef::Folder(id) => &self.folders[id].key,
            ContainerRef::File(index) => &self.file_groups[index].key,
        }
    }

    pub fn container_label(&self, container: ContainerRef) -> &str {
        match container {
            ContainerRef::Folder(id) => &self.folders[id].label,
            ContainerRef::File(index) => &self.file_groups[index].label,
        }
    }

    pub fn descendant_host_count(&self, container: ContainerRef) -> usize {
        match container {
            ContainerRef::Folder(id) => self.folders[id].descendant_host_count,
            ContainerRef::File(index) => self.file_groups[index].descendant_host_count,
        }
    }
}

/// Segments of a group string: `/` separates them, doubled separators
/// collapse, and each segment is trimmed (`muxus_core::folder_paths`, shared
/// with the backend and the command-line launch).
pub fn folder_segments(group: Option<&str>) -> Vec<String> {
    folder_paths::folder_path_segments(group.unwrap_or(""))
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn segments(path: &str) -> Vec<String> {
    folder_segments(Some(path))
}

/// The canonical stored form of a group string; '' means "no folder".
pub fn normalize_group_path(group: Option<&str>) -> String {
    folder_paths::normalize_folder_path(group.unwrap_or(""))
}

/// A folder name is a single segment, so the separator can never appear in one.
pub fn sanitize_folder_name(name: &str) -> String {
    name.split(FOLDER_SEPARATOR)
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Identity of a folder, case-insensitive per segment: the same rule the flat
/// groups already used, applied at every level.
pub fn folder_key(path: &str) -> String {
    folder_paths::sidebar_folder_key(path)
}

pub fn folder_path(segments: &[String]) -> String {
    segments.join(FOLDER_SEPARATOR)
}

/// '' for a top-level folder.
pub fn folder_parent_path(path: &str) -> String {
    folder_paths::folder_parent_path(path)
}

pub fn folder_label(path: &str) -> String {
    folder_paths::folder_label(path)
}

pub fn folder_depth(path: &str) -> usize {
    folder_paths::folder_depth(path)
}

/// Segment-aware ancestry. `Production` is not inside `Prod`: comparing the raw
/// strings with starts_with would say otherwise and quietly move the wrong hosts.
pub fn is_descendant_path(candidate: &str, ancestor: &str) -> bool {
    folder_paths::is_descendant_folder_path(candidate, ancestor)
}

pub fn is_same_path(a: &str, b: &str) -> bool {
    folder_key(a) == folder_key(b)
}

/// The path `host_path` ends up at when the folder `from` is renamed to `to`.
pub fn rename_folder_path(host_path: &str, from: &str, to: &str) -> Option<String> {
    folder_paths::rename_folder_path_under(host_path, from, to)
}

/// Re-parenting a folder is a rename to `<newParent>/<its own name>`.
pub fn move_folder_path(path: &str, new_parent_path: &str) -> String {
    let mut out = segments(new_parent_path);
    out.push(folder_label(path));
    folder_path(&out)
}

/// Every ancestor of a path, outermost first, excluding the path itself.
pub fn ancestor_paths(path: &str) -> Vec<String> {
    let segments = segments(path);
    (1..segments.len())
        .map(|end| folder_path(&segments[..end]))
        .collect()
}

/// Options for `build_host_tree`.
#[derive(Default)]
pub struct BuildOptions<'a> {
    /// Folders that exist only in preferences (they hold no host yet).
    pub known_folders: &'a [String],
    /// Manual sibling order for a parent, innermost-first by folder key.
    pub folder_order: Option<FolderOrderLookup<'a>>,
}

/// Manual sibling order stored under a parent key (`FolderPrefs::folder_order`).
pub type FolderOrderLookup<'a> = &'a dyn Fn(&str) -> Option<Vec<String>>;

/// Re-parent the flat groups `group_managed_hosts` produced into a folder tree.
///
/// Only `custom` groups nest; ssh_config file groups keep their existing flat,
/// read-only shape and are pinned below every folder. Intermediate folders are
/// synthesized, so a lone `A/B/C` still yields navigable `A` and `A/B` levels.
pub fn build_host_tree(groups: &[ManagedHostGroup], options: &BuildOptions<'_>) -> HostTree {
    let mut tree = HostTree::default();
    let mut root_folders: Vec<FolderId> = Vec::new();

    // Create the folder and every missing ancestor; first writer fixes casing.
    let mut ensure_folder = |tree: &mut HostTree, path: &str| -> Option<FolderId> {
        let segments = segments(path);
        let mut parent: Option<FolderId> = None;
        for depth in 0..segments.len() {
            let current_path = folder_path(&segments[..=depth]);
            let key = folder_key(&current_path);
            let id = match tree.folders_by_key.get(&key) {
                Some(&id) => id,
                None => {
                    let path = match parent {
                        Some(parent) => format!(
                            "{}{FOLDER_SEPARATOR}{}",
                            tree.folders[parent].path, segments[depth]
                        ),
                        None => segments[depth].clone(),
                    };
                    let id = tree.folders.len();
                    tree.folders.push(FolderNode {
                        key: key.clone(),
                        path,
                        label: segments[depth].clone(),
                        depth,
                        parent_key: parent.map(|parent| tree.folders[parent].key.clone()),
                        children: Vec::new(),
                        descendant_host_count: 0,
                        empty: true,
                    });
                    tree.folders_by_key.insert(key, id);
                    match parent {
                        Some(parent) => tree.folders[parent].children.push(TreeChild::Folder(id)),
                        None => root_folders.push(id),
                    }
                    id
                }
            };
            parent = Some(id);
        }
        parent
    };

    // Folders are created in sorted path order so the label a case-insensitive
    // collision settles on never depends on the order hosts arrived in.
    let custom_groups: Vec<&ManagedHostGroup> = groups
        .iter()
        .filter(|group| group.kind == GroupKind::Custom)
        .collect();
    let mut declared: Vec<String> = custom_groups
        .iter()
        .map(|group| group.label.as_str())
        .chain(options.known_folders.iter().map(String::as_str))
        .map(|path| normalize_group_path(Some(path)))
        .filter(|path| !path.is_empty())
        .collect();
    declared.sort_by(|a, b| locale_compare(a, b));
    for path in &declared {
        ensure_folder(&mut tree, path);
    }

    for group in &custom_groups {
        let Some(folder) = ensure_folder(&mut tree, &group.label) else {
            continue;
        };
        let (depth, key) = (tree.folders[folder].depth, tree.folders[folder].key.clone());
        for host in &group.hosts {
            tree.folders[folder]
                .children
                .push(TreeChild::Host(HostNode {
                    key: managed_host_key(host),
                    host: host.clone(),
                    depth: depth + 1,
                    parent_key: key.clone(),
                }));
        }
    }

    tree.file_groups = groups
        .iter()
        .filter(|group| group.kind == GroupKind::File)
        .map(|group| FileGroupNode {
            key: group.key.clone(),
            label: group.label.clone(),
            tooltip: group.tooltip.clone(),
            descendant_host_count: group.hosts.len(),
            children: group
                .hosts
                .iter()
                .map(|host| HostNode {
                    key: managed_host_key(host),
                    host: host.clone(),
                    depth: 1,
                    parent_key: group.key.clone(),
                })
                .collect(),
        })
        .collect();

    let rank_in = |parent_key: &str| -> Option<HashMap<String, usize>> {
        let order = options.folder_order.and_then(|lookup| lookup(parent_key))?;
        if order.is_empty() {
            return None;
        }
        Some(
            order
                .into_iter()
                .enumerate()
                .map(|(index, key)| (key, index))
                .collect(),
        )
    };
    for &folder in &root_folders {
        finalize_folder(&mut tree, folder, &rank_in);
    }
    sort_folders(&tree, &mut root_folders, rank_in(TREE_ROOT_KEY).as_ref());

    tree.roots = root_folders
        .into_iter()
        .map(ContainerRef::Folder)
        .chain((0..tree.file_groups.len()).map(ContainerRef::File))
        .collect();
    let mut hosts = Vec::new();
    for &root in &tree.roots {
        collect_container_hosts(&tree, root, &mut hosts);
    }
    tree.hosts = hosts;
    tree
}

fn collect_container_hosts(tree: &HostTree, container: ContainerRef, out: &mut Vec<ManagedHost>) {
    match container {
        ContainerRef::Folder(id) => {
            for child in &tree.folders[id].children {
                match child {
                    TreeChild::Host(node) => out.push(node.host.clone()),
                    TreeChild::Folder(child) => {
                        collect_container_hosts(tree, ContainerRef::Folder(*child), out)
                    }
                }
            }
        }
        ContainerRef::File(index) => out.extend(
            tree.file_groups[index]
                .children
                .iter()
                .map(|n| n.host.clone()),
        ),
    }
}

/// Every host below a container, in render order (`collectHosts` in SessionSidebar).
pub fn collect_hosts(tree: &HostTree, container: ContainerRef) -> Vec<ManagedHost> {
    let mut out = Vec::new();
    collect_container_hosts(tree, container, &mut out);
    out
}

type RankLookup<'a> = dyn Fn(&str) -> Option<HashMap<String, usize>> + 'a;

/// Sort children and roll host counts up, depth-first.
fn finalize_folder(tree: &mut HostTree, folder: FolderId, rank_in: &RankLookup<'_>) -> usize {
    let children = std::mem::take(&mut tree.folders[folder].children);
    let mut count = 0;
    let mut folders = Vec::new();
    let mut rest = Vec::new();
    for child in children {
        match child {
            TreeChild::Folder(id) => {
                count += finalize_folder(tree, id, rank_in);
                folders.push(id);
            }
            TreeChild::Host(node) => {
                count += 1;
                rest.push(TreeChild::Host(node));
            }
        }
    }
    // Folders sort before hosts; hosts keep the order group_managed_hosts settled
    // (sortOrder → name), so the sortOrder column keeps working.
    let rank = rank_in(&tree.folders[folder].key);
    sort_folders(tree, &mut folders, rank.as_ref());
    let node = &mut tree.folders[folder];
    node.children = folders
        .into_iter()
        .map(TreeChild::Folder)
        .chain(rest)
        .collect();
    node.descendant_host_count = count;
    node.empty = count == 0;
    count
}

/// Manual order first, then alphabetical for anything the user has not placed:
/// the same rule hosts follow with `sortOrder`, so a folder a user dragged stays
/// put while a newly created sibling still lands somewhere predictable.
fn sort_folders(tree: &HostTree, folders: &mut [FolderId], rank: Option<&HashMap<String, usize>>) {
    folders.sort_by(|&a, &b| {
        let rank_of = |id: FolderId| {
            rank.and_then(|r| r.get(&tree.folders[id].key).copied())
                .unwrap_or(usize::MAX)
        };
        rank_of(a)
            .cmp(&rank_of(b))
            .then_with(|| locale_compare(&tree.folders[a].label, &tree.folders[b].label))
    });
}

/// What a visible row draws: self-contained, so a row can render without the tree.
#[derive(Clone, Debug, PartialEq)]
pub enum RowNode {
    Host {
        host: ManagedHost,
        parent_key: String,
    },
    Folder {
        id: FolderId,
        path: String,
        label: String,
        parent_key: Option<String>,
        descendant_host_count: usize,
    },
    /// An ssh_config file group.
    File {
        index: usize,
        label: String,
        tooltip: Option<String>,
        descendant_host_count: usize,
    },
}

impl RowNode {
    pub fn is_host(&self) -> bool {
        matches!(self, RowNode::Host { .. })
    }

    pub fn is_folder(&self) -> bool {
        matches!(self, RowNode::Folder { .. })
    }

    /// The label a row shows: a host's display name, a folder's last segment.
    pub fn label(&self) -> &str {
        match self {
            RowNode::Host { host, .. } => host.display_name(),
            RowNode::Folder { label, .. } | RowNode::File { label, .. } => label,
        }
    }

    /// The container this row stands for, when it is one.
    pub fn container(&self) -> Option<ContainerRef> {
        match self {
            RowNode::Host { .. } => None,
            RowNode::Folder { id, .. } => Some(ContainerRef::Folder(*id)),
            RowNode::File { index, .. } => Some(ContainerRef::File(*index)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisibleNode {
    pub node: RowNode,
    pub key: String,
    pub depth: usize,
    /// aria-level is 1-based.
    pub level: usize,
    pub pos_in_set: usize,
    pub set_size: usize,
    /// `None` for hosts: a leaf must not carry aria-expanded at all.
    pub expanded: Option<bool>,
    /// Colour of the nearest coloured ancestor folder, for the indent rail.
    pub rail_color: Option<String>,
    /// Ancestor folder keys, outermost first; one indent guide per entry.
    pub ancestors: Arc<[String]>,
}

/// The whole visible list, flattened once. Rendering, arrow-key navigation,
/// type-ahead and drop hit-testing all read this single array, so they can never
/// disagree about what is on screen.
pub fn flatten_visible_tree(
    tree: &HostTree,
    is_expanded: &dyn Fn(&str) -> bool,
    folder_color: &dyn Fn(&str) -> Option<String>,
) -> Vec<VisibleNode> {
    enum Item<'t> {
        Container(ContainerRef),
        Host(&'t HostNode),
    }
    struct Walk<'t, 'f> {
        tree: &'t HostTree,
        is_expanded: &'f dyn Fn(&str) -> bool,
        folder_color: &'f dyn Fn(&str) -> Option<String>,
        out: Vec<VisibleNode>,
    }
    impl<'t> Walk<'t, '_> {
        fn walk(
            &mut self,
            items: &[Item<'t>],
            depth: usize,
            ancestors: Arc<[String]>,
            inherited: Option<String>,
        ) {
            let set_size = items.len();
            for (index, item) in items.iter().enumerate() {
                let (key, node, own_color, children): (
                    String,
                    RowNode,
                    Option<String>,
                    Vec<Item<'t>>,
                ) = match item {
                    Item::Host(host) => (
                        host.key.clone(),
                        RowNode::Host {
                            host: host.host.clone(),
                            parent_key: host.parent_key.clone(),
                        },
                        None,
                        Vec::new(),
                    ),
                    Item::Container(ContainerRef::Folder(id)) => {
                        let folder = &self.tree.folders[*id];
                        (
                            folder.key.clone(),
                            RowNode::Folder {
                                id: *id,
                                path: folder.path.clone(),
                                label: folder.label.clone(),
                                parent_key: folder.parent_key.clone(),
                                descendant_host_count: folder.descendant_host_count,
                            },
                            (self.folder_color)(&folder.key),
                            folder
                                .children
                                .iter()
                                .map(|child| match child {
                                    TreeChild::Folder(id) => {
                                        Item::Container(ContainerRef::Folder(*id))
                                    }
                                    TreeChild::Host(host) => Item::Host(host),
                                })
                                .collect(),
                        )
                    }
                    Item::Container(ContainerRef::File(group)) => {
                        let file = &self.tree.file_groups[*group];
                        (
                            file.key.clone(),
                            RowNode::File {
                                index: *group,
                                label: file.label.clone(),
                                tooltip: file.tooltip.clone(),
                                descendant_host_count: file.descendant_host_count,
                            },
                            None,
                            file.children.iter().map(Item::Host).collect(),
                        )
                    }
                };
                let rail_color = own_color.or_else(|| inherited.clone());
                let expanded = (!node.is_host()).then(|| (self.is_expanded)(&key));
                self.out.push(VisibleNode {
                    node,
                    key: key.clone(),
                    depth,
                    level: depth + 1,
                    pos_in_set: index + 1,
                    set_size,
                    expanded,
                    rail_color: rail_color.clone(),
                    ancestors: ancestors.clone(),
                });
                if expanded == Some(true) && !children.is_empty() {
                    let mut nested: Vec<String> = ancestors.to_vec();
                    nested.push(key);
                    self.walk(&children, depth + 1, nested.into(), rail_color);
                }
            }
        }
    }

    let mut walk = Walk {
        tree,
        is_expanded,
        folder_color,
        out: Vec::new(),
    };
    let roots: Vec<Item<'_>> = tree
        .roots
        .iter()
        .map(|&root| Item::Container(root))
        .collect();
    walk.walk(&roots, 0, Arc::from(Vec::<String>::new()), None);
    walk.out
}

/// Sibling host keys of a container, in render order: what reordering acts on.
pub fn sibling_host_keys(tree: &HostTree, container: ContainerRef) -> Vec<String> {
    match container {
        ContainerRef::Folder(id) => tree.folders[id]
            .children
            .iter()
            .filter_map(|child| match child {
                TreeChild::Host(node) => Some(node.key.clone()),
                TreeChild::Folder(_) => None,
            })
            .collect(),
        ContainerRef::File(index) => tree.file_groups[index]
            .children
            .iter()
            .map(|node| node.key.clone())
            .collect(),
    }
}

/// Sibling folder keys of a container, in render order.
pub fn sibling_folder_keys(tree: &HostTree, container: ContainerRef) -> Vec<String> {
    match container {
        ContainerRef::Folder(id) => tree.folders[id]
            .children
            .iter()
            .filter_map(|child| match child {
                TreeChild::Folder(id) => Some(tree.folders[*id].key.clone()),
                TreeChild::Host(_) => None,
            })
            .collect(),
        ContainerRef::File(_) => Vec::new(),
    }
}

/// Top-level folder keys, in render order; file groups are never reordered.
pub fn root_folder_keys(tree: &HostTree) -> Vec<String> {
    tree.roots
        .iter()
        .filter_map(|root| match root {
            ContainerRef::Folder(id) => Some(tree.folders[*id].key.clone()),
            ContainerRef::File(_) => None,
        })
        .collect()
}

/// Every container depth-first (`allContainers` in SessionSidebar), so sibling
/// lookups can scan once.
pub fn all_containers(tree: &HostTree) -> Vec<ContainerRef> {
    fn walk(tree: &HostTree, container: ContainerRef, out: &mut Vec<ContainerRef>) {
        out.push(container);
        if let ContainerRef::Folder(id) = container {
            for child in &tree.folders[id].children {
                if let TreeChild::Folder(child) = child {
                    walk(tree, ContainerRef::Folder(*child), out);
                }
            }
        }
    }
    let mut out = Vec::new();
    for &root in &tree.roots {
        walk(tree, root, &mut out);
    }
    out
}

/// Where a folder sits among its siblings, and under which order key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderSiblings {
    pub parent_key: String,
    pub keys: Vec<String>,
}

/// Where a folder sits among its siblings, and under which order key. Top-level
/// folders report `TREE_ROOT_KEY`, so callers never special-case the root.
pub fn folder_siblings(tree: &HostTree, key: &str) -> Option<FolderSiblings> {
    let folder = tree.folder_by_key(key)?;
    let Some(parent_key) = &folder.parent_key else {
        return Some(FolderSiblings {
            parent_key: TREE_ROOT_KEY.into(),
            keys: root_folder_keys(tree),
        });
    };
    let &parent = tree.folders_by_key.get(parent_key)?;
    Some(FolderSiblings {
        parent_key: tree.folders[parent].key.clone(),
        keys: sibling_folder_keys(tree, ContainerRef::Folder(parent)),
    })
}

/// The order key a folder's children are stored under.
pub fn order_key_for_parent(parent: Option<&FolderNode>) -> String {
    parent
        .map(|parent| parent.key.clone())
        .unwrap_or_else(|| TREE_ROOT_KEY.into())
}

/// Every folder path in use plus each of its ancestors, for pickers.
///
/// Deeper paths are rebuilt from the casing their ancestors already settled on,
/// so a picker never offers `production/eu` and `Production/EU/Edge` side by
/// side. Sorting the input first makes the winning casing independent of the
/// order the caller happened to collect paths in.
pub fn expand_folder_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut canonical: HashMap<String, String> = HashMap::new();
    let mut sorted: Vec<String> = paths
        .into_iter()
        .map(|path| normalize_group_path(Some(path)))
        .filter(|path| !path.is_empty())
        .collect();
    sorted.sort_by(|a, b| locale_compare(a, b));
    for path in &sorted {
        let mut prefix = String::new();
        for segment in segments(path) {
            let candidate = if prefix.is_empty() {
                segment
            } else {
                format!("{prefix}{FOLDER_SEPARATOR}{segment}")
            };
            let key = folder_key(&candidate);
            prefix = canonical.entry(key).or_insert(candidate).clone();
        }
    }
    let unique: HashSet<String> = canonical.into_values().collect();
    let mut out: Vec<String> = unique.into_iter().collect();
    out.sort_by(|a, b| locale_compare(a, b));
    out
}

/// Every folder path a picker should offer: the groups in use, each of their
/// ancestors, and any folder the user created that holds no host yet. Offering
/// only leaf paths would make `Production` unreachable the moment its last host
/// moved down into `Production/EU`.
pub fn known_folder_paths(
    ssh_hosts: &[SshHostEntry],
    saved_profiles: &[SavedHostProfile],
    extra_paths: &[String],
) -> Vec<String> {
    let groups = known_host_groups(ssh_hosts, saved_profiles);
    expand_folder_paths(groups.iter().chain(extra_paths).map(String::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::managed::group_managed_hosts;
    use crate::hosts::test_support::{ROOT, ssh_host, ssh_host_with};

    fn tree_of(hosts: &[SshHostEntry]) -> HostTree {
        tree_with(hosts, &[])
    }

    fn tree_with(hosts: &[SshHostEntry], known: &[&str]) -> HostTree {
        let known: Vec<String> = known.iter().map(|s| s.to_string()).collect();
        build_host_tree(
            &group_managed_hosts(hosts, &[], &[ROOT.into()], Some(ROOT), ""),
            &BuildOptions {
                known_folders: &known,
                folder_order: None,
            },
        )
    }

    fn ordered(hosts: &[SshHostEntry], order: &[(&str, Vec<String>)]) -> HostTree {
        let order: HashMap<String, Vec<String>> = order
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let lookup = move |key: &str| order.get(key).cloned();
        build_host_tree(
            &group_managed_hosts(hosts, &[], &[ROOT.into()], Some(ROOT), ""),
            &BuildOptions {
                known_folders: &[],
                folder_order: Some(&lookup),
            },
        )
    }

    fn folder<'t>(tree: &'t HostTree, path: &str) -> &'t FolderNode {
        tree.folders
            .iter()
            .find(|folder| folder.path == path)
            .unwrap_or_else(|| panic!("no folder {path}"))
    }

    fn root_labels(tree: &HostTree) -> Vec<String> {
        tree.roots
            .iter()
            .map(|&root| tree.container_label(root).to_string())
            .collect()
    }

    fn child_names(tree: &HostTree, node: &FolderNode) -> Vec<String> {
        node.children
            .iter()
            .map(|child| match child {
                TreeChild::Folder(id) => tree.folders[*id].label.clone(),
                TreeChild::Host(host) => host.key.clone(),
            })
            .collect()
    }

    fn child_kinds(node: &FolderNode) -> Vec<&'static str> {
        node.children
            .iter()
            .map(|child| {
                if matches!(child, TreeChild::Folder(_)) {
                    "folder"
                } else {
                    "host"
                }
            })
            .collect()
    }

    fn all_expanded(_: &str) -> bool {
        true
    }

    fn no_color(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn normalizes_padded_doubled_and_edge_separators_on_read() {
        assert_eq!(
            folder_segments(Some("  Production / EU  ")),
            ["Production", "EU"]
        );
        assert_eq!(folder_segments(Some("/a//b/")), ["a", "b"]);
        assert!(folder_segments(Some("")).is_empty());
        assert!(folder_segments(None).is_empty());
        assert_eq!(normalize_group_path(Some(" /Prod// EU /")), "Prod/EU");
    }

    #[test]
    fn keeps_a_folder_name_to_one_segment() {
        assert_eq!(sanitize_folder_name("k8s/prod"), "k8s prod");
        assert_eq!(sanitize_folder_name("  spaced  out  "), "spaced out");
    }

    #[test]
    fn identifies_folders_case_insensitively_at_every_level() {
        assert_eq!(folder_key("Prod/EU"), folder_key("prod/eu"));
        assert_ne!(folder_key("Prod/EU"), folder_key("Prod/US"));
    }

    #[test]
    fn compares_ancestry_by_segment_not_by_string_prefix() {
        assert!(is_descendant_path("Prod/EU", "Prod"));
        assert!(is_descendant_path("Prod/EU/Edge", "prod"));
        // The bug this guards: "Production".starts_with("Prod") is true.
        assert!(!is_descendant_path("Production", "Prod"));
        assert!(!is_descendant_path("Prod", "Prod"));
        assert!(!is_descendant_path("Prod", ""));
    }

    #[test]
    fn walks_parents_and_ancestors() {
        assert_eq!(folder_parent_path("A/B/C"), "A/B");
        assert_eq!(folder_parent_path("A"), "");
        assert_eq!(ancestor_paths("A/B/C"), ["A", "A/B"]);
        assert!(ancestor_paths("A").is_empty());
    }

    #[test]
    fn rewrites_descendants_when_a_folder_is_renamed_or_re_parented() {
        assert_eq!(
            rename_folder_path("Prod", "Prod", "Production").as_deref(),
            Some("Production")
        );
        assert_eq!(
            rename_folder_path("Prod/EU/Edge", "Prod", "Production").as_deref(),
            Some("Production/EU/Edge")
        );
        assert_eq!(rename_folder_path("Production", "Prod", "X"), None);
        assert_eq!(rename_folder_path("Prodigy/EU", "Prod", "X"), None);
        assert_eq!(move_folder_path("Prod/EU", "Lab"), "Lab/EU");
        assert_eq!(move_folder_path("Prod/EU", ""), "EU");
    }

    #[test]
    fn nests_a_three_level_path_with_correct_depth_and_parents() {
        let built = tree_of(&[
            ssh_host("edge-1", Some("Production/EU/Edge")),
            ssh_host("core-1", Some("Production/EU/Core")),
            ssh_host("us-1", Some("Production/US")),
        ]);
        let production = folder(&built, "Production");
        assert_eq!(
            (production.depth, production.parent_key.as_deref()),
            (0, None)
        );
        let eu = folder(&built, "Production/EU");
        assert_eq!(eu.depth, 1);
        assert_eq!(eu.parent_key, Some(folder_key("Production")));
        assert_eq!(eu.label, "EU");
        let edge = folder(&built, "Production/EU/Edge");
        assert_eq!(edge.depth, 2);
        assert_eq!(edge.parent_key, Some(folder_key("Production/EU")));
        // Every host is grouped, so no ssh_config file group survives.
        assert_eq!(root_labels(&built), ["Production"]);
    }

    #[test]
    fn synthesizes_intermediate_folders_that_hold_no_host_of_their_own() {
        let built = tree_of(&[ssh_host("deep", Some("A/B/C"))]);
        let mut paths: Vec<_> = built.folders.iter().map(|f| f.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, ["A", "A/B", "A/B/C"]);
        assert_eq!(child_kinds(folder(&built, "A")), ["folder"]);
        assert_eq!(child_kinds(folder(&built, "A/B/C")), ["host"]);
    }

    #[test]
    fn rolls_the_host_count_up_through_every_ancestor() {
        let built = tree_of(&[
            ssh_host("a", Some("P/EU/Edge")),
            ssh_host("b", Some("P/EU/Edge")),
            ssh_host("c", Some("P/EU")),
            ssh_host("d", Some("P/US")),
        ]);
        assert_eq!(folder(&built, "P").descendant_host_count, 4);
        assert_eq!(folder(&built, "P/EU").descendant_host_count, 3);
        assert_eq!(folder(&built, "P/EU/Edge").descendant_host_count, 2);
        assert_eq!(folder(&built, "P/US").descendant_host_count, 1);
    }

    #[test]
    fn collapses_case_variant_paths_onto_one_folder() {
        let built = tree_of(&[
            ssh_host("a", Some("Prod/EU")),
            ssh_host("b", Some("prod/eu")),
        ]);
        assert_eq!(built.folders.len(), 2);
        assert_eq!(
            built
                .folders
                .iter()
                .find(|f| f.depth == 1)
                .unwrap()
                .descendant_host_count,
            2
        );
    }

    #[test]
    fn picks_a_shared_ancestor_casing_independently_of_host_order() {
        // Two distinct group labels disagree on how to spell their common parent;
        // which spelling wins must not depend on which host was seen first.
        let hosts = [
            ssh_host("a", Some("Prod/EU")),
            ssh_host("b", Some("prod/US")),
        ];
        let paths = |tree: HostTree| {
            let mut out: Vec<_> = tree.folders.iter().map(|f| f.path.clone()).collect();
            out.sort();
            out
        };
        let forward = paths(tree_of(&hosts));
        let mut reversed_hosts = hosts.to_vec();
        reversed_hosts.reverse();
        let reversed = paths(tree_of(&reversed_hosts));
        assert_eq!(forward, reversed);
        assert_eq!(forward.len(), 3);
    }

    #[test]
    fn sorts_folders_before_hosts_and_keeps_the_host_order_it_was_given() {
        let built = tree_of(&[
            ssh_host_with("zulu", Some("Ops"), Some(2)),
            ssh_host_with("alpha", Some("Ops"), Some(1)),
            ssh_host("nested", Some("Ops/Sub")),
        ]);
        let ops = folder(&built, "Ops");
        assert_eq!(child_names(&built, ops), ["Sub", "ssh:alpha", "ssh:zulu"]);
        assert_eq!(
            sibling_host_keys(&built, ContainerRef::Folder(built.folders_by_key[&ops.key])),
            ["ssh:alpha", "ssh:zulu"]
        );
    }

    #[test]
    fn sorts_sibling_folders_alphabetically_until_the_user_places_them() {
        let built = tree_of(&[
            ssh_host("a", Some("Zed")),
            ssh_host("b", Some("Alpha")),
            ssh_host("c", Some("mid")),
        ]);
        assert_eq!(root_labels(&built), ["Alpha", "mid", "Zed"]);
    }

    #[test]
    fn honours_a_manual_sibling_order_alphabetical_for_the_rest() {
        let hosts = [
            ssh_host("a", Some("Zed")),
            ssh_host("b", Some("Alpha")),
            ssh_host("c", Some("mid")),
        ];
        let built = ordered(
            &hosts,
            &[(TREE_ROOT_KEY, vec![folder_key("Zed"), folder_key("mid")])],
        );
        // Placed folders keep their order; Alpha was never dragged, so it follows.
        assert_eq!(root_labels(&built), ["Zed", "mid", "Alpha"]);
    }

    #[test]
    fn orders_nested_folders_under_their_own_parent_key() {
        let hosts = [ssh_host("a", Some("P/Zed")), ssh_host("b", Some("P/Alpha"))];
        let built = ordered(
            &hosts,
            &[("folder:p", vec![folder_key("P/Zed"), folder_key("P/Alpha")])],
        );
        assert_eq!(child_names(&built, folder(&built, "P")), ["Zed", "Alpha"]);
    }

    #[test]
    fn keeps_folders_before_hosts_whatever_the_manual_order_says() {
        let hosts = [
            ssh_host("nested", Some("Ops/Sub")),
            ssh_host("direct", Some("Ops")),
        ];
        let built = ordered(&hosts, &[("folder:ops", vec![folder_key("Ops/Sub")])]);
        assert_eq!(child_kinds(folder(&built, "Ops")), ["folder", "host"]);
    }

    #[test]
    fn ignores_order_entries_for_folders_that_no_longer_exist() {
        let hosts = [ssh_host("a", Some("Zed")), ssh_host("b", Some("Alpha"))];
        let built = ordered(
            &hosts,
            &[(TREE_ROOT_KEY, vec![folder_key("Gone"), folder_key("Zed")])],
        );
        assert_eq!(root_labels(&built), ["Zed", "Alpha"]);
    }

    #[test]
    fn reports_where_a_folder_sits_among_its_siblings() {
        let built = tree_of(&[
            ssh_host("a", Some("Zed")),
            ssh_host("b", Some("Alpha")),
            ssh_host("c", Some("Alpha/Deep")),
        ]);
        assert_eq!(
            folder_siblings(&built, &folder_key("Zed")),
            Some(FolderSiblings {
                parent_key: TREE_ROOT_KEY.into(),
                keys: vec![folder_key("Alpha"), folder_key("Zed")],
            })
        );
        assert_eq!(
            folder_siblings(&built, &folder_key("Alpha/Deep")),
            Some(FolderSiblings {
                parent_key: folder_key("Alpha"),
                keys: vec![folder_key("Alpha/Deep")]
            })
        );
        assert_eq!(folder_siblings(&built, "folder:nope"), None);
    }

    #[test]
    fn pins_ssh_config_file_groups_last_and_never_nests_them() {
        let built = tree_of(&[ssh_host("grouped", Some("Ops")), ssh_host("loose", None)]);
        let &last = built.roots.last().unwrap();
        let ContainerRef::File(index) = last else {
            panic!("last root is not a file group");
        };
        assert!(!built.file_groups[index].children.is_empty());
    }

    #[test]
    fn materializes_known_folders_that_hold_no_host_yet() {
        let built = tree_with(&[ssh_host("a", Some("Ops"))], &["Staging/Blue"]);
        let staging = folder(&built, "Staging");
        assert!(staging.empty);
        assert_eq!(staging.descendant_host_count, 0);
        let blue = folder(&built, "Staging/Blue");
        assert!(blue.empty);
        assert_eq!(blue.descendant_host_count, 0);
        assert!(!folder(&built, "Ops").empty);
    }

    fn flatten_fixture() -> HostTree {
        tree_of(&[
            ssh_host("edge", Some("P/EU/Edge")),
            ssh_host("us", Some("P/US")),
            ssh_host("loose", None),
        ])
    }

    fn row<'r>(rows: &'r [VisibleNode], key: &str) -> &'r VisibleNode {
        rows.iter()
            .find(|row| row.key == key)
            .unwrap_or_else(|| panic!("no row {key}"))
    }

    #[test]
    fn hides_an_entire_subtree_when_its_folder_is_collapsed() {
        let collapsed = folder_key("P/EU");
        let rows = flatten_visible_tree(&flatten_fixture(), &|key| key != collapsed, &no_color);
        assert!(rows.iter().any(|row| row.key == collapsed));
        assert!(!rows.iter().any(|row| row.key == "ssh:edge"));
        assert!(rows.iter().any(|row| row.key == "ssh:us"));
    }

    #[test]
    fn reports_aria_level_position_and_set_size_per_level() {
        let rows = flatten_visible_tree(&flatten_fixture(), &all_expanded, &no_color);
        let eu = row(&rows, &folder_key("P/EU"));
        assert_eq!((eu.level, eu.pos_in_set, eu.set_size), (2, 1, 2));
        let edge = row(&rows, "ssh:edge");
        assert_eq!((edge.level, edge.depth), (4, 3));
        // Two roots: the P folder and the ssh_config file group.
        let sizes: Vec<_> = rows
            .iter()
            .filter(|r| r.level == 1)
            .map(|r| r.set_size)
            .collect();
        assert_eq!(sizes, [2, 2]);
    }

    #[test]
    fn omits_aria_expanded_on_hosts_and_reports_it_on_containers() {
        let rows = flatten_visible_tree(&flatten_fixture(), &all_expanded, &no_color);
        assert_eq!(row(&rows, "ssh:edge").expanded, None);
        assert_eq!(row(&rows, &folder_key("P")).expanded, Some(true));
    }

    #[test]
    fn inherits_the_rail_colour_from_the_nearest_coloured_ancestor() {
        let colored = folder_key("P");
        let rows = flatten_visible_tree(&flatten_fixture(), &all_expanded, &|key| {
            (key == colored).then(|| "#ef5350".to_string())
        });
        assert_eq!(
            row(&rows, &folder_key("P/EU")).rail_color.as_deref(),
            Some("#ef5350")
        );
        assert_eq!(
            row(&rows, "ssh:edge").rail_color.as_deref(),
            Some("#ef5350")
        );
        assert_eq!(row(&rows, "ssh:loose").rail_color, None);
    }

    #[test]
    fn lists_ancestors_outermost_first_so_indent_guides_can_be_drawn() {
        let rows = flatten_visible_tree(&flatten_fixture(), &all_expanded, &no_color);
        assert_eq!(
            &*row(&rows, "ssh:edge").ancestors,
            [folder_key("P"), folder_key("P/EU"), folder_key("P/EU/Edge")]
        );
    }

    #[test]
    fn offers_every_ancestor_and_dedupes_case_insensitively() {
        assert_eq!(
            expand_folder_paths(["Production/EU/Edge", "Production/EU", "Lab"]),
            ["Lab", "Production", "Production/EU", "Production/EU/Edge"]
        );
    }

    #[test]
    fn rebuilds_deeper_paths_from_the_casing_their_ancestors_settled_on() {
        // Never offer "production/eu" and "Production/EU/Edge" in the same list.
        assert_eq!(
            expand_folder_paths(["Production/EU/Edge", "production/eu"]),
            ["production", "production/eu", "production/eu/Edge"]
        );
    }

    #[test]
    fn drops_blank_and_separator_only_entries() {
        assert!(expand_folder_paths(["", "  ", "///"]).is_empty());
    }

    #[test]
    fn exposes_every_host_exactly_once_in_render_order() {
        let built = tree_of(&[
            ssh_host("b", Some("Ops")),
            ssh_host("a", Some("Ops")),
            ssh_host("loose", None),
        ]);
        let keys: Vec<_> = built
            .hosts
            .iter()
            .map(|host| match host {
                ManagedHost::Ssh(entry) => entry.alias.clone(),
                ManagedHost::Profile(entry) => entry.id.clone(),
            })
            .collect();
        assert_eq!(keys, ["a", "b", "loose"]);
    }

    #[test]
    fn keeps_folders_by_key_and_the_root_list_consistent() {
        let built = tree_of(&[ssh_host("a", Some("A/B"))]);
        for &root in &built.roots {
            if let ContainerRef::Folder(id) = root {
                assert_eq!(built.folders_by_key.get(&built.folders[id].key), Some(&id));
            }
        }
    }

    #[test]
    fn offers_every_folder_in_use_plus_its_ancestors() {
        assert_eq!(
            known_folder_paths(
                &[
                    ssh_host("a", Some("Production/EU/Edge")),
                    ssh_host("b", Some("Lab"))
                ],
                &[],
                &[]
            ),
            ["Lab", "Production", "Production/EU", "Production/EU/Edge"]
        );
    }

    #[test]
    fn includes_folders_that_exist_only_in_preferences() {
        assert_eq!(
            known_folder_paths(&[ssh_host("a", Some("Lab"))], &[], &["Staging/Blue".into()]),
            ["Lab", "Staging", "Staging/Blue"]
        );
    }

    #[test]
    fn offers_nothing_when_no_host_is_grouped() {
        assert!(known_folder_paths(&[ssh_host("a", None)], &[], &[]).is_empty());
    }
}
