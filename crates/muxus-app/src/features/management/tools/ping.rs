//! Ping and traceroute from the device (`tools/PingTool.tsx`,
//! `tools/TracerouteTool.tsx`).

use muxus_core::management::{GrpcJson, ManagementError, grpc_methods};
use muxus_ui::gpui_kit::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px, relative,
};
use muxus_ui::theme::{ActiveTheme, Theme};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::super::logic::format::js_number_string;
use super::super::logic::tools::{nanos_to_ms, number_of};
use super::super::navigator::ListenerIndex;
use super::super::view::ManagementView;
use super::super::widgets::{MONO, switch_row, text_tint};
use super::super::workbench::ListenerForApp;
use super::kit::{ToolField, digits_number, error_alert};
use super::{contained, field_row, section, spacer, stat, tool_header};
use muxus_ui::widgets::button::Button;
use muxus_ui::widgets::button::ButtonColor;
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::toggle_button::ToggleButtonGroup;
use muxus_ui::widgets::toggle_button::ToggleOption;
use muxus_ui::widgets::tooltip_lines::TooltipExt;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PingReply {
    pub sequence: f64,
    pub source: String,
    pub bytes: f64,
    pub ttl: f64,
    /// Nanoseconds.
    pub time: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PingSummary {
    pub sent: f64,
    pub received: f64,
    pub min: f64,
    pub avg: f64,
    pub max: f64,
    pub std_dev: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PingState {
    pub destination: String,
    pub network_instance: String,
    pub count: u64,
    pub interval_ms: u64,
    pub size: u64,
    pub source: String,
    /// `auto`, `IPV4` or `IPV6`.
    pub l3: String,
    pub do_not_fragment: bool,
    pub do_not_resolve: bool,
    pub running: bool,
    pub replies: Vec<PingReply>,
    pub summary: Option<PingSummary>,
    pub error: Option<ManagementError>,
    pub target: Option<String>,
}

impl Default for PingState {
    fn default() -> Self {
        Self {
            destination: String::new(),
            network_instance: String::new(),
            count: 5,
            interval_ms: 1000,
            size: 56,
            source: String::new(),
            l3: "auto".into(),
            do_not_fragment: false,
            do_not_resolve: false,
            running: false,
            replies: Vec::new(),
            summary: None,
            error: None,
            target: None,
        }
    }
}

/// The Ping request for this state.
pub fn ping_request(state: &PingState) -> Value {
    let trim = |text: &str| muxus_core::js::js_trim(text).to_owned();
    let mut request = Map::new();
    request.insert("destination".into(), json!(trim(&state.destination)));
    request.insert("count".into(), json!(state.count));
    request.insert(
        "interval".into(),
        json!((state.interval_ms * 1_000_000).to_string()),
    );
    request.insert("size".into(), json!(state.size));
    if !trim(&state.network_instance).is_empty() {
        request.insert(
            "network_instance".into(),
            json!(trim(&state.network_instance)),
        );
    }
    if !trim(&state.source).is_empty() {
        request.insert("source".into(), json!(trim(&state.source)));
    }
    if state.l3 != "auto" {
        request.insert("l3protocol".into(), json!(state.l3));
    }
    if state.do_not_fragment {
        request.insert("do_not_fragment".into(), json!(true));
    }
    if state.do_not_resolve {
        request.insert("do_not_resolve".into(), json!(true));
    }
    Value::Object(request)
}

/// Fold Ping responses into the state: replies, and the summary at the end.
pub fn apply_ping_messages(state: &mut PingState, messages: &[GrpcJson]) {
    for message in messages {
        if message.contains_key("sent") || message.contains_key("received") {
            state.summary = Some(PingSummary {
                sent: number_of(message.get("sent")),
                received: number_of(message.get("received")),
                min: number_of(message.get("min_time")),
                avg: number_of(message.get("avg_time")),
                max: number_of(message.get("max_time")),
                std_dev: number_of(message.get("std_dev")),
            });
        } else {
            state.replies.push(PingReply {
                sequence: number_of(message.get("sequence")),
                source: message
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                bytes: number_of(message.get("bytes")),
                ttl: number_of(message.get("ttl")),
                time: number_of(message.get("time")),
            });
        }
    }
}

/// `nanosToMs` of a number.
fn ms(nanos: f64) -> String {
    nanos_to_ms(
        serde_json::Number::from_f64(nanos)
            .map(Value::Number)
            .as_ref(),
    )
}

/// `loss.toFixed(loss % 1 ? 1 : 0)`.
fn loss_text(loss: f64) -> String {
    if loss % 1. != 0. {
        format!("{loss:.1}%")
    } else {
        format!("{loss:.0}%")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Hop {
    pub hop: f64,
    pub address: String,
    pub name: Option<String>,
    /// Nanoseconds, one per probe.
    pub rtts: Vec<f64>,
    pub state: Option<String>,
    pub as_path: Option<Vec<Value>>,
    pub mpls: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TraceHeader {
    pub address: String,
    pub name: Option<String>,
    pub hops: f64,
    pub packet_size: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TracerouteState {
    pub destination: String,
    pub network_instance: String,
    pub max_ttl: u64,
    /// `ICMP`, `UDP` or `TCP`.
    pub l4: String,
    pub do_not_resolve: bool,
    pub running: bool,
    pub hops: Vec<Hop>,
    pub header: Option<TraceHeader>,
    pub error: Option<ManagementError>,
}

impl Default for TracerouteState {
    fn default() -> Self {
        Self {
            destination: String::new(),
            network_instance: String::new(),
            max_ttl: 30,
            l4: "ICMP".into(),
            do_not_resolve: true,
            running: false,
            hops: Vec::new(),
            header: None,
            error: None,
        }
    }
}

fn text_of(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Number(number)) => Some(number.to_string()),
        Some(Value::Bool(flag)) => Some(flag.to_string()),
        _ => None,
    }
}

/// Fold Traceroute responses into the state: the header, then a probe per message.
pub fn apply_trace_messages(state: &mut TracerouteState, messages: &[GrpcJson]) {
    for message in messages {
        let Some(hop) = message.get("hop") else {
            state.header = Some(TraceHeader {
                address: text_of(message.get("destination_address")).unwrap_or_default(),
                name: text_of(message.get("destination_name")).filter(|name| !name.is_empty()),
                hops: number_of(message.get("hops")),
                packet_size: number_of(message.get("packet_size")),
            });
            continue;
        };
        let number = number_of(Some(hop));
        let address = text_of(message.get("address"));
        let rtt = number_of(message.get("rtt"));
        let wanted = address.clone().unwrap_or_default();
        if let Some(existing) = state
            .hops
            .iter_mut()
            .find(|hop| hop.hop == number && hop.address == wanted)
        {
            existing.rtts.push(rtt);
        } else {
            let hop_state = text_of(message.get("state"))
                .filter(|state| !state.is_empty() && state != "DEFAULT" && state != "NONE");
            state.hops.push(Hop {
                hop: number,
                address: address.unwrap_or_else(|| "*".into()),
                name: text_of(message.get("name")).filter(|name| !name.is_empty()),
                rtts: vec![rtt],
                state: hop_state,
                as_path: message
                    .get("as_path")
                    .and_then(Value::as_array)
                    .filter(|path| !path.is_empty())
                    .cloned(),
                mpls: message
                    .get("mpls")
                    .and_then(Value::as_object)
                    .filter(|mpls| !mpls.is_empty())
                    .cloned(),
            });
        }
    }
    state.hops.sort_by(|a, b| {
        a.hop
            .partial_cmp(&b.hop)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// The Traceroute request for this state.
pub fn trace_request(state: &TracerouteState) -> Value {
    let trim = |text: &str| muxus_core::js::js_trim(text).to_owned();
    let mut request = Map::new();
    request.insert("destination".into(), json!(trim(&state.destination)));
    request.insert("max_ttl".into(), json!(state.max_ttl));
    request.insert("l4protocol".into(), json!(state.l4));
    if !trim(&state.network_instance).is_empty() {
        request.insert(
            "network_instance".into(),
            json!(trim(&state.network_instance)),
        );
    }
    if state.do_not_resolve {
        request.insert("do_not_resolve".into(), json!(true));
    }
    Value::Object(request)
}

fn toggle(labels: &[&str]) -> Vec<ToggleOption> {
    labels.iter().map(|text| ToggleOption::new(*text)).collect()
}

/// One bar per reply, scaled to the slowest (`RttChart`).
fn rtt_chart(replies: &[PingReply], expected: u64, theme: &Theme) -> Div {
    let p = &theme.palette;
    let slots = (expected as usize).max(replies.len());
    let max = replies.iter().map(|reply| reply.time).fold(1., f64::max);
    let width = 1. / slots.max(1) as f32;
    let mut chart = div().h(px(72.)).flex().items_end().gap(px(2.)).mb(px(12.));
    for index in 0..slots {
        let reply = replies
            .iter()
            .find(|candidate| candidate.sequence == (index + 1) as f64)
            .or_else(|| replies.get(index));
        let height = match reply {
            Some(reply) => ((reply.time / max) * 68.).max(4.) as f32,
            None => 2.,
        };
        let tip = match reply {
            Some(reply) => format!(
                "seq {}: {} ms",
                js_number_string(reply.sequence),
                ms(reply.time)
            ),
            None => "waiting".into(),
        };
        let mut bar = div()
            .id(("rtt", index))
            .w(relative(width))
            .max_w(px(28.))
            .h(px(height))
            .rounded_t(px(3.));
        bar = match reply {
            Some(_) => bar.bg(p.primary.main).opacity(0.85),
            None => bar.bg(p.action.selected),
        };
        chart = chart.child(bar.mui_tooltip(tip));
    }
    chart
}

impl ManagementView {
    pub(super) fn render_ping(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let can_run = self.grpc().is_some();
        let instances = self.network_instances(cx);
        let state: PingState = self.tool("ping");
        let more = self.ui.tools.local.more;
        let destination_set = !muxus_core::js::js_trim(&state.destination).is_empty();

        let mut target = field_row(8.)
            .child(
                self.tool_field(
                    ToolField::new("ping.destination", "Destination", state.destination.clone())
                        .placeholder("10.0.0.2 or host name")
                        .basis(220.)
                        .mono(),
                    window,
                    cx,
                ),
            )
            .child(
                self.tool_field(
                    ToolField::new(
                        "ping.instance",
                        "Network instance",
                        state.network_instance.clone(),
                    )
                    .placeholder("default")
                    .width(190.)
                    .options(instances),
                    window,
                    cx,
                ),
            )
            .child(self.tool_field(
                ToolField::new("ping.count", "Count", state.count.to_string()).width(76.),
                window,
                cx,
            ))
            .child(
                self.tool_field(
                    ToolField::new("ping.interval", "Interval", state.interval_ms.to_string())
                        .width(110.)
                        .end("ms"),
                    window,
                    cx,
                ),
            )
            .child(
                Button::new("ping-more", "More")
                    .tint(text_tint(p.text.secondary, &theme))
                    .end_icon("ExpandMore", 8.)
                    .end_icon_turned(more)
                    .on_click(
                        cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                            this.ui.tools.local.more = !this.ui.tools.local.more;
                            cx.notify();
                        }),
                    ),
            )
            .child(spacer());
        target = target.child(if state.running {
            Button::new("ping-stop", "Stop")
                .color(ButtonColor::Error)
                .outlined()
                .start_icon("StopRounded")
                .on_click(cx.listener_for_app(|this: &mut Self, _| this.cancel_stream("ping")))
                .into_any_element()
        } else {
            contained("ping-start", "Ping", ButtonColor::Primary)
                .start_icon("PlayArrowRounded")
                .disabled(!can_run || !destination_set)
                .on_click(
                    cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                        this.start_ping(cx)
                    }),
                )
                .into_any_element()
        });
        let mut body = div().child(target);
        if more {
            let l3 = ["auto", "IPV4", "IPV6"]
                .iter()
                .position(|value| *value == state.l3);
            body = body.child(
                field_row(12.)
                    .pt(px(12.))
                    .child(
                        self.tool_field(
                            ToolField::new("ping.size", "Size", state.size.to_string())
                                .width(130.)
                                .end("bytes"),
                            window,
                            cx,
                        ),
                    )
                    .child(
                        self.tool_field(
                            ToolField::new("ping.source", "Source", state.source.clone())
                                .placeholder("any")
                                .width(170.),
                            window,
                            cx,
                        ),
                    )
                    .child(
                        ToggleButtonGroup::exclusive(
                            "ping-l3",
                            toggle(&["Auto", "IPv4", "IPv6"]),
                            l3,
                            cx.listener_index(|this: &mut Self, index, cx: &mut Context<Self>| {
                                this.update_tool::<PingState>("ping", |state| {
                                    state.l3 = ["auto", "IPV4", "IPV6"][index].into();
                                });
                                cx.notify();
                            }),
                        )
                        .inline(3.2, 8.8, 12.),
                    )
                    .child(switch_row(
                        "ping-df",
                        state.do_not_fragment,
                        "Don\u{2019}t fragment",
                        &theme,
                        cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                            this.update_tool::<PingState>("ping", |state| {
                                state.do_not_fragment = !state.do_not_fragment
                            });
                            cx.notify();
                        }),
                    ))
                    .child(switch_row(
                        "ping-dnr",
                        state.do_not_resolve,
                        "Don\u{2019}t resolve names",
                        &theme,
                        cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                            this.update_tool::<PingState>("ping", |state| {
                                state.do_not_resolve = !state.do_not_resolve
                            });
                            cx.notify();
                        }),
                    )),
            );
        }

        let mut column = div()
            .child(tool_header(
                "NetworkPing",
                "Ping",
                "gnoi.system.System",
                "Ping from the device itself, in any of its network instances. Replies stream in as they arrive.",
                None,
                &theme,
            ))
            .child(section("Target", None, body, &theme));
        if let Some(error) = &state.error {
            column = column.child(error_alert(
                error,
                &theme,
                Some(std::rc::Rc::new(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<PingState>("ping", |state| state.error = None);
                        cx.notify();
                    },
                ))),
            ));
        }
        if !state.replies.is_empty() || state.running || state.summary.is_some() {
            column = column.child(section(
                &match &state.target {
                    Some(target) => format!("Ping {target}"),
                    None => "Ping".into(),
                },
                None,
                ping_results(&state, &theme),
                &theme,
            ));
        }
        column.into_any_element()
    }

    pub(super) fn start_ping(&mut self, cx: &mut Context<Self>) {
        let state: PingState = self.tool("ping");
        let destination = muxus_core::js::js_trim(&state.destination).to_owned();
        if self.grpc().is_none() || destination.is_empty() {
            return;
        }
        let request = ping_request(&state);
        self.update_tool::<PingState>("ping", |state| {
            state.running = true;
            state.replies.clear();
            state.summary = None;
            state.error = None;
            state.target = Some(destination);
        });
        self.start_stream(
            "ping",
            grpc_methods::PING,
            Some(request),
            None,
            cx,
            |this, messages, _| {
                this.update_tool::<PingState>("ping", |state| apply_ping_messages(state, &messages))
            },
            |this, error, _| {
                this.update_tool::<PingState>("ping", |state| {
                    state.running = false;
                    if let Some(error) = error.filter(|error| error.cancelled != Some(true)) {
                        state.error = Some(error);
                    }
                })
            },
        );
        cx.notify();
    }

    pub(super) fn ping_field(&mut self, name: &str, value: String) {
        self.update_tool::<PingState>("ping", |state| match name {
            "ping.destination" => state.destination = value,
            "ping.instance" => state.network_instance = value,
            "ping.count" => state.count = digits_number(&value, 1.).clamp(1., 1000.) as u64,
            "ping.interval" => state.interval_ms = digits_number(&value, 1000.).max(10.) as u64,
            "ping.size" => state.size = digits_number(&value, 0.).clamp(0., 65_000.) as u64,
            "ping.source" => state.source = value,
            _ => {}
        });
    }

    pub(super) fn render_traceroute(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let can_run = self.grpc().is_some();
        let instances = self.network_instances(cx);
        let state: TracerouteState = self.tool("traceroute");
        let destination_set = !muxus_core::js::js_trim(&state.destination).is_empty();
        let l4 = ["ICMP", "UDP", "TCP"]
            .iter()
            .position(|value| *value == state.l4);
        let mut target = field_row(8.)
            .child(
                self.tool_field(
                    ToolField::new(
                        "traceroute.destination",
                        "Destination",
                        state.destination.clone(),
                    )
                    .placeholder("10.0.0.2 or host name")
                    .basis(220.)
                    .mono(),
                    window,
                    cx,
                ),
            )
            .child(
                self.tool_field(
                    ToolField::new(
                        "traceroute.instance",
                        "Network instance",
                        state.network_instance.clone(),
                    )
                    .placeholder("default")
                    .width(190.)
                    .options(instances),
                    window,
                    cx,
                ),
            )
            .child(self.tool_field(
                ToolField::new("traceroute.ttl", "Max hops", state.max_ttl.to_string()).width(90.),
                window,
                cx,
            ))
            .child(
                ToggleButtonGroup::exclusive(
                    "trace-l4",
                    toggle(&["ICMP", "UDP", "TCP"]),
                    l4,
                    cx.listener_index(|this: &mut Self, index, cx: &mut Context<Self>| {
                        this.update_tool::<TracerouteState>("traceroute", |state| {
                            state.l4 = ["ICMP", "UDP", "TCP"][index].into();
                        });
                        cx.notify();
                    }),
                )
                .inline(3.2, 8.8, 12.),
            )
            .child(switch_row(
                "trace-resolve",
                !state.do_not_resolve,
                "Resolve names",
                &theme,
                cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                    this.update_tool::<TracerouteState>("traceroute", |state| {
                        state.do_not_resolve = !state.do_not_resolve
                    });
                    cx.notify();
                }),
            ))
            .child(spacer());
        target = target.child(if state.running {
            Button::new("trace-stop", "Stop")
                .color(ButtonColor::Error)
                .outlined()
                .start_icon("StopRounded")
                .on_click(
                    cx.listener_for_app(|this: &mut Self, _| this.cancel_stream("traceroute")),
                )
                .into_any_element()
        } else {
            contained("trace-start", "Trace", ButtonColor::Primary)
                .start_icon("PlayArrowRounded")
                .disabled(!can_run || !destination_set)
                .on_click(
                    cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                        this.start_traceroute(cx)
                    }),
                )
                .into_any_element()
        });
        let mut column = div()
            .child(tool_header(
                "RouteOutlined",
                "Traceroute",
                "gnoi.system.System",
                "Trace the path from the device to a destination, hop by hop, with round-trip times, AS paths and MPLS labels where the device reports them.",
                None,
                &theme,
            ))
            .child(section("Target", None, target, &theme));
        if let Some(error) = &state.error {
            column = column.child(error_alert(
                error,
                &theme,
                Some(std::rc::Rc::new(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<TracerouteState>("traceroute", |state| {
                            state.error = None
                        });
                        cx.notify();
                    },
                ))),
            ));
        }
        if !state.hops.is_empty() || state.running {
            let title = match &state.header {
                Some(header) => format!(
                    "Route to {}",
                    header
                        .name
                        .clone()
                        .unwrap_or_else(|| header.address.clone())
                ),
                None => "Route".into(),
            };
            let actions = state.header.as_ref().map(|header| {
                super::kit::caption_text(
                    format!(
                        "{} \u{b7} {} byte packets \u{b7} up to {} hops",
                        header.address,
                        js_number_string(header.packet_size),
                        js_number_string(header.hops)
                    ),
                    p.text.secondary,
                    &theme,
                )
                .into_any_element()
            });
            column = column.child(section(&title, actions, route(&state, &theme), &theme));
        }
        column.into_any_element()
    }

    pub(super) fn start_traceroute(&mut self, cx: &mut Context<Self>) {
        let state: TracerouteState = self.tool("traceroute");
        if self.grpc().is_none() || muxus_core::js::js_trim(&state.destination).is_empty() {
            return;
        }
        let request = trace_request(&state);
        self.update_tool::<TracerouteState>("traceroute", |state| {
            state.running = true;
            state.hops.clear();
            state.header = None;
            state.error = None;
        });
        self.start_stream(
            "traceroute",
            grpc_methods::TRACEROUTE,
            Some(request),
            None,
            cx,
            |this, messages, _| {
                this.update_tool::<TracerouteState>("traceroute", |state| {
                    apply_trace_messages(state, &messages)
                })
            },
            |this, error, _| {
                this.update_tool::<TracerouteState>("traceroute", |state| {
                    state.running = false;
                    if let Some(error) = error.filter(|error| error.cancelled != Some(true)) {
                        state.error = Some(error);
                    }
                })
            },
        );
        cx.notify();
    }

    pub(super) fn traceroute_field(&mut self, name: &str, value: String) {
        self.update_tool::<TracerouteState>("traceroute", |state| match name {
            "traceroute.destination" => state.destination = value,
            "traceroute.instance" => state.network_instance = value,
            "traceroute.ttl" => state.max_ttl = digits_number(&value, 30.).clamp(1., 255.) as u64,
            _ => {}
        });
    }
}

/// The running numbers, the chart and the reply lines of a ping.
fn ping_results(state: &PingState, theme: &Theme) -> Div {
    let p = &theme.palette;
    let received = state
        .summary
        .as_ref()
        .map_or(state.replies.len() as f64, |summary| summary.received);
    let sent = state
        .summary
        .as_ref()
        .map_or(state.replies.len() as f64, |summary| summary.sent);
    let loss = state
        .summary
        .as_ref()
        .filter(|summary| summary.sent != 0.)
        .map(|summary| (1. - summary.received / summary.sent) * 100.);
    let times: Vec<f64> = state.replies.iter().map(|reply| reply.time).collect();
    let or = |summary: Option<f64>, fallback: f64| {
        summary
            .filter(|value| *value != 0. && !value.is_nan())
            .unwrap_or(fallback)
    };
    let timed = |value: f64| {
        if times.is_empty() {
            "\u{2014}".to_owned()
        } else {
            format!("{} ms", ms(value))
        }
    };
    let min = or(
        state.summary.as_ref().map(|s| s.min),
        times.iter().copied().fold(f64::INFINITY, f64::min),
    );
    let max = or(
        state.summary.as_ref().map(|s| s.max),
        times.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    );
    let avg = or(
        state.summary.as_ref().map(|s| s.avg),
        times.iter().sum::<f64>() / times.len().max(1) as f64,
    );
    let mut body = div();
    if state.running {
        body = body.child(
            div()
                .mb(px(12.))
                .child(muxus_ui::widgets::display::linear_progress(
                    theme,
                    state.replies.len() as f32 / state.count.max(1) as f32,
                )),
        );
    }
    let mut stats = div()
        .flex()
        .flex_wrap()
        .gap(px(24.))
        .mb(px(16.))
        .child(stat("Sent", js_number_string(sent), None, theme))
        .child(stat(
            "Received",
            js_number_string(received),
            (received != 0.).then_some(p.success.main),
            theme,
        ))
        .child(stat(
            "Loss",
            loss.map_or_else(|| "\u{2014}".to_owned(), loss_text),
            loss.map(|loss| {
                if loss == 0. {
                    p.success.main
                } else if loss < 100. {
                    p.warning.main
                } else {
                    p.error.main
                }
            }),
            theme,
        ))
        .child(stat("Min", timed(min), None, theme))
        .child(stat("Avg", timed(avg), None, theme))
        .child(stat("Max", timed(max), None, theme));
    if let Some(summary) = &state.summary {
        stats = stats.child(stat(
            "Std dev",
            format!("{} ms", ms(summary.std_dev)),
            None,
            theme,
        ));
    }
    body = body
        .child(stats)
        .child(rtt_chart(&state.replies, state.count, theme));
    let line = 12.5 * 1.7;
    let mut lines = div()
        .id("ping-replies")
        .font_family(MONO)
        .text_size(px(12.5))
        .line_height(px(line))
        .max_h(px(240.))
        .overflow_y_scroll();
    for reply in &state.replies {
        lines = lines.child(
            div()
                .flex()
                .flex_wrap()
                .child(format!("{} bytes from ", js_number_string(reply.bytes)))
                .child(
                    div()
                        .font_weight(FontWeight(700.))
                        .child(reply.source.clone()),
                )
                .child(format!(
                    ": seq={} ttl={} time=",
                    js_number_string(reply.sequence),
                    js_number_string(reply.ttl)
                ))
                .child(
                    div()
                        .text_color(p.info.main)
                        .child(format!("{} ms", ms(reply.time))),
                ),
        );
    }
    body.child(lines)
}

/// The hops of a traceroute on a line, as far as they are known.
fn route(state: &TracerouteState, theme: &Theme) -> Div {
    let p = &theme.palette;
    let reached = state
        .header
        .as_ref()
        .is_some_and(|header| state.hops.iter().any(|hop| hop.address == header.address));
    let mut list = div().relative().pl(px(24.)).child(
        div()
            .absolute()
            .left(px(9.))
            .top(px(10.))
            .bottom(px(10.))
            .w(px(2.))
            .bg(p.divider)
            .rounded(px(theme.radius)),
    );
    for (index, hop) in state.hops.iter().enumerate() {
        let last = reached
            && state
                .header
                .as_ref()
                .is_some_and(|header| hop.address == header.address);
        let border = if last {
            p.success.main
        } else if hop.address == "*" {
            p.text.disabled
        } else {
            p.primary.main
        };
        let named = hop.name.as_ref().is_some_and(|name| *name != hop.address);
        let mut name_line = div()
            .flex()
            .flex_wrap()
            .font_family(MONO)
            .text_size(px(13.))
            .line_height(px(13. * 1.5))
            .font_weight(FontWeight(600.));
        if named {
            name_line = name_line
                .child(format!("{} ", hop.name.clone().unwrap_or_default()))
                .child(
                    div()
                        .font_weight(FontWeight(400.))
                        .text_color(p.text.secondary)
                        .child(format!("({})", hop.address)),
                );
        } else {
            name_line = name_line.child(hop.address.clone());
        }
        let mut middle = div().flex_1().min_w_0().child(name_line);
        if hop.as_path.is_some() || hop.mpls.is_some() || hop.state.is_some() {
            let mut chips = div().flex().gap(px(4.)).mt(px(2.));
            if let Some(hop_state) = &hop.state {
                chips = chips.child(
                    Chip::new(hop_state.replace('_', " ").to_lowercase())
                        .outlined()
                        .color(p.warning.main)
                        .height(18.)
                        .font_size(10.5),
                );
            }
            if let Some(path) = &hop.as_path {
                let joined = path
                    .iter()
                    .map(|asn| match asn {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                chips = chips.child(
                    Chip::new(format!("AS {joined}"))
                        .height(18.)
                        .font_size(10.5),
                );
            }
            if let Some(mpls) = &hop.mpls {
                for (key, value) in mpls {
                    let value = match value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    chips = chips.child(
                        Chip::new(format!("{key} {value}"))
                            .height(18.)
                            .font_size(10.5),
                    );
                }
            }
            middle = middle.child(chips);
        }
        let mut rtts = div().flex().gap(px(4.));
        for rtt in &hop.rtts {
            rtts = rtts.child(
                Chip::new(format!("{} ms", ms(*rtt)))
                    .outlined()
                    .height(20.)
                    .font_size(11.)
                    .mono(),
            );
        }
        list = list.child(
            div()
                .id(SharedString::from(format!("hop-{index}")))
                .relative()
                .flex()
                .items_center()
                .gap(px(12.))
                .py(px(6.))
                .child(
                    div()
                        .absolute()
                        .left(px(-21.))
                        .top_0()
                        .bottom_0()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .size(px(12.))
                                .rounded_full()
                                .border_2()
                                .border_color(border)
                                .bg(if last {
                                    p.success.main
                                } else {
                                    p.background_paper
                                }),
                        ),
                )
                .child(
                    div()
                        .w(px(26.))
                        .flex_none()
                        .font_family(MONO)
                        .text_size(px(12.))
                        .line_height(px(12. * 1.5))
                        .text_color(p.text.secondary)
                        .child(js_number_string(hop.hop)),
                )
                .child(middle)
                .child(rtts),
        );
    }
    if state.running {
        list = list.child(
            div()
                .relative()
                .flex()
                .items_center()
                .gap(px(12.))
                .py(px(6.))
                .child(
                    div()
                        .absolute()
                        .left(px(-21.))
                        .top_0()
                        .bottom_0()
                        .flex()
                        .items_center()
                        .child(CircularProgress::new(
                            ("ping-progress", 1usize),
                            12.,
                            p.primary.main,
                        )),
                )
                .child(super::kit::body_text(
                    "Probing the next hop \u{2026}",
                    p.text.secondary,
                    theme,
                )),
        );
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(value: Value) -> GrpcJson {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn builds_ping_requests() {
        let state = PingState {
            destination: " 10.0.0.2 ".into(),
            network_instance: "mgmt".into(),
            l3: "IPV6".into(),
            do_not_fragment: true,
            ..PingState::default()
        };
        assert_eq!(
            ping_request(&state),
            json!({
                "destination": "10.0.0.2",
                "count": 5,
                "interval": "1000000000",
                "size": 56,
                "network_instance": "mgmt",
                "l3protocol": "IPV6",
                "do_not_fragment": true
            })
        );
    }

    #[test]
    fn folds_replies_and_the_summary() {
        let mut state = PingState::default();
        apply_ping_messages(
            &mut state,
            &[
                message(
                    json!({"source": "10.0.0.2", "time": "1500000", "bytes": 64, "sequence": 1, "ttl": 64}),
                ),
                message(
                    json!({"sent": 1, "received": 1, "min_time": "1500000", "avg_time": "1500000", "max_time": "1500000"}),
                ),
            ],
        );
        assert_eq!(state.replies.len(), 1);
        assert_eq!(state.replies[0].time, 1_500_000.);
        assert_eq!(
            state.summary.as_ref().map(|summary| summary.received),
            Some(1.)
        );
        assert_eq!(loss_text(0.), "0%");
        assert_eq!(loss_text(100. / 3.), "33.3%");
        assert_eq!(ms(1_500_000.), "1.50");
    }

    #[test]
    fn folds_traceroute_probes_into_hops() {
        let mut state = TracerouteState::default();
        apply_trace_messages(
            &mut state,
            &[
                message(json!({"destination_address": "10.0.0.9", "hops": 30, "packet_size": 60})),
                message(json!({"hop": 2, "address": "10.0.0.9", "rtt": "2000000"})),
                message(
                    json!({"hop": 1, "address": "10.0.0.1", "rtt": "1000000", "state": "DEFAULT"}),
                ),
                message(json!({"hop": 1, "address": "10.0.0.1", "rtt": "1100000"})),
                message(json!({"hop": 3, "rtt": "0", "state": "HOST_UNREACHABLE"})),
                message(json!({"hop": 3, "rtt": "0"})),
            ],
        );
        let header = state.header.clone().unwrap();
        assert_eq!(header.address, "10.0.0.9");
        assert_eq!(header.packet_size, 60.);
        let hops: Vec<(f64, &str, usize)> = state
            .hops
            .iter()
            .map(|hop| (hop.hop, hop.address.as_str(), hop.rtts.len()))
            .collect();
        // A probe without an address never matches the "*" hop it made (as in React).
        assert_eq!(
            hops,
            vec![
                (1., "10.0.0.1", 2),
                (2., "10.0.0.9", 1),
                (3., "*", 1),
                (3., "*", 1)
            ]
        );
        assert_eq!(state.hops[0].state, None);
        assert_eq!(state.hops[2].state.as_deref(), Some("HOST_UNREACHABLE"));
    }
}
