//! Scrolling and reflow logic for the terminal grid

use crate::cell::Cell;
use std::collections::VecDeque;

use crate::grid::{Grid, ScrollbackReflow};

impl Grid {
    /// Rotate the rows `[top, bottom]` by `n` (up when `up`, else down)
    /// together with their wrap flags and content generations, then blank
    /// the `n` rows that wrapped around. Rows move by pointer; no cell is
    /// copied. When `to_scrollback` (an upward scroll of a region anchored
    /// at the top of the screen), the rows leaving the top move into the
    /// scrollback, oldest first, and the rows the scrollback evicts are
    /// reused as the blank rows. Returns how many lines reached the
    /// scrollback.
    pub(in crate::grid) fn rotate_region(
        &mut self,
        n: usize,
        top: usize,
        bottom: usize,
        up: bool,
        to_scrollback: bool,
    ) -> usize {
        let n = n.min(bottom + 1 - top);
        if n == 0 {
            return 0;
        }
        let region = top..=bottom;
        let blanks = if up {
            self.screen[region.clone()].rotate_left(n);
            self.row_used[region.clone()].rotate_left(n);
            self.wrapped[region.clone()].rotate_left(n);
            self.row_content_gen[region].rotate_left(n);
            (bottom + 1 - n)..=bottom
        } else {
            self.screen[region.clone()].rotate_right(n);
            self.row_used[region.clone()].rotate_right(n);
            self.wrapped[region.clone()].rotate_right(n);
            self.row_content_gen[region].rotate_right(n);
            top..=(top + n - 1)
        };
        let to_scrollback = to_scrollback && self.max_scrollback > 0 && self.cols > 0;
        for row in blanks.clone() {
            let wrapped = std::mem::replace(&mut self.wrapped[row], false);
            let used = std::mem::replace(&mut self.row_used[row], 0).min(self.cols);
            if to_scrollback {
                self.scroll_row_off(row, used, wrapped);
            } else {
                self.screen[row][..used].fill(Cell::default());
            }
        }
        // Blanked rows changed content; rows that merely moved keep the
        // content generation they already had (ENH-038).
        self.mark_rows_content(*blanks.start(), *blanks.end());
        if to_scrollback {
            self.total_lines_scrolled += n;
            // The floor is computed after the pushes, so a batch that
            // crosses the cap mid-way still evicts zones in the rows it
            // dropped.
            let floor = self
                .total_lines_scrolled
                .saturating_sub(self.scrollback_lines);
            self.evict_zones(floor);
            n
        } else {
            0
        }
    }

    /// Move screen row `row` (with `used` written cells) into the scrollback
    /// and leave a blank row in its place.
    ///
    /// A full-width line moves by pointer, and the row the scrollback
    /// evicts becomes the blank screen row. With compact scrollback
    /// ([`Grid::set_compact_scrollback`]) a line is stored without its
    /// trailing default cells instead: only the written cells are copied,
    /// so the cost of a scrolled line follows its text rather than the
    /// screen width, and the screen row stays in place (warm in cache) for
    /// the next line.
    fn scroll_row_off(&mut self, row: usize, used: usize, wrapped: bool) {
        let len = if wrapped || !self.compact_scrollback {
            self.cols
        } else {
            super::used_len(&self.screen[row][..used])
        };
        if len == self.cols {
            let line = std::mem::take(&mut self.screen[row]);
            let spare = self.push_scrollback_row(line, wrapped);
            self.screen[row] = self.recycle_row(spare);
        } else {
            let cells = &mut self.screen[row];
            // `take` leaves default cells behind, which blanks the copied
            // part of the screen row on the way.
            let line: Box<[Cell]> = cells[..len].iter_mut().map(std::mem::take).collect();
            cells[len..used].fill(Cell::default());
            let _evicted = self.push_scrollback_row(line, wrapped);
        }
    }

    /// Scroll up by n lines
    pub fn scroll_up(&mut self, n: usize) {
        let n = n.min(self.rows);
        if n == 0 {
            return;
        }
        // The top n rows move into scrollback by ownership (no per-cell
        // clone), the rest shift up by pointer, and the n fresh blank rows
        // at the bottom take fresh content stamps (ENH-038).
        let bottom = self.rows - 1;
        self.rotate_region(n, 0, bottom, true, true);
        self.mark_rows_damage(0, bottom);
        self.record_scroll_op(0, bottom, n as i32);
    }

    /// Scroll down by n lines
    pub fn scroll_down(&mut self, n: usize) {
        let n = n.min(self.rows);
        if n == 0 {
            return;
        }
        // Rows move down by pointer; rows [0, n) land on stale bottom
        // content and are cleared.
        let bottom = self.rows - 1;
        self.rotate_region(n, 0, bottom, false, false);
        self.mark_rows_damage(0, bottom);
        self.record_scroll_op(0, bottom, -(n as i32));
    }

    /// Scroll up within a region. Returns `false` if parameters are invalid.
    pub fn scroll_region_up(&mut self, n: usize, top: usize, bottom: usize) -> bool {
        if top >= self.rows || bottom >= self.rows || top > bottom {
            #[cfg(debug_assertions)]
            eprintln!(
                "Invalid scroll region up: top={} bottom={} rows={}",
                top, bottom, self.rows
            );
            return false;
        }

        let n = n.min(bottom - top + 1);

        let whole_screen = top == 0 && bottom == self.rows - 1 && self.max_scrollback > 0;
        if whole_screen && n > 0 {
            // scroll_up marks every row damaged itself.
            self.scroll_up(n);
            return true;
        }
        self.mark_rows_damage(top, bottom);
        if whole_screen {
            return true;
        }

        // A region anchored at the top of the screen (a status line below
        // it) still sends its departing lines to the scrollback.
        self.rotate_region(n, top, bottom, true, top == 0);
        if n > 0 {
            self.record_scroll_op(top, bottom, n as i32);
        }
        true
    }

    /// Scroll down within a region. Returns `false` if parameters are invalid.
    pub fn scroll_region_down(&mut self, n: usize, top: usize, bottom: usize) -> bool {
        if top >= self.rows || bottom >= self.rows || top > bottom {
            #[cfg(debug_assertions)]
            eprintln!(
                "Invalid scroll region down: top={} bottom={} rows={}",
                top, bottom, self.rows
            );
            return false;
        }

        let n = n.min(bottom - top + 1);
        self.mark_rows_damage(top, bottom);
        self.rotate_region(n, top, bottom, false, false);
        if n > 0 {
            self.record_scroll_op(top, bottom, -(n as i32));
        }
        true
    }

    /// Resize the grid
    /// Resize WITHOUT reflow: every row keeps its screen position and is
    /// truncated or right-padded to the new width; wrap flags are cleared.
    ///
    /// The alternate screen must resize this way. A full-screen app draws
    /// the alt screen cell by cell and redraws it on SIGWINCH — often only
    /// the cells it believes changed (ratatui, curses). Reflowing joins or
    /// splits its rows, so cells the app thinks are still in place move,
    /// and the scramble persists (kanban-tui/top garbled after any
    /// resize). xterm, tmux, and every other emulator leave the alt screen
    /// un-reflowed for exactly this reason.
    pub fn resize_without_reflow(&mut self, cols: usize, rows: usize) {
        if self.cols == cols && self.rows == rows {
            return;
        }
        if cols == 0 || rows == 0 {
            return;
        }
        let copy_cols = cols.min(self.cols);
        let mut screen = std::mem::take(&mut self.screen);
        screen.truncate(rows);
        for row in &mut screen {
            let mut cells = std::mem::take(row).into_vec();
            cells.truncate(copy_cols);
            cells.resize(cols, Cell::default());
            *row = cells.into_boxed_slice();
        }
        screen.resize_with(rows, || super::blank_row(cols));
        self.screen = screen;
        self.row_used.truncate(rows);
        for used in &mut self.row_used {
            *used = (*used).min(cols);
        }
        self.row_used.resize(rows, 0);
        self.wrapped = vec![false; rows];
        self.cols = cols;
        self.rows = rows;
        self.reset_damage_for_resize();
    }

    /// Reset per-row damage generations to the new row count and mark every
    /// row: a resize moves content even when the cell data survives
    /// unchanged.
    fn reset_damage_for_resize(&mut self) {
        self.row_gen = vec![0u64; self.rows];
        self.row_content_gen = vec![0u64; self.rows];
        self.mark_rows_content(0, self.rows.saturating_sub(1));
        // A resize or reflow moves/rewraps content wholesale; the scroll
        // log cannot describe it, so older generations get the full-redraw
        // sentinel (ENH-038).
        self.invalidate_scroll_log();
    }

    /// Resize the visible grid to `cols` × `rows`. A width change reflows the
    /// visible screen; the scrollback is never rebuilt — stored lines keep
    /// the width they scrolled off at (renderers handle short lines by
    /// padding). A height-only change resizes in place. Every visible row is
    /// marked damaged. A no-op when the size is unchanged or either
    /// dimension is zero.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        if self.cols == cols && self.rows == rows {
            return;
        }

        if cols == 0 || rows == 0 {
            return;
        }

        if self.cols == cols {
            // Width unchanged: Optimized path using simple Vec resizing
            // This implicitly handles growing (padding with default) and shrinking (truncating)
            // for the main grid, without touching scrollback.

            self.screen.resize_with(rows, || super::blank_row(cols));
            self.row_used.resize(rows, 0);
            self.wrapped.resize(rows, false);
            self.rows = rows;
            self.reset_damage_for_resize();

            // Scrollback remains identical (no push/pull)
            // Zones remain valid as they track absolute indices
            return;
        }

        // Width changed: reflow the visible screen. Scrollback lines keep
        // their original widths — resizing cost is bounded by the viewport
        // rows, never the scrollback — unless the scrollback reflows too
        // (Muxus patch, `reflow_scrollback`).
        let old_cols = self.cols;
        let old_rows = self.rows;
        if self.reflow_scrollback && self.max_scrollback > 0 {
            self.reflow_scrollback_rows(cols);
        }

        self.reflow_main_grid(old_cols, old_rows, cols, rows);
        self.reset_damage_for_resize();
    }

    /// Push reflowed excess lines into scrollback (resize path) with the
    /// same bookkeeping as a scroll: every pushed row advances
    /// `total_lines_scrolled`, and zones that fall below the new scrollback
    /// floor are evicted into the evicted buffer for `ZoneScrolledOut`.
    fn reflow_push_lines(&mut self, cells: &[Cell], wrapped: &[bool], cols: usize, count: usize) {
        if self.max_scrollback == 0 || count == 0 {
            return;
        }
        for line_idx in 0..count {
            let start = line_idx * cols;
            let is_wrapped = wrapped.get(line_idx).copied().unwrap_or(false);
            let _evicted = self.push_scrollback_row(
                cells[start..start + cols].to_vec().into_boxed_slice(),
                is_wrapped,
            );
        }
        self.total_lines_scrolled += count;
        // The floor is computed after the pushes, so a batch that crosses
        // the cap mid-way still evicts zones in the rows it dropped.
        let floor = self
            .total_lines_scrolled
            .saturating_sub(self.scrollback_lines);
        self.evict_zones(floor);
    }

    fn reflow_main_grid(
        &mut self,
        old_cols: usize,
        old_rows: usize,
        new_cols: usize,
        new_rows: usize,
    ) {
        let logical_lines = self.extract_main_grid_logical_lines(old_cols, old_rows);
        let mut all_cells = Vec::new();
        let mut all_wrapped = Vec::new();

        for logical_line in logical_lines {
            let (cells, wrapped_flags) = self.rewrap_logical_line(&logical_line, new_cols);

            if cells.is_empty() {
                for _ in 0..new_cols {
                    all_cells.push(Cell::default());
                }
                all_wrapped.push(false);
                continue;
            }

            for (i, row_cells) in cells.chunks(new_cols).enumerate() {
                all_cells.extend(row_cells.iter().cloned());
                while all_cells.len() % new_cols != 0 {
                    all_cells.push(Cell::default());
                }
                all_wrapped.push(wrapped_flags.get(i).copied().unwrap_or(false));
            }
        }

        let mut last_content_line = 0;
        for (line_idx, _) in all_wrapped.iter().enumerate() {
            let start = line_idx * new_cols;
            let end = (start + new_cols).min(all_cells.len());
            if all_cells[start..end]
                .iter()
                .any(|c| c.c != ' ' || !c.is_empty())
            {
                last_content_line = line_idx + 1;
            }
        }

        let effective_lines = last_content_line.max(1);
        if effective_lines > new_rows {
            let excess_lines = effective_lines - new_rows;
            self.reflow_push_lines(&all_cells, &all_wrapped, new_cols, excess_lines);
            let keep_start = excess_lines * new_cols;
            all_cells = all_cells[keep_start..].to_vec();
            all_wrapped = all_wrapped[excess_lines..].to_vec();
        }

        let mut new_wrapped = vec![false; new_rows];
        let lines_to_copy = all_wrapped.len().min(new_rows);
        new_wrapped[..lines_to_copy].copy_from_slice(&all_wrapped[..lines_to_copy]);
        // Only whole rows of the reflowed buffer carry over (a partial last
        // row stays blank, as before).
        let whole = (all_cells.len() / new_cols).min(lines_to_copy);
        all_cells.truncate(whole * new_cols);

        self.cols = new_cols;
        self.rows = new_rows;
        self.screen = super::rows_from_cells(&all_cells, new_cols, new_rows);
        self.row_used = vec![new_cols; new_rows];
        self.wrapped = new_wrapped;
    }

    /// Muxus patch: rewrap the scrollback's lines to `new_cols` (xterm reflows its whole buffer
    /// on a width change). Each logical line (rows joined by their wrap flags) is cut again at
    /// the new width: a narrower screen no longer clips stored lines, a wider one joins what
    /// the old width wrapped. The last logical line is left as it is when it continues onto
    /// the screen (the screen reflows on its own). Where every old line went is kept for the
    /// caller ([`Grid::take_scrollback_reflow`]); the lines above the scrollback keep their
    /// absolute numbers, and lines past the capacity are evicted as a scroll evicts them.
    fn reflow_scrollback_rows(&mut self, new_cols: usize) {
        let old_len = self.scrollback_rows.len();
        if old_len == 0 || new_cols == 0 {
            return;
        }
        let floor = self
            .total_lines_scrolled
            .saturating_sub(self.scrollback_lines);
        // The logical line that continues onto the screen stays as it is.
        let mut open_tail = old_len;
        if self.scrollback_wrapped.back() == Some(&true) {
            open_tail -= 1;
            while open_tail > 0 && self.scrollback_wrapped[open_tail - 1] {
                open_tail -= 1;
            }
        }
        let old_rows = std::mem::take(&mut self.scrollback_rows);
        let old_wrapped = std::mem::take(&mut self.scrollback_wrapped);
        let mut out = Rewrap::new(new_cols, self.compact_scrollback, old_len);
        let mut lines = Vec::with_capacity(old_len);
        let mut index = 0;
        while index < open_tail {
            loop {
                let row = &old_rows[index];
                let last = !old_wrapped[index];
                let cells = if last {
                    &row[..super::used_len(row)]
                } else {
                    &row[..]
                };
                lines.push(out.place_row(cells));
                index += 1;
                if last {
                    break;
                }
            }
            out.end_line();
        }
        for (row, wrapped) in old_rows.into_iter().zip(old_wrapped).skip(open_tail) {
            lines.push(out.rows.len());
            out.rows.push_back(row);
            out.wrapped.push_back(wrapped);
        }
        let new_len = out.rows.len();
        self.scrollback_rows = out.rows;
        self.scrollback_wrapped = out.wrapped;
        // The same lines above the scrollback stay trimmed; zones follow their lines.
        let moved = |abs: usize| -> usize {
            match abs.checked_sub(floor) {
                Some(index) if index < old_len => floor + lines[index],
                // Screen lines move with the scrollback's new length.
                Some(_) => (abs + new_len).saturating_sub(old_len),
                None => abs,
            }
        };
        for zone in &mut self.zones {
            zone.abs_row_start = moved(zone.abs_row_start);
            zone.abs_row_end = moved(zone.abs_row_end);
        }
        self.total_lines_scrolled = floor + new_len;
        while self.scrollback_rows.len() > self.max_scrollback {
            self.scrollback_rows.pop_front();
            self.scrollback_wrapped.pop_front();
        }
        self.scrollback_lines = self.scrollback_rows.len();
        let floor_after = self
            .total_lines_scrolled
            .saturating_sub(self.scrollback_lines);
        self.evict_zones(floor_after);
        self.last_scrollback_reflow = Some(ScrollbackReflow {
            lines,
            old_len,
            new_len,
        });
    }

    fn extract_main_grid_logical_lines(&self, old_cols: usize, old_rows: usize) -> Vec<Vec<Cell>> {
        let mut logical_lines = Vec::new();
        let mut current_line = Vec::new();
        for row in 0..old_rows {
            for col in 0..old_cols {
                if let Some(cell) = self.get(col, row) {
                    if !cell.flags.wide_char_spacer() {
                        current_line.push(cell.clone());
                    }
                }
            }
            if !self.is_line_wrapped(row) {
                while current_line
                    .last()
                    .is_some_and(|c| c.c == ' ' && c.is_empty())
                {
                    current_line.pop();
                }
                logical_lines.push(std::mem::take(&mut current_line));
            }
        }
        if !current_line.is_empty() {
            logical_lines.push(current_line);
        }
        logical_lines
    }

    fn rewrap_logical_line(&self, line: &[Cell], width: usize) -> (Vec<Cell>, Vec<bool>) {
        let mut new_cells = Vec::new();
        let mut wrapped_flags = Vec::new();
        let mut current_col = 0;

        for cell in line {
            let char_width = cell.width as usize;
            if current_col + char_width > width {
                while current_col < width {
                    new_cells.push(Cell::default());
                    current_col += 1;
                }
                wrapped_flags.push(true);
                current_col = 0;
            }
            new_cells.push(cell.clone());
            current_col += char_width;
            for _ in 1..char_width {
                let mut spacer = Cell::default();
                spacer.flags.set_wide_char_spacer(true);
                new_cells.push(spacer);
            }
        }
        wrapped_flags.push(false);
        (new_cells, wrapped_flags)
    }
}

/// Muxus patch: scrollback rows built by cutting logical lines at a new width
/// ([`Grid::reflow_scrollback_rows`]).
struct Rewrap {
    cols: usize,
    compact: bool,
    rows: VecDeque<Box<[Cell]>>,
    wrapped: VecDeque<bool>,
    current: Vec<Cell>,
}

impl Rewrap {
    fn new(cols: usize, compact: bool, capacity: usize) -> Self {
        Self {
            cols,
            compact,
            rows: VecDeque::with_capacity(capacity),
            wrapped: VecDeque::with_capacity(capacity),
            current: Vec::with_capacity(cols),
        }
    }

    /// Append one stored row's cells to the logical line being rebuilt. Returns the new row
    /// that holds the row's first cell (for an empty row, the row being filled).
    fn place_row(&mut self, cells: &[Cell]) -> usize {
        let mut first = None;
        for cell in cells.iter().filter(|cell| !cell.flags.wide_char_spacer()) {
            let width = (cell.width as usize).max(1);
            // A character that does not fit starts the next row; a wide one leaves the
            // last cell blank, as xterm's reflow does.
            if !self.current.is_empty() && self.current.len() + width > self.cols {
                self.flush(true);
            }
            first.get_or_insert(self.rows.len());
            self.current.push(cell.clone());
            for _ in 1..width {
                let mut spacer = Cell::default();
                spacer.flags.set_wide_char_spacer(true);
                self.current.push(spacer);
            }
        }
        first.unwrap_or(self.rows.len())
    }

    /// The logical line ends: its last row goes in unwrapped.
    fn end_line(&mut self) {
        self.flush(false);
    }

    /// A wrapped row keeps the full width (it continues on the next row); a line's last row
    /// is stored without its blank tail when the scrollback is compact.
    fn flush(&mut self, wrapped: bool) {
        let mut row = std::mem::take(&mut self.current);
        if wrapped || !self.compact {
            let width = self.cols.max(row.len());
            row.resize(width, Cell::default());
        }
        self.rows.push_back(row.into_boxed_slice());
        self.wrapped.push_back(wrapped);
        self.current = Vec::with_capacity(self.cols);
    }
}

#[cfg(test)]
mod tests {
    use crate::cell::Cell;
    use crate::grid::{Grid, ScrollDamage, MAX_SCROLL_OPS};

    fn grid() -> Grid {
        Grid::new(8, 6, 100)
    }

    fn fill(grid: &mut Grid, ch: char) {
        for row in 0..grid.rows() {
            for col in 0..grid.cols() {
                grid.set(col, row, Cell::new(ch));
            }
        }
    }

    fn content_dirty_rows(grid: &Grid, since: u64) -> Vec<u32> {
        let mut rows = Vec::new();
        grid.for_each_content_damage_range_since(since, |s, e| rows.extend(s..=e));
        rows
    }

    #[test]
    fn scroll_up_reports_delta_and_only_cleared_rows() {
        let mut g = grid();
        fill(&mut g, 'x');
        let since = g.generation();
        g.scroll_up(1);

        let d = g.scroll_damage_since(since);
        assert_eq!(
            d,
            ScrollDamage {
                full_redraw: false,
                top: 0,
                bottom: 5,
                delta: 1
            }
        );
        assert_eq!(content_dirty_rows(&g, since), vec![5]);
        // Positional damage still covers every row: ENH-025 behavior unchanged.
        assert_eq!(g.damage_indices(since).count(), 6);
    }

    #[test]
    fn scroll_down_reports_negative_delta_and_cleared_top_rows() {
        let mut g = grid();
        fill(&mut g, 'x');
        let since = g.generation();
        g.scroll_down(2);

        let d = g.scroll_damage_since(since);
        assert_eq!(d.delta, -2);
        assert_eq!(d.top, 0);
        assert_eq!(d.bottom, 5);
        assert!(!d.full_redraw);
        assert_eq!(content_dirty_rows(&g, since), vec![0, 1]);
    }

    #[test]
    fn region_scroll_reports_the_region() {
        let mut g = grid();
        fill(&mut g, 'x');
        let since = g.generation();
        assert!(g.scroll_region_up(1, 2, 4));

        let d = g.scroll_damage_since(since);
        assert_eq!(d.top, 2);
        assert_eq!(d.bottom, 4);
        assert_eq!(d.delta, 1);
        assert!(!d.full_redraw);
        assert_eq!(content_dirty_rows(&g, since), vec![4]);
    }

    #[test]
    fn same_region_ops_compose_by_sum() {
        let mut g = grid();
        fill(&mut g, 'x');
        let since = g.generation();
        g.scroll_up(1);
        g.scroll_up(2);

        assert_eq!(g.scroll_damage_since(since).delta, 3);
    }

    #[test]
    fn mixed_regions_fall_back_to_full_redraw() {
        let mut g = grid();
        fill(&mut g, 'x');
        let since = g.generation();
        g.scroll_up(1);
        assert!(g.scroll_region_up(1, 1, 3));

        assert!(g.scroll_damage_since(since).full_redraw);
    }

    #[test]
    fn no_scrollback_grid_still_records_ops() {
        // Alt-screen-shaped grid: max_scrollback == 0, so scroll_region_up
        // takes the inline path instead of delegating to scroll_up.
        let mut g = Grid::new(8, 6, 0);
        fill(&mut g, 'x');
        let since = g.generation();
        assert!(g.scroll_region_up(1, 0, 5));

        let d = g.scroll_damage_since(since);
        assert_eq!(d.delta, 1);
        assert_eq!(content_dirty_rows(&g, since), vec![5]);
        assert_eq!(g.total_lines_scrolled(), 0);
    }

    #[test]
    fn resize_clears_scrollback_and_restore_invalidate_the_log() {
        let mut g = grid();
        fill(&mut g, 'x');
        let snap = g.capture_snapshot();

        let since = g.generation();
        g.resize(4, 3);
        assert!(g.scroll_damage_since(since).full_redraw);

        let since = g.generation();
        g.clear_scrollback();
        assert!(g.scroll_damage_since(since).full_redraw);

        let since = g.generation();
        g.restore_from_snapshot(&snap);
        assert!(g.scroll_damage_since(since).full_redraw);
    }

    #[test]
    fn log_overflow_falls_back_to_full_redraw() {
        let mut g = grid();
        fill(&mut g, 'x');
        let since = g.generation();
        for _ in 0..MAX_SCROLL_OPS {
            g.scroll_up(1);
        }
        // At the cap the whole history is still expressible.
        assert_eq!(g.scroll_damage_since(since).delta, MAX_SCROLL_OPS as i32);

        g.scroll_up(1);
        assert!(g.scroll_damage_since(since).full_redraw);
    }
}
