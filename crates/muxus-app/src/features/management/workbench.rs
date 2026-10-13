//! The workbench frame (`Workbench.tsx` and the waiting screens of
//! `ManagementViewImpl.tsx`): the session header with the area switch and
//! the device chips, the connection banner, the navigator beside the work
//! area, and the divider between request and result.

use std::rc::Rc;

use muxus_core::management::{GnmiTlsMode, ManagementSessionInfo, NetconfBase};
use muxus_core::protocol::ManagementProfile;
use muxus_mgmt::workbench::store::{NavigatorTab, RunStatus, WorkbenchArea};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    App, BoxShadow, ClickEvent, Context, Div, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, canvas, div, point, px, relative,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::button::elevation;
use muxus_ui::widgets::tooltip::{AnchorBounds, Placement, Tooltip, bounds_probe};
use muxus_ui::widgets::{Button, ButtonColor, IconButton};

use super::logic::device::{
    AREAS, TlsTone, base_label, base_tooltip, encodings_tooltip, netconf_module_chip, tls_label,
    tls_tone, tls_tooltip,
};
use super::ui::{Drag, NAVIGATOR_DEFAULT_WIDTH, clamp_navigator};
use super::view::{ManagementView, SessionPhase};
use super::widgets::MONO;
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::tooltip_lines::LinesTooltip;

fn protocol_icon(profile: &ManagementProfile) -> &'static str {
    match profile {
        ManagementProfile::Gnmi(_) => "SensorsOutlined",
        ManagementProfile::Netconf(_) => "AccountTreeOutlined",
    }
}

fn protocol_name(profile: &ManagementProfile) -> &'static str {
    match profile {
        ManagementProfile::Gnmi(_) => "gNMI",
        ManagementProfile::Netconf(_) => "NETCONF",
    }
}

fn address_of(profile: &ManagementProfile) -> String {
    match profile {
        ManagementProfile::Gnmi(profile) => format!("{}:{}", profile.host, profile.port),
        ManagementProfile::Netconf(profile) => format!("{}:{}", profile.host, profile.port),
    }
}

/// Typography `subtitle1`.
fn subtitle1(text: impl Into<SharedString>, theme: &Theme) -> impl IntoElement {
    let variant = theme.typography.subtitle1;
    label(text)
        .size(variant.size)
        .weight(variant.weight)
        .line_height(variant.line_height)
        .color(theme.palette.text.primary)
        .no_truncate()
}

impl ManagementView {
    /// Before the first session is up: connecting, idle or ended.
    pub(super) fn render_connect_screen(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = *cx.theme();
        let p = &theme.palette;
        let body2 = theme.typography.body2;
        let heading = format!("{} · {}", protocol_name(&self.profile), self.title);
        let address = address_of(&self.profile);
        let protocol_icon = protocol_icon(&self.profile);
        let mut stack = div()
            .absolute()
            .inset_0()
            .px(px(24.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(16.));
        if self.phase == SessionPhase::Connecting {
            let status = self
                .status_text
                .as_deref()
                .and_then(|text| text.split('\n').next())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Connecting to {address} …"));
            stack = stack
                .child(
                    div()
                        .relative()
                        .size(px(56.))
                        .child(CircularProgress::new(
                            ("workbench-progress", 1usize),
                            56.,
                            p.primary.main,
                        ))
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(protocol_icon, 24., p.primary.main)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(4.))
                        .child(subtitle1(heading, &theme))
                        .child(
                            div()
                                .max_w(px(520.))
                                .text_center()
                                .text_size(px(body2.size))
                                .line_height(px(body2.line_height))
                                .text_color(p.text.secondary)
                                .child(status),
                        ),
                )
                .child(
                    Button::new("cancel-connect", "Cancel")
                        .color(ButtonColor::Inherit)
                        .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                );
        } else {
            let ended = self.phase == SessionPhase::Ended;
            let message = if self.phase == SessionPhase::Idle {
                address
            } else {
                self.end_message
                    .clone()
                    .unwrap_or_else(|| "Disconnected.".into())
            };
            let action = if self.phase == SessionPhase::Idle {
                "Connect"
            } else if self.redial.is_some() {
                "Reconnect now"
            } else {
                "Reconnect"
            };
            let mut text = div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.))
                .child(subtitle1(heading, &theme))
                .child(
                    div()
                        .max_w(px(560.))
                        .text_center()
                        .text_size(px(body2.size))
                        .line_height(px(body2.line_height))
                        .text_color(p.text.secondary)
                        .child(message),
                );
            if let Some(redial) = self.redial_text() {
                let caption = theme.typography.caption;
                text = text.child(
                    label(redial)
                        .size(caption.size)
                        .line_height(caption.line_height)
                        .color(p.text.secondary)
                        .no_truncate(),
                );
            }
            stack = stack
                .child(
                    div()
                        .size(px(64.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(p.action.hover)
                        .child(icon(
                            if ended {
                                "LinkOffOutlined"
                            } else {
                                protocol_icon
                            },
                            30.,
                            if ended {
                                p.error.main
                            } else {
                                p.text.secondary
                            },
                        )),
                )
                .child(text)
                .child(
                    Button::new("reconnect-screen", action)
                        .contained()
                        .start_icon("Refresh")
                        .on_click(cx.listener(|this, _, window, cx| this.reconnect(window, cx))),
                );
        }
        stack
    }

    /// The workbench once a session has been up.
    pub(super) fn render_workbench(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = *cx.theme();
        let narrow = self.ui.narrow();
        // Narrow panes start with the navigator tucked away.
        if narrow && !self.ui.was_narrow {
            self.store.update(|state| state.navigator_open = false);
        }
        self.ui.was_narrow = narrow;
        if self.connected() {
            self.load_explorer_root();
        }
        let measured = self.ui.width.clone();
        let header = self.render_header(narrow, window, cx);
        let banner = if self.phase != SessionPhase::Connected {
            Some(self.render_banner(cx))
        } else {
            None
        };
        let area = self.store.read(|state| state.area);
        let tools = self.is_gnmi() && area != WorkbenchArea::Data;
        let mut root = div()
            .id("workbench")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .bg(theme.palette.background_default)
            .on_key_down(
                cx.listener(|this, event: &muxus_ui::gpui_kit::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        let running = this.store.read(|state| {
                            state
                                .runs
                                .iter()
                                .find(|run| run.status == RunStatus::Running && !run.hidden)
                                .map(|run| run.id.clone())
                        });
                        if let Some(id) = running {
                            this.controller.cancel(&id);
                            cx.stop_propagation();
                        }
                    }
                }),
            )
            .on_mouse_move(cx.listener(Self::on_drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_drag_end))
            .child(
                canvas(
                    move |bounds, _, _| measured.set(muxus_ui::zoom::unzoom(bounds.size.width)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(header)
            .children(banner);
        if tools {
            root = root.child(self.render_tools_area(narrow, window, cx));
        } else {
            root = root.child(self.render_body(narrow, window, cx));
        }
        root.relative()
    }

    fn render_header(&mut self, narrow: bool, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let (navigator_open, area) = self.store.read(|state| (state.navigator_open, state.area));
        let data_area = !self.is_gnmi() || area == WorkbenchArea::Data;
        let profile = self.dialed_profile().clone();
        let info = self.controller.info.clone();
        let status_color = match self.phase {
            SessionPhase::Connected => p.success.main,
            SessionPhase::Connecting => p.warning.main,
            _ => p.text.disabled,
        };
        let address = match &info {
            Some(ManagementSessionInfo::Gnmi(info)) => (info.address.clone(), info.via.clone()),
            Some(ManagementSessionInfo::Netconf(info)) => (info.address.clone(), info.via.clone()),
            None => (address_of(&profile), None),
        };
        let address = match address.1 {
            Some(via) => format!("{} via {via}", address.0),
            None => address.0,
        };
        let subtitle2 = theme.typography.subtitle2;
        let caption = theme.typography.caption;
        let navigator_label = if navigator_open {
            "Hide the navigator"
        } else {
            "Show the navigator"
        };
        let mut header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .min_h(px(44.))
            .min_w_0()
            .border_b_1()
            .border_color(p.divider)
            .bg(p.background_paper)
            .child(
                IconButton::new("toggle-navigator", "VerticalSplitOutlined")
                    .color(if navigator_open && data_area {
                        ButtonColor::Primary
                    } else {
                        ButtonColor::Default
                    })
                    .disabled(!data_area)
                    .tooltip(if data_area { navigator_label } else { "" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.store
                            .update(|state| state.navigator_open = !navigator_open);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .rounded(px(theme.radius * 1.5))
                    .bg(alpha(p.primary.main, 0.12))
                    .child(icon(protocol_icon(&profile), 17., p.primary.main)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .flex_shrink(1.)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .min_w_0()
                            .child(
                                label(self.title.clone())
                                    .size(subtitle2.size)
                                    .weight(650.)
                                    .line_height(subtitle2.line_height)
                                    .color(p.text.primary),
                            )
                            .child(
                                div()
                                    .size(px(7.))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(status_color),
                            ),
                    )
                    .child(
                        label(address)
                            .family(MONO)
                            .size(11.)
                            .line_height(caption.line_height)
                            .color(p.text.secondary),
                    ),
            );
        if self.is_gnmi() {
            header = header.child(self.render_area_switch(narrow, cx));
        }
        header = header.child(div().flex_1());
        if !narrow {
            match &info {
                Some(ManagementSessionInfo::Gnmi(info)) => {
                    header = header.child(self.render_gnmi_chips(info, cx));
                }
                Some(ManagementSessionInfo::Netconf(info)) => {
                    header = header.child(self.render_netconf_chips(info, cx));
                }
                None => {}
            }
        }
        header
            .child(div().w(px(1.)).flex_none().self_stretch().bg(p.divider))
            .child(
                IconButton::new("reconnect", "Refresh")
                    .tooltip("Reconnect")
                    .on_click(cx.listener(|this, _, window, cx| this.reconnect(window, cx))),
            )
            .child(
                IconButton::new("disconnect", "LinkOffOutlined")
                    .tooltip("Disconnect")
                    .disabled(self.phase != SessionPhase::Connected)
                    .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
            )
    }

    fn render_area_switch(
        &mut self,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = *cx.theme();
        let p = &theme.palette;
        let area = self.store.read(|state| state.area);
        // A rotation waiting for its finalize rolls back when the session ends; keep it in sight.
        let pending_rotation = self.store.read(|state| {
            state.tools.values().any(|tool| {
                tool.get("rotation")
                    .and_then(|rotation| rotation.get("phase"))
                    .and_then(|phase| phase.as_str())
                    == Some("pending")
            })
        });
        let mut switch = div()
            .flex()
            .flex_none()
            .p(px(3.))
            .gap(px(2.))
            .rounded(px(theme.radius * 2.))
            .bg(p.action.hover);
        for (index, (value, text, hint, icon_name)) in AREAS.iter().enumerate() {
            let target = match index {
                0 => WorkbenchArea::Data,
                1 => WorkbenchArea::Operations,
                _ => WorkbenchArea::Security,
            };
            let selected = area == target;
            let ink = if selected {
                p.text.primary
            } else {
                p.text.secondary
            };
            let hover_ink = p.text.primary;
            let anchor = AnchorBounds::default();
            let tooltip = if narrow {
                format!("{text}: {hint}")
            } else {
                (*hint).to_owned()
            };
            let mut button = div()
                .id(SharedString::from(format!("area-{value}")))
                .relative()
                .flex()
                .items_center()
                .gap(px(4.8))
                .px(px(if narrow { 7.2 } else { 10. }))
                .h(px(26.))
                .rounded(px(theme.radius * 1.5))
                .cursor_pointer()
                .text_color(ink)
                .hover(move |s| s.text_color(hover_ink))
                .child(icon(
                    icon_name,
                    16.,
                    if selected { p.primary.main } else { ink },
                ))
                .child(bounds_probe(&anchor))
                .tooltip(Tooltip::anchored(tooltip, anchor, Placement::Bottom))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.store.update(|state| state.area = target);
                    cx.notify();
                }));
            if !narrow {
                button = button.child(
                    label(*text)
                        .size(12.5)
                        .weight(if selected { 650. } else { 500. })
                        .line_height(18.75)
                        .color(ink)
                        .no_truncate(),
                );
            }
            if selected {
                button = button.bg(p.background_paper).shadow(vec![BoxShadow {
                    color: alpha(p.common_black, 0.18),
                    offset: point(px(0.), px(1.)),
                    blur_radius: px(2.),
                    spread_radius: px(0.),
                    inset: false,
                }]);
            }
            if target == WorkbenchArea::Security && pending_rotation {
                button = button.child(
                    div()
                        .absolute()
                        .top(px(3.))
                        .right(px(3.))
                        .size(px(6.))
                        .rounded_full()
                        .bg(p.warning.main),
                );
            }
            switch = switch.child(button);
        }
        switch
    }

    fn chip(text: impl Into<SharedString>) -> Chip {
        Chip::new(text)
            .outlined()
            .height(22.)
            .font_size(11.5)
            .label_padding(7.2)
            .icon_margin(4.8)
    }

    fn render_gnmi_chips(
        &mut self,
        info: &muxus_core::management::GnmiSessionInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = *cx.theme();
        let p = &theme.palette;
        let tls = &info.tls;
        let tone = tls_tone(tls);
        let mut tls_chip = Self::chip(tls_label(tls))
            .icon(if tls.mode == GnmiTlsMode::Plaintext {
                "NoEncryptionGmailerrorredOutlined"
            } else {
                "HttpsOutlined"
            })
            .icon_size(14.);
        tls_chip = match tone {
            TlsTone::Warning => {
                tls_chip.color(theme.status_text_color(muxus_ui::theme::Status::Warning))
            }
            TlsTone::Success => {
                tls_chip.color(theme.status_text_color(muxus_ui::theme::Status::Success))
            }
            TlsTone::Default => tls_chip,
        };
        let (lines, fingerprint) = tls_tooltip(tls);
        let tls_anchor = AnchorBounds::default();
        let models = Self::chip(format!("{} models", info.models.len())).on_click(
            "models-chip",
            cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                this.store
                    .update(|state| state.set_navigator(NavigatorTab::Device));
                cx.notify();
            }),
        );
        let _ = p;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .child(Self::chip(format!(
                "gNMI {}",
                if info.version.is_empty() {
                    "?"
                } else {
                    &info.version
                }
            )))
            .child(
                Self::chip(format!("{} encodings", info.encodings.len()))
                    .tooltip("encodings-chip", encodings_tooltip(&info.encodings)),
            )
            .child(models)
            .child(
                div()
                    .id("tls-chip")
                    .relative()
                    .flex()
                    .flex_none()
                    .child(tls_chip)
                    .child(bounds_probe(&tls_anchor))
                    .tooltip(LinesTooltip::builder(lines, fingerprint, tls_anchor)),
            )
    }

    fn render_netconf_chips(
        &mut self,
        info: &muxus_core::management::NetconfSessionInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (modules, featured) = netconf_module_chip(&info.capabilities);
        let base: NetconfBase = info.base;
        let mut row = div().flex().flex_none().items_center().gap(px(6.)).child(
            Self::chip(format!("base:{}", base_label(base)))
                .tooltip("base-chip", base_tooltip(base)),
        );
        if !info.session_id.is_empty() {
            row = row.child(Self::chip(format!("session {}", info.session_id)));
        }
        row.child(
            Self::chip(modules)
                .tooltip("modules-chip", featured)
                .on_click(
                    "modules-chip",
                    cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                        this.store
                            .update(|state| state.set_navigator(NavigatorTab::Device));
                        cx.notify();
                    }),
                ),
        )
    }

    fn render_banner(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = *cx.theme();
        let p = &theme.palette;
        let connecting = self.phase == SessionPhase::Connecting;
        let end = self
            .end_message
            .clone()
            .unwrap_or_else(|| "Disconnected.".into());
        let message = if connecting {
            self.status_text
                .as_deref()
                .and_then(|text| text.split('\n').next())
                .map(str::to_owned)
                .unwrap_or_else(|| "Reconnecting …".into())
        } else if let Some(redial) = self.redial {
            format!(
                "{end} Reconnecting in {} s (attempt {} of {}).",
                self.redial_remaining()
                    .unwrap_or_else(|| (redial.delay_ms as f64 / 1000.).round() as u64),
                redial.attempt,
                muxus_session::recovery::AUTO_RECONNECT_DELAYS_MS.len()
            )
        } else {
            format!("{end} Your requests, results and history are kept.")
        };
        let body2 = theme.typography.body2;
        let tint = if connecting {
            p.info.main
        } else {
            p.warning.main
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(4.))
            .min_h(px(36.))
            .border_b_1()
            .border_color(p.divider)
            .bg(alpha(tint, 0.12))
            .child(if connecting {
                CircularProgress::new(("workbench-progress", 2usize), 14., p.primary.main)
                    .into_any_element()
            } else {
                icon("LinkOffOutlined", 17., p.warning.main).into_any_element()
            })
            .child(
                label(message)
                    .size(body2.size)
                    .line_height(body2.line_height)
                    .color(p.text.primary)
                    .grow(),
            )
            .child(if connecting {
                Button::new("banner-cancel", "Cancel")
                    .color(ButtonColor::Inherit)
                    .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx)))
                    .into_any_element()
            } else {
                Button::new(
                    "banner-reconnect",
                    if self.redial.is_some() {
                        "Reconnect now"
                    } else {
                        "Reconnect"
                    },
                )
                .contained()
                .start_icon("Refresh")
                .on_click(cx.listener(|this, _, window, cx| this.reconnect(window, cx)))
                .into_any_element()
            })
    }

    /// The data area: navigator, request editor, divider, result.
    fn render_body(
        &mut self,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = *cx.theme();
        let p = &theme.palette;
        let navigator_open = self.store.read(|state| state.navigator_open);
        let mut body = div().flex_1().min_h_0().flex().relative();
        if navigator_open {
            let width = if narrow {
                self.ui.navigator_width.min(340.)
            } else {
                self.ui.navigator_width
            };
            if narrow {
                body = body.child(
                    div()
                        .id("navigator-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(theme.components.scrim)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.store.update(|state| state.navigator_open = false);
                            cx.notify();
                        })),
                );
            }
            let mut panel = div()
                .w(px(width))
                .flex_none()
                .border_r_1()
                .border_color(p.divider)
                .bg(p.sidebar)
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.render_navigator(window, cx));
            panel = if narrow {
                panel
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .shadow(elevation(8, &theme))
            } else {
                panel.relative().child(
                    div()
                        .id("navigator-grip")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(px(-5.))
                        .w(px(8.))
                        .cursor_col_resize()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.ui.drag = Some(Drag::Navigator {
                                    start_x: muxus_ui::zoom::unzoom(event.position.x),
                                    start_width: this.ui.navigator_width,
                                });
                                cx.stop_propagation();
                            }),
                        )
                        .on_click(cx.listener(
                            |this, event: &muxus_ui::gpui_kit::ClickEvent, _, cx| {
                                if event.click_count() == 2 {
                                    this.ui.navigator_width = NAVIGATOR_DEFAULT_WIDTH;
                                    cx.notify();
                                }
                            },
                        )),
                )
            };
            body = body.child(panel);
        }
        let main_anchor = self.ui.main.clone();
        let share = self.ui.request_share;
        let mut request = div()
            .min_h(px(120.))
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(self.render_request_editor(window, cx));
        request = match share {
            None => request.flex_shrink(1.).max_h(relative(0.62)),
            Some(share) => request.flex_none().h(relative(share)),
        };
        let bar = if self.is_gnmi() {
            self.render_gnmi_commit_bar(cx)
        } else {
            self.render_candidate_bar(cx)
        };
        let divider = theme.palette.divider;
        let hover = alpha(p.primary.main, 0.15);
        let main = div()
            .relative()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(bounds_probe(&main_anchor))
            .children(bar)
            .child(request)
            .child(
                div()
                    .id("split")
                    .relative()
                    .h(px(6.))
                    .flex_none()
                    .border_t_1()
                    .border_color(divider)
                    .cursor_row_resize()
                    .hover(move |s| s.bg(hover))
                    .child(
                        div()
                            .absolute()
                            .top(px(1.))
                            .left(relative(0.5))
                            .ml(px(-18.))
                            .w(px(36.))
                            .h(px(3.))
                            .rounded(px(2.))
                            .bg(divider),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.ui.drag = Some(Drag::Split);
                            cx.stop_propagation();
                        }),
                    )
                    .on_click(cx.listener(
                        |this, event: &muxus_ui::gpui_kit::ClickEvent, _, cx| {
                            if event.click_count() == 2 {
                                this.ui.request_share = None;
                                cx.notify();
                            }
                        },
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_result_pane(window, cx)),
            );
        body.child(main)
    }

    fn on_drag_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.ui.drag else {
            return;
        };
        if !event.dragging() {
            self.ui.drag = None;
            return;
        }
        match drag {
            Drag::Navigator {
                start_x,
                start_width,
            } => {
                let width = start_width + muxus_ui::zoom::unzoom(event.position.x) - start_x;
                self.ui.navigator_width = clamp_navigator(width, self.ui.width.get());
            }
            Drag::Split => {
                if let Some(bounds) = self.ui.main.get() {
                    let share = muxus_ui::zoom::unzoom(event.position.y - bounds.top())
                        / muxus_ui::zoom::unzoom(bounds.size.height).max(1.);
                    self.ui.request_share = Some(share.clamp(0.15, 0.85));
                }
            }
        }
        cx.notify();
    }

    fn on_drag_end(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.ui.drag.take().is_some() {
            cx.notify();
        }
    }
}

/// `cx.listener` for handlers that only need the view (chip clicks).
pub(super) trait ListenerForApp<T> {
    /// A click handler for the kit's widgets that updates this view.
    fn listener_for_app(
        &self,
        f: impl Fn(&mut T, &mut Context<T>) + 'static,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static;

    /// A close handler (backdrops, Escape) that updates this view.
    fn closer_for_app(
        &self,
        f: impl Fn(&mut T, &mut Context<T>) + 'static,
    ) -> impl Fn(&mut Window, &mut App) + 'static;
}

impl<T: 'static> ListenerForApp<T> for Context<'_, T> {
    fn listener_for_app(
        &self,
        f: impl Fn(&mut T, &mut Context<T>) + 'static,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        let entity = self.weak_entity();
        let f = Rc::new(f);
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            let f = f.clone();
            let _ = entity.update(cx, |this, cx| f(this, cx));
        }
    }

    fn closer_for_app(
        &self,
        f: impl Fn(&mut T, &mut Context<T>) + 'static,
    ) -> impl Fn(&mut Window, &mut App) + 'static {
        let entity = self.weak_entity();
        let f = Rc::new(f);
        move |_: &mut Window, cx: &mut App| {
            let f = f.clone();
            let _ = entity.update(cx, |this, cx| f(this, cx));
        }
    }
}
