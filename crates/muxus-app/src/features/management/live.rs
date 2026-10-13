//! Live telemetry (`LiveView.tsx`): a subscription's values as a sortable
//! table with rates, sparklines and ages, frozen on demand while updates
//! keep arriving.

use super::workbench::ListenerForApp;
use std::rc::Rc;
use std::time::{Duration, Instant};

use muxus_core::gnmi_path::format_gnmi_path;
use muxus_core::management::{GnmiSubscriptionListMode, GnmiSubscriptionMode};
use muxus_mgmt::workbench::live_table::{LiveRow, format_rate, format_si};
use muxus_mgmt::workbench::requests::{ManagementDraft, non_empty_paths};
use muxus_mgmt::workbench::store::{RunRecord, RunStatus};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, BoxShadow, Context, Div, Hsla, InteractiveElement, IntoElement, ParentElement,
    PathBuilder, SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, point, px,
    relative, uniform_list,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::IconButton;

use super::fields::FieldKey;
use super::logic::format::export_filename;
use super::logic::live::{
    FLASH_MS, ROW_HEIGHT, SPARK_HEIGHT, SPARK_WIDTH, Sort, SortKey, ago, common_prefix,
    empty_message, live_csv, sorted_rows, sparkline, status_label,
};
use super::logic::values::{value_text, value_tone};
use super::path_text::path_text;
use super::tree_view::tone_color;
use super::view::ManagementView;
use super::widgets::MONO;
use muxus_ui::widgets::button::Button;
use muxus_ui::widgets::button::ButtonColor;
use muxus_ui::widgets::search_field::input_base;

fn sparkline_element(points: &[f64], color: Hsla) -> AnyElement {
    let coordinates = sparkline(points);
    if coordinates.is_empty() {
        return div()
            .w(px(SPARK_WIDTH))
            .h(px(SPARK_HEIGHT))
            .into_any_element();
    }
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let origin = bounds.origin;
            let at = |(x, y): (f32, f32)| point(origin.x + px(x), origin.y + px(y));
            let mut fill = PathBuilder::fill();
            fill.move_to(at((0., SPARK_HEIGHT)));
            for coordinate in &coordinates {
                fill.line_to(at(*coordinate));
            }
            fill.line_to(at((SPARK_WIDTH, SPARK_HEIGHT)));
            fill.close();
            if let Ok(path) = fill.build() {
                window.paint_path(path, alpha(color, 0.12));
            }
            let mut stroke = PathBuilder::stroke(px(1.4));
            stroke.move_to(at(coordinates[0]));
            for coordinate in &coordinates[1..] {
                stroke.line_to(at(*coordinate));
            }
            if let Ok(path) = stroke.build() {
                window.paint_path(path, color);
            }
        },
    )
    .w(px(SPARK_WIDTH))
    .h(px(SPARK_HEIGHT))
    .into_any_element()
}

impl ManagementView {
    pub(super) fn render_live_view(
        &mut self,
        run: &RunRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let Some(table) = run.live.clone() else {
            return div();
        };
        let ManagementDraft::Gnmi(draft) = &run.draft else {
            return div();
        };
        let streaming = run.status == RunStatus::Streaming;
        let paused = self.ui.live.paused && streaming;
        let filter_value = self.ui.live_filter.clone();
        let sort = self.ui.live.sort;
        let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
        // A paused display keeps the rows it showed when it froze.
        let (rows, leaf_updates, bytes, started_at, synced): (Vec<LiveRow>, u64, u64, u64, bool) =
            if paused && let Some(frozen) = &self.ui.live_frozen {
                frozen.clone()
            } else {
                let table = table.lock().expect("live table");
                let rows: Vec<LiveRow> = sorted_rows(table.rows.values(), &filter_value, sort)
                    .into_iter()
                    .cloned()
                    .collect();
                let snapshot = (
                    rows,
                    table.leaf_updates,
                    table.bytes,
                    table.started_at,
                    table.synced,
                );
                if paused {
                    self.ui.live_frozen = Some(snapshot.clone());
                }
                snapshot
            };
        if !paused {
            self.ui.live_frozen = None;
        }
        let elapsed = ((now_ms as f64 - started_at as f64) / 1000.).max(1.);
        let prefix = common_prefix(&non_empty_paths(&draft.paths));
        let skip = prefix.as_ref().map_or(0, |prefix| prefix.elems.len());
        let caption = theme.typography.caption;
        let body2 = theme.typography.body2;
        let dot = if streaming {
            if paused {
                p.warning.main
            } else {
                p.success.main
            }
        } else {
            p.text.disabled
        };
        let mut status_dot = div().size(px(8.)).flex_none().rounded_full().bg(dot);
        if streaming && !paused {
            status_dot = status_dot.shadow(vec![BoxShadow {
                color: alpha(p.success.main, 0.2),
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(3.),
                inset: false,
            }]);
        }
        let filter = self.bound_input(FieldKey::LiveFilter, "Filter", &filter_value, window, cx);
        let mut header = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(4.))
            .min_h(px(40.))
            .border_b_1()
            .border_color(p.divider)
            .child(status_dot)
            .child(
                label(status_label(streaming, paused, synced))
                    .size(body2.size)
                    .weight(600.)
                    .line_height(body2.line_height)
                    .no_truncate(),
            )
            .child(
                label(format!(
                    "{} leaves · {} updates · {}B/s",
                    rows.len(),
                    format_si(leaf_updates as f64, 3),
                    format_si(bytes as f64 / elapsed, 3)
                ))
                .size(caption.size)
                .line_height(caption.line_height)
                .color(p.text.secondary)
                .no_truncate(),
            );
        if let Some(prefix) = prefix.as_ref().filter(|prefix| !prefix.elems.is_empty()) {
            header = header.child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(caption.size))
                    .line_height(px(caption.line_height))
                    .text_color(p.text.secondary)
                    .child("under")
                    .child(path_text(
                        &format_gnmi_path(prefix),
                        0,
                        false,
                        false,
                        &theme,
                    )),
            );
        }
        header = header.child(div().flex_1()).child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .px(px(8.))
                .h(px(28.))
                .w(px(200.))
                .border_1()
                .border_color(p.divider)
                .rounded(px(theme.radius))
                .child(icon("Search", 16., p.text.secondary))
                .child(input_base(&filter, 12.5, false, cx)),
        );
        let id = run.id.clone();
        if draft.subscribe.list_mode == GnmiSubscriptionListMode::Poll && streaming {
            let poll = id.clone();
            header = header.child(Button::new("poll", "Poll").start_icon("Sync").on_click(
                cx.listener_for_app(move |this: &mut Self, _cx: &mut Context<Self>| {
                    this.controller.poll(&poll);
                }),
            ));
        }
        if streaming {
            header = header.child(
                IconButton::new(
                    "pause",
                    if paused {
                        "PlayArrowRounded"
                    } else {
                        "PauseRounded"
                    },
                )
                .tooltip(if paused {
                    "Resume the display"
                } else {
                    "Freeze the display (updates keep arriving)"
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.ui.live.paused = !paused;
                    cx.notify();
                })),
            );
        }
        let clear = table.clone();
        let csv_rows = rows.clone();
        let file = export_filename(
            &format!("{} telemetry", self.title),
            "csv",
            chrono::Local::now(),
        );
        header = header
            .child(
                IconButton::new("clear-values", "DeleteSweepOutlined")
                    .tooltip("Clear values")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        clear.lock().expect("live table").clear();
                        this.ui.live_frozen = None;
                        cx.notify();
                    })),
            )
            .child(
                IconButton::new("export-csv", "FileDownloadOutlined")
                    .tooltip("Export as CSV")
                    .disabled(rows.is_empty())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let refs: Vec<&LiveRow> = csv_rows.iter().collect();
                        this.save_file(file.clone(), live_csv(&refs), cx);
                    })),
            );
        if streaming {
            let stop = id.clone();
            header = header.child(
                Button::new("stop-live", "Stop")
                    .color(ButtonColor::Error)
                    .outlined()
                    .start_icon("StopRounded")
                    .on_click(cx.listener_for_app(
                        move |this: &mut Self, _cx: &mut Context<Self>| {
                            this.controller.cancel(&stop);
                        },
                    )),
            );
        }
        let columns = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .px(px(12.))
            .h(px(26.))
            .border_b_1()
            .border_color(p.divider)
            .bg(p.background_paper)
            .child(
                self.sort_header(SortKey::Path, "Path", None, false, sort, &theme, cx)
                    .flex_1(),
            )
            .child(
                div()
                    .w(relative(0.22))
                    .min_w(px(90.))
                    .child(self.sort_header(SortKey::Path, "Value", None, false, sort, &theme, cx)),
            )
            .child(self.sort_header(SortKey::Rate, "Rate", Some(96.), true, sort, &theme, cx))
            .child(div().w(px(84.)).child(super::widgets::caps(
                "Trend",
                10.5,
                650.,
                p.text.secondary,
            )))
            .child(self.sort_header(SortKey::Updated, "Age", Some(44.), true, sort, &theme, cx))
            .child(self.sort_header(SortKey::Updates, "#", Some(48.), true, sort, &theme, cx));
        let body: AnyElement = if rows.is_empty() {
            let mut empty = div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .p(px(24.))
                .text_color(p.text.secondary)
                .child(
                    label(empty_message(streaming, &filter_value))
                        .size(body2.size)
                        .line_height(body2.line_height)
                        .color(p.text.secondary)
                        .no_truncate(),
                );
            if streaming && draft.subscribe.mode == GnmiSubscriptionMode::OnChange {
                empty = empty.child(
                    label("On-change subscriptions only report when a value changes.")
                        .size(caption.size)
                        .line_height(caption.line_height)
                        .color(p.text.secondary)
                        .no_truncate(),
                );
            }
            empty.into_any_element()
        } else {
            let rows = Rc::new(rows);
            let count = rows.len();
            let now = Instant::now();
            uniform_list(
                "live-rows",
                count,
                cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                    let theme = *cx.theme();
                    range
                        .filter_map(|index| rows.get(index))
                        .map(|row| live_row(row, skip, now, now_ms, &theme))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.ui.live_scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(header)
            .child(columns)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
    }

    #[allow(clippy::too_many_arguments)]
    fn sort_header(
        &mut self,
        key: SortKey,
        text: &str,
        width: Option<f32>,
        right: bool,
        sort: Sort,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = &theme.palette;
        let active = sort.key == key;
        let ink = if active {
            p.text.primary
        } else {
            p.text.secondary
        };
        let mut header = div()
            .id(SharedString::from(format!("sort-{text}")))
            .flex()
            .items_center()
            .gap(px(2.))
            .min_w_0()
            .cursor_pointer()
            .child(super::widgets::caps(text.to_owned(), 10.5, 650., ink))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.ui.live.sort = this.ui.live.sort.clicked(key);
                cx.notify();
            }));
        if active {
            let mut arrow = icon("ArrowDownward", 12., ink);
            if !sort.descending {
                arrow = arrow.with_transformation(muxus_ui::gpui_kit::Transformation::rotate(
                    muxus_ui::gpui_kit::radians(std::f32::consts::PI),
                ));
            }
            header = header.child(arrow);
        }
        if right {
            header = header.justify_end();
        }
        let mut cell = div().flex().min_w_0().child(header);
        cell = match width {
            Some(width) => cell.w(px(width)).flex_none(),
            None => cell.w_full(),
        };
        if right {
            cell = cell.justify_end();
        }
        cell
    }
}

fn live_row(row: &LiveRow, skip: usize, now: Instant, now_ms: u64, theme: &Theme) -> AnyElement {
    let p = &theme.palette;
    // Counters change with every sample; only state changes deserve attention.
    let flashing = !row.counter
        && row
            .changed_at
            .is_some_and(|changed| now.duration_since(changed) < Duration::from_millis(FLASH_MS));
    let trend = if row.counter {
        p.primary.main
    } else {
        p.secondary.main
    };
    let value = value_text(Some(&row.value));
    let hover = p.action.hover;
    let mut element = div()
        .id(SharedString::from(row.path.clone()))
        .w_full()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(12.))
        .h(px(ROW_HEIGHT))
        .text_size(px(12.5))
        .border_b_1()
        .border_color(p.divider)
        .hover(move |s| s.bg(hover))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .child(path_text(&row.path, skip, false, false, theme)),
        )
        .child({
            let mut cell = div()
                .w(relative(0.22))
                .min_w(px(90.))
                .font_family(MONO)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .text_color(tone_color(value_tone(Some(&row.value)), theme))
                .rounded(px(6.))
                .px(px(4.))
                .mx(px(-4.))
                .child(value);
            if flashing {
                cell = cell.bg(alpha(p.warning.main, 0.28));
            }
            cell
        })
        .child(
            div()
                .w(px(96.))
                .flex_none()
                .flex()
                .justify_end()
                .font_family(MONO)
                .text_size(px(12.))
                .whitespace_nowrap()
                .text_color(if row.rate.is_some_and(|rate| rate != 0.) {
                    p.text.primary
                } else {
                    p.text.disabled
                })
                .child(match row.rate {
                    Some(rate) => format_rate(&row.path, rate),
                    None => "—".into(),
                }),
        )
        .child(
            div()
                .w(px(84.))
                .flex_none()
                .child(sparkline_element(&row.history, trend)),
        )
        .child(
            div()
                .w(px(44.))
                .flex_none()
                .flex()
                .justify_end()
                .text_size(px(11.5))
                .text_color(p.text.secondary)
                .child(ago(row.received_at, now_ms)),
        )
        .child(
            div()
                .w(px(48.))
                .flex_none()
                .flex()
                .justify_end()
                .font_family(MONO)
                .text_size(px(11.5))
                .text_color(p.text.secondary)
                .child(row.updates.to_string()),
        );
    if row.deleted {
        element = element.opacity(0.45).line_through();
    }
    element.into_any_element()
}
