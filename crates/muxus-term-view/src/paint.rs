//! Painting a terminal frame: cell backgrounds, text on the cell grid, decorations, the cursor,
//! images, the scrollbar and the overview ruler. A [`Frame`] is a snapshot the view builds on each
//! render; painting only reads it.

use std::sync::Arc;

use gpui_kit::{
    App, BorderStyle, Bounds, Font, FontFallbacks, FontStyle, FontWeight, Hsla, Pixels,
    RenderImage, SharedString, StrikethroughStyle, TextAlign, TextRun,
    UnderlineStyle as GpuiUnderline, Window, fill, outline, point, size,
};
use muxus_term::color::Rgba;
use muxus_term::style::UnderlineStyle;
use muxus_term::terminal::CursorShape;
// Painting works in layout space (scaled with the interface): `px` here takes laid-out
// lengths as they are; the CSS constants below are scaled once.
use muxus_ui::zoom::{css, layout_px as px};

pub(crate) fn hsla(color: Rgba) -> Hsla {
    gpui_kit::Rgba {
        r: f32::from(color.r) / 255.0,
        g: f32::from(color.g) / 255.0,
        b: f32::from(color.b) / 255.0,
        a: f32::from(color.a) / 255.0,
    }
    .into()
}

/// The fonts a terminal draws with.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FontSet {
    pub family: SharedString,
    pub fallbacks: Vec<String>,
    pub size: Pixels,
}

impl FontSet {
    pub fn font(&self, bold: bool, italic: bool) -> Font {
        Font {
            family: self.family.clone(),
            features: Default::default(),
            fallbacks: (!self.fallbacks.is_empty())
                .then(|| FontFallbacks::from_fonts(self.fallbacks.clone())),
            weight: if bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
        }
    }
}

/// One cell ready to paint.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PaintCell {
    /// What to draw; a space for blank and hidden cells.
    pub text: String,
    pub width: u8,
    pub fg: Hsla,
    pub bg: Option<Hsla>,
    pub underline: UnderlineStyle,
    pub underline_color: Hsla,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub overline: bool,
    /// Under the pointer as part of a link: underlined.
    pub link: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct FrameCursor {
    pub row: usize,
    pub col: usize,
    pub width: u8,
    pub shape: CursorShape,
    pub focused: bool,
    /// The blink phase hides it.
    pub hidden: bool,
    pub color: Hsla,
    pub accent: Hsla,
    /// The character under a block cursor, repainted in the accent color.
    pub text: String,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone)]
pub(crate) struct FrameImage {
    /// Row of the top edge relative to the viewport (negative when it starts above).
    pub row: isize,
    pub col: usize,
    pub image: Arc<RenderImage>,
    /// Size in logical pixels.
    pub width: f32,
    pub height: f32,
    pub below_text: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct RulerMark {
    /// Fraction of the buffer where the mark starts and ends.
    pub start: f32,
    pub end: f32,
    pub lane: RulerLane,
    pub color: Hsla,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RulerLane {
    Left,
    Center,
}

#[derive(Clone, Debug)]
pub(crate) struct Scrollbar {
    /// Slider top and height as fractions of the track.
    pub top: f32,
    pub height: f32,
    pub color: Hsla,
}

/// Cell geometry in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CellSize {
    pub width: f32,
    pub height: f32,
    /// Moves glyphs onto the baseline Chromium uses (it centres the rounded font height), which
    /// sits a fraction of a pixel above GPUI's.
    pub baseline_shift: f32,
}

#[derive(Clone)]
pub(crate) struct Frame {
    /// Shared with the view's cache: unchanged rows are not rebuilt.
    pub rows: std::rc::Rc<Vec<Vec<PaintCell>>>,
    pub cell: CellSize,
    pub fonts: FontSet,
    pub background: Hsla,
    pub cursor: Option<FrameCursor>,
    pub images: Vec<FrameImage>,
    pub scrollbar: Option<Scrollbar>,
    pub ruler: Vec<RulerMark>,
    pub buffer_lines: usize,
    /// IME composition shown at the cursor.
    pub preedit: Option<String>,
}

/// Width reserved at the right for the scrollbar and the overview ruler (`scrollbar.width`).
pub(crate) const SCROLLBAR_WIDTH_CSS: f32 = 14.0;
/// Padding of the `.xterm` element inside the bordered box.
pub(crate) const INNER_PADDING_CSS: f32 = 4.0;

/// The scrollbar's width in layout space.
pub(crate) fn scrollbar_width() -> f32 {
    css(SCROLLBAR_WIDTH_CSS)
}

/// The padding inside the terminal's box, in layout space.
pub(crate) fn inner_padding() -> f32 {
    css(INNER_PADDING_CSS)
}

fn cell_x(origin: f32, col: usize, cell: CellSize) -> f32 {
    origin + col as f32 * cell.width
}

/// Paint the frame into `bounds` (the `.xterm` padding box).
pub(crate) fn paint_frame(
    frame: &Frame,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let origin_x = f32::from(bounds.origin.x) + inner_padding();
    let origin_y = f32::from(bounds.origin.y) + inner_padding();
    let cell = frame.cell;
    let line_height = px(cell.height);

    for image in frame.images.iter().filter(|image| image.below_text) {
        paint_image(image, origin_x, origin_y, cell, bounds, window);
    }

    for (row_index, row) in frame.rows.iter().enumerate() {
        let y = origin_y + row_index as f32 * cell.height;
        paint_backgrounds(row, origin_x, y, cell, window);
        paint_text(
            row,
            origin_x,
            y,
            cell,
            &frame.fonts,
            line_height,
            window,
            cx,
        );
        paint_decorations(row, origin_x, y, cell, window);
    }

    for image in frame.images.iter().filter(|image| !image.below_text) {
        paint_image(image, origin_x, origin_y, cell, bounds, window);
    }

    if let Some(cursor) = &frame.cursor {
        paint_cursor(
            cursor,
            origin_x,
            origin_y,
            cell,
            &frame.fonts,
            line_height,
            window,
            cx,
        );
    }
    if let (Some(preedit), Some(cursor)) = (&frame.preedit, &frame.cursor) {
        paint_preedit(
            preedit,
            cursor,
            frame,
            origin_x,
            origin_y,
            line_height,
            window,
            cx,
        );
    }

    let grid_height = frame.rows.len() as f32 * cell.height;
    paint_ruler(frame, bounds, grid_height, window);
    if let Some(scrollbar) = &frame.scrollbar {
        let track_x =
            f32::from(bounds.origin.x + bounds.size.width) - inner_padding() - scrollbar_width();
        let track_height = f32::from(bounds.size.height) - 2.0 * inner_padding();
        let top = origin_y + scrollbar.top * track_height;
        let height = (scrollbar.height * track_height)
            .max(20.0)
            .min(track_height);
        window.paint_quad(fill(
            Bounds::new(
                point(px(track_x), px(top)),
                size(px(scrollbar_width()), px(height)),
            ),
            scrollbar.color,
        ));
    }
}

fn paint_backgrounds(
    row: &[PaintCell],
    origin_x: f32,
    y: f32,
    cell: CellSize,
    window: &mut Window,
) {
    let mut col = 0;
    while col < row.len() {
        let Some(bg) = row[col].bg else {
            col += 1;
            continue;
        };
        let start = col;
        while col < row.len() && row[col].bg == Some(bg) {
            col += 1;
        }
        // Snap the edges to whole pixels so neighbouring runs leave no seam.
        let x0 = cell_x(origin_x, start, cell).round();
        let x1 = cell_x(origin_x, col, cell).round();
        let y0 = y.round();
        let y1 = (y + cell.height).round();
        window.paint_quad(fill(
            Bounds::new(point(px(x0), px(y0)), size(px(x1 - x0), px(y1 - y0))),
            bg,
        ));
    }
}

fn text_run(cell: &PaintCell, fonts: &FontSet, len: usize) -> TextRun {
    let underline = match cell.underline {
        _ if cell.link => Some(GpuiUnderline {
            thickness: px(1.0),
            color: Some(cell.fg),
            wavy: false,
        }),
        UnderlineStyle::Single => Some(GpuiUnderline {
            thickness: px(1.0),
            color: Some(cell.underline_color),
            wavy: false,
        }),
        UnderlineStyle::Curly => Some(GpuiUnderline {
            thickness: px(1.0),
            color: Some(cell.underline_color),
            wavy: true,
        }),
        _ => None,
    };
    TextRun {
        len,
        font: fonts.font(cell.bold, cell.italic),
        color: cell.fg,
        background_color: None,
        underline,
        strikethrough: cell.strikethrough.then(|| StrikethroughStyle {
            thickness: px(1.0),
            color: Some(cell.fg),
        }),
    }
}

/// Everything [`text_run`] reads from a cell: two cells with equal keys make equal runs.
#[derive(Clone, Copy, PartialEq)]
struct RunKey {
    bold: bool,
    italic: bool,
    color: Hsla,
    /// The underline's color and whether it is wavy.
    underline: Option<(Hsla, bool)>,
    strikethrough: bool,
}

impl RunKey {
    fn of(cell: &PaintCell) -> Self {
        let underline = match cell.underline {
            _ if cell.link => Some((cell.fg, false)),
            UnderlineStyle::Single => Some((cell.underline_color, false)),
            UnderlineStyle::Curly => Some((cell.underline_color, true)),
            _ => None,
        };
        Self {
            bold: cell.bold,
            italic: cell.italic,
            color: cell.fg,
            underline,
            strikethrough: cell.strikethrough,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_text(
    row: &[PaintCell],
    origin_x: f32,
    y: f32,
    cell: CellSize,
    fonts: &FontSet,
    line_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let trimmed = row
        .iter()
        .rposition(|c| {
            c.text != " " || c.underline != UnderlineStyle::None || c.link || c.strikethrough
        })
        .map_or(0, |i| i + 1);
    let mut col = 0;
    while col < trimmed {
        let current = &row[col];
        if current.width == 0 {
            col += 1;
            continue;
        }
        if current.width == 2 {
            // Wide characters are shaped on their own and centred on their two cells.
            let text: SharedString = current.text.clone().into();
            let run = text_run(current, fonts, text.len());
            let shaped = window
                .text_system()
                .shape_line(text, fonts.size, &[run], None);
            let x = cell_x(origin_x, col, cell);
            let offset = ((2.0 * cell.width) - f32::from(shaped.width)).max(0.0) / 2.0;
            let _ = shaped.paint(
                point(px(x + offset), px(y + cell.baseline_shift)),
                line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
            col += 1;
            continue;
        }
        // A run of single-width cells, glyphs snapped to the cell grid.
        let start = col;
        let mut text = String::new();
        let mut runs: Vec<TextRun> = Vec::new();
        let mut last_key = None;
        while col < trimmed && row[col].width == 1 {
            let piece = &row[col];
            let before = text.len();
            text.push_str(&piece.text);
            let len = text.len() - before;
            // Cells that would make equal runs extend the last one; the run (and its font) is
            // only built when the style changes.
            let key = RunKey::of(piece);
            match runs.last_mut() {
                Some(last) if last_key == Some(key) => last.len += len,
                _ => {
                    runs.push(text_run(piece, fonts, len));
                    last_key = Some(key);
                }
            }
            col += 1;
        }
        if text.trim().is_empty()
            && runs
                .iter()
                .all(|r| r.underline.is_none() && r.strikethrough.is_none())
        {
            continue;
        }
        let shaped =
            window
                .text_system()
                .shape_line(text.into(), fonts.size, &runs, Some(px(cell.width)));
        let _ = shaped.paint(
            point(
                px(cell_x(origin_x, start, cell)),
                px(y + cell.baseline_shift),
            ),
            line_height,
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

fn paint_decorations(
    row: &[PaintCell],
    origin_x: f32,
    y: f32,
    cell: CellSize,
    window: &mut Window,
) {
    for (col, current) in row.iter().enumerate() {
        if current.width == 0 || current.link {
            continue;
        }
        let x = cell_x(origin_x, col, cell);
        let width = cell.width * f32::from(current.width.max(1));
        let baseline = (y + cell.height - 2.0).round();
        let line = |window: &mut Window, x: f32, y: f32, w: f32| {
            window.paint_quad(fill(
                Bounds::new(point(px(x), px(y)), size(px(w), px(1.0))),
                current.underline_color,
            ));
        };
        match current.underline {
            UnderlineStyle::Double => {
                line(window, x, baseline - 2.0, width);
                line(window, x, baseline, width);
            }
            UnderlineStyle::Dotted => {
                let mut dot = x;
                while dot < x + width {
                    line(window, dot, baseline, 1.0);
                    dot += 2.0;
                }
            }
            UnderlineStyle::Dashed => {
                let mut dash = x;
                while dash < x + width {
                    line(window, dash, baseline, 3.0f32.min(x + width - dash));
                    dash += 5.0;
                }
            }
            _ => {}
        }
        if current.overline {
            window.paint_quad(fill(
                Bounds::new(point(px(x), px(y)), size(px(width), px(1.0))),
                current.fg,
            ));
        }
    }
}

fn paint_image(
    image: &FrameImage,
    origin_x: f32,
    origin_y: f32,
    cell: CellSize,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    let x = cell_x(origin_x, image.col, cell);
    let y = origin_y + image.row as f32 * cell.height;
    let image_bounds = Bounds::new(point(px(x), px(y)), size(px(image.width), px(image.height)));
    let _ = window.paint_image(
        bounds,
        image_bounds,
        Default::default(),
        image.image.clone(),
        0,
        false,
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_cursor(
    cursor: &FrameCursor,
    origin_x: f32,
    origin_y: f32,
    cell: CellSize,
    fonts: &FontSet,
    line_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let x = cell_x(origin_x, cursor.col, cell);
    let y = origin_y + cursor.row as f32 * cell.height;
    let width = cell.width * f32::from(cursor.width.max(1));
    let cell_bounds = Bounds::new(point(px(x), px(y)), size(px(width), px(cell.height)));
    if !cursor.focused {
        // xterm's inactive cursor: a one-pixel outline.
        window.paint_quad(outline(cell_bounds, cursor.color, BorderStyle::Solid));
        return;
    }
    if cursor.hidden {
        return;
    }
    match cursor.shape {
        CursorShape::Block => {
            window.paint_quad(fill(cell_bounds, cursor.color));
            if !cursor.text.trim().is_empty() {
                let text: SharedString = cursor.text.clone().into();
                let run = TextRun {
                    len: text.len(),
                    font: fonts.font(cursor.bold, cursor.italic),
                    color: cursor.accent,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let force = (cursor.width == 1).then(|| px(cell.width));
                let shaped = window
                    .text_system()
                    .shape_line(text, fonts.size, &[run], force);
                let _ = shaped.paint(
                    point(px(x), px(y + cell.baseline_shift)),
                    line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        }
        CursorShape::Bar => {
            window.paint_quad(fill(
                Bounds::new(point(px(x), px(y)), size(px(1.0), px(cell.height))),
                cursor.color,
            ));
        }
        CursorShape::Underline => {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(x), px(y + cell.height - 1.0)),
                    size(px(width), px(1.0)),
                ),
                cursor.color,
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_preedit(
    preedit: &str,
    cursor: &FrameCursor,
    frame: &Frame,
    origin_x: f32,
    origin_y: f32,
    line_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    if preedit.is_empty() {
        return;
    }
    let x = cell_x(origin_x, cursor.col, frame.cell);
    let y = origin_y + cursor.row as f32 * frame.cell.height;
    let text: SharedString = preedit.to_string().into();
    let run = TextRun {
        len: text.len(),
        font: frame.fonts.font(false, false),
        color: cursor.color,
        background_color: Some(frame.background),
        underline: Some(GpuiUnderline {
            thickness: px(1.0),
            color: Some(cursor.color),
            wavy: false,
        }),
        strikethrough: None,
    };
    let shaped = window
        .text_system()
        .shape_line(text, frame.fonts.size, &[run], None);
    window.paint_quad(fill(
        Bounds::new(
            point(px(x), px(y)),
            size(shaped.width, px(frame.cell.height)),
        ),
        frame.background,
    ));
    let _ = shaped.paint(
        point(px(x), px(y)),
        line_height,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// xterm's overview ruler: three lanes in the scrollbar's width at the right edge of the padding
/// box, marks at least six pixels high.
fn paint_ruler(frame: &Frame, bounds: Bounds<Pixels>, grid_height: f32, window: &mut Window) {
    if frame.ruler.is_empty() || frame.buffer_lines == 0 {
        return;
    }
    let right = f32::from(bounds.origin.x + bounds.size.width);
    let left = right - scrollbar_width();
    let top = f32::from(bounds.origin.y);
    let border = 1.0;
    let outer = ((scrollbar_width() - border) / 3.0).floor();
    let inner = ((scrollbar_width() - border) / 3.0).ceil();
    let pixels_per_line = grid_height / frame.buffer_lines as f32;
    let mark_height = pixels_per_line.clamp(6.0, 12.0).round();
    for mark in &frame.ruler {
        let (x, width) = match mark.lane {
            RulerLane::Left => (left + border, outer),
            RulerLane::Center => (left + border + outer, inner),
        };
        let y = ((grid_height - 1.0) * mark.start - mark_height / 2.0).round();
        let height = ((grid_height - 1.0) * (mark.end - mark.start) + mark_height).round();
        window.paint_quad(fill(
            Bounds::new(point(px(x), px(top + y)), size(px(width), px(height))),
            mark.color,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(edit: impl FnOnce(&mut PaintCell)) -> PaintCell {
        let mut cell = PaintCell {
            text: "a".into(),
            width: 1,
            fg: hsla(Rgba {
                r: 200,
                g: 200,
                b: 200,
                a: 255,
            }),
            bg: None,
            underline: UnderlineStyle::None,
            underline_color: hsla(Rgba {
                r: 200,
                g: 200,
                b: 200,
                a: 255,
            }),
            bold: false,
            italic: false,
            strikethrough: false,
            overline: false,
            link: false,
        };
        edit(&mut cell);
        cell
    }

    #[test]
    fn equal_run_keys_mean_equal_text_runs() {
        let red = hsla(Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        });
        let fonts = FontSet {
            family: "JetBrains Mono".into(),
            fallbacks: vec!["Symbols Nerd Font Mono".into()],
            size: muxus_ui::zoom::px(13.0),
        };
        let cells = [
            cell(|_| {}),
            cell(|c| c.text = "b".into()),
            cell(|c| c.bg = Some(red)),
            cell(|c| c.overline = true),
            cell(|c| c.fg = red),
            cell(|c| c.bold = true),
            cell(|c| c.italic = true),
            cell(|c| c.strikethrough = true),
            cell(|c| c.link = true),
            cell(|c| c.underline = UnderlineStyle::Single),
            cell(|c| c.underline = UnderlineStyle::Curly),
            cell(|c| c.underline = UnderlineStyle::Double),
            cell(|c| c.underline = UnderlineStyle::Dotted),
            cell(|c| c.underline_color = red),
            cell(|c| {
                c.underline = UnderlineStyle::Single;
                c.underline_color = red;
            }),
            cell(|c| {
                c.link = true;
                c.underline = UnderlineStyle::Curly;
            }),
        ];
        for a in &cells {
            for b in &cells {
                assert_eq!(
                    RunKey::of(a) == RunKey::of(b),
                    text_run(a, &fonts, 1) == text_run(b, &fonts, 1),
                    "{a:?} / {b:?}"
                );
            }
        }
    }
}
