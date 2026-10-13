//! Serializing the buffer: the replayable ANSI form persisted as a scrollback snapshot, and the
//! HTML export. Adapted from xterm.js's `SerializeAddon` (MIT), so snapshots written by Muxus and
//! Muxus-rs replay the same way in both.

use crate::buffer::TextBuffer;
use crate::style::{CellColor, CellStyle, StyledCell, UnderlineStyle};
use crate::terminal::Emulator;

fn equal_fg(a: &CellStyle, b: &CellStyle) -> bool {
    a.fg == b.fg
}

fn equal_bg(a: &CellStyle, b: &CellStyle) -> bool {
    a.bg == b.bg
}

fn attributes_equal(a: &CellStyle, b: &CellStyle) -> bool {
    equal_fg(a, b) && equal_bg(a, b) && a.flags_equal(b)
}

fn push_color(sgr: &mut Vec<String>, color: CellColor, foreground: bool) {
    match color {
        CellColor::Rgb(r, g, b) => sgr.extend([
            if foreground { "38" } else { "48" }.into(),
            "2".into(),
            r.to_string(),
            g.to_string(),
            b.to_string(),
        ]),
        CellColor::Palette(index) if index >= 16 => sgr.extend([
            if foreground { "38" } else { "48" }.into(),
            "5".into(),
            index.to_string(),
        ]),
        CellColor::Palette(index) => {
            let base = match (foreground, index & 8 != 0) {
                (true, false) => 30,
                (true, true) => 90,
                (false, false) => 40,
                (false, true) => 100,
            };
            sgr.push((base + u32::from(index & 7)).to_string());
        }
        CellColor::Default => sgr.push(if foreground { "39" } else { "49" }.into()),
    }
}

/// The SGR parameters that turn `old` into `cell` (xterm's `_diffStyle`).
fn diff_style(cell: &CellStyle, old: &CellStyle) -> Vec<String> {
    let mut sgr = Vec::new();
    if attributes_equal(cell, old) {
        return sgr;
    }
    if cell.is_default() {
        if !old.is_default() {
            sgr.push("0".into());
        }
        return sgr;
    }
    if !equal_fg(cell, old) {
        push_color(&mut sgr, cell.fg, true);
    }
    if !equal_bg(cell, old) {
        push_color(&mut sgr, cell.bg, false);
    }
    if !cell.flags_equal(old) {
        if cell.inverse != old.inverse {
            sgr.push(if cell.inverse { "7" } else { "27" }.into());
        }
        if cell.bold != old.bold {
            sgr.push(if cell.bold { "1" } else { "22" }.into());
        }
        let underline_equal = (cell.underline == UnderlineStyle::None
            && old.underline == UnderlineStyle::None)
            || (cell.underline == old.underline && cell.underline_color == old.underline_color);
        if !underline_equal {
            match cell.underline {
                UnderlineStyle::None => sgr.push("24".into()),
                UnderlineStyle::Single if cell.underline_color.is_none() => sgr.push("4".into()),
                style => {
                    sgr.push(format!("4:{}", style.sgr()));
                    match cell.underline_color {
                        Some(CellColor::Rgb(r, g, b)) => sgr.push(format!("58:2::{r}:{g}:{b}")),
                        Some(CellColor::Palette(index)) => sgr.push(format!("58:5:{index}")),
                        _ => {}
                    }
                }
            }
        }
        if cell.overline != old.overline {
            sgr.push(if cell.overline { "53" } else { "55" }.into());
        }
        if cell.blink != old.blink {
            sgr.push(if cell.blink { "5" } else { "25" }.into());
        }
        if cell.invisible != old.invisible {
            sgr.push(if cell.invisible { "8" } else { "28" }.into());
        }
        if cell.italic != old.italic {
            sgr.push(if cell.italic { "3" } else { "23" }.into());
        }
        if cell.dim != old.dim {
            sgr.push(if cell.dim { "2" } else { "22" }.into());
        }
        if cell.strikethrough != old.strikethrough {
            sgr.push(if cell.strikethrough { "9" } else { "29" }.into());
        }
    }
    sgr
}

/// One line of cells as SGR and text from the default style, without moving the cursor.
pub fn serialize_line_cells(cells: &[StyledCell]) -> String {
    let mut out = String::new();
    let mut current = CellStyle::default();
    let mut blanks = 0usize;
    for cell in cells {
        if cell.width == 0 {
            continue;
        }
        if cell.text.is_empty() && cell.style.bg == current.bg {
            blanks += usize::from(cell.width);
            continue;
        }
        let sgr = diff_style(&cell.style, &current);
        if blanks > 0 {
            out.push_str(&format!("\x1b[{blanks}C"));
            blanks = 0;
        }
        if !sgr.is_empty() {
            out.push_str(&format!("\x1b[{}m", sgr.join(";")));
            current = cell.style;
        }
        out.push_str(if cell.text.is_empty() {
            " "
        } else {
            &cell.text
        });
    }
    out
}

struct StringSerializer<'a> {
    term: &'a Emulator,
    rows: Vec<String>,
    separators: Vec<String>,
    current_row: String,
    null_cell_count: usize,
    cursor_style: CellStyle,
    cursor_style_row: usize,
    cursor_style_col: usize,
    background_cell: CellStyle,
    first_row: usize,
    last_cursor_row: usize,
    last_cursor_col: usize,
    last_content_cursor_row: usize,
    last_content_cursor_col: usize,
}

impl<'a> StringSerializer<'a> {
    fn cell(&self, row: usize, col: usize) -> StyledCell {
        self.term
            .styled_line(row)
            .and_then(|line| line.get(col).cloned())
            .unwrap_or_default()
    }

    fn next_cell(&mut self, cell: &StyledCell, row: usize, col: usize) {
        if cell.width == 0 {
            return;
        }
        let is_empty = cell.text.is_empty();
        let sgr = diff_style(&cell.style, &self.cursor_style);
        let style_changed = if is_empty {
            !equal_bg(&self.cursor_style, &cell.style)
        } else {
            !sgr.is_empty()
        };
        if style_changed {
            if self.null_cell_count > 0 {
                if !equal_bg(&self.cursor_style, &self.background_cell) {
                    self.current_row += &format!("\x1b[{}X", self.null_cell_count);
                }
                self.current_row += &format!("\x1b[{}C", self.null_cell_count);
                self.null_cell_count = 0;
            }
            self.last_content_cursor_row = row;
            self.last_cursor_row = row;
            self.last_content_cursor_col = col;
            self.last_cursor_col = col;
            self.current_row += &format!("\x1b[{}m", sgr.join(";"));
            self.cursor_style = cell.style;
            self.cursor_style_row = row;
            self.cursor_style_col = col;
        }
        if is_empty {
            self.null_cell_count += usize::from(cell.width);
        } else {
            if self.null_cell_count > 0 {
                if equal_bg(&self.cursor_style, &self.background_cell) {
                    self.current_row += &format!("\x1b[{}C", self.null_cell_count);
                } else {
                    self.current_row += &format!("\x1b[{}X", self.null_cell_count);
                    self.current_row += &format!("\x1b[{}C", self.null_cell_count);
                }
                self.null_cell_count = 0;
            }
            self.current_row += &cell.text;
            self.last_content_cursor_row = row;
            self.last_cursor_row = row;
            self.last_content_cursor_col = col + usize::from(cell.width);
            self.last_cursor_col = self.last_content_cursor_col;
        }
    }

    fn row_end(&mut self, row: usize, is_last_row: bool) {
        if self.null_cell_count > 0 && !equal_bg(&self.cursor_style, &self.background_cell) {
            self.current_row += &format!("\x1b[{}X", self.null_cell_count);
        }
        let mut separator = String::new();
        if !is_last_row {
            if row - self.first_row >= self.term.rows() {
                self.background_cell = self
                    .cell(self.cursor_style_row, self.cursor_style_col)
                    .style;
            }
            if !self.term.is_wrapped(row + 1) {
                separator = "\r\n".into();
                self.last_cursor_row = row + 1;
                self.last_cursor_col = 0;
            } else {
                let line = self.term.styled_line(row).unwrap_or_default();
                let length = line.len();
                let last = line
                    .get(length.wrapping_sub(1))
                    .cloned()
                    .unwrap_or_default();
                let second_last = line
                    .get(length.wrapping_sub(2))
                    .cloned()
                    .unwrap_or_default();
                let next_first = self.cell(row + 1, 0);
                let next_double = next_first.width > 1;
                let mut valid = false;
                // xterm's operator precedence: `chars && double ? count <= 1 : count <= 0`.
                let condition = if !next_first.text.is_empty() && next_double {
                    self.null_cell_count <= 1
                } else {
                    self.null_cell_count == 0
                };
                if condition {
                    if (!last.text.is_empty() || last.width == 0)
                        && equal_bg(&last.style, &next_first.style)
                    {
                        valid = true;
                    }
                    if next_double
                        && (!second_last.text.is_empty() || second_last.width == 0)
                        && equal_bg(&last.style, &next_first.style)
                        && equal_bg(&second_last.style, &next_first.style)
                    {
                        valid = true;
                    }
                }
                if !valid {
                    separator = "-".repeat(self.null_cell_count + 1);
                    separator += "\x1b[1D\x1b[1X";
                    if self.null_cell_count > 0 {
                        separator += "\x1b[A";
                        separator += &format!("\x1b[{}C", length - self.null_cell_count);
                        separator += &format!("\x1b[{}X", self.null_cell_count);
                        separator += &format!("\x1b[{}D", length - self.null_cell_count);
                        separator += "\x1b[B";
                    }
                    self.last_content_cursor_row = row + 1;
                    self.last_content_cursor_col = 0;
                    self.last_cursor_row = row + 1;
                    self.last_cursor_col = 0;
                }
            }
        }
        self.rows.push(std::mem::take(&mut self.current_row));
        self.separators.push(separator);
        self.null_cell_count = 0;
    }

    fn finish(mut self, exclude_final_cursor_position: bool) -> String {
        let mut row_end = self.rows.len();
        if self.term.length() - self.first_row <= self.term.rows() {
            row_end = self.last_content_cursor_row + 1 - self.first_row;
            self.last_cursor_col = self.last_content_cursor_col;
            self.last_cursor_row = self.last_content_cursor_row;
        }
        let mut content = String::new();
        for index in 0..row_end.min(self.rows.len()) {
            content += &self.rows[index];
            if index + 1 < row_end {
                content += &self.separators[index];
            }
        }
        if !exclude_final_cursor_position {
            let cursor = self.term.cursor();
            let (real_row, real_col) = (cursor.line as isize, cursor.col as isize);
            let (last_row, last_col) =
                (self.last_cursor_row as isize, self.last_cursor_col as isize);
            if real_row != last_row || real_col != last_col {
                let down = real_row - last_row;
                if down > 0 {
                    content += &format!("\x1b[{down}B");
                } else if down < 0 {
                    content += &format!("\x1b[{}A", -down);
                }
                let right = real_col - last_col;
                if right > 0 {
                    content += &format!("\x1b[{right}C");
                } else if right < 0 {
                    content += &format!("\x1b[{}D", -right);
                }
            }
        }
        // xterm closes with the SGR difference to the live pen; the pen is not exposed here, so
        // the snapshot ends in the last cell's style (a replay is followed by a reset divider).
        content
    }
}

/// The buffer as a replayable ANSI stream, xterm's `serialize({ scrollback })` with modes and the
/// alternate screen excluded (what snapshots store). `None` scrollback serializes everything.
pub fn serialize_buffer(term: &Emulator, scrollback: Option<usize>) -> String {
    let max_rows = term.length();
    let rows = scrollback.map_or(max_rows, |depth| (depth + term.rows()).min(max_rows));
    let start = max_rows - rows;
    let end = max_rows.saturating_sub(1);
    let mut serializer = StringSerializer {
        term,
        rows: Vec::new(),
        separators: Vec::new(),
        current_row: String::new(),
        null_cell_count: 0,
        cursor_style: CellStyle::default(),
        cursor_style_row: 0,
        cursor_style_col: 0,
        background_cell: CellStyle::default(),
        first_row: start,
        last_cursor_row: start,
        last_cursor_col: 0,
        last_content_cursor_row: start,
        last_content_cursor_col: 0,
    };
    for row in start..=end {
        if let Some(line) = term.styled_line(row) {
            for (col, cell) in line.iter().enumerate().take(term.cols()) {
                serializer.next_cell(cell, row, col);
            }
        }
        serializer.row_end(row, row == end);
    }
    serializer.finish(false)
}

fn html_escape(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// The buffer as a standalone HTML document with colors preserved ("Export as HTML",
/// `serializeAsHTML({ includeGlobalBackground: true })`).
pub fn serialize_as_html(term: &Emulator, font_family: &str, font_size: f32) -> String {
    let colors = term.colors();
    let theme = term.theme();
    let mut html = String::from("<html><body><!--StartFragment--><pre>");
    html += &format!(
        "<div style='color: {}; background-color: {}; font-family: {font_family}; font-size: {font_size}px;'>",
        theme.foreground, theme.background
    );
    let hex = |color: CellColor| -> Option<String> {
        match color {
            CellColor::Default => None,
            CellColor::Palette(index) => Some(colors.palette.ansi[index as usize].to_hex()),
            CellColor::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        }
    };
    // The previous cell carries over from row to row, as in xterm.
    let mut old = CellStyle::default();
    for row in 0..term.length() {
        let mut current = String::new();
        for cell in term.styled_line(row).unwrap_or_default() {
            if cell.width == 0 {
                continue;
            }
            let style = cell.style;
            if !attributes_equal(&style, &old) {
                let mut content: Vec<String> = Vec::new();
                if let Some(fg) = hex(style.fg) {
                    content.push(format!("color: {fg};"));
                }
                if let Some(bg) = hex(style.bg) {
                    content.push(format!("background-color: {bg};"));
                }
                if style.inverse {
                    content.push("color: #000000; background-color: #BFBFBF;".into());
                }
                if style.bold {
                    content.push("font-weight: bold;".into());
                }
                let mut decorations: Vec<&str> = Vec::new();
                match style.underline {
                    UnderlineStyle::None => {}
                    UnderlineStyle::Single => decorations.push("underline"),
                    UnderlineStyle::Double => decorations.push("underline double"),
                    UnderlineStyle::Curly => decorations.push("underline wavy"),
                    UnderlineStyle::Dotted => decorations.push("underline dotted"),
                    UnderlineStyle::Dashed => decorations.push("underline dashed"),
                }
                if style.overline {
                    decorations.push("overline");
                }
                if style.strikethrough {
                    decorations.push("line-through");
                }
                if style.blink {
                    decorations.push("blink");
                }
                if !decorations.is_empty() {
                    content.push(format!("text-decoration: {};", decorations.join(" ")));
                }
                if style.underline != UnderlineStyle::None
                    && let Some(color) = style.underline_color.and_then(hex)
                {
                    content.push(format!("text-decoration-color: {color};"));
                }
                if style.invisible {
                    content.push("visibility: hidden;".into());
                }
                if style.italic {
                    content.push("font-style: italic;".into());
                }
                if style.dim {
                    content.push("opacity: 0.5;".into());
                }
                current += &if content.is_empty() {
                    "</span><span>".to_string()
                } else {
                    format!("</span><span style='{}'>", content.join(" "))
                };
                old = style;
            }
            current += &if cell.text.is_empty() {
                " ".to_string()
            } else {
                html_escape(&cell.text)
            };
        }
        html += &format!("<div><span>{current}</span></div>");
    }
    html += "</div></pre><!--EndFragment--></body></html>";
    html
}

/// The replayable snapshot to persist, stepping down the scrollback depth until it fits (see
/// [`crate::scrollback::serialize_scrollback`]).
pub fn snapshot(term: &Emulator, max_body_bytes: Option<usize>) -> Option<String> {
    if term.is_alt_screen() {
        // The normal buffer is what a restore shows; the alternate screen is never stored. The
        // emulator exposes the active screen only, so a full-screen program's session stores
        // nothing new until it exits.
        return None;
    }
    crate::scrollback::serialize_scrollback(
        |depth| serialize_buffer(term, Some(depth)),
        max_body_bytes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::line_text;
    use crate::palette::terminal_scheme;
    use crate::terminal::EmulatorConfig;

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

    fn replay(data: &str, cols: u16, rows: u16) -> Emulator {
        let mut term = emulator(cols, rows);
        term.write(data.as_bytes());
        term
    }

    #[test]
    fn serializes_plain_lines_with_crlf_and_the_cursor() {
        let term = replay("one\r\ntwo", 10, 4);
        assert_eq!(serialize_buffer(&term, None), "one\r\ntwo");
    }

    #[test]
    fn serializes_colors_and_attributes_like_xterm() {
        let term = replay(
            "\x1b[1;31mred\x1b[0m plain \x1b[38;5;196mx\x1b[48;2;1;2;3my",
            30,
            2,
        );
        // The blank rest of the row resets the style, as in xterm.
        assert_eq!(
            serialize_buffer(&term, None),
            "\x1b[31;1mred\x1b[0m plain \x1b[38;5;196mx\x1b[48;2;1;2;3my\x1b[0m"
        );
    }

    #[test]
    fn round_trips_through_a_replay() {
        let source = replay(
            "\x1b[32mgreen\x1b[0m\r\nab  cd\r\n\x1b[7minverse\x1b[0m\r\nlong line wrapping",
            10,
            6,
        );
        let data = serialize_buffer(&source, None);
        let copy = replay(&data, 10, 6);
        for line in 0..source.length() {
            assert_eq!(
                line_text(&copy, line),
                line_text(&source, line),
                "line {line}"
            );
            assert_eq!(
                copy.styled_line(line),
                source.styled_line(line),
                "line {line}"
            );
            assert_eq!(
                copy.is_wrapped(line),
                source.is_wrapped(line),
                "line {line}"
            );
        }
    }

    #[test]
    fn snapshots_step_down_and_drop_the_viewport_tail() {
        let mut term = emulator(20, 5);
        for index in 0..30 {
            term.write(format!("line {index}\r\n").as_bytes());
        }
        term.write(b"$ ");
        let data = snapshot(&term, None).unwrap();
        assert!(data.starts_with("line 0\r\n"));
        assert!(data.ends_with("$ "));
    }

    #[test]
    fn exports_html_with_colors() {
        let term = replay("\x1b[1;31mred & <b>\x1b[0m", 20, 2);
        let html = serialize_as_html(&term, "JetBrains Mono", 14.0);
        assert!(html.starts_with("<html><body><!--StartFragment--><pre><div style='color: #cccccc; background-color: #181818;"));
        assert!(
            html.contains(
                "<span style='color: #cd3131; font-weight: bold;'>red &amp; &lt;b></span>"
            )
        );
    }
}
