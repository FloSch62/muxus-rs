//! Golden JSON written by the TypeScript build.
//!
//! `fixtures/zod-golden.json` holds inputs run through the TypeScript's zod
//! schemas (shared/src/ws-protocol.ts and the route schemas of the server)
//! and, for each accepted input, `JSON.stringify` of what zod returned. The
//! Rust types must accept and reject the same inputs and serialize to the
//! same bytes, since this JSON is stored in the database both builds share.
//! Server messages carry no schema; their fixtures are the literal objects
//! the backend sends, which must survive a Rust round trip unchanged.

use muxus_core::api::{
    OpenSshMetadataPatch, SavedHostProfileInput, TunnelInput, WorkspaceSaveRequest,
};
use muxus_core::protocol::{
    DesktopClientMessage, DesktopServerMessage, ManagementClientMessage, ManagementServerMessage,
    SessionProfile, TerminalClientMessage, TerminalServerMessage,
};
use muxus_core::validate::{self, Validate, ValidationError};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const FIXTURE: &str = include_str!("fixtures/zod-golden.json");

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    schema: String,
    input: Value,
    output: Option<String>,
    #[serde(default)]
    rejected: bool,
}

fn parsed<T: DeserializeOwned + Validate + Serialize>(
    input: Value,
) -> Result<String, ValidationError> {
    let value: T = validate::parse_value(input)?;
    Ok(serde_json::to_string(&value).expect("serializable"))
}

fn round_trip<T: DeserializeOwned + Serialize>(input: Value) -> Result<String, ValidationError> {
    let value: T = serde_json::from_value(input)
        .map_err(|error| ValidationError::new("", error.to_string()))?;
    Ok(serde_json::to_string(&value).expect("serializable"))
}

fn run(case: &Case) -> Result<String, ValidationError> {
    let input = case.input.clone();
    match case.schema.as_str() {
        "sessionProfile" => parsed::<SessionProfile>(input),
        "terminalClient" => parsed::<TerminalClientMessage>(input),
        "desktopClient" => parsed::<DesktopClientMessage>(input),
        "managementClient" => parsed::<ManagementClientMessage>(input),
        "metadataPatch" => parsed::<OpenSshMetadataPatch>(input),
        "workspaceSave" => parsed::<WorkspaceSaveRequest>(input),
        "tunnel" => parsed::<TunnelInput>(input),
        "savedProfile" => parsed::<SavedHostProfileInput>(input),
        "terminalServer" => round_trip::<TerminalServerMessage>(input),
        "desktopServer" => round_trip::<DesktopServerMessage>(input),
        "managementServer" => round_trip::<ManagementServerMessage>(input),
        other => panic!("unknown schema {other}"),
    }
}

#[test]
fn rust_types_write_what_the_typescript_writes() {
    let cases: Vec<Case> = serde_json::from_str(FIXTURE).expect("valid fixture");
    assert!(cases.len() > 100, "fixture is complete");
    let mut failures = Vec::new();
    for case in &cases {
        let label = format!("{}/{}", case.schema, case.name);
        match (run(case), case.rejected, &case.output) {
            (Ok(actual), false, Some(expected)) if &actual == expected => {}
            (Ok(actual), false, Some(expected)) => {
                failures.push(format!(
                    "{label}: wrote\n  {actual}\nTypeScript wrote\n  {expected}"
                ));
            }
            (Err(error), false, _) => {
                failures.push(format!("{label}: rejected ({error}) but zod accepts it"))
            }
            (Ok(actual), true, _) => {
                failures.push(format!("{label}: accepted as {actual} but zod rejects it"))
            }
            (Err(_), true, _) => {}
            (Ok(_), false, None) => failures.push(format!("{label}: fixture has no output")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_schema_has_accepted_and_rejected_cases() {
    let cases: Vec<Case> = serde_json::from_str(FIXTURE).expect("valid fixture");
    for schema in [
        "sessionProfile",
        "terminalClient",
        "desktopClient",
        "managementClient",
        "metadataPatch",
        "workspaceSave",
        "tunnel",
        "savedProfile",
    ] {
        assert!(
            cases
                .iter()
                .any(|case| case.schema == schema && !case.rejected),
            "{schema} accepted"
        );
        assert!(
            cases
                .iter()
                .any(|case| case.schema == schema && case.rejected),
            "{schema} rejected"
        );
    }
}
