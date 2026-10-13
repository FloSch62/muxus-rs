//! Filtering and sorting the saved workspace catalog (port of
//! `client/src/workspace-list.ts`), and the default names new workspaces get
//! (`nextWorkspaceName` in `workspace-persistence.ts`).

use std::cmp::Ordering;
use std::collections::HashSet;

use muxus_core::api::WorkspaceSummary;
use muxus_core::collate::locale_compare;
use muxus_core::js::js_trim;

/// The dialog's sort choices (`WorkspaceSort`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkspaceSort {
    #[default]
    Recent,
    Name,
    Created,
}

impl WorkspaceSort {
    pub const ALL: [WorkspaceSort; 3] = [
        WorkspaceSort::Recent,
        WorkspaceSort::Name,
        WorkspaceSort::Created,
    ];

    /// The menu label.
    pub fn label(self) -> &'static str {
        match self {
            WorkspaceSort::Recent => "Recent activity",
            WorkspaceSort::Name => "Name",
            WorkspaceSort::Created => "Date created",
        }
    }
}

/// One run of a name: digits compare by value (`numeric: true`), everything
/// else as text.
enum Chunk<'a> {
    Digits(&'a str),
    Text(&'a str),
}

fn chunks(value: &str) -> Vec<Chunk<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut digits: Option<bool> = None;
    for (index, c) in value.char_indices() {
        let is_digit = c.is_ascii_digit();
        if digits.is_some_and(|previous| previous != is_digit) {
            let piece = &value[start..index];
            out.push(if digits == Some(true) {
                Chunk::Digits(piece)
            } else {
                Chunk::Text(piece)
            });
            start = index;
        }
        digits = Some(is_digit);
    }
    if start < value.len() {
        let piece = &value[start..];
        out.push(if digits == Some(true) {
            Chunk::Digits(piece)
        } else {
            Chunk::Text(piece)
        });
    }
    out
}

fn compare_digits(left: &str, right: &str) -> Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

/// `localeCompare(…, { numeric: true, sensitivity: 'base' })`: case-blind,
/// with digit runs compared as numbers, so "Lab 2" sorts before "Lab 10".
pub fn compare_names_natural(left: &str, right: &str) -> Ordering {
    let (left, right) = (left.to_lowercase(), right.to_lowercase());
    let (a, b) = (chunks(&left), chunks(&right));
    for (x, y) in a.iter().zip(b.iter()) {
        let order = match (x, y) {
            (Chunk::Digits(x), Chunk::Digits(y)) => compare_digits(x, y),
            (Chunk::Digits(x), Chunk::Text(y))
            | (Chunk::Text(x), Chunk::Digits(y))
            | (Chunk::Text(x), Chunk::Text(y)) => locale_compare(x, y),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

fn compare_names(left: &WorkspaceSummary, right: &WorkspaceSummary) -> Ordering {
    compare_names_natural(&left.name, &right.name)
}

fn compare_newest(
    left: &WorkspaceSummary,
    right: &WorkspaceSummary,
    timestamp: impl Fn(&WorkspaceSummary) -> &str,
) -> Ordering {
    timestamp(right)
        .cmp(timestamp(left))
        .then_with(|| compare_names(left, right))
}

/// Filter and sort a catalog without touching the backend-owned order. The
/// active workspace always comes first.
pub fn select_workspaces(
    workspaces: &[WorkspaceSummary],
    query: &str,
    sort: WorkspaceSort,
    active_id: Option<&str>,
) -> Vec<WorkspaceSummary> {
    let query = js_trim(query).to_lowercase();
    let mut visible: Vec<WorkspaceSummary> = workspaces
        .iter()
        .filter(|workspace| query.is_empty() || workspace.name.to_lowercase().contains(&query))
        .cloned()
        .collect();
    visible.sort_by(|left, right| {
        if Some(left.id.as_str()) == active_id {
            return Ordering::Less;
        }
        if Some(right.id.as_str()) == active_id {
            return Ordering::Greater;
        }
        match sort {
            WorkspaceSort::Name => compare_names(left, right),
            WorkspaceSort::Created => compare_newest(left, right, |w| &w.created_at),
            WorkspaceSort::Recent => compare_newest(left, right, |w| {
                w.last_opened_at.as_deref().unwrap_or(&w.updated_at)
            }),
        }
    });
    visible
}

/// `nextWorkspaceName`: "Workspace N" for the first N no workspace is called yet.
pub fn next_workspace_name(workspaces: &[WorkspaceSummary]) -> String {
    let names: HashSet<String> = workspaces
        .iter()
        .map(|workspace| js_trim(&workspace.name).to_lowercase())
        .collect();
    let mut number = 1;
    while names.contains(&format!("workspace {number}")) {
        number += 1;
    }
    format!("Workspace {number}")
}

/// What a row says under the name (`activityLabel`).
pub fn activity_label(workspace: &WorkspaceSummary) -> String {
    match &workspace.last_opened_at {
        Some(opened) => format!(
            "Opened {}",
            crate::features::launcher::time_format::format_timestamp(opened)
        ),
        None => format!(
            "Updated {}",
            crate::features::launcher::time_format::format_timestamp(&workspace.updated_at)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(
        id: &str,
        name: &str,
        created_at: &str,
        updated_at: Option<&str>,
        last_opened_at: Option<&str>,
    ) -> WorkspaceSummary {
        WorkspaceSummary {
            id: id.into(),
            name: name.into(),
            created_at: created_at.into(),
            updated_at: updated_at.unwrap_or(created_at).into(),
            last_opened_at: last_opened_at.map(Into::into),
            is_locked: false,
            is_startup: false,
        }
    }

    fn catalog() -> Vec<WorkspaceSummary> {
        vec![
            workspace("alpha", "Lab 2", "2026-01-01", Some("2026-01-04"), None),
            workspace(
                "beta",
                "Lab 10",
                "2026-01-03",
                Some("2026-01-03"),
                Some("2026-01-05"),
            ),
            workspace(
                "gamma",
                "Production",
                "2026-01-02",
                Some("2026-01-02"),
                None,
            ),
        ]
    }

    fn ids(list: &[WorkspaceSummary]) -> Vec<&str> {
        list.iter().map(|workspace| workspace.id.as_str()).collect()
    }

    #[test]
    fn searches_names_case_insensitively_and_sorts_them_naturally() {
        assert_eq!(
            ids(&select_workspaces(
                &catalog(),
                "LAB",
                WorkspaceSort::Name,
                None
            )),
            ["alpha", "beta"]
        );
    }

    #[test]
    fn sorts_by_last_activity_and_keeps_the_active_workspace_first() {
        assert_eq!(
            ids(&select_workspaces(
                &catalog(),
                "",
                WorkspaceSort::Recent,
                Some("alpha")
            )),
            ["alpha", "beta", "gamma"]
        );
    }

    #[test]
    fn sorts_newly_created_workspaces_first_without_mutating_the_catalog() {
        let original = catalog();
        let catalog = catalog();
        assert_eq!(
            ids(&select_workspaces(
                &catalog,
                "",
                WorkspaceSort::Created,
                None
            )),
            ["beta", "gamma", "alpha"]
        );
        assert_eq!(catalog, original);
    }

    #[test]
    fn names_new_workspaces_after_the_first_free_number() {
        let catalog = vec![
            workspace("a", "Workspace 1", "2026-01-01", None, None),
            workspace("b", " workspace 2 ", "2026-01-01", None, None),
        ];
        assert_eq!(next_workspace_name(&catalog), "Workspace 3");
        assert_eq!(next_workspace_name(&[]), "Workspace 1");
    }

    #[test]
    fn compares_names_by_number_and_without_case() {
        assert_eq!(compare_names_natural("lab 2", "Lab 10"), Ordering::Less);
        assert_eq!(compare_names_natural("Lab", "lab"), Ordering::Equal);
        assert_eq!(compare_names_natural("a", "b"), Ordering::Less);
        assert_eq!(compare_names_natural("x02", "x2"), Ordering::Equal);
    }
}
