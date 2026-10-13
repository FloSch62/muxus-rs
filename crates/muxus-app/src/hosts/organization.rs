//! Grouping and ordering of ssh_config hosts. Port of `client/src/host-organization.ts`.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::hosts::search::{matches_tokens, search_tokens, searchable_text};
use crate::model::SshHostEntry;
use crate::util::locale_compare;

/// The swatches the host and folder editors offer.
pub use crate::model::palette::HOST_COLORS;

/// A Muxus group the user made, or an ssh_config file's ungrouped hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupKind {
    Custom,
    File,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostGroup<'a> {
    pub key: String,
    pub label: String,
    pub kind: GroupKind,
    pub tooltip: Option<String>,
    pub hosts: Vec<&'a SshHostEntry>,
}

pub fn host_display_name(host: &SshHostEntry) -> &str {
    host.metadata
        .as_ref()
        .and_then(|metadata| metadata.display_name.as_deref())
        .unwrap_or(&host.alias)
}

pub fn host_address(host: &SshHostEntry) -> String {
    let resolved = &host.resolved;
    let user = resolved
        .user
        .as_deref()
        .map(|user| format!("{user}@"))
        .unwrap_or_default();
    let port = if resolved.port != 22 {
        format!(":{}", resolved.port)
    } else {
        String::new()
    };
    format!("{user}{}{port}", resolved.hostname)
}

/// Everything a search may look at, lower-cased.
pub fn host_search_text(host: &SshHostEntry) -> String {
    let metadata = host.metadata.as_ref();
    searchable_text(
        host.aliases
            .iter()
            .map(|alias| Some(alias.as_str()))
            .chain([
                Some(host.resolved.hostname.as_str()),
                host.resolved.user.as_deref(),
                host.description.as_deref(),
                metadata.and_then(|m| m.display_name.as_deref()),
                metadata.and_then(|m| m.group.as_deref()),
            ]),
    )
}

/// Match against tokens the caller split once, for list scans.
pub fn matches_host_tokens(host: &SshHostEntry, tokens: &[&str]) -> bool {
    tokens.is_empty() || matches_tokens(&host_search_text(host), tokens)
}

/// Before or after the row a drop lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropEdge {
    Before,
    After,
}

/// Build the persisted order after dropping hosts before/after another, or
/// append them when the group header itself is the target. Several dropped
/// hosts land together, in the order given.
pub fn host_order_after_drop(
    aliases: &[String],
    source_aliases: &[String],
    target_alias: Option<&str>,
    edge: DropEdge,
) -> Vec<String> {
    let mut next: Vec<String> = aliases
        .iter()
        .filter(|alias| !source_aliases.contains(alias))
        .cloned()
        .collect();
    let Some(target) = target_alias else {
        next.extend(source_aliases.iter().cloned());
        return next;
    };
    let Some(target_index) = next.iter().position(|alias| alias == target) else {
        next.extend(source_aliases.iter().cloned());
        return next;
    };
    let at = target_index + usize::from(edge == DropEdge::After);
    next.splice(at..at, source_aliases.iter().cloned());
    next
}

/// `sortOrder` first, then the display name: the order hosts take in a group.
pub(crate) fn compare_sort_order(a: Option<i64>, b: Option<i64>) -> Ordering {
    // `Number.MAX_SAFE_INTEGER` stands in for "never placed".
    a.unwrap_or(i64::MAX).cmp(&b.unwrap_or(i64::MAX))
}

fn compare_hosts(a: &SshHostEntry, b: &SshHostEntry) -> Ordering {
    compare_sort_order(
        a.metadata.as_ref().and_then(|m| m.sort_order),
        b.metadata.as_ref().and_then(|m| m.sort_order),
    )
    .then_with(|| locale_compare(host_display_name(a), host_display_name(b)))
}

/// The file name a config-file group is labelled with, minus `.conf`/`.config`.
fn config_file_label(file: &str) -> String {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    name.strip_suffix(".conf")
        .or_else(|| name.strip_suffix(".config"))
        .unwrap_or(name)
        .to_string()
}

/// Custom Muxus groups take priority. Hosts without one retain the useful
/// config-file grouping, so existing users do not lose structure.
pub fn group_hosts<'a>(
    hosts: &'a [SshHostEntry],
    files: &[String],
    root_file: Option<&str>,
    filter: &str,
    has_additional_custom_groups: bool,
) -> Vec<HostGroup<'a>> {
    let mut custom: Vec<HostGroup<'a>> = Vec::new();
    let mut custom_index: HashMap<String, usize> = HashMap::new();
    let mut by_file: Vec<(String, Vec<&'a SshHostEntry>)> = Vec::new();
    let normalized = filter.trim().to_lowercase();
    let tokens = search_tokens(&normalized);

    for host in hosts {
        if !matches_host_tokens(host, &tokens) {
            continue;
        }
        let group = host
            .metadata
            .as_ref()
            .and_then(|m| m.group.as_deref())
            .map(str::trim)
            .filter(|group| !group.is_empty());
        if let Some(group) = group {
            let key = group.to_lowercase();
            let index = *custom_index.entry(key.clone()).or_insert_with(|| {
                custom.push(HostGroup {
                    key: format!("custom:{key}"),
                    label: group.to_string(),
                    kind: GroupKind::Custom,
                    tooltip: None,
                    hosts: Vec::new(),
                });
                custom.len() - 1
            });
            custom[index].hosts.push(host);
        } else if let Some((_, list)) = by_file.iter_mut().find(|(file, _)| *file == host.file) {
            list.push(host);
        } else {
            by_file.push((host.file.clone(), vec![host]));
        }
    }

    custom.sort_by(|a, b| locale_compare(&a.label, &b.label));
    for group in &mut custom {
        group.hosts.sort_by(|a, b| compare_hosts(a, b));
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

    let file_groups = by_file.into_iter().map(|(file, mut list)| {
        let filename = config_file_label(&file);
        let root = Some(file.as_str()) == root_file;
        list.sort_by(|a, b| compare_hosts(a, b));
        HostGroup {
            key: format!("file:{file}"),
            label: match (has_custom_groups, root) {
                (true, true) => "Ungrouped".to_string(),
                (true, false) => format!("Ungrouped · {filename}"),
                (false, true) => "Hosts".to_string(),
                (false, false) => filename,
            },
            kind: GroupKind::File,
            tooltip: Some(file),
            hosts: list,
        }
    });

    custom.into_iter().chain(file_groups).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HostMetadata, ResolvedHostSettings};

    #[derive(Default)]
    struct Opts<'a> {
        file: Option<&'a str>,
        group: Option<&'a str>,
        display_name: Option<&'a str>,
        sort_order: Option<i64>,
    }

    fn host(alias: &str, options: Opts) -> SshHostEntry {
        let has_metadata = options.group.is_some()
            || options.display_name.is_some()
            || options.sort_order.is_some();
        SshHostEntry {
            alias: alias.into(),
            aliases: vec![alias.into()],
            file: options.file.unwrap_or("/home/test/.ssh/config").into(),
            resolved: ResolvedHostSettings {
                hostname: format!("{alias}.example.com"),
                port: 22,
                ..Default::default()
            },
            metadata: has_metadata.then(|| HostMetadata {
                profile_id: alias.into(),
                group: options.group.map(Into::into),
                display_name: options.display_name.map(Into::into),
                sort_order: options.sort_order,
                ..Default::default()
            }),
            description: None,
            options: Default::default(),
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn builds_same_group_and_cross_group_drop_orders() {
        assert_eq!(
            host_order_after_drop(
                &strings(&["alpha", "bravo", "charlie"]),
                &strings(&["alpha"]),
                Some("bravo"),
                DropEdge::After
            ),
            ["bravo", "alpha", "charlie"]
        );
        assert_eq!(
            host_order_after_drop(
                &strings(&["alpha", "bravo"]),
                &strings(&["charlie"]),
                Some("alpha"),
                DropEdge::Before
            ),
            ["charlie", "alpha", "bravo"]
        );
        assert_eq!(
            host_order_after_drop(
                &strings(&["alpha", "bravo"]),
                &strings(&["charlie"]),
                None,
                DropEdge::After
            ),
            ["alpha", "bravo", "charlie"]
        );
    }

    #[test]
    fn lands_several_dropped_hosts_together_in_the_order_given() {
        assert_eq!(
            host_order_after_drop(
                &strings(&["a", "b", "c", "d", "e"]),
                &strings(&["a", "x", "d"]),
                Some("c"),
                DropEdge::After
            ),
            ["b", "c", "a", "x", "d", "e"]
        );
        assert_eq!(
            host_order_after_drop(
                &strings(&["a", "b", "c"]),
                &strings(&["c", "a"]),
                Some("b"),
                DropEdge::Before
            ),
            ["c", "a", "b"]
        );
        assert_eq!(
            host_order_after_drop(
                &strings(&["a", "b"]),
                &strings(&["x", "y"]),
                None,
                DropEdge::After
            ),
            ["a", "b", "x", "y"]
        );
    }

    #[test]
    fn puts_custom_groups_first_while_retaining_config_file_structure() {
        let hosts = [
            host("plain", Opts::default()),
            host(
                "database",
                Opts {
                    group: Some("Production"),
                    ..Default::default()
                },
            ),
            host(
                "api",
                Opts {
                    group: Some("Production"),
                    ..Default::default()
                },
            ),
            host(
                "lab",
                Opts {
                    file: Some("/home/test/.ssh/config.d/lab.conf"),
                    ..Default::default()
                },
            ),
        ];
        let files = strings(&[
            "/home/test/.ssh/config",
            "/home/test/.ssh/config.d/lab.conf",
        ]);
        let groups = group_hosts(&hosts, &files, Some("/home/test/.ssh/config"), "", false);
        let labels: Vec<_> = groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["Production", "Ungrouped", "Ungrouped · lab"]);
        let aliases: Vec<_> = groups[0].hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(aliases, ["api", "database"]);
    }

    #[test]
    fn searches_group_and_display_names() {
        let hosts = [
            host(
                "db-01",
                Opts {
                    group: Some("Production"),
                    display_name: Some("Primary database"),
                    ..Default::default()
                },
            ),
            host(
                "web-01",
                Opts {
                    group: Some("Staging"),
                    ..Default::default()
                },
            ),
        ];
        assert_eq!(
            group_hosts(&hosts, &[], None, "production", false)[0].hosts[0].alias,
            "db-01"
        );
        assert_eq!(
            group_hosts(&hosts, &[], None, "primary", false)[0].hosts[0].alias,
            "db-01"
        );
    }

    #[test]
    fn uses_a_persisted_drag_order_ahead_of_display_names() {
        let hosts = [
            host(
                "alpha",
                Opts {
                    sort_order: Some(2),
                    ..Default::default()
                },
            ),
            host(
                "bravo",
                Opts {
                    sort_order: Some(0),
                    ..Default::default()
                },
            ),
            host(
                "charlie",
                Opts {
                    sort_order: Some(1),
                    ..Default::default()
                },
            ),
        ];
        let files = strings(&["/home/test/.ssh/config"]);
        let groups = group_hosts(&hosts, &files, Some("/home/test/.ssh/config"), "", false);
        let aliases: Vec<_> = groups[0].hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(aliases, ["bravo", "charlie", "alpha"]);
    }

    // host-search.test.ts: token matching through groupHosts.
    #[test]
    fn matches_every_token_separately_across_name_and_folder() {
        let hosts = [
            host(
                "MyAirframe1Tail",
                Opts {
                    group: Some("AF-Tails"),
                    ..Default::default()
                },
            ),
            host(
                "MyAirframe1",
                Opts {
                    group: Some("AFs"),
                    ..Default::default()
                },
            ),
        ];
        let root = "/home/test/.ssh/config";
        let aliases: Vec<_> = group_hosts(&hosts, &strings(&[root]), Some(root), "af tail", false)
            .iter()
            .flat_map(|g| g.hosts.iter().map(|h| h.alias.clone()))
            .collect();
        assert_eq!(aliases, ["MyAirframe1Tail"]);
    }

    #[test]
    fn keeps_matching_a_plain_substring_anywhere_in_the_host() {
        let mut router = host("router", Opts::default());
        router.resolved.hostname = "edge01.lab.test".into();
        let hosts = [router, host("switch", Opts::default())];
        let root = "/home/test/.ssh/config";
        let aliases: Vec<_> = group_hosts(&hosts, &strings(&[root]), Some(root), "edge01", false)
            .iter()
            .flat_map(|g| g.hosts.iter().map(|h| h.alias.clone()))
            .collect();
        assert_eq!(aliases, ["router"]);
    }

    #[test]
    fn formats_addresses() {
        let mut entry = host("web", Opts::default());
        assert_eq!(host_address(&entry), "web.example.com");
        entry.resolved.user = Some("deploy".into());
        entry.resolved.port = 2222;
        assert_eq!(host_address(&entry), "deploy@web.example.com:2222");
    }
}
