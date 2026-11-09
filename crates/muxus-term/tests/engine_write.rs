//! How the engine writes text into the grid, through the emulator a tab runs: wrapping, wide
//! characters, combining marks and the edits that cut a wide character in half.

use muxus_term::buffer::line_text;
use muxus_term::palette::terminal_scheme;
use muxus_term::terminal::{Emulator, EmulatorConfig};

fn emulator(cols: u16, rows: u16) -> Emulator {
    Emulator::new(
        EmulatorConfig {
            cols,
            rows,
            scrollback: 100,
        },
        &terminal_scheme(None, &[]).theme,
    )
}

/// The text of screen row `row`.
fn row(term: &Emulator, row: usize) -> String {
    line_text(term, term.scrollback_len() + row).unwrap()
}

/// Each cell of screen row `row` as its text and width (0 for the cell a wide character covers).
fn cells(term: &Emulator, row: usize) -> Vec<(String, u8)> {
    term.styled_line(term.scrollback_len() + row)
        .unwrap()
        .into_iter()
        .map(|cell| (cell.text, cell.width))
        .collect()
}

/// The cursor as (column, screen row).
fn cursor(term: &Emulator) -> (usize, usize) {
    let cursor = term.cursor();
    (cursor.col, cursor.line - term.scrollback_len())
}

#[test]
fn a_wide_character_in_the_last_column_wraps_the_next_character() {
    let mut term = emulator(6, 3);
    term.write("abcd中x".as_bytes());
    assert_eq!(row(&term, 0), "abcd中");
    assert_eq!(row(&term, 1), "x");
    assert_eq!(cursor(&term), (1, 1));
}

#[test]
fn a_combining_mark_after_the_last_column_joins_the_character_there() {
    // NFD text (macOS file names): the accent belongs to the c in the last column.
    let mut term = emulator(3, 3);
    term.write("abc\u{301}d".as_bytes());
    assert_eq!(row(&term, 0), "ab\u{107}");
    assert_eq!(row(&term, 1), "d");
}
