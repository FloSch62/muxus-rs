//! Dragging hosts and folders in the tree (`useTreeDnd.ts`): the rules are
//! `muxus_app::sidebar::tree_dnd`; this is the gesture, the drop markers and the
//! drop itself, which the window carries out.

use muxus_app::hosts::RowNode;
use muxus_app::hosts::organization::{DropEdge, host_order_after_drop};
use muxus_app::hosts::tree::{
    ContainerRef, TREE_ROOT_KEY, folder_key, root_folder_keys, sibling_folder_keys,
    sibling_host_keys,
};
use muxus_app::sidebar::tree_dnd::{
    DragSource, DropTarget, can_drop, container_for, drag_source_for_row, drop_target_for_row,
    dropped_folder_path, folder_order_after_drop, is_pure_reorder, same_target, target_path,
};
use muxus_ui::gpui_kit::{
    App, AppContext, Context, DragMoveEvent, InteractiveElement, IntoElement, ParentElement,
    Render, Stateful, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Theme};

use crate::ui::sidebar::{SessionSidebar, SidebarAction, SidebarEvent};

/// What travels with a dragged row.
#[derive(Clone, Debug)]
pub struct DraggedTreeRow {
    pub key: String,
    pub label: String,
    pub count: usize,
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
#[derive(Clone, Debug, Default)]
pub struct TreeDrag {
    pub source: Option<DragSource>,
    pub target: Option<DropTarget>,
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
}

/// The folder path and sibling order a drop of hosts writes (`onDropHosts`).
#[derive(Clone, Debug)]
pub struct HostDrop {
    pub host_keys: Vec<String>,
    /// `None` for a pure reorder within the hosts' own folder.
    pub path: Option<String>,
    pub order: Vec<String>,
}

/// A folder dropped somewhere else (`onDropFolder`).
#[derive(Clone, Debug)]
pub struct FolderDrop {
    pub from: String,
    pub to: String,
    /// The new sibling order when it was dropped beside a folder.
    pub placement: Option<(String, Vec<String>)>,
}

impl SessionSidebar {
    /// A row's drag handlers and drop marker, when reordering is possible.
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
        if matches!(row.node, RowNode::File { .. }) {
            return element;
        }
        let key = row.key.clone();
        let dragged = self.drag.dragged_keys().contains(&key.as_str());
        let marker = match &self.drag.target {
            Some(DropTarget::IntoFolder { folder_key }) if folder_key == &key => Some(None),
            Some(DropTarget::HostEdge { host_key, edge, .. }) if host_key == &key => {
                Some(Some(*edge))
            }
            Some(DropTarget::FolderEdge {
                folder_key, edge, ..
            }) if folder_key == &key => Some(Some(*edge)),
            _ => None,
        };
        let count = if self.selection.len() > 1 && self.selection.contains(&key) {
            self.selection.len()
        } else {
            1
        };
        let payload = DraggedTreeRow {
            key: key.clone(),
            label: row.node.label().to_string(),
            count,
        };
        let primary = theme.palette.primary.main;
        let mut element = element
            .on_drag(payload, |row, _, _, cx: &mut App| {
                let text = if row.count > 1 {
                    format!("{} hosts", row.count)
                } else {
                    row.label.clone()
                };
                cx.new(|_| DragPreview { text })
            })
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<DraggedTreeRow>, _, cx| {
                    if !event.bounds.contains(&event.event.position) {
                        return;
                    }
                    let height = muxus_ui::zoom::unzoom(event.bounds.size.height);
                    let ratio = if height <= 0. {
                        0.5
                    } else {
                        (muxus_ui::zoom::unzoom(event.event.position.y - event.bounds.origin.y)
                            / height)
                            .clamp(0., 1.)
                    };
                    let dragged = event.drag(cx).key.clone();
                    this.drag_over(&dragged, index, f64::from(ratio), cx);
                }),
            )
            .on_drop(cx.listener(|this, _: &DraggedTreeRow, _, cx| this.finish_drop(cx)));
        if dragged {
            element = element.opacity(0.45);
        }
        match marker {
            Some(None) => element.bg(muxus_ui::color::alpha(primary, 0.12)),
            Some(Some(edge)) => {
                let line = div().absolute().left_0().right_0().h(px(2.)).bg(primary);
                element.child(match edge {
                    DropEdge::Before => line.top(px(-1.)),
                    DropEdge::After => line.bottom(px(-1.)),
                })
            }
            None => element,
        }
    }

    /// The pointer moved over row `index` at `ratio` of its height.
    fn drag_over(&mut self, dragged_key: &str, index: usize, ratio: f64, cx: &mut Context<Self>) {
        if self.drag.source.is_none() {
            let Some(row) = self.rows.iter().find(|row| row.key == dragged_key) else {
                return;
            };
            self.drag.source = drag_source_for_row(row, &self.tree, &self.selection);
        }
        let Some(source) = self.drag.source.clone() else {
            return;
        };
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let next = drop_target_for_row(row, ratio);
        let next = can_drop(&source, &next, &self.tree).then_some(next);
        let changed = match (&self.drag.target, &next) {
            (None, None) => false,
            (current, Some(next)) => !same_target(current.as_ref(), next),
            (Some(_), None) => true,
        };
        if changed {
            self.drag.target = next;
            cx.notify();
        }
    }

    /// The drag ended over the tree: work out the move, hand it to the window.
    fn finish_drop(&mut self, cx: &mut Context<Self>) {
        let drag = std::mem::take(&mut self.drag);
        cx.notify();
        let (Some(source), Some(target)) = (drag.source, drag.target) else {
            return;
        };
        if !can_drop(&source, &target, &self.tree) {
            return;
        }
        let destination = target_path(&target, &self.tree);
        match &source {
            DragSource::Folder {
                folder_key: source_key,
                path,
            } => {
                let Some(destination) = destination else {
                    return;
                };
                let to = dropped_folder_path(&source, &destination);
                let placement = match &target {
                    DropTarget::FolderEdge {
                        folder_key: beside,
                        edge,
                        ..
                    } => {
                        let container = container_for(&target, &self.tree);
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
                cx.emit(SidebarEvent::Action(SidebarAction::DropFolder(
                    FolderDrop {
                        from: path.clone(),
                        to,
                        placement,
                    },
                )));
            }
            DragSource::Host { hosts } => {
                let host_keys: Vec<String> =
                    hosts.iter().map(|host| host.host_key.clone()).collect();
                let siblings = container_for(&target, &self.tree)
                    .map(|container: ContainerRef| sibling_host_keys(&self.tree, container))
                    .unwrap_or_default();
                let (target_key, edge) = match &target {
                    DropTarget::HostEdge { host_key, edge, .. } => (Some(host_key.as_str()), *edge),
                    _ => (None, DropEdge::After),
                };
                let order = host_order_after_drop(&siblings, &host_keys, target_key, edge);
                let path = if is_pure_reorder(&source, &target) {
                    None
                } else {
                    destination
                };
                cx.emit(SidebarEvent::Action(SidebarAction::DropHosts(HostDrop {
                    host_keys,
                    path,
                    order,
                })));
            }
        }
    }

    /// A drag that ended outside the tree leaves no markers behind.
    pub(crate) fn settle_drag(&mut self, cx: &App) {
        if (self.drag.source.is_some() || self.drag.target.is_some()) && !cx.has_active_drag() {
            self.drag = TreeDrag::default();
        }
    }
}
