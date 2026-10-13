//! The host catalog as the sidebar sees it: both host sources (ssh_config
//! entries and hosts saved by Muxus) grouped, searched and built into the
//! folder tree. Ported from `client/src/{host-organization,saved-hosts,
//! managed-hosts,host-search,host-tree}.ts`.

pub mod managed;
pub mod organization;
pub mod saved;
pub mod search;
pub mod tree;

#[cfg(test)]
pub(crate) mod test_support;

pub use managed::{
    ManagedHost, ManagedHostGroup, ManagedHostRef, best_managed_host_match, group_managed_hosts,
    managed_host_address, managed_host_display_name, managed_host_key,
};
pub use organization::GroupKind;
pub use tree::{
    ContainerRef, FolderNode, HostNode, HostTree, RowNode, TreeChild, VisibleNode, build_host_tree,
    flatten_visible_tree,
};
