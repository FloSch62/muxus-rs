//! The pane tree behind the tab canvas (port of `client/src/state/workspace-layout.ts`).
//!
//! A canvas is a binary tree of splits whose leaves are panes; each pane shows
//! one of its tabs. Every operation is a pure function from tree to tree, so the
//! tabs store can rebuild the canvas without ever unmounting a session.

use std::collections::HashMap;

use crate::model::SessionProfile;

/// One pane: a tab strip plus the tab it shows.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneLeaf {
    pub id: String,
    pub active_tab_id: Option<String>,
}

/// Horizontal splits stack left/right; vertical splits stack top/bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

impl SplitDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            SplitDirection::Horizontal => "horizontal",
            SplitDirection::Vertical => "vertical",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SplitNode {
    pub id: String,
    pub direction: SplitDirection,
    /// Fraction occupied by `children[0]`.
    pub ratio: f64,
    pub children: Box<[PaneNode; 2]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaneNode {
    Pane(PaneLeaf),
    Split(SplitNode),
}

impl PaneNode {
    /// A leaf pane with the given id and active tab.
    pub fn pane(id: impl Into<String>, active_tab_id: Option<&str>) -> Self {
        PaneNode::Pane(PaneLeaf {
            id: id.into(),
            active_tab_id: active_tab_id.map(str::to_owned),
        })
    }

    /// A split of two subtrees.
    pub fn split(
        id: impl Into<String>,
        direction: SplitDirection,
        ratio: f64,
        first: PaneNode,
        second: PaneNode,
    ) -> Self {
        PaneNode::Split(SplitNode {
            id: id.into(),
            direction,
            ratio,
            children: Box::new([first, second]),
        })
    }

    pub fn id(&self) -> &str {
        match self {
            PaneNode::Pane(pane) => &pane.id,
            PaneNode::Split(split) => &split.id,
        }
    }

    pub fn is_pane(&self) -> bool {
        matches!(self, PaneNode::Pane(_))
    }

    pub fn as_pane(&self) -> Option<&PaneLeaf> {
        match self {
            PaneNode::Pane(pane) => Some(pane),
            PaneNode::Split(_) => None,
        }
    }

    pub fn as_split(&self) -> Option<&SplitNode> {
        match self {
            PaneNode::Pane(_) => None,
            PaneNode::Split(split) => Some(split),
        }
    }
}

/// Where a new pane goes, or which neighbour to look for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaneDirection {
    Left,
    Right,
    Up,
    Down,
}

impl PaneDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            PaneDirection::Left => "left",
            PaneDirection::Right => "right",
            PaneDirection::Up => "up",
            PaneDirection::Down => "down",
        }
    }
}

pub const PANE_DIRECTIONS: [PaneDirection; 4] = [
    PaneDirection::Left,
    PaneDirection::Right,
    PaneDirection::Up,
    PaneDirection::Down,
];

/// Horizontal splits stack left/right; vertical splits stack top/bottom.
pub fn split_axis(direction: PaneDirection) -> SplitDirection {
    match direction {
        PaneDirection::Left | PaneDirection::Right => SplitDirection::Horizontal,
        PaneDirection::Up | PaneDirection::Down => SplitDirection::Vertical,
    }
}

/// Left/up place the new pane before the existing one.
pub fn places_pane_first(direction: PaneDirection) -> bool {
    matches!(direction, PaneDirection::Left | PaneDirection::Up)
}

/// A live tab as it is written into a saved workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct PersistableTerminalTab {
    pub id: String,
    pub pane_id: String,
    pub title: String,
    pub profile: SessionProfile,
    pub color: Option<String>,
    pub pinned: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RestoredTerminalTab {
    pub id: String,
    pub pane_id: String,
    pub title: String,
    pub profile: SessionProfile,
    pub color: Option<String>,
    pub pinned: bool,
    /// Local shells start fresh immediately; remote sessions dial only when the restore opts in.
    pub connect_on_mount: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RestoreWorkspaceOptions {
    /// Dial restored remote sessions instead of waiting for a key press.
    pub connect_remote: bool,
}

/// A rectangle in canvas fractions: `0..1` on both axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl LayoutRect {
    pub const FULL: LayoutRect = LayoutRect {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlatPane {
    pub pane: PaneLeaf,
    pub rect: LayoutRect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlatDivider {
    pub split_id: String,
    pub direction: SplitDirection,
    /// The whole split's rectangle; the divider sits at `ratio` along its axis.
    pub bounds: LayoutRect,
    pub ratio: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlatPaneLayout {
    pub panes: Vec<FlatPane>,
    pub dividers: Vec<FlatDivider>,
}

/// Flatten a nested split tree into stable, absolute pane rectangles.
pub fn flatten_pane_layout(root: &PaneNode) -> FlatPaneLayout {
    fn visit(node: &PaneNode, rect: LayoutRect, result: &mut FlatPaneLayout) {
        match node {
            PaneNode::Pane(pane) => result.panes.push(FlatPane {
                pane: pane.clone(),
                rect,
            }),
            PaneNode::Split(split) => {
                result.dividers.push(FlatDivider {
                    split_id: split.id.clone(),
                    direction: split.direction,
                    bounds: rect,
                    ratio: split.ratio,
                });
                if split.direction == SplitDirection::Horizontal {
                    let first_width = rect.width * split.ratio;
                    visit(
                        &split.children[0],
                        LayoutRect {
                            width: first_width,
                            ..rect
                        },
                        result,
                    );
                    visit(
                        &split.children[1],
                        LayoutRect {
                            x: rect.x + first_width,
                            width: rect.width - first_width,
                            ..rect
                        },
                        result,
                    );
                } else {
                    let first_height = rect.height * split.ratio;
                    visit(
                        &split.children[0],
                        LayoutRect {
                            height: first_height,
                            ..rect
                        },
                        result,
                    );
                    visit(
                        &split.children[1],
                        LayoutRect {
                            y: rect.y + first_height,
                            height: rect.height - first_height,
                            ..rect
                        },
                        result,
                    );
                }
            }
        }
    }
    let mut result = FlatPaneLayout::default();
    visit(root, LayoutRect::FULL, &mut result);
    result
}

pub fn find_pane<'a>(root: &'a PaneNode, pane_id: &str) -> Option<&'a PaneLeaf> {
    match root {
        PaneNode::Pane(pane) => (pane.id == pane_id).then_some(pane),
        PaneNode::Split(split) => find_pane(&split.children[0], pane_id)
            .or_else(|| find_pane(&split.children[1], pane_id)),
    }
}

pub fn contains_pane(root: &PaneNode, pane_id: &str) -> bool {
    find_pane(root, pane_id).is_some()
}

pub fn first_pane(root: &PaneNode) -> &PaneLeaf {
    match root {
        PaneNode::Pane(pane) => pane,
        PaneNode::Split(split) => first_pane(&split.children[0]),
    }
}

/// Panes in visual reading order: the order keyboard cycling follows.
pub fn panes_in_order(root: &PaneNode) -> Vec<&PaneLeaf> {
    fn collect<'a>(node: &'a PaneNode, out: &mut Vec<&'a PaneLeaf>) {
        match node {
            PaneNode::Pane(pane) => out.push(pane),
            PaneNode::Split(split) => {
                collect(&split.children[0], out);
                collect(&split.children[1], out);
            }
        }
    }
    let mut out = Vec::new();
    collect(root, &mut out);
    out
}

pub fn count_panes(root: &PaneNode) -> usize {
    match root {
        PaneNode::Pane(_) => 1,
        PaneNode::Split(split) => count_panes(&split.children[0]) + count_panes(&split.children[1]),
    }
}

/// The tab on screen in each pane, in reading order. A zoomed pane covers the
/// whole canvas, so while one is zoomed it is the only pane anyone can see:
/// anything that acts on "what is visible" must not reach the panes behind it.
pub fn visible_tab_ids(root: &PaneNode, zoomed_pane_id: Option<&str>) -> Vec<String> {
    let zoomed = zoomed_pane_id.and_then(|id| find_pane(root, id));
    let panes = match zoomed {
        Some(pane) => vec![pane],
        None => panes_in_order(root),
    };
    panes
        .into_iter()
        .filter_map(|pane| pane.active_tab_id.clone())
        .collect()
}

/// Which child of a split a path descends into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    First,
    Second,
}

impl Branch {
    pub fn index(self) -> usize {
        match self {
            Branch::First => 0,
            Branch::Second => 1,
        }
    }

    pub fn other(self) -> Branch {
        match self {
            Branch::First => Branch::Second,
            Branch::Second => Branch::First,
        }
    }
}

/// Splits from the root down to a pane, with the branch each one descends.
pub fn split_path<'a>(root: &'a PaneNode, pane_id: &str) -> Vec<(&'a SplitNode, Branch)> {
    let PaneNode::Split(split) = root else {
        return Vec::new();
    };
    for branch in [Branch::First, Branch::Second] {
        let child = &split.children[branch.index()];
        if !contains_pane(child, pane_id) {
            continue;
        }
        let mut path = vec![(split, branch)];
        path.extend(split_path(child, pane_id));
        return path;
    }
    Vec::new()
}

/// The split a pane shares with its closest sibling subtree.
pub fn parent_split<'a>(root: &'a PaneNode, pane_id: &str) -> Option<(&'a SplitNode, Branch)> {
    split_path(root, pane_id).pop()
}

/// The pane a directional move lands on, chosen by geometry rather than tree
/// shape: candidates must sit on that side and overlap the source pane across
/// the movement axis; the nearest one wins, then the one sharing the most edge.
pub fn neighbor_pane_id(
    root: &PaneNode,
    pane_id: &str,
    direction: PaneDirection,
) -> Option<String> {
    const EPSILON: f64 = 1e-6;
    let FlatPaneLayout { panes, .. } = flatten_pane_layout(root);
    let source = panes.iter().find(|entry| entry.pane.id == pane_id)?;
    let horizontal = split_axis(direction) == SplitDirection::Horizontal;
    let forward = matches!(direction, PaneDirection::Right | PaneDirection::Down);
    // (start, end, crossStart, crossEnd)
    let span = |rect: &LayoutRect| {
        if horizontal {
            (rect.x, rect.x + rect.width, rect.y, rect.y + rect.height)
        } else {
            (rect.y, rect.y + rect.height, rect.x, rect.x + rect.width)
        }
    };
    let from = span(&source.rect);

    let mut best_id: Option<&str> = None;
    let mut best_score = f64::INFINITY;
    for candidate in &panes {
        if candidate.pane.id == pane_id {
            continue;
        }
        let to = span(&candidate.rect);
        let gap = if forward {
            to.0 - from.1
        } else {
            from.0 - to.1
        };
        if gap < -EPSILON {
            continue;
        }
        let overlap = from.3.min(to.3) - from.2.max(to.2);
        if overlap <= EPSILON {
            continue;
        }
        let score = gap * 1_000.0 - overlap;
        if score < best_score {
            best_score = score;
            best_id = Some(&candidate.pane.id);
        }
    }
    best_id.map(str::to_owned)
}

/// Give every pane an equal share of the canvas along its split axis.
pub fn equalize_splits(root: &PaneNode) -> PaneNode {
    match root {
        PaneNode::Pane(_) => root.clone(),
        PaneNode::Split(split) => {
            let first = equalize_splits(&split.children[0]);
            let second = equalize_splits(&split.children[1]);
            let first_count = count_panes(&first) as f64;
            let ratio = clamp_ratio(first_count / (first_count + count_panes(&second) as f64));
            PaneNode::Split(SplitNode {
                id: split.id.clone(),
                direction: split.direction,
                ratio,
                children: Box::new([first, second]),
            })
        }
    }
}

/// Replace the pane with `pane_id` by whatever `update` builds from it.
pub fn update_pane(
    root: &PaneNode,
    pane_id: &str,
    update: impl FnOnce(&PaneLeaf) -> PaneNode,
) -> PaneNode {
    fn walk(
        node: &PaneNode,
        pane_id: &str,
        update: &mut Option<impl FnOnce(&PaneLeaf) -> PaneNode>,
    ) -> PaneNode {
        match node {
            PaneNode::Pane(pane) if pane.id == pane_id => match update.take() {
                Some(update) => update(pane),
                None => node.clone(),
            },
            PaneNode::Pane(_) => node.clone(),
            PaneNode::Split(split) => {
                let first = walk(&split.children[0], pane_id, update);
                let second = walk(&split.children[1], pane_id, update);
                split.with_children(first, second)
            }
        }
    }
    walk(root, pane_id, &mut Some(update))
}

/// Point a pane at a different tab.
pub fn set_active_tab(root: &PaneNode, pane_id: &str, active_tab_id: Option<String>) -> PaneNode {
    update_pane(root, pane_id, |pane| {
        PaneNode::Pane(PaneLeaf {
            id: pane.id.clone(),
            active_tab_id,
        })
    })
}

impl SplitNode {
    /// This split with new children.
    fn with_children(&self, first: PaneNode, second: PaneNode) -> PaneNode {
        PaneNode::Split(SplitNode {
            id: self.id.clone(),
            direction: self.direction,
            ratio: self.ratio,
            children: Box::new([first, second]),
        })
    }
}

pub fn update_split_ratio(root: &PaneNode, split_id: &str, ratio: f64) -> PaneNode {
    match root {
        PaneNode::Pane(_) => root.clone(),
        PaneNode::Split(split) if split.id == split_id => PaneNode::Split(SplitNode {
            ratio: clamp_ratio(ratio),
            ..split.clone()
        }),
        PaneNode::Split(split) => {
            let first = update_split_ratio(&split.children[0], split_id, ratio);
            let second = update_split_ratio(&split.children[1], split_id, ratio);
            split.with_children(first, second)
        }
    }
}

/// Remove an empty pane and collapse its parent split around the sibling.
pub fn remove_pane(root: &PaneNode, pane_id: &str) -> Option<PaneNode> {
    match root {
        PaneNode::Pane(pane) => (pane.id != pane_id).then(|| root.clone()),
        PaneNode::Split(split) => {
            let first = remove_pane(&split.children[0], pane_id);
            let second = remove_pane(&split.children[1], pane_id);
            match (first, second) {
                (None, second) => second,
                (first, None) => first,
                (Some(first), Some(second)) => Some(split.with_children(first, second)),
            }
        }
    }
}

/// A saved host a non-terminal workspace tab refers to (`WorkspaceConnectionRef`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceConnectionRef {
    /// `openssh` or `profile`.
    pub source: String,
    /// OpenSSH alias when source=openssh, stable database profile ID otherwise.
    pub id: String,
}

/// `WorkspaceTabLayout`: one tab as a saved workspace stores it.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkspaceTabLayout {
    Terminal {
        id: String,
        title: String,
        profile: Box<SessionProfile>,
        cwd_hint: Option<String>,
        /// User-set color flag marking the tab.
        color: Option<String>,
        /// Pinned tabs stay at the start of their pane's tab strip.
        pinned: Option<bool>,
        /// Restoring a layout may offer a fresh connection; it never resumes a process.
        offer_reconnect: bool,
    },
    Sftp {
        id: String,
        title: String,
        connection: WorkspaceConnectionRef,
        path: Option<String>,
    },
    Editor {
        id: String,
        title: String,
        connection: WorkspaceConnectionRef,
        path: Option<String>,
    },
}

impl WorkspaceTabLayout {
    pub fn id(&self) -> &str {
        match self {
            WorkspaceTabLayout::Terminal { id, .. }
            | WorkspaceTabLayout::Sftp { id, .. }
            | WorkspaceTabLayout::Editor { id, .. } => id,
        }
    }
}

/// `WorkspaceNode`: the persisted pane tree.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkspaceNode {
    Pane {
        id: String,
        tabs: Vec<WorkspaceTabLayout>,
        active_tab_id: Option<String>,
    },
    Split {
        id: String,
        direction: SplitDirection,
        /// Fraction occupied by children[0].
        ratio: f64,
        children: Box<[WorkspaceNode; 2]>,
    },
}

/// `WorkspaceLayoutV1` (the `version: 1` tag is implied).
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceLayout {
    pub root: Option<WorkspaceNode>,
    pub active_pane_id: Option<String>,
}

pub fn serialize_workspace(
    root: &PaneNode,
    tabs: &[PersistableTerminalTab],
    active_pane_id: &str,
) -> WorkspaceLayout {
    let mut by_pane: HashMap<&str, Vec<&PersistableTerminalTab>> = HashMap::new();
    for tab in tabs {
        by_pane.entry(tab.pane_id.as_str()).or_default().push(tab);
    }
    fn serialize_node(
        node: &PaneNode,
        by_pane: &HashMap<&str, Vec<&PersistableTerminalTab>>,
    ) -> WorkspaceNode {
        match node {
            PaneNode::Split(split) => WorkspaceNode::Split {
                id: split.id.clone(),
                direction: split.direction,
                ratio: split.ratio,
                children: Box::new([
                    serialize_node(&split.children[0], by_pane),
                    serialize_node(&split.children[1], by_pane),
                ]),
            },
            PaneNode::Pane(pane) => {
                let pane_tabs = by_pane
                    .get(pane.id.as_str())
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let active_tab_id = if pane_tabs
                    .iter()
                    .any(|tab| Some(&tab.id) == pane.active_tab_id.as_ref())
                {
                    pane.active_tab_id.clone()
                } else {
                    pane_tabs.first().map(|tab| tab.id.clone())
                };
                WorkspaceNode::Pane {
                    id: pane.id.clone(),
                    tabs: pane_tabs
                        .iter()
                        .map(|tab| WorkspaceTabLayout::Terminal {
                            id: tab.id.clone(),
                            title: tab.title.clone(),
                            profile: Box::new(tab.profile.clone()),
                            cwd_hint: match &tab.profile {
                                SessionProfile::Local(local) => local.cwd.clone(),
                                _ => None,
                            },
                            color: tab.color.clone(),
                            pinned: tab.pinned.then_some(true),
                            offer_reconnect: true,
                        })
                        .collect(),
                    active_tab_id,
                }
            }
        }
    }
    WorkspaceLayout {
        root: Some(serialize_node(root, &by_pane)),
        active_pane_id: Some(active_pane_id.to_owned()),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RestoredWorkspace {
    pub root: PaneNode,
    pub tabs: Vec<RestoredTerminalTab>,
    pub active_pane_id: String,
    pub active_id: Option<String>,
}

pub fn restore_workspace(
    layout: &WorkspaceLayout,
    options: RestoreWorkspaceOptions,
) -> Option<RestoredWorkspace> {
    let root = layout.root.as_ref()?;
    let mut tabs = Vec::new();
    fn restore_node(
        node: &WorkspaceNode,
        options: RestoreWorkspaceOptions,
        tabs: &mut Vec<RestoredTerminalTab>,
    ) -> PaneNode {
        match node {
            WorkspaceNode::Split {
                id,
                direction,
                ratio,
                children,
            } => {
                let first = restore_node(&children[0], options, tabs);
                let second = restore_node(&children[1], options, tabs);
                PaneNode::split(id.clone(), *direction, clamp_ratio(*ratio), first, second)
            }
            WorkspaceNode::Pane {
                id,
                tabs: pane_tabs,
                active_tab_id,
            } => {
                let mut terminal_ids = Vec::new();
                for tab in pane_tabs {
                    let WorkspaceTabLayout::Terminal {
                        id: tab_id,
                        title,
                        profile,
                        color,
                        pinned,
                        offer_reconnect,
                        ..
                    } = tab
                    else {
                        continue;
                    };
                    // Workspaces written before TERM became fixed may still carry an
                    // override; the profile model here has no `term` field, so the
                    // override is already gone by the time a layout reaches this point.
                    let connect_on_mount = matches!(**profile, SessionProfile::Local(_))
                        || (options.connect_remote && *offer_reconnect);
                    terminal_ids.push(tab_id.clone());
                    tabs.push(RestoredTerminalTab {
                        id: tab_id.clone(),
                        pane_id: id.clone(),
                        title: title.clone(),
                        profile: (**profile).clone(),
                        color: color.clone(),
                        pinned: pinned.unwrap_or(false),
                        connect_on_mount,
                    });
                }
                let active = if terminal_ids
                    .iter()
                    .any(|tab_id| Some(tab_id) == active_tab_id.as_ref())
                {
                    active_tab_id.clone()
                } else {
                    terminal_ids.first().cloned()
                };
                PaneNode::Pane(PaneLeaf {
                    id: id.clone(),
                    active_tab_id: active,
                })
            }
        }
    }
    let root = restore_node(root, options, &mut tabs);
    let requested = layout
        .active_pane_id
        .as_deref()
        .and_then(|id| find_pane(&root, id));
    let active_pane = requested.unwrap_or_else(|| first_pane(&root)).clone();
    Some(RestoredWorkspace {
        root,
        tabs,
        active_pane_id: active_pane.id,
        active_id: active_pane.active_tab_id,
    })
}

pub(crate) fn clamp_ratio(ratio: f64) -> f64 {
    ratio.clamp(0.1, 0.9)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, active: Option<&str>) -> PaneNode {
        PaneNode::pane(id, active)
    }

    fn root() -> PaneNode {
        PaneNode::split(
            "split-root",
            SplitDirection::Horizontal,
            0.6,
            pane("pane-left", Some("tab-a")),
            PaneNode::split(
                "split-right",
                SplitDirection::Vertical,
                0.5,
                pane("pane-top", None),
                pane("pane-bottom", Some("tab-b")),
            ),
        )
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> LayoutRect {
        LayoutRect {
            x,
            y,
            width,
            height,
        }
    }

    fn split_of(node: &PaneNode) -> &SplitNode {
        node.as_split().expect("a split")
    }

    #[test]
    fn flattens_nested_splits_into_stable_absolute_pane_rectangles() {
        let flat = flatten_pane_layout(&root());
        let panes: Vec<_> = flat
            .panes
            .iter()
            .map(|entry| (entry.pane.id.as_str(), entry.rect))
            .collect();
        assert_eq!(
            panes,
            vec![
                ("pane-left", rect(0.0, 0.0, 0.6, 1.0)),
                ("pane-top", rect(0.6, 0.0, 0.4, 0.5)),
                ("pane-bottom", rect(0.6, 0.5, 0.4, 0.5)),
            ]
        );
        let dividers: Vec<_> = flat
            .dividers
            .iter()
            .map(|divider| divider.split_id.as_str())
            .collect();
        assert_eq!(dividers, vec!["split-root", "split-right"]);
    }

    #[test]
    fn updates_one_leaf_and_clamps_divider_ratios() {
        let activated = set_active_tab(&root(), "pane-top", Some("tab-c".into()));
        let resized = update_split_ratio(&activated, "split-right", 0.99);
        let right = split_of(&split_of(&resized).children[1]);
        assert_eq!(right.ratio, 0.9);
        assert_eq!(
            right.children[0]
                .as_pane()
                .unwrap()
                .active_tab_id
                .as_deref(),
            Some("tab-c")
        );
    }

    #[test]
    fn collapses_a_parent_split_when_an_empty_pane_is_removed() {
        let without_top = remove_pane(&root(), "pane-top").unwrap();
        let split = split_of(&without_top);
        assert_eq!(split.id, "split-root");
        assert_eq!(split.children[0].id(), "pane-left");
        assert_eq!(split.children[1].id(), "pane-bottom");
    }

    #[test]
    fn counts_the_tab_of_every_pane_on_screen_and_only_the_zoomed_one() {
        assert_eq!(visible_tab_ids(&root(), None), vec!["tab-a", "tab-b"]);
        // A zoomed pane covers the canvas: the rest are behind it, not on screen.
        assert_eq!(visible_tab_ids(&root(), Some("pane-bottom")), vec!["tab-b"]);
        // A pane the zoom outlived leaves the whole canvas visible again.
        assert_eq!(
            visible_tab_ids(&root(), Some("pane-gone")),
            vec!["tab-a", "tab-b"]
        );
    }

    #[test]
    fn follows_what_the_eye_sees_rather_than_the_tree_shape() {
        let root = root();
        // pane-left spans the full height, so both right-hand panes border it.
        assert_eq!(
            neighbor_pane_id(&root, "pane-left", PaneDirection::Right).as_deref(),
            Some("pane-top")
        );
        assert_eq!(
            neighbor_pane_id(&root, "pane-bottom", PaneDirection::Left).as_deref(),
            Some("pane-left")
        );
        assert_eq!(
            neighbor_pane_id(&root, "pane-top", PaneDirection::Down).as_deref(),
            Some("pane-bottom")
        );
        assert_eq!(
            neighbor_pane_id(&root, "pane-bottom", PaneDirection::Up).as_deref(),
            Some("pane-top")
        );
    }

    #[test]
    fn has_no_neighbour_at_the_edges_of_the_canvas() {
        let root = root();
        assert_eq!(
            neighbor_pane_id(&root, "pane-left", PaneDirection::Left),
            None
        );
        assert_eq!(
            neighbor_pane_id(&root, "pane-left", PaneDirection::Up),
            None
        );
        assert_eq!(
            neighbor_pane_id(&root, "pane-top", PaneDirection::Right),
            None
        );
        assert_eq!(
            neighbor_pane_id(&root, "pane-bottom", PaneDirection::Down),
            None
        );
        assert_eq!(
            neighbor_pane_id(&root, "missing", PaneDirection::Left),
            None
        );
    }

    #[test]
    fn skips_panes_that_only_touch_at_a_corner() {
        let stacked = PaneNode::split(
            "split-root",
            SplitDirection::Horizontal,
            0.5,
            PaneNode::split(
                "split-left",
                SplitDirection::Vertical,
                0.5,
                pane("pane-nw", None),
                pane("pane-sw", None),
            ),
            PaneNode::split(
                "split-right",
                SplitDirection::Vertical,
                0.5,
                pane("pane-ne", None),
                pane("pane-se", None),
            ),
        );
        assert_eq!(
            neighbor_pane_id(&stacked, "pane-nw", PaneDirection::Right).as_deref(),
            Some("pane-ne")
        );
        assert_eq!(
            neighbor_pane_id(&stacked, "pane-sw", PaneDirection::Right).as_deref(),
            Some("pane-se")
        );
    }

    #[test]
    fn reports_pane_order_membership_and_the_enclosing_split() {
        let root = root();
        let order: Vec<_> = panes_in_order(&root)
            .iter()
            .map(|pane| pane.id.as_str())
            .collect();
        assert_eq!(order, vec!["pane-left", "pane-top", "pane-bottom"]);
        assert!(contains_pane(&root, "pane-bottom"));
        assert!(!contains_pane(&root, "pane-missing"));
        let (split, branch) = parent_split(&root, "pane-top").unwrap();
        assert_eq!((split.id.as_str(), branch), ("split-right", Branch::First));
        let (split, branch) = parent_split(&root, "pane-left").unwrap();
        assert_eq!((split.id.as_str(), branch), ("split-root", Branch::First));
    }

    #[test]
    fn weights_an_even_layout_by_how_many_panes_each_side_holds() {
        let even = equalize_splits(&root());
        let split = split_of(&even);
        assert_eq!(split.id, "split-root");
        assert_eq!(split.ratio, 1.0 / 3.0);
        assert_eq!(split.children[0].id(), "pane-left");
        assert_eq!(split.children[1].id(), "split-right");
        assert_eq!(split_of(&split.children[1]).ratio, 0.5);
    }

    fn tab(
        id: &str,
        pane_id: &str,
        title: &str,
        profile: SessionProfile,
    ) -> PersistableTerminalTab {
        PersistableTerminalTab {
            id: id.into(),
            pane_id: pane_id.into(),
            title: title.into(),
            profile,
            color: None,
            pinned: false,
        }
    }

    fn local_in(cwd: &str) -> SessionProfile {
        SessionProfile::Local(crate::model::LocalProfile {
            cwd: Some(cwd.into()),
            ..Default::default()
        })
    }

    fn pane_tabs(node: &WorkspaceNode) -> (&[WorkspaceTabLayout], Option<&str>) {
        match node {
            WorkspaceNode::Pane {
                tabs,
                active_tab_id,
                ..
            } => (tabs, active_tab_id.as_deref()),
            WorkspaceNode::Split { .. } => panic!("expected a pane"),
        }
    }

    #[test]
    fn round_trips_pinned_tabs_in_their_strip_order() {
        let mut pinned = tab(
            "pinned-tab",
            "pane",
            "Pinned",
            crate::model::ssh_profile("pinned"),
        );
        pinned.pinned = true;
        let layout = serialize_workspace(
            &pane("pane", Some("regular-tab")),
            &[
                pinned,
                tab(
                    "regular-tab",
                    "pane",
                    "Regular",
                    crate::model::ssh_profile("regular"),
                ),
            ],
            "pane",
        );
        let (tabs, _) = pane_tabs(layout.root.as_ref().unwrap());
        assert!(
            matches!(&tabs[0], WorkspaceTabLayout::Terminal { id, pinned: Some(true), .. } if id == "pinned-tab")
        );
        assert!(
            matches!(&tabs[1], WorkspaceTabLayout::Terminal { id, pinned: None, .. } if id == "regular-tab")
        );

        let restored = restore_workspace(&layout, RestoreWorkspaceOptions::default()).unwrap();
        let pins: Vec<_> = restored
            .tabs
            .iter()
            .map(|tab| (tab.id.as_str(), tab.pinned))
            .collect();
        assert_eq!(pins, vec![("pinned-tab", true), ("regular-tab", false)]);
    }

    #[test]
    fn does_not_persist_a_blank_chooser_as_the_active_terminal() {
        let layout = serialize_workspace(
            &pane("pane", Some("blank-tab")),
            &[tab(
                "session-tab",
                "pane",
                "Production",
                crate::model::ssh_profile("production"),
            )],
            "pane",
        );
        let (tabs, active) = pane_tabs(layout.root.as_ref().unwrap());
        assert_eq!(active, Some("session-tab"));
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].id(), "session-tab");
    }

    fn mixed_layout() -> WorkspaceLayout {
        serialize_workspace(
            &root(),
            &[
                tab(
                    "tab-a",
                    "pane-left",
                    "Production",
                    crate::model::ssh_profile("production"),
                ),
                tab("tab-b", "pane-bottom", "Local", local_in("/srv/app")),
            ],
            "pane-bottom",
        )
    }

    #[test]
    fn automatically_reconnects_local_tabs_while_remote_tabs_wait() {
        let layout = mixed_layout();
        let WorkspaceNode::Split { children, .. } = layout.root.as_ref().unwrap() else {
            panic!("expected a split");
        };
        let (left_tabs, _) = pane_tabs(&children[0]);
        assert!(matches!(
            &left_tabs[0],
            WorkspaceTabLayout::Terminal { profile, offer_reconnect: true, .. } if **profile == crate::model::ssh_profile("production")
        ));
        let WorkspaceNode::Split {
            children: right, ..
        } = &children[1]
        else {
            panic!("expected a split");
        };
        assert!(pane_tabs(&right[0]).0.is_empty());
        assert!(matches!(
            &pane_tabs(&right[1]).0[0],
            WorkspaceTabLayout::Terminal { cwd_hint: Some(cwd), offer_reconnect: true, .. } if cwd == "/srv/app"
        ));

        let restored = restore_workspace(&layout, RestoreWorkspaceOptions::default()).unwrap();
        assert_eq!(restored.active_pane_id, "pane-bottom");
        assert_eq!(restored.active_id.as_deref(), Some("tab-b"));
        let tabs: Vec<_> = restored
            .tabs
            .iter()
            .map(|tab| (tab.id.as_str(), tab.pane_id.as_str(), tab.connect_on_mount))
            .collect();
        assert_eq!(
            tabs,
            vec![
                ("tab-a", "pane-left", false),
                ("tab-b", "pane-bottom", true)
            ]
        );
    }

    #[test]
    fn dials_remote_tabs_too_when_the_restore_opts_in() {
        let restored = restore_workspace(
            &mixed_layout(),
            RestoreWorkspaceOptions {
                connect_remote: true,
            },
        )
        .unwrap();
        let tabs: Vec<_> = restored
            .tabs
            .iter()
            .map(|tab| (tab.id.as_str(), tab.connect_on_mount))
            .collect();
        assert_eq!(tabs, vec![("tab-a", true), ("tab-b", true)]);
    }

    /// The TypeScript test strips a retired `term` override at the restore
    /// boundary. The profile model has no such field, so restoring hands back
    /// exactly the profiles that were saved.
    #[test]
    fn restores_profiles_unchanged() {
        let layout = serialize_workspace(
            &pane("pane", Some("ssh-tab")),
            &[
                tab(
                    "ssh-tab",
                    "pane",
                    "Router",
                    crate::model::ssh_profile("router"),
                ),
                tab(
                    "screen-tab",
                    "pane",
                    "Screen host",
                    crate::model::ssh_profile("screen-host"),
                ),
            ],
            "pane",
        );
        let restored = restore_workspace(&layout, RestoreWorkspaceOptions::default()).unwrap();
        let profiles: Vec<_> = restored
            .tabs
            .iter()
            .map(|tab| tab.profile.clone())
            .collect();
        assert_eq!(
            profiles,
            vec![
                crate::model::ssh_profile("router"),
                crate::model::ssh_profile("screen-host")
            ]
        );
    }
}
