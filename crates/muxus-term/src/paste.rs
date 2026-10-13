//! Pasting into a terminal: the multi-line safety check, the preview's line-number gutter, paste
//! pacing and the bytes a paste sends (ports `client/src/terminal/paste-safety.ts`,
//! `paste-line-numbers.ts`, `paste-pacing.ts` and the terminal half of `shared/src/paste-pacing.ts`).

pub use muxus_core::paste_pacing::{
    BRACKETED_PASTE_END, BRACKETED_PASTE_START, MAX_PACED_PASTE_LENGTH, MAX_PASTE_CHAR_DELAY_MS,
    MAX_PASTE_LINE_DELAY_MS, PastePacing, is_paced_paste, prepare_paste_text, terminal_paste_data,
};

/// A paste with a line break could run several shell commands, so it is previewed first.
pub fn requires_paste_confirmation(text: &str) -> bool {
    text.contains(['\r', '\n'])
}

/// Lines in a paste: LF, CR and CRLF each end one.
pub fn paste_line_count(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 1;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => count += 1,
            b'\r' => {
                count += 1;
                if bytes.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
            }
            _ => {}
        }
        index += 1;
    }
    count
}

const EDITOR_LINE_HEIGHT_PX: f64 = 18.0;
const MAX_RENDERED_LINE_NUMBERS: usize = 20;

/// The line-number labels visible in the fixed-height paste editor.
#[derive(Debug, Clone, PartialEq)]
pub struct PasteLineNumberWindow {
    pub labels: String,
    pub offset_px: f64,
}

/// Return only the line-number labels visible in the fixed-height paste editor.
pub fn paste_line_number_window(
    line_count: f64,
    scroll_top: f64,
    line_height_px: Option<f64>,
) -> PasteLineNumberWindow {
    let bounded_line_count = line_count.floor().max(1.0) as usize;
    let bounded_scroll_top = scroll_top.max(0.0);
    let line_height = line_height_px
        .filter(|height| height.is_finite() && *height > 0.0)
        .unwrap_or(EDITOR_LINE_HEIGHT_PX);
    let first_index =
        ((bounded_scroll_top / line_height).floor() as usize).min(bounded_line_count - 1);
    let end_index = bounded_line_count.min(first_index + MAX_RENDERED_LINE_NUMBERS);
    let labels = (first_index..end_index)
        .map(|index| (index + 1).to_string())
        .collect::<Vec<_>>()
        .join("\n");
    PasteLineNumberWindow {
        labels,
        offset_px: bounded_scroll_top - first_index as f64 * line_height,
    }
}

/// Which delay a typed value is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteDelayKind {
    Line,
    Char,
}

impl PasteDelayKind {
    pub fn max(self) -> u32 {
        match self {
            Self::Line => MAX_PASTE_LINE_DELAY_MS,
            Self::Char => MAX_PASTE_CHAR_DELAY_MS,
        }
    }
}

/// A typed delay: empty is `None`, anything else a whole number clamped to range.
pub fn parse_paste_delay(text: &str, kind: PasteDelayKind) -> Option<u32> {
    if text.trim().is_empty() {
        return None;
    }
    let value: f64 = text.trim().parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    // JavaScript's Math.round rounds halves up.
    let rounded = (value + 0.5).floor();
    Some(rounded.clamp(0.0, f64::from(kind.max())) as u32)
}

/// A host's own paste delays where it sets them, the Terminal settings' otherwise.
pub fn host_paste_pacing(
    default_line_delay_ms: u32,
    default_char_delay_ms: u32,
    host_line_delay_ms: Option<u32>,
    host_char_delay_ms: Option<u32>,
) -> PastePacing {
    PastePacing {
        line_delay_ms: host_line_delay_ms.unwrap_or(default_line_delay_ms),
        char_delay_ms: host_char_delay_ms.unwrap_or(default_char_delay_ms),
    }
}

/// A paste's running time, rounded for reading: "under a second", "about 12 s", "about 4 min".
pub fn format_paste_duration(ms: f64) -> String {
    if ms < 1000.0 {
        return "under a second".into();
    }
    if ms < 90_000.0 {
        return format!("about {} s", (ms / 1000.0 + 0.5).floor());
    }
    format!("about {} min", (ms / 60_000.0 + 0.5).floor())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_a_single_line_without_interruption() {
        assert!(!requires_paste_confirmation("echo hello"));
        assert_eq!(paste_line_count("echo hello"), 1);
    }

    #[test]
    fn requires_confirmation_for_lf_crlf_cr_and_a_trailing_newline() {
        for text in ["one\ntwo", "one\r\ntwo", "one\rtwo", "rm -rf ./target\n"] {
            assert!(requires_paste_confirmation(text));
            assert_eq!(paste_line_count(text), 2);
        }
    }

    #[test]
    fn counts_a_very_large_paste() {
        assert_eq!(paste_line_count(&"line\n".repeat(100_000)), 100_001);
    }

    #[test]
    fn renders_at_most_the_visible_gutter_window_for_a_very_large_paste() {
        let window = paste_line_number_window(1_000_000.0, 0.0, None);
        let labels: Vec<&str> = window.labels.split('\n').collect();
        assert_eq!(labels.len(), 20);
        assert!(window.labels.starts_with("1\n2\n"));
        assert!(window.labels.ends_with("\n19\n20"));
        assert_eq!(window.offset_px, 0.0);
    }

    #[test]
    fn moves_the_bounded_window_to_match_the_editor_scroll_position() {
        let window = paste_line_number_window(1_000_000.0, 999_980.0 * 18.0 + 7.0, None);
        let labels: Vec<&str> = window.labels.split('\n').collect();
        assert_eq!(labels.len(), 20);
        assert_eq!(labels[0], "999981");
        assert_eq!(*labels.last().unwrap(), "1000000");
        assert_eq!(window.offset_px, 7.0);
    }

    #[test]
    fn uses_the_measured_editor_line_height_when_the_interface_is_scaled() {
        let window = paste_line_number_window(1_000.0, 981.0 * 21.3571, Some(21.3571));
        let labels: Vec<&str> = window.labels.split('\n').collect();
        assert_eq!(labels[0], "982");
        assert_eq!(*labels.last().unwrap(), "1000");
        assert!(window.offset_px.abs() < 0.005);
    }

    #[test]
    fn follows_the_terminal_settings_where_the_host_sets_nothing() {
        assert_eq!(
            host_paste_pacing(150, 2, None, None),
            PastePacing {
                line_delay_ms: 150,
                char_delay_ms: 2
            }
        );
    }

    #[test]
    fn takes_each_delay_a_host_sets_including_0_to_turn_pacing_off() {
        assert_eq!(
            host_paste_pacing(150, 2, Some(500), None),
            PastePacing {
                line_delay_ms: 500,
                char_delay_ms: 2
            }
        );
        assert_eq!(
            host_paste_pacing(150, 2, Some(0), Some(0)),
            PastePacing {
                line_delay_ms: 0,
                char_delay_ms: 0
            }
        );
    }

    #[test]
    fn rounds_the_running_time_for_reading() {
        assert_eq!(format_paste_duration(400.0), "under a second");
        assert_eq!(format_paste_duration(12_400.0), "about 12 s");
        assert_eq!(format_paste_duration(240_000.0), "about 4 min");
    }

    #[test]
    fn reads_a_typed_delay_as_empty_or_a_whole_number_clamped_to_range() {
        assert_eq!(parse_paste_delay("", PasteDelayKind::Line), None);
        assert_eq!(parse_paste_delay("  ", PasteDelayKind::Char), None);
        assert_eq!(parse_paste_delay("250", PasteDelayKind::Line), Some(250));
        assert_eq!(parse_paste_delay("12.6", PasteDelayKind::Char), Some(13));
        assert_eq!(parse_paste_delay("-5", PasteDelayKind::Line), Some(0));
        assert_eq!(
            parse_paste_delay("99999", PasteDelayKind::Line),
            Some(10_000)
        );
        assert_eq!(parse_paste_delay("5000", PasteDelayKind::Char), Some(1_000));
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
    fn paces_only_when_a_delay_is_set() {
        assert!(!is_paced_paste(PastePacing {
            line_delay_ms: 0,
            char_delay_ms: 0
        }));
        assert!(is_paced_paste(PastePacing {
            line_delay_ms: 0,
            char_delay_ms: 1
        }));
    }
}
