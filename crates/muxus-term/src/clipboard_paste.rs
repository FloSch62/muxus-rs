//! Pasting the clipboard into a terminal, including images (port of
//! `client/src/terminal/clipboard-paste.ts`). An image is stored where the session can read it —
//! uploaded over SFTP for SSH, written locally for local shells — and its path is pasted.
//!
//! Pastes are applied strictly in the order they were asked for, even though reading the
//! clipboard and uploading an image take varying time. The queue is executor-agnostic: its futures
//! only need to be polled.

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// What the clipboard held when it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardPayload {
    Text(String),
    /// PNG bytes.
    Image(Vec<u8>),
    Empty,
    /// The clipboard could not be read.
    Unavailable,
}

/// What a paste did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardPasteResult {
    PastedText,
    PastedImagePath,
    SkippedEmpty,
    SkippedUnavailable,
}

/// Where a pasted image was stored: on one SSH connection's host, or on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalImageLocation {
    Ssh { connection_id: String },
    Local,
}

/// The tab an image path is about to be pasted into, at paste time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalImagePasteTarget<'a> {
    pub stored: Option<&'a TerminalImageLocation>,
    /// The tab's session kind (`ssh`, `local`, …).
    pub kind: Option<&'a str>,
    pub connection_id: Option<&'a str>,
    pub input_ready: bool,
    pub transport_open: bool,
}

/// Whether the stored image is readable from the tab's current session.
pub fn is_current_terminal_image_paste_target(target: &TerminalImagePasteTarget<'_>) -> bool {
    let Some(stored) = target.stored else {
        return false;
    };
    if !target.input_ready || !target.transport_open {
        return false;
    }
    // A local file is readable from any local session; a remote one only through the connection
    // it was uploaded over.
    match stored {
        TerminalImageLocation::Local => target.kind == Some("local"),
        TerminalImageLocation::Ssh { connection_id } => {
            target.kind == Some("ssh") && target.connection_id == Some(connection_id.as_str())
        }
    }
}

/// Paste captured text, or store a captured image and paste its path.
pub async fn paste_terminal_clipboard<E, U, UF, T, TF, P, PF>(
    payload: ClipboardPayload,
    upload_image: U,
    paste_text: T,
    paste_image_path: P,
) -> Result<ClipboardPasteResult, E>
where
    U: FnOnce(Vec<u8>) -> UF,
    UF: Future<Output = Result<String, E>>,
    T: FnOnce(String) -> TF,
    TF: Future<Output = Result<(), E>>,
    P: FnOnce(String) -> PF,
    PF: Future<Output = Result<(), E>>,
{
    match payload {
        ClipboardPayload::Text(text) => {
            paste_text(text).await?;
            Ok(ClipboardPasteResult::PastedText)
        }
        ClipboardPayload::Empty => Ok(ClipboardPasteResult::SkippedEmpty),
        ClipboardPayload::Unavailable => Ok(ClipboardPasteResult::SkippedUnavailable),
        ClipboardPayload::Image(png) => {
            let path = upload_image(png).await?;
            paste_image_path(path).await?;
            Ok(ClipboardPasteResult::PastedImagePath)
        }
    }
}

/// Why a queued paste did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteQueueError<E> {
    /// "Too many clipboard pastes are waiting."
    TooManyPastes,
    /// "Too much clipboard image data is waiting."
    TooMuchImageData,
    /// The queue was cancelled (the terminal closed).
    Aborted,
    /// Reading the clipboard or applying the paste failed.
    Failed(E),
}

impl<E: std::fmt::Display> std::fmt::Display for PasteQueueError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyPastes => f.write_str("Too many clipboard pastes are waiting."),
            Self::TooMuchImageData => f.write_str("Too much clipboard image data is waiting."),
            Self::Aborted => f.write_str("The paste was cancelled."),
            Self::Failed(error) => error.fmt(f),
        }
    }
}

/// Cancellation of one queued paste, handed to its apply step.
#[derive(Debug, Clone)]
pub struct AbortSignal(async_channel::Receiver<()>);

impl AbortSignal {
    pub fn is_aborted(&self) -> bool {
        self.0.is_closed()
    }

    /// Resolves when the paste is cancelled.
    pub async fn aborted(&self) {
        while self.0.recv().await.is_ok() {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteQueueLimits {
    pub max_pending_image_bytes: usize,
    pub max_pending_pastes: usize,
}

impl Default for PasteQueueLimits {
    fn default() -> Self {
        Self {
            max_pending_image_bytes: 36 * 1024 * 1024,
            max_pending_pastes: 8,
        }
    }
}

#[derive(Default)]
struct QueueState {
    pending_pastes: usize,
    pending_image_bytes: usize,
    cancels: Vec<(u64, async_channel::Sender<()>)>,
    next_id: u64,
    /// Completes when the most recently queued paste has settled.
    tail: Option<async_channel::Receiver<()>>,
}

/// Applies clipboard pastes one at a time, in the order they were asked for. Reading the clipboard
/// starts at once; applying waits for the pastes before it.
#[derive(Clone, Default)]
pub struct TerminalClipboardPasteQueue {
    limits: PasteQueueLimits,
    state: Arc<Mutex<QueueState>>,
}

struct Settle {
    state: Arc<Mutex<QueueState>>,
    id: u64,
    reserved_image_bytes: AtomicUsize,
    _done: async_channel::Sender<()>,
}

impl Drop for Settle {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending_pastes -= 1;
        state.pending_image_bytes -= self.reserved_image_bytes.load(Ordering::Relaxed);
        state.cancels.retain(|(id, _)| *id != self.id);
    }
}

impl TerminalClipboardPasteQueue {
    pub fn new(limits: PasteQueueLimits) -> Self {
        Self {
            limits,
            state: Arc::default(),
        }
    }

    /// Abort every queued and running paste.
    pub fn cancel_all(&self) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for (_, cancel) in &state.cancels {
            cancel.close();
        }
    }

    /// Queue one paste: `capture` reads the clipboard (it is not called when the queue is full),
    /// `apply` pastes what it read once every earlier paste has settled.
    pub fn enqueue<E, C, CF, A, AF, T>(
        &self,
        capture: C,
        apply: A,
    ) -> impl Future<Output = Result<T, PasteQueueError<E>>> + use<E, C, CF, A, AF, T>
    where
        C: FnOnce(AbortSignal) -> CF,
        CF: Future<Output = Result<ClipboardPayload, E>>,
        A: FnOnce(ClipboardPayload, AbortSignal) -> AF,
        AF: Future<Output = Result<T, E>>,
    {
        let admitted = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.pending_pastes >= self.limits.max_pending_pastes {
                None
            } else {
                state.pending_pastes += 1;
                state.next_id += 1;
                let id = state.next_id;
                let (cancel_tx, cancel_rx) = async_channel::bounded::<()>(1);
                state.cancels.push((id, cancel_tx));
                let (done_tx, done_rx) = async_channel::bounded::<()>(1);
                let previous = state.tail.replace(done_rx);
                Some((id, AbortSignal(cancel_rx), previous, done_tx))
            }
        };
        let state = self.state.clone();
        let limits = self.limits;
        let capture = admitted
            .as_ref()
            .map(|(_, signal, _, _)| capture(signal.clone()));
        async move {
            let (Some((id, signal, previous, done)), Some(capture)) = (admitted, capture) else {
                return Err(PasteQueueError::TooManyPastes);
            };
            let settle = Settle {
                state: state.clone(),
                id,
                reserved_image_bytes: AtomicUsize::new(0),
                _done: done,
            };
            // Read the clipboard while the pastes before this one finish. An image reserves its
            // share of the byte budget as soon as it is read.
            let capture = async {
                let payload = capture.await.map_err(PasteQueueError::Failed)?;
                if let ClipboardPayload::Image(png) = &payload {
                    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                    if guard.pending_image_bytes + png.len() > limits.max_pending_image_bytes {
                        return Err(PasteQueueError::TooMuchImageData);
                    }
                    guard.pending_image_bytes += png.len();
                    settle
                        .reserved_image_bytes
                        .store(png.len(), Ordering::Relaxed);
                }
                Ok(payload)
            };
            let wait_turn = async {
                if let Some(previous) = previous {
                    while previous.recv().await.is_ok() {}
                }
            };
            let (captured, ()) = futures_lite::future::zip(capture, wait_turn).await;
            if signal.is_aborted() {
                return Err(PasteQueueError::Aborted);
            }
            let payload = captured?;
            let applied = {
                let apply = pin!(apply(payload, signal.clone()));
                let aborted = pin!(async {
                    signal.aborted().await;
                    Err(PasteQueueError::Aborted)
                });
                futures_lite::future::or(
                    async { apply.await.map_err(PasteQueueError::Failed) },
                    aborted,
                )
                .await
            };
            if signal.is_aborted() {
                return Err(PasteQueueError::Aborted);
            }
            drop(settle);
            applied
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::future::{block_on, zip};

    type R<T> = Result<T, String>;

    fn gate() -> (async_channel::Sender<()>, async_channel::Receiver<()>) {
        async_channel::bounded(1)
    }

    #[test]
    fn waits_for_text_paste_confirmation_before_completing() {
        let (confirm_tx, confirm_rx) = gate();
        let seen = Arc::new(Mutex::new(None));
        let record = seen.clone();
        let operation = paste_terminal_clipboard(
            ClipboardPayload::Text("one\ntwo".into()),
            |_| async { R::Ok(String::new()) },
            move |text| async move {
                *record.lock().unwrap() = Some(text);
                let _ = confirm_rx.recv().await;
                R::Ok(())
            },
            |_| async { R::Ok(()) },
        );
        let confirm = async {
            futures_lite::future::yield_now().await;
            assert_eq!(seen.lock().unwrap().as_deref(), Some("one\ntwo"));
            confirm_tx.close();
        };
        let (result, ()) = block_on(zip(operation, confirm));
        assert_eq!(result, Ok(ClipboardPasteResult::PastedText));
    }

    #[test]
    fn reports_captured_unavailable_clipboard_access_without_applying_input() {
        let result = block_on(paste_terminal_clipboard(
            ClipboardPayload::Unavailable,
            |_| async { R::Ok(String::new()) },
            |_| async { panic!("no text paste") },
            |_| async { panic!("no image paste") },
        ));
        assert_eq!(result, Ok(ClipboardPasteResult::SkippedUnavailable));
    }

    fn target<'a>(
        stored: &'a TerminalImageLocation,
        kind: &'a str,
        connection: Option<&'a str>,
        ready: bool,
        open: bool,
    ) -> TerminalImagePasteTarget<'a> {
        TerminalImagePasteTarget {
            stored: Some(stored),
            kind: Some(kind),
            connection_id: connection,
            input_ready: ready,
            transport_open: open,
        }
    }

    #[test]
    fn accepts_an_input_ready_ssh_target_before_its_display_status_settles() {
        let stored = TerminalImageLocation::Ssh {
            connection_id: "connection-1".into(),
        };
        assert!(is_current_terminal_image_paste_target(&target(
            &stored,
            "ssh",
            Some("connection-1"),
            true,
            true
        )));
        assert!(!is_current_terminal_image_paste_target(&target(
            &stored,
            "ssh",
            Some("connection-1"),
            false,
            true
        )));
    }

    #[test]
    fn only_pastes_a_remote_image_path_into_the_connection_that_received_it() {
        let stored = TerminalImageLocation::Ssh {
            connection_id: "connection-1".into(),
        };
        assert!(!is_current_terminal_image_paste_target(&target(
            &stored,
            "ssh",
            Some("connection-2"),
            true,
            true
        )));
    }

    #[test]
    fn pastes_a_local_image_path_into_any_live_local_session() {
        let stored = TerminalImageLocation::Local;
        assert!(is_current_terminal_image_paste_target(&target(
            &stored, "local", None, true, true
        )));
        assert!(!is_current_terminal_image_paste_target(&target(
            &stored, "local", None, true, false
        )));
        assert!(!is_current_terminal_image_paste_target(&target(
            &stored,
            "ssh",
            Some("connection-1"),
            true,
            true
        )));
    }

    #[test]
    fn pastes_an_uploaded_image_path_through_the_image_callback() {
        let pasted = Arc::new(Mutex::new(None));
        let record = pasted.clone();
        let result = block_on(paste_terminal_clipboard(
            ClipboardPayload::Image(vec![1]),
            |_| async { R::Ok("/tmp/muxus-paste.png".to_string()) },
            |_| async { panic!("no text paste") },
            move |path| async move {
                *record.lock().unwrap() = Some(path);
                R::Ok(())
            },
        ));
        assert_eq!(result, Ok(ClipboardPasteResult::PastedImagePath));
        assert_eq!(
            pasted.lock().unwrap().as_deref(),
            Some("/tmp/muxus-paste.png")
        );
    }

    #[test]
    fn does_not_hide_image_upload_failures() {
        let result = block_on(paste_terminal_clipboard(
            ClipboardPayload::Image(vec![1]),
            |_| async { R::<String>::Err("SFTP is disabled".into()) },
            |_| async { R::Ok(()) },
            |_| async { R::Ok(()) },
        ));
        assert_eq!(result, Err("SFTP is disabled".to_string()));
    }

    fn text(
        value: &str,
    ) -> impl FnOnce(AbortSignal) -> std::future::Ready<R<ClipboardPayload>> + use<> {
        let value = value.to_string();
        move |_| std::future::ready(Ok(ClipboardPayload::Text(value)))
    }

    #[test]
    fn applies_captured_clipboard_payloads_in_invocation_order() {
        let queue = TerminalClipboardPasteQueue::default();
        let applied = Arc::new(Mutex::new(Vec::<String>::new()));
        let (gate_tx, gate_rx) = gate();
        let log = applied.clone();
        let first = queue.enqueue(text("first"), move |payload, _| async move {
            let ClipboardPayload::Text(text) = payload else {
                panic!()
            };
            log.lock().unwrap().push(format!("start:{text}"));
            let _ = gate_rx.recv().await;
            log.lock().unwrap().push(format!("finish:{text}"));
            R::Ok(())
        });
        let log = applied.clone();
        let second = queue.enqueue(text("second"), move |payload, _| async move {
            let ClipboardPayload::Text(text) = payload else {
                panic!()
            };
            log.lock().unwrap().push(format!("start:{text}"));
            log.lock().unwrap().push(format!("finish:{text}"));
            R::Ok(())
        });
        let check = applied.clone();
        let release = async move {
            for _ in 0..3 {
                futures_lite::future::yield_now().await;
            }
            assert_eq!(*check.lock().unwrap(), ["start:first"]);
            gate_tx.close();
        };
        let ((a, b), ()) = block_on(zip(zip(first, second), release));
        assert_eq!((a, b), (Ok(()), Ok(())));
        assert_eq!(
            *applied.lock().unwrap(),
            [
                "start:first",
                "finish:first",
                "start:second",
                "finish:second"
            ]
        );
    }

    #[test]
    fn rejects_image_payloads_beyond_the_pending_byte_budget() {
        let queue = TerminalClipboardPasteQueue::new(PasteQueueLimits {
            max_pending_image_bytes: 4,
            max_pending_pastes: 2,
        });
        let (gate_tx, gate_rx) = gate();
        let first = queue.enqueue(
            |_| async { R::Ok(ClipboardPayload::Image(vec![0; 3])) },
            move |_, _| async move {
                let _ = gate_rx.recv().await;
                R::Ok(())
            },
        );
        let second = queue.enqueue(
            |_| async { R::Ok(ClipboardPayload::Image(vec![0; 3])) },
            |_, _| async { R::Ok(()) },
        );
        let release = async move {
            futures_lite::future::yield_now().await;
            gate_tx.close();
        };
        let ((a, b), ()) = block_on(zip(zip(first, second), release));
        assert_eq!(a, Ok(()));
        assert_eq!(b, Err(PasteQueueError::TooMuchImageData));
    }

    #[test]
    fn rejects_clipboard_input_beyond_the_pending_operation_limit() {
        let queue = TerminalClipboardPasteQueue::new(PasteQueueLimits {
            max_pending_image_bytes: 4,
            max_pending_pastes: 1,
        });
        let (gate_tx, gate_rx) = gate();
        let first = queue.enqueue(text("first"), move |_, _| async move {
            let _ = gate_rx.recv().await;
            R::Ok(())
        });
        let captured = Arc::new(Mutex::new(false));
        let flag = captured.clone();
        let rejected = queue.enqueue(
            move |_| {
                *flag.lock().unwrap() = true;
                async { R::Ok(ClipboardPayload::Text("second".into())) }
            },
            |_, _| async { R::Ok(()) },
        );
        assert_eq!(block_on(rejected), Err(PasteQueueError::TooManyPastes));
        assert!(!*captured.lock().unwrap());
        gate_tx.close();
        assert_eq!(block_on(first), Ok(()));
    }

    #[test]
    fn aborts_an_active_clipboard_operation_when_the_queue_is_cancelled() {
        let queue = TerminalClipboardPasteQueue::default();
        let (started_tx, started_rx) = gate();
        let operation = queue.enqueue(text("waiting"), move |_, signal| async move {
            started_tx.close();
            signal.aborted().await;
            R::<()>::Err("aborted".into())
        });
        let cancel = {
            let queue = queue.clone();
            async move {
                let _ = started_rx.recv().await;
                queue.cancel_all();
            }
        };
        let (result, ()) = block_on(zip(operation, cancel));
        assert_eq!(result, Err(PasteQueueError::Aborted));
    }

    #[test]
    fn prioritizes_cancellation_over_a_late_clipboard_capture_error() {
        let queue = TerminalClipboardPasteQueue::default();
        let (capture_tx, capture_rx) = gate();
        let operation = queue.enqueue(
            move |_| async move {
                let _ = capture_rx.recv().await;
                R::<ClipboardPayload>::Err("clipboard permission denied".into())
            },
            |_, _| async { R::Ok(()) },
        );
        let cancel = {
            let queue = queue.clone();
            async move {
                futures_lite::future::yield_now().await;
                queue.cancel_all();
                capture_tx.close();
            }
        };
        let (result, ()) = block_on(zip(operation, cancel));
        assert_eq!(result, Err(PasteQueueError::Aborted));
    }
}
