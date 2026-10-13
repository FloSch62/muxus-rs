//! The route schemas ported next to the API types (metadata, login
//! sequences, tunnels, saved hosts), beyond what the golden fixtures cover.

use muxus_core::api::{LoginSequence, OpenSshMetadataPatch, TunnelInput, is_valid_js_regex};
use muxus_core::validate::parse_value;
use serde_json::json;

#[test]
fn regex_validity_follows_javascripts_lenient_grammar() {
    // Each verdict was checked against `new RegExp(pattern)` in Node.
    for pattern in [
        "a{",
        "]",
        "(?<a>x)",
        "(?=a)*",
        "\\c",
        "[\\d-z]",
        "(?<!a)",
        "[]",
        "[^]",
        "err(or)?",
        "^\\s*Password:\\s*$",
        "a{2,}?",
        "x{3}",
        "(a|b)+?",
        "[a-z0-9._-]+",
    ] {
        assert!(is_valid_js_regex(pattern), "{pattern} is valid");
    }
    for pattern in [
        "(",
        ")",
        "{1}",
        "a**",
        "*a",
        "(?<a",
        "[z-a]",
        "x{3,1}",
        "(?x)",
        "a|*",
        "\\",
        "(?<=a)*",
        "a{1,2}{3}",
        "[abc",
        "a+*",
        "^*",
    ] {
        assert!(!is_valid_js_regex(pattern), "{pattern} is invalid");
    }
}

#[test]
fn login_sequences_check_their_steps() {
    let sequence: LoginSequence = parse_value(json!({ "steps": [
        { "id": "a", "kind": "wait", "pattern": "[#>] ?$", "regex": true, "timeoutSeconds": 10 },
        { "id": "b", "kind": "send", "text": "", "enter": true },
    ] }))
    .unwrap();
    assert_eq!(sequence.steps.len(), 2);
    let error = parse_value::<LoginSequence>(json!({ "steps": [
        { "id": "a", "kind": "wait", "pattern": "(", "regex": true, "timeoutSeconds": 10 },
    ] }))
    .unwrap_err();
    assert_eq!(error.message, "invalid regular expression");
    assert_eq!(error.path, "steps.0.pattern");
    // A literal pattern is never compiled, so "(" is fine.
    assert!(
        parse_value::<LoginSequence>(json!({ "steps": [
            { "id": "a", "kind": "wait", "pattern": "(", "timeoutSeconds": 10 },
        ] }))
        .is_ok()
    );
}

#[test]
fn metadata_patches_need_a_field() {
    let error = parse_value::<OpenSshMetadataPatch>(json!({})).unwrap_err();
    assert_eq!(error.message, "at least one metadata field is required");
    assert!(parse_value::<OpenSshMetadataPatch>(json!({ "disableSftp": false })).is_ok());
}

#[test]
fn tunnels_need_a_target_unless_dynamic() {
    let error = parse_value::<TunnelInput>(
        json!({ "type": "remote", "bindPort": 80, "target": "x", "targetHost": "h" }),
    )
    .unwrap_err();
    assert_eq!(
        error.message,
        "targetHost and targetPort are required for local/remote tunnels"
    );
    let tunnel: TunnelInput =
        parse_value(json!({ "type": "dynamic", "bindPort": 1080, "target": "  proxy  " })).unwrap();
    assert_eq!(tunnel.target, "proxy");
}
