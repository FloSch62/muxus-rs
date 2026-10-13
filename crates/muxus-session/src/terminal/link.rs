//! The session's side of a transport's byte stream: ordered, non-blocking input with drain
//! tracking, and the transport's events.

use std::future::Future;
use std::sync::Mutex;

use muxus_core::stream::{
    BreakOutcome, TerminalSize, TransportChannels, TransportEvent, TransportFeedback,
    TransportInput,
};

/// Input to one transport. Writes never block: they queue in order and a pump feeds the
/// transport's bounded input. Each `Data` write is numbered, so a paced paste can wait until
/// that write has left (`drain()` in the TypeScript transports).
pub(crate) struct TransportLink {
    queue: async_channel::Sender<TransportInput>,
    /// `Data` messages queued so far; the lock keeps numbering and queue order the same.
    sent: Mutex<u64>,
    pub feedback: TransportFeedback,
}

impl TransportLink {
    /// Takes over a transport's channels; returns the link and the transport's events.
    pub fn new(
        channels: TransportChannels,
    ) -> (
        std::sync::Arc<Self>,
        async_channel::Receiver<TransportEvent>,
    ) {
        let TransportChannels {
            input,
            events,
            feedback,
        } = channels;
        let (queue_tx, queue_rx) = async_channel::unbounded::<TransportInput>();
        tokio::spawn(async move {
            while let Ok(message) = queue_rx.recv().await {
                let close = matches!(message, TransportInput::Close);
                if input.send(message).await.is_err() || close {
                    break;
                }
            }
            input.close();
        });
        (
            std::sync::Arc::new(Self {
                queue: queue_tx,
                sent: Mutex::new(0),
                feedback,
            }),
            events,
        )
    }

    /// Queue terminal input; returns its number (0 when the session is over).
    pub fn write(&self, data: Vec<u8>) -> u64 {
        let mut sent = self
            .sent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.queue.try_send(TransportInput::Data(data)).is_err() {
            return 0;
        }
        *sent += 1;
        *sent
    }

    /// Queue terminal input; the future settles once it has left for the remote end.
    pub fn write_drained(
        &self,
        data: Vec<u8>,
    ) -> impl Future<Output = Result<(), String>> + Send + 'static {
        let number = self.write(data);
        let wait = self.feedback.until_written(number);
        async move {
            if number == 0 {
                return Err("the session is closed".to_string());
            }
            wait.await;
            Ok(())
        }
    }

    pub fn resize(&self, cols: u32, rows: u32) {
        let clamp = |value: u32| u16::try_from(value).unwrap_or(u16::MAX);
        let _ = self.queue.try_send(TransportInput::Resize(TerminalSize {
            cols: clamp(cols),
            rows: clamp(rows),
            pixel_width: 0,
            pixel_height: 0,
        }));
    }

    pub fn send_break(&self, duration_ms: u32) {
        let _ = self.queue.try_send(TransportInput::Break { duration_ms });
    }

    /// End the transport from this side.
    pub fn close(&self) {
        let _ = self.queue.try_send(TransportInput::Close);
        self.queue.close();
    }
}

/// The `break-result` answer for an outcome (`answerBreak`).
pub(crate) fn break_result(outcome: Option<BreakOutcome>) -> (bool, Option<String>) {
    match outcome {
        Some(BreakOutcome::Sent) => (true, None),
        Some(BreakOutcome::Failed(reason)) => {
            (false, Some(format!("Could not send BREAK: {reason}")))
        }
        Some(BreakOutcome::Declined(message)) => (false, Some(message)),
        None => (
            false,
            Some("Could not send BREAK: the connection is closed".to_string()),
        ),
    }
}
