//! MUI `Tooltip` (`MuiTooltip` override: #2e2e34, 12px, 6px 10px padding,
//! 6px corners, arrow on).
//!
//! MUI centres the bubble on the control it describes: 14px below it (or
//! above, beside) with a 12×6 arrow pointing back at it. GPUI lays a tooltip
//! out at the pointer, so controls record their bounds while painting
//! ([`bounds_probe`]) and the tooltip view offsets the bubble from the pointer
//! to where MUI would put it.

use std::cell::Cell;
use std::rc::Rc;

use crate::zoom::{css, layout_px, px};
use gpui_kit::{
    AnyView, App, AppContext, Bounds, Context, FontWeight, IntoElement, ParentElement, Pixels,
    Point, Render, SharedString, Styled, Window, canvas, div,
};

use crate::icon::icon;
use crate::label::label;
use crate::theme::{ActiveTheme, fonts};

/// Where the bubble sits relative to its control (`placement`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
}

/// Shared slot a control writes its painted bounds into.
pub type AnchorBounds = Rc<Cell<Option<Bounds<Pixels>>>>;

/// An invisible, absolutely positioned element that records its parent's
/// bounds every frame. Put it inside the control as the last child. It is
/// pinned to the parent's top-left corner: in a parent that is not a flex
/// container, an absolute child without offsets would sit at its static
/// position, below the control's content.
pub fn bounds_probe(anchor: &AnchorBounds) -> impl IntoElement {
    let anchor = anchor.clone();
    canvas(
        move |bounds, _, _| anchor.set(Some(bounds)),
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// A popup placed from `anchor`'s bounds as they were painted last frame: when
/// the control has moved since (a window settling at its size, a list loading
/// above it), the view renders once more, so the popup never stays where the
/// control was. Call it from the render that placed the popup at `used`.
pub fn follow_anchor<V: 'static>(
    anchor: &AnchorBounds,
    used: Bounds<Pixels>,
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::Context<V>,
) {
    let anchor = anchor.clone();
    cx.on_next_frame(window, move |_, _, cx| {
        if anchor.get() != Some(used) {
            cx.notify();
        }
    });
}

/// MUI's gap between control and bubble, arrow included.
const GAP: f32 = 14.;
const ARROW_W: f32 = 12.;
const ARROW_H: f32 = 6.;
/// Wide enough for any bubble (MUI caps them at 300px), so it can centre itself.
const SLOT: f32 = 640.;

/// A plain text tooltip view.
pub struct Tooltip {
    text: SharedString,
    lines: Vec<SharedString>,
    anchor: Option<Bounds<Pixels>>,
    pointer: Point<Pixels>,
    placement: Placement,
}

impl Tooltip {
    /// Builder for `.tooltip(...)`: bubble at the pointer (no recorded anchor).
    pub fn text(
        text: impl Into<SharedString>,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        Self::build(text.into(), Vec::new(), None, Placement::Bottom)
    }

    /// Builder for a control that records its bounds into `anchor`.
    pub fn anchored(
        text: impl Into<SharedString>,
        anchor: AnchorBounds,
        placement: Placement,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        Self::build(text.into(), Vec::new(), Some(anchor), placement)
    }

    /// A multi-line card: a bold first line, then dimmer detail lines (the host hover card).
    pub fn card(
        title: impl Into<SharedString>,
        lines: Vec<SharedString>,
        anchor: Option<AnchorBounds>,
        placement: Placement,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        Self::build(title.into(), lines, anchor, placement)
    }

    fn build(
        text: SharedString,
        lines: Vec<SharedString>,
        anchor: Option<AnchorBounds>,
        placement: Placement,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        move |window, cx| {
            let pointer = window.mouse_position();
            let anchor = anchor.as_ref().and_then(|cell| cell.get());
            let (text, lines) = (text.clone(), lines.clone());
            cx.new(|_| Tooltip {
                text,
                lines,
                anchor,
                pointer,
                placement,
            })
            .into()
        }
    }
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bg = theme.components.tooltip_bg;
        let mut bubble = div()
            .max_w(px(300.))
            .px(px(10.))
            .py(px(6.))
            .rounded(px(6.))
            .bg(bg)
            .text_color(theme.palette.common_white)
            .font_family(fonts::UI)
            .text_size(px(12.))
            .font_weight(FontWeight(500.))
            .line_height(px(18.));
        bubble = if self.lines.is_empty() {
            bubble.child(
                label(self.text.clone())
                    .size(12.)
                    .weight(500.)
                    .line_height(18.)
                    .color(theme.palette.common_white)
                    .no_truncate(),
            )
        } else {
            bubble
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().font_weight(FontWeight(600.)).child(self.text.clone()))
                .children(
                    self.lines
                        .iter()
                        .map(|line| div().opacity(0.7).child(line.clone())),
                )
        };
        let Some(anchor) = self.anchor else {
            return div().mt(px(GAP)).child(bubble);
        };
        // GPUI puts this view's origin 1px right of and below the pointer.
        let origin = self.pointer + gpui_kit::point(layout_px(1.), layout_px(1.));
        let center_x = f32::from(anchor.center().x - origin.x);
        let center_y = f32::from(anchor.center().y - origin.y);
        let arrow = |rotation: f32| {
            icon("TooltipArrow", ARROW_W, bg)
                .h(px(ARROW_H))
                .with_transformation(gpui_kit::Transformation::rotate(gpui_kit::radians(
                    rotation,
                )))
        };
        let slot = match self.placement {
            Placement::Bottom => div()
                .absolute()
                .left(layout_px(center_x - css(SLOT) / 2.))
                .top(layout_px(
                    f32::from(anchor.bottom() - origin.y) + css(GAP - ARROW_H),
                ))
                .w(px(SLOT))
                .flex()
                .flex_col()
                .items_center()
                .child(arrow(0.))
                .child(bubble),
            Placement::Top => div()
                .absolute()
                .left(layout_px(center_x - css(SLOT) / 2.))
                .bottom(layout_px(
                    f32::from(origin.y - anchor.top()) + css(GAP - ARROW_H),
                ))
                .w(px(SLOT))
                .flex()
                .flex_col()
                .items_center()
                .child(bubble)
                .child(arrow(std::f32::consts::PI)),
            Placement::Right => div()
                .absolute()
                .left(layout_px(
                    f32::from(anchor.right() - origin.x) + css(GAP - ARROW_H),
                ))
                .top(layout_px(center_y - css(SLOT) / 2.))
                .h(px(SLOT))
                .flex()
                .items_center()
                .child(
                    div()
                        .w(px(ARROW_H))
                        .flex()
                        .justify_center()
                        .child(arrow(-std::f32::consts::FRAC_PI_2)),
                )
                .child(bubble),
            Placement::Left => div()
                .absolute()
                .right(layout_px(
                    f32::from(origin.x - anchor.left()) + css(GAP - ARROW_H),
                ))
                .top(layout_px(center_y - css(SLOT) / 2.))
                .h(px(SLOT))
                .flex()
                .items_center()
                .child(bubble)
                .child(
                    div()
                        .w(px(ARROW_H))
                        .flex()
                        .justify_center()
                        .child(arrow(std::f32::consts::FRAC_PI_2)),
                ),
        };
        div().size(px(0.)).child(slot)
    }
}
