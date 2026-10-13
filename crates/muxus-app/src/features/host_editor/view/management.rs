//! gNMI and NETCONF sections (`ManagementHostEditorContent.tsx`).

use std::rc::Rc;

use muxus_core::api::SavedHostKind;
use muxus_core::management::{GnmiEncoding, GnmiTlsMode};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, PathPromptOptions,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::theme::{ActiveTheme, Status, fonts};
use muxus_ui::widgets::Alert;

use super::HostEditor;
use super::desktop::gateway_field;
use super::form::{self, Choice};
use super::ssh::{row, stack};
use crate::features::host_editor::logic::js::digits_only;
use crate::features::host_editor::widgets::section::intro;
use crate::features::host_editor::widgets::visuals::host_color_picker;
use muxus_ui::theme::Theme;
use muxus_ui::widgets::Button;
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::typography::VariantExt;
use muxus_ui::widgets::typography::weight;
use muxus_ui::widgets::typography::{body2, caption, para, text};

/// The client field the management TLS files map to.
#[derive(Clone, Copy)]
enum TlsFile {
    Ca,
    Cert,
    Key,
}

impl HostEditor {
    pub(super) fn management_general(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let gnmi = self.kind == SavedHostKind::Gnmi;
        let name = form::text_input(
            self,
            "mgmt-name",
            window,
            cx,
            self.management.name.clone(),
            |this, v, cx| {
                this.management.name = v;
                this.changed(cx);
                None
            },
        );
        let host = form::text_input(
            self,
            "mgmt-host",
            window,
            cx,
            self.management.host.clone(),
            |this, v, cx| {
                this.management.host = v;
                this.changed(cx);
                None
            },
        );
        let port = form::text_input(
            self,
            "mgmt-port",
            window,
            cx,
            self.management.port.clone(),
            |this, v, cx| {
                let digits = digits_only(&v);
                let fix = (digits != v).then(|| digits.clone());
                this.management.port = digits;
                this.changed(cx);
                fix
            },
        );
        let host_helper = match &self.management.ssh_gateway {
            Some(gateway) => Some(format!("Resolved by {}", gateway.target)),
            None if !gnmi => {
                Some("An ssh_config alias works too: its user, keys and jump hosts apply.".into())
            }
            None => None,
        };
        let default_port = self.management_kind().default_port();
        let mut col = stack(2.)
            .child(
                FormField::input("mgmt-name-field", &name)
                    .label("Name")
                    .required(true)
                    .placeholder(if gnmi {
                        "leaf1 telemetry"
                    } else {
                        "leaf1 config"
                    })
                    .helper("How this host appears in the host list"),
            )
            .child(
                row(1.5)
                    .child(
                        FormField::input("mgmt-host-field", &host)
                            .label("Host")
                            .placeholder("leaf1.lab.example.com")
                            .helper_opt(host_helper),
                    )
                    .child(
                        FormField::input("mgmt-port-field", &port)
                            .label("Port")
                            .placeholder(default_port.to_string())
                            .width(130.),
                    ),
            );
        let known = self.data.known_folders();
        let group = self.management.group.clone();
        col = col.child(form::folder_field(
            self,
            "mgmt-group",
            "Folder",
            &group,
            known,
            Some("Optional — use / to nest, e.g. Lab/Fabric.".into()),
            None,
            false,
            window,
            cx,
            |this, group, cx| {
                this.management.group = group;
                this.changed(cx);
            },
        ));
        let entity = cx.entity().downgrade();
        col = col
            .child(host_color_picker(
                &theme,
                "mgmt-color",
                "Color",
                self.management.color.as_deref(),
                false,
                30.,
                Rc::new(move |color, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.management.color = color;
                        this.changed(cx);
                    });
                }),
            ))
            .child(para(
                if gnmi {
                    "gNMI runs over gRPC, usually on port 57400 (SR Linux, Arista EOS) or 9339 (the IANA port). The session opens a workbench to browse, get, set and stream telemetry."
                } else {
                    "NETCONF runs over SSH on port 830. The session opens a workbench to browse configuration and state, edit the candidate and commit."
                },
                body2(),
                theme.palette.text.secondary,
            ));
        col.into_any_element()
    }

    pub(super) fn management_login(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let gnmi = self.kind == SavedHostKind::Gnmi;
        let user = form::text_input(
            self,
            "mgmt-user",
            window,
            cx,
            self.management.username.clone(),
            |this, v, cx| {
                this.management.username = v;
                this.changed(cx);
                None
            },
        );
        stack(2.)
            .child(
                FormField::input("mgmt-user-field", &user)
                    .label("User name")
                    .placeholder(if gnmi { "admin" } else { "From ssh_config, or your local user" })
                    .helper(if gnmi {
                        "Leave empty to be asked when connecting, or for devices that accept unauthenticated gNMI."
                    } else {
                        "Leave empty to use the User from ssh_config."
                    }),
            )
            .child(para(
                if gnmi {
                    "Muxus asks for the password when it connects and sends it with every gNMI call, as gNMI requires. Tick “Remember this password” there to keep it in the encrypted password vault."
                } else {
                    "NETCONF logs in like an SSH session: keys from ssh_config and the agent are tried first, then Muxus asks for a password, which you can keep in the password vault."
                },
                body2(),
                theme.palette.text.secondary,
            ))
            .into_any_element()
    }

    fn file_field(
        &mut self,
        file: TlsFile,
        label: &str,
        placeholder: &str,
        helper: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (key, value) = match file {
            TlsFile::Ca => ("mgmt-ca", self.management.ca_file.clone()),
            TlsFile::Cert => ("mgmt-cert", self.management.cert_file.clone()),
            TlsFile::Key => ("mgmt-key", self.management.key_file.clone()),
        };
        let input = form::text_input(self, key, window, cx, value, move |this, v, cx| {
            match file {
                TlsFile::Ca => this.management.ca_file = v,
                TlsFile::Cert => this.management.cert_file = v,
                TlsFile::Key => this.management.key_file = v,
            }
            this.changed(cx);
            None
        });
        let entity = cx.entity().downgrade();
        row(1.)
            .items_start()
            .child(
                FormField::input(SharedString::from(format!("{key}-field")), &input)
                    .label(label.to_owned())
                    .placeholder(placeholder.to_owned())
                    .helper_opt(helper)
                    .mono(12.5),
            )
            .child(
                Button::new(SharedString::from(format!("{key}-browse")), "Browse…")
                    .outlined()
                    .start_icon("FolderOpenOutlined")
                    .on_click(move |_, _, cx| {
                        let chosen = crate::util::file_dialog::pick_paths(
                            PathPromptOptions {
                                files: true,
                                directories: false,
                                multiple: false,
                                prompt: None,
                            },
                            cx,
                        );
                        let entity = entity.clone();
                        cx.spawn(async move |cx| {
                            if let Some(paths) = chosen.await
                                && let Some(path) = paths.first()
                            {
                                let path = path.to_string_lossy().to_string();
                                let _ = entity.update_in(cx, |this, window, cx| {
                                    this.inputs.set(key, &path, window, cx);
                                    match file {
                                        TlsFile::Ca => this.management.ca_file = path,
                                        TlsFile::Cert => this.management.cert_file = path,
                                        TlsFile::Key => this.management.key_file = path,
                                    }
                                    this.changed(cx);
                                });
                            }
                        })
                        .detach();
                    })
                    .min_width(0.),
            )
            .into_any_element()
    }

    pub(super) fn management_security(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let modes = [
            (
                GnmiTlsMode::Verify,
                "VerifiedUserOutlined",
                "TLS",
                "Verify the certificate. One that does not verify is shown once and pinned, like an SSH host key.",
            ),
            (
                GnmiTlsMode::SkipVerify,
                "HttpsOutlined",
                "TLS, don’t verify",
                "Encrypt, but accept any certificate. For labs whose certificates change on every redeploy.",
            ),
            (
                GnmiTlsMode::Plaintext,
                "NoEncryptionGmailerrorredOutlined",
                "Plain text",
                "No TLS at all. Passwords and data cross the network readable.",
            ),
        ];
        let mut cards = row(1.);
        for (mode, icon_name, title, description) in modes {
            let selected = self.management.tls == mode;
            let ink = if selected {
                p.primary.main
            } else {
                p.text.primary
            };
            let entity = cx.entity().downgrade();
            let hover = if selected {
                p.primary.main
            } else {
                p.text.secondary
            };
            cards = cards.child(
                div()
                    .id(SharedString::from(format!("tls-{title}")))
                    .flex_1()
                    .min_w_0()
                    .p(Theme::spacing(1.25))
                    .rounded(px(theme.radius * 1.5))
                    .border_1()
                    .border_color(if selected { p.primary.main } else { p.divider })
                    .when_selected(selected, alpha(p.primary.main, 0.08))
                    .hover(move |s| s.border_color(hover))
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.management.tls = mode;
                            this.changed(cx);
                        });
                    })
                    .child(
                        row(1.)
                            .items_center()
                            .mb(Theme::spacing(0.5))
                            .child(icon(icon_name, SMALL, ink))
                            .child(text(title, body2().weight(weight::SEMIBOLD), ink)),
                    )
                    .child(para(
                        description,
                        caption().line_height(caption().size * 1.4),
                        p.text.secondary,
                    )),
            );
        }
        let mut col = stack(2.).child(cards);
        if self.management.tls == GnmiTlsMode::Plaintext && self.management.ssh_gateway.is_none() {
            col = col.child(Alert::new(
                Status::Warning,
                "Without TLS the password travels in clear text with every request. Outside a lab, use TLS or reach the device through an SSH gateway (Connection route).",
            ));
        }
        if self.management.tls != GnmiTlsMode::Plaintext {
            col = col.child(self.file_field(
                TlsFile::Ca,
                "CA certificate",
                "clab-mylab/.tls/ca/ca.pem",
                Some("Verify against this authority instead of the system's. containerlab writes one per lab."),
                window,
                cx,
            ));
            let name = form::text_input(
                self,
                "mgmt-tls-name",
                window,
                cx,
                self.management.tls_server_name.clone(),
                |this, v, cx| {
                    this.management.tls_server_name = v;
                    this.changed(cx);
                    None
                },
            );
            let placeholder = if self.management.host.is_empty() {
                "leaf1.example.com".to_owned()
            } else {
                self.management.host.clone()
            };
            col = col
                .child(
                    FormField::input("mgmt-tls-name-field", &name)
                        .label("Certificate name")
                        .placeholder(placeholder)
                        .helper("When the certificate names the device differently from the host above."),
                )
                .child(intro(&theme, "Client certificate", "For devices that require mutual TLS."))
                .child(self.file_field(TlsFile::Cert, "Certificate", "client.pem", None, window, cx))
                .child(self.file_field(TlsFile::Key, "Key", "client.key", None, window, cx));
        }
        let _ = fonts::MONO;
        col.into_any_element()
    }

    pub(super) fn management_route(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let gnmi = self.kind == SavedHostKind::Gnmi;
        let data = self.data.clone();
        let gateway = self.management.ssh_gateway.clone();
        stack(2.)
            .child(intro(
                &theme,
                "SSH gateway",
                if gnmi {
                    "Reach the gNMI server through an SSH host, like ssh -L: the host and port above are connected from the gateway. TLS still runs end to end between Muxus and the device."
                } else {
                    "Reach the device through an SSH jump host, like ssh -J. Jump hosts set in ssh_config for this host apply as well."
                },
            ))
            .child(gateway_field(
                self,
                "mgmt-gateway",
                if gnmi { "SSH gateway" } else { "Jump host" },
                gateway.as_ref(),
                &data,
                window,
                cx,
                |this, gateway, cx| {
                    this.management.ssh_gateway = gateway;
                    this.changed(cx);
                },
            ))
            .into_any_element()
    }

    pub(super) fn management_options(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let encoding = self.management.encoding;
        stack(2.)
            .child(form::select(
                self,
                "mgmt-encoding",
                |body| {
                    FormField::new("mgmt-encoding-field", body)
                        .label("Encoding")
                        .helper("How values travel. Automatic picks JSON_IETF when the device offers it.")
                },
                &encoding,
                vec![
                    Choice::new(None, "Automatic"),
                    Choice::new(Some(GnmiEncoding::JsonIetf), "JSON_IETF"),
                    Choice::new(Some(GnmiEncoding::Json), "JSON"),
                    Choice::new(Some(GnmiEncoding::Proto), "PROTO"),
                    Choice::new(Some(GnmiEncoding::Ascii), "ASCII"),
                    Choice::new(Some(GnmiEncoding::Bytes), "BYTES"),
                ],
                encoding.is_none().then(|| muxus_ui::widgets::form_field::FieldBody::Value(SharedString::default())),
                window,
                cx,
                |this, encoding, cx| {
                    this.management.encoding = encoding;
                    this.changed(cx);
                },
            ))
            .into_any_element()
    }
}

trait WhenSelected: Sized {
    fn when_selected(self, selected: bool, color: muxus_ui::gpui_kit::Hsla) -> Self;
}

impl<T: Styled> WhenSelected for T {
    fn when_selected(self, selected: bool, color: muxus_ui::gpui_kit::Hsla) -> Self {
        if selected { self.bg(color) } else { self }
    }
}
