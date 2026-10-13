//! The highlighting profiles Muxus ships (port of
//! `client/src/builtin-highlight-profiles.ts`).
//!
//! The profiles are built as JSON with the key order the TypeScript writes
//! (`id, name, keyword, caseSensitive, wholeWord, regex, foreground,
//! background`), so preferences seeded by this build are byte-identical to
//! the Electron build's.

use std::sync::LazyLock;

use serde_json::{Map, Value, json};

use muxus_core::api::KeywordHighlightProfile;

use super::highlight_profiles::MAX_KEYWORD_HIGHLIGHT_PROFILES;

// Mid-tone text colors stay readable on both light and dark terminal schemes;
// severities use filled badges so they do not depend on the scheme at all.
const GREEN: &str = "#16a34a";
const RED: &str = "#dc2626";
const AMBER: &str = "#d97706";
const BLUE: &str = "#3b82f6";
const CYAN: &str = "#0891b2";
const PURPLE: &str = "#a855f7";

const HEX_GROUP: &str = r"[\da-f]{1,4}";

pub const NOKIA_SROS_PROFILE_ID: &str = "muxus-nokia-sros";
pub const NOKIA_SRLINUX_PROFILE_ID: &str = "muxus-nokia-srlinux";

/// A regex rule: `{ id, name, keyword, caseSensitive: false, wholeWord: false,
/// regex: true, ...options }`, options overriding in place.
fn pattern(id: &str, name: &str, keyword: &str, options: &[(&str, Value)]) -> Value {
    let mut rule = Map::new();
    rule.insert("id".into(), id.into());
    rule.insert("name".into(), name.into());
    rule.insert("keyword".into(), keyword.into());
    rule.insert("caseSensitive".into(), false.into());
    rule.insert("wholeWord".into(), false.into());
    rule.insert("regex".into(), true.into());
    for (key, value) in options {
        rule.insert((*key).into(), value.clone());
    }
    Value::Object(rule)
}

fn fg(color: &str) -> Vec<(&'static str, Value)> {
    vec![("foreground", color.into())]
}

fn fg_cs(color: &str) -> Vec<(&'static str, Value)> {
    vec![("foreground", color.into()), ("caseSensitive", true.into())]
}

fn major_badge() -> Vec<(&'static str, Value)> {
    vec![
        ("foreground", "#ffffff".into()),
        ("background", "#b91c1c".into()),
    ]
}

fn minor_badge() -> Vec<(&'static str, Value)> {
    vec![
        ("foreground", "#1c1917".into()),
        ("background", "#f59e0b".into()),
    ]
}

// A hyphen counts as part of the word so YANG leaves such as oper-down-reason
// and hold-time up are not colored as states.
fn state(words: &str) -> String {
    format!(r"(?<![\w-])(?:{words})(?![\w-])")
}

/// Address rules shared by both platforms; their IDs repeat across profiles.
fn address_rules() -> Vec<Value> {
    let h = HEX_GROUP;
    vec![
        pattern(
            "ipv4",
            "IPv4 addresses",
            r"(?<![\d.])(?:\d{1,3}\.){3}\d{1,3}(?:/\d{1,2})?(?!\.?\d)",
            &fg(CYAN),
        ),
        pattern(
            "ipv6",
            "IPv6 addresses",
            &format!(
                r"(?<![\w:])(?:(?:{h}:){{7}}{h}|(?:{h}:){{1,7}}:(?:{h}(?::{h}){{0,6}})?|::(?:{h}(?::{h}){{0,6}})?)(?:/\d{{1,3}})?(?![\w:]|\.\d)"
            ),
            &fg(CYAN),
        ),
        pattern(
            "mac",
            "MAC addresses",
            r"\b[\da-f]{2}(?:[:-][\da-f]{2}){5}\b",
            &fg(PURPLE),
        ),
    ]
}

fn profile(id: &str, name: &str, rules: Vec<Value>) -> Value {
    json!({ "id": id, "name": name, "rules": rules })
}

fn nokia_sros() -> Value {
    let mut rules = address_rules();
    rules.extend([
        pattern(
            "port",
            "Ports and LAGs",
            r"\b\d{1,2}/\d{1,2}/(?:c\d{1,3}(?:/\d{1,3})?|\d{1,3})\b|\blag-\d+\b",
            &fg(BLUE),
        ),
        pattern(
            "state-up",
            "Up and enabled",
            &state("up|established|inservice|enabled?"),
            &fg(GREEN),
        ),
        pattern(
            "state-admin-down",
            "Administratively down",
            &format!(r"{}|(?<!no )\bshutdown\b", state("disabled?|ghost")),
            &fg(AMBER),
        ),
        pattern(
            "state-down",
            "Down and failed",
            &state("down|outofservice|failed|failure"),
            &fg(RED),
        ),
        pattern(
            "bgp-transitional",
            "BGP sessions not established",
            r"\b(?:Idle|Connect|OpenSent|OpenConfirm|IDLE|CONNECT|ACTIVE|OPENSENT|OPENCONFIRM)\b",
            &fg_cs(AMBER),
        ),
        pattern(
            "severity-major",
            "Critical and major alarms",
            r"\b(?:CRITICAL|MAJOR)\b",
            &{
                let mut options = major_badge();
                options.push(("caseSensitive", true.into()));
                options
            },
        ),
        pattern(
            "severity-minor",
            "Minor alarms and warnings",
            r"\b(?:MINOR|WARNING)\b",
            &{
                let mut options = minor_badge();
                options.push(("caseSensitive", true.into()));
                options
            },
        ),
        pattern(
            "severity-cleared",
            "Cleared alarms",
            r"\bCLEARED\b",
            &[
                ("foreground", "#ffffff".into()),
                ("background", "#15803d".into()),
                ("caseSensitive", true.into()),
            ],
        ),
        pattern("severity-info", "Info messages", r"\bINFO\b", &fg_cs(BLUE)),
        pattern("cli-error", "CLI errors", r"^Error:.*", &fg_cs(RED)),
        // The MD-CLI context line: "*(ex)[/configure]" means uncommitted
        // changes in an exclusive, private, global or read-only configuration session.
        pattern(
            "mdcli-config-mode",
            "MD-CLI configuration mode",
            r"^[!*]*\((?:ex|pr|gl|ro)\)(?=\[)",
            &fg(AMBER),
        ),
    ]);
    profile(NOKIA_SROS_PROFILE_ID, "Nokia SR OS", rules)
}

fn nokia_srlinux() -> Value {
    let mut rules = address_rules();
    rules.extend([
        pattern(
            "interface",
            "Interfaces",
            r"\b(?:ethernet-\d+/\d+(?:/\d+)?|mgmt\d+|lag\d+|irb\d+|lo\d+|system\d+)(?:\.\d+)?\b",
            &fg(BLUE),
        ),
        pattern(
            "state-up",
            "Up and enabled",
            &state("up|enabled?|established"),
            &fg(GREEN),
        ),
        pattern(
            "state-admin-down",
            "Administratively down",
            &state("disabled?"),
            &fg(AMBER),
        ),
        pattern(
            "state-down",
            "Down and failed",
            &state("down|failed|failure"),
            &fg(RED),
        ),
        // Lowercase only: table headers such as "Active" are not session states.
        pattern(
            "bgp-transitional",
            "BGP sessions not established",
            &state("idle|active|connect|opensent|openconfirm"),
            &fg_cs(AMBER),
        ),
        pattern(
            "severity-major",
            "Critical and major alarms",
            &state("critical|major"),
            &major_badge(),
        ),
        pattern(
            "severity-minor",
            "Minor alarms and warnings",
            &state("minor|warning"),
            &minor_badge(),
        ),
        pattern("cli-error", "CLI errors", r"^Error:.*", &fg_cs(RED)),
        pattern("cli-warning", "CLI warnings", r"^Warning:.*", &fg_cs(AMBER)),
        json!({
            "id": "commit-ok",
            "name": "Commit succeeded",
            "keyword": "All changes have been committed",
            "foreground": GREEN,
            "caseSensitive": true,
            "wholeWord": false,
        }),
        // The context line above the prompt, e.g. "--{ + candidate shared default }--[  ]--".
        pattern(
            "prompt-context",
            "Prompt context",
            r"^--\{[^}]*\}--\[[^\]]*\]--",
            &fg(BLUE),
        ),
        pattern(
            "prompt-candidate",
            "Candidate mode",
            r"^--\{[^}]*\bcandidate\b[^}]*\}--",
            &fg(AMBER),
        ),
    ]);
    profile(NOKIA_SRLINUX_PROFILE_ID, "Nokia SR Linux", rules)
}

static BUILTIN_JSON: LazyLock<Vec<Value>> = LazyLock::new(|| vec![nokia_sros(), nokia_srlinux()]);

static BUILTIN_TYPED: LazyLock<Vec<KeywordHighlightProfile>> = LazyLock::new(|| {
    BUILTIN_JSON
        .iter()
        .map(|profile| {
            serde_json::from_value(profile.clone()).expect("built-in profiles are well formed")
        })
        .collect()
});

/// The profiles Muxus ships, as JSON in the TypeScript's key order. They are
/// ordinary, editable profiles with stable IDs.
pub fn builtin_highlight_profiles_json() -> &'static [Value] {
    &BUILTIN_JSON
}

/// The profiles Muxus ships.
pub fn builtin_highlight_profiles() -> &'static [KeywordHighlightProfile] {
    &BUILTIN_TYPED
}

/// The shipped Nokia SR OS profile.
pub fn nokia_sros_highlight_profile() -> &'static KeywordHighlightProfile {
    &BUILTIN_TYPED[0]
}

/// The shipped Nokia SR Linux profile.
pub fn nokia_srlinux_highlight_profile() -> &'static KeywordHighlightProfile {
    &BUILTIN_TYPED[1]
}

fn profile_id(profile: &Value) -> Option<&str> {
    profile.get("id").and_then(Value::as_str)
}

/// Append each built-in profile that is not installed, within the profile
/// limit (on stored JSON, as the preference migration runs it).
pub fn with_builtin_highlight_profiles_json(profiles: &[Value]) -> Vec<Value> {
    let mut next = profiles.to_vec();
    let installed: Vec<&str> = profiles.iter().filter_map(profile_id).collect();
    for profile in builtin_highlight_profiles_json() {
        if next.len() >= MAX_KEYWORD_HIGHLIGHT_PROFILES {
            break;
        }
        if !installed.contains(&profile_id(profile).unwrap_or_default()) {
            next.push(profile.clone());
        }
    }
    next
}

/// [`with_builtin_highlight_profiles_json`] for typed profiles.
pub fn with_builtin_highlight_profiles(
    profiles: &[KeywordHighlightProfile],
) -> Vec<KeywordHighlightProfile> {
    let mut next = profiles.to_vec();
    for profile in builtin_highlight_profiles() {
        if next.len() >= MAX_KEYWORD_HIGHLIGHT_PROFILES {
            break;
        }
        if !profiles.iter().any(|installed| installed.id == profile.id) {
            next.push(profile.clone());
        }
    }
    next
}

/// Name installed built-in rules that were seeded before they had names.
/// Only a rule that still has its shipped ID and pattern, and no name of its
/// own, changes (on stored JSON; the name lands after the other keys, as
/// `{ ...rule, name }` puts it).
pub fn with_builtin_rule_names_json(profiles: &[Value]) -> Vec<Value> {
    profiles
        .iter()
        .map(|profile| {
            let Some(builtin) = builtin_highlight_profiles_json().iter().find(|candidate| {
                profile_id(candidate).is_some() && profile_id(candidate) == profile_id(profile)
            }) else {
                return profile.clone();
            };
            let shipped = builtin["rules"].as_array().cloned().unwrap_or_default();
            let mut next = profile.clone();
            if let Some(rules) = next.get_mut("rules").and_then(Value::as_array_mut) {
                for rule in rules.iter_mut() {
                    let Some(record) = rule.as_object_mut() else {
                        continue;
                    };
                    let unnamed = !record.get("name").is_some_and(is_truthy);
                    let shipped_rule = shipped.iter().find(|candidate| {
                        candidate.get("id") == record.get("id") && record.contains_key("id")
                    });
                    if let Some(shipped_rule) = shipped_rule
                        && unnamed
                        && shipped_rule.get("name").is_some_and(is_truthy)
                        && shipped_rule.get("keyword") == record.get("keyword")
                    {
                        record.insert("name".into(), shipped_rule["name"].clone());
                    }
                }
            }
            next
        })
        .collect()
}

/// [`with_builtin_rule_names_json`] for typed profiles.
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
            let mut next = profile.clone();
            for rule in &mut next.rules {
                let unnamed = rule.name.as_deref().is_none_or(str::is_empty);
                if let Some(shipped) = builtin
                    .rules
                    .iter()
                    .find(|candidate| candidate.id == rule.id)
                    && unnamed
                    && shipped.name.as_deref().is_some_and(|name| !name.is_empty())
                    && shipped.keyword == rule.keyword
                {
                    rule.name = shipped.name.clone();
                }
            }
            next
        })
        .collect()
}

/// JavaScript truthiness of a JSON value.
pub(crate) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}
