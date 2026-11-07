//! MUI `Slider` (horizontal, one thumb) under the Muxus theme.
//!
//! Geometry (MUI's styles): a 4px rail and track (`size="small"`: 2px) with
//! 13px of padding above and below, so the control is 30px (28px) tall; the
//! rail is the color at 0.38, the track the color itself, the thumb a 20px
//! (12px) disc centred on the value with elevation 2 (none when small). The
//! thumb grows a ring of the color at 0.16: 8px while hovered or focused
//! from the keyboard, 14px while pressed. Disabled sliders are grey 400
//! (grey 600 in dark mode).
//!
//! Behaviour (`useSlider`): a press anywhere on the control jumps to the
//! value under the pointer and a drag follows it, even outside the control;
//! the arrows step by `step` (with Shift by `shiftStep`), Page Up/Down by
//! `shiftStep`, Home/End jump to the ends. Values are rounded to `step`
//! from `min` (`roundValueToStep`) and clamped. The slider is a tab stop.

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    App, AppContext, BoxShadow, ElementId, EmptyView, EntityId, FocusHandle, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div, point, relative,
};

use crate::color::alpha;
use crate::theme::ActiveTheme;
use crate::widgets::button::elevation;
use crate::widgets::tooltip::{AnchorBounds, bounds_probe};

/// Called with the slider's new value.
pub type ChangeHandler = Rc<dyn Fn(f64, &mut Window, &mut App)>;

/// The values a slider takes: `min`, `max`, `step` and `shiftStep`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SliderRange {
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub shift_step: f64,
}

impl SliderRange {
    /// MUI's defaults: step 1, shiftStep 10.
    pub fn new(min: f64, max: f64) -> Self {
        Self {
            min,
            max,
            step: 1.,
            shift_step: 10.,
        }
    }

    pub fn step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }

    pub fn shift_step(mut self, step: f64) -> Self {
        self.shift_step = step;
        self
    }

    pub fn clamp(&self, value: f64) -> f64 {
        if value.is_nan() {
            return self.min;
        }
        value.max(self.min).min(self.max)
    }

    /// Where `value` sits along the rail, 0–1 (`valueToPercent` of the clamped value).
    pub fn fraction(&self, value: f64) -> f32 {
        if self.max <= self.min {
            return 0.;
        }
        ((self.clamp(value) - self.min) / (self.max - self.min)) as f32
    }

    /// The value under a pointer `fraction` of the way along the rail
    /// (`getValueAtFinger`): rounded to the step, clamped to the range.
    pub fn value_at(&self, fraction: f64) -> f64 {
        let value = (self.max - self.min) * fraction + self.min;
        let value = if self.step > 0. {
            round_value_to_step(value, self.step, self.min)
        } else {
            value
        };
        self.clamp(value)
    }

    /// The value a key press moves `value` to (`createHandleHiddenInputKeyDown`), or
    /// `None` for a key the slider does not take. GPUI key names: `left`, `pageup`…
    ///
    /// MUI adds the step to the value as it is; the sum is rounded to the step's
    /// decimals here so repeated presses do not collect floating point noise
    /// (1.05 + 0.05 stays 1.1).
    pub fn value_for_key(&self, value: f64, key: &str, shift: bool) -> Option<f64> {
        let step = if shift { self.shift_step } else { self.step };
        let up = |by: f64| (value + by).min(self.max);
        let down = |by: f64| (value - by).max(self.min);
        let next = match key {
            "up" | "right" => up(step),
            "down" | "left" => down(step),
            "pageup" => up(self.shift_step),
            "pagedown" => down(self.shift_step),
            "home" => self.min,
            "end" => self.max,
            _ => return None,
        };
        let decimals = decimal_precision(self.step).max(decimal_precision(self.shift_step));
        Some(self.clamp(to_fixed(next, decimals)))
    }
}

/// MUI's `getDecimalPrecision`: the decimals a step is written with
/// (`0.05` → 2, `2.5` → 1, `1` → 0).
pub fn decimal_precision(number: f64) -> usize {
    if number.abs() < 1. && number != 0. {
        // `toExponential`: "5e-2", "2.5e-1".
        let text = format!("{number:e}");
        let (mantissa, exponent) = text.split_once("e-").unwrap_or((&text, "0"));
        let decimals = mantissa.split_once('.').map_or(0, |(_, d)| d.len());
        return decimals + exponent.parse::<usize>().unwrap_or(0);
    }
    format!("{number}")
        .split_once('.')
        .map_or(0, |(_, decimals)| decimals.len())
}

/// MUI's `roundValueToStep`: the nearest multiple of `step` from `min`, written
/// with the step's decimals.
pub fn round_value_to_step(value: f64, step: f64, min: f64) -> f64 {
    // `Math.round` rounds halves up, toward +∞.
    let nearest = ((value - min) / step + 0.5).floor() * step + min;
    to_fixed(nearest, decimal_precision(step))
}

/// `Number(value.toFixed(decimals))`.
fn to_fixed(value: f64, decimals: usize) -> f64 {
    format!("{value:.decimals$}").parse().unwrap_or(value)
}

/// The keyed state a slider keeps between frames.
struct SliderState {
    focus: FocusHandle,
    bounds: AnchorBounds,
    /// Pressed (the 14px ring) until the button is released.
    active: bool,
}

/// What a slider drag carries: which slider it belongs to.
struct SliderDrag {
    owner: EntityId,
}

/// See the module docs.
#[derive(IntoElement)]
pub struct Slider {
    id: ElementId,
    value: f64,
    range: SliderRange,
    small: bool,
    disabled: bool,
    width: Option<f32>,
    color: Option<Hsla>,
    on_change: Option<ChangeHandler>,
}

impl Slider {
    pub fn new(id: impl Into<ElementId>, value: f64, range: SliderRange) -> Self {
        Self {
            id: id.into(),
            value,
            range,
            small: false,
            disabled: false,
            width: None,
            color: None,
            on_change: None,
        }
    }

    /// `size="small"`.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// `sx={{ width }}`; the full width of the container otherwise.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// `color`: the palette color (primary by default).
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// `onChange`: called with each new value while pressing, dragging or stepping.
    pub fn on_change(mut self, handler: impl Fn(f64, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

/// The ring MUI draws around the thumb (`0 0 0 <spread>px`).
fn ring(color: Hsla, spread: f32) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: alpha(color, 0.16),
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(spread),
        inset: false,
    }]
}

impl RenderOnce for Slider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| SliderState {
            focus: cx.focus_handle().tab_stop(true),
            bounds: AnchorBounds::default(),
            active: false,
        });
        let owner = state.entity_id();
        let (focus, bounds, active) = {
            let state = state.read(cx);
            (state.focus.clone(), state.bounds.clone(), state.active)
        };
        let theme = cx.theme();
        let p = &theme.palette;
        let color = if self.disabled {
            if theme.mode.is_dark() {
                p.grey.g600
            } else {
                p.grey.g400
            }
        } else {
            self.color.unwrap_or(p.primary.main)
        };
        let (thickness, thumb) = if self.small { (2., 12.) } else { (4., 20.) };
        let fraction = self.range.fraction(self.value);
        let focus_visible = focus.is_focused(window) && window.last_input_was_keyboard();
        let active = active && !self.disabled;

        let mut thumb_disc = div().size(px(thumb)).rounded(px(thumb / 2.)).bg(color);
        let mut shadow = if self.small {
            Vec::new()
        } else {
            elevation(2, theme)
        };
        if active {
            shadow.extend(ring(color, 14.));
        } else if focus_visible && !self.disabled {
            shadow.extend(ring(color, 8.));
        }
        thumb_disc = thumb_disc.shadow(shadow.clone());
        if !self.disabled && !active {
            let mut hovered = shadow;
            hovered.extend(ring(color, 8.));
            thumb_disc = thumb_disc.group_hover("slider-thumb", move |s| s.shadow(hovered));
        }
        // The thumb's 42px hit target (`::after`), centred on the value.
        let hit = 42.;
        let center_y = 13. + thickness / 2.;
        let thumb_box = div()
            .group("slider-thumb")
            .absolute()
            .left(relative(fraction))
            .top(px(center_y - hit / 2.))
            .ml(px(-hit / 2.))
            .size(px(hit))
            .flex()
            .items_center()
            .justify_center()
            .child(thumb_disc);

        let mut root = div()
            .id(self.id)
            .relative()
            .flex_none()
            .h(px(26. + thickness))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(13.))
                    .h(px(thickness))
                    .rounded(px(thickness / 2.))
                    .bg(alpha(color, 0.38)),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(13.))
                    .h(px(thickness))
                    .w(relative(fraction))
                    .rounded(px(thickness / 2.))
                    .bg(color),
            )
            .child(thumb_box)
            .child(bounds_probe(&bounds));
        root = match self.width {
            Some(width) => root.w(px(width)),
            None => root.w_full(),
        };
        if self.disabled {
            return root;
        }

        let range = self.range;
        let value = self.value;
        let on_change = self.on_change;
        let change = move |next: f64, window: &mut Window, cx: &mut App| {
            if let Some(handler) = &on_change
                && next != value
            {
                handler(next, window, cx);
            }
        };
        let change = Rc::new(change);
        let press_change = change.clone();
        let press_state = state.clone();
        let release_state = state.clone();
        let release_out_state = state.clone();
        let drag_change = change.clone();
        root.cursor_pointer()
            .track_focus(&focus)
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                if let Some(bounds) = bounds.get()
                    && bounds.size.width > px(0.)
                {
                    let x = (event.position.x - bounds.origin.x) / bounds.size.width;
                    press_change(range.value_at(f64::from(x)), window, cx);
                }
                press_state.update(cx, |state, cx| {
                    state.active = true;
                    cx.notify();
                });
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                release_state.update(cx, |state, cx| {
                    state.active = false;
                    cx.notify();
                });
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                release_out_state.update(cx, |state, cx| {
                    if state.active {
                        state.active = false;
                        cx.notify();
                    }
                });
            })
            .on_drag(SliderDrag { owner }, |_, _, _, cx| cx.new(|_| EmptyView))
            .on_drag_move::<SliderDrag>(move |event, window, cx| {
                if event.drag(cx).owner != owner || event.bounds.size.width <= px(0.) {
                    return;
                }
                let x = (event.event.position.x - event.bounds.origin.x) / event.bounds.size.width;
                drag_change(range.value_at(f64::from(x)), window, cx);
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                if keystroke.modifiers.control || keystroke.modifiers.alt {
                    return;
                }
                if let Some(next) =
                    range.value_for_key(value, &keystroke.key, keystroke.modifiers.shift)
                {
                    cx.stop_propagation();
                    change(next, window, cx);
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_precision_matches_mui() {
        assert_eq!(decimal_precision(1.), 0);
        assert_eq!(decimal_precision(5.), 0);
        assert_eq!(decimal_precision(0.05), 2);
        assert_eq!(decimal_precision(0.25), 2);
        assert_eq!(decimal_precision(0.1), 1);
        assert_eq!(decimal_precision(2.5), 1);
        assert_eq!(decimal_precision(0.00000001), 8);
    }

    #[test]
    fn values_round_to_the_step_from_min() {
        assert_eq!(round_value_to_step(1.123, 0.05, 1.), 1.1);
        assert_eq!(round_value_to_step(1.126, 0.05, 1.), 1.15);
        assert_eq!(round_value_to_step(12.4, 1., 8.), 12.);
        // Halves round up, as `Math.round` does.
        assert_eq!(round_value_to_step(12.5, 1., 8.), 13.);
        assert_eq!(round_value_to_step(22., 5., 10.), 20.);
        assert_eq!(round_value_to_step(23., 5., 10.), 25.);
        // Steps count from `min`, not from zero.
        assert_eq!(round_value_to_step(13., 5., 1.), 11.);
    }

    #[test]
    fn a_pointer_position_maps_to_a_stepped_clamped_value() {
        let font = SliderRange::new(8., 24.);
        assert_eq!(font.value_at(0.), 8.);
        assert_eq!(font.value_at(1.), 24.);
        assert_eq!(font.value_at(0.5), 16.);
        assert_eq!(font.value_at(0.53), 16.);
        assert_eq!(font.value_at(-0.2), 8.);
        assert_eq!(font.value_at(1.4), 24.);
        let line = SliderRange::new(1., 1.6).step(0.05);
        assert_eq!(line.value_at(0.5), 1.3);
        assert_eq!(line.value_at(0.26), 1.15);
        assert_eq!(line.value_at(1.), 1.6);
        let dim = SliderRange::new(10., 60.).step(5.);
        assert_eq!(dim.value_at(0.31), 25.);
    }

    #[test]
    fn a_value_maps_to_a_clamped_fraction() {
        let range = SliderRange::new(8., 24.);
        assert_eq!(range.fraction(8.), 0.);
        assert_eq!(range.fraction(16.), 0.5);
        assert_eq!(range.fraction(40.), 1.);
        assert_eq!(range.fraction(0.), 0.);
        assert_eq!(SliderRange::new(5., 5.).fraction(5.), 0.);
    }

    #[test]
    fn keys_step_as_mui_does() {
        let range = SliderRange::new(8., 24.);
        assert_eq!(range.value_for_key(12., "right", false), Some(13.));
        assert_eq!(range.value_for_key(12., "up", false), Some(13.));
        assert_eq!(range.value_for_key(12., "left", false), Some(11.));
        assert_eq!(range.value_for_key(12., "down", false), Some(11.));
        assert_eq!(range.value_for_key(12., "right", true), Some(22.));
        assert_eq!(range.value_for_key(20., "right", true), Some(24.));
        assert_eq!(range.value_for_key(12., "pageup", false), Some(22.));
        assert_eq!(range.value_for_key(12., "pagedown", false), Some(8.));
        assert_eq!(range.value_for_key(12., "home", false), Some(8.));
        assert_eq!(range.value_for_key(12., "end", false), Some(24.));
        assert_eq!(range.value_for_key(24., "right", false), Some(24.));
        assert_eq!(range.value_for_key(12., "a", false), None);
        let line = SliderRange::new(1., 1.6).step(0.05);
        assert_eq!(line.value_for_key(1.05, "right", false), Some(1.1));
        assert_eq!(line.value_for_key(1.1, "right", false), Some(1.15));
        assert_eq!(line.value_for_key(1.6, "right", false), Some(1.6));
    }
}
