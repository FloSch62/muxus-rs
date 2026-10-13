//! Geometry every tree row shares: hosts, folders and the fixed rows above the
//! tree all read from here so they cannot drift apart. The numbers of
//! `client/src/components/sidebar/tree-row-style.ts`, in logical pixels.

use crate::hosts::tree::MAX_INDENT_DEPTH;

/// One line of text, tight enough that nesting still leaves rows on screen.
pub const TREE_ROW_HEIGHT: f32 = 26.0;
/// Vertical margin on each row; adjacent margins collapse into one gap.
pub const TREE_ROW_GAP: f32 = 1.0;
/// Distance from one row's top to the next. Every tree row has exactly this
/// geometry: the host tree only mounts the rows near the viewport and sizes the
/// spacers for the rest from it.
pub const TREE_ROW_PITCH: f32 = TREE_ROW_HEIGHT + TREE_ROW_GAP;
pub const TREE_INDENT_STEP: f32 = 14.0;
pub const TREE_BASE_INSET: f32 = 8.0;
/// Label type: 13px at weight 450 with -0.1px tracking; folders use 550.
pub const TREE_LABEL_SIZE: f32 = 13.0;
pub const TREE_LABEL_WEIGHT: f32 = 450.0;
pub const TREE_FOLDER_LABEL_WEIGHT: f32 = 550.0;
pub const TREE_LABEL_TRACKING: f32 = -0.1;

/// Where a row's content starts, clamped so deep trees keep a usable name column.
pub fn indent_px(depth: usize) -> f32 {
    TREE_BASE_INSET + depth.min(MAX_INDENT_DEPTH) as f32 * TREE_INDENT_STEP
}

/// One vertical indent guide of a row, spanning its full height.
#[derive(Clone, Debug, PartialEq)]
pub struct IndentGuide {
    /// Left edge, from the row's left edge.
    pub x: f32,
    pub width: f32,
    /// The rail colour, or `None` for the divider colour.
    pub color: Option<String>,
}

/// Indent guides: one hairline per ancestor, plus a thicker line in the
/// folder's colour for the nearest coloured one (the innermost guide marks the
/// row's own parent, so it carries the colour).
pub fn indent_guides(depth: usize, rail_color: Option<&str>) -> Vec<IndentGuide> {
    let levels = depth.min(MAX_INDENT_DEPTH);
    (0..levels)
        .map(|level| {
            let colored = level == levels - 1 && rail_color.is_some();
            IndentGuide {
                x: TREE_BASE_INSET + level as f32 * TREE_INDENT_STEP + 6.0,
                width: if colored { 2.0 } else { 1.0 },
                color: if colored {
                    rail_color.map(str::to_string)
                } else {
                    None
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indents_and_clamps() {
        assert_eq!(indent_px(0), 8.0);
        assert_eq!(indent_px(2), 36.0);
        assert_eq!(indent_px(9), indent_px(MAX_INDENT_DEPTH));
    }

    #[test]
    fn colours_only_the_innermost_guide() {
        assert!(indent_guides(0, Some("#fff")).is_empty());
        let guides = indent_guides(2, Some("#ef4444"));
        assert_eq!(
            guides,
            [
                IndentGuide {
                    x: 14.0,
                    width: 1.0,
                    color: None
                },
                IndentGuide {
                    x: 28.0,
                    width: 2.0,
                    color: Some("#ef4444".into())
                },
            ]
        );
        assert_eq!(
            indent_guides(1, None),
            [IndentGuide {
                x: 14.0,
                width: 1.0,
                color: None
            }]
        );
    }
}
