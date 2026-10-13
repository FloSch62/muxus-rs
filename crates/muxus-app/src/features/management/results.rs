//! The result pane (`ResultPane.tsx`): what the selected run returned, as a
//! tree, a table of leaves or the raw reply, with errors explained and the
//! quick starts of an empty workbench.

use super::navigator::ListenerIndex;
use super::workbench::ListenerForApp;
use std::rc::Rc;

use muxus_core::gnmi_path::split_path_options;
use muxus_core::management::{GnmiNotification, ManagementProtocol, ManagementResult};
use muxus_mgmt::workbench::data_tree::{
    DataNode, collect_leaves, count_leaves, create_root, merge_xml, node_to_json,
};
use muxus_mgmt::workbench::requests::{GnmiSetItemOp, ManagementDraft, NetconfOperation};
use muxus_mgmt::workbench::store::{ResultView, RunRecord, RunStatus};
use muxus_mgmt::workbench::xml::{RpcErrorInfo, parse_rpc_reply, pretty_xml, serialize_element};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, Context, Div, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px, relative, uniform_list,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::{Alert, IconButton};

use super::fields::{EditorKey, FieldKey};
use super::logic::device::run_header_meta;
use super::logic::editor::HOTKEY_MOD_LABEL;
use super::logic::format::{export_filename, plural, time_of};
use super::logic::results::{
    empty_text, fix_path, get_counts, hint_for, leaf_table_skip, netconf_failed, ok_view_message,
    previous_run, quick_starts, set_timestamp_ms, verify_draft,
};
use super::logic::tree::{expand_all, initial_expansion, needle_of, tree_from_notifications};
use super::logic::values::{value_text, value_tone};
use super::navigator::status_icon;
use super::path_text::path_text;
use super::tree_view::{TreeOptions, rows_of, tone_color};
use super::ui::{ResultTree, TreeSource};
use super::view::ManagementView;
use super::widgets::MONO;
use muxus_ui::widgets::button::Button;
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::search_field::input_base;
use muxus_ui::widgets::toggle_button::ToggleButtonGroup;
use muxus_ui::widgets::toggle_button::ToggleOption;

/// The Tree/Table/JSON (or Tree/XML) switch.
fn view_toggle(
    options: &[(&str, &str)],
    selected: usize,
    on_select: impl Fn(usize, &mut Window, &mut muxus_ui::gpui_kit::App) + 'static,
) -> ToggleButtonGroup {
    ToggleButtonGroup::exclusive(
        "result-view",
        options
            .iter()
            .map(|(text, icon_name)| ToggleOption::new(*text).icon(*icon_name))
            .collect(),
        Some(selected),
        on_select,
    )
    .inline(2., 7.2, 11.5)
    .icon_style(14., 4.)
}

fn toolbar(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .px(px(12.))
        .py(px(4.))
        .min_h(px(38.))
        .border_b_1()
        .border_color(theme.palette.divider)
}

fn caption_text(text: impl Into<SharedString>, theme: &Theme) -> impl IntoElement {
    let caption = theme.typography.caption;
    label(text)
        .size(caption.size)
        .line_height(caption.line_height)
        .color(theme.palette.text.secondary)
        .no_truncate()
}

impl ManagementView {
    pub(super) fn render_result_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let run = self.store.read(|state| {
            state
                .runs
                .iter()
                .find(|run| Some(&run.id) == state.selected_run_id.as_ref())
                .cloned()
        });
        let Some(run) = run else {
            return self.render_empty_result(cx).into_any_element();
        };
        let mut pane = div().flex_1().min_w_0().flex().flex_col();
        if run.live.is_none() {
            pane = pane.child(self.render_run_header(&run, cx));
        }
        pane.child(self.render_run_body(&run, window, cx))
            .into_any_element()
    }

    fn render_run_header(&mut self, run: &RunRecord, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let body2 = theme.typography.body2;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .min_h(px(36.))
            .border_b_1()
            .border_color(p.divider)
            .child(status_icon(run, 16., &theme))
            .child(
                label(run.label.clone())
                    .size(body2.size)
                    .weight(650.)
                    .line_height(body2.line_height)
                    .color(p.text.primary)
                    .no_truncate(),
            )
            .child(
                div()
                    .id("run-summary")
                    .min_w_0()
                    .flex_shrink(1.)
                    .child(
                        label(run.summary.clone())
                            .family(MONO)
                            .size(12.)
                            .line_height(12. * 1.43)
                            .color(p.text.secondary),
                    )
                    .tooltip(muxus_ui::widgets::Tooltip::text(run.summary.clone())),
            )
            .child(div().flex_1())
            .child(caption_text(run_header_meta(run), &theme))
    }

    fn render_run_body(
        &mut self,
        run: &RunRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        if run.live.is_some() {
            return self.render_live_view(run, window, cx).into_any_element();
        }
        match run.status {
            RunStatus::Running => {
                let id = run.id.clone();
                return div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .child(CircularProgress::new(
                        ("results-progress", 1usize),
                        28.,
                        p.primary.main,
                    ))
                    .child(
                        label("Waiting for the device …")
                            .size(theme.typography.body2.size)
                            .line_height(theme.typography.body2.line_height)
                            .color(p.text.secondary)
                            .no_truncate(),
                    )
                    .child(
                        muxus_ui::widgets::Button::new("cancel-waiting", "Cancel (Esc)")
                            .color(muxus_ui::widgets::ButtonColor::Inherit)
                            .on_click(
                                cx.listener(move |this, _, _, _| this.controller.cancel(&id)),
                            ),
                    )
                    .into_any_element();
            }
            RunStatus::Error | RunStatus::Cancelled => {
                return self.render_error_view(run, cx).into_any_element();
            }
            _ => {}
        }
        match run.result.clone() {
            Some(ManagementResult::GnmiGet { notifications }) => {
                self.render_get_view(run, &notifications, window, cx)
            }
            Some(ManagementResult::GnmiSet { timestamp, results }) => self
                .render_set_result(run, &timestamp, &results, cx)
                .into_any_element(),
            Some(ManagementResult::NetconfRpc { xml, .. }) => {
                self.render_netconf_reply(run, &xml, window, cx)
            }
            _ => div().into_any_element(),
        }
    }

    // Errors

    fn render_error_view(&mut self, run: &RunRecord, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let error = run
            .error
            .clone()
            .unwrap_or(muxus_core::management::ManagementError {
                message: "Failed.".into(),
                code: None,
                cancelled: None,
            });
        let options = split_path_options(&error.message);
        let cancelled = run.status == RunStatus::Cancelled;
        let body2 = theme.typography.body2;
        let mut title = div().flex().items_center().gap(px(8.)).child(
            label(if cancelled {
                "Cancelled".to_owned()
            } else {
                format!("{} failed", run.label)
            })
            .size(body2.size)
            .weight(650.)
            .line_height(body2.line_height)
            .no_truncate(),
        );
        if let Some(code) = &error.code {
            title = title.child(Chip::new(code.clone()).height(20.).font_size(11.).mono());
        }
        let mut content = div().flex().flex_col().gap(px(8.)).child(title).child(
            div()
                .font_family(MONO)
                .text_size(px(12.5))
                .line_height(px(12.5 * 1.43))
                .child(error.message.clone()),
        );
        let gnmi_draft = match &run.draft {
            ManagementDraft::Gnmi(draft) => Some(draft.clone()),
            ManagementDraft::Netconf(_) => None,
        };
        if let (Some(bad), Some(draft)) = (options.bad.clone(), gnmi_draft)
            && !options.options.is_empty()
        {
            let caption = theme.typography.caption;
            let mut chips = div().flex().flex_wrap().gap(px(4.));
            for option in options.options {
                let bad = bad.clone();
                let draft = draft.clone();
                let fix = option.clone();
                chips = chips.child(Chip::new(option).font_size(11.5).mono().on_click(
                    ("results-chip", 2usize),
                    cx.listener_for_app(move |this: &mut Self, cx: &mut Context<Self>| {
                        let next = fix_path(&draft, &bad, &fix);
                        this.store
                            .update(|state| state.draft = ManagementDraft::Gnmi(next.clone()));
                        if next.operation != muxus_mgmt::workbench::requests::GnmiOperation::Set {
                            this.controller
                                .run(Some(ManagementDraft::Gnmi(next)), false);
                        }
                        cx.notify();
                    }),
                ));
            }
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        label(format!("The device offers these instead of “{bad}”:"))
                            .size(caption.size)
                            .line_height(caption.line_height)
                            .color(p.text.secondary),
                    )
                    .child(chips),
            );
        }
        if let Some(hint) = hint_for(error.code.as_deref(), &error.message) {
            content = content.child(caption_text(hint, &theme));
        }
        let mut alert = Alert::new(
            if cancelled {
                Status::Info
            } else {
                Status::Error
            },
            "",
        )
        .outlined()
        .content(content);
        if !cancelled {
            alert = alert.icon("ErrorRounded");
        }
        div().p(px(16.)).child(alert)
    }

    // gNMI Get

    /// The tree of a run's result, built once.
    fn result_tree(&mut self, run: &RunRecord) -> &mut ResultTree {
        if !self.ui.results.contains_key(&run.id) {
            let root = match &run.result {
                Some(ManagementResult::GnmiGet { notifications }) => {
                    let registry = self.store.read(|state| state.explorer.registry.clone());
                    tree_from_notifications(notifications, &registry)
                }
                Some(ManagementResult::NetconfRpc { xml, .. }) => {
                    let mut root = create_root(Default::default());
                    let schema = matches!(&run.draft, ManagementDraft::Netconf(draft) if draft.operation == NetconfOperation::GetSchema);
                    if !schema && let Some(data) = parse_rpc_reply(xml).and_then(|reply| reply.data)
                    {
                        merge_xml(&mut root, &data);
                    }
                    root
                }
                _ => create_root(Default::default()),
            };
            let expanded = initial_expansion(&root);
            self.ui.results.insert(
                run.id.clone(),
                ResultTree {
                    root,
                    expanded,
                    selected: None,
                },
            );
        }
        self.ui.results.get_mut(&run.id).expect("inserted above")
    }

    fn render_tree_tools(&mut self, run_id: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let expand = run_id.to_owned();
        let collapse = run_id.to_owned();
        vec![
            IconButton::new("expand-all", "UnfoldMore")
                .tooltip("Expand all")
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(tree) = this.ui.results.get_mut(&expand) {
                        tree.expanded = expand_all(&tree.root);
                    }
                    cx.notify();
                }))
                .into_any_element(),
            IconButton::new("collapse-all", "UnfoldLess")
                .tooltip("Collapse all")
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(tree) = this.ui.results.get_mut(&collapse) {
                        tree.expanded.clear();
                    }
                    cx.notify();
                }))
                .into_any_element(),
        ]
    }

    fn render_compare_button(&mut self, run: &RunRecord, cx: &mut Context<Self>) -> AnyElement {
        let previous = self
            .store
            .read(|state| previous_run(&state.runs, run).map(|previous| previous.started_at));
        let run = run.clone();
        IconButton::new("compare", "CompareArrows")
            .tooltip(match previous {
                Some(at) => format!("Compare with the run at {}", time_of(at)),
                None => "Run the same request again to compare".into(),
            })
            .disabled(previous.is_none())
            .on_click(cx.listener(move |this, _, _, cx| this.open_compare(&run, cx)))
            .into_any_element()
    }

    fn render_filter_box(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let value = self.ui.result_filter.clone();
        let input = self.bound_input(FieldKey::ResultFilter, "Filter", &value, window, cx);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .px(px(8.))
            .h(px(28.))
            .w(px(190.))
            .border_1()
            .border_color(theme.palette.divider)
            .rounded(px(theme.radius))
            .child(icon("Search", 16., theme.palette.text.secondary))
            .child(input_base(&input, 12.5, false, cx))
    }

    fn render_get_view(
        &mut self,
        run: &RunRecord,
        notifications: &[GnmiNotification],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let view = self.store.read(|state| state.result_view);
        let filter = self.ui.result_filter.clone();
        let tree = self.result_tree(run);
        let leaves = count_leaves(&tree.root);
        let json = serde_json::to_string_pretty(&node_to_json(&tree.root)).unwrap_or_default();
        let mut bar = toolbar(&theme)
            .child(view_toggle(
                &[
                    ("Tree", "AccountTreeOutlined"),
                    ("Table", "TableRowsOutlined"),
                    ("JSON", "DataObjectOutlined"),
                ],
                match view {
                    ResultView::Tree => 0,
                    ResultView::Table => 1,
                    ResultView::Raw => 2,
                },
                cx.listener_index(|this, index, cx| {
                    let view = [ResultView::Tree, ResultView::Table, ResultView::Raw][index];
                    this.store.update(|state| state.result_view = view);
                    cx.notify();
                }),
            ))
            .child(caption_text(
                get_counts(leaves, notifications.len()),
                &theme,
            ))
            .child(div().flex_1());
        if view != ResultView::Raw {
            bar = bar.child(self.render_filter_box(window, cx));
        }
        if view == ResultView::Tree {
            bar = bar.children(self.render_tree_tools(&run.id, cx));
        }
        let raw_text = serde_json::to_string_pretty(notifications).unwrap_or_default();
        let save_name = export_filename(
            &format!("gnmi {}", run.summary),
            "json",
            chrono::Local::now(),
        );
        let save_text = raw_text.clone();
        bar = bar
            .child(self.render_compare_button(run, cx))
            .child(
                IconButton::new("copy-json", "ContentCopy")
                    .tooltip("Copy as JSON")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy(json.clone(), "the result as JSON", cx);
                    })),
            )
            .child(
                IconButton::new("save-json", "FileDownloadOutlined")
                    .tooltip("Save as JSON")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.save_file(save_name.clone(), save_text.clone(), cx);
                    })),
            );
        let body: AnyElement = match view {
            ResultView::Tree => self.render_result_tree(
                run,
                &filter,
                if filter.is_empty() {
                    "The device returned no data for this path."
                } else {
                    "Nothing matches the filter."
                },
                cx,
            ),
            ResultView::Table => {
                let requested = match &run.draft {
                    ManagementDraft::Gnmi(draft) => draft.paths.clone(),
                    ManagementDraft::Netconf(_) => Vec::new(),
                };
                self.render_leaf_table(run, &filter, &requested, cx)
            }
            ResultView::Raw => div()
                .flex_1()
                .min_w_0()
                .p(px(8.))
                .child(self.code_editor(
                    EditorKey::Result(run.id.clone()),
                    "json",
                    &raw_text,
                    true,
                    None,
                    true,
                    "",
                    window,
                    cx,
                ))
                .into_any_element(),
        };
        let _ = p;
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(bar)
            .child(div().flex_1().min_h_0().flex().child(body))
            .into_any_element()
    }

    fn render_result_tree(
        &mut self,
        run: &RunRecord,
        filter: &str,
        empty: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let tree = self.result_tree(run);
        let rows = rows_of(
            &tree.root,
            &TreeOptions {
                expanded: &tree.expanded,
                filter,
                lazy: false,
                loading: None,
                errors: None,
            },
        );
        let selected = tree.selected.clone();
        let empty = paragraph(empty.to_owned())
            .size(theme.typography.body2.size)
            .line_height(theme.typography.body2.line_height)
            .color(theme.palette.text.secondary)
            .into_any_element();
        let scroll = self.ui.result_scroll.clone();
        let focus = self.tree_focus("result", cx);
        div()
            .flex_1()
            .min_w_0()
            .child(self.render_tree(
                TreeSource::Result(run.id.clone()),
                Rc::new(rows),
                selected,
                filter,
                empty,
                scroll,
                &focus,
                cx,
            ))
            .into_any_element()
    }

    fn render_leaf_table(
        &mut self,
        run: &RunRecord,
        filter: &str,
        requested: &[String],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tree = self.result_tree(run);
        let needle = needle_of(filter);
        let rows: Vec<_> = collect_leaves(&tree.root, usize::MAX)
            .into_iter()
            .filter(|row| {
                needle.is_empty()
                    || row.path.to_lowercase().contains(&needle)
                    || value_text(row.value.as_ref())
                        .to_lowercase()
                        .contains(&needle)
            })
            .collect();
        let skip = leaf_table_skip(requested);
        let rows = Rc::new(rows);
        let count = rows.len();
        let list = uniform_list(
            "leaf-table",
            count,
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                let theme = *cx.theme();
                let p = &theme.palette;
                range
                    .filter_map(|index| rows.get(index))
                    .map(|row| {
                        let value = value_text(row.value.as_ref());
                        let hover = p.action.hover;
                        div()
                            .id(SharedString::from(row.path.clone()))
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(16.))
                            .px(px(12.))
                            .h(px(26.))
                            .text_size(px(12.5))
                            .border_b_1()
                            .border_color(p.divider)
                            .hover(move |s| s.bg(hover))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .child(path_text(&row.path, skip, false, false, &theme)),
                            )
                            .child(
                                div()
                                    .w(relative(0.35))
                                    .font_family(MONO)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(tone_color(value_tone(row.value.as_ref()), &theme))
                                    .child(value),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.ui.table_scroll)
        .flex_1()
        .min_w_0();
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(list)
            .into_any_element()
    }

    // gNMI Set

    fn render_set_result(
        &mut self,
        run: &RunRecord,
        timestamp: &str,
        results: &[muxus_core::management::GnmiSetResult],
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let body2 = theme.typography.body2;
        let at = set_timestamp_ms(timestamp);
        let headline = format!(
            "The device applied {}{}.",
            plural(results.len(), "change", "changes"),
            if at > 0 {
                format!(" at {}", time_of(at))
            } else {
                String::new()
            }
        );
        let mut list = div().flex().flex_col().gap(px(2.));
        for result in results {
            let op = serde_json::to_value(result.op)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            list = list.child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(Chip::new(op).height(18.).font_size(10.5).min_width(56.))
                    .child(div().text_size(px(12.5)).child(path_text(
                        &result.path,
                        0,
                        false,
                        true,
                        &theme,
                    ))),
            );
        }
        let mut content = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                label(headline)
                    .size(body2.size)
                    .weight(650.)
                    .line_height(body2.line_height)
                    .no_truncate(),
            )
            .child(list);
        if let ManagementDraft::Gnmi(draft) = &run.draft
            && draft
                .set
                .iter()
                .any(|item| item.op != GnmiSetItemOp::Delete)
        {
            let draft = draft.clone();
            content = content.child(
                div().flex().child(
                    Button::new("verify", "Get the configuration now")
                        .start_icon("PlayArrowRounded")
                        .on_click(cx.listener_for_app(
                            move |this: &mut Self, cx: &mut Context<Self>| {
                                if let Some(next) = verify_draft(&draft) {
                                    let next = ManagementDraft::Gnmi(next);
                                    this.store.update(|state| state.draft = next.clone());
                                    this.controller.run(Some(next), false);
                                    cx.notify();
                                }
                            },
                        )),
                ),
            );
        }
        div()
            .p(px(16.))
            .child(Alert::new(Status::Success, "").outlined().content(content))
    }

    // NETCONF

    fn render_rpc_error(error: &RpcErrorInfo, theme: &Theme) -> Div {
        let body2 = theme.typography.body2;
        let mut title = div().flex().flex_wrap().items_center().gap(px(6.)).child(
            label(
                error
                    .message
                    .clone()
                    .or_else(|| error.tag.clone())
                    .unwrap_or_else(|| "Error".into()),
            )
            .size(body2.size)
            .weight(650.)
            .line_height(body2.line_height)
            .no_truncate(),
        );
        if let Some(tag) = &error.tag {
            title = title.child(Chip::new(tag.clone()).height(20.).font_size(11.).mono());
        }
        if let Some(kind) = &error.error_type {
            title = title.child(
                Chip::new(kind.clone())
                    .height(20.)
                    .font_size(11.)
                    .outlined(),
            );
        }
        let mut content = div().flex().flex_col().gap(px(6.)).child(title);
        if let Some(path) = &error.path {
            content = content.child(
                div()
                    .font_family(MONO)
                    .text_size(px(12.))
                    .child(format!("at {path}")),
            );
        }
        if !error.info.is_empty() {
            let caption = theme.typography.caption;
            content = content.child(div().flex().flex_col().gap(px(2.)).children(
                error.info.iter().map(|(name, value)| {
                    div()
                        .font_family(MONO)
                        .text_size(px(caption.size))
                        .line_height(px(caption.line_height))
                        .flex()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_color(theme.palette.text.secondary)
                                .child(format!("{name}:")),
                        )
                        .child(value.clone())
                }),
            ));
        }
        let status = if error.severity.as_deref() == Some("warning") {
            Status::Warning
        } else {
            Status::Error
        };
        div().child(Alert::new(status, "").outlined().content(content))
    }

    fn render_netconf_reply(
        &mut self,
        run: &RunRecord,
        xml: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let view = self.store.read(|state| state.result_view);
        let reply = parse_rpc_reply(xml);
        let draft = match &run.draft {
            ManagementDraft::Netconf(draft) => draft.clone(),
            ManagementDraft::Gnmi(_) => return div().into_any_element(),
        };
        let schema = (draft.operation == NetconfOperation::GetSchema)
            .then(|| reply.as_ref().and_then(|reply| reply.data.as_ref()))
            .flatten()
            .map(|data| data.text_content().trim().to_owned());
        if let Some(schema) = schema {
            let lines = schema.split('\n').count();
            let name = format!(
                "{}.yang",
                if draft.schema_identifier.is_empty() {
                    "module"
                } else {
                    &draft.schema_identifier
                }
            );
            let save = schema.clone();
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    toolbar(&theme)
                        .child(caption_text(
                            format!("YANG module {} · {lines} lines", draft.schema_identifier),
                            &theme,
                        ))
                        .child(div().flex_1())
                        .child(
                            IconButton::new("save-yang", "FileDownloadOutlined")
                                .tooltip("Save as .yang")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.save_file(name.clone(), save.clone(), cx)
                                })),
                        ),
                )
                .child(div().flex_1().min_h_0().p(px(8.)).flex().child(
                    div().flex_1().min_w_0().child(self.code_editor(
                        EditorKey::Result(run.id.clone()),
                        "text",
                        &schema,
                        true,
                        None,
                        true,
                        "",
                        window,
                        cx,
                    )),
                ))
                .into_any_element();
        }
        let has_data = reply.as_ref().is_some_and(|reply| reply.data.is_some());
        let ok = reply.as_ref().is_some_and(|reply| reply.ok);
        let errors = reply
            .as_ref()
            .map(|reply| reply.errors.clone())
            .unwrap_or_default();
        let filter = self.ui.result_filter.clone();
        let leaves = if has_data {
            count_leaves(&self.result_tree(run).root)
        } else {
            0
        };
        let raw = view == ResultView::Raw;
        let mut bar = toolbar(&theme);
        if has_data {
            bar = bar
                .child(view_toggle(
                    &[
                        ("Tree", "AccountTreeOutlined"),
                        ("XML", "DataObjectOutlined"),
                    ],
                    if raw { 1 } else { 0 },
                    cx.listener_index(|this, index, cx| {
                        let view = if index == 0 {
                            ResultView::Tree
                        } else {
                            ResultView::Raw
                        };
                        this.store.update(|state| state.result_view = view);
                        cx.notify();
                    }),
                ))
                .child(caption_text(plural(leaves, "value", "values"), &theme));
        }
        if !has_data && ok {
            bar = bar.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(icon("CheckCircleRounded", 16., p.success.main))
                    .child(
                        label("OK")
                            .size(theme.typography.body2.size)
                            .weight(600.)
                            .line_height(theme.typography.body2.line_height)
                            .no_truncate(),
                    ),
            );
        }
        bar = bar.child(div().flex_1());
        if has_data && !raw {
            bar = bar.child(self.render_filter_box(window, cx));
            bar = bar.children(self.render_tree_tools(&run.id, cx));
        }
        if has_data {
            bar = bar.child(self.render_compare_button(run, cx));
        }
        let copy = pretty_xml(xml);
        let save = copy.clone();
        let save_name = export_filename(
            &format!("netconf {}", run.label),
            "xml",
            chrono::Local::now(),
        );
        bar = bar
            .child(
                IconButton::new("copy-reply", "ContentCopy")
                    .tooltip("Copy the reply")
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.copy(copy.clone(), "the reply", cx)),
                    ),
            )
            .child(
                IconButton::new("save-xml", "FileDownloadOutlined")
                    .tooltip("Save as XML")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.save_file(save_name.clone(), save.clone(), cx)
                    })),
            );
        let mut column = div().flex_1().min_h_0().flex().flex_col().child(bar);
        if !errors.is_empty() {
            column = column.child(
                div()
                    .id("rpc-errors")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(8.))
                    .p(px(12.))
                    .pb(px(if has_data { 0. } else { 12. }))
                    .max_h(relative(0.45))
                    .overflow_y_scroll()
                    .children(
                        errors
                            .iter()
                            .map(|error| Self::render_rpc_error(error, &theme)),
                    ),
            );
        }
        if reply.is_none() {
            column = column.child(div().m(px(12.)).child(Alert::new(
                Status::Warning,
                "The reply is not a well-formed rpc-reply; it is shown as received.",
            )));
        }
        let body: AnyElement = if has_data && !raw {
            self.render_result_tree(
                run,
                &filter,
                if filter.is_empty() {
                    "The reply’s <data> is empty: nothing matched the filter."
                } else {
                    "Nothing matches the filter."
                },
                cx,
            )
        } else if !has_data && ok && errors.is_empty() && !raw {
            let clean = self.store.read(|state| {
                state
                    .candidate
                    .as_ref()
                    .is_some_and(|candidate| !candidate.checking && !candidate.dirty)
            });
            let (title, detail) = ok_view_message(&draft, clean);
            let mut ok_view = div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .px(px(24.))
                .text_center()
                .child(icon("CheckCircleRounded", 40., p.success.main))
                .child(
                    label(title)
                        .size(theme.typography.subtitle1.size)
                        .weight(theme.typography.subtitle1.weight)
                        .line_height(theme.typography.subtitle1.line_height)
                        .no_truncate(),
                );
            if let Some(detail) = detail {
                ok_view = ok_view.child(
                    div().max_w(px(460.)).child(
                        paragraph(detail)
                            .size(theme.typography.body2.size)
                            .line_height(theme.typography.body2.line_height)
                            .color(p.text.secondary)
                            .center(),
                    ),
                );
            }
            ok_view.into_any_element()
        } else if has_data || reply.is_none() || (errors.is_empty() && !ok) || raw {
            let shown = pretty_xml(xml);
            div()
                .flex_1()
                .min_w_0()
                .p(px(8.))
                .child(self.code_editor(
                    EditorKey::Result(run.id.clone()),
                    "html",
                    &shown,
                    true,
                    None,
                    true,
                    "",
                    window,
                    cx,
                ))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        column
            .child(div().flex_1().min_h_0().flex().child(body))
            .into_any_element()
    }

    /// The reply's `<data>` pretty-printed, for comparing two runs.
    pub(super) fn data_xml(source: &str) -> String {
        match parse_rpc_reply(source).and_then(|reply| reply.data) {
            Some(data) => pretty_xml(&serialize_element(&data)),
            None => pretty_xml(source),
        }
    }

    // Empty

    fn render_empty_result(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let connected = self.connected();
        let models: Vec<String> = self
            .controller
            .gnmi_info()
            .map(|info| info.models.iter().map(|model| model.name.clone()).collect())
            .unwrap_or_default();
        let starts = quick_starts(&models, self.protocol());
        let body2 = theme.typography.body2;
        // The shortcut sentence never breaks (`whiteSpace: nowrap`).
        let text = format!(
            "{}{}",
            empty_text(self.protocol()),
            format!("{HOTKEY_MOD_LABEL}Enter runs it.").replace(' ', "\u{a0}")
        );
        let mut column = div()
            .min_h(relative(1.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p(px(24.))
            .gap(px(20.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.))
                    .max_w(px(520.))
                    .child(
                        label("Nothing run yet")
                            .size(theme.typography.subtitle1.size)
                            .weight(theme.typography.subtitle1.weight)
                            .line_height(theme.typography.subtitle1.line_height)
                            .color(p.text.primary)
                            .no_truncate(),
                    )
                    .child(
                        paragraph(text)
                            .size(body2.size)
                            .line_height(body2.line_height)
                            .color(p.text.secondary)
                            .center(),
                    ),
            );
        if connected {
            let mut list = div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .w_full()
                .max_w(px(460.))
                .child(super::widgets::caps(
                    "Try",
                    theme.typography.caption.size,
                    600.,
                    p.text.secondary,
                ));
            for (index, start) in starts.into_iter().enumerate() {
                let hover_border = p.primary.main;
                let hover_bg = alpha(p.primary.main, 0.05);
                let draft = start.draft.clone();
                list = list.child(
                    div()
                        .id(("quick-start", index))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(theme.radius * 1.5))
                        .border_1()
                        .border_color(p.divider)
                        .bg(p.background_paper)
                        .cursor_pointer()
                        .hover(move |s| s.border_color(hover_border).bg(hover_bg))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.store.update(|state| state.draft = draft.clone());
                            this.controller.run(Some(draft.clone()), false);
                            cx.notify();
                        }))
                        .child(icon(start.icon, SMALL, p.primary.main))
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    label(start.label)
                                        .size(body2.size)
                                        .weight(600.)
                                        .line_height(body2.line_height)
                                        .color(p.text.primary)
                                        .no_truncate(),
                                )
                                .child(super::widgets::inline_span(
                                    start.detail,
                                    11.,
                                    MONO,
                                    p.text.secondary,
                                )),
                        ),
                );
            }
            column = column.child(list);
        }
        let _ = (
            ManagementProtocol::Gnmi,
            netconf_failed as fn(&RunRecord) -> bool,
            DataNode::is_leaf,
        );
        // `justifyContent: safe center`: centred while it fits, from the top once it scrolls.
        div().flex_1().min_h_0().flex().flex_col().child(
            div()
                .id("empty-result")
                .size_full()
                .overflow_y_scroll()
                .child(column),
        )
    }
}
