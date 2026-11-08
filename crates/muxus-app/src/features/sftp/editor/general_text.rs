//! The General Text language (`client/src/editor/monaco-setup.ts`): Monaco's Monarch tokenizer
//! for logs, configuration and notes that no other language claims. Levels (`ERROR`, `WARN`,
//! `INFO`), `key: value` and `key=value` pairs, dates, times, addresses, numbers with units,
//! links, variables, paths and quoted strings get their Monaco token, and brackets outside
//! strings and comments take the bracket pair colors.
//!
//! Monarch tries each rule, in order, anchored at the current position of the line, and takes
//! the first that matches; a position no rule matches is one character of plain text. A rule
//! starting with `^` is tried at the start of the line only. The rules are matched against the
//! rest of the line, so a leading `\b` only says the next character is a word character; the
//! patterns below drop it where the rule starts with one anyway.

use std::ops::Range;
use std::rc::Rc;
use std::sync::LazyLock;

use muxus_ui::gpui_kit::base::input::{
    FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter, InputHighlighterFactory, Rope,
};
use muxus_ui::gpui_kit::{Context, HighlightStyle, SharedString, Window};
use regex_automata::{Anchored, Input, meta, util::syntax};

use super::language::GENERAL_TEXT_LANGUAGE_ID;
use crate::util::code_editor::{
    BracketColor, BracketPools, SharedProbe, color_brackets, probe_highlighter,
    probed_highlighter_factory,
};

/// What a rule makes of its match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Token(&'static str),
    /// `@brackets`: the bracket's own delimiter token.
    Brackets,
    /// `['', 'attribute.name', '', 'delimiter']` over the rule's four groups.
    Attribute,
}

/// The tokenizer's `root` rules, in Monarch order (`ignoreCase: true`; JavaScript's `\w`, `\d`
/// and `\b` are ASCII).
const RULES: &[(&str, Action)] = &[
    (r"^(?:<{7}|={7}|>{7}).*$", Action::Token("invalid")),
    (r"^\s*#{1,6}\s+.*$", Action::Token("keyword")),
    (r"^\s*(?:#|;|//).*$", Action::Token("comment")),
    (
        r"(?:ERROR|FATAL|CRITICAL|PANIC|FAIL(?:ED|URE)?)(?-u:\b)",
        Action::Token("invalid"),
    ),
    (r"(?:WARN|WARNING|CAUTION)(?-u:\b)", Action::Token("regexp")),
    (r"(?:INFO|NOTICE|SUCCESS|OK)(?-u:\b)", Action::Token("type")),
    (r"(?:TRACE|DEBUG|VERBOSE)(?-u:\b)", Action::Token("comment")),
    (
        r"(?:TODO|FIXME|HACK|NOTE|IMPORTANT|XXX)(?-u:\b)",
        Action::Token("keyword"),
    ),
    (ATTRIBUTE, Action::Attribute),
    (
        r"(?:true|false|yes|no|on|off|enabled|disabled)(?-u:\b)",
        Action::Token("keyword"),
    ),
    (
        r"(?:null|nil|none|undefined|n/a)(?-u:\b)",
        Action::Token("constant"),
    ),
    (
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}[T ][0-9:.+-]*Z?(?-u:\b)",
        Action::Token("number"),
    ),
    (
        r"[0-9]{1,2}:[0-9]{2}(?::[0-9]{2}(?:\.[0-9]+)?)?(?-u:\b)",
        Action::Token("number"),
    ),
    (
        r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?::[0-9]{1,5})?(?-u:\b)",
        Action::Token("number"),
    ),
    (
        r"[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}(?-u:\b)",
        Action::Token("number.hex"),
    ),
    (r"0x[0-9a-f]+(?-u:\b)", Action::Token("number.hex")),
    (
        r"[0-9]+(?:\.[0-9]+)?(?:ms|s|min|h|d|b|kb|mb|gb|tb|kib|mib|gib|tib|hz|khz|mhz|ghz|%)(?-u:\b)",
        Action::Token("number"),
    ),
    (r"[0-9]+(?:\.[0-9]+)?(?-u:\b)", Action::Token("number")),
    (
        r#"(?:https?|wss?|ftp)://[^\s<>{}\[\]"']+"#,
        Action::Token("string.link"),
    ),
    // `\b[\w.+-]+@…`: the boundary at the start asks for a word character first.
    (
        r"[0-9A-Za-z_][0-9A-Za-z_.+-]*@[0-9A-Za-z_.-]+\.[A-Za-z]{2,}(?-u:\b)",
        Action::Token("string.link"),
    ),
    (
        r"\$\{?[A-Za-z_][0-9A-Za-z_.-]*\}?",
        Action::Token("variable"),
    ),
    // `(?:^|\s)`: inside the rule `^` is the current position, not the line's start.
    (
        r"(?:|\s)--?[A-Za-z][0-9A-Za-z_-]*",
        Action::Token("variable"),
    ),
    (
        r#"[A-Za-z]:\\(?:[^\\/:*?"<>|\r\n]+\\)*[^\\/:*?"<>|\r\n]*"#,
        Action::Token("string.path"),
    ),
    (
        r"(?:~|\.{1,2})?/(?:[0-9A-Za-z_.@+-]+/)*[0-9A-Za-z_.@+-]+",
        Action::Token("string.path"),
    ),
    (r#""(?:\\.|[^"\\])*""#, Action::Token("string")),
    (r"'(?:\\.|[^'\\])*'", Action::Token("string")),
    (r"[{}()\[\]]", Action::Brackets),
    (r"[=:,|]", Action::Token("delimiter")),
];

const ATTRIBUTE: &str = r"^(\s*(?:-\s+)?)([A-Za-z_][0-9A-Za-z_.-]*)(\s*)([:=])";

/// Monaco leaves longer lines untokenized (`maxTokenizationLineLength`).
const MAX_TOKENIZATION_LINE_LENGTH: usize = 20_000;

static RULES_REGEX: LazyLock<meta::Regex> = LazyLock::new(|| {
    meta::Builder::new()
        .syntax(syntax::Config::new().case_insensitive(true))
        .build_many(
            &RULES
                .iter()
                .map(|(pattern, _)| *pattern)
                .collect::<Vec<_>>(),
        )
        .expect("the General Text rules compile")
});

static ATTRIBUTE_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(&format!("(?i){ATTRIBUTE}")).expect("valid pattern"));

/// A token of one line: its byte range in the line and Monarch's token name.
pub type LineToken = (Range<usize>, &'static str);

/// The tokens of one line (without its line break), plain text left out.
pub fn tokenize_line(line: &str) -> Vec<LineToken> {
    let mut tokens = Vec::new();
    if line.encode_utf16().count() > MAX_TOKENIZATION_LINE_LENGTH {
        return tokens;
    }
    let mut pos = 0;
    while pos < line.len() {
        let input = Input::new(line).range(pos..).anchored(Anchored::Yes);
        let Some(found) = RULES_REGEX.find(input).filter(|m| !m.is_empty()) else {
            pos += line[pos..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let range = found.range();
        match RULES[found.pattern().as_usize()].1 {
            Action::Token(token) => tokens.push((range.clone(), token)),
            Action::Brackets => {
                let token = match &line[range.clone()] {
                    "{" | "}" => "delimiter.curly",
                    "[" | "]" => "delimiter.square",
                    _ => "delimiter.parenthesis",
                };
                tokens.push((range.clone(), token));
            }
            Action::Attribute => {
                if let Some(groups) = ATTRIBUTE_REGEX.captures(line) {
                    for (index, token) in [(2, "attribute.name"), (4, "delimiter")] {
                        if let Some(group) = groups.get(index).filter(|g| !g.is_empty()) {
                            tokens.push((group.range(), token));
                        }
                    }
                }
            }
        }
        pos = range.end;
    }
    tokens
}

/// Monaco's standard token types (`/\b(comment|string|regex|regexp)\b/` on the token name):
/// brackets inside strings, comments and regular expressions are not brackets; inside any
/// other token (a heading, a conflict marker) they are.
fn holds_brackets(token: &str) -> bool {
    !token
        .split(['.', '-'])
        .any(|part| matches!(part, "comment" | "string" | "regex" | "regexp"))
}

/// The byte offset where each line starts.
fn line_starts(text: &Rope) -> Vec<usize> {
    let mut starts = vec![0];
    let mut offset = 0;
    for chunk in text.chunks() {
        for (index, byte) in chunk.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(offset + index + 1);
            }
        }
        offset += chunk.len();
    }
    starts
}

fn line_of(starts: &[usize], offset: usize) -> usize {
    starts
        .partition_point(|&start| start <= offset)
        .saturating_sub(1)
}

/// The text of `line` without its line break.
fn line_text(text: &Rope, starts: &[usize], line: usize) -> String {
    let start = starts[line];
    let end = starts.get(line + 1).copied().unwrap_or(text.len());
    let mut content = text.slice(start..end).to_string();
    if content.ends_with('\n') {
        content.pop();
        if content.ends_with('\r') {
            content.pop();
        }
    }
    content
}

/// The General Text highlighter: tokens cached per line, retokenized for the lines an edit
/// touched (General Text keeps no state from one line to the next).
pub struct GeneralTextHighlighter {
    text: Rope,
    starts: Vec<usize>,
    lines: Vec<Vec<LineToken>>,
    /// Document brackets with their color.
    brackets: Vec<(usize, BracketColor)>,
}

impl Default for GeneralTextHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl GeneralTextHighlighter {
    pub fn new() -> Self {
        Self {
            text: Rope::new(),
            starts: vec![0],
            lines: vec![Vec::new()],
            brackets: Vec::new(),
        }
    }

    /// Take the new text, retokenizing what `edit` changed (everything without one).
    pub fn sync(&mut self, edit: Option<&InputEdit>, text: &Rope) {
        let starts = line_starts(text);
        match edit {
            Some(edit) if self.lines.len() == self.starts.len() => {
                let old_first = line_of(&self.starts, edit.start_byte);
                let old_last = line_of(&self.starts, edit.old_end_byte);
                let new_first = line_of(&starts, edit.start_byte);
                let new_last = line_of(&starts, edit.new_end_byte);
                let fresh: Vec<Vec<LineToken>> = (new_first..=new_last)
                    .map(|line| tokenize_line(&line_text(text, &starts, line)))
                    .collect();
                let old_last = old_last.min(self.lines.len().saturating_sub(1));
                self.lines.splice(old_first..=old_last, fresh);
            }
            _ => {
                self.lines = (0..starts.len())
                    .map(|line| tokenize_line(&line_text(text, &starts, line)))
                    .collect();
            }
        }
        if self.lines.len() != starts.len() {
            // The edit did not line up with the cache; start over.
            self.lines = (0..starts.len())
                .map(|line| tokenize_line(&line_text(text, &starts, line)))
                .collect();
        }
        self.starts = starts;
        self.text = text.clone();
        let mut found = Vec::new();
        for (line, tokens) in self.lines.iter().enumerate() {
            let start = self.starts[line];
            for (range, token) in tokens {
                if !holds_brackets(token) {
                    continue;
                }
                let from = start + range.start;
                let content = self.text.slice(from..start + range.end).to_string();
                found.extend(
                    content
                        .char_indices()
                        .filter(|(_, c)| matches!(c, '(' | ')' | '[' | ']' | '{' | '}'))
                        .map(|(offset, c)| (from + offset, c)),
                );
            }
        }
        self.brackets = color_brackets(&found, BracketPools::PerKind);
    }

    /// The tokens of the document's lines, as cached.
    pub fn line_tokens(&self, line: usize) -> &[LineToken] {
        self.lines.get(line).map_or(&[], Vec::as_slice)
    }

    pub fn bracket_colors(&self) -> &[(usize, BracketColor)] {
        &self.brackets
    }
}

/// Monaco's indentation folding (no language folds General Text otherwise): a line folds the
/// lines after it that are indented deeper, up to the last of them.
pub fn indentation_folds(lines: &[String], tab_size: usize) -> Vec<(usize, usize)> {
    let indent = |line: &str| -> Option<usize> {
        let mut width = 0;
        for c in line.chars() {
            match c {
                ' ' => width += 1,
                '\t' => width += tab_size - width % tab_size,
                _ => return Some(width),
            }
        }
        None
    };
    let indents: Vec<Option<usize>> = lines.iter().map(|line| indent(line)).collect();
    let mut folds = Vec::new();
    for (start, level) in indents.iter().enumerate() {
        let Some(level) = *level else { continue };
        let mut end = None;
        for (line, deeper) in indents.iter().enumerate().skip(start + 1) {
            match deeper {
                None => continue,
                Some(deeper) if *deeper > level => end = Some(line),
                Some(_) => break,
            }
        }
        if let Some(end) = end {
            folds.push((start, end));
        }
    }
    folds
}

/// The remote editor's highlighters: General Text by its tokenizer, every other language by
/// its tree-sitter grammar (brackets colored per kind, `independentColorPoolPerBracketType`).
/// Each parse is reported to `probe` (problem markers, the minimap).
pub fn remote_editor_highlighters(probe: SharedProbe) -> InputHighlighterFactory {
    let grammars = probed_highlighter_factory(BracketPools::PerKind, probe.clone());
    Rc::new(move |language: &str| {
        if language == GENERAL_TEXT_LANGUAGE_ID {
            Some(probe_highlighter(
                Box::new(GeneralTextHighlighter::new()),
                &probe,
            ))
        } else {
            grammars(language)
        }
    })
}

impl InputHighlighter for GeneralTextHighlighter {
    fn language(&self) -> SharedString {
        GENERAL_TEXT_LANGUAGE_ID.into()
    }

    fn update(
        &mut self,
        edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        _cx: &mut Context<muxus_ui::gpui_kit::base::input::EditorState>,
    ) {
        self.sync(edit.as_ref(), text);
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut runs: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        let mut cursor = range.start;
        let first = self
            .brackets
            .partition_point(|(offset, _)| *offset < range.start);
        let mut brackets = self.brackets[first..].iter().peekable();
        let mut push = |runs: &mut Vec<(Range<usize>, HighlightStyle)>,
                        run: Range<usize>,
                        style: HighlightStyle| {
            let run = run.start.max(range.start)..run.end.min(range.end);
            if run.is_empty() {
                return;
            }
            if cursor < run.start {
                runs.push((cursor..run.start, HighlightStyle::default()));
            }
            cursor = run.end;
            runs.push((run, style));
        };
        let first_line = line_of(&self.starts, range.start);
        let last_line = line_of(&self.starts, range.end);
        for line in first_line..=last_line.min(self.lines.len().saturating_sub(1)) {
            let start = self.starts[line];
            for (token_range, token) in &self.lines[line] {
                let absolute = start + token_range.start..start + token_range.end;
                let style = resolver.style(token).unwrap_or_default();
                // Brackets inside the token take their pair color.
                let mut from = absolute.start;
                while let Some(&&(offset, color)) = brackets.peek() {
                    if offset >= absolute.end {
                        break;
                    }
                    brackets.next();
                    if offset < from {
                        continue;
                    }
                    push(&mut runs, from..offset, style);
                    let bracket_style = resolver.style(color.token()).unwrap_or(style);
                    push(&mut runs, offset..offset + 1, bracket_style);
                    from = offset + 1;
                }
                push(&mut runs, from..absolute.end, style);
            }
        }
        if cursor < range.end {
            runs.push((cursor..range.end, HighlightStyle::default()));
        }
        runs
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        let lines: Vec<String> = (0..self.starts.len())
            .map(|line| line_text(&self.text, &self.starts, line))
            .collect();
        indentation_folds(&lines, 2)
            .into_iter()
            .map(|(start, end)| FoldRange::new(start, end))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(line: &str) -> Vec<(&str, &'static str)> {
        tokenize_line(line)
            .into_iter()
            .map(|(range, token)| (&line[range], token))
            .collect()
    }

    #[test]
    fn colors_log_levels_dates_and_addresses() {
        assert_eq!(
            tokens("2026-07-25T20:17:03Z ERROR db 10.0.0.12:5432 refused"),
            vec![
                ("2026-07-25T20:17:03Z", "number"),
                ("ERROR", "invalid"),
                ("10.0.0.12:5432", "number"),
            ]
        );
        assert_eq!(
            tokens("[warn] retry in 500ms"),
            vec![
                ("[", "delimiter.square"),
                ("warn", "regexp"),
                ("]", "delimiter.square"),
                ("500ms", "number"),
            ]
        );
        // `WARNING` is the longer alternative once `WARN` leaves no boundary.
        assert_eq!(tokens("WARNING"), vec![("WARNING", "regexp")]);
        assert_eq!(tokens("info ok"), vec![("info", "type"), ("ok", "type")]);
    }

    #[test]
    fn matches_rules_against_the_rest_of_the_line() {
        // Monarch drops the left context: `ERROR` inside a word is still a level.
        assert_eq!(tokens("fooERROR"), vec![("ERROR", "invalid")]);
        // `(?:^|\s)` is satisfied at every position, so a dash inside a word starts a flag.
        assert_eq!(tokens("a-b"), vec![("-b", "variable")]);
        assert_eq!(
            tokens("run --force -v"),
            vec![(" --force", "variable"), (" -v", "variable")]
        );
    }

    #[test]
    fn reads_key_value_pairs_at_the_start_of_a_line_only() {
        assert_eq!(
            tokens("  - LOG_LEVEL: info"),
            vec![
                ("LOG_LEVEL", "attribute.name"),
                (":", "delimiter"),
                ("info", "type")
            ]
        );
        assert_eq!(
            tokens("timeout=30s"),
            vec![
                ("timeout", "attribute.name"),
                ("=", "delimiter"),
                ("30s", "number")
            ]
        );
        assert_eq!(tokens("x a=b"), vec![("=", "delimiter")]);
    }

    #[test]
    fn takes_whole_lines_for_headings_comments_and_conflict_markers() {
        assert_eq!(
            tokens("## Notes (draft)"),
            vec![("## Notes (draft)", "keyword")]
        );
        assert_eq!(tokens("  ; disabled"), vec![("  ; disabled", "comment")]);
        assert_eq!(tokens("// TODO later"), vec![("// TODO later", "comment")]);
        assert_eq!(tokens("<<<<<<< HEAD"), vec![("<<<<<<< HEAD", "invalid")]);
        // A heading needs a space after the hashes; `#tag` is a comment.
        assert_eq!(tokens("#tag"), vec![("#tag", "comment")]);
    }

    #[test]
    fn finds_links_paths_variables_and_strings() {
        assert_eq!(
            tokens("see https://example.com/a?b=1 or ops@example.org"),
            vec![
                ("https://example.com/a?b=1", "string.link"),
                ("ops@example.org", "string.link"),
            ]
        );
        assert_eq!(
            tokens("cp ${HOME}/x ~/backup/x.tar.gz"),
            vec![
                ("${HOME}", "variable"),
                ("/x", "string.path"),
                ("~/backup/x.tar.gz", "string.path"),
            ]
        );
        assert_eq!(
            tokens(r#"run C:\Program Files\app.exe "quoted \" text" 'single'"#),
            vec![
                (r"C:\Program Files\app.exe ", "string.path"),
                (r#""quoted \" text""#, "string"),
                ("'single'", "string"),
            ]
        );
        assert_eq!(
            tokens("id 123e4567-e89b-12d3-a456-426614174000 0xFF null n/a"),
            vec![
                ("123e4567-e89b-12d3-a456-426614174000", "number.hex"),
                ("0xFF", "number.hex"),
                ("null", "constant"),
                ("n/a", "constant"),
            ]
        );
    }

    #[test]
    fn colors_brackets_outside_strings_only() {
        let mut highlighter = GeneralTextHighlighter::new();
        let text = Rope::from_str("f(\"(\") [x]\n# (heading)\n");
        highlighter.sync(None, &text);
        let offsets: Vec<usize> = highlighter
            .bracket_colors()
            .iter()
            .map(|(offset, _)| *offset)
            .collect();
        // The quoted bracket is a string; a heading's brackets are brackets (Monaco counts
        // them in any token that is not a string, comment or regular expression).
        assert_eq!(offsets, vec![1, 5, 7, 9, 13, 21]);
        assert!(holds_brackets("keyword"));
        assert!(!holds_brackets("string.link"));
    }

    fn point(row: usize, column: usize) -> muxus_ui::gpui_kit::base::input::Point {
        muxus_ui::gpui_kit::base::input::Point::new(row, column)
    }

    #[test]
    fn retokenizes_only_the_lines_an_edit_touched() {
        let mut highlighter = GeneralTextHighlighter::new();
        let before = Rope::from_str("ERROR one\nplain\nWARN three\n");
        highlighter.sync(None, &before);
        // Replace "plain" with "INFO\nOK".
        let after = Rope::from_str("ERROR one\nINFO\nOK\nWARN three\n");
        let start = 10;
        let edit = InputEdit {
            start_byte: start,
            old_end_byte: start + 5,
            new_end_byte: start + 7,
            start_position: point(1, 0),
            old_end_position: point(1, 5),
            new_end_position: point(2, 2),
        };
        highlighter.sync(Some(&edit), &after);
        let names: Vec<Vec<&str>> = (0..5)
            .map(|line| {
                highlighter
                    .line_tokens(line)
                    .iter()
                    .map(|(_, t)| *t)
                    .collect()
            })
            .collect();
        assert_eq!(
            names,
            vec![
                vec!["invalid"],
                vec!["type"],
                vec!["type"],
                vec!["regexp"],
                vec![]
            ]
        );
    }

    #[test]
    fn folds_deeper_indented_lines() {
        let lines: Vec<String> = ["a", "  b", "", "  c", "d", "  e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(indentation_folds(&lines, 2), vec![(0, 3), (4, 5)]);
    }
}
