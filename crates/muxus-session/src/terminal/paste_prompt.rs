//! Reading the device's answer to each line of a paste that waits for the
//! prompt (`server/src/ws/paste-prompt.ts`).

use std::time::{Duration, Instant};

use muxus_core::js::{js_trim, utf16_len, utf16_prefix};

use crate::login_sequence::TerminalTextStream;
use crate::login_sequence::matcher::{LOGIN_OUTPUT_WINDOW, LOGIN_PATTERN_BACKTRACK_LIMIT};

/// How much of one line's answer is read for an error; later output is only searched for the
/// prompt.
const ANSWER_ERROR_LIMIT: usize = 64 * 1024;
/// Longest line, sent or received, a stopped paste quotes.
const REPORTED_LINE_MAX: usize = 200;
/// How long the regex runs of one check may take together (the login
/// sequence's limit for one match).
pub const PASTE_PATTERN_TIME_LIMIT: Duration = Duration::from_millis(100);

/// What the output read so far says about the line sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptWatchResult {
    Prompt,
    /// An output line matched the error pattern.
    Error {
        line: String,
    },
    /// A pattern could not be run to the end, such as one past its effort
    /// limit; it must not run again.
    Failed {
        message: String,
    },
}

/// Reads the output that answers the line a paced paste just sent, the way a
/// login sequence reads output: escape sequences stripped, line endings
/// reduced to `\n`, chunks joined. Every complete line is checked once
/// against the error pattern, a line at a time (`^` and `$` mark its ends).
/// The prompt is looked for on the unfinished last line, and only once the
/// device has started a new line, so the echo of the sent line never passes
/// for the prompt. Each regex run covers at most the login sequence's window,
/// is bounded in effort, and the runs of one check are abandoned past its
/// time limit.
pub struct PastePromptWatch {
    stream: TerminalTextStream,
    prompt: fancy_regex::Regex,
    error: fancy_regex::Regex,
    /// Complete lines not checked for an error yet.
    unchecked: String,
    /// Characters (UTF-16 code units) of complete lines taken in since the line was sent.
    taken: usize,
    /// The unfinished last line, where a prompt waits.
    current: String,
    new_line: bool,
    time_limit: Duration,
}

fn compile(pattern: &str, multi_line: bool) -> Result<fancy_regex::Regex, String> {
    let source = if multi_line {
        format!("(?m){pattern}")
    } else {
        pattern.to_string()
    };
    fancy_regex::RegexBuilder::new(&source)
        .backtrack_limit(LOGIN_PATTERN_BACKTRACK_LIMIT)
        .build()
        .map_err(|error| error.to_string())
}

impl PastePromptWatch {
    /// `Err` with the reason for a pattern that does not compile.
    pub fn new(prompt_pattern: &str, error_pattern: &str) -> Result<Self, String> {
        Self::with_time_limit(prompt_pattern, error_pattern, PASTE_PATTERN_TIME_LIMIT)
    }

    pub fn with_time_limit(
        prompt_pattern: &str,
        error_pattern: &str,
        time_limit: Duration,
    ) -> Result<Self, String> {
        Ok(Self {
            stream: TerminalTextStream::new(),
            prompt: compile(prompt_pattern, false)?,
            error: compile(error_pattern, true)?,
            unchecked: String::new(),
            taken: 0,
            current: String::new(),
            new_line: false,
            time_limit,
        })
    }

    /// Output from here on answers the line about to be sent; anything earlier is forgotten.
    pub fn reset(&mut self) {
        self.unchecked.clear();
        self.taken = 0;
        self.current.clear();
        self.new_line = false;
    }

    pub fn append(&mut self, data: &[u8]) {
        let text = self.stream.write(data);
        if text.is_empty() {
            return;
        }
        let Some(last_break) = text.rfind('\n') else {
            self.current.push_str(&text);
            self.current = tail(&self.current).to_string();
            return;
        };
        self.new_line = true;
        if self.taken < ANSWER_ERROR_LIMIT {
            // Cut to the limit before it is kept, so one large chunk cannot carry past it.
            let joined = format!("{}{}", self.current, &text[..=last_break]);
            let lines = utf16_prefix(&joined, ANSWER_ERROR_LIMIT - self.taken);
            self.unchecked.push_str(lines);
            self.taken += utf16_len(lines);
        }
        self.current = tail(&text[last_break + 1..]).to_string();
    }

    /// An error line in the answer so far, or the prompt when `prompt` is set;
    /// `None` while there is neither. Every regex run of one check shares the
    /// time limit, so a check holds up the backend no longer than one login
    /// sequence match.
    pub fn check(&mut self, prompt: bool) -> Option<PromptWatchResult> {
        let deadline = Instant::now() + self.time_limit;
        let too_long = |pattern: &str| PromptWatchResult::Failed {
            message: format!("the {pattern} pattern took too long to match"),
        };
        let failed = |pattern: &str, error: fancy_regex::Error| match error {
            fancy_regex::Error::RuntimeError(fancy_regex::RuntimeError::BacktrackLimitExceeded) => {
                too_long(pattern)
            }
            other => PromptWatchResult::Failed {
                message: format!("the {pattern} pattern could not be matched: {other}"),
            },
        };
        while !self.unchecked.is_empty() {
            if Instant::now() > deadline {
                return Some(too_long("error"));
            }
            let piece = leading_lines(&self.unchecked).to_string();
            self.unchecked.drain(..piece.len());
            match self.error.find(&piece) {
                Ok(Some(found)) => {
                    return Some(PromptWatchResult::Error {
                        line: matched_line(&piece, found.start(), found.end()),
                    });
                }
                Ok(None) => {}
                Err(error) => return Some(failed("error", error)),
            }
        }
        if !prompt || !self.new_line {
            return None;
        }
        if Instant::now() > deadline {
            return Some(too_long("prompt"));
        }
        match self.prompt.is_match(&self.current) {
            Ok(true) => Some(PromptWatchResult::Prompt),
            Ok(false) => None,
            Err(error) => Some(failed("prompt", error)),
        }
    }
}

/// The newest [`LOGIN_OUTPUT_WINDOW`] code units of `text`.
fn tail(text: &str) -> &str {
    let units = utf16_len(text);
    if units <= LOGIN_OUTPUT_WINDOW {
        return text;
    }
    let mut skip = units - LOGIN_OUTPUT_WINDOW;
    for (index, c) in text.char_indices() {
        if skip == 0 {
            return &text[index..];
        }
        skip = skip.saturating_sub(c.len_utf16());
    }
    ""
}

/// The whole lines that fit one regex run, or a window's worth of a longer line.
fn leading_lines(text: &str) -> &str {
    if utf16_len(text) <= LOGIN_OUTPUT_WINDOW {
        return text;
    }
    let window = utf16_prefix(text, LOGIN_OUTPUT_WINDOW);
    match window.rfind('\n') {
        Some(last_break) => &text[..=last_break],
        None => window,
    }
}

/// The output line a match is on, trimmed and shortened for a notice.
fn matched_line(text: &str, start: usize, end: usize) -> String {
    let bytes = text.as_bytes();
    let mut index = start;
    // A match may start on the line break before the line it is about.
    while bytes.get(index) == Some(&b'\n') && index + 1 < end {
        index += 1;
    }
    let line_start = text[..index].rfind('\n').map_or(0, |at| at + 1);
    let line_end = text[index..].find('\n').map_or(text.len(), |at| index + at);
    shortened(js_trim(&text[line_start..line_end]))
}

/// A pasted line as a stop notice quotes it: as the device shows it, trimmed and shortened.
pub fn pasted_line_text(data: &str) -> String {
    let shown = TerminalTextStream::new().write(data.as_bytes());
    shortened(js_trim(&shown))
}

fn shortened(line: &str) -> String {
    if utf16_len(line) > REPORTED_LINE_MAX {
        format!("{}…", utf16_prefix(line, REPORTED_LINE_MAX - 1))
    } else {
        line.to_string()
    }
}

/// Port of `tests/unit/server/paste-prompt.test.ts`.
#[cfg(test)]
mod tests {
    use muxus_core::paste_pacing::{DEFAULT_PASTE_ERROR_PATTERN, DEFAULT_PASTE_PROMPT_PATTERN};

    use super::*;

    fn defaults() -> PastePromptWatch {
        PastePromptWatch::new(DEFAULT_PASTE_PROMPT_PATTERN, DEFAULT_PASTE_ERROR_PATTERN).unwrap()
    }

    fn watching(chunks: &[&str]) -> PastePromptWatch {
        let mut watch = defaults();
        for chunk in chunks {
            watch.append(chunk.as_bytes());
        }
        watch
    }

    fn error(line: &str) -> Option<PromptWatchResult> {
        Some(PromptWatchResult::Error { line: line.into() })
    }

    fn failed(message: &str) -> Option<PromptWatchResult> {
        Some(PromptWatchResult::Failed {
            message: message.into(),
        })
    }

    #[test]
    fn finds_the_prompt_on_the_line_the_device_starts_after_the_echo() {
        let mut watch = defaults();
        watch.append(b"description uplink to core#");
        // The echo of a line ending in # is not the prompt.
        assert_eq!(watch.check(true), None);
        watch.append(b"\r\n");
        assert_eq!(watch.check(true), None);
        watch.append(b"R1(config-if)# ");
        assert_eq!(watch.check(true), Some(PromptWatchResult::Prompt));
        // Shell and Junos prompts too.
        assert_eq!(
            watching(&["ls\r\n", "file\r\nuser@host:~$ "]).check(true),
            Some(PromptWatchResult::Prompt)
        );
        assert_eq!(
            watching(&["show\r\n", "{master}\r\nadmin@mx> "]).check(true),
            Some(PromptWatchResult::Prompt)
        );
    }

    #[test]
    fn reports_the_output_line_an_error_pattern_matches_a_line_at_a_time() {
        let mut watch = watching(&[
            "interface bogus\r\n",
            "                ^\r\n% Invalid input detected at '^' marker.\r\n\r\nR1(config)#",
        ]);
        assert_eq!(
            watch.check(true),
            error("% Invalid input detected at '^' marker.")
        );
        // ^ is the start of a line, not of the output.
        assert_eq!(
            watching(&["x\r\nnot % Invalid\r\nR1#"]).check(true),
            Some(PromptWatchResult::Prompt)
        );
        assert!(matches!(
            watching(&["x\r\n% Incomplete command.\r\n"]).check(true),
            Some(PromptWatchResult::Error { .. })
        ));
    }

    #[test]
    fn reads_through_escape_sequences_carriage_returns_and_chunk_boundaries() {
        let mut watch = watching(&[
            "show\r",
            "\n\x1b[31m% Ambig",
            "uous comm\x1b[0",
            "mand: \"sh\"\x1b[K\r",
            "\n",
        ]);
        assert_eq!(watch.check(true), error("% Ambiguous command: \"sh\""));
        assert_eq!(
            watching(&["x\r\n\x1b]0;R1\x07R1\x1b[1", "m#\x1b[0m"]).check(true),
            Some(PromptWatchResult::Prompt)
        );
    }

    #[test]
    fn forgets_what_came_before_the_line_it_answers() {
        let mut watch = watching(&["% Invalid input\r\nR1#"]);
        watch.reset();
        assert_eq!(watch.check(true), None);
        watch.append(b"next\r\nR1#");
        assert_eq!(watch.check(true), Some(PromptWatchResult::Prompt));
    }

    #[test]
    fn checks_every_line_of_a_long_answer_once_and_only_so_much_of_it() {
        let mut watch = defaults();
        watch.append(
            format!(
                "{}% Invalid input\r\n",
                "interface Ethernet1\r\n".repeat(1000)
            )
            .as_bytes(),
        );
        assert_eq!(watch.check(true), error("% Invalid input"));
        assert_eq!(watch.check(true), None);
        // An error early in an answer that takes several regex runs.
        let answer = format!(
            "x\r\n% Invalid input\r\n{}R1#",
            "interface Ethernet1\r\n".repeat(1000)
        );
        assert_eq!(watching(&[&answer]).check(true), error("% Invalid input"));

        // Past the first 64 KiB of an answer, output is only searched for the prompt.
        let mut flood = defaults();
        flood.append(format!("{}\r\n", "x".repeat(99)).repeat(700).as_bytes());
        flood.append(b"% Invalid input\r\nR1#");
        assert_eq!(flood.check(true), Some(PromptWatchResult::Prompt));
        // Also when the error comes in the same chunk as the first 64 KiB.
        let chunk = format!(
            "{}% Invalid input\r\nR1#",
            format!("{}\r\n", "x".repeat(99)).repeat(700)
        );
        assert_eq!(
            watching(&[&chunk]).check(true),
            Some(PromptWatchResult::Prompt)
        );
    }

    // The TypeScript stops a run after a time limit. Here a plain pattern runs on a linear-time
    // engine and cannot backtrack at all; a pattern that needs backtracking (a lookaround) gives
    // up at the effort limit instead.
    #[test]
    fn looks_for_the_prompt_only_when_asked_to_and_keeps_checking_errors() {
        let mut watch = PastePromptWatch::new("^((?=a)a+)+$", DEFAULT_PASTE_ERROR_PATTERN).unwrap();
        watch.append(format!("x\r\n{}!", "a".repeat(40)).as_bytes());
        assert_eq!(watch.check(false), None);
        assert_eq!(
            watch.check(true),
            failed("the prompt pattern took too long to match")
        );
        assert_eq!(watching(&["x\r\nR1#"]).check(false), None);
        assert!(matches!(
            watching(&["x\r\n% Invalid input\r\nR1#"]).check(false),
            Some(PromptWatchResult::Error { .. })
        ));
    }

    #[test]
    fn spends_no_more_than_the_time_limit_on_one_check_over_every_regex_run() {
        // Backtracks a while on every line, without passing the effort limit in one run.
        let error_pattern = "^((?=x)x+x+)+y";
        let window = format!("{}\r\n", "x".repeat(15)).repeat(480);
        let mut probe = PastePromptWatch::with_time_limit(
            DEFAULT_PASTE_PROMPT_PATTERN,
            error_pattern,
            Duration::from_secs(10),
        )
        .unwrap();
        probe.append(window.as_bytes());
        let started = Instant::now();
        let _ = probe.check(true);
        let limit = started.elapsed().mul_f64(2.5).max(Duration::from_millis(1));

        let mut watch =
            PastePromptWatch::with_time_limit(DEFAULT_PASTE_PROMPT_PATTERN, error_pattern, limit)
                .unwrap();
        watch.append(window.repeat(8).as_bytes());
        let started = Instant::now();
        assert_eq!(
            watch.check(true),
            failed("the error pattern took too long to match")
        );
        assert!(started.elapsed() < limit * 2 + Duration::from_millis(50));
    }

    #[test]
    fn abandons_a_pattern_that_backtracks_too_long_instead_of_stalling() {
        let mut watch =
            PastePromptWatch::new(DEFAULT_PASTE_PROMPT_PATTERN, "^((?=a)a+)+$").unwrap();
        watch.append(format!("{}!\r\n", "a".repeat(40)).as_bytes());
        let started = Instant::now();
        assert_eq!(
            watch.check(true),
            failed("the error pattern took too long to match")
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn refuses_a_pattern_that_does_not_compile() {
        assert!(PastePromptWatch::new("([", DEFAULT_PASTE_ERROR_PATTERN).is_err());
    }

    #[test]
    fn quotes_a_pasted_line_as_the_device_shows_it_trimmed_and_shortened() {
        assert_eq!(
            pasted_line_text("  switchport mode acess\r"),
            "switchport mode acess"
        );
        assert_eq!(
            pasted_line_text("\x1b[31mdescription\x1b[0m uplink\x07\r"),
            "description uplink"
        );
        assert_eq!(
            pasted_line_text(&format!("description {}\r", "a".repeat(300))),
            format!("description {}…", "a".repeat(187))
        );
    }
}
