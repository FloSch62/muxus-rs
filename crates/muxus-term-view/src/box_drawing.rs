//! Box Drawing and Block Elements (U+2500–U+259F) drawn on the cell grid, the way xterm.js's
//! GPU renderer draws them (`customGlyphs`), instead of taken from a font.
//!
//! The bundled JetBrains Mono leaves this range out, and the symbol font's glyphs for it are an
//! em wide and shorter than the cell, which squashes block-character logos and breaks frame
//! lines apart (Muxus 1bb4f3c). Drawn here, every shape fills its own cell and nothing else
//! (e4312ab: an indent guide never sticks out past its block), and each edge lands on a whole
//! device pixel, so neighbouring cells meet without seams or antialiased hairlines at any cell
//! size (the WebGL patch in the same commit).
//!
//! The shapes are adapted from xterm.js's `CustomGlyphDefinitions.ts` (MIT, see
//! `ATTRIBUTION.md`): lines are SVG-like paths on the unit cell with a light (1) or heavy (3)
//! stroke in CSS pixels, block elements are rectangles in eighths of the cell.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// One piece of a glyph, in device pixels from the cell's top-left corner.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Shape {
    /// A filled rectangle `[x0, x1) × [y0, y1)`, on whole device pixels.
    Rect { x0: f32, y0: f32, x1: f32, y1: f32 },
    /// The whole cell at this share of the color (the shades ░ ▒ ▓).
    Shade(f32),
    /// A stroked path (diagonals, rounded corners), clipped to the cell.
    Stroke { segments: Vec<Segment>, width: f32 },
    /// Filled subpaths (Powerline symbols), nonzero winding as the canvas fills them, clipped to
    /// the cell.
    Fill { subpaths: Vec<Vec<Segment>> },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Segment {
    Move(f32, f32),
    Line(f32, f32),
    Cubic {
        c1: (f32, f32),
        c2: (f32, f32),
        to: (f32, f32),
    },
    Quad {
        c: (f32, f32),
        to: (f32, f32),
    },
    Close,
}

/// The glyph for `text` when it is one character drawn on the grid: here, or by
/// [`powerline`](crate::powerline).
pub(crate) fn box_glyph_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    (chars.next().is_none()
        && (('\u{2500}'..='\u{259f}').contains(&c) || crate::powerline::is_powerline(c)))
    .then_some(c)
}

// ---- definitions ----------------------------------------------------------------------------

/// │
const TOP_TO_BOTTOM: &str = "M.5,0 L.5,1";
/// ─
const LEFT_TO_RIGHT: &str = "M0,.5 L1,.5";
/// └
const TOP_TO_RIGHT: &str = "M.5,0 L.5,.5 L1,.5";
/// ┘
const TOP_TO_LEFT: &str = "M.5,0 L.5,.5 L0,.5";
/// ┐
const LEFT_TO_BOTTOM: &str = "M0,.5 L.5,.5 L.5,1";
/// ┌
const RIGHT_TO_BOTTOM: &str = "M0.5,1 L.5,.5 L1,.5";
/// ╵
const MIDDLE_TO_TOP: &str = "M.5,.5 L.5,0";
/// ╴
const MIDDLE_TO_LEFT: &str = "M.5,.5 L0,.5";
/// ╶
const MIDDLE_TO_RIGHT: &str = "M.5,.5 L1,.5";
/// ╷
const MIDDLE_TO_BOTTOM: &str = "M.5,.5 L.5,1";
/// ┴
const T_TOP: &str = "M0,.5 L1,.5 M.5,.5 L.5,0";
/// ┤
const T_LEFT: &str = "M.5,0 L.5,1 M.5,.5 L0,.5";
/// ├
const T_RIGHT: &str = "M.5,0 L.5,1 M.5,.5 L1,.5";
/// ┬
const T_BOTTOM: &str = "M0,.5 L1,.5 M.5,.5 L.5,1";
/// ┼
const CROSS: &str = "M0,.5 L1,.5 M.5,0 L.5,1";
/// ╌: .2 empty, .3 filled
const TWO_DASHES_HORIZONTAL: &str = "M.1,.5 L.4,.5 M.6,.5 L.9,.5";
/// ┄: .1333 empty, .2 filled
const THREE_DASHES_HORIZONTAL: &str = "M.0667,.5 L.2667,.5 M.4,.5 L.6,.5 M.7333,.5 L.9333,.5";
/// ┉: .1 empty, .15 filled
const FOUR_DASHES_HORIZONTAL: &str = "M.05,.5 L.2,.5 M.3,.5 L.45,.5 M.55,.5 L.7,.5 M.8,.5 L.95,.5";
/// ╎
const TWO_DASHES_VERTICAL: &str = "M.5,.1 L.5,.4 M.5,.6 L.5,.9";
/// ┆
const THREE_DASHES_VERTICAL: &str = "M.5,.0667 L.5,.2667 M.5,.4 L.5,.6 M.5,.7333 L.5,.9333";
/// ┊ (xterm's definition joins its two middle dashes)
const FOUR_DASHES_VERTICAL: &str = "M.5,.05 L.5,.2 M.5,.3 L.5,.45 L.5,.55 M.5,.7 L.5,.95";

const LIGHT: f32 = 1.;
const HEAVY: f32 = 3.;

/// A path and its stroke width in CSS pixels.
type Part = (String, f32);

fn one(path: &str, weight: f32) -> Vec<Part> {
    vec![(path.to_string(), weight)]
}

/// A light part and a heavy part.
fn mixed(light: &str, heavy: &str) -> Vec<Part> {
    vec![(light.to_string(), LIGHT), (heavy.to_string(), HEAVY)]
}

/// The paths of a box-drawing character. `xp` and `yp` are the gap of double lines, as
/// fractions of the cell's width and height.
fn line_parts(c: char, xp: f32, yp: f32) -> Option<Vec<Part>> {
    let (l, r) = (0.5 - xp, 0.5 + xp);
    let (t, b) = (0.5 - yp, 0.5 + yp);
    // The arcs' straight part: how far the line runs before it bends.
    let bend = yp / 0.15 * 0.5;
    let f = |path: String| vec![(path, LIGHT)];
    Some(match c {
        // Light and heavy solid lines (2500-2503)
        '─' => one(LEFT_TO_RIGHT, LIGHT),
        '━' => one(LEFT_TO_RIGHT, HEAVY),
        '│' => one(TOP_TO_BOTTOM, LIGHT),
        '┃' => one(TOP_TO_BOTTOM, HEAVY),
        // Light and heavy dashed lines (2504-250B)
        '┄' => one(THREE_DASHES_HORIZONTAL, LIGHT),
        '┅' => one(THREE_DASHES_HORIZONTAL, HEAVY),
        '┆' => one(THREE_DASHES_VERTICAL, LIGHT),
        '┇' => one(THREE_DASHES_VERTICAL, HEAVY),
        '┈' => one(FOUR_DASHES_HORIZONTAL, LIGHT),
        '┉' => one(FOUR_DASHES_HORIZONTAL, HEAVY),
        '┊' => one(FOUR_DASHES_VERTICAL, LIGHT),
        '┋' => one(FOUR_DASHES_VERTICAL, HEAVY),
        // Light and heavy line box components (250C-254B)
        '┌' => one(RIGHT_TO_BOTTOM, LIGHT),
        '┍' => mixed(MIDDLE_TO_BOTTOM, MIDDLE_TO_RIGHT),
        '┎' => mixed(MIDDLE_TO_RIGHT, MIDDLE_TO_BOTTOM),
        '┏' => one(RIGHT_TO_BOTTOM, HEAVY),
        '┐' => one(LEFT_TO_BOTTOM, LIGHT),
        '┑' => mixed(MIDDLE_TO_BOTTOM, MIDDLE_TO_LEFT),
        '┒' => mixed(MIDDLE_TO_LEFT, MIDDLE_TO_BOTTOM),
        '┓' => one(LEFT_TO_BOTTOM, HEAVY),
        '└' => one(TOP_TO_RIGHT, LIGHT),
        '┕' => mixed(MIDDLE_TO_TOP, MIDDLE_TO_RIGHT),
        '┖' => mixed(MIDDLE_TO_RIGHT, MIDDLE_TO_TOP),
        '┗' => one(TOP_TO_RIGHT, HEAVY),
        '┘' => one(TOP_TO_LEFT, LIGHT),
        '┙' => mixed(MIDDLE_TO_TOP, MIDDLE_TO_LEFT),
        '┚' => mixed(MIDDLE_TO_LEFT, MIDDLE_TO_TOP),
        '┛' => one(TOP_TO_LEFT, HEAVY),
        '├' => one(T_RIGHT, LIGHT),
        '┝' => mixed(TOP_TO_BOTTOM, MIDDLE_TO_RIGHT),
        '┞' => mixed(RIGHT_TO_BOTTOM, MIDDLE_TO_TOP),
        '┟' => mixed(TOP_TO_RIGHT, MIDDLE_TO_BOTTOM),
        '┠' => mixed(MIDDLE_TO_RIGHT, TOP_TO_BOTTOM),
        '┡' => mixed(MIDDLE_TO_BOTTOM, TOP_TO_RIGHT),
        '┢' => mixed(MIDDLE_TO_TOP, RIGHT_TO_BOTTOM),
        '┣' => one(T_RIGHT, HEAVY),
        '┤' => one(T_LEFT, LIGHT),
        '┥' => mixed(TOP_TO_BOTTOM, MIDDLE_TO_LEFT),
        '┦' => mixed(LEFT_TO_BOTTOM, MIDDLE_TO_TOP),
        '┧' => mixed(TOP_TO_LEFT, MIDDLE_TO_BOTTOM),
        '┨' => mixed(MIDDLE_TO_LEFT, TOP_TO_BOTTOM),
        '┩' => mixed(MIDDLE_TO_BOTTOM, TOP_TO_LEFT),
        '┪' => mixed(MIDDLE_TO_TOP, LEFT_TO_BOTTOM),
        '┫' => one(T_LEFT, HEAVY),
        '┬' => one(T_BOTTOM, LIGHT),
        '┭' => mixed(RIGHT_TO_BOTTOM, MIDDLE_TO_LEFT),
        '┮' => mixed(LEFT_TO_BOTTOM, MIDDLE_TO_RIGHT),
        '┯' => mixed(MIDDLE_TO_BOTTOM, LEFT_TO_RIGHT),
        '┰' => mixed(LEFT_TO_RIGHT, MIDDLE_TO_BOTTOM),
        '┱' => mixed(MIDDLE_TO_RIGHT, LEFT_TO_BOTTOM),
        '┲' => mixed(MIDDLE_TO_LEFT, RIGHT_TO_BOTTOM),
        '┳' => one(T_BOTTOM, HEAVY),
        '┴' => one(T_TOP, LIGHT),
        '┵' => mixed(TOP_TO_RIGHT, MIDDLE_TO_LEFT),
        '┶' => mixed(TOP_TO_LEFT, MIDDLE_TO_RIGHT),
        '┷' => mixed(MIDDLE_TO_TOP, LEFT_TO_RIGHT),
        '┸' => mixed(LEFT_TO_RIGHT, MIDDLE_TO_TOP),
        '┹' => mixed(MIDDLE_TO_RIGHT, TOP_TO_LEFT),
        '┺' => mixed(MIDDLE_TO_LEFT, TOP_TO_RIGHT),
        '┻' => one(T_TOP, HEAVY),
        '┼' => one(CROSS, LIGHT),
        '┽' => mixed(
            &format!("{TOP_TO_BOTTOM} {MIDDLE_TO_RIGHT}"),
            MIDDLE_TO_LEFT,
        ),
        '┾' => mixed(
            &format!("{TOP_TO_BOTTOM} {MIDDLE_TO_LEFT}"),
            MIDDLE_TO_RIGHT,
        ),
        '┿' => mixed(TOP_TO_BOTTOM, LEFT_TO_RIGHT),
        '╀' => mixed(
            &format!("{LEFT_TO_RIGHT} {MIDDLE_TO_BOTTOM}"),
            MIDDLE_TO_TOP,
        ),
        '╁' => mixed(
            &format!("{MIDDLE_TO_TOP} {LEFT_TO_RIGHT}"),
            MIDDLE_TO_BOTTOM,
        ),
        '╂' => mixed(LEFT_TO_RIGHT, TOP_TO_BOTTOM),
        '╃' => mixed(RIGHT_TO_BOTTOM, TOP_TO_LEFT),
        '╄' => mixed(LEFT_TO_BOTTOM, TOP_TO_RIGHT),
        '╅' => mixed(TOP_TO_RIGHT, LEFT_TO_BOTTOM),
        '╆' => mixed(TOP_TO_LEFT, RIGHT_TO_BOTTOM),
        '╇' => mixed(
            MIDDLE_TO_BOTTOM,
            &format!("{MIDDLE_TO_TOP} {LEFT_TO_RIGHT}"),
        ),
        '╈' => mixed(
            MIDDLE_TO_TOP,
            &format!("{LEFT_TO_RIGHT} {MIDDLE_TO_BOTTOM}"),
        ),
        '╉' => mixed(
            MIDDLE_TO_RIGHT,
            &format!("{TOP_TO_BOTTOM} {MIDDLE_TO_LEFT}"),
        ),
        '╊' => mixed(
            MIDDLE_TO_LEFT,
            &format!("{TOP_TO_BOTTOM} {MIDDLE_TO_RIGHT}"),
        ),
        '╋' => one(CROSS, HEAVY),
        // Light and heavy dashed lines (254C-254F)
        '╌' => one(TWO_DASHES_HORIZONTAL, LIGHT),
        '╍' => one(TWO_DASHES_HORIZONTAL, HEAVY),
        '╎' => one(TWO_DASHES_VERTICAL, LIGHT),
        '╏' => one(TWO_DASHES_VERTICAL, HEAVY),
        // Double lines (2550-2551)
        '═' => f(format!("M0,{t} L1,{t} M0,{b} L1,{b}")),
        '║' => f(format!("M{l},0 L{l},1 M{r},0 L{r},1")),
        // Light and double line box components (2552-256C)
        '╒' => f(format!("M.5,1 L.5,{t} L1,{t} M.5,{b} L1,{b}")),
        '╓' => f(format!("M{l},1 L{l},.5 L1,.5 M{r},.5 L{r},1")),
        '╔' => f(format!("M1,{t} L{l},{t} L{l},1 M1,{b} L{r},{b} L{r},1")),
        '╕' => f(format!("M0,{t} L.5,{t} L.5,1 M0,{b} L.5,{b}")),
        '╖' => f(format!("M{r},1 L{r},.5 L0,.5 M{l},.5 L{l},1")),
        '╗' => f(format!("M0,{b} L{l},{b} L{l},1 M0,{t} L{r},{t} L{r},1")),
        '╘' => f(format!("M.5,0 L.5,{b} L1,{b} M.5,{t} L1,{t}")),
        '╙' => f(format!("M1,.5 L{l},.5 L{l},0 M{r},.5 L{r},0")),
        '╚' => f(format!("M1,{t} L{r},{t} L{r},0 M1,{b} L{l},{b} L{l},0")),
        '╛' => f(format!("M0,{b} L.5,{b} L.5,0 M0,{t} L.5,{t}")),
        '╜' => f(format!("M0,.5 L{r},.5 L{r},0 M{l},.5 L{l},0")),
        '╝' => f(format!("M0,{t} L{l},{t} L{l},0 M0,{b} L{r},{b} L{r},0")),
        '╞' => f(format!("{TOP_TO_BOTTOM} M.5,{t} L1,{t} M.5,{b} L1,{b}")),
        '╟' => f(format!("M{l},0 L{l},1 M{r},0 L{r},1 M{r},.5 L1,.5")),
        '╠' => f(format!(
            "M{l},0 L{l},1 M1,{b} L{r},{b} L{r},1 M1,{t} L{r},{t} L{r},0"
        )),
        '╡' => f(format!("{TOP_TO_BOTTOM} M0,{t} L.5,{t} M0,{b} L.5,{b}")),
        '╢' => f(format!("M0,.5 L{l},.5 M{l},0 L{l},1 M{r},0 L{r},1")),
        '╣' => f(format!(
            "M{r},0 L{r},1 M0,{b} L{l},{b} L{l},1 M0,{t} L{l},{t} L{l},0"
        )),
        '╤' => f(format!("M0,{t} L1,{t} M0,{b} L1,{b} M.5,{b} L.5,1")),
        '╥' => f(format!("{LEFT_TO_RIGHT} M{l},.5 L{l},1 M{r},.5 L{r},1")),
        '╦' => f(format!(
            "M0,{t} L1,{t} M0,{b} L{l},{b} L{l},1 M1,{b} L{r},{b} L{r},1"
        )),
        '╧' => f(format!("M.5,0 L.5,{t} M0,{t} L1,{t} M0,{b} L1,{b}")),
        '╨' => f(format!("{LEFT_TO_RIGHT} M{l},.5 L{l},0 M{r},.5 L{r},0")),
        '╩' => f(format!(
            "M0,{b} L1,{b} M0,{t} L{l},{t} L{l},0 M1,{t} L{r},{t} L{r},0"
        )),
        '╪' => f(format!("{TOP_TO_BOTTOM} M0,{t} L1,{t} M0,{b} L1,{b}")),
        '╫' => f(format!("{LEFT_TO_RIGHT} M{l},0 L{l},1 M{r},0 L{r},1")),
        '╬' => f(format!(
            "M0,{b} L{l},{b} L{l},1 M1,{b} L{r},{b} L{r},1 M0,{t} L{l},{t} L{l},0 M1,{t} L{r},{t} L{r},0"
        )),
        // Character cell arcs (256D-2570)
        '╭' => f(format!("M.5,1 L.5,{0} C.5,{0},.5,.5,1,.5", 0.5 + bend)),
        '╮' => f(format!("M.5,1 L.5,{0} C.5,{0},.5,.5,0,.5", 0.5 + bend)),
        '╯' => f(format!("M.5,0 L.5,{0} C.5,{0},.5,.5,0,.5", 0.5 - bend)),
        '╰' => f(format!("M.5,0 L.5,{0} C.5,{0},.5,.5,1,.5", 0.5 - bend)),
        // Character cell diagonals (2571-2573)
        '╱' => one("M1,0 L0,1", LIGHT),
        '╲' => one("M0,0 L1,1", LIGHT),
        '╳' => one("M1,0 L0,1 M0,0 L1,1", LIGHT),
        // Light and heavy half lines (2574-257B)
        '╴' => one(MIDDLE_TO_LEFT, LIGHT),
        '╵' => one(MIDDLE_TO_TOP, LIGHT),
        '╶' => one(MIDDLE_TO_RIGHT, LIGHT),
        '╷' => one(MIDDLE_TO_BOTTOM, LIGHT),
        '╸' => one(MIDDLE_TO_LEFT, HEAVY),
        '╹' => one(MIDDLE_TO_TOP, HEAVY),
        '╺' => one(MIDDLE_TO_RIGHT, HEAVY),
        '╻' => one(MIDDLE_TO_BOTTOM, HEAVY),
        // Mixed light and heavy lines (257C-257F)
        '╼' => mixed(MIDDLE_TO_LEFT, MIDDLE_TO_RIGHT),
        '╽' => mixed(MIDDLE_TO_TOP, MIDDLE_TO_BOTTOM),
        '╾' => mixed(MIDDLE_TO_RIGHT, MIDDLE_TO_LEFT),
        '╿' => mixed(MIDDLE_TO_BOTTOM, MIDDLE_TO_TOP),
        _ => return None,
    })
}

/// Block elements as `(x, y, w, h)` in eighths of the cell.
fn block_parts(c: char) -> Option<&'static [(u8, u8, u8, u8)]> {
    Some(match c {
        '▀' => &[(0, 0, 8, 4)],
        '▁' => &[(0, 7, 8, 1)],
        '▂' => &[(0, 6, 8, 2)],
        '▃' => &[(0, 5, 8, 3)],
        '▄' => &[(0, 4, 8, 4)],
        '▅' => &[(0, 3, 8, 5)],
        '▆' => &[(0, 2, 8, 6)],
        '▇' => &[(0, 1, 8, 7)],
        '█' => &[(0, 0, 8, 8)],
        '▉' => &[(0, 0, 7, 8)],
        '▊' => &[(0, 0, 6, 8)],
        '▋' => &[(0, 0, 5, 8)],
        '▌' => &[(0, 0, 4, 8)],
        '▍' => &[(0, 0, 3, 8)],
        '▎' => &[(0, 0, 2, 8)],
        '▏' => &[(0, 0, 1, 8)],
        '▐' => &[(4, 0, 4, 8)],
        '▔' => &[(0, 0, 8, 1)],
        '▕' => &[(7, 0, 1, 8)],
        '▖' => &[(0, 4, 4, 4)],
        '▗' => &[(4, 4, 4, 4)],
        '▘' => &[(0, 0, 4, 4)],
        '▙' => &[(0, 0, 4, 8), (0, 4, 8, 4)],
        '▚' => &[(0, 0, 4, 4), (4, 4, 4, 4)],
        '▛' => &[(0, 0, 4, 8), (4, 0, 4, 4)],
        '▜' => &[(0, 0, 8, 4), (4, 0, 4, 8)],
        '▝' => &[(4, 0, 4, 4)],
        '▞' => &[(4, 0, 4, 4), (0, 4, 4, 4)],
        '▟' => &[(4, 0, 4, 8), (0, 4, 8, 4)],
        _ => return None,
    })
}

// ---- rasterizing ----------------------------------------------------------------------------

/// xterm's `translateArgs`: a unit coordinate to device pixels, on the half pixel so a one-pixel
/// line is crisp, kept inside the cell.
fn snap(unit: f32, size: f32) -> f32 {
    let value = unit * size;
    if value == 0. {
        return 0.;
    }
    // `Math.round` rounds halves up.
    ((value + 0.5 + 0.5).floor() - 0.5).clamp(0., size)
}

/// One subpath: its start and its segments.
type Subpath = Vec<Segment>;

fn parse_path(path: &str, width: f32, height: f32) -> Vec<Subpath> {
    let mut subpaths: Vec<Subpath> = Vec::new();
    for instruction in path.split(' ') {
        let mut chars = instruction.chars();
        let Some(kind) = chars.next() else { continue };
        let args: Vec<f32> = chars
            .as_str()
            .split(',')
            .filter_map(|arg| arg.parse().ok())
            .collect();
        let point = |index: usize| (snap(args[index], width), snap(args[index + 1], height));
        match kind {
            'M' if args.len() >= 2 => subpaths.push(vec![Segment::Move(point(0).0, point(0).1)]),
            'L' if args.len() >= 2 => {
                if let Some(current) = subpaths.last_mut() {
                    current.push(Segment::Line(point(0).0, point(0).1));
                }
            }
            'C' if args.len() >= 6 => {
                if let Some(current) = subpaths.last_mut() {
                    current.push(Segment::Cubic {
                        c1: point(0),
                        c2: point(2),
                        to: point(4),
                    });
                }
            }
            _ => {}
        }
    }
    subpaths
}

/// A rectangle snapped outward-or-inward to whole pixels inside the cell, at least one pixel
/// thick.
fn pixel_rect(x0: f32, y0: f32, x1: f32, y1: f32, width: f32, height: f32) -> Option<Shape> {
    let snap_span = |a: f32, b: f32, size: f32| {
        let (mut a, mut b) = (a.round().clamp(0., size), b.round().clamp(0., size));
        if b <= a {
            if a >= size {
                a = (size - 1.).max(0.);
            }
            b = (a + 1.).min(size);
        }
        (a, b)
    };
    let (x0, x1) = snap_span(x0, x1, width);
    let (y0, y1) = snap_span(y0, y1, height);
    (x1 > x0 && y1 > y0).then_some(Shape::Rect { x0, y0, x1, y1 })
}

/// A subpath of horizontal and vertical lines as rectangles: butt ends, square (miter) joins,
/// as the canvas strokes them.
fn line_rects(subpath: &[Segment], stroke: f32, width: f32, height: f32) -> Option<Vec<Shape>> {
    let half = stroke / 2.;
    let points = subpath
        .iter()
        .map(|segment| match *segment {
            Segment::Move(x, y) | Segment::Line(x, y) => Some((x, y)),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let lines = points.len().saturating_sub(1);
    let mut shapes = Vec::new();
    for index in 0..lines {
        let ((ax, ay), (bx, by)) = (points[index], points[index + 1]);
        // A joint with the previous or next line takes the square corner.
        let start = if index > 0 { half } else { 0. };
        let end = if index + 1 < lines { half } else { 0. };
        let rect = if ay == by {
            let sign = if bx >= ax { 1. } else { -1. };
            let (x0, x1) = (ax - sign * start, bx + sign * end);
            pixel_rect(x0.min(x1), ay - half, x0.max(x1), ay + half, width, height)
        } else if ax == bx {
            let sign = if by >= ay { 1. } else { -1. };
            let (y0, y1) = (ay - sign * start, by + sign * end);
            pixel_rect(ax - half, y0.min(y1), ax + half, y0.max(y1), width, height)
        } else {
            return None;
        };
        shapes.extend(rect);
    }
    Some(shapes)
}

/// The shapes of `c` in a cell of `width` × `height` device pixels. `css_pixel` is the size of
/// a CSS pixel in device pixels (the window's scale times the interface zoom), which line
/// weights are given in.
pub(crate) fn glyph_shapes(c: char, width: f32, height: f32, css_pixel: f32) -> Option<Vec<Shape>> {
    if width <= 0. || height <= 0. {
        return None;
    }
    if let Some(boxes) = block_parts(c) {
        // Every edge on a whole device pixel (Muxus's patch to xterm's WebGL addon).
        return Some(
            boxes
                .iter()
                .filter_map(|&(x, y, w, h)| {
                    let edge = |eighths: u8, size: f32| (f32::from(eighths) * size / 8.).round();
                    let (x0, x1) = (edge(x, width), edge(x + w, width));
                    let (y0, y1) = (edge(y, height), edge(y + h, height));
                    (x1 > x0 && y1 > y0).then_some(Shape::Rect { x0, y0, x1, y1 })
                })
                .collect(),
        );
    }
    match c {
        '░' => return Some(vec![Shape::Shade(0.25)]),
        '▒' => return Some(vec![Shape::Shade(0.5)]),
        '▓' => return Some(vec![Shape::Shade(0.75)]),
        _ => {}
    }
    let xp = 0.15;
    let yp = 0.15 / height * width;
    let parts = line_parts(c, xp, yp)?;
    let mut shapes = Vec::new();
    for (path, weight) in parts {
        let stroke = (weight * css_pixel).max(1.);
        for subpath in parse_path(&path, width, height) {
            match line_rects(&subpath, stroke, width, height) {
                Some(rects) => shapes.extend(rects),
                None => shapes.push(Shape::Stroke {
                    segments: subpath,
                    width: stroke,
                }),
            }
        }
    }
    Some(shapes)
}

type CacheKey = (char, u32, u32, u32, u32);

thread_local! {
    static SHAPES: RefCell<HashMap<CacheKey, Option<Rc<Vec<Shape>>>>> = RefCell::new(HashMap::new());
}

/// [`glyph_shapes`] or [`powerline::shapes`](crate::powerline::shapes), remembered per cell and
/// font size (a frame draws the same few glyphs many times). `font_size` is in device pixels.
pub(crate) fn cached_glyph_shapes(
    c: char,
    width: f32,
    height: f32,
    css_pixel: f32,
    font_size: f32,
) -> Option<Rc<Vec<Shape>>> {
    let key = (
        c,
        width.to_bits(),
        height.to_bits(),
        css_pixel.to_bits(),
        font_size.to_bits(),
    );
    SHAPES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(shapes) = cache.get(&key) {
            return shapes.clone();
        }
        if cache.len() > 4096 {
            cache.clear();
        }
        let shapes = crate::powerline::shapes(c, width, height, font_size)
            .or_else(|| glyph_shapes(c, width, height, css_pixel))
            .map(Rc::new);
        cache.insert(key, shapes.clone());
        shapes
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rects(c: char, width: f32, height: f32) -> Vec<(f32, f32, f32, f32)> {
        glyph_shapes(c, width, height, 1.)
            .unwrap()
            .into_iter()
            .map(|shape| match shape {
                Shape::Rect { x0, y0, x1, y1 } => (x0, y0, x1, y1),
                other => panic!("{c}: not a rectangle: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn draws_the_whole_range_and_nothing_else() {
        for code in 0x2500..=0x259f {
            let c = char::from_u32(code).unwrap();
            assert!(box_glyph_char(&c.to_string()).is_some());
            let shapes = glyph_shapes(c, 8., 17., 1.).unwrap_or_else(|| panic!("{c} undrawn"));
            assert!(!shapes.is_empty(), "{c} draws nothing");
        }
        assert_eq!(box_glyph_char("a"), None);
        assert_eq!(box_glyph_char("─a"), None);
        assert_eq!(box_glyph_char("\u{25a0}"), None);
    }

    #[test]
    fn horizontal_lines_span_the_cell_so_neighbours_meet() {
        // A 7×17 cell (JetBrains Mono at 13px): one pixel row through the middle.
        assert_eq!(rects('─', 7., 17.), [(0., 8., 7., 9.)]);
        assert_eq!(rects('━', 7., 17.), [(0., 7., 7., 10.)]);
        assert_eq!(rects('│', 7., 17.), [(3., 0., 4., 17.)]);
    }

    #[test]
    fn corners_join_square_and_stay_in_the_cell() {
        // ┌: down from the middle and right to the edge, the corner filled.
        assert_eq!(rects('┌', 8., 16.), [(4., 8., 5., 16.), (4., 8., 8., 9.)]);
        for code in 0x2500..=0x257f {
            let c = char::from_u32(code).unwrap();
            for shape in glyph_shapes(c, 9., 19., 2.).unwrap() {
                if let Shape::Rect { x0, y0, x1, y1 } = shape {
                    assert!(
                        0. <= x0 && x1 <= 9. && 0. <= y0 && y1 <= 19.,
                        "{c} leaves its cell"
                    );
                    assert_eq!(
                        (x0.fract(), y0.fract(), x1.fract(), y1.fract()),
                        (0., 0., 0., 0.)
                    );
                }
            }
        }
    }

    #[test]
    fn double_lines_keep_a_gap() {
        let lines = rects('═', 8., 17.);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].3 < lines[1].1, "{lines:?}");
        assert_eq!(rects('║', 8., 17.).len(), 2);
    }

    #[test]
    fn blocks_fill_eighths_on_whole_pixels() {
        assert_eq!(rects('█', 7., 17.), [(0., 0., 7., 17.)]);
        // Halves of an odd cell round, so ▀ over ▄ tiles without a seam.
        let upper = rects('▀', 7., 17.);
        let lower = rects('▄', 7., 17.);
        assert_eq!(upper[0].3, lower[0].1);
        assert_eq!((upper[0].1, lower[0].3), (0., 17.));
        assert_eq!(rects('▐', 7., 17.), [(4., 0., 7., 17.)]);
        assert_eq!(rects('▚', 8., 16.), [(0., 0., 4., 8.), (4., 8., 8., 16.)]);
    }

    #[test]
    fn shades_and_curves_are_drawn_as_such() {
        assert_eq!(glyph_shapes('▒', 8., 16., 1.).unwrap(), [Shape::Shade(0.5)]);
        let arc = glyph_shapes('╭', 8., 16., 1.).unwrap();
        assert!(matches!(&arc[0], Shape::Stroke { segments, .. }
            if segments.iter().any(|segment| matches!(segment, Segment::Cubic { .. }))));
        assert!(matches!(
            &glyph_shapes('╱', 8., 16., 1.).unwrap()[0],
            Shape::Stroke { .. }
        ));
    }

    #[test]
    fn line_weights_follow_the_device_pixel_ratio() {
        let rects = glyph_shapes('─', 16., 34., 2.).unwrap();
        let Shape::Rect { y0, y1, .. } = rects[0] else {
            panic!()
        };
        assert_eq!(y1 - y0, 2.);
    }

    #[test]
    fn the_cache_returns_the_same_shapes() {
        let first = cached_glyph_shapes('┼', 8., 17., 1., 13.).unwrap();
        let again = cached_glyph_shapes('┼', 8., 17., 1., 13.).unwrap();
        assert!(Rc::ptr_eq(&first, &again));
        assert_eq!(cached_glyph_shapes('a', 8., 17., 1., 13.), None);
    }
}
