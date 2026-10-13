//! Shapes shared by the NETCONF and gNMI management sessions (port of
//! `shared/src/management.ts`). The backend speaks the device protocols
//! (NETCONF over an SSH subsystem, gNMI over gRPC) and hands the UI decoded,
//! JSON-friendly messages: gNMI values are already unwrapped from their
//! TypedValue and paths are gNMI path strings.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManagementProtocol {
    Gnmi,
    Netconf,
}

impl ManagementProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            ManagementProtocol::Gnmi => "gnmi",
            ManagementProtocol::Netconf => "netconf",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "gnmi" => Some(ManagementProtocol::Gnmi),
            "netconf" => Some(ManagementProtocol::Netconf),
            _ => None,
        }
    }
}

pub const GNMI_DEFAULT_PORT: u16 = 57400;
pub const NETCONF_DEFAULT_PORT: u16 = 830;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GnmiEncoding {
    JsonIetf,
    Json,
    Proto,
    Ascii,
    Bytes,
}

pub const GNMI_ENCODINGS: [GnmiEncoding; 5] = [
    GnmiEncoding::JsonIetf,
    GnmiEncoding::Json,
    GnmiEncoding::Proto,
    GnmiEncoding::Ascii,
    GnmiEncoding::Bytes,
];

/// The encodings a gNMI Set value may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GnmiSetEncoding {
    JsonIetf,
    Json,
    Ascii,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GnmiDataType {
    #[default]
    All,
    Config,
    State,
    Operational,
}

pub const GNMI_DATA_TYPES: [GnmiDataType; 4] = [
    GnmiDataType::All,
    GnmiDataType::Config,
    GnmiDataType::State,
    GnmiDataType::Operational,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GnmiSubscriptionListMode {
    Stream,
    Once,
    Poll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GnmiSubscriptionMode {
    #[default]
    TargetDefined,
    OnChange,
    Sample,
}

/// How a gNMI host secures its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GnmiTlsMode {
    /// Verify the certificate; one that does not verify is shown once and pinned.
    #[default]
    Verify,
    /// TLS without checking the certificate at all (lab devices with throwaway certificates).
    SkipVerify,
    /// Plain-text HTTP/2: no encryption.
    Plaintext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GnmiValueType {
    Json,
    JsonIetf,
    String,
    Int,
    Uint,
    Bool,
    Double,
    Float,
    Decimal,
    Bytes,
    Ascii,
    Leaflist,
    Any,
    Proto,
    None,
}

/// A TypedValue, unwrapped. JSON encodings are parsed; 64-bit integers stay
/// numbers while they are exact and become decimal strings beyond that;
/// bytes are base64.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GnmiValue {
    #[serde(rename = "type")]
    pub kind: GnmiValueType,
    pub value: serde_json::Value,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GnmiUpdate {
    /// Absolute path: the notification prefix joined with the update's own path.
    pub path: String,
    pub value: GnmiValue,
    pub duplicates: Option<u64>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GnmiNotification {
    /// Nanoseconds since the epoch, as a decimal string (exceeds 2^53).
    pub timestamp: String,
    pub prefix: Option<String>,
    pub atomic: Option<bool>,
    pub updates: Vec<GnmiUpdate>,
    /// Absolute paths the device removed.
    pub deletes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GnmiModel {
    pub name: String,
    pub organization: String,
    pub version: String,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementTlsInfo {
    pub mode: GnmiTlsMode,
    /// The certificate verified against trusted authorities and named the host.
    pub verified: Option<bool>,
    /// The certificate was accepted because it is the one pinned before.
    pub pinned: Option<bool>,
    pub fingerprint: Option<String>,
    pub subject: Option<String>,
    pub issuer: Option<String>,
    pub valid_to: Option<String>,
    pub protocol: Option<String>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GnmiSessionInfo {
    /// user@host:port as dialed.
    pub address: String,
    /// SSH gateway or jump host the session runs through.
    pub via: Option<String>,
    pub connected_at: String,
    pub version: String,
    pub encodings: Vec<GnmiEncoding>,
    pub models: Vec<GnmiModel>,
    pub tls: ManagementTlsInfo,
    /// gRPC services the server lists through reflection
    /// (gnoi.system.System, gnsi.authz.v1.Authz, …); absent when it does not
    /// offer reflection.
    pub services: Option<Vec<String>>,
}

/// NETCONF framing in use: 1.1 (chunked) when both ends offered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NetconfBase {
    #[serde(rename = "1.0")]
    V1_0,
    #[serde(rename = "1.1")]
    V1_1,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetconfSessionInfo {
    /// user@host:port as dialed.
    pub address: String,
    /// SSH gateway or jump host the session runs through.
    pub via: Option<String>,
    pub connected_at: String,
    pub session_id: String,
    pub base: NetconfBase,
    pub capabilities: Vec<String>,
    pub server_software: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "lowercase")]
pub enum ManagementSessionInfo {
    Gnmi(GnmiSessionInfo),
    Netconf(NetconfSessionInfo),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GnmiSetOp {
    Update,
    Replace,
    Delete,
    UnionReplace,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GnmiSetResult {
    pub path: String,
    pub op: GnmiSetOp,
}

/// A gNOI/gNSI protobuf message as JSON: proto field names, enums by name,
/// bytes as base64, 64-bit integers as numbers while exact and as decimal
/// strings beyond.
pub type GrpcJson = serde_json::Map<String, serde_json::Value>;

/// What a finished request returns, by request op.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ManagementResult {
    GnmiCapabilities {
        info: GnmiSessionInfo,
    },
    GnmiGet {
        notifications: Vec<GnmiNotification>,
    },
    GnmiSet {
        timestamp: String,
        results: Vec<GnmiSetResult>,
    },
    #[serde(rename_all = "camelCase")]
    NetconfRpc {
        xml: String,
        message_id: String,
    },
    /// A unary gNOI/gNSI call: the response in its JSON form.
    GrpcCall {
        method: String,
        response: GrpcJson,
    },
    /// A file download finished; its bytes arrived as `gnoi-file-data` messages.
    #[serde(rename_all = "camelCase")]
    GnoiFileGet {
        size: u64,
        verified: bool,
        hash_method: Option<String>,
    },
    GnoiFilePut {
        size: u64,
    },
    /// The certificate the device presents on a fresh TLS connection right now.
    TlsProbe {
        certificate: PresentedCertificateInfo,
    },
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentedCertificateInfo {
    pub fingerprint: String,
    pub subject: String,
    pub issuer: String,
    pub valid_from: String,
    pub valid_to: String,
    /// Why it does not verify against trusted authorities, if it does not.
    pub verification_error: Option<String>,
}

/// gNOI and gNSI calls the workbench makes, by gRPC method path.
pub mod grpc_methods {
    pub const PING: &str = "/gnoi.system.System/Ping";
    pub const TRACEROUTE: &str = "/gnoi.system.System/Traceroute";
    pub const TIME: &str = "/gnoi.system.System/Time";
    pub const REBOOT: &str = "/gnoi.system.System/Reboot";
    pub const REBOOT_STATUS: &str = "/gnoi.system.System/RebootStatus";
    pub const CANCEL_REBOOT: &str = "/gnoi.system.System/CancelReboot";
    pub const KILL_PROCESS: &str = "/gnoi.system.System/KillProcess";
    pub const FILE_STAT: &str = "/gnoi.file.File/Stat";
    pub const FILE_REMOVE: &str = "/gnoi.file.File/Remove";
    pub const HEALTHZ_GET: &str = "/gnoi.healthz.Healthz/Get";
    pub const HEALTHZ_LIST: &str = "/gnoi.healthz.Healthz/List";
    pub const HEALTHZ_ACKNOWLEDGE: &str = "/gnoi.healthz.Healthz/Acknowledge";
    pub const HEALTHZ_CHECK: &str = "/gnoi.healthz.Healthz/Check";
    pub const BGP_CLEAR: &str = "/gnoi.bgp.BGP/ClearBGPNeighbor";
    pub const OS_VERIFY: &str = "/gnoi.os.OS/Verify";
    pub const AUTHZ_GET: &str = "/gnsi.authz.v1.Authz/Get";
    pub const AUTHZ_PROBE: &str = "/gnsi.authz.v1.Authz/Probe";
    pub const AUTHZ_ROTATE: &str = "/gnsi.authz.v1.Authz/Rotate";
    pub const PATHZ_GET: &str = "/gnsi.pathz.v1.Pathz/Get";
    pub const PATHZ_PROBE: &str = "/gnsi.pathz.v1.Pathz/Probe";
    pub const PATHZ_ROTATE: &str = "/gnsi.pathz.v1.Pathz/Rotate";
    pub const CERTZ_PROFILES: &str = "/gnsi.certz.v1.Certz/GetProfileList";
    pub const CERTZ_ADD_PROFILE: &str = "/gnsi.certz.v1.Certz/AddProfile";
    pub const CERTZ_DELETE_PROFILE: &str = "/gnsi.certz.v1.Certz/DeleteProfile";
    pub const CERTZ_ROTATE: &str = "/gnsi.certz.v1.Certz/Rotate";
    pub const CREDENTIALZ_PUBLIC_KEYS: &str = "/gnsi.credentialz.v1.Credentialz/GetPublicKeys";
    pub const CREDENTIALZ_ROTATE_ACCOUNT: &str =
        "/gnsi.credentialz.v1.Credentialz/RotateAccountCredentials";
    pub const ACCTZ_STREAM: &str = "/gnsi.acctz.v1.AcctzStream/RecordSubscribe";
    pub const ACCTZ_BIDI: &str = "/gnsi.acctz.v1.Acctz/RecordSubscribe";

    /// Every method above, for "is this one Muxus knows" checks.
    pub const ALL: &[&str] = &[
        PING,
        TRACEROUTE,
        TIME,
        REBOOT,
        REBOOT_STATUS,
        CANCEL_REBOOT,
        KILL_PROCESS,
        FILE_STAT,
        FILE_REMOVE,
        HEALTHZ_GET,
        HEALTHZ_LIST,
        HEALTHZ_ACKNOWLEDGE,
        HEALTHZ_CHECK,
        BGP_CLEAR,
        OS_VERIFY,
        AUTHZ_GET,
        AUTHZ_PROBE,
        AUTHZ_ROTATE,
        PATHZ_GET,
        PATHZ_PROBE,
        PATHZ_ROTATE,
        CERTZ_PROFILES,
        CERTZ_ADD_PROFILE,
        CERTZ_DELETE_PROFILE,
        CERTZ_ROTATE,
        CREDENTIALZ_PUBLIC_KEYS,
        CREDENTIALZ_ROTATE_ACCOUNT,
        ACCTZ_STREAM,
        ACCTZ_BIDI,
    ];
}

/// A gRPC status, or a NETCONF transport problem, as the UI shows it.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagementError {
    pub message: String,
    /// gRPC status name (`UNAVAILABLE`, `INVALID_ARGUMENT`, …) for gNMI.
    pub code: Option<String>,
    /// The request was cancelled from the client.
    pub cancelled: Option<bool>,
}

/// Messages the backend sends on a management session besides the shared
/// login round-trips.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ManagementStreamMessage {
    #[serde(rename_all = "camelCase")]
    Result {
        id: String,
        result: ManagementResult,
        duration_ms: u64,
        /// Encoded size of the response, as received from the device.
        bytes: u64,
    },
    #[serde(rename_all = "camelCase")]
    Error {
        id: String,
        error: ManagementError,
        duration_ms: u64,
    },
    /// Subscription updates, batched by the backend.
    GnmiNotifications {
        id: String,
        notifications: Vec<GnmiNotification>,
        bytes: u64,
    },
    /// The device finished sending the initial state of a subscription.
    GnmiSync { id: String },
    /// A subscription ended: ONCE completed, the device closed it, or it was cancelled.
    StreamEnd {
        id: String,
        error: Option<ManagementError>,
    },
    /// A NETCONF `<notification>`, after `<create-subscription>`.
    #[serde(rename_all = "camelCase")]
    NetconfNotification {
        xml: String,
        event_time: Option<String>,
        received_at: String,
    },
    /// Messages of a streaming gNOI/gNSI call, batched by the backend.
    GrpcMessages { id: String, messages: Vec<GrpcJson> },
    /// A chunk of a file download, base64.
    GnoiFileData { id: String, data: String },
    /// An upload chunk reached the device; send the next one.
    GnoiFileAck { id: String, bytes: u64 },
}

impl ManagementStreamMessage {
    /// The `op` tags this enum covers.
    pub const OPS: &'static [&'static str] = &[
        "result",
        "error",
        "gnmi-notifications",
        "gnmi-sync",
        "stream-end",
        "netconf-notification",
        "grpc-messages",
        "gnoi-file-data",
        "gnoi-file-ack",
    ];
}

/// A request kept in a host's library. The request itself is the UI's editor
/// state (operation, paths, options, body), stored as given.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedManagementRequest {
    pub id: String,
    pub protocol: ManagementProtocol,
    /// The saved host it belongs to; absent offers it for every host of the protocol.
    pub profile_id: Option<String>,
    pub name: String,
    pub request: serde_json::Map<String, serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedManagementRequestInput {
    pub id: Option<String>,
    pub protocol: ManagementProtocol,
    pub profile_id: Option<String>,
    pub name: String,
    pub request: serde_json::Map<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_info_is_tagged_by_protocol() {
        let json = r#"{"protocol":"netconf","address":"admin@r1:830","connectedAt":"2026-01-01T00:00:00.000Z","sessionId":"7","base":"1.1","capabilities":["urn:ietf:params:netconf:base:1.1"]}"#;
        let info: ManagementSessionInfo = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&info).unwrap(), json);
    }

    #[test]
    fn stream_messages_use_kebab_ops() {
        let message = ManagementStreamMessage::GnoiFileAck {
            id: "r1".into(),
            bytes: 10,
        };
        assert_eq!(
            serde_json::to_string(&message).unwrap(),
            r#"{"op":"gnoi-file-ack","id":"r1","bytes":10}"#
        );
        let result: ManagementStreamMessage = serde_json::from_str(
            r#"{"op":"result","id":"r","result":{"op":"netconf-rpc","xml":"<ok/>","messageId":"1"},"durationMs":3,"bytes":5}"#,
        )
        .unwrap();
        assert!(matches!(result, ManagementStreamMessage::Result { .. }));
    }
}
