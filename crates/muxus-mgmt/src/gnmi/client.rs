//! A gNMI client: Capabilities, Get, Set and Subscribe on one HTTP/2
//! connection, plus raw calls for the gNOI and gNSI services that share the
//! gNMI server.

use std::time::Duration;

use bytes::Bytes;
use muxus_core::gnmi_path::{GnmiPathError, format_gnmi_path, join_gnmi_paths, parse_gnmi_path};
use muxus_core::js::js_trim;
use muxus_core::management::{
    GnmiDataType, GnmiEncoding, GnmiModel, GnmiNotification, GnmiSetOp, GnmiSetResult,
    GnmiSubscriptionListMode, GnmiSubscriptionMode,
};
use muxus_core::protocol::{GnmiCommit, GnmiSetValue, GnmiSubscription};
use prost::Message;
use tokio_util::sync::CancellationToken;

use super::grpc::{CallEvent, CallHandle, CallOptions, GrpcCall, GrpcConnection, GrpcIo};
use super::values::{
    encoding_number, encodings_from_proto, models_from_proto, notification_to_json,
    path_from_proto, path_text_to_proto, set_value_to_proto,
};
use crate::error::{Error, GrpcError, code};
use crate::proto::{gnmi, gnmi_ext};

const SERVICE: &str = "/gnmi.gNMI";
pub(crate) const UNARY_TIMEOUT: Duration = Duration::from_secs(120);

/// What the device says it supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GnmiCapabilities {
    pub version: String,
    pub encodings: Vec<GnmiEncoding>,
    pub models: Vec<GnmiModel>,
    /// Encoded size of the response.
    pub bytes: u64,
}

/// A Get, as the UI asks for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GnmiGetRequest {
    pub prefix: Option<String>,
    pub paths: Vec<String>,
    pub data_type: GnmiDataType,
    /// gNMI depth extension: levels below each path to return.
    pub depth: Option<u32>,
    /// Abandon the request once the response grows past this many bytes.
    pub max_bytes: Option<u64>,
}

/// A Set, as the UI asks for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GnmiSetRequest {
    pub prefix: Option<String>,
    pub updates: Vec<GnmiSetValue>,
    pub replaces: Vec<GnmiSetValue>,
    pub deletes: Vec<String>,
    pub commit: Option<GnmiCommit>,
}

/// A Subscribe, as the UI asks for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GnmiSubscribeRequest {
    pub prefix: Option<String>,
    pub mode: GnmiSubscriptionListMode,
    pub updates_only: bool,
    pub subscriptions: Vec<GnmiSubscription>,
}

/// A finished Get.
#[derive(Debug, Clone, PartialEq)]
pub struct GetOutcome {
    pub notifications: Vec<GnmiNotification>,
    pub bytes: u64,
}

/// A finished Set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetOutcome {
    pub timestamp: String,
    pub results: Vec<GnmiSetResult>,
    pub bytes: u64,
}

/// What a subscription delivers.
#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionEvent {
    /// An update, with the encoded bytes it took.
    Notification(GnmiNotification, u64),
    /// The device finished sending the initial state.
    Sync,
    /// The subscription ended; `None` is a clean end.
    End(Option<GrpcError>),
}

fn has_text(text: &Option<String>) -> bool {
    text.as_deref()
        .is_some_and(|text| !js_trim(text).is_empty())
}

fn optional_prefix(prefix: &Option<String>) -> Result<Option<gnmi::Path>, GnmiPathError> {
    match prefix {
        Some(text) if has_text(prefix) => path_text_to_proto(text).map(Some),
        _ => Ok(None),
    }
}

fn data_type_number(data_type: GnmiDataType) -> i32 {
    match data_type {
        GnmiDataType::All => 0,
        GnmiDataType::Config => 1,
        GnmiDataType::State => 2,
        GnmiDataType::Operational => 3,
    }
}

fn list_mode_number(mode: GnmiSubscriptionListMode) -> i32 {
    match mode {
        GnmiSubscriptionListMode::Stream => 0,
        GnmiSubscriptionListMode::Once => 1,
        GnmiSubscriptionListMode::Poll => 2,
    }
}

fn subscription_mode_number(mode: GnmiSubscriptionMode) -> i32 {
    match mode {
        GnmiSubscriptionMode::TargetDefined => 0,
        GnmiSubscriptionMode::OnChange => 1,
        GnmiSubscriptionMode::Sample => 2,
    }
}

fn update_result_op(op: i32) -> GnmiSetOp {
    match op {
        1 => GnmiSetOp::Delete,
        2 => GnmiSetOp::Replace,
        3 => GnmiSetOp::Update,
        4 => GnmiSetOp::UnionReplace,
        _ => GnmiSetOp::Invalid,
    }
}

/// Pre-0.7 targets put failures in the response body instead of the status.
fn reject_embedded_error(error: Option<&gnmi::Error>) -> Result<(), GrpcError> {
    match error {
        Some(error) if error.code != 0 || !error.message.is_empty() => Err(GrpcError::new(
            if error.code == 0 {
                code::UNKNOWN
            } else {
                error.code as i32
            },
            error.message.clone(),
        )),
        _ => Ok(()),
    }
}

fn decode<M: Message + Default>(bytes: &[u8]) -> Result<M, GrpcError> {
    M::decode(bytes).map_err(|err| {
        GrpcError::new(
            code::INTERNAL,
            format!("Could not decode the response: {err}"),
        )
    })
}

/// Reject malformed path text up front with a readable message.
pub fn validate_gnmi_paths<'a>(
    paths: impl IntoIterator<Item = Option<&'a str>>,
) -> Result<(), GnmiPathError> {
    for path in paths.into_iter().flatten() {
        if !js_trim(path).is_empty() {
            parse_gnmi_path(path)?;
        }
    }
    Ok(())
}

/// One gNMI target, one HTTP/2 connection, any number of calls on it.
#[derive(Clone)]
pub struct GnmiClient {
    connection: GrpcConnection,
}

impl GnmiClient {
    /// Wrap a Muxus-owned stream (TCP, TLS, an SSH channel) in an HTTP/2
    /// session. `authority` is host:port as the device is addressed.
    pub async fn connect(
        io: impl GrpcIo,
        authority: &str,
        secure: bool,
    ) -> Result<Self, GrpcError> {
        Ok(Self {
            connection: GrpcConnection::connect(io, authority, secure).await?,
        })
    }

    /// gNMI has no login step: the credentials ride along as metadata on every call.
    pub fn set_credentials(&self, username: Option<&str>, password: Option<&str>) {
        let mut entries = Vec::new();
        if let Some(username) = username.filter(|name| !name.is_empty()) {
            entries.push(("username", username));
            if let Some(password) = password {
                entries.push(("password", password));
            }
        }
        self.connection.set_metadata(&entries);
    }

    /// Resolves with the reason once the connection ends.
    pub fn closed(&self) -> impl Future<Output = String> + Send + 'static {
        self.connection.closed()
    }

    pub fn is_closed(&self) -> bool {
        self.connection.closed_reason().is_some()
    }

    pub fn close(&self) {
        self.connection.close("The session was closed.");
    }

    pub async fn capabilities(
        &self,
        cancel: Option<&CancellationToken>,
    ) -> Result<GnmiCapabilities, GrpcError> {
        let request = gnmi::CapabilityRequest::default().encode_to_vec();
        let (message, bytes) = self.unary("Capabilities", request, None, cancel).await?;
        let response: gnmi::CapabilityResponse = decode(&message)?;
        Ok(GnmiCapabilities {
            version: response.g_nmi_version,
            encodings: encodings_from_proto(&response.supported_encodings),
            models: models_from_proto(&response.supported_models),
            bytes,
        })
    }

    pub async fn get(
        &self,
        request: &GnmiGetRequest,
        encoding: GnmiEncoding,
        cancel: Option<&CancellationToken>,
    ) -> Result<GetOutcome, Error> {
        let body = gnmi::GetRequest {
            prefix: optional_prefix(&request.prefix)?,
            path: request
                .paths
                .iter()
                .map(|path| path_text_to_proto(path))
                .collect::<Result<_, _>>()?,
            r#type: data_type_number(request.data_type),
            encoding: encoding_number(encoding),
            extension: request
                .depth
                .filter(|depth| *depth > 0)
                .map(|level| {
                    vec![gnmi_ext::Extension {
                        ext: Some(gnmi_ext::extension::Ext::Depth(gnmi_ext::Depth { level })),
                    }]
                })
                .unwrap_or_default(),
            ..Default::default()
        };
        let (message, bytes) = self
            .unary("Get", body.encode_to_vec(), request.max_bytes, cancel)
            .await?;
        let response: gnmi::GetResponse = decode(&message)?;
        #[allow(deprecated)]
        reject_embedded_error(response.error.as_ref())?;
        Ok(GetOutcome {
            notifications: response
                .notification
                .iter()
                .map(notification_to_json)
                .collect(),
            bytes,
        })
    }

    pub async fn set(
        &self,
        request: &GnmiSetRequest,
        cancel: Option<&CancellationToken>,
    ) -> Result<SetOutcome, Error> {
        let update = |item: &GnmiSetValue| -> Result<gnmi::Update, GnmiPathError> {
            Ok(gnmi::Update {
                path: Some(path_text_to_proto(&item.path)?),
                val: Some(set_value_to_proto(&item.value, item.encoding)),
                ..Default::default()
            })
        };
        let extension = request.commit.as_ref().map(|commit| {
            use gnmi_ext::commit::Action;
            let (id, action) = match commit {
                GnmiCommit::Commit {
                    id,
                    rollback_seconds,
                } => (
                    id,
                    Action::Commit(gnmi_ext::CommitRequest {
                        rollback_duration: rollback_seconds.filter(|seconds| *seconds > 0).map(
                            |seconds| prost_types::Duration {
                                seconds: i64::from(seconds),
                                nanos: 0,
                            },
                        ),
                    }),
                ),
                GnmiCommit::Confirm { id } => (id, Action::Confirm(gnmi_ext::CommitConfirm {})),
                GnmiCommit::Cancel { id } => (id, Action::Cancel(gnmi_ext::CommitCancel {})),
            };
            gnmi_ext::Extension {
                ext: Some(gnmi_ext::extension::Ext::Commit(gnmi_ext::Commit {
                    id: id.clone(),
                    action: Some(action),
                })),
            }
        });
        let body = gnmi::SetRequest {
            prefix: optional_prefix(&request.prefix)?,
            delete: request
                .deletes
                .iter()
                .map(|path| path_text_to_proto(path))
                .collect::<Result<_, _>>()?,
            replace: request
                .replaces
                .iter()
                .map(update)
                .collect::<Result<_, _>>()?,
            update: request
                .updates
                .iter()
                .map(update)
                .collect::<Result<_, _>>()?,
            extension: extension.into_iter().collect(),
            ..Default::default()
        };
        let (message, bytes) = self
            .unary("Set", body.encode_to_vec(), None, cancel)
            .await?;
        let response: gnmi::SetResponse = decode(&message)?;
        #[allow(deprecated)]
        reject_embedded_error(response.message.as_ref())?;
        let prefix = response
            .prefix
            .as_ref()
            .map(|prefix| path_from_proto(Some(prefix)));
        Ok(SetOutcome {
            timestamp: response.timestamp.to_string(),
            results: response
                .response
                .iter()
                .map(|result| GnmiSetResult {
                    path: format_gnmi_path(&join_gnmi_paths(
                        prefix.as_ref(),
                        &path_from_proto(result.path.as_ref()),
                    )),
                    op: update_result_op(result.op),
                })
                .collect(),
            bytes,
        })
    }

    async fn unary(
        &self,
        method: &str,
        request: Vec<u8>,
        max_bytes: Option<u64>,
        cancel: Option<&CancellationToken>,
    ) -> Result<(Bytes, u64), GrpcError> {
        self.connection
            .unary(
                &format!("{SERVICE}/{method}"),
                Bytes::from(request),
                CallOptions {
                    timeout: Some(UNARY_TIMEOUT),
                    max_bytes,
                },
                cancel,
            )
            .await
    }

    /// One request, one reply, for a gNOI/gNSI method on this connection.
    pub async fn unary_call(
        &self,
        path: &str,
        request: Bytes,
        cancel: Option<&CancellationToken>,
    ) -> Result<(Bytes, u64), GrpcError> {
        self.connection
            .unary(
                path,
                request,
                CallOptions {
                    timeout: Some(UNARY_TIMEOUT),
                    max_bytes: None,
                },
                cancel,
            )
            .await
    }

    /// A raw call on this connection, for the gNOI and gNSI services that
    /// share the gNMI server. It ends with the session like every other call.
    pub fn open_call(&self, path: &str, options: CallOptions) -> GrpcCall {
        self.connection.open_call(path, options)
    }

    /// Start a subscription. Every path is validated before anything goes on the wire.
    pub fn subscribe(
        &self,
        request: &GnmiSubscribeRequest,
        encoding: GnmiEncoding,
    ) -> Result<GnmiSubscriptionStream, GnmiPathError> {
        let prefix = optional_prefix(&request.prefix)?;
        let subscriptions = request
            .subscriptions
            .iter()
            .map(|subscription| {
                Ok(gnmi::Subscription {
                    path: Some(path_text_to_proto(&subscription.path)?),
                    mode: subscription_mode_number(subscription.mode),
                    sample_interval: subscription
                        .sample_interval_ms
                        .map_or(0, |ms| ms * 1_000_000),
                    suppress_redundant: subscription.suppress_redundant == Some(true),
                    heartbeat_interval: subscription
                        .heartbeat_interval_ms
                        .map_or(0, |ms| ms * 1_000_000),
                })
            })
            .collect::<Result<Vec<_>, GnmiPathError>>()?;
        let call = self
            .connection
            .open_call(&format!("{SERVICE}/Subscribe"), CallOptions::default());
        let message = gnmi::SubscribeRequest {
            request: Some(gnmi::subscribe_request::Request::Subscribe(
                gnmi::SubscriptionList {
                    prefix,
                    subscription: subscriptions,
                    mode: list_mode_number(request.mode),
                    encoding: encoding_number(encoding),
                    updates_only: request.updates_only,
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        call.handle.write(Bytes::from(message.encode_to_vec()));
        Ok(GnmiSubscriptionStream {
            control: SubscriptionControl {
                handle: call.handle.clone(),
                poll_mode: request.mode == GnmiSubscriptionListMode::Poll,
            },
            call,
            previous_bytes: 0,
        })
    }
}

/// Poll or cancel a running subscription; cheap to clone.
#[derive(Clone)]
pub struct SubscriptionControl {
    handle: CallHandle,
    poll_mode: bool,
}

impl SubscriptionControl {
    /// Ask a POLL subscription for a fresh snapshot.
    pub fn poll(&self) {
        if self.poll_mode {
            let poll = gnmi::SubscribeRequest {
                request: Some(gnmi::subscribe_request::Request::Poll(gnmi::Poll {})),
                ..Default::default()
            };
            self.handle.write(Bytes::from(poll.encode_to_vec()));
        }
    }

    pub fn cancel(&self) {
        self.handle.cancel(None);
    }
}

/// A running subscription's events.
pub struct GnmiSubscriptionStream {
    call: GrpcCall,
    control: SubscriptionControl,
    previous_bytes: u64,
}

impl GnmiSubscriptionStream {
    pub fn control(&self) -> SubscriptionControl {
        self.control.clone()
    }

    /// The next event; `None` once the end was delivered.
    pub async fn next(&mut self) -> Option<SubscriptionEvent> {
        loop {
            match self.call.next().await? {
                CallEvent::End(error) => return Some(SubscriptionEvent::End(error)),
                CallEvent::Message(message) => {
                    let total = self.call.handle.bytes();
                    let received = total.saturating_sub(self.previous_bytes);
                    self.previous_bytes = total;
                    let response = match gnmi::SubscribeResponse::decode(message.as_ref()) {
                        Ok(response) => response,
                        Err(err) => {
                            self.call.handle.cancel(Some(GrpcError::new(
                                code::INTERNAL,
                                format!("Could not decode a subscription update: {err}"),
                            )));
                            continue;
                        }
                    };
                    #[allow(deprecated)]
                    match response.response {
                        Some(gnmi::subscribe_response::Response::Error(error)) => {
                            self.call.handle.cancel(Some(GrpcError::new(
                                if error.code == 0 {
                                    code::UNKNOWN
                                } else {
                                    error.code as i32
                                },
                                error.message,
                            )));
                        }
                        Some(gnmi::subscribe_response::Response::SyncResponse(true)) => {
                            return Some(SubscriptionEvent::Sync);
                        }
                        Some(gnmi::subscribe_response::Response::Update(notification)) => {
                            return Some(SubscriptionEvent::Notification(
                                notification_to_json(&notification),
                                received,
                            ));
                        }
                        Some(gnmi::subscribe_response::Response::SyncResponse(false)) | None => {}
                    }
                }
            }
        }
    }
}
