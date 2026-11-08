//! `components/CompareDialog.tsx`: compares the text "Compare with…" was
//! chosen on with a second text picked here, side by side or inline, with
//! whitespace and leading timestamps optionally ignored, and exports the
//! comparison as a unified diff.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use muxus_core::api::SessionLogSummary;
use muxus_session::Backend;
use muxus_store::history::SessionHistoryQuery;
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    HighlightStyle, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    PathPromptOptions, Render, SharedString, StatefulInteractiveElement, Styled, StyledText, Task,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts};
use muxus_ui::widgets::dialog::{
    dialog_actions, dialog_content, dialog_paper, dialog_title, modal,
};
use muxus_ui::widgets::form_control::{FormControl, FormControlLabel};
use muxus_ui::widgets::listbox::list_subheader;
use muxus_ui::widgets::menu::{self, MenuItem};
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::toggle_button::{ToggleButton, ToggleButtonGroup};
use muxus_ui::widgets::typography::{body2, text};
use muxus_ui::widgets::{Alert, AnchorBounds, Button, IconButton, bounds_probe};

use super::rows::{DiffModel, InlineRow, Line, SideRow, Visible, diff_model, visible_rows};
use super::sources::{
    CompareText, comparable, history_compare_label, history_compare_text, local_file_compare_text,
    remote_file_compare_text, terminal_compare_text,
};
use super::text::{
    CompareOptions, UnifiedDiffOptions, prepare_compare_text, texts_match, unified_diff,
};
use crate::export::export_filename;
use crate::features::launcher::kit::{AskAnswer, AskDialog, AskRequest, PromptField, show_toast};
use crate::features::management::palette::diff_colors;

/// One line of the diff (Monaco's line height at 12.5px).
const LINE: f32 = 19.;
/// Below this width the side-by-side view turns inline
/// (`renderSideBySideInlineBreakpoint`).
const INLINE_BREAKPOINT: f32 = 700.;
/// Session history records offered in the menu.
const HISTORY_LIMIT: u32 = 50;

/// An open terminal the second side can come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareTerminal {
    pub id: String,
    pub title: String,
    pub has_selection: bool,
}

/// A connected session whose files can be read over SFTP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareRemote {
    pub conn_id: String,
    pub host: String,
}

/// What the dialog reads from the window it opened in.
pub trait CompareEnv {
    fn terminals(&self, cx: &App) -> Vec<CompareTerminal>;
    /// A terminal's output (soft-wrapped rows joined), or its selection.
    fn terminal_text(&self, id: &str, selection: bool, cx: &App) -> Option<String>;
    /// Connected sessions with a file browser.
    fn remote_hosts(&self, cx: &App) -> Vec<CompareRemote>;
}

/// What the dialog asks the window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompareEvent {
    Close,
}

/// The prepared texts and their diff, for one choice of sides and options.
struct Computed {
    key: DiffKey,
    original: String,
    modified: String,
    same: bool,
    model: Rc<DiffModel>,
}

/// The diff, the view (inline or not) and how many stretches each view unfolded.
type VisibleKey = (DiffKey, bool, usize, usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DiffKey {
    second: u64,
    swapped: bool,
    options: CompareOptions,
}

pub struct CompareDialog {
    backend: Backend,
    env: Rc<dyn CompareEnv>,
    source: CompareText,
    second: Option<CompareText>,
    /// Changes with each second side, so its diff is computed again.
    second_seq: u64,
    swapped: bool,
    inline: bool,
    options: CompareOptions,
    menu_open: bool,
    menu_anchor: AnchorBounds,
    history: Option<Vec<SessionLogSummary>>,
    loading: bool,
    /// Only the last pick lands, however the loads finish.
    latest_pick: u64,
    computed: Option<Computed>,
    computing: Option<(DiffKey, Task<()>)>,
    /// The rows laid out last, and what they were made for.
    visible: Option<(VisibleKey, Rc<Vec<Visible>>)>,
    /// Folded stretches the user opened, per view.
    expanded_side: HashSet<usize>,
    expanded_inline: HashSet<usize>,
    ask: Option<(Entity<AskDialog>, CompareRemote)>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
}

impl EventEmitter<CompareEvent> for CompareDialog {}

impl Focusable for CompareDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Open the dialog over a window with its first side.
pub fn open_compare_dialog(
    backend: Backend,
    env: Rc<dyn CompareEnv>,
    source: CompareText,
    window: &mut Window,
    cx: &mut App,
) -> Entity<CompareDialog> {
    cx.new(|cx| CompareDialog::new(backend, env, source, window, cx))
}

impl CompareDialog {
    pub fn new(
        backend: Backend,
        env: Rc<dyn CompareEnv>,
        source: CompareText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            backend,
            env,
            source,
            second: None,
            second_seq: 0,
            swapped: false,
            inline: false,
            options: CompareOptions::default(),
            menu_open: false,
            menu_anchor: AnchorBounds::default(),
            history: None,
            loading: false,
            latest_pick: 0,
            computed: None,
            computing: None,
            visible: None,
            expanded_side: HashSet::new(),
            expanded_inline: HashSet::new(),
            ask: None,
            scroll: UniformListScrollHandle::new(),
            focus,
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(CompareEvent::Close);
    }

    /// The two sides in view order.
    fn sides(&self) -> (&CompareText, Option<&CompareText>) {
        match (&self.second, self.swapped) {
            (Some(second), true) => (second, Some(&self.source)),
            (second, _) => (&self.source, second.as_ref()),
        }
    }

    fn diff_key(&self) -> Option<DiffKey> {
        self.second.as_ref().map(|_| DiffKey {
            second: self.second_seq,
            swapped: self.swapped,
            options: self.options,
        })
    }

    /// Prepare both texts and diff them off the UI thread when the sides or
    /// the options changed.
    fn ensure_computed(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.diff_key() else {
            return;
        };
        if self.computed.as_ref().is_some_and(|c| c.key == key)
            || self.computing.as_ref().is_some_and(|(k, _)| *k == key)
        {
            return;
        }
        let (left, right) = self.sides();
        let (left, right) = (
            left.text.clone(),
            right.map(|r| r.text.clone()).unwrap_or_default(),
        );
        let options = self.options;
        let work = cx.background_spawn(async move {
            let original = prepare_compare_text(&left, options);
            let modified = prepare_compare_text(&right, options);
            // "No differences" is a line check; the unified diff is only built for an export.
            let same = texts_match(&original, &modified, options.ignore_whitespace);
            let model = diff_model(&original, &modified, options.ignore_whitespace);
            (original, modified, same, model)
        });
        let task = cx.spawn(async move |this, cx| {
            let (original, modified, same, model) = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.diff_key() != Some(key) {
                    return;
                }
                this.computing = None;
                this.expanded_side.clear();
                this.expanded_inline.clear();
                this.computed = Some(Computed {
                    key,
                    original,
                    modified,
                    same,
                    model: Rc::new(model),
                });
                cx.notify();
            });
        });
        self.computing = Some((key, task));
    }

    /// `pick`: load a second side; only the last pick lands.
    fn pick<F>(&mut self, load: F, cx: &mut Context<Self>)
    where
        F: std::future::Future<Output = Result<CompareText, String>> + 'static,
    {
        self.menu_open = false;
        self.loading = true;
        self.latest_pick += 1;
        let request = self.latest_pick;
        cx.spawn(async move |this, cx| {
            let side = load.await.and_then(comparable);
            let _ = this.update(cx, |this, cx| {
                if request != this.latest_pick {
                    return;
                }
                this.loading = false;
                match side {
                    Ok(side) => {
                        this.second = Some(side);
                        this.second_seq += 1;
                        this.ensure_computed(cx);
                    }
                    Err(message) => show_toast(Status::Error, message, cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_open = true;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let page = backend
                .session_history(SessionHistoryQuery {
                    limit: HISTORY_LIMIT,
                    ..SessionHistoryQuery::default()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.history = page.ok().map(|page| page.sessions);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn pick_terminal(
        &mut self,
        terminal: CompareTerminal,
        selection: bool,
        cx: &mut Context<Self>,
    ) {
        let text = self.env.terminal_text(&terminal.id, selection, cx);
        self.pick(
            async move {
                let text = text.ok_or_else(|| "That terminal is closed.".to_string())?;
                Ok(terminal_compare_text(&terminal.title, text, selection))
            },
            cx,
        );
    }

    fn pick_history(&mut self, session: SessionLogSummary, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        self.pick(
            async move { history_compare_text(&backend, &session).await },
            cx,
        );
    }

    fn pick_local_file(&mut self, cx: &mut Context<Self>) {
        self.menu_open = false;
        let answer = crate::util::file_dialog::pick_paths(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let Some(path) = answer
                .await
                .and_then(|paths: Vec<PathBuf>| paths.into_iter().next())
            else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.pick(async move { local_file_compare_text(&path) }, cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// "File on host…": ask for the path, starting from the first side's when
    /// it was read on that connection.
    fn ask_remote_path(
        &mut self,
        remote: CompareRemote,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu_open = false;
        let initial = self
            .source
            .remote
            .as_ref()
            .filter(|file| file.conn_id == remote.conn_id)
            .map(|file| file.path.clone())
            .unwrap_or_default();
        let request = AskRequest {
            title: format!("Compare with a file on {}", remote.host),
            description: None,
            confirm_label: "Compare".into(),
            destructive: false,
            prompt: Some(PromptField {
                label: "Path".into(),
                initial_value: initial,
                placeholder: None,
                allow_empty: false,
                max_len: None,
            }),
        };
        let dialog = cx.new(|cx| AskDialog::new(request, window, cx));
        cx.subscribe_in(
            &dialog,
            window,
            |this, _, answer: &AskAnswer, window, cx| {
                let Some((_, remote)) = this.ask.take() else {
                    return;
                };
                window.focus(&this.focus, cx);
                if let AskAnswer::Value(path) = answer {
                    let backend = this.backend.clone();
                    let path = path.clone();
                    this.pick(
                        async move {
                            remote_file_compare_text(&backend, &remote.conn_id, &remote.host, &path)
                                .await
                        },
                        cx,
                    );
                }
                cx.notify();
            },
        )
        .detach();
        self.ask = Some((dialog, remote));
        cx.notify();
    }

    /// The diff of the sides and options in view, once it is computed.
    fn current(&self) -> Option<&Computed> {
        let key = self.diff_key()?;
        self.computed
            .as_ref()
            .filter(|computed| computed.key == key)
    }

    fn export_diff(&mut self, cx: &mut Context<Self>) {
        let Some(computed) = self.current() else {
            return;
        };
        let (left, right) = self.sides();
        let Some(right) = right else {
            return;
        };
        let diff = unified_diff(
            &computed.original,
            &computed.modified,
            &UnifiedDiffOptions {
                original_label: left.label.clone(),
                modified_label: right.label.clone(),
                ignore_whitespace: self.options.ignore_whitespace,
                context: None,
            },
        );
        let name = export_filename(&format!("{} vs {}", left.label, right.label), "diff");
        let directory = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| {
                let downloads = home.join("Downloads");
                if downloads.is_dir() { downloads } else { home }
            })
            .unwrap_or_else(std::env::temp_dir);
        let chosen = crate::util::file_dialog::ask_save_path(&directory, &name, cx);
        cx.spawn(async move |_, cx| {
            let Some(path) = chosen.await.path().map(std::path::Path::to_path_buf) else {
                return;
            };
            if let Err(error) = std::fs::write(&path, diff) {
                cx.update(|cx| {
                    show_toast(
                        Status::Error,
                        format!("Could not save the file: {error}"),
                        cx,
                    )
                });
            }
        })
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" || self.ask.is_some() {
            return;
        }
        cx.stop_propagation();
        if std::mem::take(&mut self.menu_open) {
            cx.notify();
        } else {
            self.close(cx);
        }
    }

    // ---- rendering --------------------------------------------------------------------

    fn render_header(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = theme.palette;
        let (left, right) = self.sides();
        let side_label = |content: SharedString, color| {
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .font_family(fonts::MONO)
                .text_size(px(12.5))
                .text_color(color)
                .child(content)
        };
        let left_label = side_label(left.label.clone().into(), p.text.primary);
        let right_label = match right {
            Some(right) => side_label(right.label.clone().into(), p.text.primary),
            None => side_label("Choose the text to compare with".into(), p.text.secondary),
        };
        let compare_with = {
            let label: AnyElement = if self.loading {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(CircularProgress::new(
                        "compare-loading",
                        14.,
                        p.action.disabled,
                    ))
                    .child("Compare with…")
                    .into_any_element()
            } else {
                div().child("Compare with…").into_any_element()
            };
            div()
                .relative()
                .flex_none()
                .child(
                    Button::new("compare-with", "Compare with…")
                        .outlined()
                        .disabled(self.loading)
                        .label_element(label)
                        .on_click(cx.listener(|this, _, _, cx| this.open_menu(cx))),
                )
                .child(bounds_probe(&self.menu_anchor))
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(left_label)
            .child(
                IconButton::new("compare-swap", "SwapHoriz")
                    .tooltip("Swap sides")
                    .disabled(self.second.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.swapped = !this.swapped;
                        this.ensure_computed(cx);
                        cx.notify();
                    })),
            )
            .child(right_label)
            .child(compare_with)
            .into_any_element()
    }

    fn render_options(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let inline = self.inline;
        let view = ToggleButtonGroup::new(vec![
            ToggleButton::new("compare-side-by-side", "Side by side", !inline).on_click(
                cx.listener(|this, _, _, cx| {
                    this.inline = false;
                    cx.notify();
                }),
            ),
            ToggleButton::new("compare-inline", "Inline", inline).on_click(cx.listener(
                |this, _, _, cx| {
                    this.inline = true;
                    cx.notify();
                },
            )),
        ])
        .inline(2., 11., 13.);
        let whitespace = self.options.ignore_whitespace;
        let timestamps = self.options.ignore_timestamps;
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(16.))
            .child(view)
            .child(
                FormControlLabel::new(
                    "compare-ignore-whitespace",
                    FormControl::Switch(whitespace),
                    "Ignore whitespace",
                )
                .small()
                .flush()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.options.ignore_whitespace = !whitespace;
                    this.ensure_computed(cx);
                    cx.notify();
                })),
            )
            .child(
                FormControlLabel::new(
                    "compare-ignore-timestamps",
                    FormControl::Switch(timestamps),
                    "Ignore leading timestamps",
                )
                .small()
                .flush()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.options.ignore_timestamps = !timestamps;
                    this.ensure_computed(cx);
                    cx.notify();
                })),
            )
            .into_any_element()
    }

    fn render_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.menu_open {
            return None;
        }
        let position = menu::below(&self.menu_anchor)?;
        let mut paper = menu::paper(theme).min_w(px(240.));
        let mut items = div()
            .id("compare-menu-items")
            .flex()
            .flex_col()
            .max_h(px(420.))
            .overflow_y_scroll();
        let terminals = self.env.terminals(cx);
        if !terminals.is_empty() {
            items = items.child(list_subheader(theme, "Terminals"));
        }
        for (index, terminal) in terminals.into_iter().enumerate() {
            let output = terminal.clone();
            items = items.child(
                MenuItem::new(("compare-terminal", index), terminal.title.clone()).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.pick_terminal(output.clone(), false, cx)
                    }),
                ),
            );
            if terminal.has_selection {
                let label = format!("{} (selection)", terminal.title);
                items = items.child(MenuItem::new(("compare-selection", index), label).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.pick_terminal(terminal.clone(), true, cx)
                    }),
                ));
            }
        }
        if let Some(history) = self.history.as_ref().filter(|h| !h.is_empty()) {
            items = items.child(list_subheader(theme, "Session history"));
            for (index, session) in history.iter().enumerate() {
                let picked = session.clone();
                items =
                    items.child(
                        MenuItem::new(("compare-history", index), history_compare_label(session))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pick_history(picked.clone(), cx)
                            })),
                    );
            }
        }
        items = items.child(list_subheader(theme, "Files")).child(
            MenuItem::new("compare-local-file", "Local file…")
                .on_click(cx.listener(|this, _, _, cx| this.pick_local_file(cx))),
        );
        // The first side's connection is offered even without a terminal for it.
        let mut remotes: Vec<CompareRemote> = Vec::new();
        if let Some(file) = &self.source.remote {
            remotes.push(CompareRemote {
                conn_id: file.conn_id.clone(),
                host: file.host.clone(),
            });
        }
        for remote in self.env.remote_hosts(cx) {
            match remotes.iter_mut().find(|r| r.conn_id == remote.conn_id) {
                Some(known) => known.host = remote.host,
                None => remotes.push(remote),
            }
        }
        for (index, remote) in remotes.into_iter().enumerate() {
            let label = format!("File on {}…", remote.host);
            items = items.child(MenuItem::new(("compare-remote", index), label).on_click(
                cx.listener(move |this, _, window, cx| {
                    this.ask_remote_path(remote.clone(), window, cx)
                }),
            ));
        }
        paper = paper.child(items);
        let dismiss = cx.entity().downgrade();
        Some(
            menu::popup(
                position,
                move |_, cx| {
                    let _ = dismiss.update(cx, |this, cx| {
                        this.menu_open = false;
                        cx.notify();
                    });
                },
                paper,
            )
            .into_any_element(),
        )
    }

    fn render_diff(
        &mut self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let frame = div()
            .flex_1()
            .min_h(px(200.))
            .min_w_0()
            .flex()
            .flex_col()
            .border_1()
            .border_color(p.divider)
            .rounded(px(theme.radius))
            .overflow_hidden()
            .bg(theme.components.code_surface);
        let Some(computed) = self.computed.as_ref().filter(|_| self.second.is_some()) else {
            let content = if self.second.is_none() {
                text(
                    "Use Compare with… to pick a terminal\u{2019}s output, a session history record or a file.",
                    body2(),
                    p.text.secondary,
                )
                .into_any_element()
            } else {
                div().into_any_element()
            };
            return div()
                .flex_1()
                .min_h(px(200.))
                .child(content)
                .into_any_element();
        };
        // `useInlineViewWhenSpaceIsLimited`.
        let width = muxus_ui::zoom::unzoom(window.viewport_size().width) - 64. - 48.;
        let inline = self.inline || width.min(1200. - 48.) < INLINE_BREAKPOINT;
        let model = computed.model.clone();
        // The rows on offer change only with the diff, the view and an unfolded stretch.
        let cache_key = (
            computed.key,
            inline,
            self.expanded_side.len(),
            self.expanded_inline.len(),
        );
        let visible = match &self.visible {
            Some((key, rows)) if *key == cache_key => rows.clone(),
            _ => {
                let rows = Rc::new(if inline {
                    visible_rows(
                        model.inline.len(),
                        &model.inline_folds,
                        &self.expanded_inline,
                    )
                } else {
                    visible_rows(model.side.len(), &model.side_folds, &self.expanded_side)
                });
                self.visible = Some((cache_key, rows.clone()));
                rows
            }
        };
        let count = visible.len();
        let theme = *theme;
        let list = uniform_list(
            "compare-diff-rows",
            count,
            cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                range
                    .filter_map(|index| visible.get(index).copied())
                    .map(|row| match row {
                        Visible::Fold { fold, count } => fold_row(fold, count, inline, &theme, cx),
                        Visible::Row(index) if inline => inline_row(&model.inline[index], &theme),
                        Visible::Row(index) => side_row(&model.side[index], &theme),
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .font_family(fonts::MONO)
        .text_size(px(12.5))
        .line_height(px(LINE))
        .text_color(p.text.primary);
        let base = self.scroll.0.borrow().base_handle.clone();
        frame
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .py(px(6.))
                    .child(list)
                    .child(muxus_ui::widgets::scrollbar::vertical(&base, &theme)),
            )
            .into_any_element()
    }
}

/// The gutter: a line number, right-aligned.
fn gutter(number: Option<usize>, color: muxus_ui::gpui_kit::Hsla) -> impl IntoElement {
    div()
        .w(px(44.))
        .flex_none()
        .pr(px(10.))
        .flex()
        .justify_end()
        .text_color(color)
        .child(number.map(|n| n.to_string()).unwrap_or_default())
}

/// A line's text with its changed characters tinted.
fn marked(line: &Line, tint: muxus_ui::gpui_kit::Hsla) -> AnyElement {
    let styled = StyledText::new(SharedString::from(line.text.clone())).with_highlights(
        line.marks.iter().map(|range| {
            (
                range.clone(),
                HighlightStyle {
                    background_color: Some(tint),
                    ..Default::default()
                },
            )
        }),
    );
    div().whitespace_nowrap().child(styled).into_any_element()
}

fn cell() -> muxus_ui::gpui_kit::Div {
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .h(px(LINE))
        .overflow_hidden()
}

fn plain(text: &str) -> AnyElement {
    div()
        .whitespace_nowrap()
        .child(SharedString::from(text.to_owned()))
        .into_any_element()
}

fn side_row(row: &SideRow, theme: &Theme) -> AnyElement {
    let colors = diff_colors(theme.mode.is_dark());
    let divider = theme.palette.divider;
    let (left, right) = match row {
        SideRow::Same {
            left,
            right,
            text,
            right_text,
        } => (
            cell()
                .child(gutter(Some(*left), colors.line_number))
                .child(plain(text)),
            cell()
                .child(gutter(Some(*right), colors.line_number))
                .child(plain(right_text.as_deref().unwrap_or(text))),
        ),
        SideRow::Changed { left, right } => {
            let side = |line: &Option<Line>, row_tint, mark_tint| match line {
                Some(line) => cell()
                    .bg(row_tint)
                    .child(gutter(Some(line.number), colors.line_number))
                    .child(marked(line, mark_tint)),
                None => cell().bg(colors.filler),
            };
            (
                side(left, colors.removed_line, colors.removed_text),
                side(right, colors.inserted_line, colors.inserted_text),
            )
        }
    };
    div()
        .flex()
        .w_full()
        .child(left.border_r_1().border_color(divider))
        .child(right)
        .into_any_element()
}

fn inline_row(row: &InlineRow, theme: &Theme) -> AnyElement {
    let colors = diff_colors(theme.mode.is_dark());
    let line = |left: Option<usize>, right: Option<usize>| {
        cell()
            .w_full()
            .child(gutter(left, colors.line_number))
            .child(gutter(right, colors.line_number))
    };
    match row {
        InlineRow::Same { left, right, text } => line(Some(*left), Some(*right)).child(plain(text)),
        InlineRow::Removed(removed) => line(Some(removed.number), None)
            .bg(colors.removed_line)
            .child(marked(removed, colors.removed_text)),
        InlineRow::Added(added) => line(None, Some(added.number))
            .bg(colors.inserted_line)
            .child(marked(added, colors.inserted_text)),
    }
    .into_any_element()
}

/// Unchanged lines folded away; a click shows them.
fn fold_row(
    fold: usize,
    count: usize,
    inline: bool,
    theme: &Theme,
    cx: &mut Context<CompareDialog>,
) -> AnyElement {
    let colors = diff_colors(theme.mode.is_dark());
    div()
        .id(("compare-fold", fold))
        .w_full()
        .h(px(LINE))
        .flex()
        .items_center()
        .justify_center()
        .bg(colors.hidden_lines)
        .text_color(theme.palette.text.secondary)
        .font_family(fonts::UI)
        .text_size(px(12.))
        .cursor_pointer()
        .child(format!("{count} hidden lines"))
        .on_click(cx.listener(move |this, _, _, cx| {
            if inline {
                this.expanded_inline.insert(fold);
            } else {
                this.expanded_side.insert(fold);
            }
            cx.notify();
        }))
        .into_any_element()
}

impl Render for CompareDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_computed(cx);
        let theme = *cx.theme();
        let p = theme.palette;
        let vw = muxus_ui::zoom::unzoom(window.viewport_size().width);
        let vh = muxus_ui::zoom::unzoom(window.viewport_size().height);
        let header = self.render_header(&theme, cx);
        let options = self.render_options(cx);
        let diff = self.render_diff(&theme, window, cx);
        let current = self.current().map(|computed| computed.same);
        let same = current == Some(true);
        let exportable = current == Some(false);
        let weak = cx.entity().downgrade();

        // `maxWidth="lg" fullWidth`, `height: min(780px, 90vh)`.
        let paper = dialog_paper(&theme)
            .w(px(1200f32.min(vw - 64.)))
            .h(px(780f32.min(vh * 0.9)))
            .child(dialog_title(&theme).pb(px(8.)).child("Compare"))
            .child(
                dialog_content(&theme, true, false)
                    .gap(px(10.))
                    .child(header)
                    .child(options)
                    .when(same, |content| {
                        content.child(Alert::new(Status::Info, "No differences."))
                    })
                    .child(diff),
            )
            .child(
                dialog_actions()
                    .px(px(24.))
                    .pb(px(16.))
                    .child(
                        Button::new("compare-export", "Export diff")
                            .start_icon("FileDownloadOutlined")
                            .disabled(!exportable)
                            .on_click(cx.listener(|this, _, _, cx| this.export_diff(cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("compare-close", "Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
            );
        let menu = self.render_menu(&theme, cx);
        let mut root = div()
            .id("compare-dialog")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .font_family(fonts::UI)
            .text_color(p.text.primary)
            .child(modal(
                paper,
                Some(Rc::new(move |_, cx: &mut App| {
                    let _ = weak.update(cx, |this, cx| this.close(cx));
                })),
            ))
            .children(menu);
        if let Some((ask, _)) = &self.ask {
            root = root.child(ask.clone());
        }
        root
    }
}
