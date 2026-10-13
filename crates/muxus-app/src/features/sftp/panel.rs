//! `client/src/components/SftpPanel.tsx`: the remote file browser, bound to a
//! live SSH connection. Files open in the editor on double-click; explicit
//! and outbound-drag downloads remain one gesture away.
//!
//! The panel is an entity the app embeds beside a terminal, docked in the
//! sidebar ([`SftpPanelOptions::in_sidebar`]) or filling an SFTP window
//! ([`SftpPanelOptions::fill`]). What it cannot do itself (open a file in the
//! editor, move between sidebar and pane, open a window, remember the width
//! or the follow choice) it reports as [`SftpPanelEvent`]s.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use muxus_core::api::{SftpEntry, SftpEntryType, SftpListResponse};
use muxus_session::ApiError;
use muxus_ssh::sftp::upload::{
    ConfirmRequest, DropPayload, UploadProgress, clean_relative_path, collect_drop,
    join_remote_path, upload_summary,
};
use muxus_ssh::sftp::{CancelSignal, ProgressMeter};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Bounds, ClickEvent, Context, CursorStyle, ElementId, Entity,
    EventEmitter, ExternalDragPayload, ExternalPaths, FileDragPaths, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, PathPromptOptions, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, canvas, div, px, relative,
    uniform_list,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts};
use muxus_ui::widgets::menu::{self, MenuItem};
use muxus_ui::widgets::{Checkbox, IconButton};

use super::dialogs::{ConfirmOptions, DialogQueue, PromptOptions, QueueOf};
use super::format::{format_mtime, format_size, parent_path, sort_entries};
use super::icons::file_type_icon;
use super::placement::{
    DEFAULT_SFTP_PANEL_WIDTH, NARROW_PANEL_WIDTH, clamp_sftp_panel_width, initial_sftp_path,
};
use super::services::{SftpListings, SftpServices, notify, notify_error, services};
use super::transfer::{TransferDirection, TransferPhase, TransferProgress, TransferProgressState};
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::progress::CircularProgress;

/// How the panel is shown (the `SftpPanel` props that are not callbacks).
#[derive(Clone, Debug)]
pub struct SftpPanelOptions {
    pub conn_id: String,
    /// Where to start; `.` is the home directory.
    pub initial_path: String,
    /// Latest working directory reported by the terminal sharing this connection.
    pub terminal_path: Option<String>,
    /// Whether the browser follows terminal working-directory reports.
    pub follow_terminal_folder: bool,
    /// Fill a standalone window instead of using the saved side-panel width.
    pub fill: bool,
    /// Docked in the window sidebar: it fills the sidebar and is titled with its session.
    pub in_sidebar: bool,
    /// The session's name, for the sidebar title and messages about uploads.
    pub host_label: Option<String>,
    /// False while kept mounted out of sight, as in the sidebar behind the hosts.
    pub shown: bool,
    /// Offer "Move to the sidebar" / "Move beside the terminal" (`onMove`).
    pub movable: bool,
    /// Offer "Open file browser in new window" (`onOpenInNewWindow`).
    pub opens_in_new_window: bool,
    /// Offer the "Follow terminal folder" box (`onFollowTerminalFolderChange`).
    pub follow_toggle: bool,
    /// `sftpPanelWidth` preference.
    pub width: f32,
    /// `sidebarPosition === 'left'`: mirrors the move-to-sidebar glyph.
    pub sidebar_on_left: bool,
}

impl SftpPanelOptions {
    pub fn new(conn_id: impl Into<String>) -> Self {
        Self {
            conn_id: conn_id.into(),
            initial_path: ".".into(),
            terminal_path: None,
            follow_terminal_folder: false,
            fill: false,
            in_sidebar: false,
            host_label: None,
            shown: true,
            movable: false,
            opens_in_new_window: false,
            follow_toggle: false,
            width: DEFAULT_SFTP_PANEL_WIDTH,
            sidebar_on_left: true,
        }
    }
}

/// What the panel asks of its host.
#[derive(Clone, Debug, PartialEq)]
pub enum SftpPanelEvent {
    /// Open the remote file in the editor (`onOpenFile`).
    OpenFile(String),
    /// Move the browser between the sidebar and its terminal (`onMove`).
    Move,
    /// Open a file browser window at this path (`onOpenInNewWindow`).
    OpenInNewWindow(String),
    /// The follow box changed (`onFollowTerminalFolderChange`).
    FollowTerminalFolder(bool),
    /// The user resized the panel; store it as `sftpPanelWidth`.
    WidthChanged(f32),
}

#[derive(Default)]
struct Listing {
    /// The path the data answers.
    requested: Option<String>,
    data: Option<SftpListResponse>,
    fetching: bool,
    error: Option<String>,
    generation: u64,
    /// The path and listing generation the request in flight answers. A repeated load of
    /// the same folder keeps it (as one query key keeps its one request) instead of
    /// starting over, which would cancel it.
    in_flight: Option<(String, u64)>,
}

struct Transfer {
    id: u64,
    state: TransferProgressState,
    cancel: CancelSignal,
}

struct ResizeDrag {
    start_x: f32,
    start_width: f32,
    width: f32,
}

#[derive(Clone, Copy, Default)]
struct Columns {
    size: f32,
    modified: f32,
}

// Chrome lays the 27.2px rows (20px icon, 3.6px padding) out on whole pixels.
const ROW_HEIGHT: f32 = 27.;
// 22.29px line, 3.6px padding and the border, painted as 31px.
const HEAD_HEIGHT: f32 = 31.;
const CELL_PX: f32 = 16.;
const MIN_NAME_COLUMN: f32 = 64.;

/// The file browser.
pub struct SftpPanel {
    options: SftpPanelOptions,
    path: String,
    path_input: Entity<InputState>,
    /// The listing path last written into the field.
    shown_path: Option<String>,
    /// A listing path to write into the field at the next render.
    pending_input: Option<String>,
    home_path: Option<String>,
    selected: Option<String>,
    menu: Option<(Point<Pixels>, SftpEntry)>,
    busy: bool,
    transfer: Option<Transfer>,
    next_transfer_id: u64,
    listing: Listing,
    entries: Vec<SftpEntry>,
    columns: Option<Columns>,
    fetch: Option<Task<()>>,
    work: Option<Task<()>>,
    dialogs: DialogQueue,
    resize: Option<ResizeDrag>,
    /// The table area's painted width, for the narrow layout and column fitting.
    body_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The pane the panel sits in; caps the width at 70% of it.
    container_width: Option<f32>,
    attached: bool,
    open_with: Option<(Entity<super::open_with::OpenWithDialog>, Subscription)>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SftpPanelEvent> for SftpPanel {}

impl Focusable for SftpPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

const QUEUE: QueueOf<SftpPanel> = |panel| &mut panel.dialogs;

fn parse_mtime_or_none(entry: &SftpEntry) -> Option<i64> {
    entry.mtime_ms
}

impl SftpPanel {
    pub fn new(options: SftpPanelOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let starting = initial_sftp_path(
            &options.initial_path,
            options.terminal_path.as_deref(),
            options.follow_terminal_folder,
        );
        let path_input = cx.new(|cx| InputState::new(window, cx).default_value(starting.clone()));
        let listings = services(cx).listings;
        let subscriptions = vec![
            cx.subscribe_in(&path_input, window, |this, input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let typed = input.read(cx).value().trim().to_string();
                    if !typed.is_empty() {
                        this.navigate(typed, window, cx);
                    }
                }
            }),
            cx.observe(&services(cx).uploads, |_, _, cx| cx.notify()),
            cx.observe(&listings, |this, listings, cx| {
                let generation = listings.read(cx).generation(&this.options.conn_id);
                if generation != this.listing.generation {
                    this.load(cx);
                }
            }),
        ];
        let mut panel = Self {
            options,
            path: starting,
            path_input,
            shown_path: None,
            pending_input: None,
            home_path: None,
            selected: None,
            menu: None,
            busy: false,
            transfer: None,
            next_transfer_id: 1,
            listing: Listing::default(),
            entries: Vec::new(),
            columns: None,
            fetch: None,
            work: None,
            dialogs: DialogQueue::new(),
            resize: None,
            body_bounds: Rc::default(),
            container_width: None,
            attached: false,
            open_with: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        panel.sync_attachment(cx);
        cx.on_release(|this, cx| {
            if this.attached {
                let conn = this.options.conn_id.clone();
                services(cx)
                    .uploads
                    .update(cx, |uploads, _| uploads.detach_browser(&conn));
            }
            if let Some(transfer) = &this.transfer {
                transfer.cancel.cancel();
            }
        })
        .detach();
        panel.load(cx);
        panel
    }

    pub fn conn_id(&self) -> &str {
        &self.options.conn_id
    }

    /// The names on screen, in their order.
    pub fn entry_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// A listing request is running.
    pub fn is_fetching(&self) -> bool {
        self.listing.fetching
    }

    /// Select an entry by name (as a click does).
    pub fn select(&mut self, name: &str, cx: &mut Context<Self>) {
        self.selected = Some(name.to_string());
        cx.notify();
    }

    /// The folder on screen.
    pub fn current_path(&self) -> String {
        self.listing
            .data
            .as_ref()
            .filter(|_| self.listing.requested.as_deref() == Some(self.path.as_str()))
            .map(|data| data.path.clone())
            .unwrap_or_else(|| self.path.clone())
    }

    /// The terminal reported a new working directory; followed while following is on.
    ///
    /// Like the client's effect on `[followTerminalFolder, terminalPath]`, only a change
    /// navigates: the shell calls this on every sync, and navigating each time would
    /// cancel the listing in flight and throw away the folder the user went to.
    pub fn set_terminal_path(
        &mut self,
        path: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.options.terminal_path == path {
            return;
        }
        self.options.terminal_path = path;
        self.follow(window, cx);
    }

    pub fn set_follow_terminal_folder(
        &mut self,
        follow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.options.follow_terminal_folder == follow {
            return;
        }
        self.options.follow_terminal_folder = follow;
        self.follow(window, cx);
        cx.notify();
    }

    fn follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.options.follow_terminal_folder {
            return;
        }
        if let Some(path) = self.options.terminal_path.clone() {
            self.navigate(path, window, cx);
        }
    }

    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.options.shown = shown;
        self.sync_attachment(cx);
        cx.notify();
    }

    /// Fills its parent (a window, the sidebar) instead of using its own width.
    pub fn fills(&self) -> bool {
        self.options.fill || self.options.in_sidebar
    }

    pub fn set_fill(&mut self, fill: bool, cx: &mut Context<Self>) {
        self.options.fill = fill;
        cx.notify();
    }

    pub fn set_width(&mut self, width: f32, cx: &mut Context<Self>) {
        self.options.width = width;
        cx.notify();
    }

    pub fn set_host_label(&mut self, host: Option<String>, cx: &mut Context<Self>) {
        self.options.host_label = host;
        cx.notify();
    }

    pub fn set_sidebar_on_left(&mut self, left: bool, cx: &mut Context<Self>) {
        self.options.sidebar_on_left = left;
        cx.notify();
    }

    /// The width of the pane the panel sits in (defaults to the window's).
    pub fn set_container_width(&mut self, width: Option<f32>) {
        self.container_width = width;
    }

    /// Files dropped onto a terminal of this connection report their upload here.
    fn sync_attachment(&mut self, cx: &mut Context<Self>) {
        if self.options.shown == self.attached {
            return;
        }
        self.attached = self.options.shown;
        let conn = self.options.conn_id.clone();
        let attach = self.attached;
        let uploads = services(cx).uploads;
        uploads.update(cx, |uploads, cx| {
            if attach {
                uploads.attach_browser(&conn);
            } else {
                uploads.detach_browser(&conn);
            }
            cx.notify();
        });
    }

    /// Go to a folder, as typing it into the path field does.
    pub fn navigate_to(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(path, window, cx);
    }

    fn navigate(&mut self, next: String, window: &mut Window, cx: &mut Context<Self>) {
        self.path = next.clone();
        self.path_input
            .update(cx, |input, cx| input.set_value(next.clone(), window, cx));
        self.shown_path = None;
        self.pending_input = None;
        self.selected = None;
        self.load(cx);
        cx.notify();
    }

    /// `refresh`: every browser of the connection lists again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let conn = self.options.conn_id.clone();
        services(cx)
            .listings
            .update(cx, |listings, cx| listings.refresh(&conn, cx));
    }

    /// `useSftpList`: cached listings show at once; stale ones are fetched again
    /// (one retry after a second, as the client's queries retry once).
    fn load(&mut self, cx: &mut Context<Self>) {
        let conn = self.options.conn_id.clone();
        let path = self.path.clone();
        let listings = services(cx).listings;
        let (generation, cached, fresh) = {
            let listings: &SftpListings = listings.read(cx);
            (
                listings.generation(&conn),
                listings
                    .get(&conn, &path)
                    .map(|cached| cached.response.clone()),
                listings.is_fresh(&conn, &path),
            )
        };
        self.listing.generation = generation;
        if self.listing.requested.as_deref() != Some(path.as_str()) {
            // Another folder: nothing of the last one stays on screen (the query of a new
            // key has no data until it answers).
            self.listing.data = None;
            self.listing.error = None;
            self.entries.clear();
            self.columns = None;
        }
        self.listing.requested = Some(path.clone());
        if let Some(cached) = cached {
            self.apply_listing(cached, cx);
        }
        if fresh {
            self.listing.fetching = false;
            self.listing.in_flight = None;
            self.fetch = None;
            return;
        }
        let key = (path.clone(), generation);
        if self.fetch.is_some() && self.listing.in_flight.as_ref() == Some(&key) {
            return;
        }
        self.listing.in_flight = Some(key);
        self.listing.fetching = true;
        let backend = services(cx).backend;
        self.fetch = Some(cx.spawn(async move |this, cx| {
            let mut result = backend.sftp_list(&conn, &path).await;
            if result.is_err() {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                result = backend.sftp_list(&conn, &path).await;
            }
            let _ = this.update(cx, |this, cx| {
                if this.listing.in_flight.as_ref() == Some(&(path.clone(), generation)) {
                    this.listing.in_flight = None;
                }
                if this.path != path {
                    return;
                }
                this.listing.fetching = false;
                match result {
                    Ok(response) => {
                        listings.update(cx, |listings, _| {
                            listings.store(&conn, &path, response.clone(), generation)
                        });
                        this.listing.error = None;
                        this.apply_listing(response, cx);
                    }
                    Err(error) => {
                        this.listing.error = Some(error.message);
                        this.listing.data = None;
                        this.entries.clear();
                        this.columns = None;
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn apply_listing(&mut self, response: SftpListResponse, cx: &mut Context<Self>) {
        if self.path == "." {
            self.home_path = Some(response.path.clone());
        }
        if self.shown_path.as_deref() != Some(response.path.as_str()) {
            self.shown_path = Some(response.path.clone());
            self.pending_input = Some(response.path.clone());
            cx.notify();
        }
        self.entries = sort_entries(&response.entries);
        self.columns = None;
        self.listing.data = Some(response);
    }

    fn home(&self) -> String {
        self.home_path
            .clone()
            .or_else(|| {
                (self.path == ".")
                    .then(|| self.listing.data.as_ref().map(|data| data.path.clone()))
                    .flatten()
            })
            .unwrap_or_else(|| ".".into())
    }

    fn selected_entry(&self) -> Option<&SftpEntry> {
        let name = self.selected.as_ref()?;
        self.entries.iter().find(|entry| &entry.name == name)
    }

    fn remote_path(&self, entry: &SftpEntry) -> String {
        join_remote_path(&self.current_path(), &entry.name)
    }

    // --- transfers --------------------------------------------------------------------------

    fn warn_busy(cx: &mut App) {
        notify(
            Status::Warning,
            "Wait for the current SFTP operation to finish.",
            cx,
        );
    }

    fn show_transfer(&mut self, id: u64, state: TransferProgressState, cx: &mut Context<Self>) {
        if let Some(transfer) = &mut self.transfer
            && transfer.id == id
        {
            if transfer.state.phase == TransferPhase::Cancelling
                && state.phase != TransferPhase::Complete
            {
                return;
            }
            transfer.state = state;
            cx.notify();
        }
    }

    fn settle_transfer(&mut self, id: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1_200))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .transfer
                    .as_ref()
                    .is_some_and(|transfer| transfer.id == id)
                {
                    this.transfer = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn cancel_transfer(&mut self, cx: &mut Context<Self>) {
        if let Some(transfer) = &mut self.transfer {
            transfer.state.phase = TransferPhase::Cancelling;
            transfer.state.bytes_per_second = 0.;
            transfer.cancel.cancel();
            cx.notify();
        }
    }

    /// `fetchFile`: download `file` with progress, then hand the bytes and the
    /// remote modification time to `deliver`.
    fn fetch_file(
        &mut self,
        entry: SftpEntry,
        file: String,
        deliver: impl FnOnce(Vec<u8>, Option<i64>, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            Self::warn_busy(cx);
            return;
        }
        let id = self.next_transfer_id;
        self.next_transfer_id += 1;
        let cancel = CancelSignal::new();
        self.busy = true;
        self.transfer = Some(Transfer {
            id,
            state: TransferProgressState::starting(
                TransferDirection::Download,
                entry.name.clone(),
                entry.size,
            ),
            cancel: cancel.clone(),
        });
        cx.notify();
        let backend = services(cx).backend;
        let conn = self.options.conn_id.clone();
        let name = entry.name.clone();
        self.work = Some(cx.spawn_in(window, async move |this, cx| {
            let progress_state = |progress: muxus_ssh::sftp::ByteProgress| TransferProgressState {
                direction: TransferDirection::Download,
                name: name.clone(),
                loaded: progress.loaded,
                total: progress.total,
                bytes_per_second: progress.bytes_per_second,
                phase: TransferPhase::Transferring,
                file_index: Some(1),
                file_count: Some(1),
            };
            let result: Result<(Vec<u8>, Option<i64>), ApiError> = async {
                let stream = backend.sftp_download(&conn, &file).await?;
                let total = stream.size;
                let mtime = stream.mtime_ms;
                let mut meter = ProgressMeter::new();
                let mut bytes = Vec::new();
                if let Some(progress) = meter.report(0, total, true) {
                    let state = progress_state(progress);
                    let _ = this.update(cx, |this, cx| this.show_transfer(id, state, cx));
                }
                loop {
                    if cancel.is_cancelled() {
                        return Err(ApiError::with_code(499, "Transfer cancelled", "aborted"));
                    }
                    // Dropping the stream on cancel stops the transfer.
                    let Some(chunk) = stream.next_chunk().await else {
                        break;
                    };
                    bytes.extend_from_slice(&chunk?);
                    if let Some(progress) = meter.report(bytes.len() as u64, total, false) {
                        let state = progress_state(progress);
                        let _ = this.update(cx, |this, cx| this.show_transfer(id, state, cx));
                    }
                }
                Ok((bytes, mtime))
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok((bytes, mtime)) => {
                        let size = bytes.len() as u64;
                        this.show_transfer(
                            id,
                            TransferProgressState {
                                direction: TransferDirection::Download,
                                name: entry.name.clone(),
                                loaded: size,
                                total: Some(size),
                                bytes_per_second: 0.,
                                phase: TransferPhase::Complete,
                                file_index: Some(1),
                                file_count: Some(1),
                            },
                            cx,
                        );
                        deliver(bytes, mtime, window, cx);
                        this.settle_transfer(id, cx);
                    }
                    Err(error) => {
                        this.transfer = None;
                        if error.code.as_deref() == Some("aborted") {
                            notify(
                                Status::Info,
                                format!("Cancelled download of {}", entry.name),
                                cx,
                            );
                        } else {
                            notify_error(&error, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    /// `download`: the file, saved where the user picks.
    fn download(&mut self, entry: SftpEntry, window: &mut Window, cx: &mut Context<Self>) {
        let file = self.remote_path(&entry);
        let name = entry.name.clone();
        self.fetch_file(
            entry,
            file,
            move |bytes, _, _, cx| save_bytes(name, bytes, cx),
            window,
            cx,
        );
    }

    /// `upload`: a drop or a pick of local files into the folder on screen.
    fn upload(&mut self, payload: DropPayload, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || payload.is_empty() {
            return;
        }
        let id = self.next_transfer_id;
        self.next_transfer_id += 1;
        let cancel = CancelSignal::new();
        self.busy = true;
        self.transfer = Some(Transfer {
            id,
            state: TransferProgressState::starting(TransferDirection::Upload, "files", None),
            cancel: cancel.clone(),
        });
        cx.notify();
        let SftpServices { backend, .. } = services(cx);
        let conn = self.options.conn_id.clone();
        let directory = self.current_path();
        let (progress_tx, progress_rx) = async_channel::unbounded::<UploadProgress>();
        let (ask_tx, ask_rx) =
            async_channel::unbounded::<(ConfirmRequest, async_channel::Sender<bool>)>();
        // Progress and replace questions come from the backend's threads.
        let progress_task = cx.spawn(async move |this, cx| {
            while let Ok(progress) = progress_rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.show_transfer(id, TransferProgressState::from_upload(&progress), cx)
                });
            }
        });
        let ask_task = cx.spawn_in(window, async move |this, cx| {
            while let Ok((request, reply)) = ask_rx.recv().await {
                let answer = this.update_in(cx, |this, window, cx| {
                    this.dialogs.confirm(
                        ConfirmOptions {
                            title: request.title,
                            description: Some(request.description),
                            confirm_label: request.confirm_label,
                            destructive: request.destructive,
                            ..ConfirmOptions::default()
                        },
                        window,
                        cx,
                    )
                });
                let confirmed = match answer {
                    Ok(answer) => answer.await.confirmed,
                    Err(_) => false,
                };
                let _ = reply.try_send(confirmed);
            }
        });
        self.work = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .sftp_upload_drop(
                    &conn,
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
            drop(ask_task);
            let _ = progress_task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(result) => {
                        notify(Status::Success, upload_summary(result, None), cx);
                        this.refresh(cx);
                        this.settle_transfer(id, cx);
                    }
                    Err(error) => {
                        this.transfer = None;
                        if error.code.as_deref() == Some(super::terminal_upload::UPLOAD_CANCELLED) {
                            notify(Status::Info, "Upload cancelled", cx);
                        } else {
                            notify_error(&error, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    fn upload_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            Self::warn_busy(cx);
            return;
        }
        let task = cx
            .background_executor()
            .spawn(async move { collect_drop(&paths) });
        cx.spawn_in(window, async move |this, cx| {
            let payload = task.await;
            let _ = this.update_in(cx, |this, window, cx| match payload {
                Ok(payload) => this.upload(payload, window, cx),
                Err(error) => notify(Status::Error, error.to_string(), cx),
            });
        })
        .detach();
    }

    fn pick_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = crate::util::file_dialog::pick_paths(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: None,
            },
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if let Some(paths) = picked.await
                && !paths.is_empty()
            {
                let _ = this.update_in(cx, |this, window, cx| this.upload_paths(paths, window, cx));
            }
        })
        .detach();
    }

    /// `run`: one remote operation, then a fresh listing.
    fn run(
        &mut self,
        operation: impl std::future::Future<Output = Result<(), ApiError>> + 'static,
        cx: &mut Context<Self>,
    ) {
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = operation.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => this.refresh(cx),
                    Err(error) => notify_error(&error, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn valid_name() -> super::dialogs::Validator {
        Rc::new(|value: &str| {
            clean_relative_path(value)
                .is_empty()
                .then(|| "Enter a name without path separators.".to_string())
        })
    }

    fn mkdir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_path();
        let answer = self.dialogs.prompt(
            PromptOptions {
                title: "New folder".into(),
                description: Some(format!("Created in {current}")),
                label: "Folder name".into(),
                confirm_label: "Create".into(),
                validate: Some(Self::valid_name()),
                ..PromptOptions::default()
            },
            QUEUE,
            window,
            cx,
        );
        let backend = services(cx).backend;
        let conn = self.options.conn_id.clone();
        cx.spawn(async move |this, cx| {
            let Some(name) = answer.await else { return };
            let target = join_remote_path(&current, &clean_relative_path(&name));
            let _ = this.update(cx, |this, cx| {
                this.run(
                    async move {
                        backend.sftp_mkdir(&conn, &target, false).await?;
                        Ok(())
                    },
                    cx,
                )
            });
        })
        .detach();
    }

    fn rename(&mut self, entry: SftpEntry, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_path();
        let from = self.remote_path(&entry);
        let answer = self.dialogs.prompt(
            PromptOptions {
                title: format!("Rename {}", entry.name),
                label: "New name".into(),
                initial_value: entry.name.clone(),
                confirm_label: "Rename".into(),
                validate: Some(Self::valid_name()),
                ..PromptOptions::default()
            },
            QUEUE,
            window,
            cx,
        );
        let backend = services(cx).backend;
        let conn = self.options.conn_id.clone();
        cx.spawn(async move |this, cx| {
            let Some(name) = answer.await else { return };
            if name == entry.name {
                return;
            }
            let to = join_remote_path(&current, &clean_relative_path(&name));
            let _ = this.update(cx, |this, cx| {
                this.run(
                    async move {
                        backend.sftp_rename(&conn, &from, &to).await?;
                        Ok(())
                    },
                    cx,
                )
            });
        })
        .detach();
    }

    fn remove(&mut self, entry: SftpEntry, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.remote_path(&entry);
        let answer = self.dialogs.confirm(
            ConfirmOptions {
                title: format!("Delete {}?", entry.name),
                description: Some(if entry.kind == SftpEntryType::Dir {
                    "The folder and everything inside it is removed from the remote host. This cannot be undone.".into()
                } else {
                    "The file is removed from the remote host. This cannot be undone.".into()
                }),
                confirm_label: "Delete".into(),
                destructive: true,
                ..ConfirmOptions::default()
            },
            window,
            cx,
        );
        let backend = services(cx).backend;
        let conn = self.options.conn_id.clone();
        cx.spawn(async move |this, cx| {
            if !answer.await.confirmed {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.run(
                    async move {
                        backend.sftp_delete(&conn, &target).await?;
                        Ok(())
                    },
                    cx,
                )
            });
        })
        .detach();
    }

    fn open_file(&mut self, path: String, cx: &mut Context<Self>) {
        cx.emit(SftpPanelEvent::OpenFile(path));
    }

    fn open_locally(
        &mut self,
        entry: SftpEntry,
        target: muxus_core::api::LocalOpenTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let file = self.remote_path(&entry);
        let conn = self.options.conn_id.clone();
        let host = self.options.host_label.clone();
        let remote = file.clone();
        let name = entry.name.clone();
        self.fetch_file(
            entry,
            file,
            move |bytes, mtime, _, cx| {
                super::local_open::open_downloaded_file(
                    super::local_open::DownloadedFile {
                        conn_id: conn,
                        remote_path: remote,
                        name,
                        host,
                        remote_mtime_ms: mtime,
                        bytes,
                    },
                    target,
                    cx,
                )
            },
            window,
            cx,
        );
    }

    /// "Open with…": the chooser this platform has.
    fn open_with(&mut self, entry: SftpEntry, window: &mut Window, cx: &mut Context<Self>) {
        use super::local_open::OpenWithChooser;
        match super::local_open::open_with_chooser() {
            Some(OpenWithChooser::System) => self.open_locally(
                entry,
                muxus_core::api::LocalOpenTarget::SystemChooser,
                window,
                cx,
            ),
            Some(OpenWithChooser::ApplicationList) => {
                // The path is taken now: a followed terminal can change folders
                // while a program is being picked.
                let dialog = cx.new(|cx| {
                    super::open_with::OpenWithDialog::new(entry.name.clone(), window, cx)
                });
                let subscription =
                    cx.subscribe_in(&dialog, window, move |this, _, event, window, cx| {
                        let event = event.clone();
                        this.open_with = None;
                        if let super::open_with::OpenWithEvent::Open(target) = event {
                            this.open_locally(entry.clone(), target, window, cx);
                        }
                        cx.notify();
                    });
                self.open_with = Some((dialog, subscription));
                cx.notify();
            }
            Some(OpenWithChooser::ProgramPicker) | None => {}
        }
    }

    // --- events --------------------------------------------------------------------------

    fn on_row_click(
        &mut self,
        entry: &SftpEntry,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(entry.name.clone());
        if event.click_count() >= 2 {
            let path = self.remote_path(entry);
            match entry.kind {
                SftpEntryType::Dir => self.navigate(path, window, cx),
                SftpEntryType::File => self.open_file(path, cx),
                _ => {}
            }
        }
        cx.notify();
    }

    fn on_row_menu(&mut self, entry: &SftpEntry, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.selected = Some(entry.name.clone());
        self.menu = Some((position, entry.clone()));
        cx.notify();
    }

    fn on_drop_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            Self::warn_busy(cx);
            return;
        }
        self.upload_paths(paths.paths().to_vec(), window, cx);
    }

    // --- rendering ------------------------------------------------------------------------

    fn columns(&mut self, window: &mut Window) -> Columns {
        if let Some(columns) = self.columns {
            return columns;
        }
        let head = |text: &str, window: &mut Window| {
            muxus_ui::text::measure_text(window, text, fonts::UI, 12., 550.)
        };
        let mut size = head("Size", window);
        let mut modified = head("Modified", window);
        for entry in &self.entries {
            if entry.kind == SftpEntryType::File {
                size = size.max(muxus_ui::text::measure_text(
                    window,
                    &format_size(entry.size),
                    fonts::UI,
                    12.,
                    400.,
                ));
            }
            modified = modified.max(muxus_ui::text::measure_text(
                window,
                &format_mtime(parse_mtime_or_none(entry)),
                fonts::UI,
                12.,
                400.,
            ));
        }
        let columns = Columns {
            size: (size + 2. * CELL_PX).max(72.),
            modified: (modified + 2. * CELL_PX).max(112.),
        };
        self.columns = Some(columns);
        columns
    }

    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme.palette;
        let movable = self.options.movable;
        let in_sidebar = self.options.in_sidebar;
        let title = match (&self.options.host_label, in_sidebar) {
            (Some(host), true) => host.clone(),
            _ => "File browser".into(),
        };
        let subtitle2 = theme.typography.subtitle2;
        let caption = theme.typography.caption;
        let mut row = div()
            .flex()
            .items_center()
            .px(px(10.))
            .pt(px(8.))
            .child(
                div()
                    .mr(px(6.))
                    .child(icon("FolderOpenOutlined", 18., p.primary.main)),
            )
            .child(
                label(title)
                    .size(subtitle2.size)
                    .weight(subtitle2.weight)
                    .line_height(subtitle2.line_height)
                    .color(p.text.primary)
                    .grow(),
            )
            .child(
                div().ml(px(6.)).child(
                    label("SFTP")
                        .size(caption.size)
                        .line_height(caption.line_height)
                        .color(p.text.secondary)
                        .no_truncate(),
                ),
            );
        if movable {
            let (glyph, tip) = if in_sidebar {
                ("VerticalSplitOutlined", "Move beside the terminal")
            } else {
                ("ViewSidebarOutlined", "Move to the sidebar")
            };
            // The glyph draws its panel on the right; mirrored, it shows the left.
            let mirrored = !in_sidebar && self.options.sidebar_on_left;
            let button = if mirrored {
                mirrored_icon_button("sftp-move", glyph, tip, theme)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SftpPanelEvent::Move)))
                    .into_any_element()
            } else {
                IconButton::new("sftp-move", glyph)
                    .icon_size(17.)
                    .tooltip(tip)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SftpPanelEvent::Move)))
                    .into_any_element()
            };
            row = row.child(div().ml(px(4.)).child(button));
        }
        if self.options.opens_in_new_window {
            row = row.child(
                div().ml(px(4.)).child(
                    IconButton::new("sftp-new-window", "OpenInNewOutlined")
                        .icon_size(17.)
                        .tooltip("Open file browser in new window")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let path = this.current_path();
                            cx.emit(SftpPanelEvent::OpenInNewWindow(path))
                        })),
                ),
            );
        }
        row
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.current_path();
        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .p(px(6.))
            .child(
                IconButton::new("sftp-parent", "ArrowUpward")
                    .tooltip("Parent directory")
                    .disabled(current == "/")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let parent = parent_path(&this.current_path());
                        this.navigate(parent, window, cx);
                    })),
            )
            .child(
                IconButton::new("sftp-home", "HomeOutlined")
                    .tooltip("Home")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let home = this.home();
                        this.navigate(home, window, cx);
                    })),
            )
            .child(
                // The toolbar's path: MUI's small outlined field with the input in
                // JetBrains Mono at 11.5px.
                FormField::input("sftp-path", &self.path_input)
                    .compact(11.5 * 1.4375 + 17., 11.5)
                    .mono(11.5)
                    .flexible(),
            )
            .child(
                IconButton::new("sftp-refresh", "Refresh")
                    .tooltip("Refresh")
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    }

    fn render_actions(
        &self,
        theme: &Theme,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let busy = self.busy;
        let file_selected = self
            .selected_entry()
            .is_some_and(|entry| entry.kind == SftpEntryType::File);
        let caption = theme.typography.caption;
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .px(px(6.))
            .pb(px(6.))
            .child(
                IconButton::new("sftp-mkdir", "CreateNewFolderOutlined")
                    .tooltip("New folder")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.mkdir(window, cx))),
            )
            .child(
                IconButton::new("sftp-upload", "UploadFileOutlined")
                    .tooltip("Upload files")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.pick_upload(window, cx))),
            )
            .child(
                IconButton::new("sftp-edit", "EditOutlined")
                    .tooltip("Open selected file in editor")
                    .disabled(busy || !file_selected)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(entry) = this.selected_entry().cloned() {
                            let path = this.remote_path(&entry);
                            this.open_file(path, cx);
                        }
                    })),
            )
            .child(
                IconButton::new("sftp-download", "DownloadOutlined")
                    .tooltip("Download selected file")
                    .disabled(busy || !file_selected)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(entry) = this.selected_entry().cloned() {
                            this.download(entry, window, cx);
                        }
                    })),
            )
            .child(div().flex_1());
        if !narrow {
            row = row.child(
                label("Drag in to upload · drag out to download")
                    .size(caption.size)
                    .line_height(caption.line_height)
                    .color(theme.palette.text.disabled)
                    .no_truncate(),
            );
        }
        row
    }

    fn render_table(
        &mut self,
        theme: &Theme,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let columns = self.columns(window);
        let available = self
            .body_bounds
            .get()
            .map(|bounds| muxus_ui::zoom::unzoom(bounds.size.width))
            .unwrap_or(self.options.width - 1.);
        let fixed = columns.size + if narrow { 0. } else { columns.modified };
        // Chrome gives the name column what the fixed columns leave, down to
        // a sliver; names longer than that run into its padding.
        let table_width = available.max(fixed + MIN_NAME_COLUMN);
        let name_width = table_width - fixed;
        let head_bg = if self.options.in_sidebar {
            p.sidebar
        } else {
            p.background_default
        };
        let head_cell = |text: &'static str| {
            label(text)
                .size(12.)
                .weight(550.)
                .line_height(22.286)
                .color(p.text.secondary)
                .no_truncate()
        };
        let mut header = div()
            .flex()
            .flex_none()
            .w(px(table_width))
            .h(px(HEAD_HEIGHT))
            .bg(head_bg)
            .border_b_1()
            .border_color(p.divider)
            .child(
                div()
                    .w(px(name_width))
                    .px(px(CELL_PX))
                    .flex()
                    .items_center()
                    .child(head_cell("Name")),
            )
            .child(
                div()
                    .w(px(columns.size))
                    .px(px(CELL_PX))
                    .flex()
                    .items_center()
                    .justify_end()
                    .child(head_cell("Size")),
            );
        if !narrow {
            header = header.child(
                div()
                    .w(px(columns.modified))
                    .px(px(CELL_PX))
                    .flex()
                    .items_center()
                    .child(head_cell("Modified")),
            );
        }

        let count = self.entries.len();
        let body: AnyElement = if count == 0 && !self.listing.fetching {
            let caption = theme.typography.caption;
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.))
                .py(px(24.))
                .w(px(table_width))
                .child(icon("FolderOffOutlined", 26., p.text.disabled))
                .child(
                    label("Empty directory")
                        .size(caption.size)
                        .line_height(caption.line_height)
                        .color(p.text.secondary)
                        .no_truncate(),
                )
                .into_any_element()
        } else {
            let theme = *theme;
            uniform_list(
                "sftp-rows",
                count,
                cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                    range
                        .map(|index| {
                            this.render_row(index, &theme, narrow, name_width, columns, cx)
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .w(px(table_width))
            .flex_1()
            .into_any_element()
        };
        let table = div()
            .flex()
            .flex_col()
            .size_full()
            .w(px(table_width))
            .child(header)
            .child(body);
        if table_width > available + 0.5 {
            div()
                .id("sftp-table-scroll")
                .size_full()
                .overflow_x_scroll()
                .child(table)
                .into_any_element()
        } else {
            table.into_any_element()
        }
    }

    fn render_row(
        &mut self,
        index: usize,
        theme: &Theme,
        narrow: bool,
        name_width: f32,
        columns: Columns,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let Some(entry) = self.entries.get(index).cloned() else {
            return div().into_any_element();
        };
        let selected = self.selected.as_deref() == Some(entry.name.as_str());
        let hover = if selected {
            alpha(
                p.primary.main,
                p.action.selected_opacity + p.action.hover_opacity,
            )
        } else {
            p.action.hover
        };
        let is_file = entry.kind == SftpEntryType::File;
        let cell_text = |text: String| {
            label(text)
                .size(12.)
                .line_height(17.16)
                .color(p.text.secondary)
                .no_truncate()
        };
        let click_entry = entry.clone();
        let menu_entry = entry.clone();
        let mut row = div()
            .id(ElementId::Name(format!("sftp-row-{}", entry.name).into()))
            .flex()
            .h(px(ROW_HEIGHT))
            .w(px(name_width
                + columns.size
                + if narrow { 0. } else { columns.modified }))
            .cursor(if is_file {
                CursorStyle::OpenHand
            } else {
                CursorStyle::PointingHand
            })
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.on_row_click(&click_entry, event, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.on_row_menu(&menu_entry, event.position, cx)
                }),
            )
            .child(
                div()
                    .w(px(name_width))
                    .min_w_0()
                    .pl(px(CELL_PX))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(file_type_icon(&entry.name, entry.kind))
                    .child(
                        label(entry.name.clone())
                            .size(12.)
                            .line_height(17.16)
                            .color(p.text.primary)
                            .grow(),
                    ),
            )
            .child(
                div()
                    .w(px(columns.size))
                    .h_full()
                    .px(px(CELL_PX))
                    .flex()
                    .items_center()
                    .justify_end()
                    .border_b_1()
                    .border_color(p.divider)
                    .child(cell_text(if is_file {
                        format_size(entry.size)
                    } else {
                        String::new()
                    })),
            );
        if !narrow {
            row = row.child(
                div()
                    .w(px(columns.modified))
                    .h_full()
                    .px(px(CELL_PX))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(p.divider)
                    .child(cell_text(format_mtime(entry.mtime_ms))),
            );
        }
        if selected {
            row = row.bg(alpha(p.primary.main, p.action.selected_opacity));
        }
        if is_file {
            row = row.tooltip(muxus_ui::widgets::Tooltip::text(
                "Double-click to edit · drag out to download",
            ));
        }
        if is_file && let Some(ticket) = entry.download_ticket.clone() {
            // Drag out to download: the file is fetched (by its drag ticket)
            // as soon as the drag starts and handed to the desktop as a file
            // once the pointer leaves the window.
            row = row
                .on_drag(
                    DraggedFile {
                        conn_id: self.options.conn_id.clone(),
                        ticket,
                        name: entry.name.clone(),
                        local: Rc::default(),
                    },
                    |dragged, _, _, cx| {
                        dragged.fetch(cx);
                        let name = dragged.name.clone();
                        cx.new(|_| DragPreview { name })
                    },
                )
                .external_drag_payload(|dragged: &DraggedFile, _, _| {
                    let file = dragged.local.borrow().clone()?;
                    Some(ExternalDragPayload::Files(FileDragPaths::new([(
                        file, false,
                    )])))
                });
        }
        row.into_any_element()
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (position, entry) = self.menu.clone()?;
        let theme = *cx.theme();
        let entity = cx.entity().downgrade();
        let is_file = entry.kind == SftpEntryType::File;
        let local = super::local_open::can_open_locally();
        let chooser = super::local_open::open_with_chooser().is_some();
        let mut items: Vec<AnyElement> = Vec::new();
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.menu = None;
            cx.notify();
        };
        if is_file {
            let e = entry.clone();
            items.push(
                MenuItem::new("sftp-menu-edit", "Open in editor")
                    .icon("EditOutlined")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let path = this.remote_path(&e);
                        close(this, cx);
                        this.open_file(path, cx);
                    }))
                    .into_any_element(),
            );
            if local {
                let e = entry.clone();
                items.push(
                    MenuItem::new("sftp-menu-default", "Open with default program")
                        .icon("LaunchOutlined")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            close(this, cx);
                            this.open_locally(
                                e.clone(),
                                muxus_core::api::LocalOpenTarget::Default,
                                window,
                                cx,
                            );
                        }))
                        .into_any_element(),
                );
            }
            if chooser {
                let e = entry.clone();
                items.push(
                    MenuItem::new("sftp-menu-open-with", "Open with…")
                        .icon("AppsOutlined")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            close(this, cx);
                            this.open_with(e.clone(), window, cx);
                        }))
                        .into_any_element(),
                );
            }
            let e = entry.clone();
            items.push(
                MenuItem::new("sftp-menu-download", "Download")
                    .icon("DownloadOutlined")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        close(this, cx);
                        this.download(e.clone(), window, cx);
                    }))
                    .into_any_element(),
            );
        }
        let e = entry.clone();
        items.push(
            MenuItem::new("sftp-menu-rename", "Rename")
                .icon("DriveFileRenameOutline")
                .on_click(cx.listener(move |this, _, window, cx| {
                    close(this, cx);
                    this.rename(e.clone(), window, cx);
                }))
                .into_any_element(),
        );
        let e = entry;
        items.push(
            MenuItem::new("sftp-menu-delete", "Delete")
                .icon("DeleteOutlined")
                .destructive()
                .on_click(cx.listener(move |this, _, window, cx| {
                    close(this, cx);
                    this.remove(e.clone(), window, cx);
                }))
                .into_any_element(),
        );
        Some(
            menu::popup(
                position,
                move |_, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.menu = None;
                        cx.notify();
                    });
                },
                menu::paper(&theme).children(items),
            )
            .into_any_element(),
        )
    }

    fn render_drop_overlay(&self, theme: &Theme) -> impl IntoElement {
        let p = theme.palette;
        let caption = theme.typography.caption;
        let subtitle2 = theme.typography.subtitle2;
        div()
            .absolute()
            .top(px(8.))
            .left(px(8.))
            .right(px(8.))
            .bottom(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .border_2()
            .border_dashed()
            .border_color(p.primary.main)
            .rounded(px(16.))
            .bg(alpha(p.background_paper, 0.92))
            .invisible()
            .group_drag_over::<ExternalPaths>("sftp-drop", |s| s.visible())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.))
                    .px(px(24.))
                    .child(icon("UploadFileOutlined", 38., p.primary.main))
                    .child(
                        label("Upload files and folders")
                            .size(subtitle2.size)
                            .weight(subtitle2.weight)
                            .line_height(subtitle2.line_height)
                            .color(p.text.primary)
                            .no_truncate(),
                    )
                    .child(
                        muxus_ui::label::paragraph(format!("Drop into {}", self.current_path()))
                            .size(caption.size)
                            .line_height(caption.line_height)
                            .color(p.text.secondary)
                            .center(),
                    ),
            )
    }

    fn render_resize_grip(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let group: SharedString = "sftp-resize".into();
        let dragging = self.resize.is_some();
        let line = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(2.5))
            .w(px(3.))
            .bg(theme.palette.primary.main);
        let line = if dragging {
            line
        } else {
            line.invisible().group_hover(group.clone(), |s| s.visible())
        };
        div()
            .id("sftp-resize")
            .group(group)
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-4.))
            .w(px(8.))
            .cursor(CursorStyle::ResizeLeftRight)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let container = this
                        .container_width
                        .unwrap_or(muxus_ui::zoom::unzoom(window.viewport_size().width));
                    if event.click_count == 2 {
                        this.resize = None;
                        let width = clamp_sftp_panel_width(DEFAULT_SFTP_PANEL_WIDTH, container);
                        this.options.width = width;
                        cx.emit(SftpPanelEvent::WidthChanged(width));
                    } else {
                        this.resize = Some(ResizeDrag {
                            start_x: muxus_ui::zoom::unzoom(event.position.x),
                            start_width: this.options.width,
                            width: this.options.width,
                        });
                    }
                    cx.notify();
                }),
            )
            .child(line)
    }

    /// While resizing, follow the pointer anywhere in the window.
    fn render_resize_tracker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.resize.as_ref()?;
        let entity = cx.entity().downgrade();
        Some(
            canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    let moving = entity.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if !phase.bubble() {
                            return;
                        }
                        let _ = moving.update(cx, |this, cx| {
                            let container = this
                                .container_width
                                .unwrap_or(muxus_ui::zoom::unzoom(window.viewport_size().width));
                            if let Some(drag) = &mut this.resize {
                                // The grip is on the left edge: dragging left widens.
                                let delta = muxus_ui::zoom::unzoom(event.position.x) - drag.start_x;
                                drag.width =
                                    clamp_sftp_panel_width(drag.start_width - delta, container);
                                cx.notify();
                            }
                        });
                    });
                    let ending = entity.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                        if !phase.bubble() {
                            return;
                        }
                        let _ = ending.update(cx, |this, cx| {
                            if let Some(drag) = this.resize.take() {
                                this.options.width = drag.width;
                                cx.emit(SftpPanelEvent::WidthChanged(drag.width));
                                cx.notify();
                            }
                        });
                    });
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
    }
}

/// A file row being dragged out of the window.
struct DraggedFile {
    conn_id: String,
    ticket: String,
    name: String,
    /// The downloaded copy, once it is on disk.
    local: Rc<std::cell::RefCell<Option<PathBuf>>>,
}

impl DraggedFile {
    fn fetch(&self, cx: &mut App) {
        let backend = services(cx).backend;
        let (conn, ticket, name) = (self.conn_id.clone(), self.ticket.clone(), self.name.clone());
        let slot = self.local.clone();
        cx.spawn(async move |cx| {
            let Ok(stream) = backend.sftp_drag_download(&conn, &ticket).await else {
                return;
            };
            let Ok(bytes) = stream.read_all().await else {
                return;
            };
            let written = cx
                .background_executor()
                .spawn(async move {
                    use std::io::Write;
                    let root = super::local_open::local_copy_root();
                    let mut copy = super::local_open::create_local_copy(&root, &name).ok()?;
                    copy.handle.write_all(&bytes).ok()?;
                    Some(copy.file)
                })
                .await;
            *slot.borrow_mut() = written;
        })
        .detach();
    }
}

/// What follows the pointer while a file row is dragged.
struct DragPreview {
    name: String,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.theme().palette;
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .bg(p.background_paper)
            .border_1()
            .border_color(p.divider)
            .child(file_type_icon(&self.name, SftpEntryType::File))
            .child(
                label(self.name.clone())
                    .size(12.)
                    .line_height(17.16)
                    .color(p.text.primary)
                    .no_truncate(),
            )
    }
}

/// A small `IconButton` whose 17px glyph is mirrored (`transform: scaleX(-1)`).
fn mirrored_icon_button(
    id: &'static str,
    glyph: &'static str,
    tip: &'static str,
    theme: &Theme,
) -> muxus_ui::gpui_kit::Stateful<muxus_ui::gpui_kit::Div> {
    let ink = theme.palette.action.active;
    let hover = alpha(ink, theme.palette.action.hover_opacity);
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(27.))
        .rounded(px(13.5))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .tooltip(muxus_ui::widgets::Tooltip::text(tip))
        .child(icon(glyph, 17., ink).with_transformation(
            muxus_ui::gpui_kit::Transformation::scale(muxus_ui::gpui_kit::size(-1., 1.)),
        ))
}

/// `saveDownload`: the save dialog Electron shows for a download, then the file.
pub fn save_bytes(name: String, bytes: Vec<u8>, cx: &mut App) {
    let directory = downloads_dir();
    let target = crate::util::file_dialog::ask_save_path(&directory, &name, cx);
    cx.spawn(async move |cx| {
        let target = target.await;
        let Some(path) = target.path().map(Path::to_path_buf) else {
            return;
        };
        let written = std::fs::write(&path, &bytes);
        cx.update(|cx| match written {
            Ok(()) => notify(Status::Success, format!("Downloaded {name}"), cx),
            Err(error) => notify(Status::Error, format!("Could not save {name}: {error}"), cx),
        });
    })
    .detach();
}

/// The user's downloads folder (`XDG_DOWNLOAD_DIR`, else `~/Downloads`, else home).
pub fn downloads_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_DOWNLOAD_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

impl Render for SftpPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(value) = self.pending_input.take() {
            self.path_input
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let theme = *cx.theme();
        let p = theme.palette;
        let fills = self.options.fill || self.options.in_sidebar;
        let width = self
            .resize
            .as_ref()
            .map_or(self.options.width, |drag| drag.width);
        let measured = self
            .body_bounds
            .get()
            .map(|bounds| muxus_ui::zoom::unzoom(bounds.size.width));
        let inline_size = if fills {
            measured.unwrap_or(width)
        } else {
            width - 1.
        };
        let narrow = inline_size < NARROW_PANEL_WIDTH;
        let conn = self.options.conn_id.clone();
        let terminal_upload = services(cx).uploads.read(cx).get(&conn).cloned();

        let mut root = div()
            .id("sftp-panel")
            .key_context("SftpPanel")
            .track_focus(&self.focus)
            .group("sftp-drop")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .h_full()
            .bg(if self.options.in_sidebar {
                p.sidebar
            } else {
                p.background_paper
            })
            .font_family(fonts::UI)
            .text_color(p.text.primary)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.on_drop_paths(paths, window, cx)
            }));
        root = if fills {
            root.w(relative(1.))
        } else {
            root.w(px(width))
                .max_w(relative(0.7))
                .border_l_1()
                .border_color(p.divider)
                .child(self.render_resize_grip(&theme, cx))
        };
        root = root
            .child(self.render_header(&theme, cx))
            .child(self.render_toolbar(cx))
            .child(self.render_actions(&theme, narrow, cx));
        if let Some(transfer) = &self.transfer {
            root = root.child(
                div().mx(px(6.)).mb(px(6.)).child(
                    TransferProgress::new("sftp-transfer", transfer.state.clone())
                        .on_cancel(cx.listener(|this, _, _, cx| this.cancel_transfer(cx))),
                ),
            );
        }
        if let Some(upload) = terminal_upload {
            let conn = conn.clone();
            let mut bar = TransferProgress::new("sftp-terminal-upload", upload.transfer.clone())
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
            root = root.child(div().mx(px(6.)).mb(px(6.)).child(bar));
        }
        let probe = self.body_bounds.clone();
        let mut body = div().relative().flex_1().min_h_0().overflow_hidden().child(
            canvas(move |bounds, _, _| probe.set(Some(bounds)), |_, _, _, _| {})
                .absolute()
                .size_full(),
        );
        if (self.listing.fetching || self.busy) && self.transfer.is_none() {
            body = body.child(
                div()
                    .absolute()
                    .top(px(8.))
                    .right(px(12.))
                    .child(CircularProgress::new("sftp-spinner", 18., p.primary.main)),
            );
        }
        body = if let Some(error) = &self.listing.error {
            let body2 = theme.typography.body2;
            body.child(
                div().p(px(16.)).child(
                    muxus_ui::label::paragraph(error.clone())
                        .size(body2.size)
                        .line_height(body2.line_height)
                        .color(p.error.main),
                ),
            )
        } else {
            body.child(self.render_table(&theme, narrow, window, cx))
        };
        root = root.child(body);
        if self.options.follow_toggle {
            let follow = self.options.follow_terminal_folder;
            root = root.child(
                div()
                    .flex()
                    .items_center()
                    .px(px(6.))
                    .py(px(2.))
                    .border_t_1()
                    .border_color(p.divider)
                    .child(
                        Checkbox::new("sftp-follow", follow)
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_follow_terminal_folder(!follow, window, cx);
                                cx.emit(SftpPanelEvent::FollowTerminalFolder(!follow));
                            })),
                    )
                    .child(
                        label("Follow terminal folder")
                            .size(12.)
                            .line_height(12. * 1.5)
                            .color(p.text.primary)
                            .no_truncate(),
                    ),
            );
        }
        root = root.child(self.render_drop_overlay(&theme));
        if let Some(menu) = self.render_menu(cx) {
            root = root.child(menu);
        }
        if let Some(tracker) = self.render_resize_tracker(cx) {
            root = root.child(tracker);
        }
        if let Some((dialog, _)) = &self.open_with {
            root = root.child(dialog.clone());
        }
        if let Some(dialog) = self.dialogs.render(QUEUE, window, cx) {
            root = root.child(dialog);
        }
        root
    }
}
