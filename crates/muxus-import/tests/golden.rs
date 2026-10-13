//! Golden cases recorded from the TypeScript build (`tests/fixtures/generate`):
//! every importer result, conversion, helper and command-line parse must
//! produce the same JSON the TypeScript produced.

use std::collections::HashSet;

use muxus_import::launch::{
    ConnectOptions, parse_command_line_launch, parse_command_line_launch_data, parse_connect_target,
};
use muxus_import::{
    ImportedSessionParseResult, ImportedSshStorage, clean_import_name, locale_compare,
    mobaxterm_connections, normalize_import_folder, parse_mobaxterm_sessions,
    parse_securecrt_sessions, securecrt_connections, stable_import_id, unique_import_alias,
};
use serde_json::Value;

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/golden.json")).expect("golden JSON")
}

fn text(value: &Value) -> &str {
    value.as_str().expect("string")
}

type Parse = fn(&str) -> Result<ImportedSessionParseResult, muxus_import::ImportError>;
type Convert =
    fn(&ImportedSessionParseResult, ImportedSshStorage) -> muxus_import::PortableConnections;

fn check_importer(cases: &Value, parse: Parse, convert: Convert) -> usize {
    let mut checked = 0;
    for case in cases.as_array().expect("cases") {
        let name = text(&case["name"]);
        let result = parse(text(&case["input"]));
        if let Some(error) = case.get("error") {
            assert_eq!(
                result
                    .as_ref()
                    .map_err(ToString::to_string)
                    .err()
                    .as_deref(),
                Some(text(error)),
                "{name}: error"
            );
            checked += 1;
            continue;
        }
        let parsed = result.unwrap_or_else(|error| panic!("{name}: rejected: {error}"));
        assert_eq!(
            serde_json::to_value(&parsed).expect("JSON"),
            case["ok"],
            "{name}: parse result"
        );
        for (storage, key) in [
            (ImportedSshStorage::Openssh, "openssh"),
            (ImportedSshStorage::Muxus, "muxus"),
        ] {
            assert_eq!(
                serde_json::to_value(convert(&parsed, storage)).expect("JSON"),
                case[key],
                "{name}: {key} connections"
            );
        }
        checked += 1;
    }
    checked
}

#[test]
fn mobaxterm_matches_the_typescript() {
    let checked = check_importer(
        &golden()["mobaxterm"],
        parse_mobaxterm_sessions,
        |parsed, storage| mobaxterm_connections(&parsed.sessions, storage),
    );
    assert_eq!(checked, 11);
}

#[test]
fn securecrt_matches_the_typescript() {
    let checked = check_importer(
        &golden()["securecrt"],
        parse_securecrt_sessions,
        |parsed, storage| securecrt_connections(&parsed.sessions, storage),
    );
    assert_eq!(checked, 11);
}

#[test]
fn helpers_match_the_typescript() {
    let golden = golden();
    for case in golden["ids"].as_array().expect("ids") {
        assert_eq!(
            stable_import_id(text(&case["prefix"]), text(&case["value"])),
            text(&case["id"])
        );
    }
    for case in golden["folders"].as_array().expect("folders") {
        assert_eq!(
            normalize_import_folder(text(&case["input"])).as_deref(),
            case["output"].as_str(),
            "{}",
            case["input"]
        );
    }
    for case in golden["names"].as_array().expect("names") {
        assert_eq!(
            clean_import_name(text(&case["input"])),
            text(&case["output"])
        );
    }
    for case in golden["aliases"].as_array().expect("aliases") {
        let used: HashSet<String> = case["used"]
            .as_array()
            .expect("used")
            .iter()
            .map(|alias| text(alias).to_owned())
            .collect();
        assert_eq!(
            unique_import_alias(
                text(&case["name"]),
                text(&case["host"]),
                &used,
                text(&case["fallback"])
            ),
            text(&case["output"])
        );
    }
}

#[test]
fn collation_matches_the_renderer() {
    let golden = golden();
    let mut values: Vec<String> = golden["collation"]["input"]
        .as_array()
        .expect("input")
        .iter()
        .map(|value| text(value).to_owned())
        .collect();
    values.sort_by(|a, b| locale_compare(a, b));
    let expected: Vec<&str> = golden["collation"]["sorted"]
        .as_array()
        .expect("sorted")
        .iter()
        .map(text)
        .collect();
    assert_eq!(values, expected);
}

#[test]
fn command_line_parsing_matches_the_typescript() {
    let golden = golden();
    let mut checked = 0;
    for case in golden["argv"].as_array().expect("argv") {
        let argv: Vec<&str> = case["argv"]
            .as_array()
            .expect("argv")
            .iter()
            .map(text)
            .collect();
        assert_eq!(
            serde_json::to_value(parse_command_line_launch(&argv)).expect("JSON"),
            case["launch"],
            "{argv:?}"
        );
        checked += 1;
    }
    for case in golden["launchData"].as_array().expect("launchData") {
        assert_eq!(
            serde_json::to_value(parse_command_line_launch_data(&case["value"])).expect("JSON"),
            case["launch"],
            "{}",
            case["value"]
        );
        checked += 1;
    }
    for case in golden["connectTargets"].as_array().expect("connectTargets") {
        let options = ConnectOptions {
            user: case["options"]["user"].as_str().map(str::to_owned),
            port: case["options"]["port"].as_str().map(str::to_owned),
        };
        assert_eq!(
            parse_connect_target(text(&case["target"]), &options).as_deref(),
            case["output"].as_str(),
            "{}",
            case["target"]
        );
        checked += 1;
    }
    assert!(checked > 80, "{checked}");
}
