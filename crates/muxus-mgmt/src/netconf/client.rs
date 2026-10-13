//! One NETCONF session on a `netconf` SSH subsystem channel: the hello
//! exchange, framing negotiation, message-id bookkeeping and notifications.
//! RPC replies are returned as text, `<rpc-error>` included; only transport
//! failures are errors.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_core::management::NetconfBase;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;

use super::framing::{NetconfDecoder, encode_chunked, encode_end_of_message};
use super::xml::{
    NETCONF_BASE_1_1, client_hello, notification_event_time, parse_hello, root_element, wrap_rpc,
};
use crate::error::NetconfError;

const HELLO_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(120);
const READ_BUFFER: usize = 64 * 1024;

/// The byte stream of a `netconf` subsystem channel (or anything shaped like one).
pub trait NetconfChannel: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> NetconfChannel for T {}

/// A channel of any kind, for code that does not care which.
pub type BoxedChannel = Box<dyn NetconfChannel>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetconfReply {
    pub xml: String,
    pub message_id: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetconfNotification {
    pub xml: String,
    pub event_time: Option<String>,
}

struct PendingRpc {
    message_id: String,
    reply: Option<oneshot::Sender<Result<NetconfReply, NetconfError>>>,
}

struct Shared {
    pending: Mutex<Vec<PendingRpc>>,
    closed: watch::Sender<Option<String>>,
    notifications: mpsc::UnboundedSender<NetconfNotification>,
}

impl Shared {
    fn dispatch(&self, xml: String) {
        let root = root_element(&xml);
        match root.as_ref().map(|root| root.name.as_str()) {
            Some("notification") => {
                let event_time = notification_event_time(&xml);
                let _ = self
                    .notifications
                    .send(NetconfNotification { xml, event_time });
            }
            Some("rpc-reply") => {
                let id = root.and_then(|root| root.attributes.get("message-id").cloned());
                let mut pending = self.pending.lock().expect("pending lock");
                // Replies come back in order; one without an id answers the oldest RPC.
                let index = match &id {
                    None => (!pending.is_empty()).then_some(0),
                    Some(id) => pending.iter().position(|entry| entry.message_id == *id),
                };
                let Some(index) = index else {
                    return;
                };
                let entry = pending.remove(index);
                if let Some(reply) = entry.reply {
                    let bytes = xml.len() as u64;
                    let _ = reply.send(Ok(NetconfReply {
                        xml,
                        message_id: entry.message_id,
                        bytes,
                    }));
                }
            }
            _ => {}
        }
    }

    fn shutdown(&self, reason: &str) {
        let changed = self.closed.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(reason.to_owned());
            true
        });
        if !changed {
            return;
        }
        for entry in self.pending.lock().expect("pending lock").drain(..) {
            if let Some(reply) = entry.reply {
                let _ = reply.send(Err(NetconfError::new(reason)));
            }
        }
    }
}

/// A NETCONF session.
pub struct NetconfClient<C: NetconfChannel> {
    pub capabilities: Vec<String>,
    pub session_id: String,
    pub base: NetconfBase,
    writer: tokio::sync::Mutex<WriteHalf<C>>,
    shared: Arc<Shared>,
    next_message_id: AtomicU64,
    reader: AbortHandle,
}

impl<C: NetconfChannel> NetconfClient<C> {
    /// Exchange hellos on a freshly opened subsystem channel. Notifications
    /// the server sends later arrive on the returned receiver.
    pub async fn open(
        channel: C,
    ) -> Result<(Self, mpsc::UnboundedReceiver<NetconfNotification>), NetconfError> {
        let (mut reader, mut writer) = tokio::io::split(channel);
        writer
            .write_all(&encode_end_of_message(&client_hello()))
            .await
            .map_err(|err| NetconfError::new(err.to_string()))?;
        let _ = writer.flush().await;
        let mut decoder = NetconfDecoder::default();
        let hello_exchange = async {
            let mut buffer = vec![0; READ_BUFFER];
            loop {
                let read = reader
                    .read(&mut buffer)
                    .await
                    .map_err(|err| NetconfError::new(err.to_string()))?;
                if read == 0 {
                    return Err(NetconfError::new(
                        "The server closed the NETCONF subsystem before its <hello>.",
                    ));
                }
                let messages = decoder.push(&buffer[..read]).map_err(|err| {
                    NetconfError::new(format!("Malformed NETCONF framing: {err}"))
                })?;
                if !messages.is_empty() {
                    return Ok(messages);
                }
            }
        };
        let messages = tokio::time::timeout(HELLO_TIMEOUT, hello_exchange)
            .await
            .map_err(|_| {
                NetconfError::new("The server sent no NETCONF <hello> within 30 seconds.")
            })??;
        let hello = parse_hello(&messages[0])?;
        let base = if hello
            .capabilities
            .iter()
            .any(|capability| capability == NETCONF_BASE_1_1)
        {
            NetconfBase::V1_1
        } else {
            NetconfBase::V1_0
        };
        // Bytes after the hello belong to the negotiated framing.
        let mut leftover: Vec<String> = messages[1..].to_vec();
        if base == NetconfBase::V1_1 {
            decoder.use_chunked();
            leftover.extend(
                decoder.push(&[]).map_err(|err| {
                    NetconfError::new(format!("Malformed NETCONF framing: {err}"))
                })?,
            );
        }
        let (notifications_tx, notifications) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            pending: Mutex::default(),
            closed: watch::channel(None).0,
            notifications: notifications_tx,
        });
        for message in leftover {
            shared.dispatch(message);
        }
        let reader = tokio::spawn(read_loop(reader, decoder, shared.clone())).abort_handle();
        Ok((
            Self {
                capabilities: hello.capabilities,
                session_id: hello.session_id.unwrap_or_default(),
                base,
                writer: tokio::sync::Mutex::new(writer),
                shared,
                next_message_id: AtomicU64::new(1),
                reader,
            },
            notifications,
        ))
    }

    /// Resolves with the reason once the session ends.
    pub fn closed(&self) -> impl Future<Output = String> + Send + 'static {
        let mut receiver = self.shared.closed.subscribe();
        async move {
            match receiver.wait_for(Option::is_some).await {
                Ok(reason) => reason.clone().unwrap_or_default(),
                Err(_) => "The NETCONF session closed.".to_owned(),
            }
        }
    }

    pub fn is_closed(&self) -> bool {
        self.shared.closed.borrow().is_some()
    }

    fn frame(&self, message: &str) -> Vec<u8> {
        match self.base {
            NetconfBase::V1_1 => encode_chunked(message).unwrap_or_default(),
            NetconfBase::V1_0 => encode_end_of_message(message),
        }
    }

    /// Send one RPC (the operation element(s) the `<rpc>` wraps) and wait for its reply.
    pub async fn rpc(
        &self,
        body: &str,
        timeout: Option<Duration>,
        cancel: Option<&CancellationToken>,
    ) -> Result<NetconfReply, NetconfError> {
        if self.is_closed() {
            return Err(NetconfError::new("The NETCONF session is closed."));
        }
        let message_id = self
            .next_message_id
            .fetch_add(1, Ordering::Relaxed)
            .to_string();
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(NetconfError::cancelled());
        }
        let (sender, receiver) = oneshot::channel();
        // The entry stays queued after a timeout or cancel: NETCONF cannot
        // abandon an RPC, so its late reply still has to find a slot.
        self.shared
            .pending
            .lock()
            .expect("pending lock")
            .push(PendingRpc {
                message_id: message_id.clone(),
                reply: Some(sender),
            });
        let frame = self.frame(&wrap_rpc(&message_id, body));
        {
            let mut writer = self.writer.lock().await;
            if let Err(err) = writer.write_all(&frame).await {
                self.shared.shutdown(&err.to_string());
            }
            let _ = writer.flush().await;
        }
        let timeout = timeout.unwrap_or(DEFAULT_RPC_TIMEOUT);
        let never = CancellationToken::new();
        let cancel = cancel.unwrap_or(&never);
        tokio::select! {
            reply = receiver => reply.unwrap_or_else(|_| Err(NetconfError::new("The NETCONF session closed."))),
            () = tokio::time::sleep(timeout) => Err(NetconfError::new(format!(
                "No reply within {} seconds.",
                (timeout.as_millis() as f64 / 1000.0).round()
            ))),
            () = cancel.cancelled() => Err(NetconfError::cancelled()),
        }
    }

    /// Say goodbye with `<close-session/>` when the session is healthy, then drop the channel.
    pub async fn close(&self) {
        if !self.is_closed() {
            let _ = self
                .rpc("<close-session/>", Some(Duration::from_secs(2)), None)
                .await;
        }
        self.shared.shutdown("The NETCONF session was closed.");
        let _ = self.writer.lock().await.shutdown().await;
        let reader = self.reader.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            reader.abort();
        });
    }
}

impl<C: NetconfChannel> Drop for NetconfClient<C> {
    fn drop(&mut self) {
        self.reader.abort();
        self.shared.shutdown("The NETCONF session was closed.");
    }
}

async fn read_loop<C: NetconfChannel>(
    mut reader: ReadHalf<C>,
    mut decoder: NetconfDecoder,
    shared: Arc<Shared>,
) {
    let mut buffer = vec![0; READ_BUFFER];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => {
                shared.shutdown("The NETCONF session closed.");
                return;
            }
            Ok(read) => read,
            Err(err) => {
                shared.shutdown(&err.to_string());
                return;
            }
        };
        match decoder.push(&buffer[..read]) {
            Ok(messages) => {
                for message in messages {
                    shared.dispatch(message);
                }
            }
            Err(err) => {
                shared.shutdown(&format!("Malformed NETCONF framing: {err}"));
                return;
            }
        }
    }
}
