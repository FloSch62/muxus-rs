//! Web page sections (`WebHostEditorContent.tsx`): the address, folder and color, and the SSH
//! gateway whose SOCKS5 proxy carries the page.

use std::rc::Rc;

use muxus_ui::gpui_kit::{AnyElement, Context, Focusable, IntoElement, ParentElement, Window};
use muxus_ui::theme::ActiveTheme;
use muxus_ui::widgets::form_field::FormField;

use super::HostEditor;
use super::desktop::gateway_field;
use super::form;
use super::ssh::stack;
use crate::features::host_editor::logic::web_draft::normalize_web_url;
use crate::features::host_editor::widgets::section::intro;
use crate::features::host_editor::widgets::visuals::host_color_picker;

const ADDRESS: &str = "web-url";

impl HostEditor {
    pub(super) fn web_general(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let name = form::text_input(
            self,
            "web-name",
            window,
            cx,
            self.web.name.clone(),
            |this, v, cx| {
                this.web.name = v;
                this.changed(cx);
                None
            },
        );
        let address = form::text_input(
            self,
            ADDRESS,
            window,
            cx,
            self.web.url.clone(),
            |this, v, cx| {
                this.web.url = v;
                this.changed(cx);
                None
            },
        );
        // `onBlur`: an address without a scheme gets http:// once the field is left.
        let normalized = normalize_web_url(&self.web.url);
        if normalized != self.web.url && !address.focus_handle(cx).is_focused(window) {
            self.web.url = normalized.clone();
            self.inputs.set(ADDRESS, &normalized, window, cx);
        }
        let helper = match &self.web.ssh_gateway {
            Some(gateway) => format!(
                "Opened through {}: names and addresses resolve on its side.",
                gateway.target
            ),
            None => "http:// is added when no scheme is given.".to_owned(),
        };
        let mut col = stack(2.)
            .child(
                FormField::input("web-name-field", &name)
                    .label("Name")
                    .required(true)
                    .placeholder("Grafana")
                    .helper("How this page appears in the host list"),
            )
            .child(
                FormField::input("web-url-field", &address)
                    .label("Address")
                    .required(true)
                    .placeholder("https://grafana.internal:3000")
                    .helper(helper),
            );
        let known = self.data.known_folders();
        let group = self.web.group.clone();
        col = col.child(form::folder_field(
            self,
            "web-group",
            "Folder",
            &group,
            known,
            Some("Optional — use / to nest, e.g. Monitoring/EU.".into()),
            None,
            false,
            window,
            cx,
            |this, group, cx| {
                this.web.group = group;
                this.changed(cx);
            },
        ));
        let entity = cx.entity().downgrade();
        col = col.child(host_color_picker(
            &theme,
            "web-color",
            "Color",
            self.web.color.as_deref(),
            false,
            30.,
            Rc::new(move |color, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.web.color = color;
                    this.changed(cx);
                });
            }),
        ));
        col.into_any_element()
    }

    pub(super) fn web_route(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let data = self.data.clone();
        let gateway = self.web.ssh_gateway.clone();
        stack(2.)
            .child(intro(
                &theme,
                "SSH gateway (SOCKS5)",
                "Open the page through an SSH host, like ssh -D: Muxus starts a SOCKS5 proxy on the gateway and the page's window uses it for every request, name lookups included. Internal names, HTTPS and links to other internal hosts work as they would on the gateway.",
            ))
            .child(gateway_field(
                self,
                "web-gateway",
                "SSH gateway",
                gateway.as_ref(),
                &data,
                window,
                cx,
                |this, gateway, cx| {
                    this.web.ssh_gateway = gateway;
                    this.changed(cx);
                },
            ))
            .into_any_element()
    }
}
