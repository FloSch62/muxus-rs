//! The persistent tunnel editor (`TunnelEditorDialog.tsx`): a forwarding
//! rule bound to an SSH host, saved to start and stop without a terminal.

use std::rc::Rc;

use muxus_core::api::{ForwardType, SshKeysResponse, TunnelRecord};
use muxus_session::Backend;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render,
    StatefulInteractiveElement, Styled, Task, Transformation, Window, div, percentage, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::dialog::modal;

use crate::features::host_editor::logic::draft::HostDraft;
use crate::features::host_editor::logic::js::digits_only;
use crate::features::host_editor::logic::tunnel::{
    ConnectionMode, TunnelDraft, TunnelEditorState, auth_summary, host_summary, jump_summary,
};
use crate::features::host_editor::services::{self, EditorData};
use crate::features::host_editor::view::form::{self, AutocompleteHandlers, Form, Suggestion};
use crate::features::host_editor::view::ssh_access::{
    SshForm, SshPending, auth_section, route_section,
};
use crate::features::host_editor::widgets::inputs::{InputCache, Menus};
use crate::features::host_editor::widgets::shell::{DIALOG_MD, dialog_width};
use crate::features::host_editor::widgets::visuals::diagram;
use muxus_ui::gpui_kit::SharedString;
use muxus_ui::widgets::Button;
use muxus_ui::widgets::Chip;
use muxus_ui::widgets::form_control::FormControl;
use muxus_ui::widgets::form_control::FormControlLabel;
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::toggle_button::ToggleButton;
use muxus_ui::widgets::toggle_button::ToggleButtonGroup;
use muxus_ui::widgets::typography::VariantExt;
use muxus_ui::widgets::typography::weight;
use muxus_ui::widgets::typography::{
    body2, caption, h6, inline_caption, mono, para, subtitle2, text,
};

#[derive(Debug, Clone, PartialEq)]
pub enum TunnelEditorEvent {
    Close,
    /// Saved; `start` asks the owner to (re)start the tunnel.
    Saved {
        record: Box<TunnelRecord>,
        start: bool,
    },
    Toast {
        status: Status,
        message: String,
    },
}

pub struct TunnelEditor {
    backend: Backend,
    draft: TunnelDraft,
    data: EditorData,
    keys: Option<SshKeysResponse>,
    pending: SshPending,
    auth_open: bool,
    jumps_open: bool,
    busy: bool,
    inputs: InputCache,
    menus: Menus,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<TunnelEditorEvent> for TunnelEditor {}

impl Focusable for TunnelEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Form for TunnelEditor {
    fn inputs(&mut self) -> &mut InputCache {
        &mut self.inputs
    }

    fn menus(&mut self) -> &mut Menus {
        &mut self.menus
    }
}

impl SshForm for TunnelEditor {
    fn ssh_draft(&self) -> &HostDraft {
        &self.draft.custom
    }

    fn ssh_draft_mut(&mut self) -> &mut HostDraft {
        &mut self.draft.custom
    }

    fn ssh_changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn ssh_keys(&self) -> Option<&SshKeysResponse> {
        self.keys.as_ref()
    }

    fn ssh_data(&self) -> &EditorData {
        &self.data
    }

    fn self_profile_id(&self) -> Option<String> {
        None
    }

    fn pending(&mut self) -> &mut SshPending {
        &mut self.pending
    }
}

/// Open the tunnel editor for a new tunnel (optionally for a host) or an existing one.
pub fn open_tunnel_editor(
    window: &mut Window,
    cx: &mut App,
    backend: Backend,
    state: TunnelEditorState,
) -> Entity<TunnelEditor> {
    cx.new(|cx: &mut Context<TunnelEditor>| {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut editor = TunnelEditor {
            backend: backend.clone(),
            draft: TunnelDraft::new(&state),
            data: EditorData::default(),
            keys: None,
            pending: SshPending::default(),
            auth_open: false,
            jumps_open: false,
            busy: false,
            inputs: InputCache::default(),
            menus: Menus::default(),
            focus,
            _tasks: Vec::new(),
        };
        let task = cx.spawn(async move |this, cx| {
            let data = services::load_editor_data(&backend).await;
            let keys = backend.ssh_keys(None).await.ok();
            let _ = this.update(cx, |this, cx| {
                this.data = data;
                this.keys = keys;
                cx.notify();
            });
        });
        editor._tasks.push(task);
        editor
    })
}

impl TunnelEditor {
    /// Save (and with `start`, ask the owner to start it): the dialog's Save and Save & start.
    pub fn save(&mut self, start: bool, cx: &mut Context<Self>) {
        if self.busy || !self.draft.valid() {
            return;
        }
        self.busy = true;
        let backend = self.backend.clone();
        let draft = self.draft.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = services::save_tunnel(&backend, &draft).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    // `Saved` closes the editor too. It goes first: an owner that drops
                    // its subscription on `Close` (the forwarding panel) never heard of the
                    // save, so "Save & start" saved but neither listed nor started the tunnel.
                    Ok(record) => {
                        cx.emit(TunnelEditorEvent::Saved {
                            record: Box::new(record),
                            start,
                        });
                        cx.emit(TunnelEditorEvent::Close);
                    }
                    Err(message) => cx.emit(TunnelEditorEvent::Toast {
                        status: Status::Error,
                        message,
                    }),
                }
                cx.notify();
            });
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            if self.menus.open.is_some() {
                self.menus.close();
                cx.notify();
            } else {
                cx.emit(TunnelEditorEvent::Close);
            }
        }
    }

    fn section_heading(
        theme: &Theme,
        icon_name: &'static str,
        title: &'static str,
        detail: &'static str,
    ) -> impl IntoElement {
        let p = &theme.palette;
        div()
            .flex()
            .items_start()
            .gap(Theme::spacing(1.))
            .mb(Theme::spacing(1.25))
            .child(
                div()
                    .flex()
                    .mt(px(0.8))
                    .child(icon(icon_name, SMALL, p.text.secondary)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(text(title, subtitle2(), p.text.primary))
                    .child(inline_caption(detail, p.text.secondary)),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn accordion(
        &self,
        theme: &Theme,
        id: &'static str,
        icon_name: &'static str,
        title: &'static str,
        summary: String,
        open: bool,
        toggle: impl Fn(&mut Window, &mut App) + 'static,
        content: Option<AnyElement>,
    ) -> impl IntoElement {
        let p = &theme.palette;
        let mut panel = div()
            .flex()
            .flex_col()
            .rounded(px(theme.radius))
            .border_1()
            .border_color(p.divider)
            .overflow_hidden()
            .child(
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .min_h(px(44.))
                    .px(Theme::spacing(2.))
                    .cursor_pointer()
                    .on_click(move |_, window, cx| toggle(window, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(Theme::spacing(1.))
                            .my(Theme::spacing(0.75))
                            .child(icon(icon_name, SMALL, p.text.secondary))
                            .child(
                                text(title, body2().weight(weight::SEMIBOLD), p.text.primary)
                                    .no_truncate(),
                            )
                            .child(text(format!("· {summary}"), caption(), p.text.secondary)),
                    )
                    .child(
                        icon("ExpandMore", 24. * 13. / 14., p.action.active).with_transformation(
                            Transformation::rotate(percentage(if open { 0.5 } else { 0. })),
                        ),
                    ),
            );
        if let Some(content) = content {
            panel = panel.child(
                div()
                    .px(Theme::spacing(2.))
                    .pt(Theme::spacing(0.5))
                    .pb(Theme::spacing(2.))
                    .child(content),
            );
        }
        panel
    }
}

impl Render for TunnelEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let entity = cx.entity().downgrade();

        let name = form::text_input(
            self,
            "tunnel-name",
            window,
            cx,
            self.draft.name.clone(),
            |this, v, cx| {
                this.draft.name = v;
                cx.notify();
                None
            },
        );
        let mode = self.draft.mode;
        let mode_toggle = |value: ConnectionMode, icon_name: &'static str, label: &'static str| {
            let entity = entity.clone();
            ToggleButton::new(
                SharedString::from(format!("tunnel-mode-{label}")),
                label,
                mode == value,
            )
            .icon(icon_name)
            .on_click(move |_, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.draft.mode = value;
                    cx.notify();
                });
            })
        };
        let mut connection = div()
            .flex()
            .flex_col()
            .child(Self::section_heading(
                &theme,
                "DnsOutlined",
                "SSH connection",
                "Choose an existing host, or keep connection settings with this tunnel.",
            ))
            .child(
                div()
                    .mb(Theme::spacing(1.5))
                    .child(ToggleButtonGroup::new(vec![
                        mode_toggle(ConnectionMode::Host, "DnsOutlined", "Configured host"),
                        mode_toggle(ConnectionMode::Custom, "TuneOutlined", "Custom SSH"),
                    ])),
            );
        if mode == ConnectionMode::Host {
            let aliases: Vec<Suggestion> = self
                .data
                .ssh_config
                .as_ref()
                .map(|c| {
                    c.hosts
                        .iter()
                        .flat_map(|h| h.aliases.clone())
                        .map(|a| Suggestion::new(a.clone(), a))
                        .collect()
                })
                .unwrap_or_default();
            let target = self.draft.host_target.clone();
            let mut col = div().flex().flex_col().gap(Theme::spacing(1.25)).child(form::autocomplete(
                self,
                "tunnel-host",
                target.clone(),
                aliases,
                |input| {
                    FormField::input("tunnel-host-field", &input)
                        .label("SSH host")
                        .placeholder("Choose an alias from ~/.ssh/config")
                        .helper("Keys, jump hosts, user, and port stay linked to this host's SSH config.")
                },
                AutocompleteHandlers {
                    on_input: Rc::new(|this: &mut TunnelEditor, v, cx| {
                        this.draft.host_target = v;
                        cx.notify();
                        None
                    }),
                    on_pick: Rc::new(|this: &mut TunnelEditor, alias, window, cx| {
                        this.inputs.set("tunnel-host", &alias, window, cx);
                        this.draft.host_target = alias;
                        cx.notify();
                    }),
                    on_enter: None,
                },
                window,
                cx,
            ));
            let selected = self
                .data
                .ssh_config
                .as_ref()
                .and_then(|c| c.hosts.iter().find(|h| h.aliases.contains(&target)));
            if let Some(host) = selected {
                let summary = host_summary(host);
                let mut row = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(Theme::spacing(0.75))
                    .px(Theme::spacing(1.25))
                    .py(Theme::spacing(1.))
                    .border_1()
                    .border_color(p.divider)
                    .rounded(px(theme.radius))
                    .bg(p.action.hover)
                    .child(div().mr_auto().child(mono(
                        summary.address,
                        caption().size,
                        caption().line_height,
                        p.text.primary,
                    )))
                    .child(Chip::new(summary.auth).outlined());
                if let Some(jumps) = summary.jumps {
                    row = row.child(Chip::new(jumps).outlined().icon("AltRouteOutlined"));
                }
                col = col.child(row);
            }
            connection = connection.child(col);
        } else {
            let host = form::text_input(
                self,
                "tunnel-custom-host",
                window,
                cx,
                self.draft.custom.alias_text.clone(),
                |this, v, cx| {
                    this.draft.custom.alias_text = v;
                    cx.notify();
                    None
                },
            );
            let user = form::text_input(
                self,
                "tunnel-custom-user",
                window,
                cx,
                self.draft.custom.user.clone(),
                |this, v, cx| {
                    this.draft.custom.user = v;
                    cx.notify();
                    None
                },
            );
            let port = form::text_input(
                self,
                "tunnel-custom-port",
                window,
                cx,
                self.draft.custom.port.clone(),
                |this, v, cx| {
                    let digits = digits_only(&v);
                    let fix = (digits != v).then(|| digits.clone());
                    this.draft.custom.port = digits;
                    cx.notify();
                    fix
                },
            );
            let (auth_toggle, jumps_toggle) = (entity.clone(), entity.clone());
            let auth_content = self.auth_open.then(|| auth_section(self, window, cx));
            let jumps_content = self.jumps_open.then(|| route_section(self, window, cx));
            connection = connection.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::spacing(1.25))
                    .child(
                        div()
                            .flex()
                            .gap(Theme::spacing(1.25))
                            .child(
                                FormField::input("tunnel-custom-host-field", &host)
                                    .label("Hostname")
                                    .placeholder("server.example.com")
                                    .helper("A hostname or IP address"),
                            )
                            .child(FormField::input("tunnel-custom-user-field", &user).label("User").placeholder("Current user").width(190.))
                            .child(FormField::input("tunnel-custom-port-field", &port).label("Port").placeholder("22").width(110.)),
                    )
                    .child(self.accordion(
                        &theme,
                        "tunnel-auth",
                        "KeyOutlined",
                        "Authentication",
                        auth_summary(&self.draft.custom),
                        self.auth_open,
                        move |_, cx| {
                            let _ = auth_toggle.update(cx, |this, cx| {
                                this.auth_open = !this.auth_open;
                                cx.notify();
                            });
                        },
                        auth_content,
                    ))
                    .child(self.accordion(
                        &theme,
                        "tunnel-jumps",
                        "AltRouteOutlined",
                        "Jump hosts",
                        jump_summary(&self.draft.custom),
                        self.jumps_open,
                        move |_, cx| {
                            let _ = jumps_toggle.update(cx, |this, cx| {
                                this.jumps_open = !this.jumps_open;
                                cx.notify();
                            });
                        },
                        jumps_content,
                    ))
                    .child(para(
                        "Muxus asks when the tunnel starts. SSH password prompts can save an accepted password in the password vault; key passphrases and 2FA answers remain transient.",
                        caption(),
                        p.text.secondary,
                    )),
            );
        }

        let kind = self.draft.kind;
        let kind_toggle = |value: ForwardType, label: &'static str| {
            let entity = entity.clone();
            ToggleButton::new(
                SharedString::from(format!("tunnel-kind-{label}")),
                label,
                kind == value,
            )
            .on_click(move |_, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.draft.kind = value;
                    cx.notify();
                });
            })
        };
        let bind = form::text_input(
            self,
            "tunnel-bind",
            window,
            cx,
            self.draft.bind_port.clone(),
            |this, v, cx| {
                let digits = digits_only(&v);
                let fix = (digits != v).then(|| digits.clone());
                this.draft.bind_port = digits;
                cx.notify();
                fix
            },
        );
        let mut fields = div().flex().gap(Theme::spacing(1.25)).child(
            FormField::input("tunnel-bind-field", &bind)
                .label(if kind == ForwardType::Remote {
                    "Port on server"
                } else {
                    "Local port"
                })
                .width(160.),
        );
        if kind != ForwardType::Dynamic {
            let dest = form::text_input(
                self,
                "tunnel-dest",
                window,
                cx,
                self.draft.target_host.clone(),
                |this, v, cx| {
                    this.draft.target_host = v;
                    cx.notify();
                    None
                },
            );
            let dest_port = form::text_input(
                self,
                "tunnel-dest-port",
                window,
                cx,
                self.draft.target_port.clone(),
                |this, v, cx| {
                    let digits = digits_only(&v);
                    let fix = (digits != v).then(|| digits.clone());
                    this.draft.target_port = digits;
                    cx.notify();
                    fix
                },
            );
            fields = fields
                .child(FormField::input("tunnel-dest-field", &dest).label("Destination host"))
                .child(
                    FormField::input("tunnel-dest-port-field", &dest_port)
                        .label("Destination port")
                        .width(180.),
                );
        }
        let rule = div()
            .flex()
            .flex_col()
            .child(Self::section_heading(
                &theme,
                "SettingsEthernetOutlined",
                "Forwarding rule",
                "The saved rule stays available until you delete it.",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::spacing(1.5))
                    .child(ToggleButtonGroup::new(vec![
                        kind_toggle(ForwardType::Local, "Local (-L)"),
                        kind_toggle(ForwardType::Remote, "Remote (-R)"),
                        kind_toggle(ForwardType::Dynamic, "SOCKS proxy (-D)"),
                    ]))
                    .child(diagram(
                        &theme,
                        kind,
                        &self.draft.bind_port,
                        &self.draft.target_host,
                        &self.draft.target_port,
                        &self.draft.server_label(),
                    ))
                    .child(fields),
            );

        let switch_row = |id: &'static str,
                          checked: bool,
                          label: &'static str,
                          detail: &'static str,
                          flip: fn(&mut TunnelEditor)| {
            let entity = entity.clone();
            FormControlLabel::new(id, FormControl::Switch(checked), "")
                .label_element(
                    div()
                        .flex()
                        .flex_col()
                        .ml(Theme::spacing(1.25))
                        .child(para(label, body2(), p.text.primary))
                        .child(inline_caption(detail, p.text.secondary)),
                )
                .small()
                .flush()
                .align_start()
                .on_click(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        flip(this);
                        cx.notify();
                    });
                })
        };
        let options = div()
            .px(Theme::spacing(1.5))
            .py(Theme::spacing(1.25))
            .rounded(px(theme.radius))
            .border_1()
            .border_color(p.divider)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::spacing(1.25))
                    .child(switch_row(
                        "tunnel-autostart",
                        self.draft.auto_start,
                        "Start when Muxus starts",
                        "Brings the tunnel up as Muxus opens, without a terminal.",
                        |this| this.draft.auto_start = !this.draft.auto_start,
                    ))
                    .child(switch_row(
                        "tunnel-reconnect",
                        self.draft.auto_reconnect,
                        "Reconnect automatically",
                        "Connects again when the SSH connection drops or the host cannot be reached yet, until you stop the tunnel.",
                        |this| this.draft.auto_reconnect = !this.draft.auto_reconnect,
                    ))
                    .child(para(
                        "Both sign in with keys, the SSH agent or a password saved in the vault. A login that needs a passphrase, 2FA code or host key confirmation waits until you start the tunnel.",
                        caption(),
                        p.text.secondary,
                    )),
            );

        let content = div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(2.5))
            .mt(Theme::spacing(0.5))
            .child(
                FormField::input("tunnel-name-field", &name)
                    .label("Name")
                    .placeholder("Production database"),
            )
            .child(connection)
            .child(rule)
            .child(options);

        let valid = self.draft.valid() && !self.busy;
        let (cancel, save, start, close) = (entity.clone(), entity.clone(), entity.clone(), entity);
        let width = dialog_width(window, DIALOG_MD);
        let max_height = muxus_ui::zoom::unzoom(window.viewport_size().height) - 64.;
        let paper = muxus_ui::widgets::dialog::dialog_paper(&theme)
            .w(px(width))
            .max_h(px(max_height))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .px(Theme::spacing(3.))
                    .pt(Theme::spacing(2.))
                    .pb(Theme::spacing(1.))
                    .child(text(self.draft.title(), h6(), p.text.primary))
                    .child(div().mt(Theme::spacing(0.25)).child(text(
                        "Saved locally. Start or stop it without opening a terminal.",
                        caption().tracking(-0.2),
                        p.text.secondary,
                    ))),
            )
            .child(
                div()
                    .id("tunnel-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(Theme::spacing(3.))
                    .pb(Theme::spacing(2.5))
                    .mt(-Theme::spacing(1.5))
                    .pt(Theme::spacing(1.5))
                    .child(content),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    .gap(Theme::spacing(1.))
                    .px(Theme::spacing(3.))
                    .pt(Theme::spacing(1.))
                    .pb(Theme::spacing(2.))
                    .child(
                        Button::new("tunnel-cancel", "Cancel").on_click(move |_, _, cx| {
                            let _ = cancel.update(cx, |_, cx| cx.emit(TunnelEditorEvent::Close));
                        }),
                    )
                    .child(
                        Button::new("tunnel-save", "Save")
                            .disabled(!valid)
                            .on_click(move |_, _, cx| {
                                let _ = save.update(cx, |this, cx| this.save(false, cx));
                            }),
                    )
                    .child(
                        Button::new("tunnel-save-start", self.draft.start_label())
                            .contained()
                            .disabled(!valid)
                            .on_click(move |_, _, cx| {
                                let _ = start.update(cx, |this, cx| this.save(true, cx));
                            }),
                    ),
            );
        div()
            .id("tunnel-editor")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(modal(
                paper,
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = close.update(cx, |_, cx| cx.emit(TunnelEditorEvent::Close));
                })),
            ))
    }
}
