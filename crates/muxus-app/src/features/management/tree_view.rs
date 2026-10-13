//! A virtualized tree for device data (`DataTreeView.tsx`): the explorer
//! and every result tree draw their rows with it, so hundreds of thousands
//! of nodes scroll smoothly.

use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;

use muxus_mgmt::workbench::data_tree::{DataNode, DataNodeKind, node_label};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, ClickEvent, Context, FontWeight, Hsla, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Pixels, Point, ScrollStrategy,
    SharedString, StatefulInteractiveElement, Styled, StyledText, UniformListScrollHandle, Window,
    div, px, relative, uniform_list,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::IconButton;
use muxus_ui::widgets::button::elevation;

use super::logic::tree::{
    INDENT, ROW_HEIGHT, TreeKey, TreeKeyAction, entry_title, highlight, needle_of, tree_key,
    visible_rows,
};
use super::logic::values::{ValueTone, value_text, value_tone};
use super::ui::{Menu, TreeSource};
use super::view::ManagementView;
use super::widgets::MONO;
use muxus_ui::widgets::progress::CircularProgress;

/// One row as drawn, owned so the list can render any range of it.
#[derive(Debug, Clone)]
pub struct RowData {
    pub id: String,
    pub label: String,
    pub kind: DataNodeKind,
    pub depth: usize,
    pub expandable: bool,
    pub expanded: bool,
    pub parent_id: Option<String>,
    pub value: Option<String>,
    pub tone: Option<ValueTone>,
    pub list_count: Option<usize>,
    pub title: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
}

/// What a tree shows besides its rows.
pub struct TreeOptions<'a> {
    pub expanded: &'a HashSet<String>,
    pub filter: &'a str,
    pub lazy: bool,
    pub loading: Option<&'a HashSet<String>>,
    pub errors: Option<&'a std::collections::HashMap<String, String>>,
}

/// The rows of `root` to draw.
pub fn rows_of(root: &DataNode, options: &TreeOptions<'_>) -> Vec<RowData> {
    visible_rows(root, options.expanded, options.filter, false, options.lazy)
        .into_iter()
        .map(|row| {
            let node = row.node;
            let leaf = node.is_leaf();
            RowData {
                id: node.id.clone(),
                label: node_label(node),
                kind: node.kind,
                depth: row.depth,
                expandable: row.expandable,
                expanded: row.expanded,
                parent_id: row.parent_id.map(str::to_owned),
                value: leaf.then(|| value_text(node.value.as_ref())),
                tone: if leaf {
                    value_tone(node.value.as_ref())
                } else {
                    None
                },
                list_count: (node.kind == DataNodeKind::List).then_some(node.children.len()),
                title: entry_title(node),
                loading: options
                    .loading
                    .is_some_and(|loading| loading.contains(&node.id)),
                error: options
                    .errors
                    .and_then(|errors| errors.get(&node.id))
                    .cloned(),
            }
        })
        .collect()
}

/// A leaf value's color: status hues in both themes, plain text otherwise.
pub fn tone_color(tone: Option<ValueTone>, theme: &Theme) -> Hsla {
    match tone {
        Some(ValueTone::Success) => theme.status_text_color(Status::Success),
        Some(ValueTone::Error) => theme.status_text_color(Status::Error),
        Some(ValueTone::Info) => theme.status_text_color(Status::Info),
        None => theme.palette.text.primary,
    }
}

/// `KindIcon`: what sort of node a row is.
pub fn kind_icon(kind: DataNodeKind, theme: &Theme) -> AnyElement {
    let p = &theme.palette;
    match kind {
        DataNodeKind::List => icon("ViewListOutlined", 14., p.text.secondary).into_any_element(),
        DataNodeKind::Entry => icon("LabelOutlined", 14., p.text.secondary).into_any_element(),
        DataNodeKind::Container | DataNodeKind::Root => {
            icon("DataObjectOutlined", 14., p.text.secondary).into_any_element()
        }
        _ => div()
            .w(px(14.))
            .flex()
            .flex_none()
            .justify_center()
            .child(div().size(px(5.)).rounded_full().bg(p.text.disabled))
            .into_any_element(),
    }
}

/// Text with the filter match marked (`Highlight`).
pub fn highlighted(text: &str, needle: &str, theme: &Theme) -> StyledText {
    let shown: SharedString = text.to_owned().into();
    match highlight(text, needle) {
        Some((before, matched, _)) => {
            let start = before.len();
            StyledText::new(shown).with_highlights([(
                start..start + matched.len(),
                muxus_ui::gpui_kit::HighlightStyle {
                    background_color: Some(alpha(theme.palette.warning.main, 0.35)),
                    ..Default::default()
                },
            )])
        }
        None => StyledText::new(shown),
    }
}

impl ManagementView {
    /// Draw a tree. `rows` were computed by [`rows_of`]; `selected` is the
    /// selected node id.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_tree(
        &mut self,
        source: TreeSource,
        rows: Rc<Vec<RowData>>,
        selected: Option<String>,
        filter: &str,
        empty: AnyElement,
        scroll: UniformListScrollHandle,
        focus: &muxus_ui::gpui_kit::FocusHandle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let needle = needle_of(filter);
        let focused_ring = alpha(theme.palette.primary.main, 0.5);
        let _ = focused_ring;
        let container = div().size_full().flex().flex_col().min_h_0();
        if rows.is_empty() {
            return container
                .child(div().p(px(16.)).child(empty))
                .into_any_element();
        }
        let count = rows.len();
        let key_rows = rows.clone();
        let key_source = source.clone();
        let list_source = source.clone();
        let list_rows = rows.clone();
        let list_selected = selected.clone();
        let list = uniform_list(
            SharedString::from(format!("tree-{source:?}")),
            count,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                this.render_tree_rows(
                    &list_source,
                    &list_rows,
                    range,
                    list_selected.as_deref(),
                    &needle,
                    window,
                    cx,
                )
            }),
        )
        .track_scroll(&scroll)
        .py(px(4.))
        .flex_1()
        .min_h_0();
        container
            .id(SharedString::from(format!("tree-box-{source:?}")))
            .track_focus(focus)
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                let key = match event.keystroke.key.as_str() {
                    "down" => TreeKey::Down,
                    "up" => TreeKey::Up,
                    "home" => TreeKey::Home,
                    "end" => TreeKey::End,
                    "pagedown" => TreeKey::PageDown,
                    "pageup" => TreeKey::PageUp,
                    "right" => TreeKey::Right,
                    "left" => TreeKey::Left,
                    "enter" => TreeKey::Enter,
                    _ => return,
                };
                this.tree_key(&key_source, &key_rows, key, &scroll, cx);
                cx.stop_propagation();
            }))
            .child(list)
            .into_any_element()
    }

    fn tree_key(
        &mut self,
        source: &TreeSource,
        rows: &[RowData],
        key: TreeKey,
        scroll: &UniformListScrollHandle,
        cx: &mut Context<Self>,
    ) {
        let selected = self.tree_selected(source);
        // `tree_key` works on borrowed rows; rebuild them shallowly.
        let nodes: Vec<DataNode> = rows
            .iter()
            .map(|row| DataNode {
                id: row.id.clone(),
                name: String::new(),
                kind: row.kind,
                path: Default::default(),
                keys: None,
                guessed_keys: None,
                value: None,
                namespace: None,
                children: Vec::new(),
                loaded: true,
            })
            .collect();
        let tree_rows: Vec<super::logic::tree::TreeRow<'_>> = rows
            .iter()
            .zip(&nodes)
            .map(|(row, node)| super::logic::tree::TreeRow {
                node,
                depth: row.depth,
                expandable: row.expandable,
                expanded: row.expanded,
                parent_id: row.parent_id.as_deref(),
            })
            .collect();
        let page = 400 / ROW_HEIGHT as usize;
        match tree_key(&tree_rows, selected.as_deref(), key, page) {
            TreeKeyAction::Move(index) => {
                self.select_tree_node(source, &rows[index].id, cx);
                scroll.scroll_to_item(index, ScrollStrategy::Nearest);
            }
            TreeKeyAction::Toggle(index) => {
                let row = &rows[index];
                self.toggle_tree_node(source, &row.id, !row.expanded, row.kind, cx);
            }
            TreeKeyAction::Activate(index) => {
                self.activate_tree_node(source, &rows[index].id, cx);
            }
            TreeKeyAction::None => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_tree_rows(
        &mut self,
        source: &TreeSource,
        rows: &[RowData],
        range: Range<usize>,
        selected: Option<&str>,
        needle: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = *cx.theme();
        let p = &theme.palette;
        let dark = theme.mode.is_dark();
        let connected = self.connected();
        let gnmi = self.is_gnmi();
        range
            .filter_map(|index| rows.get(index).map(|row| (index, row)))
            .map(|(index, row)| {
                let is_selected = selected == Some(row.id.as_str());
                let leaf = matches!(row.kind, DataNodeKind::Leaf | DataNodeKind::LeafList);
                let bg = if is_selected {
                    alpha(p.primary.main, if dark { 0.22 } else { 0.12 })
                } else {
                    muxus_ui::gpui_kit::Hsla::transparent_black()
                };
                let hover = if is_selected {
                    alpha(p.primary.main, if dark { 0.26 } else { 0.15 })
                } else {
                    p.action.hover
                };
                let group: SharedString = format!("tree-row-{index}").into();
                let id = row.id.clone();
                let row_source = source.clone();
                let chevron = if row.loading {
                    CircularProgress::new(("tree_view-progress", 1usize), 11., p.primary.main)
                        .into_any_element()
                } else if row.expandable {
                    let mut chevron = icon("ChevronRight", 16., p.text.secondary);
                    if row.expanded {
                        chevron = chevron.with_transformation(
                            muxus_ui::gpui_kit::Transformation::rotate(
                                muxus_ui::gpui_kit::radians(std::f32::consts::FRAC_PI_2),
                            ),
                        );
                    }
                    chevron.into_any_element()
                } else {
                    div().into_any_element()
                };
                let toggle_source = source.clone();
                let toggle_id = row.id.clone();
                let (expandable, expanded, kind) = (row.expandable, row.expanded, row.kind);
                let mut element = div()
                    .id(SharedString::from(format!("row-{}", row.id)))
                    .group(group.clone())
                    .relative()
                    .h(px(ROW_HEIGHT))
                    .pl(px(6. + row.depth as f32 * INDENT))
                    .pr(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(theme.radius))
                    .text_size(px(12.5))
                    .whitespace_nowrap()
                    .bg(bg)
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener({
                        let source = source.clone();
                        let id = id.clone();
                        move |this, event: &ClickEvent, _, cx| {
                            if event.click_count() == 2 {
                                if expandable {
                                    this.toggle_tree_node(&source, &id, !expanded, kind, cx);
                                } else {
                                    this.activate_tree_node(&source, &id, cx);
                                }
                            } else {
                                this.select_tree_node(&source, &id, cx);
                            }
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener({
                            let source = row_source.clone();
                            let id = id.clone();
                            move |this, event: &MouseDownEvent, _, cx| {
                                this.select_tree_node(&source, &id, cx);
                                this.open_node_menu(source.clone(), id.clone(), event.position, cx);
                            }
                        }),
                    )
                    .child(
                        div()
                            .id("chevron")
                            .w(px(16.))
                            .h(px(16.))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .child(chevron)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                if expandable {
                                    this.toggle_tree_node(
                                        &toggle_source,
                                        &toggle_id,
                                        !expanded,
                                        kind,
                                        cx,
                                    );
                                } else {
                                    this.select_tree_node(&toggle_source, &toggle_id, cx);
                                }
                            })),
                    )
                    .child(kind_icon(row.kind, &theme))
                    .child({
                        let mut name = div()
                            .font_family(MONO)
                            .font_weight(FontWeight(match row.kind {
                                DataNodeKind::Entry => 600.,
                                _ if leaf => 400.,
                                _ => 500.,
                            }))
                            .line_height(px(18.75))
                            .text_color(if leaf {
                                p.text.secondary
                            } else {
                                p.text.primary
                            })
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(highlighted(&row.label, needle, &theme));
                        name = if leaf {
                            name.flex_none().max_w(relative(0.55))
                        } else {
                            name.flex_shrink(1.)
                        };
                        name
                    });
                if let Some(title) = &row.title {
                    element = element.tooltip(muxus_ui::widgets::Tooltip::text(title.clone()));
                }
                if let Some(count) = row.list_count {
                    element = element.child(
                        div()
                            .flex_none()
                            .px(px(6.))
                            .rounded(px(64.))
                            .bg(p.action.selected)
                            .text_size(px(10.5))
                            .line_height(px(16.))
                            .text_color(p.text.secondary)
                            .child(count.to_string()),
                    );
                }
                if leaf {
                    let value = row.value.clone().unwrap_or_default();
                    let shown = if value.is_empty() {
                        "\"\"".to_owned()
                    } else {
                        value
                    };
                    element = element.child(
                        div()
                            .font_family(MONO)
                            .line_height(px(18.75))
                            .text_color(tone_color(row.tone, &theme))
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(highlighted(&shown, needle, &theme)),
                    );
                } else {
                    element = element.child(div().flex_1());
                }
                if let Some(error) = &row.error {
                    element = element.child(
                        div()
                            .id("row-error")
                            .flex_none()
                            .child(icon("ErrorOutlineOutlined", 15., p.error.main))
                            .tooltip(muxus_ui::widgets::Tooltip::text(error.clone())),
                    );
                }
                if connected && row.kind != DataNodeKind::Root {
                    let get_source = source.clone();
                    let get_id = row.id.clone();
                    let watch_source = source.clone();
                    let watch_id = row.id.clone();
                    let mut actions = div()
                        .id("row-actions")
                        .absolute()
                        .right(px(4.))
                        .top(px(2.))
                        .bottom(px(2.))
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .px(px(2.))
                        .rounded(px(theme.radius))
                        .bg(p.background_paper)
                        .shadow(elevation(1, &theme))
                        .invisible()
                        .group_hover(group, |s| s.visible())
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            IconButton::new("row-get", "PlayArrowRounded")
                                .icon_size(16.)
                                .padding(2.)
                                .tooltip(if gnmi { "Get" } else { "Get with state" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.node_get(&get_source, &get_id, cx);
                                })),
                        );
                    if gnmi {
                        actions = actions.child(
                            IconButton::new("row-watch", "MonitorHeartOutlined")
                                .icon_size(15.)
                                .padding(2.)
                                .tooltip("Watch (subscribe)")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.node_watch(&watch_source, &watch_id, cx);
                                })),
                        );
                    }
                    element = element.child(actions);
                }
                div().w_full().px(px(4.)).child(element).into_any_element()
            })
            .collect()
    }

    /// Open the node menu at the pointer.
    pub(super) fn open_node_menu(
        &mut self,
        source: TreeSource,
        node_id: String,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.ui.menu = Some(Menu::Node {
            source,
            node_id,
            position,
        });
        cx.notify();
    }
}
