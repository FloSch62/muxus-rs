//! The SSH gateway a connection goes through, and the gateways to try in order when it cannot
//! be reached (`host-editor/SshGatewayRouteField.tsx`). Fallbacks only appear once a first
//! gateway is chosen; clearing it clears them too.

use muxus_core::protocol::{MAX_JUMP_FALLBACKS, SavedHostSessionProfile, SshGateway};
use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div,
};
use muxus_ui::theme::ActiveTheme;
use muxus_ui::widgets::{Button, IconButton};

use super::HostEditor;
use super::desktop::gateway_field;
use super::ssh::{caption_block, row, stack};
use crate::features::host_editor::services::EditorData;
use crate::hosts::system_openssh::alias_uses_system_open_ssh;

/// Carrying a connection through a host needs the built-in client: a gateway option set to
/// use system OpenSSH is listed but cannot be picked. `key` is the option's
/// `ssh:<alias>` or `profile:<id>`.
pub(super) fn gateway_unavailable(data: &EditorData, key: &str) -> bool {
    if let Some(alias) = key.strip_prefix("ssh:") {
        let hosts = data
            .ssh_config
            .as_ref()
            .map(|c| c.hosts.as_slice())
            .unwrap_or(&[]);
        return alias_uses_system_open_ssh(alias, hosts);
    }
    key.strip_prefix("profile:").is_some_and(|id| {
        data.saved_hosts.iter().any(|host| {
            host.id == id
                && matches!(host.profile, SavedHostSessionProfile::Ssh(_))
                && host.metadata.system_open_ssh == Some(true)
        })
    })
}

/// Where a gateway route lives in the editor's drafts.
pub(super) struct GatewayRoute {
    pub get: fn(&HostEditor) -> (Option<SshGateway>, Vec<SshGateway>),
    pub set: fn(&mut HostEditor, Option<SshGateway>, Vec<SshGateway>),
}

impl HostEditor {
    /// `SshGatewayRouteField`: the first gateway, then a "Then try" field per fallback.
    pub(super) fn gateway_route_field(
        &mut self,
        key: &'static str,
        label: &'static str,
        route: GatewayRoute,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let data = self.data.clone();
        let (gateway, fallbacks) = (route.get)(self);
        let (get, set) = (route.get, route.set);
        let fallback_prefix = format!("{key}-fallback-");
        let mut col = stack(1.5).child(gateway_field(
            self,
            key,
            label,
            gateway.as_ref(),
            &data,
            window,
            cx,
            move |this, gateway, cx| {
                let (_, fallbacks) = get(this);
                let fallbacks = if gateway.is_some() {
                    fallbacks
                } else {
                    Vec::new()
                };
                set(this, gateway, fallbacks);
                this.changed(cx);
            },
        ));
        let Some(gateway) = gateway else {
            return col.into_any_element();
        };
        let entity = cx.entity().downgrade();
        let mut list = stack(1.);
        for (index, fallback) in fallbacks.iter().enumerate() {
            let prefix = fallback_prefix.clone();
            let remove = entity.clone();
            let remove_prefix = fallback_prefix.clone();
            let field_label = format!("Then try ({})", index + 1);
            // A fallback being picked is kept as a blank row until it has a host.
            let value = (!fallback.target.is_empty()).then(|| fallback.clone());
            let field = gateway_field(
                self,
                &format!("{prefix}{index}"),
                &field_label,
                value.as_ref(),
                &data,
                window,
                cx,
                move |this, picked, cx| {
                    let (gateway, mut fallbacks) = get(this);
                    match picked {
                        Some(picked) => {
                            if let Some(slot) = fallbacks.get_mut(index) {
                                *slot = picked;
                            }
                        }
                        None => {
                            if index < fallbacks.len() {
                                fallbacks.remove(index);
                            }
                            this.inputs.remove_prefix(&prefix);
                        }
                    }
                    set(this, gateway, fallbacks);
                    this.changed(cx);
                },
            );
            list = list.child(
                row(1.)
                    .items_center()
                    .child(div().flex_1().min_w_0().child(field))
                    .child(
                        IconButton::new(
                            SharedString::from(format!("{remove_prefix}remove-{index}")),
                            "DeleteOutlined",
                        )
                        .on_click(move |_, _, cx| {
                            let _ = remove.update(cx, |this, cx| {
                                let (gateway, mut fallbacks) = get(this);
                                if index < fallbacks.len() {
                                    fallbacks.remove(index);
                                }
                                // Rows below move up: their fields start again from the drafts.
                                this.inputs.remove_prefix(&remove_prefix);
                                set(this, gateway, fallbacks);
                                this.changed(cx);
                            });
                        }),
                    ),
            );
        }
        if !fallbacks.is_empty() {
            list = list.child(caption_block(
                &theme,
                format!(
                    "Tried in order if {} cannot be reached or refuses the login.",
                    gateway.target
                ),
            ));
        }
        if fallbacks.len() < MAX_JUMP_FALLBACKS {
            let add = entity.clone();
            let blank = fallbacks.iter().any(|item| item.target.is_empty());
            list = list.child(
                row(1.).child(
                    Button::new(
                        SharedString::from(format!("{key}-add-fallback")),
                        "Add fallback gateway",
                    )
                    .start_icon("Add")
                    .disabled(blank)
                    .on_click(move |_, _, cx| {
                        let _ = add.update(cx, |this, cx| {
                            let (gateway, mut fallbacks) = get(this);
                            fallbacks.push(SshGateway {
                                target: String::new(),
                                profile_id: None,
                            });
                            set(this, gateway, fallbacks);
                            this.changed(cx);
                        });
                    }),
                ),
            );
        }
        col = col.child(list);
        col.into_any_element()
    }
}
