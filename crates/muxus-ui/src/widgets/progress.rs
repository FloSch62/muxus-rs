//! MUI `CircularProgress`: indeterminate, a 3.6/44-thick arc that grows,
//! shrinks and turns, 1.4 s per cycle; or `determinate`, an arc from the top,
//! clockwise, over `value` percent of the circle. `size` is the circle's
//! diameter in px (`size={15}`), `color` its ink (`color="inherit"` passes the
//! text color), `thickness` the stroke in the 44-unit box (MUI's default 3.6).

use std::f32::consts::TAU;
use std::time::Duration;

use crate::zoom::px;
use gpui_kit::{
    Animation, AnimationExt, App, ElementId, Hsla, IntoElement, ParentElement, PathBuilder, Pixels,
    Point, RenderOnce, Styled, Window, canvas, div, point,
};

#[derive(IntoElement)]
pub struct CircularProgress {
    id: ElementId,
    size: f32,
    color: Hsla,
    thickness: f32,
    /// `variant="determinate"` with this `value` (0–100).
    value: Option<f32>,
}

impl CircularProgress {
    pub fn new(id: impl Into<ElementId>, size: f32, color: Hsla) -> Self {
        Self {
            id: id.into(),
            size,
            color,
            thickness: 3.6,
            value: None,
        }
    }

    /// The stroke width in the 44-unit box (`thickness={6}`).
    pub fn thickness(mut self, thickness: f32) -> Self {
        self.thickness = thickness;
        self
    }

    /// `variant="determinate" value={value}`: a still arc over `value` percent.
    pub fn determinate(mut self, value: f32) -> Self {
        self.value = Some(value.clamp(0., 100.));
        self
    }
}

/// The circle in the 44-unit box: radius and stroke, scaled to `size`.
fn ring(size: f32, thickness: f32) -> (f32, f32) {
    let scale = size / 44.;
    ((44. - thickness) / 2. * scale, thickness * scale)
}

impl RenderOnce for CircularProgress {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (size, color, thickness) = (self.size, self.color, self.thickness);
        if let Some(value) = self.value {
            return div()
                .size(px(size))
                .flex_none()
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let (radius, stroke) = ring(size, thickness);
                            let sweep = value / 100. * TAU;
                            if sweep > 0. {
                                paint_ring_arc(
                                    bounds.origin,
                                    size,
                                    radius,
                                    stroke,
                                    -TAU / 4.,
                                    sweep,
                                    color,
                                    window,
                                );
                            }
                        },
                    )
                    .size_full(),
                )
                .into_any_element();
        }
        div()
            .size(px(size))
            .flex_none()
            .with_animation(
                self.id,
                Animation::new(Duration::from_millis(1400)).repeat(),
                move |element, delta| {
                    element.child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, window, _| {
                                paint_arc(bounds.origin, size, thickness, delta, color, window)
                            },
                        )
                        .size_full(),
                    )
                },
            )
            .into_any_element()
    }
}

fn paint_arc(
    origin: Point<Pixels>,
    size: f32,
    thickness: f32,
    delta: f32,
    color: Hsla,
    window: &mut Window,
) {
    // MUI's keyframes on a 44-unit circle (radius 20.2 at the default thickness): the dash
    // runs from 1 to 100 units and back while its offset moves -15 then -125.
    let (radius, stroke) = ring(size, thickness);
    let circumference = TAU * (44. - thickness) / 2.;
    let (dash, offset) = if delta < 0.5 {
        let t = delta / 0.5;
        (1. + 99. * t, -15. * t)
    } else {
        let t = (delta - 0.5) / 0.5;
        (100. - 99. * t, -15. - 110. * t)
    };
    let rotation = delta * TAU - TAU / 4.;
    let start = rotation - offset / circumference * TAU;
    let sweep = (dash / circumference * TAU).max(0.02);
    paint_ring_arc(origin, size, radius, stroke, start, sweep, color, window);
}

/// An arc of the ring from `start` over `sweep` radians, clockwise.
#[allow(clippy::too_many_arguments)]
fn paint_ring_arc(
    origin: Point<Pixels>,
    size: f32,
    radius: f32,
    stroke: f32,
    start: f32,
    sweep: f32,
    color: Hsla,
    window: &mut Window,
) {
    let center = point(origin.x + px(size / 2.), origin.y + px(size / 2.));
    let steps = 24;
    let mut builder = PathBuilder::stroke(px(stroke));
    for step in 0..=steps {
        let angle = start + sweep * step as f32 / steps as f32;
        let at = point(
            center.x + px(radius * angle.cos()),
            center.y + px(radius * angle.sin()),
        );
        if step == 0 {
            builder.move_to(at);
        } else {
            builder.line_to(at);
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// MUI's indeterminate `LinearProgress` at `height` px (the launcher's 2px
/// strip under its search box): the primary color at 0.25 for the track
/// with a primary segment, as the bar's first keyframe shows it.
pub fn linear_progress_indeterminate(theme: &crate::theme::Theme, height: f32) -> gpui_kit::Div {
    let primary = theme.palette.primary.main;
    div()
        .h(px(height))
        .w_full()
        .flex_none()
        .overflow_hidden()
        .bg(crate::color::alpha(primary, 0.25))
        .child(div().h_full().w(gpui_kit::relative(0.4)).bg(primary))
}
