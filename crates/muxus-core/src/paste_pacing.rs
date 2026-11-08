//! Paced pastes (port of `shared/src/paste-pacing.ts`).
//!
//! Lengths and positions follow JavaScript: `MAX_PACED_PASTE_LENGTH` counts
//! UTF-16 code units, and a paced paste types one code point at a time.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::api::TerminalEnterKey;

/// Longest wait after each pasted line, in milliseconds.
pub const MAX_PASTE_LINE_DELAY_MS: u32 = 10_000;
/// Longest wait after each pasted character, in milliseconds.
pub const MAX_PASTE_CHAR_DELAY_MS: u32 = 1_000;
/// Largest paste the backend paces, in UTF-16 code units.
pub const MAX_PACED_PASTE_LENGTH: usize = 1_000_000;

pub const BRACKETED_PASTE_START: &str = "\x1b[200~";
pub const BRACKETED_PASTE_END: &str = "\x1b[201~";

/// The prompt a paste waits for after each line, unless the host sets its own.
pub const DEFAULT_PASTE_PROMPT_PATTERN: &str = "[>#$]\\s*$";
/// An output line that stops a paste, unless the host sets its own: an IOS-style rejection.
pub const DEFAULT_PASTE_ERROR_PATTERN: &str = "^%\\s*(Invalid|Incomplete|Ambiguous)";
/// Longest wait for the prompt after each line, unless the host sets its own.
pub const DEFAULT_PASTE_PROMPT_TIMEOUT_MS: u32 = 5_000;
pub const MIN_PASTE_PROMPT_TIMEOUT_MS: u32 = 100;
pub const MAX_PASTE_PROMPT_TIMEOUT_MS: u32 = 60_000;
/// Longest prompt or error pattern, in characters (UTF-16 code units).
pub const MAX_PASTE_PATTERN_LENGTH: usize = 500;

/// Pacing a paste by the device's answers: after each line, wait for the
/// prompt instead of the line delay, and stop at the first rejected line.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PastePromptWait {
    /// Regex the line the device is on matches once it is ready for the next line.
    pub prompt_pattern: String,
    /// Longest wait for the prompt after a line, in milliseconds; then the line delay applies.
    pub timeout_ms: u32,
    /// Regex an output line matches when the device rejected the line sent.
    pub error_pattern: String,
}

/// A host's stored prompt waiting (`Partial<PastePromptWait>`): a field left
/// out uses the default. Present at all means prompt waiting is on.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PastePromptWaitSetting {
    pub prompt_pattern: Option<String>,
    pub timeout_ms: Option<u32>,
    pub error_pattern: Option<String>,
}

/// How slowly a paste is typed into a session. Zero for both sends it at once.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PastePacing {
    /// Wait after each line, in milliseconds.
    pub line_delay_ms: u32,
    /// Wait after each character, in milliseconds.
    pub char_delay_ms: u32,
    /// Wait for the prompt after each line; absent paces by the delays alone.
    pub prompt: Option<PastePromptWait>,
}

impl PastePacing {
    /// Delays only, without prompt waiting.
    pub fn delays(line_delay_ms: u32, char_delay_ms: u32) -> Self {
        Self {
            line_delay_ms,
            char_delay_ms,
            prompt: None,
        }
    }

    pub fn is_paced(&self) -> bool {
        is_paced_paste(self)
    }
}

pub fn is_paced_paste(pacing: &PastePacing) -> bool {
    pacing.line_delay_ms > 0 || pacing.char_delay_ms > 0 || pacing.prompt.is_some()
}

/// The pacing a paste goes out with. A bracketed paste reaches the program
/// whole, with no prompt after each line, so it keeps the delays alone.
pub fn paste_pacing_for(pacing: &PastePacing, bracketed: bool) -> PastePacing {
    if !bracketed || pacing.prompt.is_none() {
        return pacing.clone();
    }
    PastePacing::delays(pacing.line_delay_ms, pacing.char_delay_ms)
}

/// A host's prompt waiting, with the defaults where it sets nothing.
pub fn resolve_paste_prompt_wait(setting: &PastePromptWaitSetting) -> PastePromptWait {
    let or_default = |value: &Option<String>, default: &str| match value.as_deref() {
        Some(value) if !value.is_empty() => value.to_owned(),
        _ => default.to_owned(),
    };
    PastePromptWait {
        prompt_pattern: or_default(&setting.prompt_pattern, DEFAULT_PASTE_PROMPT_PATTERN),
        timeout_ms: setting
            .timeout_ms
            .unwrap_or(DEFAULT_PASTE_PROMPT_TIMEOUT_MS),
        error_pattern: or_default(&setting.error_pattern, DEFAULT_PASTE_ERROR_PATTERN),
    }
}

/// Why a prompt or error pattern cannot be used, or `None` when it can.
///
/// The pattern is checked against the JavaScript grammar (no flags), then
/// compiled to test it against empty text. V8's own wording for a pattern
/// that does not compile is not reproduced; the message starts the same way.
pub fn paste_pattern_problem(pattern: &str) -> Option<String> {
    if crate::js::utf16_len(pattern) > MAX_PASTE_PATTERN_LENGTH {
        return Some(format!(
            "Use at most {MAX_PASTE_PATTERN_LENGTH} characters."
        ));
    }
    if !crate::api::is_valid_js_regex(pattern) {
        return Some(format!(
            "Invalid regular expression: /{pattern}/: Invalid pattern"
        ));
    }
    // Such a pattern matches any output at all, so it could never tell anything apart.
    let matches_empty = match fancy_regex::Regex::new(pattern) {
        Ok(regex) => regex.is_match("").unwrap_or(false),
        // A JavaScript pattern the Rust engine cannot read; JavaScript accepted it.
        Err(_) => false,
    };
    if matches_empty {
        return Some("This pattern matches empty text, so it would match any output.".into());
    }
    None
}

/// A whole number of milliseconds the prompt wait may take. Takes any JSON
/// value, since the TypeScript checks `unknown`.
pub fn valid_paste_prompt_timeout(value: &serde_json::Value) -> bool {
    value.as_f64().is_some_and(|number| {
        number.fract() == 0.0
            && number >= f64::from(MIN_PASTE_PROMPT_TIMEOUT_MS)
            && number <= f64::from(MAX_PASTE_PROMPT_TIMEOUT_MS)
    })
}

/// Shape and range check for a setting read from a backup
/// (`isPastePromptWait`); the store's caller validates it again.
pub fn is_paste_prompt_wait(value: &serde_json::Value) -> bool {
    let Some(setting) = value.as_object() else {
        return false;
    };
    let pattern = |key: &str| match setting.get(key) {
        None => true,
        Some(field) => field
            .as_str()
            .is_some_and(|pattern| paste_pattern_problem(pattern).is_none()),
    };
    pattern("promptPattern")
        && setting
            .get("timeoutMs")
            .is_none_or(valid_paste_prompt_timeout)
        && pattern("errorPattern")
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

/// Prepared text typed with a host's Enter key: every CR, which is Enter in
/// prepared text, goes out as CR LF or LF instead. Bracketed pastes keep CR:
/// the program reading them takes the text as a paste, not as keys, and
/// expects the CR every terminal sends there.
pub fn with_enter_key(prepared: &str, enter_key: TerminalEnterKey) -> String {
    match enter_key {
        TerminalEnterKey::Cr => prepared.to_owned(),
        key => prepared.replace('\r', key.data()),
    }
}

/// Everything an unpaced paste sends, markers included: what xterm.js's own
/// paste sends, with the host's Enter key when the paste is not bracketed.
pub fn terminal_paste_data(text: &str, bracketed: bool, enter_key: TerminalEnterKey) -> String {
    let prepared = prepare_paste_text(text, bracketed);
    if bracketed {
        format!("{BRACKETED_PASTE_START}{prepared}{BRACKETED_PASTE_END}")
    } else {
        with_enter_key(&prepared, enter_key)
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
pub fn paste_pacing_steps<'a>(prepared: &'a str, pacing: &PastePacing) -> PasteSteps<'a> {
    PasteSteps {
        prepared,
        start: 0,
        line_delay_ms: pacing.line_delay_ms,
        char_delay_ms: pacing.char_delay_ms,
    }
}

/// Iterator returned by [`paste_pacing_steps`].
#[derive(Debug, Clone)]
pub struct PasteSteps<'a> {
    prepared: &'a str,
    start: usize,
    line_delay_ms: u32,
    char_delay_ms: u32,
}

impl Iterator for PasteSteps<'_> {
    type Item = PasteStep;

    fn next(&mut self) -> Option<PasteStep> {
        let prepared = self.prepared;
        if self.start >= prepared.len() {
            return None;
        }
        let per_character = self.char_delay_ms > 0;
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
            (if per_character { self.char_delay_ms } else { 0 })
                + (if line_end { self.line_delay_ms } else { 0 })
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
pub fn paste_pacing_duration_ms(prepared: &str, pacing: &PastePacing) -> u64 {
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
        paste_pacing_steps(text, &PastePacing::delays(line_delay_ms, char_delay_ms)).collect()
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
    fn keeps_the_bytes_of_cr_the_default_enter_key_exactly_as_before() {
        let text = "conf t\nhostname r1\r\nend\r";
        assert_eq!(
            terminal_paste_data(text, false, TerminalEnterKey::Cr),
            "conf t\rhostname r1\rend\r"
        );
        assert_eq!(with_enter_key("a\rb", TerminalEnterKey::Cr), "a\rb");
    }

    #[test]
    fn sends_every_line_break_of_an_unbracketed_paste_as_the_hosts_enter_key() {
        assert_eq!(
            terminal_paste_data("a\nb\r\nc\rd", false, TerminalEnterKey::Crlf),
            "a\r\nb\r\nc\r\nd"
        );
        assert_eq!(
            terminal_paste_data("a\nb\r\nc\rd", false, TerminalEnterKey::Lf),
            "a\nb\nc\nd"
        );
    }

    #[test]
    fn keeps_cr_inside_a_bracketed_paste_which_the_program_reads_as_text_not_keys() {
        for key in [TerminalEnterKey::Crlf, TerminalEnterKey::Lf] {
            assert_eq!(
                terminal_paste_data("ls\nexit\n", true, key),
                "\x1b[200~ls\rexit\r\x1b[201~"
            );
        }
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
        let pacing = PastePacing::delays(100, 5);
        let text = "ab\rc\r";
        let total: u64 = steps(text, 100, 5)
            .iter()
            .map(|step| u64::from(step.delay_ms))
            .sum();
        assert_eq!(paste_pacing_duration_ms(text, &pacing), total);
        assert_eq!(paste_pacing_duration_ms(text, &pacing), 4 * 5 + 100);
        assert_eq!(
            paste_pacing_duration_ms("one\rtwo\rthree", &PastePacing::delays(250, 0)),
            500
        );
        assert_eq!(paste_pacing_duration_ms("", &pacing), 0);
    }

    #[test]
    fn paces_only_when_a_delay_is_set_or_the_prompt_is_awaited() {
        assert!(!is_paced_paste(&PastePacing::delays(0, 0)));
        assert!(is_paced_paste(&PastePacing::delays(0, 1)));
        assert!(is_paced_paste(&PastePacing {
            prompt: Some(resolve_paste_prompt_wait(&PastePromptWaitSetting::default())),
            ..PastePacing::delays(0, 0)
        }));
        assert!(valid_paste_delay(&json!(0), 10));
        assert!(valid_paste_delay(&json!(10), 10));
        assert!(!valid_paste_delay(&json!(11), 10));
        assert!(!valid_paste_delay(&json!(1.5), 10));
        assert!(!valid_paste_delay(&json!("5"), 10));
    }

    #[test]
    fn fills_in_the_defaults_where_a_host_sets_nothing() {
        assert_eq!(
            resolve_paste_prompt_wait(&PastePromptWaitSetting::default()),
            PastePromptWait {
                prompt_pattern: "[>#$]\\s*$".into(),
                timeout_ms: 5000,
                error_pattern: "^%\\s*(Invalid|Incomplete|Ambiguous)".into(),
            }
        );
        assert_eq!(
            resolve_paste_prompt_wait(&PastePromptWaitSetting {
                prompt_pattern: Some("[>#\\]]\\s*$".into()),
                timeout_ms: Some(20_000),
                error_pattern: None,
            }),
            PastePromptWait {
                prompt_pattern: "[>#\\]]\\s*$".into(),
                timeout_ms: 20_000,
                error_pattern: DEFAULT_PASTE_ERROR_PATTERN.into(),
            }
        );
    }

    #[test]
    fn matches_the_usual_prompts_and_rejections_with_the_defaults() {
        let prompt = fancy_regex::Regex::new(DEFAULT_PASTE_PROMPT_PATTERN).unwrap();
        for line in ["R1(config)#", "R1>", "admin@mx1> ", "user@host:~$ "] {
            assert!(prompt.is_match(line).unwrap(), "{line}");
        }
        assert!(!prompt.is_match("Building configuration...").unwrap());
        let error = fancy_regex::Regex::new(DEFAULT_PASTE_ERROR_PATTERN).unwrap();
        for line in [
            "% Invalid input detected at '^' marker.",
            "% Incomplete command.",
            "%Ambiguous command: \"sh\"",
        ] {
            assert!(error.is_match(line).unwrap(), "{line}");
        }
        assert!(!error.is_match("interface % Invalid").unwrap());
    }

    #[test]
    fn leaves_a_bracketed_paste_the_delays_alone() {
        let pacing = PastePacing {
            prompt: Some(resolve_paste_prompt_wait(&PastePromptWaitSetting::default())),
            ..PastePacing::delays(100, 0)
        };
        assert_eq!(paste_pacing_for(&pacing, false), pacing);
        assert_eq!(paste_pacing_for(&pacing, true), PastePacing::delays(100, 0));
        let unpaced = PastePacing {
            line_delay_ms: 0,
            ..pacing
        };
        assert!(!is_paced_paste(&paste_pacing_for(&unpaced, true)));
    }

    #[test]
    fn refuses_a_pattern_that_does_not_compile_is_too_long_or_matches_empty_text() {
        assert_eq!(paste_pattern_problem(DEFAULT_PASTE_PROMPT_PATTERN), None);
        assert!(
            paste_pattern_problem("([")
                .unwrap()
                .starts_with("Invalid regular expression")
        );
        assert_eq!(
            paste_pattern_problem(&"x".repeat(501)).as_deref(),
            Some("Use at most 500 characters.")
        );
        assert_eq!(
            paste_pattern_problem("").as_deref(),
            Some("This pattern matches empty text, so it would match any output.")
        );
        assert!(paste_pattern_problem("#?\\s*$").is_some());
    }

    #[test]
    fn bounds_the_timeout_to_whole_milliseconds_from_100_ms_to_a_minute() {
        assert!(valid_paste_prompt_timeout(&json!(100)));
        assert!(valid_paste_prompt_timeout(&json!(60_000)));
        assert!(!valid_paste_prompt_timeout(&json!(99)));
        assert!(!valid_paste_prompt_timeout(&json!(60_001)));
        assert!(!valid_paste_prompt_timeout(&json!(1500.5)));
        assert!(!valid_paste_prompt_timeout(&json!("5000")));
    }

    #[test]
    fn checks_a_prompt_wait_read_from_a_backup() {
        assert!(is_paste_prompt_wait(
            &json!({ "promptPattern": "[>#\\]]\\s*$", "timeoutMs": 20_000 })
        ));
        assert!(is_paste_prompt_wait(&json!({})));
        assert!(!is_paste_prompt_wait(&json!({ "promptPattern": "([" })));
        assert!(!is_paste_prompt_wait(&json!({ "errorPattern": "" })));
        assert!(!is_paste_prompt_wait(&json!({ "timeoutMs": 10 })));
        assert!(!is_paste_prompt_wait(&json!([])));
        assert!(!is_paste_prompt_wait(&json!(null)));
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

    #[test]
    fn takes_prompt_waiting_as_an_optional_part_of_a_paste() {
        let paste = json!({ "op": "paste", "text": "x\n", "bracketed": false, "lineDelayMs": 0, "charDelayMs": 0 });
        let prompt =
            serde_json::to_value(resolve_paste_prompt_wait(&PastePromptWaitSetting::default()))
                .unwrap();
        let with_prompt = |prompt: serde_json::Value| {
            let mut body = paste.clone();
            body["prompt"] = prompt;
            body
        };
        // A paste from a client that knows nothing of it still parses, without it.
        let parsed = parse_terminal_client_message(paste.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), paste);
        let parsed = parse_terminal_client_message(with_prompt(prompt.clone())).unwrap();
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            with_prompt(prompt.clone())
        );
        let mut bad = Vec::new();
        for (key, value) in [
            ("timeoutMs", json!(0)),
            ("timeoutMs", json!(60_001)),
            ("promptPattern", json!("")),
            ("errorPattern", json!("x".repeat(501))),
        ] {
            let mut changed = prompt.clone();
            changed[key] = value;
            bad.push(changed);
        }
        bad.push(json!({ "promptPattern": prompt["promptPattern"] }));
        for prompt in bad {
            assert!(
                parse_terminal_client_message(with_prompt(prompt.clone())).is_err(),
                "{prompt}"
            );
        }
    }

    #[test]
    fn accepts_the_hosts_enter_key_by_name_only() {
        let paste = json!({ "op": "paste", "text": "x", "bracketed": false, "lineDelayMs": 100, "charDelayMs": 0 });
        let with_key = |key: &str| {
            let mut body = paste.clone();
            body["enterKey"] = json!(key);
            parse_terminal_client_message(body)
        };
        assert!(with_key("crlf").is_ok());
        assert!(with_key("lf").is_ok());
        assert!(with_key("\n").is_err());
    }
}
