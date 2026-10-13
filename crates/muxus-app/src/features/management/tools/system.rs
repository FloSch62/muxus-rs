//! Health, reboot, processes, BGP, and the clock and OS version
//! (`tools/SystemTools.tsx`).

use std::time::Instant;

use muxus_core::gnmi_path::try_parse_gnmi_path;
use muxus_core::management::{GnmiDataType, ManagementError, ManagementResult, grpc_methods};
use muxus_ui::gpui_kit::{
    AnyElement, Context, Div, IntoElement, ParentElement, Styled, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::display::Alert;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::super::logic::editor::Confirmation;
use super::super::logic::format::{js_number_string, locale_date_time};
use super::super::logic::tools::{device_time, gnoi_path, number_of};
use super::super::path_text::path_text;
use super::super::ui::{ConfirmAction, Dialog, SelectField};
use super::super::view::ManagementView;
use super::super::widgets::switch_row;
use super::super::workbench::ListenerForApp;
use super::kit::{ToolField, body_text, caption_text, digits_number, error_alert};
use super::{ToolAction, contained, field_row, section, spacer, stat, tool_header};
use muxus_ui::widgets::button::Button;
use muxus_ui::widgets::button::ButtonColor;
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::progress::CircularProgress;

// Health

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HealthState {
    pub path: String,
    pub include_acknowledged: bool,
    pub loading: bool,
    pub components: Option<Vec<Value>>,
    pub error: Option<ManagementError>,
}

/// Component paths worth offering for this device.
pub fn health_suggestions(models: &[String]) -> Vec<String> {
    let list: &[&str] = if models.iter().any(|model| model.contains("srl_nokia")) {
        &[
            "/platform/control[slot=A]",
            "/platform/chassis",
            "/platform/linecard[slot=1]",
            "/platform/fan-tray[id=1]",
            "/platform/power-supply[id=1]",
        ]
    } else {
        &[
            "/components/component[name=chassis]",
            "/components/component[name=Supervisor1]",
        ]
    };
    list.iter().map(|path| (*path).to_owned()).collect()
}

/// A protobuf Timestamp `{seconds, nanos}` as a local date and time.
fn timestamp(value: Option<&Value>) -> String {
    let seconds = value.and_then(|value| value.get("seconds"));
    let zero = match seconds {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.is_empty() || text == "0",
        Some(Value::Number(number)) => number.as_f64() == Some(0.),
        _ => false,
    };
    if zero {
        return "\u{2014}".into();
    }
    locale_date_time((number_of(seconds) * 1000.) as i64)
}

fn has_seconds(value: Option<&Value>) -> bool {
    match value.and_then(|value| value.get("seconds")) {
        None | Some(Value::Null) => false,
        Some(Value::String(text)) => !text.is_empty() && text != "0",
        Some(Value::Number(number)) => number.as_f64() != Some(0.),
        Some(_) => true,
    }
}

// Reboot

/// Reboot methods: value, label and what it does.
pub const REBOOT_METHODS: &[(&str, &str, &str)] = &[
    ("COLD", "Cold", "Restart the whole system"),
    (
        "WARM",
        "Warm",
        "Restart the software, keep forwarding where supported",
    ),
    (
        "NSF",
        "Non-stop forwarding",
        "Restart without interrupting traffic",
    ),
    ("POWERDOWN", "Power down", "Turn the system off"),
    ("HALT", "Halt", "Stop the system without powering off"),
    ("POWERUP", "Power up", "Power up a component"),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RebootState {
    pub method: String,
    pub delay_seconds: u64,
    pub message: String,
    pub force: bool,
    pub status: Option<Map<String, Value>>,
    pub error: Option<ManagementError>,
    pub checked_at: Option<i64>,
}

impl Default for RebootState {
    fn default() -> Self {
        Self {
            method: "COLD".into(),
            delay_seconds: 0,
            message: "Requested from Muxus".into(),
            force: false,
            status: None,
            error: None,
            checked_at: None,
        }
    }
}

impl RebootState {
    pub fn active(&self) -> bool {
        self.status
            .as_ref()
            .and_then(|status| status.get("active"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// The Reboot request for this state.
pub fn reboot_request(state: &RebootState) -> Value {
    let mut request = Map::new();
    request.insert("method".into(), json!(state.method));
    if state.delay_seconds != 0 {
        request.insert(
            "delay".into(),
            json!((u128::from(state.delay_seconds) * 1_000_000_000).to_string()),
        );
    }
    let message = muxus_core::js::js_trim(&state.message);
    if !message.is_empty() {
        request.insert("message".into(), json!(message));
    }
    if state.force {
        request.insert("force".into(), json!(true));
    }
    Value::Object(request)
}

/// What the pending reboot alert says.
pub fn pending_text(status: &Map<String, Value>) -> String {
    let method = status
        .get("method")
        .map(|method| match method {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        })
        .unwrap_or_default()
        .to_lowercase();
    let wait = match status.get("wait") {
        None | Some(Value::Null) => 0.,
        Some(value) => (number_of(Some(value)) / 1e9).round(),
    };
    let wait = if wait != 0. && !wait.is_nan() {
        format!(", in {} s", js_number_string(wait))
    } else {
        String::new()
    };
    let reason = match status.get("reason").and_then(Value::as_str) {
        Some(reason) if !reason.is_empty() => format!(": {reason}"),
        _ => ".".into(),
    };
    format!("A {method} reboot is pending{wait}{reason}")
}

// Processes

pub const SIGNALS: &[&str] = &["SIGNAL_TERM", "SIGNAL_KILL", "SIGNAL_HUP", "SIGNAL_ABRT"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProcessState {
    pub name: String,
    pub pid: String,
    pub signal: String,
    pub restart: bool,
    pub error: Option<ManagementError>,
    pub applications: Option<Vec<String>>,
}

impl Default for ProcessState {
    fn default() -> Self {
        Self {
            name: String::new(),
            pid: String::new(),
            signal: "SIGNAL_TERM".into(),
            restart: true,
            error: None,
            applications: None,
        }
    }
}

// BGP

pub const BGP_MODES: &[(&str, &str, &str)] = &[
    ("SOFT", "Soft", "Re-send routes to the neighbor"),
    (
        "SOFTIN",
        "Soft in",
        "Ask the neighbor to re-send its routes",
    ),
    (
        "GRACEFUL_RESET",
        "Graceful reset",
        "Reset with graceful restart",
    ),
    ("HARD_RESET", "Hard reset", "Tear the session down"),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BgpState {
    pub address: String,
    pub instance: String,
    pub mode: String,
    pub error: Option<ManagementError>,
}

impl Default for BgpState {
    fn default() -> Self {
        Self {
            address: String::new(),
            instance: String::new(),
            mode: "SOFT".into(),
            error: None,
        }
    }
}

// System

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SystemState {
    pub device_time: Option<String>,
    pub local_at: Option<i64>,
    pub round_trip_ms: Option<i64>,
    pub version: Option<String>,
    pub os_error: Option<ManagementError>,
    pub error: Option<ManagementError>,
}

/// Device time minus local time, corrected for half the round trip.
pub fn skew_ms(state: &SystemState) -> Option<f64> {
    let device = state.device_time.as_ref()?;
    let local = state.local_at.filter(|local| *local != 0)?;
    let round_trip = state.round_trip_ms?;
    let device_ms = device.parse::<i128>().ok()? / 1_000_000;
    Some(device_ms as f64 - (local as f64 - round_trip as f64 / 2.))
}

/// The offset as the clock section shows it.
pub fn skew_text(skew: f64) -> String {
    let digits = if skew.abs() < 10_000. { 2 } else { 0 };
    format!(
        "{}{:.*} s",
        if skew > 0. { "+" } else { "" },
        digits,
        skew / 1000.
    )
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

impl ManagementView {
    // Health

    pub(super) fn render_health(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let can_run = self.grpc().is_some();
        let state: HealthState = self.tool("health");
        let suggestions = health_suggestions(&self.model_names());
        let row =
            field_row(8.)
                .child(
                    self.tool_field(
                        ToolField::new("health.path", "Component path", state.path.clone())
                            .placeholder(suggestions[0].clone())
                            .basis(320.)
                            .mono()
                            .options(suggestions),
                        window,
                        cx,
                    ),
                )
                .child(switch_row(
                    "health-acknowledged",
                    state.include_acknowledged,
                    "Include acknowledged",
                    &theme,
                    cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<HealthState>("health", |state| {
                            state.include_acknowledged = !state.include_acknowledged
                        });
                        cx.notify();
                    }),
                ))
                .child(
                    Button::new("health-events", "Events")
                        .outlined()
                        .disabled(!can_run || state.loading)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| this.run_health(false, cx),
                        )),
                )
                .child(
                    contained("health-get", "Get health", ButtonColor::Primary)
                        .disabled(!can_run || state.loading)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| this.run_health(true, cx),
                        )),
                );
        let mut column = div()
            .child(tool_header(
                "HealthAndSafetyOutlined",
                "Health",
                "gnoi.healthz.Healthz",
                "The health the device reports for a component: its current status, past unhealthy events with their debug artifacts, and acknowledging them.",
                None,
                &theme,
            ))
            .child(section("Component", None, row, &theme));
        if let Some(error) = &state.error {
            column = column.child(error_alert(
                error,
                &theme,
                Some(std::rc::Rc::new(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<HealthState>("health", |state| state.error = None);
                        cx.notify();
                    },
                ))),
            ));
        }
        if state.loading {
            column = column.child(CircularProgress::new(
                ("system-progress", 1usize),
                22.,
                p.primary.main,
            ));
        }
        if let Some(components) = state.components.as_ref().filter(|_| !state.loading) {
            let title = if components.len() == 1 {
                "Status".to_owned()
            } else {
                format!("{} events", components.len())
            };
            let body = if components.is_empty() {
                body_text(
                    "No health events for this component.",
                    p.text.secondary,
                    &theme,
                )
            } else {
                let mut list = div();
                for (index, component) in components.iter().enumerate() {
                    list = list.child(self.health_card(component, &index.to_string(), &theme, cx));
                }
                list
            };
            column = column.child(section(&title, None, body, &theme));
        }
        column.into_any_element()
    }

    fn health_card(
        &mut self,
        component: &Value,
        id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = &theme.palette;
        let status = component
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let healthy = status == "STATUS_HEALTHY";
        let unhealthy = status == "STATUS_UNHEALTHY";
        let acknowledged = component
            .get("acknowledged")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let event_id = component
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let status_icon = if healthy {
            icon("CheckCircleRounded", 20., p.success.main)
        } else if unhealthy {
            icon("ErrorRounded", 20., p.error.main)
        } else {
            icon("HealthAndSafetyOutlined", 20., p.text.disabled)
        };
        let mut chip = Chip::new(if healthy {
            "Healthy"
        } else if unhealthy {
            "Unhealthy"
        } else {
            "Unknown"
        })
        .height(22.);
        if healthy {
            chip = chip.palette(p.success);
        } else if unhealthy {
            chip = chip.palette(p.error);
        }
        let mut head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(status_icon)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.))
                    .line_height(px(13. * 1.5))
                    .child(path_text(
                        &super::kit::json_path_text(component.get("path")),
                        0,
                        false,
                        true,
                        theme,
                    )),
            )
            .child(chip);
        if acknowledged {
            head = head.child(Chip::new("Acknowledged").outlined().height(22.));
        }
        if unhealthy && !acknowledged && event_id.is_some() {
            let target = component.clone();
            head = head.child(
                Button::new(
                    muxus_ui::gpui_kit::SharedString::from(format!("ack-{id}")),
                    "Acknowledge",
                )
                .on_click(cx.listener_for_app(
                    move |this: &mut Self, cx: &mut Context<Self>| {
                        this.acknowledge_health(&target, cx)
                    },
                )),
            );
        }
        let mut facts = div()
            .flex()
            .flex_wrap()
            .gap(px(24.))
            .mt(px(8.))
            .pl(px(28.))
            .child(caption_text(
                format!("Since {}", timestamp(component.get("created"))),
                p.text.secondary,
                theme,
            ));
        if has_seconds(component.get("expires")) {
            facts = facts.child(caption_text(
                format!("Expires {}", timestamp(component.get("expires"))),
                p.text.secondary,
                theme,
            ));
        }
        if let Some(event) = &event_id {
            facts = facts.child(
                caption_text(format!("event {event}"), p.text.secondary, theme)
                    .font_family(super::super::widgets::MONO),
            );
        }
        let mut card = div()
            .border_1()
            .border_color(if unhealthy { p.error.main } else { p.divider })
            .rounded(px(theme.radius * 1.5))
            .p(px(12.))
            .mb(px(8.))
            .child(head)
            .child(facts);
        if let Some(artifacts) = component
            .get("artifacts")
            .and_then(Value::as_array)
            .filter(|list| !list.is_empty())
        {
            let mut list = div().flex().flex_col().gap(px(2.)).mt(px(8.)).pl(px(28.));
            for artifact in artifacts {
                let file = artifact.get("file");
                let name = file
                    .and_then(|file| file.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| text_of(artifact.get("id")));
                let size = file
                    .map(|file| number_of(file.get("size")))
                    .filter(|size| *size != 0. && !size.is_nan());
                let text = match size {
                    Some(size) => {
                        format!("artifact {name} \u{b7} {} bytes", js_number_string(size))
                    }
                    None => format!("artifact {name}"),
                };
                list = list.child(
                    caption_text(text, p.text.primary, theme)
                        .font_family(super::super::widgets::MONO),
                );
            }
            card = card.child(list);
        }
        if let Some(children) = component
            .get("subcomponents")
            .and_then(Value::as_array)
            .filter(|list| !list.is_empty())
        {
            let mut nested = div().mt(px(8.)).pl(px(28.));
            for (index, child) in children.iter().enumerate() {
                nested = nested.child(self.health_card(child, &format!("{id}-{index}"), theme, cx));
            }
            card = card.child(nested);
        }
        card
    }

    fn model_names(&self) -> Vec<String> {
        self.controller
            .gnmi_info()
            .map(|info| info.models.iter().map(|model| model.name.clone()).collect())
            .unwrap_or_default()
    }

    pub(super) fn run_health(&mut self, get: bool, cx: &mut Context<Self>) {
        if self.grpc().is_none() {
            return;
        }
        let state: HealthState = self.tool("health");
        let suggestions = health_suggestions(&self.model_names());
        let text = muxus_core::js::js_trim(&state.path);
        let text = if text.is_empty() {
            suggestions[0].as_str()
        } else {
            text
        };
        let Some(parsed) = try_parse_gnmi_path(text) else {
            self.update_tool::<HealthState>("health", |state| {
                state.error = Some(super::kit::message_error(
                    "The component path does not parse.",
                ))
            });
            cx.notify();
            return;
        };
        self.update_tool::<HealthState>("health", |state| {
            state.loading = true;
            state.error = None;
        });
        let path = gnoi_path(&parsed.elems, parsed.origin.as_deref());
        if get {
            self.grpc_call(
                grpc_methods::HEALTHZ_GET,
                json!({ "path": path }),
                cx,
                |this, result, _| {
                    this.update_tool::<HealthState>("health", |state| {
                        state.loading = false;
                        match result {
                            Ok(response) => {
                                state.components =
                                    Some(response.get("component").cloned().into_iter().collect());
                            }
                            Err(error) => state.error = Some(error),
                        }
                    })
                },
            );
        } else {
            let request =
                json!({ "path": path, "include_acknowledged": state.include_acknowledged });
            self.grpc_call(
                grpc_methods::HEALTHZ_LIST,
                request,
                cx,
                |this, result, _| {
                    this.update_tool::<HealthState>("health", |state| {
                        state.loading = false;
                        match result {
                            Ok(response) => {
                                state.components = Some(
                                    response
                                        .get("statuses")
                                        .and_then(Value::as_array)
                                        .cloned()
                                        .unwrap_or_default(),
                                );
                            }
                            Err(error) => state.error = Some(error),
                        }
                    })
                },
            );
        }
        cx.notify();
    }

    fn acknowledge_health(&mut self, component: &Value, cx: &mut Context<Self>) {
        let (Some(path), Some(id)) = (component.get("path"), component.get("id")) else {
            return;
        };
        let request = json!({ "path": path, "id": id });
        self.grpc_call(
            grpc_methods::HEALTHZ_ACKNOWLEDGE,
            request,
            cx,
            |this, result, cx| match result {
                Ok(_) => {
                    this.toast(Status::Success, "Acknowledged.", cx);
                    this.run_health(false, cx);
                }
                Err(error) => {
                    this.update_tool::<HealthState>("health", |state| state.error = Some(error))
                }
            },
        );
    }

    // Reboot

    pub(super) fn render_reboot(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let can_run = self.grpc().is_some();
        let state: RebootState = self.tool("reboot");
        let active = state.active();
        let refresh = Button::new("reboot-refresh", "Refresh")
            .disabled(!can_run)
            .on_click(
                cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                    this.refresh_reboot(cx)
                }),
            )
            .into_any_element();
        let mut column = div().child(tool_header(
            "RestartAlt",
            "Reboot",
            "gnoi.system.System",
            "See whether a reboot is pending, schedule one with a delay, or cancel it before it happens.",
            Some(refresh),
            &theme,
        ));
        if let Some(error) = &state.error {
            column = column.child(error_alert(
                error,
                &theme,
                Some(std::rc::Rc::new(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<RebootState>("reboot", |state| state.error = None);
                        cx.notify();
                    },
                ))),
            ));
        }
        let status_body = match state.status.as_ref().filter(|_| active) {
            Some(status) => div().child(
                Alert::new(Status::Warning, pending_text(status)).action(
                    Button::new("cancel-reboot", "Cancel reboot")
                        .color(ButtonColor::Inherit)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| this.cancel_reboot(cx),
                        )),
                ),
            ),
            None => {
                let status = state.status.clone().unwrap_or_default();
                let mut stats = div()
                    .flex()
                    .flex_wrap()
                    .gap(px(24.))
                    .child(stat("Pending", "No", Some(p.success.main), &theme))
                    .child(stat(
                        "Reboots",
                        match status.get("count") {
                            Some(Value::Number(count)) => count.to_string(),
                            Some(Value::String(count)) => count.clone(),
                            _ => "0".into(),
                        },
                        None,
                        &theme,
                    ));
                let last = status
                    .get("status")
                    .and_then(|status| status.get("status"))
                    .and_then(Value::as_str)
                    .filter(|status| !status.is_empty() && *status != "STATUS_UNKNOWN");
                if let Some(last) = last {
                    stats = stats.child(stat(
                        "Last result",
                        last.replacen("STATUS_", "", 1).to_lowercase(),
                        None,
                        &theme,
                    ));
                }
                let when = status.get("when").filter(|when| match when {
                    Value::String(text) => !text.is_empty() && text != "0",
                    Value::Number(number) => number.as_f64() != Some(0.),
                    _ => false,
                });
                if let Some(when) = when {
                    stats = stats.child(stat(
                        "Last at",
                        device_time(Some(when), false),
                        None,
                        &theme,
                    ));
                }
                stats
            }
        };
        column = column.child(section("Status", None, status_body, &theme));
        let method_label = REBOOT_METHODS
            .iter()
            .find(|(value, _, _)| *value == state.method)
            .map_or(state.method.clone(), |(_, label, _)| (*label).to_owned());
        let form = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                field_row(8.)
                    .child(self.select_field(
                        SelectField::Tool("reboot.method"),
                        "Method",
                        method_label,
                        200.,
                        false,
                        cx,
                    ))
                    .child(
                        self.tool_field(
                            ToolField::new(
                                "reboot.delay",
                                "Delay",
                                state.delay_seconds.to_string(),
                            )
                            .width(110.)
                            .end("s"),
                            window,
                            cx,
                        ),
                    )
                    .child(
                        self.tool_field(
                            ToolField::new("reboot.message", "Message", state.message.clone())
                                .basis(200.),
                            window,
                            cx,
                        ),
                    )
                    .child(switch_row(
                        "reboot-force",
                        state.force,
                        "Force",
                        &theme,
                        cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                            this.update_tool::<RebootState>("reboot", |state| {
                                state.force = !state.force
                            });
                            cx.notify();
                        }),
                    )),
            )
            .child(
                div().flex().child(
                    contained("reboot-open", "Reboot\u{2026}", ButtonColor::Error)
                        .start_icon("RestartAlt")
                        .disabled(!can_run || active)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| this.ask_reboot(cx),
                        )),
                ),
            );
        column
            .child(section("Reboot the device", None, form, &theme))
            .into_any_element()
    }

    pub(super) fn refresh_reboot(&mut self, cx: &mut Context<Self>) {
        self.ui.tools.reboot_refreshed_at = Some(Instant::now());
        self.grpc_call(
            grpc_methods::REBOOT_STATUS,
            json!({}),
            cx,
            |this, result, _| {
                this.update_tool::<RebootState>("reboot", |state| match result {
                    Ok(status) => {
                        state.status = Some(status);
                        state.checked_at = Some(now_ms());
                    }
                    Err(error) => state.error = Some(error),
                })
            },
        );
    }

    fn ask_reboot(&mut self, cx: &mut Context<Self>) {
        let state: RebootState = self.tool("reboot");
        let method = state.method.to_lowercase();
        let description = if state.delay_seconds != 0 {
            format!(
                "The device reboots ({method}) in {} seconds. You can cancel it until then.",
                state.delay_seconds
            )
        } else {
            format!(
                "The device reboots ({method}) right away. Every session to it drops, this one included."
            )
        };
        self.ui.tools.local.typed.clear();
        self.ui.prompt_initial = Some(String::new());
        self.ui.dialog = Some(Dialog::TypedConfirm {
            title: format!("Reboot {}?", self.title),
            description,
            expected: self.title.to_string(),
            confirm_label: "Reboot".into(),
            action: ToolAction::Reboot,
        });
        cx.notify();
    }

    pub(super) fn reboot(&mut self, cx: &mut Context<Self>) {
        let state: RebootState = self.tool("reboot");
        let delay = state.delay_seconds;
        self.grpc_call(
            grpc_methods::REBOOT,
            reboot_request(&state),
            cx,
            move |this, result, cx| match result {
                Ok(_) => {
                    let message = if delay != 0 {
                        format!("Reboot scheduled in {delay} s.")
                    } else {
                        "Reboot requested.".to_owned()
                    };
                    this.toast(Status::Success, message, cx);
                    this.refresh_reboot(cx);
                }
                Err(error) => {
                    this.update_tool::<RebootState>("reboot", |state| state.error = Some(error))
                }
            },
        );
    }

    fn cancel_reboot(&mut self, cx: &mut Context<Self>) {
        self.grpc_call(
            grpc_methods::CANCEL_REBOOT,
            json!({ "message": "Cancelled from Muxus" }),
            cx,
            |this, result, cx| match result {
                Ok(_) => {
                    this.toast(Status::Success, "The reboot was cancelled.", cx);
                    this.refresh_reboot(cx);
                }
                Err(error) => {
                    this.update_tool::<RebootState>("reboot", |state| state.error = Some(error))
                }
            },
        );
    }

    // Processes

    pub(super) fn render_process(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let can_run = self.grpc().is_some();
        let state: ProcessState = self.tool("process");
        let target_set = !muxus_core::js::js_trim(&state.name).is_empty()
            || !muxus_core::js::js_trim(&state.pid).is_empty();
        let mut column = div().child(tool_header(
            "MemoryOutlined",
            "Processes",
            "gnoi.system.System",
            "Restart a misbehaving daemon, or send it a signal, without logging in to a shell.",
            None,
            &theme,
        ));
        if let Some(error) = &state.error {
            column = column.child(error_alert(
                error,
                &theme,
                Some(std::rc::Rc::new(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<ProcessState>("process", |state| state.error = None);
                        cx.notify();
                    },
                ))),
            ));
        }
        let row =
            field_row(8.)
                .child(
                    self.tool_field(
                        ToolField::new("process.name", "Name", state.name.clone())
                            .placeholder("bgp_mgr")
                            .basis(220.)
                            .options(state.applications.clone().unwrap_or_default()),
                        window,
                        cx,
                    ),
                )
                .child(
                    self.tool_field(
                        ToolField::new("process.pid", "PID", state.pid.clone())
                            .placeholder("optional")
                            .width(100.),
                        window,
                        cx,
                    ),
                )
                .child(self.select_field(
                    SelectField::Tool("process.signal"),
                    "Signal",
                    state.signal.replacen("SIGNAL_", "", 1),
                    120.,
                    false,
                    cx,
                ))
                .child(switch_row(
                    "process-restart",
                    state.restart,
                    "Restart it",
                    &theme,
                    cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<ProcessState>("process", |state| {
                            state.restart = !state.restart
                        });
                        cx.notify();
                    }),
                ))
                .child(spacer())
                .child(
                    contained(
                        "process-send",
                        if state.restart {
                            "Restart\u{2026}"
                        } else {
                            "Send\u{2026}"
                        },
                        ButtonColor::Warning,
                    )
                    .disabled(!can_run || !target_set)
                    .on_click(cx.listener_for_app(
                        |this: &mut Self, cx: &mut Context<Self>| this.ask_kill(cx),
                    )),
                );
        column
            .child(section("Process", None, row, &theme))
            .into_any_element()
    }

    /// SR Linux names its processes as applications; offer them.
    pub(super) fn load_applications(&mut self, cx: &mut Context<Self>) {
        self.gnmi_names(
            &["/system/app-management/application[name=*]/name"],
            GnmiDataType::State,
            |result, names| {
                let ManagementResult::GnmiGet { notifications } = result else {
                    return;
                };
                for notification in notifications {
                    for update in &notification.updates {
                        if let Some(name) = super::kit::key_value(&update.path, "application[name=")
                        {
                            names.insert(name);
                        }
                        super::kit::collect_names(&update.value.value, names, false);
                    }
                }
            },
            cx,
            |this, names, _| {
                this.update_tool::<ProcessState>("process", |state| {
                    state.applications = Some(names.unwrap_or_default())
                });
            },
        );
    }

    fn ask_kill(&mut self, cx: &mut Context<Self>) {
        let state: ProcessState = self.tool("process");
        let name = muxus_core::js::js_trim(&state.name).to_owned();
        let pid = muxus_core::js::js_trim(&state.pid).to_owned();
        if self.grpc().is_none() || (name.is_empty() && pid.is_empty()) {
            return;
        }
        let target = if name.is_empty() {
            format!("PID {}", state.pid)
        } else {
            name
        };
        let signal = state.signal.replacen("SIGNAL_", "", 1);
        self.ui.dialog = Some(Dialog::Confirm {
            confirmation: Confirmation {
                title: format!(
                    "{} {target}?",
                    if state.restart { "Restart" } else { "Stop" }
                ),
                description: format!(
                    "{signal} goes to {target} on {}{}.",
                    self.title,
                    if state.restart {
                        ", and the device starts it again"
                    } else {
                        ""
                    }
                ),
                confirm_label: if state.restart {
                    "Restart"
                } else {
                    "Send signal"
                }
                .into(),
                destructive: true,
            },
            action: ConfirmAction::Tool(ToolAction::KillProcess),
        });
        cx.notify();
    }

    pub(super) fn kill_process(&mut self, cx: &mut Context<Self>) {
        let state: ProcessState = self.tool("process");
        let name = muxus_core::js::js_trim(&state.name).to_owned();
        let pid = muxus_core::js::js_trim(&state.pid).to_owned();
        let target = if name.is_empty() {
            format!("PID {}", state.pid)
        } else {
            name.clone()
        };
        let mut request = Map::new();
        if !name.is_empty() {
            request.insert("name".into(), json!(name));
        }
        if !pid.is_empty() {
            request.insert(
                "pid".into(),
                json!(number_of(Some(&Value::String(state.pid.clone())))),
            );
        }
        request.insert("signal".into(), json!(state.signal));
        request.insert("restart".into(), json!(state.restart));
        let restart = state.restart;
        self.grpc_call(
            grpc_methods::KILL_PROCESS,
            Value::Object(request),
            cx,
            move |this, result, cx| match result {
                Ok(_) => this.toast(
                    Status::Success,
                    format!(
                        "{} {target}.",
                        if restart { "Restarted" } else { "Signalled" }
                    ),
                    cx,
                ),
                Err(error) => {
                    this.update_tool::<ProcessState>("process", |state| state.error = Some(error))
                }
            },
        );
    }

    // BGP

    pub(super) fn render_bgp(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let can_run = self.grpc().is_some();
        let instances = self.network_instances(cx);
        let state: BgpState = self.tool("bgp");
        let mut column = div().child(tool_header(
            "HubOutlined",
            "BGP",
            "gnoi.bgp.BGP",
            "Clear a BGP session: soft to exchange routes again, or a reset to bring it down and up.",
            None,
            &theme,
        ));
        if let Some(error) = &state.error {
            column = column.child(error_alert(
                error,
                &theme,
                Some(std::rc::Rc::new(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<BgpState>("bgp", |state| state.error = None);
                        cx.notify();
                    },
                ))),
            ));
        }
        let mode_label = BGP_MODES
            .iter()
            .find(|(value, _, _)| *value == state.mode)
            .map_or(state.mode.clone(), |(_, label, _)| (*label).to_owned());
        let row =
            field_row(8.)
                .child(
                    self.tool_field(
                        ToolField::new("bgp.address", "Neighbor address", state.address.clone())
                            .placeholder("192.0.2.1")
                            .basis(200.)
                            .mono(),
                        window,
                        cx,
                    ),
                )
                .child(
                    self.tool_field(
                        ToolField::new("bgp.instance", "Network instance", state.instance.clone())
                            .placeholder("default")
                            .width(190.)
                            .options(instances),
                        window,
                        cx,
                    ),
                )
                .child(self.select_field(
                    SelectField::Tool("bgp.mode"),
                    "Mode",
                    mode_label,
                    170.,
                    false,
                    cx,
                ))
                .child(spacer())
                .child(
                    contained("bgp-clear", "Clear\u{2026}", ButtonColor::Primary)
                        .disabled(!can_run || muxus_core::js::js_trim(&state.address).is_empty())
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| this.ask_bgp_clear(cx),
                        )),
                );
        column
            .child(section("Neighbor", None, row, &theme))
            .into_any_element()
    }

    fn ask_bgp_clear(&mut self, cx: &mut Context<Self>) {
        let state: BgpState = self.tool("bgp");
        let address = muxus_core::js::js_trim(&state.address).to_owned();
        if self.grpc().is_none() || address.is_empty() {
            return;
        }
        let (_, label, detail) = BGP_MODES
            .iter()
            .find(|(value, _, _)| *value == state.mode)
            .copied()
            .unwrap_or(BGP_MODES[0]);
        self.ui.dialog = Some(Dialog::Confirm {
            confirmation: Confirmation {
                title: format!("Clear BGP neighbor {address}?"),
                description: format!("{label}: {}.", detail.to_lowercase()),
                confirm_label: "Clear".into(),
                destructive: state.mode == "HARD_RESET",
            },
            action: ConfirmAction::Tool(ToolAction::ClearBgp),
        });
        cx.notify();
    }

    pub(super) fn clear_bgp(&mut self, cx: &mut Context<Self>) {
        let state: BgpState = self.tool("bgp");
        let address = muxus_core::js::js_trim(&state.address).to_owned();
        let mut request = Map::new();
        request.insert("address".into(), json!(address));
        request.insert("mode".into(), json!(state.mode));
        let instance = muxus_core::js::js_trim(&state.instance);
        if !instance.is_empty() {
            request.insert("routing_instance".into(), json!(instance));
        }
        self.grpc_call(
            grpc_methods::BGP_CLEAR,
            Value::Object(request),
            cx,
            move |this, result, cx| match result {
                Ok(_) => this.toast(Status::Success, format!("Cleared {address}."), cx),
                Err(error) => {
                    this.update_tool::<BgpState>("bgp", |state| state.error = Some(error))
                }
            },
        );
    }

    // System

    pub(super) fn render_system(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let can_run = self.grpc().is_some();
        let state: SystemState = self.tool("system");
        let refresh = Button::new("system-refresh", "Refresh")
            .disabled(!can_run)
            .on_click(
                cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                    this.refresh_system(cx)
                }),
            )
            .into_any_element();
        let mut column = div().child(tool_header(
            "AccessTime",
            "System",
            "gnoi.system.System \u{b7} gnoi.os.OS",
            "The device's clock compared with this computer's, and the software version it runs.",
            Some(refresh),
            &theme,
        ));
        if let Some(error) = &state.error {
            column = column.child(error_alert(error, &theme, None));
        }
        let skew = skew_ms(&state);
        let device = state
            .device_time
            .as_ref()
            .filter(|time| !time.is_empty())
            .map_or_else(
                || "\u{2014}".to_owned(),
                |time| device_time(Some(&Value::String(time.clone())), false),
            );
        let clock = div()
            .flex()
            .flex_wrap()
            .gap(px(32.))
            .child(stat("Device time", device, None, &theme))
            .child(stat(
                "Offset",
                skew.map_or_else(|| "\u{2014}".to_owned(), skew_text),
                skew.map(|skew| {
                    if skew.abs() < 1000. {
                        p.success.main
                    } else if skew.abs() < 60_000. {
                        p.warning.main
                    } else {
                        p.error.main
                    }
                }),
                &theme,
            ))
            .child(stat(
                "Round trip",
                state
                    .round_trip_ms
                    .map_or_else(|| "\u{2014}".to_owned(), |ms| format!("{ms} ms")),
                None,
                &theme,
            ));
        column = column.child(section("Clock", None, clock, &theme));
        let software = match state.version.as_ref().filter(|version| !version.is_empty()) {
            Some(version) => stat("Version", version.clone(), None, &theme),
            None => body_text(
                match &state.os_error {
                    Some(error) if error.code.as_deref() == Some("UNIMPLEMENTED") => {
                        "The device does not report its version over gNOI OS.".to_owned()
                    }
                    Some(error) => error.message.clone(),
                    None => "Not read yet.".to_owned(),
                },
                p.text.secondary,
                &theme,
            ),
        };
        column
            .child(section("Software", None, software, &theme))
            .into_any_element()
    }

    pub(super) fn refresh_system(&mut self, cx: &mut Context<Self>) {
        let started = Instant::now();
        self.grpc_call(
            grpc_methods::TIME,
            json!({}),
            cx,
            move |this, result, cx| {
                this.update_tool::<SystemState>("system", |state| match result {
                    Ok(response) => {
                        let time = match response.get("time") {
                            Some(Value::String(text)) => text.clone(),
                            Some(Value::Number(number)) => number.to_string(),
                            _ => "0".into(),
                        };
                        state.device_time = Some(time);
                        state.local_at = Some(now_ms());
                        state.round_trip_ms = Some(started.elapsed().as_millis() as i64);
                        state.error = None;
                    }
                    Err(error) => state.error = Some(error),
                });
                this.grpc_call(grpc_methods::OS_VERIFY, json!({}), cx, |this, result, _| {
                    this.update_tool::<SystemState>("system", |state| match result {
                        Ok(response) => {
                            state.version = Some(text_of(response.get("version")));
                            state.os_error = response
                                .get("activation_fail_message")
                                .and_then(Value::as_str)
                                .filter(|message| !message.is_empty())
                                .map(super::kit::message_error);
                        }
                        Err(error) => {
                            state.version = None;
                            state.os_error = Some(error);
                        }
                    })
                });
            },
        );
    }
}

/// Options of the tool selects in this module.
pub fn select_options(name: &str) -> Option<Vec<(String, Option<String>)>> {
    let options = match name {
        "reboot.method" => REBOOT_METHODS
            .iter()
            .map(|(_, label, detail)| ((*label).to_owned(), Some((*detail).to_owned())))
            .collect(),
        "process.signal" => SIGNALS
            .iter()
            .map(|signal| (signal.replacen("SIGNAL_", "", 1), None))
            .collect(),
        "bgp.mode" => BGP_MODES
            .iter()
            .map(|(_, label, detail)| ((*label).to_owned(), Some((*detail).to_owned())))
            .collect(),
        _ => return None,
    };
    Some(options)
}

impl ManagementView {
    /// A choice in one of this module's selects.
    pub(super) fn system_select(&mut self, name: &str, index: usize) {
        match name {
            "reboot.method" => {
                if let Some((value, _, _)) = REBOOT_METHODS.get(index) {
                    self.update_tool::<RebootState>("reboot", |state| {
                        state.method = (*value).into()
                    });
                }
            }
            "process.signal" => {
                if let Some(signal) = SIGNALS.get(index) {
                    self.update_tool::<ProcessState>("process", |state| {
                        state.signal = (*signal).into()
                    });
                }
            }
            "bgp.mode" => {
                if let Some((value, _, _)) = BGP_MODES.get(index) {
                    self.update_tool::<BgpState>("bgp", |state| state.mode = (*value).into());
                }
            }
            _ => {}
        }
    }

    /// A field of this module's tools changed.
    pub(super) fn system_field(&mut self, name: &str, value: String) {
        match name {
            "health.path" => self.update_tool::<HealthState>("health", |state| state.path = value),
            "reboot.delay" => self.update_tool::<RebootState>("reboot", |state| {
                state.delay_seconds = digits_number(&value, 0.) as u64;
            }),
            "reboot.message" => {
                self.update_tool::<RebootState>("reboot", |state| state.message = value)
            }
            "process.name" => {
                self.update_tool::<ProcessState>("process", |state| state.name = value)
            }
            "process.pid" => self.update_tool::<ProcessState>("process", |state| {
                state.pid = value.chars().filter(char::is_ascii_digit).collect();
            }),
            "bgp.address" => self.update_tool::<BgpState>("bgp", |state| state.address = value),
            "bgp.instance" => self.update_tool::<BgpState>("bgp", |state| state.instance = value),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_component_paths_by_model() {
        assert_eq!(
            health_suggestions(&["srl_nokia-platform".into()])[0],
            "/platform/control[slot=A]"
        );
        assert_eq!(
            health_suggestions(&[])[0],
            "/components/component[name=chassis]"
        );
    }

    #[test]
    fn builds_reboot_requests() {
        let state = RebootState {
            delay_seconds: 30,
            force: true,
            ..RebootState::default()
        };
        assert_eq!(
            reboot_request(&state),
            json!({"method": "COLD", "delay": "30000000000", "message": "Requested from Muxus", "force": true})
        );
        let plain = RebootState {
            message: "  ".into(),
            ..RebootState::default()
        };
        assert_eq!(reboot_request(&plain), json!({"method": "COLD"}));
    }

    #[test]
    fn describes_a_pending_reboot() {
        let status =
            json!({"active": true, "method": "WARM", "wait": "45000000000", "reason": "upgrade"});
        assert_eq!(
            pending_text(status.as_object().unwrap()),
            "A warm reboot is pending, in 45 s: upgrade"
        );
        let bare = json!({"active": true});
        assert_eq!(
            pending_text(bare.as_object().unwrap()),
            "A  reboot is pending."
        );
    }

    #[test]
    fn measures_the_clock_offset() {
        let state = SystemState {
            device_time: Some("1700000001500000000".into()),
            local_at: Some(1_700_000_000_100),
            round_trip_ms: Some(200),
            ..SystemState::default()
        };
        assert_eq!(skew_ms(&state), Some(1500.));
        assert_eq!(skew_text(1500.), "+1.50 s");
        assert_eq!(skew_text(-90_000.), "-90 s");
        assert_eq!(timestamp(Some(&json!({"seconds": "0"}))), "\u{2014}");
    }
}
