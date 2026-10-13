//! One transfer as the progress bar shows it (`client/src/transfer-progress.ts`)
//! and the bar itself (`components/TransferProgress.tsx`): name, progress,
//! bytes and speed, with Cancel while it can still stop.

use std::time::Duration;

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
