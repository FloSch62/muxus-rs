//! Content colors of the code and diff views: what Monaco's `vs` and
//! `vs-dark` editor themes paint (a syntax scheme, not chrome). The editor
//! surface itself is the theme's `code_surface` token.

use muxus_ui::color::{hex, rgba8};
use muxus_ui::gpui_kit::Hsla;

/// The diff editor's colors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiffColors {
    /// Background of an inserted line.
    pub inserted_line: Hsla,
    /// Background of a removed line.
    pub removed_line: Hsla,
    /// The changed characters on an inserted line.
    pub inserted_text: Hsla,
    /// The changed characters on a removed line.
    pub removed_text: Hsla,
    /// Line numbers in the gutter.
    pub line_number: Hsla,
    /// The empty side of a line that exists on one side only.
    pub filler: Hsla,
    /// The bar standing in for unchanged lines that are folded away.
    pub hidden_lines: Hsla,
}

/// Monaco's diff colors for the light or dark theme.
pub fn diff_colors(dark: bool) -> DiffColors {
    DiffColors {
        inserted_line: rgba8(155, 185, 85, 0.2),
        removed_line: rgba8(255, 0, 0, 0.2),
        inserted_text: rgba8(156, 204, 44, if dark { 0.2 } else { 0.25 }),
        removed_text: rgba8(255, 0, 0, 0.2),
        line_number: if dark { hex("#858585") } else { hex("#237893") },
        filler: if dark {
            rgba8(204, 204, 204, 0.06)
        } else {
            rgba8(34, 34, 34, 0.04)
        },
        hidden_lines: if dark {
            rgba8(255, 255, 255, 0.05)
        } else {
            rgba8(0, 0, 0, 0.04)
        },
    }
}
