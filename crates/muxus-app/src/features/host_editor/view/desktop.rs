//! RDP and VNC sections (`RemoteDesktopHostEditorContent.tsx`) and the SSH
//! gateway picker they share with the management editor (`SshGatewayField.tsx`).

use std::rc::Rc;

use muxus_core::api::SavedHostKind;
use muxus_core::protocol::{SavedHostSessionProfile, SshGateway};
use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div,
};
use muxus_ui::theme::{ActiveTheme, Status};
use muxus_ui::widgets::Alert;

use super::HostEditor;
use super::form::{self, AutocompleteHandlers, Form, Suggestion};
use super::ssh::{row, stack};
use crate::features::host_editor::logic::js::digits_only;
use crate::features::host_editor::services::EditorData;
use crate::features::host_editor::widgets::section::intro;
use crate::features::host_editor::widgets::visuals::host_color_picker;
use crate::hosts::organization::host_display_name;
use crate::hosts::saved::{saved_host_address, saved_host_display_name};
use crate::util::locale_compare;
use muxus_ui::widgets::form_control::FormControl;
use muxus_ui::widgets::form_control::FormControlLabel;
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::typography::{body2, inline_caption, para};

/// The SSH hosts a connection can be carried through: ssh_config aliases
/// and saved SSH hosts, sorted by name. `(key, label, detail, gateway)`.
pub(super) fn gateway_options(data: &EditorData) -> Vec<(String, String, String, SshGateway)> {
    let mut options: Vec<(String, String, String, SshGateway)> = data
        .ssh_config
        .as_ref()
        .map(|c| c.hosts.as_slice())
        .unwrap_or(&[])
        .iter()
        .map(|host| {
            (
                format!("ssh:{}", host.alias),
                host_display_name(host).to_owned(),
                host.alias.clone(),
                SshGateway {
                    target: host.alias.clone(),
                    profile_id: None,
                },
            )
        })
        .collect();
    for profile in &data.saved_hosts {
        if let SavedHostSessionProfile::Ssh(ssh) = &profile.profile {
            options.push((
                format!("profile:{}", profile.id),
                saved_host_display_name(profile).to_owned(),
                saved_host_address(profile),
                SshGateway {
                    target: ssh.target.clone(),
                    profile_id: Some(profile.id.clone()),
                },
            ));
        }
    }
    options.sort_by(|a, b| locale_compare(&a.1, &b.1));
    options
}

/// `SshGatewayField`: pick an SSH host, or none for a direct connection.
#[allow(clippy::too_many_arguments)]
pub(super) fn gateway_field<V: Form>(
    view: &mut V,
    key: &str,
    label: &str,
    value: Option<&SshGateway>,
    data: &EditorData,
    window: &mut Window,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, Option<SshGateway>, &mut Context<V>) + 'static,
) -> AnyElement {
    let options = gateway_options(data);
    let selected_key = value.map(|g| match &g.profile_id {
        Some(id) => format!("profile:{id}"),
        None => format!("ssh:{}", g.target),
    });
    let selected_label = value
        .map(|g| {
            options
                .iter()
                .find(|o| Some(&o.0) == selected_key.as_ref())
                .map(|o| o.1.clone())
                .unwrap_or_else(|| g.target.clone())
        })
        .unwrap_or_default();
    let suggestions: Vec<Suggestion> = options
        .iter()
        .map(|(key, label, detail, _)| {
            Suggestion::new(key.clone(), label.clone()).detail(detail.clone())
        })
        .collect();
    let on_change = Rc::new(on_change);
    let (typed, picked) = (on_change.clone(), on_change);
    let label = label.to_owned();
    let pick_key = key.to_owned();
    form::autocomplete(
        view,
        key,
        selected_label,
        suggestions,
        move |input| {
            FormField::input(SharedString::from(format!("{label}-gateway")), &input)
                .label(label.clone())
                .placeholder("Direct connection")
                .select(false)
                .padding_right(65.)
        },
        AutocompleteHandlers {
            on_input: Rc::new(move |this: &mut V, text, cx| {
                // Not free solo: clearing the text clears the gateway.
                if text.trim().is_empty() {
                    typed(this, None, cx);
                }
                None
            }),
            on_pick: Rc::new(move |this: &mut V, key, window, cx| {
                if let Some((_, label, _, gateway)) = options.iter().find(|o| o.0 == key) {
                    this.inputs().set(&pick_key, label, window, cx);
                    picked(this, Some(gateway.clone()), cx);
                }
            }),
            on_enter: None,
        },
        window,
        cx,
    )
}

impl HostEditor {
    pub(super) fn desktop_general(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let rdp = self.kind == SavedHostKind::Rdp;
        let name = form::text_input(
            self,
            "desktop-name",
            window,
            cx,
            self.desktop.name.clone(),
            |this, v, cx| {
                this.desktop.name = v;
                this.changed(cx);
                None
            },
        );
        let host = form::text_input(
            self,
            "desktop-host",
            window,
            cx,
            self.desktop.host.clone(),
            |this, v, cx| {
                this.desktop.host = v;
                this.changed(cx);
                None
            },
        );
        let port = form::text_input(
            self,
            "desktop-port",
            window,
            cx,
            self.desktop.port.clone(),
            |this, v, cx| {
                let digits = digits_only(&v);
                let fix = (digits != v).then(|| digits.clone());
                this.desktop.port = digits;
                this.changed(cx);
                fix
            },
        );
        let default_port = self.desktop_kind().default_port();
        let mut col = stack(2.)
            .child(
                FormField::input("desktop-name-field", &name)
                    .label("Name")
                    .required(true)
                    .placeholder(if rdp { "Build server" } else { "Lab console" })
                    .helper("How this host appears in the host list"),
            )
            .child(
                row(1.5)
                    .child(
                        FormField::input("desktop-host-field", &host)
                            .label("Host")
                            .placeholder(if rdp {
                                "win-build.example.com"
                            } else {
                                "lab-vm.example.com"
                            })
                            .helper_opt(
                                self.desktop
                                    .ssh_gateway
                                    .as_ref()
                                    .map(|g| format!("Resolved by {}", g.target)),
                            ),
                    )
                    .child(
                        FormField::input("desktop-port-field", &port)
                            .label("Port")
                            .placeholder(default_port.to_string())
                            .width(130.),
                    ),
            );
        let known = self.data.known_folders();
        let group = self.desktop.group.clone();
        col = col.child(form::folder_field(
            self,
            "desktop-group",
            "Folder",
            &group,
            known,
            Some("Optional — use / to nest, e.g. Lab/Windows.".into()),
            None,
            false,
            window,
            cx,
            |this, group, cx| {
                this.desktop.group = group;
                this.changed(cx);
            },
        ));
        let entity = cx.entity().downgrade();
        col = col.child(host_color_picker(
            &theme,
            "desktop-color",
            "Color",
            self.desktop.color.as_deref(),
            false,
            30.,
            Rc::new(move |color, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.desktop.color = color;
                    this.changed(cx);
                });
            }),
        ));
        if !rdp && self.desktop.ssh_gateway.is_none() {
            col = col.child(Alert::new(
                Status::Warning,
                "Most VNC servers do not encrypt their traffic. Outside a trusted network, reach the host through an SSH gateway (Connection route).",
            ));
        }
        col.into_any_element()
    }

    pub(super) fn desktop_logon(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let rdp = self.kind == SavedHostKind::Rdp;
        let user = form::text_input(
            self,
            "desktop-user",
            window,
            cx,
            self.desktop.username.clone(),
            |this, v, cx| {
                this.desktop.username = v;
                this.changed(cx);
                None
            },
        );
        let mut col = stack(2.).child(
            FormField::input("desktop-user-field", &user)
                .label("User name")
                .placeholder(if rdp { "Administrator" } else { "Only when the server asks for one" })
                .helper(if rdp {
                    "Leave empty to be asked when connecting. DOMAIN\\user and user@domain work too."
                } else {
                    "VNC servers usually ask for a password only; some (macOS, VeNCrypt) also want a user name."
                }),
        );
        if rdp {
            let domain = form::text_input(
                self,
                "desktop-domain",
                window,
                cx,
                self.desktop.domain.clone(),
                |this, v, cx| {
                    this.desktop.domain = v;
                    this.changed(cx);
                    None
                },
            );
            col = col.child(
                FormField::input("desktop-domain-field", &domain)
                    .label("Domain")
                    .placeholder("Optional"),
            );
        }
        col.child(para(
            "Muxus asks for the password when it connects. Tick “Remember this password” there to keep it in the encrypted password vault.",
            body2(),
            theme.palette.text.secondary,
        ))
        .into_any_element()
    }

    pub(super) fn desktop_route(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let rdp = self.kind == SavedHostKind::Rdp;
        let data = self.data.clone();
        let gateway = self.desktop.ssh_gateway.clone();
        stack(2.)
            .child(intro(
                &theme,
                "SSH gateway",
                format!(
                    "Reach the {} server through an SSH host, like ssh -L: the host and port above are resolved and connected on the gateway's side. Useful for servers behind a bastion or listening only on localhost.",
                    if rdp { "Remote Desktop" } else { "VNC" }
                ),
            ))
            .child(gateway_field(self, "desktop-gateway", "SSH gateway", gateway.as_ref(), &data, window, cx, |this, gateway, cx| {
                this.desktop.ssh_gateway = gateway;
                this.changed(cx);
            }))
            .into_any_element()
    }

    pub(super) fn desktop_options(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let rdp = self.kind == SavedHostKind::Rdp;
        let option = |id: &'static str,
                      checked: bool,
                      title: &'static str,
                      sub: &'static str,
                      flip: fn(&mut HostEditor),
                      cx: &mut Context<Self>| {
            let entity = cx.entity().downgrade();
            FormControlLabel::new(id, FormControl::Switch(checked), "")
                .label_element(
                    div()
                        .flex()
                        .flex_col()
                        .child(para(title, body2(), p.text.primary))
                        .child(inline_caption(sub, p.text.secondary)),
                )
                .flush()
                .on_click(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        flip(this);
                        this.changed(cx);
                    });
                })
        };
        let mut col = stack(1.).child(option(
            "share-clipboard",
            self.desktop.share_clipboard,
            "Share the clipboard",
            "Copy and paste text between this computer and the remote desktop. The server can read what you copy while its tab has focus.",
            |this| this.desktop.share_clipboard = !this.desktop.share_clipboard,
            cx,
        ));
        if !rdp {
            col = col
                .child(option(
                    "resize-remote",
                    self.desktop.resize_remote,
                    "Resize the remote desktop to fit",
                    "Ask the server to change its resolution to match the pane instead of scaling the picture. Servers such as TigerVNC support this; shared physical screens usually do not.",
                    |this| this.desktop.resize_remote = !this.desktop.resize_remote,
                    cx,
                ))
                .child(option(
                    "view-only",
                    self.desktop.view_only,
                    "View only",
                    "Watch the screen without sending keyboard or mouse input.",
                    |this| this.desktop.view_only = !this.desktop.view_only,
                    cx,
                ));
        } else {
            col = col.child(div().pt(muxus_ui::gpui_kit::px(8.)).child(para(
                "The remote desktop follows the size of its pane when the server supports resizing (Windows 8.1 / Server 2012 R2 and later, xrdp); otherwise the picture is scaled to fit.",
                body2(),
                p.text.secondary,
            )));
        }
        col.into_any_element()
    }
}
