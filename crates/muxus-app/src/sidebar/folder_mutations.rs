//! Which hosts a folder edit touches, and where each lands. Port of
//! `client/src/components/sidebar/folder-mutations.ts`.

use crate::hosts::ManagedHost;
use crate::hosts::tree::{
    MAX_FOLDER_DEPTH, MAX_GROUP_PATH, folder_depth, folder_parent_path, folder_path,
    folder_segments, is_descendant_path, is_same_path, normalize_group_path, rename_folder_path,
};

/// One host's group after a folder operation; `None` clears it to the root.
#[derive(Clone, Debug, PartialEq)]
pub struct FolderMove {
    pub host: ManagedHost,
    pub group: Option<String>,
}

/// Which hosts a rename or re-parent touches, and where each lands.
///
/// Always call this with the *unfiltered* host lists. Computing it from the
/// filtered sidebar would silently leave every non-matching host behind on the
/// old path, splitting one folder into two.
pub fn folder_rewrite_plan(hosts: &[ManagedHost], from: &str, to: &str) -> Vec<FolderMove> {
    let target = normalize_group_path(Some(to));
    // Compared exactly rather than by folder identity: folders match
    // case-insensitively, but capitalisation is still part of the name, so
    // "prod" → "Prod" is a rename the sidebar has to carry out.
    if target.is_empty() || target == normalize_group_path(Some(from)) {
        return Vec::new();
    }
    hosts
        .iter()
        .filter_map(|host| {
            let current = host.group().filter(|group| !group.is_empty())?;
            let next = rename_folder_path(current, from, &target)?;
            (next != current).then(|| FolderMove {
                host: host.clone(),
                group: Some(next),
            })
        })
        .collect()
}

/// Deleting a folder lifts its contents one level up rather than dropping them:
/// removing `Production/EU` leaves its hosts in `Production` and promotes
/// `Production/EU/Edge` to `Production/Edge`.
pub fn delete_folder_plan(hosts: &[ManagedHost], path: &str) -> Vec<FolderMove> {
    let parent_segments = folder_segments(Some(&folder_parent_path(path)));
    let depth = folder_depth(path);
    hosts
        .iter()
        .filter_map(|host| {
            let current = host.group().filter(|group| !group.is_empty())?;
            if !is_same_path(current, path) && !is_descendant_path(current, path) {
                return None;
            }
            let mut segments = parent_segments.clone();
            segments.extend(folder_segments(Some(current)).into_iter().skip(depth));
            let next = folder_path(&segments);
            Some(FolderMove {
                host: host.clone(),
                group: (!next.is_empty()).then_some(next),
            })
        })
        .collect()
}

/// Assign hosts to a folder; an empty path moves them to the root. Hosts that
/// are already in it have nothing to write and are left out.
pub fn move_hosts_plan(hosts: &[ManagedHost], path: &str) -> Vec<FolderMove> {
    let normalized = normalize_group_path(Some(path));
    let group = (!normalized.is_empty()).then_some(normalized);
    hosts
        .iter()
        .filter(|host| !is_same_path(host.group().unwrap_or(""), path))
        .map(|host| FolderMove {
            host: host.clone(),
            group: group.clone(),
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderProblem {
    Empty,
    TooDeep,
    TooLong,
    IntoDescendant,
}

/// Why a folder cannot be renamed or moved to `to`, if it cannot.
pub fn folder_target_problem(from: &str, to: &str) -> Option<FolderProblem> {
    let target = normalize_group_path(Some(to));
    if target.is_empty() {
        return Some(FolderProblem::Empty);
    }
    // The server caps the stored string in UTF-16 code units, as JS counts it.
    if target.encode_utf16().count() > MAX_GROUP_PATH {
        return Some(FolderProblem::TooLong);
    }
    if folder_depth(&target) > MAX_FOLDER_DEPTH {
        return Some(FolderProblem::TooDeep);
    }
    if !from.is_empty() && is_descendant_path(&target, from) {
        return Some(FolderProblem::IntoDescendant);
    }
    None
}

pub fn folder_problem_message(problem: FolderProblem, name: &str) -> String {
    match problem {
        FolderProblem::Empty => "Enter a folder name.".into(),
        FolderProblem::TooLong => format!("That path is longer than {MAX_GROUP_PATH} characters."),
        FolderProblem::TooDeep => format!("Folders can nest {MAX_FOLDER_DEPTH} levels deep."),
        FolderProblem::IntoDescendant => format!("A {name} cannot be moved inside itself."),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::hosts::test_support::ssh_host;
    use crate::model::HostMetadata;

    fn host(alias: &str, group: Option<&str>) -> ManagedHost {
        let mut entry = ssh_host(alias, group);
        entry.metadata = Some(HostMetadata {
            profile_id: format!("ssh-{alias}"),
            group: group.map(Into::into),
            ..Default::default()
        });
        ManagedHost::ssh(entry)
    }

    fn hosts() -> Vec<ManagedHost> {
        vec![
            host("a", Some("Prod")),
            host("b", Some("Prod/EU")),
            host("c", Some("Prod/EU/Edge")),
            host("d", Some("Production")),
            host("e", Some("Prodigy/EU")),
            host("f", None),
        ]
    }

    /// Terse view of a plan: alias → resulting group.
    fn summary(plan: &[FolderMove]) -> BTreeMap<String, Option<String>> {
        plan.iter()
            .map(|m| {
                let ManagedHost::Ssh(entry) = &m.host else {
                    unreachable!()
                };
                (entry.alias.clone(), m.group.clone())
            })
            .collect()
    }

    fn expect(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.map(Into::into)))
            .collect()
    }

    #[test]
    fn rewrites_the_folder_and_every_descendant_path() {
        assert_eq!(
            summary(&folder_rewrite_plan(&hosts(), "Prod", "Staging")),
            expect(&[
                ("a", Some("Staging")),
                ("b", Some("Staging/EU")),
                ("c", Some("Staging/EU/Edge"))
            ])
        );
    }

    #[test]
    fn leaves_folders_that_merely_share_a_string_prefix_alone() {
        let touched = summary(&folder_rewrite_plan(&hosts(), "Prod", "Staging"));
        for alias in ["d", "e", "f"] {
            assert!(!touched.contains_key(alias));
        }
    }

    #[test]
    fn re_parents_a_nested_folder() {
        assert_eq!(
            summary(&folder_rewrite_plan(&hosts(), "Prod/EU", "Lab/EU")),
            expect(&[("b", Some("Lab/EU")), ("c", Some("Lab/EU/Edge"))])
        );
    }

    #[test]
    fn merges_into_an_existing_folder_when_the_target_already_exists() {
        assert_eq!(
            summary(&folder_rewrite_plan(&hosts(), "Prodigy", "Prod")),
            expect(&[("e", Some("Prod/EU"))])
        );
    }

    #[test]
    fn plans_nothing_for_an_unused_folder_or_a_no_op_rename() {
        assert!(folder_rewrite_plan(&hosts(), "Nowhere", "Elsewhere").is_empty());
        assert!(folder_rewrite_plan(&hosts(), "Prod", "Prod").is_empty());
        assert!(folder_rewrite_plan(&hosts(), "Prod", " Prod ").is_empty());
        assert!(folder_rewrite_plan(&hosts(), "Prod", "  ").is_empty());
    }

    #[test]
    fn rewrites_a_folder_whose_name_changes_only_in_capitalisation() {
        assert_eq!(
            summary(&folder_rewrite_plan(&hosts(), "Prod", "prod")),
            expect(&[
                ("a", Some("prod")),
                ("b", Some("prod/EU")),
                ("c", Some("prod/EU/Edge"))
            ])
        );
    }

    #[test]
    fn lifts_a_nested_folder_one_level_up() {
        assert_eq!(
            summary(&delete_folder_plan(&hosts(), "Prod/EU")),
            expect(&[("b", Some("Prod")), ("c", Some("Prod/Edge"))])
        );
    }

    #[test]
    fn clears_the_group_entirely_when_a_top_level_folder_goes() {
        assert_eq!(
            summary(&delete_folder_plan(&hosts(), "Prod")),
            expect(&[("a", None), ("b", Some("EU")), ("c", Some("EU/Edge"))])
        );
    }

    #[test]
    fn plans_nothing_for_an_empty_folder() {
        assert!(delete_folder_plan(&hosts(), "Staging").is_empty());
    }

    #[test]
    fn assigns_a_folder_normalizing_the_path() {
        assert_eq!(
            summary(&move_hosts_plan(&hosts()[..2], " /Lab// EU /")),
            expect(&[("a", Some("Lab/EU")), ("b", Some("Lab/EU"))])
        );
    }

    #[test]
    fn clears_the_folder_when_the_path_is_empty() {
        assert_eq!(
            summary(&move_hosts_plan(&hosts()[..1], "   ")),
            expect(&[("a", None)])
        );
    }

    #[test]
    fn leaves_out_hosts_that_are_already_in_the_folder_whatever_its_casing() {
        assert_eq!(
            summary(&move_hosts_plan(&hosts(), "prod/eu")),
            expect(&[
                ("a", Some("prod/eu")),
                ("c", Some("prod/eu")),
                ("d", Some("prod/eu")),
                ("e", Some("prod/eu")),
                ("f", Some("prod/eu")),
            ])
        );
        assert!(move_hosts_plan(&hosts()[5..], "").is_empty());
    }

    #[test]
    fn accepts_an_ordinary_rename_or_re_parent() {
        assert_eq!(folder_target_problem("Prod", "Staging"), None);
        assert_eq!(folder_target_problem("Prod", "Lab/Prod"), None);
    }

    #[test]
    fn rejects_an_empty_target() {
        assert_eq!(
            folder_target_problem("Prod", "  /  "),
            Some(FolderProblem::Empty)
        );
    }

    #[test]
    fn rejects_moving_a_folder_inside_itself() {
        assert_eq!(
            folder_target_problem("Prod", "Prod/Sub"),
            Some(FolderProblem::IntoDescendant)
        );
        // Sharing a prefix is not the same as being a descendant.
        assert_eq!(folder_target_problem("Prod", "Production"), None);
    }

    #[test]
    fn rejects_paths_past_the_depth_and_length_caps() {
        assert_eq!(
            folder_target_problem("", "a/b/c/d/e/f/g/h/i"),
            Some(FolderProblem::TooDeep)
        );
        assert_eq!(
            folder_target_problem("", &"x".repeat(301)),
            Some(FolderProblem::TooLong)
        );
    }

    #[test]
    fn explains_every_problem_it_can_report() {
        for problem in [
            FolderProblem::Empty,
            FolderProblem::TooDeep,
            FolderProblem::TooLong,
            FolderProblem::IntoDescendant,
        ] {
            assert!(
                folder_problem_message(problem, "folder")
                    .chars()
                    .any(|c| !c.is_whitespace())
            );
        }
    }
}
