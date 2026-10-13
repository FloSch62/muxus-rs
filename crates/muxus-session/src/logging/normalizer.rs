//! `server/src/session-logging/terminal-text-normalizer.ts`.

use muxus_core::api::SessionLineTimestamp;

use super::utf8::Utf8Stream;

/// Cursor arithmetic guard: a CSI parameter beyond any real screen is
/// clamped, so a hostile `ESC[999999999B` cannot allocate rows without end.
const MAX_CSI_PARAM: i64 = 65_535;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Text,
    Escape,
    EscapeIntermediate,
    Csi,
    String,
    StringEscape,
}

/// Small streaming terminal-line reconciler used only for search and
/// readable transcripts. It applies the cursor/erase operations used by
/// shells and progress displays, retaining a few editable rows so prompt
/// redraws replace their earlier state instead of becoming duplicate
/// transcript lines. The original byte events remain untouched for exact export.
#[derive(Debug, Clone)]
pub struct TerminalTextNormalizer {
    editable_rows: usize,
    decoder: Utf8Stream,
    state: State,
    csi: String,
    rows: Vec<Vec<char>>,
    cursor_row: usize,
    cursor_col: usize,
    saved_cursor: (usize, usize),
    committed: String,
    /// Length of `committed` in UTF-16 code units: line offsets use them.
    committed_units: usize,
    row_times: Vec<Option<String>>,
    current_time: String,
    committed_times: Vec<SessionLineTimestamp>,
    emitted_times: Vec<SessionLineTimestamp>,
}

impl Default for TerminalTextNormalizer {
    fn default() -> Self {
        Self::new(4)
    }
}

impl TerminalTextNormalizer {
    /// `editable_rows` rows above the cursor stay editable before they are
    /// committed; input uses 0, output 4.
    pub fn new(editable_rows: usize) -> Self {
        Self {
            editable_rows,
            decoder: Utf8Stream::new(),
            state: State::Text,
            csi: String::new(),
            rows: vec![Vec::new()],
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor: (0, 0),
            committed: String::new(),
            committed_units: 0,
            row_times: Vec::new(),
            current_time: String::new(),
            committed_times: Vec::new(),
            emitted_times: Vec::new(),
        }
    }

    /// The UTF-16 offsets and times of the lines the last call returned.
    pub fn take_line_timestamps(&mut self) -> Vec<SessionLineTimestamp> {
        std::mem::take(&mut self.emitted_times)
    }

    /// Feed bytes received at `recorded_at` (an ISO timestamp); returns the
    /// rows that are final now.
    pub fn write(&mut self, data: &[u8], recorded_at: &str) -> String {
        self.current_time = recorded_at.to_string();
        let decoded = self.decoder.decode(data);
        self.process(&decoded);
        self.commit_ready_rows();
        self.take_committed()
    }

    /// Flush the currently visible rows and start a fresh transcript screen.
    pub fn drain(&mut self) -> String {
        self.commit_all_rows();
        self.reset_rows();
        self.take_committed()
    }

    /// Final decoder flush plus all still-editable rows at session end.
    pub fn finish(&mut self) -> String {
        let tail = self.decoder.finish();
        self.process(&tail);
        self.drain()
    }

    fn process(&mut self, decoded: &str) {
        for ch in decoded.chars() {
            let code = ch as u32;
            match self.state {
                State::Text => match ch {
                    '\x1b' => self.state = State::Escape,
                    '\r' => self.cursor_col = 0,
                    '\n' => {
                        let row = self.cursor_row;
                        if self.row_time(row).is_none() {
                            self.set_row_time(row);
                        }
                        self.cursor_row += 1;
                        self.ensure_row(self.cursor_row);
                    }
                    '\x08' => self.cursor_col = self.cursor_col.saturating_sub(1),
                    '\t' => {
                        let spaces = 8 - (self.cursor_col % 8);
                        for _ in 0..spaces {
                            self.put(' ');
                        }
                    }
                    _ if code >= 0x20 && code != 0x7f => self.put(ch),
                    _ => {}
                },
                State::Escape => {
                    if ch == '[' {
                        self.csi.clear();
                        self.state = State::Csi;
                    } else if matches!(ch, ']' | 'P' | '_' | '^') {
                        self.state = State::String;
                    } else if (0x20..=0x2f).contains(&code) {
                        self.state = State::EscapeIntermediate;
                    } else {
                        self.handle_escape(ch);
                        self.state = State::Text;
                    }
                }
                State::EscapeIntermediate => {
                    if (0x30..=0x7e).contains(&code) {
                        self.state = State::Text;
                    }
                }
                State::Csi => {
                    if (0x40..=0x7e).contains(&code) {
                        self.handle_csi(ch);
                        self.csi.clear();
                        self.state = State::Text;
                    } else {
                        self.csi.push(ch);
                    }
                }
                State::String => {
                    if code == 0x07 {
                        self.state = State::Text;
                    } else if code == 0x1b {
                        self.state = State::StringEscape;
                    }
                }
                State::StringEscape => {
                    self.state = match ch {
                        '\\' => State::Text,
                        '\x1b' => State::StringEscape,
                        _ => State::String,
                    };
                }
            }
        }
    }

    fn row_time(&self, row: usize) -> Option<&String> {
        self.row_times.get(row).and_then(Option::as_ref)
    }

    fn set_row_time(&mut self, row: usize) {
        if self.row_times.len() <= row {
            self.row_times.resize(row + 1, None);
        }
        self.row_times[row] = Some(self.current_time.clone());
    }

    fn put(&mut self, ch: char) {
        let col = self.cursor_col;
        let row_index = self.cursor_row;
        let row = self.ensure_row(row_index);
        while row.len() < col {
            row.push(' ');
        }
        if col < row.len() {
            row[col] = ch;
        } else {
            row.push(ch);
        }
        self.set_row_time(row_index);
        self.cursor_col += 1;
    }

    fn handle_escape(&mut self, final_char: char) {
        match final_char {
            '7' => self.saved_cursor = (self.cursor_row, self.cursor_col),
            '8' => {
                (self.cursor_row, self.cursor_col) = self.saved_cursor;
                self.ensure_row(self.cursor_row);
            }
            'D' => {
                self.cursor_row += 1;
                self.ensure_row(self.cursor_row);
            }
            'E' => {
                self.cursor_row += 1;
                self.cursor_col = 0;
                self.ensure_row(self.cursor_row);
            }
            'M' => {
                self.cursor_row = self.cursor_row.saturating_sub(1);
                self.ensure_row(self.cursor_row);
            }
            'c' => self.reset_rows(),
            _ => {}
        }
    }

    fn handle_csi(&mut self, final_char: char) {
        let private_mode = self.csi.starts_with(['?', '>']);
        let raw = if private_mode {
            &self.csi[1..]
        } else {
            self.csi.as_str()
        };
        let params: Vec<i64> = raw.split(';').map(js_parse_int).collect();
        let first = params.first().copied().unwrap_or(0);
        let amount = first.max(1) as usize;
        if private_mode && matches!(final_char, 'h' | 'l') {
            return;
        }
        match final_char {
            'A' => self.cursor_row = self.cursor_row.saturating_sub(amount),
            'B' => self.cursor_row += amount,
            'C' => self.cursor_col += amount,
            'D' => self.cursor_col = self.cursor_col.saturating_sub(amount),
            'E' => {
                self.cursor_row += amount;
                self.cursor_col = 0;
            }
            'F' => {
                self.cursor_row = self.cursor_row.saturating_sub(amount);
                self.cursor_col = 0;
            }
            'G' | '`' => self.cursor_col = amount - 1,
            'H' | 'f' => {
                let row = params
                    .first()
                    .copied()
                    .filter(|&value| value != 0)
                    .unwrap_or(1);
                let col = params
                    .get(1)
                    .copied()
                    .filter(|&value| value != 0)
                    .unwrap_or(1);
                self.cursor_row = (row - 1).max(0) as usize;
                self.cursor_col = (col - 1).max(0) as usize;
            }
            'd' => self.cursor_row = amount - 1,
            'J' => self.erase_display(first),
            'K' => self.erase_line(first),
            'P' => {
                self.set_row_time(self.cursor_row);
                let col = self.cursor_col;
                let row = self.ensure_row(self.cursor_row);
                if col < row.len() {
                    let end = (col + amount).min(row.len());
                    row.drain(col..end);
                }
            }
            '@' => {
                self.set_row_time(self.cursor_row);
                let col = self.cursor_col;
                let row = self.ensure_row(self.cursor_row);
                let at = col.min(row.len());
                row.splice(at..at, std::iter::repeat_n(' ', amount));
            }
            'X' => {
                self.set_row_time(self.cursor_row);
                let col = self.cursor_col;
                let row = self.ensure_row(self.cursor_row);
                for index in col..col.saturating_add(amount).min(row.len()) {
                    row[index] = ' ';
                }
            }
            's' => self.saved_cursor = (self.cursor_row, self.cursor_col),
            'u' => (self.cursor_row, self.cursor_col) = self.saved_cursor,
            _ => {}
        }
        self.ensure_row(self.cursor_row);
    }

    fn erase_display(&mut self, mode: i64) {
        match mode {
            2 | 3 => {
                // A deliberate clear still belongs to searchable history, while
                // the following screen starts without stale editable prompt fragments.
                self.commit_all_rows();
                self.reset_rows();
            }
            0 => {
                self.erase_line(0);
                self.rows.truncate(self.cursor_row + 1);
                self.row_times.truncate(self.cursor_row + 1);
            }
            1 => {
                for row in 0..self.cursor_row {
                    if let Some(cells) = self.rows.get_mut(row) {
                        cells.clear();
                    }
                    self.set_row_time(row);
                }
                self.erase_line(1);
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: i64) {
        self.set_row_time(self.cursor_row);
        let col = self.cursor_col;
        let row = self.ensure_row(self.cursor_row);
        match mode {
            2 => row.clear(),
            1 => {
                if !row.is_empty() {
                    let end = col.min(row.len() - 1);
                    for cell in &mut row[..=end] {
                        *cell = ' ';
                    }
                }
            }
            _ => row.truncate(col),
        }
    }

    fn ensure_row(&mut self, index: usize) -> &mut Vec<char> {
        while self.rows.len() <= index {
            self.rows.push(Vec::new());
        }
        &mut self.rows[index]
    }

    fn commit_ready_rows(&mut self) {
        let count = self.cursor_row.saturating_sub(self.editable_rows);
        for index in 0..count {
            self.record_line_time(index);
            let text = line_text(&self.rows[index]);
            self.push_committed(&text);
            self.push_committed("\n");
        }
        if count == 0 {
            return;
        }
        self.rows.drain(..count.min(self.rows.len()));
        self.row_times.drain(..count.min(self.row_times.len()));
        self.cursor_row -= count;
        self.saved_cursor = (
            self.saved_cursor.0.saturating_sub(count),
            self.saved_cursor.1,
        );
    }

    fn commit_all_rows(&mut self) {
        let Some(last) = self.rows.iter().rposition(|row| !line_text(row).is_empty()) else {
            return;
        };
        let total = self.rows.len();
        for index in 0..=last {
            self.record_line_time(index);
            let text = line_text(&self.rows[index]);
            self.push_committed(&text);
            if index < last || last < total - 1 {
                self.push_committed("\n");
            }
        }
    }

    fn push_committed(&mut self, text: &str) {
        self.committed.push_str(text);
        self.committed_units += text.encode_utf16().count();
    }

    fn record_line_time(&mut self, index: usize) {
        let recorded_at = self
            .row_time(index)
            .cloned()
            .unwrap_or_else(|| self.current_time.clone());
        self.committed_times.push(SessionLineTimestamp {
            offset: self.committed_units as u64,
            recorded_at,
        });
    }

    fn reset_rows(&mut self) {
        self.row_times.clear();
        self.rows = vec![Vec::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.saved_cursor = (0, 0);
    }

    fn take_committed(&mut self) -> String {
        self.committed_units = 0;
        self.emitted_times = std::mem::take(&mut self.committed_times);
        std::mem::take(&mut self.committed)
    }
}

fn line_text(row: &[char]) -> String {
    let end = row
        .iter()
        .rposition(|&ch| ch != ' ')
        .map_or(0, |index| index + 1);
    row[..end].iter().collect()
}

/// `Number.parseInt(value, 10)`, with an empty or unparsable value as 0 and
/// the result clamped to a sane cursor range.
fn js_parse_int(value: &str) -> i64 {
    let trimmed = value.trim_start();
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let digits: &str = &digits[..digits.bytes().take_while(u8::is_ascii_digit).count()];
    if digits.is_empty() {
        return 0;
    }
    let magnitude = digits.parse::<i64>().unwrap_or(i64::MAX).min(MAX_CSI_PARAM);
    if negative { -magnitude } else { magnitude }
}
