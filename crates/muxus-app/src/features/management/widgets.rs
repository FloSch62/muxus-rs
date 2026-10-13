//! Workbench-specific compositions of the `muxus-ui` kit: the forms'
//! uppercase field labels, labelled switches, and text buttons in an ink
//! of their own.

use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{App, ClickEvent, ElementId, Hsla, IntoElement, SharedString, Window};
use muxus_ui::theme::Theme;
use muxus_ui::widgets::button::ButtonTint;
use muxus_ui::widgets::form_control::{FormControl, FormControlLabel};
use muxus_ui::widgets::typography::{body2, text};

pub use muxus_ui::widgets::typography::{caps, inline_span};

pub use muxus_ui::theme::fonts::MONO;

/// `<FieldLabel>`: 10.5px uppercase caption in text.secondary at 600.
pub fn field_label(content: impl Into<SharedString>, theme: &Theme) -> impl IntoElement {
    caps(content, 10.5, 600., theme.palette.text.secondary)
}

/// `FormControlLabel` with a small `Switch` and a body2 label.
pub fn switch_row(
    id: impl Into<ElementId>,
    checked: bool,
    content: impl Into<SharedString>,
    theme: &Theme,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    FormControlLabel::new(id, FormControl::Switch(checked), "")
        .small()
        .label_element(text(content, body2(), theme.palette.text.primary).no_truncate())
        .on_click(on_toggle)
}

/// [`switch_row`] in a MUI `Stack` without `useFlexGap`, which resets its
/// children's margins.
pub fn switch_row_flush(
    id: impl Into<ElementId>,
    checked: bool,
    content: impl Into<SharedString>,
    theme: &Theme,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    FormControlLabel::new(id, FormControl::Switch(checked), "")
        .small()
        .flush()
        .label_element(text(content, body2(), theme.palette.text.primary).no_truncate())
        .on_click(on_toggle)
}

/// A text button in an ink of its own (`sx={{ color }}`), hover tinted with it.
pub fn text_tint(ink: Hsla, theme: &Theme) -> ButtonTint {
    ButtonTint {
        ink,
        border: None,
        background: None,
        hover_border: None,
        hover_background: Some(alpha(ink, theme.palette.action.hover_opacity)),
    }
}
