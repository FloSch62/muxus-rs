//! Typography helpers: apply a MUI variant, or the dense chrome label style
//! the sidebar rows and tabs share.

use crate::zoom::px;
use gpui_kit::{FontWeight, Styled};

use crate::theme::{TextVariant, fonts};

/// Extension for any styled element.
pub trait TextStyleExt: Styled + Sized {
    /// Font family, size, weight and line height of a MUI typography variant.
    fn variant(self, variant: TextVariant) -> Self {
        self.font_family(fonts::UI)
            .text_size(px(variant.size))
            .font_weight(FontWeight(variant.weight))
            .line_height(px(variant.line_height))
    }

    /// The bundled UI font at an explicit size and weight.
    fn ui_font(self, size: f32, weight: f32) -> Self {
        self.font_family(fonts::UI)
            .text_size(px(size))
            .font_weight(FontWeight(weight))
    }

    /// Chrome label type (`treeLabelSx`): 13px at weight 450, one line, ellipsized.
    /// The -0.1px tracking is not applied: GPUI has no letter spacing.
    fn chrome_label(self) -> Self {
        self.ui_font(13., 450.)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
    }
}

impl<T: Styled> TextStyleExt for T {}

/// The width in CSS px of `text` set on one line in `family` at `size` and
/// `weight` (CSS max-content), for layouts that size a column to its text.
pub fn measure_text(
    window: &gpui_kit::Window,
    text: &str,
    family: &str,
    size: f32,
    weight: f32,
) -> f32 {
    let run = gpui_kit::TextRun {
        len: text.len(),
        font: gpui_kit::Font {
            family: family.to_owned().into(),
            features: gpui_kit::FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight(weight),
            style: gpui_kit::FontStyle::Normal,
        },
        color: gpui_kit::Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(text.to_owned().into(), px(size), &[run], None);
    crate::zoom::unzoom(line.width)
}
