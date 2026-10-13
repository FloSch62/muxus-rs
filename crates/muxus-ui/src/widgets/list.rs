//! MUI `List` pieces as the theme styles them: `ListItemButton` (6px
//! corners, 1px × 6px margins, the list hover tint and the selected pill),
//! `ListItemIcon` and `ListItemText`.
//!
//! Rows stack in block layout in the browser, so the 1px top and bottom
//! margins of neighbouring rows collapse into one: a row here keeps only its
//! top margin and the list adds the last row's bottom margin itself.
//!
//! Chrome stacks rows at fractional positions while GPUI snaps every box to
//! the device pixel grid, so a long list of equal rows drifts.
//! [`pitch_correction`] gives each row the half pixel it needs to start
//! where Chrome puts it.

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Point, RenderOnce, Stateful, StatefulInteractiveElement, Styled, Window,
    div,
};

use crate::theme::ActiveTheme;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type HoverHandler = Rc<dyn Fn(&mut Window, &mut App)>;
type PointHandler = Rc<dyn Fn(&Point<Pixels>, &mut Window, &mut App)>;

/// The theme's `MuiListItemButton` margin (`1px 6px`).
pub const ROW_MARGIN_X: f32 = 6.;
/// The theme's `MuiListItemButton` corner radius.
pub const ROW_RADIUS: f32 = 6.;

/// `ListItemButton`.
#[derive(IntoElement)]
pub struct ListItemButton {
    id: ElementId,
    selected: bool,
    disabled: bool,
    radius: f32,
    margin_x: f32,
    padding_x: f32,
    padding_y: f32,
    min_height: Option<f32>,
    extra_bottom: f32,
    align_start: bool,
    hover: bool,
    divider: bool,
    children: Vec<AnyElement>,
    on_click: Option<ClickHandler>,
    on_hover: Option<HoverHandler>,
    on_right_click: Option<PointHandler>,
}

impl ListItemButton {
    /// A row with MUI's default 16px × 8px padding.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            selected: false,
            disabled: false,
            radius: ROW_RADIUS,
            margin_x: ROW_MARGIN_X,
            padding_x: 16.,
            padding_y: 8.,
            min_height: None,
            extra_bottom: 0.,
            align_start: false,
            hover: true,
            divider: false,
            children: Vec::new(),
            on_click: None,
            on_hover: None,
            on_right_click: None,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Dimmed to 0.38 and inert.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// `sx={{ borderRadius }}` in px.
    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    /// `sx={{ mx }}` in px.
    pub fn margin_x(mut self, margin: f32) -> Self {
        self.margin_x = margin;
        self
    }

    /// `sx={{ px, py }}` in px.
    pub fn padding(mut self, x: f32, y: f32) -> Self {
        self.padding_x = x;
        self.padding_y = y;
        self
    }

    /// `sx={{ minHeight }}` (border box) in px.
    pub fn min_height(mut self, height: f32) -> Self {
        self.min_height = Some(height);
        self
    }

    /// Extra space under the row (see [`pitch_correction`]).
    pub fn extra_bottom(mut self, extra: f32) -> Self {
        self.extra_bottom = extra;
        self
    }

    /// `alignItems: 'flex-start'`.
    pub fn align_start(mut self) -> Self {
        self.align_start = true;
        self
    }

    /// A divider under the row (`borderBottom: 1`).
    pub fn divider(mut self) -> Self {
        self.divider = true;
        self
    }

    /// No hover tint (lists that move the selection with the pointer).
    pub fn no_hover(mut self) -> Self {
        self.hover = false;
        self
    }

    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_any_element());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// The pointer moved over the row.
    pub fn on_hover(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_hover = Some(Rc::new(handler));
        self
    }

    /// A right click, with the window position it landed on.
    pub fn on_right_click(
        mut self,
        handler: impl Fn(&Point<Pixels>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_right_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ListItemButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let c = &theme.components;
        let mut row: Stateful<Div> = div()
            .id(self.id)
            .relative()
            .flex()
            .flex_none()
            .mx(px(self.margin_x))
            .mt(px(1.))
            .mb(px(self.extra_bottom))
            .px(px(self.padding_x))
            .py(px(self.padding_y))
            .rounded(px(self.radius));
        row = if self.align_start {
            row.items_start()
        } else {
            row.items_center()
        };
        if let Some(height) = self.min_height {
            row = row.min_h(px(height));
        }
        if self.divider {
            row = row.border_b_1().border_color(theme.palette.divider);
        }
        if self.selected {
            row = row.bg(c.selected_pill);
        }
        if self.disabled {
            row = row.opacity(0.38);
        } else {
            row = row.cursor_pointer();
            if self.hover {
                let hover = if self.selected {
                    c.selected_pill_hover
                } else {
                    c.list_hover
                };
                row = row.hover(move |s| s.bg(hover));
            }
            if let Some(handler) = self.on_click {
                row = row
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(move |event, window, cx| handler(event, window, cx));
            }
            if let Some(handler) = self.on_hover {
                row = row.on_mouse_move(move |_, window, cx| handler(window, cx));
            }
            if let Some(handler) = self.on_right_click {
                row = row.on_mouse_down(MouseButton::Right, move |event, window, cx| {
                    cx.stop_propagation();
                    handler(&event.position, window, cx)
                });
            }
        }
        row.children(self.children)
    }
}

/// `ListSubheader`: a section label in a list or menu (body2, 500,
/// text.secondary, a 48px line, 16px in).
#[derive(IntoElement)]
pub struct ListSubheader {
    label: gpui_kit::SharedString,
    compact: bool,
    caps: bool,
}

impl ListSubheader {
    pub fn new(label: impl Into<gpui_kit::SharedString>) -> Self {
        Self {
            label: label.into(),
            compact: false,
            caps: false,
        }
    }

    /// The host picker's folder headers: `fontSize: 10.5`, a 26px line,
    /// uppercase with 0.65px tracking.
    pub fn caps(mut self) -> Self {
        self.caps = true;
        self
    }

    /// The command menu's group headers: `lineHeight: '28px'`,
    /// `fontSize: 12`, `fontWeight: 600`, no background.
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }
}

impl RenderOnce for ListSubheader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let p = &theme.palette;
        let body2 = theme.typography.body2;
        let (size, weight, line, tracking) = if self.compact {
            (12., 600., 28., 0.)
        } else if self.caps {
            (10.5, 500., 26., 0.65)
        } else {
            (body2.size, 500., 48., 0.)
        };
        let content = if self.caps {
            gpui_kit::SharedString::from(self.label.to_uppercase())
        } else {
            self.label
        };
        let mut header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(line))
            .px(px(16.));
        if !self.compact {
            header = header.bg(p.background_paper);
        }
        header.child(
            crate::label::label(content)
                .size(size)
                .weight(weight)
                .line_height(line)
                .tracking(tracking)
                .color(p.text.secondary),
        )
    }
}

/// `ListItemIcon` with its `minWidth` (MUI: 56px; dense dialogs use less).
pub fn list_item_icon(min_width: f32, icon: impl IntoElement) -> Div {
    div().flex().flex_none().min_w(px(min_width)).child(icon)
}

/// `ListItemText`: a primary line and an optional secondary one, with MUI's
/// 4px (6px with two lines) vertical margins.
pub fn list_item_text(primary: impl IntoElement, secondary: Option<AnyElement>) -> Div {
    let two = secondary.is_some();
    let mut column = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .my(px(if two { 6. } else { 4. }))
        .child(primary);
    if let Some(secondary) = secondary {
        column = column.child(secondary);
    }
    column
}

/// The extra space (0 or half a pixel) row `index` needs so that the next
/// row starts where Chrome puts it, given the row's pitch in Chrome and the
/// pitch GPUI lays it out at.
pub fn pitch_correction(index: usize, chrome_pitch: f32, gpui_pitch: f32) -> f32 {
    let snap = |value: f32| (value * 2.).round() / 2.;
    let here = snap(index as f32 * chrome_pitch);
    let next = snap((index + 1) as f32 * chrome_pitch);
    (next - here - gpui_pitch).max(0.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spreads_the_fraction_over_the_rows() {
        let extras: Vec<f32> = (0..4).map(|i| pitch_correction(i, 61.38, 61.)).collect();
        assert_eq!(extras, [0.5, 0.5, 0., 0.5]);
        assert_eq!(pitch_correction(0, 61., 61.), 0.);
    }
}
