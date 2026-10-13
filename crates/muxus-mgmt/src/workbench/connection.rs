//! The UI end of a management session (port of
//! `client/src/management/session-client.ts`): login round-trips become
//! [`ConnectionEvent`]s, requests resolve to their result, subscriptions
//! and gNOI/gNSI streams deliver their updates on channels.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use muxus_core::management::GnmiSubscriptionListMode;
use muxus_core::management::{
    GnmiDataType, GnmiEncoding, GnmiNotification, GrpcJson, ManagementError, ManagementResult,
    ManagementSessionInfo, ManagementStreamMessage,
};
use muxus_core::protocol::{
    AuthPromptInfo, DesktopCertificateChallenge, ExitReason, GnmiCommit, GnmiSetValue,
    GnmiSubscription, HostKeyChallenge, ManagementClientMessage, ManagementProfile,
    ManagementServerMessage,
};
use tokio::sync::{mpsc, oneshot};

use super::requests::now_base36;

/// A request as the workbench sends it; the connection adds the id.
#[derive(Debug, Clone, PartialEq)]
pub enum RequestMessage {
    GnmiCapabilities,
    GnmiGet {
        prefix: Option<String>,
        paths: Vec<String>,
        data_type: GnmiDataType,
        encoding: Option<GnmiEncoding>,
        depth: Option<u32>,
        max_bytes: Option<u64>,
    },
    GnmiSet {
        prefix: Option<String>,
        updates: Vec<GnmiSetValue>,
        replaces: Vec<GnmiSetValue>,
        deletes: Vec<String>,
        commit: Option<GnmiCommit>,
    },
    NetconfRpc {
        xml: String,
        timeout_ms: Option<u64>,
    },
    GrpcCall {
        method: String,
        request: Option<GrpcJson>,
    },
    GnoiFileGet {
        remote_file: String,
    },
    TlsProbe,
}

impl RequestMessage {
    pub fn into_client_message(self, id: String) -> ManagementClientMessage {
        match self {
            RequestMessage::GnmiCapabilities => ManagementClientMessage::GnmiCapabilities { id },
            RequestMessage::GnmiGet {
                prefix,
                paths,
                data_type,
                encoding,
                depth,
                max_bytes,
            } => ManagementClientMessage::GnmiGet {
                id,
                prefix,
                paths,
                data_type,
                encoding,
                depth,
                max_bytes,
            },
            RequestMessage::GnmiSet {
                prefix,
                updates,
                replaces,
                deletes,
                commit,
            } => ManagementClientMessage::GnmiSet {
                id,
                prefix,
                updates,
                replaces,
                deletes,
                commit,
            },
            RequestMessage::NetconfRpc { xml, timeout_ms } => ManagementClientMessage::NetconfRpc {
                id,
                xml,
                timeout_ms,
            },
            RequestMessage::GrpcCall { method, request } => ManagementClientMessage::GrpcCall {
                id,
                method,
                request,
            },
            RequestMessage::GnoiFileGet { remote_file } => {
                ManagementClientMessage::GnoiFileGet { id, remote_file }
            }
            RequestMessage::TlsProbe => ManagementClientMessage::TlsProbe { id },
        }
    }
}

/// A subscription as the workbench asks for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeMessage {
    pub prefix: Option<String>,
    pub mode: GnmiSubscriptionListMode,
    pub encoding: Option<GnmiEncoding>,
    pub updates_only: Option<bool>,
    pub subscriptions: Vec<GnmiSubscription>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RequestOutcome {
    pub result: ManagementResult,
    pub duration_ms: u64,
    pub bytes: u64,
}

/// A request that failed, as the backend described it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", error.message)]
pub struct ManagementRequestError {
    pub error: ManagementError,
    pub duration_ms: u64,
}

impl ManagementRequestError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            error: ManagementError {
                message: message.into(),
                code: None,
                cancelled: None,
            },
            duration_ms: 0,
        }
    }
}

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Something to call later: cancel a request, poll a subscription.
pub type Canceller = Arc<dyn Fn() + Send + Sync>;

/// A request on its way: await `outcome`, or cancel it.
pub struct PendingRequest {
    pub id: String,
    pub outcome: BoxFuture<Result<RequestOutcome, ManagementRequestError>>,
    pub cancel: Canceller,
}

/// What a subscription delivers.
#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionUpdate {
    Notifications(Vec<GnmiNotification>, u64),
    Sync,
    End(Option<ManagementError>),
}

/// A live subscription: its updates, and how to poll or stop it.
pub struct LiveSubscription {
    pub id: String,
    pub updates: mpsc::UnboundedReceiver<SubscriptionUpdate>,
    pub poll: Canceller,
    pub cancel: Canceller,
}

/// What the workbench needs from a session; the real one is
/// [`ManagementConnection`], tests use fake devices.
pub trait WorkbenchTransport: Send + Sync + 'static {
    fn is_closed(&self) -> bool;
    fn request(&self, message: RequestMessage) -> PendingRequest;
    fn subscribe(&self, message: SubscribeMessage) -> LiveSubscription;
}

/// Session events that are not answers to a request. They are rare, so the
/// larger ones are not boxed.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionEvent {
    Status {
        message: String,
        transient: bool,
    },
    AuthPrompt(AuthPromptInfo),
    HostKey(HostKeyChallenge),
    Certificate(DesktopCertificateChallenge),
    Ready {
        info: ManagementSessionInfo,
        profile: ManagementProfile,
    },
    NetconfNotification {
        xml: String,
        event_time: Option<String>,
        received_at: String,
    },
    /// The session ended; `exit` is what the backend said, if anything.
    Closed {
        exit: Option<(Option<String>, ExitReason)>,
    },
}

/// Messages of a streaming gNOI/gNSI call.
#[derive(Debug, Clone, PartialEq)]
pub enum GrpcStreamUpdate {
    Messages(Vec<GrpcJson>),
    End(Option<ManagementError>),
}

/// An open gNOI/gNSI stream: more messages can follow on bidirectional calls.
pub struct GrpcStream {
    pub id: String,
    pub updates: mpsc::UnboundedReceiver<GrpcStreamUpdate>,
    connection: ManagementConnection,
}

impl GrpcStream {
    pub fn send(&self, message: GrpcJson) {
        self.connection.send(ManagementClientMessage::GrpcSend {
            id: self.id.clone(),
            message,
        });
    }

    pub fn close_send(&self) {
        self.connection
            .send(ManagementClientMessage::GrpcCloseSend {
                id: self.id.clone(),
            });
    }

    pub fn cancel(&self) {
        self.connection.send(ManagementClientMessage::Cancel {
            id: self.id.clone(),
        });
    }
}

/// A finished download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedFile {
    pub data: Vec<u8>,
    pub size: u64,
    pub verified: bool,
    pub hash_method: Option<String>,
}

/// Upload chunk size; each one waits for the device before the next.
const UPLOAD_CHUNK_BYTES: usize = 1024 * 1024;

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_request_id() -> String {
    let counter = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("r{}{}", now_base36(), radix36(counter))
}

fn radix36(mut value: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).expect("ASCII digits")
}

type Responder = oneshot::Sender<Result<RequestOutcome, ManagementRequestError>>;

#[derive(Default)]
struct Routes {
    requests: HashMap<String, Responder>,
    subscriptions: HashMap<String, mpsc::UnboundedSender<SubscriptionUpdate>>,
    streams: HashMap<String, mpsc::UnboundedSender<GrpcStreamUpdate>>,
    downloads: HashMap<String, mpsc::UnboundedSender<Vec<u8>>>,
    uploads: HashMap<String, mpsc::UnboundedSender<Result<u64, ManagementRequestError>>>,
}

struct Inner {
    sender: async_channel::Sender<ManagementClientMessage>,
    routes: Mutex<Routes>,
    closed: AtomicBool,
    exit: Mutex<Option<(Option<String>, ExitReason)>>,
}

/// The UI end of one management session.
#[derive(Clone)]
pub struct ManagementConnection {
    inner: Arc<Inner>,
}

impl ManagementConnection {
    /// Wrap a session's input; sends `connect` with `profile` right away.
    pub fn new(
        sender: async_channel::Sender<ManagementClientMessage>,
        profile: ManagementProfile,
    ) -> Self {
        let connection = Self {
            inner: Arc::new(Inner {
                sender,
                routes: Mutex::default(),
                closed: AtomicBool::new(false),
                exit: Mutex::new(None),
            }),
        };
        connection.send(ManagementClientMessage::Connect { profile });
        connection
    }

    /// Route every message from the session until it ends, forwarding the
    /// events that are not answers.
    pub async fn dispatch(
        self,
        receiver: async_channel::Receiver<ManagementServerMessage>,
        events: async_channel::Sender<ConnectionEvent>,
    ) {
        while let Ok(message) = receiver.recv().await {
            if let Some(event) = self.receive(message) {
                let _ = events.send(event).await;
            }
        }
        let _ = events.send(self.finish()).await;
    }

    fn routes(&self) -> std::sync::MutexGuard<'_, Routes> {
        self.inner.routes.lock().expect("routes lock")
    }

    /// Handle one message from the session; returns the event it is, if it
    /// answers no request.
    pub fn receive(&self, message: ManagementServerMessage) -> Option<ConnectionEvent> {
        match message {
            ManagementServerMessage::Status { message, transient } => {
                Some(ConnectionEvent::Status {
                    message,
                    transient: transient == Some(true),
                })
            }
            ManagementServerMessage::AuthPrompt(info) => Some(ConnectionEvent::AuthPrompt(info)),
            ManagementServerMessage::HostKey(challenge) => {
                Some(ConnectionEvent::HostKey(challenge))
            }
            ManagementServerMessage::Certificate(challenge) => {
                Some(ConnectionEvent::Certificate(challenge))
            }
            ManagementServerMessage::Ready { info, profile } => {
                Some(ConnectionEvent::Ready { info, profile })
            }
            ManagementServerMessage::Exit { message, reason } => {
                *self.inner.exit.lock().expect("exit lock") = Some((message, reason));
                None
            }
            ManagementServerMessage::Stream(message) => self.receive_stream(message),
        }
    }

    fn receive_stream(&self, message: ManagementStreamMessage) -> Option<ConnectionEvent> {
        let mut routes = self.routes();
        match message {
            ManagementStreamMessage::Result {
                id,
                result,
                duration_ms,
                bytes,
            } => {
                if let Some(pending) = routes.requests.remove(&id) {
                    let _ = pending.send(Ok(RequestOutcome {
                        result,
                        duration_ms,
                        bytes,
                    }));
                }
            }
            ManagementStreamMessage::Error {
                id,
                error,
                duration_ms,
            } => {
                let failure = ManagementRequestError {
                    error: error.clone(),
                    duration_ms,
                };
                if let Some(pending) = routes.requests.remove(&id) {
                    let _ = pending.send(Err(failure));
                    return None;
                }
                // A subscription or stream that failed before it started reports as an error.
                if let Some(subscription) = routes.subscriptions.remove(&id) {
                    let _ = subscription.send(SubscriptionUpdate::End(Some(error.clone())));
                }
                if let Some(stream) = routes.streams.remove(&id) {
                    let _ = stream.send(GrpcStreamUpdate::End(Some(error)));
                }
                if let Some(upload) = routes.uploads.remove(&id) {
                    let _ = upload.send(Err(failure));
                }
            }
            ManagementStreamMessage::GnmiNotifications {
                id,
                notifications,
                bytes,
            } => {
                if let Some(subscription) = routes.subscriptions.get(&id) {
                    let _ =
                        subscription.send(SubscriptionUpdate::Notifications(notifications, bytes));
                }
            }
            ManagementStreamMessage::GrpcMessages { id, messages } => {
                if let Some(stream) = routes.streams.get(&id) {
                    let _ = stream.send(GrpcStreamUpdate::Messages(messages));
                }
            }
            ManagementStreamMessage::GnoiFileData { id, data } => {
                if let (Some(download), Ok(bytes)) =
                    (routes.downloads.get(&id), BASE64.decode(data))
                {
                    let _ = download.send(bytes);
                }
            }
            ManagementStreamMessage::GnoiFileAck { id, bytes } => {
                if let Some(upload) = routes.uploads.get(&id) {
                    let _ = upload.send(Ok(bytes));
                }
            }
            ManagementStreamMessage::GnmiSync { id } => {
                if let Some(subscription) = routes.subscriptions.get(&id) {
                    let _ = subscription.send(SubscriptionUpdate::Sync);
                }
            }
            ManagementStreamMessage::StreamEnd { id, error } => {
                if let Some(subscription) = routes.subscriptions.remove(&id) {
                    let _ = subscription.send(SubscriptionUpdate::End(error.clone()));
                }
                if let Some(stream) = routes.streams.remove(&id) {
                    let _ = stream.send(GrpcStreamUpdate::End(error));
                }
            }
            ManagementStreamMessage::NetconfNotification {
                xml,
                event_time,
                received_at,
            } => {
                return Some(ConnectionEvent::NetconfNotification {
                    xml,
                    event_time,
                    received_at,
                });
            }
        }
        None
    }

    /// The session ended: everything still waiting fails with what the backend said.
    pub fn finish(&self) -> ConnectionEvent {
        self.inner.closed.store(true, Ordering::Release);
        let exit = self.inner.exit.lock().expect("exit lock").clone();
        let message = exit
            .as_ref()
            .and_then(|(message, _)| message.clone())
            .unwrap_or_else(|| "The session closed.".into());
        let mut routes = self.routes();
        for (_, pending) in routes.requests.drain() {
            let _ = pending.send(Err(ManagementRequestError::new(message.clone())));
        }
        for (_, subscription) in routes.subscriptions.drain() {
            let _ = subscription.send(SubscriptionUpdate::End(Some(ManagementError {
                message: message.clone(),
                code: None,
                cancelled: None,
            })));
        }
        for (_, stream) in routes.streams.drain() {
            let _ = stream.send(GrpcStreamUpdate::End(Some(ManagementError {
                message: message.clone(),
                code: None,
                cancelled: None,
            })));
        }
        for (_, upload) in routes.uploads.drain() {
            let _ = upload.send(Err(ManagementRequestError::new(message.clone())));
        }
        routes.downloads.clear();
        ConnectionEvent::Closed { exit }
    }

    pub fn closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    pub fn send(&self, message: ManagementClientMessage) {
        if !self.closed() {
            let _ = self.inner.sender.try_send(message);
        }
    }

    /// Stop the session from this end.
    pub fn close(&self) {
        self.inner.sender.close();
    }

    fn cancel_fn(&self, id: &str) -> Canceller {
        let connection = self.clone();
        let id = id.to_owned();
        Arc::new(move || connection.send(ManagementClientMessage::Cancel { id: id.clone() }))
    }

    /// A unary gNOI/gNSI call; resolves to the response JSON.
    pub async fn grpc_call(
        &self,
        method: &str,
        request: GrpcJson,
    ) -> Result<GrpcJson, ManagementRequestError> {
        let outcome = self
            .request(RequestMessage::GrpcCall {
                method: method.to_owned(),
                request: Some(request),
            })
            .outcome
            .await?;
        Ok(match outcome.result {
            ManagementResult::GrpcCall { response, .. } => response,
            _ => GrpcJson::new(),
        })
    }

    /// A streaming gNOI/gNSI call.
    pub fn grpc_stream(&self, method: &str, request: Option<GrpcJson>) -> GrpcStream {
        let id = next_request_id();
        let (sender, updates) = mpsc::unbounded_channel();
        if self.closed() {
            let _ = sender.send(GrpcStreamUpdate::End(Some(ManagementError {
                message: "The session is closed.".into(),
                code: None,
                cancelled: None,
            })));
        } else {
            self.routes().streams.insert(id.clone(), sender);
            self.send(ManagementClientMessage::GrpcStream {
                id: id.clone(),
                method: method.to_owned(),
                request,
            });
        }
        GrpcStream {
            id,
            updates,
            connection: self.clone(),
        }
    }

    /// Download a file from the device over gNOI, its hash checked by the backend.
    pub fn download_file(
        &self,
        remote_file: &str,
        on_progress: impl Fn(u64) + Send + 'static,
    ) -> (
        BoxFuture<Result<DownloadedFile, ManagementRequestError>>,
        Canceller,
    ) {
        let (chunks_tx, mut chunks) = mpsc::unbounded_channel();
        let pending = self.request(RequestMessage::GnoiFileGet {
            remote_file: remote_file.to_owned(),
        });
        self.routes()
            .downloads
            .insert(pending.id.clone(), chunks_tx);
        let connection = self.clone();
        let id = pending.id.clone();
        let mut outcome = pending.outcome;
        let future = async move {
            let mut data = Vec::new();
            let result = loop {
                tokio::select! {
                    chunk = chunks.recv() => match chunk {
                        Some(chunk) => {
                            data.extend_from_slice(&chunk);
                            on_progress(data.len() as u64);
                        }
                        None => break (&mut outcome).await,
                    },
                    result = &mut outcome => {
                        // Chunks sent before the result are already queued.
                        while let Ok(chunk) = chunks.try_recv() {
                            data.extend_from_slice(&chunk);
                        }
                        break result;
                    }
                }
            };
            connection.routes().downloads.remove(&id);
            let outcome = result?;
            Ok(match outcome.result {
                ManagementResult::GnoiFileGet {
                    size,
                    verified,
                    hash_method,
                } => DownloadedFile {
                    data,
                    size,
                    verified,
                    hash_method,
                },
                _ => DownloadedFile {
                    size: data.len() as u64,
                    data,
                    verified: false,
                    hash_method: None,
                },
            })
        };
        (Box::pin(future), pending.cancel)
    }

    /// Upload a file to the device over gNOI, a chunk at a time as the device takes them.
    pub fn upload_file(
        &self,
        remote_file: &str,
        file: Vec<u8>,
        permissions: Option<u32>,
        on_progress: impl Fn(u64) + Send + 'static,
    ) -> (BoxFuture<Result<u64, ManagementRequestError>>, Canceller) {
        let id = next_request_id();
        let cancel = self.cancel_fn(&id);
        if self.closed() {
            return (
                Box::pin(async { Err(ManagementRequestError::new("The session is closed.")) }),
                cancel,
            );
        }
        let (result_tx, result_rx) = oneshot::channel();
        let (acks_tx, mut acks) = mpsc::unbounded_channel();
        {
            let mut routes = self.routes();
            routes.requests.insert(id.clone(), result_tx);
            routes.uploads.insert(id.clone(), acks_tx);
        }
        self.send(ManagementClientMessage::GnoiFilePut {
            id: id.clone(),
            remote_file: remote_file.to_owned(),
            permissions,
            size: file.len() as u64,
        });
        let connection = self.clone();
        let future = async move {
            let mut offset = 0;
            let mut result = Box::pin(result_rx);
            let outcome = loop {
                tokio::select! {
                    ack = acks.recv() => match ack {
                        Some(Ok(bytes)) => {
                            on_progress(bytes);
                            let end = (offset + UPLOAD_CHUNK_BYTES).min(file.len());
                            let data = BASE64.encode(&file[offset..end]);
                            offset = end;
                            connection.send(ManagementClientMessage::GnoiFileChunk {
                                id: id.clone(),
                                data,
                                last: Some(end >= file.len()),
                            });
                        }
                        Some(Err(err)) => break Err(err),
                        None => break (&mut result).await.unwrap_or_else(|_| Err(ManagementRequestError::new("The session closed."))),
                    },
                    outcome = &mut result => {
                        break outcome.unwrap_or_else(|_| Err(ManagementRequestError::new("The session closed.")));
                    }
                }
            };
            connection.routes().uploads.remove(&id);
            let outcome = outcome?;
            Ok(match outcome.result {
                ManagementResult::GnoiFilePut { size } => size,
                _ => file.len() as u64,
            })
        };
        (Box::pin(future), cancel)
    }
}

impl WorkbenchTransport for ManagementConnection {
    fn is_closed(&self) -> bool {
        self.closed()
    }

    fn request(&self, message: RequestMessage) -> PendingRequest {
        let id = next_request_id();
        let cancel = self.cancel_fn(&id);
        if self.closed() {
            return PendingRequest {
                id,
                outcome: Box::pin(async {
                    Err(ManagementRequestError::new("The session is closed."))
                }),
                cancel,
            };
        }
        let (sender, receiver) = oneshot::channel();
        self.routes().requests.insert(id.clone(), sender);
        self.send(message.into_client_message(id.clone()));
        PendingRequest {
            id,
            outcome: Box::pin(async move {
                receiver
                    .await
                    .unwrap_or_else(|_| Err(ManagementRequestError::new("The session closed.")))
            }),
            cancel,
        }
    }

    fn subscribe(&self, message: SubscribeMessage) -> LiveSubscription {
        let id = next_request_id();
        let (sender, updates) = mpsc::unbounded_channel();
        if self.closed() {
            let _ = sender.send(SubscriptionUpdate::End(Some(ManagementError {
                message: "The session is closed.".into(),
                code: None,
                cancelled: None,
            })));
        } else {
            self.routes().subscriptions.insert(id.clone(), sender);
            self.send(ManagementClientMessage::GnmiSubscribe {
                id: id.clone(),
                prefix: message.prefix,
                mode: message.mode,
                encoding: message.encoding,
                updates_only: message.updates_only,
                subscriptions: message.subscriptions,
            });
        }
        let connection = self.clone();
        let poll_id = id.clone();
        LiveSubscription {
            poll: Arc::new(move || {
                connection.send(ManagementClientMessage::GnmiPoll {
                    id: poll_id.clone(),
                })
            }),
            cancel: self.cancel_fn(&id),
            id,
            updates,
        }
    }
}

#[cfg(test)]
mod tests {
    use muxus_core::management::ManagementResult;

    use super::*;

    #[tokio::test]
    async fn routes_results_streams_and_the_end_of_the_session() {
        let (sender, outgoing) = async_channel::unbounded();
        let profile: ManagementProfile = serde_json::from_value(serde_json::json!({
            "kind": "netconf", "host": "r1"
        }))
        .unwrap();
        let connection = ManagementConnection::new(sender, profile);
        assert!(matches!(
            outgoing.recv().await.unwrap(),
            ManagementClientMessage::Connect { .. }
        ));
        let pending = connection.request(RequestMessage::NetconfRpc {
            xml: "<get/>".into(),
            timeout_ms: None,
        });
        let ManagementClientMessage::NetconfRpc { id, .. } = outgoing.recv().await.unwrap() else {
            panic!("not an RPC");
        };
        assert_eq!(id, pending.id);
        assert!(
            connection
                .receive(ManagementServerMessage::Stream(
                    ManagementStreamMessage::Result {
                        id: id.clone(),
                        result: ManagementResult::NetconfRpc {
                            xml: "<ok/>".into(),
                            message_id: "1".into()
                        },
                        duration_ms: 3,
                        bytes: 5,
                    }
                ))
                .is_none()
        );
        let outcome = pending.outcome.await.unwrap();
        assert_eq!(outcome.bytes, 5);

        let mut subscription = connection.subscribe(SubscribeMessage {
            prefix: None,
            mode: GnmiSubscriptionListMode::Once,
            encoding: None,
            updates_only: None,
            subscriptions: vec![],
        });
        let _ = outgoing.recv().await.unwrap();
        connection.receive(ManagementServerMessage::Stream(
            ManagementStreamMessage::GnmiSync {
                id: subscription.id.clone(),
            },
        ));
        assert_eq!(
            subscription.updates.recv().await,
            Some(SubscriptionUpdate::Sync)
        );

        let waiting = connection.request(RequestMessage::GnmiCapabilities);
        connection.receive(ManagementServerMessage::Exit {
            message: Some("The device went away.".into()),
            reason: ExitReason::Disconnected,
        });
        let ConnectionEvent::Closed { exit } = connection.finish() else {
            panic!("not closed");
        };
        assert_eq!(exit.unwrap().1, ExitReason::Disconnected);
        assert_eq!(
            waiting.outcome.await.unwrap_err().error.message,
            "The device went away."
        );
        assert!(matches!(
            subscription.updates.recv().await,
            Some(SubscriptionUpdate::End(Some(_)))
        ));
        assert!(
            connection
                .request(RequestMessage::TlsProbe)
                .outcome
                .await
                .is_err()
        );
    }
}
