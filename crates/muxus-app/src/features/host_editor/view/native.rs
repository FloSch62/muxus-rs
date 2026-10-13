//! Telnet and serial sections (`NativeHostEditorContent.tsx`).

use std::rc::Rc;

use muxus_core::api::SavedHostKind;
use muxus_core::protocol::{SerialFlowControl, SerialParity, StopBits};
use muxus_ui::gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::theme::{ActiveTheme, Status};
use muxus_ui::widgets::{Alert, IconButton, Tooltip};

use super::HostEditor;
use super::form::{self, AutocompleteHandlers, Choice, Suggestion};
use super::ssh::{row, stack};
use crate::features::host_editor::logic::js::digits_only;
use crate::features::host_editor::logic::native_draft::{COMMON_BAUD_RATES, platform_path_example};
use crate::features::host_editor::widgets::section::intro;
use crate::features::host_editor::widgets::visuals::host_color_picker;
use muxus_ui::theme::Theme;
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::typography::{body1, text};

impl HostEditor {
    pub(super) fn native_general(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let telnet = self.kind == SavedHostKind::Telnet;
        let name = form::text_input(
            self,
            "native-name",
            window,
            cx,
            self.native.name.clone(),
            |this, v, cx| {
                this.native.name = v;
                this.changed(cx);
                None
            },
        );
        let mut col = stack(2.).child(
            FormField::input("native-name-field", &name)
                .label("Name")
                .required(true)
                .placeholder(if telnet {
                    "Core router"
                } else {
                    "Console cable"
                })
                .helper("How this host appears in the host list"),
        );
        if telnet {
            let host = form::text_input(
                self,
                "native-host",
                window,
                cx,
                self.native.host.clone(),
                |this, v, cx| {
                    this.native.host = v;
                    this.changed(cx);
                    None
                },
            );
            let port = form::text_input(
                self,
                "native-port",
                window,
                cx,
                self.native.port.clone(),
                |this, v, cx| {
                    let digits = digits_only(&v);
                    let fix = (digits != v).then(|| digits.clone());
                    this.native.port = digits;
                    this.changed(cx);
                    fix
                },
            );
            col = col.child(
                row(1.5)
                    .child(
                        FormField::input("native-host-field", &host)
                            .label("Host")
                            .placeholder("router.example.com"),
                    )
                    .child(
                        FormField::input("native-port-field", &port)
                            .label("Port")
                            .width(130.),
                    ),
            );
        } else {
            col = col.child(self.serial_port_fields(window, cx));
        }
        let known = self.data.known_folders();
        let group = self.native.group.clone();
        col = col.child(form::folder_field(
            self,
            "native-group",
            "Folder",
            &group,
            known,
            Some("Optional — use / to nest, e.g. Lab/Bench 3.".into()),
            None,
            false,
            window,
            cx,
            |this, group, cx| {
                this.native.group = group;
                this.changed(cx);
            },
        ));
        let entity = cx.entity().downgrade();
        col = col.child(host_color_picker(
            &theme,
            "native-color",
            "Color",
            self.native.color.as_deref(),
            false,
            30.,
            Rc::new(move |color, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.native.color = color;
                    this.changed(cx);
                });
            }),
        ));
        if telnet {
            col = col.child(Alert::new(
                Status::Warning,
                "Telnet is unencrypted. Credentials and terminal traffic are sent in plaintext.",
            ));
        }
        col.into_any_element()
    }

    fn load_serial_ports(&mut self, cx: &mut Context<Self>) {
        if self.serial_fetching {
            return;
        }
        self.serial_fetching = true;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let ports = backend.serial_ports().await;
            let _ = this.update(cx, |this, cx| {
                this.serial_fetching = false;
                this.serial_ports = Some(ports.map(|r| r.ports).map_err(|e| e.message));
                cx.notify();
            });
        })
        .detach();
    }

    fn serial_port_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.serial_ports.is_none() {
            self.load_serial_ports(cx);
        }
        let ports: Vec<Suggestion> = match &self.serial_ports {
            Some(Ok(ports)) => ports
                .iter()
                .map(|port| {
                    let detail = [port.manufacturer.clone(), port.serial_number.clone()]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · ");
                    let suggestion = Suggestion::new(port.path.clone(), port.path.clone());
                    if detail.is_empty() {
                        suggestion
                    } else {
                        suggestion.detail(detail)
                    }
                })
                .collect(),
            _ => Vec::new(),
        };
        let path = form::autocomplete(
            self,
            "serial-path",
            self.native.path.clone(),
            ports,
            |input| {
                FormField::input("serial-path-field", &input)
                    .label("Serial port")
                    .placeholder(platform_path_example())
                    .helper("Select a detected device or enter its OS-native path.")
            },
            AutocompleteHandlers {
                on_input: Rc::new(|this: &mut HostEditor, v, cx| {
                    this.native.path = v;
                    this.changed(cx);
                    None
                }),
                on_pick: Rc::new(|this: &mut HostEditor, path, window, cx| {
                    this.inputs.set("serial-path", &path, window, cx);
                    this.native.path = path;
                    this.changed(cx);
                }),
                on_enter: None,
            },
            window,
            cx,
        );
        let entity = cx.entity().downgrade();
        let mut col = stack(2.).child(
            row(0.5)
                .items_start()
                .child(div().flex_1().min_w_0().child(path))
                .child(
                    div()
                        .id("refresh-serial-tip")
                        .mt(Theme::spacing(0.75))
                        .tooltip(Tooltip::text("Refresh serial ports"))
                        .child(
                            IconButton::new("refresh-serial", "Refresh")
                                .medium()
                                .icon_size(muxus_ui::icon::SMALL)
                                .disabled(self.serial_fetching)
                                .on_click(move |_, _, cx| {
                                    let _ =
                                        entity.update(cx, |this, cx| this.load_serial_ports(cx));
                                }),
                        ),
                ),
        );
        if matches!(self.serial_ports, Some(Err(_))) {
            col = col.child(Alert::new(
                Status::Warning,
                "Serial ports could not be enumerated. Enter a device path manually.",
            ));
        }
        let rates: Vec<Suggestion> = COMMON_BAUD_RATES
            .iter()
            .map(|rate| Suggestion::new(rate.to_string(), rate.to_string()))
            .collect();
        col = col.child(form::autocomplete(
            self,
            "serial-baud",
            self.native.baud_rate.clone(),
            rates,
            |input| FormField::input("serial-baud-field", &input).label("Baud rate"),
            AutocompleteHandlers {
                on_input: Rc::new(|this: &mut HostEditor, v, cx| {
                    this.native.baud_rate = v;
                    this.changed(cx);
                    None
                }),
                on_pick: Rc::new(|this: &mut HostEditor, rate, window, cx| {
                    this.inputs.set("serial-baud", &rate, window, cx);
                    this.native.baud_rate = rate;
                    this.changed(cx);
                }),
                on_enter: None,
            },
            window,
            cx,
        ));
        col.into_any_element()
    }

    pub(super) fn native_line(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let data_bits = self.native.data_bits;
        let stop_bits = self.native.stop_bits;
        let parity = self.native.parity;
        let flow = self.native.flow_control;
        let data = form::select(
            self,
            "data-bits",
            |body| {
                FormField::new("data-bits-field", body)
                    .label("Data bits")
                    .flexible()
            },
            &data_bits,
            [5u8, 6, 7, 8]
                .iter()
                .map(|b| Choice::new(*b, b.to_string()))
                .collect(),
            None,
            window,
            cx,
            |this, bits, cx| {
                this.native.data_bits = bits;
                this.changed(cx);
            },
        );
        let stop = form::select(
            self,
            "stop-bits",
            |body| {
                FormField::new("stop-bits-field", body)
                    .label("Stop bits")
                    .flexible()
            },
            &stop_bits,
            vec![
                Choice::new(StopBits::One, "1"),
                Choice::new(StopBits::OnePointFive, "1.5"),
                Choice::new(StopBits::Two, "2"),
            ],
            None,
            window,
            cx,
            |this, bits, cx| {
                this.native.stop_bits = bits;
                this.changed(cx);
            },
        );
        let parity_select = form::select(
            self,
            "parity",
            |body| {
                FormField::new("parity-field", body)
                    .label("Parity")
                    .flexible()
            },
            &parity,
            vec![
                Choice::new(SerialParity::None, "None"),
                Choice::new(SerialParity::Even, "Even"),
                Choice::new(SerialParity::Odd, "Odd"),
                Choice::new(SerialParity::Mark, "Mark"),
                Choice::new(SerialParity::Space, "Space"),
            ],
            None,
            window,
            cx,
            |this, parity, cx| {
                this.native.parity = parity;
                this.changed(cx);
            },
        );
        let flow_select = form::select(
            self,
            "flow-control",
            |body| {
                FormField::new("flow-control-field", body)
                    .label("Flow control")
                    .flexible()
            },
            &flow,
            vec![
                Choice::new(SerialFlowControl::None, "None"),
                Choice::new(SerialFlowControl::Hardware, "Hardware (RTS/CTS)"),
                Choice::new(SerialFlowControl::Software, "Software (XON/XOFF)"),
            ],
            None,
            window,
            cx,
            |this, flow, cx| {
                this.native.flow_control = flow;
                this.changed(cx);
            },
        );
        let duration = form::text_input(
            self,
            "break-duration",
            window,
            cx,
            self.native.break_duration_ms.clone(),
            |this, v, cx| {
                this.native.break_duration_ms = v;
                this.changed(cx);
                None
            },
        );
        stack(2.)
            .child(intro(
                &theme,
                "Line settings",
                "The defaults (8-N-1, no flow control) match most consoles.",
            ))
            .child(
                row(1.5)
                    .child(div().flex_1().min_w_0().child(data))
                    .child(div().flex_1().min_w_0().child(stop)),
            )
            .child(
                row(1.5)
                    .child(div().flex_1().min_w_0().child(parity_select))
                    .child(div().flex_1().min_w_0().child(flow_select)),
            )
            .child(
                div().max_w(px(260.)).child(
                    FormField::input("break-duration-field", &duration)
                        .label("Break duration")
                        .helper("How long Send BREAK holds the line.")
                        .end(text("ms", body1(), theme.palette.text.secondary).no_truncate()),
                ),
            )
            .into_any_element()
    }
}
