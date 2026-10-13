//! Find in terminal: xterm's `SearchAddon` (MIT) over a [`TextBuffer`] — literal, whole-word and
//! regex search across soft-wrapped lines, next/previous from the current selection, highlights
//! for every match and the counts the search box shows.

use crate::buffer::TextBuffer;
use crate::keywords::translate_js_regex;
use crate::selection::BufferPoint;

/// Search highlight colors Muxus uses (`SEARCH_DECORATIONS` in `TerminalViewImpl.tsx`).
pub const MATCH_BACKGROUND: &str = "#594b24";
pub const MATCH_OVERVIEW_RULER: &str = "#d7b84b";
pub const ACTIVE_MATCH_BACKGROUND: &str = "#b77b23";
pub const ACTIVE_MATCH_OVERVIEW_RULER: &str = "#ffb74d";

/// Matches highlighted at most.
pub const HIGHLIGHT_LIMIT: usize = 1000;

const NON_WORD_CHARACTERS: &str = " ~!@#$%^&*()+`-=[]{}|\\;:\"',./<>?";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

/// A match: its first cell and its length in cells (spanning rows when it wraps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub row: usize,
    pub col: usize,
    pub size: usize,
}

impl SearchResult {
    /// The cells the match covers as a selection range (end exclusive).
    pub fn range(&self, cols: usize) -> (BufferPoint, BufferPoint) {
        let end = self.row * cols + self.col + self.size;
        let (end_row, end_col) = (end / cols, end % cols);
        let end = if end_col == 0 && end_row > self.row {
            BufferPoint::new(end_row - 1, cols)
        } else {
            BufferPoint::new(end_row, end_col)
        };
        (BufferPoint::new(self.row, self.col), end)
    }

    /// Whether a cell is part of the match.
    pub fn contains(&self, point: BufferPoint, cols: usize) -> bool {
        let start = self.row * cols + self.col;
        let index = point.line * cols + point.col;
        point.col < cols && index >= start && index < start + self.size
    }
}

/// A logical line: the text of a row and the rows that continue it, and where each cell's text
/// starts.
struct LogicalLine {
    text: String,
    /// `(byte offset, row offset, column)` for every cell that has text, in order.
    cells: Vec<(usize, usize, usize)>,
}

fn logical_line<B: TextBuffer + ?Sized>(buffer: &B, row: usize) -> Option<LogicalLine> {
    let mut text = String::new();
    let mut cells = Vec::new();
    let mut line = row;
    loop {
        let row_cells = buffer.line_cells(line)?;
        let wraps = line + 1 < buffer.length() && buffer.is_wrapped(line + 1);
        let mut end = if wraps {
            row_cells.len()
        } else {
            crate::buffer::trimmed_length(&row_cells)
        };
        if wraps
            && let Some(last) = row_cells.last()
            && last.text.is_empty()
            && last.width == 1
            && buffer
                .line_cells(line + 1)
                .and_then(|next| next.first().map(|c| c.width == 2))
                == Some(true)
        {
            // A wide character that did not fit wrapped to the next row.
            end = end.saturating_sub(1);
        }
        let mut col = 0;
        while col < end.min(row_cells.len()) {
            let cell = &row_cells[col];
            cells.push((text.len(), line - row, col));
            text.push_str(if cell.text.is_empty() {
                " "
            } else {
                &cell.text
            });
            col += usize::from(cell.width.max(1));
        }
        if !wraps {
            break;
        }
        line += 1;
    }
    Some(LogicalLine { text, cells })
}

impl LogicalLine {
    /// Byte offset of the cell at `row_offset`/`col` (or the end of the text past it).
    fn offset_of(&self, row_offset: usize, col: usize) -> usize {
        self.cells
            .iter()
            .find(|(_, r, c)| (*r, *c) >= (row_offset, col))
            .map_or(self.text.len(), |(offset, _, _)| *offset)
    }

    /// The cell holding byte `offset`, or just past the last cell.
    fn cell_at(&self, offset: usize, cols: usize) -> (usize, usize) {
        match self
            .cells
            .iter()
            .rev()
            .find(|(start, _, _)| *start <= offset)
        {
            Some((start, row, col)) if *start == offset => (*row, *col),
            Some((_, row, col)) => {
                // Inside a multi-byte cell: count it as that cell.
                let _ = cols;
                (*row, *col)
            }
            None => (0, 0),
        }
    }

    /// The cell just past the text ending at byte `end`.
    fn cell_after(&self, end: usize, cols: usize) -> (usize, usize) {
        match self.cells.iter().find(|(start, _, _)| *start >= end) {
            Some((_, row, col)) => (*row, *col),
            None => {
                let Some((_, row, col)) = self.cells.last() else {
                    return (0, 0);
                };
                let last_width = self.text[self.cells.last().map_or(0, |c| c.0)..]
                    .chars()
                    .next()
                    .map_or(1, |c| {
                        unicode_width::UnicodeWidthChar::width(c)
                            .unwrap_or(1)
                            .max(1)
                    });
                let next = col + last_width;
                if next >= cols {
                    (row + 1, next - cols)
                } else {
                    (*row, next)
                }
            }
        }
    }
}

fn is_whole_word(line: &str, start: usize, end: usize) -> bool {
    let before = line[..start].chars().next_back();
    let after = line[end..].chars().next();
    before.is_none_or(|c| NON_WORD_CHARACTERS.contains(c))
        && after.is_none_or(|c| NON_WORD_CHARACTERS.contains(c))
}

fn compile(term: &str, options: &SearchOptions) -> Option<fancy_regex::Regex> {
    let translated = translate_js_regex(term);
    let source = if options.case_sensitive {
        translated
    } else {
        format!("(?i){translated}")
    };
    fancy_regex::RegexBuilder::new(&source)
        .backtrack_limit(200_000)
        .build()
        .ok()
}

/// Find `term` in the logical line starting at `row`, at or after column `col` (or, in reverse,
/// the last match ending before it).
fn find_in_line<B: TextBuffer + ?Sized>(
    buffer: &B,
    term: &str,
    row: usize,
    col: usize,
    options: &SearchOptions,
    reverse: bool,
) -> Option<SearchResult> {
    let cols = buffer.cols().max(1);
    // Only search from the first row of a wrapped line.
    if buffer.is_wrapped(row) {
        if reverse {
            return None;
        }
        let mut start = row;
        let mut col = col;
        while start > 0 && buffer.is_wrapped(start) {
            start -= 1;
            col += cols;
        }
        return find_in_line(buffer, term, start, col, options, false);
    }
    let line = logical_line(buffer, row)?;
    let offset = line.offset_of(col / cols, col % cols).min(line.text.len());
    // Candidate matches in search order; the first whole word wins. (xterm stops at the first
    // candidate on a line even when it is not a whole word, missing later ones on that line.)
    let candidates: Vec<(usize, usize)> = if options.regex {
        let pattern = compile(term, options)?;
        if reverse {
            let haystack = &line.text[..offset];
            let mut found_all = Vec::new();
            let mut from = 0;
            while from <= haystack.len() {
                let Ok(Some(found)) = pattern.find_from_pos(haystack, from) else {
                    break;
                };
                if found.end() > found.start() {
                    found_all.push((found.start(), found.end()));
                }
                // Continue one character after the match start.
                from = found.start()
                    + haystack[found.start()..]
                        .chars()
                        .next()
                        .map_or(1, char::len_utf8);
            }
            found_all.reverse();
            found_all
        } else {
            let mut found_all = Vec::new();
            let mut from = offset;
            while from <= line.text.len() {
                let Ok(Some(found)) = pattern.find_from_pos(&line.text, from) else {
                    break;
                };
                if found.end() > found.start() {
                    found_all.push((found.start(), found.end()));
                    if !options.whole_word {
                        break;
                    }
                    from = found.end();
                } else {
                    from = found.start()
                        + line.text[found.start()..]
                            .chars()
                            .next()
                            .map_or(1, char::len_utf8);
                }
            }
            found_all
        }
    } else {
        let (haystack, needle) = if options.case_sensitive {
            (line.text.clone(), term.to_string())
        } else {
            (line.text.to_lowercase(), term.to_lowercase())
        };
        if haystack.len() != line.text.len() {
            // Lower-casing changed byte lengths; fall back to a case-sensitive scan.
            return find_in_line(
                buffer,
                term,
                row,
                col,
                &SearchOptions {
                    case_sensitive: true,
                    ..*options
                },
                reverse,
            );
        }
        let mut found_all = Vec::new();
        if reverse {
            let mut end = offset;
            while let Some(found) = haystack[..end].rfind(&needle) {
                found_all.push((found, found + needle.len()));
                if found == 0 {
                    break;
                }
                end = found + needle.len() - 1;
                while !haystack.is_char_boundary(end) {
                    end -= 1;
                }
            }
        } else {
            let mut from = offset;
            while let Some(found) = haystack.get(from..).and_then(|rest| rest.find(&needle)) {
                let start = from + found;
                found_all.push((start, start + needle.len()));
                from = start + needle.chars().next().map_or(1, char::len_utf8);
            }
        }
        found_all
    };
    let (start, end) = candidates
        .into_iter()
        .find(|(start, end)| !options.whole_word || is_whole_word(&line.text, *start, *end))?;
    let (start_row, start_col) = line.cell_at(start, cols);
    let (end_row, end_col) = line.cell_after(end, cols);
    let size = (end_row * cols + end_col).saturating_sub(start_row * cols + start_col);
    Some(SearchResult {
        row: row + start_row,
        col: start_col,
        size,
    })
}

fn find_forward<B: TextBuffer + ?Sized>(
    buffer: &B,
    term: &str,
    start_row: usize,
    start_col: usize,
    options: &SearchOptions,
) -> Option<SearchResult> {
    if let Some(result) = find_in_line(buffer, term, start_row, start_col, options, false) {
        return Some(result);
    }
    (start_row + 1..buffer.length())
        .find_map(|row| find_in_line(buffer, term, row, 0, options, false))
}

/// Every match from the top, up to [`HIGHLIGHT_LIMIT`].
pub fn find_all<B: TextBuffer + ?Sized>(
    buffer: &B,
    term: &str,
    options: &SearchOptions,
) -> Vec<SearchResult> {
    let mut results: Vec<SearchResult> = Vec::new();
    if term.is_empty() {
        return results;
    }
    let cols = buffer.cols().max(1);
    let mut next = find_forward(buffer, term, 0, 0, options);
    while let Some(result) = next {
        if results
            .last()
            .is_some_and(|previous| previous.row == result.row && previous.col == result.col)
            || results.len() >= HIGHLIGHT_LIMIT
        {
            break;
        }
        let mut next_col = result.col + result.size;
        let mut next_row = result.row;
        if next_col >= cols {
            next_row += next_col / cols;
            next_col %= cols;
        }
        results.push(result);
        next = find_forward(buffer, term, next_row, next_col, options);
    }
    results
}

/// The search box's state for one terminal.
#[derive(Debug, Clone, Default)]
pub struct TerminalSearch {
    cached_term: Option<String>,
    options: SearchOptions,
    results: Vec<SearchResult>,
    active: Option<SearchResult>,
}

impl TerminalSearch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop the highlights and the active match.
    pub fn clear(&mut self) {
        self.cached_term = None;
        self.results.clear();
        self.active = None;
    }

    pub fn results(&self) -> &[SearchResult] {
        &self.results
    }

    pub fn active(&self) -> Option<&SearchResult> {
        self.active.as_ref()
    }

    /// `(index, count)` for the "3/12" label: the active match's position among the highlights,
    /// -1 when there is none.
    pub fn result_index(&self) -> (isize, usize) {
        let index = self
            .active
            .as_ref()
            .and_then(|active| self.results.iter().position(|r| r == active))
            .map_or(-1, |i| i as isize);
        (index, self.results.len())
    }

    fn update_highlights<B: TextBuffer + ?Sized>(
        &mut self,
        buffer: &B,
        term: &str,
        options: &SearchOptions,
    ) {
        if self.cached_term.as_deref() != Some(term) || self.options != *options {
            self.results = find_all(buffer, term, options);
        }
        self.options = *options;
    }

    /// Re-run the highlights after the buffer changed (xterm does this on output), keeping the
    /// active match where it still exists.
    pub fn refresh<B: TextBuffer + ?Sized>(&mut self, buffer: &B) {
        let Some(term) = self.cached_term.clone() else {
            return;
        };
        let options = self.options;
        self.results = find_all(buffer, &term, &options);
        if let Some(active) = &self.active
            && !self.results.contains(active)
        {
            self.active = None;
        }
    }

    /// Find the next match after the selection (from its start while the term is being typed, so
    /// the match grows in place). Returns the match to select.
    pub fn find_next<B: TextBuffer + ?Sized>(
        &mut self,
        buffer: &B,
        term: &str,
        options: &SearchOptions,
        selection: Option<(BufferPoint, BufferPoint)>,
    ) -> Option<SearchResult> {
        if term.is_empty() {
            self.clear();
            return None;
        }
        self.update_highlights(buffer, term, options);
        let (start_row, start_col) = match selection {
            Some((_, end)) if self.cached_term.as_deref() == Some(term) => (end.line, end.col),
            Some((start, _)) => (start.line, start.col),
            None => (0, 0),
        };
        let cols = buffer.cols().max(1);
        let start_col = start_col.min(cols - 1);
        let mut result = find_in_line(buffer, term, start_row, start_col, options, false);
        if result.is_none() {
            result = (start_row + 1..buffer.length())
                .find_map(|row| find_in_line(buffer, term, row, 0, options, false));
        }
        if result.is_none() && start_row != 0 {
            result =
                (0..start_row).find_map(|row| find_in_line(buffer, term, row, 0, options, false));
        }
        if result.is_none()
            && let Some((start, _)) = selection
        {
            result = find_in_line(buffer, term, start.line, 0, options, false);
        }
        self.cached_term = Some(term.to_string());
        self.active = result.clone();
        result
    }

    /// Find the previous match before the selection, wrapping around at the top.
    pub fn find_previous<B: TextBuffer + ?Sized>(
        &mut self,
        buffer: &B,
        term: &str,
        options: &SearchOptions,
        selection: Option<(BufferPoint, BufferPoint)>,
    ) -> Option<SearchResult> {
        if term.is_empty() {
            self.clear();
            return None;
        }
        self.update_highlights(buffer, term, options);
        let cols = buffer.cols().max(1);
        let last_row = buffer.length().saturating_sub(1);
        let mut start_row = last_row;
        let mut start_col = cols;
        let mut result = None;
        if let Some((start, end)) = selection {
            start_row = start.line;
            start_col = start.col;
            if self.cached_term.as_deref() != Some(term) {
                // Try to grow the selection to the right first.
                result = find_in_line(
                    buffer,
                    term,
                    start.line,
                    start.col.min(cols - 1),
                    options,
                    false,
                );
                if result.is_none() {
                    start_row = end.line;
                    start_col = end.col;
                }
            }
        }
        if result.is_none() {
            result = find_in_line(buffer, term, start_row, start_col, options, true);
        }
        if result.is_none() {
            let col = start_col.max(cols);
            result = (0..start_row)
                .rev()
                .find_map(|row| find_in_line(buffer, term, row, col, options, true));
        }
        if result.is_none() && start_row != last_row {
            result = (start_row..=last_row)
                .rev()
                .find_map(|row| find_in_line(buffer, term, row, cols, options, true));
        }
        self.cached_term = Some(term.to_string());
        self.active = result.clone();
        result
    }

    /// Buffer lines with a highlighted match (overview-ruler marks), without duplicates.
    pub fn highlighted_lines(&self) -> Vec<usize> {
        let mut lines: Vec<usize> = self.results.iter().map(|r| r.row).collect();
        lines.dedup();
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::StringBuffer;

    fn buffer() -> StringBuffer {
        StringBuffer::new(
            10,
            &[
                ("error one", false),
                ("ok", false),
                ("an error w", false),
                ("rapped err", true),
                ("or", true),
                ("ERROR", false),
            ],
        )
    }

    #[test]
    fn finds_every_match_across_soft_wraps() {
        let results = find_all(&buffer(), "error", &SearchOptions::default());
        let positions: Vec<(usize, usize, usize)> =
            results.iter().map(|r| (r.row, r.col, r.size)).collect();
        assert_eq!(positions, [(0, 0, 5), (2, 3, 5), (3, 7, 5), (5, 0, 5)]);
        let sensitive = find_all(
            &buffer(),
            "ERROR",
            &SearchOptions {
                case_sensitive: true,
                ..Default::default()
            },
        );
        assert_eq!(sensitive.len(), 1);
    }

    #[test]
    fn honours_whole_word_and_regex_options() {
        let buffer = StringBuffer::new(20, &[("errors error", false)]);
        let whole = find_all(
            &buffer,
            "error",
            &SearchOptions {
                whole_word: true,
                ..Default::default()
            },
        );
        assert_eq!(whole.iter().map(|r| r.col).collect::<Vec<_>>(), [7]);
        let regex = find_all(
            &buffer,
            r"err\w+",
            &SearchOptions {
                regex: true,
                ..Default::default()
            },
        );
        assert_eq!(
            regex.iter().map(|r| (r.col, r.size)).collect::<Vec<_>>(),
            [(0, 6), (7, 5)]
        );
    }

    #[test]
    fn steps_through_matches_and_wraps_around() {
        let buffer = buffer();
        let mut search = TerminalSearch::new();
        let options = SearchOptions::default();
        let first = search.find_next(&buffer, "error", &options, None).unwrap();
        assert_eq!((first.row, first.col), (0, 0));
        assert_eq!(search.result_index(), (0, 4));
        let second = search
            .find_next(&buffer, "error", &options, Some(first.range(10)))
            .unwrap();
        assert_eq!((second.row, second.col), (2, 3));
        let back = search
            .find_previous(&buffer, "error", &options, Some(second.range(10)))
            .unwrap();
        assert_eq!((back.row, back.col), (0, 0));
        let wrapped = search
            .find_previous(&buffer, "error", &options, Some(back.range(10)))
            .unwrap();
        assert_eq!((wrapped.row, wrapped.col), (5, 0));
        assert_eq!(search.result_index(), (3, 4));
    }

    #[test]
    fn grows_the_match_in_place_while_typing() {
        let buffer = buffer();
        let mut search = TerminalSearch::new();
        let options = SearchOptions::default();
        let partial = search.find_next(&buffer, "err", &options, None).unwrap();
        let full = search
            .find_next(&buffer, "error", &options, Some(partial.range(10)))
            .unwrap();
        assert_eq!((full.row, full.col, full.size), (0, 0, 5));
    }

    #[test]
    fn maps_a_wrapped_match_to_cells() {
        let result = SearchResult {
            row: 3,
            col: 8,
            size: 5,
        };
        assert_eq!(
            result.range(10),
            (BufferPoint::new(3, 8), BufferPoint::new(4, 3))
        );
        assert!(result.contains(BufferPoint::new(4, 2), 10));
        assert!(!result.contains(BufferPoint::new(4, 3), 10));
    }
}
