//! Reading a transfer file: the envelope and the bounded collection shapes
//! (`parseTransferDocument` and its validators in `client/src/data-transfer.ts`).
//!
//! The checks run on the parsed JSON exactly as the TypeScript runs them on
//! `unknown`, so a file one build accepts the other accepts too.

use std::collections::HashSet;

use serde_json::{Map, Value};

use muxus_core::js;

use super::{
    BACKUP_FORMAT, LEGACY_TRANSFER_VERSION, TRANSFER_VERSION, TransferDocument, TransferError,
};

/// Validate the stable envelope and bounded collection shapes before any
/// restore request is made. The backend remains the final, strict validator
/// for individual hosts and tunnels.
pub fn parse_transfer_document(text: &str) -> Result<TransferDocument, TransferError> {
    let parsed: Value = serde_json::from_str(text)
        .map_err(|_| TransferError::new("This file is not valid JSON."))?;
    let Some(record) = parsed.as_object() else {
        return Err(TransferError::new("This is not a Muxus transfer file."));
    };
    let version = record.get("version");
    let version_number = version.and_then(Value::as_f64);
    let version = match version_number {
        Some(number) if number == f64::from(LEGACY_TRANSFER_VERSION) => LEGACY_TRANSFER_VERSION,
        Some(number) if number == f64::from(TRANSFER_VERSION) => TRANSFER_VERSION,
        Some(number) => {
            return Err(TransferError::new(format!(
                "Muxus transfer version {} is not supported.",
                js::js_number(number)
            )));
        }
        None => {
            return Err(TransferError::new(
                "This file is missing a supported transfer version.",
            ));
        }
    };
    if record.get("format").and_then(Value::as_str) != Some(BACKUP_FORMAT) {
        return Err(TransferError::new("This is not a Muxus backup file."));
    }
    let created_at = record.get("createdAt").and_then(Value::as_str);
    let data = record.get("data").and_then(Value::as_object);
    let (Some(created_at), Some(data)) = (created_at, data) else {
        return Err(TransferError::new("The Muxus transfer file is incomplete."));
    };
    if !js_date_parses(created_at) {
        return Err(TransferError::new("The Muxus transfer file is incomplete."));
    }
    validate_connections(data, version)?;
    validate_backup_data(data)?;
    Ok(TransferDocument::from_validated(parsed))
}

fn validate_connections(data: &Map<String, Value>, version: u32) -> Result<(), TransferError> {
    let incomplete =
        || TransferError::new("The connection data in this file is incomplete or too large.");
    let ssh_hosts = bounded_array(data.get("sshHosts"), 10_000).ok_or_else(incomplete)?;
    let saved_hosts = bounded_array(data.get("savedHosts"), 10_000).ok_or_else(incomplete)?;
    let host_order = bounded_array(data.get("hostOrder"), 20_000).ok_or_else(incomplete)?;
    let ssh_valid = ssh_hosts.iter().all(|host| {
        let Some(host) = host.as_object() else {
            return false;
        };
        non_empty_string(host.get("alias"))
            && bounded_array(host.get("aliases"), 100).is_some_and(|aliases| {
                !aliases.is_empty() && aliases.iter().all(|alias| non_empty_string(Some(alias)))
            })
            && host.get("options").is_some_and(Value::is_object)
            && host.get("description").is_none_or(Value::is_string)
            && host.get("metadata").is_none_or(Value::is_object)
    });
    let saved_valid = saved_hosts.iter().all(|host| {
        let Some(host) = host.as_object() else {
            return false;
        };
        let kind = host
            .get("profile")
            .and_then(Value::as_object)
            .map(|profile| profile.get("kind").and_then(Value::as_str));
        non_empty_string(host.get("id"))
            && non_empty_string(host.get("name"))
            && match kind {
                Some(Some("telnet" | "serial")) => true,
                Some(Some("ssh" | "rdp" | "vnc" | "gnmi" | "netconf")) => version >= 2,
                _ => false,
            }
            && host.get("metadata").is_some_and(Value::is_object)
    });
    let order_valid = host_order.iter().all(|reference| {
        let Some(reference) = reference.as_object() else {
            return false;
        };
        match reference.get("kind").and_then(Value::as_str) {
            Some("ssh") => non_empty_string(reference.get("alias")),
            Some("profile") => non_empty_string(reference.get("id")),
            _ => false,
        }
    });
    if !ssh_valid || !saved_valid || !order_valid {
        return Err(incomplete());
    }

    let ssh_aliases: Vec<&str> = ssh_hosts
        .iter()
        .filter_map(|host| host.get("alias").and_then(Value::as_str))
        .collect();
    let profile_ids: Vec<&str> = saved_hosts
        .iter()
        .filter_map(|host| host.get("id").and_then(Value::as_str))
        .collect();
    let order_keys: Vec<String> = host_order
        .iter()
        .map(|reference| {
            let text = |key: &str| {
                reference
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            };
            if text("kind") == "ssh" {
                format!("ssh:{}", text("alias"))
            } else {
                format!("profile:{}", text("id"))
            }
        })
        .collect();
    let available: HashSet<String> = ssh_aliases
        .iter()
        .map(|alias| format!("ssh:{alias}"))
        .chain(profile_ids.iter().map(|id| format!("profile:{id}")))
        .collect();
    if has_duplicates(&ssh_aliases)
        || has_duplicates(&profile_ids)
        || has_duplicates(&order_keys)
        || order_keys.iter().any(|key| !available.contains(key))
    {
        return Err(TransferError::new(
            "The connection data contains duplicate or unknown items.",
        ));
    }
    Ok(())
}

fn validate_backup_data(data: &Map<String, Value>) -> Result<(), TransferError> {
    let incomplete = || TransferError::new("The backup data is incomplete or too large.");
    if !data.get("preferences").is_some_and(Value::is_object) {
        return Err(incomplete());
    }
    let tunnels = bounded_array(data.get("tunnels"), 10_000).ok_or_else(incomplete)?;
    let policies = bounded_array(data.get("loggingPolicies"), 20_000).ok_or_else(incomplete)?;
    let history = data
        .get("historySettings")
        .and_then(Value::as_object)
        .ok_or_else(incomplete)?;
    let tunnels_valid = tunnels.iter().all(|tunnel| {
        let Some(tunnel) = tunnel.as_object() else {
            return false;
        };
        non_empty_string(tunnel.get("id"))
            && non_empty_string(tunnel.get("target"))
            && ["local", "remote", "dynamic"].contains(&js_string(tunnel.get("type")).as_str())
            && is_integer(tunnel.get("bindPort"))
            && tunnel.get("autoStart").is_none_or(Value::is_boolean)
            && tunnel.get("autoReconnect").is_none_or(Value::is_boolean)
    });
    let policies_valid = policies.iter().all(|entry| {
        let Some(entry) = entry.as_object() else {
            return false;
        };
        let Some(policy) = entry.get("policy").and_then(Value::as_object) else {
            return false;
        };
        non_empty_string(entry.get("profileKey"))
            && policy.get("enabled").is_some_and(Value::is_boolean)
            && policy.get("captureInput").is_some_and(Value::is_boolean)
            && is_integer(policy.get("maxPartBytes"))
            && is_integer(policy.get("maxParts"))
            && policy.get("logToFile").is_none_or(Value::is_boolean)
    });
    let history_valid = finite_number(history.get("maxTotalBytes"))
        && finite_number(history.get("minFreeBytes"))
        && finite_number(history.get("minFreePercent"));
    let log_files_valid = data.get("logFileSettings").is_none_or(|settings| {
        settings.as_object().is_some_and(|settings| {
            settings
                .get("filenamePattern")
                .and_then(Value::as_str)
                .is_some_and(|pattern| !pattern.is_empty() && js::utf16_len(pattern) <= 200)
                && settings.get("timestamps").is_some_and(Value::is_boolean)
        })
    });
    let folders_valid = data.get("folderSettings").is_none_or(|folders| {
        bounded_array(Some(folders), 500).is_some_and(|folders| {
            folders.iter().all(|entry| {
                entry.as_object().is_some_and(|entry| {
                    entry
                        .get("path")
                        .and_then(Value::as_str)
                        .is_some_and(|path| !path.is_empty() && js::utf16_len(path) <= 300)
                        && entry.get("auth").is_some_and(Value::is_object)
                })
            })
        })
    });
    if !tunnels_valid || !policies_valid || !history_valid || !log_files_valid || !folders_valid {
        return Err(incomplete());
    }
    let tunnel_ids: Vec<String> = tunnels
        .iter()
        .map(|tunnel| js_string(tunnel.get("id")))
        .collect();
    let policy_keys: Vec<String> = policies
        .iter()
        .map(|entry| js_string(entry.get("profileKey")))
        .collect();
    if has_duplicates(&tunnel_ids) || has_duplicates(&policy_keys) {
        return Err(TransferError::new(
            "The backup contains duplicate saved items.",
        ));
    }
    Ok(())
}

fn has_duplicates<T: AsRef<str>>(items: &[T]) -> bool {
    let mut seen = HashSet::new();
    !items.iter().all(|item| seen.insert(item.as_ref()))
}

/// `boundedArray`: an array of at most `max` entries.
pub(super) fn bounded_array(value: Option<&Value>, max: usize) -> Option<&Vec<Value>> {
    value
        .and_then(Value::as_array)
        .filter(|entries| entries.len() <= max)
}

/// `nonEmptyString`.
pub(super) fn non_empty_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty())
}

/// `finiteNumber`.
pub(super) fn finite_number(value: Option<&Value>) -> bool {
    value.and_then(Value::as_f64).is_some_and(f64::is_finite)
}

/// `Number.isInteger`.
pub(super) fn is_integer(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_f64)
        .is_some_and(|number| number.is_finite() && number.fract() == 0.0)
}

/// `String(value)` for the JSON values a transfer file can hold.
fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Number(number)) => number
            .as_f64()
            .map_or_else(|| number.to_string(), |n| js::js_number(n).to_string()),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(entries)) => entries
            .iter()
            .map(|entry| match entry {
                Value::Null => String::new(),
                other => js_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

/// Whether `Date.parse` reads the text as a date: the ISO 8601 forms
/// (date only, local date-time, and with an offset), RFC 2822 and SQLite's
/// `YYYY-MM-DD HH:MM:SS`.
pub(super) fn js_date_parses(text: &str) -> bool {
    js_date_parse(text).is_some()
}

/// `new Date(text)` for the forms [`js_date_parses`] accepts, in UTC.
pub(super) fn js_date_parse(text: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
    let text = text.trim();
    if let Some(parsed) = js::parse_timestamp(text) {
        return Some(parsed);
    }
    if let Ok(parsed) = DateTime::parse_from_rfc2822(text) {
        return Some(parsed.with_timezone(&Utc));
    }
    // ISO date-only forms (`YYYY-MM-DD`, `YYYY-MM`, `YYYY`) are UTC midnight.
    let date_only = match text.len() {
        10 => NaiveDate::parse_from_str(text, "%Y-%m-%d").ok(),
        7 => NaiveDate::parse_from_str(&format!("{text}-01"), "%Y-%m-%d").ok(),
        _ => None,
    };
    if let Some(date) = date_only {
        return date.and_hms_opt(0, 0, 0).map(|naive| naive.and_utc());
    }
    if text.len() == 4
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && let Some(date) = text
            .parse::<i32>()
            .ok()
            .and_then(|year| NaiveDate::from_ymd_opt(year, 1, 1))
    {
        return date.and_hms_opt(0, 0, 0).map(|naive| naive.and_utc());
    }
    // A date-time without an offset is local time.
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return Local
                .from_local_datetime(&naive)
                .earliest()
                .map(|local| local.with_timezone(&Utc));
        }
    }
    None
}
