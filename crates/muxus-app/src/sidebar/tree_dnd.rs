//! Drag and drop rules for the host tree. Port of
//! `client/src/components/sidebar/tree-dnd.ts`.

use crate::hosts::organization::DropEdge;
use crate::hosts::tree::{
    ContainerRef, FolderNode, HostTree, MAX_FOLDER_DEPTH, TreeChild, folder_depth, folder_path,
    folder_segments, is_descendant_path, is_same_path,
};
use crate::hosts::{RowNode, VisibleNode};
use crate::sidebar::tree_selection::Selection;

/// A host being dragged, and the container it is dragged out of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraggedHost {
    pub host_key: String,
    pub parent_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragSource {
    /// The grabbed host alone, or every host of the selection it belongs to, in tree order.
    Host {
        hosts: Vec<DraggedHost>,
    },
    Folder {
        folder_key: String,
        path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropTarget {
    IntoFolder {
        folder_key: String,
    },
    HostEdge {
        host_key: String,
        parent_key: String,
        edge: DropEdge,
    },
    FolderEdge {
        folder_key: String,
        /// Parent of the folder being dropped next to; `None` means top level.
        parent_key: Option<String>,
        edge: DropEdge,
    },
    Root,
}

/// What a pointer at `ratio` down a row means, per node kind. A folder's middle
/// band drops inside it; its top and bottom quarters place the dragged item
/// beside it, which is what makes folders manually orderable.
pub fn drop_target_for_row(row: &VisibleNode, ratio: f64) -> DropTarget {
    if let RowNode::Host { parent_key, .. } = &row.node {
        return DropTarget::HostEdge {
            host_key: row.key.clone(),
            parent_key: parent_key.clone(),
            edge: if ratio < 0.5 {
                DropEdge::Before
            } else {
                DropEdge::After
            },
        };
    }
    if ratio > 0.25 && ratio < 0.75 {
        return DropTarget::IntoFolder {
            folder_key: row.key.clone(),
        };
    }
    // ssh_config file groups have no order of their own to join.
    let RowNode::Folder { parent_key, .. } = &row.node else {
        return DropTarget::Root;
    };
    DropTarget::FolderEdge {
        folder_key: row.key.clone(),
        parent_key: parent_key.clone(),
        edge: if ratio <= 0.25 {
            DropEdge::Before
        } else {
            DropEdge::After
        },
    }
}

/// What dragging `row` picks up. A host inside a selection of several brings the
/// whole selection along, collapsed folders included, the way a file manager
/// moves every selected file; a host outside the selection drags alone.
pub fn drag_source_for_row(
    row: &VisibleNode,
    tree: &HostTree,
    selection: &Selection,
) -> Option<DragSource> {
    match &row.node {
        RowNode::Host { parent_key, .. } => {
            let hosts = if selection.len() > 1 && selection.contains(&row.key) {
                let mut out = Vec::new();
                for &root in &tree.roots {
                    host_nodes(tree, root, &mut |key, parent| {
                        if selection.contains(key) {
                            out.push(DraggedHost {
                                host_key: key.to_string(),
                                parent_key: parent.to_string(),
                            });
                        }
                    });
                }
                out
            } else {
                vec![DraggedHost {
                    host_key: row.key.clone(),
                    parent_key: parent_key.clone(),
                }]
            };
            Some(DragSource::Host { hosts })
        }
        RowNode::Folder { path, .. } => Some(DragSource::Folder {
            folder_key: row.key.clone(),
            path: path.clone(),
        }),
        // ssh_config file groups belong to the config file, not to the sidebar.
        RowNode::File { .. } => None,
    }
}

/// The folder path a drop target lands in; '' is the root.
///
/// `None` rather than '' when the target resolves to an ssh_config file
/// group: that is not a folder, and nothing may be dropped into it.
pub fn target_path(target: &DropTarget, tree: &HostTree) -> Option<String> {
    let folder_path_of = |key: &str| tree.folder_by_key(key).map(|folder| folder.path.clone());
    match target {
        DropTarget::Root => Some(String::new()),
        DropTarget::IntoFolder { folder_key } => folder_path_of(folder_key),
        DropTarget::HostEdge { parent_key, .. } => folder_path_of(parent_key),
        // Beside a top-level folder means joining the top level.
        DropTarget::FolderEdge { parent_key, .. } => match parent_key {
            Some(parent_key) => folder_path_of(parent_key),
            None => Some(String::new()),
        },
    }
}

pub fn container_for(target: &DropTarget, tree: &HostTree) -> Option<ContainerRef> {
    let key = match target {
        DropTarget::IntoFolder { folder_key } => Some(folder_key.as_str()),
        DropTarget::HostEdge { parent_key, .. } => Some(parent_key.as_str()),
        DropTarget::FolderEdge { parent_key, .. } => parent_key.as_deref(),
        DropTarget::Root => None,
    }?;
    tree.find_container(key)
}

/// Whether a drop is allowed. Kept pure and exhaustive because the drag preview
/// and the commit both have to agree: a target that highlights but refuses on
/// release reads as a bug.
pub fn can_drop(source: &DragSource, target: &DropTarget, tree: &HostTree) -> bool {
    let destination = target_path(target, tree);

    let (folder_key, path) = match source {
        DragSource::Host { hosts } => {
            // Beside a host that is itself being dragged would place it relative to itself.
            if let DropTarget::HostEdge { host_key, .. } = target
                && hosts.iter().any(|host| &host.host_key == host_key)
            {
                return false;
            }
            // Reordering within the container the hosts already sit in changes no
            // group, so it stays available even inside a read-only ssh_config file group.
            if is_pure_reorder(source, target) {
                return true;
            }
            // Otherwise a host is changing folders, and a file group is not one.
            return destination.is_some();
        }
        DragSource::Folder { folder_key, path } => (folder_key, path),
    };

    let Some(destination) = destination else {
        return false;
    };
    let Some(folder) = tree.folder_by_key(folder_key) else {
        return false;
    };
    if is_same_path(&destination, path) || is_descendant_path(&destination, path) {
        return false;
    }
    // Dropping a folder beside itself would not move it anywhere.
    if let DropTarget::FolderEdge {
        folder_key: beside, ..
    } = target
        && beside == folder_key
    {
        return false;
    }
    // Landing in the parent it already has is a reorder when a position was
    // named, and a no-op when it was not.
    let segments = folder_segments(Some(path));
    let current_parent = folder_path(&segments[..segments.len().saturating_sub(1)]);
    if is_same_path(&destination, &current_parent)
        && !matches!(target, DropTarget::FolderEdge { .. })
    {
        return false;
    }
    folder_depth(&destination) + subtree_height(tree, folder) < MAX_FOLDER_DEPTH
}

/// The sibling order after dropping a folder beside another. The dragged folder
/// may arrive from elsewhere, so its old key is removed and its new one, which
/// differs whenever the drop also re-parents it, is inserted.
pub fn folder_order_after_drop(
    sibling_keys: &[String],
    source_old_key: &str,
    source_new_key: &str,
    target_key: Option<&str>,
    edge: DropEdge,
) -> Vec<String> {
    let mut next: Vec<String> = sibling_keys
        .iter()
        .filter(|key| key.as_str() != source_old_key && key.as_str() != source_new_key)
        .cloned()
        .collect();
    let index = target_key.and_then(|target| next.iter().position(|key| key == target));
    match index {
        Some(index) => next.insert(
            index + usize::from(edge == DropEdge::After),
            source_new_key.to_string(),
        ),
        None => next.push(source_new_key.to_string()),
    }
    next
}

/// Whether two drop targets mean the same landing spot, so a drag-over that
/// resolves to the one already held can leave state untouched.
///
/// Every field that moves the drop has to be compared. Treating two edges of the
/// same kind as interchangeable would keep the first one the pointer touched and
/// commit there, wherever the drag was actually released.
pub fn same_target(a: Option<&DropTarget>, b: &DropTarget) -> bool {
    match (a, b) {
        (
            Some(DropTarget::IntoFolder { folder_key: a }),
            DropTarget::IntoFolder { folder_key: b },
        ) => a == b,
        (
            Some(DropTarget::HostEdge {
                host_key: a,
                edge: a_edge,
                ..
            }),
            DropTarget::HostEdge {
                host_key: b,
                edge: b_edge,
                ..
            },
        ) => a == b && a_edge == b_edge,
        (
            Some(DropTarget::FolderEdge {
                folder_key: a,
                edge: a_edge,
                ..
            }),
            DropTarget::FolderEdge {
                folder_key: b,
                edge: b_edge,
                ..
            },
        ) => a == b && a_edge == b_edge,
        // Only the root is left, and there is one of it.
        (Some(DropTarget::Root), DropTarget::Root) => true,
        _ => false,
    }
}

/// A drop that only changes order, never which container any dragged host is in.
pub fn is_pure_reorder(source: &DragSource, target: &DropTarget) -> bool {
    let DragSource::Host { hosts } = source else {
        return false;
    };
    let container_key = match target {
        DropTarget::HostEdge { parent_key, .. } => Some(parent_key.as_str()),
        DropTarget::FolderEdge { parent_key, .. } => parent_key.as_deref(),
        DropTarget::IntoFolder { folder_key } => Some(folder_key.as_str()),
        DropTarget::Root => None,
    };
    container_key.is_some_and(|key| hosts.iter().all(|host| host.parent_key == key))
}

/// Every host below a container, in render order: `(key, parent key)`.
fn host_nodes(tree: &HostTree, container: ContainerRef, visit: &mut dyn FnMut(&str, &str)) {
    match container {
        ContainerRef::Folder(id) => {
            for child in &tree.folders[id].children {
                match child {
                    TreeChild::Host(node) => visit(&node.key, &node.parent_key),
                    TreeChild::Folder(child) => {
                        host_nodes(tree, ContainerRef::Folder(*child), visit)
                    }
                }
            }
        }
        ContainerRef::File(index) => {
            for node in &tree.file_groups[index].children {
                visit(&node.key, &node.parent_key);
            }
        }
    }
}

/// Deepest nesting below a folder, in levels; a leaf folder is 0.
fn subtree_height(tree: &HostTree, folder: &FolderNode) -> usize {
    folder
        .children
        .iter()
        .filter_map(|child| match child {
            TreeChild::Folder(id) => Some(subtree_height(tree, &tree.folders[*id]) + 1),
            TreeChild::Host(_) => None,
        })
        .max()
        .unwrap_or(0)
}

/// The path a dragged folder ends up at, keeping its own name.
pub fn dropped_folder_path(source: &DragSource, destination: &str) -> String {
    let DragSource::Folder { path, .. } = source else {
        return destination.to_string();
    };
    let mut segments = folder_segments(Some(destination));
    segments.extend(folder_segments(Some(path)).pop());
    folder_path(&segments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::managed::group_managed_hosts;
    use crate::hosts::test_support::{ROOT, ssh_host};
    use crate::hosts::tree::{BuildOptions, build_host_tree, flatten_visible_tree, folder_key};

    fn build(hosts: &[crate::model::SshHostEntry]) -> HostTree {
        build_host_tree(
            &group_managed_hosts(hosts, &[], &[ROOT.into()], Some(ROOT), ""),
            &BuildOptions::default(),
        )
    }

    ///  Prod            folder
    ///    EU            folder
    ///      edge        host
    ///    core          host
    ///  Lab             folder
    ///    lab-1         host
    ///  Hosts           ssh_config file group
    ///    loose         host
    fn tree() -> HostTree {
        build(&[
            ssh_host("edge", Some("Prod/EU")),
            ssh_host("core", Some("Prod")),
            ssh_host("lab-1", Some("Lab")),
            ssh_host("loose", None),
        ])
    }

    fn rows(tree: &HostTree) -> Vec<VisibleNode> {
        flatten_visible_tree(tree, &|_| true, &|_| None)
    }

    fn row(tree: &HostTree, key: &str) -> VisibleNode {
        rows(tree).into_iter().find(|row| row.key == key).unwrap()
    }

    fn file_group_key() -> String {
        format!("file:{ROOT}")
    }

    fn dragged(host: &str, parent: &str) -> DraggedHost {
        DraggedHost {
            host_key: host.into(),
            parent_key: parent.into(),
        }
    }

    fn drag_edge() -> DragSource {
        DragSource::Host {
            hosts: vec![dragged("ssh:edge", &folder_key("Prod/EU"))],
        }
    }

    fn drag_loose() -> DragSource {
        DragSource::Host {
            hosts: vec![dragged("ssh:loose", &file_group_key())],
        }
    }

    fn drag_folder(path: &str) -> DragSource {
        DragSource::Folder {
            folder_key: folder_key(path),
            path: path.into(),
        }
    }

    fn into(path: &str) -> DropTarget {
        DropTarget::IntoFolder {
            folder_key: folder_key(path),
        }
    }

    fn selection(keys: &[&str]) -> Selection {
        keys.iter().map(|k| k.to_string()).collect()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn splits_a_host_row_into_a_before_and_an_after_half() {
        let t = tree();
        assert!(matches!(
            drop_target_for_row(&row(&t, "ssh:edge"), 0.2),
            DropTarget::HostEdge {
                edge: DropEdge::Before,
                ..
            }
        ));
        assert!(matches!(
            drop_target_for_row(&row(&t, "ssh:edge"), 0.8),
            DropTarget::HostEdge {
                edge: DropEdge::After,
                ..
            }
        ));
    }

    #[test]
    fn treats_the_middle_of_a_folder_as_drop_inside() {
        let t = tree();
        assert_eq!(
            drop_target_for_row(&row(&t, &folder_key("Lab")), 0.5),
            into("Lab")
        );
    }

    #[test]
    fn treats_a_folder_edge_as_place_beside_this_folder() {
        let t = tree();
        assert_eq!(
            drop_target_for_row(&row(&t, &folder_key("Prod/EU")), 0.1),
            DropTarget::FolderEdge {
                folder_key: folder_key("Prod/EU"),
                parent_key: Some(folder_key("Prod")),
                edge: DropEdge::Before,
            }
        );
        // A top-level folder has no parent key: its edges join the top level.
        assert_eq!(
            drop_target_for_row(&row(&t, &folder_key("Prod")), 0.9),
            DropTarget::FolderEdge {
                folder_key: folder_key("Prod"),
                parent_key: None,
                edge: DropEdge::After
            }
        );
    }

    #[test]
    fn sends_an_ssh_config_file_group_edge_to_the_root() {
        let t = tree();
        assert_eq!(
            drop_target_for_row(&row(&t, &file_group_key()), 0.9),
            DropTarget::Root
        );
    }

    #[test]
    fn reorders_a_folder_among_the_siblings_it_already_has() {
        let t = tree();
        let target = DropTarget::FolderEdge {
            folder_key: folder_key("Lab"),
            parent_key: None,
            edge: DropEdge::Before,
        };
        assert!(can_drop(&drag_folder("Prod"), &target, &t));
        assert_eq!(target_path(&target, &t).as_deref(), Some(""));
    }

    #[test]
    fn refuses_to_drop_a_folder_beside_itself() {
        let t = tree();
        let target = DropTarget::FolderEdge {
            folder_key: folder_key("Prod"),
            parent_key: None,
            edge: DropEdge::After,
        };
        assert!(!can_drop(&drag_folder("Prod"), &target, &t));
    }

    #[test]
    fn still_refuses_a_folder_edge_inside_its_own_subtree() {
        let t = tree();
        let target = DropTarget::FolderEdge {
            folder_key: folder_key("Prod/EU"),
            parent_key: Some(folder_key("Prod")),
            edge: DropEdge::Before,
        };
        assert!(!can_drop(&drag_folder("Prod"), &target, &t));
    }

    #[test]
    fn inserts_before_or_after_the_folder_it_was_dropped_on() {
        let siblings = strings(&["a", "b", "c"]);
        assert_eq!(
            folder_order_after_drop(&siblings, "c", "c", Some("a"), DropEdge::Before),
            ["c", "a", "b"]
        );
        assert_eq!(
            folder_order_after_drop(&siblings, "a", "a", Some("b"), DropEdge::After),
            ["b", "a", "c"]
        );
    }

    #[test]
    fn appends_when_no_target_folder_was_named() {
        assert_eq!(
            folder_order_after_drop(&strings(&["a", "b"]), "x", "x", None, DropEdge::After),
            ["a", "b", "x"]
        );
    }

    #[test]
    fn uses_the_new_key_when_the_drop_also_re_parents_the_folder() {
        // Dragging Lab into Prod changes its key from folder:lab to folder:prod/lab.
        assert_eq!(
            folder_order_after_drop(
                &strings(&["a", "b"]),
                "folder:lab",
                "folder:prod/lab",
                Some("a"),
                DropEdge::After
            ),
            ["a", "folder:prod/lab", "b"]
        );
    }

    #[test]
    fn describes_hosts_and_folders() {
        let t = tree();
        assert_eq!(
            drag_source_for_row(&row(&t, "ssh:edge"), &t, &Selection::new()),
            Some(drag_edge())
        );
        assert_eq!(
            drag_source_for_row(&row(&t, &folder_key("Prod")), &t, &Selection::new()),
            Some(drag_folder("Prod"))
        );
    }

    #[test]
    fn refuses_to_drag_an_ssh_config_file_group() {
        let t = tree();
        assert_eq!(
            drag_source_for_row(&row(&t, &file_group_key()), &t, &Selection::new()),
            None
        );
    }

    #[test]
    fn carries_the_whole_selection_in_tree_order_when_a_selected_host_is_grabbed() {
        let t = tree();
        let picked = selection(&["ssh:loose", "ssh:edge", "ssh:lab-1"]);
        assert_eq!(
            drag_source_for_row(&row(&t, "ssh:lab-1"), &t, &picked),
            Some(DragSource::Host {
                // Top-level folders sort alphabetically, so Lab comes before Prod.
                hosts: vec![
                    dragged("ssh:lab-1", &folder_key("Lab")),
                    dragged("ssh:edge", &folder_key("Prod/EU")),
                    dragged("ssh:loose", &file_group_key()),
                ],
            })
        );
    }

    #[test]
    fn includes_selected_hosts_inside_collapsed_folders() {
        // The visible rows do not matter: the selection is resolved against the tree.
        let t = tree();
        let prod = folder_key("Prod");
        let collapsed = flatten_visible_tree(&t, &|key| key != prod, &|_| None);
        let lab = collapsed
            .iter()
            .find(|entry| entry.key == "ssh:lab-1")
            .unwrap();
        let Some(DragSource::Host { hosts }) =
            drag_source_for_row(lab, &t, &selection(&["ssh:lab-1", "ssh:core"]))
        else {
            panic!("not a host drag");
        };
        let keys: Vec<_> = hosts.iter().map(|h| h.host_key.as_str()).collect();
        assert_eq!(keys, ["ssh:lab-1", "ssh:core"]);
    }

    #[test]
    fn drags_a_host_outside_the_selection_on_its_own() {
        let t = tree();
        assert_eq!(
            drag_source_for_row(
                &row(&t, "ssh:edge"),
                &t,
                &selection(&["ssh:core", "ssh:lab-1"])
            ),
            Some(drag_edge())
        );
    }

    #[test]
    fn resolves_folders_and_the_root_but_not_file_groups() {
        let t = tree();
        assert_eq!(target_path(&DropTarget::Root, &t).as_deref(), Some(""));
        assert_eq!(
            target_path(&into("Prod/EU"), &t).as_deref(),
            Some("Prod/EU")
        );
        assert_eq!(
            target_path(
                &DropTarget::IntoFolder {
                    folder_key: file_group_key()
                },
                &t
            ),
            None
        );
    }

    #[test]
    fn moves_a_host_between_folders_and_out_to_the_root() {
        let t = tree();
        assert!(can_drop(&drag_edge(), &into("Lab"), &t));
        assert!(can_drop(&drag_edge(), &DropTarget::Root, &t));
    }

    #[test]
    fn never_drops_anything_into_an_ssh_config_file_group() {
        let t = tree();
        let into_file = DropTarget::IntoFolder {
            folder_key: file_group_key(),
        };
        assert!(!can_drop(&drag_edge(), &into_file, &t));
        assert!(!can_drop(&drag_folder("Prod"), &into_file, &t));
    }

    #[test]
    fn still_allows_reordering_inside_a_file_group_which_changes_no_folder() {
        let t = tree();
        let target = DropTarget::HostEdge {
            host_key: "ssh:other".into(),
            parent_key: file_group_key(),
            edge: DropEdge::After,
        };
        assert!(can_drop(&drag_loose(), &target, &t));
        assert!(is_pure_reorder(&drag_loose(), &target));
    }

    #[test]
    fn refuses_to_drop_a_host_onto_itself() {
        let t = tree();
        let on_self = DropTarget::HostEdge {
            host_key: "ssh:edge".into(),
            parent_key: folder_key("Prod/EU"),
            edge: DropEdge::Before,
        };
        assert!(!can_drop(&drag_edge(), &on_self, &t));
    }

    #[test]
    fn refuses_to_move_a_folder_into_itself_or_a_descendant() {
        let t = tree();
        assert!(!can_drop(&drag_folder("Prod"), &into("Prod"), &t));
        assert!(!can_drop(&drag_folder("Prod"), &into("Prod/EU"), &t));
    }

    #[test]
    fn refuses_a_folder_move_that_changes_nothing() {
        let t = tree();
        // Lab is already at the root.
        assert!(!can_drop(&drag_folder("Lab"), &DropTarget::Root, &t));
        // EU is already inside Prod.
        assert!(!can_drop(&drag_folder("Prod/EU"), &into("Prod"), &t));
    }

    #[test]
    fn allows_a_genuine_folder_re_parent() {
        assert!(can_drop(&drag_folder("Lab"), &into("Prod"), &tree()));
    }

    #[test]
    fn allows_a_move_that_lands_exactly_on_the_depth_cap() {
        // target/a/b/c/d/e/f/g is eight levels, which is MAX_FOLDER_DEPTH.
        let deep = build(&[
            ssh_host("x", Some("a/b/c/d/e/f/g")),
            ssh_host("y", Some("target")),
        ]);
        assert!(can_drop(&drag_folder("a"), &into("target"), &deep));
    }

    #[test]
    fn refuses_a_move_that_would_nest_past_the_depth_cap() {
        let deeper = build(&[
            ssh_host("x", Some("a/b/c/d/e/f/g/h")),
            ssh_host("y", Some("target")),
        ]);
        assert!(!can_drop(&drag_folder("a"), &into("target"), &deeper));
    }

    fn drag_mixed() -> DragSource {
        DragSource::Host {
            hosts: vec![
                dragged("ssh:core", &folder_key("Prod")),
                dragged("ssh:lab-1", &folder_key("Lab")),
            ],
        }
    }

    #[test]
    fn moves_several_hosts_into_a_folder_or_out_to_the_root() {
        let t = tree();
        assert!(can_drop(&drag_mixed(), &into("Prod/EU"), &t));
        assert!(can_drop(&drag_mixed(), &DropTarget::Root, &t));
        // Lab already holds one of them; the other still moves in.
        assert!(can_drop(&drag_mixed(), &into("Lab"), &t));
        assert!(!is_pure_reorder(&drag_mixed(), &into("Lab")));
    }

    #[test]
    fn refuses_to_place_several_hosts_beside_one_of_themselves() {
        let t = tree();
        let target = DropTarget::HostEdge {
            host_key: "ssh:lab-1".into(),
            parent_key: folder_key("Lab"),
            edge: DropEdge::After,
        };
        assert!(!can_drop(&drag_mixed(), &target, &t));
    }

    #[test]
    fn reorders_inside_a_file_group_only_when_every_host_already_sits_there() {
        let t = tree();
        let into_file = DropTarget::HostEdge {
            host_key: "ssh:loose".into(),
            parent_key: file_group_key(),
            edge: DropEdge::After,
        };
        assert!(!can_drop(&drag_mixed(), &into_file, &t));
        let from_file = DragSource::Host {
            hosts: vec![
                dragged("ssh:a", &file_group_key()),
                dragged("ssh:b", &file_group_key()),
            ],
        };
        assert!(can_drop(&from_file, &into_file, &t));
        assert!(is_pure_reorder(&from_file, &into_file));
    }

    #[test]
    fn finds_folders_and_file_groups_alike() {
        let t = tree();
        let key = |found: Option<ContainerRef>| found.map(|c| t.container_key(c).to_string());
        assert_eq!(
            key(container_for(&into("Prod"), &t)),
            Some(folder_key("Prod"))
        );
        let edge = DropTarget::HostEdge {
            host_key: "ssh:loose".into(),
            parent_key: file_group_key(),
            edge: DropEdge::After,
        };
        assert_eq!(key(container_for(&edge, &t)), Some(file_group_key()));
        assert_eq!(container_for(&DropTarget::Root, &t), None);
    }

    fn folder_edge(path: &str, edge: DropEdge) -> DropTarget {
        DropTarget::FolderEdge {
            folder_key: folder_key(path),
            parent_key: None,
            edge,
        }
    }

    #[test]
    fn holds_the_current_target_when_the_pointer_resolves_to_the_same_spot() {
        assert!(same_target(
            Some(&folder_edge("Prod", DropEdge::Before)),
            &folder_edge("Prod", DropEdge::Before)
        ));
        assert!(same_target(Some(&DropTarget::Root), &DropTarget::Root));
    }

    #[test]
    fn separates_the_two_edges_of_one_folder() {
        assert!(!same_target(
            Some(&folder_edge("Prod", DropEdge::Before)),
            &folder_edge("Prod", DropEdge::After)
        ));
    }

    #[test]
    fn separates_the_same_edge_of_two_folders() {
        assert!(!same_target(
            Some(&folder_edge("Prod", DropEdge::Before)),
            &folder_edge("Lab", DropEdge::Before)
        ));
    }

    #[test]
    fn separates_host_edges_folders_and_kinds() {
        let before = DropTarget::HostEdge {
            host_key: "ssh:edge".into(),
            parent_key: folder_key("Prod/EU"),
            edge: DropEdge::Before,
        };
        let after = DropTarget::HostEdge {
            host_key: "ssh:edge".into(),
            parent_key: folder_key("Prod/EU"),
            edge: DropEdge::After,
        };
        let other = DropTarget::HostEdge {
            host_key: "ssh:core".into(),
            parent_key: folder_key("Prod/EU"),
            edge: DropEdge::Before,
        };
        assert!(!same_target(Some(&before), &after));
        assert!(!same_target(Some(&before), &other));
        assert!(!same_target(
            Some(&before),
            &folder_edge("Prod", DropEdge::Before)
        ));
        assert!(!same_target(Some(&into("Prod")), &into("Lab")));
        assert!(!same_target(None, &DropTarget::Root));
    }

    #[test]
    fn keeps_the_folder_name_and_swaps_its_parent() {
        assert_eq!(dropped_folder_path(&drag_folder("Prod"), "Lab"), "Lab/Prod");
        assert_eq!(dropped_folder_path(&drag_folder("Prod"), ""), "Prod");
        assert_eq!(
            dropped_folder_path(&drag_folder("Prod/EU"), "Lab"),
            "Lab/EU"
        );
    }
}
