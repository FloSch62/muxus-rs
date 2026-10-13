//! gRPC over a connection Muxus owns.
//!
//! The byte stream underneath (TCP, an SSH channel, Muxus's own TLS with
//! certificate pinning) is opened by Muxus, so the HTTP/2 connection is set
//! up here with hyper rather than through a tonic `Channel`, and tonic only
//! frames the calls. Messages travel as raw bytes: gNMI decodes them with
//! prost, gNOI/gNSI through the descriptor pool.
//!
//! Every call counts the response bytes it received (the size shown with a
//! result) and can stop once a response grows past a limit, like the
//! TypeScript `GrpcCall`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use bytes::{Buf, BufMut, Bytes};
use http::header::{CONTENT_TYPE, USER_AGENT};
use http::uri::PathAndQuery;
use http::{HeaderName, HeaderValue, StatusCode, Uri};
use http_body::{Body as HttpBody, Frame};
use hyper::body::Incoming;
use hyper::client::conn::http2;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;
use tonic::Status;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};

use crate::error::{GrpcError, code};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Big Get responses arrive faster with a generous flow-control window.
pub const WINDOW_BYTES: u32 = 32 * 1024 * 1024;
const PING_INTERVAL: Duration = Duration::from_secs(30);
const PING_TIMEOUT: Duration = Duration::from_secs(20);

/// Something a gRPC connection can run over.
pub trait GrpcIo: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> GrpcIo for T {}

/// Messages as they are on the wire.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawCodec;

impl Codec for RawCodec {
    type Encode = Bytes;
    type Decode = Bytes;
    type Encoder = RawCodec;
    type Decoder = RawCodec;

    fn encoder(&mut self) -> RawCodec {
        RawCodec
    }

    fn decoder(&mut self) -> RawCodec {
        RawCodec
    }
}

impl Encoder for RawCodec {
    type Item = Bytes;
    type Error = Status;

    fn encode(&mut self, item: Bytes, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        dst.put(item);
        Ok(())
    }
}

impl Decoder for RawCodec {
    type Item = Bytes;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}

/// Response bytes received by one call, and where it gives up.
#[derive(Debug, Default)]
pub(crate) struct CallMeter {
    bytes: AtomicU64,
    max_bytes: Option<u64>,
}

impl CallMeter {
    fn new(max_bytes: Option<u64>) -> Self {
        Self {
            bytes: AtomicU64::new(0),
            max_bytes,
        }
    }

    fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
}

pub fn format_bytes(bytes: u64) -> String {
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", value / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", value / 1024.0 / 1024.0)
    } else {
        format!("{:.1} GiB", value / 1024.0 / 1024.0 / 1024.0)
    }
}

/// A response body that counts what arrives and stops past the call's limit.
pub struct ResponseBody {
    inner: Option<Incoming>,
    meter: Option<Arc<CallMeter>>,
}

impl HttpBody for ResponseBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        match ready!(Pin::new(inner).poll_frame(cx)) {
            Some(Ok(frame)) => {
                if let (Some(data), Some(meter)) = (frame.data_ref(), &this.meter) {
                    let total = meter.bytes.fetch_add(data.len() as u64, Ordering::Relaxed)
                        + data.len() as u64;
                    if let Some(max) = meter.max_bytes
                        && total > max
                    {
                        this.inner = None;
                        let status = Status::resource_exhausted(format!(
                            "The response grew past {}; ask for a narrower path.",
                            format_bytes(max)
                        ));
                        return Poll::Ready(Some(Err(Box::new(status))));
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Some(Err(err)) => Poll::Ready(Some(Err(Box::new(err)))),
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.as_ref().is_none_or(HttpBody::is_end_stream)
    }
}

fn http_status_to_grpc(status: StatusCode) -> i32 {
    match status.as_u16() {
        400 => code::INTERNAL,
        401 => code::UNAUTHENTICATED,
        403 => code::PERMISSION_DENIED,
        404 => code::UNIMPLEMENTED,
        429 | 502 | 503 | 504 => code::UNAVAILABLE,
        _ => code::UNKNOWN,
    }
}

/// A non-gRPC HTTP answer, as the trailers-only status tonic understands.
fn http_failure(status: StatusCode) -> http::Response<ResponseBody> {
    let message = format!("The device answered HTTP {}.", status.as_u16());
    let mut response = http::Response::new(ResponseBody {
        inner: None,
        meter: None,
    });
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc"));
    headers.insert(
        "grpc-status",
        HeaderValue::from_str(&http_status_to_grpc(status).to_string()).expect("digits"),
    );
    headers.insert(
        "grpc-message",
        HeaderValue::from_str(&message).expect("ASCII message"),
    );
    response
}

/// The HTTP/2 connection as a tower service for tonic.
#[derive(Clone)]
pub(crate) struct Channel {
    send: http2::SendRequest<tonic::body::Body>,
    metadata: Arc<Mutex<Vec<(HeaderName, HeaderValue)>>>,
}

impl tower_service::Service<http::Request<tonic::body::Body>> for Channel {
    type Response = http::Response<ResponseBody>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, hyper::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), hyper::Error>> {
        self.send.poll_ready(cx)
    }

    fn call(&mut self, mut request: http::Request<tonic::body::Body>) -> Self::Future {
        let meter = request.extensions_mut().remove::<Arc<CallMeter>>();
        let headers = request.headers_mut();
        headers.insert(USER_AGENT, HeaderValue::from_static("muxus-gnmi"));
        headers.insert("grpc-accept-encoding", HeaderValue::from_static("identity"));
        for (name, value) in self.metadata.lock().expect("metadata lock").iter() {
            headers.insert(name.clone(), value.clone());
        }
        let response = self.send.send_request(request);
        Box::pin(async move {
            let response = response.await?;
            if response.status() != StatusCode::OK {
                return Ok(http_failure(response.status()));
            }
            Ok(response.map(|body| ResponseBody {
                inner: Some(body),
                meter,
            }))
        })
    }
}

struct ConnectionState {
    closed: watch::Sender<Option<String>>,
    task: Mutex<Option<AbortHandle>>,
}

impl ConnectionState {
    fn shutdown(&self, reason: String) {
        self.closed.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(reason);
            true
        });
        if let Some(task) = self.task.lock().expect("task lock").take() {
            task.abort();
        }
    }
}

impl Drop for ConnectionState {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().expect("task lock").take() {
            task.abort();
        }
    }
}

fn connection_failure(err: &hyper::Error) -> String {
    let mut source: Option<&dyn std::error::Error> = Some(err);
    while let Some(current) = source {
        if current.to_string().contains("keep-alive timed out") {
            return "The device stopped answering keepalives.".into();
        }
        source = current.source();
    }
    let mut message = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(current) = source {
        message = format!("{message}: {current}");
        source = current.source();
    }
    message
}

/// Call deadline and size limit.
#[derive(Debug, Clone, Copy, Default)]
pub struct CallOptions {
    /// Deadline for the whole call, sent as `grpc-timeout`.
    pub timeout: Option<Duration>,
    /// Fail with RESOURCE_EXHAUSTED once the response passes this many bytes.
    pub max_bytes: Option<u64>,
}

/// One HTTP/2 connection to a gRPC server and any number of calls on it.
#[derive(Clone)]
pub struct GrpcConnection {
    channel: Channel,
    origin: Uri,
    state: Arc<ConnectionState>,
}

impl GrpcConnection {
    /// Run HTTP/2 over a Muxus-owned stream. `authority` is host:port as the
    /// device is addressed (HTTP/2 `:authority`); `secure` picks the scheme.
    pub async fn connect(
        io: impl GrpcIo,
        authority: &str,
        secure: bool,
    ) -> Result<Self, GrpcError> {
        let origin = Uri::builder()
            .scheme(if secure { "https" } else { "http" })
            .authority(authority)
            .path_and_query("/")
            .build()
            .map_err(|err| GrpcError::new(code::INVALID_ARGUMENT, err.to_string()))?;
        let (send, connection) = http2::Builder::new(TokioExecutor::new())
            .timer(TokioTimer::new())
            .initial_stream_window_size(WINDOW_BYTES)
            .initial_connection_window_size(WINDOW_BYTES)
            .keep_alive_interval(PING_INTERVAL)
            .keep_alive_timeout(PING_TIMEOUT)
            .keep_alive_while_idle(true)
            .handshake::<_, tonic::body::Body>(TokioIo::new(io))
            .await
            .map_err(|err| GrpcError::new(code::UNAVAILABLE, connection_failure(&err)))?;
        let (closed, _) = watch::channel(None);
        let state = Arc::new(ConnectionState {
            closed,
            task: Mutex::new(None),
        });
        let watcher = Arc::downgrade(&state);
        let task = tokio::spawn(async move {
            let reason = match connection.await {
                Ok(()) => "The device closed the connection.".to_owned(),
                Err(err) => connection_failure(&err),
            };
            if let Some(state) = watcher.upgrade() {
                state.shutdown(reason);
            }
        });
        *state.task.lock().expect("task lock") = Some(task.abort_handle());
        Ok(Self {
            channel: Channel {
                send,
                metadata: Arc::default(),
            },
            origin,
            state,
        })
    }

    /// Headers sent with every call (gNMI carries its login here).
    pub fn set_metadata(&self, entries: &[(&str, &str)]) {
        let headers = entries
            .iter()
            .filter_map(|(name, value)| {
                Some((
                    HeaderName::from_bytes(name.as_bytes()).ok()?,
                    HeaderValue::from_bytes(value.as_bytes()).ok()?,
                ))
            })
            .collect();
        *self.channel.metadata.lock().expect("metadata lock") = headers;
    }

    /// Why the connection ended, once it has.
    pub fn closed_reason(&self) -> Option<String> {
        self.state.closed.borrow().clone()
    }

    /// Resolves with the reason once the connection ends.
    pub fn closed(&self) -> impl Future<Output = String> + Send + 'static {
        let mut receiver = self.state.closed.subscribe();
        async move {
            match receiver.wait_for(Option::is_some).await {
                Ok(reason) => reason.clone().unwrap_or_default(),
                Err(_) => "The connection closed.".to_owned(),
            }
        }
    }

    /// End the connection and every call on it.
    pub fn close(&self, reason: &str) {
        self.state.shutdown(reason.to_owned());
    }

    fn grpc(&self) -> tonic::client::Grpc<Channel> {
        tonic::client::Grpc::with_origin(self.channel.clone(), self.origin.clone())
            .max_decoding_message_size(usize::MAX)
            .max_encoding_message_size(usize::MAX)
    }

    /// Start a call; write the request(s) and end this side through the handle.
    pub fn open_call(&self, path: &str, options: CallOptions) -> GrpcCall {
        let (requests_tx, requests_rx) = mpsc::unbounded_channel();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let queued = Arc::new(Queued::default());
        let meter = Arc::new(CallMeter::new(options.max_bytes));
        let cancel = CancellationToken::new();
        let cancel_error = Arc::new(Mutex::new(None));
        let handle = CallHandle {
            requests: Arc::new(Mutex::new(Some(requests_tx))),
            queued: queued.clone(),
            cancel: cancel.clone(),
            cancel_error: cancel_error.clone(),
            meter: meter.clone(),
        };
        if let Some(reason) = self.closed_reason() {
            let _ = events_tx.send(CallEvent::End(Some(GrpcError::new(
                code::UNAVAILABLE,
                session_closed(&reason),
            ))));
            return GrpcCall {
                handle,
                events: events_rx,
                done: false,
            };
        }
        let path = match PathAndQuery::try_from(path) {
            Ok(path) => path,
            Err(err) => {
                let _ = events_tx.send(CallEvent::End(Some(GrpcError::new(
                    code::INVALID_ARGUMENT,
                    err.to_string(),
                ))));
                return GrpcCall {
                    handle,
                    events: events_rx,
                    done: false,
                };
            }
        };
        let mut grpc = self.grpc();
        let connection = self.clone();
        let closed = self.closed();
        tokio::spawn(async move {
            let stream = RequestStream {
                rx: requests_rx,
                queued,
            };
            let mut request = tonic::Request::new(stream);
            request.extensions_mut().insert(meter);
            if let Some(timeout) = options.timeout {
                request.set_timeout(timeout);
            }
            let events = events_tx.clone();
            let run = async move {
                grpc.ready()
                    .await
                    .map_err(|err| GrpcError::new(code::UNAVAILABLE, connection_failure(&err)))?;
                let response = grpc.streaming(request, path, RawCodec).await?;
                let mut body = response.into_inner();
                while let Some(message) = body.message().await? {
                    let _ = events.send(CallEvent::Message(message));
                }
                Ok::<(), GrpcError>(())
            };
            let outcome = tokio::select! {
                biased;
                () = cancel.cancelled() => Some(
                    cancel_error
                        .lock()
                        .expect("cancel lock")
                        .take()
                        .unwrap_or_else(GrpcError::cancelled),
                ),
                reason = closed => Some(GrpcError::new(code::UNAVAILABLE, reason)),
                result = run => result.err().map(|err| match connection.closed_reason() {
                    // A transport failure reads better as why the connection ended.
                    Some(reason) if is_transport_failure(&err) => {
                        GrpcError::new(code::UNAVAILABLE, reason)
                    }
                    _ => err,
                }),
            };
            let _ = events_tx.send(CallEvent::End(outcome));
        });
        GrpcCall {
            handle,
            events: events_rx,
            done: false,
        }
    }

    /// One request, one response: the last message the device sent, and the
    /// response size.
    pub async fn unary(
        &self,
        path: &str,
        request: Bytes,
        options: CallOptions,
        cancel: Option<&CancellationToken>,
    ) -> Result<(Bytes, u64), GrpcError> {
        if let Some(reason) = self.closed_reason() {
            return Err(GrpcError::new(code::UNAVAILABLE, session_closed(&reason)));
        }
        let mut call = self.open_call(path, options);
        call.handle.write(request);
        call.handle.end();
        let mut response = None;
        let never = CancellationToken::new();
        let cancel = cancel.unwrap_or(&never);
        let mut cancelled = false;
        loop {
            let event = tokio::select! {
                () = cancel.cancelled(), if !cancelled => {
                    cancelled = true;
                    call.handle.cancel(None);
                    continue;
                }
                event = call.next() => event,
            };
            match event {
                Some(CallEvent::Message(message)) => response = Some(message),
                Some(CallEvent::End(None)) | None => break,
                Some(CallEvent::End(Some(err))) => return Err(err),
            }
        }
        match response {
            Some(message) => Ok((message, call.handle.bytes())),
            None => Err(GrpcError::new(
                code::INTERNAL,
                "The device sent no response message.",
            )),
        }
    }
}

fn session_closed(_reason: &str) -> String {
    "The session is closed.".into()
}

fn is_transport_failure(err: &GrpcError) -> bool {
    matches!(
        err.code,
        code::UNKNOWN | code::UNAVAILABLE | code::INTERNAL | code::CANCELLED
    )
}

/// Requests written but not yet taken by the HTTP/2 stream.
#[derive(Default)]
struct Queued {
    count: AtomicUsize,
    drained: Notify,
}

struct RequestStream {
    rx: mpsc::UnboundedReceiver<Bytes>,
    queued: Arc<Queued>,
}

impl tokio_stream::Stream for RequestStream {
    type Item = Bytes;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Bytes>> {
        let item = ready!(self.rx.poll_recv(cx));
        if item.is_some() {
            self.queued.count.fetch_sub(1, Ordering::AcqRel);
            self.queued.drained.notify_waiters();
        }
        Poll::Ready(item)
    }
}

impl Drop for RequestStream {
    fn drop(&mut self) {
        // Nothing queued will go out any more; let writers waiting for room go.
        self.queued.count.store(0, Ordering::Release);
        self.queued.drained.notify_waiters();
    }
}

/// What a call delivers: response messages, then its end (an error, or
/// `None` for OK) exactly once.
#[derive(Debug, Clone, PartialEq)]
pub enum CallEvent {
    Message(Bytes),
    End(Option<GrpcError>),
}

/// The writing and cancelling side of a call; cheap to clone.
#[derive(Clone)]
pub struct CallHandle {
    requests: Arc<Mutex<Option<mpsc::UnboundedSender<Bytes>>>>,
    queued: Arc<Queued>,
    cancel: CancellationToken,
    cancel_error: Arc<Mutex<Option<GrpcError>>>,
    meter: Arc<CallMeter>,
}

impl CallHandle {
    /// Encoded response bytes received so far.
    pub fn bytes(&self) -> u64 {
        self.meter.bytes()
    }

    /// Queue a request message; ignored once this side has ended.
    pub fn write(&self, message: Bytes) -> bool {
        let requests = self.requests.lock().expect("requests lock");
        let Some(sender) = requests.as_ref() else {
            return false;
        };
        self.queued.count.fetch_add(1, Ordering::AcqRel);
        if sender.send(message).is_err() {
            self.queued.count.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }

    /// Write and wait until HTTP/2 took the message: backpressure for uploads.
    pub async fn write_drained(&self, message: Bytes) -> Result<(), GrpcError> {
        if self.cancel.is_cancelled() || !self.write(message) {
            return Err(GrpcError::new(code::UNAVAILABLE, "The call has ended."));
        }
        loop {
            let drained = self.queued.drained.notified();
            if self.queued.count.load(Ordering::Acquire) == 0 {
                return Ok(());
            }
            if self.cancel.is_cancelled() {
                return Err(GrpcError::new(code::UNAVAILABLE, "The call has ended."));
            }
            tokio::select! {
                () = drained => {}
                () = self.cancel.cancelled() => {}
            }
        }
    }

    /// Half-close: no more requests from this side.
    pub fn end(&self) {
        self.requests.lock().expect("requests lock").take();
    }

    /// Stop the call; it ends with `error`, or CANCELLED.
    pub fn cancel(&self, error: Option<GrpcError>) {
        if self.cancel.is_cancelled() {
            return;
        }
        if let Some(error) = error {
            *self.cancel_error.lock().expect("cancel lock") = Some(error);
        }
        self.cancel.cancel();
    }
}

/// A call in progress: its handle plus the events it delivers.
pub struct GrpcCall {
    pub handle: CallHandle,
    events: mpsc::UnboundedReceiver<CallEvent>,
    done: bool,
}

impl GrpcCall {
    /// The next response message or the end; `None` after the end was delivered.
    pub async fn next(&mut self) -> Option<CallEvent> {
        if self.done {
            return None;
        }
        let event = self.events.recv().await;
        match event {
            Some(CallEvent::End(_)) | None => self.done = true,
            Some(CallEvent::Message(_)) => {}
        }
        event.or(Some(CallEvent::End(Some(GrpcError::new(
            code::UNAVAILABLE,
            "The call ended without a status.",
        )))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_like_the_typescript() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(24 * 1024 * 1024), "24.0 MiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
    }

    #[test]
    fn maps_http_failures_to_grpc_statuses() {
        assert_eq!(
            http_status_to_grpc(StatusCode::NOT_FOUND),
            code::UNIMPLEMENTED
        );
        let response = http_failure(StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["grpc-status"], "14");
        assert_eq!(
            response.headers()["grpc-message"],
            "The device answered HTTP 503."
        );
    }
}
