//! What Monaco's JSON worker does for the remote editor: problem markers (the syntax errors of
//! `vscode-json-languageservice`'s parser, with Monaco's defaults: trailing commas and comments
//! are errors, duplicate keys warnings) and Format Document (`jsonc-parser`'s `format`).

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    OpenBrace,
    CloseBrace,
    OpenBracket,
    CloseBracket,
    Comma,
    Colon,
    Null,
    True,
    False,
    String,
    Number,
    LineComment,
    BlockComment,
    LineBreak,
    Trivia,
    Unknown,
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanError {
    None,
    UnexpectedEndOfComment,
    UnexpectedEndOfString,
    UnexpectedEndOfNumber,
    InvalidUnicode,
    InvalidEscapeCharacter,
    InvalidCharacter,
}

/// `jsonc-parser`'s scanner, over bytes (everything it tells apart is ASCII).
struct Scanner<'a> {
    text: &'a [u8],
    pos: usize,
    value: String,
    token_offset: usize,
    token: Token,
    error: ScanError,
}

fn is_white_space(c: u8) -> bool {
    c == b' ' || c == b'\t'
}

fn is_line_break(c: u8) -> bool {
    c == b'\n' || c == b'\r'
}

impl<'a> Scanner<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text: text.as_bytes(),
            pos: 0,
            value: String::new(),
            token_offset: 0,
            token: Token::Unknown,
            error: ScanError::None,
        }
    }

    fn at(&self, pos: usize) -> Option<u8> {
        self.text.get(pos).copied()
    }

    fn token_length(&self) -> usize {
        self.pos - self.token_offset
    }

    fn slice(&self, range: Range<usize>) -> String {
        String::from_utf8_lossy(&self.text[range]).into_owned()
    }

    fn scan_hex_digits(&mut self, count: usize) -> Option<u32> {
        let mut digits = 0;
        let mut value = 0u32;
        while digits < count {
            let Some(c) = self.at(self.pos).and_then(|c| (c as char).to_digit(16)) else {
                break;
            };
            value = value * 16 + c;
            self.pos += 1;
            digits += 1;
        }
        (digits == count).then_some(value)
    }

    fn scan_number(&mut self) -> String {
        let start = self.pos;
        if self.at(self.pos) == Some(b'0') {
            self.pos += 1;
        } else {
            self.pos += 1;
            while self.at(self.pos).is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if self.at(self.pos) == Some(b'.') {
            self.pos += 1;
            if self.at(self.pos).is_some_and(|c| c.is_ascii_digit()) {
                while self.at(self.pos).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            } else {
                self.error = ScanError::UnexpectedEndOfNumber;
                return self.slice(start..self.pos);
            }
        }
        let mut end = self.pos;
        if matches!(self.at(self.pos), Some(b'E' | b'e')) {
            self.pos += 1;
            if matches!(self.at(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.at(self.pos).is_some_and(|c| c.is_ascii_digit()) {
                while self.at(self.pos).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
                end = self.pos;
            } else {
                self.error = ScanError::UnexpectedEndOfNumber;
            }
        }
        self.slice(start..end)
    }

    fn scan_string(&mut self) -> String {
        let mut result = Vec::new();
        let mut start = self.pos;
        loop {
            let Some(c) = self.at(self.pos) else {
                result.extend_from_slice(&self.text[start..self.pos]);
                self.error = ScanError::UnexpectedEndOfString;
                break;
            };
            if c == b'"' {
                result.extend_from_slice(&self.text[start..self.pos]);
                self.pos += 1;
                break;
            }
            if c == b'\\' {
                result.extend_from_slice(&self.text[start..self.pos]);
                self.pos += 1;
                let Some(escaped) = self.at(self.pos) else {
                    self.error = ScanError::UnexpectedEndOfString;
                    break;
                };
                self.pos += 1;
                match escaped {
                    b'"' => result.push(b'"'),
                    b'\\' => result.push(b'\\'),
                    b'/' => result.push(b'/'),
                    b'b' => result.push(8),
                    b'f' => result.push(12),
                    b'n' => result.push(b'\n'),
                    b'r' => result.push(b'\r'),
                    b't' => result.push(b'\t'),
                    b'u' => match self.scan_hex_digits(4) {
                        Some(code) => {
                            let c = char::from_u32(code).unwrap_or('\u{fffd}');
                            let mut buffer = [0; 4];
                            result.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
                        }
                        None => self.error = ScanError::InvalidUnicode,
                    },
                    _ => self.error = ScanError::InvalidEscapeCharacter,
                }
                start = self.pos;
                continue;
            }
            if c <= 0x1f {
                if is_line_break(c) {
                    result.extend_from_slice(&self.text[start..self.pos]);
                    self.error = ScanError::UnexpectedEndOfString;
                    break;
                }
                // Marked, but the string goes on.
                self.error = ScanError::InvalidCharacter;
            }
            self.pos += 1;
        }
        String::from_utf8_lossy(&result).into_owned()
    }

    fn is_unknown_content(c: u8) -> bool {
        !(is_white_space(c)
            || is_line_break(c)
            || matches!(c, b'}' | b']' | b'{' | b'[' | b'"' | b':' | b',' | b'/'))
    }

    fn scan(&mut self) -> Token {
        self.value.clear();
        self.error = ScanError::None;
        self.token_offset = self.pos;
        let len = self.text.len();
        let Some(code) = self.at(self.pos) else {
            self.token_offset = len;
            self.token = Token::Eof;
            return self.token;
        };
        if is_white_space(code) {
            while self.at(self.pos).is_some_and(is_white_space) {
                self.pos += 1;
            }
            self.token = Token::Trivia;
            return self.token;
        }
        if is_line_break(code) {
            self.pos += 1;
            if code == b'\r' && self.at(self.pos) == Some(b'\n') {
                self.pos += 1;
            }
            self.token = Token::LineBreak;
            return self.token;
        }
        let single = |token| (token, 1);
        let (token, advance) = match code {
            b'{' => single(Token::OpenBrace),
            b'}' => single(Token::CloseBrace),
            b'[' => single(Token::OpenBracket),
            b']' => single(Token::CloseBracket),
            b':' => single(Token::Colon),
            b',' => single(Token::Comma),
            b'"' => {
                self.pos += 1;
                self.value = self.scan_string();
                self.token = Token::String;
                return self.token;
            }
            b'/' => {
                let start = self.pos;
                if self.at(self.pos + 1) == Some(b'/') {
                    self.pos += 2;
                    while self.at(self.pos).is_some_and(|c| !is_line_break(c)) {
                        self.pos += 1;
                    }
                    self.value = self.slice(start..self.pos);
                    self.token = Token::LineComment;
                    return self.token;
                }
                if self.at(self.pos + 1) == Some(b'*') {
                    self.pos += 2;
                    let safe_length = len.saturating_sub(1);
                    let mut closed = false;
                    while self.pos < safe_length {
                        let c = self.text[self.pos];
                        if c == b'*' && self.at(self.pos + 1) == Some(b'/') {
                            self.pos += 2;
                            closed = true;
                            break;
                        }
                        self.pos += 1;
                    }
                    if !closed {
                        self.pos = (self.pos + 1).min(len);
                        self.error = ScanError::UnexpectedEndOfComment;
                    }
                    self.value = self.slice(start..self.pos);
                    self.token = Token::BlockComment;
                    return self.token;
                }
                single(Token::Unknown)
            }
            b'-' if !self.at(self.pos + 1).is_some_and(|c| c.is_ascii_digit()) => {
                single(Token::Unknown)
            }
            b'-' | b'0'..=b'9' => {
                let mut value = String::new();
                if code == b'-' {
                    value.push('-');
                    self.pos += 1;
                }
                value.push_str(&self.scan_number());
                self.value = value;
                self.token = Token::Number;
                return self.token;
            }
            _ => {
                while self.at(self.pos).is_some_and(Self::is_unknown_content) {
                    self.pos += 1;
                }
                if self.token_offset != self.pos {
                    self.value = self.slice(self.token_offset..self.pos);
                    self.token = match self.value.as_str() {
                        "true" => Token::True,
                        "false" => Token::False,
                        "null" => Token::Null,
                        _ => Token::Unknown,
                    };
                    return self.token;
                }
                // One character, whole (a multi-byte character is one unknown token).
                let width = std::str::from_utf8(&self.text[self.pos..])
                    .ok()
                    .and_then(|rest| rest.chars().next())
                    .map_or(1, char::len_utf8);
                (Token::Unknown, width)
            }
        };
        self.pos += advance;
        self.token = token;
        token
    }
}

/// A problem marker's severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A problem marker: where, what, how bad.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub range: Range<usize>,
    pub message: &'static str,
    pub severity: Severity,
}

struct Parser<'a> {
    text: &'a str,
    scanner: Scanner<'a>,
    problems: Vec<Problem>,
    last_problem_offset: Option<usize>,
    comments: Vec<Range<usize>>,
}

impl Parser<'_> {
    fn token(&self) -> Token {
        self.scanner.token
    }

    fn scan_next(&mut self) -> Token {
        loop {
            let token = self.scanner.scan();
            self.check_scan_error();
            match token {
                Token::LineComment | Token::BlockComment => {
                    let start = self.scanner.token_offset;
                    self.comments
                        .push(start..start + self.scanner.token_length());
                }
                Token::Trivia | Token::LineBreak => {}
                other => return other,
            }
        }
    }

    fn error_at(&mut self, message: &'static str, range: Range<usize>, severity: Severity) {
        if self.problems.is_empty() || self.last_problem_offset != Some(range.start) {
            self.last_problem_offset = Some(range.start);
            self.problems.push(Problem {
                range,
                message,
                severity,
            });
        }
    }

    /// `_error`: at the current token, or the last character before an empty one.
    fn error(&mut self, message: &'static str, skip_until_after: &[Token], skip_until: &[Token]) {
        let mut start = self.scanner.token_offset;
        let mut end = start + self.scanner.token_length();
        if start == end && start > 0 {
            let before = &self.text[..start];
            let mut chars = before.char_indices().rev().peekable();
            start = chars.next().map_or(0, |(offset, _)| offset);
            while start > 0 && self.text[start..].starts_with(char::is_whitespace) {
                start = chars.next().map_or(0, |(offset, _)| offset);
            }
            end = start + self.text[start..].chars().next().map_or(1, char::len_utf8);
        }
        self.error_at(message, start..end, Severity::Error);
        if skip_until_after.len() + skip_until.len() > 0 {
            let mut token = self.token();
            while token != Token::Eof {
                if skip_until_after.contains(&token) {
                    self.scan_next();
                    break;
                } else if skip_until.contains(&token) {
                    break;
                }
                token = self.scan_next();
            }
        }
    }

    fn check_scan_error(&mut self) {
        let message = match self.scanner.error {
            ScanError::None => return,
            ScanError::InvalidUnicode => "Invalid unicode sequence in string.",
            ScanError::InvalidEscapeCharacter => "Invalid escape character in string.",
            ScanError::UnexpectedEndOfNumber => "Unexpected end of number.",
            ScanError::UnexpectedEndOfComment => "Unexpected end of comment.",
            ScanError::UnexpectedEndOfString => "Unexpected end of string.",
            ScanError::InvalidCharacter => {
                "Invalid characters in string. Control characters must be escaped."
            }
        };
        self.error(message, &[], &[]);
    }

    fn line_of(&self, offset: usize) -> usize {
        self.text.as_bytes()[..offset.min(self.text.len())]
            .iter()
            .filter(|&&c| c == b'\n')
            .count()
    }

    fn parse_array(&mut self) -> bool {
        if self.token() != Token::OpenBracket {
            return false;
        }
        self.scan_next();
        let mut needs_comma = false;
        while !matches!(self.token(), Token::CloseBracket | Token::Eof) {
            if self.token() == Token::Comma {
                if !needs_comma {
                    self.error("Value expected", &[], &[]);
                }
                let comma = self.scanner.token_offset;
                self.scan_next();
                if self.token() == Token::CloseBracket {
                    if needs_comma {
                        self.error_at("Trailing comma", comma..comma + 1, Severity::Error);
                    }
                    continue;
                }
            } else if needs_comma {
                self.error("Expected comma", &[], &[]);
            }
            if !self.parse_value() {
                self.error("Value expected", &[], &[Token::CloseBracket, Token::Comma]);
            }
            needs_comma = true;
        }
        if self.token() != Token::CloseBracket {
            self.error("Expected comma or closing bracket", &[], &[]);
            return true;
        }
        self.scan_next();
        true
    }

    /// A property; `keys` holds the keys seen in the object (with where they were, until a
    /// duplicate was reported for them).
    fn parse_property(&mut self, keys: &mut Vec<(String, Option<Range<usize>>)>) -> bool {
        let key_range;
        let key_value;
        if self.token() == Token::String {
            key_range =
                self.scanner.token_offset..self.scanner.token_offset + self.scanner.token_length();
            key_value = self.scanner.value.clone();
            self.scan_next();
        } else if self.token() == Token::Unknown {
            // A more helpful message.
            self.error("Property keys must be doublequoted", &[], &[]);
            key_range =
                self.scanner.token_offset..self.scanner.token_offset + self.scanner.token_length();
            key_value = self.scanner.value.clone();
            self.scan_next();
        } else {
            return false;
        }
        // `"//"` keys are the comment convention of JSON files without comments.
        if key_value != "//" {
            match keys.iter_mut().find(|(key, _)| *key == key_value) {
                Some((_, seen)) => {
                    self.error_at("Duplicate object key", key_range.clone(), Severity::Warning);
                    if let Some(first) = seen.take() {
                        self.error_at("Duplicate object key", first, Severity::Warning);
                    }
                }
                None => keys.push((key_value, Some(key_range.clone()))),
            }
        }
        if self.token() == Token::Colon {
            self.scan_next();
        } else {
            self.error("Colon expected", &[], &[]);
            if self.token() == Token::String
                && self.line_of(key_range.end) < self.line_of(self.scanner.token_offset)
            {
                return true;
            }
        }
        if !self.parse_value() {
            self.error("Value expected", &[], &[Token::CloseBrace, Token::Comma]);
        }
        true
    }

    fn parse_object(&mut self) -> bool {
        if self.token() != Token::OpenBrace {
            return false;
        }
        let mut keys = Vec::new();
        self.scan_next();
        let mut needs_comma = false;
        while !matches!(self.token(), Token::CloseBrace | Token::Eof) {
            if self.token() == Token::Comma {
                if !needs_comma {
                    self.error("Property expected", &[], &[]);
                }
                let comma = self.scanner.token_offset;
                self.scan_next();
                if self.token() == Token::CloseBrace {
                    if needs_comma {
                        self.error_at("Trailing comma", comma..comma + 1, Severity::Error);
                    }
                    continue;
                }
            } else if needs_comma {
                self.error("Expected comma", &[], &[]);
            }
            if !self.parse_property(&mut keys) {
                self.error("Property expected", &[], &[Token::CloseBrace, Token::Comma]);
            }
            needs_comma = true;
        }
        if self.token() != Token::CloseBrace {
            self.error("Expected comma or closing brace", &[], &[]);
            return true;
        }
        self.scan_next();
        true
    }

    fn parse_value(&mut self) -> bool {
        match self.token() {
            Token::OpenBracket => self.parse_array(),
            Token::OpenBrace => self.parse_object(),
            Token::String | Token::Number | Token::Null | Token::True | Token::False => {
                self.scan_next();
                true
            }
            _ => false,
        }
    }
}

/// Monaco's JSON markers for `text`: syntax errors, duplicate keys (warnings) and comments
/// ("Comments are not permitted in JSON."), each place and message once.
pub fn json_problems(text: &str) -> Vec<Problem> {
    let mut parser = Parser {
        text,
        scanner: Scanner::new(text),
        problems: Vec::new(),
        last_problem_offset: None,
        comments: Vec::new(),
    };
    if parser.scan_next() != Token::Eof {
        if !parser.parse_value() {
            parser.error("Expected a JSON object, array or literal.", &[], &[]);
        } else if parser.token() != Token::Eof {
            parser.error("End of file expected.", &[], &[]);
        }
    }
    let mut problems = parser.problems;
    problems.extend(parser.comments.into_iter().map(|range| Problem {
        range,
        message: "Comments are not permitted in JSON.",
        severity: Severity::Error,
    }));
    let mut seen = std::collections::HashSet::new();
    problems.retain(|problem| seen.insert((problem.range.start, problem.message)));
    problems
}

/// How Format Document indents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    pub tab_size: usize,
    pub insert_spaces: bool,
}

/// `jsonc-parser`'s `format` over the whole document, applied: one value per line, indented,
/// comments kept. A document it cannot read is left as it is where it stops making sense.
pub fn format_json(text: &str, options: FormatOptions) -> String {
    let eol = text
        .bytes()
        .position(|c| c == b'\r' || c == b'\n')
        .map_or("\n", |index| {
            if text.as_bytes()[index] == b'\r' {
                if text.as_bytes().get(index + 1) == Some(&b'\n') {
                    "\r\n"
                } else {
                    "\r"
                }
            } else {
                "\n"
            }
        });
    let indent_value = if options.insert_spaces {
        " ".repeat(if options.tab_size == 0 {
            4
        } else {
            options.tab_size
        })
    } else {
        "\t".to_string()
    };
    let mut scanner = Scanner::new(text);
    let mut line_breaks = 0usize;
    let mut has_error = false;
    let mut indent_level: isize = 0;
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let scan_next = |scanner: &mut Scanner, line_breaks: &mut usize, has_error: &mut bool| {
        let mut token = scanner.scan();
        *line_breaks = 0;
        while matches!(token, Token::Trivia | Token::LineBreak) {
            if token == Token::LineBreak {
                *line_breaks = 1;
            }
            token = scanner.scan();
        }
        *has_error = token == Token::Unknown || scanner.error != ScanError::None;
        token
    };
    let new_line_and_indent = |level: isize| -> String {
        let mut out = eol.to_string();
        for _ in 0..level.max(0) {
            out.push_str(&indent_value);
        }
        out
    };
    let add_edit = |edits: &mut Vec<(Range<usize>, String)>,
                    has_error: bool,
                    content: String,
                    range: Range<usize>| {
        if !has_error && text[range.clone()] != content {
            edits.push((range, content));
        }
    };
    let mut first = scan_next(&mut scanner, &mut line_breaks, &mut has_error);
    if first != Token::Eof {
        let first_start = scanner.token_offset;
        add_edit(&mut edits, has_error, String::new(), 0..first_start);
    }
    while first != Token::Eof {
        let mut first_end = scanner.token_offset + scanner.token_length();
        let mut second = scan_next(&mut scanner, &mut line_breaks, &mut has_error);
        let mut replace = String::new();
        let mut needs_line_break = false;
        while line_breaks == 0 && matches!(second, Token::LineComment | Token::BlockComment) {
            let comment_start = scanner.token_offset;
            add_edit(&mut edits, has_error, " ".into(), first_end..comment_start);
            first_end = scanner.token_offset + scanner.token_length();
            needs_line_break = second == Token::LineComment;
            replace = if needs_line_break {
                new_line_and_indent(indent_level)
            } else {
                String::new()
            };
            second = scan_next(&mut scanner, &mut line_breaks, &mut has_error);
        }
        if second == Token::CloseBrace {
            if first != Token::OpenBrace {
                indent_level -= 1;
                replace = new_line_and_indent(indent_level);
            }
        } else if second == Token::CloseBracket {
            if first != Token::OpenBracket {
                indent_level -= 1;
                replace = new_line_and_indent(indent_level);
            }
        } else {
            match first {
                Token::OpenBracket | Token::OpenBrace => {
                    indent_level += 1;
                    replace = new_line_and_indent(indent_level);
                }
                Token::Comma | Token::LineComment => replace = new_line_and_indent(indent_level),
                Token::BlockComment => {
                    if line_breaks > 0 {
                        replace = new_line_and_indent(indent_level);
                    } else if !needs_line_break {
                        replace = " ".into();
                    }
                }
                Token::Colon => {
                    if !needs_line_break {
                        replace = " ".into();
                    }
                }
                Token::String => {
                    if second == Token::Colon && !needs_line_break {
                        replace = String::new();
                    }
                }
                Token::Null
                | Token::True
                | Token::False
                | Token::Number
                | Token::CloseBrace
                | Token::CloseBracket => {
                    if matches!(second, Token::LineComment | Token::BlockComment)
                        && !needs_line_break
                    {
                        replace = " ".into();
                    } else if second != Token::Comma && second != Token::Eof {
                        has_error = true;
                    }
                }
                Token::Unknown => has_error = true,
                _ => {}
            }
            if line_breaks > 0 && matches!(second, Token::LineComment | Token::BlockComment) {
                replace = new_line_and_indent(indent_level);
            }
        }
        if second == Token::Eof {
            replace = String::new();
        }
        let second_start = scanner.token_offset;
        add_edit(&mut edits, has_error, replace, first_end..second_start);
        first = second;
    }
    let mut formatted = text.to_string();
    for (range, content) in edits.into_iter().rev() {
        formatted.replace_range(range, &content);
    }
    formatted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(text: &str) -> Vec<(&'static str, &str, Severity)> {
        json_problems(text)
            .into_iter()
            .map(|problem| (problem.message, &text[problem.range], problem.severity))
            .collect()
    }

    #[test]
    fn accepts_valid_json() {
        assert!(
            json_problems(r#"{"a": [1, 2.5e3, -0.1, true, null], "b": {"c": "é"}}"#).is_empty()
        );
        assert!(json_problems("").is_empty());
    }

    #[test]
    fn reports_missing_and_extra_punctuation_where_monaco_does() {
        assert_eq!(
            messages("{\"a\": 1 \"b\": 2}"),
            vec![("Expected comma", "\"b\"", Severity::Error)]
        );
        assert_eq!(
            messages("{\"a\": 1,}"),
            vec![("Trailing comma", ",", Severity::Error)]
        );
        assert_eq!(
            messages("[1, 2,]"),
            vec![("Trailing comma", ",", Severity::Error)]
        );
        assert_eq!(
            messages("{\"a\" 1}"),
            vec![("Colon expected", "1", Severity::Error)]
        );
        // An empty token reports the last character before it.
        assert_eq!(
            messages("{\"a\": 1\n"),
            vec![("Expected comma or closing brace", "1", Severity::Error)]
        );
        assert_eq!(
            messages("{a: 1}"),
            vec![("Property keys must be doublequoted", "a", Severity::Error)]
        );
        assert_eq!(
            messages("{} []"),
            vec![("End of file expected.", "[", Severity::Error)]
        );
        assert_eq!(
            messages("nope"),
            vec![(
                "Expected a JSON object, array or literal.",
                "nope",
                Severity::Error
            )]
        );
    }

    #[test]
    fn reports_strings_numbers_comments_and_duplicate_keys() {
        assert_eq!(
            messages("{\"a\": \"x\\q\"}"),
            vec![(
                "Invalid escape character in string.",
                "\"x\\q\"",
                Severity::Error
            )]
        );
        assert_eq!(
            messages("[1.]"),
            vec![("Unexpected end of number.", "1.", Severity::Error)]
        );
        assert_eq!(
            messages("// note\n{\"k\": 1, \"k\": 2}"),
            vec![
                ("Duplicate object key", "\"k\"", Severity::Warning),
                ("Duplicate object key", "\"k\"", Severity::Warning),
                (
                    "Comments are not permitted in JSON.",
                    "// note",
                    Severity::Error
                ),
            ]
        );
        let problems = json_problems("{\"k\": 1, \"k\": 2}");
        assert_eq!(problems[0].range, 9..12);
        assert_eq!(problems[1].range, 1..4);
    }

    #[test]
    fn formats_like_jsonc_parser() {
        let options = FormatOptions {
            tab_size: 2,
            insert_spaces: true,
        };
        assert_eq!(
            format_json(r#"{"a":1,"b":[true,null,{}],"c":{"d":"x"}}"#, options),
            "{\n  \"a\": 1,\n  \"b\": [\n    true,\n    null,\n    {}\n  ],\n  \"c\": {\n    \"d\": \"x\"\n  }\n}"
        );
        // Comments stay; the end of line follows the document's.
        assert_eq!(
            format_json("{ // first\r\n\"a\":  1 /* one */ }", options),
            "{ // first\r\n  \"a\": 1 /* one */\r\n}"
        );
        let tabs = FormatOptions {
            tab_size: 4,
            insert_spaces: false,
        };
        assert_eq!(format_json("[1,[2]]", tabs), "[\n\t1,\n\t[\n\t\t2\n\t]\n]");
        // Next to a token it cannot read, nothing moves.
        assert_eq!(format_json("{\"a\":x}", options), "{\n  \"a\":x\n}");
    }
}
