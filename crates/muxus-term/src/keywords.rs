//! Keyword highlighting: matching rules against terminal lines and turning matches into cell
//! decorations (port of `client/src/terminal/keyword-matching.ts` and
//! `keyword-highlighting.ts`).
//!
//! Regex rules are JavaScript regular expressions (that is what Muxus stores and exports). They
//! are translated to `fancy-regex`, which supports the lookaround the built-in profiles use. A
//! pattern that backtracks catastrophically hits the engine's backtrack limit instead of freezing
//! the UI; it is then paused for the session exactly like the TypeScript worker pauses a pattern
//! that stalls, so the worker and its watchdog are not needed.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use crate::buffer::{CellText, TextBuffer};
use crate::selection::BufferPoint;

pub use muxus_core::api::{
    HostKeywordHighlightConfig, KeywordHighlightProfile, KeywordHighlightRule,
};

/// Rule helpers the shared type lacks.
pub trait KeywordRuleExt {
    /// The keyword is a regular expression (`regex: true`).
    fn is_regex(&self) -> bool;
}

impl KeywordRuleExt for KeywordHighlightRule {
    fn is_regex(&self) -> bool {
        self.regex == Some(true)
    }
}

/// The rule fields that decide what a rule matches.
pub trait KeywordMatcher {
    fn keyword(&self) -> &str;
    fn case_sensitive(&self) -> bool;
    fn whole_word(&self) -> bool;
    fn regex(&self) -> bool;
}

impl KeywordMatcher for KeywordHighlightRule {
    fn keyword(&self) -> &str {
        &self.keyword
    }
    fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }
    fn whole_word(&self) -> bool {
        self.whole_word
    }
    fn regex(&self) -> bool {
        self.is_regex()
    }
}

/// Resolve the effective rules for a local/ad-hoc terminal or one saved host.
pub fn resolve_keyword_highlights(
    global_rules: &[KeywordHighlightRule],
    host_config: Option<&HostKeywordHighlightConfig>,
    profile_rules: &[KeywordHighlightRule],
) -> Vec<KeywordHighlightRule> {
    let Some(host) = host_config else {
        return global_rules.to_vec();
    };
    let mut rules = Vec::new();
    if host.inherit_global {
        rules.extend_from_slice(global_rules);
    }
    rules.extend_from_slice(profile_rules);
    rules.extend_from_slice(&host.rules);
    rules
}

/// One match: byte offsets into the line and the rule that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordMatch<'r, R> {
    pub start: usize,
    pub end: usize,
    pub rule: &'r R,
}

pub const MAX_KEYWORD_DECORATIONS: usize = 500;

fn is_word_character(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_whole_word(text: &str, start: usize, end: usize) -> bool {
    !is_word_character(text[..start].chars().next_back())
        && !is_word_character(text[end..].chars().next())
}

/// Translate a JavaScript regular expression source to fancy-regex syntax: `\d`, `\w` and `\b`
/// are ASCII in JavaScript, and named groups use `(?P<name>)`.
pub fn translate_js_regex(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len() + 16);
    let mut in_class = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == '\\' && index + 1 < chars.len() {
            let next = chars[index + 1];
            index += 2;
            let replacement = match (in_class, next) {
                (false, 'd') => "[0-9]",
                (false, 'D') => "[^0-9]",
                (false, 'w') => "[A-Za-z0-9_]",
                (false, 'W') => "[^A-Za-z0-9_]",
                (false, 'b') => {
                    r"(?:(?<![A-Za-z0-9_])(?=[A-Za-z0-9_])|(?<=[A-Za-z0-9_])(?![A-Za-z0-9_]))"
                }
                (false, 'B') => {
                    r"(?:(?<=[A-Za-z0-9_])(?=[A-Za-z0-9_])|(?<![A-Za-z0-9_])(?![A-Za-z0-9_]))"
                }
                (true, 'd') => "0-9",
                (true, 'w') => "A-Za-z0-9_",
                (_, '/') => "/",
                _ => {
                    out.push('\\');
                    out.push(next);
                    continue;
                }
            };
            out.push_str(replacement);
            continue;
        }
        if in_class {
            if c == ']' {
                in_class = false;
            }
            out.push(c);
            index += 1;
            continue;
        }
        if c == '[' {
            in_class = true;
            out.push(c);
            // A leading `^` or `]` belongs to the class syntax.
            if chars.get(index + 1) == Some(&'^') {
                out.push('^');
                index += 1;
            }
            index += 1;
            continue;
        }
        if c == '(' && chars.get(index + 1) == Some(&'?') && chars.get(index + 2) == Some(&'<') {
            let after = chars.get(index + 3).copied();
            if after.is_some_and(|a| a != '=' && a != '!') {
                out.push_str("(?P<");
                index += 3;
                continue;
            }
        }
        out.push(c);
        index += 1;
    }
    out
}

const MAX_COMPILED_PATTERNS: usize = 256;
/// Steps a regex may backtrack on one line before it counts as stalled.
const BACKTRACK_LIMIT: usize = 200_000;

type CompiledPattern = Option<Arc<fancy_regex::Regex>>;

// Keyed by source and flags rather than by rule object.
static COMPILED_PATTERNS: LazyLock<Mutex<HashMap<String, CompiledPattern>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn compile(keyword: &str, case_sensitive: bool) -> Result<fancy_regex::Regex, String> {
    let translated = translate_js_regex(keyword);
    let source = if case_sensitive {
        translated
    } else {
        format!("(?i){translated}")
    };
    fancy_regex::RegexBuilder::new(&source)
        .backtrack_limit(BACKTRACK_LIMIT)
        .build()
        .map_err(|error| format!("Invalid regular expression: /{keyword}/: {error}"))
}

fn rule_pattern<R: KeywordMatcher + ?Sized>(rule: &R) -> CompiledPattern {
    let key = format!(
        "{}/{}",
        if rule.case_sensitive() { "g" } else { "gi" },
        rule.keyword()
    );
    let mut cache = COMPILED_PATTERNS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pattern) = cache.get(&key) {
        return pattern.clone();
    }
    if cache.len() >= MAX_COMPILED_PATTERNS {
        cache.clear();
    }
    let pattern = compile(rule.keyword(), rule.case_sensitive())
        .ok()
        .map(Arc::new);
    cache.insert(key, pattern.clone());
    pattern
}

// Patterns that ran too long. They stay paused for the session, so a pattern that hung once is
// not retried on every frame; editing it produces a new pattern that is tried again.
static SLOW_PATTERNS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
type SlowListener = Box<dyn Fn(&str) + Send + Sync>;
static SLOW_LISTENERS: LazyLock<Mutex<Vec<(u64, SlowListener)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static NEXT_LISTENER: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));

pub fn is_slow_keyword_pattern<R: KeywordMatcher + ?Sized>(rule: &R) -> bool {
    rule.regex()
        && SLOW_PATTERNS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(rule.keyword())
}

/// Pause a pattern for the session and tell the listeners (the app shows a warning naming it).
pub fn mark_slow_keyword_pattern(keyword: &str) {
    let inserted = SLOW_PATTERNS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(keyword.to_string());
    if !inserted {
        return;
    }
    for (_, listener) in SLOW_LISTENERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
    {
        listener(keyword);
    }
}

/// The number of paused patterns; it changes whenever a pattern is paused.
pub fn slow_keyword_pattern_count() -> usize {
    SLOW_PATTERNS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .len()
}

/// Unsubscribes a slow-pattern listener when dropped.
pub struct SlowPatternSubscription(u64);

impl Drop for SlowPatternSubscription {
    fn drop(&mut self) {
        SLOW_LISTENERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(id, _)| *id != self.0);
    }
}

/// Call `listener` with each newly paused pattern while the subscription lives.
pub fn subscribe_slow_keyword_patterns(
    listener: impl Fn(&str) + Send + Sync + 'static,
) -> SlowPatternSubscription {
    let id = {
        let mut next = NEXT_LISTENER.lock().unwrap_or_else(|e| e.into_inner());
        *next += 1;
        *next
    };
    SLOW_LISTENERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id, Box::new(listener)));
    SlowPatternSubscription(id)
}

/// Why a regex rule cannot match, so editors can flag it; literals never fail.
pub fn keyword_pattern_error(keyword: &str, regex: bool) -> Option<String> {
    if !regex || keyword.is_empty() {
        return None;
    }
    if let Err(error) = compile(keyword, true) {
        return Some(error);
    }
    SLOW_PATTERNS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(keyword)
        .then(|| {
            "This pattern took too long to match and is paused. Simplify it to try again.".into()
        })
}

/// The first reason a host's rules cannot be saved, or `None` when all are usable.
pub fn keyword_highlight_rules_problem(rules: &[KeywordHighlightRule]) -> Option<String> {
    for rule in rules {
        if rule.keyword.trim().is_empty() {
            return Some("Every highlighting rule needs a keyword.".into());
        }
        if let Some(error) = keyword_pattern_error(&rule.keyword, rule.is_regex()) {
            return Some(format!("Highlighting: {error}"));
        }
    }
    None
}

fn find_pattern_matches<'r, R: KeywordMatcher>(
    text: &str,
    rule: &'r R,
    matches: &mut Vec<KeywordMatch<'r, R>>,
    limit: usize,
) {
    // An invalid pattern stays inert while its editor shows the syntax error.
    let Some(pattern) = rule_pattern(rule) else {
        return;
    };
    let mut from = 0;
    while matches.len() < limit && from <= text.len() {
        let found = match pattern.find_from_pos(text, from) {
            Ok(Some(found)) => found,
            Ok(None) => break,
            Err(_) => {
                // Backtracked past the limit: pause the pattern like a stalled worker.
                mark_slow_keyword_pattern(rule.keyword());
                return;
            }
        };
        let (start, end) = (found.start(), found.end());
        if end == start {
            // Step past an empty match (`^`, `\b`, `x*`) instead of looping on it.
            from = start + text[start..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        if !rule.whole_word() || is_whole_word(text, start, end) {
            matches.push(KeywordMatch { start, end, rule });
        }
        from = end;
    }
}

/// `text` folded to lower case one character at a time, with the byte offset in `text` of every
/// folded character (and of the end), so matches map back exactly.
fn fold_case(text: &str) -> (Vec<char>, Vec<usize>) {
    let mut chars = Vec::with_capacity(text.len());
    let mut offsets = Vec::with_capacity(text.len() + 1);
    for (offset, c) in text.char_indices() {
        chars.push(c.to_lowercase().next().unwrap_or(c));
        offsets.push(offset);
    }
    offsets.push(text.len());
    (chars, offsets)
}

fn find_literal<'r, R: KeywordMatcher>(
    text: &str,
    rule: &'r R,
    matches: &mut Vec<KeywordMatch<'r, R>>,
    limit: usize,
) {
    let (haystack, offsets): (Vec<char>, Vec<usize>) = if rule.case_sensitive() {
        let mut offsets: Vec<usize> = text.char_indices().map(|(offset, _)| offset).collect();
        offsets.push(text.len());
        (text.chars().collect(), offsets)
    } else {
        fold_case(text)
    };
    let needle: Vec<char> = if rule.case_sensitive() {
        rule.keyword().chars().collect()
    } else {
        rule.keyword()
            .chars()
            .map(|c| c.to_lowercase().next().unwrap_or(c))
            .collect()
    };
    if needle.is_empty() || needle.len() > haystack.len() {
        return;
    }
    let mut from = 0;
    while from + needle.len() <= haystack.len() && matches.len() < limit {
        let Some(found) = haystack[from..]
            .windows(needle.len())
            .position(|window| window == needle.as_slice())
        else {
            break;
        };
        let start_char = from + found;
        let (start, end) = (offsets[start_char], offsets[start_char + needle.len()]);
        if !rule.whole_word() || is_whole_word(text, start, end) {
            matches.push(KeywordMatch { start, end, rule });
        }
        from = start_char + needle.len().max(1);
    }
}

/// Find keyword and pattern matches in rule order, including overlapping rules.
pub fn find_keyword_matches<'r, R: KeywordMatcher>(
    text: &str,
    rules: &'r [R],
    limit: usize,
) -> Vec<KeywordMatch<'r, R>> {
    let mut matches = Vec::new();
    for rule in rules {
        if rule.keyword().is_empty() || matches.len() >= limit {
            continue;
        }
        if rule.regex() {
            find_pattern_matches(text, rule, &mut matches, limit);
        } else {
            find_literal(text, rule, &mut matches, limit);
        }
    }
    matches
}

/// Match every rule against every line, one rule at a time; `on_regex_rule` is called before each
/// regex rule starts. Returns flat `[line, rule, start, end]` quadruples. Each rule keeps at most
/// `limit` matches, which is all [`group_keyword_matches`] can use when it applies the same limit
/// in line order.
pub fn match_keyword_lines<R: KeywordMatcher>(
    rules: &[R],
    lines: &[&str],
    mut on_regex_rule: impl FnMut(usize),
    limit: usize,
) -> Vec<usize> {
    let mut flat = Vec::new();
    for (rule_index, rule) in rules.iter().enumerate() {
        if rule.regex() {
            on_regex_rule(rule_index);
        }
        let mut remaining = limit;
        for (line_index, line) in lines.iter().enumerate() {
            if remaining == 0 {
                break;
            }
            for found in find_keyword_matches(line, std::slice::from_ref(rule), remaining) {
                flat.extend([line_index, rule_index, found.start, found.end]);
                remaining -= 1;
            }
        }
    }
    flat
}

/// Turn [`match_keyword_lines`] output back into per-line matches in rule order, as
/// [`find_keyword_matches`] orders them, keeping the first `limit` in line order.
pub fn group_keyword_matches<'r, R>(
    flat: &[usize],
    rules: &'r [R],
    line_count: usize,
    limit: usize,
) -> Vec<Vec<KeywordMatch<'r, R>>> {
    let mut lines: Vec<Vec<KeywordMatch<'r, R>>> = (0..line_count).map(|_| Vec::new()).collect();
    for quad in flat.as_chunks::<4>().0 {
        if let (Some(rule), Some(line)) = (rules.get(quad[1]), lines.get_mut(quad[0])) {
            line.push(KeywordMatch {
                start: quad[2],
                end: quad[3],
                rule,
            });
        }
    }
    let mut remaining = limit;
    for line in &mut lines {
        line.truncate(remaining);
        remaining -= line.len();
    }
    lines
}

/// A colored range of cells on one buffer line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordDecoration {
    pub line: usize,
    /// First column.
    pub col: usize,
    /// Columns covered.
    pub width: usize,
    pub foreground: String,
    pub background: Option<String>,
}

struct CellSegment {
    text_start: usize,
    text_end: usize,
    cell_start: usize,
    cell_end: usize,
}

fn line_text_and_cells(cells: &[CellText], max_columns: usize) -> (String, Vec<CellSegment>) {
    let mut text = String::new();
    let mut segments = Vec::new();
    for (column, cell) in cells.iter().enumerate().take(max_columns) {
        if cell.width == 0 {
            continue;
        }
        let text_start = text.len();
        text.push_str(if cell.text.is_empty() {
            " "
        } else {
            &cell.text
        });
        segments.push(CellSegment {
            text_start,
            text_end: text.len(),
            cell_start: column,
            cell_end: column + usize::from(cell.width.max(1)),
        });
    }
    let trimmed = text.trim_end().len();
    text.truncate(trimmed);
    (text, segments)
}

fn cells_for_match(segments: &[CellSegment], start: usize, end: usize) -> Option<(usize, usize)> {
    let first = segments.iter().find(|segment| start < segment.text_end)?;
    let last = segments
        .iter()
        .take_while(|segment| segment.text_start < end)
        .last()?;
    Some((first.cell_start, last.cell_end - first.cell_start))
}

/// Decorations for the visible rows `first_line..first_line + rows` of the normal buffer.
/// Full-screen applications (the alternate buffer) are not highlighted, as in Muxus on xterm.
pub fn keyword_decorations<B: TextBuffer + ?Sized>(
    buffer: &B,
    first_line: usize,
    rows: usize,
    rules: &[KeywordHighlightRule],
) -> Vec<KeywordDecoration> {
    let active: Vec<&KeywordHighlightRule> = rules
        .iter()
        .filter(|rule| {
            !rule.keyword.is_empty() && !(rule.is_regex() && is_slow_keyword_pattern(*rule))
        })
        .collect();
    if active.is_empty() {
        return Vec::new();
    }
    let cols = buffer.cols();
    let end = buffer.length().min(first_line + rows);
    let mut snapshots = Vec::new();
    for line in first_line..end {
        if let Some(cells) = buffer.line_cells(line) {
            let (text, segments) = line_text_and_cells(&cells, cols);
            snapshots.push((line, text, segments));
        }
    }
    let texts: Vec<&str> = snapshots.iter().map(|(_, text, _)| text.as_str()).collect();
    let flat = match_keyword_lines(&active, &texts, |_| {}, MAX_KEYWORD_DECORATIONS);
    let grouped = group_keyword_matches(&flat, &active, snapshots.len(), MAX_KEYWORD_DECORATIONS);
    let mut decorations = Vec::new();
    for ((line, _, segments), matches) in snapshots.iter().zip(grouped) {
        for found in matches {
            if let Some((col, width)) = cells_for_match(segments, found.start, found.end) {
                decorations.push(KeywordDecoration {
                    line: *line,
                    col,
                    width,
                    foreground: found.rule.foreground.clone(),
                    background: found.rule.background.clone(),
                });
            }
        }
    }
    decorations
}

impl<T: KeywordMatcher + ?Sized> KeywordMatcher for &T {
    fn keyword(&self) -> &str {
        (**self).keyword()
    }
    fn case_sensitive(&self) -> bool {
        (**self).case_sensitive()
    }
    fn whole_word(&self) -> bool {
        (**self).whole_word()
    }
    fn regex(&self) -> bool {
        (**self).regex()
    }
}

/// The decoration covering a cell, if any: the last rule wins, so host rules override globals.
pub fn decoration_at(
    decorations: &[KeywordDecoration],
    point: BufferPoint,
) -> Option<&KeywordDecoration> {
    decorations.iter().rev().find(|decoration| {
        decoration.line == point.line
            && point.col >= decoration.col
            && point.col < decoration.col + decoration.width
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::StringBuffer;

    fn rule(id: &str, keyword: &str) -> KeywordHighlightRule {
        KeywordHighlightRule {
            id: id.into(),
            name: None,
            keyword: keyword.into(),
            foreground: "#ffffff".into(),
            background: None,
            case_sensitive: false,
            whole_word: false,
            regex: None,
        }
    }

    fn regex(id: &str, keyword: &str) -> KeywordHighlightRule {
        KeywordHighlightRule {
            regex: Some(true),
            ..rule(id, keyword)
        }
    }

    fn spans(text: &str, rules: &[KeywordHighlightRule]) -> Vec<(usize, usize)> {
        find_keyword_matches(text, rules, MAX_KEYWORD_DECORATIONS)
            .iter()
            .map(|m| (m.start, m.end))
            .collect()
    }

    #[test]
    fn finds_every_literal_occurrence_case_insensitively() {
        assert_eq!(
            spans("error: ERROR and error", &[rule("error", "ERROR")]),
            [(0, 5), (7, 12), (17, 22)]
        );
    }

    #[test]
    fn supports_case_sensitive_and_whole_word_rules() {
        let warn = KeywordHighlightRule {
            case_sensitive: true,
            whole_word: true,
            ..rule("warn", "WARN")
        };
        let starts: Vec<usize> = spans("WARN warning preWARN WARN_ WARN", &[warn])
            .iter()
            .map(|s| s.0)
            .collect();
        assert_eq!(starts, [0, 27]);
    }

    #[test]
    fn matches_regex_rules_honoring_case_and_whole_word_options() {
        let text = "Port 1/1/1 Up, port 1/1/12 up, 21/1/1x";
        let port = KeywordHighlightRule {
            whole_word: true,
            ..regex("port", r"\d+/\d+/\d+")
        };
        let found: Vec<&str> = spans(text, &[port])
            .iter()
            .map(|(s, e)| &text[*s..*e])
            .collect();
        assert_eq!(found, ["1/1/1", "1/1/12"]);
        let up = regex("up", r"\bup\b");
        let sensitive = KeywordHighlightRule {
            case_sensitive: true,
            ..up.clone()
        };
        assert_eq!(
            spans(text, &[sensitive])
                .iter()
                .map(|s| s.0)
                .collect::<Vec<_>>(),
            [27]
        );
        assert_eq!(spans(text, &[up]).len(), 2);
    }

    #[test]
    fn treats_a_regex_keyword_literally_unless_the_rule_opts_in() {
        assert_eq!(
            spans("a.c abc", &[rule("dot", "a.c")])
                .iter()
                .map(|s| s.0)
                .collect::<Vec<_>>(),
            [0]
        );
        assert_eq!(
            spans("a.c abc", &[regex("dot", "a.c")])
                .iter()
                .map(|s| s.0)
                .collect::<Vec<_>>(),
            [0, 4]
        );
    }

    #[test]
    fn skips_empty_regex_matches_without_looping_and_ignores_invalid_patterns() {
        assert!(spans("abc", &[regex("empty", "x*")]).is_empty());
        assert!(spans("abc", &[regex("anchor", "^")]).is_empty());
        assert!(spans("(abc", &[regex("broken", "(abc")]).is_empty());
    }

    #[test]
    fn stops_at_the_decoration_limit() {
        assert_eq!(
            find_keyword_matches("a a a a", &[regex("a", "a")], 2).len(),
            2
        );
    }

    #[test]
    fn keeps_rule_order_so_later_host_decorations_can_override_globals() {
        let rules = [rule("global", "down"), rule("host", "down")];
        let ids: Vec<&str> = find_keyword_matches("down", &rules, MAX_KEYWORD_DECORATIONS)
            .iter()
            .map(|m| m.rule.id.as_str())
            .collect();
        assert_eq!(ids, ["global", "host"]);
    }

    #[test]
    fn resolves_global_profile_and_host_rules() {
        let global = vec![rule("global", "ERROR")];
        let profile = vec![rule("profile", "WARNING")];
        let host = vec![rule("host", "FAILED")];
        let config = |inherit: bool, profile_id: Option<&str>| HostKeywordHighlightConfig {
            inherit_global: inherit,
            profile_id: profile_id.map(String::from),
            rules: host.clone(),
        };
        assert_eq!(resolve_keyword_highlights(&global, None, &[]), global);
        assert_eq!(
            resolve_keyword_highlights(&global, Some(&config(true, None)), &[]),
            [global.clone(), host.clone()].concat()
        );
        assert_eq!(
            resolve_keyword_highlights(&global, Some(&config(false, None)), &[]),
            host
        );
        assert_eq!(
            resolve_keyword_highlights(&global, Some(&config(true, Some("nokia-sros"))), &profile),
            [global.clone(), profile.clone(), host.clone()].concat()
        );
        assert_eq!(
            resolve_keyword_highlights(&global, Some(&config(false, Some("nokia-sros"))), &profile),
            [profile, host].concat()
        );
    }

    #[test]
    fn matches_rule_by_rule_and_announces_each_regex_rule_before_it_runs() {
        let rules = [rule("down", "down"), regex("port", r"\d+/\d+")];
        let mut started = Vec::new();
        let flat = match_keyword_lines(
            &rules,
            &["1/1 down", "down 2/2"],
            |index| started.push(index),
            MAX_KEYWORD_DECORATIONS,
        );
        assert_eq!(started, [1]);
        assert_eq!(flat, [0, 0, 4, 8, 1, 0, 0, 4, 0, 1, 0, 3, 1, 1, 5, 8]);
    }

    #[test]
    fn regroups_by_line_in_rule_order_keeping_the_first_matches_in_line_order() {
        let rules = [rule("down", "down"), regex("port", r"\d+/\d+")];
        let flat = match_keyword_lines(
            &rules,
            &["1/1 down", "down 2/2"],
            |_| {},
            MAX_KEYWORD_DECORATIONS,
        );
        let lines = group_keyword_matches(&flat, &rules, 2, MAX_KEYWORD_DECORATIONS);
        let ids: Vec<Vec<&str>> = lines
            .iter()
            .map(|m| m.iter().map(|m| m.rule.id.as_str()).collect())
            .collect();
        assert_eq!(ids, [["down", "port"], ["down", "port"]]);
        assert_eq!(
            lines[0],
            find_keyword_matches("1/1 down", &rules, MAX_KEYWORD_DECORATIONS)
        );
        let capped: Vec<usize> = group_keyword_matches(&flat, &rules, 2, 3)
            .iter()
            .map(Vec::len)
            .collect();
        assert_eq!(capped, [2, 1]);
    }

    #[test]
    fn caps_each_rule_so_a_busy_pattern_cannot_flood_the_result() {
        let flat = match_keyword_lines(&[rule("a", "a")], &["a a a", "a a"], |_| {}, 4);
        assert_eq!(flat.len() / 4, 4);
    }

    #[test]
    fn pauses_a_slow_pattern_reports_it_and_notifies_subscribers_once() {
        let slow = regex("slow", "(a+)+$");
        let calls = Arc::new(Mutex::new(0));
        let counter = calls.clone();
        let subscription = subscribe_slow_keyword_patterns(move |_| *counter.lock().unwrap() += 1);
        let before = slow_keyword_pattern_count();
        mark_slow_keyword_pattern(&slow.keyword);
        mark_slow_keyword_pattern(&slow.keyword);
        assert_eq!(*calls.lock().unwrap(), 1);
        assert_eq!(slow_keyword_pattern_count(), before + 1);
        assert!(is_slow_keyword_pattern(&slow));
        // The same text as a literal keyword cannot backtrack, so it is not paused.
        assert!(!is_slow_keyword_pattern(&KeywordHighlightRule {
            regex: None,
            ..slow.clone()
        }));
        assert!(
            keyword_pattern_error(&slow.keyword, true)
                .unwrap()
                .contains("took too long")
        );
        assert!(
            keyword_highlight_rules_problem(&[slow])
                .unwrap()
                .starts_with("Highlighting: This pattern took too long")
        );
        drop(subscription);
    }

    #[test]
    fn pauses_a_catastrophic_pattern_instead_of_hanging() {
        let evil = regex("evil", r"(a|aa)+(?=c)");
        let line = "a".repeat(60);
        assert!(
            find_keyword_matches(&line, std::slice::from_ref(&evil), MAX_KEYWORD_DECORATIONS)
                .is_empty()
        );
        assert!(is_slow_keyword_pattern(&evil));
    }

    #[test]
    fn reports_regex_syntax_errors_only_for_regex_rules() {
        assert!(
            keyword_pattern_error("(up", true)
                .unwrap()
                .contains("Invalid regular expression")
        );
        assert_eq!(keyword_pattern_error("(up", false), None);
        assert_eq!(keyword_pattern_error("(up|down)", true), None);
    }

    #[test]
    fn names_the_first_rule_that_cannot_be_saved() {
        assert_eq!(
            keyword_highlight_rules_problem(&[rule("ok", "up"), rule("empty", " ")]).as_deref(),
            Some("Every highlighting rule needs a keyword.")
        );
        assert!(
            keyword_highlight_rules_problem(&[regex("broken", "[a-")])
                .unwrap()
                .starts_with("Highlighting: Invalid regular expression")
        );
        assert_eq!(
            keyword_highlight_rules_problem(&[regex("ok", "[a-z]+")]),
            None
        );
    }

    #[test]
    fn decorates_the_cells_a_match_covers() {
        let buffer = StringBuffer::new(20, &[("中 ERROR here", false)]);
        let red = KeywordHighlightRule {
            background: Some("#7f1d1d".into()),
            ..rule("e", "error")
        };
        let decorations = keyword_decorations(&buffer, 0, 10, &[red]);
        assert_eq!(decorations.len(), 1);
        assert_eq!((decorations[0].col, decorations[0].width), (3, 5));
        assert!(decoration_at(&decorations, BufferPoint::new(0, 7)).is_some());
        assert!(decoration_at(&decorations, BufferPoint::new(0, 8)).is_none());
    }

    #[test]
    fn translates_javascript_regex_classes() {
        assert_eq!(translate_js_regex(r"[\d.]x\d"), "[0-9.]x[0-9]");
        assert_eq!(
            translate_js_regex(r"(?<name>a)(?<=b)(?<!c)"),
            "(?P<name>a)(?<=b)(?<!c)"
        );
        assert_eq!(translate_js_regex(r"a\/b"), "a/b");
    }
}
