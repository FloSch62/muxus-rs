//! Duplicate a host into a numbered range (`host-editor/name-range.ts`):
//! a range such as `web[2-10]` in the name of a new or duplicated host creates
//! one host per number on save. Leading zeros keep the width, and the
//! HostName follows the name or takes a range of its own.

use std::collections::HashSet;
use std::sync::LazyLock;

use muxus_core::js::{is_js_whitespace, js_trim};
use regex::Regex;

use super::draft::{HostDraft, Storage};

/// More copies than this in one save is almost certainly a typo.
pub const MAX_RANGE_HOSTS: usize = 250;

/// A numeric range written into a host name, e.g. `web[2-10]` or `db[01-12]`.
#[derive(Debug, Clone, PartialEq)]
pub struct NameRange {
    pub prefix: String,
    pub suffix: String,
    /// The numbers as JavaScript reads them (`Number(digits)`): exact only up to 2^53.
    pub start: f64,
    pub end: f64,
    /// Zero-padded width when the start was written with a leading zero.
    pub width: usize,
}

static RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([0-9]+)\s*(?:-|\.\.|:)\s*([0-9]+)\]").unwrap());
/// Something that looks like a range was meant: a bracket around a digit.
static RANGE_ATTEMPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\s*[0-9]|[0-9]\s*\]").unwrap());

/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.;

pub fn parse_name_range(text: &str) -> Option<NameRange> {
    let captures = RANGE.captures(text)?;
    let whole = captures.get(0)?;
    let from = &captures[1];
    let to = &captures[2];
    Some(NameRange {
        prefix: text[..whole.start()].to_string(),
        suffix: text[whole.end()..].to_string(),
        start: from.parse().unwrap_or(f64::INFINITY),
        end: to.parse().unwrap_or(f64::INFINITY),
        width: if from.len() > 1 && from.starts_with('0') {
            from.len()
        } else {
            0
        },
    })
}

/// Values in the range; only meaningful once it is known to count up.
fn range_size(range: &NameRange) -> f64 {
    range.end - range.start + 1.
}

fn is_safe_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0. && value.abs() <= MAX_SAFE_INTEGER
}

/// Above 2^53 `n++` can stop changing `n`, and digits collapse together.
fn exact(range: &NameRange) -> bool {
    is_safe_integer(range.start) && is_safe_integer(range.end)
}

fn pad(number: u64, width: usize) -> String {
    format!("{number:0width$}")
}

pub fn expand_name_range(range: &NameRange) -> Vec<String> {
    let (start, end) = (range.start as u64, range.end as u64);
    (start..=end)
        .map(|n| format!("{}{}{}", range.prefix, pad(n, range.width), range.suffix))
        .collect()
}

static TRAILING_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.*?)([0-9]+)$").unwrap());

/// The suggestion shown when duplicating: `app1` → `app[2-10]`, `db01` → `db[02-10]`.
pub fn range_example(source_name: Option<&str>) -> String {
    let source = source_name.unwrap_or_default();
    let Some(captures) = TRAILING_NUMBER.captures(source) else {
        let base = if source.is_empty() { "web" } else { source };
        return format!("{base}[1-10]");
    };
    let base = &captures[1];
    let digits = &captures[2];
    let number: f64 = digits.parse().unwrap_or(f64::INFINITY);
    if !is_safe_integer(number + 9.) {
        return format!("{base}[1-10]");
    }
    let width = if digits.starts_with('0') {
        digits.len()
    } else {
        0
    };
    let number = number as u64;
    format!(
        "{base}[{}-{}]",
        pad(number + 1, width),
        pad(number + 9, width)
    )
}

/// One complete draft per host to create, or why the range cannot be saved.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftRange {
    /// One complete draft per host to create, in order.
    pub drafts: Vec<HostDraft>,
    pub problem: Option<String>,
}

fn fail(problem: impl Into<String>) -> Option<DraftRange> {
    Some(DraftRange {
        drafts: Vec::new(),
        problem: Some(problem.into()),
    })
}

const WRITE_RANGE: &str = "Write the range as name[from-to], e.g. web[2-10].";

/// Expand a draft whose name holds a `[from-to]` range into one draft per host.
///
/// The HostName follows along: its own range expands in step with the name's;
/// otherwise, where it contains the duplicated host's name as a whole word
/// (`app1` in `app1.corp.local`), that part becomes each new name. Anything
/// else — an IP address, say — is copied as it is. A display name needs a
/// range of its own, so the hosts can be told apart.
///
/// `existing_names` are lowercase. Returns `None` when nothing asks for a
/// range, so a plain save happens.
pub fn expand_draft_range(
    draft: &HostDraft,
    source_name: Option<&str>,
    existing_names: &HashSet<String>,
) -> Option<DraftRange> {
    let text = js_trim(&draft.alias_text);
    let hostname = js_trim(&draft.hostname);
    let Some(range) = parse_name_range(text) else {
        // Brackets that do not parse would otherwise be saved literally.
        if RANGE_ATTEMPT.is_match(text) {
            return fail(WRITE_RANGE);
        }
        if parse_name_range(hostname).is_some() {
            return fail("A HostName range needs a range in the name too, e.g. web[2-10].");
        }
        return None;
    };

    if count_ranges(text) > 1 {
        return fail("Use one [from-to] range in the name.");
    }
    if RANGE_ATTEMPT.is_match(&RANGE.replace(text, "")) {
        return fail(WRITE_RANGE);
    }
    if draft.storage == Storage::Openssh && text.chars().any(is_js_whitespace) {
        return fail("A range needs a single alias — remove the extra aliases.");
    }
    if !exact(&range) {
        return fail("The range numbers are too large.");
    }
    if range.end < range.start {
        return fail("The range must count up, e.g. [2-10].");
    }
    let count = range_size(&range);
    if count > MAX_RANGE_HOSTS as f64 {
        return fail(format!(
            "A range can create at most {MAX_RANGE_HOSTS} hosts."
        ));
    }

    let names = expand_name_range(&range);
    let hostnames = match range_values("HostName", hostname, names.len()) {
        Err(problem) => return fail(problem),
        Ok(values) => values,
    };
    let display_name = js_trim(&draft.display_name);
    let display_names = match range_values("display name", display_name, names.len()) {
        Err(problem) => return fail(problem),
        Ok(values) => values,
    };
    if display_names.is_none() && !display_name.is_empty() {
        return fail("Give the display name a range too, e.g. Web [1-5], or leave it empty.");
    }

    if let Some(taken) = names
        .iter()
        .find(|name| existing_names.contains(&name.to_lowercase()))
    {
        return fail(format!(
            "\"{taken}\" already exists — pick a range without it."
        ));
    }

    let followed = hostnames.unwrap_or_else(|| follow_source_name(hostname, source_name, &names));
    Some(DraftRange {
        drafts: names
            .iter()
            .enumerate()
            .map(|(i, name)| HostDraft {
                alias_text: name.clone(),
                hostname: followed.get(i).cloned().unwrap_or_default(),
                display_name: display_names
                    .as_ref()
                    .and_then(|names| names.get(i).cloned())
                    .unwrap_or_default(),
                ..draft.clone()
            })
            .collect(),
        problem: None,
    })
}

fn count_ranges(text: &str) -> usize {
    RANGE.find_iter(text).count()
}

/// A field's own range, expanded in step with the name: the values, a
/// problem, or `None` when the field has no range. The size is compared
/// before anything is built, so a huge range costs nothing.
fn range_values(label: &str, text: &str, count: usize) -> Result<Option<Vec<String>>, String> {
    let Some(own) = parse_name_range(text) else {
        return Ok(None);
    };
    if count_ranges(text) > 1 {
        return Err(format!("Use one [from-to] range in the {label}."));
    }
    if !exact(&own) {
        return Err(format!("The {label} range numbers are too large."));
    }
    let size = if own.end >= own.start {
        range_size(&own)
    } else {
        0.
    };
    if size != count as f64 {
        return Err(format!(
            "The {label} range has {size} values but the name range has {count}."
        ));
    }
    Ok(Some(expand_name_range(&own)))
}

fn ascii_alnum_at(text: &str, at: usize, before: bool) -> bool {
    let c = if before {
        text[..at].chars().next_back()
    } else {
        text[at..].chars().next()
    };
    c.is_some_and(|c| c.is_ascii_alphanumeric())
}

/// The first whole-word occurrence of `word` in `text`, ignoring case
/// (`/(?<![A-Za-z0-9])word(?![A-Za-z0-9])/i`): its byte range.
fn find_word(text: &str, word: &str) -> Option<(usize, usize)> {
    if word.is_empty() {
        return None;
    }
    let lower_word: Vec<char> = word.chars().flat_map(char::to_lowercase).collect();
    for (start, _) in text.char_indices() {
        let mut matched = 0;
        let mut end = start;
        for c in text[start..].chars() {
            if matched == lower_word.len() {
                break;
            }
            let lower: Vec<char> = c.to_lowercase().collect();
            if lower_word[matched..].starts_with(&lower) {
                matched += lower.len();
                end += c.len_utf8();
            } else {
                break;
            }
        }
        if matched == lower_word.len()
            && !ascii_alnum_at(text, start, true)
            && !ascii_alnum_at(text, end, false)
        {
            return Some((start, end));
        }
    }
    None
}

fn follow_source_name(hostname: &str, source_name: Option<&str>, names: &[String]) -> Vec<String> {
    if let Some((start, end)) = source_name.and_then(|source| find_word(hostname, source)) {
        return names
            .iter()
            .map(|name| format!("{}{name}{}", &hostname[..start], &hostname[end..]))
            .collect();
    }
    names.iter().map(|_| hostname.to_string()).collect()
}

/// One line for the name field: what the save will create.
pub fn describe_draft_range(drafts: &[HostDraft]) -> String {
    let (Some(first), Some(last)) = (drafts.first(), drafts.last()) else {
        return String::new();
    };
    let names = if drafts.len() == 1 {
        first.alias_text.clone()
    } else {
        format!("{} \u{2026} {}", first.alias_text, last.alias_text)
    };
    let target = if first.hostname.is_empty() {
        String::new()
    } else if first.hostname == last.hostname {
        format!(" \u{2014} all connect to {}", first.hostname)
    } else {
        format!(
            " \u{2014} HostName {} \u{2026} {}",
            first.hostname, last.hostname
        )
    };
    format!(
        "Creates {} host{}: {names}{target}",
        drafts.len(),
        if drafts.len() == 1 { "" } else { "s" }
    )
}

/// What stops a range from being saved: the range itself, or any one of its
/// hosts (a later name can be longer than the first, say).
pub fn range_problem(range: &DraftRange) -> Option<String> {
    if let Some(problem) = &range.problem {
        return Some(problem.clone());
    }
    for draft in &range.drafts {
        if let Some(problem) = super::draft::draft_problem(draft) {
            return Some(if range.drafts.len() > 1 {
                format!("{}: {problem}", draft.alias_text)
            } else {
                problem
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    //! Port of `tests/unit/client/host-name-range.test.ts`.

    use super::super::draft::blank_draft;
    use super::*;

    fn draft_in(alias: &str, hostname: &str, storage: Storage) -> HostDraft {
        HostDraft {
            alias_text: alias.into(),
            hostname: hostname.into(),
            user: "ops".into(),
            port: "2222".into(),
            ..blank_draft("", "", storage)
        }
    }

    fn draft(alias: &str, hostname: &str) -> HostDraft {
        draft_in(alias, hostname, Storage::Openssh)
    }

    fn expand(draft: &HostDraft, source: Option<&str>) -> Option<DraftRange> {
        expand_draft_range(draft, source, &HashSet::new())
    }

    fn problem(draft: &HostDraft, source: Option<&str>) -> String {
        expand(draft, source)
            .and_then(|range| range.problem)
            .unwrap_or_default()
    }

    fn names(range: &DraftRange) -> Vec<&str> {
        range.drafts.iter().map(|d| d.alias_text.as_str()).collect()
    }

    fn hostnames(range: &DraftRange) -> Vec<&str> {
        range.drafts.iter().map(|d| d.hostname.as_str()).collect()
    }

    #[test]
    fn leaves_names_without_a_range_alone() {
        assert_eq!(parse_name_range("grvdgmapp1"), None);
        assert_eq!(
            expand(&draft("grvdgmapp1-copy", "x"), Some("grvdgmapp1")),
            None
        );
    }

    #[test]
    fn expands_a_range_and_follows_the_duplicated_name_in_the_hostname() {
        let result = expand(
            &draft("grvdgmapp[2-10]", "grvdgmapp1.corp.local"),
            Some("grvdgmapp1"),
        )
        .unwrap();
        assert_eq!(result.problem, None);
        assert_eq!(
            names(&result),
            [
                "grvdgmapp2",
                "grvdgmapp3",
                "grvdgmapp4",
                "grvdgmapp5",
                "grvdgmapp6",
                "grvdgmapp7",
                "grvdgmapp8",
                "grvdgmapp9",
                "grvdgmapp10"
            ]
        );
        assert_eq!(hostnames(&result)[0], "grvdgmapp2.corp.local");
        assert_eq!(hostnames(&result)[8], "grvdgmapp10.corp.local");
        // Everything else is copied from the source.
        assert!(
            result
                .drafts
                .iter()
                .all(|d| d.user == "ops" && d.port == "2222")
        );
    }

    #[test]
    fn accepts_dot_dot_and_colon_separators_and_keeps_zero_padding() {
        assert_eq!(
            names(&expand(&draft("db[08..10]", ""), None).unwrap()),
            ["db08", "db09", "db10"]
        );
        assert_eq!(
            names(&expand(&draft("db[1:3]-eu", ""), None).unwrap()),
            ["db1-eu", "db2-eu", "db3-eu"]
        );
    }

    #[test]
    fn only_replaces_the_source_name_as_a_whole_word() {
        let result = expand(&draft("app[2-3]", "app10.corp"), Some("app1")).unwrap();
        assert_eq!(hostnames(&result), ["app10.corp", "app10.corp"]);
    }

    #[test]
    fn matches_the_source_name_in_the_hostname_case_insensitively() {
        let result = expand(&draft("APP[2-3]", "app1.corp"), Some("APP1")).unwrap();
        assert_eq!(hostnames(&result), ["APP2.corp", "APP3.corp"]);
    }

    #[test]
    fn expands_a_hostname_range_in_step_with_the_name() {
        let result = expand(&draft("web[2-4]", "10.0.0.[12-14]"), Some("web1")).unwrap();
        assert_eq!(hostnames(&result), ["10.0.0.12", "10.0.0.13", "10.0.0.14"]);
        assert!(problem(&draft("web[2-4]", "10.0.0.[12-20]"), Some("web1")).contains("9 values"));
    }

    #[test]
    fn copies_an_unrelated_hostname_and_an_empty_one_unchanged() {
        assert_eq!(
            hostnames(&expand(&draft("web[2-3]", "10.0.0.1"), Some("web1")).unwrap()),
            ["10.0.0.1", "10.0.0.1"]
        );
        assert_eq!(
            hostnames(&expand(&draft("web[2-3]", ""), Some("web1")).unwrap()),
            ["", ""]
        );
    }

    #[test]
    fn reports_ranges_that_cannot_be_saved() {
        assert!(problem(&draft("web[5-2]", ""), None).contains("count up"));
        assert!(problem(&draft("web[1-1000]", ""), None).contains("at most"));
        assert!(problem(&draft("r[1-2]c[1-2]", ""), None).contains("one [from-to]"));
        assert!(problem(&draft("web[1-2] other", ""), None).contains("single alias"));
        let existing: HashSet<String> = ["web2".to_string()].into();
        assert!(
            expand_draft_range(&draft("web[1-3]", ""), None, &existing)
                .and_then(|range| range.problem)
                .unwrap()
                .contains("\"web2\" already exists")
        );
    }

    #[test]
    fn allows_spaces_in_muxus_profile_names() {
        let result = expand(&draft_in("edge router [1-2]", "r1", Storage::Muxus), None).unwrap();
        assert_eq!(names(&result), ["edge router 1", "edge router 2"]);
    }

    #[test]
    fn suggests_a_range_from_the_duplicated_name() {
        assert_eq!(range_example(Some("grvdgmapp1")), "grvdgmapp[2-10]");
        assert_eq!(range_example(Some("bastion")), "bastion[1-10]");
        // Counts on from the number, whatever its size, and keeps its width.
        assert_eq!(range_example(Some("app12")), "app[13-21]");
        assert_eq!(range_example(Some("app9")), "app[10-18]");
        assert_eq!(range_example(Some("db01")), "db[02-10]");
        assert_eq!(range_example(Some("db099")), "db[100-108]");
        assert_eq!(range_example(None), "web[1-10]");
    }

    #[test]
    fn checks_a_hostname_range_against_the_name_range_before_expanding_it() {
        // Four billion values would freeze the window if they were built first.
        let started = std::time::Instant::now();
        assert!(
            problem(&draft("web[2-4]", "10.0.0.[1-4000000000]"), Some("web1"))
                .contains("4000000000 values but the name range has 3")
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn refuses_numbers_too_large_to_count_exactly() {
        assert!(
            problem(&draft("x[99999999999999999-99999999999999999]", ""), None)
                .contains("too large")
        );
        assert!(
            problem(
                &draft("x[1-2]", "10.[99999999999999999-99999999999999999]"),
                None
            )
            .contains("too large")
        );
    }

    #[test]
    fn refuses_brackets_that_are_not_one_well_formed_range() {
        // A name with stray brackets would otherwise be saved literally.
        assert!(problem(&draft("web[2-10", ""), None).contains("name[from-to]"));
        assert!(problem(&draft("web[2,3]", ""), None).contains("name[from-to]"));
        // A HostName range needs a name range to step with.
        assert!(problem(&draft("web5", "10.0.0.[12-20]"), None).contains("range in the name too"));
        // Only one range in the HostName.
        assert!(problem(&draft("web[1-3]", "10.[1-3].0.[1-3]"), None).contains("one [from-to]"));
        // An IPv6 HostName is not a range.
        assert_eq!(expand(&draft("web1", "fd00::1"), None), None);
    }

    #[test]
    fn gives_each_host_its_own_display_name_or_none() {
        let display = |name: &str| HostDraft {
            display_name: name.into(),
            ..draft("web[1-3]", "")
        };
        assert!(problem(&display("Web"), None).contains("display name"));
        let ranged = expand(&display("Web [1-3]"), None).unwrap();
        let shown: Vec<&str> = ranged
            .drafts
            .iter()
            .map(|d| d.display_name.as_str())
            .collect();
        assert_eq!(shown, ["Web 1", "Web 2", "Web 3"]);
        assert!(problem(&display("Web [1-2]"), None).contains("display name range has 2 values"));
        let none = expand(&draft("web[1-3]", ""), None).unwrap();
        assert!(none.drafts.iter().all(|d| d.display_name.is_empty()));
    }

    #[test]
    fn describes_what_a_save_will_create() {
        let result = expand(
            &draft("grvdgmapp[2-10]", "grvdgmapp1.corp"),
            Some("grvdgmapp1"),
        )
        .unwrap();
        assert_eq!(
            describe_draft_range(&result.drafts),
            "Creates 9 hosts: grvdgmapp2 \u{2026} grvdgmapp10 \u{2014} HostName grvdgmapp2.corp \u{2026} grvdgmapp10.corp"
        );
        let same = expand(&draft("web[2-3]", "10.0.0.1"), Some("web1")).unwrap();
        assert!(describe_draft_range(&same.drafts).contains("all connect to 10.0.0.1"));
    }

    #[test]
    fn names_the_host_a_problem_belongs_to() {
        let range = expand(&draft("web[1-2]", ""), None).unwrap();
        // ssh_config hosts may leave the HostName out; nothing is wrong.
        assert_eq!(range_problem(&range), None);
        let profiles = expand(&draft_in("web[1-2]", "", Storage::Muxus), None).unwrap();
        assert_eq!(
            range_problem(&profiles).as_deref(),
            Some("web1: Enter a hostname or IP address.")
        );
    }
}
