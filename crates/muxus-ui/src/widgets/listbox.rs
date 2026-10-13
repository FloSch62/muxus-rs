//! The option list a `Select` or an `Autocomplete` opens under its field:
//! a menu paper at least as wide as the field, scrolling past a maximum
//! height, with [`super::menu::MenuItem`]s as options and
//! [`list_subheader`]s as group labels.

use crate::zoom::px;
use gpui_kit::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, point,
};

use crate::theme::Theme;
use crate::widgets::menu;
use crate::widgets::tooltip::AnchorBounds;
use crate::widgets::typography::{VariantExt, body1, body2, text};

/// `ListSubheader`: a group label in the list, body2 at weight 500 on a
/// 48px line, in the secondary ink.
pub fn list_subheader(theme: &Theme, label: impl Into<SharedString>) -> AnyElement {
    let variant = body2().weight(500.).line_height(48.).tracking(0.);
    div()
        .px(px(16.))
        .h(px(48.))
        .flex()
        .flex_none()
        .items_center()
        .bg(theme.palette.background_paper)
        .child(text(label, variant, theme.palette.text.secondary))
        .into_any_element()
}

/// The list under `anchor` (a field whose bounds were recorded), at least as
/// wide as the field; "No options" when `items` is empty. Pressing outside
/// dismisses it.
pub fn listbox_popup(
    theme: &Theme,
    anchor: &AnchorBounds,
    items: Vec<AnyElement>,
    max_height: f32,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
) -> Option<impl IntoElement> {
    let bounds = anchor.get()?;
    let mut list = div()
        .id("listbox-popup")
        .flex()
        .flex_col()
        .max_h(px(max_height))
        .overflow_y_scroll();
    if items.is_empty() {
        list = list.child(div().px(px(16.)).py(px(14.)).child(text(
            "No options",
            body1(),
            theme.palette.text.secondary,
        )));
    }
    list = list.children(items);
    let paper = menu::paper(theme).min_w(bounds.size.width).child(list);
    let position = point(bounds.origin.x, bounds.origin.y + bounds.size.height);
    // MUI's Autocomplete listbox is a Popper, not a modal menu: a press outside
    // closes it and still lands (the field the user moves to takes the focus).
    Some(menu::popup(position, on_dismiss, paper).non_modal())
}

/// MUI's default `filterOptions`: the options whose label contains what was
/// typed, ignoring case and surrounding space.
pub fn filter_options<'a, T>(
    options: &'a [T],
    input: &str,
    label: impl Fn(&T) -> String,
) -> Vec<&'a T> {
    let needle = input.trim().to_lowercase();
    options
        .iter()
        .filter(|option| needle.is_empty() || label(option).to_lowercase().contains(&needle))
        .collect()
}
