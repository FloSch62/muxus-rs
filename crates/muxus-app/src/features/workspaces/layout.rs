//! A window's layout as a saved workspace stores it: conversion between the
//! app's pane tree ([`crate::state::workspace_layout::WorkspaceLayout`]) and
//! the wire type the backend saves ([`WorkspaceLayoutV1`]), and the snapshot
//! the autosave compares (`currentSnapshot` in `workspace-persistence.ts`).

use std::collections::HashSet;

use muxus_core::api::{
    LayoutVersion1, SplitDirection as CoreSplit, WorkspaceConnectionRef as CoreConnection,
    WorkspaceConnectionSource, WorkspaceConnectionTab, WorkspaceLayoutV1, WorkspaceMultiExecGroup,
    WorkspaceNode as CoreNode, WorkspacePane, WorkspaceSplit, WorkspaceTabLayout as CoreTab,
    WorkspaceTerminalTab,
};

use crate::features::command_buttons::multi_exec::MultiExecState;
use crate::state::tabs::TabsState;
use crate::state::workspace_layout::{
    PersistableTerminalTab, SplitDirection, WorkspaceConnectionRef, WorkspaceLayout, WorkspaceNode,
    WorkspaceTabLayout, serialize_workspace,
};

fn connection_to_core(connection: &WorkspaceConnectionRef) -> CoreConnection {
    CoreConnection {
        source: if connection.source == "profile" {
            WorkspaceConnectionSource::Profile
        } else {
            WorkspaceConnectionSource::Openssh
        },
        id: connection.id.clone(),
    }
}

fn connection_from_core(connection: &CoreConnection) -> WorkspaceConnectionRef {
    WorkspaceConnectionRef {
        source: match connection.source {
            WorkspaceConnectionSource::Profile => "profile".into(),
            WorkspaceConnectionSource::Openssh => "openssh".into(),
        },
        id: connection.id.clone(),
    }
}

fn tab_to_core(tab: &WorkspaceTabLayout) -> CoreTab {
    match tab {
        WorkspaceTabLayout::Terminal {
            id,
            title,
            profile,
            cwd_hint,
            color,
            pinned,
            offer_reconnect,
        } => CoreTab::Terminal(WorkspaceTerminalTab {
            id: id.clone(),
            title: title.clone(),
            profile: (**profile).clone(),
            cwd_hint: cwd_hint.clone(),
            color: color.clone(),
            pinned: *pinned,
            offer_reconnect: *offer_reconnect,
        }),
        WorkspaceTabLayout::Sftp {
            id,
            title,
            connection,
            path,
        } => CoreTab::Sftp(WorkspaceConnectionTab {
            id: id.clone(),
            title: title.clone(),
            connection: connection_to_core(connection),
            path: path.clone(),
        }),
        WorkspaceTabLayout::Editor {
            id,
            title,
            connection,
            path,
        } => CoreTab::Editor(WorkspaceConnectionTab {
            id: id.clone(),
            title: title.clone(),
            connection: connection_to_core(connection),
            path: path.clone(),
        }),
    }
}

fn tab_from_core(tab: &CoreTab) -> WorkspaceTabLayout {
    match tab {
        CoreTab::Terminal(tab) => WorkspaceTabLayout::Terminal {
            id: tab.id.clone(),
            title: tab.title.clone(),
            profile: Box::new(tab.profile.clone()),
            cwd_hint: tab.cwd_hint.clone(),
            color: tab.color.clone(),
            pinned: tab.pinned,
            offer_reconnect: tab.offer_reconnect,
        },
        CoreTab::Sftp(tab) => WorkspaceTabLayout::Sftp {
            id: tab.id.clone(),
            title: tab.title.clone(),
            connection: connection_from_core(&tab.connection),
            path: tab.path.clone(),
        },
        CoreTab::Editor(tab) => WorkspaceTabLayout::Editor {
            id: tab.id.clone(),
            title: tab.title.clone(),
            connection: connection_from_core(&tab.connection),
            path: tab.path.clone(),
        },
    }
}

fn node_to_core(node: &WorkspaceNode) -> CoreNode {
    match node {
        WorkspaceNode::Pane {
            id,
            tabs,
            active_tab_id,
        } => CoreNode::Pane(WorkspacePane {
            id: id.clone(),
            tabs: tabs.iter().map(tab_to_core).collect(),
            active_tab_id: active_tab_id.clone(),
        }),
        WorkspaceNode::Split {
            id,
            direction,
            ratio,
            children,
        } => CoreNode::Split(WorkspaceSplit {
            id: id.clone(),
            direction: match direction {
                SplitDirection::Horizontal => CoreSplit::Horizontal,
                SplitDirection::Vertical => CoreSplit::Vertical,
            },
            ratio: *ratio,
            children: Box::new([node_to_core(&children[0]), node_to_core(&children[1])]),
        }),
    }
}

fn node_from_core(node: &CoreNode) -> WorkspaceNode {
    match node {
        CoreNode::Pane(pane) => WorkspaceNode::Pane {
            id: pane.id.clone(),
            tabs: pane.tabs.iter().map(tab_from_core).collect(),
            active_tab_id: pane.active_tab_id.clone(),
        },
        CoreNode::Split(split) => WorkspaceNode::Split {
            id: split.id.clone(),
            direction: match split.direction {
                CoreSplit::Horizontal => SplitDirection::Horizontal,
                CoreSplit::Vertical => SplitDirection::Vertical,
            },
            ratio: split.ratio,
            children: Box::new([
                node_from_core(&split.children[0]),
                node_from_core(&split.children[1]),
            ]),
        },
    }
}

/// The app's layout as the backend stores it.
pub fn layout_to_core(layout: &WorkspaceLayout) -> WorkspaceLayoutV1 {
    WorkspaceLayoutV1 {
        version: LayoutVersion1,
        root: layout.root.as_ref().map(node_to_core),
        active_pane_id: layout.active_pane_id.clone(),
    }
}

/// A stored layout as the app's pane tree.
pub fn layout_from_core(layout: &WorkspaceLayoutV1) -> WorkspaceLayout {
    WorkspaceLayout {
        root: layout.root.as_ref().map(node_from_core),
        active_pane_id: layout.active_pane_id.clone(),
    }
}

/// What the autosave writes, and the text it compares to tell a change.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceSnapshot {
    pub layout: WorkspaceLayoutV1,
    pub multi_exec_groups: Vec<WorkspaceMultiExecGroup>,
    /// `JSON.stringify({ layout, multiExecGroups })`.
    pub serialized: String,
}

impl WorkspaceSnapshot {
    pub fn new(layout: WorkspaceLayoutV1, multi_exec_groups: Vec<WorkspaceMultiExecGroup>) -> Self {
        let serialized = serialize(&layout, &multi_exec_groups);
        Self {
            layout,
            multi_exec_groups,
            serialized,
        }
    }
}

/// `JSON.stringify({ layout, multiExecGroups })` of a stored workspace.
pub fn serialize(layout: &WorkspaceLayoutV1, groups: &[WorkspaceMultiExecGroup]) -> String {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Persisted<'a> {
        layout: &'a WorkspaceLayoutV1,
        multi_exec_groups: &'a [WorkspaceMultiExecGroup],
    }
    serde_json::to_string(&Persisted {
        layout,
        multi_exec_groups: groups,
    })
    .unwrap_or_default()
}

/// `currentSnapshot`: the window's session tabs and the multi-exec groups
/// that still name two of them.
pub fn window_snapshot(tabs: &TabsState, multi_exec: &MultiExecState) -> WorkspaceSnapshot {
    let session_tabs: Vec<PersistableTerminalTab> = tabs
        .tabs
        .iter()
        .filter_map(|tab| {
            Some(PersistableTerminalTab {
                id: tab.id.clone(),
                pane_id: tab.pane_id.clone(),
                title: tab.title.clone(),
                profile: tab.profile.clone()?,
                color: tab.color.clone(),
                pinned: tab.pinned,
            })
        })
        .collect();
    let layout = serialize_workspace(&tabs.root, &session_tabs, &tabs.active_pane_id);
    let ids: HashSet<&str> = session_tabs.iter().map(|tab| tab.id.as_str()).collect();
    WorkspaceSnapshot::new(layout_to_core(&layout), multi_exec.persistable_groups(&ids))
}

#[cfg(test)]
mod tests {
    use muxus_core::protocol::{SessionProfile, SshProfile};

    use super::*;
    use crate::state::tabs::IdGen;
    use crate::state::workspace_layout::PaneDirection;

    #[test]
    fn round_trips_a_split_window_through_the_stored_form() {
        let mut tabs = TabsState::with_pane("pane-a", IdGen::sequential());
        let web = tabs.open(SessionProfile::Ssh(SshProfile::new("web-01")), "web-01");
        tabs.split("pane-a", PaneDirection::Right);
        let db = tabs.open(SessionProfile::Ssh(SshProfile::new("db-01")), "db-01");
        let mut multi = MultiExecState::default();
        multi.set_selection([web.clone(), db.clone()]);
        multi.save_group("Both", None);
        multi.save_group("Gone", Some(&[web.clone(), "tab-closed".into()]));

        let snapshot = window_snapshot(&tabs, &multi);
        assert_eq!(
            snapshot.multi_exec_groups.len(),
            1,
            "a group naming one live tab is dropped"
        );
        assert_eq!(snapshot.layout.tab_ids(), [web.as_str(), db.as_str()]);
        let json: serde_json::Value = serde_json::from_str(&snapshot.serialized).unwrap();
        assert_eq!(json["layout"]["version"], 1);
        assert_eq!(json["layout"]["root"]["type"], "split");
        assert_eq!(
            json["layout"]["root"]["children"][0]["tabs"][0]["kind"],
            "terminal"
        );
        assert_eq!(json["multiExecGroups"][0]["name"], "Both");

        let app = layout_from_core(&snapshot.layout);
        assert_eq!(layout_to_core(&app), snapshot.layout);
    }

    #[test]
    fn an_empty_window_stores_a_single_empty_pane() {
        let tabs = TabsState::with_pane("pane-a", IdGen::sequential());
        let snapshot = window_snapshot(&tabs, &MultiExecState::default());
        let json: serde_json::Value = serde_json::from_str(&snapshot.serialized).unwrap();
        assert_eq!(json["layout"]["root"]["type"], "pane");
        assert_eq!(json["multiExecGroups"], serde_json::json!([]));
    }
}
