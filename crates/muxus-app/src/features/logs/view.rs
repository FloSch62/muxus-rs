//! `components/LogViewerDialog.tsx`: the diagnostic log viewer over the
//! backend's in-memory buffer (settings → Debug → View logs). It polls the
//! buffer every two seconds while open, follows new output until scrolled
//! back, filters by level and text, and copies, exports or clears it.

use std::rc::Rc;
use std::time::Duration;

use muxus_core::api::{AppLogEntry, AppLogsResponse};
use muxus_session::Backend;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Hsla, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window,
    div, px, relative,
};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts, sizes};
use muxus_ui::widgets::dialog::{
    CloseHandler, DialogWidth, dialog_actions, dialog_content, dialog_paper, dialog_title,
};
use muxus_ui::widgets::display::{AlertVariant, alert_ink};
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::menu::{self, MenuItem};
use muxus_ui::widgets::tooltip::AnchorBounds;
use muxus_ui::widgets::{Alert, Button, Select};

use super::model::{
    DEBUG_OFF_NOTICE, EMPTY_TEXT, LevelFilter, LevelInk, count_label, export_text,
    format_log_entry, level_ink, pinned_to_end, shows_debug_notice, visible_entries,
};
use crate::export::export_filename;
use crate::features::launcher::kit::{show_error_toast, show_toast};

/// How often the buffer is read again while the viewer is open.
const POLL: Duration = Duration::from_secs(2);
/// The paper is 80% of the window tall (`height: '80vh'`).
const PAPER_HEIGHT: f32 = 0.8;
/// The Level select's width and the filter field's maximum width (`sx`).
const LEVEL_WIDTH: f32 = 130.;
const FILTER_MAX_WIDTH: f32 = 360.;
/// The log lines' line height (`lineHeight: 1.6`).
const LOG_LINE_HEIGHT: f32 = 1.6;

/// What the viewer tells its window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogViewerEvent {
    /// Close, Escape or the backdrop (`setLogViewerOpen(false)`).
    Close,
}

pub struct LogViewerDialog {
    backend: Backend,
    logs: Option<AppLogsResponse>,
    filter: LevelFilter,
    level_open: bool,
    level_anchor: AnchorBounds,
    query: Entity<InputState>,
    scroll: ScrollHandle,
    /// Follow new output like a terminal until the user scrolls back up.
    pinned: bool,
    /// The entry count the list was last scrolled to the end for.
    followed: Option<usize>,
    focus: FocusHandle,
    poll: Task<()>,
    _events: Subscription,
}

impl EventEmitter<LogViewerEvent> for LogViewerDialog {}

impl Focusable for LogViewerDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl LogViewerDialog {
    pub fn new(backend: Backend, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx));
        let events = cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.followed = None;
                cx.notify();
            }
        });
        let poll_backend = backend.clone();
        let poll = cx.spawn(async move |this, cx| {
            loop {
                let logs = poll_backend.app_logs().await;
                let alive = this
                    .update(cx, |this, cx| {
                        if let Ok(logs) = logs {
                            this.set_logs(logs, cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
                cx.background_executor().timer(POLL).await;
            }
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            backend,
            logs: None,
            filter: LevelFilter::All,
            level_open: false,
            level_anchor: AnchorBounds::default(),
            query,
            scroll: ScrollHandle::new(),
            pinned: true,
            followed: None,
            focus,
            poll,
            _events: events,
        }
    }

    /// Show a buffer without asking the backend (tests, the documentation scene).
    pub fn set_logs(&mut self, logs: AppLogsResponse, cx: &mut Context<Self>) {
        if self.logs.as_ref() != Some(&logs) {
            self.logs = Some(logs);
            cx.notify();
        }
    }

    /// Stop reading the backend (the documentation scene shows fixed entries).
    pub fn stop_polling(&mut self) {
        self.poll = Task::ready(());
    }

    pub fn set_filter(&mut self, filter: LevelFilter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.level_open = false;
        self.followed = None;
        cx.notify();
    }

    fn entries(&self, cx: &App) -> Vec<AppLogEntry> {
        let query = self.query.read(cx).value().to_string();
        self.logs
            .as_ref()
            .map(|logs| {
                visible_entries(&logs.entries, self.filter, &query)
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn export(&self, cx: &App) -> String {
        let entries = self.entries(cx);
        let listed: Vec<&AppLogEntry> = entries.iter().collect();
        let debug = self.logs.as_ref().is_some_and(|logs| logs.debug_enabled);
        export_text(&listed, debug, chrono::Utc::now().timestamp_millis())
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.clear_app_logs().await;
            match result {
                Ok(_) => {
                    let logs = backend.app_logs().await;
                    let _ = this.update(cx, |this, cx| {
                        if let Ok(logs) = logs {
                            this.set_logs(logs, cx);
                        }
                    });
                }
                Err(error) => {
                    cx.update(|cx| show_error_toast(error.message, cx));
                }
            }
        })
        .detach();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.export(cx)));
        show_toast(Status::Success, "Log copied to the clipboard.", cx);
    }

    /// `saveTextFile(exportFilename('debug log', 'log'), …)`: ask where, then write.
    fn save(&mut self, cx: &mut Context<Self>) {
        let text = self.export(cx);
        let name = export_filename("debug log", "log");
        let chosen = crate::util::file_dialog::ask_save_path(&downloads_folder(), &name, cx);
        cx.spawn(async move |_, cx| {
            let Some(path) = chosen.await.path().map(std::path::Path::to_path_buf) else {
                return;
            };
            if let Err(error) = std::fs::write(&path, text) {
                cx.update(|cx| {
                    show_error_toast(format!("Could not save {}: {error}", path.display()), cx)
                });
            }
        })
        .detach();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(LogViewerEvent::Close);
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            if self.level_open {
                self.level_open = false;
                cx.notify();
            } else {
                self.close(cx);
            }
        }
    }

    /// Track whether the list is scrolled to its end, and keep it there while
    /// it is as entries arrive.
    fn follow(&mut self, count: usize) {
        let client = f32::from(self.scroll.bounds().size.height);
        let max = f32::from(self.scroll.max_offset().y);
        let top = -f32::from(self.scroll.offset().y);
        if self.followed == Some(count) {
            self.pinned = pinned_to_end(max + client, top, client);
            return;
        }
        if self.pinned {
            self.scroll.scroll_to_bottom();
        }
        self.followed = Some(count);
    }

    /// The Level select's menu, under the field.
    fn level_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.level_open {
            return None;
        }
        let position = menu::below(&self.level_anchor)?;
        let entity = cx.entity().downgrade();
        let mut content = menu::paper(theme).min_w(px(LEVEL_WIDTH));
        for filter in LevelFilter::ALL {
            let entity = entity.clone();
            content = content.child(
                MenuItem::new(
                    SharedString::from(format!("log-level-{filter:?}")),
                    filter.label(),
                )
                .selected(filter == self.filter)
                .on_click(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| this.set_filter(filter, cx));
                }),
            );
        }
        Some(
            menu::popup(
                position,
                move |_, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.level_open = false;
                        cx.notify();
                    });
                },
                content,
            )
            .into_any_element(),
        )
    }
}

/// The folder a browser download would land in.
fn downloads_folder() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

fn line_ink(theme: &Theme, ink: LevelInk) -> Hsla {
    let p = &theme.palette;
    match ink {
        LevelInk::Error => p.error.main,
        LevelInk::Warning => p.warning.main,
        LevelInk::Secondary => p.text.secondary,
        LevelInk::Primary => p.text.primary,
    }
}

impl Render for LogViewerDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let typography = theme.typography;
        let entries = self.entries(cx);
        let total = self.logs.as_ref().map_or(0, |logs| logs.entries.len());
        self.follow(entries.len());
        let viewport = window.viewport_size();
        let margin = Theme::spacing_css(4.);
        let width = DialogWidth::Lg
            .px()
            .min(muxus_ui::zoom::unzoom(viewport.width) - 2. * margin);
        let height = muxus_ui::zoom::unzoom(viewport.height) * PAPER_HEIGHT;

        let toggle = cx.entity().downgrade();
        let level = Select::new("log-level", self.filter.label())
            .label("Level")
            .width(LEVEL_WIDTH)
            .open(self.level_open)
            .track_bounds(&self.level_anchor)
            .on_click(move |_, _, cx| {
                let _ = toggle.update(cx, |this, cx| {
                    this.level_open = !this.level_open;
                    cx.notify();
                });
            });
        let query_field = FormField::input("log-filter", &self.query)
            .label("Filter")
            .placeholder("host, message, error \u{2026}");
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::spacing(1.5))
            .child(level)
            .child(
                div()
                    .flex_1()
                    .max_w(px(FILTER_MAX_WIDTH))
                    .child(query_field),
            )
            .child(div().flex_1())
            .child(
                label(count_label(entries.len(), total))
                    .variant(typography.caption)
                    .color(p.text.secondary)
                    .no_truncate(),
            );

        let mono = sizes::TEXT_SMALL;
        let mut list = div()
            .id("log-viewer-entries")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .rounded(px(theme.radius))
            .border_1()
            .border_color(p.divider)
            .bg(p.background_default)
            .p(Theme::spacing(1.5))
            .font_family(fonts::SYSTEM_MONO)
            .text_size(px(mono))
            .line_height(px(mono * LOG_LINE_HEIGHT));
        if entries.is_empty() {
            list = list.child(
                paragraph(EMPTY_TEXT)
                    .variant(typography.body2)
                    .color(p.text.secondary),
            );
        } else {
            for entry in &entries {
                list = list.child(
                    div()
                        .w(relative(1.))
                        .whitespace_normal()
                        .text_color(line_ink(&theme, level_ink(entry.level)))
                        .child(format_log_entry(entry)),
                );
            }
        }

        let mut content = dialog_content(&theme, true, true)
            .p(Theme::spacing(2.))
            .gap(Theme::spacing(1.5))
            .child(toolbar);
        if shows_debug_notice(self.logs.as_ref()) {
            content = content.child(
                Alert::new(Status::Info, "").compact().content(
                    paragraph(DEBUG_OFF_NOTICE)
                        .variant(typography.body2)
                        .color(alert_ink(&theme, Status::Info, AlertVariant::Standard)),
                ),
            );
        }
        content = content.child(list);

        let empty = entries.is_empty();
        let entity = cx.entity().downgrade();
        let (clear, copy, save, close) = (
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
        );
        let actions = dialog_actions()
            .child(
                Button::new("log-clear", "Clear")
                    .start_icon("DeleteOutlineOutlined")
                    .on_click(move |_, _, cx| {
                        let _ = clear.update(cx, |this, cx| this.clear(cx));
                    }),
            )
            .child(
                Button::new("log-copy", "Copy")
                    .start_icon("ContentCopyOutlined")
                    .disabled(empty)
                    .on_click(move |_, _, cx| {
                        let _ = copy.update(cx, |this, cx| this.copy(cx));
                    }),
            )
            .child(
                Button::new("log-export", "Export")
                    .start_icon("DownloadOutlined")
                    .disabled(empty)
                    .on_click(move |_, _, cx| {
                        let _ = save.update(cx, |this, cx| this.save(cx));
                    }),
            )
            .child(div().flex_1())
            .child(
                Button::new("log-close", "Close")
                    .contained()
                    .on_click(move |_, _, cx| {
                        let _ = close.update(cx, |this, cx| this.close(cx));
                    }),
            );

        let paper = dialog_paper(&theme)
            .w(px(width))
            .h(px(height))
            .child(
                dialog_title(&theme)
                    .pb(Theme::spacing(1.))
                    .child(label("Logs").variant(typography.h6).color(p.text.primary)),
            )
            .child(content)
            .child(actions);
        let backdrop = entity;
        let on_close: CloseHandler = Rc::new(move |_, cx| {
            let _ = backdrop.update(cx, |this, cx| this.close(cx));
        });
        let menu = self.level_menu(&theme, cx);
        div()
            .id("log-viewer")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(modal_layer(
                window,
                "log-viewer-backdrop",
                paper,
                Some(on_close),
            ))
            .children(menu)
    }
}

/// Open the viewer; the window drops it on [`LogViewerEvent::Close`].
pub fn open_log_viewer(
    backend: Backend,
    window: &mut Window,
    cx: &mut App,
) -> Entity<LogViewerDialog> {
    cx.new(|cx| LogViewerDialog::new(backend, window, cx))
}
