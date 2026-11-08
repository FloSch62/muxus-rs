//! MUI `Table`, `TableRow`, `TableCell` and `TableSortLabel` under the
//! Muxus theme, with the sorting the tables built on them use.
//!
//! GPUI has no table layout, so a table is a column of rows and a row a
//! line of cells; each [`TableCell`] gets its column's width (fixed, or a
//! share of what is left, as `width: auto` columns split it).
//!
//! Cells (`TableCell.js` with the `MuiTableCell` override): body2 text with
//! 16px padding (`size="small"`: 6px × 16px) and a divider line under the
//! cell; head cells are 12px at weight 550 in the secondary ink on a
//! pxToRem(24) line. Rows (`TableRow`): `hover` tints a row with
//! `action.hover`, `selected` with the primary color at the selected
//! opacity (plus the hover opacity under the pointer). `TableSortLabel`: the
//! label with an 18px `ArrowDownward` 4px to each side, hidden until the
//! label is hovered (0.5) or active (1, secondary ink), pointing down for
//! descending and up for ascending; the active label is text.primary.

use std::cmp::Ordering;
use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, Hsla, InteractiveElement, IntoElement,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Transformation,
    Window, div, percentage,
};

use crate::color::alpha;
use crate::icon::icon;
use crate::label::label;
use crate::theme::{ActiveTheme, Theme};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// The order of a sorted column (`order`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortDirection {
    #[default]
    Asc,
    Desc,
}

impl SortDirection {
    pub fn flipped(self) -> Self {
        match self {
            SortDirection::Asc => SortDirection::Desc,
            SortDirection::Desc => SortDirection::Asc,
        }
    }

    /// `compare` in this direction (`getComparator`): descending turns it around.
    pub fn apply(self, ordering: Ordering) -> Ordering {
        match self {
            SortDirection::Asc => ordering,
            SortDirection::Desc => ordering.reverse(),
        }
    }
}

/// Which column a table is sorted by and how (`orderBy` and `order`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortOrder<C> {
    pub column: C,
    pub direction: SortDirection,
}

impl<C: PartialEq + Copy> SortOrder<C> {
    pub fn new(column: C, direction: SortDirection) -> Self {
        Self { column, direction }
    }

    /// `handleRequestSort`: a click on the sorted column turns its order around,
    /// a click on another column sorts by it ascending.
    pub fn request(&mut self, column: C) {
        let ascending = self.column == column && self.direction == SortDirection::Asc;
        self.direction = if ascending {
            SortDirection::Desc
        } else {
            SortDirection::Asc
        };
        self.column = column;
    }

    /// The direction `column`'s label shows: `Some` while it is the sorted column.
    pub fn direction_of(&self, column: C) -> Option<SortDirection> {
        (self.column == column).then_some(self.direction)
    }
}

/// Sort `rows` by `compare` in `direction`, keeping equal rows in their order
/// (`stableSort`).
pub fn sort_rows<T>(
    rows: &mut [T],
    direction: SortDirection,
    mut compare: impl FnMut(&T, &T) -> Ordering,
) {
    rows.sort_by(|a, b| direction.apply(compare(a, b)));
}

/// The row order `sort_rows` would give, as indices into `rows`.
pub fn sorted_indices<T>(
    rows: &[T],
    direction: SortDirection,
    mut compare: impl FnMut(&T, &T) -> Ordering,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| direction.apply(compare(&rows[a], &rows[b])));
    order
}

/// `align` of a cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CellAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// How wide a column is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CellWidth {
    /// `sx={{ width }}` in px.
    Fixed(f32),
    /// A share of the width the fixed columns leave (`width: auto`).
    Flex(f32),
}

impl Default for CellWidth {
    fn default() -> Self {
        CellWidth::Flex(1.)
    }
}

/// `<Table>`: the rows stacked, as wide as the container.
pub fn table() -> Div {
    div().flex().flex_col().w_full()
}

/// One cell (see the module docs).
#[derive(IntoElement)]
pub struct TableCell {
    content: CellContent,
    head: bool,
    small: bool,
    align: CellAlign,
    width: CellWidth,
    border: bool,
    padding_y: Option<f32>,
    font_size: Option<f32>,
    ink: Option<Hsla>,
}

enum CellContent {
    Text(SharedString),
    Element(AnyElement),
}

impl TableCell {
    fn new(content: CellContent, head: bool) -> Self {
        Self {
            content,
            head,
            small: false,
            align: CellAlign::Left,
            width: CellWidth::default(),
            border: true,
            padding_y: None,
            font_size: None,
            ink: None,
        }
    }

    /// A `TableHead` cell holding text.
    pub fn head(text: impl Into<SharedString>) -> Self {
        Self::new(CellContent::Text(text.into()), true)
    }

    /// A `TableBody` cell holding text (ellipsized at the cell's width).
    pub fn body(text: impl Into<SharedString>) -> Self {
        Self::new(CellContent::Text(text.into()), false)
    }

    /// A head cell holding any element (a [`TableSortLabel`]).
    pub fn head_element(element: impl IntoElement) -> Self {
        Self::new(CellContent::Element(element.into_any_element()), true)
    }

    /// A body cell holding any element.
    pub fn element(element: impl IntoElement) -> Self {
        Self::new(CellContent::Element(element.into_any_element()), false)
    }

    /// `size="small"`: 6px × 16px padding.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    pub fn align(mut self, align: CellAlign) -> Self {
        self.align = align;
        self
    }

    pub fn width(mut self, width: CellWidth) -> Self {
        self.width = width;
        self
    }

    /// `border: 0`.
    pub fn no_border(mut self) -> Self {
        self.border = false;
        self
    }

    /// `sx={{ py }}` in px.
    pub fn padding_y(mut self, padding: f32) -> Self {
        self.padding_y = Some(padding);
        self
    }

    /// `sx={{ fontSize }}`.
    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = Some(size);
        self
    }

    /// `sx={{ color }}`.
    pub fn ink(mut self, color: Hsla) -> Self {
        self.ink = Some(color);
        self
    }
}

/// The text style of a cell: (size, weight, line height, ink).
fn cell_text(theme: &Theme, head: bool) -> (f32, f32, f32, Hsla) {
    let body2 = theme.typography.body2;
    if head {
        // `fontSize: 12` and weight 550 from the theme over MUI's head cell
        // (`lineHeight: pxToRem(24)`, text.primary).
        let line = theme.typography.px_to_rem(24.);
        (12., 550., line, theme.palette.text.secondary)
    } else {
        (
            body2.size,
            body2.weight,
            body2.line_height,
            theme.palette.text.primary,
        )
    }
}

impl RenderOnce for TableCell {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (size, weight, line_height, ink) = cell_text(theme, self.head);
        let size = self.font_size.unwrap_or(size);
        let ink = self.ink.unwrap_or(ink);
        let pad_y = self.padding_y.unwrap_or(if self.small { 6. } else { 16. });
        let mut cell = div()
            .flex()
            .items_center()
            .min_w_0()
            .px(px(16.))
            .py(px(pad_y))
            .text_size(px(size))
            .line_height(px(line_height))
            .text_color(ink);
        cell = match self.width {
            CellWidth::Fixed(width) => cell.flex_none().w(px(width)),
            CellWidth::Flex(share) => cell.flex_basis(px(0.)).flex_shrink(1.).flex_grow(share),
        };
        cell = match self.align {
            CellAlign::Left => cell.justify_start(),
            CellAlign::Center => cell.justify_center(),
            // `flexDirection: row-reverse`: content hugs the right edge.
            CellAlign::Right => cell.justify_end(),
        };
        if self.border {
            cell = cell.border_b_1().border_color(theme.palette.divider);
        }
        match self.content {
            CellContent::Text(text) => cell.child(
                label(text)
                    .size(size)
                    .weight(weight)
                    .line_height(line_height)
                    .color(ink),
            ),
            CellContent::Element(element) => cell.child(element),
        }
    }
}

/// One row of cells.
#[derive(IntoElement)]
pub struct TableRow {
    id: ElementId,
    cells: Vec<TableCell>,
    hover: bool,
    selected: bool,
    head: bool,
    on_click: Option<ClickHandler>,
}

impl TableRow {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            cells: Vec::new(),
            hover: false,
            selected: false,
            head: false,
            on_click: None,
        }
    }

    /// A `TableHead` row: `background.default` behind it, as a sticky header has.
    pub fn head(mut self) -> Self {
        self.head = true;
        self
    }

    pub fn cell(mut self, cell: TableCell) -> Self {
        self.cells.push(cell);
        self
    }

    pub fn cells(mut self, cells: impl IntoIterator<Item = TableCell>) -> Self {
        self.cells.extend(cells);
        self
    }

    /// `hover`: tint the row under the pointer.
    pub fn hover(mut self, hover: bool) -> Self {
        self.hover = hover;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

/// A row's background: (resting, under the pointer).
pub fn row_background(theme: &Theme, hover: bool, selected: bool) -> (Option<Hsla>, Option<Hsla>) {
    let p = &theme.palette;
    if selected {
        let selected_bg = alpha(p.primary.main, p.action.selected_opacity);
        let selected_hover = alpha(
            p.primary.main,
            p.action.selected_opacity + p.action.hover_opacity,
        );
        (Some(selected_bg), Some(selected_hover))
    } else if hover {
        (None, Some(p.action.hover))
    } else {
        (None, None)
    }
}

impl RenderOnce for TableRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let mut row = div().id(self.id).flex().w_full().children(self.cells);
        if self.head {
            row = row.bg(theme.palette.background_default);
        }
        let (resting, hovered) = row_background(theme, self.hover, self.selected);
        if let Some(bg) = resting {
            row = row.bg(bg);
        }
        if let Some(bg) = hovered {
            row = row.hover(move |s| s.bg(bg));
        }
        if let Some(handler) = self.on_click {
            row = row
                .cursor_pointer()
                .on_click(move |event, window, cx| handler(event, window, cx));
        }
        row
    }
}

/// `TableSortLabel` (see the module docs). `direction` is `Some` while the
/// column is the sorted one (`active`).
#[derive(IntoElement)]
pub struct TableSortLabel {
    id: ElementId,
    label: SharedString,
    direction: Option<SortDirection>,
    /// The direction an inactive label's arrow shows on hover.
    idle: SortDirection,
    on_click: Option<ClickHandler>,
}

impl TableSortLabel {
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        direction: Option<SortDirection>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            direction,
            idle: SortDirection::Asc,
            on_click: None,
        }
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for TableSortLabel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let p = &theme.palette;
        let (size, weight, line_height, inherit) = cell_text(theme, true);
        let active = self.direction.is_some();
        let direction = self.direction.unwrap_or(self.idle);
        let ink = if active { p.text.primary } else { inherit };
        let arrow_ink = if active { p.text.secondary } else { ink };
        // `ArrowDownward` points down for descending, turned for ascending.
        let turn = if direction == SortDirection::Asc {
            0.5
        } else {
            0.
        };
        let group: SharedString = format!("sort-label-{:?}", self.id).into();
        let arrow = div()
            .flex()
            .flex_none()
            .mx(px(4.))
            .opacity(if active { 1. } else { 0. })
            .when_inactive(!active, group.clone())
            .child(
                icon("ArrowDownward", 18., arrow_ink)
                    .with_transformation(Transformation::rotate(percentage(turn))),
            );
        let mut root = div()
            .id(self.id)
            .group(group)
            .flex()
            .items_center()
            .cursor_pointer()
            .tab_stop(true)
            .focusable()
            .child(
                label(self.label)
                    .size(size)
                    .weight(weight)
                    .line_height(line_height)
                    .color(ink),
            )
            .child(arrow);
        if let Some(handler) = self.on_click {
            root = root.on_click(move |event, window, cx| handler(event, window, cx));
        }
        root
    }
}

trait InactiveArrow: InteractiveElement + Sized {
    /// An inactive label's arrow shows at 0.5 while the label is hovered.
    fn when_inactive(self, inactive: bool, group: SharedString) -> Self {
        if inactive {
            self.group_hover(group, |s| s.opacity(0.5))
        } else {
            self
        }
    }
}

impl InactiveArrow for Div {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Column {
        Name,
        Size,
    }

    #[test]
    fn a_click_flips_the_sorted_column_and_starts_others_ascending() {
        let mut order = SortOrder::new(Column::Name, SortDirection::Asc);
        order.request(Column::Name);
        assert_eq!(order, SortOrder::new(Column::Name, SortDirection::Desc));
        order.request(Column::Name);
        assert_eq!(order, SortOrder::new(Column::Name, SortDirection::Asc));
        order.request(Column::Name);
        order.request(Column::Size);
        assert_eq!(order, SortOrder::new(Column::Size, SortDirection::Asc));
        assert_eq!(order.direction_of(Column::Size), Some(SortDirection::Asc));
        assert_eq!(order.direction_of(Column::Name), None);
    }

    #[test]
    fn sorting_is_stable_in_both_directions() {
        let rows = [("b", 2), ("a", 1), ("c", 2), ("d", 1)];
        let by_size = |a: &(&str, i32), b: &(&str, i32)| a.1.cmp(&b.1);
        assert_eq!(
            sorted_indices(&rows, SortDirection::Asc, by_size),
            vec![1, 3, 0, 2]
        );
        // Descending turns the comparison around; ties keep their order.
        assert_eq!(
            sorted_indices(&rows, SortDirection::Desc, by_size),
            vec![0, 2, 1, 3]
        );
        let mut names = rows;
        sort_rows(&mut names, SortDirection::Desc, |a, b| a.0.cmp(b.0));
        assert_eq!(
            names.iter().map(|r| r.0).collect::<Vec<_>>(),
            ["d", "c", "b", "a"]
        );
    }

    #[test]
    fn directions_flip() {
        assert_eq!(SortDirection::Asc.flipped(), SortDirection::Desc);
        assert_eq!(SortDirection::Desc.flipped(), SortDirection::Asc);
        assert_eq!(SortDirection::Desc.apply(Ordering::Less), Ordering::Greater);
    }
}
