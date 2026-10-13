//! MUI `Typography`: text set in one of the theme's variants.
//!
//! The variants are theme tokens that do not change with the mode, so they
//! are also reachable without a theme at hand ([`body1`], [`caption`], …).
//! MUI only gives its variants letter spacing with its default (Roboto) font
//! family; the theme sets Inter, so only h5/h6 carry the theme's own -0.2px.

use std::sync::LazyLock;

use crate::zoom::px;
use gpui_kit::{Div, Hsla, ParentElement, SharedString, Styled, div};

use crate::label::{Label, Paragraph, label, paragraph};
use crate::theme::{Mode, TextVariant, Theme, Typography, fonts};

static TYPOGRAPHY: LazyLock<Typography> = LazyLock::new(|| Theme::new(Mode::Light).typography);

/// The theme's typography scale.
pub fn typography() -> &'static Typography {
    &TYPOGRAPHY
}

/// MUI `pxToRem(n)` resolved to px under the 13px theme.
pub fn rem(size: f32) -> f32 {
    TYPOGRAPHY.px_to_rem(size)
}

pub fn h6() -> TextVariant {
    TYPOGRAPHY.h6
}

pub fn subtitle2() -> TextVariant {
    TYPOGRAPHY.subtitle2
}

pub fn body1() -> TextVariant {
    TYPOGRAPHY.body1
}

pub fn body2() -> TextVariant {
    TYPOGRAPHY.body2
}

pub fn caption() -> TextVariant {
    TYPOGRAPHY.caption
}

/// The `button` variant at a button's font size (`pxToRem(13)` for small
/// buttons and toggle buttons), line height 1.75.
pub fn button(size: f32) -> TextVariant {
    TextVariant {
        size,
        line_height: size * 1.75,
        ..TYPOGRAPHY.button
    }
}

/// The font weights the client sets on top of the variants (`fontWeight`).
pub mod weight {
    pub const REGULAR: f32 = 400.;
    pub const MEDIUM: f32 = 500.;
    pub const SEMIBOLD: f32 = 600.;
    pub const BOLD: f32 = 700.;
}

/// Monospace sizes for paths, keys and code in forms (`fontSize: 12` / `13`).
pub mod code_size {
    pub const SMALL: f32 = 12.;
    pub const BODY: f32 = 13.;
}

/// `sx` tweaks of a variant (`fontWeight`, `fontSize`, `lineHeight`,
/// `letterSpacing`), keeping the rest.
pub trait VariantExt: Sized {
    fn weight(self, weight: f32) -> Self;
    fn size(self, size: f32) -> Self;
    fn line_height(self, line_height: f32) -> Self;
    fn tracking(self, tracking: f32) -> Self;
}

impl VariantExt for TextVariant {
    fn weight(self, weight: f32) -> Self {
        Self { weight, ..self }
    }

    fn size(self, size: f32) -> Self {
        Self { size, ..self }
    }

    fn line_height(self, line_height: f32) -> Self {
        Self {
            line_height,
            ..self
        }
    }

    fn tracking(self, letter_spacing: f32) -> Self {
        Self {
            letter_spacing,
            ..self
        }
    }
}

/// One line of text in a variant, ellipsized when it does not fit.
pub fn text(content: impl Into<SharedString>, variant: TextVariant, color: Hsla) -> Label {
    label(content).variant(variant).color(color)
}

/// Wrapping text in a variant.
pub fn para(content: impl Into<SharedString>, variant: TextVariant, color: Hsla) -> Paragraph {
    paragraph(content).variant(variant).color(color)
}

/// One line in the monospace font (`fontFamily: "JetBrains Mono"`).
pub fn mono(content: impl Into<SharedString>, size: f32, line_height: f32, color: Hsla) -> Label {
    label(content)
        .family(fonts::MONO)
        .size(size)
        .line_height(line_height)
        .color(color)
}

/// A caption span that follows block text inside a body1 element (a body2
/// title, then a caption, as labelled controls show them). Chrome lays the
/// span out in a line box with the body1 strut, so the line is 1.5 × 14.86px
/// tall and the caption's baseline sits where the strut's would.
pub fn inline_caption(content: impl Into<SharedString>, color: Hsla) -> Div {
    let strut = body1();
    let cap = caption();
    let line = strut.line_height;
    // Inter's ascent and descent per em.
    let (ascent, descent) = (0.968_75, 0.241_210_94);
    let baseline = |size: f32| (line - (ascent + descent) * size) / 2. + ascent * size;
    div().min_h(px(line)).child(
        div()
            .relative()
            .top(px(baseline(strut.size) - baseline(cap.size)))
            .child(para(content, cap.line_height(line), color)),
    )
}

/// An uppercase caption heading (`textTransform: uppercase` with 0.4px
/// tracking at `size` and `weight`): field labels, stat labels, list section heads.
pub fn caps(content: impl Into<SharedString>, size: f32, weight: f32, color: Hsla) -> Label {
    let content: SharedString = content.into();
    label(SharedString::from(content.to_uppercase()))
        .size(size)
        .weight(weight)
        .line_height(size * 1.66)
        .tracking(0.4)
        .color(color)
        .no_truncate()
}

/// [`inline_caption`] for any size and family: a span of `size` px text
/// after block text, on the body1 strut's line box and baseline (one line).
pub fn inline_span(
    content: impl Into<SharedString>,
    size: f32,
    family: &'static str,
    color: Hsla,
) -> Div {
    let strut = body1();
    let line = strut.line_height;
    // Inter's ascent and descent per em.
    let (ascent, descent) = (0.968_75, 0.241_210_94);
    let baseline = |size: f32| (line - (ascent + descent) * size) / 2. + ascent * size;
    div().min_h(px(line)).child(
        div()
            .relative()
            .top(px(baseline(strut.size) - baseline(size)))
            .child(
                label(content)
                    .family(family)
                    .size(size)
                    .line_height(line)
                    .color(color),
            ),
    )
}

/// How far below the top of its line a line of `variant` text has its
/// baseline (Inter's ascent and descent, the leading split evenly). Two
/// lines on `alignItems: 'baseline'` line up when the shorter one moves down
/// by the difference.
pub fn baseline(variant: TextVariant) -> f32 {
    let (ascent, descent) = (0.968_75, 0.241_210_94);
    (variant.line_height - (ascent + descent) * variant.size) / 2. + ascent * variant.size
}
