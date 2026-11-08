//! Monaco's minimap as the client configures it (`minimap: { autohide: 'mouseover',
//! renderCharacters: false, showSlider: 'mouseover', size: 'fit', maxColumn: 90 }`, on the
//! right at scale 1): every line drawn as blocks in its tokens' colors, hidden until the
//! pointer is over it, then faded in with the slider that marks the visible part. A press
//! anywhere moves the slider's middle there and drags it; problems, occurrences and the
//! selection are marked on their lines.
//!
//! The layout is `EditorLayoutInfoComputer._computeMinimapLayout`, the slider
//! `MinimapLayout.create` and the drawing `InnerMinimap` (Monaco Editor 0.57, MIT). The lines
//! are the model's: with word wrap on, Monaco draws the wrapped rows.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use muxus_ui::gpui_kit::base::input::{EditorState, HighlightStyleResolver, Rope, RopeExt as _};
use muxus_ui::gpui_kit::{
    App, Bounds, DispatchPhase, Entity, Hitbox, HitboxBehavior, Hsla, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, RenderImage, Rgba, Styled,
    Window, canvas, div, fill, point, size,
};
use muxus_ui::syntax::{MonacoColors, MonacoTokens};

use super::markers::{Marker, Severity};
use crate::util::code_editor::SharedProbe;

/// `MINIMAP_GUTTER_WIDTH`: blank columns on the minimap's left.
const GUTTER_WIDTH: f32 = 8.;
/// `minimap.maxColumn`.
const MAX_COLUMN: f32 = 90.;
/// `renderCharacters: false`: blocks three device rows tall at scale 1.
const BLOCK_CHAR_HEIGHT: f32 = 3.;
/// The minimap fades in and out over half a second (`transition: opacity 0.5s`).
const FADE: Duration = Duration::from_millis(500);
/// `.minimap canvas { opacity: 0.9 }`.
const CANVAS_OPACITY: f32 = 0.9;

/// What the minimap's layout depends on (`EditorLayoutInfoComputer`'s input), in CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinimapInput {
    pub outer_width: f32,
    pub outer_height: f32,
    /// Where the text starts (gutter and line numbers before it).
    pub content_left: f32,
    pub line_height: f32,
    /// `typicalHalfwidthCharacterWidth`.
    pub char_width: f32,
    pub padding_top: f32,
    pub padding_bottom: f32,
    pub line_count: usize,
    pub pixel_ratio: f32,
    pub scrollbar_width: f32,
}

/// The minimap's geometry (`EditorLayoutInfo.minimap`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinimapGeometry {
    /// Its width in CSS pixels, and its left edge from the editor's.
    pub width: f32,
    pub left: f32,
    /// All lines squeezed into the editor's height (`fill`, or sampling).
    pub height_is_editor_height: bool,
    /// More lines than device rows: every few lines one is drawn.
    pub sampling: bool,
    /// Device pixels per block (`fontScale`) and per line.
    pub scale: f32,
    pub line_height: f32,
    /// The canvas in device pixels and in CSS pixels.
    pub canvas_inner_width: u32,
    pub canvas_inner_height: u32,
    pub canvas_outer_width: f32,
    pub canvas_outer_height: f32,
}

/// `computeContainedMinimapLineCount` (without scrolling beyond the last line):
/// (extra lines before, extra lines after, desired ratio, minimap line count).
fn contained_line_count(input: &MinimapInput) -> (f32, f32, f32, usize) {
    let before = (input.padding_top / input.line_height).floor();
    let after = (input.padding_bottom / input.line_height).floor();
    let desired =
        (before + input.line_count as f32 + after) / (input.pixel_ratio * input.outer_height);
    let count = (input.line_count as f32 / desired).floor().max(0.) as usize;
    (before, after, desired, count)
}

/// `_computeMinimapLayout` for `size: 'fit'`, blocks, scale 1, on the right, no wrapping.
pub fn minimap_geometry(input: &MinimapInput) -> MinimapGeometry {
    let pixel_ratio = input.pixel_ratio;
    let mut scale: f32 = if pixel_ratio >= 2. { 2. } else { 1. };
    let configured_scale = scale;
    let mut canvas_inner_height = (pixel_ratio * input.outer_height).floor();
    let canvas_outer_height = canvas_inner_height / pixel_ratio;
    let mut height_is_editor_height = false;
    let mut sampling = false;
    let mut line_height = BLOCK_CHAR_HEIGHT * scale;
    let mut char_width = scale / pixel_ratio;
    let mut width_multiplier = 1.;
    let (before, after, desired, contained) = contained_line_count(input);
    let lines = input.line_count as f32;
    if contained == 0 || lines / contained as f32 > 1. {
        height_is_editor_height = true;
        sampling = true;
        scale = 1.;
        line_height = 1.;
        char_width = scale / pixel_ratio;
    } else {
        let effective = ((before + lines + after) * line_height).ceil();
        if effective > canvas_inner_height {
            // `fit` becomes `fill`: the lines shrink to the editor's height.
            height_is_editor_height = true;
            line_height = (input.line_height * pixel_ratio).min((1. / desired).floor().max(1.));
            scale = (configured_scale + 1.).min((line_height / BLOCK_CHAR_HEIGHT).floor().max(1.));
            if scale > configured_scale {
                width_multiplier = (scale / configured_scale).min(2.);
            }
            char_width = scale / pixel_ratio / width_multiplier;
            let typical_viewport_lines = input.outer_height / input.line_height;
            canvas_inner_height =
                (typical_viewport_lines.max(before + lines + after) * line_height).ceil();
        }
    }
    let max_width = (MAX_COLUMN * char_width).floor();
    let remaining = input.outer_width - input.content_left;
    let fitted = ((remaining - input.scrollbar_width - 2.) * char_width
        / (input.char_width + char_width))
        .floor()
        .max(0.);
    let width = max_width.min(fitted + GUTTER_WIDTH);
    let inner_width = (pixel_ratio * width).floor();
    let canvas_outer_width = inner_width / pixel_ratio;
    MinimapGeometry {
        width,
        left: input.outer_width - width - input.scrollbar_width,
        height_is_editor_height,
        sampling,
        scale,
        line_height,
        canvas_inner_width: (inner_width * width_multiplier).floor() as u32,
        canvas_inner_height: canvas_inner_height.max(0.) as u32,
        canvas_outer_width,
        canvas_outer_height,
    }
}

/// `MinimapSampling.compute` for a fresh sampling: the line (0-based) each minimap row shows.
pub fn sampled_lines(input: &MinimapInput) -> Vec<usize> {
    let (_, _, _, count) = contained_line_count(input);
    let lines = input.line_count;
    if count == 0 || lines == 0 {
        return Vec::new();
    }
    let ratio = lines as f64 / count as f64;
    let half = ratio / 2.;
    let mut result = vec![0; count];
    if count > 1 {
        for (index, line) in result.iter_mut().enumerate().take(count - 1) {
            // Monaco's line numbers are 1-based.
            *line = ((index as f64 * ratio + half).round() as usize)
                .saturating_sub(1)
                .min(lines - 1);
        }
        result[count - 1] = lines - 1;
    }
    result
}

/// Where the slider sits (`MinimapLayout`), in CSS pixels from the minimap's top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SliderLayout {
    pub needed: bool,
    pub top: f32,
    pub height: f32,
    /// Slider pixels per scrolled pixel.
    pub ratio: f32,
    /// Blank minimap lines above the first.
    pub top_padding_lines: f32,
}

/// `MinimapLayout.create` for a minimap that shows every line (`fit` never scrolls it).
/// `line_count` is the number of minimap rows, `real_line_count` the document's lines.
#[allow(clippy::too_many_arguments)]
pub fn slider_layout(
    geometry: &MinimapGeometry,
    input: &MinimapInput,
    line_count: usize,
    real_line_count: usize,
    scroll_top: f32,
    scroll_height: f32,
    viewport_height: f32,
) -> SliderLayout {
    let pixel_ratio = input.pixel_ratio;
    let line_height = input.line_height;
    let minimap_height = input.outer_height;
    if geometry.height_is_editor_height {
        let logical =
            real_line_count as f32 * line_height + input.padding_top + input.padding_bottom;
        let height = (viewport_height * viewport_height / logical)
            .floor()
            .max(1.);
        let max_top = (minimap_height - height).max(0.);
        let ratio = max_top / (scroll_height - viewport_height);
        return SliderLayout {
            needed: max_top > 0.,
            top: scroll_top * ratio,
            height,
            ratio,
            top_padding_lines: (input.padding_top / line_height).floor(),
        };
    }
    let viewport_lines = viewport_height / line_height;
    let height = (viewport_lines * geometry.line_height / pixel_ratio).floor();
    let extra_top = (input.padding_top / line_height).floor();
    let extra_bottom = (input.padding_bottom / line_height).floor();
    let mut max_top = if extra_bottom > 0. {
        (extra_top + line_count as f32 + extra_bottom - viewport_lines - 1.) * geometry.line_height
            / pixel_ratio
    } else {
        ((extra_top + line_count as f32) * geometry.line_height / pixel_ratio - height).max(0.)
    };
    max_top = max_top.min(minimap_height - height);
    let ratio = max_top / (scroll_height - viewport_height);
    SliderLayout {
        needed: max_top > 0.,
        top: scroll_top * ratio,
        height,
        ratio,
        top_padding_lines: extra_top,
    }
}

/// The scroll position that puts the slider's middle where the slider was pressed, moved by
/// `delta` (`getDesiredScrollTopFromDelta`).
pub fn scroll_top_for_delta(start_scroll_top: f32, delta: f32, ratio: f32) -> f32 {
    if !ratio.is_finite() || ratio <= 0. {
        return start_scroll_top;
    }
    (start_scroll_top + delta / ratio).round()
}

/// `isFullWidthCharacter`.
fn full_width(c: char) -> bool {
    let code = c as u32;
    (0x2E80..=0xD7AF).contains(&code)
        || (0xF900..=0xFAFF).contains(&code)
        || (0xFF01..=0xFF5E).contains(&code)
}

/// An opaque color in device bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb8 {
    pub fn from_hsla(color: Hsla) -> Self {
        let Rgba { r, g, b, .. } = color.to_rgb();
        let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
        Self {
            r: byte(r),
            g: byte(g),
            b: byte(b),
        }
    }

    /// `from` moved `amount` of the way to `self`.
    fn over(self, from: Rgb8, amount: f32) -> Rgb8 {
        let mix = |a: u8, b: u8| (f32::from(b) + (f32::from(a) - f32::from(b)) * amount) as u8;
        Rgb8 {
            r: mix(self.r, from.r),
            g: mix(self.g, from.g),
            b: mix(self.b, from.b),
        }
    }
}

/// One line as the minimap draws it: its text and its colored runs (byte ranges in the
/// line, sorted).
pub struct MinimapLine<'a> {
    pub text: &'a str,
    pub runs: Vec<(Range<usize>, Rgb8)>,
}

/// The blocks of `lines` (one per minimap row) on `background`, as BGRA bytes of a canvas
/// `width` by `height` device pixels, with the canvas opacity applied over the background
/// (`InnerMinimap._renderLine`, `blockRenderChar`).
pub fn render_blocks(
    lines: &[MinimapLine],
    geometry: &MinimapGeometry,
    top_padding_lines: f32,
    foreground: Rgb8,
    background: Rgb8,
    tab_size: usize,
) -> Vec<u8> {
    let width = geometry.canvas_inner_width as usize;
    let height = geometry.canvas_inner_height as usize;
    let mut buffer = Vec::with_capacity(width * height * 4);
    for _ in 0..width * height {
        buffer.extend_from_slice(&[background.b, background.g, background.r, 255]);
    }
    let char_width = geometry.scale as usize;
    let line_height = geometry.line_height as usize;
    let block_height = if line_height == 1 {
        1
    } else {
        2 * geometry.scale as usize
    };
    let render_line_height = BLOCK_CHAR_HEIGHT as usize * geometry.scale as usize;
    let inner_padding = if line_height > render_line_height {
        (line_height - render_line_height) / 2
    } else {
        0
    };
    let tab_size = tab_size.max(1);
    let mut dy = top_padding_lines as usize * line_height;
    for line in lines {
        if dy + block_height > height {
            break;
        }
        let y = dy + inner_padding;
        let max_dx = width.saturating_sub(char_width);
        let mut dx = GUTTER_WIDTH as usize;
        let mut column = 0usize;
        let mut run = 0usize;
        'chars: for (offset, c) in line.text.char_indices() {
            if dx > max_dx {
                break;
            }
            match c {
                '\t' => {
                    let spaces = tab_size - column % tab_size;
                    column += spaces;
                    dx += spaces * char_width;
                }
                ' ' => {
                    column += 1;
                    dx += char_width;
                }
                '\r' | '\n' => break,
                _ => {
                    while run < line.runs.len() && line.runs[run].0.end <= offset {
                        run += 1;
                    }
                    let color = line
                        .runs
                        .get(run)
                        .filter(|(range, _)| range.start <= offset)
                        .map_or(foreground, |(_, color)| *color);
                    // Half way to the token color, then the canvas opacity.
                    let block = color.over(background, 0.5 * CANVAS_OPACITY);
                    let count = if full_width(c) { 2 } else { 1 };
                    for _ in 0..count {
                        if y + block_height <= height && dx + char_width <= width {
                            for row in y..y + block_height {
                                for x in dx..dx + char_width {
                                    let at = (row * width + x) * 4;
                                    buffer[at] = block.b;
                                    buffer[at + 1] = block.g;
                                    buffer[at + 2] = block.r;
                                }
                            }
                        }
                        column += 1;
                        dx += char_width;
                        if dx > max_dx {
                            break 'chars;
                        }
                    }
                }
            }
        }
        dy += line_height;
    }
    buffer
}

/// The device x of `column` (in characters) on `line`, as the decorations find it
/// (`getXOffsetForPosition`).
fn column_x(line: &str, column: usize, tab_size: usize, char_width: f32, canvas_width: f32) -> f32 {
    if column == 0 {
        return GUTTER_WIDTH;
    }
    if column as f32 * char_width >= canvas_width {
        return canvas_width;
    }
    let mut x = GUTTER_WIDTH;
    for (index, c) in line.chars().enumerate() {
        if index == column {
            return x;
        }
        x += if c == '\t' {
            tab_size as f32 * char_width
        } else if full_width(c) {
            2. * char_width
        } else {
            char_width
        };
        if x >= canvas_width {
            return canvas_width;
        }
    }
    if column <= line.chars().count() {
        x
    } else {
        canvas_width
    }
}

/// A rectangle the minimap marks, in device pixels of its canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinimapMark {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: Hsla,
}

/// What a decoration on the minimap covers: a byte range, its color, its stacking.
pub struct MinimapDecoration {
    pub range: Range<usize>,
    pub color: Hsla,
    pub z_index: u8,
}

/// The decorations' rectangles (`renderDecorations`): each decorated line tinted at half its
/// color (the topmost decoration claiming the line), then the decorated text in full. `rows`
/// maps each minimap row to its line.
pub fn decoration_marks(
    text: &Rope,
    decorations: &mut [MinimapDecoration],
    rows: &[usize],
    geometry: &MinimapGeometry,
    top_padding_lines: f32,
    tab_size: usize,
) -> Vec<MinimapMark> {
    decorations.sort_by_key(|decoration| decoration.z_index);
    let canvas_width = geometry.canvas_inner_width as f32;
    let char_width = geometry.scale;
    let line_height = geometry.line_height;
    let row_y = |row: usize| (row as f32 + top_padding_lines) * line_height;
    // The minimap rows a line range covers.
    let rows_of = |first: usize, last: usize| {
        let start = rows.partition_point(|&line| line < first);
        let end = rows.partition_point(|&line| line <= last);
        start..end
    };
    let mut marks = Vec::new();
    let mut claimed = vec![false; rows.len()];
    for decoration in decorations.iter().rev() {
        let first = text.offset_to_point(decoration.range.start).row;
        let last = text.offset_to_point(decoration.range.end).row;
        let mut half = decoration.color;
        half.a *= 0.5;
        for row in rows_of(first, last) {
            if std::mem::replace(&mut claimed[row], true) {
                continue;
            }
            marks.push(MinimapMark {
                x: GUTTER_WIDTH,
                y: row_y(row),
                width: canvas_width - GUTTER_WIDTH,
                height: line_height,
                color: half,
            });
        }
    }
    for decoration in decorations.iter() {
        let start = text.offset_to_point(decoration.range.start);
        let end = text.offset_to_point(decoration.range.end);
        let (first, last) = (start.row, end.row);
        // The column (in characters) of a byte column of `content`.
        let chars = |content: &str, bytes: usize| {
            let mut bytes = bytes.min(content.len());
            while !content.is_char_boundary(bytes) {
                bytes -= 1;
            }
            content[..bytes].chars().count()
        };
        for row in rows_of(first, last) {
            let line = rows[row];
            let content = text.slice_line(line).to_string();
            let content = content.trim_end_matches(['\n', '\r']);
            let start_column = if line == first {
                chars(content, start.column)
            } else {
                0
            };
            let end_column = if line == last {
                chars(content, end.column)
            } else {
                content.chars().count()
            };
            let x1 = column_x(content, start_column, tab_size, char_width, canvas_width);
            let x2 = column_x(content, end_column, tab_size, char_width, canvas_width);
            if x2 > x1 {
                marks.push(MinimapMark {
                    x: x1,
                    y: row_y(row),
                    width: x2 - x1,
                    height: line_height,
                    color: decoration.color,
                });
            }
        }
    }
    marks
}

/// The decorations Monaco puts on the minimap here: problems (errors over warnings), the
/// occurrence and selection highlights, the selection.
pub fn minimap_decorations(
    markers: &[Marker],
    occurrences: &[Range<usize>],
    selection: Range<usize>,
    dark: bool,
) -> Vec<MinimapDecoration> {
    let colors = MonacoColors::new(dark);
    let mut decorations: Vec<MinimapDecoration> = occurrences
        .iter()
        .map(|range| MinimapDecoration {
            range: range.clone(),
            color: colors.selection_highlight,
            z_index: 0,
        })
        .collect();
    decorations.extend(markers.iter().map(|marker| match marker.severity {
        // `minimap.errorHighlight`, `minimap.warningHighlight`.
        Severity::Error => MinimapDecoration {
            range: marker.range.clone(),
            color: colors.overview_ruler_error,
            z_index: 30,
        },
        Severity::Warning => MinimapDecoration {
            range: marker.range.clone(),
            color: colors.warning,
            z_index: 20,
        },
    }));
    if !selection.is_empty() {
        // `minimap.selectionHighlight` is the selection's color; drawn last.
        decorations.push(MinimapDecoration {
            range: selection,
            color: colors.selection,
            z_index: 40,
        });
    }
    decorations
}

/// The minimap's state between frames: whether the pointer is over it (it fades with that),
/// a drag of its slider, and the drawn lines.
#[derive(Default)]
pub struct MinimapState {
    hovered: bool,
    slider_hovered: bool,
    /// When `hovered` last changed.
    changed: Option<Instant>,
    drag: Option<SliderDrag>,
    image: Option<(ImageKey, Arc<RenderImage>)>,
    retired: Vec<Arc<RenderImage>>,
}

#[derive(Clone, Copy, Debug)]
struct SliderDrag {
    start_y: f32,
    start_scroll_top: f32,
    ratio: f32,
}

/// What the drawn lines depend on.
#[derive(Clone, Debug, PartialEq)]
struct ImageKey {
    revision: u64,
    probe_version: u64,
    dark: bool,
    width: u32,
    height: u32,
    line_height: u32,
    sampling: bool,
    line_count: usize,
    tab_size: usize,
}

/// A shared handle on one editor's minimap state.
pub type SharedMinimap = Rc<RefCell<MinimapState>>;

impl MinimapState {
    /// How visible the minimap is now (0 hidden, 1 shown), and whether it is still fading.
    fn opacity(&self) -> (f32, bool) {
        let shown = self.hovered || self.drag.is_some();
        let Some(changed) = self.changed else {
            return (if shown { 1. } else { 0. }, false);
        };
        let t = (changed.elapsed().as_secs_f32() / FADE.as_secs_f32()).min(1.);
        let opacity = if shown { t } else { 1. - t };
        (opacity, t < 1.)
    }

    fn set_hovered(&mut self, hovered: bool) {
        if self.hovered != hovered {
            // Reverse a fade in progress from where it is.
            let (opacity, _) = self.opacity();
            self.hovered = hovered;
            let shown = hovered || self.drag.is_some();
            let progress = if shown { opacity } else { 1. - opacity };
            self.changed = Some(Instant::now() - FADE.mul_f32(progress));
        }
    }

    /// Images to drop from the sprite atlas.
    pub fn take_retired(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.retired)
    }
}

/// What the minimap draws from the document.
pub struct MinimapData {
    pub probe: SharedProbe,
    /// Moves with every change of the text.
    pub revision: u64,
    pub grammar: String,
    pub markers: Vec<Marker>,
    pub occurrences: Vec<Range<usize>>,
    pub tab_size: usize,
}

/// The editor frame the minimap lays out in.
#[derive(Clone, Copy, Debug)]
pub struct MinimapFrame {
    pub padding_top: f32,
    pub padding_bottom: f32,
    pub content_left: f32,
    pub char_width: f32,
    pub scrollbar_width: f32,
}

/// The minimap over the editor's right side, before the overview ruler.
pub fn minimap_layer(
    editor: Entity<EditorState>,
    state: SharedMinimap,
    data: Rc<MinimapData>,
    frame: MinimapFrame,
    dark: bool,
) -> impl IntoElement {
    let prepaint_editor = editor.clone();
    div().absolute().top_0().left_0().size_full().child(
        canvas(
            move |bounds, window, cx| {
                let pixel_ratio = window.scale_factor();
                let layout = layouts(prepaint_editor.read(cx), frame, bounds, pixel_ratio);
                let area = Bounds::new(
                    point(
                        bounds.origin.x + Pixels::from(layout.1.left),
                        bounds.origin.y,
                    ),
                    size(Pixels::from(layout.1.width), bounds.size.height),
                );
                // The minimap takes the pointer over its area, shown or not.
                let hitbox = window.insert_hitbox(area, HitboxBehavior::BlockMouse);
                (layout, area, hitbox)
            },
            move |_, (layout, area, hitbox), window, cx| {
                paint_minimap(
                    &editor, &state, &data, dark, layout, area, hitbox, window, cx,
                );
            },
        )
        .size_full(),
    )
}

type Layouts = (MinimapInput, MinimapGeometry, Vec<usize>, SliderLayout, f32);

/// The layout for the editor's state now.
fn layouts(
    editor: &EditorState,
    frame: MinimapFrame,
    bounds: Bounds<Pixels>,
    pixel_ratio: f32,
) -> Layouts {
    let line_height = editor.line_height().map_or(18., f32::from);
    let line_count = editor.text().lines_len().max(1);
    let input = MinimapInput {
        outer_width: f32::from(bounds.size.width),
        outer_height: f32::from(bounds.size.height),
        content_left: frame.content_left,
        line_height,
        char_width: frame.char_width,
        padding_top: frame.padding_top,
        padding_bottom: frame.padding_bottom,
        line_count,
        pixel_ratio,
        scrollbar_width: frame.scrollbar_width,
    };
    let geometry = minimap_geometry(&input);
    let rows: Vec<usize> = if geometry.sampling {
        sampled_lines(&input)
    } else {
        (0..line_count).collect()
    };
    let viewport = input.outer_height;
    let content = frame.padding_top + line_count as f32 * line_height + frame.padding_bottom;
    let scroll_height = content.max(viewport);
    let scroll_top = -f32::from(editor.scroll_offset().y);
    let slider = slider_layout(
        &geometry,
        &input,
        rows.len(),
        line_count,
        scroll_top,
        scroll_height,
        viewport,
    );
    (input, geometry, rows, slider, scroll_height - viewport)
}

#[allow(clippy::too_many_arguments)]
fn paint_minimap(
    editor: &Entity<EditorState>,
    state: &SharedMinimap,
    data: &Rc<MinimapData>,
    dark: bool,
    (_, geometry, rows, slider, max_scroll): Layouts,
    area: Bounds<Pixels>,
    hitbox: Hitbox,
    window: &mut Window,
    cx: &mut App,
) {
    let pixel_ratio = window.scale_factor();
    if geometry.width <= GUTTER_WIDTH {
        return;
    }
    let colors = MonacoColors::new(dark);
    // The pointer: over the minimap it shows, over the slider the slider lightens, a press
    // drags the slider from its middle.
    {
        let state = state.clone();
        let editor = editor.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let over = area.contains(&event.position);
            let mut minimap = state.borrow_mut();
            let slider_area = Bounds::new(
                point(area.origin.x, area.origin.y + Pixels::from(slider.top)),
                size(area.size.width, Pixels::from(slider.height)),
            );
            let over_slider = slider.needed && slider_area.contains(&event.position);
            let changed = minimap.hovered != over || minimap.slider_hovered != over_slider;
            minimap.set_hovered(over);
            minimap.slider_hovered = over_slider;
            if let Some(drag) = minimap.drag
                && event.pressed_button == Some(MouseButton::Left)
            {
                let top = scroll_top_for_delta(
                    drag.start_scroll_top,
                    f32::from(event.position.y) - drag.start_y,
                    drag.ratio,
                )
                .clamp(0., max_scroll.max(0.));
                drop(minimap);
                editor.update(cx, |editor, cx| {
                    let x = editor.scroll_offset().x;
                    editor.set_scroll_offset(point(x, Pixels::from(-top)), cx);
                });
                window.refresh();
            } else if changed {
                window.refresh();
            }
        });
    }
    {
        let state = state.clone();
        let editor = editor.clone();
        let hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }
            cx.stop_propagation();
            if !slider.needed {
                return;
            }
            // `size !== 'proportional'`: as if the slider's middle had been pressed.
            let start_y = f32::from(area.origin.y) + slider.top + slider.height / 2.;
            let start_scroll_top = -f32::from(editor.read(cx).scroll_offset().y);
            state.borrow_mut().drag = Some(SliderDrag {
                start_y,
                start_scroll_top,
                ratio: slider.ratio,
            });
            let y = f32::from(event.position.y);
            if y != start_y {
                let top = scroll_top_for_delta(start_scroll_top, y - start_y, slider.ratio)
                    .clamp(0., max_scroll.max(0.));
                editor.update(cx, |editor, cx| {
                    let x = editor.scroll_offset().x;
                    editor.set_scroll_offset(point(x, Pixels::from(-top)), cx);
                });
            }
            window.refresh();
        });
    }
    {
        let state = state.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, _| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            let mut minimap = state.borrow_mut();
            if minimap.drag.take().is_some() {
                let over = area.contains(&event.position);
                minimap.hovered = !over;
                minimap.set_hovered(over);
                window.refresh();
            }
        });
    }
    let (opacity, fading) = state.borrow().opacity();
    if fading {
        window.request_animation_frame();
    }
    if opacity <= 0. {
        return;
    }
    // The lines, drawn again when the text, its colors or the layout changed.
    let key = ImageKey {
        revision: data.revision,
        probe_version: data.probe.borrow().version(),
        dark,
        width: geometry.canvas_inner_width,
        height: geometry.canvas_inner_height,
        line_height: geometry.line_height as u32,
        sampling: geometry.sampling,
        line_count: rows.len(),
        tab_size: data.tab_size,
    };
    let cached = state
        .borrow()
        .image
        .as_ref()
        .filter(|(drawn, _)| *drawn == key)
        .map(|(_, image)| image.clone());
    let image = match cached {
        Some(image) => image,
        None => {
            let Some(image) = draw_lines(
                editor.read(cx).text(),
                data,
                &rows,
                &geometry,
                &slider,
                dark,
            ) else {
                return;
            };
            let mut minimap = state.borrow_mut();
            if let Some((_, old)) = minimap.image.replace((key, image.clone())) {
                minimap.retired.push(old);
            }
            image
        }
    };
    for old in state.borrow_mut().take_retired() {
        let _ = window.drop_image(old);
    }
    let canvas_bounds = Bounds::new(
        area.origin,
        size(
            Pixels::from(geometry.canvas_outer_width),
            Pixels::from(geometry.canvas_outer_height),
        ),
    );
    let _ = window.paint_image(area, canvas_bounds, Default::default(), image, 0, false);
    // The decorations, over the lines (and under the same canvas opacity).
    let state_ref = editor.read(cx);
    let mut decorations = minimap_decorations(
        &data.markers,
        &data.occurrences,
        state_ref.selected_range(),
        dark,
    );
    let marks = decoration_marks(
        state_ref.text(),
        &mut decorations,
        &rows,
        &geometry,
        slider.top_padding_lines,
        data.tab_size,
    );
    let to_css = |device: f32| Pixels::from(device / pixel_ratio);
    let canvas_scale_y = geometry.canvas_outer_height / geometry.canvas_inner_height.max(1) as f32;
    for mark in marks {
        let mut color = mark.color;
        color.a *= CANVAS_OPACITY;
        window.paint_quad(fill(
            Bounds::new(
                point(
                    area.origin.x + to_css(mark.x),
                    area.origin.y + Pixels::from(mark.y * canvas_scale_y),
                ),
                size(
                    to_css(mark.width),
                    Pixels::from(mark.height * canvas_scale_y),
                ),
            ),
            color,
        ));
    }
    // The slider, while the pointer is over the minimap or drags it.
    if slider.needed {
        let minimap = state.borrow();
        let color = if minimap.drag.is_some() {
            colors.minimap_slider_active
        } else if minimap.slider_hovered {
            colors.minimap_slider_hover
        } else {
            colors.minimap_slider
        };
        window.paint_quad(fill(
            Bounds::new(
                point(area.origin.x, area.origin.y + Pixels::from(slider.top)),
                size(area.size.width, Pixels::from(slider.height)),
            ),
            color,
        ));
    }
    // Fading: the editor's background over everything at the remaining transparency.
    if opacity < 1. {
        let mut veil = colors.background;
        veil.a = 1. - opacity;
        window.paint_quad(fill(area, veil));
    }
}

/// Draw the lines of the rows into an image.
fn draw_lines(
    text: &Rope,
    data: &MinimapData,
    rows: &[usize],
    geometry: &MinimapGeometry,
    slider: &SliderLayout,
    dark: bool,
) -> Option<Arc<RenderImage>> {
    if geometry.canvas_inner_width == 0 || geometry.canvas_inner_height == 0 {
        return None;
    }
    let colors = MonacoColors::new(dark);
    let resolver = MonacoTokens::new(dark, data.grammar.clone());
    let probe = data.probe.borrow();
    let contents: Vec<(usize, String)> = rows
        .iter()
        .map(|&line| {
            let start = text.line_start_offset(line);
            let content = text.slice_line(line).to_string();
            (start, content)
        })
        .collect();
    let lines: Vec<MinimapLine> = contents
        .iter()
        .map(|(start, content)| {
            let range = *start..*start + content.len();
            let runs = probe
                .styles(&range, resolver.as_ref() as &dyn HighlightStyleResolver)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(run, style)| {
                    let color = style.color?;
                    Some((
                        run.start.saturating_sub(*start)..run.end.saturating_sub(*start),
                        Rgb8::from_hsla(color),
                    ))
                })
                .collect();
            MinimapLine {
                text: content,
                runs,
            }
        })
        .collect();
    let bytes = render_blocks(
        &lines,
        geometry,
        slider.top_padding_lines,
        Rgb8::from_hsla(colors.foreground),
        Rgb8::from_hsla(colors.background),
        data.tab_size,
    );
    let buffer = image::RgbaImage::from_raw(
        geometry.canvas_inner_width,
        geometry.canvas_inner_height,
        bytes,
    )?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(width: f32, height: f32, lines: usize, ratio: f32) -> MinimapInput {
        MinimapInput {
            outer_width: width,
            outer_height: height,
            content_left: 83.,
            line_height: 18.,
            char_width: 7.8,
            padding_top: 12.,
            padding_bottom: 12.,
            line_count: lines,
            pixel_ratio: ratio,
            scrollbar_width: 14.,
        }
    }

    #[test]
    fn lays_out_like_monaco() {
        // The reference editor: 13 lines fit at scale 2, one CSS pixel per column.
        let fitting = minimap_geometry(&input(790., 760., 13, 2.));
        assert_eq!(fitting.width, 86.);
        assert_eq!(fitting.left, 790. - 86. - 14.);
        assert!(!fitting.height_is_editor_height && !fitting.sampling);
        assert_eq!((fitting.scale, fitting.line_height), (2., 6.));
        assert_eq!(fitting.canvas_inner_width, 172);
        assert_eq!(fitting.canvas_inner_height, 1520);
        // 400 lines do not fit at six device rows each: `fill` at three.
        let fill = minimap_geometry(&input(790., 760., 400, 2.));
        assert!(fill.height_is_editor_height && !fill.sampling);
        assert_eq!((fill.scale, fill.line_height), (1., 3.));
        assert_eq!(fill.canvas_inner_height, 1200);
        // More lines than device rows: one row per sampled line.
        let sampled = minimap_geometry(&input(790., 760., 5_000, 2.));
        assert!(sampled.sampling);
        assert_eq!((sampled.scale, sampled.line_height), (1., 1.));
        assert_eq!(sampled.width, 45.);
        // At 1x, scale 1.
        let low = minimap_geometry(&input(790., 760., 13, 1.));
        assert_eq!((low.scale, low.line_height, low.width), (1., 3., 86.));
    }

    #[test]
    fn samples_lines_evenly_ending_with_the_last() {
        let lines = sampled_lines(&input(790., 100., 1_000, 1.));
        // 100 device rows (no extra lines at 18px with 12px paddings).
        assert_eq!(lines.len(), 100);
        assert_eq!(lines[0], 4);
        assert_eq!(lines[1], 14);
        assert_eq!(*lines.last().unwrap(), 999);
    }

    #[test]
    fn maps_the_slider_to_the_scroll_position() {
        let input = input(790., 760., 100, 2.);
        let geometry = minimap_geometry(&input);
        // 100 lines at 6 device rows fit: slider of 42 lines at 3 CSS px each.
        assert!(!geometry.height_is_editor_height);
        let scroll_height = 12. + 100. * 18. + 12.;
        let top = slider_layout(&geometry, &input, 100, 100, 0., scroll_height, 760.);
        assert!(top.needed);
        assert_eq!(top.height, 126.);
        assert_eq!(top.top, 0.);
        let bottom = slider_layout(
            &geometry,
            &input,
            100,
            100,
            scroll_height - 760.,
            scroll_height,
            760.,
        );
        // At the bottom the slider ends with the last line: 300 - 126.
        assert!((bottom.top - 174.).abs() < 0.01, "{bottom:?}");
        // A short document needs no slider.
        let short = slider_layout(&geometry, &input, 10, 10, 0., 760., 760.);
        assert!(!short.needed);
        // Squeezed to the editor's height, the slider's height is the visible share.
        let long_input = MinimapInput {
            line_count: 5_000,
            ..input
        };
        let long = minimap_geometry(&long_input);
        let long_height = 24. + 5_000. * 18.;
        let slider = slider_layout(&long, &long_input, 1520, 5_000, 0., long_height, 760.);
        assert_eq!(slider.height, 6.);
        assert!(slider.needed);
        // Dragging by the slider's travel scrolls the whole way.
        let travel = 760. - slider.height;
        assert_eq!(
            scroll_top_for_delta(0., travel, slider.ratio),
            long_height - 760.
        );
    }

    #[test]
    fn draws_blocks_in_token_colors_after_the_gutter() {
        let geometry = MinimapGeometry {
            width: 20.,
            left: 0.,
            height_is_editor_height: false,
            sampling: false,
            scale: 1.,
            line_height: 3.,
            canvas_inner_width: 20,
            canvas_inner_height: 6,
            canvas_outer_width: 20.,
            canvas_outer_height: 6.,
        };
        let red = Rgb8 { r: 255, g: 0, b: 0 };
        let white = Rgb8 {
            r: 255,
            g: 255,
            b: 255,
        };
        let black = Rgb8 { r: 0, g: 0, b: 0 };
        let lines = [
            MinimapLine {
                text: "a b",
                runs: vec![(2..3, red)],
            },
            MinimapLine {
                text: "\tc",
                runs: Vec::new(),
            },
        ];
        let bytes = render_blocks(&lines, &geometry, 0., black, white, 4);
        let pixel = |x: usize, y: usize| {
            let at = (y * 20 + x) * 4;
            (bytes[at + 2], bytes[at + 1], bytes[at])
        };
        // Columns start after the eight-pixel gutter; the space is not drawn.
        assert_eq!(pixel(7, 0), (255, 255, 255));
        assert_eq!(pixel(8, 0), (140, 140, 140));
        assert_eq!(pixel(9, 0), (255, 255, 255));
        // `b` is red half way, under the canvas opacity.
        assert_eq!(pixel(10, 0), (255, 140, 140));
        // Blocks are two rows of the three.
        assert_eq!(pixel(8, 1), (140, 140, 140));
        assert_eq!(pixel(8, 2), (255, 255, 255));
        // A tab is four columns.
        assert_eq!(pixel(12, 3), (140, 140, 140));
        assert_eq!(pixel(11, 3), (255, 255, 255));
    }

    #[test]
    fn marks_decorated_lines_and_text() {
        let text = Rope::from("abc\n\tdef\nghi");
        let geometry = MinimapGeometry {
            width: 40.,
            left: 0.,
            height_is_editor_height: false,
            sampling: false,
            scale: 2.,
            line_height: 6.,
            canvas_inner_width: 80,
            canvas_inner_height: 60,
            canvas_outer_width: 40.,
            canvas_outer_height: 30.,
        };
        let red = muxus_ui::color::rgba8(255, 0, 0, 1.);
        let mut decorations = vec![MinimapDecoration {
            range: 5..7,
            color: red,
            z_index: 30,
        }];
        let marks = decoration_marks(&text, &mut decorations, &[0, 1, 2], &geometry, 0., 4);
        assert_eq!(marks.len(), 2);
        // The line at half, across the canvas after the gutter.
        assert_eq!((marks[0].x, marks[0].y, marks[0].width), (8., 6., 72.));
        assert_eq!(marks[0].color.a, 0.5);
        // `de` after the tab (four columns of two pixels).
        assert_eq!((marks[1].x, marks[1].width), (16., 4.));
    }
}
