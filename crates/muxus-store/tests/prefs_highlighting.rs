//! Port of `tests/unit/client/highlight-profiles.test.ts` and the storage
//! parts of `builtin-highlight-profiles.test.ts` (matching the patterns
//! against terminal text belongs to the terminal view).

use muxus_core::api::{KeywordHighlightProfile, KeywordHighlightRule};
use muxus_store::prefs::builtin_highlight_profiles::*;
use muxus_store::prefs::highlight_profiles::*;
use serde_json::{Value, json};

fn nokia() -> KeywordHighlightProfile {
    KeywordHighlightProfile {
        id: "nokia-sros".into(),
        name: "Nokia SR OS".into(),
        rules: vec![KeywordHighlightRule {
            id: "major-alarm".into(),
            name: None,
            keyword: "MAJOR".into(),
            foreground: "#ffffff".into(),
            background: Some("#b91c1c".into()),
            case_sensitive: true,
            whole_word: true,
            regex: None,
        }],
    }
}

fn with_rules(rules: Vec<KeywordHighlightRule>) -> KeywordHighlightProfile {
    KeywordHighlightProfile { rules, ..nokia() }
}

fn rule0() -> KeywordHighlightRule {
    nokia().rules[0].clone()
}

/// Stand-in for the terminal's pattern check: unbalanced groups do not compile.
fn pattern_error(rule: &KeywordHighlightRule) -> Option<String> {
    if rule.regex != Some(true) {
        return None;
    }
    let mut depth = 0i32;
    let mut escaped = false;
    for c in rule.keyword.chars() {
        match (escaped, c) {
            (true, _) => escaped = false,
            (false, '\\') => escaped = true,
            (false, '(') => depth += 1,
            (false, ')') => depth -= 1,
            _ => {}
        }
    }
    (depth != 0).then(|| {
        format!(
            "Invalid regular expression: /{}/: Unterminated group",
            rule.keyword
        )
    })
}

#[test]
fn round_trips_a_bounded_versioned_profile_document() {
    let document = create_highlight_profile_document(&[nokia()]).unwrap();
    assert_eq!(document.format, HIGHLIGHT_PROFILE_FORMAT);
    assert_eq!(document.profiles, vec![nokia()]);
    assert_eq!(
        parse_highlight_profile_document(&serde_json::to_string(&document).unwrap()).unwrap(),
        document
    );
}

#[test]
fn keeps_literal_only_files_at_version_1() {
    let document = create_highlight_profile_document(&[nokia()]).unwrap();
    assert_eq!(document.version, 1);
    assert_eq!(
        parse_highlight_profile_document(&serde_json::to_string(&document).unwrap())
            .unwrap()
            .version,
        1
    );
}

#[test]
fn writes_version_2_when_a_rule_is_a_regex() {
    let regex_rule = KeywordHighlightRule {
        id: "port".into(),
        keyword: r"\d+/\d+/\d+".into(),
        regex: Some(true),
        ..rule0()
    };
    let profile = with_rules(vec![rule0(), regex_rule]);
    let document = create_highlight_profile_document(std::slice::from_ref(&profile)).unwrap();
    assert_eq!(document.version, HIGHLIGHT_PROFILE_VERSION);
    assert_eq!(HIGHLIGHT_PROFILE_VERSION, 2);
    assert_eq!(
        parse_highlight_profile_document(&serde_json::to_string(&document).unwrap())
            .unwrap()
            .profiles,
        vec![profile]
    );
}

#[test]
fn rejects_a_version_1_file_that_claims_regex_rules() {
    let regex_rule = KeywordHighlightRule {
        keyword: r"\bMAJOR\b".into(),
        regex: Some(true),
        ..rule0()
    };
    let document = create_highlight_profile_document(&[with_rules(vec![regex_rule])]).unwrap();
    let mut value = serde_json::to_value(&document).unwrap();
    value["version"] = json!(1);
    let error = parse_highlight_profile_document(&value.to_string()).unwrap_err();
    assert!(
        error.contains("regex rules, which need highlighting profile version 2"),
        "{error}"
    );
}

#[test]
fn drops_a_false_regex_flag_rather_than_exporting_it() {
    let profile = with_rules(vec![KeywordHighlightRule {
        regex: Some(false),
        ..rule0()
    }]);
    let document = create_highlight_profile_document(&[profile]).unwrap();
    assert_eq!(document.profiles[0].rules[0].regex, None);
    assert!(!serde_json::to_string(&document).unwrap().contains("regex"));
}

#[test]
fn round_trips_an_optional_rule_name_and_rejects_an_empty_one() {
    let named = with_rules(vec![KeywordHighlightRule {
        name: Some("Major alarms".into()),
        ..rule0()
    }]);
    let document = create_highlight_profile_document(std::slice::from_ref(&named)).unwrap();
    assert_eq!(
        parse_highlight_profile_document(&serde_json::to_string(&document).unwrap())
            .unwrap()
            .profiles,
        vec![named]
    );
    let mut empty = serde_json::to_value(nokia()).unwrap();
    empty["rules"][0]["name"] = json!("");
    assert!(!is_keyword_highlight_profile_array(&json!([empty])));
}

#[test]
fn rejects_a_non_boolean_regex_flag() {
    let mut profile = serde_json::to_value(nokia()).unwrap();
    profile["rules"][0]["regex"] = json!("yes");
    assert!(!is_keyword_highlight_profile_array(&json!([profile])));
}

#[test]
fn rejects_unrelated_future_and_malformed_documents() {
    assert!(
        parse_highlight_profile_document("{no json")
            .unwrap_err()
            .contains("valid JSON")
    );
    let created_at = muxus_core::js::now_iso();
    let other = json!({ "format": "another-format", "version": 2, "createdAt": created_at, "profiles": [nokia()] });
    assert!(
        parse_highlight_profile_document(&other.to_string())
            .unwrap_err()
            .contains("not a Muxus highlighting profile")
    );
    let future = json!({ "format": HIGHLIGHT_PROFILE_FORMAT, "version": 3, "createdAt": created_at, "profiles": [nokia()] });
    assert!(
        parse_highlight_profile_document(&future.to_string())
            .unwrap_err()
            .contains("not supported")
    );
    let mut red = serde_json::to_value(nokia()).unwrap();
    red["rules"][0]["foreground"] = json!("red");
    assert!(!is_keyword_highlight_profile_array(&json!([red])));
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
fn allows_replacements_at_the_limit_but_rejects_one_beyond_it() {
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
    let one_more = KeywordHighlightProfile {
        id: "one-too-many".into(),
        name: "One too many".into(),
        ..nokia()
    };
    assert!(
        merge_highlight_profiles(&existing, &[one_more])
            .unwrap_err()
            .contains("limit of 100 highlighting profiles")
    );
}

#[test]
fn rejects_duplicate_profile_and_rule_ids() {
    assert!(!are_valid_highlight_profiles(&[nokia(), nokia()]));
    assert!(!are_valid_highlight_profiles(&[with_rules(vec![
        rule0(),
        rule0()
    ])]));
}

fn edited_rules() -> Vec<KeywordHighlightRule> {
    vec![
        KeywordHighlightRule {
            name: Some("Major alarms".into()),
            ..rule0()
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
fn json_rules_hide_ids_and_round_trip_every_field() {
    let rules = edited_rules();
    let text = keyword_highlight_rules_to_json(&rules);
    assert!(!text.contains("\"id\""));
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        parsed[1],
        json!({ "keyword": r"\d+/\d+/\d+", "regex": true, "caseSensitive": false, "wholeWord": true, "foreground": "#3b82f6" })
    );
    assert_eq!(
        parse_keyword_highlight_rules_json(&text, &rules, pattern_error).unwrap(),
        rules
    );
}

#[test]
fn json_rules_default_flags_keep_positional_ids_and_generate_new_ones() {
    let rules = edited_rules();
    let parsed = parse_keyword_highlight_rules_json(
        &json!([
            { "keyword": "down", "foreground": "#dc2626" },
            { "keyword": "up", "foreground": "#16a34a", "name": "" },
            { "keyword": "idle", "foreground": "#d97706", "background": null },
        ])
        .to_string(),
        &rules,
        pattern_error,
    )
    .unwrap();
    assert_eq!(parsed[0].id, "major-alarm");
    assert_eq!(parsed[1].id, "port");
    assert!(parsed[2].id.starts_with("highlight-"));
    assert_eq!(
        serde_json::to_value(&parsed[0]).unwrap(),
        json!({ "id": "major-alarm", "keyword": "down", "foreground": "#dc2626", "caseSensitive": false, "wholeWord": false })
    );
    assert_eq!(parsed[1].name, None);
    assert_eq!(parsed[2].background, None);
}

#[test]
fn json_rules_pasted_from_a_profile_file_keep_their_ids() {
    let rules = edited_rules();
    let mut first = serde_json::to_value(&rules[1]).unwrap();
    first["id"] = json!("major-alarm");
    let mut second = serde_json::to_value(&rules[0]).unwrap();
    second["id"] = json!("other");
    let parsed = parse_keyword_highlight_rules_json(
        &json!([first, second]).to_string(),
        &rules,
        pattern_error,
    )
    .unwrap();
    assert_eq!(
        parsed
            .iter()
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>(),
        vec!["major-alarm", "other"]
    );
}

#[test]
fn json_rules_do_not_reuse_a_positional_id_an_explicit_entry_claims() {
    let parsed = parse_keyword_highlight_rules_json(
        &json!([
            { "keyword": "a", "foreground": "#ffffff" },
            { "id": "major-alarm", "keyword": "b", "foreground": "#ffffff" },
        ])
        .to_string(),
        &edited_rules(),
        pattern_error,
    )
    .unwrap();
    assert_eq!(parsed[1].id, "major-alarm");
    assert_ne!(parsed[0].id, "major-alarm");
}

#[test]
fn json_rules_name_the_first_problem_with_its_rule_number() {
    let parse =
        |text: String| parse_keyword_highlight_rules_json(&text, &[], pattern_error).unwrap_err();
    assert!(parse("[{".into()).contains("not valid JSON"));
    assert!(parse(json!({ "keyword": "x" }).to_string()).contains("array of rules"));
    assert!(
        parse(json!([{ "foreground": "#ffffff" }]).to_string())
            .contains("Rule 1 needs a \"keyword\"")
    );
    assert!(
        parse(json!([{ "keyword": "x", "foreground": "#ffffff" }, { "keyword": "y", "foreground": "red" }]).to_string())
            .contains("Rule 2: \"foreground\" must be a color")
    );
    assert!(
        parse(
            json!([{ "keyword": "x", "foreground": "#ffffff", "colour": "#000000" }]).to_string()
        )
        .contains("unknown field \"colour\"")
    );
    assert!(
        parse(json!([{ "keyword": "x", "foreground": "#ffffff", "regex": "yes" }]).to_string())
            .contains("\"regex\" must be true or false")
    );
    assert!(
        parse(json!([{ "keyword": "(x", "foreground": "#ffffff", "regex": true }]).to_string())
            .contains("Rule 1: Invalid regular expression")
    );
    assert!(
        parse(
            json!([
                { "id": "a", "keyword": "x", "foreground": "#ffffff" },
                { "id": "a", "keyword": "y", "foreground": "#ffffff" },
            ])
            .to_string()
        )
        .contains("Rule 2 needs a unique \"id\"")
    );
}

#[test]
fn builtin_profiles_are_valid_named_and_survive_an_export() {
    let builtins = builtin_highlight_profiles();
    assert!(are_valid_highlight_profiles(builtins));
    assert!(is_keyword_highlight_profile_array(&Value::Array(
        builtin_highlight_profiles_json().to_vec()
    )));
    for profile in builtins {
        // A name says what each regex is for; the patterns alone are hard to read.
        let names: Vec<_> = profile
            .rules
            .iter()
            .map(|rule| rule.name.clone().unwrap_or_default())
            .collect();
        assert!(names.iter().all(|name| !name.is_empty()));
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
    }
    let document = create_highlight_profile_document(builtins).unwrap();
    assert_eq!(document.version, 2);
    assert_eq!(
        parse_highlight_profile_document(&serde_json::to_string(&document).unwrap())
            .unwrap()
            .profiles,
        builtins
    );
}

#[test]
fn builtins_are_appended_after_existing_profiles() {
    let custom = KeywordHighlightProfile {
        id: "custom".into(),
        name: "Custom".into(),
        rules: vec![],
    };
    let ids: Vec<String> = with_builtin_highlight_profiles(std::slice::from_ref(&custom))
        .into_iter()
        .map(|profile| profile.id)
        .collect();
    assert_eq!(
        ids,
        vec!["custom", "muxus-nokia-sros", "muxus-nokia-srlinux"]
    );

    let edited = KeywordHighlightProfile {
        name: "My SR OS".into(),
        rules: vec![],
        ..nokia_sros_highlight_profile().clone()
    };
    assert_eq!(
        with_builtin_highlight_profiles(std::slice::from_ref(&edited)),
        vec![edited, nokia_srlinux_highlight_profile().clone()]
    );

    let full: Vec<KeywordHighlightProfile> = (0..MAX_KEYWORD_HIGHLIGHT_PROFILES - 1)
        .map(|index| KeywordHighlightProfile {
            id: format!("custom-{index}"),
            ..custom.clone()
        })
        .collect();
    assert_eq!(
        with_builtin_highlight_profiles(&full).len(),
        MAX_KEYWORD_HIGHLIGHT_PROFILES
    );
}

fn unnamed(profile: &KeywordHighlightProfile) -> KeywordHighlightProfile {
    KeywordHighlightProfile {
        rules: profile
            .rules
            .iter()
            .map(|rule| KeywordHighlightRule {
                name: None,
                ..rule.clone()
            })
            .collect(),
        ..profile.clone()
    }
}

#[test]
fn restores_the_shipped_names_on_rules_seeded_without_them() {
    assert_eq!(
        with_builtin_rule_names(&[
            unnamed(nokia_sros_highlight_profile()),
            unnamed(nokia_srlinux_highlight_profile())
        ]),
        builtin_highlight_profiles()
    );
}

#[test]
fn keeps_user_names_changed_patterns_added_rules_and_other_profiles() {
    let rules = unnamed(nokia_sros_highlight_profile()).rules;
    let added = KeywordHighlightRule {
        id: "mine".into(),
        keyword: "LAB".into(),
        ..rules[0].clone()
    };
    let mut edited_rules = vec![
        KeywordHighlightRule {
            name: Some("Loopbacks".into()),
            ..rules[0].clone()
        },
        KeywordHighlightRule {
            keyword: "fe80::".into(),
            ..rules[1].clone()
        },
        added.clone(),
    ];
    edited_rules.extend(rules[2..].iter().cloned());
    let edited = KeywordHighlightProfile {
        rules: edited_rules,
        ..nokia_sros_highlight_profile().clone()
    };
    let custom = KeywordHighlightProfile {
        id: "custom".into(),
        name: "Custom".into(),
        rules: vec![added],
    };
    let result = with_builtin_rule_names(&[edited, custom.clone()]);
    let names: Vec<Option<String>> = result[0].rules[..3]
        .iter()
        .map(|rule| rule.name.clone())
        .collect();
    assert_eq!(names, vec![Some("Loopbacks".into()), None, None]);
    assert_eq!(result[0].rules[3].name.as_deref(), Some("MAC addresses"));
    assert_eq!(result[1], custom);
}

#[test]
fn json_variants_match_the_typed_ones() {
    let seeded: Vec<Value> = [
        nokia_sros_highlight_profile(),
        nokia_srlinux_highlight_profile(),
    ]
    .iter()
    .map(|p| serde_json::to_value(unnamed(p)).unwrap())
    .collect();
    let named: Vec<KeywordHighlightProfile> = with_builtin_rule_names_json(&seeded)
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();
    assert_eq!(named, builtin_highlight_profiles());
    let appended = with_builtin_highlight_profiles_json(&[
        json!({ "id": "custom", "name": "Custom", "rules": [] }),
    ]);
    assert_eq!(appended.len(), 3);
    assert_eq!(&appended[1..], builtin_highlight_profiles_json());
}
