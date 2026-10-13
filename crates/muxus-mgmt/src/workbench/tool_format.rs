//! Reading and writing the shapes gNOI and gNSI use, for the operations and
//! security tools (port of `client/src/management/tool-format.ts`).

use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use muxus_core::management::GrpcJson;
use serde_json::Value;

use super::requests::now_ms;

/// gNOI reports permissions as their octal digits written in decimal (644),
/// not as a mode number; `rw-r--r--` either way.
pub fn permission_string(value: Option<u64>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let text = format!("{value:0>3}");
    let digits = &text[text.len() - 3..];
    if !digits.chars().all(|c| ('0'..='7').contains(&c)) {
        return value.to_string();
    }
    digits
        .chars()
        .map(|digit| {
            let bits = digit.to_digit(8).unwrap_or(0);
            format!(
                "{}{}{}",
                if bits & 4 != 0 { 'r' } else { '-' },
                if bits & 2 != 0 { 'w' } else { '-' },
                if bits & 1 != 0 { 'x' } else { '-' }
            )
        })
        .collect()
}

fn key_type_of(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "ssh-ed25519" => "KEY_TYPE_ED25519",
        "ecdsa-sha2-nistp256" => "KEY_TYPE_ECDSA_P_256",
        "ecdsa-sha2-nistp384" => "KEY_TYPE_ECDSA_P_384",
        "ecdsa-sha2-nistp521" => "KEY_TYPE_ECDSA_P_521",
        "ssh-rsa" => "KEY_TYPE_RSA_2048",
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedKey {
    /// `ssh-ed25519`, as in the authorized_keys line.
    pub kind: String,
    /// The base64 key data, as in the authorized_keys line.
    pub blob: String,
    pub comment: String,
    /// gNSI's KeyType name.
    pub key_type: String,
}

/// The modulus size of an ssh-rsa key blob: string type, mpint e, mpint n.
fn rsa_bits(blob: &str) -> Option<usize> {
    let bytes = BASE64.decode(blob).ok()?;
    let mut offset = 0;
    let mut next = || -> Option<&[u8]> {
        let length = u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?) as usize;
        let start = offset + 4;
        offset = start + length;
        bytes.get(start..offset)
    };
    next()?;
    next()?;
    let modulus = next()?;
    let leading_zero = usize::from(modulus.first() == Some(&0));
    Some((modulus.len() - leading_zero) * 8)
}

fn is_base64_blob(blob: &str) -> bool {
    let data = blob.trim_end_matches('=');
    !data.is_empty()
        && data
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
}

/// OpenSSH public key lines, or the first problem with them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthorizedKeys {
    pub keys: Vec<ParsedKey>,
    pub problem: Option<String>,
}

/// OpenSSH public key lines; options before the key type are not supported here.
pub fn parse_authorized_keys(text: &str) -> AuthorizedKeys {
    let mut keys = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let kind = words.next().unwrap_or_default();
        let blob = words.next().unwrap_or_default();
        let comment = words.collect::<Vec<_>>().join(" ");
        let problem = |message: String| AuthorizedKeys {
            keys: keys.clone(),
            problem: Some(message),
        };
        let Some(mut key_type) = key_type_of(kind).map(str::to_owned) else {
            let what = if kind.is_empty() { "the key" } else { kind };
            return problem(format!(
                "Line {}: {what} is not a key type gNSI can carry.",
                index + 1
            ));
        };
        if !is_base64_blob(blob) {
            return problem(format!("Line {}: the key data is not base64.", index + 1));
        }
        if kind == "ssh-rsa" {
            let Some(bits) = rsa_bits(blob) else {
                return problem(format!("Line {}: the RSA key data is damaged.", index + 1));
            };
            if ![2048, 3072, 4096].contains(&bits) {
                return problem(format!(
                    "Line {}: gNSI carries RSA keys of 2048, 3072 or 4096 bits, not {bits}.",
                    index + 1
                ));
            }
            key_type = format!("KEY_TYPE_RSA_{bits}");
        }
        keys.push(ParsedKey {
            kind: kind.to_owned(),
            blob: blob.to_owned(),
            comment,
            key_type,
        });
    }
    AuthorizedKeys {
        keys,
        problem: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthzVerdict {
    Permit,
    Deny,
    Error,
}

/// One accounting record, flattened for the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountingRecord {
    pub key: String,
    /// Milliseconds since the epoch.
    pub at: i64,
    pub user: String,
    pub role: Option<String>,
    pub service: String,
    pub action: String,
    pub authz: Option<AuthzVerdict>,
    pub authz_detail: Option<String>,
    pub remote: Option<String>,
    pub session: Option<String>,
    pub arrived: u64,
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => super::data_tree::js_number_text(number),
        _ => String::new(),
    }
}

fn object(value: Option<&Value>) -> Option<&GrpcJson> {
    value.and_then(Value::as_object)
}

fn number(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.0),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(f64::NAN),
        Some(Value::Bool(flag)) => f64::from(u8::from(*flag)),
        Some(Value::Null) | None => 0.0,
        _ => f64::NAN,
    }
}

fn service_name(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "CMD_SERVICE_TYPE_SHELL" => "shell",
        "CMD_SERVICE_TYPE_CLI" => "CLI",
        "CMD_SERVICE_TYPE_WEBUI" => "web UI",
        "CMD_SERVICE_TYPE_RESTCONF" => "RESTCONF",
        "CMD_SERVICE_TYPE_NETCONF" => "NETCONF",
        "GRPC_SERVICE_TYPE_GNMI" => "gNMI",
        "GRPC_SERVICE_TYPE_GNOI" => "gNOI",
        "GRPC_SERVICE_TYPE_GNSI" => "gNSI",
        "GRPC_SERVICE_TYPE_GRIBI" => "gRIBI",
        "GRPC_SERVICE_TYPE_P4RT" => "P4Runtime",
        _ => return None,
    })
}

static RECORD_COUNTER: AtomicU64 = AtomicU64::new(0);

/// An acctz RecordResponse as one table row.
pub fn accounting_record(message: &GrpcJson) -> AccountingRecord {
    let empty = GrpcJson::new();
    let session = object(message.get("session_info")).unwrap_or(&empty);
    let user = object(session.get("user")).unwrap_or(&empty);
    let timestamp = object(message.get("timestamp")).unwrap_or(&empty);
    let cmd = object(message.get("cmd_service"));
    let grpc = object(message.get("grpc_service"));
    let service = cmd.or(grpc).unwrap_or(&empty);
    let authz = object(service.get("authz")).unwrap_or(&empty);
    let status = text(authz.get("status"));
    let args = cmd
        .and_then(|cmd| cmd.get("cmd_args"))
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .map(|arg| text(Some(arg)))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let seconds = number(timestamp.get("seconds"));
    let nanos = number(timestamp.get("nanos"));
    let lowercase_without =
        |value: String, prefix: &str| value.strip_prefix(prefix).unwrap_or(&value).to_lowercase();
    let session_status = text(session.get("status"));
    let service_type = text(service.get("service_type"));
    let detail = text(authz.get("detail"));
    let remote = text(session.get("remote_address"));
    let role = lowercase_without(text(user.get("role")), "SYSTEM_ROLE_");
    AccountingRecord {
        key: (RECORD_COUNTER.fetch_add(1, Ordering::Relaxed) + 1).to_string(),
        at: (seconds * 1000.0 + (nanos / 1e6).floor()) as i64,
        user: Some(text(user.get("identity")))
            .filter(|identity| !identity.is_empty())
            .unwrap_or_else(|| "—".into()),
        role: (!role.is_empty()).then_some(role),
        service: service_name(&service_type).map_or_else(
            || {
                if cmd.is_some() {
                    "command".into()
                } else if grpc.is_some() {
                    "gRPC".into()
                } else {
                    "—".into()
                }
            },
            str::to_owned,
        ),
        action: if let Some(cmd) = cmd {
            [text(cmd.get("cmd")), args]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        } else if let Some(grpc) = grpc {
            text(grpc.get("rpc_name"))
        } else {
            lowercase_without(session_status.clone(), "SESSION_STATUS_")
        },
        authz: if status.ends_with("PERMIT") {
            Some(AuthzVerdict::Permit)
        } else if status.ends_with("DENY") {
            Some(AuthzVerdict::Deny)
        } else if status.ends_with("ERROR") {
            Some(AuthzVerdict::Error)
        } else {
            None
        },
        authz_detail: (!detail.is_empty()).then_some(detail),
        remote: (!remote.is_empty()).then_some(remote),
        session: (!session_status.is_empty())
            .then(|| lowercase_without(session_status, "SESSION_STATUS_")),
        arrived: now_ms(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_gnoi_permissions_written_as_octal_digits() {
        assert_eq!(permission_string(Some(644)), "rw-r--r--");
        assert_eq!(permission_string(Some(755)), "rwxr-xr-x");
        assert_eq!(permission_string(Some(0)), "---------");
        assert_eq!(permission_string(None), "");
        assert_eq!(permission_string(Some(999)), "999");
    }

    #[test]
    fn parses_authorized_keys_and_sizes_rsa_keys() {
        let rsa3072 = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQDOyztBIThj3WKsWnxGSpcvpfh5AGmTqWJoOqPAC1ZWo4JUSFC0w0uIlV1a9728sz9nFeiIKypvwy63AeWtTjpdQHVe3KKjYFoxvZDbwWoLPOIphyGJBgQA9ukwdBgN1nzwD9KfVZG1+bK5zxB7y5YPquknPaKYng5gVCc7zMDtjBq2PoN29/UjvrD9Iq2jX5PjKP8Ys6xpmenqUAuPUn54nCJ8TXHldnQ+scKqyqIerEF7tNO6Ej8xOMo/n/ChQ9i1AURqoxEKGMVvKUAQJPeKnIaXjrpCdNfu2k2ax7/l2GGqn5GSSmTjfTK8WHMTWzULYRuOwoNamxHTrzkBZ6Nb8Kw8xq4H8pBCplnqJZ/mP61t414s38s/ZavweanbW/eG6/6akXDdtUdZHCGczcVwV4JC/Zt6s/tqz3w8V3w+Z4gR+N9HKwOuTmhyGS1IPjFcIFUbSwWAe3ONz8w31imz+SGLnuJylNENi/P3g6PZM7CFFNQqi27aMz8zaKwDOAE= test@rsa";
        let ed25519 = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFso1Qvz22DNSG95I5vUdco9JYihwF40Irse71T+5YQz me at laptop";
        let parsed = parse_authorized_keys(&format!("# team keys\n{rsa3072}\n\n{ed25519}\n"));
        assert_eq!(parsed.problem, None);
        assert_eq!(
            parsed
                .keys
                .iter()
                .map(|key| (key.key_type.as_str(), key.comment.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("KEY_TYPE_RSA_3072", "test@rsa"),
                ("KEY_TYPE_ED25519", "me at laptop")
            ]
        );
        assert!(
            parse_authorized_keys("ssh-dss AAAAB3NzaC1kc3M=")
                .problem
                .unwrap()
                .starts_with("Line 1: ssh-dss")
        );
        assert!(
            parse_authorized_keys("ssh-ed25519 not*base64")
                .problem
                .unwrap()
                .contains("not base64")
        );
    }

    #[test]
    fn flattens_accounting_records_for_the_table() {
        let grpc = accounting_record(
            json!({
                "timestamp": {"seconds": 1_760_000_000, "nanos": 250_000_000},
                "session_info": {"user": {"identity": "admin", "role": "admin"}, "remote_address": "10.0.0.9", "status": "SESSION_STATUS_OPERATION"},
                "grpc_service": {"service_type": "GRPC_SERVICE_TYPE_GNMI", "rpc_name": "/gnmi.gNMI/Set", "authz": {"status": "AUTHZ_STATUS_PERMIT"}}
            })
            .as_object()
            .unwrap(),
        );
        assert_eq!(grpc.at, 1_760_000_000_250);
        assert_eq!(grpc.user, "admin");
        assert_eq!(grpc.service, "gNMI");
        assert_eq!(grpc.action, "/gnmi.gNMI/Set");
        assert_eq!(grpc.authz, Some(AuthzVerdict::Permit));
        assert_eq!(grpc.remote.as_deref(), Some("10.0.0.9"));
        let cli = accounting_record(
            json!({
                "session_info": {"user": {"identity": "ops"}},
                "cmd_service": {"service_type": "CMD_SERVICE_TYPE_CLI", "cmd": "show", "cmd_args": ["version"], "authz": {"status": "AUTHZ_STATUS_DENY", "detail": "read-only role"}}
            })
            .as_object()
            .unwrap(),
        );
        assert_eq!(cli.service, "CLI");
        assert_eq!(cli.action, "show version");
        assert_eq!(cli.authz, Some(AuthzVerdict::Deny));
        assert_eq!(cli.authz_detail.as_deref(), Some("read-only role"));
        assert_ne!(cli.key, grpc.key);
    }
}
