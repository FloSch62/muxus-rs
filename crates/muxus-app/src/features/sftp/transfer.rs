//! One transfer as the progress bar shows it (`client/src/transfer-progress.ts`)
//! and the bar itself (`components/TransferProgress.tsx`): name, progress,
//! bytes and speed, with Cancel while it can still stop.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use muxus_session::ApiError;
use muxus_ssh::sftp::CancelSignal;
use muxus_ssh::sftp::upload::UploadProgress;
// The `IntoElement` derive names its paths `gpui::`.
pub use muxus_ssh::sftp::upload::TransferPhase;
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::gpui;
use muxus_ui::gpui_kit::{
    Animation, AnimationExt, App, ClickEvent, ElementId, InteractiveElement, IntoElement,
    MouseButton, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled,
    Window, div, px, relative,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Theme};

use super::format::{format_size, format_speed, to_fixed};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferDirection {
    Upload,
    Download,
}

/// `TransferProgressState`.
#[derive(Clone, Debug, PartialEq)]
pub struct TransferProgressState {
    pub direction: TransferDirection,
    pub name: String,
    pub loaded: u64,
    pub total: Option<u64>,
    pub bytes_per_second: f64,
    pub phase: TransferPhase,
    pub file_index: Option<usize>,
    pub file_count: Option<usize>,
}

impl TransferProgressState {
    /// A single file about to move.
    pub fn starting(
        direction: TransferDirection,
        name: impl Into<String>,
        total: Option<u64>,
    ) -> Self {
        Self {
            direction,
            name: name.into(),
            loaded: 0,
            total,
            bytes_per_second: 0.,
            phase: TransferPhase::Preparing,
            file_index: Some(1),
            file_count: Some(1),
        }
    }

    /// The bar's state for an upload's progress report.
    pub fn from_upload(progress: &UploadProgress) -> Self {
        Self {
            direction: TransferDirection::Upload,
            name: progress.name.clone(),
            loaded: progress.loaded,
            total: progress.total,
            bytes_per_second: progress.bytes_per_second,
            phase: progress.phase,
            file_index: Some(progress.file_index),
            file_count: Some(progress.file_count),
        }
    }

    /// Done so far, 0–100; `None` while the size is unknown.
    pub fn percent(&self) -> Option<f64> {
        if self.phase == TransferPhase::Complete {
            return Some(100.);
        }
        match self.total {
            Some(total) if total > 0 => Some((self.loaded as f64 / total as f64 * 100.).min(100.)),
            _ => None,
        }
    }

    /// Cancel is offered until the transfer finishes or is already stopping.
    pub fn cancellable(&self) -> bool {
        !matches!(
            self.phase,
            TransferPhase::Complete | TransferPhase::Finalizing | TransferPhase::Cancelling
        )
    }

    /// The bar's title; `host` names the remote end instead of "remote".
    pub fn label(&self, host: Option<&str>) -> String {
        let name = &self.name;
        let on_host = host.map_or_else(|| " on remote".to_string(), |host| format!(" on {host}"));
        let to_host = host.map_or_else(String::new, |host| format!(" to {host}"));
        let upload = self.direction == TransferDirection::Upload;
        match self.phase {
            TransferPhase::Complete if upload => format!("Uploaded {name}{to_host}"),
            TransferPhase::Complete => format!("Downloaded {name}"),
            TransferPhase::Cancelling => format!("Cancelling {name}…"),
            TransferPhase::Finalizing => format!("Finishing {name}{on_host}…"),
            TransferPhase::Preparing => format!("Preparing {name}…"),
            TransferPhase::Transferring if upload => format!("Uploading {name}{to_host}"),
            TransferPhase::Transferring => format!("Downloading {name}"),
        }
    }

    /// The bytes line: `1.0 KiB / 2.0 KiB · 512 B/s`.
    pub fn bytes_line(&self) -> String {
        let mut text = format_size(Some(self.loaded));
        if self.total.is_some() {
            text.push_str(&format!(" / {}", format_size(self.total)));
        }
        if self.phase == TransferPhase::Transferring {
            text.push_str(&format!(" · {}", format_speed(self.bytes_per_second)));
        }
        text
    }

    /// `File 2 of 5`, shown for more than one file.
    pub fn file_line(&self) -> Option<String> {
        let count = self.file_count.unwrap_or(1);
        (count > 1).then(|| format!("File {} of {count}", self.file_index.unwrap_or(0)))
    }
}

/// Why a transfer was stopped before its end (the reason its `AbortSignal` carries).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The progress bar's Cancel: only it reads as cancelled (`TRANSFER_CANCELLED`).
    Cancelled,
    /// The transfer's session lost the connection (`CONNECTION_LOST`).
    ConnectionLost,
    /// Any other reason, as when its file browser closes.
    Closed,
}

/// The code of the error a stopped download ends with (`AbortError`); a stopped upload ends
/// with [`UPLOAD_CANCELLED`](super::terminal_upload::UPLOAD_CANCELLED).
pub const TRANSFER_ABORTED: &str = "aborted";

/// A transfer's stop request with its reason (the `AbortController` of the TypeScript client).
/// The first reason given stays.
#[derive(Clone, Debug, Default)]
pub struct TransferStop {
    signal: CancelSignal,
    reason: Rc<Cell<Option<StopReason>>>,
}

impl TransferStop {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stop(&self, reason: StopReason) {
        if self.reason.get().is_none() {
            self.reason.set(Some(reason));
            self.signal.cancel();
        }
    }

    pub fn reason(&self) -> Option<StopReason> {
        self.reason.get()
    }

    pub fn is_stopped(&self) -> bool {
        self.reason.get().is_some()
    }

    /// The signal the backend's transfers check.
    pub fn signal(&self) -> &CancelSignal {
        &self.signal
    }

    /// Completes once the transfer was stopped.
    pub async fn stopped(&self) {
        self.signal.cancelled().await
    }

    /// The error a stopped transfer ends with (`signal.throwIfAborted()`).
    pub fn check(&self) -> Result<(), ApiError> {
        match self.reason.get() {
            Some(reason) => Err(abort_error(reason)),
            None => Ok(()),
        }
    }
}

/// What a transfer stopped for `reason` fails with.
pub fn abort_error(reason: StopReason) -> ApiError {
    let message = match reason {
        StopReason::ConnectionLost => "Connection lost",
        StopReason::Cancelled | StopReason::Closed => "Transfer cancelled",
    };
    ApiError::with_code(499, message, TRANSFER_ABORTED)
}

/// Whether `error` only says that the transfer was stopped (`isAbortError`).
pub fn is_abort_error(error: &ApiError) -> bool {
    error.status == 499
        && matches!(
            error.code.as_deref(),
            Some(TRANSFER_ABORTED | super::terminal_upload::UPLOAD_CANCELLED)
        )
}

/// What to tell the user about a transfer that stopped with an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoppedTransfer {
    /// Cancelled after Cancel: an information.
    Cancelled(String),
    /// The session lost its connection: an error.
    ConnectionLost(String),
    /// The failure itself.
    Failed,
    /// Stopped for another reason, as when its file browser closes: nothing.
    Silent,
}

/// The messages of one kind of transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopMessages {
    pub cancelled: String,
    pub connection_lost: String,
}

/// Say why a transfer stopped with `error`: cancelled after Cancel, the lost connection, or
/// the failure itself. Stopped for any other reason, as when its file browser closes, it
/// stops without a message (`reportStoppedTransfer`).
pub fn stopped_transfer(
    error: &ApiError,
    stop: &TransferStop,
    messages: StopMessages,
) -> StoppedTransfer {
    match stop.reason() {
        Some(StopReason::Cancelled) => StoppedTransfer::Cancelled(messages.cancelled),
        Some(StopReason::ConnectionLost) => {
            StoppedTransfer::ConnectionLost(messages.connection_lost)
        }
        Some(StopReason::Closed) if is_abort_error(error) => StoppedTransfer::Silent,
        _ => StoppedTransfer::Failed,
    }
}

/// `Math.round(percent)` as the bar prints it.
fn percent_text(percent: Option<f64>) -> String {
    match percent {
        Some(value) => format!("{}%", to_fixed((value + 0.5).floor(), 0)),
        None => "—".into(),
    }
}

type CancelHandler = std::rc::Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// `<TransferProgress transfer host onCancel />`.
#[derive(IntoElement)]
pub struct TransferProgress {
    id: ElementId,
    transfer: TransferProgressState,
    host: Option<SharedString>,
    on_cancel: Option<CancelHandler>,
}

impl TransferProgress {
    pub fn new(id: impl Into<ElementId>, transfer: TransferProgressState) -> Self {
        Self {
            id: id.into(),
            transfer,
            host: None,
            on_cancel: None,
        }
    }

    pub fn host(mut self, host: impl Into<SharedString>) -> Self {
        self.host = Some(host.into());
        self
    }

    pub fn on_cancel(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_cancel = Some(std::rc::Rc::new(handler));
        self
    }
}

/// `<Button color="error" sx={{ minWidth: 0, px: 0.75, py: 0.1 }}>Cancel</Button>`.
fn cancel_button(id: ElementId, theme: &Theme, on_click: CancelHandler) -> impl IntoElement {
    let ink = theme.palette.error.main;
    let size = theme.typography.px_to_rem(13.);
    let hover = alpha(ink, theme.palette.action.hover_opacity);
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .px(px(6.))
        .py(px(0.8))
        .rounded(px(7.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| on_click(event, window, cx))
        .child(
            label("Cancel")
                .size(size)
                .weight(theme.typography.button.weight)
                .line_height(size * 1.75)
                .color(ink)
                .no_truncate(),
        )
}

/// A full-width indeterminate `LinearProgress` (the editor's loading bar).
pub fn indeterminate_bar(theme: &Theme, id: &'static str) -> impl IntoElement {
    div().flex_none().w(relative(1.)).child(indeterminate(
        theme,
        theme.palette.primary.main,
        id.into(),
    ))
}

/// MUI's indeterminate `LinearProgress`: two bars sweeping across.
fn indeterminate(
    theme: &Theme,
    color: muxus_ui::gpui_kit::Hsla,
    id: ElementId,
) -> impl IntoElement {
    let track = alpha(theme.palette.text.secondary, 0.15);
    div()
        .relative()
        .h(px(4.))
        .w(relative(1.))
        .rounded(px(3.))
        .overflow_hidden()
        .bg(track)
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .bg(color)
                .with_animation(
                    id,
                    Animation::new(Duration::from_millis(2100)).repeat(),
                    |bar, delta| {
                        // keyframes 0%: left -35% right 100%; 60%: left 100% right -90%.
                        let t = (delta / 0.6).min(1.);
                        let left = -0.35 + 1.35 * t;
                        let right = 1. - 1.9 * t;
                        let width = (1. - right - left).max(0.);
                        bar.left(relative(left)).w(relative(width))
                    },
                ),
        )
}

impl RenderOnce for TransferProgress {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let caption = theme.typography.caption;
        let transfer = &self.transfer;
        let complete = transfer.phase == TransferPhase::Complete;
        let percent = transfer.percent();
        let title = transfer.label(self.host.as_deref());
        let accent = if complete {
            p.success.main
        } else {
            p.primary.main
        };
        let icon_name = match transfer.direction {
            TransferDirection::Upload => "UploadFileOutlined",
            TransferDirection::Download => "DownloadOutlined",
        };
        let caption_label = |text: String| {
            label(text)
                .size(caption.size)
                .line_height(caption.line_height)
                .tabular_nums()
        };
        let mut head = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .mb(px(4.8))
            .child(icon(icon_name, 17., accent))
            .child(
                caption_label(title)
                    .weight(600.)
                    .color(p.text.primary)
                    .grow(),
            )
            .child(caption_label(percent_text(percent)).color(p.text.secondary));
        if let Some(cancel) = self.on_cancel.clone()
            && transfer.cancellable()
        {
            head = head.child(cancel_button(
                ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "cancel".into()),
                &theme,
                cancel,
            ));
        }
        let bar = match percent {
            Some(value) => div()
                .h(px(4.))
                .w(relative(1.))
                .rounded(px(3.))
                .overflow_hidden()
                .bg(alpha(p.text.secondary, 0.15))
                .child(
                    div()
                        .h(relative(1.))
                        .w(relative((value / 100.) as f32))
                        .bg(accent),
                )
                .into_any_element(),
            None => indeterminate(
                &theme,
                accent,
                ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "bar".into()),
            )
            .into_any_element(),
        };
        let mut foot = div()
            .flex()
            .justify_between()
            .gap(px(8.))
            .mt(px(4.4))
            .child(caption_label(transfer.bytes_line()).color(p.text.secondary));
        if let Some(files) = transfer.file_line() {
            foot = foot.child(caption_label(files).color(p.text.secondary));
        }
        div()
            .flex()
            .flex_col()
            .p(px(8.))
            .border_1()
            .rounded(px(8.))
            .border_color(if complete {
                alpha(p.success.main, 0.45)
            } else {
                p.divider
            })
            .bg(if complete {
                alpha(p.success.main, 0.07)
            } else {
                alpha(p.primary.main, 0.04)
            })
            .child(head)
            .child(bar)
            .child(foot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(phase: TransferPhase, direction: TransferDirection) -> TransferProgressState {
        TransferProgressState {
            direction,
            name: "a.txt".into(),
            loaded: 512,
            total: Some(2048),
            bytes_per_second: 1024.,
            phase,
            file_index: Some(2),
            file_count: Some(3),
        }
    }

    fn messages() -> StopMessages {
        StopMessages {
            cancelled: "Upload cancelled".into(),
            connection_lost: "Upload stopped: connection lost".into(),
        }
    }

    fn stopped_for(reason: Option<StopReason>) -> TransferStop {
        let stop = TransferStop::new();
        if let Some(reason) = reason {
            stop.stop(reason);
        }
        stop
    }

    /// What a stopped upload fails with, whatever the reason.
    fn aborted() -> ApiError {
        ApiError::with_code(
            499,
            "Upload cancelled",
            super::super::terminal_upload::UPLOAD_CANCELLED,
        )
    }

    #[test]
    fn reads_as_cancelled_only_after_cancel() {
        assert_eq!(
            stopped_transfer(
                &aborted(),
                &stopped_for(Some(StopReason::Cancelled)),
                messages()
            ),
            StoppedTransfer::Cancelled("Upload cancelled".into())
        );
    }

    #[test]
    fn says_nothing_when_its_file_browser_closed() {
        assert_eq!(
            stopped_transfer(
                &aborted(),
                &stopped_for(Some(StopReason::Closed)),
                messages()
            ),
            StoppedTransfer::Silent
        );
        assert_eq!(
            stopped_transfer(
                &abort_error(StopReason::Closed),
                &stopped_for(Some(StopReason::Closed)),
                messages()
            ),
            StoppedTransfer::Silent
        );
    }

    #[test]
    fn says_so_when_the_connection_was_lost() {
        assert_eq!(
            stopped_transfer(
                &aborted(),
                &stopped_for(Some(StopReason::ConnectionLost)),
                messages()
            ),
            StoppedTransfer::ConnectionLost("Upload stopped: connection lost".into())
        );
    }

    #[test]
    fn shows_a_failure_also_one_that_arrives_as_the_browser_closes() {
        let denied = ApiError::internal("permission denied");
        assert_eq!(
            stopped_transfer(&denied, &stopped_for(None), messages()),
            StoppedTransfer::Failed
        );
        let no_program = ApiError::internal("no program opens this file");
        assert_eq!(
            stopped_transfer(
                &no_program,
                &stopped_for(Some(StopReason::Closed)),
                messages()
            ),
            StoppedTransfer::Failed
        );
        // An abort the transfer did not ask for is a failure too.
        assert_eq!(
            stopped_transfer(&aborted(), &stopped_for(None), messages()),
            StoppedTransfer::Failed
        );
    }

    #[test]
    fn a_stop_keeps_its_first_reason_and_fails_what_checks_it() {
        let stop = TransferStop::new();
        assert!(!stop.is_stopped());
        assert!(stop.check().is_ok());
        assert!(!stop.signal().is_cancelled());

        stop.stop(StopReason::ConnectionLost);
        stop.stop(StopReason::Cancelled);

        assert_eq!(stop.reason(), Some(StopReason::ConnectionLost));
        assert!(stop.signal().is_cancelled());
        let error = stop.check().unwrap_err();
        assert_eq!(error.message, "Connection lost");
        assert!(is_abort_error(&error));
        assert!(!is_abort_error(&ApiError::internal("Connection lost")));
    }

    #[test]
    fn labels_each_phase() {
        use TransferDirection::*;
        use TransferPhase::*;
        assert_eq!(
            state(Complete, Upload).label(Some("web-01")),
            "Uploaded a.txt to web-01"
        );
        assert_eq!(state(Complete, Download).label(None), "Downloaded a.txt");
        assert_eq!(state(Cancelling, Upload).label(None), "Cancelling a.txt…");
        assert_eq!(
            state(Finalizing, Upload).label(None),
            "Finishing a.txt on remote…"
        );
        assert_eq!(
            state(Finalizing, Upload).label(Some("db")),
            "Finishing a.txt on db…"
        );
        assert_eq!(state(Preparing, Download).label(None), "Preparing a.txt…");
        assert_eq!(state(Transferring, Upload).label(None), "Uploading a.txt");
        assert_eq!(
            state(Transferring, Download).label(Some("x")),
            "Downloading a.txt"
        );
    }

    #[test]
    fn shows_bytes_speed_files_and_percent() {
        let transfer = state(TransferPhase::Transferring, TransferDirection::Upload);
        assert_eq!(transfer.bytes_line(), "512 B / 2.0 KiB · 1.0 KiB/s");
        assert_eq!(transfer.file_line().as_deref(), Some("File 2 of 3"));
        assert_eq!(percent_text(transfer.percent()), "25%");
        let unknown = TransferProgressState {
            total: None,
            ..transfer.clone()
        };
        assert_eq!(percent_text(unknown.percent()), "—");
        let done = state(TransferPhase::Complete, TransferDirection::Upload);
        assert_eq!(done.percent(), Some(100.));
        assert!(!done.cancellable());
        assert!(transfer.cancellable());
    }
}
