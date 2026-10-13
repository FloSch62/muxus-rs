//! JavaScript semantics the TypeScript code relies on implicitly.
//!
//! String lengths in the TypeScript (and in every zod limit) count UTF-16 code
//! units, `String.prototype.trim` strips a slightly different whitespace set
//! than Rust's `str::trim`, `JSON.stringify` writes `1` rather than `1.0`, and
//! optional fields distinguish "absent" from `null`. These helpers make the
//! Rust port agree with all of that.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `string.length` in JavaScript: UTF-16 code units.
pub fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// `Array.from(string).length` in JavaScript: Unicode code points.
pub fn code_point_len(value: &str) -> usize {
    value.chars().count()
}

/// Whether `String.prototype.trim` strips this character (ECMAScript
/// WhiteSpace and LineTerminator).
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `String.prototype.trim`.
pub fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

/// `String.prototype.trimStart`.
pub fn js_trim_start(value: &str) -> &str {
    value.trim_start_matches(is_js_whitespace)
}

/// `String.prototype.trimEnd`.
pub fn js_trim_end(value: &str) -> &str {
    value.trim_end_matches(is_js_whitespace)
}

/// Trim a string in place the way zod's `.trim()` does.
pub fn trim_in_place(value: &mut String) {
    let trimmed = js_trim(value);
    if trimmed.len() != value.len() {
        *value = trimmed.to_owned();
    }
}

/// `String.prototype.slice(0, n)` on UTF-16 code units, never splitting a
/// surrogate pair into invalid UTF-8 (a split pair is dropped).
pub fn utf16_prefix(value: &str, units: usize) -> &str {
    let mut count = 0;
    for (index, c) in value.char_indices() {
        let width = c.len_utf16();
        if count + width > units {
            return &value[..index];
        }
        count += width;
    }
    value
}

/// A JavaScript number as `JSON.stringify` writes it: integral values without
/// a fraction (`1`, not `1.0`). Use with `#[serde(with = "js::number")]`.
pub mod number {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Largest integer a JavaScript number holds exactly.
    pub const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        if value.is_finite() && value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER {
            // Exact: the value is integral and within the safe range.
            serializer.serialize_i64(*value as i64)
        } else if value.is_finite() {
            serializer.serialize_f64(*value)
        } else {
            // JSON.stringify writes NaN and Infinity as null.
            serializer.serialize_none()
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        f64::deserialize(deserializer)
    }

    /// The same, for `Option<f64>` fields.
    pub mod option {
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer>(
            value: &Option<f64>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match value {
                Some(value) => super::serialize(value, serializer),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<f64>, D::Error> {
            Option::<f64>::deserialize(deserializer)
        }
    }
}

/// A JSON number written the way JavaScript writes it (`1`, not `1.0`).
pub fn js_number(value: f64) -> serde_json::Value {
    if value.is_finite() && value.fract() == 0.0 && value.abs() <= number::MAX_SAFE_INTEGER {
        serde_json::Value::from(value as i64)
    } else {
        serde_json::Number::from_f64(value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number)
    }
}

/// A field of a partial update typed `T | null` and optional in TypeScript:
/// absent keeps the stored value, `null` clears it, a value replaces it.
///
/// Use with `#[serde(default, skip_serializing_if = "Patch::is_keep")]`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Patch<T> {
    /// The key was absent.
    #[default]
    Keep,
    /// The key was `null`.
    Clear,
    /// The key carried a value.
    Set(T),
}

impl<T> Patch<T> {
    pub fn is_keep(&self) -> bool {
        matches!(self, Patch::Keep)
    }

    /// `None` when absent, `Some(None)` when cleared, `Some(Some(v))` when set.
    pub fn as_update(&self) -> Option<Option<&T>> {
        match self {
            Patch::Keep => None,
            Patch::Clear => Some(None),
            Patch::Set(value) => Some(Some(value)),
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Patch<U> {
        match self {
            Patch::Keep => Patch::Keep,
            Patch::Clear => Patch::Clear,
            Patch::Set(value) => Patch::Set(f(value)),
        }
    }

    pub fn as_ref(&self) -> Patch<&T> {
        match self {
            Patch::Keep => Patch::Keep,
            Patch::Clear => Patch::Clear,
            Patch::Set(value) => Patch::Set(value),
        }
    }

    pub fn as_mut(&mut self) -> Patch<&mut T> {
        match self {
            Patch::Keep => Patch::Keep,
            Patch::Clear => Patch::Clear,
            Patch::Set(value) => Patch::Set(value),
        }
    }
}

impl<T> From<Option<T>> for Patch<T> {
    /// `None` clears, `Some` sets.
    fn from(value: Option<T>) -> Self {
        match value {
            Some(value) => Patch::Set(value),
            None => Patch::Clear,
        }
    }
}

impl<T: Serialize> Serialize for Patch<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Patch::Keep | Patch::Clear => serializer.serialize_none(),
            Patch::Set(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Patch::Set(value),
            None => Patch::Clear,
        })
    }
}

/// `new Date().toISOString()`: UTC with milliseconds and a `Z` suffix.
pub fn iso_timestamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// `new Date().toISOString()` for the current time.
pub fn now_iso() -> String {
    iso_timestamp(chrono::Utc::now())
}

/// `Date.parse` for the ISO-8601 forms the TypeScript writes, plus SQLite's
/// `CURRENT_TIMESTAMP` form (`YYYY-MM-DD HH:MM:SS`), read as UTC because
/// SQLite writes it in UTC.
pub fn parse_timestamp(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{DateTime, NaiveDateTime, Utc};
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc())
}

/// `Number.prototype.toFixed`: ties round away from zero on the exact
/// binary value (0.0625 → "0.063"), where Rust's formatter rounds them to even.
pub fn to_fixed(value: f64, digits: usize) -> String {
    if !value.is_finite() {
        return if value.is_nan() {
            "NaN".into()
        } else if value > 0. {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    // Enough digits to see the exact binary value well past the cut.
    let exact = format!("{:.*}", digits + 25, value.abs());
    let (int_part, frac_part) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut digits_vec: Vec<u8> = int_part
        .bytes()
        .chain(frac_part.bytes().take(digits))
        .map(|b| b - b'0')
        .collect();
    let round_up = frac_part.as_bytes().get(digits).is_some_and(|d| *d >= b'5');
    if round_up {
        let mut index = digits_vec.len();
        loop {
            if index == 0 {
                digits_vec.insert(0, 1);
                break;
            }
            index -= 1;
            if digits_vec[index] == 9 {
                digits_vec[index] = 0;
            } else {
                digits_vec[index] += 1;
                break;
            }
        }
    }
    let int_len = digits_vec.len() - digits;
    let mut text: String = digits_vec[..int_len]
        .iter()
        .map(|d| char::from(b'0' + d))
        .collect();
    if digits > 0 {
        text.push('.');
        text.extend(digits_vec[int_len..].iter().map(|d| char::from(b'0' + d)));
    }
    // `-0` prints unsigned; any other negative value keeps its sign, even
    // when it rounds to zero.
    if value < 0. {
        text.insert(0, '-');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_like_javascript() {
        assert_eq!(js_trim("\u{FEFF} a \u{00A0}"), "a");
        // NEL is White_Space for Rust but not for JavaScript.
        assert_eq!(js_trim("\u{0085}a"), "\u{0085}a");
    }

    #[test]
    fn counts_utf16_units() {
        assert_eq!(utf16_len("a😀"), 3);
        assert_eq!(code_point_len("a😀"), 2);
        assert_eq!(utf16_prefix("a😀b", 2), "a");
        assert_eq!(utf16_prefix("a😀b", 3), "a😀");
    }

    #[test]
    fn numbers_serialize_like_json_stringify() {
        #[derive(Serialize)]
        struct N {
            #[serde(with = "number")]
            a: f64,
            #[serde(with = "number")]
            b: f64,
        }
        assert_eq!(
            serde_json::to_string(&N { a: 1.0, b: 0.15 }).unwrap(),
            r#"{"a":1,"b":0.15}"#
        );
        assert_eq!(js_number(2.0).to_string(), "2");
    }

    #[test]
    fn patch_distinguishes_absent_and_null() {
        #[derive(Deserialize, Serialize, Debug, PartialEq)]
        struct P {
            #[serde(default, skip_serializing_if = "Patch::is_keep")]
            v: Patch<String>,
        }
        let absent: P = serde_json::from_str("{}").unwrap();
        let null: P = serde_json::from_str(r#"{"v":null}"#).unwrap();
        let set: P = serde_json::from_str(r#"{"v":"x"}"#).unwrap();
        assert_eq!(absent.v, Patch::Keep);
        assert_eq!(null.v, Patch::Clear);
        assert_eq!(set.v, Patch::Set("x".into()));
        assert_eq!(serde_json::to_string(&absent).unwrap(), "{}");
        assert_eq!(serde_json::to_string(&null).unwrap(), r#"{"v":null}"#);
    }

    #[test]
    fn formats_iso_timestamps() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05.006Z")
            .unwrap()
            .to_utc();
        assert_eq!(iso_timestamp(at), "2026-01-02T03:04:05.006Z");
        assert_eq!(
            parse_timestamp("2026-01-02 03:04:05")
                .map(iso_timestamp)
                .as_deref(),
            Some("2026-01-02T03:04:05.000Z")
        );
    }
}
