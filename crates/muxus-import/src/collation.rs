//! `String.prototype.localeCompare` as the Electron renderer runs it: ICU's
//! root collation at tertiary strength.
//!
//! This is a compact approximation that is exact for the ASCII strings host
//! names and folders almost always are: whitespace, then punctuation and
//! symbols in CLDR root order, then digits, then letters compared without
//! regard to case first, accents second and case last (lowercase first).
//! Other characters keep a stable order by code point inside their class.

use std::cmp::Ordering;

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Printable ASCII punctuation and symbols in CLDR root collation order.
const ASCII_PUNCTUATION: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

const WHITESPACE_BASE: u32 = 0x10;
const PUNCTUATION_BASE: u32 = 0x100;
const SYMBOL_BASE: u32 = 0x1000;
const DIGIT_BASE: u32 = 0x0020_0000;
const LATIN_BASE: u32 = 0x0021_0000;
const LETTER_BASE: u32 = 0x0030_0000;

/// Compare like `a.localeCompare(b)` in the renderer.
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    let left = SortKey::new(a);
    let right = SortKey::new(b);
    left.primary
        .cmp(&right.primary)
        .then_with(|| left.secondary.cmp(&right.secondary))
        .then_with(|| left.tertiary.cmp(&right.tertiary))
}

struct SortKey {
    primary: Vec<u32>,
    secondary: Vec<u32>,
    tertiary: Vec<u32>,
}

impl SortKey {
    fn new(value: &str) -> Self {
        let mut key = SortKey {
            primary: Vec::with_capacity(value.len()),
            secondary: Vec::with_capacity(value.len()),
            tertiary: Vec::with_capacity(value.len()),
        };
        for c in value.nfd() {
            if is_combining_mark(c) {
                // Accents only matter once the base letters tie.
                key.secondary.push(0x100 + u32::from(c));
                continue;
            }
            if c == 'ß' {
                for _ in 0..2 {
                    key.push(latin_weight('s'), 2);
                }
                continue;
            }
            let Some(primary) = primary_weight(c) else {
                continue;
            };
            let case = if c.is_uppercase() { 2 } else { 1 };
            key.push(primary, case);
        }
        key
    }

    fn push(&mut self, primary: u32, case: u32) {
        self.primary.push(primary);
        self.secondary.push(0x05);
        self.tertiary.push(case);
    }
}

fn latin_weight(c: char) -> u32 {
    LATIN_BASE + u32::from(c.to_ascii_lowercase()) - u32::from('a')
}

/// `None` for characters the collation ignores (controls and format characters).
fn primary_weight(c: char) -> Option<u32> {
    match c {
        '\t' => Some(WHITESPACE_BASE),
        '\n' => Some(WHITESPACE_BASE + 1),
        '\u{0B}' => Some(WHITESPACE_BASE + 2),
        '\u{0C}' => Some(WHITESPACE_BASE + 3),
        '\r' => Some(WHITESPACE_BASE + 4),
        '\u{85}' => Some(WHITESPACE_BASE + 5),
        '\u{2028}' => Some(WHITESPACE_BASE + 6),
        '\u{2029}' => Some(WHITESPACE_BASE + 7),
        c if c.is_whitespace() => Some(WHITESPACE_BASE + 8),
        c if c.is_control() || is_format(c) => None,
        c if c.is_ascii_alphabetic() => Some(latin_weight(c)),
        c if c.is_ascii_digit() => Some(DIGIT_BASE + u32::from(c) - u32::from('0')),
        c if c.is_ascii() => ASCII_PUNCTUATION
            .find(c)
            .map(|index| PUNCTUATION_BASE + index as u32),
        c if c.is_alphabetic() => {
            let lower = c.to_lowercase().next().unwrap_or(c);
            Some(LETTER_BASE + u32::from(lower))
        }
        c if c.is_numeric() => Some(DIGIT_BASE + 0x10 + u32::from(c)),
        c => Some(SYMBOL_BASE + u32::from(c)),
    }
}

/// Unicode format characters (Cf) that collation ignores.
fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{AD}'
            | '\u{600}'..='\u{605}'
            | '\u{61C}'
            | '\u{6DD}'
            | '\u{70F}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(values: &[&str]) -> Vec<String> {
        let mut values: Vec<String> = values.iter().map(|v| (*v).to_owned()).collect();
        values.sort_by(|a, b| locale_compare(a, b));
        values
    }

    #[test]
    fn letters_ignore_case_until_everything_else_ties() {
        assert_eq!(sorted(&["b", "A", "a", "B"]), ["a", "A", "b", "B"]);
        assert_eq!(sorted(&["ab", "Aa"]), ["Aa", "ab"]);
        assert_eq!(sorted(&["e", "é", "f", "E"]), ["e", "E", "é", "f"]);
        assert_eq!(locale_compare("Lab", "lab"), Ordering::Greater);
        assert_eq!(locale_compare("same", "same"), Ordering::Equal);
    }

    #[test]
    fn punctuation_sorts_before_digits_and_letters() {
        assert_eq!(
            sorted(&["a", "1", "_", " ", "-"]),
            [" ", "_", "-", "1", "a"]
        );
        assert_eq!(sorted(&["10", "9", "1"]), ["1", "10", "9"]);
        assert_eq!(sorted(&["a-b", "a b", "ab"]), ["a b", "a-b", "ab"]);
    }
}
