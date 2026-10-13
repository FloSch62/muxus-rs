//! Reusable keyword highlighting profiles: validation, the portable profile
//! file, and rules edited as JSON (port of `client/src/highlight-profiles.ts`).
//!
//! Validators take `serde_json::Value` because, as in the TypeScript, they
//! judge untrusted stored or imported data before it becomes typed.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use muxus_core::api::{KeywordHighlightProfile, KeywordHighlightRule};
use muxus_core::js;

use super::command_buttons::{bounded_string, new_preference_id};

pub const HIGHLIGHT_PROFILE_FORMAT: &str = "muxus-keyword-highlighting-profiles";
/// Version 2 added regex rules.
pub const HIGHLIGHT_PROFILE_VERSION: u32 = 2;
/// Files without regex rules keep version 1 so older releases can still import them.
const LITERAL_HIGHLIGHT_PROFILE_VERSION: u32 = 1;
pub const MAX_HIGHLIGHT_PROFILE_FILE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_KEYWORD_HIGHLIGHT_PROFILES: usize = 100;
pub const MAX_KEYWORD_HIGHLIGHT_RULES: usize = 100;

/// The portable file shared by the highlighting settings import/export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightProfileDocument {
    pub format: String,
    pub version: u32,
    pub created_at: String,
    pub profiles: Vec<KeywordHighlightProfile>,
}

/// `/^#[0-9a-f]{6}$/i`.
pub fn valid_hex_color(value: &Value) -> bool {
    value.as_str().is_some_and(is_hex_color)
}

/// `/^#[0-9a-f]{6}$/i` on text.
pub fn is_hex_color(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit)
}

/// `validKeywordHighlightRule`.
pub fn valid_keyword_highlight_rule(value: &Value) -> bool {
    let Some(rule) = value.as_object() else {
        return false;
    };
    let get = |key: &str| rule.get(key);
    bounded_string(get("id").unwrap_or(&Value::Null), 100).is_some()
        && get("name").is_none_or(|name| bounded_string(name, 100).is_some())
        && bounded_string(get("keyword").unwrap_or(&Value::Null), 500).is_some()
        && get("foreground").is_some_and(valid_hex_color)
        && get("background").is_none_or(valid_hex_color)
        && get("caseSensitive").is_some_and(Value::is_boolean)
        && get("wholeWord").is_some_and(Value::is_boolean)
        && get("regex").is_none_or(Value::is_boolean)
}

/// `isKeywordHighlightProfileArray`: bounded, unique profile ids, named
/// profiles, and valid rules with unique ids.
pub fn is_keyword_highlight_profile_array(value: &Value) -> bool {
    let Some(entries) = value.as_array() else {
        return false;
    };
    if entries.len() > MAX_KEYWORD_HIGHLIGHT_PROFILES {
        return false;
    }
    let mut profile_ids = HashSet::new();
    entries.iter().all(|entry| {
        let Some(profile) = entry.as_object() else {
            return false;
        };
        let Some(id) = bounded_string(profile.get("id").unwrap_or(&Value::Null), 200) else {
            return false;
        };
        let Some(name) = bounded_string(profile.get("name").unwrap_or(&Value::Null), 200) else {
            return false;
        };
        let Some(rules) = profile.get("rules").and_then(Value::as_array) else {
            return false;
        };
        if profile_ids.contains(id)
            || js::js_trim(name).is_empty()
            || rules.len() > MAX_KEYWORD_HIGHLIGHT_RULES
        {
            return false;
        }
        let mut rule_ids = HashSet::new();
        let rules_ok = rules.iter().all(|rule| {
            if !valid_keyword_highlight_rule(rule) {
                return false;
            }
            rule_ids.insert(rule["id"].as_str().unwrap_or_default().to_owned())
        });
        if !rules_ok {
            return false;
        }
        profile_ids.insert(id.to_owned());
        true
    })
}

/// [`is_keyword_highlight_profile_array`] for typed profiles.
pub fn are_valid_highlight_profiles(profiles: &[KeywordHighlightProfile]) -> bool {
    serde_json::to_value(profiles).is_ok_and(|value| is_keyword_highlight_profile_array(&value))
}

/// Strip unknown fields and a false regex flag (`copyProfile`).
pub fn copy_profile(profile: &KeywordHighlightProfile) -> KeywordHighlightProfile {
    KeywordHighlightProfile {
        id: profile.id.clone(),
        name: profile.name.clone(),
        rules: profile
            .rules
            .iter()
            .map(|rule| KeywordHighlightRule {
                regex: rule.regex.filter(|regex| *regex),
                ..rule.clone()
            })
            .collect(),
    }
}

fn has_regex_rule(profiles: &[KeywordHighlightProfile]) -> bool {
    profiles
        .iter()
        .any(|profile| profile.rules.iter().any(|rule| rule.regex == Some(true)))
}

/// Build the portable file for an export.
pub fn create_highlight_profile_document(
    profiles: &[KeywordHighlightProfile],
) -> Result<HighlightProfileDocument, String> {
    if profiles.is_empty() || !are_valid_highlight_profiles(profiles) {
        return Err("Select at least one valid highlighting profile to export.".into());
    }
    Ok(HighlightProfileDocument {
        format: HIGHLIGHT_PROFILE_FORMAT.into(),
        // A release that predates regex rules would otherwise match their
        // patterns as literal text; the newer version makes it refuse the file.
        version: if has_regex_rule(profiles) {
            HIGHLIGHT_PROFILE_VERSION
        } else {
            LITERAL_HIGHLIGHT_PROFILE_VERSION
        },
        created_at: js::now_iso(),
        profiles: profiles.iter().map(copy_profile).collect(),
    })
}

/// A JavaScript number the way template strings print it.
pub(crate) fn js_number_text(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// Parse an untrusted, standalone highlighting-profile file.
pub fn parse_highlight_profile_document(text: &str) -> Result<HighlightProfileDocument, String> {
    let parsed: Value = serde_json::from_str(text)
        .map_err(|_| "This highlighting profile file is not valid JSON.".to_owned())?;
    let Some(record) = parsed.as_object().filter(|record| {
        record.get("format").and_then(Value::as_str) == Some(HIGHLIGHT_PROFILE_FORMAT)
    }) else {
        return Err("This is not a Muxus highlighting profile file.".into());
    };
    let version = record.get("version").and_then(Value::as_f64);
    let version = match version {
        Some(v) if v == f64::from(LITERAL_HIGHLIGHT_PROFILE_VERSION) => {
            LITERAL_HIGHLIGHT_PROFILE_VERSION
        }
        Some(v) if v == f64::from(HIGHLIGHT_PROFILE_VERSION) => HIGHLIGHT_PROFILE_VERSION,
        Some(v) => {
            return Err(format!(
                "Highlighting profile version {} is not supported.",
                js_number_text(v)
            ));
        }
        None => {
            return Err("This file is missing a supported highlighting profile version.".into());
        }
    };
    let created_at = record.get("createdAt").and_then(Value::as_str);
    let profiles = record.get("profiles").unwrap_or(&Value::Null);
    let Some(created_at) = created_at.filter(|at| js::parse_timestamp(at).is_some()) else {
        return Err("The highlighting profile file is incomplete or invalid.".into());
    };
    if !is_keyword_highlight_profile_array(profiles)
        || profiles.as_array().is_none_or(Vec::is_empty)
    {
        return Err("The highlighting profile file is incomplete or invalid.".into());
    }
    let profiles: Vec<KeywordHighlightProfile> = serde_json::from_value(profiles.clone())
        .map_err(|_| "The highlighting profile file is incomplete or invalid.".to_owned())?;
    // Version 1 predates regex rules, and a release of that era would match
    // the pattern as literal text. A version 1 file that claims one is mislabeled.
    if version == LITERAL_HIGHLIGHT_PROFILE_VERSION && has_regex_rule(&profiles) {
        return Err(format!(
            "This file contains regex rules, which need highlighting profile version {HIGHLIGHT_PROFILE_VERSION}; it says version {LITERAL_HIGHLIGHT_PROFILE_VERSION}."
        ));
    }
    Ok(HighlightProfileDocument {
        format: HIGHLIGHT_PROFILE_FORMAT.into(),
        version,
        created_at: created_at.to_owned(),
        profiles: profiles.iter().map(copy_profile).collect(),
    })
}

/// Replace matching IDs and retain unrelated profiles during an import.
pub fn merge_highlight_profiles(
    current: &[KeywordHighlightProfile],
    imported: &[KeywordHighlightProfile],
) -> Result<Vec<KeywordHighlightProfile>, String> {
    let mut merged: Vec<KeywordHighlightProfile> = current.to_vec();
    for profile in imported {
        match merged.iter_mut().find(|existing| existing.id == profile.id) {
            Some(existing) => *existing = profile.clone(),
            None => merged.push(profile.clone()),
        }
    }
    if merged.len() > MAX_KEYWORD_HIGHLIGHT_PROFILES {
        return Err(format!(
            "This import would exceed the limit of {MAX_KEYWORD_HIGHLIGHT_PROFILES} highlighting profiles. Delete an existing profile and try again."
        ));
    }
    if !are_valid_highlight_profiles(&merged) {
        return Err("The merged highlighting profiles are invalid.".into());
    }
    Ok(merged)
}

/// Rules as the JSON editor shows them: no IDs, fields in reading order.
pub fn keyword_highlight_rules_to_json(rules: &[KeywordHighlightRule]) -> String {
    let entries: Vec<Value> = rules
        .iter()
        .map(|rule| {
            let mut entry = Map::new();
            if let Some(name) = rule.name.as_deref().filter(|name| !name.is_empty()) {
                entry.insert("name".into(), name.into());
            }
            entry.insert("keyword".into(), rule.keyword.clone().into());
            entry.insert("regex".into(), rule.regex.unwrap_or(false).into());
            entry.insert("caseSensitive".into(), rule.case_sensitive.into());
            entry.insert("wholeWord".into(), rule.whole_word.into());
            entry.insert("foreground".into(), rule.foreground.clone().into());
            if let Some(background) = rule
                .background
                .as_deref()
                .filter(|background| !background.is_empty())
            {
                entry.insert("background".into(), background.into());
            }
            Value::Object(entry)
        })
        .collect();
    serde_json::to_string_pretty(&entries).expect("JSON values always serialize")
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

/// Parse rules edited as JSON. Flags default to false and IDs are optional:
/// an entry without one keeps the ID of the rule at its position, so rules
/// pasted from an exported profile file work as well as hand-written ones.
/// Returns an error naming the first problem and its 1-based rule number.
///
/// `pattern_error` checks a rule's pattern the way the terminal will match it
/// (`keywordPatternError`), and returns why it cannot be used.
pub fn parse_keyword_highlight_rules_json(
    text: &str,
    previous: &[KeywordHighlightRule],
    pattern_error: impl Fn(&KeywordHighlightRule) -> Option<String>,
) -> Result<Vec<KeywordHighlightRule>, String> {
    let parsed: Value =
        serde_json::from_str(text).map_err(|error| format!("This is not valid JSON: {error}"))?;
    let Some(entries) = parsed.as_array() else {
        return Err("Expected a JSON array of rules: [ { ... } ].".into());
    };
    if entries.len() > MAX_KEYWORD_HIGHLIGHT_RULES {
        return Err(format!(
            "A rule list holds at most {MAX_KEYWORD_HIGHLIGHT_RULES} rules."
        ));
    }
    let mut explicit_ids: HashSet<String> = HashSet::new();
    let mut records = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let label = format!("Rule {}", index + 1);
        let Some(record) = entry.as_object() else {
            return Err(format!("{label} must be an object."));
        };
        if let Some(unknown) = record
            .keys()
            .find(|key| !JSON_RULE_FIELDS.contains(&key.as_str()))
        {
            return Err(format!("{label} has an unknown field \"{unknown}\"."));
        }
        if let Some(id) = record.get("id") {
            match bounded_string(id, 100) {
                Some(id) if !explicit_ids.contains(id) => {
                    explicit_ids.insert(id.to_owned());
                }
                _ => {
                    return Err(format!(
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
        let get = |key: &str| record.get(key);
        let Some(keyword) = bounded_string(get("keyword").unwrap_or(&Value::Null), 500) else {
            return Err(format!(
                "{label} needs a \"keyword\" of 1 to 500 characters."
            ));
        };
        if let Some(name) = get("name")
            && name.as_str() != Some("")
            && bounded_string(name, 100).is_none()
        {
            return Err(format!(
                "{label}: \"name\" must be text of up to 100 characters."
            ));
        }
        if !get("foreground").is_some_and(valid_hex_color) {
            return Err(format!(
                "{label}: \"foreground\" must be a color such as \"#ff0000\"."
            ));
        }
        if let Some(background) = get("background").filter(|background| !background.is_null())
            && !valid_hex_color(background)
        {
            return Err(format!(
                "{label}: \"background\" must be a color such as \"#7f1d1d\", or left out."
            ));
        }
        for flag in ["regex", "caseSensitive", "wholeWord"] {
            if get(flag).is_some_and(|value| !value.is_boolean()) {
                return Err(format!("{label}: \"{flag}\" must be true or false."));
            }
        }
        let explicit = get("id").and_then(Value::as_str);
        let mut id = explicit
            .map(str::to_owned)
            .or_else(|| previous.get(index).map(|rule| rule.id.clone()));
        if id.is_none()
            || (explicit.is_none() && id.as_ref().is_some_and(|id| used_ids.contains(id)))
        {
            id = Some(new_preference_id("highlight"));
        }
        let id = id.expect("set above");
        used_ids.insert(id.clone());
        let rule = KeywordHighlightRule {
            id,
            name: get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            keyword: keyword.to_owned(),
            foreground: get("foreground")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            background: get("background")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            case_sensitive: get("caseSensitive") == Some(&Value::Bool(true)),
            whole_word: get("wholeWord") == Some(&Value::Bool(true)),
            regex: (get("regex") == Some(&Value::Bool(true))).then_some(true),
        };
        if let Some(problem) = pattern_error(&rule) {
            return Err(format!("{label}: {problem}"));
        }
        rules.push(rule);
    }
    Ok(rules)
}
