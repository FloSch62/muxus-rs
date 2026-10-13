//! Fixtures shared by the host and sidebar tests, mirroring the TypeScript ones.

use crate::model::{HostMetadata, ResolvedHostSettings, SshHostEntry};

pub const ROOT: &str = "/home/test/.ssh/config";

/// An ssh_config host in `ROOT`; it carries metadata only when grouped.
pub fn ssh_host(alias: &str, group: Option<&str>) -> SshHostEntry {
    ssh_host_with(alias, group, None)
}

/// Like `ssh_host`, with a persisted drag position.
pub fn ssh_host_with(alias: &str, group: Option<&str>, sort_order: Option<i64>) -> SshHostEntry {
    SshHostEntry {
        alias: alias.into(),
        aliases: vec![alias.into()],
        file: ROOT.into(),
        resolved: ResolvedHostSettings {
            hostname: format!("{alias}.example.test"),
            port: 22,
            ..Default::default()
        },
        metadata: (group.is_some() || sort_order.is_some()).then(|| HostMetadata {
            profile_id: format!("ssh-{alias}"),
            group: group.map(Into::into),
            sort_order,
            ..Default::default()
        }),
        description: None,
        options: Default::default(),
    }
}

/// A visible row carrying only what navigation and selection read; the node
/// payload is a placeholder host or folder.
pub fn visible_row(
    key: &str,
    depth: usize,
    expanded: Option<bool>,
    host: bool,
) -> crate::hosts::VisibleNode {
    use crate::hosts::{ManagedHost, RowNode, VisibleNode};
    let node = if host {
        RowNode::Host {
            host: ManagedHost::ssh(ssh_host(key, None)),
            parent_key: String::new(),
        }
    } else {
        RowNode::Folder {
            id: 0,
            path: key.into(),
            label: key.into(),
            parent_key: None,
            descendant_host_count: 0,
        }
    };
    VisibleNode {
        node,
        key: key.into(),
        depth,
        level: depth + 1,
        pos_in_set: 1,
        set_size: 1,
        expanded,
        rail_color: None,
        ancestors: Vec::<String>::new().into(),
    }
}
