//! String ordering that stands in for JavaScript's `String.prototype.localeCompare`.
//!
//! Shared by every crate that sorts the way the TypeScript did (originally written for the app's
//! sidebar). The TypeScript sorts host names, folder labels and paths with
//! `localeCompare`, which in Node and Chromium is ICU collation for the default
//! locale (the CLDR root order for English). This is an approximation of that
//! order which agrees with ICU on everything the sidebar sorts in practice:
//!
//! - Comparison is first case-insensitive (ICU's primary strength), so `alpha`,
//!   `Bravo` and `charlie` sort as words, not as ASCII.
//! - Characters are classed as ICU's default ("non-ignorable") ordering does:
//!   whitespace, then punctuation and symbols (in DUCET order for ASCII), then
//!   digits, then letters. `web-01` therefore sorts before `web01` and `web 1`
//!   before both.
//! - Digits compare one by one, as ICU does without numeric collation:
//!   `host10` sorts before `host9`.
//! - On a tie, lowercase sorts before uppercase at the first difference
//!   (ICU's tertiary strength), so `prod` comes before `Prod`.
//!
//! Accents are folded only for the Latin-1 letters (é sorts with e); other
//! scripts sort after Latin by code point. ICU's secondary (accent) level and
//! contractions are not modelled.

use std::cmp::Ordering;

/// Compare two strings the way `a.localeCompare(b)` orders them (see the module docs).
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    let primary = a
        .chars()
        .map(primary_weight)
        .cmp(b.chars().map(primary_weight));
    if primary != Ordering::Equal {
        return primary;
    }
    // Tertiary: at the first character that differs only in case, lowercase wins.
    for (left, right) in a.chars().zip(b.chars()) {
        if left == right {
            continue;
        }
        let (left_upper, right_upper) = (left.is_uppercase(), right.is_uppercase());
        if left_upper != right_upper {
            return if left_upper {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
    }
    // Same letters and case: only accents (or nothing) differ.
    a.cmp(b)
}

/// ASCII punctuation and symbols in the order the CLDR root collation puts them.
const PUNCTUATION_ORDER: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

/// `(class, weight)`: whitespace < punctuation/symbols < digits < letters.
fn primary_weight(c: char) -> (u8, u32) {
    if c.is_whitespace() {
        return (0, c as u32);
    }
    if let Some(index) = PUNCTUATION_ORDER.find(c) {
        return (1, index as u32);
    }
    if c.is_ascii_digit() {
        return (3, c as u32 - '0' as u32);
    }
    if c.is_alphabetic() {
        let base = fold_latin1(c);
        let lower = base.to_lowercase().next().unwrap_or(base);
        // ASCII letters first, every other script after them by code point.
        return if lower.is_ascii_lowercase() {
            (4, lower as u32 - 'a' as u32)
        } else {
            (5, lower as u32)
        };
    }
    if let Some(digit) = c.to_digit(10) {
        return (3, digit);
    }
    // Any other symbol sorts with punctuation, after the ASCII ones.
    (2, c as u32)
}

/// Strip the accent from Latin-1 letters so `é` sorts with `e`, as ICU does at primary strength.
fn fold_latin1(c: char) -> char {
    match c {
        'À'..='Å' | 'à'..='å' => 'a',
        'Ç' | 'ç' => 'c',
        'È'..='Ë' | 'è'..='ë' => 'e',
        'Ì'..='Ï' | 'ì'..='ï' => 'i',
        'Ñ' | 'ñ' => 'n',
        'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' => 'o',
        'Ù'..='Ü' | 'ù'..='ü' => 'u',
        'Ý' | 'ý' | 'ÿ' => 'y',
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(items: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = items.iter().map(|s| s.to_string()).collect();
        out.sort_by(|a, b| locale_compare(a, b));
        out
    }

    #[test]
    fn ignores_case_at_first() {
        assert_eq!(
            sorted(&["charlie", "Bravo", "alpha"]),
            ["alpha", "Bravo", "charlie"]
        );
        assert_eq!(sorted(&["Zed", "Alpha", "mid"]), ["Alpha", "mid", "Zed"]);
    }

    #[test]
    fn puts_lowercase_first_on_a_case_tie() {
        assert_eq!(locale_compare("prod", "Prod"), Ordering::Less);
        assert_eq!(locale_compare("Prod", "prod"), Ordering::Greater);
        assert_eq!(locale_compare("Prod/EU", "prod/US"), Ordering::Less);
    }

    #[test]
    fn orders_whitespace_punctuation_digits_letters() {
        assert_eq!(
            sorted(&["web01", "web-01", "web 1"]),
            ["web 1", "web-01", "web01"]
        );
        assert_eq!(sorted(&["a", "1", "-"]), ["-", "1", "a"]);
    }

    #[test]
    fn compares_digits_one_at_a_time() {
        assert_eq!(sorted(&["host9", "host10"]), ["host10", "host9"]);
    }

    #[test]
    fn sorts_a_prefix_first() {
        assert_eq!(
            locale_compare("production/eu", "Production/EU/Edge"),
            Ordering::Less
        );
        assert_eq!(locale_compare("db", "db-01"), Ordering::Less);
    }

    #[test]
    fn folds_latin1_accents() {
        assert_eq!(sorted(&["f", "é", "d"]), ["d", "é", "f"]);
    }

    #[test]
    fn is_equal_only_for_equal_strings() {
        assert_eq!(locale_compare("same", "same"), Ordering::Equal);
        assert_ne!(locale_compare("é", "e"), Ordering::Equal);
    }
}
