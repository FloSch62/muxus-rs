//! `PathText`: a gNMI path set for reading, separators and key names
//! receding, key values standing out.

use muxus_ui::gpui_kit::{
    AnyElement, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString, Styled,
    StyledText, div,
};
use muxus_ui::theme::Theme;

use super::logic::path_text::{PathSegment, PathTone, text_segments};
use super::widgets::MONO;

/// The path as styled text; unparseable text is shown as it is.
pub fn styled_path(text: &str, skip: usize, show_modules: bool, theme: &Theme) -> StyledText {
    let p = &theme.palette;
    let Some(segments) = text_segments(text, skip, show_modules) else {
        return StyledText::new(SharedString::from(text.to_owned()));
    };
    styled_segments(&segments, theme, p.text.primary)
}

/// Segments as styled text over a base color.
pub fn styled_segments(
    segments: &[PathSegment],
    theme: &Theme,
    _base: muxus_ui::gpui_kit::Hsla,
) -> StyledText {
    let p = &theme.palette;
    let mut text = String::new();
    let mut highlights = Vec::new();
    for segment in segments {
        let start = text.len();
        text.push_str(&segment.text);
        let style = match segment.tone {
            PathTone::Secondary => Some(HighlightStyle {
                color: Some(p.text.secondary),
                ..Default::default()
            }),
            PathTone::Disabled => Some(HighlightStyle {
                color: Some(p.text.disabled),
                ..Default::default()
            }),
            PathTone::KeyValue => Some(HighlightStyle {
                color: Some(p.secondary.main),
                font_weight: Some(FontWeight(550.)),
                ..Default::default()
            }),
            PathTone::Name => None,
        };
        if let Some(style) = style
            && start < text.len()
        {
            highlights.push((start..text.len(), style));
        }
    }
    StyledText::new(SharedString::from(text)).with_highlights(highlights)
}

/// `<PathText>` in the mono font. `nowrap` keeps it on one line, ellipsized.
pub fn path_text(
    text: &str,
    skip: usize,
    show_modules: bool,
    wrap: bool,
    theme: &Theme,
) -> AnyElement {
    let styled = styled_path(text, skip, show_modules, theme);
    let mut element = div()
        .font_family(MONO)
        .text_color(theme.palette.text.primary)
        .min_w_0();
    if !wrap {
        element = element
            .w_full()
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis();
    }
    element.child(styled).into_any_element()
}
