//! OSC 7501, the Program Status Protocol (port of `client/src/terminal/program-status.ts`): a
//! program in the terminal reports what it is doing (working, blocked on the user, done, failed)
//! so the terminal can show it without reading the screen.
//!
//! ```text
//! ESC ] 7501 ; state=working:app=make:msg=<base64> ST
//! ```
//!
//! Every report replaces one record, addressed by an optional `/`-separated id. Everything in a
//! report is untrusted program output.
//!
//! The emulator parses the sequences ([`parse_program_status`]) and answers the support query
//! inline, so the reply keeps its place among the other answers; the records live with the app,
//! keyed by tab, so a replaced view keeps them.

use base64::Engine;
use base64::alphabet::STANDARD;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

pub const PROGRAM_STATUS_OSC: u32 = 7501;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgramState {
    Idle,
    Working,
    Blocked,
    Done,
    Error,
}

impl ProgramState {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "idle" => Self::Idle,
            "working" => Self::Working,
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            "error" => Self::Error,
            _ => return None,
        })
    }

    /// Done and error: results that stay until the tab is looked at.
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Error)
    }

    fn priority(self) -> u8 {
        match self {
            Self::Blocked => 4,
            Self::Error => 3,
            Self::Working => 2,
            Self::Done => 1,
            Self::Idle => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgramBlockedKind {
    Permission,
    Question,
    Auth,
}

impl ProgramBlockedKind {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "permission" => Self::Permission,
            "question" => Self::Question,
            "auth" => Self::Auth,
            _ => return None,
        })
    }
}

/// One program's latest report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatusRecord {
    /// Empty for the root record.
    pub id: String,
    pub state: ProgramState,
    pub kind: Option<ProgramBlockedKind>,
    /// 0–100 while working or blocked; `None` means indeterminate.
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
    /// Monotonic update order, for eviction and tie-breaking (0 until applied).
    pub seq: u64,
}

impl ProgramStatusRecord {
    /// A record with only an id and a state.
    pub fn new(id: impl Into<String>, state: ProgramState) -> Self {
        Self {
            id: id.into(),
            state,
            kind: None,
            progress: None,
            app: None,
            title: None,
            msg: None,
            seq: 0,
        }
    }
}

/// One parsed OSC 7501 sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramStatusReport {
    /// `OSC 7501 ; ?`: which states does this terminal show?
    Query,
    /// Drop the record `id` and everything below it.
    Clear { id: String },
    /// Replace one record (its `seq` is set when it is applied).
    Report(ProgramStatusRecord),
}

/// What the emulator tells its owner about programs' status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramStatusSignal {
    /// A clear or a report (the support query is answered by the emulator itself).
    Report(ProgramStatusReport),
    /// A new shell prompt (OSC 133/633 `A`): the foreground program ended.
    Prompt,
    /// A full terminal reset (RIS): nothing reported before applies.
    Reset,
}

/// The reply to `OSC 7501 ; ?`: the states and blocked kinds this terminal shows.
pub const PROGRAM_STATUS_QUERY_REPLY: &str =
    "\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";

pub const MAX_PROGRAM_STATUS_RECORDS: usize = 256;
const MAX_SEQUENCE_BYTES: usize = 4096;
const MAX_MSG_BYTES: usize = 2048;
const MAX_TITLE_BYTES: usize = 192;
const MAX_ID_LENGTH: usize = 128;
const MAX_ID_DEPTH: usize = 8;

/// `^[A-Za-z0-9_.,+/=-]*$`
fn valid_value(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"_.,+/=-".contains(&byte))
}

/// `^[A-Za-z0-9_.+-]{1,32}$`, the id segments and the app name.
fn valid_name(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.+-".contains(&byte))
}

fn valid_id(id: &str) -> bool {
    if id.len() > MAX_ID_LENGTH {
        return false;
    }
    let segments: Vec<&str> = id.split('/').collect();
    segments.len() <= MAX_ID_DEPTH && segments.iter().all(|segment| valid_name(segment))
}

/// `^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}(?:==)?|[A-Za-z0-9+/]{3}=?)?$`: padding is
/// optional but, when present, complete.
fn valid_base64(value: &str) -> bool {
    let body = value.trim_end_matches('=');
    let padding = value.len() - body.len();
    if !body
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
    {
        return false;
    }
    matches!((body.len() % 4, padding), (0, 0) | (2, 0 | 2) | (3, 0 | 1))
}

/// `atob`: padding optional, stray low bits ignored (forgiving-base64).
const FORGIVING_BASE64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// C0, DEL and C1 controls: never allowed in decoded text.
fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
}

/// Bidi overrides and invisible characters that could disguise the text.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
    )
}

/// Decode a base64 text value, or `None` when the whole report must be dropped.
fn decode_text(value: &str, max_bytes: usize) -> Option<String> {
    if !valid_base64(value) {
        return None;
    }
    let bytes = FORGIVING_BASE64.decode(value).ok()?;
    if bytes.len() > max_bytes {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if text.chars().any(is_control) {
        return None;
    }
    Some(text.chars().filter(|c| !is_invisible(*c)).collect())
}

/// Parse one OSC 7501 payload (the part after `7501;`).
pub fn parse_program_status(data: &str) -> Option<ProgramStatusReport> {
    if data.len() > MAX_SEQUENCE_BYTES {
        return None;
    }
    if data == "?" || data.starts_with("?:") {
        return Some(ProgramStatusReport::Query);
    }

    // Malformed pairs are skipped; a repeated key keeps its last value.
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    for pair in data.split(':') {
        let Some(separator) = pair.find('=').filter(|separator| *separator > 0) else {
            continue;
        };
        let (key, value) = (&pair[..separator], &pair[separator + 1..]);
        if !valid_value(value) {
            continue;
        }
        pairs.retain(|(existing, _)| *existing != key);
        pairs.push((key, value));
    }
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(existing, _)| *existing == key)
            .map(|(_, value)| *value)
    };

    let state = get("state")?;
    let id = get("id").unwrap_or("");
    if !id.is_empty() && !valid_id(id) {
        return None;
    }
    if state == "clear" {
        return Some(ProgramStatusReport::Clear { id: id.to_string() });
    }
    let state = ProgramState::parse(state)?;

    let mut record = ProgramStatusRecord::new(id, state);
    if state == ProgramState::Blocked {
        record.kind = get("kind").and_then(ProgramBlockedKind::parse);
    }
    if matches!(state, ProgramState::Working | ProgramState::Blocked)
        && let Some(progress) = get("progress")
        && (1..=3).contains(&progress.len())
        && progress.bytes().all(|byte| byte.is_ascii_digit())
        && let Ok(value) = progress.parse::<u8>()
        && value <= 100
    {
        record.progress = Some(value);
    }
    if let Some(app) = get("app")
        && valid_name(app)
    {
        record.app = Some(app.to_string());
    }
    for (key, max_bytes) in [("title", MAX_TITLE_BYTES), ("msg", MAX_MSG_BYTES)] {
        let Some(value) = get(key) else { continue };
        let text = decode_text(value, max_bytes)?;
        if !text.is_empty() {
            match key {
                "title" => record.title = Some(text),
                _ => record.msg = Some(text),
            }
        }
    }
    Some(ProgramStatusReport::Report(record))
}

fn is_within(id: &str, ancestor: &str) -> bool {
    ancestor.is_empty()
        || id == ancestor
        || id
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Apply a clear or a report to one terminal's records. A query changes nothing.
pub fn apply_program_status(
    records: &mut Vec<ProgramStatusRecord>,
    report: &ProgramStatusReport,
    seq: u64,
) {
    match report {
        ProgramStatusReport::Query => {}
        ProgramStatusReport::Clear { id } => records.retain(|record| !is_within(&record.id, id)),
        ProgramStatusReport::Report(record) => {
            records.retain(|existing| existing.id != record.id);
            records.push(ProgramStatusRecord {
                seq,
                ..record.clone()
            });
            if records.len() > MAX_PROGRAM_STATUS_RECORDS {
                // Evict the least recently updated record.
                if let Some(oldest) = records
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, record)| record.seq)
                    .map(|(index, _)| index)
                {
                    records.remove(oldest);
                }
            }
        }
    }
}

/// Working and blocked last only as long as the program: a new shell prompt or the end of the
/// session drops them. Done and error stay until replaced. Returns whether anything went.
pub fn drop_transient_program_status(records: &mut Vec<ProgramStatusRecord>) -> bool {
    let before = records.len();
    records.retain(|record| !matches!(record.state, ProgramState::Working | ProgramState::Blocked));
    records.len() != before
}

/// The one record a tab shows: the most urgent state, the latest report among equals. Done and
/// error count only while they are unseen. Idle shows nothing. The app name is inherited from
/// the nearest ancestor that set one.
pub fn summarize_program_status(
    records: &[ProgramStatusRecord],
    include_finished: bool,
) -> Option<ProgramStatusRecord> {
    let mut best: Option<&ProgramStatusRecord> = None;
    for record in records {
        if record.state == ProgramState::Idle || (!include_finished && record.state.is_finished()) {
            continue;
        }
        let better = match best {
            None => true,
            Some(best) => {
                record.state.priority() > best.state.priority()
                    || (record.state.priority() == best.state.priority() && record.seq > best.seq)
            }
        };
        if better {
            best = Some(record);
        }
    }
    let best = best?;
    if best.app.is_some() {
        return Some(best.clone());
    }
    let mut id = best.id.as_str();
    while !id.is_empty() {
        id = id.rfind('/').map_or("", |separator| &id[..separator]);
        if let Some(app) = records
            .iter()
            .find(|record| record.id == id)
            .and_then(|record| record.app.as_ref())
        {
            return Some(ProgramStatusRecord {
                app: Some(app.clone()),
                ..best.clone()
            });
        }
    }
    Some(best.clone())
}

/// The headline a tab's status reads as.
pub fn program_status_label(
    state: ProgramState,
    kind: Option<ProgramBlockedKind>,
    progress: Option<u8>,
) -> String {
    match state {
        ProgramState::Working => match progress {
            None => "Working".into(),
            Some(progress) => format!("Working, {progress}%"),
        },
        ProgramState::Blocked => match kind {
            Some(ProgramBlockedKind::Permission) => "Waiting for your permission".into(),
            Some(ProgramBlockedKind::Question) => "Waiting for your answer".into(),
            Some(ProgramBlockedKind::Auth) => "Waiting for you to sign in".into(),
            None => "Waiting for you".into(),
        },
        ProgramState::Done => "Done".into(),
        ProgramState::Error => "Failed".into(),
        ProgramState::Idle => "Idle".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    fn report(data: &str) -> ProgramStatusRecord {
        match parse_program_status(data) {
            Some(ProgramStatusReport::Report(record)) => record,
            other => panic!("not a report: {data}: {other:?}"),
        }
    }

    fn records(lines: &[&str]) -> Vec<ProgramStatusRecord> {
        let mut result = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let parsed = parse_program_status(line).expect(line);
            assert_ne!(parsed, ProgramStatusReport::Query, "{line}");
            apply_program_status(&mut result, &parsed, index as u64 + 1);
        }
        result
    }

    fn ids(records: &[ProgramStatusRecord]) -> Vec<&str> {
        records.iter().map(|record| record.id.as_str()).collect()
    }

    // ---- parsing OSC 7501 reports ----

    #[test]
    fn reads_a_full_report_with_base64_text() {
        let record = report(&format!(
            "state=working:app=make:progress=40:title={}:msg={}",
            b64("Build"),
            b64("Linking ✓")
        ));
        assert_eq!(
            record,
            ProgramStatusRecord {
                progress: Some(40),
                app: Some("make".into()),
                title: Some("Build".into()),
                msg: Some("Linking ✓".into()),
                ..ProgramStatusRecord::new("", ProgramState::Working)
            }
        );
    }

    #[test]
    fn recognises_the_support_query() {
        assert_eq!(parse_program_status("?"), Some(ProgramStatusReport::Query));
        assert_eq!(
            parse_program_status("?:states=x"),
            Some(ProgramStatusReport::Query)
        );
    }

    #[test]
    fn requires_a_known_state() {
        assert_eq!(parse_program_status("app=make"), None);
        assert_eq!(parse_program_status("state=sleeping"), None);
    }

    #[test]
    fn addresses_clears_by_id() {
        assert_eq!(
            parse_program_status("state=clear:id=build/test"),
            Some(ProgramStatusReport::Clear {
                id: "build/test".into()
            })
        );
        assert_eq!(
            parse_program_status("state=clear"),
            Some(ProgramStatusReport::Clear { id: String::new() })
        );
    }

    #[test]
    fn skips_malformed_pairs_ignores_unknown_keys_and_keeps_the_last_repeat() {
        assert_eq!(
            report("state=idle:noise:future=1:app=a:app=b:state=done:app=bad app"),
            ProgramStatusRecord {
                app: Some("b".into()),
                ..ProgramStatusRecord::new("", ProgramState::Done)
            }
        );
    }

    #[test]
    fn keeps_kind_only_for_blocked_and_progress_only_while_working_or_blocked() {
        let blocked = report("state=blocked:kind=permission:progress=10");
        assert_eq!(blocked.kind, Some(ProgramBlockedKind::Permission));
        assert_eq!(blocked.progress, Some(10));
        assert_eq!(report("state=blocked:kind=coffee").kind, None);
        assert_eq!(
            report("state=done:kind=question:progress=50"),
            ProgramStatusRecord::new("", ProgramState::Done)
        );
        assert_eq!(report("state=working:progress=101").progress, None);
        assert_eq!(report("state=working:progress=0100").progress, None);
    }

    #[test]
    fn discards_a_report_whose_text_is_invalid_or_carries_control_characters() {
        assert_eq!(parse_program_status("state=done:msg=abcde"), None);
        assert_eq!(
            parse_program_status("state=done:msg=@@@"),
            Some(ProgramStatusReport::Report(ProgramStatusRecord::new(
                "",
                ProgramState::Done
            )))
        );
        assert_eq!(
            parse_program_status(&format!("state=done:msg={}", b64("a\x1b[31mred"))),
            None
        );
        assert_eq!(
            parse_program_status(&format!("state=done:title={}", b64("a\u{85}b"))),
            None
        );
        let invalid_utf8 = base64::engine::general_purpose::STANDARD.encode([0xff, 0xfe]);
        assert_eq!(
            parse_program_status(&format!("state=done:msg={invalid_utf8}")),
            None
        );
    }

    #[test]
    fn decodes_base64_the_way_atob_does() {
        // Unpadded and padded spellings, and stray low bits, decode alike.
        assert_eq!(report("state=done:msg=aGk").msg.as_deref(), Some("hi"));
        assert_eq!(report("state=done:msg=aGk=").msg.as_deref(), Some("hi"));
        assert_eq!(report("state=done:msg=aGl").msg.as_deref(), Some("hi"));
        assert_eq!(report("state=done:msg=aA==").msg.as_deref(), Some("h"));
        // Too much padding is not base64.
        assert_eq!(parse_program_status("state=done:msg=aGk=="), None);
        assert_eq!(parse_program_status("state=done:msg=a"), None);
    }

    #[test]
    fn strips_bidi_overrides_and_invisible_characters_from_text() {
        assert_eq!(
            report(&format!(
                "state=done:msg={}",
                b64("safe\u{202e}txt.exe\u{200b}")
            ))
            .msg
            .as_deref(),
            Some("safetxt.exe")
        );
    }

    #[test]
    fn enforces_the_size_limits() {
        assert_eq!(
            parse_program_status(&format!("state=done:msg={}", b64(&"x".repeat(2049)))),
            None
        );
        assert_eq!(
            parse_program_status(&format!("state=done:title={}", b64(&"x".repeat(193)))),
            None
        );
        assert_eq!(
            parse_program_status(&format!("state=done:id={}", ["a"; 9].join("/"))),
            None
        );
        assert_eq!(
            parse_program_status(&format!("state=done:id={}", "a".repeat(33))),
            None
        );
        assert_eq!(
            parse_program_status(&format!("state=done:app=x:{}", "z".repeat(4096))),
            None
        );
    }

    // ---- program status records ----

    #[test]
    fn replaces_a_record_completely_on_each_report() {
        let result = records(&[
            &format!("state=working:app=make:msg={}", b64("one")),
            "state=done",
        ]);
        assert_eq!(
            result,
            vec![ProgramStatusRecord {
                seq: 2,
                ..ProgramStatusRecord::new("", ProgramState::Done)
            }]
        );
    }

    #[test]
    fn clears_a_record_together_with_its_children() {
        let mut result = records(&[
            "state=working:id=build",
            "state=working:id=build/test",
            "state=working:id=builder",
            "state=clear:id=build",
        ]);
        assert_eq!(ids(&result), ["builder"]);
        apply_program_status(
            &mut result,
            &ProgramStatusReport::Clear { id: String::new() },
            9,
        );
        assert!(result.is_empty());
    }

    #[test]
    fn evicts_the_least_recently_updated_record_when_full() {
        let mut result = Vec::new();
        for index in 0..=MAX_PROGRAM_STATUS_RECORDS {
            apply_program_status(
                &mut result,
                &ProgramStatusReport::Report(ProgramStatusRecord::new(
                    format!("r{index}"),
                    ProgramState::Idle,
                )),
                index as u64 + 1,
            );
        }
        assert_eq!(result.len(), MAX_PROGRAM_STATUS_RECORDS);
        assert!(!result.iter().any(|record| record.id == "r0"));
    }

    #[test]
    fn drops_working_and_blocked_but_keeps_finished_results() {
        let mut result = records(&[
            "state=working:id=a",
            "state=blocked:id=b",
            "state=done:id=c",
            "state=idle:id=d",
        ]);
        assert!(drop_transient_program_status(&mut result));
        assert_eq!(ids(&result), ["c", "d"]);
        let mut settled = records(&["state=done"]);
        assert!(!drop_transient_program_status(&mut settled));
        assert_eq!(settled.len(), 1);
    }

    // ---- summarizing a tab ----

    #[test]
    fn shows_the_most_urgent_state() {
        let result = records(&[
            "state=working:id=a",
            "state=blocked:id=b:kind=question",
            "state=error:id=c",
        ]);
        assert_eq!(summarize_program_status(&result, true).unwrap().id, "b");
    }

    #[test]
    fn shows_finished_results_only_while_unseen() {
        let result = records(&["state=done:id=a", "state=working:id=b", "state=error:id=c"]);
        assert_eq!(summarize_program_status(&result, true).unwrap().id, "c");
        assert_eq!(summarize_program_status(&result, false).unwrap().id, "b");
        assert_eq!(
            summarize_program_status(&records(&["state=done"]), false),
            None
        );
    }

    #[test]
    fn shows_nothing_for_idle_programs() {
        assert_eq!(
            summarize_program_status(&records(&["state=idle"]), true),
            None
        );
    }

    #[test]
    fn prefers_the_latest_of_equally_urgent_records() {
        let result = records(&["state=working:id=a", "state=working:id=b"]);
        assert_eq!(summarize_program_status(&result, true).unwrap().id, "b");
    }

    #[test]
    fn inherits_the_app_name_from_an_ancestor() {
        let result = records(&["state=idle:app=agent", "state=working:id=helper/one"]);
        let summary = summarize_program_status(&result, true).unwrap();
        assert_eq!(summary.id, "helper/one");
        assert_eq!(summary.app.as_deref(), Some("agent"));
    }

    #[test]
    fn labels_each_state() {
        use ProgramState::*;
        assert_eq!(program_status_label(Working, None, None), "Working");
        assert_eq!(
            program_status_label(Working, None, Some(30)),
            "Working, 30%"
        );
        assert_eq!(
            program_status_label(Blocked, Some(ProgramBlockedKind::Auth), None),
            "Waiting for you to sign in"
        );
        assert_eq!(program_status_label(Blocked, None, None), "Waiting for you");
        assert_eq!(program_status_label(Error, None, None), "Failed");
    }

    #[test]
    fn the_query_reply_lists_what_muxus_shows() {
        assert_eq!(
            PROGRAM_STATUS_QUERY_REPLY,
            "\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\"
        );
    }
}
