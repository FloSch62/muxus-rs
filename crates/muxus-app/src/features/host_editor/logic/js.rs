//! The few JavaScript semantics the editors' validation depends on.

use muxus_core::js::{is_js_whitespace, js_trim};

/// `Number(text)` for form text: surrounding whitespace ignored, empty is 0,
/// `0x`/`0o`/`0b` prefixes, decimals with exponents, `Infinity`; anything
/// else is `NaN`.
pub fn js_number(text: &str) -> f64 {
    let text = js_trim(text);
    if text.is_empty() {
        return 0.;
    }
    let radix = |digits: &str, radix: u32| {
        if digits.is_empty() {
            return f64::NAN;
        }
        digits
            .chars()
            .try_fold(0f64, |acc, c| {
                c.to_digit(radix)
                    .map(|d| acc * f64::from(radix) + f64::from(d))
            })
            .unwrap_or(f64::NAN)
    };
    let lower = text.to_ascii_lowercase();
    if let Some(digits) = lower.strip_prefix("0x") {
        return radix(digits, 16);
    }
    if let Some(digits) = lower.strip_prefix("0o") {
        return radix(digits, 8);
    }
    if let Some(digits) = lower.strip_prefix("0b") {
        return radix(digits, 2);
    }
    match text {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    // Rust also accepts "inf", "nan" and friends; JavaScript does not.
    let valid = text
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
    if !valid {
        return f64::NAN;
    }
    text.parse::<f64>().unwrap_or(f64::NAN)
}

/// `Number.isInteger(Number(text))`, with the integer when it is one.
pub fn js_integer(text: &str) -> Option<f64> {
    let value = js_number(text);
    (value.is_finite() && value.fract() == 0.).then_some(value)
}

/// `portOk`: a whole number from 1 to 65535.
pub fn port_ok(text: &str) -> bool {
    js_integer(text).is_some_and(|n| n > 0. && n < 65_536.)
}

/// `Number(text)` as a port, for text that `port_ok` accepted.
pub fn to_port(text: &str) -> Option<u16> {
    js_integer(text)
        .filter(|n| *n >= 0. && *n <= f64::from(u16::MAX))
        .map(|n| n as u16)
}

/// `text.trim().split(/\s+/).filter(Boolean)`.
pub fn split_words(text: &str) -> Vec<String> {
    js_trim(text)
        .split(is_js_whitespace)
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `value.trim() || undefined`.
pub fn non_empty(text: &str) -> Option<String> {
    let trimmed = js_trim(text);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `/\s/.test(text)`.
pub fn has_space(text: &str) -> bool {
    text.chars().any(is_js_whitespace)
}

/// `text.replace(/[^\d]/g, '')`: what the port fields keep of a keystroke.
pub fn digits_only(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_follow_javascript() {
        assert_eq!(js_number(""), 0.);
        assert_eq!(js_number(" 22 "), 22.);
        assert_eq!(js_number("0x10"), 16.);
        assert_eq!(js_number("1e3"), 1000.);
        assert!(js_number("2.5").fract() != 0.);
        assert!(js_number("inf").is_nan());
        assert!(js_number("12a").is_nan());
        assert!(port_ok("65535"));
        assert!(!port_ok("65536"));
        assert!(!port_ok(""));
        assert!(!port_ok("0"));
        assert_eq!(split_words("  a  b\tc "), vec!["a", "b", "c"]);
    }
}
