//! Accounting records as they happen (`tools/AcctzTool.tsx`).

use std::rc::Rc;

use muxus_core::management::{ManagementError, grpc_methods};
use muxus_mgmt::workbench::tool_format::{AccountingRecord, AuthzVerdict, accounting_record};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ListAlignment, ListSizingBehavior,
    ListState, ParentElement, SharedString, Styled, Window, div, list, px,
};
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::IconButton;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::fields::FieldKey;
use super::super::logic::format::{export_filename, iso_ms, time_of};
use super::super::navigator::ListenerIndex;
use super::super::view::ManagementView;
use super::super::widgets::MONO;
use super::super::workbench::ListenerForApp;
use super::kit::{body_text, error_alert};
use super::{contained, section, tool_header};
use muxus_ui::widgets::button::Button;
use muxus_ui::widgets::button::ButtonColor;
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::search_field::search_field;
use muxus_ui::widgets::toggle_button::ToggleButtonGroup;
use muxus_ui::widgets::toggle_button::ToggleOption;
use muxus_ui::widgets::tooltip_lines::TooltipExt;
use muxus_ui::widgets::typography::caps;

const MAX_RECORDS: usize = 5000;

/// The `since` choices: value, label and seconds of history.
pub const SINCE: &[(&str, &str, i64)] = &[
    ("live", "New only", 0),
    ("15m", "15 min", 900),
    ("1h", "1 h", 3600),
    ("24h", "24 h", 86_400),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AcctzState {
    pub since: String,
    pub running: bool,
    pub filter: String,
    pub error: Option<ManagementError>,
}

impl Default for AcctzState {
    fn default() -> Self {
        Self {
            since: "1h".into(),
            running: false,
            filter: String::new(),
            error: None,
        }
    }
}

/// The records (not JSON, so kept beside the store), and when the last batch came.
pub struct AcctzRecords {
    pub records: Vec<AccountingRecord>,
    pub now: u64,
    pub list: ListState,
    /// How many rows the list was last told about.
    pub shown: usize,
}

impl Default for AcctzRecords {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            now: 0,
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            shown: 0,
        }
    }
}

/// The records a filter keeps.
pub fn filter_records<'a>(
    records: &'a [AccountingRecord],
    filter: &str,
) -> Vec<&'a AccountingRecord> {
    let needle = muxus_core::js::js_trim(filter).to_lowercase();
    records
        .iter()
        .filter(|record| {
            needle.is_empty()
                || format!(
                    "{} {} {} {}",
                    record.user,
                    record.service,
                    record.action,
                    record.remote.clone().unwrap_or_default()
                )
                .to_lowercase()
                .contains(&needle)
        })
        .collect()
}

fn verdict_text(verdict: AuthzVerdict) -> &'static str {
    match verdict {
        AuthzVerdict::Permit => "permit",
        AuthzVerdict::Deny => "deny",
        AuthzVerdict::Error => "error",
    }
}

/// The records as CSV, quoted where needed.
pub fn records_csv(records: &[&AccountingRecord]) -> String {
    let mut lines = vec!["time,user,role,service,action,authorization,remote,session".to_owned()];
    for record in records {
        let cells = [
            iso_ms(record.at),
            record.user.clone(),
            record.role.clone().unwrap_or_default(),
            record.service.clone(),
            record.action.clone(),
            record
                .authz
                .map(verdict_text)
                .unwrap_or_default()
                .to_owned(),
            record.remote.clone().unwrap_or_default(),
            record.session.clone().unwrap_or_default(),
        ];
        lines.push(
            cells
                .iter()
                .map(|cell| {
                    if cell.contains(['"', ',', '\n']) {
                        format!("\"{}\"", cell.replace('"', "\"\""))
                    } else {
                        cell.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    lines.join("\n")
}

/// `toLocaleDateString(undefined, { month: 'short', day: 'numeric' })`.
fn short_date(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|at| at.format("%b %-d").to_string())
        .unwrap_or_default()
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

impl ManagementView {
    pub(super) fn render_acctz(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let can_run = self.grpc().is_some();
        let state: AcctzState = self.tool("acctz");
        let filter = self.bound_input(
            FieldKey::Tool("acctz.filter"),
            "Filter",
            &state.filter,
            window,
            cx,
        );
        let records: Vec<AccountingRecord> =
            filter_records(&self.ui.tools.acctz.records, &state.filter)
                .into_iter()
                .cloned()
                .collect();
        let mut column = div().child(tool_header(
            "ReceiptLongOutlined",
            "Accounting",
            "gnsi.acctz.v1",
            "Who did what on the device: every CLI command and gRPC call, with the user, where they came from and whether it was allowed, as it happens.",
            None,
            &theme,
        ));
        if let Some(error) = &state.error {
            column = column.child(error_alert(
                error,
                &theme,
                Some(Rc::new(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<AcctzState>("acctz", |state| state.error = None);
                        cx.notify();
                    },
                ))),
            ));
        }
        let since = SINCE.iter().position(|(value, _, _)| *value == state.since);
        let csv_rows = records.clone();
        let mut actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .w(px(160.))
                    .child(search_field(&filter, 28., None, cx)),
            )
            .child(
                ToggleButtonGroup::exclusive(
                    "acctz-since",
                    SINCE
                        .iter()
                        .map(|(_, label, _)| ToggleOption::new(*label))
                        .collect(),
                    since,
                    cx.listener_index(|this: &mut Self, index, cx: &mut Context<Self>| {
                        this.update_tool::<AcctzState>("acctz", |state| {
                            state.since = SINCE[index].0.into()
                        });
                        cx.notify();
                    }),
                )
                .inline(1.6, 8., 11.5)
                .disabled(state.running),
            )
            .child(
                IconButton::new("acctz-clear", "DeleteSweepOutlined")
                    .tooltip("Clear")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ui.tools.acctz.records.clear();
                        cx.notify();
                    })),
            )
            .child(
                IconButton::new("acctz-export", "FileDownloadOutlined")
                    .tooltip("Export as CSV")
                    .disabled(records.is_empty())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let rows: Vec<&AccountingRecord> = csv_rows.iter().collect();
                        let name = export_filename("accounting", "csv", chrono::Local::now());
                        this.save_file(name, records_csv(&rows), cx);
                    })),
            );
        actions = actions.child(if state.running {
            Button::new("acctz-stop", "Stop")
                .color(ButtonColor::Error)
                .outlined()
                .start_icon("StopRounded")
                .on_click(cx.listener_for_app(|this: &mut Self, _| this.cancel_stream("acctz")))
                .into_any_element()
        } else {
            contained("acctz-watch", "Watch", ButtonColor::Primary)
                .start_icon("PlayArrowRounded")
                .disabled(!can_run)
                .on_click(
                    cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                        this.start_acctz(cx)
                    }),
                )
                .into_any_element()
        });
        let body: AnyElement = if records.is_empty() {
            body_text(
                if state.running {
                    "Waiting for activity on the device \u{2026}"
                } else {
                    "Watch streams the accounting records: the history you choose, then everything that happens next."
                },
                p.text.secondary,
                &theme,
            )
            .into_any_element()
        } else {
            self.acctz_table(records, &theme).into_any_element()
        };
        column
            .child(section(
                if state.running { "Live" } else { "Records" },
                Some(actions.into_any_element()),
                body,
                &theme,
            ))
            .into_any_element()
    }

    fn acctz_table(
        &mut self,
        records: Vec<AccountingRecord>,
        theme: &Theme,
    ) -> impl IntoElement + use<> {
        let p = theme.palette;
        let count = records.len();
        if self.ui.tools.acctz.shown != count {
            self.ui.tools.acctz.list.reset(count);
            self.ui.tools.acctz.shown = count;
        }
        let now = self.ui.tools.acctz.now;
        let header = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(14.))
            .h(px(28.))
            .border_b_1()
            .border_color(p.divider)
            .bg(p.background_paper)
            .child(
                div()
                    .w(px(150.))
                    .child(caps("Time", 10.5, 650., p.text.secondary)),
            )
            .child(
                div()
                    .w(px(120.))
                    .child(caps("User", 10.5, 650., p.text.secondary)),
            )
            .child(
                div()
                    .w(px(120.))
                    .child(caps("Via", 10.5, 650., p.text.secondary)),
            )
            .child(
                div()
                    .flex_1()
                    .child(caps("What", 10.5, 650., p.text.secondary)),
            )
            .child(div().w(px(70.)));
        let records = Rc::new(records);
        let theme = *theme;
        let rows = list(self.ui.tools.acctz.list.clone(), move |index, _, _| {
            let Some(record) = records.get(index) else {
                return div().into_any_element();
            };
            record_row(record, now, &theme).into_any_element()
        })
        .with_sizing_behavior(ListSizingBehavior::Infer)
        .min_w(px(720.))
        .max_h(px(520. - 28.));
        div()
            .mx(px(-14.))
            .mb(px(-14.))
            .overflow_hidden()
            .child(div().min_w(px(720.)).child(header).child(rows))
    }

    fn start_acctz(&mut self, cx: &mut Context<Self>) {
        if self.grpc().is_none() {
            return;
        }
        let state: AcctzState = self.tool("acctz");
        let history = SINCE
            .iter()
            .find(|(value, _, _)| *value == state.since)
            .map_or(0, |(_, _, seconds)| *seconds);
        let since_seconds = chrono::Utc::now().timestamp() - history;
        let method = if self.controller.has_service("gnsi.acctz.v1.AcctzStream") {
            grpc_methods::ACCTZ_STREAM
        } else {
            grpc_methods::ACCTZ_BIDI
        };
        self.update_tool::<AcctzState>("acctz", |state| {
            state.running = true;
            state.error = None;
        });
        self.ui.tools.acctz.records.clear();
        self.start_stream(
            "acctz",
            method,
            Some(json!({ "timestamp": { "seconds": since_seconds } })),
            None,
            cx,
            |this, messages, _| {
                let acctz = &mut this.ui.tools.acctz;
                acctz.now = now_ms();
                let mut fresh: Vec<AccountingRecord> =
                    messages.iter().map(accounting_record).collect();
                fresh.reverse();
                fresh.append(&mut acctz.records);
                fresh.truncate(MAX_RECORDS);
                acctz.records = fresh;
            },
            |this, error, _| {
                this.update_tool::<AcctzState>("acctz", |state| {
                    state.running = false;
                    if let Some(error) = error.filter(|error| error.cancelled != Some(true)) {
                        state.error = Some(error);
                    }
                })
            },
        );
        cx.notify();
    }

    pub(super) fn acctz_field(&mut self, name: &str, value: String) {
        if name == "acctz.filter" {
            self.update_tool::<AcctzState>("acctz", |state| state.filter = value);
        }
    }
}

/// One record.
fn record_row(record: &AccountingRecord, now: u64, theme: &Theme) -> impl IntoElement + use<> {
    let p = &theme.palette;
    let fresh = now.saturating_sub(record.arrived) < 1500;
    let mut time = div()
        .w(px(150.))
        .flex_none()
        .flex()
        .font_family(MONO)
        .text_size(px(12.))
        .line_height(px(12. * 1.5))
        .text_color(p.text.secondary);
    if record.at != 0 {
        time = time.child(time_of(record.at.max(0) as u64)).child(
            div()
                .ml(px(6.))
                .text_color(p.text.disabled)
                .child(short_date(record.at)),
        );
    } else {
        time = time.child("\u{2014}");
    }
    let mut user = div().w(px(120.)).flex_none().min_w_0().child(
        muxus_ui::label::label(record.user.clone())
            .size(12.5)
            .weight(600.)
            .line_height(12.5 * 1.5)
            .color(p.text.primary),
    );
    if let Some(role) = record.role.as_ref().filter(|role| **role != record.user) {
        user = user.child(
            muxus_ui::label::label(role.clone())
                .size(10.5)
                .line_height(10.5 * 1.5)
                .color(p.text.secondary),
        );
    }
    let mut via = div().w(px(120.)).flex_none().min_w_0().child(
        div().flex().child(
            Chip::new(record.service.clone())
                .height(18.)
                .font_size(10.5),
        ),
    );
    if let Some(remote) = &record.remote {
        via = via.child(
            div()
                .id(SharedString::from(format!("remote-{}", record.key)))
                .child(
                    muxus_ui::label::label(remote.clone())
                        .family(MONO)
                        .size(10.5)
                        .line_height(10.5 * 1.5)
                        .color(p.text.secondary),
                )
                .mui_tooltip(remote.clone()),
        );
    }
    let what = div()
        .id(SharedString::from(format!("action-{}", record.key)))
        .flex_1()
        .min_w_0()
        .child(
            muxus_ui::label::label(record.action.clone())
                .family(MONO)
                .size(12.)
                .line_height(12. * 1.5)
                .color(p.text.primary),
        )
        .mui_tooltip(record.action.clone());
    let mut verdict = div().w(px(70.)).flex_none().flex().justify_end();
    if let Some(authz) = record.authz {
        let color = if authz == AuthzVerdict::Permit {
            p.success.main
        } else {
            p.error.main
        };
        verdict = verdict.child(
            Chip::new(match authz {
                AuthzVerdict::Permit => "allowed",
                AuthzVerdict::Deny => "denied",
                AuthzVerdict::Error => "error",
            })
            .outlined()
            .color(color)
            .height(18.)
            .font_size(10.5)
            .tooltip(
                ("acctz-chip-tip", 2usize),
                record.authz_detail.clone().unwrap_or_default(),
            ),
        );
    }
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(14.))
        .min_h(px(30.))
        .border_b_1()
        .border_color(p.divider)
        .child(time)
        .child(user)
        .child(via)
        .child(what)
        .child(verdict);
    if fresh {
        row = row.bg(alpha(p.info.main, 0.08));
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(user: &str, action: &str) -> AccountingRecord {
        accounting_record(
            json!({
                "session_info": {"user": {"identity": user}, "remote_address": "10.0.0.9"},
                "timestamp": {"seconds": 1_700_000_000, "nanos": 0},
                "cmd_service": {"service_type": "CMD_SERVICE_TYPE_CLI", "cmd": action, "authz": {"status": "AUTHZ_STATUS_PERMIT"}}
            })
            .as_object()
            .unwrap(),
        )
    }

    #[test]
    fn filters_and_exports_records() {
        let records = vec![
            record("alice", "show version"),
            record("bob", "info, \"quoted\""),
        ];
        assert_eq!(filter_records(&records, "ALICE").len(), 1);
        assert_eq!(filter_records(&records, "10.0.0.9").len(), 2);
        let csv = records_csv(&records.iter().collect::<Vec<_>>());
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "time,user,role,service,action,authorization,remote,session"
        );
        assert_eq!(
            lines[1],
            "2023-11-14T22:13:20.000Z,alice,,CLI,show version,permit,10.0.0.9,"
        );
        assert!(lines[2].contains("\"info, \"\"quoted\"\"\""));
    }
}
