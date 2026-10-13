//! Mouse selection in a terminal buffer: character, word (double click), line (triple click) and
//! column (Alt+drag) selection, and the text a selection copies.
//!
//! The rules follow xterm.js's `SelectionService`/`SelectionModel` (MIT): word separators,
//! wrapped-line handling and the copied text (`selectionText`). `client/src/terminal/
//! selection-text.ts` exists to repair an xterm cache bug that dropped explicit trailing spaces;
//! this implementation keeps them by construction.

use crate::buffer::{CellText, TextBuffer, translate_to_string};

/// xterm's default `wordSeparator`.
pub const WORD_SEPARATORS: &str = " ()[]{}',\"`";

/// A cell position in the buffer: a buffer line (scrollback first) and a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct BufferPoint {
    pub line: usize,
    pub col: usize,
}

impl BufferPoint {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionMode {
    /// Character-wise selection from a press point.
    #[default]
    Normal,
    /// Started by a double click: grows a word at a time.
    Word,
    /// Started by a triple click: grows a (wrapped) line at a time.
    Line,
    /// Alt+drag: a rectangle of columns.
    Column,
}

/// One selection in progress or made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub mode: SelectionMode,
    /// Where the selection began (start of the first word or line for word and line mode).
    start: BufferPoint,
    /// Columns the initial word or line covers, kept selected while dragging.
    start_length: usize,
    /// The point the pointer dragged to; `None` until it moved (a plain click selects nothing).
    end: Option<BufferPoint>,
    select_all: bool,
}

fn is_separator(cell: &CellText) -> bool {
    let text = if cell.text.is_empty() {
        " "
    } else {
        cell.text.as_str()
    };
    text.chars().count() == 1 && WORD_SEPARATORS.contains(text)
}

/// The columns `[start, end)` of the word under `point`, following wrapped lines. A point on
/// whitespace covers the run of spaces when `allow_whitespace` is set and is `None` otherwise (a
/// double click into blank space selects nothing).
pub fn word_at<B: TextBuffer + ?Sized>(
    buffer: &B,
    point: BufferPoint,
    allow_whitespace: bool,
) -> Option<(BufferPoint, BufferPoint)> {
    let cols = buffer.cols();
    if point.col >= cols {
        return None;
    }
    let cells = buffer.line_cells(point.line)?;
    let mut col = point.col.min(cells.len().saturating_sub(1));
    // A click on the right half of a wide character selects from its left half.
    while col > 0 && cells.get(col).is_some_and(|cell| cell.width == 0) {
        col -= 1;
    }
    let cell = cells.get(col)?;
    let is_space = cell.text.is_empty() || cell.text == " ";
    if is_space {
        let mut start = col;
        let mut end = col + 1;
        let blank = |c: &CellText| c.text.is_empty() || c.text == " ";
        while start > 0 && blank(&cells[start - 1]) {
            start -= 1;
        }
        while end < cells.len() && blank(&cells[end]) {
            end += 1;
        }
        return allow_whitespace.then_some((
            BufferPoint::new(point.line, start),
            BufferPoint::new(point.line, end),
        ));
    }
    if is_separator(cell) {
        return Some((
            BufferPoint::new(point.line, col),
            BufferPoint::new(point.line, col + usize::from(cell.width.max(1))),
        ));
    }
    let word_cell =
        |cell: &CellText| cell.width == 0 || (!is_separator(cell) && !cell.text.is_empty());
    // Expand left, following wrapped lines above.
    let mut start = BufferPoint::new(point.line, col);
    let mut line_cells = cells.clone();
    loop {
        while start.col > 0 && word_cell(&line_cells[start.col - 1]) {
            start.col -= 1;
        }
        if start.col == 0 && start.line > 0 && buffer.is_wrapped(start.line) {
            let Some(above) = buffer.line_cells(start.line - 1) else {
                break;
            };
            let last = above.len().min(cols);
            if last > 0 && word_cell(&above[last - 1]) {
                start = BufferPoint::new(start.line - 1, last);
                line_cells = above;
                continue;
            }
        }
        break;
    }
    // Expand right, following wrapped lines below.
    let mut end = BufferPoint::new(point.line, col + usize::from(cell.width.max(1)));
    let mut line_cells = cells;
    loop {
        while end.col < line_cells.len() && word_cell(&line_cells[end.col]) {
            end.col += 1;
        }
        if end.col >= line_cells.len().min(cols)
            && end.line + 1 < buffer.length()
            && buffer.is_wrapped(end.line + 1)
        {
            let Some(below) = buffer.line_cells(end.line + 1) else {
                break;
            };
            if below.first().is_some_and(word_cell) {
                end = BufferPoint::new(end.line + 1, 0);
                line_cells = below;
                continue;
            }
        }
        break;
    }
    Some((start, end))
}

/// The first and last buffer line of the logical (soft-wrapped) line containing `line`.
pub fn wrapped_range<B: TextBuffer + ?Sized>(buffer: &B, line: usize) -> (usize, usize) {
    let mut first = line;
    while first > 0 && buffer.is_wrapped(first) {
        first -= 1;
    }
    let mut last = line;
    while last + 1 < buffer.length() && buffer.is_wrapped(last + 1) {
        last += 1;
    }
    (first, last)
}

impl Selection {
    /// A press at `point` in `mode`. Word and line mode select immediately; normal and column mode
    /// wait for the pointer to move.
    pub fn begin<B: TextBuffer + ?Sized>(
        buffer: &B,
        mode: SelectionMode,
        point: BufferPoint,
    ) -> Option<Self> {
        match mode {
            SelectionMode::Normal | SelectionMode::Column => Some(Self {
                mode,
                start: point,
                start_length: 0,
                end: None,
                select_all: false,
            }),
            SelectionMode::Word => {
                let (start, end) = word_at(buffer, point, false)?;
                let length = Self::span(buffer, start, end);
                Some(Self {
                    mode,
                    start,
                    start_length: length,
                    end: None,
                    select_all: false,
                })
            }
            SelectionMode::Line => {
                let (first, last) = wrapped_range(buffer, point.line);
                let cols = buffer.cols();
                Some(Self {
                    mode,
                    start: BufferPoint::new(first, 0),
                    start_length: cols * (last - first + 1),
                    end: None,
                    select_all: false,
                })
            }
        }
    }

    /// Everything in the buffer.
    pub fn all() -> Self {
        Self {
            mode: SelectionMode::Normal,
            start: BufferPoint::default(),
            start_length: 0,
            end: None,
            select_all: true,
        }
    }

    fn span<B: TextBuffer + ?Sized>(buffer: &B, start: BufferPoint, end: BufferPoint) -> usize {
        let cols = buffer.cols().max(1);
        (end.line * cols + end.col).saturating_sub(start.line * cols + start.col)
    }

    /// Where the initial word or line ends. An end on a line boundary stays on the line it ends,
    /// as column `cols`, so copying it adds no empty row.
    fn start_end<B: TextBuffer + ?Sized>(&self, buffer: &B) -> BufferPoint {
        let cols = buffer.cols().max(1);
        let offset = self.start.line * cols + self.start.col + self.start_length;
        let (line, col) = (offset / cols, offset % cols);
        if col == 0 && line > 0 {
            BufferPoint::new(line - 1, cols)
        } else {
            BufferPoint::new(line, col)
        }
    }

    /// The pointer moved to `point` while the button is held.
    pub fn extend<B: TextBuffer + ?Sized>(&mut self, buffer: &B, point: BufferPoint) {
        self.select_all = false;
        match self.mode {
            SelectionMode::Normal | SelectionMode::Column => self.end = Some(point),
            SelectionMode::Word => {
                let word = word_at(buffer, point, true);
                self.end = Some(if point < self.start {
                    word.map_or(point, |(start, _)| start)
                } else {
                    word.map_or(point, |(_, end)| end)
                });
            }
            SelectionMode::Line => {
                let (first, last) = wrapped_range(buffer, point.line);
                self.end = Some(if point.line < self.start.line {
                    BufferPoint::new(first, 0)
                } else {
                    BufferPoint::new(last, buffer.cols())
                });
            }
        }
    }

    /// Whether anything is selected (a click without a drag selects nothing).
    pub fn is_empty<B: TextBuffer + ?Sized>(&self, buffer: &B) -> bool {
        match self.range(buffer) {
            Some((start, end)) => {
                start == end || (self.mode == SelectionMode::Column && start.col == end.col)
            }
            None => true,
        }
    }

    /// The selected range, start inclusive and end exclusive, in reading order. For a column
    /// selection the columns are the rectangle's left and right edges.
    pub fn range<B: TextBuffer + ?Sized>(&self, buffer: &B) -> Option<(BufferPoint, BufferPoint)> {
        if self.select_all {
            let last = buffer.length().checked_sub(1)?;
            return Some((
                BufferPoint::new(0, 0),
                BufferPoint::new(last, buffer.cols()),
            ));
        }
        let initial_end = if self.start_length > 0 {
            Some(self.start_end(buffer))
        } else {
            None
        };
        match (self.end, initial_end) {
            (None, None) => None,
            (None, Some(end)) => Some((self.start, end)),
            (Some(end), initial) => {
                if self.mode == SelectionMode::Column {
                    let (top, bottom) = if end.line < self.start.line {
                        (end.line, self.start.line)
                    } else {
                        (self.start.line, end.line)
                    };
                    let (left, right) = if end.col < self.start.col {
                        (end.col, self.start.col)
                    } else {
                        (self.start.col, end.col)
                    };
                    return Some((BufferPoint::new(top, left), BufferPoint::new(bottom, right)));
                }
                if end < self.start {
                    // Dragging backwards keeps the initial word or line selected.
                    let anchor = initial.unwrap_or(self.start);
                    Some((end, anchor))
                } else {
                    Some((self.start, initial.map_or(end, |initial| initial.max(end))))
                }
            }
        }
    }

    /// Whether a cell is selected, for painting.
    pub fn contains<B: TextBuffer + ?Sized>(&self, buffer: &B, point: BufferPoint) -> bool {
        let Some((start, end)) = self.range(buffer) else {
            return false;
        };
        if self.mode == SelectionMode::Column && !self.select_all {
            return point.line >= start.line
                && point.line <= end.line
                && point.col >= start.col
                && point.col < end.col;
        }
        point >= start && point < end
    }

    /// The copied text: wrapped rows join, other rows end with `line_ending` (`\r\n` on Windows,
    /// `\n` elsewhere), blank cells at the end of a row are dropped and non-breaking spaces become
    /// spaces.
    pub fn text<B: TextBuffer + ?Sized>(&self, buffer: &B, line_ending: &str) -> String {
        let Some((start, end)) = self.range(buffer) else {
            return String::new();
        };
        let row = |line: usize, from: usize, to: Option<usize>| -> String {
            buffer
                .line_cells(line)
                .map(|cells| translate_to_string(&cells, true, from, to))
                .unwrap_or_default()
        };
        let mut result: Vec<String> = Vec::new();
        if self.mode == SelectionMode::Column && !self.select_all {
            if start.col == end.col {
                return String::new();
            }
            for line in start.line..=end.line {
                result.push(row(line, start.col, Some(end.col)));
            }
        } else {
            result.push(row(
                start.line,
                start.col,
                (start.line == end.line).then_some(end.col),
            ));
            for line in start.line + 1..end.line {
                let text = row(line, 0, None);
                if buffer.is_wrapped(line) {
                    result.last_mut().expect("first row pushed").push_str(&text);
                } else {
                    result.push(text);
                }
            }
            if start.line != end.line {
                let text = row(end.line, 0, Some(end.col));
                if buffer.is_wrapped(end.line) {
                    result.last_mut().expect("first row pushed").push_str(&text);
                } else {
                    result.push(text);
                }
            }
        }
        result
            .iter()
            .map(|line| line.replace('\u{a0}', " "))
            .collect::<Vec<_>>()
            .join(line_ending)
    }

    /// Move the selection when `count` lines were dropped from the top of the scrollback. Returns
    /// `false` when the selection scrolled out entirely.
    pub fn shift_up(&mut self, count: usize) -> bool {
        if self.select_all || count == 0 {
            return true;
        }
        let end_line = self
            .end
            .map_or(self.start.line, |end| end.line.max(self.start.line));
        if end_line < count {
            return false;
        }
        self.start.line = self.start.line.saturating_sub(count);
        if let Some(end) = &mut self.end {
            end.line = end.line.saturating_sub(count);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::StringBuffer;

    fn select(buffer: &StringBuffer, start: (usize, usize), end: (usize, usize)) -> Selection {
        let mut selection = Selection::begin(
            buffer,
            SelectionMode::Normal,
            BufferPoint::new(start.0, start.1),
        )
        .unwrap();
        selection.extend(buffer, BufferPoint::new(end.0, end.1));
        selection
    }

    #[test]
    fn keeps_the_explicit_wrapped_space_from_issue_100() {
        let first = "cfm service create vs XXXXXXXXXX alarm-priority 2 remote-mep-aging on remote-mep-aging-time 300 ccm-interval ";
        let second = "100ms alarm-time 0";
        let buffer = StringBuffer::new(first.len(), &[(first, false), (second, true)]);
        let selection = select(&buffer, (0, 0), (1, second.len()));
        assert_eq!(selection.text(&buffer, "\n"), format!("{first}{second}"));
    }

    #[test]
    fn keeps_trailing_spaces_before_a_hard_line_break() {
        let buffer = StringBuffer::new(10, &[("abc ", false), ("def", false)]);
        assert_eq!(
            select(&buffer, (0, 0), (1, 3)).text(&buffer, "\n"),
            "abc \ndef"
        );
    }

    #[test]
    fn uses_windows_line_endings_when_asked() {
        let buffer = StringBuffer::new(10, &[("abc ", false), ("def", false)]);
        assert_eq!(
            select(&buffer, (0, 0), (1, 3)).text(&buffer, "\r\n"),
            "abc \r\ndef"
        );
    }

    #[test]
    fn copies_a_column_selection_row_by_row() {
        let buffer = StringBuffer::new(10, &[("abcdef", false), ("ghijkl", true)]);
        let mut selection =
            Selection::begin(&buffer, SelectionMode::Column, BufferPoint::new(0, 1)).unwrap();
        selection.extend(&buffer, BufferPoint::new(1, 4));
        assert_eq!(selection.text(&buffer, "\n"), "bcd\nhij");
        assert!(selection.contains(&buffer, BufferPoint::new(1, 2)));
        assert!(!selection.contains(&buffer, BufferPoint::new(1, 4)));
    }

    #[test]
    fn leaves_a_single_row_selection_untouched() {
        let buffer = StringBuffer::new(10, &[("abc ", false)]);
        assert_eq!(select(&buffer, (0, 0), (0, 4)).text(&buffer, "\n"), "abc ");
        assert_eq!(select(&buffer, (0, 0), (0, 10)).text(&buffer, "\n"), "abc ");
    }

    #[test]
    fn a_click_without_a_drag_selects_nothing() {
        let buffer = StringBuffer::new(10, &[("abc", false)]);
        let selection =
            Selection::begin(&buffer, SelectionMode::Normal, BufferPoint::new(0, 1)).unwrap();
        assert!(selection.is_empty(&buffer));
        assert_eq!(selection.text(&buffer, "\n"), "");
    }

    #[test]
    fn double_click_selects_a_word_across_a_soft_wrap() {
        let buffer = StringBuffer::new(8, &[("echo hel", false), ("lo world", true)]);
        let selection =
            Selection::begin(&buffer, SelectionMode::Word, BufferPoint::new(0, 6)).unwrap();
        assert_eq!(selection.text(&buffer, "\n"), "hello");
        let buffer = StringBuffer::new(20, &[("ls (foo/bar.txt)", false)]);
        let selection =
            Selection::begin(&buffer, SelectionMode::Word, BufferPoint::new(0, 6)).unwrap();
        assert_eq!(selection.text(&buffer, "\n"), "foo/bar.txt");
        assert!(Selection::begin(&buffer, SelectionMode::Word, BufferPoint::new(0, 18)).is_none());
    }

    #[test]
    fn dragging_after_a_double_click_grows_by_words() {
        let buffer = StringBuffer::new(20, &[("one two three", false)]);
        let mut selection =
            Selection::begin(&buffer, SelectionMode::Word, BufferPoint::new(0, 5)).unwrap();
        selection.extend(&buffer, BufferPoint::new(0, 9));
        assert_eq!(selection.text(&buffer, "\n"), "two three");
        selection.extend(&buffer, BufferPoint::new(0, 1));
        assert_eq!(selection.text(&buffer, "\n"), "one two");
    }

    #[test]
    fn triple_click_selects_the_whole_wrapped_line() {
        let buffer = StringBuffer::new(
            4,
            &[("$ ls", false), ("abcd", false), ("ef", true), ("x", false)],
        );
        let selection =
            Selection::begin(&buffer, SelectionMode::Line, BufferPoint::new(2, 1)).unwrap();
        assert_eq!(selection.text(&buffer, "\n"), "abcdef");
    }

    #[test]
    fn select_all_covers_the_buffer_and_follows_scrollback_trimming() {
        let buffer = StringBuffer::new(4, &[("a", false), ("b", false)]);
        assert_eq!(Selection::all().text(&buffer, "\n"), "a\nb");
        let mut selection = select(&buffer, (1, 0), (1, 1));
        assert!(selection.shift_up(1));
        assert_eq!(
            selection.range(&buffer),
            Some((BufferPoint::new(0, 0), BufferPoint::new(0, 1)))
        );
        assert!(!selection.shift_up(1));
    }
}
