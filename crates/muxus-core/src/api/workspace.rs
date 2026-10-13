//! Workspaces (saved window layouts), multi-exec groups and terminal snapshots.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::js;
use crate::protocol::SessionProfile;
use crate::validate::{self, Validate, ValidationError, join};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceConnectionSource {
    Openssh,
    Profile,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceConnectionRef {
    pub source: WorkspaceConnectionSource,
    /// OpenSSH alias when source=openssh, stable database profile ID otherwise.
    pub id: String,
}

/// A terminal tab of a saved layout.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTerminalTab {
    pub id: String,
    pub title: String,
    pub profile: SessionProfile,
    pub cwd_hint: Option<String>,
    /// User-set color flag marking the tab.
    pub color: Option<String>,
    /// Pinned tabs stay at the start of their pane's tab strip.
    pub pinned: Option<bool>,
    /// Restoring a layout may offer a fresh connection; it never resumes a process.
    pub offer_reconnect: bool,
}

/// An SFTP or editor tab of a saved layout.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConnectionTab {
    pub id: String,
    pub title: String,
    pub connection: WorkspaceConnectionRef,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceTabLayout {
    Terminal(WorkspaceTerminalTab),
    Sftp(WorkspaceConnectionTab),
    Editor(WorkspaceConnectionTab),
}

impl WorkspaceTabLayout {
    pub fn id(&self) -> &str {
        match self {
            WorkspaceTabLayout::Terminal(tab) => &tab.id,
            WorkspaceTabLayout::Sftp(tab) | WorkspaceTabLayout::Editor(tab) => &tab.id,
        }
    }
}

// Written in zod's output order (`id`, then `kind`) so a saved layout's JSON
// matches what the TypeScript stores byte for byte.
impl Serialize for WorkspaceTabLayout {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[skip_serializing_none]
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Terminal<'a> {
            id: &'a str,
            kind: &'static str,
            title: &'a str,
            profile: &'a SessionProfile,
            cwd_hint: Option<&'a str>,
            color: Option<&'a str>,
            pinned: Option<bool>,
            offer_reconnect: bool,
        }
        #[skip_serializing_none]
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Connection<'a> {
            id: &'a str,
            kind: &'static str,
            title: &'a str,
            connection: &'a WorkspaceConnectionRef,
            path: Option<&'a str>,
        }
        match self {
            WorkspaceTabLayout::Terminal(tab) => Terminal {
                id: &tab.id,
                kind: "terminal",
                title: &tab.title,
                profile: &tab.profile,
                cwd_hint: tab.cwd_hint.as_deref(),
                color: tab.color.as_deref(),
                pinned: tab.pinned,
                offer_reconnect: tab.offer_reconnect,
            }
            .serialize(serializer),
            WorkspaceTabLayout::Sftp(tab) | WorkspaceTabLayout::Editor(tab) => Connection {
                id: &tab.id,
                kind: if matches!(self, WorkspaceTabLayout::Sftp(_)) {
                    "sftp"
                } else {
                    "editor"
                },
                title: &tab.title,
                connection: &tab.connection,
                path: tab.path.as_deref(),
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for WorkspaceTabLayout {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "lowercase")]
        enum Wire {
            Terminal(WorkspaceTerminalTab),
            Sftp(WorkspaceConnectionTab),
            Editor(WorkspaceConnectionTab),
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Terminal(tab) => WorkspaceTabLayout::Terminal(tab),
            Wire::Sftp(tab) => WorkspaceTabLayout::Sftp(tab),
            Wire::Editor(tab) => WorkspaceTabLayout::Editor(tab),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePane {
    pub id: String,
    pub tabs: Vec<WorkspaceTabLayout>,
    pub active_tab_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSplit {
    pub id: String,
    pub direction: SplitDirection,
    /// Fraction occupied by `children[0]`.
    #[serde(with = "js::number")]
    pub ratio: f64,
    pub children: Box<[WorkspaceNode; 2]>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceNode {
    Pane(WorkspacePane),
    Split(WorkspaceSplit),
}

impl WorkspaceNode {
    pub fn id(&self) -> &str {
        match self {
            WorkspaceNode::Pane(pane) => &pane.id,
            WorkspaceNode::Split(split) => &split.id,
        }
    }
}

// zod's output order: `id`, then `type`.
impl Serialize for WorkspaceNode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[skip_serializing_none]
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Pane<'a> {
            id: &'a str,
            #[serde(rename = "type")]
            kind: &'static str,
            tabs: &'a [WorkspaceTabLayout],
            active_tab_id: Option<&'a str>,
        }
        #[derive(Serialize)]
        struct Split<'a> {
            id: &'a str,
            #[serde(rename = "type")]
            kind: &'static str,
            direction: SplitDirection,
            #[serde(with = "js::number")]
            ratio: f64,
            children: &'a [WorkspaceNode; 2],
        }
        match self {
            WorkspaceNode::Pane(pane) => Pane {
                id: &pane.id,
                kind: "pane",
                tabs: &pane.tabs,
                active_tab_id: pane.active_tab_id.as_deref(),
            }
            .serialize(serializer),
            WorkspaceNode::Split(split) => Split {
                id: &split.id,
                kind: "split",
                direction: split.direction,
                ratio: split.ratio,
                children: &split.children,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for WorkspaceNode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "lowercase")]
        enum Wire {
            Pane(WorkspacePane),
            Split(WorkspaceSplit),
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Pane(pane) => WorkspaceNode::Pane(pane),
            Wire::Split(split) => WorkspaceNode::Split(split),
        })
    }
}

/// The only layout version there is; serialized as the number `1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayoutVersion1;

impl Serialize for LayoutVersion1 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(1)
    }
}

impl<'de> Deserialize<'de> for LayoutVersion1 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let version = f64::deserialize(deserializer)?;
        if version == 1.0 {
            Ok(LayoutVersion1)
        } else {
            Err(serde::de::Error::custom("Invalid input: expected 1"))
        }
    }
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLayoutV1 {
    pub version: LayoutVersion1,
    /// `null` for an empty window (always written, never left out).
    #[serialize_always]
    #[serde(serialize_with = "serialize_root")]
    pub root: Option<WorkspaceNode>,
    pub active_pane_id: Option<String>,
}

fn serialize_root<S: serde::Serializer>(
    root: &Option<WorkspaceNode>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match root {
        Some(node) => node.serialize(serializer),
        None => serializer.serialize_none(),
    }
}

impl WorkspaceLayoutV1 {
    /// An empty layout (`{ version: 1, root: null }`).
    pub fn empty() -> Self {
        Self {
            version: LayoutVersion1,
            root: None,
            active_pane_id: None,
        }
    }

    /// Every tab id in the layout, in tree order.
    pub fn tab_ids(&self) -> Vec<&str> {
        let mut ids = Vec::new();
        fn walk<'a>(node: &'a WorkspaceNode, ids: &mut Vec<&'a str>) {
            match node {
                WorkspaceNode::Pane(pane) => {
                    ids.extend(pane.tabs.iter().map(WorkspaceTabLayout::id))
                }
                WorkspaceNode::Split(split) => {
                    walk(&split.children[0], ids);
                    walk(&split.children[1], ids);
                }
            }
        }
        if let Some(root) = &self.root {
            walk(root, &mut ids);
        }
        ids
    }
}

/// A reusable mirrored-input target set owned by one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMultiExecGroup {
    pub id: String,
    pub name: String,
    /// Stable workspace terminal-tab IDs.
    pub tab_ids: Vec<String>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRecord {
    pub id: String,
    pub name: String,
    pub layout: WorkspaceLayoutV1,
    pub multi_exec_groups: Vec<WorkspaceMultiExecGroup>,
    /// Locked workspaces only change through an explicit save.
    pub is_locked: bool,
    /// At most one workspace is selected for startup.
    pub is_startup: bool,
    pub created_at: String,
    pub updated_at: String,
    pub last_opened_at: Option<String>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub id: String,
    pub name: String,
    pub is_locked: bool,
    pub is_startup: bool,
    pub created_at: String,
    pub updated_at: String,
    pub last_opened_at: Option<String>,
}

impl From<&WorkspaceRecord> for WorkspaceSummary {
    fn from(record: &WorkspaceRecord) -> Self {
        Self {
            id: record.id.clone(),
            name: record.name.clone(),
            is_locked: record.is_locked,
            is_startup: record.is_startup,
            created_at: record.created_at.clone(),
            updated_at: record.updated_at.clone(),
            last_opened_at: record.last_opened_at.clone(),
        }
    }
}

/// Persisted scrollback for one workspace terminal tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshotRecord {
    pub tab_id: String,
    /// Serialized terminal buffer, replayable by writing it back to a terminal.
    pub data: String,
    /// Encoding version used to distinguish legacy unmarked UI dividers.
    pub format_version: u32,
    pub updated_at: String,
}

/// Upper bound for one tab's serialized scrollback, enforced on both sides.
pub const TERMINAL_SNAPSHOT_MAX_CHARS: usize = 512_000;
pub const TERMINAL_SNAPSHOT_FORMAT_VERSION: u32 = 2;

/// Body of PUT /api/workspaces (`workspaceSaveSchema`).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSaveRequest {
    pub id: Option<String>,
    pub name: String,
    pub layout: WorkspaceLayoutV1,
    #[serde(default)]
    pub overwrite_locked: bool,
    #[serde(default)]
    pub create_if_missing: bool,
    #[serde(default)]
    pub allocate_default_name: bool,
    #[serde(default)]
    pub multi_exec_groups: Vec<WorkspaceMultiExecGroup>,
}

impl Validate for WorkspaceConnectionRef {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::string(&join(path, "id"), &self.id, 1, usize::MAX)
    }
}

impl Validate for WorkspaceTabLayout {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        match self {
            WorkspaceTabLayout::Terminal(tab) => {
                validate::string(&join(path, "id"), &tab.id, 1, usize::MAX)?;
                validate::string(&join(path, "title"), &tab.title, 0, 500)?;
                tab.profile.validate_at(&join(path, "profile"))?;
                validate::opt_string(&join(path, "color"), &tab.color, 0, 32)
            }
            WorkspaceTabLayout::Sftp(tab) | WorkspaceTabLayout::Editor(tab) => {
                validate::string(&join(path, "id"), &tab.id, 1, usize::MAX)?;
                validate::string(&join(path, "title"), &tab.title, 0, 500)?;
                tab.connection.validate_at(&join(path, "connection"))
            }
        }
    }
}

impl Validate for WorkspaceNode {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        match self {
            WorkspaceNode::Pane(pane) => {
                validate::string(&join(path, "id"), &pane.id, 1, usize::MAX)?;
                pane.tabs.validate_at(&join(path, "tabs"))
            }
            WorkspaceNode::Split(split) => {
                validate::string(&join(path, "id"), &split.id, 1, usize::MAX)?;
                validate::range(&join(path, "ratio"), split.ratio, 0.1, 0.9)?;
                let [first, second] = &mut *split.children;
                first.validate_at(&join(path, "children.0"))?;
                second.validate_at(&join(path, "children.1"))
            }
        }
    }
}

impl Validate for WorkspaceLayoutV1 {
    /// `workspaceLayoutSchema`: unique node and tab ids, an active tab inside
    /// its pane, and an active pane that exists.
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        let Some(root) = &mut self.root else {
            return Ok(());
        };
        root.validate_at(&join(path, "root"))?;
        let mut node_ids = HashSet::new();
        let mut pane_ids = HashSet::new();
        let mut tab_ids = HashSet::new();
        fn walk<'a>(
            node: &'a WorkspaceNode,
            path: &str,
            node_ids: &mut HashSet<&'a str>,
            pane_ids: &mut HashSet<&'a str>,
            tab_ids: &mut HashSet<&'a str>,
        ) -> Result<(), ValidationError> {
            if !node_ids.insert(node.id()) {
                return Err(ValidationError::new(
                    path,
                    format!("duplicate workspace node id \"{}\"", node.id()),
                ));
            }
            match node {
                WorkspaceNode::Split(split) => {
                    walk(&split.children[0], path, node_ids, pane_ids, tab_ids)?;
                    walk(&split.children[1], path, node_ids, pane_ids, tab_ids)
                }
                WorkspaceNode::Pane(pane) => {
                    pane_ids.insert(&pane.id);
                    let mut local = HashSet::new();
                    for tab in &pane.tabs {
                        if !tab_ids.insert(tab.id()) {
                            return Err(ValidationError::new(
                                path,
                                format!("duplicate workspace tab id \"{}\"", tab.id()),
                            ));
                        }
                        local.insert(tab.id());
                    }
                    if let Some(active) = pane.active_tab_id.as_deref().filter(|id| !id.is_empty())
                        && !local.contains(active)
                    {
                        return Err(ValidationError::new(
                            path,
                            format!("active tab \"{active}\" is not in pane \"{}\"", pane.id),
                        ));
                    }
                    Ok(())
                }
            }
        }
        walk(root, path, &mut node_ids, &mut pane_ids, &mut tab_ids)?;
        if let Some(active) = self.active_pane_id.as_deref().filter(|id| !id.is_empty())
            && !pane_ids.contains(active)
        {
            return Err(ValidationError::new(
                path,
                format!("active pane \"{active}\" does not exist"),
            ));
        }
        Ok(())
    }
}

impl Validate for WorkspaceSaveRequest {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::opt_string(&join(path, "id"), &self.id, 1, usize::MAX)?;
        validate::trimmed(&join(path, "name"), &mut self.name, 1, 200)?;
        self.layout.validate_at(&join(path, "layout"))?;
        for (index, group) in self.multi_exec_groups.iter_mut().enumerate() {
            let at = join(path, &format!("multiExecGroups.{index}"));
            validate::string(&join(&at, "id"), &group.id, 1, usize::MAX)?;
            validate::trimmed(&join(&at, "name"), &mut group.name, 1, 200)?;
            validate::items(&join(&at, "tabIds"), group.tab_ids.len(), 2, usize::MAX)?;
            validate::each_string(&join(&at, "tabIds"), &group.tab_ids, 1, usize::MAX)?;
            if group.tab_ids.iter().collect::<HashSet<_>>().len() != group.tab_ids.len() {
                return Err(ValidationError::new(
                    at,
                    format!(
                        "multi-exec group \"{}\" contains duplicate tabs",
                        group.name
                    ),
                ));
            }
        }
        if self.create_if_missing && self.id.is_none() {
            return Err(ValidationError::new(
                path,
                "createIfMissing requires a workspace id",
            ));
        }
        if self.allocate_default_name && self.id.is_some() {
            return Err(ValidationError::new(
                path,
                "allocateDefaultName cannot update a workspace",
            ));
        }
        let mut terminal_tabs = HashSet::new();
        fn collect<'a>(node: &'a WorkspaceNode, into: &mut HashSet<&'a str>) {
            match node {
                WorkspaceNode::Split(split) => {
                    collect(&split.children[0], into);
                    collect(&split.children[1], into);
                }
                WorkspaceNode::Pane(pane) => {
                    for tab in &pane.tabs {
                        if let WorkspaceTabLayout::Terminal(tab) = tab {
                            into.insert(tab.id.as_str());
                        }
                    }
                }
            }
        }
        if let Some(root) = &self.layout.root {
            collect(root, &mut terminal_tabs);
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for group in &self.multi_exec_groups {
            if !ids.insert(group.id.as_str()) {
                return Err(ValidationError::new(
                    path,
                    format!("duplicate multi-exec group id \"{}\"", group.id),
                ));
            }
            if !names.insert(js::js_trim(&group.name).to_lowercase()) {
                return Err(ValidationError::new(
                    path,
                    format!("duplicate multi-exec group name \"{}\"", group.name),
                ));
            }
            for tab in &group.tab_ids {
                if !terminal_tabs.contains(tab.as_str()) {
                    return Err(ValidationError::new(
                        path,
                        format!(
                            "multi-exec group \"{}\" references unknown terminal tab \"{tab}\"",
                            group.name
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_round_trip_in_zod_key_order() {
        let json = r#"{"version":1,"root":{"id":"s","type":"split","direction":"horizontal","ratio":0.5,"children":[{"id":"p1","type":"pane","tabs":[{"id":"t1","kind":"terminal","title":"web","profile":{"kind":"ssh","target":"web"},"offerReconnect":true}],"activeTabId":"t1"},{"id":"p2","type":"pane","tabs":[{"id":"t2","kind":"sftp","title":"files","connection":{"source":"openssh","id":"web"},"path":"/tmp"}]}]},"activePaneId":"p1"}"#;
        let layout: WorkspaceLayoutV1 = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&layout).unwrap(), json);
        assert_eq!(layout.tab_ids(), vec!["t1", "t2"]);
        assert_eq!(
            serde_json::to_string(&WorkspaceLayoutV1::empty()).unwrap(),
            r#"{"version":1,"root":null}"#
        );
    }

    #[test]
    fn save_requests_check_ids_and_groups() {
        let body = serde_json::json!({
            "name": "  Dev ",
            "layout": {"version": 1, "root": {"id": "p", "type": "pane", "tabs": [
                {"id": "a", "kind": "terminal", "title": "a", "profile": {"kind": "local"}, "offerReconnect": true},
                {"id": "b", "kind": "terminal", "title": "b", "profile": {"kind": "local"}, "offerReconnect": true}
            ]}},
            "multiExecGroups": [{"id": "g", "name": "both", "tabIds": ["a", "b"]}]
        });
        let parsed: WorkspaceSaveRequest = validate::parse_value(body.clone()).unwrap();
        assert_eq!(parsed.name, "Dev");
        let mut unknown = body.clone();
        unknown["multiExecGroups"][0]["tabIds"] = serde_json::json!(["a", "zzz"]);
        assert!(validate::parse_value::<WorkspaceSaveRequest>(unknown).is_err());
        let mut duplicate = body;
        duplicate["layout"]["root"]["tabs"][1]["id"] = serde_json::json!("a");
        assert!(validate::parse_value::<WorkspaceSaveRequest>(duplicate).is_err());
    }
}
