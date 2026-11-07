//! String ordering for JavaScript's `String.prototype.localeCompare`.
//!
//! Shared by every crate that sorts the way the TypeScript did. The TypeScript sorts host
//! names, folder labels and paths with `localeCompare`, which in Node and Chromium is ICU
//! collation for the default locale: for English, the CLDR root order (English has no
//! tailoring) at tertiary strength, punctuation not ignored, no numeric ordering. This is
//! that collation, from ICU4X (`icu_collator`), so it agrees with the renderer beyond ASCII
//! too:
//!
//! - Letters compare without regard to case and accents first, so `alpha`, `Bravo` and
//!   `charlie` sort as words, and `é` sorts with `e`.
//! - Whitespace, then punctuation and symbols, then digits, then letters: `web 1` before
//!   `web-01` before `web01`.
//! - Digits compare one by one: `host10` sorts before `host9`.
//! - Accents break a tie next (`e` < `é`), case last, lowercase first (`prod` < `Prod`).
//! - `ß` sorts as `ss`, just after it; other scripts follow Latin in the root order (Greek,
//!   Cyrillic, …), each in its alphabet's order.
//!
//! Strings the collation finds equal (only ignorable characters differ) compare equal, as
//! `localeCompare` returns 0 for them.
//!
//! Only the root collation's data is built in. ICU4X's compiled data carries every locale's
//! tailoring (about 1 MB in the binary); Muxus only ever sorts in the root order, so the
//! tailorings are left out (about 0.3 MB with the collator itself).

use std::cmp::Ordering;

use icu_collator::options::CollatorOptions;
use icu_collator::provider::{
    CollationDiacriticsV1, CollationJamoV1, CollationMetadataV1, CollationReorderingV1,
    CollationRootV1, CollationSpecialPrimariesV1, CollationTailoringV1,
};
use icu_collator::{Collator, CollatorPreferences};
use icu_normalizer::provider::{NormalizerNfdDataV1, NormalizerNfdTablesV1};
use icu_provider::prelude::*;

/// The root collation's data from the compiled data, and no tailorings. Only what this
/// provider hands out stays in the binary: the linker drops the tailorings' tables, since
/// nothing refers to them.
struct RootCollation;

macro_rules! compiled {
    ($($marker:ty => $baked:path),* $(,)?) => {$(
        impl DataProvider<$marker> for RootCollation {
            fn load(&self, request: DataRequest) -> Result<DataResponse<$marker>, DataError> {
                DataProvider::<$marker>::load(&$baked, request)
            }
        }
    )*};
}

compiled!(
    CollationRootV1 => icu_collator::provider::Baked,
    CollationSpecialPrimariesV1 => icu_collator::provider::Baked,
    CollationDiacriticsV1 => icu_collator::provider::Baked,
    CollationJamoV1 => icu_collator::provider::Baked,
    CollationMetadataV1 => icu_collator::provider::Baked,
    CollationReorderingV1 => icu_collator::provider::Baked,
    NormalizerNfdDataV1 => icu_normalizer::provider::Baked,
    NormalizerNfdTablesV1 => icu_normalizer::provider::Baked,
);

/// The root locale has no tailoring; the collator only asks for one when a locale's
/// metadata says it has one.
impl DataProvider<CollationTailoringV1> for RootCollation {
    fn load(&self, request: DataRequest) -> Result<DataResponse<CollationTailoringV1>, DataError> {
        Err(DataErrorKind::IdentifierNotFound.with_req(CollationTailoringV1::INFO, request))
    }
}

/// The root collation with `Intl.Collator`'s defaults (tertiary strength, punctuation
/// counts, no numeric ordering), or `None` should the built-in data not load.
fn root_collator() -> Option<Collator> {
    Collator::try_new_unstable(
        &RootCollation,
        CollatorPreferences::default(),
        CollatorOptions::default(),
    )
    .ok()
}

thread_local! {
    // A collator over static data is cheap to make, but not `Sync`: one per thread.
    static COLLATOR: Option<Collator> = root_collator();
}

/// Compare two strings the way `a.localeCompare(b)` orders them (see the module docs).
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    COLLATOR.with(|collator| match collator {
        Some(collator) => collator.as_borrowed().compare(a, b),
        None => a.cmp(b),
    })
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
    fn the_root_collation_loads() {
        assert!(root_collator().is_some());
    }

    #[test]
    fn ignores_case_at_first() {
        assert_eq!(
            sorted(&["charlie", "Bravo", "alpha"]),
            ["alpha", "Bravo", "charlie"]
        );
        assert_eq!(sorted(&["Zed", "Alpha", "mid"]), ["Alpha", "mid", "Zed"]);
        assert_eq!(sorted(&["b", "A", "a", "B"]), ["a", "A", "b", "B"]);
        assert_eq!(sorted(&["ab", "Aa"]), ["Aa", "ab"]);
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
        assert_eq!(
            sorted(&["a", "1", "_", " ", "-"]),
            [" ", "_", "-", "1", "a"]
        );
        assert_eq!(sorted(&["a-b", "a b", "ab"]), ["a b", "a-b", "ab"]);
    }

    #[test]
    fn compares_digits_one_at_a_time() {
        assert_eq!(sorted(&["host9", "host10"]), ["host10", "host9"]);
        assert_eq!(sorted(&["10", "9", "1"]), ["1", "10", "9"]);
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
    fn accents_come_after_the_letters_and_before_case() {
        assert_eq!(sorted(&["f", "é", "d"]), ["d", "é", "f"]);
        assert_eq!(sorted(&["e", "é", "f", "E"]), ["e", "E", "é", "f"]);
        // Beyond Latin-1: the accent is secondary wherever it sits.
        assert_eq!(
            sorted(&["Łódź", "Lodz", "lodz", "Lodzia"]),
            ["lodz", "Lodz", "Łódź", "Lodzia"]
        );
        assert_eq!(
            sorted(&["Zürich", "Zug", "Zurich"]),
            ["Zug", "Zurich", "Zürich"]
        );
        // The first accent difference decides (no French backwards accents in the root).
        assert_eq!(sorted(&["côte", "coté", "cote"]), ["cote", "coté", "côte"]);
        // Precomposed and decomposed forms are the same string to the collation.
        assert_eq!(locale_compare("e\u{301}", "\u{e9}"), Ordering::Equal);
    }

    #[test]
    fn sharp_s_sorts_as_ss() {
        assert_eq!(sorted(&["st", "ß", "ss", "sr"]), ["sr", "ss", "ß", "st"]);
        assert_eq!(
            sorted(&["Straße", "Strasse", "Strase"]),
            ["Strase", "Strasse", "Straße"]
        );
        assert_eq!(sorted(&["æ", "ae", "af"]), ["ae", "æ", "af"]);
    }

    #[test]
    fn other_scripts_follow_latin_in_their_own_order() {
        assert_eq!(
            sorted(&["яблоко", "Ωmega", "zulu", "Альфа", "beta", "αλφα"]),
            ["beta", "zulu", "αλφα", "Ωmega", "Альфа", "яблоко"]
        );
        // Cyrillic ё sorts with е, as an accented е.
        assert_eq!(sorted(&["ж", "ё", "е", "д"]), ["д", "е", "ё", "ж"]);
        // Japanese kana in gojūon order, after every alphabet.
        assert_eq!(sorted(&["か", "あ", "z", "さ"]), ["z", "あ", "か", "さ"]);
        // Digits of other scripts are digits.
        assert_eq!(sorted(&["a", "٣", "2"]), ["2", "٣", "a"]);
    }

    #[test]
    fn is_equal_for_equal_strings_and_ignorables_only() {
        assert_eq!(locale_compare("same", "same"), Ordering::Equal);
        assert_ne!(locale_compare("é", "e"), Ordering::Equal);
        // A soft hyphen or zero-width space is ignorable, as in `localeCompare`.
        assert_eq!(locale_compare("ab", "a\u{ad}b"), Ordering::Equal);
        assert_eq!(locale_compare("ab", "a\u{200b}b"), Ordering::Equal);
    }

    #[test]
    fn works_on_every_thread() {
        let sorted = std::thread::spawn(|| sorted(&["b", "A", "a"]))
            .join()
            .unwrap();
        assert_eq!(sorted, ["a", "A", "b"]);
    }
}
