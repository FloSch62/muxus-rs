//! The SSH access sections the host editor and the tunnel editor share
//! (`host-editor/AuthSection.tsx`, `RouteSection.tsx`).

use std::rc::Rc;

use muxus_core::api::{SshKeysResponse, saved_host_hop, saved_host_hop_id};
use muxus_core::js::js_trim;
use muxus_core::protocol::{SavedHostSessionProfile, StrictHostKeyChecking};
use muxus_ui::gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, PathPromptOptions,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::ActiveTheme;
use muxus_ui::widgets::{IconButton, Tooltip};

use super::form::{self, AutocompleteHandlers, Choice, Form, Suggestion};
use super::ssh::{caption_block, row, stack};
use crate::features::host_editor::logic::draft::{
    AuthMode, HostDraft, IdentityAgentMode, RouteMode, Storage, draft_aliases,
};
use crate::features::host_editor::logic::help::{
    agent_detection_status, agent_source_help, host_verification_help,
};
use crate::features::host_editor::services::EditorData;
use crate::features::host_editor::widgets::section::caption_intro;
use crate::hosts::saved::{jump_hop_label, saved_host_display_name};
use crate::util::locale_compare;
use muxus_ui::theme::Theme;
use muxus_ui::widgets::Button;
use muxus_ui::widgets::Chip;
use muxus_ui::widgets::display::divider;
use muxus_ui::widgets::form_control::FormControl;
use muxus_ui::widgets::form_control::FormControlLabel;
use muxus_ui::widgets::form_control::label_with_caption;
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::typography::VariantExt;
use muxus_ui::widgets::typography::code_size;
use muxus_ui::widgets::typography::{body2, caption, mono, para, text};

/// The hop numbers in the route list (`fontSize: 12`).
const HOP_INDEX_SIZE: f32 = 12.;

/// What the access sections type into, besides the draft itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshPending {
    pub key: String,
    pub certificate: String,
    pub hop: String,
}

/// A view that edits an SSH [`HostDraft`] (the host editor, the tunnel editor).
pub trait SshForm: Form {
    fn ssh_draft(&self) -> &HostDraft;
    fn ssh_draft_mut(&mut self) -> &mut HostDraft;
    fn ssh_changed(&mut self, cx: &mut Context<Self>);
    fn ssh_keys(&self) -> Option<&SshKeysResponse>;
    fn ssh_data(&self) -> &EditorData;
    /// The saved host being edited, which cannot be its own jump host.
    fn self_profile_id(&self) -> Option<String>;
    fn pending(&mut self) -> &mut SshPending;
}

pub fn auth_section<V: SshForm>(
    view: &mut V,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let config_backed = view.ssh_draft().storage == Storage::Openssh;
    let entity = cx.entity().downgrade();
    let radio = |id: &'static str,
                 mode: AuthMode,
                 title: &'static str,
                 sub: &'static str,
                 current: AuthMode| {
        let entity = entity.clone();
        FormControlLabel::new(id, FormControl::Radio(current == mode), "")
            .label_element(label_with_caption(&theme, title, sub, 2., false))
            .small()
            .on_click(move |_, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.ssh_draft_mut().auth_mode = mode;
                    this.ssh_changed(cx);
                });
            })
    };
    let current = view.ssh_draft().auth_mode;
    let mut col = stack(2.).child(
        div()
            .flex()
            .flex_col()
            .child(radio(
                "auth-default",
                AuthMode::Default,
                "Agent & default keys",
                "OpenSSH order: SSH agent first, then ~/.ssh/id_* files",
                current,
            ))
            .child(radio(
                "auth-key",
                AuthMode::Key,
                "Specific key file",
                if config_backed {
                    "Writes IdentityFile — exactly these keys are offered"
                } else {
                    "Exactly these keys are offered for this Muxus connection"
                },
                current,
            ))
            .child(radio(
                "auth-password",
                AuthMode::Password,
                "Password / interactive",
                "Skips public keys (PubkeyAuthentication no); you are prompted on connect",
                current,
            )),
    );

    if current == AuthMode::Key {
        col = col.child(key_files(view, window, cx));
    }

    col = col.child(divider(&theme));
    let agent_mode = view.ssh_draft().identity_agent_mode;
    let mut agent = stack(1.5).child(caption_intro(
        &theme,
        "SSH agent",
        if current == AuthMode::Key {
            "Specific-key login does not use the agent. This source is still available for forwarding."
        } else {
            "Choose which local agent supplies keys for this host."
        },
    ));
    agent = agent.child(form::select(
        view,
        "agent-source",
        |body| {
            FormField::new("agent-source-field", body)
                .label("Agent source")
                .helper(agent_source_help(agent_mode, config_backed))
        },
        &agent_mode,
        vec![
            Choice::new(
                IdentityAgentMode::Default,
                if config_backed {
                    "Use SSH configuration"
                } else {
                    "Default environment agent"
                },
            ),
            Choice::new(
                IdentityAgentMode::Environment,
                "SSH_AUTH_SOCK environment agent",
            ),
            Choice::new(
                IdentityAgentMode::Custom,
                "Custom socket or environment variable",
            ),
            Choice::new(IdentityAgentMode::None, "Do not use an agent"),
        ],
        None,
        window,
        cx,
        |this, mode, cx| {
            this.ssh_draft_mut().identity_agent_mode = mode;
            if mode == IdentityAgentMode::None {
                this.ssh_draft_mut().forward_agent = false;
            }
            this.ssh_changed(cx);
        },
    ));
    if agent_mode == IdentityAgentMode::Custom {
        let socket = form::text_input(
            view,
            "agent-socket",
            window,
            cx,
            view.ssh_draft().identity_agent.clone(),
            |this, v, cx| {
                this.ssh_draft_mut().identity_agent = v;
                this.ssh_changed(cx);
                None
            },
        );
        agent = agent.child(
            FormField::input("agent-socket-field", &socket)
                .label("Agent socket")
                .placeholder("~/.1password/agent.sock or ${CUSTOM_SOCK}")
                .helper("Accepts a socket path, $VAR, or ${VAR}."),
        );
    }
    let forward = view.ssh_draft().forward_agent;
    let toggle = entity.clone();
    agent = agent
        .child(
            FormControlLabel::new("forward-agent", FormControl::Switch(forward), "")
                .label_element(label_with_caption(
                    &theme,
                    "Forward agent",
                    "Lets the remote host use your selected local SSH agent",
                    0.,
                    agent_mode == IdentityAgentMode::None,
                ))
                .small()
                .flush()
                .disabled(agent_mode == IdentityAgentMode::None)
                .on_click(move |_, _, cx| {
                    let _ = toggle.update(cx, |this, cx| {
                        this.ssh_draft_mut().forward_agent = !this.ssh_draft_mut().forward_agent;
                        this.ssh_changed(cx);
                    });
                }),
        )
        .child(caption_block(
            &theme,
            agent_detection_status(view.ssh_draft(), view.ssh_keys()),
        ));
    col = col.child(agent).child(divider(&theme));

    let strict = view.ssh_draft().strict_host_key_checking;
    let mut strict_choices = vec![Choice::new(
        None,
        if config_backed {
            "Use SSH configuration"
        } else {
            "Ask before trusting"
        },
    )];
    if config_backed {
        strict_choices.push(Choice::new(
            Some(StrictHostKeyChecking::Ask),
            "Ask before trusting a new host",
        ));
    }
    strict_choices.extend([
        Choice::new(
            Some(StrictHostKeyChecking::AcceptNew),
            "Trust new hosts automatically",
        ),
        Choice::new(Some(StrictHostKeyChecking::Yes), "Require a saved host key"),
        Choice::new(
            Some(StrictHostKeyChecking::No),
            "Disable strict checking (no)",
        ),
    ]);
    col = col.child(
        stack(1.5)
            .child(caption_intro(
                &theme,
                "Host-key security",
                "Control what happens when a server has not been seen before.",
            ))
            .child(form::select(
                view,
                "host-verification",
                |body| {
                    FormField::new("host-verification-field", body)
                        .label("Host verification")
                        .helper(host_verification_help(strict, config_backed))
                },
                &strict,
                strict_choices,
                None,
                window,
                cx,
                |this, value, cx| {
                    this.ssh_draft_mut().strict_host_key_checking = value;
                    this.ssh_changed(cx);
                },
            )),
    );
    let _ = p;
    col.into_any_element()
}

/// The specific-key panel: chosen keys, the key picker, certificates.
fn key_files<V: SshForm>(view: &mut V, window: &mut Window, cx: &mut Context<V>) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let config_backed = view.ssh_draft().storage == Storage::Openssh;
    let keys = view.ssh_keys().cloned();
    let key_meta = |file: &str| {
        keys.as_ref().and_then(|keys| {
            let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
            keys.keys
                .iter()
                .find(|k| k.path == file || format!("~/.ssh/{}", k.name) == file || k.name == name)
                .cloned()
        })
    };
    let mut panel = stack(1.)
        .pl(Theme::spacing(1.))
        .border_l_2()
        .border_color(p.divider);
    for (index, file) in view
        .ssh_draft()
        .identity_files
        .clone()
        .into_iter()
        .enumerate()
    {
        let meta = key_meta(&file);
        let entity = cx.entity().downgrade();
        let mut line = row(1.)
            .items_center()
            .child(icon("KeyOutlined", 16., p.text.secondary))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(mono(file.clone(), 12., 12. * 1.5, p.text.primary).no_truncate()),
            );
        if meta.as_ref().is_some_and(|m| m.in_agent) {
            line = line.child(
                div()
                    .id(SharedString::from(format!("agent-chip-{index}")))
                    .tooltip(Tooltip::text("Loaded in the SSH agent"))
                    .child(
                        Chip::new("agent")
                            .outlined()
                            .palette(theme.palette.success)
                            .icon("CheckCircleOutlined")
                            .height(20.),
                    ),
            );
        }
        if meta.as_ref().is_some_and(|m| m.encrypted) {
            line = line.child(
                div()
                    .id(SharedString::from(format!("lock-{index}")))
                    .tooltip(Tooltip::text(
                        "Passphrase-protected — you may be prompted on connect",
                    ))
                    .child(icon("LockOutlined", 14., p.text.disabled)),
            );
        }
        line = line.child(
            IconButton::new(
                SharedString::from(format!("remove-key-{index}")),
                "DeleteOutlined",
            )
            .on_click(move |_, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    if index < this.ssh_draft_mut().identity_files.len() {
                        this.ssh_draft_mut().identity_files.remove(index);
                    }
                    this.ssh_changed(cx);
                });
            }),
        );
        panel = panel.child(line);
    }

    let available: Vec<Suggestion> = view
        .ssh_keys()
        .map(|keys| {
            keys.keys
                .iter()
                .filter(|k| {
                    !view.ssh_draft().identity_files.contains(&k.path)
                        && !view
                            .ssh_draft()
                            .identity_files
                            .contains(&format!("~/.ssh/{}", k.name))
                })
                .map(|k| {
                    let detail = [k.key_type.clone(), k.comment.clone()]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · ");
                    Suggestion::new(k.path.clone(), k.name.clone())
                        .detail(if detail.is_empty() {
                            k.path.clone()
                        } else {
                            detail
                        })
                        .leading("KeyOutlined")
                })
                .collect()
        })
        .unwrap_or_default();
    let pending_key = view.pending().key.clone();
    panel = panel.child(form::autocomplete(
        view,
        "add-key",
        pending_key,
        available,
        |input| {
            FormField::input("add-key-field", &input)
                .label("Add key")
                .placeholder("pick from ~/.ssh or type a path")
        },
        AutocompleteHandlers {
            on_input: Rc::new(|this: &mut V, v, cx| {
                this.pending().key = v;
                cx.notify();
                None
            }),
            on_pick: Rc::new(|this: &mut V, path, window, cx| add_key(this, path, window, cx)),
            on_enter: Some(Rc::new(|this: &mut V, window, cx| {
                let pending = this.pending().key.clone();
                add_key(this, pending, window, cx);
            })),
        },
        window,
        cx,
    ));
    let browse = cx.entity().downgrade();
    panel = panel.child(
        row(1.)
            .items_center()
            .justify_between()
            .child(text("Key stored elsewhere?", caption(), p.text.secondary))
            .child(
                Button::new("browse-key", "Browse files…")
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
                        let browse = browse.clone();
                        cx.spawn(async move |cx| {
                            if let Some(paths) = chosen.await
                                && let Some(path) = paths.first()
                            {
                                let path = path.to_string_lossy().to_string();
                                let _ = browse.update_in(cx, |this, window, cx| {
                                    add_key(this, path, window, cx)
                                });
                            }
                        })
                        .detach();
                    }),
            ),
    );

    let mut certs = stack(1.).pt(Theme::spacing(1.)).child(
        div()
            .flex()
            .flex_col()
            .child(para("User certificates", body2(), p.text.primary))
            .child(muxus_ui::widgets::typography::inline_caption(
                format!(
                    "{}Each certificate is matched to its private key above.",
                    if config_backed {
                        "Writes CertificateFile. "
                    } else {
                        "Each saved certificate is used with this Muxus connection. "
                    }
                ),
                p.text.secondary,
            )),
    );
    for (index, file) in view
        .ssh_draft()
        .certificate_files
        .clone()
        .into_iter()
        .enumerate()
    {
        let entity = cx.entity().downgrade();
        certs = certs.child(
            row(1.)
                .items_center()
                .child(icon("VerifiedUserOutlined", 16., p.text.secondary))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(mono(file, code_size::SMALL, 18., p.text.primary).no_truncate()),
                )
                .child(
                    IconButton::new(
                        SharedString::from(format!("remove-cert-{index}")),
                        "DeleteOutlined",
                    )
                    .on_click(move |_, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            if index < this.ssh_draft_mut().certificate_files.len() {
                                this.ssh_draft_mut().certificate_files.remove(index);
                            }
                            this.ssh_changed(cx);
                        });
                    }),
                ),
        );
    }
    let pending_certificate = view.pending().certificate.clone();
    let cert = form::text_input_enter(
        view,
        "add-certificate",
        window,
        cx,
        pending_certificate,
        |this, v, cx| {
            this.pending().certificate = v;
            cx.notify();
            None
        },
        |this, window, cx| add_certificate(this, window, cx),
    );
    let add = cx.entity().downgrade();
    certs = certs.child(
        row(1.)
            .items_start()
            .child(
                FormField::input("add-certificate-field", &cert)
                    .label("Add certificate")
                    .placeholder("~/.ssh/id_ed25519-cert.pub"),
            )
            .child(
                div().mt(Theme::spacing(0.5)).child(
                    IconButton::new("add-certificate-button", "Add")
                        .medium()
                        .disabled(js_trim(&view.pending().certificate).is_empty())
                        .on_click(move |_, window, cx| {
                            let _ = add.update(cx, |this, cx| add_certificate(this, window, cx));
                        }),
                ),
            ),
    );
    panel = panel.child(certs).child(caption_block(
        &theme,
        "Only these key files are offered for login (IdentitiesOnly yes). The agent can still be forwarded after connecting.",
    ));
    panel.into_any_element()
}

fn add_key<V: SshForm>(view: &mut V, value: String, window: &mut Window, cx: &mut Context<V>) {
    let value = js_trim(&value).to_owned();
    if value.is_empty() {
        return;
    }
    view.ssh_draft_mut().identity_files.push(value);
    view.pending().key.clear();
    view.inputs().set("add-key", "", window, cx);
    view.ssh_changed(cx);
}

fn add_certificate<V: SshForm>(view: &mut V, window: &mut Window, cx: &mut Context<V>) {
    let file = js_trim(&view.pending().certificate).to_owned();
    if file.is_empty() || view.ssh_draft().certificate_files.contains(&file) {
        return;
    }
    view.ssh_draft_mut().certificate_files.push(file);
    view.pending().certificate.clear();
    view.inputs().set("add-certificate", "", window, cx);
    view.ssh_changed(cx);
}

pub fn route_section<V: SshForm>(
    view: &mut V,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let config_backed = view.ssh_draft().storage == Storage::Openssh;
    let entity = cx.entity().downgrade();
    let mode = view.ssh_draft().route_mode;
    let radio = |id: &'static str, value: RouteMode, title: &'static str, sub: &'static str| {
        let entity = entity.clone();
        FormControlLabel::new(id, FormControl::Radio(mode == value), "")
            .label_element(label_with_caption(&theme, title, sub, 2., false))
            .small()
            .on_click(move |_, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.ssh_draft_mut().route_mode = value;
                    this.ssh_changed(cx);
                });
            })
    };
    let mut col = stack(2.5).child(
        div()
            .flex()
            .flex_col()
            .child(radio(
                "route-direct",
                RouteMode::Direct,
                "Direct connection",
                "Connect to the target without a proxy",
            ))
            .child(radio(
                "route-jump",
                RouteMode::Jump,
                "Jump hosts",
                if config_backed {
                    "Writes ProxyJump — connect through one or more SSH bastions"
                } else {
                    "Connect through one or more SSH bastions"
                },
            ))
            .child(radio(
                "route-command",
                RouteMode::Command,
                "Proxy command",
                if config_backed {
                    "Writes ProxyCommand — use a command's stdin/stdout as the transport"
                } else {
                    "Use a command's stdin/stdout as the transport"
                },
            )),
    );

    let saved = view.ssh_data().saved_hosts.clone();
    let hop_name = |hop: &str| jump_hop_label(hop, Some(&saved));
    let hosts = view
        .ssh_data()
        .ssh_config
        .as_ref()
        .map(|c| c.hosts.clone())
        .unwrap_or_default();
    let known_alias = |hop: &str| hosts.iter().any(|h| h.aliases.iter().any(|a| a == hop));
    let target = draft_aliases(view.ssh_draft())
        .into_iter()
        .next()
        .or_else(|| {
            (!view.ssh_draft().hostname.is_empty()).then(|| view.ssh_draft().hostname.clone())
        })
        .unwrap_or_else(|| "target".into());

    let arrow = || icon("ArrowForward", 14., p.text.disabled);
    let mut path = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(Theme::spacing(0.75))
        .child(
            Chip::new("This computer")
                .outlined()
                .icon("ComputerOutlined"),
        );
    path = path.child(arrow());
    if mode == RouteMode::Jump {
        for (index, hop) in view.ssh_draft().proxy_jump.iter().enumerate() {
            let tip = if saved_host_hop_id(hop).is_some() {
                "Jump host saved in Muxus"
            } else if known_alias(hop) {
                "Jump host from your config"
            } else {
                "Ad-hoc jump host"
            };
            path = path
                .child(
                    div()
                        .id(SharedString::from(format!("hop-chip-{index}")))
                        .tooltip(Tooltip::text(tip))
                        .child(
                            Chip::new(hop_name(hop))
                                .outlined()
                                .palette(theme.palette.primary)
                                .icon("DnsOutlined"),
                        ),
                )
                .child(arrow());
        }
    }
    if mode == RouteMode::Command {
        path = path
            .child(
                Chip::new("ProxyCommand")
                    .outlined()
                    .palette(theme.palette.primary)
                    .icon("TerminalOutlined"),
            )
            .child(arrow());
    }
    path = path.child(Chip::new(target.clone()).icon("DnsOutlined"));
    col = col.child(path);

    if mode == RouteMode::Direct {
        col = col.child(para(
            format!("Muxus opens the TCP connection to {target} itself."),
            body2(),
            p.text.secondary,
        ));
    }

    if mode == RouteMode::Jump {
        let mut list = stack(0.5);
        let count = view.ssh_draft().proxy_jump.len();
        for (index, hop) in view.ssh_draft().proxy_jump.clone().into_iter().enumerate() {
            let name = hop_name(&hop);
            let saved_hop = saved_host_hop_id(&hop).is_some();
            let (up, down, remove) = (entity.clone(), entity.clone(), entity.clone());
            list = list.child(
                row(1.)
                    .items_center()
                    .child(
                        div().w(px(20.)).flex().justify_end().child(
                            text(
                                format!("{}.", index + 1),
                                body2()
                                    .size(HOP_INDEX_SIZE)
                                    .line_height(HOP_INDEX_SIZE * 1.5),
                                p.text.disabled,
                            )
                            .no_truncate(),
                        ),
                    )
                    .child(div().flex_1().min_w_0().child(if saved_hop {
                        text(
                            name,
                            body2().line_height(body2().size * 1.5),
                            p.text.primary,
                        )
                        .into_any_element()
                    } else {
                        mono(name, code_size::BODY, code_size::BODY * 1.5, p.text.primary)
                            .into_any_element()
                    }))
                    .child(
                        IconButton::new(
                            SharedString::from(format!("hop-up-{index}")),
                            "ArrowUpward",
                        )
                        .icon_size(16.)
                        .disabled(index == 0)
                        .on_click(move |_, _, cx| {
                            let _ = up.update(cx, |this, cx| {
                                this.ssh_draft_mut().proxy_jump.swap(index, index - 1);
                                this.ssh_changed(cx);
                            });
                        }),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("hop-down-{index}")),
                            "ArrowDownward",
                        )
                        .icon_size(16.)
                        .disabled(index + 1 == count)
                        .on_click(move |_, _, cx| {
                            let _ = down.update(cx, |this, cx| {
                                this.ssh_draft_mut().proxy_jump.swap(index, index + 1);
                                this.ssh_changed(cx);
                            });
                        }),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("hop-remove-{index}")),
                            "DeleteOutlined",
                        )
                        .on_click(move |_, _, cx| {
                            let _ = remove.update(cx, |this, cx| {
                                this.ssh_draft_mut().proxy_jump.remove(index);
                                this.ssh_changed(cx);
                            });
                        }),
                    ),
            );
        }
        let self_aliases = draft_aliases(view.ssh_draft());
        let mut suggestions: Vec<Suggestion> = hosts
            .iter()
            .map(|h| h.alias.clone())
            .filter(|a| !self_aliases.contains(a) && !view.ssh_draft().proxy_jump.contains(a))
            .map(|a| Suggestion::new(a.clone(), a))
            .collect();
        let self_id = view.self_profile_id();
        let mut saved_options: Vec<_> = if view.ssh_draft().storage == Storage::Muxus {
            saved
                .iter()
                .filter(|h| {
                    matches!(h.profile, SavedHostSessionProfile::Ssh(_))
                        && Some(&h.id) != self_id.as_ref()
                })
                .collect()
        } else {
            Vec::new()
        };
        saved_options
            .sort_by(|a, b| locale_compare(saved_host_display_name(a), saved_host_display_name(b)));
        let saved_hops: Vec<Suggestion> = saved_options
            .into_iter()
            .map(|h| saved_host_hop(&h.id))
            .filter(|hop| !view.ssh_draft().proxy_jump.contains(hop))
            .map(|hop| {
                Suggestion::new(hop.clone(), jump_hop_label(&hop, Some(&saved)))
                    .group("Saved in Muxus")
            })
            .collect();
        if !saved_hops.is_empty() {
            suggestions = suggestions
                .into_iter()
                .map(|s| s.group("From your SSH config"))
                .collect();
        }
        suggestions.extend(saved_hops);
        let add = entity.clone();
        let pending_hop = view.pending().hop.clone();
        let hop_disabled = js_trim(&pending_hop).is_empty();
        list = list.child(
            row(1.)
                .items_start()
                .pt(Theme::spacing(1.))
                .child(div().flex_1().min_w_0().child(form::autocomplete(
                    view,
                    "add-hop",
                    pending_hop,
                    suggestions,
                    move |input| {
                        FormField::input("add-hop-field", &input)
                            .label("Add jump host")
                            .placeholder(if config_backed {
                                "alias from config, or user@host:port"
                            } else {
                                "Muxus host, alias from config, or user@host:port"
                            })
                    },
                    AutocompleteHandlers {
                        on_input: Rc::new(|this: &mut V, v, cx| {
                            this.pending().hop = v;
                            cx.notify();
                            None
                        }),
                        on_pick: Rc::new(|this: &mut V, hop, window, cx| {
                            add_hop(this, hop, window, cx)
                        }),
                        on_enter: Some(Rc::new(|this: &mut V, window, cx| {
                            let pending = this.pending().hop.clone();
                            add_hop(this, pending, window, cx);
                        })),
                    },
                    window,
                    cx,
                )))
                .child(
                    div().pt(Theme::spacing(0.5)).child(
                        IconButton::new("add-hop-button", "Add")
                            .medium()
                            .disabled(hop_disabled)
                            .on_click(move |_, window, cx| {
                                let _ = add.update(cx, |this, cx| {
                                    let pending = this.pending().hop.clone();
                                    add_hop(this, pending, window, cx);
                                });
                            }),
                    ),
                ),
        );
        list = list.child(caption_block(
            &theme,
            if view.ssh_draft().storage == Storage::Muxus {
                "Each hop is authenticated and host-key verified with its own settings: a Muxus host with what it saved, any other hop through your SSH config."
            } else {
                "Each hop is authenticated and host-key verified using its own SSH config."
            },
        ));
        col = col.child(list);
    }

    if mode == RouteMode::Command {
        let command = form::text_input(
            view,
            "proxy-command",
            window,
            cx,
            view.ssh_draft().proxy_command.clone(),
            |this, v, cx| {
                this.ssh_draft_mut().proxy_command = v;
                this.ssh_changed(cx);
                None
            },
        );
        col = col.child(
            FormField::input("proxy-command-field", &command)
                .label("ProxyCommand")
                .placeholder("cloudflared access ssh --hostname %h")
                .helper("Supports OpenSSH tokens %% (percent), %h (host), %n (alias), %p (port), and %r (user)."),
        );
    }
    col.into_any_element()
}

fn add_hop<V: SshForm>(view: &mut V, value: String, window: &mut Window, cx: &mut Context<V>) {
    let value = js_trim(&value).to_owned();
    if value.is_empty()
        || value
            .chars()
            .any(|c| c == ',' || muxus_core::js::is_js_whitespace(c))
        || view.ssh_draft().proxy_jump.contains(&value)
    {
        return;
    }
    view.ssh_draft_mut().proxy_jump.push(value);
    view.pending().hop.clear();
    view.inputs().set("add-hop", "", window, cx);
    view.ssh_changed(cx);
}
