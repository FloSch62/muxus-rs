//! The device's file system over gNOI File (`tools/FilesTool.tsx`): browse,
//! preview, download, upload and delete, every transfer hash-checked.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use muxus_core::api::SftpEntryType;
use muxus_core::management::{GrpcJson, ManagementError, grpc_methods};
use muxus_mgmt::workbench::connection::ManagementConnection;
use muxus_mgmt::workbench::files::{
    EntryKind, FileEntry, basename, crumbs_of, entry_from_stat, file_system_of, is_top, join_path,
    kind_from_stat, parent_of, trim_separators,
};
use muxus_mgmt::workbench::tool_format::permission_string;
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, Context, ExternalPaths, InteractiveElement, IntoElement, ParentElement,
    PathPromptOptions, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::IconButton;
use serde_json::{Value, json};

use super::super::fields::{EditorKey, FieldKey};
use super::super::logic::editor::Confirmation;
use super::super::logic::tools::device_time;
use super::super::ui::{ConfirmAction, Dialog};
use super::super::view::ManagementView;
use super::super::widgets::MONO;
use super::super::workbench::ListenerForApp;
use super::kit::{body_text, caption_text, error_alert, error_of, message_error};
use super::{ToolAction, section, tool_header};
use crate::features::sftp::format::format_size;
use crate::features::sftp::icons::file_type_icon;
use muxus_ui::widgets::button::Button;
use muxus_ui::widgets::button::ButtonColor;
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::search_field::input_base;
use muxus_ui::widgets::tooltip_lines::TooltipExt;
use muxus_ui::widgets::typography::caps;

/// Files bigger than this are downloaded rather than shown.
const PREVIEW_LIMIT: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub id: u64,
    pub name: String,
    pub upload: bool,
    pub bytes: u64,
    pub total: Option<u64>,
    pub done: bool,
    pub error: Option<String>,
    pub verified: bool,
}

/// What the Files tool shows (React keeps it in the workbench store).
#[derive(Debug, Clone, Default)]
pub struct FilesData {
    /// `None` until browsed: the device's home (`/` or `cf3:\`).
    pub cwd: Option<String>,
    pub entries: Vec<FileEntry>,
    pub loading: bool,
    pub error: Option<ManagementError>,
    pub transfers: Vec<Transfer>,
}

/// The Files tool's component state.
#[derive(Debug, Clone, Default)]
pub struct FilesLocal {
    /// The "Go to path" box; `None` shows the folder.
    pub path_input: Option<String>,
    /// Bumped by every listing, so a stale one does not overwrite a newer one.
    pub listing: u64,
}

/// The editor language a preview is shown in.
fn preview_language(path: &str) -> &'static str {
    match crate::features::sftp::editor::language::language_for_path(path, "") {
        "json" => "json",
        "xml" => "html",
        "python" => "python",
        "shell" => "bash",
        _ => "text",
    }
}

/// Names in the order `localeCompare` puts them (case folded first).
fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| b.name.cmp(&a.name))
    });
}

/// Whether the start of a file says it is binary.
fn looks_binary(data: &[u8]) -> bool {
    data.iter().take(4096).any(|byte| *byte == 0)
}

async fn stat(
    connection: &ManagementConnection,
    path: &str,
) -> Result<Vec<FileEntry>, ManagementError> {
    let mut request = GrpcJson::new();
    request.insert("path".into(), Value::String(path.to_owned()));
    let response = connection
        .grpc_call(grpc_methods::FILE_STAT, request)
        .await
        .map_err(error_of)?;
    Ok(response
        .get("stats")
        .and_then(Value::as_array)
        .map(|stats| {
            stats
                .iter()
                .filter_map(Value::as_object)
                .map(entry_from_stat)
                .collect()
        })
        .unwrap_or_default())
}

impl ManagementView {
    fn files_home(&self) -> String {
        file_system_of(
            self.controller
                .gnmi_info()
                .map(|info| info.models.as_slice()),
        )
        .home
    }

    fn files_cwd(&self) -> String {
        self.ui
            .tools
            .files
            .cwd
            .clone()
            .unwrap_or_else(|| self.files_home())
    }

    pub(super) fn files_mounted(&mut self, cx: &mut Context<Self>) {
        let files = &self.ui.tools.files;
        if files.entries.is_empty() && !files.loading && files.error.is_none() {
            let cwd = self.files_cwd();
            self.open_path(cwd, cx);
        }
    }

    pub(super) fn render_files(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let can_run = self.grpc().is_some();
        let cwd = self.files_cwd();
        let home = self.files_home();
        let path_value = self
            .ui
            .tools
            .local
            .files
            .path_input
            .clone()
            .unwrap_or_else(|| cwd.clone());
        let path_input = self.bound_input(
            FieldKey::Tool("files.path"),
            "Go to path",
            &path_value,
            window,
            cx,
        );
        let files = self.ui.tools.files.clone();

        let up_target = parent_of(&cwd);
        let reload_target = cwd.clone();
        let actions =
            div()
                .flex()
                .gap(px(4.))
                .child(
                    IconButton::new("files-up", "ArrowUpward")
                        .tooltip("Up")
                        .disabled(is_top(&cwd))
                        .on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.open_path(up_target.clone(), cx)
                            }),
                        ),
                )
                .child(
                    IconButton::new("files-reload", "Refresh")
                        .tooltip("Reload")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_path(reload_target.clone(), cx)
                        })),
                )
                .child(
                    Button::new("files-upload", "Upload")
                        .start_icon("FileUploadOutlined")
                        .disabled(!can_run)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| this.pick_uploads(cx),
                        )),
                )
                .into_any_element();

        // Breadcrumbs and the path box.
        let mut crumbs = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(2.))
            .mb(px(8.));
        if cwd.starts_with('/') {
            crumbs = crumbs.child(self.crumb_button("crumb-root", "/".into(), "/".into(), cx));
        }
        let trail = crumbs_of(&cwd);
        let last = trail.len().saturating_sub(1);
        for (index, crumb) in trail.into_iter().enumerate() {
            let mut piece = div().flex().items_center().child(self.crumb_button(
                SharedString::from(format!("crumb-{index}")),
                crumb.label,
                crumb.path,
                cx,
            ));
            if index < last {
                piece = piece.child(
                    body_text("/", p.text.secondary, &theme)
                        .text_size(px(theme.typography.body1.size)),
                );
            }
            crumbs = crumbs.child(piece);
        }
        let path_focused = self.input_focused(&FieldKey::Tool("files.path"), window, cx);
        crumbs = crumbs.child(div().flex_1()).child(
            div()
                .w(px(260.))
                .h(px(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .border_1()
                .border_color(if path_focused {
                    p.primary.main
                } else {
                    p.divider
                })
                .rounded(px(theme.radius))
                .child(input_base(&path_input, 12.5, true, cx)),
        );
        let _ = home;

        let mut body = div().child(crumbs);
        if let Some(error) = &files.error {
            body = body.child(error_alert(
                error,
                &theme,
                Some(std::rc::Rc::new(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.ui.tools.files.error = None;
                        cx.notify();
                    },
                ))),
            ));
        }
        body = body.child(self.file_list(&files, &theme, cx));
        let mut column = div()
            .child(tool_header(
                "FolderOpenOutlined",
                "Files",
                "gnoi.file.File",
                "Browse the device's file system, read files, and move them in and out. Every transfer is checked against the hash the device reports.",
                None,
                &theme,
            ))
            .child(section("Browse", Some(actions), body, &theme));
        if !files.transfers.is_empty() {
            let clear = Button::new("transfers-clear", "Clear")
                .color(ButtonColor::Inherit)
                .on_click(
                    cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                        this.ui
                            .tools
                            .files
                            .transfers
                            .retain(|transfer| !transfer.done);
                        cx.notify();
                    }),
                )
                .into_any_element();
            column = column.child(section(
                "Transfers",
                Some(clear),
                transfers(&files.transfers, &theme),
                &theme,
            ));
        }
        column.into_any_element()
    }

    fn crumb_button(
        &mut self,
        id: impl Into<SharedString>,
        text: String,
        path: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        Button::new(id.into(), text)
            .min_width(0.)
            .padding_y(4.)
            .padding_x(6.)
            .mono()
            .on_click(
                cx.listener_for_app(move |this: &mut Self, cx: &mut Context<Self>| {
                    this.open_path(path.clone(), cx)
                }),
            )
            .into_any_element()
    }

    fn file_list(
        &mut self,
        files: &FilesData,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = &theme.palette;
        let primary = p.primary.main;
        let mut header = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(12.))
            .h(px(30.))
            .flex_none()
            .border_b_1()
            .border_color(p.divider)
            .bg(p.background_paper);
        header = header
            .child(
                div()
                    .flex_1()
                    .child(caps("Name", 10.5, 650., p.text.secondary)),
            )
            .child(div().w(px(90.)).flex().justify_end().child(caps(
                "Size",
                10.5,
                650.,
                p.text.secondary,
            )))
            .child(
                div()
                    .w(px(170.))
                    .child(caps("Modified", 10.5, 650., p.text.secondary)),
            )
            .child(
                div()
                    .w(px(90.))
                    .child(caps("Permissions", 10.5, 650., p.text.secondary)),
            )
            .child(div().w(px(96.)));
        let mut rows = div().flex().flex_col();
        if files.loading && files.entries.is_empty() {
            rows = rows.child(div().p(px(24.)).flex().justify_center().child(
                CircularProgress::new(("files-progress", 1usize), 20., p.primary.main),
            ));
        } else if files.entries.is_empty() {
            rows = rows.child(div().p(px(20.)).flex().justify_center().child(body_text(
                if files.error.is_some() {
                    "Nothing to show."
                } else {
                    "This folder is empty. Drop files here to upload them."
                },
                p.text.secondary,
                theme,
            )));
        } else {
            for (index, entry) in files.entries.iter().enumerate() {
                rows = rows.child(self.file_row(index, entry, theme, cx));
            }
        }
        div()
            .id("file-list")
            .border_1()
            .border_color(p.divider)
            .rounded(px(theme.radius * 1.5))
            .max_h(px(420.))
            .overflow_y_scroll()
            .drag_over::<ExternalPaths>(move |style, _, _, _| {
                style
                    .border_color(primary)
                    .border_dashed()
                    .bg(alpha(primary, 0.06))
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                let paths: Vec<PathBuf> = paths.paths().to_vec();
                if !paths.is_empty() {
                    this.upload_paths(paths, cx);
                }
            }))
            .child(header)
            .child(rows)
            .into_any_element()
    }

    fn file_row(
        &mut self,
        index: usize,
        entry: &FileEntry,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = &theme.palette;
        let group: SharedString = format!("file-row-{index}").into();
        let hover = p.action.hover;
        let dir = entry.kind == EntryKind::Dir;
        let file = entry.kind == EntryKind::File;
        let title = if dir {
            format!("Open {}", entry.name)
        } else if file {
            format!("Preview {}", entry.name)
        } else {
            "Could not look inside; open it to try".to_owned()
        };
        let activate = entry.clone();
        let double = entry.clone();
        let mut row_actions = div()
            .w(px(96.))
            .flex()
            .justify_end()
            .invisible()
            .group_hover(group.clone(), |s| s.visible());
        if !dir {
            let target = entry.clone();
            row_actions = row_actions.child(
                IconButton::new(
                    SharedString::from(format!("download-{index}")),
                    "FileDownloadOutlined",
                )
                .icon_size(17.)
                .tooltip("Download")
                .on_click(
                    cx.listener(move |this, _, _, cx| this.download_entry(target.clone(), cx)),
                ),
            );
        }
        let path = entry.path.clone();
        row_actions = row_actions.child(
            IconButton::new(SharedString::from(format!("copy-{index}")), "ContentCopy")
                .icon_size(15.)
                .tooltip("Copy path")
                .on_click(
                    cx.listener(move |this, _, _, cx| this.copy(path.clone(), "the path", cx)),
                ),
        );
        if !dir {
            let target = entry.clone();
            row_actions = row_actions.child(
                IconButton::new(
                    SharedString::from(format!("delete-{index}")),
                    "DeleteOutlined",
                )
                .icon_size(17.)
                .tooltip("Delete")
                .on_click(cx.listener(move |this, _, _, cx| this.ask_remove(&target, cx))),
            );
        }
        let name_ink = p.text.primary;
        div()
            .id(SharedString::from(format!("file-{index}")))
            .group(group)
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(12.))
            .h(px(32.))
            .flex_none()
            .border_b_1()
            .border_color(p.divider)
            .hover(move |s| s.bg(hover))
            .on_click(
                cx.listener(move |this, event: &muxus_ui::gpui_kit::ClickEvent, _, cx| {
                    if event.click_count() == 2 {
                        this.activate_entry(double.clone(), cx);
                    }
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(20.))
                            .flex()
                            .justify_center()
                            .opacity(if entry.kind == EntryKind::Unknown {
                                0.5
                            } else {
                                1.
                            })
                            .child(file_type_icon(
                                &entry.name,
                                if dir {
                                    SftpEntryType::Dir
                                } else {
                                    SftpEntryType::File
                                },
                            )),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("name-{index}")))
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .cursor_pointer()
                            .text_size(px(13.))
                            .line_height(px(13. * 1.5))
                            .font_weight(muxus_ui::gpui_kit::FontWeight(if dir {
                                600.
                            } else {
                                400.
                            }))
                            .text_color(name_ink)
                            .hover(|s| s.underline())
                            .child(entry.name.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_entry(activate.clone(), cx)
                            }))
                            .mui_tooltip(title),
                    ),
            )
            .child(
                div()
                    .w(px(90.))
                    .flex()
                    .justify_end()
                    .font_family(MONO)
                    .text_size(px(12.))
                    .text_color(p.text.secondary)
                    .child(if file {
                        format_size(Some(entry.size.unwrap_or(0)))
                    } else {
                        String::new()
                    }),
            )
            .child(
                div()
                    .w(px(170.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(px(12.))
                    .text_color(p.text.secondary)
                    .child(device_time(
                        entry
                            .modified
                            .as_ref()
                            .map(|text| Value::String(text.clone()))
                            .as_ref(),
                        false,
                    )),
            )
            .child(
                div()
                    .w(px(90.))
                    .font_family(MONO)
                    .text_size(px(12.))
                    .text_color(p.text.secondary)
                    .child(permission_string(entry.permissions)),
            )
            .child(row_actions)
            .into_any_element()
    }

    fn activate_entry(&mut self, entry: FileEntry, cx: &mut Context<Self>) {
        if entry.kind == EntryKind::File {
            self.preview_entry(entry, cx);
        } else {
            self.open_path(entry.path, cx);
        }
    }

    /// Enter in the path box.
    pub(super) fn files_go(&mut self, cx: &mut Context<Self>) {
        let typed = self
            .ui
            .tools
            .local
            .files
            .path_input
            .clone()
            .unwrap_or_else(|| self.files_cwd());
        let typed = muxus_core::js::js_trim(&typed).to_owned();
        let target = if typed.is_empty() {
            self.files_home()
        } else {
            typed
        };
        self.open_path(target, cx);
    }

    pub(super) fn files_field(&mut self, name: &str, value: String) {
        if name == "files.path" {
            self.ui.tools.local.files.path_input = Some(value);
        }
    }

    /// List a folder; a file opens its folder and a preview.
    pub(super) fn open_path(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(connection) = self.grpc() else {
            return;
        };
        self.ui.tools.local.files.listing += 1;
        let ticket = self.ui.tools.local.files.listing;
        self.ui.tools.local.files.path_input = Some(path.clone());
        let files = &mut self.ui.tools.files;
        files.cwd = Some(path.clone());
        files.loading = true;
        files.error = None;
        let target = path.clone();
        let listing = self.on_runtime(async move { stat(&connection, &target).await });
        cx.spawn(async move |this, cx| {
            let result = listing
                .await
                .unwrap_or_else(|| Err(message_error("The session closed.")));
            let _ = this.update(cx, |this, cx| {
                if ticket != this.ui.tools.local.files.listing {
                    return;
                }
                match result {
                    Ok(mut entries) => {
                        if entries.len() == 1 && entries[0].path == trim_separators(&path) {
                            // A file: show its folder and open it.
                            this.ui.tools.files.loading = false;
                            let entry = entries.remove(0);
                            this.open_path(parent_of(&path), cx);
                            this.preview_entry(entry, cx);
                            return;
                        }
                        sort_entries(&mut entries);
                        this.ui.tools.files.entries = entries.clone();
                        this.ui.tools.files.loading = false;
                        this.classify(entries, ticket, cx);
                    }
                    Err(error) => {
                        let files = &mut this.ui.tools.files;
                        files.loading = false;
                        files.entries.clear();
                        files.error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Directories are only told apart from files by statting them: a file stats as itself.
    fn classify(&mut self, entries: Vec<FileEntry>, ticket: u64, cx: &mut Context<Self>) {
        let Some(connection) = self.grpc() else {
            return;
        };
        let queue: VecDeque<FileEntry> = entries
            .into_iter()
            .filter(|entry| entry.kind == EntryKind::Unknown)
            .take(300)
            .collect();
        if queue.is_empty() {
            return;
        }
        let queue = Arc::new(Mutex::new(queue));
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<(String, EntryKind)>();
        let runtime = self.runtime().clone();
        for _ in 0..8 {
            let queue = queue.clone();
            let sender = sender.clone();
            let connection = connection.clone();
            runtime.spawn(async move {
                loop {
                    let Some(entry) = queue.lock().ok().and_then(|mut queue| queue.pop_front())
                    else {
                        return;
                    };
                    let kind = match stat(&connection, &entry.path).await {
                        Ok(children) => kind_from_stat(&entry.path, &children),
                        Err(_) => EntryKind::Unknown,
                    };
                    if sender.send((entry.path, kind)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(sender);
        cx.spawn(async move |this, cx| {
            while let Some((path, kind)) = receiver.recv().await {
                let keep = this
                    .update(cx, |this, cx| {
                        if ticket != this.ui.tools.local.files.listing {
                            return false;
                        }
                        if let Some(entry) = this
                            .ui
                            .tools
                            .files
                            .entries
                            .iter_mut()
                            .find(|entry| entry.path == path)
                        {
                            entry.kind = kind;
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    // Stop the workers: nothing reads their answers any more.
                    if let Ok(mut queue) = queue.lock() {
                        queue.clear();
                    }
                    return;
                }
            }
        })
        .detach();
    }

    fn new_transfer(&mut self, name: String, upload: bool, total: Option<u64>) -> u64 {
        self.ui.tools.transfer_counter += 1;
        let id = self.ui.tools.transfer_counter;
        let transfers = &mut self.ui.tools.files.transfers;
        transfers.insert(
            0,
            Transfer {
                id,
                name,
                upload,
                bytes: 0,
                total,
                done: false,
                error: None,
                verified: false,
            },
        );
        transfers.truncate(8);
        id
    }

    fn update_transfer(&mut self, id: u64, update: impl FnOnce(&mut Transfer)) {
        if let Some(transfer) = self
            .ui
            .tools
            .files
            .transfers
            .iter_mut()
            .find(|transfer| transfer.id == id)
        {
            update(transfer);
        }
    }

    /// Fetch a file, reporting progress; `done` gets its bytes or the error.
    fn fetch_file(
        &mut self,
        path: String,
        transfer: Option<u64>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<(Vec<u8>, bool), String>, &mut Context<Self>) + 'static,
    ) {
        let Some(connection) = self.grpc() else {
            return;
        };
        enum Progress {
            Bytes(u64),
            Done(Result<(Vec<u8>, bool), String>),
        }
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Progress>();
        let progress = sender.clone();
        self.runtime().spawn(async move {
            let (future, _cancel) = connection.download_file(&path, move |bytes| {
                let _ = progress.send(Progress::Bytes(bytes));
            });
            let result = future
                .await
                .map(|file| (file.data, file.verified))
                .map_err(|err| err.error.message);
            let _ = sender.send(Progress::Done(result));
        });
        cx.spawn(async move |this, cx| {
            let mut done = Some(done);
            while let Some(message) = receiver.recv().await {
                let finished = matches!(message, Progress::Done(_));
                let _ = this.update(cx, |this, cx| {
                    match message {
                        Progress::Bytes(bytes) => {
                            if let Some(id) = transfer {
                                this.update_transfer(id, |transfer| transfer.bytes = bytes);
                            }
                        }
                        Progress::Done(result) => {
                            if let Some(done) = done.take() {
                                done(this, result, cx);
                            }
                        }
                    }
                    cx.notify();
                });
                if finished {
                    return;
                }
            }
        })
        .detach();
    }

    fn download_entry(&mut self, entry: FileEntry, cx: &mut Context<Self>) {
        if self.grpc().is_none() {
            return;
        }
        let id = self.new_transfer(entry.name.clone(), false, entry.size);
        let name = entry.name.clone();
        self.fetch_file(
            entry.path,
            Some(id),
            cx,
            move |this, result, cx| match result {
                Ok((data, verified)) => {
                    let size = data.len() as u64;
                    this.update_transfer(id, |transfer| {
                        transfer.bytes = size;
                        transfer.done = true;
                        transfer.verified = verified;
                    });
                    this.save_bytes(name, data, cx);
                }
                Err(error) => this.update_transfer(id, |transfer| {
                    transfer.done = true;
                    transfer.error = Some(error);
                }),
            },
        );
        cx.notify();
    }

    /// Ask where to save a download, then write it.
    fn save_bytes(&mut self, name: String, data: Vec<u8>, cx: &mut Context<Self>) {
        let directory = dirs_downloads();
        let chosen = crate::util::file_dialog::ask_save_path(&directory, &name, cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = chosen.await.path().map(std::path::Path::to_path_buf) else {
                return;
            };
            if let Err(error) = std::fs::write(&path, data) {
                let _ = this.update(cx, |this, cx| {
                    this.toast(Status::Error, format!("Could not save: {error}"), cx)
                });
            }
        })
        .detach();
    }

    fn preview_entry(&mut self, entry: FileEntry, cx: &mut Context<Self>) {
        if self.grpc().is_none() {
            return;
        }
        if entry.size.unwrap_or(0) > PREVIEW_LIMIT {
            self.download_entry(entry, cx);
            return;
        }
        self.ui.dialog = Some(Dialog::FilePreview {
            path: entry.path.clone(),
            text: None,
            loading: true,
            error: None,
        });
        let path = entry.path.clone();
        self.fetch_file(entry.path.clone(), None, cx, move |this, result, cx| {
            let open = matches!(&this.ui.dialog, Some(Dialog::FilePreview { path: shown, .. }) if *shown == path);
            match result {
                Ok((data, _)) if looks_binary(&data) => {
                    // Binary content gets downloaded instead of shown as mojibake.
                    if open {
                        this.ui.dialog = None;
                    }
                    this.download_entry(entry, cx);
                }
                Ok((data, _)) => {
                    if open {
                        this.ui.dialog = Some(Dialog::FilePreview {
                            path,
                            text: Some(String::from_utf8_lossy(&data).into_owned()),
                            loading: false,
                            error: None,
                        });
                    }
                }
                Err(error) => {
                    if open {
                        this.ui.dialog = Some(Dialog::FilePreview {
                            path,
                            text: None,
                            loading: false,
                            error: Some(error),
                        });
                    }
                }
            }
        });
        cx.notify();
    }

    /// The preview's Download button.
    pub(in super::super) fn download_previewed(&mut self, path: String, cx: &mut Context<Self>) {
        let entry = self
            .ui
            .tools
            .files
            .entries
            .iter()
            .find(|candidate| candidate.path == path)
            .cloned()
            .unwrap_or_else(|| FileEntry {
                name: basename(&path),
                path,
                size: None,
                modified: None,
                permissions: None,
                kind: EntryKind::File,
            });
        self.download_entry(entry, cx);
    }

    fn ask_remove(&mut self, entry: &FileEntry, cx: &mut Context<Self>) {
        if self.grpc().is_none() {
            return;
        }
        self.ui.dialog = Some(Dialog::Confirm {
            confirmation: Confirmation {
                title: format!("Delete {}?", entry.name),
                description: format!("{} is removed from the device.", entry.path),
                confirm_label: "Delete".into(),
                destructive: true,
            },
            action: ConfirmAction::Tool(ToolAction::DeleteFile {
                path: entry.path.clone(),
                name: entry.name.clone(),
            }),
        });
        cx.notify();
    }

    pub(super) fn remove_file(&mut self, path: String, name: String, cx: &mut Context<Self>) {
        self.grpc_call(
            grpc_methods::FILE_REMOVE,
            json!({ "remote_file": path }),
            cx,
            move |this, result, cx| match result {
                Ok(_) => {
                    this.toast(Status::Success, format!("Deleted {name}."), cx);
                    let cwd = this.files_cwd();
                    this.open_path(cwd, cx);
                }
                Err(error) => this.ui.tools.files.error = Some(error),
            },
        );
    }

    fn pick_uploads(&mut self, cx: &mut Context<Self>) {
        let paths = crate::util::file_dialog::pick_paths(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: None,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let Some(paths) = paths.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.upload_paths(paths, cx));
        })
        .detach();
    }

    /// Upload local files into the folder shown, one after another.
    fn upload_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(connection) = self.grpc() else {
            return;
        };
        let directory = self.files_cwd();
        let permissions = file_system_of(
            self.controller
                .gnmi_info()
                .map(|info| info.models.as_slice()),
        )
        .permissions;
        let mut jobs = Vec::new();
        for path in paths {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let size = std::fs::metadata(&path).map(|meta| meta.len()).ok();
            let id = self.new_transfer(name.clone(), true, size);
            jobs.push((id, path, join_path(&directory, &name)));
        }
        enum Progress {
            Bytes(u64, u64),
            Done(u64, Result<u64, String>),
            Finished,
        }
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Progress>();
        self.runtime().spawn(async move {
            for (id, local, remote) in jobs {
                let data = match std::fs::read(&local) {
                    Ok(data) => data,
                    Err(error) => {
                        let _ = sender.send(Progress::Done(id, Err(error.to_string())));
                        continue;
                    }
                };
                let progress = sender.clone();
                let (future, _cancel) =
                    connection.upload_file(&remote, data, Some(permissions), move |bytes| {
                        let _ = progress.send(Progress::Bytes(id, bytes));
                    });
                let result = future.await.map_err(|err| err.error.message);
                let _ = sender.send(Progress::Done(id, result));
            }
            let _ = sender.send(Progress::Finished);
        });
        cx.spawn(async move |this, cx| {
            while let Some(message) = receiver.recv().await {
                let finished = matches!(message, Progress::Finished);
                let _ = this.update(cx, |this, cx| {
                    match message {
                        Progress::Bytes(id, bytes) => {
                            this.update_transfer(id, |transfer| transfer.bytes = bytes)
                        }
                        Progress::Done(id, Ok(size)) => this.update_transfer(id, |transfer| {
                            transfer.bytes = size;
                            transfer.done = true;
                            transfer.verified = true;
                        }),
                        Progress::Done(id, Err(error)) => this.update_transfer(id, |transfer| {
                            transfer.done = true;
                            transfer.error = Some(error);
                        }),
                        Progress::Finished => {
                            let cwd = this.files_cwd();
                            if cwd == directory {
                                this.open_path(cwd, cx);
                            } else {
                                this.open_path(directory.clone(), cx);
                            }
                        }
                    }
                    cx.notify();
                });
                if finished {
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// The preview dialog's body: a spinner, the error, or the text read-only.
    pub(in super::super) fn file_preview_body(
        &mut self,
        path: &str,
        text: Option<&str>,
        loading: bool,
        error: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        if loading {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(CircularProgress::new(
                    ("files-progress", 2usize),
                    24.,
                    theme.palette.primary.main,
                ))
                .into_any_element();
        }
        if let Some(error) = error {
            return error_alert(&message_error(error), &theme, None).into_any_element();
        }
        match text {
            Some(text) => div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(self.code_editor(
                    EditorKey::Tool("files.preview"),
                    preview_language(path),
                    text,
                    true,
                    None,
                    true,
                    "",
                    window,
                    cx,
                ))
                .into_any_element(),
            None => div().into_any_element(),
        }
    }
}

/// The transfers, newest first.
fn transfers(list: &[Transfer], theme: &Theme) -> AnyElement {
    let p = &theme.palette;
    let mut column = div().flex().flex_col().gap(px(8.));
    for transfer in list {
        let size_text = match (&transfer.error, transfer.total) {
            (Some(error), _) => error.clone(),
            (None, Some(total)) => format!(
                "{} of {}",
                format_size(Some(transfer.bytes)),
                format_size(Some(total))
            ),
            (None, None) => format_size(Some(transfer.bytes)),
        };
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(icon(
                if transfer.upload {
                    "FileUploadOutlined"
                } else {
                    "FileDownloadOutlined"
                },
                16.,
                p.text.primary,
            ))
            .child(
                div().flex_1().min_w_0().child(
                    muxus_ui::label::label(transfer.name.clone())
                        .size(theme.typography.body2.size)
                        .line_height(theme.typography.body2.line_height)
                        .color(p.text.primary),
                ),
            )
            .child(caption_text(
                size_text,
                if transfer.error.is_some() {
                    p.error.main
                } else {
                    p.text.secondary
                },
                theme,
            ));
        if transfer.done && transfer.error.is_none() {
            row = row.child(
                div()
                    .id(SharedString::from(format!("verified-{}", transfer.id)))
                    .flex()
                    .child(icon(
                        "CheckCircleRounded",
                        16.,
                        if transfer.verified {
                            p.success.main
                        } else {
                            p.text.disabled
                        },
                    ))
                    .mui_tooltip(if transfer.verified {
                        "Hash checked"
                    } else {
                        "The device sent no hash to check"
                    }),
            );
        }
        let mut item = div().child(row);
        if !transfer.done {
            let bar = match transfer.total.filter(|total| *total > 0) {
                Some(total) => muxus_ui::widgets::display::linear_progress(
                    theme,
                    transfer.bytes as f32 / total as f32,
                ),
                None => indeterminate(theme),
            };
            item = item.child(div().mt(px(4.)).child(bar));
        }
        column = column.child(item);
    }
    column.into_any_element()
}

/// An indeterminate bar (no total known).
fn indeterminate(theme: &Theme) -> muxus_ui::gpui_kit::Div {
    muxus_ui::widgets::display::linear_progress(theme, 0.35)
}

fn dirs_downloads() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Downloads"))
        .filter(|path| path.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> FileEntry {
        FileEntry {
            path: format!("/{name}"),
            name: name.into(),
            size: None,
            modified: None,
            permissions: None,
            kind: EntryKind::Unknown,
        }
    }

    #[test]
    fn sorts_names_like_locale_compare() {
        let mut entries = vec![entry("b"), entry("B"), entry("a"), entry("C")];
        sort_entries(&mut entries);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "B", "C"]);
    }

    #[test]
    fn tells_binary_files_apart() {
        assert!(looks_binary(b"ab\0cd"));
        assert!(!looks_binary(b"plain text"));
        assert_eq!(preview_language("/etc/config.json"), "json");
        assert_eq!(preview_language("/var/log/messages"), "text");
    }
}
