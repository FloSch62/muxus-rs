//! Plain-text session log file names (from `shared/src/api-types.ts`).

use chrono::{Datelike, NaiveDateTime, Timelike};

use crate::js;

pub const DEFAULT_SESSION_LOG_FILE_PATTERN: &str = "{host}_{date}_{time}.log";

/// Placeholders a log file name pattern can use.
pub const SESSION_LOG_FILE_PLACEHOLDERS: [&str; 5] = ["host", "title", "kind", "date", "time"];

/// The values a log file name pattern is filled with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLogFileNameValues {
    pub host: String,
    pub title: String,
    pub kind: String,
    /// Local wall-clock time the log starts at.
    pub started_at: NaiveDateTime,
}

const LOG_FILE_VALUE_MAX: usize = 80;

/// Characters some file system refuses in a name, control characters included.
fn is_unsafe_in_name(c: char) -> bool {
    matches!(
        c,
        '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\u{0000}'..='\u{001f}' | '\u{007f}'
    )
}

/// The same set without `/`, which separates folders in a pattern.
fn is_unsafe_in_pattern(c: char) -> bool {
    c != '/' && is_unsafe_in_name(c)
}

fn replace_unsafe(value: &str) -> String {
    value
        .chars()
        .map(|c| if is_unsafe_in_name(c) { '_' } else { c })
        .collect()
}

fn is_dot_only(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c == '.')
}

/// The `{name}` placeholders of a pattern: `(start, end, name)` byte ranges
/// covering the braces, for `\{([^{}]*)\}`.
fn placeholders(pattern: &str) -> Vec<(usize, usize, &str)> {
    let mut found = Vec::new();
    let bytes = pattern.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'{' {
            let inner_start = index + 1;
            let mut cursor = inner_start;
            while cursor < bytes.len() && bytes[cursor] != b'{' && bytes[cursor] != b'}' {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'}' {
                found.push((index, cursor + 1, &pattern[inner_start..cursor]));
                index = cursor + 1;
                continue;
            }
        }
        index += 1;
    }
    found
}

/// Why a file name pattern cannot be used, or `None` when it can. `/`
/// separates subfolders; everything else must be portable to Windows, macOS
/// and Linux.
pub fn session_log_file_pattern_error(pattern: &str) -> Option<String> {
    let trimmed = js::js_trim(pattern);
    if trimmed.is_empty() {
        return Some("Enter a file name pattern.".into());
    }
    if js::utf16_len(trimmed) > 200 {
        return Some("The pattern is longer than 200 characters.".into());
    }
    let mut chars = trimmed.chars();
    let first = chars.next();
    let second = chars.next();
    if matches!(first, Some('/' | '\\'))
        || (first.is_some_and(|c| c.is_ascii_alphabetic()) && second == Some(':'))
    {
        return Some(
            "The pattern is relative to the log folder; it cannot be an absolute path.".into(),
        );
    }
    let found = placeholders(trimmed);
    for (_, _, name) in &found {
        if !SESSION_LOG_FILE_PLACEHOLDERS.contains(name) {
            return Some(format!("Unknown placeholder {{{name}}}."));
        }
    }
    let mut literal = String::new();
    let mut last = 0;
    for (start, end, _) in &found {
        literal.push_str(&trimmed[last..*start]);
        last = *end;
    }
    literal.push_str(&trimmed[last..]);
    if literal.contains(['{', '}']) {
        return Some("A placeholder is missing its { or }.".into());
    }
    if literal.chars().any(is_unsafe_in_pattern) {
        return Some("The pattern contains a character that is not allowed in file names.".into());
    }
    if trimmed.split('/').any(|segment| {
        let segment = js::js_trim(segment);
        segment.is_empty() || is_dot_only(segment)
    }) {
        return Some("Every folder and file name in the pattern needs a name.".into());
    }
    None
}

/// The relative path a pattern names, as folder segments and a file name.
/// Values are made safe for any file system, so a host or title can never
/// add folders or leave the log folder.
pub fn session_log_file_name(pattern: &str, values: &SessionLogFileNameValues) -> Vec<String> {
    let at = values.started_at;
    let date = format!("{}-{:02}-{:02}", at.year(), at.month(), at.day());
    let time = format!("{:02}-{:02}-{:02}", at.hour(), at.minute(), at.second());
    let replacement = |name: &str| -> Option<&str> {
        match name {
            "host" => Some(&values.host),
            "title" => Some(&values.title),
            "kind" => Some(&values.kind),
            "date" => Some(&date),
            "time" => Some(&time),
            _ => None,
        }
    };
    js::js_trim(pattern)
        .split('/')
        .map(|segment| {
            let mut named = String::new();
            let mut last = 0;
            for (start, end, name) in placeholders(segment) {
                named.push_str(&segment[last..start]);
                match replacement(name) {
                    Some(value) => {
                        named.push_str(js::utf16_prefix(&replace_unsafe(value), LOG_FILE_VALUE_MAX))
                    }
                    None => named.push_str(&segment[start..end]),
                }
                last = end;
            }
            named.push_str(&segment[last..]);
            // Windows drops trailing dots and spaces, and dot-only names are not names.
            let replaced = replace_unsafe(&named);
            let safe = js::js_trim(&replaced).trim_end_matches(['.', ' ']);
            if !safe.is_empty() && !is_dot_only(safe) {
                safe.to_owned()
            } else {
                "_".to_owned()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn values(host: &str, title: &str, kind: &str) -> SessionLogFileNameValues {
        SessionLogFileNameValues {
            host: host.into(),
            title: title.into(),
            kind: kind.into(),
            started_at: NaiveDate::from_ymd_opt(2026, 10, 4)
                .unwrap()
                .and_hms_opt(9, 5, 7)
                .unwrap(),
        }
    }

    #[test]
    fn fills_the_default_pattern_with_the_host_and_local_start_time() {
        assert_eq!(
            session_log_file_name(
                DEFAULT_SESSION_LOG_FILE_PATTERN,
                &values("admin@router1", "Core router", "ssh")
            ),
            vec!["admin@router1_2026-10-04_09-05-07.log"]
        );
    }

    #[test]
    fn turns_slashes_in_the_pattern_into_folders_but_never_in_values() {
        assert_eq!(
            session_log_file_name(
                "{kind}/{title}/{date}.log",
                &values("10.0.0.1:23", "../etc/passwd", "telnet")
            ),
            vec!["telnet", ".._etc_passwd", "2026-10-04.log"]
        );
        assert_eq!(
            session_log_file_name("{host}.log", &values("COM3:?", "", "serial")),
            vec!["COM3__.log"]
        );
    }

    #[test]
    fn never_yields_an_empty_or_dot_only_name() {
        assert_eq!(
            session_log_file_name("{title}/{host}", &values("..", "   ", "ssh")),
            vec!["_", "_"]
        );
    }

    #[test]
    fn explains_unusable_patterns() {
        let error = |pattern: &str| session_log_file_pattern_error(pattern).unwrap_or_default();
        assert_eq!(
            session_log_file_pattern_error(DEFAULT_SESSION_LOG_FILE_PATTERN),
            None
        );
        assert_eq!(
            session_log_file_pattern_error("{host}/{date}/{time}.log"),
            None
        );
        assert!(error("  ").contains("Enter"));
        assert!(error("{hostname}.log").contains("{hostname}"));
        assert!(error("{host.log").contains("placeholder"));
        assert!(error("/var/log/{host}.log").contains("absolute"));
        assert!(error("C:{host}.log").contains("absolute"));
        assert!(error("../{host}.log").contains("needs a name"));
        assert!(error("{host}//x.log").contains("needs a name"));
        assert!(error("{host}:{time}.log").contains("not allowed"));
        assert!(error("{host}\\{time}.log").contains("not allowed"));
    }
}
