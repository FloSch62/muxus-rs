//! `components/TerminalSchemeSelect.tsx`: the scheme picker shared by the
//! terminal color settings and the custom scheme editor, and the compact
//! terminal preview (`SchemeLabel`) both the closed picker and its menu show.

use crate::features::settings::palette::MISSING_SCHEME_COLOR;
use std::rc::Rc;

use muxus_store::prefs::PrefsState;
use muxus_term::custom_schemes::{
    CustomTerminalScheme as TermCustomScheme, terminal_schemes_from_custom,
};
use muxus_term::palette::{TerminalScheme, terminal_schemes};
use muxus_ui::color::{hex, rgba8, try_hex};
use muxus_ui::gpui_kit::{AnyElement, Div, IntoElement, ParentElement, Styled, div, px};
use muxus_ui::label::label;
use muxus_ui::theme::Theme;

use super::kit::{BODY2, CAPTION, snap};
use super::window::MenuEntry;

/// The custom schemes in the terminal crate's shape (the stored JSON is the same).
pub fn term_custom_schemes(prefs: &PrefsState) -> Vec<TermCustomScheme> {
    serde_json::to_value(&prefs.custom_terminal_schemes)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// Custom schemes back into the preferences' shape.
pub fn store_custom_schemes(
    schemes: &[TermCustomScheme],
) -> Vec<muxus_store::prefs::custom_schemes::CustomTerminalScheme> {
    serde_json::to_value(schemes)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// The user's schemes as pickers list them.
pub fn custom_terminal_schemes(prefs: &PrefsState) -> Vec<TerminalScheme> {
    terminal_schemes_from_custom(&term_custom_schemes(prefs))
}

/// Every scheme on offer, the user's own first (`terminalSchemeGroups`).
pub fn terminal_scheme_groups(
    custom: &[TerminalScheme],
) -> Vec<(&'static str, Vec<TerminalScheme>)> {
    let builtin = terminal_schemes();
    let mut groups = Vec::new();
    if !custom.is_empty() {
        groups.push(("Custom schemes", custom.to_vec()));
    }
    groups.push((
        "Light schemes",
        builtin
            .iter()
            .filter(|scheme| scheme.light)
            .cloned()
            .collect(),
    ));
    groups.push((
        "Dark schemes",
        builtin
            .iter()
            .filter(|scheme| !scheme.light)
            .cloned()
            .collect(),
    ));
    groups
}

const SWATCH_KEYS: [&str; 6] = ["red", "green", "yellow", "blue", "magenta", "cyan"];

fn css(color: &str) -> muxus_ui::gpui_kit::Hsla {
    try_hex(color).unwrap_or_else(|| hex(MISSING_SCHEME_COLOR))
}

/// `SchemeLabel`: a 60×28 swatch of six ANSI colors on the scheme's
/// background, the name, and optionally Light/Dark at the end.
pub fn scheme_label(theme: &Theme, scheme: &TerminalScheme, show_mode: bool) -> Div {
    let p = &theme.palette;
    let colors = &scheme.theme;
    // 60×28 inside its 1px border.
    let mut swatch = div()
        .w(px(62.))
        .h(px(30.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(3.))
        .bg(css(&colors.background))
        .border_1()
        .border_color(if scheme.light {
            rgba8(0, 0, 0, 0.14)
        } else {
            rgba8(255, 255, 255, 0.14)
        })
        .rounded(px(6.));
    for key in SWATCH_KEYS {
        swatch = swatch.child(
            div()
                .w(px(5.))
                .h(px(12.))
                .rounded(px(2.))
                .bg(css(colors.color(key).unwrap_or(MISSING_SCHEME_COLOR))),
        );
    }
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .w_full()
        .min_w_0()
        .child(swatch)
        .child(
            label(scheme.name.clone())
                .size(BODY2)
                .weight(500.)
                .line_height(snap(BODY2 * 1.43))
                .color(p.text.primary),
        );
    if show_mode {
        row = row.child(
            div().ml_auto().pr(px(4.)).flex_none().child(
                label(if scheme.light { "Light" } else { "Dark" })
                    .size(CAPTION)
                    .line_height(snap(CAPTION * 1.66))
                    .color(p.text.secondary)
                    .no_truncate(),
            ),
        );
    }
    row
}

/// The scheme menu: a subheader per group, then each scheme with its preview.
pub fn scheme_menu_entries(custom: &[TerminalScheme], header_prefix: &str) -> Vec<MenuEntry> {
    let mut entries = Vec::new();
    for (title, schemes) in terminal_scheme_groups(custom) {
        entries.push(MenuEntry::Header(format!("{header_prefix}{title}").into()));
        for scheme in schemes {
            let rich_scheme = scheme.clone();
            entries.push(MenuEntry::Item {
                value: scheme.id.clone(),
                label: scheme.name.clone().into(),
                secondary: None,
                rich: Some(Rc::new(move |theme: &Theme| {
                    scheme_label(theme, &rich_scheme, false).into_any_element()
                })),
            });
        }
    }
    entries
}

/// The closed picker's value (`renderValue`).
pub fn scheme_value(theme: &Theme, scheme: &TerminalScheme) -> AnyElement {
    scheme_label(theme, scheme, true).into_any_element()
}
