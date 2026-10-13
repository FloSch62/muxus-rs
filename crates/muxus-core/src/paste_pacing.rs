//! Paced pastes (port of `shared/src/paste-pacing.ts`).
//!
//! Lengths and positions follow JavaScript: `MAX_PACED_PASTE_LENGTH` counts
//! UTF-16 code units, and a paced paste types one code point at a time.

use serde::{Deserialize, Serialize};

/// Longest wait after each pasted line, in milliseconds.
pub const MAX_PASTE_LINE_DELAY_MS: u32 = 10_000;
/// Longest wait after each pasted character, in milliseconds.
pub const MAX_PASTE_CHAR_DELAY_MS: u32 = 1_000;
/// Largest paste the backend paces, in UTF-16 code units.
pub const MAX_PACED_PASTE_LENGTH: usize = 1_000_000;

pub const BRACKETED_PASTE_START: &str = "\x1b[200~";
pub const BRACKETED_PASTE_END: &str = "\x1b[201~";

/// How slowly a paste is typed into a session. Zero for both sends it at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PastePacing {
    /// Wait after each line, in milliseconds.
    pub line_delay_ms: u32,
    /// Wait after each character, in milliseconds.
    pub char_delay_ms: u32,
}

impl PastePacing {
    pub fn is_paced(&self) -> bool {
        is_paced_paste(*self)
    }
}

pub fn is_paced_paste(pacing: PastePacing) -> bool {
    pacing.line_delay_ms > 0 || pacing.char_delay_ms > 0
}

/// A delay a host or the settings may store: a whole number of milliseconds
/// in range. Takes any JSON value, since the TypeScript checks `unknown`.
pub fn valid_paste_delay(value: &serde_json::Value, max: u32) -> bool {
    value
        .as_f64()
        .is_some_and(|number| number.fract() == 0.0 && number >= 0.0 && number <= f64::from(max))
}

/// Text the way a terminal sends a paste, as xterm.js prepares it: every line
/// break (LF or CRLF) becomes CR, the Enter key. Inside bracketed paste, ESC
/// shows as ␛ so the text cannot close the bracket early.
pub fn prepare_paste_text(text: &str, bracketed: bool) -> String {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        normalized.replace('\x1b', "\u{241b}")
    } else {
        normalized
    }
}

/// Everything an unpaced paste sends, markers included: what xterm.js's own paste sends.
pub fn terminal_paste_data(text: &str, bracketed: bool) -> String {
    let prepared = prepare_paste_text(text, bracketed);
    if bracketed {
        format!("{BRACKETED_PASTE_START}{prepared}{BRACKETED_PASTE_END}")
    } else {
        prepared
    }
}

/// Lines in prepared paste text: every CR ends one, and trailing text is one more.
pub fn pasted_line_count(prepared: &str) -> usize {
    let lines = prepared.bytes().filter(|&byte| byte == b'\r').count();
    if !prepared.is_empty() && !prepared.ends_with('\r') {
        lines + 1
    } else {
        lines
    }
}

/// One write of a paced paste.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteStep {
    pub data: String,
    /// The step ends with a line break.
    pub line_end: bool,
    /// Wait before the next step; zero after the last one.
    pub delay_ms: u32,
}

/// The writes of a paced paste of prepared text: a line at a time, or a
/// character (code point) at a time when there is a character delay. Every
/// character but the last is followed by the character delay, and every line
/// break but a final one by the line delay as well.
pub fn paste_pacing_steps(prepared: &str, pacing: PastePacing) -> PasteSteps<'_> {
    PasteSteps {
        prepared,
        start: 0,
        pacing,
    }
}

/// Iterator returned by [`paste_pacing_steps`].
#[derive(Debug, Clone)]
pub struct PasteSteps<'a> {
    prepared: &'a str,
    start: usize,
    pacing: PastePacing,
}

impl Iterator for PasteSteps<'_> {
    type Item = PasteStep;

    fn next(&mut self) -> Option<PasteStep> {
        let prepared = self.prepared;
        if self.start >= prepared.len() {
            return None;
        }
        let per_character = self.pacing.char_delay_ms > 0;
        let rest = &prepared[self.start..];
        let next = if per_character {
            self.start + rest.chars().next().map_or(1, char::len_utf8)
        } else {
            match rest.find('\r') {
                Some(offset) => self.start + offset + 1,
                None => prepared.len(),
            }
        };
        let line_end = prepared.as_bytes()[next - 1] == b'\r';
        let delay_ms = if next >= prepared.len() {
            0
        } else {
            (if per_character {
                self.pacing.char_delay_ms
            } else {
                0
            }) + (if line_end {
                self.pacing.line_delay_ms
            } else {
                0
            })
        };
        let step = PasteStep {
            data: prepared[self.start..next].to_owned(),
            line_end,
            delay_ms,
        };
        self.start = next;
        Some(step)
    }
}

/// How long the pauses of a paced paste add up to, in milliseconds.
pub fn paste_pacing_duration_ms(prepared: &str, pacing: PastePacing) -> u64 {
    if prepared.is_empty() {
        return 0;
    }
    let characters = prepared.chars().count() as u64;
    let mut line_breaks = prepared.chars().filter(|&c| c == '\r').count() as u64;
    if prepared.ends_with('\r') {
        line_breaks -= 1;
    }
    (characters - 1) * u64::from(pacing.char_delay_ms)
        + line_breaks * u64::from(pacing.line_delay_ms)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::protocol::parse_terminal_client_message;

    fn steps(text: &str, line_delay_ms: u32, char_delay_ms: u32) -> Vec<PasteStep> {
        paste_pacing_steps(
            text,
            PastePacing {
                line_delay_ms,
                char_delay_ms,
            },
        )
        .collect()
    }

    fn step(data: &str, line_end: bool, delay_ms: u32) -> PasteStep {
        PasteStep {
            data: data.to_owned(),
            line_end,
            delay_ms,
        }
    }

    #[test]
    fn turns_every_lf_and_crlf_into_the_cr_a_terminal_sends_for_enter() {
        assert_eq!(prepare_paste_text("a\nb\r\nc\rd\n", false), "a\rb\rc\rd\r");
    }

    #[test]
    fn defangs_esc_only_inside_bracketed_paste() {
        assert_eq!(prepare_paste_text("x\x1b[201~y", false), "x\x1b[201~y");
        assert_eq!(prepare_paste_text("x\x1b[201~y", true), "x\u{241b}[201~y");
    }

    #[test]
    fn sends_an_unpaced_paste_exactly_as_xterm_would() {
        assert_eq!(terminal_paste_data("ls\nexit\n", false), "ls\rexit\r");
        assert_eq!(
            terminal_paste_data("ls\nexit\n", true),
            "\x1b[200~ls\rexit\r\x1b[201~"
        );
    }

    #[test]
    fn counts_the_lines_a_paste_types_in() {
        assert_eq!(pasted_line_count(""), 0);
        assert_eq!(pasted_line_count("show version"), 1);
        assert_eq!(pasted_line_count("a\rb\r"), 2);
        assert_eq!(pasted_line_count("a\rb\rc"), 3);
        assert_eq!(pasted_line_count("\r\r"), 2);
    }

    #[test]
    fn writes_a_line_at_a_time_and_waits_after_every_line_but_the_last() {
        assert_eq!(
            steps("conf t\rhostname r1\rend\r", 200, 0),
            vec![
                step("conf t\r", true, 200),
                step("hostname r1\r", true, 200),
                step("end\r", true, 0)
            ]
        );
        assert_eq!(
            steps("a\rtrailing", 50, 0),
            vec![step("a\r", true, 50), step("trailing", false, 0)]
        );
    }

    #[test]
    fn writes_a_character_at_a_time_adding_the_line_delay_at_breaks() {
        assert_eq!(
            steps("ab\rc", 100, 5),
            vec![
                step("a", false, 5),
                step("b", false, 5),
                step("\r", true, 105),
                step("c", false, 0)
            ]
        );
    }

    #[test]
    fn never_splits_a_character_outside_the_basic_plane() {
        let data: Vec<String> = steps("é🙂x", 0, 1)
            .into_iter()
            .map(|step| step.data)
            .collect();
        assert_eq!(data, vec!["é", "🙂", "x"]);
    }

    #[test]
    fn adds_the_pauses_up_to_the_time_the_paste_takes() {
        let pacing = PastePacing {
            line_delay_ms: 100,
            char_delay_ms: 5,
        };
        let text = "ab\rc\r";
        let total: u64 = steps(text, 100, 5)
            .iter()
            .map(|step| u64::from(step.delay_ms))
            .sum();
        assert_eq!(paste_pacing_duration_ms(text, pacing), total);
        assert_eq!(paste_pacing_duration_ms(text, pacing), 4 * 5 + 100);
        assert_eq!(
            paste_pacing_duration_ms(
                "one\rtwo\rthree",
                PastePacing {
                    line_delay_ms: 250,
                    char_delay_ms: 0
                }
            ),
            500
        );
        assert_eq!(paste_pacing_duration_ms("", pacing), 0);
    }

    #[test]
    fn paces_only_when_a_delay_is_set_and_only_accepts_whole_milliseconds_in_range() {
        assert!(!is_paced_paste(PastePacing {
            line_delay_ms: 0,
            char_delay_ms: 0
        }));
        assert!(is_paced_paste(PastePacing {
            line_delay_ms: 0,
            char_delay_ms: 1
        }));
        assert!(valid_paste_delay(&json!(0), 10));
        assert!(valid_paste_delay(&json!(10), 10));
        assert!(!valid_paste_delay(&json!(11), 10));
        assert!(!valid_paste_delay(&json!(1.5), 10));
        assert!(!valid_paste_delay(&json!("5"), 10));
    }

    #[test]
    fn accepts_a_paste_with_delays_in_range_and_a_cancel() {
        assert!(
            parse_terminal_client_message(json!({
                "op": "paste", "text": "show run\n", "bracketed": true, "lineDelayMs": 250, "charDelayMs": 0
            }))
            .is_ok()
        );
        assert!(parse_terminal_client_message(json!({ "op": "paste-cancel" })).is_ok());
    }

    #[test]
    fn rejects_delays_out_of_range_and_empty_pastes() {
        let paste = json!({ "op": "paste", "text": "x", "bracketed": false, "lineDelayMs": 0, "charDelayMs": 0 });
        let with = |key: &str, value: serde_json::Value| {
            let mut body = paste.clone();
            body[key] = value;
            parse_terminal_client_message(body)
        };
        assert!(with("lineDelayMs", json!(10_001)).is_err());
        assert!(with("charDelayMs", json!(-1)).is_err());
        assert!(with("text", json!("")).is_err());
    }
}
