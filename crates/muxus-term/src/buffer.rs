//! A read-only view of a terminal buffer for the text utilities (links, keyword highlighting,
//! search, selection, export). The emulator implements it; tests implement it over plain strings.

use unicode_width::UnicodeWidthChar;

/// One cell as the text utilities see it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CellText {
    /// The cell's grapheme; empty for a blank cell.
    pub text: String,
    /// Columns the character occupies: 1, 2 for a wide character, 0 for the cell a wide character
    /// to its left covers.
    pub width: u8,
}

impl CellText {
    pub fn new(text: impl Into<String>, width: u8) -> Self {
        Self {
            text: text.into(),
            width,
        }
    }

    pub fn blank() -> Self {
        Self {
            text: String::new(),
            width: 1,
        }
    }
}

/// Lines of a terminal buffer, scrollback first.
pub trait TextBuffer {
    /// Lines in the buffer: scrollback followed by the screen.
    fn length(&self) -> usize;
    /// Columns of the screen.
    fn cols(&self) -> usize;
    /// The line continues the one above it (a soft wrap, not a line break).
    fn is_wrapped(&self, line: usize) -> bool;
    /// The cells of one line, or `None` past the end.
    fn line_cells(&self, line: usize) -> Option<Vec<CellText>>;
}

/// Index just past the last cell with content, counting a wide character's full width.
pub fn trimmed_length(cells: &[CellText]) -> usize {
    for index in (0..cells.len()).rev() {
        let cell = &cells[index];
        if !cell.text.is_empty() {
            return index + usize::from(cell.width.max(1));
        }
    }
    0
}

/// The text of `cells[start_col..end_col]` as xterm's `translateToString`: blank cells read as
/// spaces, the cell a wide character covers is skipped, and `trim_right` stops after the last cell
/// with content.
pub fn translate_to_string(
    cells: &[CellText],
    trim_right: bool,
    start_col: usize,
    end_col: Option<usize>,
) -> String {
    let mut end = end_col.unwrap_or(cells.len()).min(cells.len());
    if trim_right {
        end = end.min(trimmed_length(cells));
    }
    let mut result = String::new();
    let mut column = start_col;
    while column < end {
        let cell = &cells[column];
        if cell.text.is_empty() {
            result.push(' ');
        } else {
            result.push_str(&cell.text);
        }
        column += usize::from(cell.width.max(1));
    }
    result
}

/// The text of one buffer line with blank cells trimmed on the right (`translateToString(true)`).
/// Spaces a program wrote are content and stay.
pub fn line_text<B: TextBuffer + ?Sized>(buffer: &B, line: usize) -> Option<String> {
    let cells = buffer.line_cells(line)?;
    Some(translate_to_string(&cells, true, 0, None))
}

/// Cells for a plain string as a program would write it (spaces are content), with Unicode
/// widths.
pub fn cells_from_str(text: &str) -> Vec<CellText> {
    let mut cells = Vec::new();
    for c in text.chars() {
        let width = c.width().unwrap_or(0);
        match width {
            0 => {
                if let Some(last) = cells
                    .iter_mut()
                    .rev()
                    .find(|cell: &&mut CellText| cell.width > 0)
                {
                    last.text.push(c);
                }
            }
            2 => {
                cells.push(CellText::new(c, 2));
                cells.push(CellText::new("", 0));
            }
            _ => cells.push(CellText::new(c, 1)),
        }
    }
    cells
}

/// A buffer over plain strings: each entry is a line and whether it soft-wraps from the one before.
#[derive(Debug, Clone, Default)]
pub struct StringBuffer {
    pub lines: Vec<(String, bool)>,
    pub cols: usize,
}

impl StringBuffer {
    pub fn new(cols: usize, lines: &[(&str, bool)]) -> Self {
        Self {
            lines: lines
                .iter()
                .map(|(text, wrapped)| (text.to_string(), *wrapped))
                .collect(),
            cols,
        }
    }
}

impl TextBuffer for StringBuffer {
    fn length(&self) -> usize {
        self.lines.len()
    }

    fn cols(&self) -> usize {
        self.cols
    }

    fn is_wrapped(&self, line: usize) -> bool {
        self.lines.get(line).is_some_and(|(_, wrapped)| *wrapped)
    }

    fn line_cells(&self, line: usize) -> Option<Vec<CellText>> {
        let (text, _) = self.lines.get(line)?;
        let mut cells = cells_from_str(text);
        while cells.len() < self.cols {
            cells.push(CellText::blank());
        }
        Some(cells)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_cells_like_xterm() {
        let mut cells = cells_from_str("a 中b");
        cells.extend([CellText::blank(), CellText::blank()]);
        assert_eq!(translate_to_string(&cells, false, 0, None), "a 中b  ");
        assert_eq!(translate_to_string(&cells, true, 0, None), "a 中b");
        assert_eq!(translate_to_string(&cells, true, 2, Some(4)), "中");
        assert_eq!(trimmed_length(&cells), 5);
    }

    #[test]
    fn trims_blank_cells_but_keeps_written_spaces() {
        let buffer = StringBuffer::new(6, &[("abc ", false), ("d", true)]);
        assert_eq!(line_text(&buffer, 0).as_deref(), Some("abc "));
        assert_eq!(line_text(&buffer, 1).as_deref(), Some("d"));
    }
}
