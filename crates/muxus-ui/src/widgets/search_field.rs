//! `InputBase`: a text input with no outline of its own, in the type a
//! screen sets on it, for controls that draw their own frame; and the
//! search box built on it (a 1px outlined row with a search icon, as list
//! filters use it).

use crate::zoom::px;
use gpui_kit::base::input::{Input, InputState};
use gpui_kit::{App, Div, Entity, Hsla, ParentElement, Styled, div};

use crate::icon::icon;
use crate::theme::{ActiveTheme, fonts};
use crate::widgets::text_field::style_input;

/// `InputBase` at `size` px (line height 1.4375em), in the UI or mono font.
pub fn input_base(state: &Entity<InputState>, size: f32, mono: bool, cx: &mut App) -> Div {
    let theme = *cx.theme();
    style_input(state, &theme, cx);
    div()
        .flex_1()
        .min_w_0()
        .font_family(if mono { fonts::MONO } else { fonts::UI })
        .text_size(px(size))
        .line_height(px(size * 1.4375))
        .text_color(theme.palette.text.primary)
        .child(Input::new(state))
}

/// [`input_base`] dimmed and read-only looking (a disabled field's text).
pub fn input_base_disabled(
    state: &Entity<InputState>,
    size: f32,
    disabled: bool,
    cx: &mut App,
) -> Div {
    let theme = *cx.theme();
    let element = input_base(state, size, false, cx);
    if !disabled {
        return element;
    }
    let ink = theme.palette.text.disabled;
    state.update(cx, |state, _| {
        state.set_editor_style(gpui_kit::base::input::InputEditorStyle {
            foreground: ink,
            muted_foreground: ink,
            background: Hsla::transparent_black(),
            border: Hsla::transparent_black(),
            selection: theme.components.text_selection,
            caret: ink,
            ..Default::default()
        })
    });
    element.text_color(ink)
}

/// A search box: `height` px, 1px divider outline with the theme's corners,
/// a 16px search icon and a 12.5px input; `background` fills it when given.
pub fn search_field(
    state: &Entity<InputState>,
    height: f32,
    background: Option<Hsla>,
    cx: &mut App,
) -> Div {
    let theme = *cx.theme();
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(8.))
        .h(px(height))
        .border_1()
        .border_color(theme.palette.divider)
        .rounded(px(theme.radius))
        .child(icon("Search", 16., theme.palette.text.secondary))
        .child(input_base(state, 12.5, false, cx));
    if let Some(background) = background {
        row = row.bg(background);
    }
    row
}
