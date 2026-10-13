//! The session history dialog (`client/src/components/SessionHistoryDialog.tsx`):
//! retained sessions on the left (full-text search, host, connection and
//! date filters, older pages on demand), the selected session's normalized
//! transcript on the right with the search hits highlighted, and its exports,
//! copy, rename, pin and delete.
//!
//! Everything goes through `muxus_session::Backend`: the list is
//! `session_history`, the transcript `session_log`, the exports
//! `export_session_log_*`.

use std::rc::Rc;
use std::time::Duration;

use muxus_core::api::{SessionLogDetail, SessionLogDirection, SessionLogStatus, SessionLogSummary};
use muxus_core::js::js_trim;
use muxus_core::session_transcript::{SessionTranscript, session_transcript};
use muxus_session::Backend;
use muxus_store::history::SessionHistoryQuery;
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, HighlightStyle, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    Render, ScrollHandle, StatefulInteractiveElement, Styled, StyledText, Subscription, Task,
    TextLayout, Window, div, point, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts};
use muxus_ui::widgets::dialog::{dialog_actions, dialog_paper, dialog_title, modal};
use muxus_ui::widgets::display::divider;
use muxus_ui::widgets::list::{ListItemButton, list_item_text, pitch_correction};
use muxus_ui::widgets::menu::{self, MenuItem};
use muxus_ui::widgets::tooltip::AnchorBounds;
use muxus_ui::widgets::{Alert, Button, ButtonColor, Chip, IconButton, Select, Switch, TextField};

use super::format::{
    MAX_PREVIEW_EVENTS, MAX_PREVIEW_MATCHES, SnippetPart, date_boundary, detail_subtitle,
    group_digits, list_secondary, match_caption, snippet_parts,
};
use super::matches::{TranscriptMatch, find_transcript_matches_in_chunks, utf16_to_byte};
use crate::export::export_filename;
use crate::features::launcher::kit::{
    AskAnswer, AskDialog, AskRequest, PromptField, show_toast, text, text_weight,
};

/// Searches wait for typing to pause this long.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// A page of the list.
const PAGE_SIZE: u32 = 50;

/// The connection filter's choices: value and label.
const KINDS: [(&str, &str); 5] = [
    ("", "All"),
    ("ssh", "SSH"),
    ("local", "Local"),
    ("serial", "Serial"),
    ("telnet", "Telnet"),
];

/// What the dialog asks the window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryEvent {
    Close,
}

#[derive(Clone, Debug)]
enum Pending {
    Delete(SessionLogSummary),
    Rename(SessionLogSummary),
}

pub struct SessionHistoryDialog {
    backend: Backend,
    search: Entity<InputState>,
    host_input: Entity<InputState>,
    from_input: Entity<InputState>,
    to_input: Entity<InputState>,
    query: String,
    host: String,
    kind: &'static str,
    kind_open: bool,
    kind_anchor: AnchorBounds,
    show_timestamps: bool,
    sessions: Vec<SessionLogSummary>,
    next_cursor: Option<String>,
    loading: bool,
    loading_more: bool,
    error: Option<String>,
    list_seq: u64,
    selected_id: Option<String>,
    detail: Option<SessionLogDetail>,
    detail_loading: bool,
    detail_seq: u64,
    transcript: SessionTranscript,
    /// Hits in the transcript, as byte ranges of its text.
    matches: Vec<std::ops::Range<usize>>,
    match_count: usize,
    active_match: usize,
    scroll_to_match: bool,
    preview_layout: Option<TextLayout>,
    preview_scroll: ScrollHandle,
    list_scroll: ScrollHandle,
    debounce: Option<Task<()>>,
    ask: Option<(Entity<AskDialog>, Pending)>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<HistoryEvent> for SessionHistoryDialog {}

impl Focusable for SessionHistoryDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Open session history over the window (`openHistory(query, selectedId)`).
pub fn open_history(
    backend: Backend,
    initial_query: &str,
    initial_selected_id: Option<String>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<SessionHistoryDialog> {
    let query = initial_query.to_owned();
    cx.new(|cx| SessionHistoryDialog::new(backend, &query, initial_selected_id, window, cx))
}

/// A date input's value as Chrome shows it: `mm/dd/yyyy`.
#[cfg(test)]
fn date_display(value: &str) -> Option<String> {
    let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    Some(date.format("%m/%d/%Y").to_string())
}

/// Read what was typed into a date field (`mm/dd/yyyy` or `yyyy-mm-dd`) as
/// the `yyyy-mm-dd` value a date input holds.
fn parse_date_input(value: &str) -> String {
    let value = js_trim(value);
    if value.is_empty() {
        return String::new();
    }
    for format in ["%m/%d/%Y", "%Y-%m-%d"] {
        if let Ok(date) = chrono::NaiveDate::parse_from_str(value, format) {
            return date.format("%Y-%m-%d").to_string();
        }
    }
    String::new()
}

impl SessionHistoryDialog {
    pub fn new(
        backend: Backend,
        initial_query: &str,
        initial_selected_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search all retained output"));
        let host_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter host"));
        let from_input = cx.new(|cx| InputState::new(window, cx).placeholder("mm/dd/yyyy"));
        let to_input = cx.new(|cx| InputState::new(window, cx).placeholder("mm/dd/yyyy"));
        if !initial_query.is_empty() {
            let value = initial_query.to_owned();
            search.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        let mut subscriptions = vec![
            cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filters_changed(cx);
                }
            }),
            cx.subscribe(&host_input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filters_changed(cx);
                }
            }),
        ];
        for input in [&from_input, &to_input] {
            subscriptions.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.reload(cx);
                }
            }));
        }
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut dialog = Self {
            backend,
            search,
            host_input,
            from_input,
            to_input,
            query: js_trim(initial_query).to_owned(),
            host: String::new(),
            kind: "",
            kind_open: false,
            kind_anchor: AnchorBounds::default(),
            show_timestamps: false,
            sessions: Vec::new(),
            next_cursor: None,
            loading: true,
            loading_more: false,
            error: None,
            list_seq: 0,
            selected_id: initial_selected_id,
            detail: None,
            detail_loading: false,
            detail_seq: 0,
            transcript: SessionTranscript {
                text: String::new(),
                chunks: Vec::new(),
            },
            matches: Vec::new(),
            match_count: 0,
            active_match: 0,
            scroll_to_match: false,
            preview_layout: None,
            preview_scroll: ScrollHandle::new(),
            list_scroll: ScrollHandle::new(),
            debounce: None,
            ask: None,
            focus,
            _subscriptions: subscriptions,
        };
        dialog.reload(cx);
        dialog
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(HistoryEvent::Close);
    }

    /// The query or host changed: search again once typing pauses.
    fn filters_changed(&mut self, cx: &mut Context<Self>) {
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                this.query = js_trim(&this.search.read(cx).value()).to_owned();
                this.host = js_trim(&this.host_input.read(cx).value()).to_owned();
                this.reload(cx);
            });
        }));
    }

    fn history_query(&self, cx: &App, cursor: Option<String>) -> SessionHistoryQuery {
        let from = parse_date_input(&self.from_input.read(cx).value());
        let to = parse_date_input(&self.to_input.read(cx).value());
        SessionHistoryQuery {
            query: (!self.query.is_empty()).then(|| self.query.clone()),
            profile_key: None,
            host: (!self.host.is_empty()).then(|| self.host.clone()),
            kind: (!self.kind.is_empty()).then(|| self.kind.to_owned()),
            started_after: date_boundary(&from, false),
            started_before: date_boundary(&to, true),
            limit: PAGE_SIZE,
            cursor,
        }
    }

    /// Load the first page for the current filters.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.list_seq += 1;
        let seq = self.list_seq;
        self.loading = true;
        cx.notify();
        let query = self.history_query(cx, None);
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.session_history(query).await;
            let _ = this.update(cx, |this, cx| {
                if this.list_seq != seq {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(response) => {
                        this.error = None;
                        this.sessions = response.sessions;
                        this.next_cursor = response.next_cursor;
                    }
                    Err(error) => {
                        this.error = Some(error.message);
                        this.sessions.clear();
                        this.next_cursor = None;
                    }
                }
                this.sync_selection(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(cursor) = self.next_cursor.clone() else {
            return;
        };
        self.loading_more = true;
        cx.notify();
        let seq = self.list_seq;
        let query = self.history_query(cx, Some(cursor));
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.session_history(query).await;
            let _ = this.update(cx, |this, cx| {
                this.loading_more = false;
                if this.list_seq != seq {
                    return;
                }
                if let Ok(response) = result {
                    this.sessions.extend(response.sessions);
                    this.next_cursor = response.next_cursor;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The selected session, or the first one.
    fn selected(&self) -> Option<&SessionLogSummary> {
        self.selected_id
            .as_ref()
            .and_then(|id| self.sessions.iter().find(|session| &session.id == id))
            .or_else(|| self.sessions.first())
    }

    /// Keep a selection that still exists, else take the first session, and
    /// load its transcript when it changed.
    fn sync_selection(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected().map(|session| session.id.clone());
        let changed = selected != self.selected_id
            || self.detail.as_ref().map(|detail| &detail.summary.id) != selected.as_ref();
        self.selected_id = selected;
        if changed || self.detail.is_some() {
            self.load_detail(cx);
        }
    }

    fn select(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.selected_id.as_deref() == Some(id) && self.detail.is_some() {
            return;
        }
        self.selected_id = Some(id.to_owned());
        self.load_detail(cx);
    }

    fn load_detail(&mut self, cx: &mut Context<Self>) {
        self.detail_seq += 1;
        let seq = self.detail_seq;
        let Some(id) = self.selected_id.clone() else {
            self.detail = None;
            self.rebuild_preview();
            return;
        };
        self.detail_loading = true;
        cx.notify();
        let query = self.query.clone();
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend
                .session_log(&id, (!query.is_empty()).then_some(query.as_str()))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.detail_seq != seq {
                    return;
                }
                this.detail_loading = false;
                this.detail = result.ok();
                this.rebuild_preview();
                cx.notify();
            });
        })
        .detach();
    }

    fn rebuild_preview(&mut self) {
        let events = self
            .detail
            .as_ref()
            .map(|detail| {
                let start = detail.events.len().saturating_sub(MAX_PREVIEW_EVENTS);
                &detail.events[start..]
            })
            .unwrap_or_default();
        self.transcript = session_transcript(events, self.show_timestamps, true);
        let found: Vec<TranscriptMatch> = find_transcript_matches_in_chunks(
            &self.transcript.chunks,
            &self.query,
            MAX_PREVIEW_MATCHES,
        );
        self.match_count = found.len();
        self.matches = found
            .iter()
            .map(|found| {
                utf16_to_byte(&self.transcript.text, found.start)
                    ..utf16_to_byte(&self.transcript.text, found.end)
            })
            .collect();
        self.active_match = 0;
        self.scroll_to_match = !self.matches.is_empty();
    }

    fn step_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.matches.len();
        if count < 2 {
            return;
        }
        self.active_match = if forward {
            (self.active_match + 1) % count
        } else {
            (self.active_match + count - 1) % count
        };
        self.scroll_to_match = true;
        cx.notify();
    }

    fn set_timestamps(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_timestamps = show;
        self.rebuild_preview();
        cx.notify();
    }

    /// Save an export through the platform's save dialog (`download`).
    fn download(
        &mut self,
        session: &SessionLogSummary,
        format: &'static str,
        cx: &mut Context<Self>,
    ) {
        let backend = self.backend.clone();
        let id = session.id.clone();
        let timestamps = self.show_timestamps;
        let extension = match format {
            "raw" => "muxlog",
            "replay" => "html",
            _ => "txt",
        };
        let file_name = export_filename(session.name(), extension);
        cx.spawn(async move |_, cx| {
            let export = match format {
                "raw" => backend.export_session_log_raw(&id).await,
                "replay" => backend.export_session_log_replay(&id).await,
                _ => backend.export_session_log_clean(&id, timestamps).await,
            };
            let export = match export {
                Ok(export) => export,
                Err(error) => {
                    cx.update(|cx| show_toast(Status::Error, error.message, cx));
                    return;
                }
            };
            let directory = dirs_download();
            let target =
                cx.update(|cx| crate::util::file_dialog::ask_save_path(&directory, &file_name, cx));
            let target = target.await;
            let Some(path) = target.path() else {
                return;
            };
            if let Err(error) = std::fs::write(path, &export.body) {
                cx.update(|cx| show_toast(Status::Error, error.to_string(), cx));
            }
        })
        .detach();
    }

    fn copy_clean_log(&mut self, session: &SessionLogSummary, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let id = session.id.clone();
        let timestamps = self.show_timestamps;
        cx.spawn(async move |_, cx| {
            let result = backend.export_session_log_clean(&id, timestamps).await;
            cx.update(|cx| match result {
                Ok(export) => {
                    let text = String::from_utf8_lossy(&export.body).into_owned();
                    if text.is_empty() {
                        show_toast(Status::Info, "The clean log is empty.", cx);
                        return;
                    }
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    show_toast(Status::Success, "Clean log copied.", cx);
                }
                Err(error) => show_toast(Status::Error, error.message, cx),
            });
        })
        .detach();
    }

    fn toggle_pin(&mut self, session: &SessionLogSummary, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let (id, pinned) = (session.id.clone(), !session.pinned);
        cx.spawn(async move |this, cx| {
            let result = backend.set_session_log_pinned(&id, pinned).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => this.reload(cx),
                Err(error) => show_toast(Status::Error, error.message, cx),
            });
        })
        .detach();
    }

    fn ask(
        &mut self,
        request: AskRequest,
        pending: Pending,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = cx.new(|cx| AskDialog::new(request, window, cx));
        cx.subscribe_in(
            &dialog,
            window,
            |this, _, answer: &AskAnswer, window, cx| {
                let Some((_, pending)) = this.ask.take() else {
                    return;
                };
                window.focus(&this.focus, cx);
                match (pending, answer) {
                    (Pending::Delete(session), AskAnswer::Confirmed) => this.delete(&session, cx),
                    (Pending::Rename(session), AskAnswer::Value(value)) => {
                        let label = if *value == session.title {
                            String::new()
                        } else {
                            value.clone()
                        };
                        if label != session.label.clone().unwrap_or_default() {
                            this.set_label(&session, label, cx);
                        }
                    }
                    _ => {}
                }
                cx.notify();
            },
        )
        .detach();
        self.ask = Some((dialog, pending));
        cx.notify();
    }

    fn request_rename(
        &mut self,
        session: &SessionLogSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ask(
            AskRequest {
                title: "Rename retained session".into(),
                description: Some(format!(
                    "Leave empty to show the recorded title “{}” again. The recorded title stays searchable.",
                    session.title
                )),
                confirm_label: "Rename".into(),
                destructive: false,
                prompt: Some(PromptField {
                    label: "Name".into(),
                    initial_value: session.name().to_owned(),
                    placeholder: Some(session.title.clone()),
                    allow_empty: true,
                    max_len: Some(200),
                }),
            },
            Pending::Rename(session.clone()),
            window,
            cx,
        );
    }

    fn set_label(&mut self, session: &SessionLogSummary, label: String, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let id = session.id.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.set_session_log_label(&id, &label).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => this.reload(cx),
                Err(error) => show_toast(Status::Error, error.message, cx),
            });
        })
        .detach();
    }

    fn request_delete(
        &mut self,
        session: &SessionLogSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ask(
            AskRequest::confirm(
                format!("Delete the retained log for “{}”?", session.name()),
                "The recorded output and its transcript are removed from the history database. This cannot be undone.",
                "Delete",
            )
            .destructive(),
            Pending::Delete(session.clone()),
            window,
            cx,
        );
    }

    fn delete(&mut self, session: &SessionLogSummary, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let id = session.id.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.delete_session_log(&id).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => {
                    show_toast(Status::Success, "Session log deleted.", cx);
                    this.reload(cx);
                }
                Err(error) => show_toast(Status::Error, error.message, cx),
            });
        })
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && self.ask.is_none() {
            cx.stop_propagation();
            if std::mem::take(&mut self.kind_open) {
                cx.notify();
            } else {
                self.close(cx);
            }
        }
    }
}

/// Where exports are offered to be saved.
fn dirs_download() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

/// MUI's `mark` tint: the warning color at 0.35 (the active hit at 0.75).
fn mark(theme: &Theme, active: bool) -> HighlightStyle {
    HighlightStyle {
        background_color: Some(alpha(
            theme.palette.warning.main,
            if active { 0.75 } else { 0.35 },
        )),
        ..HighlightStyle::default()
    }
}

/// A Chrome date input: its `mm/dd/yyyy` mask in the text color and the
/// calendar glyph at its end.
fn date_field(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    theme: &Theme,
) -> TextField {
    let ink = theme.palette.text.primary;
    // Chrome's calendar picker indicator: a 12px page with a solid header.
    let glyph = div()
        .relative()
        .w(px(11.))
        .h(px(12.))
        .flex_none()
        .border_1()
        .border_color(ink)
        .rounded(px(1.))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(3.))
                .bg(ink),
        );
    TextField::new(id, input)
        .label(label)
        .placeholder("mm/dd/yyyy")
        .strong_placeholder()
        .end(glyph)
        .full_width()
}

impl SessionHistoryDialog {
    /// The search, host, connection and date filters.
    fn filters(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = &theme.palette;
        let kind_label = KINDS
            .iter()
            .find(|(value, _)| *value == self.kind)
            .map(|(_, label)| *label)
            .unwrap_or("All");
        let kind_toggle = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(Theme::spacing(1.))
            .p(Theme::spacing(1.5))
            .child(
                TextField::new("history-search", &self.search)
                    .placeholder("Search all retained output")
                    .start(icon("Search", SMALL, p.action.active))
                    .full_width(),
            )
            .child(
                TextField::new("history-host", &self.host_input)
                    .placeholder("Filter host")
                    .full_width(),
            )
            .child(
                Select::new(
                    "history-kind",
                    if self.kind.is_empty() { "" } else { kind_label },
                )
                .label("Connection")
                .open(self.kind_open)
                .full_width()
                .track_bounds(&self.kind_anchor)
                .on_click(move |_, _, cx| {
                    let _ = kind_toggle.update(cx, |this, cx| {
                        this.kind_open = !this.kind_open;
                        cx.notify();
                    });
                }),
            )
            .child(
                div()
                    .flex()
                    .gap(Theme::spacing(1.))
                    .child(div().flex_1().min_w_0().child(date_field(
                        "history-from",
                        "From",
                        &self.from_input,
                        theme,
                    )))
                    .child(div().flex_1().min_w_0().child(date_field(
                        "history-to",
                        "To",
                        &self.to_input,
                        theme,
                    ))),
            )
            .into_any_element()
    }

    /// The retained sessions.
    fn session_list(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = &theme.palette;
        let t = &theme.typography;
        let weak = cx.entity().downgrade();
        let selected_id = self.selected().map(|session| session.id.clone());
        let mut list = div()
            .id("history-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll);
        // MUI: 1 + 4 + 6 + 2 × 18.59 + 6 + 4 per row; GPUI lays it out at 60.
        let chrome_pitch = 1. + 2. * Theme::spacing_css(0.5) + 12. + 2. * t.body2.line_height;
        for (index, session) in self.sessions.iter().enumerate() {
            let selected = selected_id.as_deref() == Some(session.id.as_str());
            let id = session.id.clone();
            let w = weak.clone();
            let mut secondary = div().flex().flex_col().child(text(
                list_secondary(session),
                t.body2,
                p.text.secondary,
            ));
            if let Some(snippet) = &session.snippet {
                secondary = secondary.child(
                    div()
                        .mt(Theme::spacing(0.5))
                        .child(snippet_view(snippet, theme)),
                );
            }
            list = list.child(
                ListItemButton::new(("history-row", index))
                    .selected(selected)
                    .align_start()
                    .padding(Theme::spacing_css(2.), Theme::spacing_css(0.5))
                    .extra_bottom(pitch_correction(index, chrome_pitch, chrome_pitch.floor()))
                    .on_click(move |_, _, cx| {
                        let _ = w.update(cx, |this, cx| this.select(&id, cx));
                    })
                    .child(list_item_text(
                        text_weight(
                            session.name().to_owned(),
                            t.body2,
                            if selected { 600. } else { t.body2.weight },
                            p.text.primary,
                        ),
                        Some(secondary.into_any_element()),
                    )),
            );
        }
        if self.next_cursor.is_some() {
            let w = weak.clone();
            list = list.child(
                Button::new(
                    "history-load-more",
                    if self.loading_more {
                        "Loading…"
                    } else {
                        "Load older sessions"
                    },
                )
                .full_width()
                .disabled(self.loading_more)
                .on_click(move |_, _, cx| {
                    let _ = w.update(cx, |this, cx| this.load_more(cx));
                }),
            );
        }
        list.into_any_element()
    }

    /// The selected session: its header, actions and transcript.
    fn session_detail(
        &mut self,
        session: SessionLogSummary,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let p = &theme.palette;
        let t = &theme.typography;
        let weak = cx.entity().downgrade();
        let mut parts = Vec::new();
        let status = if session.paused {
            "paused"
        } else {
            session.status.as_str()
        };
        let chip = if session.status == SessionLogStatus::Active {
            Chip::new(status).palette(p.success)
        } else {
            Chip::new(status)
        };
        parts.push(
            div()
                .flex()
                .items_start()
                .gap(Theme::spacing(1.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(text(session.name().to_owned(), t.h6, p.text.primary))
                        .child(text(detail_subtitle(&session), t.body2, p.text.secondary)),
                )
                .child(chip)
                .into_any_element(),
        );
        let (s1, s2, s3, s4, s5, s6, s7) = (
            session.clone(),
            session.clone(),
            session.clone(),
            session.clone(),
            session.clone(),
            session.clone(),
            session.clone(),
        );
        let (w1, w2, w3, w4, w5, w6, w7) = (
            weak.clone(),
            weak.clone(),
            weak.clone(),
            weak.clone(),
            weak.clone(),
            weak.clone(),
            weak.clone(),
        );
        parts.push(
            div()
                .flex()
                .items_center()
                .gap(Theme::spacing(1.))
                .child(
                    Button::new("history-replay", "HTML replay")
                        .outlined()
                        .start_icon("CodeOutlined")
                        .on_click(move |_, _, cx| {
                            let _ = w1.update(cx, |this, cx| this.download(&s1, "replay", cx));
                        }),
                )
                .child(
                    Button::new("history-clean", "Clean log")
                        .outlined()
                        .start_icon("DescriptionOutlined")
                        .on_click(move |_, _, cx| {
                            let _ = w2.update(cx, |this, cx| this.download(&s2, "clean", cx));
                        }),
                )
                .child(
                    Button::new("history-raw", "Raw log")
                        .outlined()
                        .start_icon("DescriptionOutlined")
                        .on_click(move |_, _, cx| {
                            let _ = w3.update(cx, |this, cx| this.download(&s3, "raw", cx));
                        }),
                )
                .child(
                    IconButton::new("history-copy", "ContentCopyOutlined")
                        .tooltip("Copy the complete clean log")
                        .on_click(move |_, _, cx| {
                            let _ = w4.update(cx, |this, cx| this.copy_clean_log(&s4, cx));
                        }),
                )
                .child(
                    IconButton::new("history-rename", "DriveFileRenameOutline")
                        .tooltip("Rename retained session")
                        .on_click(move |_, window, cx| {
                            let _ = w5.update(cx, |this, cx| this.request_rename(&s5, window, cx));
                        }),
                )
                .child(
                    IconButton::new(
                        "history-pin",
                        if session.pinned {
                            "PushPin"
                        } else {
                            "PushPinOutlined"
                        },
                    )
                    .tooltip(if session.pinned {
                        "Allow this session to be removed by retention"
                    } else {
                        "Protect this session from quota and age cleanup"
                    })
                    .on_click(move |_, _, cx| {
                        let _ = w6.update(cx, |this, cx| this.toggle_pin(&s6, cx));
                    }),
                )
                .child(
                    IconButton::new("history-delete", "DeleteOutlined")
                        .color(ButtonColor::Error)
                        .disabled(session.status == SessionLogStatus::Active)
                        .tooltip(if session.status == SessionLogStatus::Active {
                            "A live log cannot be deleted safely."
                        } else {
                            "Delete retained session"
                        })
                        .on_click(move |_, window, cx| {
                            let _ = w7.update(cx, |this, cx| this.request_delete(&s7, window, cx));
                        }),
                )
                .into_any_element(),
        );
        if !self.query.is_empty() && !self.detail_loading {
            let (up, down) = (weak.clone(), weak.clone());
            parts.push(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::spacing(0.5))
                    .child(text(
                        match_caption(self.match_count, self.active_match),
                        t.caption,
                        p.text.secondary,
                    ))
                    .child(
                        IconButton::new("history-prev-match", "KeyboardArrowUp")
                            .disabled(self.match_count < 2)
                            .tooltip("Previous match")
                            .on_click(move |_, _, cx| {
                                let _ = up.update(cx, |this, cx| this.step_match(false, cx));
                            }),
                    )
                    .child(
                        IconButton::new("history-next-match", "KeyboardArrowDown")
                            .disabled(self.match_count < 2)
                            .tooltip("Next match")
                            .on_click(move |_, _, cx| {
                                let _ = down.update(cx, |this, cx| this.step_match(true, cx));
                            }),
                    )
                    .into_any_element(),
            );
        }
        let toggle = weak.clone();
        let show = self.show_timestamps;
        let mut timestamps = div().flex().flex_col().child(
            div()
                .id("history-timestamps")
                .flex()
                .items_center()
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    let _ = toggle.update(cx, |this, cx| this.set_timestamps(!show, cx));
                })
                .child(Switch::new("history-timestamps-switch", show).small())
                .child(text("Show timestamps (UTC)", t.body1, p.text.primary)),
        );
        if show {
            let approximate = self.detail.as_ref().is_some_and(|detail| {
                detail.events.iter().any(|event| {
                    event.direction != SessionLogDirection::System
                        && !event.text.is_empty()
                        && event.line_timestamps.as_ref().is_none_or(Vec::is_empty)
                })
            });
            timestamps = timestamps.child(text(
                format!(
                    "Time each line last changed in Muxus. Applies to clean exports and copies.{}",
                    if approximate {
                        " Older recordings use approximate event times."
                    } else {
                        ""
                    }
                ),
                t.caption,
                p.text.secondary,
            ));
        }
        parts.push(timestamps.into_any_element());

        // The transcript (`Paper variant="outlined" component="pre"`).
        let preview: AnyElement = if self.detail_loading {
            preview_text("Loading transcript…".into(), theme).into_any_element()
        } else if self.transcript.text.is_empty() {
            preview_text("No normalized terminal output was retained.".into(), theme)
                .into_any_element()
        } else {
            let highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = self
                .matches
                .iter()
                .enumerate()
                .map(|(index, range)| (range.clone(), mark(theme, index == self.active_match)))
                .collect();
            let styled = StyledText::new(self.transcript.text.clone()).with_highlights(highlights);
            self.preview_layout = Some(styled.layout().clone());
            styled.into_any_element()
        };
        if self.scroll_to_match {
            self.scroll_active_match_into_view(window, cx);
        }
        parts.push(
            div()
                .id("history-preview")
                .flex_1()
                .min_h(px(180.))
                .overflow_y_scroll()
                .track_scroll(&self.preview_scroll)
                .p(Theme::spacing(1.5))
                .rounded(px(theme.radius))
                .border_1()
                .border_color(p.divider)
                .bg(p.background_default)
                .font_family(fonts::SYSTEM_MONO)
                .text_size(px(t.code.size))
                .line_height(px(t.code.line_height))
                .text_color(p.text.primary)
                .child(preview)
                .into_any_element(),
        );
        if let Some(detail) = &self.detail
            && detail.events_truncated
        {
            parts.push(
                text(
                    format!(
                        "Previewing {} of the retained events{}. Exports contain the complete retained log.",
                        group_digits(detail.events.len()),
                        if self.query.is_empty() {
                            " (newest first)"
                        } else {
                            " around the first match"
                        }
                    ),
                    t.caption,
                    p.text.secondary,
                )
                .into_any_element(),
            );
        }
        parts
    }
}

impl Render for SessionHistoryDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let t = theme.typography;
        let weak = cx.entity().downgrade();
        let viewport = window.viewport_size();
        let (vw, vh) = (
            muxus_ui::zoom::unzoom(viewport.width),
            muxus_ui::zoom::unzoom(viewport.height),
        );

        let title = dialog_title(&theme)
            .gap(Theme::spacing(1.))
            .child(text("Session history", t.h6, p.text.primary))
            .child(
                Chip::new(format!(
                    "{}{} loaded",
                    self.sessions.len(),
                    if self.next_cursor.is_some() { "+" } else { "" }
                ))
                .outlined(),
            );

        let empty_line = |message: &str| {
            div()
                .p(Theme::spacing(2.))
                .child(text(message.to_owned(), t.body1, p.text.secondary))
        };
        let mut left = div()
            .flex()
            .flex_col()
            .w(px(340.))
            .flex_none()
            .min_h_0()
            // `borderRight: { md: 1 }` resets the border color after
            // `borderColor: 'divider'`, so the line is drawn in the text color.
            .border_r_1()
            .border_color(p.text.primary)
            .child(self.filters(&theme, cx))
            .child(divider(&theme))
            .child(self.session_list(&theme, cx));
        if self.loading {
            left = left.child(empty_line("Loading history…"));
        } else if self.sessions.is_empty() {
            left = left.child(empty_line(if self.query.is_empty() {
                "Logs will appear here after a logging-enabled terminal starts."
            } else {
                "No retained sessions match this search."
            }));
        }

        let mut right = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .p(Theme::spacing(2.))
            .gap(Theme::spacing(1.5));
        if let Some(error) = &self.error {
            right = right.child(Alert::new(Status::Error, error.clone()));
        }
        match self.selected().cloned() {
            Some(session) => {
                right = right.children(self.session_detail(session, &theme, window, cx))
            }
            None => {
                right = right.child(text(
                    "Choose a session to inspect its transcript.",
                    t.body1,
                    p.text.secondary,
                ));
            }
        }

        // `maxWidth="lg" fullWidth`, `height: min(820px, 90vh)`.
        let height = 820f32.min(vh * 0.9);
        let width = 1200f32.min(vw - 64.);
        let w = weak.clone();
        let paper = dialog_paper(&theme)
            .w(px(width))
            .h(px(height))
            .child(title)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_b_1()
                    .border_color(p.divider)
                    .child(left)
                    .child(right),
            )
            .child(
                dialog_actions().child(Button::new("history-done", "Done").contained().on_click(
                    move |_, _, cx| {
                        let _ = w.update(cx, |this, cx| this.close(cx));
                    },
                )),
            );

        let close = weak.clone();
        let mut root = div()
            .id("session-history")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .font_family(fonts::UI)
            .child(modal(
                paper,
                Some(Rc::new(move |_, cx: &mut App| {
                    let _ = close.update(cx, |this, cx| this.close(cx));
                })),
            ));
        if self.kind_open
            && let Some(bounds) = self.kind_anchor.get()
        {
            let mut items = menu::paper(&theme).min_w(bounds.size.width);
            for (index, (value, label)) in KINDS.iter().enumerate() {
                let value: &'static str = value;
                let w = weak.clone();
                items = items.child(
                    MenuItem::new(("history-kind-option", index), *label)
                        .selected(value == self.kind)
                        .on_click(move |_, _, cx| {
                            let _ = w.update(cx, |this, cx| {
                                this.kind = value;
                                this.kind_open = false;
                                this.reload(cx);
                            });
                        }),
                );
            }
            let w = weak.clone();
            root = root.child(menu::popup(
                bounds.bottom_left(),
                move |_, cx| {
                    let _ = w.update(cx, |this, cx| {
                        this.kind_open = false;
                        cx.notify();
                    });
                },
                items,
            ));
        }
        if let Some((ask, _)) = &self.ask {
            root = root.child(ask.clone());
        }
        root
    }
}

impl SessionHistoryDialog {
    /// Centre the active hit in the preview once its text has been laid out
    /// (`scrollIntoView({ block: 'center' })`).
    fn scroll_active_match_into_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = &self.preview_layout else {
            return;
        };
        let Some(range) = self.matches.get(self.active_match) else {
            self.scroll_to_match = false;
            return;
        };
        let viewport = self.preview_scroll.bounds();
        let (Some(position), true) = (
            layout.position_for_index(range.start),
            viewport.size.height > px(0.),
        ) else {
            // Not laid out yet: try again on the next frame.
            let entity = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = entity.update(cx, |_, cx| cx.notify());
            });
            return;
        };
        self.scroll_to_match = false;
        let content_top = layout.bounds().top() - self.preview_scroll.offset().y;
        let line_top = position.y - content_top;
        let target = line_top - viewport.size.height / 2. + px(9.);
        let max = self.preview_scroll.max_offset().y;
        let offset = target.max(px(0.)).min(max);
        self.preview_scroll.set_offset(point(px(0.), -offset));
    }
}

/// The transcript box's placeholder lines.
fn preview_text(message: String, theme: &Theme) -> impl IntoElement {
    div().text_color(theme.palette.text.primary).child(message)
}

/// A search snippet with its hits marked.
fn snippet_view(snippet: &str, theme: &Theme) -> impl IntoElement {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for part in snippet_parts(snippet) {
        match part {
            SnippetPart::Text(value) => text.push_str(&value),
            SnippetPart::Mark(value) => {
                let start = text.len();
                text.push_str(&value);
                highlights.push((start..text.len(), mark(theme, false)));
            }
        }
    }
    let caption = theme.typography.caption;
    div()
        .text_size(px(caption.size))
        .line_height(px(caption.line_height))
        .text_color(theme.palette.text.secondary)
        .child(StyledText::new(text).with_highlights(highlights))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_typed_dates_like_a_date_input() {
        assert_eq!(parse_date_input("07/25/2026"), "2026-07-25");
        assert_eq!(parse_date_input("2026-07-25"), "2026-07-25");
        assert_eq!(parse_date_input("07/25"), "");
        assert_eq!(date_display("2026-07-25").as_deref(), Some("07/25/2026"));
    }
}
