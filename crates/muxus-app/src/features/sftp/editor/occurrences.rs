//! Occurrence highlighting: Monaco's `occurrencesHighlight: 'singleFile'` (every whole-word,
//! case-sensitive occurrence of the word at the cursor, the textual fallback languages without a
//! symbol highlighter get) and `selectionHighlight` (other occurrences of the selected text).

use std::ops::Range;

/// Monaco's `USUAL_WORD_SEPARATORS`.
const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

/// Neither documents nor selections past this size are searched.
pub const MAX_SEARCHED_BYTES: usize = 4 * 1024 * 1024;

fn is_separator(c: char) -> bool {
    c.is_whitespace() || WORD_SEPARATORS.contains(c)
}

/// The word at (or just before) `offset`, as Monaco's default word definition finds it:
/// `-?\d*\.\d\w*` numbers or runs of characters that are not separators.
pub fn word_at(text: &str, offset: usize) -> Option<Range<usize>> {
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let line_end = text[offset..]
        .find('\n')
        .map_or(text.len(), |index| offset + index);
    let line = &text[line_start..line_end];
    let column = offset - line_start;
    word_ranges(line)
        .into_iter()
        .find(|range| range.start <= column && column <= range.end)
        .map(|range| line_start + range.start..line_start + range.end)
}

/// The words of a line under Monaco's `DEFAULT_WORD_REGEXP`.
fn word_ranges(line: &str) -> Vec<Range<usize>> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let end_of = |index: usize| chars.get(index).map_or(line.len(), |(offset, _)| *offset);
    let mut words = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        // `-?\d*\.\d\w*`
        let mut j = i;
        if chars[j].1 == '-' {
            j += 1;
        }
        while j < chars.len() && chars[j].1.is_ascii_digit() {
            j += 1;
        }
        if j + 1 < chars.len() && chars[j].1 == '.' && chars[j + 1].1.is_ascii_digit() {
            j += 2;
            while j < chars.len() && (chars[j].1.is_ascii_alphanumeric() || chars[j].1 == '_') {
                j += 1;
            }
            words.push(chars[i].0..end_of(j));
            i = j;
            continue;
        }
        if is_separator(chars[i].1) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && !is_separator(chars[j].1) {
            j += 1;
        }
        words.push(chars[i].0..end_of(j));
        i = j;
    }
    words
}

/// Every occurrence of `needle` in `text`. `whole_word` keeps those whose edges sit at word
/// boundaries (Monaco's `isValidMatch` with the usual separators); without `match_case` the
/// comparison ignores case.
pub fn find_matches(
    text: &str,
    needle: &str,
    match_case: bool,
    whole_word: bool,
) -> Vec<Range<usize>> {
    if needle.is_empty() || text.len() > MAX_SEARCHED_BYTES {
        return Vec::new();
    }
    let (haystack, pattern);
    let (search_in, search_for) = if match_case {
        (text, needle)
    } else {
        haystack = text.to_lowercase();
        pattern = needle.to_lowercase();
        // Lowercasing may change lengths outside ASCII; then fall back to the exact text.
        if haystack.len() == text.len() && pattern.len() == needle.len() {
            (haystack.as_str(), pattern.as_str())
        } else {
            (text, needle)
        }
    };
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(index) = search_in[from..].find(search_for) {
        let start = from + index;
        let end = start + search_for.len();
        if !whole_word || is_whole_word(text, start, end) {
            found.push(start..end);
        }
        from = start + search_for[..].chars().next().map_or(1, char::len_utf8);
    }
    found
}

fn is_whole_word(text: &str, start: usize, end: usize) -> bool {
    let matched = &text[start..end];
    let left = start == 0
        || text[..start].chars().next_back().is_some_and(is_separator)
        || matched.chars().next().is_some_and(is_separator);
    let right = end == text.len()
        || text[end..].chars().next().is_some_and(is_separator)
        || matched.chars().next_back().is_some_and(is_separator);
    left && right
}

/// What gets highlighted for a cursor or selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Occurrences {
    /// `wordHighlightText`: the word at the cursor, wherever it occurs (itself included).
    pub words: Vec<Range<usize>>,
    /// `selectionHighlight`: other occurrences of the selected text.
    pub selections: Vec<Range<usize>>,
}

/// The highlights for `selection` in `text`: with no selection, every occurrence of the word
/// at the cursor; with text selected on one line, every other place that text occurs.
pub fn occurrences(text: &str, selection: Range<usize>) -> Occurrences {
    if text.len() > MAX_SEARCHED_BYTES {
        return Occurrences::default();
    }
    if selection.is_empty() {
        let Some(word) = word_at(text, selection.start) else {
            return Occurrences::default();
        };
        return Occurrences {
            words: find_matches(text, &text[word], true, true),
            selections: Vec::new(),
        };
    }
    let selected = &text[selection.clone()];
    // Only a selection within one line that is not just blanks.
    if selected.contains('\n') || selected.trim().is_empty() {
        return Occurrences::default();
    }
    Occurrences {
        words: Vec::new(),
        selections: find_matches(text, selected, false, false)
            .into_iter()
            .filter(|range| *range != selection)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_word_at_or_just_before_the_cursor() {
        let text = "services:\n  edge:\n    image: x-1.5.0";
        assert_eq!(word_at(text, 0), Some(0..8));
        assert_eq!(word_at(text, 8), Some(0..8));
        assert_eq!(word_at(text, 12), Some(12..16));
        assert_eq!(word_at(text, 9), None);
        // `-?\d*\.\d\w*` reads `-1.5` as one word.
        let number = text.find(".5").unwrap();
        assert_eq!(&text[word_at(text, number + 1).unwrap()], "-1.5");
    }

    #[test]
    fn highlights_whole_word_occurrences_of_the_word_at_the_cursor() {
        let text = "edge: edge-1 edges\nedge";
        let found = occurrences(text, 1..1);
        let words: Vec<&str> = found.words.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(words, vec!["edge", "edge", "edge"]);
        assert_eq!(found.words[1], 6..10);
        assert!(found.selections.is_empty());
        // Case matters for words.
        assert_eq!(occurrences("Edge edge", 0..0).words, vec![0..4]);
    }

    #[test]
    fn highlights_other_occurrences_of_a_selection_ignoring_case() {
        let text = "Port 80\nport 8080 PORT";
        let found = occurrences(text, 0..4);
        assert_eq!(found.selections, vec![8..12, 18..22]);
        assert!(found.words.is_empty());
        assert_eq!(occurrences(text, 0..9), Occurrences::default());
        assert_eq!(occurrences("a  b  c", 1..3), Occurrences::default());
    }
}
