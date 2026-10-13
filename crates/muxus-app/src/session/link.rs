//! A terminal session as the window sees it: an ordered stream of output and
//! protocol messages in, input and protocol messages out.
//!
//! This is the `/ws/terminal` socket of the TypeScript client
//! (`shared/src/ws-protocol.ts`) without the socket. The backend's session
//! handle speaks it directly; a bare transport, such as a local pty opened
//! without the backend, is adapted with [`SessionLink::from_transport`], which
//! answers like the server's local branch did: `ready` at once, the shell's end
//! as an `exit` message with reason `completed`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use muxus_core::protocol::{
    DEFAULT_BREAK_DURATION_MS, ExitReason, TerminalClientMessage, TerminalServerMessage,
};
use muxus_core::stream::{TerminalSize, TransportChannels, TransportEvent, TransportInput};

/// A boxed `Send` future, so readers and writers can live behind trait objects.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a session reports, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkEvent {
    /// Terminal output.
    Data(Vec<u8>),
    /// A control message (`status`, `ready`, `auth-prompt`, `exit`, …), boxed:
    /// rare next to output, and much larger.
    Message(Box<TerminalServerMessage>),
    /// The session's channel closed; nothing follows.
    Closed { reason: Option<String> },
}

/// What the window sends to a session.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkInput {
    /// Keystrokes and pasted text, already encoded.
    Data(Vec<u8>),
    /// A control message (`resize`, `auth-response`, `send-break`, …), boxed.
    Message(Box<TerminalClientMessage>),
    /// End the session from the window's side.
    Close,
}

impl LinkEvent {
    pub fn message(message: TerminalServerMessage) -> Self {
        LinkEvent::Message(Box::new(message))
    }
}

impl LinkInput {
    pub fn message(message: TerminalClientMessage) -> Self {
        LinkInput::Message(Box::new(message))
    }
}

/// The receiving half of a session.
pub trait LinkReader: Send + 'static {
    /// The next event, waiting for one; `None` once the session is gone.
    fn next_event(&mut self) -> BoxFuture<'_, Option<LinkEvent>>;
    /// The next event if one is already queued.
    fn try_next_event(&mut self) -> Option<LinkEvent>;
}

/// The sending half of a session. Shared: the input pump, dialogs and menus
/// all write to the same session.
pub trait LinkWriter: Send + Sync + 'static {
    /// Queue `input`; resolves once it is queued (it waits while a bounded
    /// queue is full). `false` once the session has ended.
    fn send(&self, input: LinkInput) -> BoxFuture<'static, bool>;
}

/// An open session: both halves.
pub struct SessionLink {
    pub reader: Box<dyn LinkReader>,
    pub writer: Arc<dyn LinkWriter>,
}

/// The session's side of [`SessionLink::channel_pair`].
#[derive(Clone, Debug)]
pub struct LinkEnds {
    pub events: async_channel::Sender<LinkEvent>,
    pub input: async_channel::Receiver<LinkInput>,
}

impl SessionLink {
    /// A link over a pair of channels: the window's half and the session's
    /// half. Backends that run their session elsewhere (and tests) feed it.
    pub fn channel_pair(capacity: usize) -> (SessionLink, LinkEnds) {
        let (events_tx, events_rx) = async_channel::bounded(capacity.max(1));
        let (input_tx, input_rx) = async_channel::unbounded();
        (
            SessionLink {
                reader: Box::new(ChannelReader(events_rx)),
                writer: Arc::new(ChannelWriter(input_tx)),
            },
            LinkEnds {
                events: events_tx,
                input: input_rx,
            },
        )
    }

    /// A session whose start already failed: it reports the failure and closes.
    pub fn failed(message: impl Into<String>) -> SessionLink {
        let (link, ends) = Self::channel_pair(2);
        let _ = ends
            .events
            .try_send(LinkEvent::message(TerminalServerMessage::Exit {
                code: Some(1),
                message: Some(message.into()),
                reason: ExitReason::Failed,
            }));
        let _ = ends.events.try_send(LinkEvent::Closed { reason: None });
        link
    }

    /// A bare transport as a session (the server's local branch): `ready` at
    /// once, output as data, the transport's end as an `exit` message.
    pub fn from_transport(channels: TransportChannels, conn_id: impl Into<String>) -> SessionLink {
        SessionLink {
            reader: Box::new(TransportReader {
                events: channels.events,
                conn_id: Some(conn_id.into()),
                closing: false,
                done: false,
            }),
            writer: Arc::new(TransportWriter {
                input: channels.input,
            }),
        }
    }
}

struct ChannelReader(async_channel::Receiver<LinkEvent>);

impl LinkReader for ChannelReader {
    fn next_event(&mut self) -> BoxFuture<'_, Option<LinkEvent>> {
        Box::pin(async move { self.0.recv().await.ok() })
    }

    fn try_next_event(&mut self) -> Option<LinkEvent> {
        self.0.try_recv().ok()
    }
}

struct ChannelWriter(async_channel::Sender<LinkInput>);

impl LinkWriter for ChannelWriter {
    fn send(&self, input: LinkInput) -> BoxFuture<'static, bool> {
        let sender = self.0.clone();
        Box::pin(async move { sender.send(input).await.is_ok() })
    }
}

struct TransportReader {
    events: async_channel::Receiver<TransportEvent>,
    /// `ready` is still to be reported.
    conn_id: Option<String>,
    /// The `exit` message went out; `Closed` follows.
    closing: bool,
    done: bool,
}

impl TransportReader {
    fn translate(&mut self, event: Option<TransportEvent>) -> Option<LinkEvent> {
        match event {
            Some(TransportEvent::Data(data)) => Some(LinkEvent::Data(data)),
            Some(TransportEvent::Exit { code, .. }) => {
                self.closing = true;
                Some(LinkEvent::message(TerminalServerMessage::Exit {
                    code,
                    message: None,
                    reason: ExitReason::Completed,
                }))
            }
            Some(TransportEvent::Error(message)) => {
                self.closing = true;
                Some(LinkEvent::message(TerminalServerMessage::Exit {
                    code: Some(1),
                    message: Some(message),
                    reason: ExitReason::Failed,
                }))
            }
            None => {
                self.done = true;
                Some(LinkEvent::Closed { reason: None })
            }
        }
    }

    /// The events that need no waiting: `ready` first, `Closed` after an exit.
    fn immediate(&mut self) -> Option<Option<LinkEvent>> {
        if self.done {
            return Some(None);
        }
        if let Some(conn_id) = self.conn_id.take() {
            return Some(Some(LinkEvent::message(TerminalServerMessage::Ready {
                conn_id,
                host: None,
                user: None,
                sftp_available: None,
                summary: None,
            })));
        }
        if self.closing {
            self.done = true;
            return Some(Some(LinkEvent::Closed { reason: None }));
        }
        None
    }
}

impl LinkReader for TransportReader {
    fn next_event(&mut self) -> BoxFuture<'_, Option<LinkEvent>> {
        Box::pin(async move {
            if let Some(event) = self.immediate() {
                return event;
            }
            let event = self.events.recv().await.ok();
            self.translate(event)
        })
    }

    fn try_next_event(&mut self) -> Option<LinkEvent> {
        if let Some(event) = self.immediate() {
            return event;
        }
        match self.events.try_recv() {
            Ok(event) => self.translate(Some(event)),
            Err(async_channel::TryRecvError::Empty) => None,
            Err(async_channel::TryRecvError::Closed) => self.translate(None),
        }
    }
}

struct TransportWriter {
    input: async_channel::Sender<TransportInput>,
}

impl LinkWriter for TransportWriter {
    fn send(&self, input: LinkInput) -> BoxFuture<'static, bool> {
        let message = match input {
            LinkInput::Data(data) => Some(TransportInput::Data(data)),
            LinkInput::Close => Some(TransportInput::Close),
            LinkInput::Message(message) => match *message {
                TerminalClientMessage::Resize { cols, rows } => {
                    Some(TransportInput::Resize(TerminalSize {
                        cols: cols.min(u32::from(u16::MAX)) as u16,
                        rows: rows.min(u32::from(u16::MAX)) as u16,
                        pixel_width: 0,
                        pixel_height: 0,
                    }))
                }
                TerminalClientMessage::SendBreak => Some(TransportInput::Break {
                    duration_ms: DEFAULT_BREAK_DURATION_MS,
                }),
                // A bare transport has no prompts, logging or pacing to answer.
                _ => None,
            },
        };
        let sender = self.input.clone();
        Box::pin(async move {
            match message {
                Some(message) => sender.send(message).await.is_ok(),
                None => !sender.is_closed(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;

    #[test]
    fn a_transport_answers_ready_then_reports_its_exit_and_closes() {
        let (channels, ends) = TransportChannels::pair(8);
        let mut link = SessionLink::from_transport(channels, "local-1");
        block_on(async {
            assert!(matches!(
                link.reader.next_event().await,
                Some(LinkEvent::Message(message))
                    if matches!(&*message, TerminalServerMessage::Ready { conn_id, .. } if conn_id == "local-1")
            ));
            ends.events
                .send(TransportEvent::Data(b"$ ".to_vec()))
                .await
                .unwrap();
            ends.events
                .send(TransportEvent::Exit {
                    code: Some(3),
                    signal: None,
                })
                .await
                .unwrap();
            assert_eq!(
                link.reader.next_event().await,
                Some(LinkEvent::Data(b"$ ".to_vec()))
            );
            assert_eq!(
                link.reader.try_next_event(),
                Some(LinkEvent::message(TerminalServerMessage::Exit {
                    code: Some(3),
                    message: None,
                    reason: ExitReason::Completed,
                }))
            );
            assert_eq!(
                link.reader.next_event().await,
                Some(LinkEvent::Closed { reason: None })
            );
            assert_eq!(link.reader.next_event().await, None);
        });
    }

    #[test]
    fn a_transport_failure_reads_as_a_failed_exit() {
        let (channels, ends) = TransportChannels::pair(8);
        let mut link = SessionLink::from_transport(channels, "local-1");
        drop(ends.events.try_send(TransportEvent::Error("no pty".into())));
        block_on(async {
            link.reader.next_event().await;
            assert_eq!(
                link.reader.next_event().await,
                Some(LinkEvent::message(TerminalServerMessage::Exit {
                    code: Some(1),
                    message: Some("no pty".into()),
                    reason: ExitReason::Failed,
                }))
            );
        });
    }

    #[test]
    fn a_transport_takes_input_resizes_breaks_and_close() {
        let (channels, ends) = TransportChannels::pair(8);
        let link = SessionLink::from_transport(channels, "local-1");
        block_on(async {
            assert!(link.writer.send(LinkInput::Data(b"ls\r".to_vec())).await);
            assert!(
                link.writer
                    .send(LinkInput::message(TerminalClientMessage::Resize {
                        cols: 120,
                        rows: 40
                    }))
                    .await
            );
            assert!(
                link.writer
                    .send(LinkInput::message(TerminalClientMessage::SendBreak))
                    .await
            );
            assert!(
                link.writer
                    .send(LinkInput::message(TerminalClientMessage::PasteCancel))
                    .await
            );
            assert!(link.writer.send(LinkInput::Close).await);
        });
        assert_eq!(
            ends.input.try_recv().unwrap(),
            TransportInput::Data(b"ls\r".to_vec())
        );
        assert!(matches!(
            ends.input.try_recv().unwrap(),
            TransportInput::Resize(TerminalSize {
                cols: 120,
                rows: 40,
                ..
            })
        ));
        assert!(matches!(
            ends.input.try_recv().unwrap(),
            TransportInput::Break { .. }
        ));
        assert_eq!(ends.input.try_recv().unwrap(), TransportInput::Close);
    }

    #[test]
    fn a_failed_start_reports_the_reason_and_closes() {
        let mut link = SessionLink::failed("spawn failed");
        block_on(async {
            assert!(matches!(
                link.reader.next_event().await,
                Some(LinkEvent::Message(message))
                    if matches!(*message, TerminalServerMessage::Exit {
                        reason: ExitReason::Failed,
                        ..
                    })
            ));
            assert_eq!(
                link.reader.next_event().await,
                Some(LinkEvent::Closed { reason: None })
            );
        });
    }
}
