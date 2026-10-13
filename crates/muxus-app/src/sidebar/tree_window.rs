//! Row windowing for the host tree. Only the rows near the sidebar's viewport
//! are mounted; spacers stand in for the rest, so a list of thousands of hosts
//! costs the same to render as a screenful. Kept pure so the arithmetic is
//! testable. Port of `client/src/components/sidebar/tree-window.ts`.

/// Rows mounted beyond each edge of the viewport, so short scrolls render nothing.
pub const TREE_OVERSCAN: usize = 12;

/// A half-open range of flattened row indices: `start` ≤ index < `end`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowWindow {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowSegment {
    Row { index: usize },
    Gap { rows: usize },
}

/// The rows whose boxes intersect the viewport. `scrolled_past` is how far the
/// list's top edge sits above the viewport's top edge: negative while the list
/// starts further down, below the fixed rows.
pub fn visible_row_span(
    scrolled_past: f64,
    viewport_height: f64,
    row_pitch: f64,
    row_count: usize,
) -> RowWindow {
    if row_count == 0 || row_pitch <= 0.0 {
        return RowWindow::default();
    }
    let count = row_count as f64;
    let start = (scrolled_past / row_pitch).floor().max(0.0).min(count);
    let end = ((scrolled_past + viewport_height.max(0.0)) / row_pitch)
        .ceil()
        .max(start)
        .min(count);
    RowWindow {
        start: start as usize,
        end: end as usize,
    }
}

/// Keep the mounted window while it still covers what is visible, so scrolling
/// within the overscan re-renders nothing; otherwise re-centre it with overscan
/// on both sides.
pub fn next_row_window(current: RowWindow, visible: RowWindow, overscan: usize) -> RowWindow {
    if visible.start >= current.start && visible.end <= current.end {
        return current;
    }
    RowWindow {
        start: visible.start.saturating_sub(overscan),
        end: visible.end + overscan,
    }
}

/// The row indices to mount, ascending: the window clamped to the list, plus
/// `pinned`: the roving tab stop has to stay mounted wherever it is, or Tab
/// could no longer reach the tree and a focused row would drop its focus.
pub fn rendered_row_indices(
    window: RowWindow,
    row_count: usize,
    pinned: Option<usize>,
) -> Vec<usize> {
    let start = window.start.min(row_count);
    let end = window.end.min(row_count).max(start);
    let mut indices = Vec::with_capacity(end - start + 1);
    if let Some(pinned) = pinned.filter(|&p| p < start) {
        indices.push(pinned);
    }
    indices.extend(start..end);
    if let Some(pinned) = pinned.filter(|&p| p >= end && p < row_count) {
        indices.push(pinned);
    }
    indices
}

/// Interleave mounted rows with spacers that hold the place of everything else.
pub fn row_segments(indices: &[usize], row_count: usize) -> Vec<RowSegment> {
    let mut segments = Vec::new();
    let mut next = 0;
    for &index in indices {
        if index > next {
            segments.push(RowSegment::Gap { rows: index - next });
        }
        segments.push(RowSegment::Row { index });
        next = index + 1;
    }
    if row_count > next {
        segments.push(RowSegment::Gap {
            rows: row_count - next,
        });
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(start: usize, end: usize) -> RowWindow {
        RowWindow { start, end }
    }

    #[test]
    fn starts_at_the_first_row_while_the_list_sits_below_the_viewport_top() {
        // 100px of fixed rows above the tree, a 270px viewport, 27px rows.
        assert_eq!(visible_row_span(-100.0, 270.0, 27.0, 1000), window(0, 7));
    }

    #[test]
    fn covers_partially_visible_rows_at_both_edges() {
        assert_eq!(visible_row_span(40.0, 100.0, 27.0, 1000), window(1, 6));
    }

    #[test]
    fn clamps_to_the_list() {
        assert_eq!(
            visible_row_span(27.0 * 990.0, 540.0, 27.0, 1000),
            window(990, 1000)
        );
        assert_eq!(
            visible_row_span(27.0 * 2000.0, 540.0, 27.0, 1000),
            window(1000, 1000)
        );
        assert_eq!(visible_row_span(0.0, 540.0, 27.0, 0), window(0, 0));
    }

    #[test]
    fn shows_nothing_for_an_unmeasured_viewport() {
        assert_eq!(visible_row_span(0.0, 0.0, 27.0, 50), window(0, 0));
    }

    #[test]
    fn keeps_the_mounted_window_while_it_still_covers_the_viewport() {
        let current = window(10, 60);
        assert_eq!(next_row_window(current, window(20, 40), 12), current);
        assert_eq!(next_row_window(current, window(10, 60), 12), current);
    }

    #[test]
    fn re_centres_with_overscan_once_the_viewport_leaves_it() {
        assert_eq!(
            next_row_window(window(10, 60), window(55, 75), 12),
            window(43, 87)
        );
        assert_eq!(
            next_row_window(window(10, 60), window(5, 25), 12),
            window(0, 37)
        );
    }

    #[test]
    fn mounts_the_window_clamped_to_the_rows_that_exist() {
        assert_eq!(rendered_row_indices(window(3, 7), 5, Some(3)), [3, 4]);
        assert!(rendered_row_indices(window(0, 40), 0, None).is_empty());
    }

    #[test]
    fn keeps_the_roving_tab_stop_mounted_wherever_it_is() {
        assert_eq!(
            rendered_row_indices(window(10, 13), 100, Some(2)),
            [2, 10, 11, 12]
        );
        assert_eq!(
            rendered_row_indices(window(10, 13), 100, Some(50)),
            [10, 11, 12, 50]
        );
        assert_eq!(
            rendered_row_indices(window(10, 13), 100, Some(11)),
            [10, 11, 12]
        );
    }

    #[test]
    fn holds_the_place_of_every_unmounted_row_with_spacers() {
        use RowSegment::*;
        assert_eq!(
            row_segments(&[2, 10, 11], 20),
            [
                Gap { rows: 2 },
                Row { index: 2 },
                Gap { rows: 7 },
                Row { index: 10 },
                Row { index: 11 },
                Gap { rows: 8 }
            ]
        );
        assert_eq!(
            row_segments(&[0, 1], 2),
            [Row { index: 0 }, Row { index: 1 }]
        );
    }

    #[test]
    fn always_accounts_for_exactly_the_whole_list() {
        for (start, end, pinned, count) in [
            (0, 40, Some(0), 774),
            (300, 350, Some(12), 774),
            (760, 800, Some(770), 774),
            (5, 10, Some(900), 774),
            (0, 40, None, 0),
        ] {
            let segments = row_segments(
                &rendered_row_indices(window(start, end), count, pinned),
                count,
            );
            let rows: usize = segments
                .iter()
                .map(|segment| match segment {
                    RowSegment::Gap { rows } => *rows,
                    RowSegment::Row { .. } => 1,
                })
                .sum();
            assert_eq!(rows, count);
        }
    }
}
