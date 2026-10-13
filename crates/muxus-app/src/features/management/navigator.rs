//! The navigator beside the work area (`Navigator.tsx`): the explorer of
//! the device's data, the saved request library, the history of runs and
//! what the device advertised.

use std::rc::Rc;

use muxus_core::gnmi_path::format_gnmi_path;
use muxus_core::management::{ManagementSessionInfo, SavedManagementRequest};
use muxus_mgmt::workbench::controller::run_label;
use muxus_mgmt::workbench::data_tree::find_node;
use muxus_mgmt::workbench::requests::{
    ManagementDraft, draft_summary, normalize_gnmi_draft, normalize_netconf_draft,
};
use muxus_mgmt::workbench::store::{ExplorerScope, NavigatorTab, RunRecord, RunStatus};
use muxus_mgmt::workbench::xml::pretty_xml;
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, ClickEvent, Context, Div, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::tooltip::{AnchorBounds, Tooltip, bounds_probe};
use muxus_ui::widgets::{Button, IconButton};

use super::workbench::ListenerForApp;

use super::fields::FieldKey;
use super::logic::device::{
    NETCONF_CAPABILITIES, encodings_text, framing_text, hello_modules, history_meta,
    library_sections, notification_event, tls_row,
};
use super::logic::format::{locale_date_time, locale_time, parse_ms};
use super::logic::results::run_failed;
use super::path_text::path_text;
use super::tree_view::{TreeOptions, rows_of};
use super::ui::TreeSource;
use super::view::ManagementView;
use super::widgets::MONO;
use muxus_ui::widgets::button::ButtonColor;
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::search_field::search_field;
use muxus_ui::widgets::toggle_button::ToggleButtonGroup;
use muxus_ui::widgets::toggle_button::ToggleOption;
use muxus_ui::widgets::tooltip_lines::LinesTooltip;
use muxus_ui::widgets::typography::caps;

fn draft_of(protocol_gnmi: bool, request: &SavedManagementRequest) -> ManagementDraft {
    if protocol_gnmi {
        ManagementDraft::Gnmi(normalize_gnmi_draft(&request.request))
    } else {
        ManagementDraft::Netconf(normalize_netconf_draft(&request.request))
    }
}

/// `section` captions of the library and notification list.
fn section_caption(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div().px(px(12.)).pt(px(8.)).pb(px(4.)).child(caps(
        text,
        10.5,
        650.,
        theme.palette.text.secondary,
    ))
}

/// `StatusIcon`: how a run went.
pub(super) fn status_icon(run: &RunRecord, size: f32, theme: &Theme) -> AnyElement {
    let p = &theme.palette;
    match run.status {
        RunStatus::Running => {
            CircularProgress::new(("navigator-progress", 1usize), size - 3., p.primary.main)
                .into_any_element()
        }
        RunStatus::Streaming => {
            icon("MonitorHeartOutlined", size, p.success.main).into_any_element()
        }
        _ if run_failed(run) => icon("ErrorRounded", size, p.error.main).into_any_element(),
        RunStatus::Cancelled => icon("ErrorRounded", size, p.text.disabled).into_any_element(),
        _ => icon("CheckCircleRounded", size, p.success.main).into_any_element(),
    }
}

impl ManagementView {
    pub(super) fn render_navigator(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let (tab, history) = self.store.read(|state| {
            (
                state.navigator,
                state.runs.iter().filter(|run| !run.hidden).count(),
            )
        });
        let tabs = [
            (NavigatorTab::Explore, "Explore"),
            (NavigatorTab::Library, "Saved"),
            (NavigatorTab::History, "History"),
            (NavigatorTab::Device, "Device"),
        ];
        let mut strip = div()
            .flex()
            .flex_none()
            .border_b_1()
            .border_color(p.divider);
        for (value, text) in tabs {
            let selected = tab == value;
            let ink = if selected {
                p.text.primary
            } else {
                p.text.secondary
            };
            let hover = p.text.primary;
            let mut content = div().flex().items_center().gap(px(4.)).child(
                label(text)
                    .size(12.5)
                    .weight(550.)
                    .line_height(15.625)
                    .color(ink)
                    .no_truncate(),
            );
            if value == NavigatorTab::History && history > 0 {
                content = content.child(
                    div()
                        .px(px(4.8))
                        .rounded(px(64.))
                        .bg(p.action.selected)
                        .text_size(px(10.))
                        .line_height(px(15.))
                        .text_color(p.text.secondary)
                        .child(history.to_string()),
                );
            }
            let mut button = div()
                .id(SharedString::from(format!("nav-{text}")))
                .relative()
                .flex_1()
                .min_w_0()
                .flex()
                .justify_center()
                .items_center()
                .h(px(39.625))
                .px(px(4.))
                .cursor_pointer()
                .hover(move |s| s.text_color(hover))
                .child(content)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.store.update(|state| state.set_navigator(value));
                    cx.notify();
                }));
            if selected {
                button = button.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(2.))
                        .rounded_t(px(2.))
                        .bg(p.text.primary),
                );
            }
            strip = strip.child(button);
        }
        let panel = match tab {
            NavigatorTab::Explore => self.render_explorer(window, cx),
            NavigatorTab::Library => self.render_library(window, cx),
            NavigatorTab::History => self.render_history(cx),
            NavigatorTab::Device => self.render_device(window, cx),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(strip)
            .child(div().flex_1().min_h_0().flex().flex_col().child(panel))
    }

    // Explorer

    fn render_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let connected = self.connected();
        let filter_value = self.ui.explorer_filter.clone();
        let filter = self.bound_input(
            FieldKey::ExplorerFilter,
            "Filter loaded data",
            &filter_value,
            window,
            cx,
        );
        let gnmi = self.is_gnmi();
        let (rows, scope, selected, root_loading, root_error, has_children, selected_path) =
            self.store.read(|state| {
                let explorer = &state.explorer;
                let rows = rows_of(
                    &explorer.root,
                    &TreeOptions {
                        expanded: &explorer.expanded,
                        filter: &filter_value,
                        lazy: gnmi || explorer.scope == ExplorerScope::All,
                        loading: Some(&explorer.loading),
                        errors: Some(&explorer.errors),
                    },
                );
                let selected_path = explorer
                    .selected
                    .as_deref()
                    .and_then(|id| find_node(&explorer.root, id))
                    .filter(|node| {
                        node.kind != muxus_mgmt::workbench::data_tree::DataNodeKind::Root
                    })
                    .map(|node| node.path.clone());
                (
                    rows,
                    explorer.scope,
                    explorer.selected.clone(),
                    explorer.loading.contains(""),
                    explorer.errors.get("").cloned(),
                    !explorer.root.children.is_empty(),
                    selected_path,
                )
            });
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .p(px(8.))
            .child(
                search_field(&filter, 30., Some(p.background_paper), cx)
                    .flex_1()
                    .min_w_0(),
            )
            .child(
                IconButton::new("reload-explorer", "Refresh")
                    .tooltip("Reload")
                    .disabled(!connected)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.store.update(|state| state.reset_explorer(None));
                        this.load_explorer_root();
                        cx.notify();
                    })),
            );
        let scope_toggle = div().px(px(8.)).pb(px(6.)).flex_none().flex().child(
            ToggleButtonGroup::exclusive(
                "explorer-scope",
                vec![
                    ToggleOption::new("Config + state"),
                    ToggleOption::new("Config"),
                ],
                Some(if scope == ExplorerScope::All { 0 } else { 1 }),
                cx.listener_index(|this, index, cx| {
                    let scope = if index == 0 {
                        ExplorerScope::All
                    } else {
                        ExplorerScope::Config
                    };
                    this.store.update(|state| state.reset_explorer(Some(scope)));
                    this.load_explorer_root();
                    cx.notify();
                }),
            )
            .sizing(1.6, 7., 11.5),
        );
        let body2 = theme.typography.body2;
        let caption = theme.typography.caption;
        let content: AnyElement =
            if root_loading && !has_children {
                div()
                    .p(px(24.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .child(CircularProgress::new(
                        ("navigator-progress", 2usize),
                        20.,
                        p.primary.main,
                    ))
                    .child(
                        label(if gnmi {
                            "Reading the top of the tree …"
                        } else {
                            "Reading the configuration …"
                        })
                        .size(caption.size)
                        .line_height(caption.line_height)
                        .color(p.text.secondary)
                        .no_truncate(),
                    )
                    .into_any_element()
            } else if let Some(error) = root_error {
                div()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(body2.size))
                            .line_height(px(body2.line_height))
                            .text_color(p.error.main)
                            .child(error),
                    )
                    .child(
                        div()
                            .flex()
                            .child(Button::new("explorer-retry", "Try again").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.controller.load_node_later("");
                                    cx.notify();
                                }),
                            )),
                    )
                    .into_any_element()
            } else {
                let empty = label(if connected {
                    if filter_value.is_empty() {
                        "The device returned no data."
                    } else {
                        "Nothing loaded matches."
                    }
                } else {
                    "Connect to browse the device."
                })
                .size(body2.size)
                .line_height(body2.line_height)
                .color(p.text.secondary)
                .no_truncate()
                .into_any_element();
                let scroll = self.ui.explorer_scroll.clone();
                let focus = self.tree_focus("explorer", cx);
                self.render_tree(
                    TreeSource::Explorer,
                    Rc::new(rows),
                    selected,
                    &filter_value,
                    empty,
                    scroll,
                    &focus,
                    cx,
                )
                .into_any_element()
            };
        let mut panel = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(scope_toggle)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .border_t_1()
                    .border_color(p.divider)
                    .child(content),
            );
        if let Some(path) = selected_path {
            let text = format_gnmi_path(&path);
            panel = panel.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .px(px(8.))
                    .py(px(4.))
                    .border_t_1()
                    .border_color(p.divider)
                    .bg(p.background_paper)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .max_h(px(48.))
                            .overflow_hidden()
                            .text_size(px(11.5))
                            .child(path_text(&text, 0, false, true, &theme)),
                    )
                    .child(
                        IconButton::new("copy-selected-path", "ContentCopy")
                            .icon_size(15.)
                            .tooltip("Copy path")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy(text.clone(), "the path", cx);
                            })),
                    ),
            );
        }
        panel
    }

    // Library

    fn render_library(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        if !self.ui.library.loaded && !self.ui.library.loading {
            self.refresh_library(cx);
        }
        let filter_value = self.ui.library_filter.clone();
        let filter = self.bound_input(
            FieldKey::LibraryFilter,
            "Find a saved request",
            &filter_value,
            window,
            cx,
        );
        let gnmi = self.is_gnmi();
        let requests = self.ui.library.requests.clone();
        let (own, shared) = library_sections(&requests, &filter_value);
        let body2 = theme.typography.body2;
        let content: AnyElement = if self.ui.library.loading && !self.ui.library.loaded {
            div()
                .p(px(16.))
                .flex()
                .justify_center()
                .child(CircularProgress::new(
                    ("navigator-progress", 3usize),
                    18.,
                    p.primary.main,
                ))
                .into_any_element()
        } else if own.is_empty() && shared.is_empty() {
            div()
                .p(px(20.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .text_center()
                .child(icon("BookmarkBorder", 28., p.text.disabled))
                .child(
                    div()
                        .text_size(px(body2.size))
                        .line_height(px(body2.line_height))
                        .text_color(p.text.secondary)
                        .child(if filter_value.is_empty() {
                            "Requests you save with the bookmark button above the editor appear here, for this host or every host."
                        } else {
                            "No saved request matches."
                        }),
                )
                .into_any_element()
        } else {
            let mut list = div().flex().flex_col();
            let title = self.title.to_string();
            let every = format!("Every {} host", if gnmi { "gNMI" } else { "NETCONF" });
            for (caption, items) in [(title, own), (every, shared)] {
                if items.is_empty() {
                    continue;
                }
                list = list.child(section_caption(caption, &theme));
                for request in items {
                    list = list.child(self.render_library_row(request, gnmi, cx));
                }
            }
            list.into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .p(px(8.))
                    .child(search_field(&filter, 30., Some(p.background_paper), cx).flex_1()),
            )
            .child(
                div()
                    .id("library")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pb(px(8.))
                    .child(content),
            )
    }

    fn render_library_row(
        &mut self,
        request: &SavedManagementRequest,
        gnmi: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let draft = draft_of(gnmi, request);
        let group: SharedString = format!("library-{}", request.id).into();
        let hover = alpha(p.text.primary, 0.05);
        let connected = self.connected();
        let body2 = theme.typography.body2;
        let load = request.clone();
        let run_button = request.clone();
        let rename = request.clone();
        let delete = request.clone();
        div()
            .id(SharedString::from(format!("saved-{}", request.id)))
            .group(group.clone())
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(6.))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                let run = event.click_count() == 2;
                this.load_saved(&load, run, cx);
            }))
            .child(Chip::new(run_label(&draft)).height(18.).font_size(10.5))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        label(request.name.clone())
                            .size(body2.size)
                            .weight(550.)
                            .line_height(body2.line_height)
                            .color(p.text.primary),
                    )
                    .child(
                        label(draft_summary(&draft))
                            .family(MONO)
                            .size(10.5)
                            .line_height(theme.typography.caption.line_height)
                            .color(p.text.secondary),
                    ),
            )
            .child(
                div()
                    .id("library-actions")
                    .flex()
                    .invisible()
                    .group_hover(group, |s| s.visible())
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        IconButton::new("run-saved", "PlayArrowRounded")
                            .icon_size(16.)
                            .tooltip("Run")
                            .disabled(!connected)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.load_saved(&run_button, true, cx);
                            })),
                    )
                    .child(
                        IconButton::new("rename-saved", "EditNoteOutlined")
                            .icon_size(16.)
                            .tooltip("Rename")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_rename(rename.clone(), window, cx);
                            })),
                    )
                    .child(
                        IconButton::new("delete-saved", "DeleteOutlined")
                            .icon_size(16.)
                            .tooltip("Delete")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm_delete(delete.clone(), cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    // History

    fn render_history(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let (runs, selected) = self.store.read(|state| {
            (
                state
                    .runs
                    .iter()
                    .filter(|run| !run.hidden)
                    .cloned()
                    .collect::<Vec<_>>(),
                state.selected_run_id.clone(),
            )
        });
        let caption = theme.typography.caption;
        let body2 = theme.typography.body2;
        let content: AnyElement = if runs.is_empty() {
            div()
                .p(px(20.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .text_center()
                .child(icon("History", 28., p.text.disabled))
                .child(
                    div()
                        .text_size(px(body2.size))
                        .line_height(px(body2.line_height))
                        .text_color(p.text.secondary)
                        .child("Every request you run is kept here with its result until the tab closes."),
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .children(runs.iter().map(|run| {
                    self.render_history_row(run, selected.as_deref() == Some(&run.id), cx)
                }))
                .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .px(px(12.))
                    .py(px(6.))
                    .child(
                        div().flex_1().min_w_0().child(
                            muxus_ui::label::paragraph(
                                "Click to show a result · double-click to run again",
                            )
                            .size(caption.size)
                            .line_height(caption.line_height)
                            .color(p.text.secondary),
                        ),
                    )
                    .child(
                        Button::new("clear-history", "Clear")
                            .color(ButtonColor::Inherit)
                            .font_size(12.)
                            .disabled(runs.is_empty())
                            .on_click(cx.listener_for_app(
                                |this: &mut Self, cx: &mut Context<Self>| {
                                    this.store.update(|state| state.clear_history());
                                    cx.notify();
                                },
                            )),
                    ),
            )
            .child(
                div()
                    .id("history")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(p.divider)
                    .child(content),
            )
    }

    fn render_history_row(
        &mut self,
        run: &RunRecord,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let failed = run_failed(run);
        let meta = history_meta(run, failed);
        let group: SharedString = format!("history-{}", run.id).into();
        let base = if selected {
            alpha(p.primary.main, 0.08)
        } else {
            muxus_ui::gpui_kit::Hsla::transparent_black()
        };
        let hover = if selected {
            alpha(p.primary.main, 0.12)
        } else {
            alpha(p.text.primary, 0.05)
        };
        let body2 = theme.typography.body2;
        let id = run.id.clone();
        let rerun = run.draft.clone();
        let load = run.draft.clone();
        let rerun_button = run.draft.clone();
        let stop = run.id.clone();
        div()
            .id(SharedString::from(format!("run-{}", run.id)))
            .group(group.clone())
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(6.))
            .cursor_pointer()
            .border_l_2()
            .border_color(if selected {
                p.primary.main
            } else {
                muxus_ui::gpui_kit::Hsla::transparent_black()
            })
            .bg(base)
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.rerun(rerun.clone(), cx);
                } else {
                    let id = id.clone();
                    this.store.update(|state| state.selected_run_id = Some(id));
                    cx.notify();
                }
            }))
            .child(status_icon(run, 15., &theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(6.))
                            .min_w_0()
                            .child(
                                label(run.label.clone())
                                    .size(body2.size)
                                    .weight(600.)
                                    .line_height(body2.line_height)
                                    .color(if failed { p.error.main } else { p.text.primary })
                                    .no_truncate(),
                            )
                            .child(
                                label(run.summary.clone())
                                    .family(MONO)
                                    .size(11.)
                                    .line_height(theme.typography.caption.line_height)
                                    .color(p.text.secondary),
                            ),
                    )
                    .child(super::widgets::inline_span(
                        meta,
                        10.5,
                        muxus_ui::theme::fonts::UI,
                        p.text.secondary,
                    )),
            )
            .child(
                div()
                    .id("history-actions")
                    .flex()
                    .invisible()
                    .group_hover(group, |s| s.visible())
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(if run.status == RunStatus::Streaming {
                        IconButton::new("stop-run", "StopRounded")
                            .icon_size(16.)
                            .tooltip("Stop")
                            .on_click(
                                cx.listener(move |this, _, _, _| this.controller.cancel(&stop)),
                            )
                    } else {
                        IconButton::new("rerun", "PlayArrowRounded")
                            .icon_size(16.)
                            .tooltip("Run again")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.rerun(rerun_button.clone(), cx)
                            }))
                    })
                    .child(
                        IconButton::new("load-run", "EditNoteOutlined")
                            .icon_size(16.)
                            .tooltip("Load into the editor")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let draft = load.clone();
                                this.store.update(|state| state.draft = draft);
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    // Device

    fn render_device(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let Some(info) = self.controller.info.clone() else {
            return div();
        };
        let filter_value = self.ui.device_filter.clone();
        let needle = muxus_core::js::js_trim(&filter_value).to_lowercase();
        let caption = theme.typography.caption;
        let info_row = |label_text: &str, value: Option<String>| -> Option<Div> {
            let value = value.filter(|value| !value.is_empty())?;
            Some(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(
                        div().w(px(84.)).flex_none().child(
                            label(label_text.to_owned())
                                .size(caption.size)
                                .line_height(caption.line_height)
                                .color(p.text.secondary),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(MONO)
                            .text_size(px(11.))
                            .line_height(px(caption.line_height))
                            .text_color(p.text.primary)
                            .child(value),
                    ),
            )
        };
        let connected_at = |iso: &str| parse_ms(iso).map(locale_date_time);
        match info {
            ManagementSessionInfo::Gnmi(info) => {
                let placeholder = format!("Find among {} models", info.models.len());
                let filter = self.bound_input(
                    FieldKey::DeviceFilter,
                    &placeholder,
                    &filter_value,
                    window,
                    cx,
                );
                let models: Vec<_> = info
                    .models
                    .iter()
                    .filter(|model| {
                        needle.is_empty()
                            || format!("{} {} {}", model.name, model.organization, model.version)
                                .to_lowercase()
                                .contains(&needle)
                    })
                    .cloned()
                    .collect();
                let rows = [
                    info_row("gNMI", Some(info.version.clone())),
                    info_row("Encodings", Some(encodings_text(&info.encodings))),
                    info_row("Address", Some(info.address.clone())),
                    info_row("Via", info.via.clone()),
                    info_row("TLS", Some(tls_row(&info.tls))),
                    info_row("Certificate", info.tls.subject.clone()),
                    info_row("Connected", connected_at(&info.connected_at)),
                ];
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .gap(px(4.))
                            .p(px(12.))
                            .border_b_1()
                            .border_color(p.divider)
                            .children(rows.into_iter().flatten()),
                    )
                    .child(
                        div().flex().flex_none().p(px(8.)).child(
                            search_field(&filter, 30., Some(p.background_paper), cx).flex_1(),
                        ),
                    )
                    .child(
                        div()
                            .id("models")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .children(models.into_iter().map(|model| {
                                let meta = [model.organization.clone(), model.version.clone()]
                                    .into_iter()
                                    .filter(|text| !text.is_empty())
                                    .collect::<Vec<_>>()
                                    .join(" · ");
                                div()
                                    .px(px(12.))
                                    .py(px(4.))
                                    .child(
                                        div()
                                            .font_family(MONO)
                                            .text_size(px(11.5))
                                            .line_height(px(11.5 * 1.43))
                                            .text_color(p.text.primary)
                                            .child(model.name),
                                    )
                                    .child(super::widgets::inline_span(
                                        meta,
                                        10.5,
                                        muxus_ui::theme::fonts::UI,
                                        p.text.secondary,
                                    ))
                            })),
                    )
            }
            ManagementSessionInfo::Netconf(info) => {
                let all_modules = hello_modules(&info.capabilities);
                let placeholder = format!("Find among {} YANG modules", all_modules.len());
                let filter = self.bound_input(
                    FieldKey::DeviceFilter,
                    &placeholder,
                    &filter_value,
                    window,
                    cx,
                );
                let modules: Vec<_> = all_modules
                    .into_iter()
                    .filter(|module| {
                        needle.is_empty() || module.name.to_lowercase().contains(&needle)
                    })
                    .collect();
                let can_schema = self.controller.has_capability("ietf-netconf-monitoring");
                let connected = self.connected();
                let banner = self.store.read(|state| state.banner.clone());
                let notifications = self.store.read(|state| state.notifications.clone());
                let mut head = div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(4.))
                    .p(px(12.))
                    .border_b_1()
                    .border_color(p.divider)
                    .children(
                        [
                            info_row("Session", Some(info.session_id.clone())),
                            info_row("Framing", Some(framing_text(info.base).into())),
                            info_row("Address", Some(info.address.clone())),
                            info_row("Via", info.via.clone()),
                            info_row("Connected", connected_at(&info.connected_at)),
                        ]
                        .into_iter()
                        .flatten(),
                    );
                if let Some(banner) = banner {
                    let anchor = AnchorBounds::default();
                    head = head.child(
                        div().flex().child(
                            div()
                                .id("login-banner")
                                .relative()
                                .child(
                                    label("Login banner")
                                        .size(caption.size)
                                        .line_height(caption.line_height)
                                        .color(p.primary.main)
                                        .no_truncate(),
                                )
                                .child(bounds_probe(&anchor))
                                .tooltip(LinesTooltip::pre(banner, anchor)),
                        ),
                    );
                }
                let mut chips = div().flex().flex_wrap().gap(px(4.)).pt(px(6.));
                for (fragment, text, detail) in NETCONF_CAPABILITIES {
                    let supported = self.controller.has_capability(fragment);
                    let mut chip = Chip::new(*text)
                        .height(20.)
                        .font_size(10.5)
                        .opacity(if supported { 1. } else { 0.5 })
                        .tooltip(
                            ("navigator-chip-tip", 2usize),
                            format!(
                                "{detail}{}",
                                if supported { "" } else { " — not supported" }
                            ),
                        );
                    chip = if supported {
                        chip.icon("CheckRounded").icon_size(13.)
                    } else {
                        chip.outlined()
                    };
                    chips = chips.child(chip);
                }
                head = head.child(chips);
                let mut panel = div().flex_1().min_h_0().flex().flex_col().child(head);
                if !notifications.is_empty() {
                    panel = panel.child(self.render_notifications(&notifications, cx));
                }
                panel
                    .child(
                        div().flex().flex_none().p(px(8.)).child(
                            search_field(&filter, 30., Some(p.background_paper), cx).flex_1(),
                        ),
                    )
                    .child(
                        div()
                            .id("modules")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .children(modules.into_iter().map(|module| {
                                let hover = alpha(p.text.primary, 0.05);
                                let name = module.name.clone();
                                let revision = module.revision.clone();
                                let mut row = div()
                                    .id(SharedString::from(format!("module-{}", module.name)))
                                    .px(px(12.))
                                    .py(px(4.))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .font_family(MONO)
                                                    .text_size(px(11.5))
                                                    .line_height(px(11.5 * 1.43))
                                                    .child(module.name.clone()),
                                            )
                                            .child(super::widgets::inline_span(
                                                module
                                                    .revision
                                                    .clone()
                                                    .unwrap_or_else(|| "no revision".into()),
                                                10.5,
                                                muxus_ui::theme::fonts::UI,
                                                p.text.secondary,
                                            )),
                                    );
                                if can_schema {
                                    row = row
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(hover))
                                        .child(
                                            div()
                                                .id("schema-icon")
                                                .child(icon(
                                                    "DescriptionOutlined",
                                                    15.,
                                                    p.text.secondary,
                                                ))
                                                .tooltip(Tooltip::text(
                                                    "Open the YANG module (get-schema)",
                                                )),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if connected {
                                                this.open_schema(&name, revision.clone(), cx);
                                            }
                                        }));
                                }
                                row
                            })),
                    )
            }
        }
    }

    fn render_notifications(
        &mut self,
        notifications: &[muxus_mgmt::workbench::store::ReceivedNotification],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let open = self.ui.notification_open;
        div()
            .id("notifications")
            .flex_none()
            .max_h(muxus_ui::gpui_kit::relative(0.4))
            .overflow_y_scroll()
            .border_b_1()
            .border_color(p.divider)
            .child(section_caption(
                format!("Notifications · {}", notifications.len()),
                &theme,
            ))
            .children(
                notifications
                    .iter()
                    .enumerate()
                    .map(|(index, notification)| {
                        let at = notification
                            .event_time
                            .as_deref()
                            .and_then(parse_ms)
                            .or_else(|| parse_ms(&notification.received_at))
                            .map(locale_time)
                            .unwrap_or_default();
                        let mut item = div().px(px(12.)).py(px(2.)).child(
                            div()
                                .id(("notification", index))
                                .flex()
                                .items_baseline()
                                .gap(px(8.))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.ui.notification_open =
                                        if this.ui.notification_open == Some(index) {
                                            None
                                        } else {
                                            Some(index)
                                        };
                                    cx.notify();
                                }))
                                .child(
                                    label(notification_event(&notification.xml))
                                        .family(MONO)
                                        .size(11.5)
                                        .line_height(theme.typography.body2.line_height)
                                        .color(p.text.primary)
                                        .grow(),
                                )
                                .child(
                                    label(at)
                                        .size(10.5)
                                        .line_height(theme.typography.caption.line_height)
                                        .color(p.text.secondary)
                                        .no_truncate(),
                                ),
                        );
                        if open == Some(index) {
                            item = item.child(
                                div()
                                    .mt(px(4.))
                                    .p(px(8.))
                                    .rounded(px(theme.radius))
                                    .bg(p.background_paper)
                                    .font_family(MONO)
                                    .text_size(px(10.5))
                                    .max_h(px(220.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .children(
                                        pretty_xml(&notification.xml)
                                            .lines()
                                            .map(|line| {
                                                div().child(SharedString::from(line.to_owned()))
                                            })
                                            .collect::<Vec<_>>(),
                                    ),
                            );
                        }
                        item
                    }),
            )
            .into_any_element()
    }

    /// The library, fetched again.
    pub(super) fn refresh_library(&mut self, cx: &mut Context<Self>) {
        let profile_id = match self.dialed_profile() {
            muxus_core::protocol::ManagementProfile::Gnmi(profile) => profile.profile_id.clone(),
            muxus_core::protocol::ManagementProfile::Netconf(profile) => profile.profile_id.clone(),
        };
        let request = self
            .backend
            .management_requests(self.protocol(), profile_id);
        self.ui.library.loading = true;
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.ui.library.loading = false;
                this.ui.library.loaded = true;
                match result {
                    Ok(requests) => this.ui.library.requests = requests,
                    Err(error) => this.toast(muxus_ui::theme::Status::Error, error.message, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_saved(&mut self, request: &SavedManagementRequest, run: bool, cx: &mut Context<Self>) {
        let draft = draft_of(self.is_gnmi(), request);
        self.store.update(|state| state.draft = draft.clone());
        if run {
            self.controller.run(Some(draft), false);
        }
        cx.notify();
    }

    fn rerun(&mut self, draft: ManagementDraft, cx: &mut Context<Self>) {
        if !self.connected() {
            return;
        }
        self.store.update(|state| state.draft = draft.clone());
        self.controller.run(Some(draft), false);
        cx.notify();
    }

    fn open_schema(&mut self, name: &str, revision: Option<String>, cx: &mut Context<Self>) {
        let draft = ManagementDraft::Netconf(muxus_mgmt::workbench::requests::NetconfDraft {
            operation: muxus_mgmt::workbench::requests::NetconfOperation::GetSchema,
            schema_identifier: name.to_owned(),
            schema_version: revision.unwrap_or_default(),
            ..muxus_mgmt::workbench::requests::default_netconf_draft()
        });
        self.store.update(|state| state.draft = draft.clone());
        self.controller.run(Some(draft), false);
        cx.notify();
    }
}

/// `cx.listener` for index callbacks (toggle groups).
pub(super) trait ListenerIndex<T> {
    fn listener_index(
        &self,
        f: impl Fn(&mut T, usize, &mut Context<T>) + 'static,
    ) -> impl Fn(usize, &mut Window, &mut muxus_ui::gpui_kit::App) + 'static;
}

impl<T: 'static> ListenerIndex<T> for Context<'_, T> {
    fn listener_index(
        &self,
        f: impl Fn(&mut T, usize, &mut Context<T>) + 'static,
    ) -> impl Fn(usize, &mut Window, &mut muxus_ui::gpui_kit::App) + 'static {
        let entity = self.weak_entity();
        let f = Rc::new(f);
        move |index, _: &mut Window, cx: &mut muxus_ui::gpui_kit::App| {
            let f = f.clone();
            let _ = entity.update(cx, |this, cx| f(this, index, cx));
        }
    }
}
