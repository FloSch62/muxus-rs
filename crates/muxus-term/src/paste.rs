//! Pasting into a terminal: the multi-line safety check, the preview's line-number gutter, paste
//! pacing and the bytes a paste sends (ports `client/src/terminal/paste-safety.ts`,
//! `paste-line-numbers.ts`, `paste-pacing.ts` and the terminal half of `shared/src/paste-pacing.ts`).

use muxus_core::paste_pacing::resolve_paste_prompt_wait;
pub use muxus_core::paste_pacing::{
    BRACKETED_PASTE_END, BRACKETED_PASTE_START, MAX_PACED_PASTE_LENGTH, MAX_PASTE_CHAR_DELAY_MS,
    MAX_PASTE_LINE_DELAY_MS, PastePacing, PastePromptWaitSetting, is_paced_paste, paste_pacing_for,
    prepare_paste_text, terminal_paste_data,
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

/// A host's own paste delays where it sets them, the Terminal settings' otherwise, and its
/// prompt waiting, which only a host turns on.
pub fn host_paste_pacing(
    default_line_delay_ms: u32,
    default_char_delay_ms: u32,
    host_line_delay_ms: Option<u32>,
    host_char_delay_ms: Option<u32>,
    host_prompt_wait: Option<&PastePromptWaitSetting>,
) -> PastePacing {
    PastePacing {
        line_delay_ms: host_line_delay_ms.unwrap_or(default_line_delay_ms),
        char_delay_ms: host_char_delay_ms.unwrap_or(default_char_delay_ms),
        prompt: host_prompt_wait.map(resolve_paste_prompt_wait),
    }
}

/// An invisible or look-alike character in a paste, and its ASCII stand-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlaggedPasteCharacter {
    /// UTF-16 offset in the text, as the TypeScript counts it.
    pub index: usize,
    /// Byte offset in the text.
    pub byte_index: usize,
    /// 1-based; line breaks count as in [`paste_line_count`].
    pub line: usize,
    /// 1-based, in characters.
    pub column: usize,
    pub ch: char,
    pub name: &'static str,
    /// What "Replace with ASCII" puts in its place; empty removes it.
    pub ascii: &'static str,
}

/// Invisible characters, typographic quotes and dashes, and their ASCII stand-ins.
fn flagged_character(c: char) -> Option<(&'static str, &'static str)> {
    Some(match c {
        '\u{a0}' => ("No-break space", " "),
        '\u{202f}' => ("Narrow no-break space", " "),
        '\u{200b}' => ("Zero-width space", ""),
        '\u{200c}' => ("Zero-width non-joiner", ""),
        '\u{200d}' => ("Zero-width joiner", ""),
        '\u{2060}' => ("Word joiner", ""),
        '\u{feff}' => ("Zero-width no-break space", ""),
        '\u{2018}' => ("Left single quote", "'"),
        '\u{2019}' => ("Right single quote", "'"),
        '\u{201a}' => ("Low single quote", "'"),
        '\u{201c}' => ("Left double quote", "\""),
        '\u{201d}' => ("Right double quote", "\""),
        '\u{201e}' => ("Low double quote", "\""),
        '\u{2013}' => ("En dash", "-"),
        // Smart dashes turn a typed "--" into an em dash.
        '\u{2014}' => ("Em dash", "--"),
        _ => return None,
    })
}

/// Cyrillic and Greek letters drawn like Latin ones.
fn latin_lookalike(c: char) -> Option<&'static str> {
    Some(match c {
        '\u{405}' => "S",
        '\u{406}' => "I",
        '\u{408}' => "J",
        '\u{410}' => "A",
        '\u{412}' => "B",
        '\u{415}' => "E",
        '\u{41a}' => "K",
        '\u{41c}' => "M",
        '\u{41d}' => "H",
        '\u{41e}' => "O",
        '\u{420}' => "P",
        '\u{421}' => "C",
        '\u{422}' => "T",
        '\u{423}' => "Y",
        '\u{425}' => "X",
        '\u{430}' => "a",
        '\u{435}' => "e",
        '\u{43e}' => "o",
        '\u{440}' => "p",
        '\u{441}' => "c",
        '\u{443}' => "y",
        '\u{445}' => "x",
        '\u{455}' => "s",
        '\u{456}' => "i",
        '\u{458}' => "j",
        '\u{4bb}' => "h",
        '\u{4cf}' => "l",
        '\u{501}' => "d",
        '\u{51b}' => "q",
        '\u{51d}' => "w",
        '\u{391}' => "A",
        '\u{392}' => "B",
        '\u{395}' => "E",
        '\u{396}' => "Z",
        '\u{397}' => "H",
        '\u{399}' => "I",
        '\u{39a}' => "K",
        '\u{39c}' => "M",
        '\u{39d}' => "N",
        '\u{39f}' => "O",
        '\u{3a1}' => "P",
        '\u{3a4}' => "T",
        '\u{3a5}' => "Y",
        '\u{3a7}' => "X",
        '\u{3b1}' => "a",
        '\u{3b9}' => "i",
        '\u{3bd}' => "v",
        '\u{3bf}' => "o",
        '\u{3c1}' => "p",
        _ => return None,
    })
}

/// Replace removes zero-width characters, so they join the letters around them into one word.
fn in_word(c: char) -> bool {
    c.is_alphabetic() || flagged_character(c).is_some_and(|(_, ascii)| ascii.is_empty())
}

/// Look-alikes count only in a word with Latin letters, so Cyrillic and Greek text is left
/// alone.
fn in_latin_word(chars: &[char], index: usize) -> bool {
    chars[..index]
        .iter()
        .rev()
        .take_while(|c| in_word(**c))
        .any(char::is_ascii_alphabetic)
        || chars[index + 1..]
            .iter()
            .take_while(|c| in_word(**c))
            .any(char::is_ascii_alphabetic)
}

/// The invisible and look-alike characters of a paste, with where they are.
pub fn flag_paste_characters(text: &str) -> Vec<FlaggedPasteCharacter> {
    let chars: Vec<char> = text.chars().collect();
    let mut flagged = Vec::new();
    let (mut line, mut column) = (1, 0);
    let (mut index, mut byte_index) = (0, 0);
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        let (here, here_byte) = (index, byte_index);
        index += c.len_utf16();
        byte_index += c.len_utf8();
        at += 1;
        if c == '\n' || c == '\r' {
            if c == '\r' && chars.get(at) == Some(&'\n') {
                index += 1;
                byte_index += 1;
                at += 1;
            }
            line += 1;
            column = 0;
            continue;
        }
        column += 1;
        if (c as u32) < 0xa0 {
            continue;
        }
        if let Some((name, ascii)) = flagged_character(c) {
            flagged.push(FlaggedPasteCharacter {
                index: here,
                byte_index: here_byte,
                line,
                column,
                ch: c,
                name,
                ascii,
            });
            continue;
        }
        if let Some(ascii) = latin_lookalike(c)
            && in_latin_word(&chars, at - 1)
        {
            let name = if (c as u32) < 0x400 {
                "Greek letter"
            } else {
                "Cyrillic letter"
            };
            flagged.push(FlaggedPasteCharacter {
                index: here,
                byte_index: here_byte,
                line,
                column,
                ch: c,
                name,
                ascii,
            });
        }
    }
    flagged
}

/// Puts each flagged character's ASCII stand-in in its place; nothing else changes.
pub fn replace_flagged_characters(text: &str, flagged: &[FlaggedPasteCharacter]) -> String {
    let mut result = String::with_capacity(text.len());
    let mut from = 0;
    for entry in flagged {
        result.push_str(&text[from..entry.byte_index]);
        result.push_str(entry.ascii);
        from = entry.byte_index + entry.ch.len_utf8();
    }
    result.push_str(&text[from..]);
    result
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
    use muxus_core::api::TerminalEnterKey;

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
            host_paste_pacing(150, 2, None, None, None),
            PastePacing::delays(150, 2)
        );
    }

    #[test]
    fn takes_each_delay_a_host_sets_including_0_to_turn_pacing_off() {
        assert_eq!(
            host_paste_pacing(150, 2, Some(500), None, None),
            PastePacing::delays(500, 2)
        );
        assert_eq!(
            host_paste_pacing(150, 2, Some(0), Some(0), None),
            PastePacing::delays(0, 0)
        );
    }

    #[test]
    fn waits_for_the_prompt_only_on_a_host_that_turns_it_on_with_the_defaults_it_leaves_empty() {
        assert_eq!(
            host_paste_pacing(150, 2, Some(300), None, None).prompt,
            None
        );
        assert_eq!(
            host_paste_pacing(150, 2, None, None, Some(&PastePromptWaitSetting::default())),
            PastePacing {
                line_delay_ms: 150,
                char_delay_ms: 2,
                prompt: Some(muxus_core::paste_pacing::PastePromptWait {
                    prompt_pattern: "[>#$]\\s*$".into(),
                    timeout_ms: 5000,
                    error_pattern: "^%\\s*(Invalid|Incomplete|Ambiguous)".into(),
                }),
            }
        );
        let own = PastePromptWaitSetting {
            prompt_pattern: Some("\\]\\s*$".into()),
            timeout_ms: Some(800),
            error_pattern: None,
        };
        let prompt = host_paste_pacing(150, 2, None, None, Some(&own))
            .prompt
            .unwrap();
        assert_eq!(
            (prompt.prompt_pattern.as_str(), prompt.timeout_ms),
            ("\\]\\s*$", 800)
        );
    }

    // --- flagging invisible and look-alike characters (paste-safety.test.ts) ---

    fn positions(text: &str) -> Vec<(usize, usize, usize, &'static str, &'static str)> {
        flag_paste_characters(text)
            .into_iter()
            .map(|f| (f.index, f.line, f.column, f.name, f.ascii))
            .collect()
    }

    #[test]
    fn leaves_plain_ascii_and_accented_latin_text_alone() {
        assert!(
            flag_paste_characters("interface Gi0/1\n description caf\u{e9} \"uplink\"").is_empty()
        );
    }

    #[test]
    fn flags_spaces_zero_width_characters_quotes_and_dashes_with_their_positions() {
        let text =
            "echo\u{a0}\u{201c}hi\u{201d}\u{200b}\nls \u{2013}l \u{2014}all\r\n\u{feff}x\u{2019}s";
        assert_eq!(
            positions(text),
            vec![
                (4, 1, 5, "No-break space", " "),
                (5, 1, 6, "Left double quote", "\""),
                (8, 1, 9, "Right double quote", "\""),
                (9, 1, 10, "Zero-width space", ""),
                (14, 2, 4, "En dash", "-"),
                (17, 2, 7, "Em dash", "--"),
                (23, 3, 1, "Zero-width no-break space", ""),
                (25, 3, 3, "Right single quote", "'"),
            ]
        );
    }

    #[test]
    fn counts_columns_in_characters_not_utf16_units() {
        let flagged = flag_paste_characters("\u{1f600}\u{a0}");
        assert_eq!((flagged[0].index, flagged[0].column), (2, 2));
    }

    #[test]
    fn flags_cyrillic_and_greek_lookalikes_only_in_words_with_latin_letters() {
        // Look-alikes at the start, middle and end of a word with Latin letters are flagged;
        // the all-Cyrillic and all-Greek words stay.
        let text =
            "hostname \u{441}ore-sw1 G\u{3bf}g0 cor\u{435} \u{441}\u{43e}\u{440} \u{3b1}\u{3b2}";
        let flagged: Vec<_> = flag_paste_characters(text)
            .into_iter()
            .map(|f| (f.index, f.name, f.ascii))
            .collect();
        assert_eq!(
            flagged,
            vec![
                (9, "Cyrillic letter", "c"),
                (19, "Greek letter", "o"),
                (26, "Cyrillic letter", "e"),
            ]
        );
    }

    #[test]
    fn cleans_lookalikes_joined_to_a_latin_word_by_a_zero_width_character_in_one_replacement() {
        let text = "ping \u{441}\u{43e}\u{440}\u{200b}e-01 sw\u{2060}\u{441}\u{43e}";
        let replaced = replace_flagged_characters(text, &flag_paste_characters(text));
        assert_eq!(replaced, "ping cope-01 swco");
        assert!(flag_paste_characters(&replaced).is_empty());
    }

    #[test]
    fn joins_words_only_across_the_characters_that_replace_removes() {
        // The NBSP and the quotes end a word, and a zero-width character alone does not make it
        // Latin.
        let text = "\u{441}\u{43e}\u{440}\u{200b} \u{441}\u{a0}Cisco \u{201c}\u{441}\u{43e}\u{440}\u{201d}core";
        let indexes: Vec<_> = flag_paste_characters(text)
            .into_iter()
            .map(|f| f.index)
            .collect();
        assert_eq!(indexes, vec![3, 6, 13, 17]);
    }

    #[test]
    fn replaces_only_the_flagged_characters() {
        let text =
            "ping\u{a0}\u{441}ore \u{441}\u{43e}\u{440} \u{201c}x\u{201d}\u{200b} \u{2014} y";
        assert_eq!(
            replace_flagged_characters(text, &flag_paste_characters(text)),
            "ping core \u{441}\u{43e}\u{440} \"x\" -- y"
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
        assert_eq!(
            terminal_paste_data("ls\nexit\n", false, TerminalEnterKey::Cr),
            "ls\rexit\r"
        );
        assert_eq!(
            terminal_paste_data("ls\nexit\n", true, TerminalEnterKey::Cr),
            "\x1b[200~ls\rexit\r\x1b[201~"
        );
    }

    #[test]
    fn paces_only_when_a_delay_is_set() {
        assert!(!is_paced_paste(&PastePacing::delays(0, 0)));
        assert!(is_paced_paste(&PastePacing::delays(0, 1)));
    }
}
