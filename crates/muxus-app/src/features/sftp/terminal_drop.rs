//! `client/src/components/TerminalFileDrop.tsx`: files dropped from outside
//! the window onto a terminal are uploaded into the SSH shell's directory, or
//! typed as shell-quoted paths. The rules are `muxus_term::file_drop` and
//! `muxus_term::drop_paths`; this is the overlay shown while dragging and
//! what a drop does.
//!
//! The terminal view hooks it up: give its frame a group name, draw
//! [`terminal_drop_overlay`] inside it, and call [`handle_terminal_drop`]
//! from `on_drop::<ExternalPaths>`.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use muxus_session::ApiError;
use muxus_ssh::sftp::CancelSignal;
use muxus_ssh::sftp::upload::{DropPayload, UploadProgress, UploadResult, collect_drop};
use muxus_term::drop_paths::{DroppedPathStyle, dropped_paths_text};
use muxus_term::file_drop::{
    DropSessionStatus, TerminalDropContext, TerminalDropIntent, drop_overlay_text,
    terminal_drop_intent,
};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, App, AsyncApp, ExternalPaths, InteractiveElement, IntoElement, ParentElement,
    SharedString, Styled, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status};

use super::services::{SftpServices, TerminalUpload, services};
use super::terminal_upload::{
    LocalFuture, SETTLE, TerminalUploadEnv, TerminalUploadRequest, start_terminal_upload,
};

/// What the drop needs to know about the terminal's tab.
#[derive(Clone, Debug)]
pub struct TerminalDropTab {
    /// `ssh`, `local`, `telnet`, `serial`, `rdp`, `vnc`.
    pub profile_kind: String,
    pub status: DropSessionStatus,
    pub conn_id: Option<String>,
    pub sftp_available: Option<bool>,
    /// The directory the shell last reported.
    pub cwd: Option<String>,
    /// The tab's title, to say where the files go.
    pub title: String,
    /// How a local shell reads quoted paths (`localShellPathStyle`); SSH shells are POSIX.
    pub path_style: DroppedPathStyle,
}

/// The drop's intent while `shift` is (or is not) held.
pub fn drop_intent(tab: &TerminalDropTab, shift: bool) -> TerminalDropIntent {
    terminal_drop_intent(&TerminalDropContext {
        profile_kind: tab.profile_kind.clone(),
        status: tab.status,
        conn_id: tab.conn_id.clone(),
        sftp_available: tab.sftp_available,
        cwd: tab.cwd.clone(),
        home: None,
        insert_modifier: shift,
        // The native app always knows a dropped file's local path.
        can_insert_paths: true,
    })
}

/// `TerminalDropOverlay`: dashed frame, icon, title and caption, inset
/// 10px 12px 12px. Hidden until files are dragged over the frame `group`.
pub fn terminal_drop_overlay(
    group: impl Into<SharedString>,
    intent: &TerminalDropIntent,
    cx: &App,
) -> Option<AnyElement> {
    let (title, caption) = drop_overlay_text(intent)?;
    let theme = cx.theme();
    let p = theme.palette;
    let unavailable = matches!(intent, TerminalDropIntent::Unavailable { .. });
    let glyph = match intent {
        TerminalDropIntent::Upload { .. } => "UploadFileOutlined",
        TerminalDropIntent::Insert { .. } => "TerminalOutlined",
        _ => "FolderOffOutlined",
    };
    let accent = if unavailable {
        p.text.disabled
    } else {
        p.primary.main
    };
    let subtitle2 = theme.typography.subtitle2;
    let caption_type = theme.typography.caption;
    let mut column = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .px(px(24.))
        .child(icon(glyph, 38., accent))
        .child(
            paragraph(title)
                .size(subtitle2.size)
                .weight(subtitle2.weight)
                .line_height(subtitle2.line_height)
                .color(p.text.primary)
                .center(),
        );
    if let Some(caption) = caption {
        column = column.child(
            label(caption)
                .size(caption_type.size)
                .line_height(caption_type.line_height)
                .color(p.text.secondary)
                .no_truncate(),
        );
    }
    Some(
        div()
            .absolute()
            .top(px(10.))
            .left(px(12.))
            .right(px(12.))
            .bottom(px(12.))
            .flex()
            .items_center()
            .justify_center()
            .border_2()
            .border_dashed()
            .border_color(accent)
            .rounded(px(16.))
            .bg(alpha(p.background_paper, 0.9))
            .invisible()
            .group_drag_over::<ExternalPaths>(group, |s| s.visible())
            .child(column)
            .into_any_element(),
    )
}

/// What a drop onto the terminal does: type the quoted paths through
/// `insert`, or upload the files over the session's connection.
pub fn handle_terminal_drop(
    paths: &ExternalPaths,
    tab: &TerminalDropTab,
    insert: impl FnOnce(String, &mut Window, &mut App),
    window: &mut Window,
    cx: &mut App,
) {
    let shift = window.modifiers().shift;
    match drop_intent(tab, shift) {
        TerminalDropIntent::Insert { .. } => {
            let paths: Vec<String> = paths
                .paths()
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            if paths.is_empty() {
                super::services::notify(
                    Status::Warning,
                    "The dropped items have no local paths to insert.",
                    cx,
                );
                return;
            }
            let words: Vec<&str> = paths.iter().map(String::as_str).collect();
            insert(dropped_paths_text(&words, &tab.path_style), window, cx);
        }
        TerminalDropIntent::Upload { directory, .. } => {
            let Some(conn_id) = tab.conn_id.clone() else {
                return;
            };
            upload_drop(
                conn_id,
                Some(tab.title.clone()),
                directory,
                paths.paths().to_vec(),
                cx,
            );
        }
        _ => {}
    }
}

/// Start the upload of dropped local files into `directory` (the home
/// folder when `None`) on the connection.
pub fn upload_drop(
    conn_id: String,
    host: Option<String>,
    directory: Option<String>,
    paths: Vec<PathBuf>,
    cx: &mut App,
) {
    let backend = services(cx).backend;
    let collect = cx
        .background_executor()
        .spawn(async move { collect_drop(&paths) });
    let home_conn = conn_id.clone();
    let prepare: LocalFuture<Result<(String, DropPayload), ApiError>> = Box::pin(async move {
        let payload = collect
            .await
            .map_err(|error| ApiError::new(0, error.to_string()))?;
        let directory = match directory {
            Some(directory) => directory,
            None => backend.sftp_home(&home_conn).await?.path,
        };
        Ok((directory, payload))
    });
    let request = TerminalUploadRequest {
        conn_id,
        host,
        prepare,
        settle: SETTLE,
    };
    cx.spawn(async move |cx| {
        let env = Rc::new(AppUploads { cx: cx.clone() });
        start_terminal_upload(request, env).await;
    })
    .detach();
}

/// [`TerminalUploadEnv`] on the app's services.
struct AppUploads {
    cx: AsyncApp,
}

impl TerminalUploadEnv for AppUploads {
    fn current(&self, conn_id: &str) -> Option<TerminalUpload> {
        self.cx
            .update(|cx| services(cx).uploads.read(cx).get(conn_id).cloned())
    }

    fn show(&self, upload: TerminalUpload) {
        self.cx.update(|cx| {
            services(cx).uploads.update(cx, |uploads, cx| {
                uploads.show(upload);
                cx.notify();
            })
        });
    }

    fn clear(&self, conn_id: &str, id: u64) {
        self.cx.update(|cx| {
            services(cx).uploads.update(cx, |uploads, cx| {
                uploads.clear(conn_id, id);
                cx.notify();
            })
        });
    }

    fn clear_after(&self, conn_id: &str, id: u64, delay: Duration) {
        let conn = conn_id.to_string();
        self.cx.update(|cx| {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(delay).await;
                cx.update(|cx| {
                    services(cx).uploads.update(cx, |uploads, cx| {
                        uploads.clear(&conn, id);
                        cx.notify();
                    })
                });
            })
            .detach();
        });
    }

    fn notify(&self, status: Status, message: String, details: Option<String>) {
        self.cx.update(|cx| {
            let notify = cx.global::<SftpServices>().notify.clone();
            notify(status, message.into(), details.map(Into::into), cx);
        });
    }

    fn refresh(&self, conn_id: &str) {
        let conn = conn_id.to_string();
        self.cx.update(|cx| {
            services(cx)
                .listings
                .update(cx, |listings, cx| listings.refresh(&conn, cx))
        });
    }

    fn upload(
        &self,
        conn_id: &str,
        directory: &str,
        payload: DropPayload,
        cancel: CancelSignal,
        mut on_progress: Box<dyn FnMut(&UploadProgress)>,
    ) -> LocalFuture<Result<UploadResult, ApiError>> {
        let backend = self.cx.update(|cx| services(cx).backend);
        let (conn_id, directory) = (conn_id.to_string(), directory.to_string());
        let (progress_tx, progress_rx) = async_channel::unbounded::<UploadProgress>();
        let cx = self.cx.clone();
        Box::pin(async move {
            let forward = cx.spawn(async move |_| {
                while let Ok(progress) = progress_rx.recv().await {
                    on_progress(&progress);
                }
            });
            // Replacing a file asks before the upload goes on, as the browser does.
            let ask_cx = cx.clone();
            let (ask_tx, ask_rx) = async_channel::unbounded::<(
                muxus_ssh::sftp::upload::ConfirmRequest,
                async_channel::Sender<bool>,
            )>();
            let asker = ask_cx.spawn(async move |cx| {
                while let Ok((request, reply)) = ask_rx.recv().await {
                    let answer = cx.update(|cx| {
                        super::local_open::ask(
                            super::dialogs::ConfirmOptions {
                                title: request.title,
                                description: Some(request.description),
                                confirm_label: request.confirm_label,
                                destructive: request.destructive,
                                ..super::dialogs::ConfirmOptions::default()
                            },
                            cx,
                        )
                    });
                    let _ = reply.try_send(answer.await.confirmed);
                }
            });
            let result = backend
                .sftp_upload_drop(
                    &conn_id,
                    &directory,
                    payload,
                    cancel,
                    move |progress| {
                        let _ = progress_tx.try_send(progress.clone());
                    },
                    move |request| {
                        let ask = ask_tx.clone();
                        Box::pin(async move {
                            let (reply, answer) = async_channel::bounded(1);
                            if ask.send((request, reply)).await.is_err() {
                                return false;
                            }
                            answer.recv().await.unwrap_or(false)
                        })
                    },
                )
                .await;
            drop(asker);
            forward.await;
            result
        })
    }
}

/// The bar for a connection's terminal upload, for the corner of the window
/// when no file browser shows the connection.
pub fn terminal_upload_corner(conn_id: &str, cx: &App) -> Option<AnyElement> {
    // Windows without the backend (the documentation scenes) have no uploads.
    let uploads = cx.try_global::<SftpServices>()?.uploads.clone();
    let uploads = uploads.read(cx);
    if uploads.browsers(conn_id) > 0 {
        return None;
    }
    let upload = uploads.get(conn_id)?.clone();
    let conn = conn_id.to_string();
    let mut bar = super::transfer::TransferProgress::new(
        SharedString::from(format!("terminal-upload-{conn_id}")),
        upload.transfer.clone(),
    )
    .on_cancel(move |_, _, cx| {
        let conn = conn.clone();
        services(cx).uploads.update(cx, |uploads, cx| {
            uploads.cancel(&conn);
            cx.notify();
        });
    });
    if let Some(host) = &upload.host {
        bar = bar.host(host.clone());
    }
    let paper = cx.theme().palette.background_paper;
    Some(
        div()
            .w(px(320.))
            .bg(paper)
            .rounded(px(8.))
            .child(bar)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use muxus_term::drop_paths::ShellQuoting;

    use super::*;

    fn tab(kind: &str) -> TerminalDropTab {
        TerminalDropTab {
            profile_kind: kind.into(),
            status: DropSessionStatus::Connected,
            conn_id: Some("conn-1".into()),
            sftp_available: Some(true),
            cwd: Some("/srv/app".into()),
            title: "web-01".into(),
            path_style: DroppedPathStyle::new(ShellQuoting::Posix),
        }
    }

    #[test]
    fn uploads_to_the_shell_directory_and_inserts_with_shift() {
        assert_eq!(
            drop_intent(&tab("ssh"), false),
            TerminalDropIntent::Upload {
                directory: Some("/srv/app".into()),
                hint: Some(muxus_term::file_drop::INSERT_PATHS_HINT.into()),
            }
        );
        assert_eq!(
            drop_intent(&tab("ssh"), true),
            TerminalDropIntent::Insert { by_modifier: true }
        );
        assert_eq!(
            drop_intent(&tab("local"), false),
            TerminalDropIntent::Insert { by_modifier: false }
        );
        assert_eq!(
            drop_intent(&tab("telnet"), true),
            TerminalDropIntent::Refuse
        );
    }
}
