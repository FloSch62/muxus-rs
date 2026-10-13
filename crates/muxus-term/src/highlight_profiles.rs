//! Keyword highlighting profiles: the import/export file, validation of untrusted rules, the JSON
//! rule editor, and the profiles Muxus ships (ports `client/src/highlight-profiles.ts` and
//! `client/src/builtin-highlight-profiles.ts`).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::custom_schemes::new_preference_id;
use crate::keywords::{
    KeywordHighlightProfile, KeywordHighlightRule, KeywordRuleExt, keyword_pattern_error,
};

pub const HIGHLIGHT_PROFILE_FORMAT: &str = "muxus-keyword-highlighting-profiles";
/// Version 2 added regex rules.
pub const HIGHLIGHT_PROFILE_VERSION: u32 = 2;
/// Files without regex rules keep version 1 so older releases can still import them.
const LITERAL_HIGHLIGHT_PROFILE_VERSION: u32 = 1;
pub const MAX_HIGHLIGHT_PROFILE_FILE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_KEYWORD_HIGHLIGHT_PROFILES: usize = 100;
pub const MAX_KEYWORD_HIGHLIGHT_RULES: usize = 100;

/// The portable highlighting-profile file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightProfileDocument {
    pub format: String,
    pub version: u32,
    pub created_at: String,
    pub profiles: Vec<KeywordHighlightProfile>,
}

/// A profile file or rule list that cannot be used; the message is shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileError(pub String);

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProfileError {}

fn error<T>(message: impl Into<String>) -> Result<T, ProfileError> {
    Err(ProfileError(message.into()))
}

/// Build the portable file shared by the highlighting settings import/export UI.
pub fn create_highlight_profile_document(
    profiles: &[KeywordHighlightProfile],
) -> Result<HighlightProfileDocument, ProfileError> {
    if profiles.is_empty() || !keyword_highlight_profiles_valid(profiles) {
        return error("Select at least one valid highlighting profile to export.");
    }
    // A release that predates regex rules would otherwise match their patterns as literal text;
    // the newer version makes it refuse the file instead.
    let has_regex = profiles
        .iter()
        .any(|profile| profile.rules.iter().any(KeywordHighlightRule::is_regex));
    Ok(HighlightProfileDocument {
        format: HIGHLIGHT_PROFILE_FORMAT.into(),
        version: if has_regex {
            HIGHLIGHT_PROFILE_VERSION
        } else {
            LITERAL_HIGHLIGHT_PROFILE_VERSION
        },
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        profiles: profiles.iter().map(copy_profile).collect(),
    })
}

/// Parse an untrusted, standalone highlighting-profile file.
pub fn parse_highlight_profile_document(
    text: &str,
) -> Result<HighlightProfileDocument, ProfileError> {
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return error("This highlighting profile file is not valid JSON.");
    };
    let Some(record) = parsed
        .as_object()
        .filter(|r| r.get("format").and_then(Value::as_str) == Some(HIGHLIGHT_PROFILE_FORMAT))
    else {
        return error("This is not a Muxus highlighting profile file.");
    };
    let version = record.get("version");
    let version = match version.and_then(Value::as_f64) {
        Some(v) if v == f64::from(LITERAL_HIGHLIGHT_PROFILE_VERSION) => {
            LITERAL_HIGHLIGHT_PROFILE_VERSION
        }
        Some(v) if v == f64::from(HIGHLIGHT_PROFILE_VERSION) => HIGHLIGHT_PROFILE_VERSION,
        Some(_) => {
            return error(format!(
                "Highlighting profile version {} is not supported.",
                version.expect("number")
            ));
        }
        None => return error("This file is missing a supported highlighting profile version."),
    };
    let created_at = record.get("createdAt").and_then(Value::as_str);
    let profiles = record.get("profiles").cloned().unwrap_or(Value::Null);
    if created_at.is_none_or(|created| chrono::DateTime::parse_from_rfc3339(created).is_err())
        || !is_keyword_highlight_profile_array(&profiles)
        || profiles.as_array().is_none_or(Vec::is_empty)
    {
        return error("The highlighting profile file is incomplete or invalid.");
    }
    let profiles: Vec<KeywordHighlightProfile> =
        serde_json::from_value(profiles).map_err(|_| {
            ProfileError("The highlighting profile file is incomplete or invalid.".into())
        })?;
    // Version 1 predates regex rules, and a release of that era would match the pattern as
    // literal text. A version 1 file that claims one is mislabeled.
    if version == LITERAL_HIGHLIGHT_PROFILE_VERSION
        && profiles
            .iter()
            .any(|profile| profile.rules.iter().any(KeywordHighlightRule::is_regex))
    {
        return error(format!(
            "This file contains regex rules, which need highlighting profile version {HIGHLIGHT_PROFILE_VERSION}; it says version {LITERAL_HIGHLIGHT_PROFILE_VERSION}."
        ));
    }
    Ok(HighlightProfileDocument {
        format: HIGHLIGHT_PROFILE_FORMAT.into(),
        version,
        created_at: created_at.unwrap_or_default().into(),
        profiles: profiles.iter().map(copy_profile).collect(),
    })
}

/// Replace matching ids and retain unrelated profiles during an import.
pub fn merge_highlight_profiles(
    current: &[KeywordHighlightProfile],
    imported: &[KeywordHighlightProfile],
) -> Result<Vec<KeywordHighlightProfile>, ProfileError> {
    let mut merged: Vec<KeywordHighlightProfile> = current.to_vec();
    for profile in imported {
        match merged.iter_mut().find(|existing| existing.id == profile.id) {
            Some(existing) => *existing = profile.clone(),
            None => merged.push(profile.clone()),
        }
    }
    if merged.len() > MAX_KEYWORD_HIGHLIGHT_PROFILES {
        return error(format!(
            "This import would exceed the limit of {MAX_KEYWORD_HIGHLIGHT_PROFILES} highlighting profiles. Delete an existing profile and try again."
        ));
    }
    if !keyword_highlight_profiles_valid(&merged) {
        return error("The merged highlighting profiles are invalid.");
    }
    Ok(merged)
}

fn bounded_string(value: &Value, max: usize) -> Option<&str> {
    let text = value.as_str()?;
    let length = text.encode_utf16().count();
    (length > 0 && length <= max).then_some(text)
}

fn valid_hex_color(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(crate::palette::is_hex_color)
}

/// Whether untrusted JSON is a valid profile list.
pub fn is_keyword_highlight_profile_array(value: &Value) -> bool {
    let Some(entries) = value.as_array() else {
        return false;
    };
    if entries.len() > MAX_KEYWORD_HIGHLIGHT_PROFILES {
        return false;
    }
    let mut profile_ids = HashSet::new();
    entries.iter().all(|entry| {
        let Some(record) = entry.as_object() else {
            return false;
        };
        let Some(id) = record.get("id").and_then(|id| bounded_string(id, 200)) else {
            return false;
        };
        if profile_ids.contains(id) {
            return false;
        }
        let Some(name) = record
            .get("name")
            .and_then(|name| bounded_string(name, 200))
        else {
            return false;
        };
        let Some(rules) = record.get("rules").and_then(Value::as_array) else {
            return false;
        };
        if name.trim().is_empty() || rules.len() > MAX_KEYWORD_HIGHLIGHT_RULES {
            return false;
        }
        let mut rule_ids = HashSet::new();
        for rule in rules {
            if !valid_keyword_highlight_rule(rule) {
                return false;
            }
            let rule_id = rule["id"].as_str().unwrap_or_default();
            if !rule_ids.insert(rule_id) {
                return false;
            }
        }
        profile_ids.insert(id.to_string());
        true
    })
}

/// The typed form of [`is_keyword_highlight_profile_array`].
pub fn keyword_highlight_profiles_valid(profiles: &[KeywordHighlightProfile]) -> bool {
    serde_json::to_value(profiles).is_ok_and(|value| is_keyword_highlight_profile_array(&value))
}

/// Whether untrusted JSON is one valid rule.
pub fn valid_keyword_highlight_rule(value: &Value) -> bool {
    let Some(record) = value.as_object() else {
        return false;
    };
    record
        .get("id")
        .and_then(|id| bounded_string(id, 100))
        .is_some()
        && record
            .get("name")
            .is_none_or(|name| bounded_string(name, 100).is_some())
        && record
            .get("keyword")
            .and_then(|keyword| bounded_string(keyword, 500))
            .is_some()
        && valid_hex_color(record.get("foreground"))
        && record
            .get("background")
            .is_none_or(|bg| valid_hex_color(Some(bg)))
        && record.get("caseSensitive").is_some_and(Value::is_boolean)
        && record.get("wholeWord").is_some_and(Value::is_boolean)
        && record.get("regex").is_none_or(Value::is_boolean)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EditorRule<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    keyword: &'a str,
    regex: bool,
    case_sensitive: bool,
    whole_word: bool,
    foreground: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    background: Option<&'a str>,
}

/// Rules as the JSON editor shows them: no ids, fields in reading order, two-space indent.
pub fn keyword_highlight_rules_to_json(rules: &[KeywordHighlightRule]) -> String {
    let editor: Vec<EditorRule<'_>> = rules
        .iter()
        .map(|rule| EditorRule {
            name: rule.name.as_deref().filter(|name| !name.is_empty()),
            keyword: &rule.keyword,
            regex: rule.is_regex(),
            case_sensitive: rule.case_sensitive,
            whole_word: rule.whole_word,
            foreground: &rule.foreground,
            background: rule.background.as_deref().filter(|bg| !bg.is_empty()),
        })
        .collect();
    serde_json::to_string_pretty(&editor).unwrap_or_else(|_| "[]".into())
}

const JSON_RULE_FIELDS: [&str; 8] = [
    "id",
    "name",
    "keyword",
    "regex",
    "caseSensitive",
    "wholeWord",
    "foreground",
    "background",
];

/// Parse rules edited as JSON. Flags default to false and ids are optional: an entry without one
/// keeps the id of the rule at its position, so rules pasted from an exported profile file work
/// as well as hand-written ones. The error names the first problem and its 1-based rule number.
pub fn parse_keyword_highlight_rules_json(
    text: &str,
    previous: &[KeywordHighlightRule],
) -> Result<Vec<KeywordHighlightRule>, ProfileError> {
    let parsed: Value = serde_json::from_str(text)
        .map_err(|e| ProfileError(format!("This is not valid JSON: {e}")))?;
    let Some(entries) = parsed.as_array() else {
        return error("Expected a JSON array of rules: [ { ... } ].");
    };
    if entries.len() > MAX_KEYWORD_HIGHLIGHT_RULES {
        return error(format!(
            "A rule list holds at most {MAX_KEYWORD_HIGHLIGHT_RULES} rules."
        ));
    }
    let mut explicit_ids = HashSet::new();
    let mut records: Vec<(&Map<String, Value>, String)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let label = format!("Rule {}", index + 1);
        let Some(record) = entry.as_object() else {
            return error(format!("{label} must be an object."));
        };
        if let Some(unknown) = record
            .keys()
            .find(|key| !JSON_RULE_FIELDS.contains(&key.as_str()))
        {
            return error(format!("{label} has an unknown field \"{unknown}\"."));
        }
        if let Some(id) = record.get("id") {
            match bounded_string(id, 100) {
                Some(id) if explicit_ids.insert(id.to_string()) => {}
                _ => {
                    return error(format!(
                        "{label} needs a unique \"id\" of up to 100 characters, or none."
                    ));
                }
            }
        }
        records.push((record, label));
    }
    let mut used_ids = explicit_ids;
    let mut rules = Vec::new();
    for (index, (record, label)) in records.into_iter().enumerate() {
        let Some(keyword) = record.get("keyword").and_then(|k| bounded_string(k, 500)) else {
            return error(format!(
                "{label} needs a \"keyword\" of 1 to 500 characters."
            ));
        };
        let name = record.get("name");
        if let Some(name) = name
            && name.as_str() != Some("")
            && bounded_string(name, 100).is_none()
        {
            return error(format!(
                "{label}: \"name\" must be text of up to 100 characters."
            ));
        }
        if !valid_hex_color(record.get("foreground")) {
            return error(format!(
                "{label}: \"foreground\" must be a color such as \"#ff0000\"."
            ));
        }
        let background = record.get("background").filter(|bg| !bg.is_null());
        if background.is_some_and(|bg| !valid_hex_color(Some(bg))) {
            return error(format!(
                "{label}: \"background\" must be a color such as \"#7f1d1d\", or left out."
            ));
        }
        for flag in ["regex", "caseSensitive", "wholeWord"] {
            if record.get(flag).is_some_and(|value| !value.is_boolean()) {
                return error(format!("{label}: \"{flag}\" must be true or false."));
            }
        }
        let explicit = record.get("id").and_then(Value::as_str);
        let mut id = explicit
            .map(String::from)
            .or_else(|| previous.get(index).map(|rule| rule.id.clone()));
        if id.is_none()
            || (explicit.is_none() && id.as_ref().is_some_and(|id| used_ids.contains(id)))
        {
            id = Some(new_preference_id("highlight"));
        }
        let id = id.expect("id assigned");
        used_ids.insert(id.clone());
        let flag = |name: &str| record.get(name).and_then(Value::as_bool) == Some(true);
        let rule = KeywordHighlightRule {
            id,
            name: name
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
                .map(String::from),
            keyword: keyword.to_string(),
            foreground: record["foreground"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            background: background
                .and_then(Value::as_str)
                .filter(|bg| !bg.is_empty())
                .map(String::from),
            case_sensitive: flag("caseSensitive"),
            whole_word: flag("wholeWord"),
            regex: flag("regex").then_some(true),
        };
        if let Some(problem) = keyword_pattern_error(&rule.keyword, rule.is_regex()) {
            return error(format!("{label}: {problem}"));
        }
        rules.push(rule);
    }
    Ok(rules)
}

/// Strip unknown fields and a false regex flag instead of carrying them into later exports.
fn copy_profile(profile: &KeywordHighlightProfile) -> KeywordHighlightProfile {
    KeywordHighlightProfile {
        id: profile.id.clone(),
        name: profile.name.clone(),
        rules: profile
            .rules
            .iter()
            .map(|rule| KeywordHighlightRule {
                regex: rule.is_regex().then_some(true),
                ..rule.clone()
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------------------------
// Built-in profiles
// ---------------------------------------------------------------------------------------------

// Mid-tone text colors stay readable on both light and dark terminal schemes; severities use
// filled badges so they do not depend on the scheme at all.
const GREEN: &str = "#16a34a";
const RED: &str = "#dc2626";
const AMBER: &str = "#d97706";
const BLUE: &str = "#3b82f6";
const CYAN: &str = "#0891b2";
const PURPLE: &str = "#a855f7";

const HEX_GROUP: &str = r"[\da-f]{1,4}";

fn pattern(id: &str, name: &str, keyword: &str, foreground: &str) -> KeywordHighlightRule {
    KeywordHighlightRule {
        id: id.into(),
        name: Some(name.into()),
        keyword: keyword.into(),
        foreground: foreground.into(),
        background: None,
        case_sensitive: false,
        whole_word: false,
        regex: Some(true),
    }
}

fn badge(rule: KeywordHighlightRule, foreground: &str, background: &str) -> KeywordHighlightRule {
    KeywordHighlightRule {
        foreground: foreground.into(),
        background: Some(background.into()),
        ..rule
    }
}

fn case_sensitive(rule: KeywordHighlightRule) -> KeywordHighlightRule {
    KeywordHighlightRule {
        case_sensitive: true,
        ..rule
    }
}

/// Address rules shared by both platforms; their ids repeat across profiles.
fn address_rules() -> Vec<KeywordHighlightRule> {
    let g = HEX_GROUP;
    vec![
        pattern(
            "ipv4",
            "IPv4 addresses",
            r"(?<![\d.])(?:\d{1,3}\.){3}\d{1,3}(?:/\d{1,2})?(?!\.?\d)",
            CYAN,
        ),
        pattern(
            "ipv6",
            "IPv6 addresses",
            &format!(
                r"(?<![\w:])(?:(?:{g}:){{7}}{g}|(?:{g}:){{1,7}}:(?:{g}(?::{g}){{0,6}})?|::(?:{g}(?::{g}){{0,6}})?)(?:/\d{{1,3}})?(?![\w:]|\.\d)"
            ),
            CYAN,
        ),
        pattern(
            "mac",
            "MAC addresses",
            r"\b[\da-f]{2}(?:[:-][\da-f]{2}){5}\b",
            PURPLE,
        ),
    ]
}

// A hyphen counts as part of the word so YANG leaves such as oper-down-reason and hold-time up
// are not colored as states.
fn state(words: &str) -> String {
    format!(r"(?<![\w-])(?:{words})(?![\w-])")
}

const MAJOR_BADGE: (&str, &str) = ("#ffffff", "#b91c1c");
const MINOR_BADGE: (&str, &str) = ("#1c1917", "#f59e0b");

fn nokia_sros() -> KeywordHighlightProfile {
    let mut rules = address_rules();
    rules.extend([
        pattern(
            "port",
            "Ports and LAGs",
            r"\b\d{1,2}/\d{1,2}/(?:c\d{1,3}(?:/\d{1,3})?|\d{1,3})\b|\blag-\d+\b",
            BLUE,
        ),
        pattern(
            "state-up",
            "Up and enabled",
            &state("up|established|inservice|enabled?"),
            GREEN,
        ),
        pattern(
            "state-admin-down",
            "Administratively down",
            &(state("disabled?|ghost") + r"|(?<!no )\bshutdown\b"),
            AMBER,
        ),
        pattern(
            "state-down",
            "Down and failed",
            &state("down|outofservice|failed|failure"),
            RED,
        ),
        case_sensitive(pattern(
            "bgp-transitional",
            "BGP sessions not established",
            r"\b(?:Idle|Connect|OpenSent|OpenConfirm|IDLE|CONNECT|ACTIVE|OPENSENT|OPENCONFIRM)\b",
            AMBER,
        )),
        case_sensitive(badge(
            pattern(
                "severity-major",
                "Critical and major alarms",
                r"\b(?:CRITICAL|MAJOR)\b",
                "",
            ),
            MAJOR_BADGE.0,
            MAJOR_BADGE.1,
        )),
        case_sensitive(badge(
            pattern(
                "severity-minor",
                "Minor alarms and warnings",
                r"\b(?:MINOR|WARNING)\b",
                "",
            ),
            MINOR_BADGE.0,
            MINOR_BADGE.1,
        )),
        case_sensitive(badge(
            pattern("severity-cleared", "Cleared alarms", r"\bCLEARED\b", ""),
            "#ffffff",
            "#15803d",
        )),
        case_sensitive(pattern("severity-info", "Info messages", r"\bINFO\b", BLUE)),
        case_sensitive(pattern("cli-error", "CLI errors", r"^Error:.*", RED)),
        // The MD-CLI context line: "*(ex)[/configure]" means uncommitted changes in an exclusive,
        // private, global or read-only configuration session.
        pattern(
            "mdcli-config-mode",
            "MD-CLI configuration mode",
            r"^[!*]*\((?:ex|pr|gl|ro)\)(?=\[)",
            AMBER,
        ),
    ]);
    KeywordHighlightProfile {
        id: "muxus-nokia-sros".into(),
        name: "Nokia SR OS".into(),
        rules,
    }
}

fn nokia_srlinux() -> KeywordHighlightProfile {
    let mut rules = address_rules();
    rules.extend([
        pattern(
            "interface",
            "Interfaces",
            r"\b(?:ethernet-\d+/\d+(?:/\d+)?|mgmt\d+|lag\d+|irb\d+|lo\d+|system\d+)(?:\.\d+)?\b",
            BLUE,
        ),
        pattern(
            "state-up",
            "Up and enabled",
            &state("up|enabled?|established"),
            GREEN,
        ),
        pattern(
            "state-admin-down",
            "Administratively down",
            &state("disabled?"),
            AMBER,
        ),
        pattern(
            "state-down",
            "Down and failed",
            &state("down|failed|failure"),
            RED,
        ),
        // Lowercase only: table headers such as "Active" are not session states.
        case_sensitive(pattern(
            "bgp-transitional",
            "BGP sessions not established",
            &state("idle|active|connect|opensent|openconfirm"),
            AMBER,
        )),
        badge(
            pattern(
                "severity-major",
                "Critical and major alarms",
                &state("critical|major"),
                "",
            ),
            MAJOR_BADGE.0,
            MAJOR_BADGE.1,
        ),
        badge(
            pattern(
                "severity-minor",
                "Minor alarms and warnings",
                &state("minor|warning"),
                "",
            ),
            MINOR_BADGE.0,
            MINOR_BADGE.1,
        ),
        case_sensitive(pattern("cli-error", "CLI errors", r"^Error:.*", RED)),
        case_sensitive(pattern(
            "cli-warning",
            "CLI warnings",
            r"^Warning:.*",
            AMBER,
        )),
        KeywordHighlightRule {
            id: "commit-ok".into(),
            name: Some("Commit succeeded".into()),
            keyword: "All changes have been committed".into(),
            foreground: GREEN.into(),
            background: None,
            case_sensitive: true,
            whole_word: false,
            regex: None,
        },
        // The context line above the prompt, e.g. "--{ + candidate shared default }--[  ]--".
        pattern(
            "prompt-context",
            "Prompt context",
            r"^--\{[^}]*\}--\[[^\]]*\]--",
            BLUE,
        ),
        pattern(
            "prompt-candidate",
            "Candidate mode",
            r"^--\{[^}]*\bcandidate\b[^}]*\}--",
            AMBER,
        ),
    ]);
    KeywordHighlightProfile {
        id: "muxus-nokia-srlinux".into(),
        name: "Nokia SR Linux".into(),
        rules,
    }
}

static BUILTIN_HIGHLIGHT_PROFILES: LazyLock<Vec<KeywordHighlightProfile>> =
    LazyLock::new(|| vec![nokia_sros(), nokia_srlinux()]);

/// Profiles Muxus ships. They are ordinary, editable profiles with stable ids.
pub fn builtin_highlight_profiles() -> &'static [KeywordHighlightProfile] {
    &BUILTIN_HIGHLIGHT_PROFILES
}

/// Append each built-in profile that is not installed, within the profile limit.
pub fn with_builtin_highlight_profiles(
    profiles: &[KeywordHighlightProfile],
) -> Vec<KeywordHighlightProfile> {
    let mut next = profiles.to_vec();
    let installed: HashSet<&str> = profiles.iter().map(|profile| profile.id.as_str()).collect();
    for profile in builtin_highlight_profiles() {
        if next.len() >= MAX_KEYWORD_HIGHLIGHT_PROFILES {
            break;
        }
        if !installed.contains(profile.id.as_str()) {
            next.push(profile.clone());
        }
    }
    next
}

/// Name installed built-in rules that were seeded before they had names. Only a rule that still
/// has its shipped id and pattern, and no name of its own, changes.
pub fn with_builtin_rule_names(
    profiles: &[KeywordHighlightProfile],
) -> Vec<KeywordHighlightProfile> {
    profiles
        .iter()
        .map(|profile| {
            let Some(builtin) = builtin_highlight_profiles()
                .iter()
                .find(|candidate| candidate.id == profile.id)
            else {
                return profile.clone();
            };
            let shipped: HashMap<&str, &KeywordHighlightRule> = builtin
                .rules
                .iter()
                .map(|rule| (rule.id.as_str(), rule))
                .collect();
            KeywordHighlightProfile {
                rules: profile
                    .rules
                    .iter()
                    .map(|rule| match shipped.get(rule.id.as_str()) {
                        Some(found)
                            if rule.name.as_deref().is_none_or(str::is_empty)
                                && found.keyword == rule.keyword
                                && found.name.is_some() =>
                        {
                            KeywordHighlightRule {
                                name: found.name.clone(),
                                ..rule.clone()
                            }
                        }
                        _ => rule.clone(),
                    })
                    .collect(),
                ..profile.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keywords::{
        MAX_KEYWORD_DECORATIONS, find_keyword_matches, keyword_highlight_rules_problem,
    };
    use serde_json::json;

    fn major_rule() -> KeywordHighlightRule {
        KeywordHighlightRule {
            id: "major-alarm".into(),
            name: None,
            keyword: "MAJOR".into(),
            foreground: "#ffffff".into(),
            background: Some("#b91c1c".into()),
            case_sensitive: true,
            whole_word: true,
            regex: None,
        }
    }

    fn nokia() -> KeywordHighlightProfile {
        KeywordHighlightProfile {
            id: "nokia-sros".into(),
            name: "Nokia SR OS".into(),
            rules: vec![major_rule()],
        }
    }

    fn message<T: fmt::Debug>(result: Result<T, ProfileError>) -> String {
        result.unwrap_err().0
    }

    #[test]
    fn round_trips_a_bounded_versioned_profile_document() {
        let document = create_highlight_profile_document(&[nokia()]).unwrap();
        assert_eq!(document.format, HIGHLIGHT_PROFILE_FORMAT);
        assert_eq!(document.profiles, vec![nokia()]);
        let text = serde_json::to_string(&document).unwrap();
        assert_eq!(parse_highlight_profile_document(&text).unwrap(), document);
    }

    #[test]
    fn keeps_literal_only_files_at_version_1() {
        let document = create_highlight_profile_document(&[nokia()]).unwrap();
        assert_eq!(document.version, 1);
        let text = serde_json::to_string(&document).unwrap();
        assert_eq!(parse_highlight_profile_document(&text).unwrap().version, 1);
    }

    #[test]
    fn writes_version_2_when_a_rule_is_a_regex_and_round_trips_the_flag() {
        let regex_rule = KeywordHighlightRule {
            id: "port".into(),
            keyword: r"\d+/\d+/\d+".into(),
            regex: Some(true),
            ..major_rule()
        };
        let profile = KeywordHighlightProfile {
            rules: vec![major_rule(), regex_rule],
            ..nokia()
        };
        let document = create_highlight_profile_document(std::slice::from_ref(&profile)).unwrap();
        assert_eq!(document.version, HIGHLIGHT_PROFILE_VERSION);
        assert_eq!(HIGHLIGHT_PROFILE_VERSION, 2);
        let text = serde_json::to_string(&document).unwrap();
        assert_eq!(
            parse_highlight_profile_document(&text).unwrap().profiles,
            vec![profile]
        );
    }

    #[test]
    fn rejects_a_version_1_file_that_claims_regex_rules() {
        let regex_rule = KeywordHighlightRule {
            keyword: r"\bMAJOR\b".into(),
            regex: Some(true),
            ..major_rule()
        };
        let document = create_highlight_profile_document(&[KeywordHighlightProfile {
            rules: vec![regex_rule],
            ..nokia()
        }])
        .unwrap();
        let mut value = serde_json::to_value(document).unwrap();
        value["version"] = json!(1);
        assert!(
            message(parse_highlight_profile_document(&value.to_string()))
                .contains("regex rules, which need highlighting profile version 2")
        );
    }

    #[test]
    fn drops_a_false_regex_flag_rather_than_exporting_it() {
        let profile = KeywordHighlightProfile {
            rules: vec![KeywordHighlightRule {
                regex: Some(false),
                ..major_rule()
            }],
            ..nokia()
        };
        let document = create_highlight_profile_document(&[profile]).unwrap();
        let value = serde_json::to_value(&document).unwrap();
        assert!(value["profiles"][0]["rules"][0].get("regex").is_none());
    }

    #[test]
    fn round_trips_an_optional_rule_name_and_rejects_an_empty_one() {
        let named = KeywordHighlightProfile {
            rules: vec![KeywordHighlightRule {
                name: Some("Major alarms".into()),
                ..major_rule()
            }],
            ..nokia()
        };
        let text = serde_json::to_string(
            &create_highlight_profile_document(std::slice::from_ref(&named)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            parse_highlight_profile_document(&text).unwrap().profiles,
            vec![named]
        );
        let mut value = serde_json::to_value([nokia()]).unwrap();
        value[0]["rules"][0]["name"] = json!("");
        assert!(!is_keyword_highlight_profile_array(&value));
    }

    #[test]
    fn rejects_a_non_boolean_regex_flag() {
        let mut value = serde_json::to_value([nokia()]).unwrap();
        value[0]["rules"][0]["regex"] = json!("yes");
        assert!(!is_keyword_highlight_profile_array(&value));
    }

    #[test]
    fn rejects_unrelated_future_and_malformed_documents() {
        assert!(message(parse_highlight_profile_document("{no json")).contains("valid JSON"));
        let now = chrono::Utc::now().to_rfc3339();
        let other = json!({ "format": "another-format", "version": 2, "createdAt": now, "profiles": [nokia()] });
        assert!(
            message(parse_highlight_profile_document(&other.to_string()))
                .contains("not a Muxus highlighting profile")
        );
        let future = json!({ "format": HIGHLIGHT_PROFILE_FORMAT, "version": 3, "createdAt": now, "profiles": [nokia()] });
        assert!(
            message(parse_highlight_profile_document(&future.to_string()))
                .contains("not supported")
        );
        let mut value = serde_json::to_value([nokia()]).unwrap();
        value[0]["rules"][0]["foreground"] = json!("red");
        assert!(!is_keyword_highlight_profile_array(&value));
    }

    #[test]
    fn updates_matching_stable_ids_without_dropping_unrelated_profiles() {
        let cisco = KeywordHighlightProfile {
            id: "cisco-ios".into(),
            name: "Cisco IOS".into(),
            ..nokia()
        };
        let updated = KeywordHighlightProfile {
            name: "Nokia SR OS 24".into(),
            ..nokia()
        };
        assert_eq!(
            merge_highlight_profiles(&[nokia(), cisco.clone()], std::slice::from_ref(&updated))
                .unwrap(),
            vec![updated, cisco]
        );
    }

    #[test]
    fn allows_replacements_at_the_profile_limit_but_rejects_a_distinct_profile_beyond_it() {
        let existing: Vec<KeywordHighlightProfile> = (0..MAX_KEYWORD_HIGHLIGHT_PROFILES)
            .map(|index| KeywordHighlightProfile {
                id: format!("profile-{index}"),
                name: format!("Profile {index}"),
                ..nokia()
            })
            .collect();
        let replacement = KeywordHighlightProfile {
            name: "Updated profile".into(),
            ..existing[0].clone()
        };
        assert_eq!(
            merge_highlight_profiles(&existing, &[replacement])
                .unwrap()
                .len(),
            MAX_KEYWORD_HIGHLIGHT_PROFILES
        );
        let extra = KeywordHighlightProfile {
            id: "one-too-many".into(),
            name: "One too many".into(),
            ..nokia()
        };
        assert!(
            message(merge_highlight_profiles(&existing, &[extra]))
                .contains("limit of 100 highlighting profiles")
        );
    }

    #[test]
    fn rejects_duplicate_profile_and_rule_ids() {
        assert!(!keyword_highlight_profiles_valid(&[nokia(), nokia()]));
        let doubled = KeywordHighlightProfile {
            rules: vec![major_rule(), major_rule()],
            ..nokia()
        };
        assert!(!keyword_highlight_profiles_valid(&[doubled]));
    }

    fn editor_rules() -> Vec<KeywordHighlightRule> {
        vec![
            KeywordHighlightRule {
                name: Some("Major alarms".into()),
                ..major_rule()
            },
            KeywordHighlightRule {
                id: "port".into(),
                name: None,
                keyword: r"\d+/\d+/\d+".into(),
                foreground: "#3b82f6".into(),
                background: None,
                case_sensitive: false,
                whole_word: true,
                regex: Some(true),
            },
        ]
    }

    #[test]
    fn hides_ids_and_round_trips_every_field() {
        let rules = editor_rules();
        let text = keyword_highlight_rules_to_json(&rules);
        assert!(!text.contains("\"id\""));
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed[1],
            json!({ "keyword": r"\d+/\d+/\d+", "regex": true, "caseSensitive": false, "wholeWord": true, "foreground": "#3b82f6" })
        );
        assert_eq!(
            parse_keyword_highlight_rules_json(&text, &rules).unwrap(),
            rules
        );
    }

    #[test]
    fn defaults_flags_keeps_positional_ids_and_generates_new_ones() {
        let text = json!([
            { "keyword": "down", "foreground": "#dc2626" },
            { "keyword": "up", "foreground": "#16a34a", "name": "" },
            { "keyword": "idle", "foreground": "#d97706", "background": null },
        ])
        .to_string();
        let parsed = parse_keyword_highlight_rules_json(&text, &editor_rules()).unwrap();
        assert_eq!(
            [parsed[0].id.as_str(), parsed[1].id.as_str()],
            ["major-alarm", "port"]
        );
        assert!(parsed[2].id.starts_with("highlight-"));
        assert_eq!(
            parsed[0],
            KeywordHighlightRule {
                id: "major-alarm".into(),
                name: None,
                keyword: "down".into(),
                foreground: "#dc2626".into(),
                background: None,
                case_sensitive: false,
                whole_word: false,
                regex: None,
            }
        );
        assert_eq!(parsed[1].name, None);
        assert_eq!(parsed[2].background, None);
    }

    #[test]
    fn accepts_rules_pasted_from_a_profile_file_keeping_their_ids() {
        let rules = editor_rules();
        let mut first = serde_json::to_value(&rules[1]).unwrap();
        first["id"] = json!("major-alarm");
        let mut second = serde_json::to_value(&rules[0]).unwrap();
        second["id"] = json!("other");
        let pasted = json!([first, second]).to_string();
        let ids: Vec<String> = parse_keyword_highlight_rules_json(&pasted, &rules)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, ["major-alarm", "other"]);
    }

    #[test]
    fn does_not_reuse_a_positional_id_that_an_explicit_entry_claims() {
        let text = json!([
            { "keyword": "a", "foreground": "#ffffff" },
            { "id": "major-alarm", "keyword": "b", "foreground": "#ffffff" },
        ])
        .to_string();
        let parsed = parse_keyword_highlight_rules_json(&text, &editor_rules()).unwrap();
        assert_eq!(parsed[1].id, "major-alarm");
        assert_ne!(parsed[0].id, "major-alarm");
    }

    #[test]
    fn names_the_first_problem_with_its_rule_number() {
        let parse =
            |value: Value| message(parse_keyword_highlight_rules_json(&value.to_string(), &[]));
        assert!(message(parse_keyword_highlight_rules_json("[{", &[])).contains("not valid JSON"));
        assert!(parse(json!({ "keyword": "x" })).contains("array of rules"));
        assert!(parse(json!([{ "foreground": "#ffffff" }])).contains("Rule 1 needs a \"keyword\""));
        assert!(
            parse(json!([{ "keyword": "x", "foreground": "#ffffff" }, { "keyword": "y", "foreground": "red" }]))
                .contains("Rule 2: \"foreground\" must be a color")
        );
        assert!(
            parse(json!([{ "keyword": "x", "foreground": "#ffffff", "colour": "#000000" }]))
                .contains("unknown field \"colour\"")
        );
        assert!(
            parse(json!([{ "keyword": "x", "foreground": "#ffffff", "regex": "yes" }]))
                .contains("\"regex\" must be true or false")
        );
        assert!(
            parse(json!([{ "keyword": "(x", "foreground": "#ffffff", "regex": true }]))
                .contains("Rule 1: Invalid regular expression")
        );
        assert!(
            parse(json!([
                { "id": "a", "keyword": "x", "foreground": "#ffffff" },
                { "id": "a", "keyword": "y", "foreground": "#ffffff" },
            ]))
            .contains("Rule 2 needs a unique \"id\"")
        );
    }

    /// `[rule id, matched text]` pairs, in the order decorations would be drawn.
    fn highlights(profile: &KeywordHighlightProfile, line: &str) -> Vec<(String, String)> {
        find_keyword_matches(line, &profile.rules, MAX_KEYWORD_DECORATIONS)
            .into_iter()
            .map(|m| (m.rule.id.clone(), line[m.start..m.end].to_string()))
            .collect()
    }

    fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(id, text)| (id.to_string(), text.to_string()))
            .collect()
    }

    #[test]
    fn builtin_profiles_are_valid_compilable_and_survive_an_export_import_round_trip() {
        assert!(keyword_highlight_profiles_valid(
            builtin_highlight_profiles()
        ));
        for profile in builtin_highlight_profiles() {
            assert_eq!(
                keyword_highlight_rules_problem(&profile.rules),
                None,
                "{}",
                profile.name
            );
            // A name says what each regex is for; the patterns alone are hard to read.
            let names: Vec<&str> = profile
                .rules
                .iter()
                .map(|rule| rule.name.as_deref().unwrap_or(""))
                .collect();
            assert!(names.iter().all(|name| !name.is_empty()));
            assert_eq!(names.iter().collect::<HashSet<_>>().len(), names.len());
        }
        let document = create_highlight_profile_document(builtin_highlight_profiles()).unwrap();
        assert_eq!(document.version, 2);
        let text = serde_json::to_string(&document).unwrap();
        assert_eq!(
            parse_highlight_profile_document(&text).unwrap().profiles,
            builtin_highlight_profiles()
        );
    }

    #[test]
    fn builtin_profiles_mark_addresses_without_catching_timestamps_dates_or_oids() {
        let sros = nokia_sros();
        assert_eq!(
            highlights(&sros, "   10.1.2.1/31      fe80::1/64   2001:db8::1:2"),
            pairs(&[
                ("ipv4", "10.1.2.1/31"),
                ("ipv6", "fe80::1/64"),
                ("ipv6", "2001:db8::1:2")
            ])
        );
        assert_eq!(
            highlights(&sros, "Hardware Address  : 1A:2B:3C:4D:5E:6F"),
            pairs(&[("mac", "1A:2B:3C:4D:5E:6F")])
        );
        assert_eq!(
            highlights(&sros, "::ffff:10.0.0.1"),
            pairs(&[("ipv4", "10.0.0.1")])
        );
        assert_eq!(
            highlights(
                &sros,
                "2024/01/01 12:00:00.000 UTC 1.3.6.1.4.1 TiMOS-C-23.10.R1"
            ),
            pairs(&[])
        );
    }

    #[test]
    fn builtin_profiles_highlight_sr_os_tables_logs_and_md_cli_context() {
        let sros = nokia_sros();
        assert_eq!(
            highlights(
                &sros,
                "to-pe2                           Up        Up/Down     Network 1/1/c1/1:0"
            ),
            pairs(&[
                ("port", "1/1/c1/1"),
                ("state-up", "Up"),
                ("state-up", "Up"),
                ("state-down", "Down")
            ])
        );
        assert_eq!(
            highlights(
                &sros,
                "1/1/c2        Down       Down          conn   lag-10"
            ),
            pairs(&[
                ("port", "1/1/c2"),
                ("port", "lag-10"),
                ("state-down", "Down"),
                ("state-down", "Down")
            ])
        );
        assert_eq!(
            highlights(
                &sros,
                "                65000       0    0 00h05m12s Connect"
            ),
            pairs(&[("bgp-transitional", "Connect")])
        );
        assert_eq!(
            highlights(
                &sros,
                "12 2024/01/01 12:00:00.000 UTC MAJOR: BGP #2002 Base \"Peer 1: 10.0.0.2: moved from higher state ESTABLISHED to lower state IDLE\""
            ),
            pairs(&[
                ("ipv4", "10.0.0.2"),
                ("state-up", "ESTABLISHED"),
                ("bgp-transitional", "IDLE"),
                ("severity-major", "MAJOR")
            ])
        );
        assert_eq!(
            highlights(&sros, "MINOR: MGMT_CORE #2201: Unknown element - \"foo\""),
            pairs(&[("severity-minor", "MINOR")])
        );
        assert_eq!(
            highlights(&sros, "Error: Bad command."),
            pairs(&[("cli-error", "Error: Bad command.")])
        );
        assert_eq!(
            highlights(
                &sros,
                "*(ex)[/configure router \"Base\" interface \"to-pe2\"]"
            ),
            pairs(&[("mdcli-config-mode", "*(ex)")])
        );
        assert_eq!(highlights(&sros, "[/]"), pairs(&[]));
    }

    #[test]
    fn builtin_profiles_distinguish_sr_os_administrative_shutdown_from_its_negation() {
        let sros = nokia_sros();
        assert_eq!(
            highlights(&sros, "            shutdown"),
            pairs(&[("state-admin-down", "shutdown")])
        );
        assert_eq!(highlights(&sros, "            no shutdown"), pairs(&[]));
        assert_eq!(
            highlights(&sros, "        admin-state disable"),
            pairs(&[("state-admin-down", "disable")])
        );
    }

    #[test]
    fn builtin_profiles_highlight_sr_linux_tables_prompts_and_commit_results() {
        let srl = nokia_srlinux();
        assert_eq!(
            highlights(
                &srl,
                "| ethernet-1/1        | enable      | up          | 25G   |"
            ),
            pairs(&[
                ("interface", "ethernet-1/1"),
                ("state-up", "enable"),
                ("state-up", "up")
            ])
        );
        assert_eq!(
            highlights(
                &srl,
                "| ethernet-1/3        | disable     | down        | 25G   |"
            ),
            pairs(&[
                ("interface", "ethernet-1/3"),
                ("state-admin-down", "disable"),
                ("state-down", "down")
            ])
        );
        assert_eq!(
            highlights(
                &srl,
                "| default  | 10.0.0.3 | ebgp | S | 65003 | active | ipv4-unicast | mgmt0.0 |"
            ),
            pairs(&[
                ("ipv4", "10.0.0.3"),
                ("interface", "mgmt0.0"),
                ("bgp-transitional", "active")
            ])
        );
        assert_eq!(
            highlights(
                &srl,
                "--{ + candidate shared default }--[ interface ethernet-1/1 ]--"
            ),
            pairs(&[
                ("interface", "ethernet-1/1"),
                (
                    "prompt-context",
                    "--{ + candidate shared default }--[ interface ethernet-1/1 ]--"
                ),
                ("prompt-candidate", "--{ + candidate shared default }--"),
            ])
        );
        assert_eq!(
            highlights(
                &srl,
                "All changes have been committed. Leaving candidate mode."
            ),
            pairs(&[("commit-ok", "All changes have been committed")])
        );
        assert_eq!(
            highlights(&srl, "Error: Path '/interface[name=foo]' is not valid"),
            pairs(&[(
                "cli-error",
                "Error: Path '/interface[name=foo]' is not valid"
            )])
        );
    }

    #[test]
    fn builtin_profiles_leave_sr_linux_yang_leaf_names_and_table_headers_alone() {
        let srl = nokia_srlinux();
        assert_eq!(
            highlights(&srl, "    oper-down-reason port-admin-disabled"),
            pairs(&[])
        );
        assert_eq!(
            highlights(
                &srl,
                "| Net-Inst | Peer | State | Uptime | AFI/SAFI | [Rx/Active/Tx] |"
            ),
            pairs(&[])
        );
    }

    #[test]
    fn appends_missing_builtins_after_existing_profiles() {
        let custom = KeywordHighlightProfile {
            id: "custom".into(),
            name: "Custom".into(),
            rules: vec![],
        };
        let ids: Vec<String> = with_builtin_highlight_profiles(&[custom])
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, ["custom", "muxus-nokia-sros", "muxus-nokia-srlinux"]);
    }

    #[test]
    fn keeps_an_installed_copy_including_the_users_edits() {
        let edited = KeywordHighlightProfile {
            name: "My SR OS".into(),
            rules: vec![],
            ..nokia_sros()
        };
        assert_eq!(
            with_builtin_highlight_profiles(std::slice::from_ref(&edited)),
            vec![edited, nokia_srlinux()]
        );
    }

    #[test]
    fn never_exceeds_the_profile_limit() {
        let full: Vec<KeywordHighlightProfile> = (0..MAX_KEYWORD_HIGHLIGHT_PROFILES - 1)
            .map(|index| KeywordHighlightProfile {
                id: format!("custom-{index}"),
                name: "Custom".into(),
                rules: vec![],
            })
            .collect();
        assert_eq!(
            with_builtin_highlight_profiles(&full).len(),
            MAX_KEYWORD_HIGHLIGHT_PROFILES
        );
    }

    fn unnamed(profile: KeywordHighlightProfile) -> KeywordHighlightProfile {
        KeywordHighlightProfile {
            rules: profile
                .rules
                .into_iter()
                .map(|rule| KeywordHighlightRule { name: None, ..rule })
                .collect(),
            ..profile
        }
    }

    #[test]
    fn restores_the_shipped_names_on_rules_seeded_without_them() {
        assert_eq!(
            with_builtin_rule_names(&[unnamed(nokia_sros()), unnamed(nokia_srlinux())]),
            builtin_highlight_profiles()
        );
    }

    #[test]
    fn keeps_user_names_changed_patterns_added_rules_and_other_profiles() {
        let rules = unnamed(nokia_sros()).rules;
        let (ipv4, ipv6, rest) = (rules[0].clone(), rules[1].clone(), rules[2..].to_vec());
        let added = KeywordHighlightRule {
            id: "mine".into(),
            keyword: "LAB".into(),
            ..ipv4.clone()
        };
        let mut edited_rules = vec![
            KeywordHighlightRule {
                name: Some("Loopbacks".into()),
                ..ipv4
            },
            KeywordHighlightRule {
                keyword: "fe80::".into(),
                ..ipv6
            },
            added.clone(),
        ];
        edited_rules.extend(rest);
        let edited = KeywordHighlightProfile {
            rules: edited_rules,
            ..nokia_sros()
        };
        let custom = KeywordHighlightProfile {
            id: "custom".into(),
            name: "Custom".into(),
            rules: vec![added],
        };
        let result = with_builtin_rule_names(&[edited, custom.clone()]);
        let names: Vec<Option<&str>> = result[0].rules[..3]
            .iter()
            .map(|rule| rule.name.as_deref())
            .collect();
        assert_eq!(names, [Some("Loopbacks"), None, None]);
        assert_eq!(result[0].rules[3].name.as_deref(), Some("MAC addresses"));
        assert_eq!(result[1], custom);
    }
}
