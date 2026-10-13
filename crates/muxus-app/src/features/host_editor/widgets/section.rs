//! The headings the host editor sections open with (`SectionIntro` and the
//! subtitle-over-caption blocks in `host-editor/*`).

use muxus_ui::gpui_kit::{AnyElement, Div, IntoElement, ParentElement, SharedString, Styled, div};
use muxus_ui::theme::Theme;
use muxus_ui::widgets::typography::{VariantExt, body2, inline_caption, para, subtitle2, text};

/// A section's bold title (`subtitle2` at `weight`).
pub fn heading(theme: &Theme, title: impl Into<SharedString>, weight: f32) -> AnyElement {
    text(
        title,
        subtitle2().weight(weight),
        theme.palette.text.primary,
    )
    .no_truncate()
    .into_any_element()
}

/// The `SectionIntro` shape: a 700-weight subtitle2 over a body2 line in the
/// secondary ink.
pub fn intro(theme: &Theme, title: impl Into<SharedString>, body: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .child(heading(theme, title, 700.))
        .child(para(body, body2(), theme.palette.text.secondary))
}

/// A subtitle2 heading with a caption under it, both in a `Box`.
pub fn caption_intro(
    theme: &Theme,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
) -> Div {
    div()
        .flex()
        .flex_col()
        .child(heading(theme, title, 600.))
        .child(inline_caption(body, theme.palette.text.secondary))
}
