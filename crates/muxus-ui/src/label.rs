//! A single-line text element with CSS `letter-spacing` and
//! `text-overflow: ellipsis`.
//!
//! GPUI's text layout has no tracking, and the Muxus chrome sets most of its
//! labels with -0.1px (sidebar rows, tabs, status bar) or -0.2px (h5/h6). Over
//! a 20-character label that is 2–4px of width, enough to move every
//! ellipsis and count badge. This element shapes the line with GPUI's text
//! system and paints the glyphs itself, adding the tracking after every
//! character as Chrome does (the trailing one included), with optional
//! ligatures off while tracking is non-zero, as CSS requires.

use std::ops::Range;
use std::sync::Arc;

use crate::zoom::{css, layout_px, px};
use gpui_kit::{
    App, Bounds, Element, ElementId, Font, FontFeatures, FontStyle, FontWeight, GlobalElementId,
    Hsla, InspectorElementId, IntoElement, LayoutId, LineLayout, Pixels, SharedString, Size, Style,
    TextRun, Window, point, relative,
};

use crate::theme::fonts;

/// `label(text)`: a one-line label in the UI font.
pub fn label(text: impl Into<SharedString>) -> Label {
    Label {
        text: text.into(),
        family: fonts::UI.into(),
        size: 13.,
        weight: 400.,
        line_height: None,
        tracking: 0.,
        color: Hsla::black(),
        truncate: true,
        grow: false,
        tabular: false,
        nudge: 0.,
        strut: None,
    }
}

pub struct Label {
    text: SharedString,
    family: SharedString,
    size: f32,
    weight: f32,
    line_height: Option<f32>,
    tracking: f32,
    color: Hsla,
    truncate: bool,
    grow: bool,
    tabular: bool,
    nudge: f32,
    strut: Option<f32>,
}

impl Label {
    /// Lay the line out as inline text inside a block whose font is `size`
    /// px (same family): Chrome's line box then has that font's strut, so the
    /// baseline sits where the block's own text would put it (a caption
    /// `span` under an h6 `DialogTitle`). Set the line height to the block's.
    pub fn strut(mut self, size: f32) -> Self {
        self.strut = Some(size);
        self
    }

    /// Move the baseline by `dy` px, for places where Chrome's rasterization
    /// lands text off its layout position (see docs/parity/ui-shell.md).
    pub fn nudge(mut self, dy: f32) -> Self {
        self.nudge = dy;
        self
    }

    /// Size, weight, line height and tracking of a theme typography variant.
    pub fn variant(mut self, variant: crate::theme::TextVariant) -> Self {
        self.size = variant.size;
        self.weight = variant.weight;
        self.line_height = Some(variant.line_height);
        self.tracking = variant.letter_spacing;
        self
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    /// Line height in px; defaults to 1.5 × size (the body line height labels inherit).
    pub fn line_height(mut self, line_height: f32) -> Self {
        self.line_height = Some(line_height);
        self
    }

    /// CSS `letter-spacing` in px.
    pub fn tracking(mut self, tracking: f32) -> Self {
        self.tracking = tracking;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = color;
        self
    }

    pub fn family(mut self, family: impl Into<SharedString>) -> Self {
        self.family = family.into();
        self
    }

    /// Draw the whole line even when it overflows (no ellipsis).
    pub fn no_truncate(mut self) -> Self {
        self.truncate = false;
        self
    }

    /// Take the free space of a flex row (`flex: 1`).
    pub fn grow(mut self) -> Self {
        self.grow = true;
        self
    }

    /// `font-variant-numeric: tabular-nums`.
    pub fn tabular_nums(mut self) -> Self {
        self.tabular = true;
        self
    }

    fn font(&self) -> Font {
        let mut features: Vec<(String, u32)> = Vec::new();
        if self.tracking != 0. {
            for tag in ["liga", "clig", "dlig", "hlig"] {
                features.push((tag.into(), 0));
            }
        }
        if self.tabular {
            features.push(("tnum".into(), 1));
        }
        Font {
            family: self.family.clone(),
            features: FontFeatures(Arc::new(features)),
            fallbacks: None,
            weight: FontWeight(self.weight),
            style: FontStyle::Normal,
        }
    }

    fn line_height_px(&self) -> f32 {
        self.line_height.unwrap_or(self.size * 1.5)
    }
}

/// The shaped line, with where each character starts (the label's paint state).
pub struct Shaped {
    layout: Arc<LineLayout>,
    /// Byte offset of every character boundary (including the end).
    boundaries: Vec<usize>,
    ellipsis: Arc<LineLayout>,
}

fn shape(text: &str, font: &Font, size: f32, color: Hsla, window: &Window) -> Arc<LineLayout> {
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .layout_line(text, px(size), &[run], None)
}

impl Shaped {
    /// The pen position where the character starting at `byte` begins.
    fn x_at(&self, byte: usize) -> f32 {
        if byte >= self.layout.len {
            return f32::from(self.layout.width);
        }
        self.layout
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter())
            .filter(|glyph| glyph.index >= byte)
            .map(|glyph| f32::from(glyph.position.x))
            .fold(f32::INFINITY, f32::min)
            .min(f32::from(self.layout.width))
    }

    fn char_count_before(&self, byte: usize) -> usize {
        self.boundaries.iter().take_while(|&&b| b < byte).count()
    }

    /// Width of the first `chars` characters with tracking after each one.
    fn tracked_width(&self, chars: usize, tracking: f32) -> f32 {
        let byte = self
            .boundaries
            .get(chars)
            .copied()
            .unwrap_or(self.layout.len);
        self.x_at(byte) + tracking * chars as f32
    }
}

impl IntoElement for Label {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Label {
    type RequestLayoutState = ();
    type PrepaintState = Option<(Shaped, Range<usize>, bool)>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style {
            flex_shrink: 1.,
            ..Default::default()
        };
        style.min_size.width = px(0.).into();
        if self.grow {
            style.flex_grow = 1.;
            style.flex_basis = relative(0.).into();
        }
        let text = self.text.clone();
        let font = self.font();
        let size = self.size;
        // The shaped line is in layout space already; the CSS lengths join it scaled.
        let tracking = css(self.tracking);
        let height = css(self.line_height_px());
        let color = self.color;
        let layout_id = window.request_measured_layout(style, move |_, _, window, _| {
            let layout = shape(&text, &font, size, color, window);
            let chars = text.chars().count() as f32;
            Size {
                width: layout_px(f32::from(layout.width) + tracking * chars),
                height: layout_px(height),
            }
        });
        let _ = cx;
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        let font = self.font();
        let layout = shape(&self.text, &font, self.size, self.color, window);
        let mut boundaries: Vec<usize> = self.text.char_indices().map(|(i, _)| i).collect();
        boundaries.push(self.text.len());
        let ellipsis = shape("…", &font, self.size, self.color, window);
        let shaped = Shaped {
            layout,
            boundaries,
            ellipsis,
        };
        let total_chars = shaped.boundaries.len() - 1;
        let available = f32::from(bounds.size.width);
        let tracking = css(self.tracking);
        let full = shaped.tracked_width(total_chars, tracking);
        if !self.truncate || full <= available + 0.01 {
            return Some((shaped, 0..self.text.len(), false));
        }
        // Keep as many characters as fit in front of the ellipsis.
        let ellipsis_width = f32::from(shaped.ellipsis.width) + tracking;
        let mut keep = 0;
        for chars in (0..total_chars).rev() {
            if shaped.tracked_width(chars, tracking) + ellipsis_width <= available {
                keep = chars;
                break;
            }
        }
        let end = shaped.boundaries[keep];
        Some((shaped, 0..end, true))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let Some((shaped, range, ellipsized)) = prepaint.take() else {
            return;
        };
        // Everything below is in layout space (scaled with the interface).
        let line_height = css(self.line_height_px());
        let tracking = css(self.tracking);
        let layout = &shaped.layout;
        // Chrome's line layout: ascent and descent rounded to whole CSS pixels,
        // the baseline a whole number of pixels below the line box top.
        let scale = self.strut.map_or(1., |strut| strut / self.size);
        let ascent = (f32::from(layout.ascent) * scale).round();
        let descent = (f32::from(layout.descent) * scale).round();
        let baseline = f32::from(bounds.origin.y)
            + (ascent + (line_height - ascent - descent) / 2.).floor()
            + css(self.nudge);
        let left = f32::from(bounds.origin.x);
        let font_size = px(self.size);
        // Clip horizontally only, as `overflow: hidden` on a line box would:
        // ascenders and descenders may reach past a tight line height.
        let mask = Bounds {
            origin: point(bounds.origin.x, bounds.origin.y - layout_px(line_height)),
            size: Size {
                width: bounds.size.width,
                height: layout_px(line_height * 3.),
            },
        };
        window.with_content_mask(Some(gpui_kit::ContentMask { bounds: mask }), |window| {
            for run in &layout.runs {
                for glyph in &run.glyphs {
                    if !range.contains(&glyph.index) {
                        continue;
                    }
                    let chars = shaped.char_count_before(glyph.index) as f32;
                    let origin = point(
                        layout_px(left + f32::from(glyph.position.x) + tracking * chars),
                        layout_px(baseline),
                    );
                    let result = if glyph.is_emoji {
                        window.paint_emoji(origin, run.font_id, glyph.id, font_size)
                    } else {
                        window.paint_glyph(origin, run.font_id, glyph.id, font_size, self.color)
                    };
                    let _ = result;
                }
            }
            if ellipsized {
                let x = left + shaped.tracked_width(shaped.char_count_before(range.end), tracking);
                for run in &shaped.ellipsis.runs {
                    for glyph in &run.glyphs {
                        let origin = point(
                            layout_px(x + f32::from(glyph.position.x)),
                            layout_px(baseline),
                        );
                        let _ = window.paint_glyph(
                            origin,
                            run.font_id,
                            glyph.id,
                            font_size,
                            self.color,
                        );
                    }
                }
            }
        });
    }
}

/// `paragraph(text)`: wrapping text set on Chrome's line model, for body
/// copy (the empty pane's description, card captions, empty states). Lines
/// break where GPUI's wrapper breaks them; each line's baseline is placed the
/// way Chrome places it, so wrapped text lines up with [`Label`]s beside it.
pub fn paragraph(text: impl Into<SharedString>) -> Paragraph {
    Paragraph {
        text: text.into(),
        family: fonts::UI.into(),
        size: 13.,
        weight: 400.,
        line_height: None,
        color: Hsla::black(),
        center: false,
    }
}

pub struct Paragraph {
    text: SharedString,
    family: SharedString,
    size: f32,
    weight: f32,
    line_height: Option<f32>,
    color: Hsla,
    center: bool,
}

impl Paragraph {
    /// Size, weight and line height of a theme typography variant.
    pub fn variant(mut self, variant: crate::theme::TextVariant) -> Self {
        self.size = variant.size;
        self.weight = variant.weight;
        self.line_height = Some(variant.line_height);
        self
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    pub fn line_height(mut self, line_height: f32) -> Self {
        self.line_height = Some(line_height);
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = color;
        self
    }

    /// `text-align: center`.
    pub fn center(mut self) -> Self {
        self.center = true;
        self
    }

    fn font(&self) -> Font {
        Font {
            family: self.family.clone(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight(self.weight),
            style: FontStyle::Normal,
        }
    }

    fn line_height_px(&self) -> f32 {
        self.line_height.unwrap_or(self.size * 1.5)
    }
}

fn wrap_text(
    text: &SharedString,
    font: &Font,
    size: f32,
    color: Hsla,
    width: Option<Pixels>,
    window: &Window,
) -> Option<gpui_kit::WrappedLine> {
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_text(text.clone(), px(size), &[run], width, None)
        .ok()
        .and_then(|mut lines| (!lines.is_empty()).then(|| lines.remove(0)))
}

impl IntoElement for Paragraph {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Paragraph {
    type RequestLayoutState = ();
    type PrepaintState = Option<gpui_kit::WrappedLine>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style {
            flex_shrink: 1.,
            ..Default::default()
        };
        style.min_size.width = px(0.).into();
        let (text, font, size, color, line_height) = (
            self.text.clone(),
            self.font(),
            self.size,
            self.color,
            css(self.line_height_px()),
        );
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, _| {
                let width = known.width.or(match available.width {
                    gpui_kit::AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let Some(wrapped) = wrap_text(&text, &font, size, color, width, window) else {
                    return Size {
                        width: px(0.),
                        height: layout_px(line_height),
                    };
                };
                let lines = wrapped.wrap_boundaries.len() + 1;
                let natural = wrapped.unwrapped_layout.width;
                Size {
                    width: if lines > 1 {
                        width.unwrap_or(natural)
                    } else {
                        natural
                    },
                    height: layout_px(line_height * lines as f32),
                }
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        // A hair of slack, so a width taffy rounded down does not re-wrap the
        // line it was measured from.
        wrap_text(
            &self.text,
            &self.font(),
            self.size,
            self.color,
            Some(bounds.size.width + layout_px(0.5)),
            window,
        )
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let Some(wrapped) = prepaint.take() else {
            return;
        };
        let layout = &wrapped.unwrapped_layout;
        let line_height = css(self.line_height_px());
        let ascent = f32::from(layout.ascent).round();
        let descent = f32::from(layout.descent).round();
        let baseline_offset = (ascent + (line_height - ascent - descent) / 2.).floor();
        let font_size = px(self.size);
        // Where each line starts and ends, in the unwrapped layout's x.
        let glyphs: Vec<(usize, usize, f32)> = layout
            .runs
            .iter()
            .enumerate()
            .flat_map(|(run_ix, run)| {
                run.glyphs
                    .iter()
                    .enumerate()
                    .map(move |(glyph_ix, glyph)| (run_ix, glyph_ix, f32::from(glyph.position.x)))
            })
            .collect();
        let mut starts: Vec<f32> = vec![0.];
        for boundary in &wrapped.wrap_boundaries {
            if let Some(&(_, _, x)) = glyphs
                .iter()
                .find(|(r, g, _)| *r == boundary.run_ix && *g == boundary.glyph_ix)
            {
                starts.push(x);
            }
        }
        let total = f32::from(layout.width);
        let line_width = |line: usize| {
            let end = starts.get(line + 1).copied().unwrap_or(total);
            end - starts[line]
        };
        let mut line = 0;
        let mut boundaries = wrapped.wrap_boundaries.iter().peekable();
        for (run_ix, run) in layout.runs.iter().enumerate() {
            for (glyph_ix, glyph) in run.glyphs.iter().enumerate() {
                if boundaries
                    .peek()
                    .is_some_and(|b| b.run_ix == run_ix && b.glyph_ix == glyph_ix)
                {
                    boundaries.next();
                    line += 1;
                }
                let indent = if self.center {
                    ((f32::from(bounds.size.width) - line_width(line)) / 2.).max(0.)
                } else {
                    0.
                };
                let x = f32::from(bounds.origin.x) + indent + f32::from(glyph.position.x)
                    - starts[line.min(starts.len() - 1)];
                let y = f32::from(bounds.origin.y) + line as f32 * line_height + baseline_offset;
                let origin = point(layout_px(x), layout_px(y));
                let _ = if glyph.is_emoji {
                    window.paint_emoji(origin, run.font_id, glyph.id, font_size)
                } else {
                    window.paint_glyph(origin, run.font_id, glyph.id, font_size, self.color)
                };
            }
        }
    }
}
