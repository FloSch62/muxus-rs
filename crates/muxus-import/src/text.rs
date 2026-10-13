//! Name, folder, alias and ID helpers shared by every importer (the helper
//! half of `client/src/session-import.ts`), plus the bits of JavaScript
//! string semantics the importers rely on.

use muxus_core::js;

const FNV_OFFSET_BASIS_64: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME_64: u64 = 0x0100_0000_01b3;

/// Normalize an imported folder path: backslashes become `/`, every segment is
/// cleaned and trimmed, empty segments drop out, and the result is capped at
/// 300 UTF-16 units. `None` means "no folder".
pub fn normalize_import_folder(value: &str) -> Option<String> {
    let replaced = value.replace('\\', "/");
    let parts: Vec<String> = replaced
        .split('/')
        .map(|part| js::js_trim(&strip_control_characters(part)).to_owned())
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("/");
    let capped = js::utf16_prefix(&joined, 300);
    let folder = capped.strip_suffix('/').unwrap_or(capped);
    (!folder.is_empty()).then(|| folder.to_owned())
}

/// Drop C0 control characters and DEL; everything else, C1 controls
/// included, is kept.
pub fn strip_control_characters(value: &str) -> String {
    value
        .chars()
        .filter(|&c| c >= '\u{20}' && c != '\u{7f}')
        .collect()
}

/// A display name: cleaned, trimmed and capped at 200 UTF-16 units.
pub fn clean_import_name(value: &str) -> String {
    let stripped = strip_control_characters(value);
    let trimmed = js::js_trim(&stripped);
    js::js_trim(js::utf16_prefix(trimmed, 200)).to_owned()
}

/// An ssh_config alias derived from `name` (or `host`, or `fallback`) that is
/// not in `used`, suffixed `-2`, `-3`, … when it is.
pub fn unique_import_alias(
    name: &str,
    host: &str,
    used: &std::collections::HashSet<String>,
    fallback: &str,
) -> String {
    let cleaned_name = clean_import_alias(name);
    let base = if !cleaned_name.is_empty() {
        cleaned_name
    } else {
        let cleaned_host = clean_import_alias(host);
        if cleaned_host.is_empty() {
            fallback.to_owned()
        } else {
            cleaned_host
        }
    };
    if !used.contains(&base) {
        return base;
    }
    let mut suffix = 2;
    while used.contains(&format!("{base}-{suffix}")) {
        suffix += 1;
    }
    format!("{base}-{suffix}")
}

/// `<prefix>-<base36 FNV-1a 64>` of `value`, hashed per Unicode code point,
/// so the same source entry gets the same ID on every import.
pub fn stable_import_id(prefix: &str, value: &str) -> String {
    let mut hash = FNV_OFFSET_BASIS_64;
    for c in value.chars() {
        hash ^= u64::from(u32::from(c));
        hash = hash.wrapping_mul(FNV_PRIME_64);
    }
    format!("{prefix}-{}", base36(hash))
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("ASCII digits")
}

/// Whitespace, `#`, `*`, `?` and `!` mean something in ssh_config Host lines,
/// so runs of them become one `-`; leading and trailing dashes drop out.
fn clean_import_alias(value: &str) -> String {
    let stripped = strip_control_characters(value);
    let trimmed = js::js_trim(&stripped);
    let mut replaced = String::with_capacity(trimmed.len());
    let mut in_run = false;
    for c in trimmed.chars() {
        if js::is_js_whitespace(c) || matches!(c, '#' | '*' | '?' | '!') {
            if !in_run {
                replaced.push('-');
                in_run = true;
            }
        } else {
            replaced.push(c);
            in_run = false;
        }
    }
    let unpadded = replaced.trim_matches('-');
    js::utf16_prefix(unpadded, 240).to_owned()
}

/// `Number.parseInt(value, 10)`: leading whitespace, an optional sign, then
/// as many decimal digits as follow. `None` is `NaN`.
pub(crate) fn js_parse_int(value: &str) -> Option<f64> {
    let rest = js::js_trim_start(value);
    let (negative, digits) = match rest.as_bytes().first() {
        Some(b'-') => (true, &rest[1..]),
        Some(b'+') => (false, &rest[1..]),
        _ => (false, rest),
    };
    let mut parsed = None;
    for byte in digits.bytes() {
        if !byte.is_ascii_digit() {
            break;
        }
        parsed = Some(parsed.unwrap_or(0.0) * 10.0 + f64::from(byte - b'0'));
    }
    parsed.map(|number| if negative { -number } else { number })
}

/// `parseInt` constrained to an inclusive integer range.
pub(crate) fn parse_int_in(value: &str, min: u32, max: u32) -> Option<u32> {
    js_parse_int(value)
        .filter(|number| *number >= f64::from(min) && *number <= f64::from(max))
        .map(|number| number as u32)
}

/// A port from `value`, or `fallback` when it is not one (1–65535).
pub(crate) fn parse_port(value: Option<&str>, fallback: u16) -> u16 {
    value
        .and_then(|value| parse_int_in(js::js_trim(value), 1, 65_535))
        .map_or(fallback, |port| port as u16)
}

/// Trim, then drop the trailing run of whitespace and semicolons
/// (`/[\s;]+$/`).
pub(crate) fn trim_trailing_separators(value: &str) -> &str {
    value.trim_end_matches(|c: char| js::is_js_whitespace(c) || c == ';')
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn stable_ids_match_the_typescript_hash() {
        // Values computed with the TypeScript `stableImportId`.
        assert_eq!(stable_import_id("x", ""), "x-33niihzj4ux45");
        assert_eq!(
            stable_import_id("mobaxterm-session", "\0web"),
            "mobaxterm-session-1p20jyeed415r"
        );
        assert_eq!(
            stable_import_id("p", "Häuser 🚀 \u{10FFFF}"),
            "p-1vgv158ywvbsn"
        );
    }

    #[test]
    fn folders_are_normalized_like_the_typescript() {
        assert_eq!(
            normalize_import_folder("Production\\Europe").as_deref(),
            Some("Production/Europe")
        );
        assert_eq!(
            normalize_import_folder(" / a //\u{1}b/ ").as_deref(),
            Some("a/b")
        );
        assert_eq!(normalize_import_folder(""), None);
        assert_eq!(normalize_import_folder(" \\ / "), None);
        let long = format!("{}/{}", "a".repeat(299), "b");
        assert_eq!(
            normalize_import_folder(&long).as_deref(),
            Some("a".repeat(299).as_str())
        );
    }

    #[test]
    fn aliases_replace_ssh_config_metacharacters() {
        let used = HashSet::new();
        assert_eq!(unique_import_alias("My host!", "h", &used, "x"), "My-host");
        assert_eq!(unique_import_alias("  #*  ", "a b", &used, "x"), "a-b");
        assert_eq!(unique_import_alias("", "", &used, "fallback"), "fallback");
        let used: HashSet<String> = ["web".to_owned(), "web-2".to_owned()].into();
        assert_eq!(unique_import_alias("web", "", &used, "x"), "web-3");
    }

    #[test]
    fn parse_int_follows_javascript() {
        assert_eq!(js_parse_int(" 2222abc"), Some(2222.0));
        assert_eq!(js_parse_int("+22"), Some(22.0));
        assert_eq!(js_parse_int("-1"), Some(-1.0));
        assert_eq!(js_parse_int("0x16"), Some(0.0));
        assert_eq!(js_parse_int("abc"), None);
        assert_eq!(js_parse_int(""), None);
        assert_eq!(js_parse_int("-"), None);
        assert_eq!(parse_port(Some("not-a-port"), 22), 22);
        assert_eq!(parse_port(Some("70000"), 22), 22);
        assert_eq!(parse_port(Some(" 2200 "), 22), 2200);
    }

    #[test]
    fn names_are_capped_and_cleaned() {
        assert_eq!(clean_import_name(" \u{7f}a\u{1}b "), "ab");
        assert_eq!(clean_import_name(&"x".repeat(250)).len(), 200);
        assert_eq!(
            clean_import_name(&format!("{} y", "x".repeat(199))),
            "x".repeat(199)
        );
    }
}
