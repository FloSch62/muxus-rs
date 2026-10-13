//! What the request editor holds. These drafts are also what the library
//! stores and what history entries replay, so they keep everything needed
//! to run the request again (port of `client/src/management/requests.ts`).

use std::sync::atomic::{AtomicU64, Ordering};

use muxus_core::js::js_trim;
use muxus_core::management::{
    GnmiDataType, GnmiEncoding, GnmiSetEncoding, GnmiSubscriptionListMode, GnmiSubscriptionMode,
};
use muxus_core::protocol::{GnmiCommit, GnmiSetValue, GnmiSubscription};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::xml::{escape_xml, unwrap_rpc};
use crate::gnmi::{GnmiGetRequest, GnmiSetRequest, GnmiSubscribeRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GnmiOperation {
    #[default]
    Get,
    Set,
    Subscribe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GnmiSetItemOp {
    #[default]
    Update,
    Replace,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GnmiSetItem {
    pub id: String,
    pub op: GnmiSetItemOp,
    pub path: String,
    /// JSON text (json_ietf/json) or raw text (ascii).
    pub value: String,
    pub encoding: GnmiSetEncoding,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GnmiSubscribeDraft {
    pub list_mode: GnmiSubscriptionListMode,
    pub mode: GnmiSubscriptionMode,
    #[serde(with = "muxus_core::js::number")]
    pub sample_seconds: f64,
    pub suppress_redundant: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "muxus_core::js::number::option"
    )]
    pub heartbeat_seconds: Option<f64>,
    pub updates_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GnmiDraft {
    pub operation: GnmiOperation,
    /// Get and Subscribe paths.
    pub paths: Vec<String>,
    pub prefix: String,
    pub data_type: GnmiDataType,
    /// Absent: the session's encoding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<GnmiEncoding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    pub set: Vec<GnmiSetItem>,
    /// Ask for automatic rollback unless confirmed (gNMI commit-confirmed extension).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_seconds: Option<u32>,
    pub subscribe: GnmiSubscribeDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetconfOperation {
    Get,
    #[default]
    GetConfig,
    EditConfig,
    Lock,
    Unlock,
    Validate,
    Commit,
    DiscardChanges,
    CancelCommit,
    CopyConfig,
    DeleteConfig,
    GetSchema,
    KillSession,
    CreateSubscription,
    Custom,
}

impl NetconfOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            NetconfOperation::Get => "get",
            NetconfOperation::GetConfig => "get-config",
            NetconfOperation::EditConfig => "edit-config",
            NetconfOperation::Lock => "lock",
            NetconfOperation::Unlock => "unlock",
            NetconfOperation::Validate => "validate",
            NetconfOperation::Commit => "commit",
            NetconfOperation::DiscardChanges => "discard-changes",
            NetconfOperation::CancelCommit => "cancel-commit",
            NetconfOperation::CopyConfig => "copy-config",
            NetconfOperation::DeleteConfig => "delete-config",
            NetconfOperation::GetSchema => "get-schema",
            NetconfOperation::KillSession => "kill-session",
            NetconfOperation::CreateSubscription => "create-subscription",
            NetconfOperation::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Datastore {
    #[default]
    Running,
    Candidate,
    Startup,
}

impl Datastore {
    pub fn as_str(self) -> &'static str {
        match self {
            Datastore::Running => "running",
            Datastore::Candidate => "candidate",
            Datastore::Startup => "startup",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterType {
    #[default]
    Subtree,
    Xpath,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetconfDraft {
    pub operation: NetconfOperation,
    pub source: Datastore,
    pub target: Datastore,
    pub filter_type: FilterType,
    /// Subtree filter XML, or an XPath expression.
    pub filter: String,
    /// edit-config `<config>` content.
    pub config: String,
    /// `''`, `merge`, `replace` or `none`.
    pub default_operation: String,
    /// `''`, `test-then-set`, `set` or `test-only`.
    pub test_option: String,
    /// `''`, `stop-on-error`, `continue-on-error` or `rollback-on-error`.
    pub error_option: String,
    /// `''`, `report-all`, `report-all-tagged`, `trim` or `explicit`.
    pub with_defaults: String,
    pub confirmed: bool,
    #[serde(with = "muxus_core::js::number")]
    pub confirm_timeout: f64,
    pub persist: String,
    pub schema_identifier: String,
    pub schema_version: String,
    pub session_id: String,
    /// Custom RPC body.
    pub rpc: String,
}

/// A request in the editor, tagged by protocol like the TypeScript union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "lowercase")]
pub enum ManagementDraft {
    Gnmi(GnmiDraft),
    Netconf(NetconfDraft),
}

static ITEM_COUNTER: AtomicU64 = AtomicU64::new(0);

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("ASCII digits")
}

pub(crate) fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// `Date.now().toString(36)`.
pub(crate) fn now_base36() -> String {
    base36(now_ms())
}

pub fn new_item_id() -> String {
    let counter = ITEM_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("i{}{counter}", now_base36())
}

pub fn default_gnmi_draft() -> GnmiDraft {
    GnmiDraft {
        operation: GnmiOperation::Get,
        paths: vec![String::new()],
        prefix: String::new(),
        data_type: GnmiDataType::All,
        encoding: None,
        depth: None,
        set: vec![GnmiSetItem {
            id: new_item_id(),
            op: GnmiSetItemOp::Update,
            path: String::new(),
            value: String::new(),
            encoding: GnmiSetEncoding::JsonIetf,
        }],
        confirm_seconds: None,
        subscribe: GnmiSubscribeDraft {
            list_mode: GnmiSubscriptionListMode::Stream,
            mode: GnmiSubscriptionMode::Sample,
            sample_seconds: 10.0,
            suppress_redundant: false,
            heartbeat_seconds: None,
            updates_only: false,
        },
    }
}

pub fn default_netconf_draft() -> NetconfDraft {
    NetconfDraft {
        operation: NetconfOperation::GetConfig,
        source: Datastore::Running,
        target: Datastore::Candidate,
        filter_type: FilterType::Subtree,
        filter: String::new(),
        config: String::new(),
        default_operation: String::new(),
        test_option: String::new(),
        error_option: String::new(),
        with_defaults: String::new(),
        confirmed: false,
        confirm_timeout: 600.0,
        persist: String::new(),
        schema_identifier: String::new(),
        schema_version: String::new(),
        session_id: String::new(),
        rpc: String::new(),
    }
}

/// `{ ...base, ...stored }`: stored fields win, missing ones keep the defaults.
fn overlay(base: Value, stored: &Map<String, Value>) -> Map<String, Value> {
    let mut merged = match base {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    for (key, value) in stored {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

/// Bring a stored gNMI draft up to the current shape (the library keeps old ones).
pub fn normalize_gnmi_draft(stored: &Map<String, Value>) -> GnmiDraft {
    let base = default_gnmi_draft();
    let mut merged = overlay(
        serde_json::to_value(&base).expect("draft serializes"),
        stored,
    );
    merged.remove("protocol");
    let subscribe = overlay(
        serde_json::to_value(&base.subscribe).expect("draft serializes"),
        stored
            .get("subscribe")
            .and_then(Value::as_object)
            .unwrap_or(&Map::new()),
    );
    merged.insert("subscribe".into(), Value::Object(subscribe));
    let mut draft: GnmiDraft =
        serde_json::from_value(Value::Object(merged)).unwrap_or_else(|_| base.clone());
    if draft.paths.is_empty() {
        draft.paths = vec![String::new()];
    }
    if draft.set.is_empty() {
        draft.set = base.set;
    }
    draft
}

/// Bring a stored NETCONF draft up to the current shape.
pub fn normalize_netconf_draft(stored: &Map<String, Value>) -> NetconfDraft {
    let base = default_netconf_draft();
    let mut merged = overlay(
        serde_json::to_value(&base).expect("draft serializes"),
        stored,
    );
    merged.remove("protocol");
    serde_json::from_value(Value::Object(merged)).unwrap_or(base)
}

pub fn non_empty_paths(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .map(|path| js_trim(path).to_owned())
        .filter(|path| !path.is_empty())
        .collect()
}

fn trimmed(text: &str) -> Option<String> {
    let text = js_trim(text);
    (!text.is_empty()).then(|| text.to_owned())
}

/// The Get a draft runs, with the session's encoding unless it names one.
pub fn gnmi_get_message(draft: &GnmiDraft) -> (GnmiGetRequest, Option<GnmiEncoding>) {
    let paths = non_empty_paths(&draft.paths);
    (
        GnmiGetRequest {
            prefix: trimmed(&draft.prefix),
            paths: if paths.is_empty() {
                vec!["/".into()]
            } else {
                paths
            },
            data_type: draft.data_type,
            depth: draft.depth.filter(|depth| *depth > 0),
            max_bytes: None,
        },
        draft.encoding,
    )
}

/// `commit_id` turns on the commit-confirmed extension when the draft asks for a rollback timer.
pub fn gnmi_set_message(draft: &GnmiDraft, commit_id: Option<&str>) -> GnmiSetRequest {
    let items: Vec<&GnmiSetItem> = draft
        .set
        .iter()
        .filter(|item| !js_trim(&item.path).is_empty())
        .collect();
    let value = |item: &&GnmiSetItem| GnmiSetValue {
        path: js_trim(&item.path).to_owned(),
        value: item.value.clone(),
        encoding: item.encoding,
    };
    GnmiSetRequest {
        prefix: trimmed(&draft.prefix),
        updates: items
            .iter()
            .filter(|item| item.op == GnmiSetItemOp::Update)
            .map(value)
            .collect(),
        replaces: items
            .iter()
            .filter(|item| item.op == GnmiSetItemOp::Replace)
            .map(value)
            .collect(),
        deletes: items
            .iter()
            .filter(|item| item.op == GnmiSetItemOp::Delete)
            .map(|item| js_trim(&item.path).to_owned())
            .collect(),
        commit: match (commit_id, draft.confirm_seconds) {
            (Some(id), Some(seconds)) if seconds > 0 => Some(GnmiCommit::Commit {
                id: id.to_owned(),
                rollback_seconds: Some(seconds),
            }),
            _ => None,
        },
    }
}

/// `Math.round` for the non-negative values drafts hold.
fn round(value: f64) -> f64 {
    (value + 0.5).floor()
}

pub fn gnmi_subscribe_message(draft: &GnmiDraft) -> (GnmiSubscribeRequest, Option<GnmiEncoding>) {
    let sub = &draft.subscribe;
    let stream = sub.list_mode == GnmiSubscriptionListMode::Stream;
    (
        GnmiSubscribeRequest {
            prefix: trimmed(&draft.prefix),
            mode: sub.list_mode,
            updates_only: sub.updates_only,
            subscriptions: non_empty_paths(&draft.paths)
                .into_iter()
                .map(|path| GnmiSubscription {
                    path,
                    mode: if stream {
                        sub.mode
                    } else {
                        GnmiSubscriptionMode::TargetDefined
                    },
                    sample_interval_ms: (stream && sub.mode == GnmiSubscriptionMode::Sample)
                        .then(|| round(sub.sample_seconds * 1000.0).max(1.0) as u64),
                    suppress_redundant: (stream && sub.suppress_redundant).then_some(true),
                    heartbeat_interval_ms: sub
                        .heartbeat_seconds
                        .filter(|seconds| stream && *seconds > 0.0)
                        .map(|seconds| round(seconds * 1000.0) as u64),
                })
                .collect(),
        },
        draft.encoding,
    )
}

/// Why the draft cannot run yet, if it cannot.
pub fn gnmi_draft_problem(draft: &GnmiDraft) -> Option<String> {
    match draft.operation {
        GnmiOperation::Set => {
            let items: Vec<&GnmiSetItem> = draft
                .set
                .iter()
                .filter(|item| !js_trim(&item.path).is_empty())
                .collect();
            if items.is_empty() {
                return Some("Add a path to change.".into());
            }
            for item in items {
                if item.op == GnmiSetItemOp::Delete || item.encoding == GnmiSetEncoding::Ascii {
                    continue;
                }
                if serde_json::from_str::<Value>(&item.value).is_err() {
                    return Some(format!(
                        "The value for {} is not valid JSON{}.",
                        js_trim(&item.path),
                        if js_trim(&item.value).is_empty() {
                            " (it is empty)"
                        } else {
                            ""
                        }
                    ));
                }
            }
            None
        }
        GnmiOperation::Subscribe if non_empty_paths(&draft.paths).is_empty() => {
            Some("Add a path to subscribe to.".into())
        }
        _ => None,
    }
}

// NETCONF

fn datastore(name: Datastore) -> String {
    format!("<{}/>", name.as_str())
}

fn filter_xml(draft: &NetconfDraft) -> String {
    let filter = js_trim(&draft.filter);
    if draft.filter_type == FilterType::None || filter.is_empty() {
        return String::new();
    }
    if draft.filter_type == FilterType::Xpath {
        return format!(r#"<filter type="xpath" select="{}"/>"#, escape_xml(filter));
    }
    format!(r#"<filter type="subtree">{filter}</filter>"#)
}

fn with_defaults_xml(draft: &NetconfDraft) -> String {
    if draft.with_defaults.is_empty() {
        return String::new();
    }
    format!(
        r#"<with-defaults xmlns="urn:ietf:params:xml:ns:yang:ietf-netconf-with-defaults">{}</with-defaults>"#,
        draft.with_defaults
    )
}

/// The operation element(s) the `<rpc>` wraps.
pub fn netconf_rpc_body(draft: &NetconfDraft) -> String {
    let persist = js_trim(&draft.persist);
    match draft.operation {
        NetconfOperation::Get => format!(
            "<get>{}{}</get>",
            filter_xml(draft),
            with_defaults_xml(draft)
        ),
        NetconfOperation::GetConfig => format!(
            "<get-config><source>{}</source>{}{}</get-config>",
            datastore(draft.source),
            filter_xml(draft),
            with_defaults_xml(draft)
        ),
        NetconfOperation::EditConfig => {
            let mut xml = format!("<edit-config><target>{}</target>", datastore(draft.target));
            if !draft.default_operation.is_empty() {
                xml += &format!(
                    "<default-operation>{}</default-operation>",
                    draft.default_operation
                );
            }
            if !draft.test_option.is_empty() {
                xml += &format!("<test-option>{}</test-option>", draft.test_option);
            }
            if !draft.error_option.is_empty() {
                xml += &format!("<error-option>{}</error-option>", draft.error_option);
            }
            xml + &format!("<config>{}</config></edit-config>", js_trim(&draft.config))
        }
        NetconfOperation::Lock => {
            format!("<lock><target>{}</target></lock>", datastore(draft.target))
        }
        NetconfOperation::Unlock => {
            format!(
                "<unlock><target>{}</target></unlock>",
                datastore(draft.target)
            )
        }
        NetconfOperation::Validate => {
            format!(
                "<validate><source>{}</source></validate>",
                datastore(draft.source)
            )
        }
        NetconfOperation::Commit => {
            if draft.confirmed {
                let timeout = round(draft.confirm_timeout).max(1.0) as u64;
                let mut xml =
                    format!("<commit><confirmed/><confirm-timeout>{timeout}</confirm-timeout>");
                if !persist.is_empty() {
                    xml += &format!("<persist>{}</persist>", escape_xml(persist));
                }
                xml + "</commit>"
            } else if !persist.is_empty() {
                format!(
                    "<commit><persist-id>{}</persist-id></commit>",
                    escape_xml(persist)
                )
            } else {
                "<commit/>".into()
            }
        }
        NetconfOperation::DiscardChanges => "<discard-changes/>".into(),
        NetconfOperation::CancelCommit => {
            if persist.is_empty() {
                "<cancel-commit/>".into()
            } else {
                format!(
                    "<cancel-commit><persist-id>{}</persist-id></cancel-commit>",
                    escape_xml(persist)
                )
            }
        }
        NetconfOperation::CopyConfig => format!(
            "<copy-config><target>{}</target><source>{}</source></copy-config>",
            datastore(draft.target),
            datastore(draft.source)
        ),
        NetconfOperation::DeleteConfig => format!(
            "<delete-config><target>{}</target></delete-config>",
            datastore(draft.target)
        ),
        NetconfOperation::GetSchema => {
            let version = js_trim(&draft.schema_version);
            format!(
                "<get-schema xmlns=\"urn:ietf:params:xml:ns:yang:ietf-netconf-monitoring\"><identifier>{}</identifier>{}<format>yang</format></get-schema>",
                escape_xml(js_trim(&draft.schema_identifier)),
                if version.is_empty() {
                    String::new()
                } else {
                    format!("<version>{}</version>", escape_xml(version))
                }
            )
        }
        NetconfOperation::KillSession => format!(
            "<kill-session><session-id>{}</session-id></kill-session>",
            escape_xml(js_trim(&draft.session_id))
        ),
        NetconfOperation::CreateSubscription => {
            let filter = js_trim(&draft.filter);
            format!(
                "<create-subscription xmlns=\"urn:ietf:params:xml:ns:netconf:notification:1.0\">{}</create-subscription>",
                if !filter.is_empty() && draft.filter_type == FilterType::Subtree {
                    format!(r#"<filter type="subtree">{filter}</filter>"#)
                } else {
                    String::new()
                }
            )
        }
        NetconfOperation::Custom => unwrap_rpc(js_trim(&draft.rpc)),
    }
}

pub fn netconf_draft_problem(draft: &NetconfDraft) -> Option<String> {
    let blank = |text: &str| js_trim(text).is_empty();
    match draft.operation {
        NetconfOperation::EditConfig if blank(&draft.config) => {
            Some("Enter the configuration to apply.".into())
        }
        NetconfOperation::GetSchema if blank(&draft.schema_identifier) => {
            Some("Enter the module name.".into())
        }
        NetconfOperation::KillSession
            if {
                let id = js_trim(&draft.session_id);
                id.is_empty() || !id.chars().all(|c| c.is_ascii_digit())
            } =>
        {
            Some("Enter a session id.".into())
        }
        NetconfOperation::Custom if blank(&draft.rpc) => Some("Enter the RPC to send.".into()),
        _ => None,
    }
}

/// Operations that change device state, which run behind a confirmation in shared sessions.
pub fn netconf_changes_state(operation: NetconfOperation) -> bool {
    matches!(
        operation,
        NetconfOperation::EditConfig
            | NetconfOperation::Commit
            | NetconfOperation::CopyConfig
            | NetconfOperation::DeleteConfig
            | NetconfOperation::DiscardChanges
            | NetconfOperation::KillSession
    )
}

/// One NETCONF operation the editor offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetconfOperationChoice {
    pub value: NetconfOperation,
    pub label: &'static str,
    pub description: &'static str,
}

/// The editor's operation menu, grouped.
pub const NETCONF_OPERATION_GROUPS: &[(&str, &[NetconfOperationChoice])] = &[
    (
        "Retrieve",
        &[
            NetconfOperationChoice {
                value: NetconfOperation::GetConfig,
                label: "get-config",
                description: "Configuration from a datastore",
            },
            NetconfOperationChoice {
                value: NetconfOperation::Get,
                label: "get",
                description: "Running configuration and state",
            },
            NetconfOperationChoice {
                value: NetconfOperation::GetSchema,
                label: "get-schema",
                description: "A YANG module from the device",
            },
        ],
    ),
    (
        "Change",
        &[
            NetconfOperationChoice {
                value: NetconfOperation::EditConfig,
                label: "edit-config",
                description: "Apply configuration to a datastore",
            },
            NetconfOperationChoice {
                value: NetconfOperation::CopyConfig,
                label: "copy-config",
                description: "Replace one datastore with another",
            },
            NetconfOperationChoice {
                value: NetconfOperation::DeleteConfig,
                label: "delete-config",
                description: "Delete a datastore",
            },
        ],
    ),
    (
        "Candidate",
        &[
            NetconfOperationChoice {
                value: NetconfOperation::Validate,
                label: "validate",
                description: "Check a datastore without applying it",
            },
            NetconfOperationChoice {
                value: NetconfOperation::Commit,
                label: "commit",
                description: "Make the candidate the running configuration",
            },
            NetconfOperationChoice {
                value: NetconfOperation::DiscardChanges,
                label: "discard-changes",
                description: "Reset the candidate to running",
            },
            NetconfOperationChoice {
                value: NetconfOperation::CancelCommit,
                label: "cancel-commit",
                description: "Roll back a confirmed commit",
            },
            NetconfOperationChoice {
                value: NetconfOperation::Lock,
                label: "lock",
                description: "Keep other sessions out of a datastore",
            },
            NetconfOperationChoice {
                value: NetconfOperation::Unlock,
                label: "unlock",
                description: "Release a lock",
            },
        ],
    ),
    (
        "Session",
        &[
            NetconfOperationChoice {
                value: NetconfOperation::CreateSubscription,
                label: "create-subscription",
                description: "Receive event notifications",
            },
            NetconfOperationChoice {
                value: NetconfOperation::KillSession,
                label: "kill-session",
                description: "End another NETCONF session",
            },
            NetconfOperationChoice {
                value: NetconfOperation::Custom,
                label: "Custom RPC",
                description: "Any RPC, written by hand",
            },
        ],
    ),
];

pub fn netconf_operation_label(operation: NetconfOperation) -> &'static str {
    NETCONF_OPERATION_GROUPS
        .iter()
        .flat_map(|(_, operations)| operations.iter())
        .find(|choice| choice.value == operation)
        .map_or(operation.as_str(), |choice| choice.label)
}

/// One-line summary for history and library rows.
pub fn draft_summary(draft: &ManagementDraft) -> String {
    match draft {
        ManagementDraft::Gnmi(draft) => {
            if draft.operation == GnmiOperation::Set {
                let summary = draft
                    .set
                    .iter()
                    .filter(|item| !js_trim(&item.path).is_empty())
                    .map(|item| {
                        let op = match item.op {
                            GnmiSetItemOp::Update => "update",
                            GnmiSetItemOp::Replace => "replace",
                            GnmiSetItemOp::Delete => "delete",
                        };
                        format!("{op} {}", js_trim(&item.path))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                return if summary.is_empty() {
                    "Set".into()
                } else {
                    summary
                };
            }
            let paths = non_empty_paths(&draft.paths).join(", ");
            if paths.is_empty() { "/".into() } else { paths }
        }
        ManagementDraft::Netconf(draft) => match draft.operation {
            NetconfOperation::Get | NetconfOperation::GetConfig => {
                let filter = js_trim(&draft.filter);
                if filter.is_empty() {
                    "everything".into()
                } else if draft.filter_type == FilterType::Xpath {
                    filter.to_owned()
                } else {
                    "with filter".into()
                }
            }
            NetconfOperation::GetSchema => draft.schema_identifier.clone(),
            NetconfOperation::EditConfig
            | NetconfOperation::Lock
            | NetconfOperation::Unlock
            | NetconfOperation::DeleteConfig => draft.target.as_str().into(),
            NetconfOperation::CopyConfig => {
                format!("{} → {}", draft.source.as_str(), draft.target.as_str())
            }
            _ => String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn builds_netconf_rpcs_from_the_editor_state() {
        let draft = default_netconf_draft();
        assert_eq!(
            netconf_rpc_body(&NetconfDraft {
                operation: NetconfOperation::GetConfig,
                source: Datastore::Candidate,
                filter_type: FilterType::Subtree,
                filter: "<system/>".into(),
                ..draft.clone()
            }),
            r#"<get-config><source><candidate/></source><filter type="subtree"><system/></filter></get-config>"#
        );
        assert_eq!(
            netconf_rpc_body(&NetconfDraft {
                operation: NetconfOperation::Get,
                filter_type: FilterType::Xpath,
                filter: "/a[b='c']".into(),
                ..draft.clone()
            }),
            r#"<get><filter type="xpath" select="/a[b=&apos;c&apos;]"/></get>"#
        );
        assert_eq!(
            netconf_rpc_body(&NetconfDraft {
                operation: NetconfOperation::Commit,
                confirmed: true,
                confirm_timeout: 120.0,
                ..draft.clone()
            }),
            "<commit><confirmed/><confirm-timeout>120</confirm-timeout></commit>"
        );
        assert_eq!(
            netconf_rpc_body(&NetconfDraft {
                operation: NetconfOperation::EditConfig,
                target: Datastore::Candidate,
                default_operation: "merge".into(),
                config: "<x/>".into(),
                ..draft
            }),
            "<edit-config><target><candidate/></target><default-operation>merge</default-operation><config><x/></config></edit-config>"
        );
    }

    #[test]
    fn builds_gnmi_set_and_subscribe_messages_and_catches_invalid_json_first() {
        let item = |id: &str, op, path: &str, value: &str| GnmiSetItem {
            id: id.into(),
            op,
            path: path.into(),
            value: value.into(),
            encoding: GnmiSetEncoding::JsonIetf,
        };
        let draft = GnmiDraft {
            operation: GnmiOperation::Set,
            set: vec![
                item("1", GnmiSetItemOp::Update, "/a", "\"x\""),
                item("2", GnmiSetItemOp::Delete, "/b", ""),
                item("3", GnmiSetItemOp::Replace, " ", ""),
            ],
            ..default_gnmi_draft()
        };
        assert_eq!(
            gnmi_set_message(&draft, None),
            GnmiSetRequest {
                prefix: None,
                updates: vec![GnmiSetValue {
                    path: "/a".into(),
                    value: "\"x\"".into(),
                    encoding: GnmiSetEncoding::JsonIetf
                }],
                replaces: vec![],
                deletes: vec!["/b".into()],
                commit: None,
            }
        );
        let broken = GnmiDraft {
            set: vec![item("1", GnmiSetItemOp::Update, "/a", "{oops")],
            ..draft
        };
        assert!(
            gnmi_draft_problem(&broken)
                .unwrap()
                .contains("not valid JSON")
        );
        let subscribe = GnmiDraft {
            operation: GnmiOperation::Subscribe,
            paths: vec!["/x".into(), String::new()],
            subscribe: GnmiSubscribeDraft {
                sample_seconds: 2.5,
                suppress_redundant: true,
                heartbeat_seconds: Some(60.0),
                ..default_gnmi_draft().subscribe
            },
            ..default_gnmi_draft()
        };
        assert_eq!(
            gnmi_subscribe_message(&subscribe).0.subscriptions,
            vec![GnmiSubscription {
                path: "/x".into(),
                mode: GnmiSubscriptionMode::Sample,
                sample_interval_ms: Some(2500),
                suppress_redundant: Some(true),
                heartbeat_interval_ms: Some(60_000),
            }]
        );
        let stored = json!({"operation": "get", "paths": []});
        assert_eq!(
            normalize_gnmi_draft(stored.as_object().unwrap()).paths,
            vec![String::new()]
        );
    }

    #[test]
    fn drafts_round_trip_through_the_library_shape() {
        let draft = ManagementDraft::Gnmi(default_gnmi_draft());
        let json = serde_json::to_value(&draft).unwrap();
        assert_eq!(json["protocol"], "gnmi");
        assert_eq!(json["dataType"], "all");
        assert_eq!(json["subscribe"]["listMode"], "stream");
        assert_eq!(json["subscribe"]["sampleSeconds"], 10.0);
        let back: ManagementDraft = serde_json::from_value(json).unwrap();
        assert_eq!(back, draft);
        let netconf = normalize_netconf_draft(
            json!({"protocol": "netconf", "operation": "commit", "confirmed": true})
                .as_object()
                .unwrap(),
        );
        assert_eq!(netconf.operation, NetconfOperation::Commit);
        assert_eq!(netconf.confirm_timeout, 600.0);
    }

    #[test]
    fn summarizes_drafts_for_history_rows() {
        assert_eq!(
            draft_summary(&ManagementDraft::Gnmi(GnmiDraft {
                paths: vec!["/a".into(), " /b ".into()],
                ..default_gnmi_draft()
            })),
            "/a, /b"
        );
        assert_eq!(
            draft_summary(&ManagementDraft::Netconf(NetconfDraft {
                operation: NetconfOperation::CopyConfig,
                ..default_netconf_draft()
            })),
            "running → candidate"
        );
        assert_eq!(
            netconf_operation_label(NetconfOperation::Custom),
            "Custom RPC"
        );
        assert_eq!(
            netconf_draft_problem(&NetconfDraft {
                operation: NetconfOperation::KillSession,
                session_id: "x".into(),
                ..default_netconf_draft()
            })
            .as_deref(),
            Some("Enter a session id.")
        );
    }
}
