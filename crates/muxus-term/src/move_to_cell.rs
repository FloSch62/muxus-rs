//! Alt+click moves the cursor (xterm's `altClickMovesCursor`): the arrow-key presses that take
//! the shell's cursor to the clicked cell. Adapted from xterm.js `browser/input/MoveToCell.ts`
//! (MIT, © 2018 The xterm.js authors).
//!
//! On the normal buffer only left and right arrows are sent: a shell's line editor moves through
//! its (possibly wrapped) input line with them. On the alternate screen (and a buffer without
//! scrollback) a full-screen program moves between rows, so the up and down arrows first reach
//! the row, then left and right reach the column.

use crate::buffer::{TextBuffer, translate_to_string};

/// What [`move_to_cell_sequence`] needs to know about the terminal.
#[derive(Debug, Clone, Copy)]
pub struct MoveToCell {
    /// The buffer line shown in screen row 0 (the scrollback's length).
    pub base: usize,
    /// Screen rows.
    pub rows: usize,
    /// The cursor's column and screen row.
    pub cursor: (usize, usize),
    /// Whether the buffer keeps a scrollback (`buffer.hasScrollback`): the normal buffer with a
    /// scrollback limit above zero.
    pub has_scrollback: bool,
    /// DECCKM: the arrows are `ESC O x` instead of `ESC [ x`.
    pub application_cursor: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
    Right,
    Left,
}

impl Direction {
    fn letter(self) -> char {
        match self {
            Direction::Up => 'A',
            Direction::Down => 'B',
            Direction::Right => 'C',
            Direction::Left => 'D',
        }
    }
}

struct Mover<'a, B: TextBuffer + ?Sized> {
    buffer: &'a B,
    state: MoveToCell,
}

/// The arrow keys that move the cursor from where it is to `target` (column, screen row).
pub fn move_to_cell_sequence<B: TextBuffer + ?Sized>(
    buffer: &B,
    state: MoveToCell,
    target: (usize, usize),
) -> String {
    let mover = Mover { buffer, state };
    let (start_x, start_y) = (state.cursor.0 as isize, state.cursor.1 as isize);
    let (target_x, target_y) = (target.0 as isize, target.1 as isize);

    // The alt buffer should try to navigate between rows.
    if !state.has_scrollback {
        return mover.reset_starting_row(start_x, start_y, target_y)
            + &mover.move_to_requested_row(start_y, target_y)
            + &mover.move_to_requested_col(start_x, start_y, target_x, target_y);
    }

    // Only move horizontally for the normal buffer.
    if start_y == target_y {
        let direction = if start_x > target_x {
            Direction::Left
        } else {
            Direction::Right
        };
        return mover.repeat((start_x - target_x).abs(), direction);
    }
    let direction = if start_y > target_y {
        Direction::Left
    } else {
        Direction::Right
    };
    let row_difference = (start_y - target_y).abs();
    let cols = buffer.cols() as isize;
    let cells_to_move = cols_from_row_end(if start_y > target_y { target_x } else { start_x }, cols)
        + (row_difference - 1) * cols
        + 1 // wrap around 1 row
        + cols_from_row_beginning(if start_y > target_y { start_x } else { target_x });
    mover.repeat(cells_to_move, direction)
}

/// Columns from a row's beginning to a column.
fn cols_from_row_beginning(x: isize) -> isize {
    x - 1
}

/// Columns from a column to the row's end.
fn cols_from_row_end(x: isize, cols: isize) -> isize {
    cols - x
}

impl<B: TextBuffer + ?Sized> Mover<'_, B> {
    fn sequence(&self, direction: Direction) -> String {
        let mode = if self.state.application_cursor {
            'O'
        } else {
            '['
        };
        format!("\x1b{mode}{}", direction.letter())
    }

    fn repeat(&self, count: isize, direction: Direction) -> String {
        if count <= 0 {
            return String::new();
        }
        self.sequence(direction).repeat(count as usize)
    }

    fn line(&self, row: isize) -> Option<usize> {
        let line = self.state.base as isize + row;
        (line >= 0 && (line as usize) < self.buffer.length()).then_some(line as usize)
    }

    fn is_wrapped(&self, row: isize) -> bool {
        self.line(row)
            .is_some_and(|line| self.buffer.is_wrapped(line))
    }

    /// `translateBufferLineToString(row, false, start, end).length`, in UTF-16 units as
    /// JavaScript counts them.
    fn text_length(&self, row: isize, start: isize, end: isize) -> isize {
        let Some(cells) = self.line(row).and_then(|line| self.buffer.line_cells(line)) else {
            return 0;
        };
        if start < 0 || end <= start {
            return 0;
        }
        translate_to_string(&cells, false, start as usize, Some(end as usize))
            .encode_utf16()
            .count() as isize
    }

    /// If the cursor starts on a wrapped row, move it back to the first row that is not wrapped
    /// so the vertical moves line up.
    fn reset_starting_row(&self, start_x: isize, start_y: isize, target_y: isize) -> String {
        if self.move_to_requested_row(start_y, target_y).is_empty() {
            return String::new();
        }
        let length = self.buffer_line(
            start_x,
            start_y,
            start_x,
            start_y - self.wrapped_rows_for_row(start_y),
            false,
        );
        self.repeat(length, Direction::Left)
    }

    /// Move to the requested row, skipping wrapped rows.
    fn move_to_requested_row(&self, start_y: isize, target_y: isize) -> String {
        let start_row = start_y - self.wrapped_rows_for_row(start_y);
        let end_row = target_y - self.wrapped_rows_for_row(target_y);
        let rows_to_move = (start_row - end_row).abs() - self.wrapped_rows_count(start_y, target_y);
        self.repeat(rows_to_move, vertical_direction(start_y, target_y))
    }

    /// Move to the requested column on the ending row.
    fn move_to_requested_col(
        &self,
        start_x: isize,
        start_y: isize,
        target_x: isize,
        target_y: isize,
    ) -> String {
        let start_row = if self.move_to_requested_row(start_y, target_y).is_empty() {
            start_y
        } else {
            target_y - self.wrapped_rows_for_row(target_y)
        };
        let direction = self.horizontal_direction(start_x, start_y, target_x, target_y);
        let length = self.buffer_line(
            start_x,
            start_row,
            target_x,
            target_y,
            direction == Direction::Right,
        );
        self.repeat(length, direction)
    }

    /// Wrapped rows between the unwrapped starting and ending rows: the cursor skips them.
    fn wrapped_rows_count(&self, start_y: isize, target_y: isize) -> isize {
        let start_row = start_y - self.wrapped_rows_for_row(start_y);
        let end_row = target_y - self.wrapped_rows_for_row(target_y);
        let step = if vertical_direction(start_y, target_y) == Direction::Up {
            -1
        } else {
            1
        };
        (0..(start_row - end_row).abs())
            .filter(|i| self.is_wrapped(start_row + step * i))
            .count() as isize
    }

    /// How many wrapped rows make up the row (the rows above it it continues).
    fn wrapped_rows_for_row(&self, mut row: isize) -> isize {
        let mut count = 0;
        let rows = self.state.rows as isize;
        let mut wraps = self.is_wrapped(row);
        while wraps && row >= 0 && row < rows {
            count += 1;
            row -= 1;
            wraps = self.is_wrapped(row);
        }
        count
    }

    fn horizontal_direction(
        &self,
        start_x: isize,
        start_y: isize,
        target_x: isize,
        target_y: isize,
    ) -> Direction {
        let start_row = if self.move_to_requested_row(start_y, target_y).is_empty() {
            start_y
        } else {
            target_y - self.wrapped_rows_for_row(target_y)
        };
        if (start_x < target_x && start_row <= target_y)
            || (start_x >= target_x && start_row < target_y)
        {
            Direction::Right
        } else {
            Direction::Left
        }
    }

    /// The length of the text from one cell to another (`bufferLine(...).length`). Leftward on
    /// one row it reads the empty range `start..end` with `start > end`, so xterm sends nothing;
    /// kept as it is.
    fn buffer_line(
        &self,
        mut start_col: isize,
        start_row: isize,
        end_col: isize,
        end_row: isize,
        forward: bool,
    ) -> isize {
        let cols = self.buffer.cols() as isize;
        // `buffer.lines.length`: the screen's rows, as rows here count from the screen's top.
        let lines = self.state.rows as isize;
        let mut current_col = start_col;
        let mut current_row = start_row;
        let mut length = 0;
        while (current_col != end_col || current_row != end_row)
            && current_row >= 0
            && current_row < lines
        {
            current_col += if forward { 1 } else { -1 };
            if forward && current_col > cols - 1 {
                length += self.text_length(current_row, start_col, current_col);
                current_col = 0;
                start_col = 0;
                current_row += 1;
            } else if !forward && current_col < 0 {
                length += self.text_length(current_row, 0, start_col + 1);
                current_col = cols - 1;
                start_col = current_col;
                current_row -= 1;
            }
        }
        length + self.text_length(current_row, start_col, current_col)
    }
}

fn vertical_direction(start_y: isize, target_y: isize) -> Direction {
    if start_y > target_y {
        Direction::Up
    } else {
        Direction::Down
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{CellText, cells_from_str};

    /// Screen rows of plain text, `wrapped[i]` saying row `i` continues the row above.
    struct Screen {
        cols: usize,
        rows: Vec<Vec<CellText>>,
        wrapped: Vec<bool>,
    }

    impl Screen {
        fn new(cols: usize, lines: &[(&str, bool)]) -> Self {
            let rows = lines
                .iter()
                .map(|(text, _)| {
                    let mut cells = cells_from_str(text);
                    cells.resize(cols, CellText::blank());
                    cells
                })
                .collect();
            Self {
                cols,
                rows,
                wrapped: lines.iter().map(|(_, wrapped)| *wrapped).collect(),
            }
        }
    }

    impl TextBuffer for Screen {
        fn length(&self) -> usize {
            self.rows.len()
        }
        fn cols(&self) -> usize {
            self.cols
        }
        fn is_wrapped(&self, line: usize) -> bool {
            self.wrapped.get(line).copied().unwrap_or(false)
        }
        fn line_cells(&self, line: usize) -> Option<Vec<CellText>> {
            self.rows.get(line).cloned()
        }
    }

    fn state(screen: &Screen, cursor: (usize, usize), has_scrollback: bool) -> MoveToCell {
        MoveToCell {
            base: 0,
            rows: screen.rows.len(),
            cursor,
            has_scrollback,
            application_cursor: false,
        }
    }

    const LEFT: &str = "\x1b[D";
    const RIGHT: &str = "\x1b[C";
    const UP: &str = "\x1b[A";
    const DOWN: &str = "\x1b[B";

    #[test]
    fn moves_along_the_shell_line_with_left_and_right_on_the_normal_buffer() {
        let screen = Screen::new(10, &[("$ echo hi", false), ("", false)]);
        assert_eq!(
            move_to_cell_sequence(&screen, state(&screen, (9, 0), true), (2, 0)),
            LEFT.repeat(7)
        );
        assert_eq!(
            move_to_cell_sequence(&screen, state(&screen, (2, 0), true), (6, 0)),
            RIGHT.repeat(4)
        );
        assert_eq!(
            move_to_cell_sequence(&screen, state(&screen, (4, 0), true), (4, 0)),
            ""
        );
    }

    #[test]
    fn crosses_rows_of_a_wrapped_line_as_one_stretch_of_columns() {
        let screen = Screen::new(10, &[("0123456789", false), ("abc", true), ("", false)]);
        // From column 2 of the second row back to column 5 of the first: the rest of the first
        // row (10 - 5), one for the wrap, and the second row's columns before the cursor (2 - 1).
        assert_eq!(
            move_to_cell_sequence(&screen, state(&screen, (2, 1), true), (5, 0)),
            LEFT.repeat(5 + 1 + 1)
        );
        assert_eq!(
            move_to_cell_sequence(&screen, state(&screen, (5, 0), true), (2, 1)),
            RIGHT.repeat(5 + 1 + 1)
        );
        // Two rows down adds a full row of columns.
        assert_eq!(
            move_to_cell_sequence(&screen, state(&screen, (5, 0), true), (2, 2)),
            RIGHT.repeat(5 + 10 + 1 + 1)
        );
    }

    #[test]
    fn uses_application_cursor_arrows() {
        let screen = Screen::new(10, &[("", false)]);
        let mut state = state(&screen, (0, 0), true);
        state.application_cursor = true;
        assert_eq!(
            move_to_cell_sequence(&screen, state, (2, 0)),
            "\x1bOC\x1bOC"
        );
    }

    #[test]
    fn moves_between_rows_then_along_the_row_on_the_alternate_screen() {
        let screen = Screen::new(
            10,
            &[
                ("line one", false),
                ("line two", false),
                ("line three", false),
            ],
        );
        // Down two rows, then right along the target row from the cursor's column.
        assert_eq!(
            move_to_cell_sequence(&screen, state(&screen, (1, 0), false), (4, 2)),
            DOWN.repeat(2) + &RIGHT.repeat(3)
        );
        // Up one row, then left: xterm reads the empty range on the target row and stays.
        assert_eq!(
            move_to_cell_sequence(&screen, state(&screen, (6, 2), false), (2, 1)),
            UP.to_string()
        );
        // Right on the same row.
        assert_eq!(
            move_to_cell_sequence(&screen, state(&screen, (1, 1), false), (5, 1)),
            RIGHT.repeat(4)
        );
    }

    #[test]
    fn counts_a_wide_character_as_one_press() {
        let screen = Screen::new(10, &[("a漢字b", false)]);
        // Cells: a, 漢, (cover), 字, (cover), b: from column 0 to column 5 passes a, 漢, 字.
        assert_eq!(
            move_to_cell_sequence(&screen, state(&screen, (0, 0), false), (5, 0)),
            RIGHT.repeat(3)
        );
    }

    #[test]
    fn skips_wrapped_rows_when_moving_down_on_the_alternate_screen() {
        let screen = Screen::new(
            4,
            &[("abcd", false), ("ef", true), ("gh", false), ("ij", false)],
        );
        // From row 0 to row 2: the wrapped row 1 belongs to row 0's line, so one press down.
        let sequence = move_to_cell_sequence(&screen, state(&screen, (0, 0), false), (0, 2));
        assert!(sequence.starts_with(DOWN), "{sequence:?}");
        assert_eq!(sequence.matches(DOWN).count(), 1, "{sequence:?}");
    }
}
