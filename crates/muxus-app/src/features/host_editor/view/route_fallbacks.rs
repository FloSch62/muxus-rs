//! Other jump routes to try, in order, when the one above fails before the target answers
//! (`FallbackRoutes` in `host-editor/RouteSection.tsx`). Each is a whole chain: one host, or
//! several joined by commas. ssh_config cannot say this, so Muxus keeps them either way. Most
//! hosts have none, so this stays one quiet button until it is asked for.

use std::rc::Rc;

use muxus_core::api::{saved_host_hop, saved_host_hop_id};
use muxus_core::js::{is_js_whitespace, js_trim};
use muxus_core::protocol::{MAX_JUMP_FALLBACKS, SavedHostSessionProfile};
use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::typography::{VariantExt, body2, code_size, mono, text};
use muxus_ui::widgets::{Button, IconButton};

use super::form::{self, AutocompleteHandlers, Suggestion};
use super::ssh::{caption_block, row, stack};
use super::ssh_access::SshForm;
use crate::features::host_editor::logic::draft::{HostDraft, Storage, draft_aliases};
use crate::hosts::saved::jump_hop_label;
use crate::hosts::system_openssh::alias_uses_system_open_ssh;
use crate::model::SavedHostProfile;

/// The hop numbers' column (`fontSize: 12`).
const INDEX_SIZE: f32 = 12.;

/// Whether a hop cannot carry this host. The ssh program only knows its own config; the
/// built-in client cannot log in to a hop that was set up for the ssh program.
pub fn hop_unavailable(
    draft: &HostDraft,
    hop: &str,
    saved: &[SavedHostProfile],
    hosts: &[crate::model::SshHostEntry],
) -> bool {
    let saved_id = saved_host_hop_id(hop);
    if draft.system_open_ssh {
        return saved_id.is_some();
    }
    match saved_id {
        Some(id) => saved
            .iter()
            .find(|host| host.id == id)
            .is_some_and(|host| host.metadata.system_open_ssh == Some(true)),
        None => alias_uses_system_open_ssh(hop, hosts),
    }
}

fn route_key(route: &[String]) -> String {
    route.join(",")
}

/// Parse a typed route; `None` when it is empty, has a space in a hop, repeats a route already
/// listed, or there is no room for another.
fn parse_route(draft: &HostDraft, value: &str) -> Option<Vec<String>> {
    let route: Vec<String> = value
        .split(',')
        .map(|hop| js_trim(hop).to_owned())
        .filter(|hop| !hop.is_empty())
        .collect();
    let taken = std::iter::once(&draft.proxy_jump)
        .chain(draft.proxy_jump_fallbacks.iter())
        .any(|existing| route_key(existing) == route_key(&route));
    let spaced = route.iter().any(|hop| hop.chars().any(is_js_whitespace));
    (!route.is_empty()
        && !spaced
        && !taken
        && draft.proxy_jump_fallbacks.len() < MAX_JUMP_FALLBACKS)
        .then_some(route)
}

fn add_route<V: SshForm>(view: &mut V, value: String, window: &mut Window, cx: &mut Context<V>) {
    let Some(route) = parse_route(view.ssh_draft(), &value) else {
        return;
    };
    view.ssh_draft_mut().proxy_jump_fallbacks.push(route);
    view.pending().fallback.clear();
    view.pending().adding_fallback = false;
    view.inputs().set("add-fallback-route", "", window, cx);
    view.ssh_changed(cx);
}

/// The fallback route list under the jump hosts, for a jump route that has hops or fallbacks.
pub fn fallback_routes<V: SshForm>(
    view: &mut V,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let entity = cx.entity().downgrade();
    let draft = view.ssh_draft().clone();
    let saved = view.ssh_data().saved_hosts.clone();
    let hop_name = |hop: &str| jump_hop_label(hop, Some(&saved));
    let label = |route: &[String]| {
        route
            .iter()
            .map(|hop| hop_name(hop))
            .collect::<Vec<_>>()
            .join(" → ")
    };
    let routes = draft.proxy_jump_fallbacks.clone();
    let count = routes.len();
    let mut col = stack(0.5);
    for (index, route) in routes.iter().enumerate() {
        let name = label(route);
        let named_saved = route.iter().any(|hop| saved_host_hop_id(hop).is_some());
        let color = if draft.system_open_ssh {
            p.text.disabled
        } else {
            p.text.primary
        };
        let (up, down, remove) = (entity.clone(), entity.clone(), entity.clone());
        col = col.child(
            row(1.)
                .items_center()
                .child(
                    div().w(px(20.)).flex().justify_end().child(
                        text(
                            "or",
                            body2().size(INDEX_SIZE).line_height(INDEX_SIZE * 1.5),
                            p.text.disabled,
                        )
                        .no_truncate(),
                    ),
                )
                .child(div().flex_1().min_w_0().child(if named_saved {
                    text(name.clone(), body2().line_height(body2().size * 1.5), color)
                        .into_any_element()
                } else {
                    mono(name.clone(), code_size::BODY, code_size::BODY * 1.5, color)
                        .into_any_element()
                }))
                .child(
                    IconButton::new(
                        SharedString::from(format!("fallback-up-{index}")),
                        "ArrowUpward",
                    )
                    .icon_size(16.)
                    .disabled(index == 0)
                    .on_click(move |_, _, cx| {
                        let _ = up.update(cx, |this, cx| {
                            this.ssh_draft_mut()
                                .proxy_jump_fallbacks
                                .swap(index, index - 1);
                            this.ssh_changed(cx);
                        });
                    }),
                )
                .child(
                    IconButton::new(
                        SharedString::from(format!("fallback-down-{index}")),
                        "ArrowDownward",
                    )
                    .icon_size(16.)
                    .disabled(index + 1 == count)
                    .on_click(move |_, _, cx| {
                        let _ = down.update(cx, |this, cx| {
                            this.ssh_draft_mut()
                                .proxy_jump_fallbacks
                                .swap(index, index + 1);
                            this.ssh_changed(cx);
                        });
                    }),
                )
                .child(
                    IconButton::new(
                        SharedString::from(format!("fallback-remove-{index}")),
                        "DeleteOutlined",
                    )
                    .on_click(move |_, _, cx| {
                        let _ = remove.update(cx, |this, cx| {
                            this.ssh_draft_mut().proxy_jump_fallbacks.remove(index);
                            this.ssh_changed(cx);
                        });
                    }),
                ),
        );
    }
    if draft.system_open_ssh {
        // The ssh program dials only the route above; keep any saved fallbacks for when the
        // option is turned off.
        col = col.child(caption_block(
            &theme,
            if routes.is_empty() {
                "Fallback routes are not available with system OpenSSH."
            } else {
                "Fallback routes are not available with system OpenSSH. They are kept for when the option is turned off."
            },
        ));
    } else if !routes.is_empty() {
        col = col.child(caption_block(
            &theme,
            if draft.storage == Storage::Openssh {
                "Tried in order if the route above fails. Saved in Muxus, not ssh_config."
            } else {
                "Tried in order if the route above fails."
            },
        ));
    }
    if count >= MAX_JUMP_FALLBACKS || draft.system_open_ssh {
        return col.into_any_element();
    }
    if !view.pending().adding_fallback {
        let start = entity.clone();
        return col
            .child(
                row(1.).child(
                    Button::new("add-fallback-route-start", "Add fallback route")
                        .start_icon("Add")
                        .on_click(move |_, _, cx| {
                            let _ = start.update(cx, |this, cx| {
                                this.pending().adding_fallback = true;
                                cx.notify();
                            });
                        }),
                ),
            )
            .into_any_element();
    }

    // Every config alias and (for a Muxus host) every other saved SSH host, minus routes taken.
    let self_aliases = draft_aliases(&draft);
    let hosts = view
        .ssh_data()
        .ssh_config
        .as_ref()
        .map(|c| c.hosts.clone())
        .unwrap_or_default();
    let self_id = view.self_profile_id();
    let taken: Vec<String> = std::iter::once(&draft.proxy_jump)
        .chain(draft.proxy_jump_fallbacks.iter())
        .map(|route| route_key(route))
        .collect();
    let mut options: Vec<String> = hosts
        .iter()
        .map(|h| h.alias.clone())
        .filter(|a| !self_aliases.contains(a))
        .collect();
    if draft.storage == Storage::Muxus {
        options.extend(
            saved
                .iter()
                .filter(|h| {
                    matches!(h.profile, SavedHostSessionProfile::Ssh(_))
                        && Some(&h.id) != self_id.as_ref()
                })
                .map(|h| saved_host_hop(&h.id)),
        );
    }
    let suggestions: Vec<Suggestion> = options
        .into_iter()
        .filter(|hop| !taken.contains(hop))
        .map(|hop| {
            let unavailable = hop_unavailable(&draft, &hop, &saved, &hosts);
            Suggestion::new(hop.clone(), hop_name(&hop)).disabled(unavailable)
        })
        .collect();
    let pending = view.pending().fallback.clone();
    let add = entity.clone();
    col.child(
        row(1.)
            .items_start()
            .pt(Theme::spacing(1.))
            .child(div().flex_1().min_w_0().child(form::autocomplete(
                view,
                "add-fallback-route",
                pending.clone(),
                suggestions,
                |input| {
                    FormField::input("add-fallback-route-field", &input)
                        .label("Fallback route")
                        .placeholder("Host, or several separated by commas")
                },
                AutocompleteHandlers {
                    on_input: Rc::new(|this: &mut V, v, cx| {
                        this.pending().fallback = v;
                        cx.notify();
                        None
                    }),
                    on_pick: Rc::new(|this: &mut V, hop, window, cx| {
                        add_route(this, hop, window, cx)
                    }),
                    on_enter: Some(Rc::new(|this: &mut V, window, cx| {
                        let pending = this.pending().fallback.clone();
                        add_route(this, pending, window, cx);
                    })),
                },
                window,
                cx,
            )))
            .child(
                div().pt(Theme::spacing(0.5)).child(
                    IconButton::new("add-fallback-route-button", "Add")
                        .medium()
                        .disabled(js_trim(&pending).is_empty())
                        .on_click(move |_, window, cx| {
                            let _ = add.update(cx, |this, cx| {
                                let pending = this.pending().fallback.clone();
                                add_route(this, pending, window, cx);
                            });
                        }),
                ),
            ),
    )
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::host_editor::logic::draft::blank_draft;

    #[test]
    fn takes_a_route_of_one_or_more_hops_once() {
        let mut draft = blank_draft("", "", Storage::Openssh);
        draft.proxy_jump = vec!["edge".into()];
        assert_eq!(
            parse_route(&draft, " dr-1 , dr-2 "),
            Some(vec!["dr-1".to_string(), "dr-2".to_string()])
        );
        // The route above, a hop with a space, nothing at all.
        assert_eq!(parse_route(&draft, "edge"), None);
        assert_eq!(parse_route(&draft, "a b"), None);
        assert_eq!(parse_route(&draft, " , "), None);
        draft.proxy_jump_fallbacks = (0..MAX_JUMP_FALLBACKS)
            .map(|i| vec![format!("b{i}")])
            .collect();
        assert_eq!(parse_route(&draft, "another"), None);
    }

    #[test]
    fn greys_hops_the_client_in_use_cannot_log_in_to() {
        let mut draft = blank_draft("", "", Storage::Muxus);
        let saved_hop = saved_host_hop("p1");
        assert!(!hop_unavailable(&draft, &saved_hop, &[], &[]));
        draft.system_open_ssh = true;
        // The ssh program cannot reach a host saved in Muxus.
        assert!(hop_unavailable(&draft, &saved_hop, &[], &[]));
        assert!(!hop_unavailable(&draft, "bastion", &[], &[]));
    }
}
