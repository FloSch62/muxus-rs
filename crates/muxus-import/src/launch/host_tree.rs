//! The sidebar's host grouping, as far as resolving a `--folder` needs it
//! (`groupHosts` in `client/src/host-organization.ts`, `groupManagedHosts`
//! in `client/src/managed-hosts.ts`, `buildHostTree` in
//! `client/src/host-tree.ts`), without search filtering, remembered empty
//! folders or manual folder order.

use std::cmp::Ordering;
use std::collections::HashMap;

use muxus_core::api::{ManagedHostRef, SavedHostProfile, SshHostEntry};
use muxus_core::js;

use crate::collation::locale_compare;

/// One sidebar host, from either host source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ManagedHost<'a> {
    Ssh(&'a SshHostEntry),
    Profile(&'a SavedHostProfile),
}

impl<'a> ManagedHost<'a> {
    /// The name the sidebar shows.
    pub fn display_name(&self) -> &'a str {
        match self {
            ManagedHost::Ssh(entry) => entry
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.display_name.as_deref())
                .unwrap_or(&entry.alias),
            ManagedHost::Profile(entry) => entry
                .metadata
                .display_name
                .as_deref()
                .unwrap_or(&entry.name),
        }
    }

    /// The host's sidebar color, if it has one.
    pub fn color(&self) -> Option<&'a str> {
        match self {
            ManagedHost::Ssh(entry) => entry.metadata.as_ref()?.color.as_deref(),
            ManagedHost::Profile(entry) => entry.metadata.color.as_deref(),
        }
    }

    pub fn to_ref(&self) -> ManagedHostRef {
        match self {
            ManagedHost::Ssh(entry) => ManagedHostRef::Ssh {
                alias: entry.alias.clone(),
            },
            ManagedHost::Profile(entry) => ManagedHostRef::Profile {
                id: entry.id.clone(),
            },
        }
    }

    /// Stable identity (`managedHostKey`): `ssh:<alias>` or `profile:<id>`.
    pub fn key(&self) -> String {
        match self {
            ManagedHost::Ssh(entry) => format!("ssh:{}", entry.alias),
            ManagedHost::Profile(entry) => format!("profile:{}", entry.id),
        }
    }

    fn sort_order(&self) -> i64 {
        const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
        match self {
            ManagedHost::Ssh(entry) => entry.metadata.as_ref().and_then(|m| m.sort_order),
            ManagedHost::Profile(entry) => entry.metadata.sort_order,
        }
        .unwrap_or(MAX_SAFE_INTEGER)
    }
}

/// Sort order inside a group: manual position, then display name.
fn compare_managed_hosts(a: &ManagedHost<'_>, b: &ManagedHost<'_>) -> Ordering {
    a.sort_order()
        .cmp(&b.sort_order())
        .then_with(|| locale_compare(a.display_name(), b.display_name()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupKind {
    Custom,
    File,
}

#[derive(Debug)]
struct ManagedHostGroup<'a> {
    key: String,
    label: String,
    kind: GroupKind,
    tooltip: Option<String>,
    hosts: Vec<ManagedHost<'a>>,
}

/// `groupManagedHosts(sshHosts, savedProfiles, files, rootFile)` without a filter.
fn group_managed_hosts<'a>(
    ssh_hosts: &'a [SshHostEntry],
    saved_profiles: &'a [SavedHostProfile],
    files: &[String],
    root_file: Option<&str>,
) -> Vec<ManagedHostGroup<'a>> {
    let mut profiles: Vec<&SavedHostProfile> = saved_profiles.iter().collect();
    profiles.sort_by(|a, b| {
        locale_compare(
            ManagedHost::Profile(a).display_name(),
            ManagedHost::Profile(b).display_name(),
        )
    });
    let has_saved_custom_groups = profiles.iter().any(|profile| {
        profile
            .metadata
            .group
            .as_deref()
            .is_some_and(|group| !js::js_trim(group).is_empty())
    });

    let mut groups = group_ssh_hosts(ssh_hosts, files, root_file, has_saved_custom_groups);
    let mut custom_index: HashMap<String, usize> = groups
        .iter()
        .enumerate()
        .filter(|(_, group)| group.kind == GroupKind::Custom)
        .map(|(index, group)| (group.key.clone(), index))
        .collect();
    let mut ungrouped = Vec::new();

    for profile in profiles {
        let label = profile
            .metadata
            .group
            .as_deref()
            .map(js::js_trim)
            .unwrap_or("");
        if label.is_empty() {
            ungrouped.push(profile);
            continue;
        }
        let key = format!("custom:{}", label.to_lowercase());
        let index = match custom_index.get(&key) {
            Some(index) => *index,
            None => {
                groups.push(ManagedHostGroup {
                    key: key.clone(),
                    label: label.to_owned(),
                    kind: GroupKind::Custom,
                    tooltip: None,
                    hosts: Vec::new(),
                });
                custom_index.insert(key, groups.len() - 1);
                groups.len() - 1
            }
        };
        groups[index].hosts.push(ManagedHost::Profile(profile));
    }

    if !ungrouped.is_empty() {
        let root_index = root_file.and_then(|root| {
            let key = format!("file:{root}");
            groups.iter().position(|group| group.key == key)
        });
        let index = match root_index {
            Some(index) => index,
            None => {
                let group = ManagedHostGroup {
                    key: "managed:ungrouped".to_owned(),
                    label: if custom_index.is_empty() {
                        "Hosts".to_owned()
                    } else {
                        "Ungrouped".to_owned()
                    },
                    kind: GroupKind::File,
                    tooltip: None,
                    hosts: Vec::new(),
                };
                let first_file = groups
                    .iter()
                    .position(|group| group.kind == GroupKind::File)
                    .unwrap_or(groups.len());
                groups.insert(first_file, group);
                first_file
            }
        };
        // A mixed group is no longer sourced solely from one ssh_config file.
        groups[index].tooltip = None;
        groups[index]
            .hosts
            .extend(ungrouped.into_iter().map(ManagedHost::Profile));
    }

    for group in &mut groups {
        group.hosts.sort_by(compare_managed_hosts);
    }
    let (mut custom, file): (Vec<_>, Vec<_>) = groups
        .into_iter()
        .partition(|group| group.kind == GroupKind::Custom);
    custom.sort_by(|a, b| locale_compare(&a.label, &b.label));
    custom.extend(file);
    custom
}

/// `groupHosts`: custom Muxus groups take priority; hosts without one keep
/// the config-file grouping.
fn group_ssh_hosts<'a>(
    hosts: &'a [SshHostEntry],
    files: &[String],
    root_file: Option<&str>,
    has_additional_custom_groups: bool,
) -> Vec<ManagedHostGroup<'a>> {
    let mut custom: Vec<ManagedHostGroup<'a>> = Vec::new();
    let mut custom_index: HashMap<String, usize> = HashMap::new();
    let mut by_file: Vec<(&'a str, Vec<ManagedHost<'a>>)> = Vec::new();

    for host in hosts {
        let group = host
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.group.as_deref())
            .map(js::js_trim)
            .unwrap_or("");
        if !group.is_empty() {
            let key = group.to_lowercase();
            let index = *custom_index.entry(key.clone()).or_insert_with(|| {
                custom.push(ManagedHostGroup {
                    key: format!("custom:{key}"),
                    label: group.to_owned(),
                    kind: GroupKind::Custom,
                    tooltip: None,
                    hosts: Vec::new(),
                });
                custom.len() - 1
            });
            custom[index].hosts.push(ManagedHost::Ssh(host));
        } else {
            match by_file.iter_mut().find(|(file, _)| *file == host.file) {
                Some((_, list)) => list.push(ManagedHost::Ssh(host)),
                None => by_file.push((&host.file, vec![ManagedHost::Ssh(host)])),
            }
        }
    }

    custom.sort_by(|a, b| locale_compare(&a.label, &b.label));
    for group in &mut custom {
        group.hosts.sort_by(compare_managed_hosts);
    }
    let has_custom_groups = !custom.is_empty() || has_additional_custom_groups;
    let file_rank = |file: &str| {
        files
            .iter()
            .position(|candidate| candidate == file)
            .unwrap_or(usize::MAX)
    };
    by_file.sort_by(|(a, _), (b, _)| {
        file_rank(a)
            .cmp(&file_rank(b))
            .then_with(|| locale_compare(a, b))
    });
    for (file, mut list) in by_file {
        let filename =
            strip_config_extension(file.rsplit(['/', '\\']).next().unwrap_or(file), false);
        let root = Some(file) == root_file;
        let label = match (has_custom_groups, root) {
            (true, true) => "Ungrouped".to_owned(),
            (true, false) => format!("Ungrouped · {filename}"),
            (false, true) => "Hosts".to_owned(),
            (false, false) => filename.to_owned(),
        };
        list.sort_by(compare_managed_hosts);
        custom.push(ManagedHostGroup {
            key: format!("file:{file}"),
            label,
            kind: GroupKind::File,
            tooltip: Some(file.to_owned()),
            hosts: list,
        });
    }
    custom
}

/// `name.replace(/\.(conf|config)$/)`, case-insensitively when asked (`/i`).
pub(crate) fn strip_config_extension(name: &str, ignore_case: bool) -> &str {
    for extension in [".conf", ".config"] {
        if name.len() < extension.len() {
            continue;
        }
        let split = name.len() - extension.len();
        let Some(tail) = name.get(split..) else {
            continue;
        };
        let matches = if ignore_case {
            tail.eq_ignore_ascii_case(extension)
        } else {
            tail == extension
        };
        if matches {
            return &name[..split];
        }
    }
    name
}

/// `folderSegments`: split on `/`, trim each segment, drop empty ones.
fn folder_segments(group: &str) -> Vec<&str> {
    muxus_core::folder_paths::folder_path_segments(group)
}

fn folder_key(segments: &[&str]) -> String {
    muxus_core::folder_paths::sidebar_folder_key(&segments.join("/"))
}

#[derive(Debug)]
pub(crate) enum TreeChild {
    Folder(usize),
    Host(usize),
}

/// A custom folder; `/` in a group string separates nesting levels.
#[derive(Debug)]
pub(crate) struct FolderNode {
    /// Canonical display path, e.g. `Production/EU`.
    pub path: String,
    /// Last segment only.
    pub label: String,
    pub children: Vec<TreeChild>,
}

/// A group that came from an ssh_config file rather than a Muxus group.
#[derive(Debug)]
pub(crate) struct FileGroupNode {
    pub label: String,
    pub tooltip: Option<String>,
    pub hosts: Vec<usize>,
}

/// The folder tree the sidebar shows, hosts referenced by index.
#[derive(Debug)]
pub(crate) struct HostTree<'a> {
    pub hosts: Vec<ManagedHost<'a>>,
    /// In creation order (`foldersByKey` insertion order).
    pub folders: Vec<FolderNode>,
    /// ssh_config file groups, pinned below every folder.
    pub file_groups: Vec<FileGroupNode>,
}

impl<'a> HostTree<'a> {
    pub fn build(
        ssh_hosts: &'a [SshHostEntry],
        saved_profiles: &'a [SavedHostProfile],
        files: &[String],
        root_file: Option<&str>,
    ) -> Self {
        let groups = group_managed_hosts(ssh_hosts, saved_profiles, files, root_file);
        let mut tree = HostTree {
            hosts: Vec::new(),
            folders: Vec::new(),
            file_groups: Vec::new(),
        };
        let mut folder_by_key: HashMap<String, usize> = HashMap::new();
        let mut root_folders: Vec<usize> = Vec::new();

        // Folders are created in sorted path order so the label a
        // case-insensitive collision settles on never depends on the order
        // hosts arrived in.
        let mut declared: Vec<String> = groups
            .iter()
            .filter(|group| group.kind == GroupKind::Custom)
            .map(|group| folder_segments(&group.label).join("/"))
            .filter(|path| !path.is_empty())
            .collect();
        declared.sort_by(|a, b| locale_compare(a, b));
        for path in &declared {
            tree.ensure_folder(path, &mut folder_by_key, &mut root_folders);
        }

        for group in groups {
            match group.kind {
                GroupKind::Custom => {
                    let Some(folder) =
                        tree.ensure_folder(&group.label, &mut folder_by_key, &mut root_folders)
                    else {
                        continue;
                    };
                    for host in group.hosts {
                        tree.hosts.push(host);
                        let index = tree.hosts.len() - 1;
                        tree.folders[folder].children.push(TreeChild::Host(index));
                    }
                }
                GroupKind::File => {
                    let mut hosts = Vec::new();
                    for host in group.hosts {
                        tree.hosts.push(host);
                        hosts.push(tree.hosts.len() - 1);
                    }
                    tree.file_groups.push(FileGroupNode {
                        label: group.label,
                        tooltip: group.tooltip,
                        hosts,
                    });
                }
            }
        }

        for &folder in &root_folders {
            tree.finalize_folder(folder);
        }
        tree
    }

    /// Create the folder and every missing ancestor; the first writer fixes casing.
    fn ensure_folder(
        &mut self,
        path: &str,
        folder_by_key: &mut HashMap<String, usize>,
        root_folders: &mut Vec<usize>,
    ) -> Option<usize> {
        let segments = folder_segments(path);
        let mut parent: Option<usize> = None;
        for depth in 0..segments.len() {
            let key = folder_key(&segments[..=depth]);
            let node = match folder_by_key.get(&key) {
                Some(&node) => node,
                None => {
                    let segment = segments[depth];
                    let path = match parent {
                        Some(parent) => format!("{}/{segment}", self.folders[parent].path),
                        None => segment.to_owned(),
                    };
                    self.folders.push(FolderNode {
                        path,
                        label: segment.to_owned(),
                        children: Vec::new(),
                    });
                    let node = self.folders.len() - 1;
                    folder_by_key.insert(key, node);
                    match parent {
                        Some(parent) => self.folders[parent].children.push(TreeChild::Folder(node)),
                        None => root_folders.push(node),
                    }
                    node
                }
            };
            parent = Some(node);
        }
        parent
    }

    /// Folders sort before hosts; hosts keep the order the grouping settled.
    fn finalize_folder(&mut self, folder: usize) {
        let children = std::mem::take(&mut self.folders[folder].children);
        let mut folders = Vec::new();
        let mut rest = Vec::new();
        for child in children {
            match child {
                TreeChild::Folder(index) => {
                    self.finalize_folder(index);
                    folders.push(index);
                }
                TreeChild::Host(_) => rest.push(child),
            }
        }
        self.sort_folders(&mut folders);
        let mut children: Vec<TreeChild> = folders.into_iter().map(TreeChild::Folder).collect();
        children.extend(rest);
        self.folders[folder].children = children;
    }

    fn sort_folders(&self, folders: &mut [usize]) {
        folders.sort_by(|a, b| locale_compare(&self.folders[*a].label, &self.folders[*b].label));
    }

    /// Every host below a folder, in render order.
    pub fn folder_hosts(&self, folder: usize) -> Vec<ManagedHost<'a>> {
        let mut hosts = Vec::new();
        self.collect(folder, &mut hosts);
        hosts
    }

    fn collect(&self, folder: usize, hosts: &mut Vec<ManagedHost<'a>>) {
        for child in &self.folders[folder].children {
            match child {
                TreeChild::Host(index) => hosts.push(self.hosts[*index]),
                TreeChild::Folder(index) => self.collect(*index, hosts),
            }
        }
    }
}
