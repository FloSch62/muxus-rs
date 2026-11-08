//! What Monaco draws around the text: the current line's box (`renderLineHighlight: 'all'`,
//! `editor.lineHighlightBorder`) and the overview ruler on the right edge, where the cursor,
//! occurrences and problems show for the whole document. The line box ends where the minimap
//! (`minimap.rs`) starts.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use muxus_ui::gpui_kit::base::input::EditorState;
use muxus_ui::gpui_kit::{
    App, Bounds, Entity, Hsla, IntoElement, Pixels, Styled, Window, canvas, fill, point, size,
};
use muxus_ui::syntax::MonacoColors;

use super::minimap::{MinimapInput, minimap_geometry};
use crate::util::code_editor::SharedProbe;

/// The overview ruler's width (`verticalScrollbarSize`).
pub const RULER_WIDTH: f32 = 14.;
/// Monaco's `MIN_DECORATION_HEIGHT` and cursor height in the ruler.
const MIN_MARK_HEIGHT: f32 = 6.;
const CURSOR_MARK_HEIGHT: f32 = 2.;
/// `editor.lineHighlightBorder` is 2px wide.
const LINE_BOX_BORDER: f32 = 2.;

/// A ruler lane. Monaco's ruler shows two lanes at the editor's width: occurrences on the
/// left, problems on the right, the cursor across both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    Center,
    Right,
    Full,
}

/// The horizontal extent of `lane` inside a ruler `width` wide at `scale` device pixels per
/// pixel: one device pixel for the border, then two lanes splitting the rest.
pub fn lane_extent(lane: Lane, width: f32, scale: f32) -> (f32, f32) {
    let canvas = (width * scale).floor();
    let remaining = canvas - 1.;
    let left = (remaining / 2.).floor();
    let (x, w) = match lane {
        Lane::Center => (1., left),
        Lane::Right => (1. + left, remaining - left),
        Lane::Full => (1., remaining),
    };
    (x / scale, w / scale)
}

/// A mark in the ruler: lines (0-based, inclusive) in a lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    pub lane: Lane,
    pub lines: (usize, usize),
    pub color: Hsla,
}

/// How the document's lines map onto the ruler (`getVerticalOffsetForLineNumber` scaled by
/// the ruler's height over the scroll height).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RulerMap {
    pub height: f32,
    pub line_height: f32,
    pub padding_top: f32,
    pub padding_bottom: f32,
    pub line_count: usize,
}

impl RulerMap {
    fn ratio(&self) -> f32 {
        let content =
            self.padding_top + self.line_count as f32 * self.line_height + self.padding_bottom;
        self.height / content.max(self.height).max(1.)
    }

    fn line_top(&self, line: usize) -> f32 {
        self.padding_top + line as f32 * self.line_height
    }

    /// The vertical extent of a mark over `lines`: at least six pixels, kept inside.
    pub fn mark_extent(&self, lines: (usize, usize)) -> (f32, f32) {
        let ratio = self.ratio();
        let mut y1 = (self.line_top(lines.0) * ratio).floor();
        let mut y2 = ((self.line_top(lines.1) + self.line_height) * ratio).floor();
        if y2 - y1 < MIN_MARK_HEIGHT {
            let half = MIN_MARK_HEIGHT / 2.;
            let center = ((y1 + y2) / 2.)
                .floor()
                .clamp(half, (self.height - half).max(half));
            y1 = center - half;
            y2 = center + half;
        }
        (y1, y2)
    }

    /// The cursor's mark: two pixels centered on the top of its line.
    pub fn cursor_extent(&self, line: usize) -> (f32, f32) {
        let half = CURSOR_MARK_HEIGHT / 2.;
        let center = (self.line_top(line) * self.ratio())
            .floor()
            .clamp(half, (self.height - half).max(half));
        (center - half, center + half)
    }
}

/// Marks of one color and lane merged where they touch, as Monaco draws them.
pub fn merge_marks(marks: &[Mark], map: &RulerMap) -> Vec<(Lane, (f32, f32), Hsla)> {
    let mut sorted: Vec<(Lane, (f32, f32), Hsla)> = marks
        .iter()
        .map(|mark| (mark.lane, map.mark_extent(mark.lines), mark.color))
        .collect();
    let key = |color: &Hsla| [color.h, color.s, color.l, color.a];
    sorted.sort_by(|a, b| {
        key(&a.2)
            .iter()
            .zip(key(&b.2).iter())
            .map(|(x, y)| x.total_cmp(y))
            .find(|order| order.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
            .then(a.1.0.total_cmp(&b.1.0))
    });
    let mut merged: Vec<(Lane, (f32, f32), Hsla)> = Vec::new();
    for (lane, (y1, y2), color) in sorted {
        if let Some(last) = merged.last_mut()
            && last.0 == lane
            && last.2 == color
            && y1 <= last.1.1 + 1.
        {
            last.1.1 = last.1.1.max(y2);
            continue;
        }
        merged.push((lane, (y1, y2), color));
    }
    merged
}

/// The minimap's width (`EditorLayoutInfoComputer._computeMinimapLayout`, `size: 'fit'`,
/// blocks): one pixel per column while the document fits the minimap at that scale, half a
/// device pixel otherwise; never wider than 90 columns.
pub fn minimap_width(
    outer_width: f32,
    content_left: f32,
    char_width: f32,
    line_count: usize,
    map: &RulerMap,
    scale: f32,
) -> f32 {
    minimap_geometry(&MinimapInput {
        outer_width,
        outer_height: map.height,
        content_left,
        line_height: map.line_height,
        char_width,
        padding_top: map.padding_top,
        padding_bottom: map.padding_bottom,
        line_count,
        pixel_ratio: scale,
        scrollbar_width: RULER_WIDTH,
    })
    .width
}

/// The current line's boxes: the gutter part and, without a selection, the text part, as
/// rectangles relative to the editor (`renderLineHighlight: 'all'`).
pub fn line_boxes(
    line_top: f32,
    line_height: f32,
    content_left: f32,
    content_right: f32,
    selection_empty: bool,
) -> (Bounds<f32>, Option<Bounds<f32>>) {
    let margin = Bounds {
        origin: muxus_ui::gpui_kit::Point { x: 0., y: line_top },
        size: muxus_ui::gpui_kit::Size {
            width: content_left,
            height: line_height,
        },
    };
    let content = selection_empty.then(|| Bounds {
        origin: muxus_ui::gpui_kit::Point {
            x: content_left,
            y: line_top,
        },
        size: muxus_ui::gpui_kit::Size {
            width: (content_right - content_left).max(0.),
            height: line_height,
        },
    });
    (margin, content)
}

/// What the overlays need from the document besides the editor's own geometry.
#[derive(Clone, Default)]
pub struct OverlayData {
    pub marks: Vec<Mark>,
    pub cursor_line: usize,
    pub line_count: usize,
    pub selection_empty: bool,
    /// The line Go to Line previews (0-based), banded across the editor.
    pub previewed_line: Option<usize>,
    /// The highlighter's probe and the parse the problems come from: a parse made while the
    /// editor rendered asks for another frame, so the problems follow it.
    pub probe: Option<(SharedProbe, u64)>,
    /// Where the minimap's width is reported for the next frame (the text and the problem
    /// widget end where it starts).
    pub minimap_room: Option<Rc<Cell<f32>>>,
}

/// The editor paddings the workspace sets (top, bottom, left), and the text's font size.
#[derive(Clone, Copy, Debug)]
pub struct EditorFrame {
    pub padding_top: Pixels,
    pub padding_bottom: Pixels,
    pub padding_left: Pixels,
    pub font_size: Pixels,
}

/// The width of a digit of the editor's font (`typicalHalfwidthCharacterWidth`).
pub fn char_width(frame: &EditorFrame, window: &mut Window) -> f32 {
    muxus_ui::text::measure_text(
        window,
        "0",
        muxus_ui::theme::fonts::MONO,
        f32::from(frame.font_size) / muxus_ui::zoom::zoom(),
        400.,
    ) * muxus_ui::zoom::zoom()
}

/// Where the text starts: the paddings, the line numbers (three digits at least), the gap
/// after them and the fold chevrons' column, as the engine lays the gutter out.
pub fn content_left(frame: &EditorFrame, line_count: usize, window: &mut Window) -> f32 {
    let digits = (line_count.max(1).ilog10() as usize + 1).clamp(3, 7);
    let numbers = muxus_ui::text::measure_text(
        window,
        &"+".repeat(digits),
        muxus_ui::theme::fonts::MONO,
        f32::from(frame.font_size) / muxus_ui::zoom::zoom(),
        400.,
    );
    f32::from(frame.padding_left) + numbers * muxus_ui::zoom::zoom() + 6. + 18.
}

fn frame_rect(
    window: &mut Window,
    rect: Bounds<Pixels>,
    width: f32,
    color: Hsla,
    sides: [bool; 4],
) {
    let w = Pixels::from(width);
    let [top, right, bottom, left] = sides;
    let (o, s) = (rect.origin, rect.size);
    if top {
        window.paint_quad(fill(Bounds::new(o, size(s.width, w)), color));
    }
    if bottom {
        window.paint_quad(fill(
            Bounds::new(point(o.x, o.y + s.height - w), size(s.width, w)),
            color,
        ));
    }
    if left {
        window.paint_quad(fill(Bounds::new(o, size(w, s.height)), color));
    }
    if right {
        window.paint_quad(fill(
            Bounds::new(point(o.x + s.width - w, o.y), size(w, s.height)),
            color,
        ));
    }
}

fn to_pixels(bounds: Bounds<f32>, origin: muxus_ui::gpui_kit::Point<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(
            origin.x + Pixels::from(bounds.origin.x),
            origin.y + Pixels::from(bounds.origin.y),
        ),
        size(
            Pixels::from(bounds.size.width),
            Pixels::from(bounds.size.height),
        ),
    )
}

/// The cursor line's top relative to `bounds`, from the caret the editor just laid out.
fn cursor_line_top(
    editor: &Entity<EditorState>,
    bounds: Bounds<Pixels>,
    cx: &App,
) -> Option<(f32, f32)> {
    let geometry = editor.read(cx).prepaint_caret_geometry()?;
    let caret = geometry.cursor_bounds();
    let line_height = f32::from(geometry.line_height());
    let center = f32::from(caret.origin.y - bounds.origin.y) + f32::from(caret.size.height) / 2.;
    Some((center - line_height / 2., line_height))
}

/// The layer under the text: the current line's text box and the overview ruler. Paints
/// after the editor laid out (it reads the caret of this frame) and before it paints.
pub fn under_text_layer(
    editor: Entity<EditorState>,
    data: OverlayData,
    frame: EditorFrame,
    dark: bool,
) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, cx| {
            let colors = MonacoColors::new(dark);
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let scale = window.scale_factor();
            let line_height = editor.read(cx).line_height().map_or(18., f32::from);
            let map = RulerMap {
                height,
                line_height,
                padding_top: f32::from(frame.padding_top),
                padding_bottom: f32::from(frame.padding_bottom),
                line_count: data.line_count,
            };
            if let Some((probe, seen)) = &data.probe
                && probe.borrow().version() != *seen
            {
                window.refresh();
            }
            let left = content_left(&frame, data.line_count, window);
            let char_width = char_width(&frame, window);
            // The text area ends where the minimap's room starts.
            let minimap = minimap_width(width, left, char_width, data.line_count, &map, scale);
            let right = width - minimap;
            if let Some(room) = &data.minimap_room
                && room.get() != minimap
            {
                room.set(minimap);
                window.refresh();
            }
            if let (Some(line), Some(geometry)) = (
                data.previewed_line,
                editor.read(cx).prepaint_caret_geometry(),
            ) {
                // `editor.rangeHighlightBackground`, the whole line.
                let top = f32::from(frame.padding_top)
                    + line as f32 * line_height
                    + f32::from(geometry.scroll_offset().y);
                window.paint_quad(fill(
                    Bounds::new(
                        point(
                            bounds.origin.x + Pixels::from(left),
                            bounds.origin.y + Pixels::from(top),
                        ),
                        size(Pixels::from(right - left), Pixels::from(line_height)),
                    ),
                    colors.range_highlight,
                ));
            }
            if let Some((top, line_height)) = cursor_line_top(&editor, bounds, cx) {
                let (_, content) = line_boxes(top, line_height, left, right, data.selection_empty);
                if let Some(content) = content {
                    frame_rect(
                        window,
                        to_pixels(content, bounds.origin),
                        LINE_BOX_BORDER,
                        colors.line_highlight_border,
                        [true; 4],
                    );
                }
            }
            // The ruler: border, marks, then the cursor.
            let ruler_x = bounds.origin.x + Pixels::from(width - RULER_WIDTH);
            let border = Pixels::from(1. / scale);
            // Monaco strokes a one-pixel line along the canvas edge: half of it shows.
            let mut border_color = colors.overview_ruler_border;
            border_color.a *= 0.5;
            window.paint_quad(fill(
                Bounds::new(
                    point(ruler_x, bounds.origin.y),
                    size(border, bounds.size.height),
                ),
                border_color,
            ));
            window.paint_quad(fill(
                Bounds::new(
                    point(ruler_x, bounds.origin.y),
                    size(Pixels::from(RULER_WIDTH), border),
                ),
                border_color,
            ));
            let lane = |lane: Lane, (y1, y2): (f32, f32)| {
                let (x, w) = lane_extent(lane, RULER_WIDTH, scale);
                Bounds::new(
                    point(
                        ruler_x + Pixels::from(x),
                        bounds.origin.y + Pixels::from(y1),
                    ),
                    size(Pixels::from(w), Pixels::from(y2 - y1)),
                )
            };
            for (mark_lane, extent, color) in merge_marks(&data.marks, &map) {
                window.paint_quad(fill(lane(mark_lane, extent), color));
            }
            window.paint_quad(fill(
                lane(Lane::Full, map.cursor_extent(data.cursor_line)),
                colors.overview_ruler_cursor,
            ));
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// The layer over the gutter: the current line's gutter box (the gutter paints over anything
/// under it, so this part goes on top; it only meets the line numbers' blank edges).
pub fn gutter_layer(
    editor: Entity<EditorState>,
    data: OverlayData,
    frame: EditorFrame,
    dark: bool,
) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, cx| {
            let colors = MonacoColors::new(dark);
            let left = content_left(&frame, data.line_count, window);
            if let Some((top, line_height)) = cursor_line_top(&editor, bounds, cx) {
                let (margin, _) = line_boxes(top, line_height, left, left, data.selection_empty);
                frame_rect(
                    window,
                    to_pixels(margin, bounds.origin),
                    LINE_BOX_BORDER,
                    colors.line_highlight_border,
                    // Without a text box the gutter box closes on the right.
                    [true, !data.selection_empty, true, true],
                );
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// The lines (0-based) a byte range covers in a document with these line starts.
pub fn lines_of(starts: &[usize], range: &Range<usize>) -> (usize, usize) {
    let line = |offset: usize| {
        starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1)
    };
    let end = if range.end > range.start {
        range.end - 1
    } else {
        range.end
    };
    (line(range.start), line(end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(height: f32, lines: usize) -> RulerMap {
        RulerMap {
            height,
            line_height: 18.,
            padding_top: 12.,
            padding_bottom: 12.,
            line_count: lines,
        }
    }

    #[test]
    fn splits_the_ruler_into_two_lanes_after_its_border() {
        // 14px at 2x: one device pixel of border, then 13 and 14 device pixels.
        assert_eq!(lane_extent(Lane::Center, 14., 2.), (0.5, 6.5));
        assert_eq!(lane_extent(Lane::Right, 14., 2.), (7., 7.));
        assert_eq!(lane_extent(Lane::Full, 14., 2.), (0.5, 13.5));
    }

    #[test]
    fn maps_lines_at_their_place_while_the_document_fits() {
        // The reference: line 1 of 13 in a 760px editor sits 12px down, 18px tall.
        let fitting = map(760., 13);
        assert_eq!(fitting.mark_extent((0, 0)), (12., 30.));
        assert_eq!(fitting.cursor_extent(0), (11., 13.));
        // A long document is squeezed; a single line keeps six pixels.
        let long = map(600., 10_000);
        let (y1, y2) = long.mark_extent((5_000, 5_000));
        assert_eq!(y2 - y1, 6.);
        assert!((y1 - 300.).abs() < 4.);
        // Marks near the edges stay inside.
        assert_eq!(long.mark_extent((9_999, 9_999)).1, 600.);
    }

    #[test]
    fn merges_touching_marks_of_one_color() {
        let gray = muxus_ui::color::rgba8(160, 160, 160, 0.8);
        let fitting = map(760., 13);
        let marks = [
            Mark {
                lane: Lane::Center,
                lines: (0, 0),
                color: gray,
            },
            Mark {
                lane: Lane::Center,
                lines: (1, 1),
                color: gray,
            },
            Mark {
                lane: Lane::Center,
                lines: (5, 5),
                color: gray,
            },
        ];
        let merged = merge_marks(&marks, &fitting);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].1, (12., 48.));
    }

    #[test]
    fn keeps_the_minimaps_room_like_monaco() {
        // The reference editor: 790px wide, text from 83px, JetBrains Mono 13px (7.8px).
        let fitting = map(760., 13);
        assert_eq!(minimap_width(790., 83., 7.8, 13, &fitting, 2.), 86.);
        assert_eq!(minimap_width(790., 83., 7.8, 13, &fitting, 1.), 86.);
        // A document taller than the minimap gets half-pixel columns.
        let long = map(760., 5_000);
        assert_eq!(minimap_width(790., 83., 7.8, 5_000, &long, 2.), 45.);
        assert_eq!(minimap_width(400., 83., 7.8, 5_000, &long, 2.), 26.);
        // Never wider than 90 columns.
        assert_eq!(minimap_width(3000., 83., 7.8, 13, &fitting, 2.), 90.);
    }

    #[test]
    fn boxes_the_gutter_and_the_text_unless_text_is_selected() {
        let (margin, content) = line_boxes(12., 18., 83., 704., true);
        assert_eq!((margin.size.width, margin.origin.y), (83., 12.));
        let content = content.unwrap();
        assert_eq!((content.origin.x, content.size.width), (83., 621.));
        assert!(line_boxes(12., 18., 83., 704., false).1.is_none());
    }

    #[test]
    fn finds_the_lines_of_a_range() {
        let starts = [0, 10, 20];
        assert_eq!(lines_of(&starts, &(2..5)), (0, 0));
        assert_eq!(lines_of(&starts, &(5..10)), (0, 0));
        assert_eq!(lines_of(&starts, &(5..11)), (0, 1));
        assert_eq!(lines_of(&starts, &(25..25)), (2, 2));
    }
}
