//! `client/src/components/RemoteEditorWorkspace.tsx` with the editor of
//! `MonacoTextEditor.tsx`: a multi-file workspace attached to one terminal
//! session (or an SFTP window). With a terminal entry, the file strip leads
//! with that session's terminal and stays above it while the terminal is
//! shown; the files stay open behind it so their undo history survives.
//!
//! Monaco is replaced by gpui-component's code editor (tree-sitter
//! highlighting); the chrome around it (file strip, header, conflict alert,
//! status bar) and the save, dirty and conflict handling are the client's.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use muxus_core::api::{EditorFileResponse, EditorFileSaveRequest, EditorFileSaveResponse};
use muxus_session::ApiError;
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::base::input::{
    Editor, EditorState, InputEditorStyle, InputEvent, Position, TabSize,
};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Edges, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels,
    Point, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window,
    div, px, relative,
};
use muxus_ui::icon::icon;
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts};
use muxus_ui::widgets::menu::{self, MenuItem};
use muxus_ui::widgets::{Button, ButtonColor, IconButton, Tooltip};

use super::super::dialogs::{ConfirmOptions, DialogQueue, QueueOf};
use super::super::format::base_name;
use super::super::icons::file_type_icon;
use super::super::services::{notify, notify_error, services};
use super::language::{
    GENERAL_TEXT_LANGUAGE_ID, header_language_label, highlighter_language, language_for_path,
    language_label, language_options,
};
use super::registry::{RemoteEditorHandle, RemoteEditorRegistration, register_remote_editor};
use crate::util::code_editor::{BracketPools, MAX_HIGHLIGHT_BYTES, highlighter_factory};
use muxus_ui::syntax::MonacoColors;
use muxus_ui::widgets::progress::CircularProgress;

/// Where the files live (`sourceKind`, with the SFTP connection).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorSource {
    /// The local machine (`/api/local-files`).
    Local,
    /// An SSH connection; `None` while the session is disconnected.
    Sftp(Option<String>),
}

impl EditorSource {
    fn available(&self) -> bool {
        match self {
            EditorSource::Local => true,
            EditorSource::Sftp(conn) => conn.is_some(),
        }
    }

    fn local(&self) -> bool {
        *self == EditorSource::Local
    }
}

/// What the workspace asks of the tab that holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteEditorEvent {
    /// Bring this file to the front (`onActivate`).
    Activate(String),
    /// The file was closed (`onClose`); the tab drops it from its list.
    Close(String),
    /// Show the session's terminal (`onShowTerminal`).
    ShowTerminal,
}

/// How the workspace is shown.
#[derive(Clone, Debug)]
pub struct RemoteEditorOptions {
    /// The tab (or window) the workspace belongs to; the close-file chord and
    /// tab closing find it by this.
    pub tab_id: String,
    pub source: EditorSource,
    pub paths: Vec<String>,
    pub active_path: Option<String>,
    /// The workspace is in front of the focused pane, so it takes focus.
    pub active: bool,
    pub terminal_shown: bool,
    /// Lead the strip with the session's terminal (`onShowTerminal`).
    pub terminal_entry: bool,
    /// The chord that toggles between terminal and editor, for the tooltip.
    pub terminal_chord: Option<String>,
}

impl RemoteEditorOptions {
    pub fn new(tab_id: impl Into<String>, source: EditorSource) -> Self {
        Self {
            tab_id: tab_id.into(),
            source,
            paths: Vec::new(),
            active_path: None,
            active: false,
            terminal_shown: false,
            terminal_entry: false,
            terminal_chord: None,
        }
    }
}

/// `EditorDocument`: the text lives in the editor state.
struct Document {
    editor: Entity<EditorState>,
    saved_content: String,
    mtime_ms: Option<i64>,
    loading: bool,
    saving: bool,
    conflict: bool,
    error: Option<String>,
    dirty: bool,
    tab_size: usize,
    hard_tabs: bool,
    crlf: bool,
    wrap: bool,
    /// The theme mode and grammar the editor was last styled for.
    styled: Option<(bool, &'static str)>,
    _events: Subscription,
}

/// What the registry asks, answered from the workspace.
struct Handle {
    dirty: std::cell::Cell<bool>,
    file_in_front: std::cell::Cell<bool>,
    close: async_channel::Sender<()>,
}

impl RemoteEditorHandle for Handle {
    fn has_dirty(&self) -> bool {
        self.dirty.get()
    }

    fn close_active(&self) -> bool {
        if !self.file_in_front.get() {
            return false;
        }
        let _ = self.close.try_send(());
        true
    }
}

/// A status bar picker that is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusMenu {
    Indentation,
    LineEnding,
    Language,
}

/// The workspace.
pub struct RemoteEditorWorkspace {
    options: RemoteEditorOptions,
    documents: HashMap<String, Document>,
    language_overrides: HashMap<String, String>,
    dialogs: DialogQueue,
    handle: Rc<Handle>,
    _registration: RemoteEditorRegistration,
    /// `Ctrl/Cmd+K` was pressed; the next key may finish the chord.
    chord_started: Option<Instant>,
    status_menu: Option<(StatusMenu, Point<Pixels>)>,
    select_widths: Option<SelectWidths>,
    focus: FocusHandle,
    loads: HashMap<String, Task<()>>,
    /// The file the editor last took the keyboard for while in front.
    focused_for: Option<String>,
    _closer: Task<()>,
}

impl EventEmitter<RemoteEditorEvent> for RemoteEditorWorkspace {}

impl Focusable for RemoteEditorWorkspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

const QUEUE: QueueOf<RemoteEditorWorkspace> = |workspace| &mut workspace.dialogs;

/// `editorFileUrl`'s two sources.
async fn read_file(
    backend: &muxus_session::Backend,
    source: &EditorSource,
    path: &str,
) -> Result<EditorFileResponse, ApiError> {
    match source {
        EditorSource::Local => backend.read_local_file(path).await,
        EditorSource::Sftp(Some(conn)) => backend.sftp_read_file(conn, path).await,
        EditorSource::Sftp(None) => Err(ApiError::new(
            0,
            "The SSH session is disconnected. Reconnect to load this file.",
        )),
    }
}

async fn save_file(
    backend: &muxus_session::Backend,
    source: &EditorSource,
    path: &str,
    request: EditorFileSaveRequest,
) -> Result<EditorFileSaveResponse, ApiError> {
    match source {
        EditorSource::Local => backend.save_local_file(path, request).await,
        EditorSource::Sftp(Some(conn)) => backend.sftp_save_file(conn, path, request).await,
        EditorSource::Sftp(None) => Err(ApiError::new(
            0,
            "The SSH session is disconnected. Reconnect to save this file.",
        )),
    }
}

fn is_conflict(error: &ApiError) -> bool {
    error.status == 409
        && matches!(
            error.code.as_deref(),
            Some("SFTP_FILE_CHANGED" | "LOCAL_FILE_CHANGED")
        )
}

impl RemoteEditorWorkspace {
    pub fn new(options: RemoteEditorOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (close_tx, close_rx) = async_channel::unbounded::<()>();
        let handle = Rc::new(Handle {
            dirty: std::cell::Cell::new(false),
            file_in_front: std::cell::Cell::new(false),
            close: close_tx,
        });
        let registration = register_remote_editor(&options.tab_id, handle.clone());
        let closer = cx.spawn_in(window, async move |this, cx| {
            while close_rx.recv().await.is_ok() {
                let _ = this.update_in(cx, |this, window, cx| {
                    if let Some(path) = this.options.active_path.clone()
                        && !this.options.terminal_shown
                    {
                        this.close(path, window, cx);
                    }
                });
            }
        });
        let mut workspace = Self {
            options,
            documents: HashMap::new(),
            language_overrides: HashMap::new(),
            dialogs: DialogQueue::new(),
            handle,
            _registration: registration,
            chord_started: None,
            status_menu: None,
            select_widths: None,
            focus: cx.focus_handle(),
            loads: HashMap::new(),
            focused_for: None,
            _closer: closer,
        };
        workspace.sync(window, cx);
        workspace
    }

    /// The tab's files changed (`paths`, `activePath`).
    pub fn set_paths(
        &mut self,
        paths: Vec<String>,
        active_path: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.options.paths = paths;
        self.options.active_path = active_path;
        // Files no longer listed are gone (closed elsewhere).
        let open: std::collections::HashSet<&String> = self.options.paths.iter().collect();
        self.documents.retain(|path, _| open.contains(path));
        self.sync(window, cx);
        cx.notify();
    }

    pub fn set_terminal_shown(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.options.terminal_shown = shown;
        self.sync(window, cx);
        cx.notify();
    }

    pub fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.options.active = active;
        self.sync(window, cx);
        cx.notify();
    }

    /// The SSH session reconnected (new connection) or went away.
    pub fn set_source(&mut self, source: EditorSource, cx: &mut Context<Self>) {
        self.options.source = source;
        let readonly = !self.options.source.available();
        for document in self.documents.values() {
            document
                .editor
                .update(cx, |state, cx| state.set_readonly(readonly, cx));
        }
        cx.notify();
    }

    pub fn paths(&self) -> &[String] {
        &self.options.paths
    }

    pub fn active_path(&self) -> Option<&str> {
        self.options.active_path.as_deref()
    }

    /// Whether any open file has unsaved changes.
    pub fn has_dirty(&self) -> bool {
        self.documents.values().any(|document| document.dirty)
    }

    /// Open (or bring forward) a file, as the tab's `openEditor` does.
    pub fn open(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.options.paths.contains(&path) {
            self.options.paths.push(path.clone());
        }
        self.options.active_path = Some(path.clone());
        self.options.terminal_shown = false;
        self.sync(window, cx);
        cx.emit(RemoteEditorEvent::Activate(path));
        cx.notify();
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Typing rewrites the document; only whether the active file has been
        // opened at all decides whether it still needs loading.
        if let Some(path) = self.options.active_path.clone()
            && !self.documents.contains_key(&path)
        {
            self.load(path, window, cx);
        }
        self.handle
            .file_in_front
            .set(self.options.active_path.is_some() && !self.options.terminal_shown);
        self.handle.dirty.set(self.has_dirty());
        // The file takes the keyboard when it comes to the front (activated, revealed, done
        // loading), once: the shell syncs on every render, and taking it back each time
        // sent everything typed anywhere else in the window (the file browser's path, the
        // sidebar search) into the file.
        let showing = self.options.active && !self.options.terminal_shown;
        if !showing {
            self.focused_for = None;
        } else if let Some(path) = self.options.active_path.clone()
            && self.focused_for.as_deref() != Some(path.as_str())
            && let Some(document) = self.documents.get(&path)
            && !document.loading
            && !self.dialogs.is_open()
        {
            let focus = document.editor.read(cx).focus_handle(cx);
            if !focus.is_focused(window) {
                window.focus(&focus, cx);
            }
            self.focused_for = Some(path);
        }
    }

    fn new_editor(
        &mut self,
        path: &str,
        content: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<EditorState>, Subscription) {
        let language = self.language(path, content);
        let grammar = if content.len() > MAX_HIGHLIGHT_BYTES {
            "text"
        } else {
            highlighter_language(&language, path)
        };
        let readonly = !self.options.source.available();
        let value = content.to_string();
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language(grammar)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
                .soft_wrap(false)
                .default_value(value);
            state.set_highlighter_factory(highlighter_factory(BracketPools::PerKind), cx);
            state.set_readonly(readonly, cx);
            state
        });
        let watched = path.to_string();
        let events = cx.subscribe_in(&editor, window, move |this, editor, event, _, cx| {
            if let InputEvent::Change = event
                && let Some(document) = this.documents.get_mut(&watched)
            {
                let text = editor.read(cx).value();
                document.dirty = text.as_ref() != document.saved_content;
                document.conflict = false;
                this.handle.dirty.set(this.has_dirty());
                cx.notify();
            }
        });
        cx.observe(&editor, |_, _, cx| cx.notify()).detach();
        (editor, events)
    }

    /// Monaco's colors for the file's grammar. Restyling hands the editor a new token
    /// resolver, which recolors every line: done only when the theme or the grammar changes,
    /// not on every render (that, and detecting the language on the whole text each time,
    /// made typing lag and fast keystrokes go missing).
    fn style_editor(&mut self, path: &str, editor: &Entity<EditorState>, cx: &mut App) {
        let dark = cx.theme().mode.is_dark();
        let grammar = {
            // Only the first line (a shebang) says anything the path does not.
            let first_line = {
                use muxus_ui::gpui_kit::base::input::RopeExt as _;
                editor.read(cx).text().slice_line(0).to_string()
            };
            let language = self.language(path, &first_line);
            highlighter_language(&language, path)
        };
        let Some(document) = self.documents.get_mut(path) else {
            return;
        };
        if document.styled == Some((dark, grammar)) {
            return;
        }
        document.styled = Some((dark, grammar));
        let base = muxus_ui::syntax::code_editor_style(dark, grammar);
        let style = InputEditorStyle {
            fold_icon_renderer: Some(crate::util::code_editor::mouseover_fold_icons(dark)),
            ..base
        };
        editor.update(cx, |state, _| {
            state.set_editor_style(style);
            // Monaco: 12px above the first line, line numbers ending 54px in
            // and the text starting 25px after them; the engine keeps 24px
            // between them.
            // (GPUI sets the first line a pixel lower than Chrome does.)
            state.set_editor_paddings(Edges {
                top: px(11.),
                right: px(14.),
                bottom: px(12.),
                left: px(34.),
            });
        });
    }

    fn language(&self, path: &str, content: &str) -> String {
        self.language_overrides
            .get(path)
            .cloned()
            .unwrap_or_else(|| language_for_path(path, content).to_string())
    }

    /// `load`: read the file, or say why it cannot be.
    fn load(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.options.source.clone();
        if !self.documents.contains_key(&path) {
            let (editor, events) = self.new_editor(&path, "", window, cx);
            self.documents.insert(
                path.clone(),
                Document {
                    editor,
                    saved_content: String::new(),
                    mtime_ms: None,
                    loading: true,
                    saving: false,
                    conflict: false,
                    error: None,
                    dirty: false,
                    tab_size: 2,
                    hard_tabs: false,
                    crlf: false,
                    wrap: false,
                    styled: None,
                    _events: events,
                },
            );
        }
        let document = self.documents.get_mut(&path).expect("just inserted");
        document.loading = true;
        document.conflict = false;
        document.error = None;
        if !source.available() {
            document.loading = false;
            document.error =
                Some("The SSH session is disconnected. Reconnect to load this file.".into());
            cx.notify();
            return;
        }
        let backend = services(cx).backend;
        let task_path = path.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = read_file(&backend, &source, &task_path).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loads.remove(&task_path);
                let Some(document) = this.documents.get_mut(&task_path) else {
                    return;
                };
                document.loading = false;
                match result {
                    Ok(file) => {
                        document.saved_content = file.content.clone();
                        document.mtime_ms = file.mtime_ms;
                        document.dirty = false;
                        document.crlf = file.content.contains("\r\n");
                        let editor = document.editor.clone();
                        let content = file.content;
                        editor.update(cx, |state, cx| state.set_value(content.clone(), window, cx));
                        // The language follows the contents (a shebang) once they are here.
                        let language = this.language(&task_path, &content);
                        let grammar = if content.len() > MAX_HIGHLIGHT_BYTES {
                            "text"
                        } else {
                            highlighter_language(&language, &task_path)
                        };
                        editor.update(cx, |state, cx| state.set_highlighter(grammar, cx));
                        if let Some(document) = this.documents.get_mut(&task_path) {
                            document.dirty = false;
                        }
                    }
                    Err(error) => document.error = Some(error.message),
                }
                this.handle.dirty.set(this.has_dirty());
                this.sync(window, cx);
                cx.notify();
            });
        });
        self.loads.insert(path, task);
        cx.notify();
    }

    fn dirty_paths(&self) -> Vec<String> {
        self.options
            .paths
            .iter()
            .filter(|path| self.documents.get(*path).is_some_and(|doc| doc.dirty))
            .cloned()
            .collect()
    }

    fn saving_count(&self) -> usize {
        self.documents.values().filter(|doc| doc.saving).count()
    }

    /// `save`: resolves whether the file was saved.
    fn save(
        &mut self,
        path: String,
        force: bool,
        announce: bool,
        cx: &mut Context<Self>,
    ) -> Option<Task<bool>> {
        let source = self.options.source.clone();
        let document = self.documents.get_mut(&path)?;
        if !source.available() || document.loading || document.saving {
            return None;
        }
        let content = document.editor.read(cx).value().to_string();
        document.saving = true;
        document.conflict = false;
        let request = EditorFileSaveRequest {
            content: content.clone(),
            expected_mtime_ms: document.mtime_ms,
            force: Some(force),
        };
        cx.notify();
        let backend = services(cx).backend;
        Some(cx.spawn(async move |this, cx| {
            let result = save_file(&backend, &source, &path, request).await;
            this.update(cx, |this, cx| {
                let saved = {
                    let Some(document) = this.documents.get_mut(&path) else {
                        return false;
                    };
                    document.saving = false;
                    match result {
                        Ok(response) => {
                            document.saved_content = content.clone();
                            document.mtime_ms = response.mtime_ms;
                            document.conflict = false;
                            document.error = None;
                            let current = document.editor.read(cx).value();
                            document.dirty = current.as_ref() != content;
                            if announce {
                                notify(Status::Success, format!("Saved {}", base_name(&path)), cx);
                            }
                            true
                        }
                        Err(error) if is_conflict(&error) => {
                            document.conflict = true;
                            false
                        }
                        Err(error) => {
                            notify_error(&error, cx);
                            false
                        }
                    }
                };
                this.handle.dirty.set(this.has_dirty());
                cx.notify();
                saved
            })
            .unwrap_or(false)
        }))
    }

    fn save_active(&mut self, force: bool, cx: &mut Context<Self>) {
        if let Some(path) = self.options.active_path.clone()
            && let Some(task) = self.save(path, force, true, cx)
        {
            task.detach();
        }
    }

    /// `saveAll`: one toast for however many files were saved.
    fn save_all(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<String> = self
            .dirty_paths()
            .into_iter()
            .filter(|path| !self.documents.get(path).is_some_and(|doc| doc.saving))
            .collect();
        if paths.is_empty() {
            return;
        }
        let first = paths[0].clone();
        let tasks: Vec<Task<bool>> = paths
            .into_iter()
            .filter_map(|path| self.save(path, false, false, cx))
            .collect();
        cx.spawn(async move |_, cx| {
            let mut saved = 0;
            for task in tasks {
                if task.await {
                    saved += 1;
                }
            }
            if saved > 0 {
                cx.update(|cx| {
                    let message = if saved == 1 {
                        format!("Saved {}", base_name(&first))
                    } else {
                        format!("Saved {saved} files")
                    };
                    notify(Status::Success, message, cx)
                });
            }
        })
        .detach();
    }

    /// `close`: asks first when the file has unsaved changes.
    pub fn close(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let dirty = self.documents.get(&path).is_some_and(|doc| doc.dirty);
        if !dirty {
            self.drop_file(path, window, cx);
            return;
        }
        let answer = self.dialogs.confirm(
            ConfirmOptions {
                title: format!("Discard changes to {}?", base_name(&path)),
                description: Some(
                    "This file has unsaved changes. Closing it now loses those edits.".into(),
                ),
                confirm_label: "Discard changes".into(),
                destructive: true,
                ..ConfirmOptions::default()
            },
            window,
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.confirmed {
                let _ = this.update_in(cx, |this, window, cx| this.drop_file(path, window, cx));
            }
        })
        .detach();
    }

    fn drop_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.documents.remove(&path);
        self.language_overrides.remove(&path);
        self.loads.remove(&path);
        // As the tab store does: the neighbour takes its place.
        if let Some(index) = self.options.paths.iter().position(|p| *p == path) {
            self.options.paths.remove(index);
            if self.options.active_path.as_deref() == Some(path.as_str()) {
                self.options.active_path = self
                    .options
                    .paths
                    .get(index.min(self.options.paths.len().saturating_sub(1)))
                    .cloned();
            }
        }
        self.sync(window, cx);
        cx.emit(RemoteEditorEvent::Close(path));
        cx.notify();
    }

    fn activate(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.options.active_path = Some(path.clone());
        self.options.terminal_shown = false;
        self.sync(window, cx);
        cx.emit(RemoteEditorEvent::Activate(path));
        cx.notify();
    }

    fn show_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.options.terminal_shown = true;
        self.sync(window, cx);
        cx.emit(RemoteEditorEvent::ShowTerminal);
        cx.notify();
    }

    fn activate_relative(&mut self, offset: isize, window: &mut Window, cx: &mut Context<Self>) {
        let paths = &self.options.paths;
        let Some(active) = &self.options.active_path else {
            return;
        };
        if paths.len() < 2 {
            return;
        }
        let index = paths.iter().position(|p| p == active).unwrap_or(0) as isize;
        let next = (index + offset).rem_euclid(paths.len() as isize) as usize;
        let path = paths[next].clone();
        self.activate(path, window, cx);
    }

    /// The strip's arrow keys reach the terminal entry ahead of the files.
    fn strip_entries(&self) -> Vec<Option<String>> {
        let mut entries: Vec<Option<String>> = Vec::new();
        if self.options.terminal_entry {
            entries.push(None);
        }
        entries.extend(self.options.paths.iter().cloned().map(Some));
        entries
    }

    fn strip_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let entries = self.strip_entries();
        if entries.is_empty() {
            return false;
        }
        let selected = if self.options.terminal_shown {
            None
        } else {
            self.options.active_path.clone()
        };
        let index = entries.iter().position(|e| *e == selected).unwrap_or(0);
        let target = match key {
            "left" | "right" => {
                if entries.len() < 2 {
                    return true;
                }
                let offset: isize = if key == "left" { -1 } else { 1 };
                let next = (index as isize + offset).rem_euclid(entries.len() as isize) as usize;
                entries[next].clone()
            }
            "home" => entries[0].clone(),
            "end" => entries[entries.len() - 1].clone(),
            _ => return false,
        };
        match target {
            None => self.show_terminal(window, cx),
            Some(path) => self.activate(path, window, cx),
        }
        true
    }

    /// The editor's commands (`editor.addCommand`): Ctrl/Cmd+S, Ctrl/Cmd+K S,
    /// Ctrl/Cmd+W, Ctrl/Cmd+PageDown/PageUp, Alt+Z.
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.options.terminal_shown || self.options.active_path.is_none() {
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let primary = modifiers.platform || modifiers.control;
        let key = keystroke.key.as_str();
        if let Some(started) = self.chord_started.take()
            && started.elapsed() < Duration::from_secs(5)
            && key == "s"
            && !modifiers.alt
        {
            cx.stop_propagation();
            self.save_all(cx);
            return;
        }
        match (primary, modifiers.shift, modifiers.alt, key) {
            (true, false, false, "s") => {
                cx.stop_propagation();
                self.save_active(false, cx);
            }
            (true, false, false, "k") => {
                cx.stop_propagation();
                self.chord_started = Some(Instant::now());
            }
            (true, false, false, "w") => {
                cx.stop_propagation();
                if let Some(path) = self.options.active_path.clone() {
                    self.close(path, window, cx);
                }
            }
            (true, false, false, "pagedown") => {
                cx.stop_propagation();
                self.activate_relative(1, window, cx);
            }
            (true, false, false, "pageup") => {
                cx.stop_propagation();
                self.activate_relative(-1, window, cx);
            }
            (false, false, true, "z") => {
                cx.stop_propagation();
                self.toggle_wrap(window, cx);
            }
            _ => {}
        }
    }

    fn toggle_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(document) = self.active_document_mut() {
            document.wrap = !document.wrap;
            let wrap = document.wrap;
            document
                .editor
                .clone()
                .update(cx, |state, cx| state.set_soft_wrap(wrap, window, cx));
            cx.notify();
        }
    }

    fn active_document_mut(&mut self) -> Option<&mut Document> {
        let path = self.options.active_path.clone()?;
        self.documents.get_mut(&path)
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.options.active_path.clone() else {
            return;
        };
        let local = self.options.source.local();
        if !self.documents.get(&path).is_some_and(|doc| doc.dirty) {
            self.load(path, window, cx);
            return;
        }
        let answer = self.dialogs.confirm(
            ConfirmOptions {
                title: if local {
                    "Reload from disk?".into()
                } else {
                    "Reload from remote?".into()
                },
                description: Some("Your unsaved changes to this file are discarded.".into()),
                confirm_label: "Discard and reload".into(),
                destructive: true,
                ..ConfirmOptions::default()
            },
            window,
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.confirmed {
                let _ = this.update_in(cx, |this, window, cx| this.load(path, window, cx));
            }
        })
        .detach();
    }

    fn reload_conflict(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.options.active_path.clone() else {
            return;
        };
        let local = self.options.source.local();
        let answer = self.dialogs.confirm(
            ConfirmOptions {
                title: if local {
                    "Load the version on disk?".into()
                } else {
                    "Load the remote version?".into()
                },
                description: Some("Your unsaved changes to this file are discarded.".into()),
                confirm_label: "Discard and reload".into(),
                destructive: true,
                ..ConfirmOptions::default()
            },
            window,
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.confirmed {
                let _ = this.update_in(cx, |this, window, cx| this.load(path, window, cx));
            }
        })
        .detach();
    }

    fn download(&mut self, cx: &mut Context<Self>) {
        let (Some(path), EditorSource::Sftp(Some(conn))) = (
            self.options.active_path.clone(),
            self.options.source.clone(),
        ) else {
            return;
        };
        let backend = services(cx).backend;
        cx.spawn(async move |_, cx| {
            let result = async {
                let stream = backend.sftp_download(&conn, &path).await?;
                stream.read_all().await
            }
            .await;
            cx.update(|cx| match result {
                Ok(bytes) => super::super::panel::save_bytes(base_name(&path), bytes, cx),
                Err(error) => notify_error(&error, cx),
            });
        })
        .detach();
    }

    fn set_language(&mut self, language: &str, cx: &mut Context<Self>) {
        let Some(path) = self.options.active_path.clone() else {
            return;
        };
        self.language_overrides
            .insert(path.clone(), language.to_string());
        if let Some(document) = self.documents.get(&path) {
            let grammar = highlighter_language(language, &path);
            document
                .editor
                .update(cx, |state, cx| state.set_highlighter(grammar, cx));
        }
        cx.notify();
    }

    fn set_indentation(&mut self, choice: Option<usize>, cx: &mut Context<Self>) {
        if let Some(document) = self.active_document_mut() {
            match choice {
                None => document.hard_tabs = true,
                Some(size) => {
                    document.hard_tabs = false;
                    document.tab_size = size;
                }
            }
            let tab = TabSize {
                tab_size: document.tab_size,
                hard_tabs: document.hard_tabs,
            };
            document
                .editor
                .clone()
                .update(cx, |state, cx| state.set_tab_size(tab, cx));
            cx.notify();
        }
    }

    fn set_line_ending(&mut self, crlf: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(document) = self.active_document_mut() {
            if document.crlf == crlf {
                return;
            }
            document.crlf = crlf;
            let editor = document.editor.clone();
            let text = editor.read(cx).value().to_string();
            let normalized = text.replace("\r\n", "\n");
            let next = if crlf {
                normalized.replace('\n', "\r\n")
            } else {
                normalized
            };
            editor.update(cx, |state, cx| state.set_value(next, window, cx));
            cx.notify();
        }
    }

    // --- rendering --------------------------------------------------------------------------

    fn strip_tab(
        &self,
        id: ElementId,
        active: bool,
        theme: &Theme,
    ) -> muxus_ui::gpui_kit::Stateful<muxus_ui::gpui_kit::Div> {
        let p = theme.palette;
        let mut tab = div()
            .id(id)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .min_w(px(120.))
            .max_w(px(220.))
            .h_full()
            .px(px(10.))
            .cursor_pointer()
            .border_r_1()
            .border_color(p.divider);
        if active {
            tab = tab.bg(p.background_default).child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(2.))
                    .bg(p.primary.main),
            );
        }
        tab
    }

    fn render_strip(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme.palette;
        let selected = if self.options.terminal_shown {
            None
        } else {
            self.options.active_path.clone()
        };
        let mut strip = div()
            .id("editor-strip")
            .flex()
            .flex_none()
            .h(px(36.))
            .bg(p.sidebar)
            .border_b_1()
            .border_color(p.divider)
            .overflow_x_scroll();
        if self.options.terminal_entry {
            let title = match &self.options.terminal_chord {
                Some(chord) => format!("Terminal ({chord})"),
                None => "Terminal".into(),
            };
            strip = strip.child(
                self.strip_tab("editor-terminal".into(), self.options.terminal_shown, theme)
                    .tooltip(Tooltip::text(title))
                    .on_click(cx.listener(|this, _, window, cx| this.show_terminal(window, cx)))
                    .child(icon("Terminal", 18., p.text.secondary))
                    .child(
                        label("Terminal")
                            .size(12.5)
                            .line_height(12.5 * 1.43)
                            .color(p.text.primary)
                            .grow(),
                    ),
            );
        }
        for (index, path) in self.options.paths.iter().enumerate() {
            let active = selected.as_deref() == Some(path.as_str());
            let dirty = self.documents.get(path).is_some_and(|doc| doc.dirty);
            let name = base_name(path);
            let group: SharedString = format!("editor-tab-{index}").into();
            let activate_path = path.clone();
            let close_path = path.clone();
            let middle_path = path.clone();
            let close_glyph = {
                let glyph = div().child(icon("Close", 14., p.action.active));
                if dirty {
                    glyph
                        .invisible()
                        .group_hover(group.clone(), |s| s.visible())
                } else {
                    glyph
                }
            };
            let mut close = div()
                .id(ElementId::NamedInteger("editor-close".into(), index as u64))
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(18.))
                .rounded(px(9.))
                .hover(|s| s.bg(p.action.hover))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close(close_path.clone(), window, cx);
                }))
                .child(close_glyph);
            if dirty {
                // The dot marks unsaved work at rest and steps aside for the ×
                // on hover, so closing is never a control you cannot see.
                close = close.child(
                    div()
                        .absolute()
                        .top(px(5.))
                        .left(px(5.))
                        .size(px(8.))
                        .rounded(px(4.))
                        .bg(p.text.secondary)
                        .group_hover(group.clone(), |s| s.invisible()),
                );
            }
            strip = strip.child(
                self.strip_tab(
                    ElementId::NamedInteger("editor-tab".into(), index as u64),
                    active,
                    theme,
                )
                .group(group)
                .tooltip(Tooltip::text(path.clone()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.activate(activate_path.clone(), window, cx)
                }))
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        this.close(middle_path.clone(), window, cx)
                    }),
                )
                .child(file_type_icon(&name, muxus_core::api::SftpEntryType::File))
                .child(
                    label(name)
                        .size(12.5)
                        .line_height(12.5 * 1.43)
                        .color(p.text.primary)
                        .grow(),
                )
                .child(close),
            );
        }
        strip
    }

    fn render_header(&self, path: &str, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme.palette;
        let caption = theme.typography.caption;
        let local = self.options.source.local();
        let available = self.options.source.available();
        let document = self.documents.get(path);
        let loading = document.is_none_or(|doc| doc.loading);
        let saving = document.is_some_and(|doc| doc.saving);
        let dirty = document.is_some_and(|doc| doc.dirty);
        let any_dirty = self.has_dirty();
        let saving_count = self.saving_count();
        let language = document
            .map(|doc| self.language(path, &doc.editor.read(cx).value()))
            .unwrap_or_else(|| GENERAL_TEXT_LANGUAGE_ID.into());
        let name = base_name(path);
        let mut header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .min_h(px(38.))
            .px(px(10.))
            .border_b_1()
            .border_color(p.divider)
            .child(file_type_icon(&name, muxus_core::api::SftpEntryType::File))
            .child(
                div()
                    .id("editor-path")
                    .flex_1()
                    .min_w_0()
                    .tooltip(Tooltip::text(path.to_string()))
                    .child(
                        label(path.to_string())
                            .family(fonts::MONO)
                            .size(caption.size)
                            .line_height(caption.line_height)
                            .color(p.text.secondary),
                    ),
            )
            .child(
                div().mr(px(4.)).child(
                    label(header_language_label(&language))
                        .size(caption.size)
                        .line_height(caption.line_height)
                        .color(p.text.disabled)
                        .no_truncate(),
                ),
            )
            .child(
                IconButton::new("editor-reload", "Refresh")
                    .tooltip(if local {
                        "Reload from disk"
                    } else {
                        "Reload from remote"
                    })
                    .disabled(loading && document.is_some_and(|doc| doc.loading))
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
            );
        if !local {
            header = header.child(
                IconButton::new("editor-download", "DownloadOutlined")
                    .tooltip("Download")
                    .disabled(!available)
                    .on_click(cx.listener(|this, _, _, cx| this.download(cx))),
            );
        }
        let save: AnyElement = if saving {
            div()
                .size(px(28.57))
                .flex()
                .items_center()
                .justify_center()
                .child(CircularProgress::new("editor-saving", 16., p.primary.main))
                .into_any_element()
        } else {
            IconButton::new("editor-save", "SaveOutlined")
                .tooltip("Save (Ctrl/Cmd+S)")
                .color(if dirty {
                    ButtonColor::Primary
                } else {
                    ButtonColor::Default
                })
                .disabled(document.is_none() || loading || !available)
                .on_click(cx.listener(|this, _, _, cx| this.save_active(false, cx)))
                .into_any_element()
        };
        let save_all: AnyElement = if saving_count > 0 {
            div()
                .size(px(28.57))
                .flex()
                .items_center()
                .justify_center()
                .child(CircularProgress::new(
                    "editor-saving-all",
                    16.,
                    p.primary.main,
                ))
                .into_any_element()
        } else {
            IconButton::new("editor-save-all", "SaveAsOutlined")
                .tooltip("Save all (Ctrl/Cmd+K, S)")
                .color(if any_dirty {
                    ButtonColor::Primary
                } else {
                    ButtonColor::Default
                })
                .disabled(!any_dirty || !available)
                .on_click(cx.listener(|this, _, _, cx| this.save_all(cx)))
                .into_any_element()
        };
        let close_path = path.to_string();
        header.child(save).child(save_all).child(
            IconButton::new("editor-close-file", "Close").on_click(
                cx.listener(move |this, _, window, cx| this.close(close_path.clone(), window, cx)),
            ),
        )
    }

    fn render_conflict(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let local = self.options.source.local();
        let p = theme.palette;
        let warning = p.warning;
        let body2 = theme.typography.body2;
        let dark = theme.mode.is_dark();
        // MUI's standard warning alert, square, with the two choices.
        let (bg, fg) = if dark {
            (
                muxus_ui::color::darken(warning.main, 0.9),
                muxus_ui::color::lighten(warning.main, 0.6),
            )
        } else {
            (
                muxus_ui::color::lighten(warning.main, 0.9),
                muxus_ui::color::darken(warning.main, 0.6),
            )
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .px(px(16.))
            .py(px(2.))
            .bg(bg)
            .text_color(fg)
            .child(div().flex().py(px(7.)).mr(px(12.)).child(icon(
                "ReportProblemOutlined",
                22.,
                warning.main,
            )))
            .child(
                div().flex_1().py(px(8.)).child(
                    paragraph(if local {
                        "The file changed on disk after you opened it."
                    } else {
                        "The remote file changed after you opened it."
                    })
                    .size(body2.size)
                    .line_height(body2.line_height)
                    .color(fg),
                ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.))
                    .child(
                        Button::new(
                            "editor-conflict-reload",
                            if local {
                                "Reload disk"
                            } else {
                                "Reload remote"
                            },
                        )
                        .color(ButtonColor::Inherit)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.reload_conflict(window, cx)),
                        ),
                    )
                    .child(
                        Button::new(
                            "editor-conflict-overwrite",
                            if local {
                                "Overwrite disk"
                            } else {
                                "Overwrite remote"
                            },
                        )
                        .contained()
                        .color(ButtonColor::Warning)
                        .on_click(cx.listener(|this, _, _, cx| this.save_active(true, cx))),
                    ),
            )
    }

    fn status_text(&self, cx: &App) -> Option<StatusLine> {
        let path = self.options.active_path.as_ref()?;
        let document = self.documents.get(path)?;
        let state = document.editor.read(cx);
        let Position { line, character } = state.cursor_position();
        let selected = state.selected_text().len_chars();
        let language = self.language(path, &state.value());
        Some(StatusLine {
            line: line as usize + 1,
            column: character as usize + 1,
            selected,
            tab_size: document.tab_size,
            hard_tabs: document.hard_tabs,
            crlf: document.crlf,
            wrap: document.wrap,
            language,
        })
    }

    fn render_status(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let widths = *self.select_widths.get_or_insert_with(|| {
            let measure = |options: &mut dyn Iterator<Item = &str>, window: &mut Window| {
                options
                    .map(|text| muxus_ui::text::measure_text(window, text, fonts::UI, 11., 400.))
                    .fold(0., f32::max)
            };
            SelectWidths {
                indent: measure(
                    &mut ["Tabs", "Spaces: 2", "Spaces: 4", "Spaces: 8"].into_iter(),
                    window,
                ),
                eol: measure(&mut ["LF", "CRLF"].into_iter(), window),
                language: measure(
                    &mut language_options().iter().map(|(_, name)| *name),
                    window,
                ),
            }
        });
        let dark = theme.mode.is_dark();
        let colors = MonacoColors::new(dark);
        let (bg, fg) = (colors.footer_background, colors.footer_ink);
        let (border, hover) = if dark {
            (
                muxus_ui::color::rgba8(255, 255, 255, 0.09),
                muxus_ui::color::rgba8(255, 255, 255, 0.1),
            )
        } else {
            (
                muxus_ui::color::rgba8(0, 0, 0, 0.1),
                muxus_ui::color::rgba8(0, 0, 0, 0.08),
            )
        };
        let readonly = !self.options.source.available();
        let status = self.status_text(cx);
        let text = |value: String| {
            label(value)
                .size(11.)
                .line_height(22.)
                .color(fg)
                .no_truncate()
        };
        let item = |id: &'static str, content: String| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .h(px(22.))
                .px(px(6.))
                .rounded(px(3.))
                .child(text(content))
        };
        // A native select is as wide as its widest option (and the arrow's room).
        let select = |id: &'static str, content: String, widest: f32, max: f32| {
            div()
                .id(id)
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .h(px(22.))
                .w(px((widest + 22.).min(max)))
                .pl(px(5.))
                .pr(px(17.))
                .rounded(px(3.))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(text(content))
                .child(div().absolute().right(px(2.)).top(px(4.)).child(icon(
                    "KeyboardArrowDown",
                    14.,
                    fg,
                )))
        };
        let mut bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .h(px(24.))
            .overflow_hidden()
            .border_t_1()
            .border_color(border)
            .bg(bg)
            .font_family(fonts::UI);
        let Some(status) = status else {
            return bar;
        };
        bar = bar.child(
            item(
                "editor-status-position",
                format!("Ln {}, Col {}", status.line, status.column),
            )
            .tooltip(Tooltip::text("Go to line (Ctrl/Cmd+G)")),
        );
        if status.selected > 0 {
            bar = bar.child(item(
                "editor-status-selection",
                format!("{} selected", status.selected),
            ));
        }
        bar = bar.child(div().flex_1().min_w(px(8.)));
        let wrap = div()
            .id("editor-status-wrap")
            .flex()
            .flex_none()
            .items_center()
            .h(px(22.))
            .px(px(6.))
            .rounded(px(3.))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .when_wrap(status.wrap, hover)
            .tooltip(Tooltip::text("Toggle word wrap (Alt+Z)"))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_wrap(window, cx)))
            .child(text("Wrap".into()));
        let format = div()
            .id("editor-status-format")
            .flex()
            .flex_none()
            .items_center()
            .h(px(22.))
            .px(px(6.))
            .rounded(px(3.))
            .when_disabled(readonly)
            .when_hoverable(!readonly, hover)
            .tooltip(Tooltip::text("Format document (Shift+Alt+F)"))
            .child(text("{ }".into()));
        let indentation = if status.hard_tabs {
            "Tabs".to_string()
        } else {
            format!("Spaces: {}", status.tab_size)
        };
        bar.child(wrap)
            .child(format)
            .child(
                select("editor-status-indent", indentation, widths.indent, 112.)
                    .tooltip(Tooltip::text("Select indentation"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &muxus_ui::gpui_kit::MouseDownEvent, _, cx| {
                            this.status_menu = Some((StatusMenu::Indentation, event.position));
                            cx.notify();
                        }),
                    ),
            )
            .child(
                select(
                    "editor-status-eol",
                    if status.crlf { "CRLF" } else { "LF" }.into(),
                    widths.eol,
                    112.,
                )
                .tooltip(Tooltip::text("Select end of line sequence"))
                .when_disabled(readonly)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(
                        move |this, event: &muxus_ui::gpui_kit::MouseDownEvent, _, cx| {
                            if !readonly {
                                this.status_menu = Some((StatusMenu::LineEnding, event.position));
                                cx.notify();
                            }
                        },
                    ),
                ),
            )
            .child(
                item("editor-status-encoding", "UTF-8".into())
                    .tooltip(Tooltip::text("File encoding")),
            )
            .child(
                select(
                    "editor-status-language",
                    language_label(&status.language).to_string(),
                    widths.language,
                    138.,
                )
                .tooltip(Tooltip::text("Select language mode"))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &muxus_ui::gpui_kit::MouseDownEvent, _, cx| {
                        this.status_menu = Some((StatusMenu::Language, event.position));
                        cx.notify();
                    }),
                ),
            )
    }

    fn render_status_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (menu_kind, position) = self.status_menu?;
        let status = self.status_text(cx)?;
        let entity = cx.entity().downgrade();
        let mut items: Vec<AnyElement> = Vec::new();
        match menu_kind {
            StatusMenu::Indentation => {
                for (index, (label_text, choice)) in [
                    ("Tabs", None),
                    ("Spaces: 2", Some(2)),
                    ("Spaces: 4", Some(4)),
                    ("Spaces: 8", Some(8)),
                ]
                .into_iter()
                .enumerate()
                {
                    let selected = match choice {
                        None => status.hard_tabs,
                        Some(size) => !status.hard_tabs && status.tab_size == size,
                    };
                    items.push(
                        MenuItem::new(
                            ElementId::NamedInteger("indent".into(), index as u64),
                            label_text,
                        )
                        .dense()
                        .selected(selected)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.status_menu = None;
                            this.set_indentation(choice, cx);
                        }))
                        .into_any_element(),
                    );
                }
            }
            StatusMenu::LineEnding => {
                for (index, crlf) in [false, true].into_iter().enumerate() {
                    items.push(
                        MenuItem::new(
                            ElementId::NamedInteger("eol".into(), index as u64),
                            if crlf { "CRLF" } else { "LF" },
                        )
                        .dense()
                        .selected(status.crlf == crlf)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.status_menu = None;
                            this.set_line_ending(crlf, window, cx);
                        }))
                        .into_any_element(),
                    );
                }
            }
            StatusMenu::Language => {
                for (index, (id, name)) in language_options().iter().enumerate() {
                    let id = *id;
                    items.push(
                        MenuItem::new(
                            ElementId::NamedInteger("language".into(), index as u64),
                            *name,
                        )
                        .dense()
                        .selected(status.language == id)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.status_menu = None;
                            this.set_language(id, cx);
                        }))
                        .into_any_element(),
                    );
                }
            }
        }
        let content = menu::paper(theme)
            .id("editor-status-menu")
            .max_h(px(320.))
            .overflow_y_scroll()
            .children(items);
        Some(
            menu::popup_at(
                position,
                muxus_ui::gpui_kit::Anchor::BottomLeft,
                move |_, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.status_menu = None;
                        cx.notify();
                    });
                },
                content,
            )
            .into_any_element(),
        )
    }
}

/// The status bar selects' natural widths.
#[derive(Clone, Copy)]
struct SelectWidths {
    indent: f32,
    eol: f32,
    language: f32,
}

/// What the status bar shows for the file in front.
struct StatusLine {
    line: usize,
    column: usize,
    selected: usize,
    tab_size: usize,
    hard_tabs: bool,
    crlf: bool,
    wrap: bool,
    language: String,
}

trait StatusStyle: Sized {
    fn when_wrap(self, on: bool, color: muxus_ui::gpui_kit::Hsla) -> Self;
    fn when_disabled(self, disabled: bool) -> Self;
    fn when_hoverable(self, hoverable: bool, color: muxus_ui::gpui_kit::Hsla) -> Self;
}

impl StatusStyle for muxus_ui::gpui_kit::Stateful<muxus_ui::gpui_kit::Div> {
    fn when_wrap(self, on: bool, color: muxus_ui::gpui_kit::Hsla) -> Self {
        if on { self.bg(color) } else { self }
    }

    fn when_disabled(self, disabled: bool) -> Self {
        if disabled {
            self.opacity(0.55).cursor_default()
        } else {
            self
        }
    }

    fn when_hoverable(self, hoverable: bool, color: muxus_ui::gpui_kit::Hsla) -> Self {
        if hoverable {
            self.cursor_pointer().hover(move |s| s.bg(color))
        } else {
            self
        }
    }
}

impl Render for RemoteEditorWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let terminal_shown = self.options.terminal_shown;
        let active_path = self.options.active_path.clone();
        if let Some(path) = &active_path
            && let Some(document) = self.documents.get(path)
        {
            let editor = document.editor.clone();
            self.style_editor(path, &editor, cx);
        }
        let mut root = div()
            .id("remote-editor")
            .key_context("RemoteEditor")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                // Arrow keys move along the strip when it has focus.
                if this.focus.is_focused(window)
                    && this.strip_key(event.keystroke.key.as_str(), window, cx)
                {
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .min_w_0()
            .bg(p.background_default)
            .font_family(fonts::UI)
            .text_color(p.text.primary)
            .child(self.render_strip(&theme, cx));
        root = if terminal_shown {
            root.flex_none()
        } else {
            root.size_full()
        };
        if let Some(path) = active_path.clone()
            && !terminal_shown
        {
            root = root.child(self.render_header(&path, &theme, cx));
            if self.documents.get(&path).is_some_and(|doc| doc.conflict) {
                root = root.child(self.render_conflict(&theme, cx));
            }
            if self.documents.get(&path).is_some_and(|doc| doc.loading) {
                root = root.child(super::super::transfer::indeterminate_bar(
                    &theme,
                    "editor-loading",
                ));
            }
        }
        // Hidden rather than dropped behind the terminal: every open file keeps
        // its editor state, and with it the undo history.
        let mut body = div().relative().flex_1().min_h_0().flex().flex_col();
        if terminal_shown {
            body = body.hidden();
        }
        let document = active_path
            .as_ref()
            .and_then(|path| self.documents.get(path));
        body = match document {
            Some(document) if document.error.is_some() => {
                let available = self.options.source.available();
                let body2 = theme.typography.body2;
                body.items_center()
                    .justify_center()
                    .p(px(24.))
                    .gap(px(12.))
                    .child(
                        paragraph(document.error.clone().unwrap_or_default())
                            .size(body2.size)
                            .line_height(body2.line_height)
                            .color(p.error.main),
                    )
                    .child(
                        Button::new("editor-retry", "Try again")
                            .start_icon("Refresh")
                            .disabled(!available)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(path) = this.options.active_path.clone() {
                                    this.load(path, window, cx);
                                }
                            })),
                    )
            }
            Some(document) if !document.loading => {
                let colors = MonacoColors::new(theme.mode.is_dark());
                body.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .bg(colors.background)
                        .font_family(fonts::MONO)
                        .text_size(px(13.))
                        .line_height(px(18.))
                        .text_color(colors.foreground)
                        .child(Editor::new(&document.editor)),
                )
                .child(self.render_status(&theme, window, cx))
            }
            _ => {
                body.items_center()
                    .justify_center()
                    .bg(alpha(p.background_paper, 0.35))
                    .children(document.is_some_and(|doc| doc.loading).then(|| {
                        CircularProgress::new("editor-loading-spinner", 22., p.primary.main)
                    }))
            }
        };
        root = root.child(body);
        if let Some(menu) = self.render_status_menu(&theme, cx) {
            root = root.child(menu);
        }
        if let Some(dialog) = self.dialogs.render(QUEUE, window, cx) {
            root = root.child(dialog);
        }
        let _ = relative(1.);
        root
    }
}
