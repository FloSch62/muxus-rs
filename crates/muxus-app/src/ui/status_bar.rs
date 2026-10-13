//! `client/src/components/StatusBar.tsx` and `api/host-stats.ts`: the active
//! session's host at a glance along the bottom of the window. Only the tab in
//! front is polled (every 3 s, the second reading after 1 s so rates appear
//! at once); readings outlive the bar's attention, so switching back to a tab
//! shows its last values at once.

use std::collections::HashMap;
use std::time::Duration;

use muxus_app::model::SessionProfile;
use muxus_app::state::tabs::{TabStatus, TerminalTab};
use muxus_core::api::HostStatsResponse;
use muxus_session::ApiError;
use muxus_session::stats::view::{
    HostStatsView, MeterLevel, TimedSample, describe_users, format_bytes, format_percent,
    format_rate, format_uptime, host_stats_view, js_to_fixed, memory_used, meter_level,
    timed_sample,
};
use muxus_store::prefs::layout::StatusBarItem;
use muxus_ui::gpui_kit::{
    AnyElement, ClickEvent, Context, Div, Hsla, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled, Task, Window,
    div, px, relative,
};
use muxus_ui::label::label;
use muxus_ui::text::TextStyleExt;
use muxus_ui::theme::{ActiveTheme, Status, Theme, layout};
use muxus_ui::widgets::menu::{self, MenuItem, menu_divider};
use muxus_ui::widgets::tooltip::{AnchorBounds, Tooltip, bounds_probe};
use muxus_ui::widgets::{IconButton, Placement};

use crate::ui::prefs::STATUS_BAR_ITEMS;
use crate::ui::shell::{AppShell, ShellMenu};

/// How often the bar refreshes.
const POLL: Duration = Duration::from_secs(3);
/// The second reading comes sooner, so CPU and traffic appear right away.
const FIRST_RATE: Duration = Duration::from_secs(1);
const RETRY: Duration = Duration::from_secs(15);
/// Rates over a longer gap (a tab shown again) are not current.
const MAX_RATE_GAP_SECONDS: f64 = 30.;

/// Where a tab's statistics come from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum StatsSource {
    Local,
    Ssh(String),
}

impl StatsSource {
    fn key(&self) -> String {
        match self {
            StatsSource::Local => "local".into(),
            StatsSource::Ssh(conn_id) => format!("ssh:{conn_id}"),
        }
    }
}

/// What the bar knows about one source.
#[derive(Clone, Debug)]
enum HostStatsState {
    Loading,
    /// `stale`: the last reading failed and these values are older.
    Ok {
        view: Box<HostStatsView>,
        stale: bool,
    },
    Unsupported,
    Error(String),
}

struct StatsEntry {
    state: HostStatsState,
    latest: Option<TimedSample>,
}

/// The polling of the active tab's host, and the last reading of every host.
#[derive(Default)]
pub struct HostStatsPoller {
    pub key: Option<String>,
    cache: HashMap<String, StatsEntry>,
    task: Option<Task<()>>,
}

impl HostStatsPoller {
    fn current(&self) -> Option<&HostStatsState> {
        self.key
            .as_ref()
            .and_then(|key| self.cache.get(key))
            .map(|entry| &entry.state)
    }

    /// Take one reading in; the delay before the next, or `None` to stop.
    fn store(
        &mut self,
        key: &str,
        response: Result<HostStatsResponse, ApiError>,
    ) -> Option<Duration> {
        let entry = self
            .cache
            .entry(key.to_string())
            .or_insert_with(|| StatsEntry {
                state: HostStatsState::Loading,
                latest: None,
            });
        match response {
            // A host that cannot run the probe is not asked again on this connection.
            Ok(HostStatsResponse::Unsupported { .. }) => {
                *entry = StatsEntry {
                    state: HostStatsState::Unsupported,
                    latest: None,
                };
                None
            }
            Ok(HostStatsResponse::Ok { sampled_at, sample }) => {
                let current = timed_sample(sample, sampled_at);
                let previous = entry
                    .latest
                    .take()
                    .filter(|latest| current.at - latest.at <= MAX_RATE_GAP_SECONDS);
                entry.state = HostStatsState::Ok {
                    view: Box::new(host_stats_view(&current, previous.as_ref())),
                    stale: false,
                };
                entry.latest = Some(current);
                Some(if previous.is_some() { POLL } else { FIRST_RATE })
            }
            Err(error) => {
                match &mut entry.state {
                    HostStatsState::Ok { stale, .. } => *stale = true,
                    state => *state = HostStatsState::Error(error.to_string()),
                }
                Some(RETRY)
            }
        }
    }
}

/// The active tab's statistics source, or why it has none (`tabSource`).
fn tab_source(tab: Option<&TerminalTab>) -> Result<StatsSource, &'static str> {
    let Some(tab) = tab else {
        return Err("No session in this tab");
    };
    let Some(profile) = &tab.profile else {
        return Err("No session in this tab");
    };
    if !matches!(
        profile,
        SessionProfile::Ssh { .. } | SessionProfile::Local { .. }
    ) {
        return Err("Statistics are shown for SSH and local sessions");
    }
    if tab.status != TabStatus::Connected {
        return Err("Not connected");
    }
    if matches!(profile, SessionProfile::Local { .. }) {
        return Ok(StatsSource::Local);
    }
    tab.conn_id
        .clone()
        .map(StatsSource::Ssh)
        .ok_or("Not connected")
}

/// Tooltip lines that exist, on one line (`details`).
fn details<'a>(parts: impl IntoIterator<Item = Option<&'a str>>) -> String {
    parts
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

fn plural_cores(cores: u32) -> String {
    format!("{cores} {}", if cores == 1 { "core" } else { "cores" })
}

/// `new Date(...).toLocaleString()` in the en-US form the client shows.
fn up_since(uptime_seconds: f64) -> String {
    let since =
        chrono::Local::now() - chrono::Duration::milliseconds((uptime_seconds * 1000.) as i64);
    since.format("%-m/%-d/%Y, %-I:%M:%S %p").to_string()
}

/// The text and decorations of one item, or `None` when the host does not report it.
struct StatParts {
    label: Option<&'static str>,
    tooltip: String,
    meter: Option<f64>,
    /// Long free text (a host or system name) gives way first.
    shrink: bool,
    value: StatValue,
}

enum StatValue {
    Ink(String),
    Muted(String),
    /// ↓ received ↑ sent.
    Traffic(String, String),
}

fn stat_parts(item: StatusBarItem, view: &HostStatsView) -> Option<StatParts> {
    let sample = &view.sample;
    let parts = match item {
        StatusBarItem::Hostname => {
            let hostname = sample.hostname.as_deref().filter(|name| !name.is_empty())?;
            StatParts {
                label: None,
                tooltip: details([Some(hostname), sample.kernel.as_deref()]),
                meter: None,
                shrink: true,
                value: StatValue::Ink(hostname.to_string()),
            }
        }
        StatusBarItem::Os => {
            let os = sample.os.as_deref().filter(|os| !os.is_empty())?;
            StatParts {
                label: None,
                tooltip: sample.kernel.clone().unwrap_or_else(|| os.to_string()),
                meter: None,
                shrink: true,
                value: StatValue::Muted(os.to_string()),
            }
        }
        StatusBarItem::Cpu => {
            let load = sample.load_average.map(|load| {
                format!(
                    "Load average {}",
                    load.iter()
                        .map(|value| js_to_fixed(*value, 2))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            });
            let cores = sample.cores.filter(|cores| *cores > 0).map(plural_cores);
            if view.cpu.is_none() && sample.cpu_time.is_none() {
                // No CPU counters (macOS and BSD hosts): the load average stands in.
                let first = sample.load_average?[0];
                return Some(StatParts {
                    label: Some("Load"),
                    tooltip: details([load.as_deref(), cores.as_deref()]),
                    meter: None,
                    shrink: false,
                    value: StatValue::Ink(js_to_fixed(first, 2)),
                });
            }
            sample.cpu_time?;
            let usage = match view.cpu {
                None => "Measuring CPU usage…".to_string(),
                Some(cpu) => format!("CPU usage {}", format_percent(cpu)),
            };
            StatParts {
                label: Some("CPU"),
                tooltip: details([Some(usage.as_str()), cores.as_deref(), load.as_deref()]),
                meter: Some(view.cpu.unwrap_or(0.)),
                shrink: false,
                value: StatValue::Ink(view.cpu.map_or("…".into(), format_percent)),
            }
        }
        StatusBarItem::Memory => {
            let memory = sample.memory.filter(|memory| memory.total_bytes > 0)?;
            let used = memory_used(&memory);
            let swap = match (memory.swap_total_bytes, memory.swap_free_bytes) {
                (Some(total), Some(free)) if total > 0 => Some(format!(
                    "Swap {} of {}",
                    format_bytes(total.saturating_sub(free) as f64),
                    format_bytes(total as f64)
                )),
                _ => None,
            };
            let summary = format!(
                "Memory {} used of {}, {} available",
                format_bytes(used as f64),
                format_bytes(memory.total_bytes as f64),
                format_bytes(memory.available_bytes as f64)
            );
            StatParts {
                label: Some("RAM"),
                tooltip: details([Some(summary.as_str()), swap.as_deref()]),
                meter: Some(used as f64 / memory.total_bytes as f64),
                shrink: false,
                value: StatValue::Ink(format!(
                    "{} / {}",
                    format_bytes(used as f64),
                    format_bytes(memory.total_bytes as f64)
                )),
            }
        }
        StatusBarItem::Disk => {
            let disk = sample.disk.as_ref().filter(|disk| disk.total_bytes > 0)?;
            // Like df: the share of the space users can have, so root's reserve counts as full.
            let usable = disk.used_bytes + disk.available_bytes;
            let fraction = if usable > 0 {
                disk.used_bytes as f64 / usable as f64
            } else {
                0.
            };
            let summary = format!(
                "{} used of {}, {} free",
                format_bytes(disk.used_bytes as f64),
                format_bytes(disk.total_bytes as f64),
                format_bytes(disk.available_bytes as f64)
            );
            StatParts {
                label: Some("Disk"),
                tooltip: details([Some(disk.mount.as_str()), Some(summary.as_str())]),
                meter: Some(fraction),
                shrink: false,
                value: StatValue::Ink(format_percent(fraction)),
            }
        }
        StatusBarItem::Network => {
            let network = sample.network.as_ref()?;
            let summary = format!(
                "{} received, {} sent since it came up",
                format_bytes(network.received_bytes as f64),
                format_bytes(network.sent_bytes as f64)
            );
            StatParts {
                label: None,
                tooltip: details([Some(network.interface.as_str()), Some(summary.as_str())]),
                meter: None,
                shrink: false,
                value: StatValue::Traffic(
                    view.receive_rate.map_or("…".into(), format_rate),
                    view.send_rate.map_or("…".into(), format_rate),
                ),
            }
        }
        StatusBarItem::Uptime => {
            let uptime = sample.uptime_seconds?;
            StatParts {
                label: Some("Up"),
                tooltip: format!("Up since {}", up_since(uptime)),
                meter: None,
                shrink: false,
                value: StatValue::Ink(format_uptime(uptime)),
            }
        }
        StatusBarItem::Users => {
            let users = sample.users.as_ref()?;
            StatParts {
                label: Some("Users"),
                tooltip: if users.is_empty() {
                    "Nobody is logged in".into()
                } else {
                    describe_users(users)
                },
                meter: None,
                shrink: false,
                value: StatValue::Ink(users.len().to_string()),
            }
        }
    };
    Some(parts)
}

fn status_text(text: impl Into<SharedString>, color: Hsla) -> impl IntoElement {
    label(text)
        .size(12.)
        .weight(450.)
        .line_height(18.)
        .tracking(-0.1)
        .tabular_nums()
        .color(color)
}

/// A small usage bar; neutral until usage gets high.
fn meter(fraction: f64, theme: &Theme) -> impl IntoElement {
    let p = theme.palette;
    let clamped = fraction.clamp(0., 1.);
    let fill = match meter_level(clamped) {
        MeterLevel::Critical => theme.status_text_color(Status::Error),
        MeterLevel::Warning => theme.status_text_color(Status::Warning),
        MeterLevel::Normal => p.text.secondary.opacity(0.75),
    };
    div()
        .flex_none()
        .w(px(32.))
        .h(px(4.))
        .rounded(px(2.))
        .overflow_hidden()
        .bg(p.text.secondary.opacity(0.2))
        .child(div().h_full().w(relative(clamped as f32)).bg(fill))
}

fn stat_element(item: StatusBarItem, parts: StatParts, theme: &Theme) -> AnyElement {
    let p = theme.palette;
    let anchor = AnchorBounds::default();
    let mut stat = div()
        .id(SharedString::from(format!("status-{}", item.id())))
        .relative()
        .flex()
        .items_center()
        .gap(px(6.))
        .tooltip(Tooltip::anchored(
            parts.tooltip,
            anchor.clone(),
            Placement::Top,
        ));
    stat = if parts.shrink {
        stat.flex_shrink(1.).min_w(px(24.)).overflow_hidden()
    } else {
        stat.flex_none()
    };
    if let Some(text) = parts.label {
        stat = stat.child(status_text(text, p.text.secondary));
    }
    if let Some(fraction) = parts.meter {
        stat = stat.child(meter(fraction, theme));
    }
    stat = match parts.value {
        StatValue::Ink(text) => stat.child(status_text(text, p.sidebar_ink)),
        StatValue::Muted(text) => stat.child(status_text(text, p.text.secondary)),
        StatValue::Traffic(down, up) => stat.child(
            div()
                .flex()
                .items_center()
                .child(status_text("↓", p.text.secondary))
                .child(status_text(format!(" {down} "), p.sidebar_ink))
                .child(div().ml(px(6.)).child(status_text("↑", p.text.secondary)))
                .child(status_text(format!(" {up}"), p.sidebar_ink)),
        ),
    };
    stat.child(bounds_probe(&anchor)).into_any_element()
}

impl AppShell {
    pub fn render_status_bar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let source = tab_source(self.tabs.active_tab());
        let items = self.prefs.status_bar_items.clone();
        self.poll_host_stats(source.clone().ok().filter(|_| !items.is_empty()), cx);
        let content: AnyElement = match (&source, self.host_stats.current()) {
            (Err(reason), _) => muted(*reason, &theme),
            (Ok(_), _) if items.is_empty() => muted("No statistics selected", &theme),
            (Ok(_), None | Some(HostStatsState::Loading)) => {
                muted("Reading host statistics…", &theme)
            }
            (Ok(_), Some(HostStatsState::Unsupported)) => with_tooltip(
                "status-unsupported",
                muted("This host does not report statistics", &theme),
                "Muxus reads statistics with a short sh script over the SSH connection. This host did not run it; network devices and Windows servers have no POSIX shell.",
            ),
            (Ok(_), Some(HostStatsState::Error(message))) => with_tooltip(
                "status-error",
                muted("Statistics unavailable", &theme),
                message.clone(),
            ),
            (Ok(_), Some(HostStatsState::Ok { view, stale })) => div()
                .flex()
                .items_center()
                .gap(px(16.))
                .min_w_0()
                .opacity(if *stale { 0.55 } else { 1. })
                .children(items.iter().filter_map(|item| {
                    stat_parts(*item, view).map(|parts| stat_element(*item, parts, &theme))
                }))
                .into_any_element(),
        };
        div()
            .id("status-bar")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(layout::STATUS_BAR_HEIGHT))
            .pl(px(12.))
            .pr(px(4.))
            .bg(p.sidebar)
            .border_t_1()
            .border_color(p.divider)
            .ui_font(12., 450.)
            .line_height(px(18.))
            .whitespace_nowrap()
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.menu = Some(ShellMenu::StatusBar {
                        position: event.position,
                    });
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .overflow_hidden()
                    .child(content),
            )
            .child(
                IconButton::new("status-bar-items", "TuneOutlined")
                    .icon_size(16.)
                    .padding(2.)
                    .ink(p.text.secondary)
                    .tooltip("Status bar items")
                    .tooltip_placement(Placement::Top)
                    .track_bounds(&self.anchors.status_items)
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        // MUI anchors this menu at the button's top-left corner.
                        let position = this
                            .anchors
                            .status_items
                            .get()
                            .map(|bounds| bounds.origin)
                            .unwrap_or_else(|| event.mouse_position().unwrap_or_default());
                        this.menu = Some(ShellMenu::StatusBar { position });
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// Poll `source` while the bar shows it; `None` stops polling.
    fn poll_host_stats(&mut self, source: Option<StatsSource>, cx: &mut Context<Self>) {
        let key = source.as_ref().map(StatsSource::key);
        if key == self.host_stats.key {
            return;
        }
        self.host_stats.key = key.clone();
        self.host_stats.task = None;
        let (Some(source), Some(key)) = (source, key) else {
            return;
        };
        let entry = self
            .host_stats
            .cache
            .entry(key.clone())
            .or_insert_with(|| StatsEntry {
                state: HostStatsState::Loading,
                latest: None,
            });
        if matches!(entry.state, HostStatsState::Unsupported) {
            return;
        }
        // The demo source has no sampler: the bar keeps reading.
        let Some(backend) = self.data.backend() else {
            return;
        };
        self.host_stats.task = Some(cx.spawn(async move |this, cx| {
            loop {
                // A hidden window stops asking; showing it again starts over.
                let visible = this
                    .update(cx, |this, _| {
                        if !this.window_visible {
                            this.host_stats.key = None;
                        }
                        this.window_visible
                    })
                    .unwrap_or(false);
                if !visible {
                    return;
                }
                let response = match &source {
                    StatsSource::Local => backend.local_host_stats().await,
                    StatsSource::Ssh(conn_id) => backend.ssh_host_stats(conn_id).await,
                };
                let Ok(Some(delay)) = this.update(cx, |this, cx| {
                    let delay = this.host_stats.store(&key, response);
                    cx.notify();
                    delay
                }) else {
                    return;
                };
                cx.background_executor().timer(delay).await;
            }
        }));
    }

    pub fn status_bar_menu(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let mut paper = menu::paper(&theme);
        for item in STATUS_BAR_ITEMS {
            let shown = self.prefs.status_bar_items.contains(&item);
            paper = paper.child(
                MenuItem::new(item.label(), item.label())
                    .dense()
                    .icon(if shown {
                        "CheckBox"
                    } else {
                        "CheckBoxOutlineBlank"
                    })
                    .icon_color(if shown {
                        theme.palette.primary.main
                    } else {
                        theme.palette.action.active
                    })
                    // Stays open, so several items can be switched in one go.
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let items = &mut this.prefs.status_bar_items;
                        if items.contains(&item) {
                            items.retain(|candidate| *candidate != item);
                        } else {
                            // Keep the canonical order.
                            items.push(item);
                            items.sort_by_key(|candidate| {
                                STATUS_BAR_ITEMS.iter().position(|i| i == candidate)
                            });
                        }
                        this.prefs_changed(cx);
                    })),
            );
        }
        paper.child(menu_divider(&theme)).child(
            MenuItem::new("hide-status-bar", "Hide status bar")
                .dense()
                .icon("VisibilityOffOutlined")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.menu = None;
                    this.prefs.show_status_bar = false;
                    this.prefs_changed(cx);
                    cx.notify();
                })),
        )
    }
}

fn muted(text: impl Into<SharedString>, theme: &Theme) -> AnyElement {
    div()
        .min_w_0()
        .overflow_hidden()
        .child(status_text(text, theme.palette.text.secondary))
        .into_any_element()
}

fn with_tooltip(
    id: &'static str,
    content: AnyElement,
    tooltip: impl Into<SharedString>,
) -> AnyElement {
    let anchor = AnchorBounds::default();
    div()
        .id(id)
        .relative()
        .min_w_0()
        .overflow_hidden()
        .tooltip(Tooltip::anchored(tooltip, anchor.clone(), Placement::Top))
        .child(content)
        .child(bounds_probe(&anchor))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::api::{CpuTime, HostStatsSample, MemoryStats, NetworkStats};

    fn sample(idle: f64, total: f64, received: u64, uptime: f64) -> HostStatsResponse {
        HostStatsResponse::Ok {
            sampled_at: 0,
            sample: HostStatsSample {
                hostname: Some("web-01".into()),
                uptime_seconds: Some(uptime),
                cpu_time: Some(CpuTime { total, idle }),
                memory: Some(MemoryStats {
                    total_bytes: 8 << 30,
                    available_bytes: 6 << 30,
                    swap_total_bytes: None,
                    swap_free_bytes: None,
                }),
                network: Some(NetworkStats {
                    interface: "eth0".into(),
                    received_bytes: received,
                    sent_bytes: 0,
                }),
                ..Default::default()
            },
        }
    }

    #[test]
    fn polls_faster_until_rates_exist() {
        let mut poller = HostStatsPoller::default();
        assert_eq!(
            poller.store("local", Ok(sample(50., 100., 0, 10.))),
            Some(FIRST_RATE)
        );
        assert_eq!(
            poller.store("local", Ok(sample(80., 200., 2048, 12.))),
            Some(POLL)
        );
        poller.key = Some("local".into());
        let Some(HostStatsState::Ok { view, stale }) = poller.current() else {
            panic!("no reading");
        };
        assert!(!stale);
        assert_eq!(view.cpu, Some(0.7));
        assert_eq!(view.receive_rate, Some(1024.));
        // A gap of more than 30 s gives no rates.
        assert_eq!(
            poller.store("local", Ok(sample(90., 300., 4096, 50.))),
            Some(FIRST_RATE)
        );
    }

    #[test]
    fn keeps_the_last_reading_on_failure_and_stops_when_unsupported() {
        let mut poller = HostStatsPoller {
            key: Some("ssh:c1".into()),
            ..Default::default()
        };
        assert_eq!(
            poller.store("ssh:c1", Err(ApiError::internal("boom"))),
            Some(RETRY)
        );
        assert!(matches!(poller.current(), Some(HostStatsState::Error(_))));
        poller.store("ssh:c1", Ok(sample(1., 2., 0, 1.)));
        poller.store("ssh:c1", Err(ApiError::internal("boom")));
        assert!(matches!(
            poller.current(),
            Some(HostStatsState::Ok { stale: true, .. })
        ));
        assert_eq!(
            poller.store(
                "ssh:c1",
                Ok(HostStatsResponse::Unsupported {
                    message: "no shell".into()
                })
            ),
            None
        );
        assert!(matches!(
            poller.current(),
            Some(HostStatsState::Unsupported)
        ));
    }

    #[test]
    fn describes_items_like_the_client() {
        let HostStatsResponse::Ok { sample, .. } = sample(50., 100., 0, 3_700.) else {
            unreachable!()
        };
        let view = host_stats_view(&timed_sample(sample, 0), None);
        let memory = stat_parts(StatusBarItem::Memory, &view).unwrap();
        assert!(matches!(memory.value, StatValue::Ink(ref text) if text == "2 GB / 8 GB"));
        assert_eq!(memory.tooltip, "Memory 2 GB used of 8 GB, 6 GB available");
        let cpu = stat_parts(StatusBarItem::Cpu, &view).unwrap();
        assert!(matches!(cpu.value, StatValue::Ink(ref text) if text == "…"));
        assert_eq!(cpu.tooltip, "Measuring CPU usage…");
        let up = stat_parts(StatusBarItem::Uptime, &view).unwrap();
        assert!(matches!(up.value, StatValue::Ink(ref text) if text == "1h 1m"));
        assert!(stat_parts(StatusBarItem::Disk, &view).is_none());
        assert!(stat_parts(StatusBarItem::Users, &view).is_none());
    }
}
