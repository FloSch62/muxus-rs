//! Powerline symbols (U+E0A0–U+E0D4) drawn on the cell grid from xterm.js's vector shapes, the
//! way its GPU renderer draws them (`customGlyphs`), instead of taken from a font.
//!
//! The bundled symbol font's separators are an em high, shorter than the cell, so a prompt's
//! segments ended in notches above and below each arrow; drawn here, an arrow spans its cell's
//! height and meets the next segment's background. Unlike box drawing, the shapes are not snapped
//! to whole pixels: xterm draws them antialiased as they are (`translateArgs` without clamping),
//! clipped to the cell.

mod paths;

use crate::box_drawing::{Segment, Shape};

/// Whether a path is filled or stroked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Kind {
    Fill,
    Stroke,
}

fn definition(c: char) -> Option<&'static (char, Kind, f32, f32, &'static str)> {
    let index = paths::PATHS
        .binary_search_by_key(&c, |entry| entry.0)
        .ok()?;
    Some(&paths::PATHS[index])
}

/// Whether this module draws `c`.
pub(crate) fn is_powerline(c: char) -> bool {
    definition(c).is_some()
}

/// The shapes of `c` in a cell of `width` × `height` device pixels, the font `font_size` device
/// pixels high (xterm's line is a twelfth of the font size).
pub(crate) fn shapes(c: char, width: f32, height: f32, font_size: f32) -> Option<Vec<Shape>> {
    let &(_, kind, left, right, path) = definition(c)?;
    let line = font_size / 12.;
    let (left, right) = (left * line / 2., right * line / 2.);
    let subpaths = parse_path(path, width - left - right, height, left);
    Some(match kind {
        Kind::Fill => vec![Shape::Fill { subpaths }],
        Kind::Stroke => subpaths
            .into_iter()
            .map(|segments| Shape::Stroke {
                segments,
                width: line.max(1.),
            })
            .collect(),
    })
}

/// xterm's `drawVectorShape` path commands (M, L, C, Q, T, Z) in device pixels: x on the cell's
/// `width` less the padding and moved right by `left`, y on its `height`.
fn parse_path(path: &str, width: f32, height: f32, left: f32) -> Vec<Vec<Segment>> {
    let mut subpaths: Vec<Vec<Segment>> = Vec::new();
    let mut current = (0., 0.);
    // The last control point, which a T reflects after a Q or a T.
    let mut control = (0., 0.);
    let mut last = ' ';
    for instruction in path.split(' ') {
        let mut chars = instruction.chars();
        let Some(kind) = chars.next() else { continue };
        if kind == 'Z' {
            if let Some(subpath) = subpaths.last_mut() {
                subpath.push(Segment::Close);
            }
            last = kind;
            continue;
        }
        let args: Vec<f32> = chars
            .as_str()
            .split(',')
            .filter_map(|arg| arg.parse().ok())
            .collect();
        if args.len() < 2 {
            continue;
        }
        let point = |index: usize| (args[index] * width + left, args[index + 1] * height);
        let segment = match kind {
            'M' => {
                current = point(0);
                control = current;
                subpaths.push(Vec::new());
                Segment::Move(current.0, current.1)
            }
            'L' => {
                current = point(0);
                control = current;
                Segment::Line(current.0, current.1)
            }
            'C' if args.len() >= 6 => {
                let (c1, c2, to) = (point(0), point(2), point(4));
                (current, control) = (to, c2);
                Segment::Cubic { c1, c2, to }
            }
            'Q' if args.len() >= 4 => {
                let (c, to) = (point(0), point(2));
                (current, control) = (to, c);
                Segment::Quad { c, to }
            }
            'T' => {
                let c = if matches!(last, 'Q' | 'T') {
                    (2. * current.0 - control.0, 2. * current.1 - control.1)
                } else {
                    current
                };
                let to = point(0);
                (current, control) = (to, c);
                Segment::Quad { c, to }
            }
            _ => continue,
        };
        if let Some(subpath) = subpaths.last_mut() {
            subpath.push(segment);
        }
        last = kind;
    }
    subpaths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_every_path_parses() {
        assert!(paths::PATHS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for &(c, ..) in paths::PATHS {
            let shapes = shapes(c, 8., 17., 13.).unwrap();
            assert!(!shapes.is_empty(), "{c:?} draws nothing");
        }
        assert!(is_powerline('\u{e0b0}'));
        assert!(!is_powerline('\u{e0c9}'));
        assert!(!is_powerline('a'));
    }

    #[test]
    fn the_solid_arrow_spans_the_cell_less_its_padding() {
        // 'M0,0 L1,.5 L0,1' with a right padding of two half lines: 12px font, 1px line.
        let shapes = shapes('\u{e0b0}', 8., 17., 12.).unwrap();
        let [Shape::Fill { subpaths }] = shapes.as_slice() else {
            panic!("{shapes:?}");
        };
        assert_eq!(
            subpaths[0],
            [
                Segment::Move(0., 0.),
                Segment::Line(7., 8.5),
                Segment::Line(0., 17.)
            ]
        );
    }

    #[test]
    fn the_left_padding_moves_the_shape_right() {
        let shapes = shapes('\u{e0b2}', 8., 17., 12.).unwrap();
        let [Shape::Fill { subpaths }] = shapes.as_slice() else {
            panic!("{shapes:?}");
        };
        assert_eq!(subpaths[0][0], Segment::Move(8., 0.));
        assert_eq!(subpaths[0][1], Segment::Line(1., 8.5));
    }

    #[test]
    fn a_smooth_quadratic_reflects_the_last_control_point() {
        let subpaths = parse_path("M0,0 Q.5,1,1,0 T2,0 Z", 1., 1., 0.);
        assert_eq!(
            subpaths[0][2],
            Segment::Quad {
                c: (1.5, -1.),
                to: (2., 0.)
            }
        );
        assert_eq!(subpaths[0][3], Segment::Close);
    }

    #[test]
    fn lines_stroke_a_twelfth_of_the_font_size() {
        let shapes = shapes('\u{e0b1}', 8., 17., 24.).unwrap();
        assert!(matches!(&shapes[0], Shape::Stroke { width, .. } if *width == 2.));
    }
}
