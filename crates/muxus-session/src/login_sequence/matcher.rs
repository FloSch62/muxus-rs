//! What a login sequence waits on (`server/src/login-sequence/output-matcher.ts`).

use std::fmt;

use crate::logging::utf8::Utf8Stream;

/// Output a wait step can match against: the newest characters (UTF-16 code
/// units, as the TypeScript counts them), with ANSI removed.
pub const LOGIN_OUTPUT_WINDOW: usize = 8192;
/// A regex that backtracks more than this many steps is abandoned instead of
/// stalling the backend (the TypeScript stops it after 100 ms).
pub const LOGIN_PATTERN_BACKTRACK_LIMIT: usize = 1_000_000;
/// Cursor-forward moves render as at most this many spaces.
const MAX_CURSOR_FORWARD: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Text,
    Escape,
    Intermediate,
    Csi,
    String,
    StringEscape,
}

/// Terminal output as the text a person reads: UTF-8 decoded across chunk
/// boundaries, escape sequences (colours, cursor movement, titles) removed
/// and line endings reduced to `\n`. A sequence split between two chunks is
/// still recognised, because the parser state carries over.
#[derive(Debug, Clone)]
pub struct TerminalTextStream {
    decoder: Utf8Stream,
    state: State,
    csi: String,
}

impl Default for TerminalTextStream {
    fn default() -> Self {
        Self {
            decoder: Utf8Stream::new(),
            state: State::Text,
            csi: String::new(),
        }
    }
}

impl TerminalTextStream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&mut self, data: &[u8]) -> String {
        let mut text = String::new();
        for ch in self.decoder.decode(data).chars() {
            let code = ch as u32;
            match self.state {
                State::Text => {
                    if code == 0x1b {
                        self.state = State::Escape;
                    } else if ch == '\n' || ch == '\t' || (code >= 0x20 && code != 0x7f) {
                        // Carriage returns, bells, backspaces and other controls carry no text.
                        text.push(ch);
                    }
                }
                State::Escape => {
                    if ch == '[' {
                        self.csi.clear();
                        self.state = State::Csi;
                    } else if matches!(ch, ']' | 'P' | 'X' | '^' | '_') {
                        self.state = State::String;
                    } else if (0x20..=0x2f).contains(&code) {
                        self.state = State::Intermediate;
                    } else {
                        self.state = State::Text;
                    }
                }
                State::Intermediate => {
                    if (0x30..=0x7e).contains(&code) {
                        self.state = State::Text;
                    }
                }
                State::Csi => {
                    if (0x40..=0x7e).contains(&code) {
                        // Some devices space words with cursor-forward instead of blanks.
                        if ch == 'C' {
                            text.extend(std::iter::repeat_n(' ', cursor_forward(&self.csi)));
                        }
                        self.state = State::Text;
                    } else if self.csi.chars().count() < 64 {
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
                    self.state = if ch == '\\' {
                        State::Text
                    } else {
                        State::String
                    };
                }
            }
        }
        text
    }
}

/// `Number.parseInt(params, 10)` as a cursor-forward count: 1 when absent or invalid.
fn cursor_forward(params: &str) -> usize {
    let digits: String = params
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    match digits.parse::<usize>() {
        Ok(count) if count > 0 => count.min(MAX_CURSOR_FORWARD),
        Ok(_) => 1,
        Err(_) if !digits.is_empty() => MAX_CURSOR_FORWARD,
        Err(_) => 1,
    }
}

/// Where a match ends in the current window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputMatch {
    /// Byte offset just past the match.
    pub end: usize,
}

/// Why a wait step could not match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchError {
    /// The pattern took too long to match.
    PatternTimeout,
    Other(String),
}

impl fmt::Display for MatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PatternTimeout => f.write_str("The pattern took too long to match."),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for MatchError {}

/// A compiled wait step pattern.
#[derive(Debug, Clone)]
pub enum WaitPattern {
    Literal(String),
    Regex(fancy_regex::Regex),
}

/// Compile a wait step's pattern; a regular expression that does not compile
/// returns its error message.
pub fn compile_wait_pattern(pattern: &str, regex: bool) -> Result<WaitPattern, String> {
    if !regex {
        return Ok(WaitPattern::Literal(pattern.to_string()));
    }
    fancy_regex::RegexBuilder::new(pattern)
        .backtrack_limit(LOGIN_PATTERN_BACKTRACK_LIMIT)
        .build()
        .map(WaitPattern::Regex)
        .map_err(|err| err.to_string())
}

/// The output a login sequence waits on. It keeps a bounded window of the
/// newest text, so text split across chunks still matches while a stream
/// that never matches cannot grow it, and a regex never runs over more than
/// the window. A match consumes the window up to its end, so the next wait
/// step only sees what came after it.
#[derive(Debug, Clone)]
pub struct LoginOutputBuffer {
    stream: TerminalTextStream,
    text: String,
    /// Byte offset a literal search may resume from: earlier text was searched.
    searched: usize,
    window: usize,
}

impl Default for LoginOutputBuffer {
    fn default() -> Self {
        Self::new(LOGIN_OUTPUT_WINDOW)
    }
}

impl LoginOutputBuffer {
    /// A buffer keeping the newest `window` UTF-16 code units.
    pub fn new(window: usize) -> Self {
        Self {
            stream: TerminalTextStream::new(),
            text: String::new(),
            searched: 0,
            window,
        }
    }

    pub fn content(&self) -> &str {
        &self.text
    }

    pub fn append(&mut self, data: &[u8]) {
        let added = self.stream.write(data);
        if added.is_empty() {
            return;
        }
        self.text.push_str(&added);
        let mut units: usize = self.text.encode_utf16().count();
        if units <= self.window {
            return;
        }
        let mut cut = 0;
        for ch in self.text.chars() {
            if units <= self.window {
                break;
            }
            units -= ch.len_utf16();
            cut += ch.len_utf8();
        }
        self.text.drain(..cut);
        self.searched = self.searched.saturating_sub(cut);
    }

    /// The first match in the window, or `None`. Regexes are bounded in effort.
    pub fn find(&mut self, pattern: &WaitPattern) -> Result<Option<OutputMatch>, MatchError> {
        match pattern {
            WaitPattern::Literal(literal) => {
                if literal.is_empty() {
                    return Ok(Some(OutputMatch { end: 0 }));
                }
                // Resume a little before the searched end, so a match spanning two chunks is found.
                let mut from = self.searched.saturating_sub(literal.len() - 1);
                while !self.text.is_char_boundary(from) {
                    from -= 1;
                }
                let found = self.text[from..]
                    .find(literal.as_str())
                    .map(|index| from + index);
                self.searched = self.text.len();
                Ok(found.map(|index| OutputMatch {
                    end: index + literal.len(),
                }))
            }
            WaitPattern::Regex(regex) => match regex.find(&self.text) {
                Ok(found) => Ok(found.map(|found| OutputMatch { end: found.end() })),
                Err(fancy_regex::Error::RuntimeError(
                    fancy_regex::RuntimeError::BacktrackLimitExceeded,
                )) => Err(MatchError::PatternTimeout),
                Err(err) => Err(MatchError::Other(err.to_string())),
            },
        }
    }

    /// Drop everything up to `end`, the end of a match.
    pub fn consume(&mut self, end: usize) {
        self.text.drain(..end.min(self.text.len()));
        self.searched = 0;
    }
}
