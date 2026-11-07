//! Go to Line (Ctrl/Cmd+G, or the status bar's position): Monaco's go-to-line quick access
//! (`gotoLineQuickAccess.ts`). `12` goes to line 12, `12:5` (or `12,5`, `12#5`) to its fifth
//! column, `-1` to the last line, `::40` to the 40th character of the file.

use muxus_core::js::{js_trim, js_trim_start};

/// Where the typed position leads, and what the picker says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GotoTarget {
    /// 1-based line, when the text names one.
    pub line: Option<usize>,
    /// 1-based column (characters, tabs counted once).
    pub column: usize,
    pub label: String,
}

/// JavaScript's `parseInt(value, 10)`: leading blanks, a sign, digits; `None` for NaN.
fn parse_int(value: &str) -> Option<i64> {
    let value = js_trim_start(value);
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let number: i64 = digits[..end].parse().unwrap_or(i64::MAX);
    Some(if negative { -number } else { number })
}

/// The visible column (0-based) before 1-based `column`, tabs to the next tab stop.
fn visible_column(line: &str, column: usize, tab_size: usize) -> usize {
    let tab_size = tab_size.max(1);
    line.chars()
        .take(column.saturating_sub(1))
        .fold(0, |visible, c| {
            if c == '\t' {
                visible + tab_size - visible % tab_size
            } else {
                visible + 1
            }
        })
}

/// The 1-based column nearest to visible column `target` (0-based).
fn column_from_visible(line: &str, target: usize, tab_size: usize) -> usize {
    let tab_size = tab_size.max(1);
    let mut before = 0;
    for (index, c) in line.chars().enumerate() {
        let after = if c == '\t' {
            before + tab_size - before % tab_size
        } else {
            before + 1
        };
        if after > target {
            // A tie goes to the column before.
            return if after - target < target - before {
                index + 2
            } else {
                index + 1
            };
        }
        before = after;
    }
    line.chars().count() + 1
}

/// What the go-to-line picker shows for `input` (the text after its `:` prefix) in a document
/// of `lines`.
pub fn parse_position(input: &str, lines: &[&str], tab_size: usize) -> GotoTarget {
    let max_line = lines.len().max(1);
    let line_text = |line: usize| lines.get(line - 1).copied().unwrap_or("");
    if let Some(offset_text) = input.strip_prefix(':') {
        // `::<offset>`: a character position, 1-based, negative from the end.
        let max_offset: usize = lines.iter().map(|line| line.chars().count()).sum::<usize>()
            + lines.len().saturating_sub(1);
        let Some(mut offset) = parse_int(offset_text) else {
            return GotoTarget {
                line: None,
                column: 1,
                label: format!("Type a character position to go to (from 1 to {max_offset})."),
            };
        };
        let reverse = offset < 0;
        offset -= offset.signum();
        if reverse {
            offset += max_offset as i64;
        }
        let mut remaining = offset.clamp(0, max_offset as i64) as usize;
        let mut line = 1;
        for (index, text) in lines.iter().enumerate() {
            let length = text.chars().count();
            line = index + 1;
            if remaining <= length || index + 1 == lines.len() {
                break;
            }
            remaining -= length + 1;
        }
        let column = remaining.min(line_text(line).chars().count()) + 1;
        let visible = visible_column(line_text(line), column, tab_size) + 1;
        return GotoTarget {
            line: Some(line),
            column,
            label: format!("Press 'Enter' to go to line {line} at column {visible}."),
        };
    }
    let parts: Vec<&str> = input.split([',', ':', '#']).collect();
    let Some(mut line) = parse_int(js_trim(parts[0])) else {
        return GotoTarget {
            line: None,
            column: 1,
            label: format!("Type a line number to go to (from 1 to {max_line})."),
        };
    };
    if line < 0 {
        line += max_line as i64 + 1;
    }
    let line = line.clamp(1, max_line as i64) as usize;
    let text = line_text(line);
    let max_column = visible_column(text, text.chars().count() + 1, tab_size) + 1;
    let column = parts.get(1).and_then(|part| parse_int(js_trim(part)));
    let Some(mut column) = column else {
        return GotoTarget {
            line: Some(line),
            column: 1,
            label: if parts.len() < 2 {
                format!(
                    "Press 'Enter' to go to line {line} or enter colon : to add a column number."
                )
            } else {
                format!(
                    "Press 'Enter' to go to line {line} or enter a column number (from 1 to {max_column})."
                )
            },
        };
    };
    if column < 0 {
        column += max_column as i64;
    }
    let column = column.clamp(1, max_column as i64) as usize;
    GotoTarget {
        line: Some(line),
        column: column_from_visible(text, column - 1, tab_size),
        label: format!("Press 'Enter' to go to line {line} at column {column}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINES: &[&str] = &["services:", "\tedge:", "  image: x", ""];

    #[test]
    fn asks_for_a_line_until_one_is_typed() {
        let target = parse_position("", LINES, 4);
        assert_eq!(target.line, None);
        assert_eq!(target.label, "Type a line number to go to (from 1 to 4).");
        assert_eq!(parse_position("x", LINES, 4).line, None);
    }

    #[test]
    fn goes_to_lines_clamped_and_from_the_end() {
        let target = parse_position("2", LINES, 4);
        assert_eq!((target.line, target.column), (Some(2), 1));
        assert_eq!(
            target.label,
            "Press 'Enter' to go to line 2 or enter colon : to add a column number."
        );
        assert_eq!(parse_position("99", LINES, 4).line, Some(4));
        assert_eq!(parse_position("-1", LINES, 4).line, Some(4));
        assert_eq!(parse_position("-4", LINES, 4).line, Some(1));
        assert_eq!(parse_position(" 3abc", LINES, 4).line, Some(3));
    }

    #[test]
    fn counts_columns_as_they_look() {
        // Line 2 is a tab (four columns) then `edge:`.
        let target = parse_position("2:", LINES, 4);
        assert_eq!(
            target.label,
            "Press 'Enter' to go to line 2 or enter a column number (from 1 to 10)."
        );
        let target = parse_position("2,5", LINES, 4);
        assert_eq!(
            (target.column, target.label.as_str()),
            (2, "Press 'Enter' to go to line 2 at column 5.")
        );
        // Inside the tab: the nearer edge, the one before on a tie.
        assert_eq!(parse_position("2#4", LINES, 4).column, 2);
        assert_eq!(parse_position("2#3", LINES, 4).column, 1);
        assert_eq!(parse_position("1:-1", LINES, 4).column, 9);
        assert_eq!(parse_position("1:99", LINES, 4).column, 10);
    }

    #[test]
    fn goes_to_character_offsets() {
        assert_eq!(
            parse_position(":", LINES, 4).label,
            "Type a character position to go to (from 1 to 28)."
        );
        // The 12th character is the `e` of `edge` (line 2, after the tab).
        let target = parse_position(":12", LINES, 4);
        assert_eq!((target.line, target.column), (Some(2), 2));
        assert_eq!(target.label, "Press 'Enter' to go to line 2 at column 5.");
        assert_eq!(parse_position(":-1", LINES, 4).line, Some(4));
    }
}
