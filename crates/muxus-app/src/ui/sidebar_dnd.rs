//! Dragging hosts and folders in the tree (`useTreeDnd.ts`): the rules are
//! `muxus_app::sidebar::tree_dnd`; this is the gesture, the drop markers, the
//! folders opened while hovering them and the drop itself, which the window
//! carries out.
//!
//! One set of listeners lives on the tree container rather than on every row, as
//! in the client: the row under the pointer is resolved from geometry (every row
//! has the same pitch), so the 1px gaps between rows, rows not mounted and the
//! space past the last row all read the way the client reads them.

use std::collections::BTreeSet;
use std::time::Duration;

use muxus_app::hosts::RowNode;
use muxus_app::hosts::organization::{DropEdge, host_order_after_drop};
use muxus_app::hosts::tree::{
    ContainerRef, TREE_ROOT_KEY, folder_key, root_folder_keys, sibling_folder_keys,
    sibling_host_keys,
};
use muxus_app::sidebar::row_style::{
    TREE_BASE_INSET, TREE_ROW_GAP, TREE_ROW_HEIGHT, TREE_ROW_PITCH, indent_px,
};
use muxus_app::sidebar::tree_dnd::{
    DragSource, DropTarget, can_drop, container_for, drag_source_for_row, drop_target_for_row,
    dropped_folder_path, folder_order_after_drop, is_pure_reorder, same_target, target_path,
};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, DragMoveEvent, InteractiveElement, IntoElement,
    ParentElement, Render, Stateful, StatefulInteractiveElement, Styled, Task, Window, div, px,
};
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Theme};

use crate::ui::sidebar::{SessionSidebar, SidebarAction, SidebarEvent};

/// Long enough not to fire while passing over, short enough to feel intentional.
const AUTO_EXPAND: Duration = Duration::from_millis(500);

/// How far from a row the pointer may be and still mean that row (`nearestRow`).
const ROW_SLACK: f32 = 4.;

/// What travels with a dragged row. The drag's source itself is kept by the
/// sidebar from the moment the drag starts.
#[derive(Clone, Debug)]
pub struct DraggedTreeRow {
    pub key: String,
}

/// The picture under the pointer while dragging.
pub struct DragPreview {
    text: String,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let p = theme.palette;
        div()
            .px(px(10.))
            .py(px(3.))
            .rounded(px(6.))
            .bg(p.background_paper)
            .border_1()
            .border_color(p.divider)
            .child(
                label(self.text.clone())
                    .size(theme.typography.body2.size)
                    .color(p.text.primary)
                    .no_truncate(),
            )
    }
}

/// The drag in progress.
#[derive(Default)]
pub struct TreeDrag {
    pub source: Option<DragSource>,
    pub target: Option<DropTarget>,
    /// Folders opened only to look inside during the drag. They close again when it
    /// ends: an accidental hover must not permanently change what the sidebar shows.
    pub expanded: BTreeSet<String>,
    /// Opens the collapsed folder the pointer rests on.
    expand_timer: Option<Task<()>>,
}

impl TreeDrag {
    fn dragged_keys(&self) -> Vec<&str> {
        match &self.source {
            Some(DragSource::Host { hosts }) => {
                hosts.iter().map(|host| host.host_key.as_str()).collect()
            }
            Some(DragSource::Folder { folder_key, .. }) => vec![folder_key.as_str()],
            None => Vec::new(),
        }
    }

    /// A drag is in flight (the root target shows).
    pub fn active(&self) -> bool {
        self.source.is_some()
    }

    /// Where `key`'s drop line goes, if the pointer marks an edge of it.
    fn edge_for(&self, key: &str) -> Option<DropEdge> {
        match &self.target {
            Some(DropTarget::HostEdge { host_key, edge, .. }) if host_key == key => Some(*edge),
            Some(DropTarget::FolderEdge {
                folder_key, edge, ..
            }) if folder_key == key => Some(*edge),
            _ => None,
        }
    }

    /// The folder the pointer would drop into.
    pub fn drop_into_key(&self) -> Option<&str> {
        match &self.target {
            Some(DropTarget::IntoFolder { folder_key }) => Some(folder_key),
            _ => None,
        }
    }
}

/// The folder path and sibling order a drop of hosts writes (`onDropHosts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDrop {
    pub host_keys: Vec<String>,
    /// `None` for a pure reorder within the hosts' own folder.
    pub path: Option<String>,
    pub order: Vec<String>,
}

/// A folder dropped somewhere else (`onDropFolder`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderDrop {
    pub from: String,
    pub to: String,
    /// The new sibling order when it was dropped beside a folder.
    pub placement: Option<(String, Vec<String>)>,
}

/// The row `y` (from the top of the tree) points at and how far down it, or
/// `None` past every row (the root). Rows have a fixed pitch, so this is
/// arithmetic, not a search; within a row's height of slack the nearest row
/// wins, the earlier one on a tie, as `nearestRow` measures it.
pub fn row_at(y: f32, rows: usize) -> Option<(usize, f32)> {
    if rows == 0 {
        return None;
    }
    let distance = |index: usize| {
        let top = index as f32 * TREE_ROW_PITCH;
        let bottom = top + TREE_ROW_HEIGHT;
        if y < top {
            top - y
        } else if y > bottom {
            y - bottom
        } else {
            0.
        }
    };
    let guess = ((y / TREE_ROW_PITCH).floor().max(0.) as usize).min(rows - 1);
    let mut best = guess.saturating_sub(1);
    for index in [guess, (guess + 1).min(rows - 1)] {
        if distance(index) < distance(best) {
            best = index;
        }
    }
    if distance(best) > ROW_SLACK {
        return None;
    }
    let top = best as f32 * TREE_ROW_PITCH;
    Some((best, ((y - top) / TREE_ROW_HEIGHT).clamp(0., 1.)))
}

impl SessionSidebar {
    /// A row's drag handle and drop markers, when reordering is possible.
    pub(crate) fn with_drag(
        &self,
        element: Stateful<muxus_ui::gpui_kit::Div>,
        index: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<muxus_ui::gpui_kit::Div> {
        if !self.reorder_enabled() {
            return element;
        }
        let row = &self.rows[index];
        // ssh_config file groups are defined by the config, not by drags.
        if matches!(row.node, RowNode::File { .. }) {
            return element;
        }
        let key = row.key.clone();
        let dragged = self.drag.dragged_keys().contains(&key.as_str());
        let primary = theme.palette.primary.main;
        let sidebar = cx.entity().downgrade();
        let mut element = element.on_drag(
            DraggedTreeRow { key: key.clone() },
            move |row, _, _, cx: &mut App| {
                let text = sidebar
                    .update(cx, |this, cx| this.start_drag(&row.key, cx))
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                cx.new(|_| DragPreview { text })
            },
        );
        if dragged {
            element = element.opacity(0.45);
        }
        if self.drag.drop_into_key() == Some(key.as_str()) {
            // `bgcolor: action.selected` with `inset 2px 0` of the primary color.
            element = element.bg(theme.palette.action.selected).child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .rounded(px(6.))
                    .border_l(px(2.))
                    .border_color(primary),
            );
        }
        if let Some(edge) = self.drag.edge_for(&key) {
            // Inset to this row's own indent, so the line reads as "at this level".
            let line = div()
                .absolute()
                .left(px(indent_px(row.depth)))
                .right(px(TREE_BASE_INSET))
                .h(px(2.))
                .rounded(px(2.))
                .bg(primary);
            element = element.child(match edge {
                DropEdge::Before => line.top(px(-1.)),
                DropEdge::After => line.bottom(px(-1.)),
            });
        }
        element
    }

    /// The drag starts: what it picks up (the grabbed host, or the selection it is
    /// part of), read once now; returns the preview's text.
    fn start_drag(&mut self, key: &str, cx: &mut Context<Self>) -> Option<String> {
        let row = self.rows.iter().find(|row| row.key == key)?;
        let label = row.node.label().to_string();
        let source = drag_source_for_row(row, &self.tree, &self.selection)?;
        let text = match &source {
            DragSource::Host { hosts } if hosts.len() > 1 => format!("{} hosts", hosts.len()),
            _ => label,
        };
        self.reset_drag();
        self.drag.source = Some(source);
        cx.notify();
        Some(text)
    }

    /// The tree container's drag listeners: hit-testing, markers and the drop.
    pub(crate) fn tree_drop_zone(
        &self,
        tree: Stateful<muxus_ui::gpui_kit::Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<muxus_ui::gpui_kit::Div> {
        if !self.reorder_enabled() {
            return tree;
        }
        tree.on_drag_move(
            cx.listener(|this, event: &DragMoveEvent<DraggedTreeRow>, _, cx| {
                let position = event.event.position;
                // Outside the tree, or scrolled out of the sidebar's view: `dragleave`.
                let inside = event.bounds.contains(&position)
                    && (this.scroll.bounds().size.height <= px(0.)
                        || this.scroll.bounds().contains(&position));
                let y = inside.then(|| muxus_ui::zoom::unzoom(position.y - event.bounds.origin.y));
                this.drag_over(y, cx);
            }),
        )
        .on_drop(cx.listener(|this, _: &DraggedTreeRow, _, cx| this.finish_drop(cx)))
    }

    /// "Drop here for no folder", under the rows while something is dragged.
    pub(crate) fn root_drop_row(&self, theme: &Theme) -> Option<AnyElement> {
        if !self.drag.active() {
            return None;
        }
        let p = theme.palette;
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .mx(px(8.))
                // `margin-top: 4px`, which the last row's 1px margin collapses into; the
                // tree's 1px gap is already there.
                .mt(px(4. - TREE_ROW_GAP))
                .h(px(22.))
                .rounded(px(8.))
                .border_1()
                .border_dashed()
                .border_color(p.divider)
                .child(
                    label("Drop here for no folder")
                        .size(11.)
                        .line_height(16.5)
                        .color(p.text.disabled)
                        .no_truncate(),
                )
                .into_any_element(),
        )
    }

    /// The pointer moved during a drag: `y` from the top of the tree, `None` outside it.
    pub(crate) fn drag_over(&mut self, y: Option<f32>, cx: &mut Context<Self>) {
        let Some(source) = self.drag.source.clone() else {
            return;
        };
        let Some(y) = y else {
            self.drag.expand_timer = None;
            if self.drag.target.take().is_some() {
                cx.notify();
            }
            return;
        };
        let next = match row_at(y, self.rows.len()) {
            Some((index, ratio)) => drop_target_for_row(&self.rows[index], f64::from(ratio)),
            None => DropTarget::Root,
        };
        if !can_drop(&source, &next, &self.tree) {
            self.drag.expand_timer = None;
            if self.drag.target.take().is_some() {
                cx.notify();
            }
            return;
        }
        if !same_target(self.drag.target.as_ref(), &next) {
            self.drag.target = Some(next.clone());
            cx.notify();
        }
        // Hovering a collapsed folder opens it so its children become targets.
        let DropTarget::IntoFolder { folder_key } = next else {
            return;
        };
        if self.drag.expanded.contains(&folder_key) || self.drag.expand_timer.is_some() {
            return;
        }
        let collapsed = self
            .rows
            .iter()
            .find(|row| row.key == folder_key)
            .is_some_and(|row| row.expanded == Some(false));
        if !collapsed {
            return;
        }
        self.drag.expand_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTO_EXPAND).await;
            let _ = this.update(cx, |this, cx| {
                this.drag.expand_timer = None;
                if this.drag.active() {
                    this.drag.expanded.insert(folder_key);
                    this.rebuild();
                    cx.notify();
                }
            });
        }));
    }

    /// End the drag state; folders opened by it close again.
    fn reset_drag(&mut self) {
        let opened = !self.drag.expanded.is_empty();
        self.drag = TreeDrag::default();
        if opened {
            self.rebuild();
        }
    }

    /// The drag ended over the tree: work out the move, hand it to the window.
    pub(crate) fn finish_drop(&mut self, cx: &mut Context<Self>) {
        let source = self.drag.source.take();
        let target = self.drag.target.take();
        self.reset_drag();
        cx.notify();
        let (Some(source), Some(target)) = (source, target) else {
            return;
        };
        if let Some(action) = self.drop_action(&source, &target) {
            cx.emit(SidebarEvent::Action(action));
        }
    }

    /// What dropping `source` on `target` asks the window to write.
    pub(crate) fn drop_action(
        &self,
        source: &DragSource,
        target: &DropTarget,
    ) -> Option<SidebarAction> {
        if !can_drop(source, target, &self.tree) {
            return None;
        }
        let destination = target_path(target, &self.tree);
        match source {
            DragSource::Folder {
                folder_key: source_key,
                path,
            } => {
                let destination = destination?;
                let to = dropped_folder_path(source, &destination);
                // Only an edge drop names a position; dropping inside a folder just
                // joins it and sorts alphabetically among its unplaced siblings.
                let placement = match target {
                    DropTarget::FolderEdge {
                        folder_key: beside,
                        edge,
                        ..
                    } => {
                        let container = container_for(target, &self.tree);
                        let siblings = match container {
                            Some(container) => sibling_folder_keys(&self.tree, container),
                            None => root_folder_keys(&self.tree),
                        };
                        let parent = container
                            .map(|container| self.tree.container_key(container).to_string())
                            .unwrap_or_else(|| TREE_ROOT_KEY.to_string());
                        Some((
                            parent,
                            folder_order_after_drop(
                                &siblings,
                                source_key,
                                &folder_key(&to),
                                Some(beside),
                                *edge,
                            ),
                        ))
                    }
                    _ => None,
                };
                Some(SidebarAction::DropFolder(FolderDrop {
                    from: path.clone(),
                    to,
                    placement,
                }))
            }
            DragSource::Host { hosts } => {
                // The order the hosts settle into among their new siblings. Dropping onto
                // the folder itself appends; dropping on an edge inserts there.
                let host_keys: Vec<String> =
                    hosts.iter().map(|host| host.host_key.clone()).collect();
                let siblings = container_for(target, &self.tree)
                    .map(|container: ContainerRef| sibling_host_keys(&self.tree, container))
                    .unwrap_or_default();
                let (target_key, edge) = match target {
                    DropTarget::HostEdge { host_key, edge, .. } => (Some(host_key.as_str()), *edge),
                    _ => (None, DropEdge::After),
                };
                let order = host_order_after_drop(&siblings, &host_keys, target_key, edge);
                let path = if is_pure_reorder(source, target) {
                    None
                } else {
                    destination
                };
                Some(SidebarAction::DropHosts(HostDrop {
                    host_keys,
                    path,
                    order,
                }))
            }
        }
    }

    /// A drag that ended outside the tree leaves no markers or opened folders behind.
    pub(crate) fn settle_drag(&mut self, cx: &App) {
        let leftover = self.drag.source.is_some()
            || self.drag.target.is_some()
            || !self.drag.expanded.is_empty();
        if leftover && !cx.has_active_drag() {
            self.reset_drag();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_hit_by_their_geometry() {
        // Inside row 0, near its top and bottom.
        assert_eq!(row_at(0., 3), Some((0, 0.)));
        assert_eq!(row_at(13., 3), Some((0, 0.5)));
        assert_eq!(row_at(26., 3), Some((0, 1.)));
        // The gap between rows 0 and 1 belongs to the nearer row, the earlier on a tie.
        assert_eq!(row_at(26.5, 3).map(|(index, _)| index), Some(0));
        assert_eq!(row_at(26.9, 3).map(|(index, _)| index), Some(1));
        // Row 2 spans 54..80; four pixels past it still count, further is the root.
        assert_eq!(row_at(83.5, 3).map(|(index, _)| index), Some(2));
        assert_eq!(row_at(85., 3), None);
        assert_eq!(row_at(-3., 3).map(|(index, _)| index), Some(0));
        assert_eq!(row_at(-5., 3), None);
        assert_eq!(row_at(10., 0), None);
    }
}
