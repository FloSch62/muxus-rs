//! Persisted scrollback (port of `client/src/terminal/scrollback-snapshots.ts`). A snapshot is
//! the serialized buffer, replayable by writing it back to a terminal; Muxus and Muxus-rs store
//! and read the same format.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

pub use muxus_core::api::{TERMINAL_SNAPSHOT_FORMAT_VERSION, TERMINAL_SNAPSHOT_MAX_CHARS};

/// Scrollback depths a snapshot tries, largest first, until one fits its budget.
pub const SNAPSHOT_SCROLLBACK_LADDER: [usize; 5] = [1_000, 400, 150, 50, 10];

/// Ceiling on the background snapshot while output keeps arriving.
pub const TERMINAL_SNAPSHOT_INTERVAL_MS: u64 = 10_000;

/// Idle gap after which output is snapshotted, so closing right after a command does not lose it
/// to the interval above.
pub const TERMINAL_SNAPSHOT_QUIET_MS: u64 = 1_500;

/// Conservative share of the 64 KiB keepalive body quota a closing window may use.
pub const PAGE_KEEPALIVE_BODY_LIMIT_BYTES: usize = 60_000;

const TERMINAL_HISTORY_DIVIDER_TEXT: &str = "[end of restored output]";
// Zero-width characters survive serialization without changing the divider's appearance. They
// give client-injected rows provenance that genuine terminal output containing the same visible
// text does not have.
const TERMINAL_HISTORY_DIVIDER_MARKER: &str = "\u{2063}\u{2060}\u{200b}\u{2063}";
static MARKED_TERMINAL_HISTORY_DIVIDER_TEXT: LazyLock<String> =
    LazyLock::new(|| format!("[{TERMINAL_HISTORY_DIVIDER_MARKER}end of restored output]"));

/// Written after replayed history, before the new session's output.
pub fn terminal_history_divider() -> String {
    format!(
        "\r\n\x1b[90m{}\x1b[0m\r\n",
        *MARKED_TERMINAL_HISTORY_DIVIDER_TEXT
    )
}

/// Written when a stored session is shown without a live connection.
pub const RESTORED_SESSION_NOTICE: &str = "\x1b[90m[restored session]\x1b[0m\r\n";

const ESC: char = '\x1b';
const CURSOR_MOVE_FINALS: &str = "ABCDEFGHdf`";
static CSI_SEQUENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("\x1b\\[[0-?]*[ -/]*[@-~]").unwrap());
static LEGACY_DIVIDER_COLOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("\x1b\\[(?:[0-9:;]*;)?(?:90|38[;:]5[;:]8)(?:;[0-9:;]*)?m").unwrap()
});

/// Start index of a cursor-move sequence (ESC [ params final) ending exactly at `end`, or `end`
/// itself when none does. A plain scan — regexes over newline runs backtrack catastrophically on
/// this input.
fn trailing_cursor_move_start(data: &[u8], end: usize) -> usize {
    if end < 3 || !CURSOR_MOVE_FINALS.as_bytes().contains(&data[end - 1]) {
        return end;
    }
    let mut index = end as isize - 2;
    while index >= 0 && (data[index as usize].is_ascii_digit() || data[index as usize] == b';') {
        index -= 1;
    }
    if index >= 1 && data[index as usize] == b'[' && data[index as usize - 1] == ESC as u8 {
        index as usize - 1
    } else {
        end
    }
}

/// A serialized buffer ends with the blank remainder of the old viewport and a cursor-
/// repositioning sequence. Replayed as-is, that tail pushes the history a screenful above the
/// divider — drop it so the divider hugs the last line.
pub fn trim_replay_tail(data: &str) -> &str {
    let bytes = data.as_bytes();
    let mut end = bytes.len();
    loop {
        if end > 0 && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r') {
            end -= 1;
            continue;
        }
        let start = trailing_cursor_move_start(bytes, end);
        if start == end {
            return &data[..end];
        }
        end = start;
    }
}

/// Remove the visual replay boundary before a terminal buffer becomes the next snapshot.
/// Otherwise every restore serializes the client-injected divider and accumulates another copy on
/// the following restore.
///
/// New dividers carry an invisible marker, so a remote process can safely emit the same visible
/// text. Previously saved dividers have no provenance; migrate only rows that also use the
/// dark-gray color written by older clients.
pub fn strip_terminal_history_dividers(data: &str, include_legacy: bool) -> String {
    data.split("\r\n")
        .filter(|row| {
            let visible = CSI_SEQUENCE.replace_all(row, "");
            if visible == MARKED_TERMINAL_HISTORY_DIVIDER_TEXT.as_str() {
                return false;
            }
            if !include_legacy || visible != TERMINAL_HISTORY_DIVIDER_TEXT {
                return true;
            }
            let text_start = row.find(TERMINAL_HISTORY_DIVIDER_TEXT).unwrap_or(0);
            !LEGACY_DIVIDER_COLOR.is_match(&row[..text_start])
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotBody<'a> {
    data: &'a str,
    format_version: u32,
}

/// The JSON body a snapshot is stored with.
pub fn snapshot_request_body(data: &str) -> String {
    serde_json::to_string(&SnapshotBody {
        data,
        format_version: TERMINAL_SNAPSHOT_FORMAT_VERSION,
    })
    .unwrap_or_default()
}

/// Wire size of a snapshot. JSON escapes every ESC to six characters, so the body is several times
/// the buffer and has to be measured, not estimated.
pub fn snapshot_body_bytes(data: &str) -> usize {
    snapshot_request_body(data).len()
}

/// Serialized recent buffer, or `None` when there is nothing worth keeping. `serialize(depth)`
/// returns the buffer with `depth` lines of scrollback, modes and the alternate screen excluded,
/// so a replay can never leave a fresh terminal stuck in a full-screen application's state. Depth
/// steps down until the result fits both the stored cap and any caller-supplied wire budget.
pub fn serialize_scrollback(
    serialize: impl Fn(usize) -> String,
    max_body_bytes: Option<usize>,
) -> Option<String> {
    for scrollback in SNAPSHOT_SCROLLBACK_LADDER {
        let serialized = strip_terminal_history_dividers(&serialize(scrollback), false);
        let data = trim_replay_tail(&serialized);
        if data.is_empty() {
            return None;
        }
        if data.encode_utf16().count() > TERMINAL_SNAPSHOT_MAX_CHARS {
            continue;
        }
        if max_body_bytes.is_some_and(|budget| snapshot_body_bytes(data) > budget) {
            continue;
        }
        return Some(data.to_string());
    }
    None
}

/// A stored snapshot ready to replay: dividers from earlier restores removed, including the
/// unmarked ones older formats wrote.
pub fn restorable_snapshot(data: &str, format_version: Option<u32>) -> String {
    strip_terminal_history_dividers(
        data,
        format_version.unwrap_or(1) < TERMINAL_SNAPSHOT_FORMAT_VERSION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(chars_per_line: usize, escapes: bool) -> impl Fn(usize) -> String {
        let unit = if escapes { "\x1b[31mx\x1b[0m" } else { "x" };
        move |scrollback| unit.repeat(scrollback * chars_per_line)
    }

    #[test]
    fn drops_the_blank_viewport_remainder_and_the_cursor_repositioning() {
        let serialized = format!(
            "deploy@web-01:~$ echo hi\r\nhi\r\n\x1b[38;5;114mdeploy@web-01\x1b[0m$ {}\x1b[37A\x1b[17C",
            "\r\n".repeat(37)
        );
        assert_eq!(
            trim_replay_tail(&serialized),
            "deploy@web-01:~$ echo hi\r\nhi\r\n\x1b[38;5;114mdeploy@web-01\x1b[0m$ "
        );
    }

    #[test]
    fn leaves_content_without_a_trailing_tail_untouched() {
        assert_eq!(trim_replay_tail("plain output"), "plain output");
        assert_eq!(trim_replay_tail(""), "");
    }

    #[test]
    fn strips_interleaved_newline_and_cursor_move_runs_completely() {
        assert_eq!(trim_replay_tail("kept\r\n\x1b[2A\r\n\x1b[5;7H"), "kept");
    }

    #[test]
    fn keeps_colours_and_non_movement_sequences_in_place() {
        assert_eq!(trim_replay_tail("\x1b[31mred\x1b[0m"), "\x1b[31mred\x1b[0m");
    }

    #[test]
    fn keeps_plain_text_ending_in_a_cursor_move_letter() {
        assert_eq!(
            trim_replay_tail("cd /srv && make USA"),
            "cd /srv && make USA"
        );
        assert_eq!(trim_replay_tail("PATH"), "PATH");
        assert_eq!(trim_replay_tail("[17C"), "[17C");
    }

    #[test]
    fn handles_long_interior_newline_runs_in_linear_time() {
        let pathological = format!("{}x", "\r\n".repeat(1_000)).repeat(20);
        let started = std::time::Instant::now();
        assert_eq!(trim_replay_tail(&pathological), pathological);
        assert!(started.elapsed().as_millis() < 1_000);
    }

    #[test]
    fn the_divider_is_a_dim_single_line_between_restored_and_live_output() {
        let divider = terminal_history_divider();
        let visible: String = divider
            .chars()
            .filter(|c| !matches!(c, '\u{200b}' | '\u{2060}' | '\u{2063}'))
            .collect();
        assert!(visible.contains("[end of restored output]"));
        assert!(divider.starts_with("\r\n"));
        assert!(divider.ends_with("\r\n"));
    }

    #[test]
    fn does_not_retain_injected_dividers_in_the_next_snapshot() {
        let divider = terminal_history_divider();
        let nested = format!("first restore{divider}second restore{divider}live output");
        assert_eq!(
            strip_terminal_history_dividers(&nested, false),
            "first restore\r\nsecond restore\r\nlive output"
        );
        assert_eq!(
            serialize_scrollback(|_| nested.clone(), None).as_deref(),
            Some("first restore\r\nsecond restore\r\nlive output")
        );
    }

    #[test]
    fn recognizes_serialized_divider_rows_independent_of_their_sgr_encoding() {
        let serialized_divider = terminal_history_divider()
            .replacen("\x1b[90m", "\x1b[38;5;8m", 1)
            .replacen("\x1b[0m", "\x1b[39m", 1);
        assert_eq!(
            strip_terminal_history_dividers(&format!("before{serialized_divider}after"), false),
            "before\r\nafter"
        );
    }

    #[test]
    fn keeps_genuine_terminal_output_equal_to_the_divider_text() {
        let output = "echo \"[end of restored output]\"\r\n[end of restored output]\r\n\x1b[90m[end of restored output]\r\n[end of restored output] from remote";
        assert_eq!(strip_terminal_history_dividers(output, false), output);
    }

    #[test]
    fn only_migrates_unmarked_dividers_with_the_legacy_client_styling() {
        let stored = "before\r\n\x1b[90m[end of restored output]\r\n\x1b[38;5;8m[end of restored output]\x1b[39m\r\nafter";
        assert_eq!(
            strip_terminal_history_dividers(stored, true),
            "before\r\nafter"
        );
        assert_eq!(restorable_snapshot(stored, None), "before\r\nafter");
        assert_eq!(restorable_snapshot(stored, Some(2)), stored);
    }

    #[test]
    fn measures_the_json_body_not_the_buffer() {
        assert_eq!(
            snapshot_body_bytes("ab"),
            "{\"data\":\"ab\",\"formatVersion\":2}".len()
        );
        // One ESC becomes the six characters \u001b in the JSON body.
        assert_eq!(
            snapshot_body_bytes("\x1b"),
            "{\"data\":\"\",\"formatVersion\":2}".len() + 6
        );
    }

    #[test]
    fn serializes_at_the_deepest_scrollback_that_fits_the_wire_budget() {
        let data = serialize_scrollback(fake(40, false), Some(8_000)).unwrap();
        assert!(snapshot_body_bytes(&data) <= 8_000);
        // Deeper than the smallest rung: the ladder stepped down, it did not bottom out.
        assert!(data.len() > SNAPSHOT_SCROLLBACK_LADDER[4] * 40);
    }

    #[test]
    fn keeps_a_colour_heavy_buffer_under_the_keepalive_cap() {
        let data =
            serialize_scrollback(fake(80, true), Some(PAGE_KEEPALIVE_BODY_LIMIT_BYTES)).unwrap();
        assert!(snapshot_body_bytes(&data) <= PAGE_KEEPALIVE_BODY_LIMIT_BYTES);
    }

    #[test]
    fn takes_the_full_depth_when_no_budget_is_imposed() {
        assert_eq!(
            serialize_scrollback(fake(40, false), None).unwrap(),
            "x".repeat(SNAPSHOT_SCROLLBACK_LADDER[0] * 40)
        );
    }

    #[test]
    fn reports_nothing_for_an_empty_buffer() {
        assert_eq!(serialize_scrollback(fake(0, false), None), None);
    }
}
