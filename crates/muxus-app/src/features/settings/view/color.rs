//! The picker a color well opens. The browser hands `<input type="color">`
//! to the system's color dialog; the native app shows its own popover below
//! the well: a saturation/value square, a hue strip and the hex value.
//! Every change applies at once, as the input's `change` events do.

use crate::features::settings::palette::{PICKER_BLACK, PICKER_WHITE};
use std::rc::Rc;

use muxus_ui::color::{hex, try_hex};
use muxus_ui::gpui_kit::{
    AnyElement, Context, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement, Rgba,
    SharedString, Styled, Window, div, linear_color_stop, linear_gradient, px,
};
use muxus_ui::theme::ActiveTheme;
use muxus_ui::widgets::{bounds_probe, menu};

use super::fields::field;
use super::window::{SettingsWindow, SliderDrag};

pub type ColorHandler = Rc<dyn Fn(&mut SettingsWindow, String, &mut Context<SettingsWindow>)>;

const SQUARE_W: f32 = 220.;
const SQUARE_H: f32 = 150.;
const HUE_H: f32 = 12.;

/// The open picker: which well, the color as hue/saturation/value, and
/// where changes go.
pub struct ColorPicker {
    pub key: String,
    pub hsv: (f32, f32, f32),
    pub on_change: ColorHandler,
}

/// `#rrggbb` → hue (0–360), saturation and value (0–1).
pub fn hex_to_hsv(value: &str) -> Option<(f32, f32, f32)> {
    let rgba: Rgba = try_hex(value)?.into();
    let (r, g, b) = (rgba.r, rgba.g, rgba.b);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let hue = if delta == 0. {
        0.
    } else if max == r {
        60. * (((g - b) / delta).rem_euclid(6.))
    } else if max == g {
        60. * ((b - r) / delta + 2.)
    } else {
        60. * ((r - g) / delta + 4.)
    };
    let saturation = if max == 0. { 0. } else { delta / max };
    Some((hue, saturation, max))
}

/// Hue, saturation and value back to `#rrggbb` (lowercase, as color inputs report).
pub fn hsv_to_hex(hue: f32, saturation: f32, value: f32) -> String {
    let chroma = value * saturation;
    let section = (hue.rem_euclid(360.)) / 60.;
    let x = chroma * (1. - (section.rem_euclid(2.) - 1.).abs());
    let (r, g, b) = match section as u32 {
        0 => (chroma, x, 0.),
        1 => (x, chroma, 0.),
        2 => (0., chroma, x),
        3 => (0., x, chroma),
        4 => (x, 0., chroma),
        _ => (chroma, 0., x),
    };
    let m = value - chroma;
    let channel = |c: f32| ((c + m) * 255.).round().clamp(0., 255.) as u8;
    format!("#{:02x}{:02x}{:02x}", channel(r), channel(g), channel(b))
}

fn hue_color(hue: f32) -> Hsla {
    hex(&hsv_to_hex(hue, 1., 1.))
}

impl SettingsWindow {
    /// Open the picker under the well recorded as `key`.
    pub fn open_color_picker(
        &mut self,
        key: &str,
        value: &str,
        on_change: ColorHandler,
        cx: &mut Context<Self>,
    ) {
        let hsv = hex_to_hsv(value).unwrap_or((0., 0., 0.));
        self.drop_inputs("color-picker-hex");
        self.state.color_picker = Some(ColorPicker {
            key: key.to_string(),
            hsv,
            on_change,
        });
        cx.notify();
    }

    fn set_picker_hsv(&mut self, hsv: (f32, f32, f32), cx: &mut Context<Self>) {
        let Some(picker) = &mut self.state.color_picker else {
            return;
        };
        picker.hsv = hsv;
        let on_change = picker.on_change.clone();
        let value = hsv_to_hex(hsv.0, hsv.1, hsv.2);
        on_change(self, value, cx);
        cx.notify();
    }

    pub fn render_color_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (key, (hue, saturation, value)) = self
            .state
            .color_picker
            .as_ref()
            .map(|picker| (picker.key.clone(), picker.hsv))?;
        let well = self.anchor(&key);
        let position = menu::below(&well)?;
        let theme = *cx.theme();
        let square_anchor = self.anchor("color-picker-square");
        let hue_anchor = self.anchor("color-picker-hue");
        let current = hsv_to_hex(hue, saturation, value);

        let square_drag = square_anchor.clone();
        let square = div()
            .id("color-picker-square")
            .relative()
            .w(px(SQUARE_W))
            .h(px(SQUARE_H))
            .rounded(px(6.))
            .overflow_hidden()
            .bg(hue_color(hue))
            .cursor_crosshair()
            .child(div().absolute().inset_0().bg(linear_gradient(
                90.,
                linear_color_stop(hex(PICKER_WHITE), 0.),
                linear_color_stop(Hsla::white().opacity(0.), 1.),
            )))
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(Hsla::black().opacity(0.), 0.),
                linear_color_stop(hex(PICKER_BLACK), 1.),
            )))
            .child(
                div()
                    .absolute()
                    .left(px(saturation * SQUARE_W - 6.))
                    .top(px((1. - value) * SQUARE_H - 6.))
                    .size(px(12.))
                    .rounded(px(6.))
                    .border_2()
                    .border_color(hex(PICKER_WHITE))
                    .shadow_sm(),
            )
            .child(bounds_probe(&square_anchor))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(
                    move |this, event: &muxus_ui::gpui_kit::MouseDownEvent, _, cx| {
                        let Some(bounds) = square_drag.get() else {
                            return;
                        };
                        let fx = (muxus_ui::zoom::unzoom(event.position.x - bounds.origin.x)
                            / SQUARE_W)
                            .clamp(0., 1.);
                        let fy = (muxus_ui::zoom::unzoom(event.position.y - bounds.origin.y)
                            / SQUARE_H)
                            .clamp(0., 1.);
                        let hue = this.state.color_picker.as_ref().map_or(0., |p| p.hsv.0);
                        this.set_picker_hsv((hue, fx, 1. - fy), cx);
                        this.drag = Some(SliderDrag {
                            anchor: square_drag.clone(),
                            on_move: Rc::new(|_, _, _| {}),
                            on_move_2d: Some(Rc::new(|this, fx, fy, cx| {
                                let hue = this.state.color_picker.as_ref().map_or(0., |p| p.hsv.0);
                                this.set_picker_hsv((hue, fx, 1. - fy), cx);
                            })),
                        });
                    },
                ),
            );

        let mut strip = div()
            .id("color-picker-hue")
            .relative()
            .flex()
            .w(px(SQUARE_W))
            .h(px(HUE_H))
            .rounded(px(6.))
            .overflow_hidden()
            .cursor_pointer();
        for segment in 0..6 {
            let from = segment as f32 * 60.;
            strip = strip.child(div().flex_1().h_full().bg(linear_gradient(
                90.,
                linear_color_stop(hue_color(from), 0.),
                linear_color_stop(hue_color(from + 59.999), 1.),
            )));
        }
        let hue_drag = hue_anchor.clone();
        let strip = strip
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(hue / 360. * SQUARE_W - 3.))
                    .w(px(6.))
                    .h(px(HUE_H))
                    .rounded(px(3.))
                    .border_2()
                    .border_color(hex(PICKER_WHITE)),
            )
            .child(bounds_probe(&hue_anchor))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(
                    move |this, event: &muxus_ui::gpui_kit::MouseDownEvent, _, cx| {
                        let Some(bounds) = hue_drag.get() else {
                            return;
                        };
                        let fx = (muxus_ui::zoom::unzoom(event.position.x - bounds.origin.x)
                            / SQUARE_W)
                            .clamp(0., 1.);
                        let (_, s, v) = this
                            .state
                            .color_picker
                            .as_ref()
                            .map_or((0., 0., 0.), |p| p.hsv);
                        this.set_picker_hsv((fx * 360., s, v), cx);
                        this.drag = Some(SliderDrag {
                            anchor: hue_drag.clone(),
                            on_move: Rc::new(|this, fx, cx| {
                                let (_, s, v) = this
                                    .state
                                    .color_picker
                                    .as_ref()
                                    .map_or((0., 0., 0.), |p| p.hsv);
                                this.set_picker_hsv((fx * 360., s, v), cx);
                            }),
                            on_move_2d: None,
                        });
                    },
                ),
            );
        let content = menu::paper(&theme).py(px(0.)).child(
            div()
                .id("color-picker")
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(12.))
                .child(square)
                .child(strip)
                .child(self.color_picker_hex(&theme, &current, window, cx)),
        );
        let weak = cx.entity().downgrade();
        Some(
            menu::popup(
                position,
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.state.color_picker = None;
                        cx.notify();
                    });
                },
                content,
            )
            .into_any_element(),
        )
    }

    fn color_picker_hex(
        &mut self,
        theme: &muxus_ui::theme::Theme,
        current: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let initial = current.to_string();
        let state = self.input(
            "color-picker-hex",
            || initial.clone(),
            None,
            false,
            window,
            cx,
            |this, value, _, cx| {
                let text = muxus_core::js::js_trim(&value).to_string();
                let text = if text.starts_with('#') {
                    text
                } else {
                    format!("#{text}")
                };
                // Only a complete `#rrggbb` is a color the wells accept.
                if text.len() == 7
                    && let Some(hsv) = hex_to_hsv(&text)
                    && let Some(picker) = &mut this.state.color_picker
                {
                    picker.hsv = hsv;
                    let on_change = picker.on_change.clone();
                    on_change(this, text.to_lowercase(), cx);
                }
            },
        );
        self.sync_input("color-picker-hex", current, window, cx);
        field(SharedString::from("color-picker-hex-field"), &state)
            .font(muxus_ui::theme::fonts::MONO, 13.)
            .render(theme, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips_hex_colors() {
        for color in [
            "#ffffff", "#000000", "#dc2626", "#3b66f5", "#16a34a", "#713f12",
        ] {
            let (h, s, v) = hex_to_hsv(color).unwrap();
            assert_eq!(hsv_to_hex(h, s, v), color, "{color}");
        }
        assert_eq!(hsv_to_hex(0., 1., 1.), "#ff0000");
        assert_eq!(hsv_to_hex(120., 1., 1.), "#00ff00");
        assert_eq!(hsv_to_hex(240., 1., 0.5), "#000080");
        assert!(hex_to_hsv("nope").is_none());
    }
}
