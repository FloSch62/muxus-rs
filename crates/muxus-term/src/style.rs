//! Cell attributes as the renderer and the serializer see them, and the color resolution xterm's
//! DOM renderer applies to every cell (`DomRendererRowFactory.createRow`, xterm.js, MIT): inverse,
//! bold-as-bright, dim, decorations, the selection, and the minimum contrast ratio.

use crate::color::{Rgba, ensure_contrast_ratio};
use crate::palette::ResolvedPalette;

/// A cell color before the palette is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CellColor {
    /// The theme's default foreground or background.
    #[default]
    Default,
    /// A palette entry: 0–15 ANSI, 16–255 the xterm cube and gray ramp.
    Palette(u8),
    Rgb(u8, u8, u8),
}

/// SGR 4:x underline styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl UnderlineStyle {
    /// The SGR 4:x number.
    pub fn sgr(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Single => 1,
            Self::Double => 2,
            Self::Curly => 3,
            Self::Dotted => 4,
            Self::Dashed => 5,
        }
    }
}

/// Everything about a cell's look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CellStyle {
    pub fg: CellColor,
    pub bg: CellColor,
    /// `None` follows the foreground.
    pub underline_color: Option<CellColor>,
    pub underline: UnderlineStyle,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
}

impl CellStyle {
    /// No colors and no attributes (xterm's `isAttributeDefault`).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn flags_equal(&self, other: &Self) -> bool {
        self.inverse == other.inverse
            && self.bold == other.bold
            && self.underline == other.underline
            && (self.underline == UnderlineStyle::None
                || self.underline_color == other.underline_color)
            && self.overline == other.overline
            && self.blink == other.blink
            && self.invisible == other.invisible
            && self.italic == other.italic
            && self.dim == other.dim
            && self.strikethrough == other.strikethrough
    }
}

/// One cell for painting and serializing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StyledCell {
    /// The grapheme; empty for a blank cell.
    pub text: String,
    /// 1, 2 for a wide character, 0 for the cell a wide character covers.
    pub width: u8,
    pub style: CellStyle,
    /// OSC 8 hyperlink id, if the cell is part of one.
    pub hyperlink: Option<u32>,
}

/// Color overrides programs set with OSC 10/11/12 (xterm applies them on top of the theme).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorOverrides {
    pub foreground: Option<Rgba>,
    pub background: Option<Rgba>,
    pub cursor: Option<Rgba>,
}

/// The theme colors in effect: the resolved palette with program overrides applied.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveColors {
    pub palette: ResolvedPalette,
    pub foreground: Rgba,
    pub background: Rgba,
    pub cursor: Rgba,
    pub cursor_accent: Rgba,
}

impl EffectiveColors {
    pub fn new(palette: &ResolvedPalette, overrides: &ColorOverrides) -> Self {
        Self {
            palette: palette.clone(),
            foreground: overrides.foreground.unwrap_or(palette.foreground),
            background: overrides.background.unwrap_or(palette.background),
            cursor: overrides.cursor.unwrap_or(palette.cursor),
            cursor_accent: palette.cursor_accent,
        }
    }

    fn color(&self, color: CellColor, foreground: bool) -> Rgba {
        match color {
            CellColor::Default => {
                if foreground {
                    self.foreground
                } else {
                    self.background
                }
            }
            CellColor::Palette(index) => self.palette.ansi[index as usize],
            CellColor::Rgb(r, g, b) => Rgba::rgb(r, g, b),
        }
    }
}

/// Paint-time context of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CellContext {
    pub selected: bool,
    /// The terminal has focus (selections are painted with the inactive color otherwise).
    pub focused: bool,
    /// A decoration's colors (keyword highlight, search match).
    pub decoration_fg: Option<Rgba>,
    pub decoration_bg: Option<Rgba>,
    /// The decoration is on the top layer (the active search match): drawn over the selection.
    pub decoration_top: bool,
    /// Minimum contrast ratio, 1 to disable.
    pub minimum_contrast_ratio: f64,
}

/// The colors a cell is painted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCell {
    /// Text color; dim text has half opacity.
    pub fg: Rgba,
    /// Background to fill; `None` leaves the terminal background showing.
    pub bg: Option<Rgba>,
    /// Underline color.
    pub underline: Rgba,
}

/// Glyphs xterm excludes from the contrast adjustment: Powerline separators and box/block
/// drawing, which must keep the exact colors to join neighbouring cells.
pub fn treat_glyph_as_background_color(text: &str) -> bool {
    let Some(c) = text.chars().next() else {
        return false;
    };
    let code = c as u32;
    (0xE0A4..=0xE0D6).contains(&code) || (0x2500..=0x259F).contains(&code)
}

/// Resolve a cell's colors exactly as xterm's DOM renderer does.
pub fn resolve_cell(
    cell: &StyledCell,
    colors: &EffectiveColors,
    context: &CellContext,
) -> ResolvedCell {
    let style = &cell.style;
    let (mut fg_mode, mut bg_mode) = (style.fg, style.bg);
    let mut fg_is_inverted_default = false;
    if style.inverse {
        std::mem::swap(&mut fg_mode, &mut bg_mode);
        fg_is_inverted_default = fg_mode == CellColor::Default;
    }

    // Decoration overrides apply after inverse.
    let mut bg_override: Option<Rgba> = None;
    let mut fg_override: Option<Rgba> = None;
    let mut is_top = false;
    if context.decoration_bg.is_some() || context.decoration_fg.is_some() {
        if let Some(bg) = context.decoration_bg {
            bg_override = Some(bg);
        }
        if let Some(fg) = context.decoration_fg {
            fg_override = Some(fg);
        }
        is_top = context.decoration_top;
    }

    // The selection pretends to be a top decoration so text is drawn above it.
    if !is_top && context.selected {
        bg_override = Some(if context.focused {
            colors.palette.selection_background_opaque
        } else {
            colors.palette.selection_inactive_background_opaque
        });
    }

    let resolved_bg = match bg_override {
        Some(bg) => bg,
        None => match bg_mode {
            CellColor::Default if style.inverse => colors.foreground,
            other => colors.color(other, false),
        },
    };
    let painted_bg = match (bg_override, bg_mode) {
        (Some(bg), _) => Some(bg),
        (None, CellColor::Default) if !style.inverse => None,
        (None, _) => Some(resolved_bg),
    };

    let base_fg = match fg_override {
        Some(fg) => fg,
        None => match fg_mode {
            CellColor::Palette(index) if style.bold && index < 8 => {
                colors.palette.ansi[index as usize + 8]
            }
            CellColor::Default if fg_is_inverted_default => colors.background,
            other => colors.color(other, true),
        },
    };

    let mut fg = base_fg;
    let mut adjusted = false;
    if context.minimum_contrast_ratio > 1.0 && !treat_glyph_as_background_color(&cell.text) {
        // Dim cells only need half the contrast, or they would not read as dim.
        let ratio = context.minimum_contrast_ratio / if style.dim { 2.0 } else { 1.0 };
        if let Some(better) = ensure_contrast_ratio(resolved_bg, base_fg, ratio) {
            fg = better;
            adjusted = true;
        }
    }
    if style.dim && !adjusted {
        fg = fg.multiply_opacity(0.5);
    }

    let underline = match style.underline_color {
        Some(CellColor::Palette(index)) if style.bold && index < 8 => {
            colors.palette.ansi[index as usize + 8]
        }
        Some(color) => colors.color(color, true),
        None => fg,
    };
    ResolvedCell {
        fg,
        bg: painted_bg,
        underline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::{ResolvedPalette, terminal_scheme};

    fn colors(scheme: &str) -> EffectiveColors {
        EffectiveColors::new(
            &ResolvedPalette::new(&terminal_scheme(Some(scheme), &[]).theme),
            &ColorOverrides::default(),
        )
    }

    fn cell(style: CellStyle) -> StyledCell {
        StyledCell {
            text: "x".into(),
            width: 1,
            style,
            hyperlink: None,
        }
    }

    fn plain() -> CellContext {
        CellContext {
            focused: true,
            minimum_contrast_ratio: 1.0,
            ..Default::default()
        }
    }

    #[test]
    fn paints_default_cells_with_the_theme() {
        let colors = colors("vscode-dark");
        let resolved = resolve_cell(&cell(CellStyle::default()), &colors, &plain());
        assert_eq!(resolved.fg, Rgba::rgb(0xcc, 0xcc, 0xcc));
        assert_eq!(resolved.bg, None);
    }

    #[test]
    fn brightens_bold_ansi_colors_and_swaps_inverse() {
        let colors = colors("vscode-dark");
        let bold_red = CellStyle {
            fg: CellColor::Palette(1),
            bold: true,
            ..Default::default()
        };
        assert_eq!(
            resolve_cell(&cell(bold_red), &colors, &plain()).fg,
            Rgba::rgb(0xf1, 0x4c, 0x4c)
        );
        let inverse = CellStyle {
            inverse: true,
            ..Default::default()
        };
        let resolved = resolve_cell(&cell(inverse), &colors, &plain());
        assert_eq!(resolved.fg, Rgba::rgb(0x18, 0x18, 0x18));
        assert_eq!(resolved.bg, Some(Rgba::rgb(0xcc, 0xcc, 0xcc)));
    }

    #[test]
    fn dims_and_keeps_contrast() {
        let colors = colors("vscode-dark");
        let dim = CellStyle {
            dim: true,
            ..Default::default()
        };
        assert_eq!(resolve_cell(&cell(dim), &colors, &plain()).fg.a, 128);
        // Blue on black lacks contrast: it is lightened until it reaches 4.5:1.
        let blue_on_black = CellStyle {
            fg: CellColor::Palette(4),
            bg: CellColor::Palette(0),
            ..Default::default()
        };
        let context = CellContext {
            minimum_contrast_ratio: 4.5,
            ..plain()
        };
        let resolved = resolve_cell(&cell(blue_on_black), &colors, &context);
        assert_ne!(resolved.fg, Rgba::rgb(0x24, 0x72, 0xc8));
        // Box drawing keeps its exact color.
        let mut line = cell(blue_on_black);
        line.text = "─".into();
        assert_eq!(
            resolve_cell(&line, &colors, &context).fg,
            Rgba::rgb(0x24, 0x72, 0xc8)
        );
    }

    #[test]
    fn selection_and_decorations_override_the_background() {
        let colors = colors("vscode-dark");
        let selected = CellContext {
            selected: true,
            ..plain()
        };
        assert_eq!(
            resolve_cell(&cell(CellStyle::default()), &colors, &selected).bg,
            Some(Rgba::rgb(0x26, 0x4f, 0x78))
        );
        let decorated = CellContext {
            decoration_fg: Some(Rgba::rgb(255, 255, 255)),
            decoration_bg: Some(Rgba::rgb(0xb9, 0x1c, 0x1c)),
            ..plain()
        };
        let resolved = resolve_cell(&cell(CellStyle::default()), &colors, &decorated);
        assert_eq!(resolved.fg, Rgba::rgb(255, 255, 255));
        assert_eq!(resolved.bg, Some(Rgba::rgb(0xb9, 0x1c, 0x1c)));
    }
}
