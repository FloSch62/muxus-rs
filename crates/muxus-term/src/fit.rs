//! Cell metrics and fitting a terminal into its pane (ports `client/src/terminal/terminal-fit.ts`
//! and the arithmetic of xterm's DOM renderer and `FitAddon.proposeDimensions`).

/// xterm never shrinks a terminal below two columns and one row.
pub const MINIMUM_COLS: u16 = 2;
pub const MINIMUM_ROWS: u16 = 1;

/// A tab that is not in front has no laid-out box to measure. Measuring zero would fall back to
/// the two-column floor, resizing the terminal — and SIGWINCHing the remote shell — to two
/// columns; the shell hard-wraps its prompt at that width with real newlines, so the debris
/// survives the refit that happens when the tab comes back. Only measure a laid-out container.
pub fn should_fit_terminal(container: Option<(f32, f32)>) -> bool {
    matches!(container, Some((width, height)) if width > 0.0 && height > 0.0)
}

/// The font measurements a cell grid derives from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontMetrics {
    /// Advance width of one character, in logical pixels (14 px JetBrains Mono: 8.4).
    pub char_width: f32,
    /// Ascent plus descent of the font, in logical pixels (xterm's `fontBoundingBoxAscent +
    /// fontBoundingBoxDescent`).
    pub char_height: f32,
}

/// The font height xterm measures (`fontBoundingBoxAscent + fontBoundingBoxDescent` of a canvas
/// `measureText`) as Chromium reports it on Linux: the ascent rounded, and the descent rounded up
/// when rounding would cut glyphs off at the bottom. JetBrains Mono at 14 px: 14 + 5 = 19.
pub fn chromium_font_height(ascent: f32, descent: f32) -> f32 {
    let ascent = ascent.abs().round();
    let exact = descent.abs();
    let mut rounded = exact.round();
    if rounded < exact {
        rounded += 1.0;
    }
    ascent + rounded
}

/// The size of one cell, as xterm's DOM renderer computes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    /// Logical pixels.
    pub width: f32,
    /// Logical pixels.
    pub height: f32,
}

/// Cell metrics for a font at a device pixel ratio, xterm's `DomRenderer._updateDimensions`:
/// the character height is rounded up to whole device pixels and the line height multiplies it,
/// rounded down; the width keeps the fractional advance (plus integer letter spacing).
pub fn cell_metrics(
    font: FontMetrics,
    line_height: f32,
    letter_spacing: f32,
    scale_factor: f32,
) -> CellMetrics {
    let dpr = if scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let device_char_width = font.char_width * dpr;
    let device_char_height = (font.char_height * dpr).ceil();
    let device_cell_width = device_char_width + letter_spacing.round();
    let device_cell_height = (device_char_height * line_height).floor();
    CellMetrics {
        width: device_cell_width / dpr,
        height: device_cell_height / dpr,
    }
}

/// Columns and rows that fit `available` logical pixels (padding and scrollbar already removed),
/// as `FitAddon.proposeDimensions`: whole pixels of the container, floored cell counts, and
/// xterm's minimums.
pub fn fit_grid(
    available_width: f32,
    available_height: f32,
    cell: CellMetrics,
) -> Option<(u16, u16)> {
    if cell.width <= 0.0 || cell.height <= 0.0 {
        return None;
    }
    let width = available_width.max(0.0).trunc();
    let height = available_height.max(0.0).trunc();
    let cols =
        ((width / cell.width).floor() as u32).clamp(u32::from(MINIMUM_COLS), u32::from(u16::MAX));
    let rows =
        ((height / cell.height).floor() as u32).clamp(u32::from(MINIMUM_ROWS), u32::from(u16::MAX));
    Some((cols as u16, rows as u16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_a_container_the_layout_has_measured() {
        assert!(should_fit_terminal(Some((800.0, 400.0))));
    }

    #[test]
    fn skips_a_pane_hidden_behind_another_tab() {
        // A hidden pane has no box to measure, and fitting would fall back to the two-column
        // floor and SIGWINCH the remote shell to that width.
        assert!(!should_fit_terminal(Some((0.0, 0.0))));
        assert!(!should_fit_terminal(Some((800.0, 0.0))));
        assert!(!should_fit_terminal(Some((0.0, 400.0))));
    }

    #[test]
    fn skips_an_unmounted_terminal() {
        assert!(!should_fit_terminal(None));
    }

    #[test]
    fn matches_xterm_cell_metrics_for_jetbrains_mono() {
        // JetBrains Mono: 600/1000 advance, 1020 ascent + 300 descent per 1000 units.
        let font = FontMetrics {
            char_width: 8.4,
            char_height: 18.48,
        };
        let cell = cell_metrics(font, 1.0, 0.0, 2.0);
        assert!((cell.width - 8.4).abs() < 1e-4);
        assert!((cell.height - 18.5).abs() < 1e-4);
        let cell = cell_metrics(font, 1.1, 0.0, 1.0);
        assert!((cell.height - 20.0).abs() < 1e-4);
    }

    #[test]
    fn measures_the_font_height_as_chromium_does() {
        // JetBrains Mono: ascent 1020, descent 300 per 1000 units.
        assert_eq!(chromium_font_height(14.28, -4.2), 19.0);
        let cell = cell_metrics(
            FontMetrics {
                char_width: 8.4,
                char_height: 19.0,
            },
            1.1,
            0.0,
            2.0,
        );
        // The docs screenshots (line height 1.1 at 2x) have 41-device-pixel rows.
        assert!((cell.height - 20.5).abs() < 1e-4);
    }

    #[test]
    fn fits_whole_cells_with_xterm_minimums() {
        let cell = CellMetrics {
            width: 8.4,
            height: 18.5,
        };
        assert_eq!(fit_grid(800.9, 400.0, cell), Some((95, 21)));
        assert_eq!(fit_grid(3.0, 3.0, cell), Some((2, 1)));
        assert_eq!(
            fit_grid(
                100.0,
                100.0,
                CellMetrics {
                    width: 0.0,
                    height: 1.0
                }
            ),
            None
        );
    }
}
