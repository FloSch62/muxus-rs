//! The gNOI and gNSI areas of a gNMI session (`tools/ToolsArea.tsx`,
//! `tools/common.tsx`): the tool list beside the open tool, and the
//! dispatch from fields, selects and dialogs to the tool they belong to.

mod acctz;
mod authz;
mod certz;
mod credentialz;
mod files;
pub mod kit;
mod pathz;
mod ping;
mod rotation;
mod system;

use muxus_mgmt::workbench::store::WorkbenchArea;
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::tooltip::{AnchorBounds, Placement, Tooltip, bounds_probe};

use super::fields::FieldKey;
use super::view::ManagementView;
use super::widgets::MONO;
use muxus_ui::widgets::button::{Button, ButtonColor};
use muxus_ui::widgets::display::Chip;

pub use kit::ToolsUi;

/// What a tool asked to confirm, or the typed confirmation it opened.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolAction {
    Reboot,
    KillProcess,
    ClearBgp,
    DeleteCertzProfile(String),
    DeleteFile { path: String, name: String },
}

/// One tool in the list.
pub struct ToolEntry {
    pub id: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub icon: &'static str,
    /// Any of these services makes the tool usable.
    pub services: &'static [&'static str],
    pub danger: bool,
}

pub const OPERATIONS: &[ToolEntry] = &[
    ToolEntry {
        id: "ping",
        label: "Ping",
        hint: "From the device, with live round-trip times",
        icon: "NetworkPing",
        services: &["gnoi.system.System"],
        danger: false,
    },
    ToolEntry {
        id: "traceroute",
        label: "Traceroute",
        hint: "The path from the device, hop by hop",
        icon: "RouteOutlined",
        services: &["gnoi.system.System"],
        danger: false,
    },
    ToolEntry {
        id: "files",
        label: "Files",
        hint: "Browse, preview, upload and download",
        icon: "FolderOpenOutlined",
        services: &["gnoi.file.File"],
        danger: false,
    },
    ToolEntry {
        id: "health",
        label: "Health",
        hint: "Component health and its evidence",
        icon: "HealthAndSafetyOutlined",
        services: &["gnoi.healthz.Healthz"],
        danger: false,
    },
    ToolEntry {
        id: "process",
        label: "Processes",
        hint: "Restart or stop a daemon",
        icon: "MemoryOutlined",
        services: &["gnoi.system.System"],
        danger: false,
    },
    ToolEntry {
        id: "bgp",
        label: "BGP",
        hint: "Reset a neighbor, hard or soft",
        icon: "HubOutlined",
        services: &["gnoi.bgp.BGP"],
        danger: false,
    },
    ToolEntry {
        id: "system",
        label: "System",
        hint: "Clock and software image",
        icon: "AccessTime",
        services: &["gnoi.system.System", "gnoi.os.OS"],
        danger: false,
    },
    ToolEntry {
        id: "reboot",
        label: "Reboot",
        hint: "Restart now or later, and watch it",
        icon: "RestartAlt",
        services: &["gnoi.system.System"],
        danger: true,
    },
];

pub const SECURITY: &[ToolEntry] = &[
    ToolEntry {
        id: "authz",
        label: "Authorization",
        hint: "Who may call which RPC",
        icon: "AdminPanelSettingsOutlined",
        services: &["gnsi.authz.v1.Authz"],
        danger: false,
    },
    ToolEntry {
        id: "pathz",
        label: "Path authorization",
        hint: "Who may read or write which path",
        icon: "RuleOutlined",
        services: &["gnsi.pathz.v1.Pathz"],
        danger: false,
    },
    ToolEntry {
        id: "certz",
        label: "Certificates",
        hint: "TLS profiles, certificates and trust",
        icon: "WorkspacePremiumOutlined",
        services: &["gnsi.certz.v1.Certz"],
        danger: false,
    },
    ToolEntry {
        id: "credentialz",
        label: "SSH credentials",
        hint: "Host keys and authorized keys",
        icon: "KeyOutlined",
        services: &["gnsi.credentialz.v1.Credentialz"],
        danger: false,
    },
    ToolEntry {
        id: "acctz",
        label: "Accounting",
        hint: "Who did what, as it happens",
        icon: "ReceiptLongOutlined",
        services: &["gnsi.acctz.v1.AcctzStream", "gnsi.acctz.v1.Acctz"],
        danger: false,
    },
];

/// `ToolHeader`: icon tile, title, service chip, what the tool does, and its actions.
pub fn tool_header(
    icon_name: &str,
    title: &str,
    service: &str,
    description: &str,
    actions: Option<AnyElement>,
    theme: &Theme,
) -> Div {
    let p = &theme.palette;
    let dark = theme.mode.is_dark();
    let mut header = div()
        .flex()
        .items_start()
        .gap(px(12.))
        .mb(px(16.))
        .child(
            div()
                .size(px(36.))
                .flex_none()
                .rounded(px(theme.radius * 2.))
                .flex()
                .items_center()
                .justify_center()
                .bg(alpha(p.primary.main, if dark { 0.14 } else { 0.1 }))
                .child(icon(icon_name, SMALL, p.primary.main)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            label(title.to_owned())
                                .size(theme.typography.subtitle1.size)
                                .weight(theme.typography.subtitle1.weight)
                                .line_height(theme.typography.subtitle1.size * 1.3)
                                .color(p.text.primary)
                                .no_truncate(),
                        )
                        .child(
                            Chip::new(service.to_owned())
                                .outlined()
                                .height(20.)
                                .font_size(10.5)
                                .mono(),
                        ),
                )
                .child(
                    div().mt(px(2.)).child(
                        paragraph(description.to_owned())
                            .size(theme.typography.body2.size)
                            .line_height(theme.typography.body2.line_height)
                            .color(p.text.secondary),
                    ),
                ),
        );
    if let Some(actions) = actions {
        header = header.child(actions);
    }
    header
}

/// `Section`: a bordered card with a title row.
pub fn section(
    title: &str,
    actions: Option<AnyElement>,
    body: impl IntoElement,
    theme: &Theme,
) -> Div {
    let p = &theme.palette;
    let subtitle2 = theme.typography.subtitle2;
    let mut head = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(8.))
        .gap_y(px(4.))
        .px(px(14.))
        .py(px(4.))
        .min_h(px(40.))
        .border_b_1()
        .border_color(p.divider)
        .child(
            div().flex_grow(1.).flex_shrink_0().child(
                label(title.to_owned())
                    .size(subtitle2.size)
                    .weight(subtitle2.weight)
                    .line_height(subtitle2.line_height)
                    .color(p.text.primary)
                    .no_truncate(),
            ),
        );
    if let Some(actions) = actions {
        head = head.child(actions);
    }
    div()
        .border_1()
        .border_color(p.divider)
        .rounded(px(theme.radius * 2.))
        .bg(p.background_paper)
        .mb(px(16.))
        .child(head)
        .child(div().p(px(14.)).child(body))
}

/// `Stat`: a big value with a small label.
pub fn stat(
    label_text: &str,
    value: impl Into<SharedString>,
    tone: Option<muxus_ui::gpui_kit::Hsla>,
    theme: &Theme,
) -> Div {
    let p = &theme.palette;
    div()
        .min_w(px(88.))
        .child(super::widgets::caps(
            label_text.to_owned(),
            10.5,
            600.,
            p.text.secondary,
        ))
        .child(
            div()
                .font_family(MONO)
                .text_size(px(18.))
                .font_weight(FontWeight(600.))
                .line_height(px(18. * 1.3))
                .text_color(tone.unwrap_or(p.text.primary))
                .child(value.into()),
        )
}

/// A wrapping row of fields and buttons (`Stack direction="row" useFlexGap flexWrap`).
pub fn field_row(gap: f32) -> Div {
    div().flex().flex_wrap().items_center().gap(px(gap))
}

/// `<Box sx={{ flex: 1 }} />`.
pub fn spacer() -> Div {
    div().flex_1()
}

/// A contained button in a palette color.
pub fn contained(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    color: ButtonColor,
) -> Button {
    Button::new(id, text).contained().color(color)
}

impl ManagementView {
    pub(super) fn render_tools_area(
        &mut self,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let (area, selected) = self.store.read(|state| {
            (
                state.area,
                match state.area {
                    WorkbenchArea::Operations => state.tool.operations.clone(),
                    _ => state.tool.security.clone(),
                },
            )
        });
        let tools = if area == WorkbenchArea::Operations {
            OPERATIONS
        } else {
            SECURITY
        };
        let current = tools
            .iter()
            .find(|tool| tool.id == selected)
            .unwrap_or(&tools[0]);
        let connected = self.connected();
        let mut nav = div().bg(p.sidebar).border_color(p.divider);
        nav = if narrow {
            nav.flex()
                .flex_none()
                .overflow_x_hidden()
                .border_b_1()
                .px(px(4.))
                .py(px(4.))
                .gap(px(2.))
        } else {
            nav.w(px(232.))
                .flex_none()
                .border_r_1()
                .py(px(8.))
                .px(px(6.))
                .flex()
                .flex_col()
        };
        if !narrow {
            nav = nav.child(div().px(px(8.)).pb(px(6.)).child(super::widgets::caps(
                if area == WorkbenchArea::Operations {
                    "gNOI"
                } else {
                    "gNSI"
                },
                10.5,
                650.,
                p.text.secondary,
            )));
        }
        for tool in tools {
            let available = tool
                .services
                .iter()
                .any(|service| self.controller.has_service(service));
            let is_selected = tool.id == current.id;
            let ink = if available {
                p.text.primary
            } else {
                p.text.disabled
            };
            let icon_color = if is_selected {
                p.primary.main
            } else if tool.danger && available {
                p.error.main
            } else {
                p.text.secondary
            };
            let hover = p.action.hover;
            let id = tool.id;
            let mut text = div().min_w_0().flex().flex_col().child(
                label(tool.label)
                    .size(13.)
                    .weight(if is_selected { 650. } else { 500. })
                    .line_height(13. * 1.3)
                    .color(ink),
            );
            if !narrow {
                text = text.child(
                    label(if available {
                        tool.hint
                    } else {
                        "Not offered by this device"
                    })
                    .size(11.)
                    .line_height(11. * 1.3)
                    .color(p.text.secondary),
                );
            }
            let mut button = div()
                .id(SharedString::from(format!("tool-{id}")))
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(10.))
                .px(px(8.))
                .py(px(if narrow { 4. } else { 6. }))
                .rounded(px(theme.radius * 1.5))
                .cursor_pointer()
                .child(icon(tool.icon, 18., icon_color))
                .child(text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.store.update(|state| state.set_tool(area, id));
                    cx.notify();
                }));
            if !narrow {
                button = button.mb(px(2.)).w_full();
            }
            button = if is_selected {
                button.bg(alpha(
                    p.primary.main,
                    if theme.mode.is_dark() { 0.18 } else { 0.1 },
                ))
            } else {
                button.hover(move |s| s.bg(hover))
            };
            if narrow && !available {
                let anchor = AnchorBounds::default();
                button = button
                    .child(bounds_probe(&anchor))
                    .tooltip(Tooltip::anchored(
                        "Not offered by this device",
                        anchor,
                        Placement::Bottom,
                    ));
            }
            nav = nav.child(button);
        }
        let mut body = div()
            .max_w(px(1080.))
            .w_full()
            .mx_auto()
            .px(px(if narrow { 12. } else { 24. }))
            .py(px(if narrow { 12. } else { 20. }))
            .flex()
            .flex_col();
        if !connected {
            body = body.child(
                div().mb(px(16.)).child(
                    label("Connect to use the tools.")
                        .size(theme.typography.body2.size)
                        .line_height(theme.typography.body2.line_height)
                        .color(p.text.secondary),
                ),
            );
        }
        self.mount_tool(current.id, cx);
        body = body.child(self.render_tool(current.id, window, cx));
        let mut area_element = div().flex_1().min_h_0().flex();
        if narrow {
            area_element = area_element.flex_col();
        }
        area_element.child(nav).child(
            div()
                .id("tool-content")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_y_scroll()
                .child(body),
        )
    }

    fn render_tool(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match id {
            "ping" => self.render_ping(window, cx),
            "traceroute" => self.render_traceroute(window, cx),
            "files" => self.render_files(window, cx),
            "health" => self.render_health(window, cx),
            "process" => self.render_process(window, cx),
            "bgp" => self.render_bgp(window, cx),
            "system" => self.render_system(window, cx),
            "reboot" => self.render_reboot(window, cx),
            "authz" => self.render_authz(window, cx),
            "pathz" => self.render_pathz(window, cx),
            "certz" => self.render_certz(window, cx),
            "credentialz" => self.render_credentialz(window, cx),
            _ => self.render_acctz(window, cx),
        }
    }

    /// React mounts the open tool and runs its effects on mount and when
    /// the connection changes; this does the same the first time a tool
    /// renders for a session.
    fn mount_tool(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(id) = OPERATIONS
            .iter()
            .chain(SECURITY)
            .map(|tool| tool.id)
            .find(|tool| *tool == id)
        else {
            return;
        };
        let connected = self.grpc().is_some();
        let key = (id, self.session_generation(), connected);
        if self.ui.tools.mounted == Some(key) {
            return;
        }
        if self.ui.tools.mounted.map(|(tool, _, _)| tool) != Some(id) {
            self.ui.tools.local = kit::ToolLocal::default();
        }
        self.ui.tools.mounted = Some(key);
        if !connected {
            return;
        }
        match id {
            "reboot" => self.refresh_reboot(cx),
            "process" => {
                let state: system::ProcessState = self.tool("process");
                if state.applications.is_none() {
                    self.load_applications(cx);
                }
            }
            "system" => {
                let state: system::SystemState = self.tool("system");
                if state.local_at.is_none_or(|at| at == 0) {
                    self.refresh_system(cx);
                }
            }
            "authz" => {
                let state: authz::AuthzState = self.tool("authz");
                if state.policy.is_none() && !state.loading {
                    self.load_authz(cx);
                }
            }
            "pathz" | "certz" | "credentialz" => {
                if !self.ui.tools.local.loaded {
                    self.ui.tools.local.loaded = true;
                    match id {
                        "pathz" => self.load_pathz(cx),
                        "certz" => self.load_certz(cx),
                        _ => self.load_credentialz(cx),
                    }
                }
            }
            "files" => self.files_mounted(cx),
            _ => {}
        }
    }

    /// Once a second while tools hold state: keep a pending reboot's countdown current.
    pub(super) fn tools_tick(&mut self, cx: &mut Context<Self>) {
        if self.ui.tools.mounted.map(|(tool, _, _)| tool) != Some("reboot") || self.grpc().is_none()
        {
            return;
        }
        let state: system::RebootState = self.tool("reboot");
        let due = self
            .ui
            .tools
            .reboot_refreshed_at
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(5));
        if state.active() && due {
            self.refresh_reboot(cx);
        }
    }

    /// Enter in a tool's field.
    pub(super) fn on_tool_enter(
        &mut self,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match name {
            "ping.destination" => self.start_ping(cx),
            "traceroute.destination" => self.start_traceroute(cx),
            "pathz.path" => self.probe_pathz(cx),
            "files.path" => self.files_go(cx),
            "confirm.typed" => self.typed_confirmed(window, cx),
            _ => {}
        }
    }

    /// A tool's field changed.
    pub(super) fn on_tool_field(
        &mut self,
        name: &'static str,
        value: String,
        cx: &mut Context<Self>,
    ) {
        match name.split('.').next().unwrap_or_default() {
            "ping" => self.ping_field(name, value),
            "traceroute" => self.traceroute_field(name, value),
            "health" | "reboot" | "process" | "bgp" => self.system_field(name, value),
            "authz" => self.authz_field(name, value),
            "pathz" => self.pathz_field(name, value),
            "credentialz" => self.credentialz_field(name, value),
            "acctz" => self.acctz_field(name, value),
            "files" => self.files_field(name, value),
            "confirm" => self.ui.tools.local.typed = value,
            _ => {}
        }
        cx.notify();
    }

    /// A field of a list row changed.
    pub(super) fn on_tool_row_field(
        &mut self,
        name: &'static str,
        index: usize,
        value: String,
        cx: &mut Context<Self>,
    ) {
        if name.starts_with("pathz.") {
            self.pathz_row_field(name, index, value);
        }
        cx.notify();
    }

    /// The options of a tool's select.
    pub(super) fn tool_select_options(
        &self,
        field: &super::ui::SelectField,
    ) -> Vec<(String, Option<String>)> {
        match field {
            super::ui::SelectField::Tool(name) => system::select_options(name).unwrap_or_default(),
            super::ui::SelectField::ToolRow(name, _) => {
                pathz::row_select_options(name).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// An option of a tool's select was chosen.
    pub(super) fn on_tool_select(&mut self, field: &super::ui::SelectField, index: usize) {
        match field {
            super::ui::SelectField::Tool(name) => self.system_select(name, index),
            super::ui::SelectField::ToolRow(name, row) => self.pathz_row_select(name, *row, index),
            _ => {}
        }
    }

    /// A tool's confirmation was accepted.
    pub(super) fn tool_confirmed(&mut self, action: ToolAction, cx: &mut Context<Self>) {
        match action {
            ToolAction::Reboot => self.reboot(cx),
            ToolAction::KillProcess => self.kill_process(cx),
            ToolAction::ClearBgp => self.clear_bgp(cx),
            ToolAction::DeleteCertzProfile(id) => self.delete_certz_profile(id, cx),
            ToolAction::DeleteFile { path, name } => self.remove_file(path, name, cx),
        }
    }

    /// The review of a rotation was applied.
    pub(super) fn tool_review_applied(&mut self, key: &'static str, cx: &mut Context<Self>) {
        match key {
            "authz" => self.rotate_authz(cx),
            "pathz" => self.rotate_pathz(cx),
            _ => {}
        }
    }

    /// Enter in the typed confirmation, or its button.
    pub(super) fn typed_confirmed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(super::ui::Dialog::TypedConfirm {
            expected, action, ..
        }) = self.ui.dialog.clone()
        else {
            return;
        };
        if !kit::typed_matches(&self.ui.tools.local.typed, &expected) {
            return;
        }
        self.ui.dialog = None;
        self.tool_confirmed(action, cx);
        cx.notify();
    }

    /// Whether `key` is a tool field with path completion (the pathz probe).
    pub(super) fn is_tool_path_field(key: &FieldKey) -> bool {
        matches!(key, FieldKey::Tool("pathz.path"))
    }
}
