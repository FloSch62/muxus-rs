//! The core of js-beautify 1.15 as `vscode-css-languageservice` and
//! `vscode-html-languageservice` bundle it for Monaco's CSS and HTML formatters: the output
//! lines with their indentation (`core/output.js`) and the input scanner (`core/inputscanner.js`).
//! Ported without line wrapping (Monaco's CSS formatter has none).

/// One output line: its indentation and the items on it (`OutputLine`).
#[derive(Clone, Debug)]
pub struct OutputLine {
    /// `-1` marks a line whose indentation came with the text.
    indent_count: isize,
    alignment_count: usize,
    items: Vec<String>,
}

impl OutputLine {
    fn new() -> Self {
        Self {
            indent_count: -1,
            alignment_count: 0,
            items: Vec::new(),
        }
    }

    fn clone_empty(&self) -> Self {
        let mut line = OutputLine::new();
        line.set_indent(self.indent_count, self.alignment_count);
        line
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn set_indent(&mut self, indent: isize, alignment: usize) {
        if self.is_empty() {
            self.indent_count = indent;
            self.alignment_count = alignment;
        }
    }

    /// The item at `index`, from the end when negative.
    pub fn item(&self, index: isize) -> Option<&str> {
        let index = if index < 0 {
            self.items.len().checked_sub(index.unsigned_abs())?
        } else {
            index as usize
        };
        self.items.get(index).map(String::as_str)
    }

    pub fn last(&self) -> Option<&str> {
        self.items.last().map(String::as_str)
    }

    fn trim(&mut self) {
        while self.last() == Some(" ") {
            self.items.pop();
        }
    }
}

/// The indentation strings (`IndentStringCache`).
#[derive(Clone, Debug)]
struct Indents {
    indent_size: usize,
    indent_string: String,
    base: String,
}

impl Indents {
    fn string(&self, level: isize, column: usize) -> String {
        if level < 0 {
            return String::new();
        }
        let column = column + level as usize * self.indent_size;
        let mut result = self.base.clone();
        if self.indent_size > 0 && column >= self.indent_size {
            let indents = column / self.indent_size;
            result.push_str(&self.indent_string.repeat(indents));
            result.push_str(&" ".repeat(column - indents * self.indent_size));
        } else {
            result.push_str(&" ".repeat(column));
        }
        result
    }
}

/// The options the output takes.
#[derive(Clone, Debug)]
pub struct OutputOptions {
    pub indent_size: usize,
    pub indent_char: char,
    pub indent_with_tabs: bool,
    pub end_with_newline: bool,
    pub indent_empty_lines: bool,
}

impl OutputOptions {
    /// `Options`: indenting with tabs takes a tab per level (a size of 1 becomes 4).
    pub fn new(tab_size: usize, insert_spaces: bool) -> Self {
        let indent_size = if tab_size == 0 { 4 } else { tab_size };
        let indent_with_tabs = !insert_spaces;
        Self {
            indent_size: if indent_with_tabs && indent_size == 1 {
                4
            } else {
                indent_size
            },
            indent_char: if insert_spaces { ' ' } else { '\t' },
            indent_with_tabs,
            end_with_newline: false,
            indent_empty_lines: false,
        }
    }
}

/// The output being built (`Output`).
#[derive(Debug)]
pub struct Output {
    indents: Indents,
    end_with_newline: bool,
    indent_empty_lines: bool,
    lines: Vec<OutputLine>,
    next_line: OutputLine,
    pub space_before_token: bool,
    pub non_breaking_space: bool,
    /// `raw`: new lines are not added.
    pub raw: bool,
}

impl Output {
    pub fn new(options: &OutputOptions, base_indent: &str) -> Self {
        let indent_string = if options.indent_with_tabs {
            options.indent_char.to_string()
        } else {
            options.indent_char.to_string().repeat(options.indent_size)
        };
        let mut output = Self {
            indents: Indents {
                indent_size: options.indent_size,
                indent_string,
                base: base_indent.to_string(),
            },
            end_with_newline: options.end_with_newline,
            indent_empty_lines: options.indent_empty_lines,
            lines: Vec::new(),
            next_line: OutputLine::new(),
            space_before_token: false,
            non_breaking_space: false,
            raw: false,
        };
        output.add_output_line();
        output
    }

    fn add_output_line(&mut self) {
        let line = self.next_line.clone_empty();
        self.lines.push(line);
    }

    fn current(&mut self) -> &mut OutputLine {
        self.lines.last_mut().expect("there is always a line")
    }

    pub fn current_line(&self) -> &OutputLine {
        self.lines.last().expect("there is always a line")
    }

    pub fn previous_line(&self) -> Option<&OutputLine> {
        self.lines
            .len()
            .checked_sub(2)
            .map(|index| &self.lines[index])
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.current_line().is_empty()
    }

    /// Start a new line unless at the start of the output or just after one (`force` adds
    /// one after a new line too). Whether a line was started.
    pub fn add_new_line(&mut self, force: bool) -> bool {
        if self.is_empty() || (!force && self.just_added_newline()) {
            return false;
        }
        if !self.raw {
            self.add_output_line();
        }
        true
    }

    pub fn just_added_newline(&self) -> bool {
        self.current_line().is_empty()
    }

    pub fn just_added_blankline(&self) -> bool {
        self.is_empty()
            || (self.current_line().is_empty()
                && self.previous_line().is_some_and(OutputLine::is_empty))
    }

    /// The indentation of the line being written and the next ones; the first line of the
    /// output is never indented.
    pub fn set_indent(&mut self, indent: isize, alignment: usize) -> bool {
        self.next_line.set_indent(indent, alignment);
        if self.lines.len() > 1 {
            self.current().set_indent(indent, alignment);
            return true;
        }
        self.current().set_indent(0, 0);
        false
    }

    /// A token, after a space when one is wanted.
    pub fn add_token(&mut self, token: &str) {
        if self.space_before_token && !self.just_added_newline() {
            self.current().items.push(" ".into());
        }
        self.current().items.push(token.to_string());
        self.space_before_token = false;
        self.non_breaking_space = false;
    }

    /// Text that keeps its own whitespace and line breaks (`add_raw_token`).
    pub fn add_raw_token(&mut self, newlines: usize, whitespace_before: &str, text: &str) {
        for _ in 0..newlines {
            self.add_output_line();
        }
        let line = self.current();
        line.set_indent(-1, 0);
        line.items.push(whitespace_before.to_string());
        line.items.push(text.to_string());
        self.space_before_token = false;
        self.non_breaking_space = false;
    }

    /// Drop trailing spaces, and empty lines at the end with `eat_newlines`.
    pub fn trim(&mut self, eat_newlines: bool) {
        self.current().trim();
        while eat_newlines && self.lines.len() > 1 && self.current_line().is_empty() {
            self.lines.pop();
            self.current().trim();
        }
    }

    /// Make sure an empty line stands above the current one, unless the lines above start
    /// with `starts_with` or end with `ends_with`.
    pub fn ensure_empty_line_above(&mut self, starts_with: &str, ends_with: &str) {
        let mut index = self.lines.len() as isize - 2;
        while index >= 0 {
            let line = &self.lines[index as usize];
            if line.is_empty() {
                break;
            }
            if !line.item(0).unwrap_or("").starts_with(starts_with)
                && line.item(-1) != Some(ends_with)
            {
                self.lines.insert(index as usize + 1, OutputLine::new());
                break;
            }
            index -= 1;
        }
    }

    /// Un-indent the lines from `index` on by one level (`remove_indent`).
    pub fn remove_indent(&mut self, index: usize) {
        for line in self.lines.iter_mut().skip(index) {
            if line.indent_count > 0 {
                line.indent_count -= 1;
            }
        }
    }

    /// The text, lines joined with `eol`.
    pub fn get_code(mut self, eol: &str) -> String {
        self.trim(true);
        if let Some(last) = self.current().items.pop() {
            let trimmed = last.trim_end_matches('\n').to_string();
            self.current().items.push(trimmed);
        }
        if self.end_with_newline {
            self.add_output_line();
        }
        let lines: Vec<String> = self
            .lines
            .iter()
            .map(|line| {
                if line.is_empty() {
                    if self.indent_empty_lines {
                        self.indents.string(line.indent_count, 0)
                    } else {
                        String::new()
                    }
                } else {
                    let mut text = self.indents.string(line.indent_count, line.alignment_count);
                    for item in &line.items {
                        text.push_str(item);
                    }
                    text
                }
            })
            .collect();
        let code = lines.join("\n");
        if eol == "\n" {
            code
        } else {
            code.replace('\n', eol)
        }
    }
}

/// JavaScript's `\s`.
pub fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// A scanner over the input's characters (`InputScanner`).
#[derive(Debug)]
pub struct InputScanner {
    chars: Vec<char>,
    position: usize,
}

impl InputScanner {
    pub fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            position: 0,
        }
    }

    pub fn has_next(&self) -> bool {
        self.position < self.chars.len()
    }

    pub fn next_char(&mut self) -> Option<char> {
        let c = self.chars.get(self.position).copied();
        if c.is_some() {
            self.position += 1;
        }
        c
    }

    pub fn back(&mut self) {
        self.position = self.position.saturating_sub(1);
    }

    pub fn peek(&self, offset: isize) -> Option<char> {
        let index = self.position as isize + offset;
        if index < 0 {
            return None;
        }
        self.chars.get(index as usize).copied()
    }

    /// The characters from here while `test` holds, consumed.
    pub fn read_while(&mut self, mut test: impl FnMut(char) -> bool) -> String {
        let start = self.position;
        while self.position < self.chars.len() && test(self.chars[self.position]) {
            self.position += 1;
        }
        self.chars[start..self.position].iter().collect()
    }

    /// The characters up to (`after`: through) the first place `found` matches, consumed;
    /// to the end when it never does. `found` gets the characters from a position and
    /// returns the match's length.
    pub fn read_until(&mut self, found: impl Fn(&[char]) -> Option<usize>, after: bool) -> String {
        let start = self.position;
        let mut end = self.chars.len();
        for index in start..self.chars.len() {
            if let Some(length) = found(&self.chars[index..]) {
                end = if after { index + length } else { index };
                break;
            }
        }
        self.position = end;
        self.chars[start..end].iter().collect()
    }

    /// [`Self::read_until`] through the match, without consuming.
    pub fn peek_until_after(&mut self, found: impl Fn(&[char]) -> Option<usize>) -> String {
        let start = self.position;
        let text = self.read_until(found, true);
        self.position = start;
        text
    }

    /// Whether `text` (lowercase) stands before the character just read.
    pub fn look_back(&self, text: &str) -> bool {
        let count = text.chars().count();
        let Some(start) = self.position.checked_sub(1) else {
            return false;
        };
        if start < count {
            return false;
        }
        let before: String = self.chars[start - count..start].iter().collect();
        before.to_lowercase() == text
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn set_position(&mut self, position: usize) {
        self.position = position.min(self.chars.len());
    }

    pub fn chars(&self) -> &[char] {
        &self.chars
    }
}

/// A match of `literal` at the start of `chars`, for [`InputScanner::read_until`].
pub fn literal(literal: &str) -> impl Fn(&[char]) -> Option<usize> + '_ {
    move |chars: &[char]| {
        let mut length = 0;
        for c in literal.chars() {
            if chars.get(length) != Some(&c) {
                return None;
            }
            length += 1;
        }
        Some(length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indents_with_spaces_or_tabs() {
        let spaces = Output::new(&OutputOptions::new(2, true), "");
        assert_eq!(spaces.indents.string(2, 0), "    ");
        assert_eq!(spaces.indents.string(-1, 0), "");
        let tabs = Output::new(&OutputOptions::new(4, false), "");
        assert_eq!(tabs.indents.string(2, 1), "\t\t ");
    }

    #[test]
    fn scans_back_and_ahead() {
        let mut scanner = InputScanner::new("url(a)");
        scanner.set_position(4);
        assert!(scanner.look_back("url"));
        assert_eq!(scanner.peek_until_after(literal(")")), "a)");
        assert_eq!(scanner.position(), 4);
    }
}
