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

#[test]
fn metadata_patch_takes_prompt_waiting_anti_idle_keys_and_fallback_routes() {
    let patch: OpenSshMetadataPatch = parse_value(json!({
        "pastePromptWait": { "promptPattern": "[>#\\]]\\s*$", "timeoutMs": 20000 },
        "antiIdle": { "intervalSeconds": 120, "text": " ", "enter": false },
        "backspaceKey": "ctrl-h",
        "enterKey": "crlf",
        "implicitCr": null,
        "localEcho": true,
        "systemOpenSsh": true,
        "discoverEnvironments": null,
        "proxyJumpFallbacks": [[" bastion-b "], ["edge", "bastion-c"]],
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&patch).unwrap(),
        json!({
            "pastePromptWait": { "promptPattern": "[>#\\]]\\s*$", "timeoutMs": 20000 },
            "antiIdle": { "intervalSeconds": 120, "text": " ", "enter": false },
            "backspaceKey": "ctrl-h",
            "enterKey": "crlf",
            "implicitCr": null,
            "localEcho": true,
            "systemOpenSsh": true,
            "discoverEnvironments": null,
            "proxyJumpFallbacks": [["bastion-b"], ["edge", "bastion-c"]],
        })
    );
    // An empty prompt wait is on with every default.
    assert!(parse_value::<OpenSshMetadataPatch>(json!({ "pastePromptWait": {} })).is_ok());
}

#[test]
fn metadata_patch_refuses_settings_the_api_refuses() {
    for body in [
        json!({ "pastePromptWait": { "promptPattern": "([" } }),
        json!({ "pastePromptWait": { "errorPattern": "" } }),
        json!({ "pastePromptWait": { "timeoutMs": 99 } }),
        json!({ "antiIdle": { "intervalSeconds": 9, "text": " ", "enter": false } }),
        json!({ "antiIdle": { "intervalSeconds": 60, "text": "", "enter": false } }),
        json!({ "antiIdle": { "intervalSeconds": 60, "text": "a\rb", "enter": false } }),
        json!({ "antiIdle": { "intervalSeconds": 60, "text": "x".repeat(257), "enter": true } }),
        json!({ "backspaceKey": "bs" }),
        json!({ "enterKey": "\r\n" }),
        json!({ "proxyJumpFallbacks": [[]] }),
        json!({ "proxyJumpFallbacks": [[" "]] }),
        json!({ "proxyJumpFallbacks": [["a"], ["b"], ["c"], ["d"], ["e"]] }),
        json!({ "proxyJumpFallbacks": [["1", "2", "3", "4", "5", "6", "7", "8", "9"]] }),
    ] {
        assert!(
            parse_value::<OpenSshMetadataPatch>(body.clone()).is_err(),
            "{body}"
        );
    }
}

#[test]
fn saved_web_hosts_need_an_http_address() {
    use muxus_core::api::SavedHostProfileInput;
    let host: SavedHostProfileInput = parse_value(json!({
        "name": "Grafana",
        "profile": {
            "kind": "web",
            "url": " https://grafana.internal:3000 ",
            "sshGateway": { "target": "bastion-a" },
        },
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&host.profile).unwrap(),
        json!({ "kind": "web", "url": "https://grafana.internal:3000", "sshGateway": { "target": "bastion-a" } })
    );
    for url in [
        "ftp://files",
        "https:///path",
        "grafana.internal",
        "HTTP:// x",
        "",
    ] {
        let body = json!({ "name": "x", "profile": { "kind": "web", "url": url } });
        assert!(parse_value::<SavedHostProfileInput>(body).is_err(), "{url}");
    }
    assert!(
        parse_value::<SavedHostProfileInput>(
            json!({ "name": "x", "profile": { "kind": "web", "url": "HTTP://Router" } })
        )
        .is_ok()
    );
}

#[test]
fn gateway_fallbacks_and_jump_fallbacks_are_bounded() {
    use muxus_core::protocol::SessionProfile;
    let gateway = json!({ "target": "bastion" });
    let rdp = |count: usize| json!({ "kind": "rdp", "host": "win", "sshGatewayFallbacks": vec![gateway.clone(); count] });
    assert!(parse_value::<SessionProfile>(rdp(4)).is_ok());
    assert!(parse_value::<SessionProfile>(rdp(5)).is_err());
    let ssh = |routes: serde_json::Value| json!({ "kind": "ssh", "target": "app", "proxyJumpFallbacks": routes });
    assert!(parse_value::<SessionProfile>(ssh(json!([["b"], ["c", "d"]]))).is_ok());
    assert!(parse_value::<SessionProfile>(ssh(json!([[]]))).is_err());
}
