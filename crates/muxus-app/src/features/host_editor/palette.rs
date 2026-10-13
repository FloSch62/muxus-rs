//! Colors the host editor offers or stores as content (not chrome): the
//! color input's choices and the colors the demo data gives its hosts and
//! folders (a new highlight rule's colors are `KeywordHighlightRule`'s). Chrome colors come from the
//! muxus-ui theme; the host color swatches are `hosts::organization::HOST_COLORS`.

/// Colors offered by the color input's popover: the host colors, terminal
/// basics and greys.
pub const COLOR_CHOICES: [&str; 20] = [
    "#ef5350", "#ff9800", "#f6c344", "#4caf72", "#26a69a", "#29b6c8", "#4285f4", "#6e78e8",
    "#ab68d4", "#ec5b91", "#ffffff", "#e6e6ea", "#cccccc", "#9d9da7", "#6e6e78", "#3f3f49",
    "#282a36", "#1e1e1e", "#181818", "#000000",
];

/// The colors of the documentation's demo data.
pub use crate::model::palette::demo;
