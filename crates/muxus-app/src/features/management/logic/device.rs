//! What the session header and the navigator's Device, Saved and History
//! panels say about the device (`Workbench.tsx`, `Navigator.tsx`,
//! `NetconfForm.tsx` `helloModules`).

use muxus_core::collate::locale_compare;
use muxus_core::js::js_trim;
use muxus_core::management::{
    GnmiEncoding, GnmiTlsMode, ManagementTlsInfo, NetconfBase, SavedManagementRequest,
};
use muxus_mgmt::workbench::live_table::format_si;
use muxus_mgmt::workbench::store::{RunRecord, RunStatus};

use super::format::time_of;

/// A YANG module the device lists in its hello, for get-schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloModule {
    pub name: String,
    pub revision: Option<String>,
    pub namespace: String,
}

/// `URLSearchParams` decoding: `+` is a space, `%XX` a byte.
fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len()
                && bytes[index + 1].is_ascii_hexdigit()
                && bytes[index + 2].is_ascii_hexdigit() =>
            {
                let hex = |byte: u8| (byte as char).to_digit(16).unwrap_or(0) as u8;
                out.push(hex(bytes[index + 1]) * 16 + hex(bytes[index + 2]));
                index += 2;
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The first value of `name` in a query string, as `URLSearchParams.get` finds it.
fn query_get(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (url_decode(key), url_decode(value)),
            None => (url_decode(pair), String::new()),
        })
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

/// YANG modules the device lists in its hello, sorted by name.
pub fn hello_modules(capabilities: &[String]) -> Vec<HelloModule> {
    let mut modules: Vec<HelloModule> = capabilities
        .iter()
        .filter_map(|capability| {
            let query = capability.find('?')?;
            let params = capability[query + 1..].replace("&amp;", "&");
            let name = query_get(&params, "module").filter(|name| !name.is_empty())?;
            Some(HelloModule {
                name,
                revision: query_get(&params, "revision"),
                namespace: capability[..query].to_owned(),
            })
        })
        .collect();
    modules.sort_by(|a, b| locale_compare(&a.name, &b.name));
    modules
}

/// NETCONF capabilities the Device panel lists: fragment, label, what it means.
pub const NETCONF_CAPABILITIES: &[(&str, &str, &str)] = &[
    (":candidate", "Candidate", "Stage changes, then commit"),
    (
        ":confirmed-commit",
        "Confirmed commit",
        "Roll back unless confirmed",
    ),
    (":validate", "Validate", "Check without applying"),
    (
        ":rollback-on-error",
        "Rollback on error",
        "All or nothing edits",
    ),
    (
        ":writable-running",
        "Writable running",
        "Edit running directly",
    ),
    (":startup", "Startup", "Separate startup datastore"),
    (":xpath", "XPath", "XPath filters"),
    (":notification", "Notifications", "Event streams"),
    (
        ":with-defaults",
        "With-defaults",
        "Control default values in replies",
    ),
    (":url", "URL", "Copy configs to and from URLs"),
];

/// Capabilities the header's module chip names in its tooltip.
pub const FEATURED_CAPABILITIES: &[(&str, &str)] = &[
    (":candidate", "candidate"),
    (":confirmed-commit", "confirmed-commit"),
    (":validate", "validate"),
    (":writable-running", "writable-running"),
    (":xpath", "xpath"),
    (":notification", "notifications"),
];

/// The header's module chip: how many modules, and the tooltip of featured capabilities.
pub fn netconf_module_chip(capabilities: &[String]) -> (String, String) {
    let modules = capabilities
        .iter()
        .filter(|capability| capability.contains("module="))
        .count();
    let has = |fragment: &str| {
        capabilities
            .iter()
            .any(|capability| capability.contains(fragment))
    };
    let featured: Vec<&str> = FEATURED_CAPABILITIES
        .iter()
        .filter(|(fragment, _)| has(fragment))
        .map(|(_, label)| *label)
        .collect();
    (
        format!("{modules} modules"),
        if featured.is_empty() {
            "No optional capabilities".into()
        } else {
            featured.join(" · ")
        },
    )
}

/// The framing chip's tooltip.
pub fn base_tooltip(base: NetconfBase) -> &'static str {
    match base {
        NetconfBase::V1_1 => "Chunked framing (base:1.1)",
        NetconfBase::V1_0 => "End-of-message framing (base:1.0)",
    }
}

pub fn base_label(base: NetconfBase) -> &'static str {
    match base {
        NetconfBase::V1_1 => "1.1",
        NetconfBase::V1_0 => "1.0",
    }
}

/// The Device panel's framing row.
pub fn framing_text(base: NetconfBase) -> &'static str {
    match base {
        NetconfBase::V1_1 => "base:1.1 (chunked)",
        NetconfBase::V1_0 => "base:1.0 (]]>]]>)",
    }
}

/// `json_ietf` as the device names an encoding.
pub fn encoding_name(encoding: GnmiEncoding) -> &'static str {
    match encoding {
        GnmiEncoding::JsonIetf => "json_ietf",
        GnmiEncoding::Json => "json",
        GnmiEncoding::Proto => "proto",
        GnmiEncoding::Ascii => "ascii",
        GnmiEncoding::Bytes => "bytes",
    }
}

/// `JSON_IETF, JSON`.
pub fn encodings_text(encodings: &[GnmiEncoding]) -> String {
    encodings
        .iter()
        .map(|encoding| encoding_name(*encoding).to_uppercase())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The encodings chip's tooltip.
pub fn encodings_tooltip(encodings: &[GnmiEncoding]) -> String {
    let text = encodings_text(encodings);
    format!(
        "Supported: {}",
        if text.is_empty() { "not listed" } else { &text }
    )
}

/// The TLS chip's label.
pub fn tls_label(tls: &ManagementTlsInfo) -> &'static str {
    if tls.mode == GnmiTlsMode::Plaintext {
        "Plain text"
    } else if tls.verified == Some(true) {
        "TLS"
    } else if tls.mode == GnmiTlsMode::SkipVerify {
        "TLS, unverified"
    } else {
        "TLS, pinned"
    }
}

/// How the TLS chip is colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsTone {
    Warning,
    Success,
    Default,
}

pub fn tls_tone(tls: &ManagementTlsInfo) -> TlsTone {
    if tls.mode == GnmiTlsMode::Plaintext {
        TlsTone::Warning
    } else if tls.verified == Some(true) || tls.pinned == Some(true) {
        TlsTone::Success
    } else {
        TlsTone::Default
    }
}

/// The TLS chip's tooltip: plain lines, then the fingerprint (set in mono).
pub fn tls_tooltip(tls: &ManagementTlsInfo) -> (Vec<String>, Option<String>) {
    if tls.mode == GnmiTlsMode::Plaintext {
        return (
            vec!["No encryption: requests and passwords cross the network readable.".into()],
            None,
        );
    }
    let mut lines = vec![
        if tls.verified == Some(true) {
            "Certificate verified."
        } else if tls.mode == GnmiTlsMode::SkipVerify {
            "Certificate not checked (skip-verify)."
        } else {
            "Certificate trusted on first use and pinned."
        }
        .to_owned(),
    ];
    if let Some(subject) = tls.subject.as_deref().filter(|text| !text.is_empty()) {
        lines.push(format!("Issued to: {subject}"));
    }
    if let Some(issuer) = tls.issuer.as_deref().filter(|text| !text.is_empty()) {
        lines.push(format!("Issued by: {issuer}"));
    }
    if let Some(valid_to) = tls.valid_to.as_deref().filter(|text| !text.is_empty()) {
        lines.push(format!("Valid until: {valid_to}"));
    }
    if let Some(protocol) = tls.protocol.as_deref().filter(|text| !text.is_empty()) {
        lines.push(protocol.to_owned());
    }
    (
        lines,
        tls.fingerprint.clone().filter(|text| !text.is_empty()),
    )
}

/// The Device panel's TLS row.
pub fn tls_row(tls: &ManagementTlsInfo) -> String {
    if tls.mode == GnmiTlsMode::Plaintext {
        return "none".into();
    }
    let state = if tls.verified == Some(true) {
        "verified"
    } else if tls.mode == GnmiTlsMode::SkipVerify {
        "not verified"
    } else {
        "pinned"
    };
    js_trim(&format!(
        "{} {state}",
        tls.protocol.as_deref().unwrap_or("")
    ))
    .to_owned()
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_name(c: char) -> bool {
    is_word(c) || c == '.' || c == '-'
}

/// `(\w+:)?notification[\s>]` at the start of `text`.
fn starts_notification(text: &str) -> bool {
    let tail = |rest: &str| {
        rest.strip_prefix("notification")
            .and_then(|after| after.chars().next())
            .is_some_and(|c| c.is_whitespace() || c == '>')
    };
    if tail(text) {
        return true;
    }
    let word: usize = text
        .chars()
        .take_while(|c| is_word(*c))
        .map(char::len_utf8)
        .sum();
    word > 0 && text[word..].starts_with(':') && tail(&text[word + 1..])
}

/// The event a NETCONF notification reports: its first element after eventTime.
pub fn notification_event(xml: &str) -> String {
    // Drop the first <eventTime>…</eventTime>.
    let mut body = xml.to_owned();
    if let Some(start) = find_event_time(&body) {
        body.replace_range(start.0..start.1, "");
    }
    let mut search = 0;
    while let Some(offset) = body[search..].find('<') {
        let at = search + offset + 1;
        search = at;
        let rest = &body[at..];
        if rest.starts_with('?') || rest.starts_with('/') || starts_notification(rest) {
            continue;
        }
        let first: usize = rest
            .chars()
            .take_while(|c| is_name(*c))
            .map(char::len_utf8)
            .sum();
        if first == 0 {
            continue;
        }
        if rest[first..].starts_with(':') {
            let second: usize = rest[first + 1..]
                .chars()
                .take_while(|c| is_name(*c))
                .map(char::len_utf8)
                .sum();
            if second > 0 {
                return rest[first + 1..first + 1 + second].to_owned();
            }
        }
        return rest[..first].to_owned();
    }
    "notification".into()
}

/// Byte range of `<(\w+:)?eventTime>[^<]*</(\w+:)?eventTime>`.
fn find_event_time(xml: &str) -> Option<(usize, usize)> {
    let mut search = 0;
    while let Some(offset) = xml[search..].find('<') {
        let start = search + offset;
        search = start + 1;
        let rest = &xml[start + 1..];
        let word: usize = rest
            .chars()
            .take_while(|c| is_word(*c))
            .map(char::len_utf8)
            .sum();
        let after_prefix = if word > 0 && rest[word..].starts_with(':') {
            &rest[word + 1..]
        } else {
            rest
        };
        let Some(text) = after_prefix.strip_prefix("eventTime>") else {
            continue;
        };
        let content: usize = text.find('<').unwrap_or(text.len());
        let closing = &text[content..];
        let Some(closing) = closing.strip_prefix("</") else {
            continue;
        };
        let word: usize = closing
            .chars()
            .take_while(|c| is_word(*c))
            .map(char::len_utf8)
            .sum();
        let closing = if word > 0 && closing[word..].starts_with(':') {
            &closing[word + 1..]
        } else {
            closing
        };
        if let Some(end) = closing.strip_prefix("eventTime>") {
            let end_at = xml.len() - end.len();
            return Some((start, end_at));
        }
    }
    None
}

/// The library's two sections: requests for this host and shared ones, filtered by name.
pub fn library_sections<'a>(
    requests: &'a [SavedManagementRequest],
    filter: &str,
) -> (
    Vec<&'a SavedManagementRequest>,
    Vec<&'a SavedManagementRequest>,
) {
    let needle = js_trim(filter).to_lowercase();
    let matching = requests
        .iter()
        .filter(|request| needle.is_empty() || request.name.to_lowercase().contains(&needle));
    let (own, shared): (Vec<_>, Vec<_>) =
        matching.partition(|request| request.profile_id.is_some());
    (own, shared)
}

/// The second line of a history row: error code, duration or `live`, size and time.
pub fn history_meta(run: &RunRecord, failed: bool) -> String {
    let mut meta = Vec::new();
    if run.status == RunStatus::Streaming {
        meta.push("live".to_owned());
    } else if let Some(duration) = run.duration_ms {
        meta.push(format!("{duration} ms"));
    }
    if let Some(bytes) = run.bytes {
        meta.push(format!("{}B", format_si(bytes as f64, 3)));
    }
    meta.push(time_of(run.started_at));
    let prefix = match (&run.error, failed) {
        (Some(error), true) => format!("{} · ", error.code.as_deref().unwrap_or("Error")),
        _ => String::new(),
    };
    format!("{prefix}{}", meta.join(" · "))
}

/// The result header's meta: duration, size and time.
pub fn run_header_meta(run: &RunRecord) -> String {
    let mut meta = Vec::new();
    if let Some(duration) = run.duration_ms {
        meta.push(format!("{duration} ms"));
    }
    if let Some(bytes) = run.bytes {
        meta.push(format!("{}B", format_si(bytes as f64, 3)));
    }
    meta.push(time_of(run.started_at));
    meta.join(" · ")
}

/// gNMI areas: value, label, hint, icon.
pub const AREAS: &[(&str, &str, &str, &str)] = &[
    (
        "data",
        "Data",
        "gNMI: read, change and stream the configuration and state",
        "DataObjectOutlined",
    ),
    (
        "operations",
        "Operations",
        "gNOI: ping, traceroute, files, health, reboot …",
        "BuildOutlined",
    ),
    (
        "security",
        "Security",
        "gNSI: authorization, certificates, SSH keys, accounting",
        "ShieldOutlined",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::management::ManagementProtocol;
    use muxus_mgmt::workbench::requests::{ManagementDraft, default_gnmi_draft};

    #[test]
    fn lists_hello_modules_with_revisions() {
        let modules = hello_modules(&[
            "urn:ietf:params:netconf:base:1.1".into(),
            "urn:ietf:params:xml:ns:yang:ietf-interfaces?module=ietf-interfaces&amp;revision=2018-02-20".into(),
            "http://example.com/a?module=alpha&features=x+y".into(),
            "urn:x?revision=1".into(),
            "urn:y?module=%62eta".into(),
        ]);
        assert_eq!(
            modules,
            vec![
                HelloModule {
                    name: "alpha".into(),
                    revision: None,
                    namespace: "http://example.com/a".into()
                },
                HelloModule {
                    name: "beta".into(),
                    revision: None,
                    namespace: "urn:y".into()
                },
                HelloModule {
                    name: "ietf-interfaces".into(),
                    revision: Some("2018-02-20".into()),
                    namespace: "urn:ietf:params:xml:ns:yang:ietf-interfaces".into()
                },
            ]
        );
    }

    #[test]
    fn names_the_event_of_a_notification() {
        let xml = r#"<notification xmlns="urn:ietf:params:xml:ns:netconf:notification:1.0"><eventTime>2026-01-01T00:00:00Z</eventTime><netconf-config-change xmlns="urn:ietf"><changed-by/></netconf-config-change></notification>"#;
        assert_eq!(notification_event(xml), "netconf-config-change");
        let prefixed = r#"<?xml version="1.0"?><nc:notification xmlns:nc="x"><nc:eventTime>t</nc:eventTime><ev:link-down xmlns:ev="y"/></nc:notification>"#;
        assert_eq!(notification_event(prefixed), "link-down");
        assert_eq!(
            notification_event("<notification></notification>"),
            "notification"
        );
        assert_eq!(notification_event("<notifications-x/>"), "notifications-x");
    }

    #[test]
    fn describes_tls() {
        let mut tls = ManagementTlsInfo {
            mode: GnmiTlsMode::Plaintext,
            verified: None,
            pinned: None,
            fingerprint: None,
            subject: None,
            issuer: None,
            valid_to: None,
            protocol: None,
        };
        assert_eq!(tls_label(&tls), "Plain text");
        assert_eq!(tls_tone(&tls), TlsTone::Warning);
        assert_eq!(tls_row(&tls), "none");
        tls.mode = GnmiTlsMode::Verify;
        tls.pinned = Some(true);
        tls.protocol = Some("TLSv1.3".into());
        tls.subject = Some("CN=leaf1".into());
        tls.fingerprint = Some("AA:BB".into());
        assert_eq!(tls_label(&tls), "TLS, pinned");
        assert_eq!(tls_tone(&tls), TlsTone::Success);
        assert_eq!(tls_row(&tls), "TLSv1.3 pinned");
        let (lines, fingerprint) = tls_tooltip(&tls);
        assert_eq!(
            lines,
            vec![
                "Certificate trusted on first use and pinned.",
                "Issued to: CN=leaf1",
                "TLSv1.3"
            ]
        );
        assert_eq!(fingerprint.as_deref(), Some("AA:BB"));
        tls.mode = GnmiTlsMode::SkipVerify;
        tls.pinned = None;
        tls.protocol = None;
        assert_eq!(tls_label(&tls), "TLS, unverified");
        assert_eq!(tls_tone(&tls), TlsTone::Default);
        assert_eq!(tls_row(&tls), "not verified");
        assert_eq!(
            encodings_tooltip(&[GnmiEncoding::JsonIetf, GnmiEncoding::Proto]),
            "Supported: JSON_IETF, PROTO"
        );
        assert_eq!(encodings_tooltip(&[]), "Supported: not listed");
    }

    #[test]
    fn summarizes_netconf_capabilities() {
        let capabilities = vec![
            "urn:ietf:params:netconf:capability:candidate:1.0".to_owned(),
            "urn:ietf:params:netconf:capability:xpath:1.0".to_owned(),
            "urn:x?module=a".to_owned(),
            "urn:y?module=b".to_owned(),
        ];
        assert_eq!(
            netconf_module_chip(&capabilities),
            ("2 modules".into(), "candidate · xpath".into())
        );
        assert_eq!(netconf_module_chip(&[]).1, "No optional capabilities");
    }

    fn saved(name: &str, profile: Option<&str>) -> SavedManagementRequest {
        SavedManagementRequest {
            id: name.into(),
            protocol: ManagementProtocol::Gnmi,
            profile_id: profile.map(Into::into),
            name: name.into(),
            request: serde_json::Map::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn splits_the_library_by_host() {
        let requests = vec![
            saved("Counters", Some("p1")),
            saved("BGP", None),
            saved("Config", None),
        ];
        let (own, shared) = library_sections(&requests, "");
        assert_eq!(own.len(), 1);
        assert_eq!(shared.len(), 2);
        let (own, shared) = library_sections(&requests, " co ");
        assert_eq!(own.len(), 1);
        assert_eq!(shared.len(), 1);
    }

    #[test]
    fn writes_history_meta() {
        let mut run = RunRecord {
            id: "r".into(),
            draft: ManagementDraft::Gnmi(default_gnmi_draft()),
            label: "Get".into(),
            summary: "/".into(),
            started_at: 0,
            status: RunStatus::Error,
            duration_ms: Some(12),
            bytes: Some(2048),
            error: Some(muxus_core::management::ManagementError {
                message: "nope".into(),
                code: Some("NOT_FOUND".into()),
                cancelled: None,
            }),
            result: None,
            live: None,
            synced: false,
            hidden: false,
        };
        let meta = history_meta(&run, true);
        assert!(meta.starts_with("NOT_FOUND · 12 ms · 2.05 kB · "), "{meta}");
        run.status = RunStatus::Streaming;
        run.bytes = None;
        assert!(history_meta(&run, false).starts_with("live · "));
        assert!(run_header_meta(&run).starts_with("12 ms · "));
    }
}
