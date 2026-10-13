//! The SSH editor's own sections (`host-editor/GeneralSection.tsx`,
//! `AuthSection.tsx`, `RouteSection.tsx`, `ForwardsSection.tsx`,
//! `AdvancedSection.tsx`).

use std::rc::Rc;

use muxus_core::api::{ForwardType, HostOptionExtra};
use muxus_core::js::js_trim;
use muxus_core::protocol::RequestTty;
use muxus_ui::gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::icon::SMALL;
use muxus_ui::theme::{ActiveTheme, Theme, fonts};
use muxus_ui::widgets::{IconButton, Tooltip};

use super::HostEditor;
use super::form::{self, Choice};
use crate::features::host_editor::logic::advanced_options::{
    LegacyPresetState, apply_legacy_preset, legacy_preset_state, unsupported_entries,
};
use crate::features::host_editor::logic::draft::{
    ForwardX11Mode, RemoteCommandMode, Storage, draft_aliases,
};
use crate::features::host_editor::logic::forwards::{describe_forward, forward_flag};
use crate::features::host_editor::logic::help::{remote_command_help, request_tty_help, x11_help};
use crate::features::host_editor::logic::js::digits_only;
use crate::features::host_editor::services::shorten_ssh_path;
use crate::features::host_editor::widgets::section::caption_intro;
use crate::features::host_editor::widgets::section::heading;
use crate::features::host_editor::widgets::visuals::{diagram, host_color_picker};
use muxus_ui::widgets::Button;
use muxus_ui::widgets::Chip;
use muxus_ui::widgets::display::divider;
use muxus_ui::widgets::form_control::FormControl;
use muxus_ui::widgets::form_control::FormControlLabel;
use muxus_ui::widgets::form_field::{FieldBody, FormField};
use muxus_ui::widgets::toggle_button::ToggleButton;
use muxus_ui::widgets::toggle_button::ToggleButtonGroup;
use muxus_ui::widgets::typography::code_size;
use muxus_ui::widgets::typography::{body2, caption, mono, para};

const NEW_FILE: &str = "__new__";

/// `shortenPath` of `GeneralSection`.
fn shorten_path(path: &str) -> String {
    shorten_ssh_path(path)
}

/// A column with MUI `Stack spacing` gaps.
/// `<Stack spacing={spacing}>`: a column with `spacing` theme units between items.
pub(super) fn stack(spacing: f32) -> muxus_ui::gpui_kit::Div {
    div().flex().flex_col().gap(Theme::spacing(spacing))
}

/// `<Stack direction="row" spacing={spacing}>`.
pub(super) fn row(spacing: f32) -> muxus_ui::gpui_kit::Div {
    div().flex().gap(Theme::spacing(spacing))
}

/// A caption block (`Typography variant="caption"` in a Stack) in the inherited ink.
pub(super) fn caption_block(theme: &Theme, content: impl Into<SharedString>) -> impl IntoElement {
    para(content, caption(), theme.palette.text.secondary)
}

impl HostEditor {
    pub(super) fn ssh_general(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let config_backed = self.ssh.storage == Storage::Openssh;
        let root_path = self
            .data
            .ssh_config
            .as_ref()
            .map(|c| c.path.clone())
            .unwrap_or_else(|| "~/.ssh/config".into());
        let files = self
            .data
            .ssh_config
            .as_ref()
            .map(|c| c.files.clone())
            .unwrap_or_default();
        let primary = draft_aliases(&self.ssh).into_iter().next();
        let mut col = stack(2.);

        if self.is_new() {
            let helper = if config_backed {
                "Writes a standard Host block that also works with ssh in any terminal."
            } else {
                "Keeps this connection in the Muxus database and does not change ssh_config."
            };
            col = col.child(form::select(
                self,
                "storage",
                |body| {
                    FormField::new("storage-field", body)
                        .label("Save host in")
                        .helper(helper)
                },
                &self.ssh.storage.clone(),
                vec![
                    Choice::new(Storage::Muxus, "Muxus app data only"),
                    Choice::new(Storage::Openssh, "OpenSSH config"),
                ],
                None,
                window,
                cx,
                |this, storage, cx| {
                    this.ssh.storage = storage;
                    this.changed(cx);
                },
            ));
        }

        let alias = form::text_input(
            self,
            "alias",
            window,
            cx,
            self.ssh.alias_text.clone(),
            |this, v, cx| {
                this.ssh.alias_text = v;
                this.changed(cx);
                None
            },
        );
        let alias_help = if config_backed {
            match &primary {
                Some(primary) => format!("Connect with: ssh {primary}"),
                None => "The name you connect as — also works in any terminal".into(),
            }
        } else {
            "How this host appears in Muxus".into()
        };
        col = col.child(
            FormField::input("alias-field", &alias)
                .label(if config_backed { "Alias" } else { "Name" })
                .required(true)
                .helper(alias_help),
        );

        let hostname = form::text_input(
            self,
            "hostname",
            window,
            cx,
            self.ssh.hostname.clone(),
            |this, v, cx| {
                this.ssh.hostname = v;
                this.changed(cx);
                None
            },
        );
        let port = form::text_input(
            self,
            "port",
            window,
            cx,
            self.ssh.port.clone(),
            |this, v, cx| {
                let digits = digits_only(&v);
                let fix = (digits != v).then(|| digits.clone());
                this.ssh.port = digits;
                this.changed(cx);
                fix
            },
        );
        col = col.child(
            row(1.5)
                .child(
                    FormField::input("hostname-field", &hostname)
                        .label("HostName")
                        .placeholder(if config_backed {
                            "defaults to the alias"
                        } else {
                            "router.example.com"
                        })
                        .required(!config_backed),
                )
                .child(
                    FormField::input("port-field", &port)
                        .label("Port")
                        .placeholder("22")
                        .width(120.),
                ),
        );
        let user = form::text_input(
            self,
            "user",
            window,
            cx,
            self.ssh.user.clone(),
            |this, v, cx| {
                this.ssh.user = v;
                this.changed(cx);
                None
            },
        );
        col = col.child(
            FormField::input("user-field", &user)
                .label("User")
                .placeholder("current user"),
        );

        if config_backed {
            let description = form::text_input(
                self,
                "description",
                window,
                cx,
                self.ssh.description.clone(),
                |this, v, cx| {
                    this.ssh.description = v;
                    this.changed(cx);
                    None
                },
            );
            col = col.child(
                FormField::input("description-field", &description)
                    .label("Description")
                    .placeholder("shown in the host list")
                    .helper("Stored as a # comment above the Host block"),
            );

            let known = self.ssh.file.is_empty() || files.contains(&self.ssh.file);
            let select_value = if self.ssh.file.is_empty() {
                root_path.clone()
            } else if known {
                self.ssh.file.clone()
            } else {
                NEW_FILE.to_owned()
            };
            let ssh_dir = match root_path.rfind(['/', '\\']) {
                Some(index) => root_path[..=index].to_owned(),
                None => String::new(),
            };
            let mut choices = vec![Choice::new(root_path.clone(), shorten_path(&root_path))];
            for file in files.iter().filter(|f| **f != root_path) {
                choices.push(Choice::new(file.clone(), shorten_path(file)));
            }
            choices.push(Choice::new(NEW_FILE.to_owned(), "New group file…"));
            let dir = ssh_dir.clone();
            let mut file_col = stack(1.).child(form::select(
                self,
                "config-file",
                |body| FormField::new("config-file-field", body).label("Config file"),
                &select_value,
                choices,
                None,
                window,
                cx,
                move |this, value, cx| {
                    if value == NEW_FILE {
                        let name = if this.new_file_name.is_empty() {
                            "group".to_owned()
                        } else {
                            this.new_file_name.clone()
                        };
                        this.apply_new_file(&dir, name);
                    } else {
                        this.ssh.file = value;
                    }
                    this.changed(cx);
                },
            ));
            if select_value == NEW_FILE {
                let dir = ssh_dir.clone();
                let group_name = form::text_input(
                    self,
                    "group-file",
                    window,
                    cx,
                    self.new_file_name.clone(),
                    move |this, v, cx| {
                        this.apply_new_file(&dir, v);
                        this.changed(cx);
                        None
                    },
                );
                let created = if self.ssh.file.is_empty() {
                    format!("{ssh_dir}config.d/…")
                } else {
                    self.ssh.file.clone()
                };
                file_col = file_col
                    .child(FormField::input("group-file-field", &group_name).label("Group name").placeholder("work"))
                    .child(caption_block(
                        &theme,
                        format!(
                            "Creates {} and adds an Include to your config — the sidebar groups hosts by file.",
                            shorten_path(&created)
                        ),
                    ));
            }
            col = col.child(file_col);
        }

        col = col.child(divider(&theme));
        let mut startup = stack(1.5).child(caption_intro(
            &theme,
            "Startup behavior",
            "Choose what opens after authentication and whether the server should allocate a terminal.",
        ));
        let mut command_choices = vec![Choice::new(
            RemoteCommandMode::Inherit,
            if config_backed {
                "Use SSH configuration"
            } else {
                "Open a login shell"
            },
        )];
        if config_backed {
            command_choices.push(Choice::new(RemoteCommandMode::Shell, "Open a login shell"));
        }
        command_choices.push(Choice::new(
            RemoteCommandMode::Command,
            "Run a startup command",
        ));
        let mode = self.ssh.remote_command_mode;
        startup = startup.child(form::select(
            self,
            "remote-command",
            |body| {
                FormField::new("remote-command-field", body)
                    .label("After connecting")
                    .helper(remote_command_help(mode, config_backed))
            },
            &mode,
            command_choices,
            None,
            window,
            cx,
            |this, mode, cx| {
                this.ssh.remote_command_mode = mode;
                this.changed(cx);
            },
        ));
        if mode == RemoteCommandMode::Command {
            let command = form::text_input(
                self,
                "startup-command",
                window,
                cx,
                self.ssh.remote_command.clone(),
                |this, v, cx| {
                    this.ssh.remote_command = v;
                    this.changed(cx);
                    None
                },
            );
            startup = startup.child(
                FormField::input("startup-command-field", &command)
                    .label("Startup command")
                    .placeholder("tmux new -A -s main")
                    .helper("Runs on the remote host instead of its login shell."),
            );
        }
        let mut tty_choices = vec![Choice::new(
            None,
            if config_backed {
                "Use SSH configuration"
            } else {
                "Automatic"
            },
        )];
        if config_backed {
            tty_choices.push(Choice::new(Some(RequestTty::Auto), "Automatic"));
        }
        tty_choices.extend([
            Choice::new(Some(RequestTty::Yes), "Always allocate a terminal"),
            Choice::new(Some(RequestTty::No), "Do not allocate a terminal"),
            Choice::new(Some(RequestTty::Force), "Force terminal allocation"),
        ]);
        let tty = self.ssh.request_tty;
        startup = startup.child(form::select(
            self,
            "request-tty",
            |body| {
                FormField::new("request-tty-field", body)
                    .label("Terminal allocation (TTY)")
                    .helper(request_tty_help(tty, mode, config_backed))
            },
            &tty,
            tty_choices,
            None,
            window,
            cx,
            |this, tty, cx| {
                this.ssh.request_tty = tty;
                this.changed(cx);
            },
        ));
        col = col.child(startup).child(divider(&theme)).child(caption_block(
            &theme,
            if config_backed {
                "Display name, group and color are local to Muxus — they never touch your ssh config."
            } else {
                "Group and color are stored with this connection in Muxus app data."
            },
        ));
        if config_backed {
            let display = form::text_input(
                self,
                "display-name",
                window,
                cx,
                self.ssh.display_name.clone(),
                |this, v, cx| {
                    this.ssh.display_name = v;
                    this.changed(cx);
                    None
                },
            );
            col = col.child(
                FormField::input("display-name-field", &display)
                    .label("Display name")
                    .placeholder(primary.clone().unwrap_or_else(|| "the alias".into()))
                    .helper("Optional — only changes how this host appears in Muxus."),
            );
        }
        let known = self.data.known_folders();
        let group = self.ssh.group.clone();
        col = col.child(form::folder_field(
            self,
            "group",
            "Folder",
            &group,
            known,
            Some("Optional — use / to nest, e.g. Production/EU.".into()),
            None,
            false,
            window,
            cx,
            |this, group, cx| {
                this.ssh.group = group;
                this.changed(cx);
            },
        ));
        let entity = cx.entity().downgrade();
        col = col.child(host_color_picker(
            &theme,
            "host-color",
            "Color",
            self.ssh.color.as_deref(),
            false,
            30.,
            Rc::new(move |color, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.ssh.color = color;
                    this.changed(cx);
                });
            }),
        ));
        col.into_any_element()
    }

    fn apply_new_file(&mut self, ssh_dir: &str, name: String) {
        let clean: String = js_trim(&name)
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            .collect();
        self.new_file_name = name;
        if !clean.is_empty() {
            self.ssh.file = format!("{ssh_dir}config.d/{clean}");
        }
    }

    pub(super) fn ssh_auth(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        super::ssh_access::auth_section(self, window, cx)
    }

    pub(super) fn ssh_route(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        super::ssh_access::route_section(self, window, cx)
    }

    pub(super) fn ssh_forwards(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let config_backed = self.ssh.storage == Storage::Openssh;
        let server = draft_aliases(&self.ssh)
            .into_iter()
            .next()
            .or_else(|| (!self.ssh.hostname.is_empty()).then(|| self.ssh.hostname.clone()))
            .unwrap_or_else(|| "SSH server".into());
        let platform = self.data.app_info.as_ref().map(|i| i.platform.clone());
        let x11 = self.data.x11.clone();
        let mode = self.ssh.forward_x11;
        let helper = x11
            .as_ref()
            .map(|x11| x11_help(mode, x11, platform.as_deref(), config_backed));
        let x11_block = stack(1.5)
            .child(caption_intro(
                &theme,
                "Graphical applications (X11)",
                "Show the windows of remote GUI programs on this computer.",
            ))
            .child(form::select(
                self,
                "forward-x11",
                |body| {
                    FormField::new("forward-x11-field", body)
                        .label("X11 forwarding")
                        .helper_opt(helper)
                },
                &mode,
                vec![
                    Choice::new(
                        ForwardX11Mode::Inherit,
                        if config_backed {
                            "Use SSH configuration"
                        } else {
                            "Automatic"
                        },
                    ),
                    Choice::new(ForwardX11Mode::Yes, "Forward X11"),
                    Choice::new(ForwardX11Mode::No, "Do not forward X11"),
                ],
                None,
                window,
                cx,
                |this, mode, cx| {
                    this.ssh.forward_x11 = mode;
                    this.changed(cx);
                },
            ));
        let mut col = stack(2.)
            .child(x11_block)
            .child(divider(&theme))
            .child(heading(&theme, "Port forwards", 600.))
            .child(self.forward_rule_form(&server, window, cx))
            .child(divider(&theme));
        if self.ssh.forwards.is_empty() {
            col = col.child(para(
                if config_backed {
                    "No forwards configured. Rules added here are written to the Host block and start with every connection."
                } else {
                    "No forwards configured. Rules added here are stored in Muxus and start with every connection."
                },
                body2(),
                p.text.secondary,
            ));
        } else {
            let mut list = stack(0.75);
            for (index, forward) in self.ssh.forwards.clone().iter().enumerate() {
                let entity = cx.entity().downgrade();
                list = list.child(
                    row(1.)
                        .items_center()
                        .child(Chip::new(forward_flag(forward.kind)).mono().width(44.))
                        .child(div().flex_1().min_w_0().child(mono(
                            describe_forward(forward),
                            12.,
                            18.59,
                            p.text.primary,
                        )))
                        .child(
                            div()
                                .id(SharedString::from(format!("forward-remove-tip-{index}")))
                                .tooltip(Tooltip::text("Remove rule"))
                                .child(
                                    IconButton::new(
                                        SharedString::from(format!("forward-remove-{index}")),
                                        "DeleteOutlined",
                                    )
                                    .on_click(
                                        move |_, _, cx| {
                                            let _ = entity.update(cx, |this, cx| {
                                                this.ssh.forwards.remove(index);
                                                this.changed(cx);
                                            });
                                        },
                                    ),
                                ),
                        ),
                );
            }
            col = col.child(list);
        }
        col.into_any_element()
    }

    /// `ForwardRuleForm`: type toggle, live diagram, the three fields and Add.
    fn forward_rule_form(
        &mut self,
        server: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let kind = self.forward_rule.kind;
        let entity = cx.entity().downgrade();
        let toggle = |id: &'static str, value: ForwardType, label: &'static str| {
            let entity = entity.clone();
            ToggleButton::new(id, label, kind == value).on_click(move |_, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.forward_rule.kind = value;
                    cx.notify();
                });
            })
        };
        let group = ToggleButtonGroup::new(vec![
            toggle("forward-local", ForwardType::Local, "Local (-L)"),
            toggle("forward-remote", ForwardType::Remote, "Remote (-R)"),
            toggle("forward-dynamic", ForwardType::Dynamic, "SOCKS proxy (-D)"),
        ]);
        let rule = self.forward_rule.clone();
        let bind = form::text_input_enter(
            self,
            "forward-bind",
            window,
            cx,
            rule.bind_port.clone(),
            |this, v, cx| {
                let digits = digits_only(&v);
                let fix = (digits != v).then(|| digits.clone());
                this.forward_rule.bind_port = digits;
                cx.notify();
                fix
            },
            |this, window, cx| this.submit_forward(window, cx),
        );
        let mut fields = row(1.5).child(
            FormField::input("forward-bind-field", &bind)
                .label(rule.bind_label())
                .width(140.),
        );
        if kind != ForwardType::Dynamic {
            let host = form::text_input(
                self,
                "forward-host",
                window,
                cx,
                rule.target_host.clone(),
                |this, v, cx| {
                    this.forward_rule.target_host = v;
                    cx.notify();
                    None
                },
            );
            let port = form::text_input_enter(
                self,
                "forward-port",
                window,
                cx,
                rule.target_port.clone(),
                |this, v, cx| {
                    let digits = digits_only(&v);
                    let fix = (digits != v).then(|| digits.clone());
                    this.forward_rule.target_port = digits;
                    cx.notify();
                    fix
                },
                |this, window, cx| this.submit_forward(window, cx),
            );
            fields = fields
                .child(FormField::input("forward-host-field", &host).label("Target host"))
                .child(
                    FormField::input("forward-port-field", &port)
                        .label("Target port")
                        .width(140.),
                );
        }
        let add = entity.clone();
        fields = fields.child(
            div().flex().items_center().child(
                Button::new("forward-add", "Add")
                    .outlined()
                    .disabled(!rule.valid())
                    .start_icon("Add")
                    .on_click(move |_, window, cx| {
                        let _ = add.update(cx, |this, cx| this.submit_forward(window, cx));
                    }),
            ),
        );
        stack(2.)
            .child(group)
            .child(diagram(
                &theme,
                kind,
                &rule.bind_port,
                &rule.target_host,
                &rule.target_port,
                server,
            ))
            .child(fields)
            .into_any_element()
    }

    fn submit_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rule) = self.forward_rule.rule() else {
            return;
        };
        self.ssh.forwards.push(rule);
        self.forward_rule.after_submit();
        self.inputs.set("forward-bind", "", window, cx);
        self.inputs.set("forward-port", "", window, cx);
        self.changed(cx);
    }

    pub(super) fn ssh_advanced(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let config_backed = self.ssh.storage == Storage::Openssh;
        let entity = cx.entity().downgrade();
        let (console, sftp) = (entity.clone(), entity.clone());
        let mut col = stack(2.).child(
            stack(0.5)
                .child(heading(&theme, "Console compatibility", 600.))
                .child(
                    FormControlLabel::new("console-compat", FormControl::Switch(self.ssh.console_compatibility), "").label_element(para("Enable console compatibility mode", muxus_ui::widgets::typography::body1(), p.text.primary))
                    .flush()
                    .on_click(move |_, _, cx| {
                        let _ = console.update(cx, |this, cx| {
                            this.ssh.console_compatibility = !this.ssh.console_compatibility;
                            this.changed(cx);
                        });
                    }),
                )
                .child(para(
                    "Skips SFTP, shell integration, and SendEnv/SetEnv requests. TTY settings still apply.",
                    body2(),
                    p.text.secondary,
                ))
                .child(
                    FormControlLabel::new("disable-sftp", FormControl::Switch(self.ssh.disable_sftp), "").label_element(para("Disable SFTP and shell integration only", muxus_ui::widgets::typography::body1(), p.text.primary))
                    .flush()
                    .on_click(move |_, _, cx| {
                        let _ = sftp.update(cx, |this, cx| {
                            this.ssh.disable_sftp = !this.ssh.disable_sftp;
                            this.changed(cx);
                        });
                    }),
                )
                .child(para(
                    "Keeps SendEnv/SetEnv and normal TTY behavior. Console mode already disables SFTP and shell integration, regardless of this setting.",
                    body2(),
                    p.text.secondary,
                )),
        );

        if config_backed {
            let tables = self
                .data
                .app_info
                .as_ref()
                .map(|i| i.ssh_algorithms.clone());
            let mut extras = stack(1.).child(para(
                "Additional ssh_config options. Applied options are used by Muxus; others are preserved for OpenSSH.",
                body2(),
                p.text.secondary,
            ));
            while self.extra_ids.len() < self.ssh.extras.len() {
                self.extra_ids.push(self.next_row_id);
                self.next_row_id += 1;
            }
            for (index, extra) in self.ssh.extras.clone().into_iter().enumerate() {
                let id = self.extra_ids[index];
                let keyword_lower = js_trim(&extra.keyword).to_lowercase();
                let unsupported =
                    unsupported_entries(&extra.keyword, &extra.value, tables.as_ref());
                let kw = form::text_input(
                    self,
                    format!("extra-kw-{id}"),
                    window,
                    cx,
                    extra.keyword.clone(),
                    move |this, v, cx| {
                        if let Some(index) = this.extra_ids.iter().position(|i| *i == id) {
                            this.ssh.extras[index].keyword = v;
                        }
                        this.changed(cx);
                        None
                    },
                );
                let value = form::text_input(
                    self,
                    format!("extra-val-{id}"),
                    window,
                    cx,
                    extra.value.clone(),
                    move |this, v, cx| {
                        if let Some(index) = this.extra_ids.iter().position(|i| *i == id) {
                            this.ssh.extras[index].value = v;
                        }
                        this.changed(cx);
                        None
                    },
                );
                let badge: Option<AnyElement> = (!keyword_lower.is_empty()).then(|| {
                    let applied =
                        muxus_core::api::DIAL_TIME_KEYWORDS.contains(&keyword_lower.as_str());
                    let (label, tip, chip) = if applied {
                        (
                            "applied",
                            "Muxus applies this option when connecting.",
                            Chip::new("applied")
                                .outlined()
                                .palette(theme.palette.success),
                        )
                    } else {
                        (
                            "kept",
                            "Kept in the config for OpenSSH — Muxus does not use this option.",
                            Chip::new("kept").outlined(),
                        )
                    };
                    div()
                        .id(SharedString::from(format!("extra-badge-{id}-{label}")))
                        .tooltip(Tooltip::text(tip))
                        .child(chip.height(18.).font_size(10.))
                        .into_any_element()
                });
                let remove = entity.clone();
                extras = extras.child(
                    row(1.)
                        .items_start()
                        .child(
                            FormField::input(
                                SharedString::from(format!("extra-kw-field-{id}")),
                                &kw,
                            )
                            .label("Option")
                            .width(180.)
                            .no_shrink(),
                        )
                        .child(
                            FormField::input(
                                SharedString::from(format!("extra-val-field-{id}")),
                                &value,
                            )
                            .label("Value")
                            .error(!unsupported.is_empty())
                            .helper_opt((!unsupported.is_empty()).then(|| {
                                format!(
                                    "Skipped when connecting — not supported by the SSH engine: {}",
                                    unsupported.join(", ")
                                )
                            })),
                        )
                        .child(
                            div()
                                .w(px(62.))
                                .flex_none()
                                .flex()
                                .justify_center()
                                .pt(Theme::spacing(1.25))
                                .children(badge),
                        )
                        .child(
                            div().mt(Theme::spacing(0.75)).child(
                                IconButton::new(
                                    SharedString::from(format!("extra-remove-{id}")),
                                    "DeleteOutlined",
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = remove.update(cx, |this, cx| {
                                        if let Some(index) =
                                            this.extra_ids.iter().position(|i| *i == id)
                                        {
                                            this.extra_ids.remove(index);
                                            this.ssh.extras.remove(index);
                                        }
                                        this.inputs.remove(&format!("extra-kw-{id}"));
                                        this.inputs.remove(&format!("extra-val-{id}"));
                                        this.changed(cx);
                                    });
                                }),
                            ),
                        ),
                );
            }
            let legacy = legacy_preset_state(&self.ssh.extras);
            let (add, preset) = (entity.clone(), entity.clone());
            extras = extras.child(
                row(1.)
                    .child(Button::new("extra-add", "Add option").start_icon("Add").on_click(move |_, _, cx| {
                        let _ = add.update(cx, |this, cx| {
                            this.ssh.extras.push(HostOptionExtra { keyword: String::new(), value: String::new() });
                            this.extra_ids.push(this.next_row_id);
                            this.next_row_id += 1;
                            this.changed(cx);
                        });
                    }))
                    .child(
                        div()
                            .id("legacy-preset-tip")
                            .tooltip(Tooltip::text(if legacy == LegacyPresetState::Conflict {
                                "An algorithm list uses a removal (-) policy the preset will not rewrite. Adjust the lists manually."
                            } else {
                                "Adds the key exchange, host key and cipher options old console servers and network gear need. Merged into the modern defaults, so current hosts keep working."
                            }))
                            .child(Button::new("legacy-preset", "Legacy device algorithms").disabled(matches!(legacy, LegacyPresetState::Enabled | LegacyPresetState::Conflict)).start_icon("SettingsEthernet").on_click(move |_, _, cx| {
                                    let _ = preset.update(cx, |this, cx| {
                                        this.ssh.extras = apply_legacy_preset(&this.ssh.extras);
                                        // Rows may have changed values: rebuild their inputs.
                                        this.inputs.remove_prefix("extra-");
                                        this.extra_ids.clear();
                                        this.changed(cx);
                                    });
                                })),
                    ),
            );
            col = col.child(extras);

            let text_body = self
                .preview_error
                .clone()
                .unwrap_or_else(|| self.preview.clone());
            let border = if self.preview_error.is_some() {
                p.error.main
            } else {
                p.divider
            };
            col = col.child(
                stack(0.75)
                    .child(para(
                        "This is written to the config:",
                        body2(),
                        p.text.secondary,
                    ))
                    .child(
                        div()
                            .p(Theme::spacing(1.5))
                            .rounded(px(theme.radius))
                            .border_1()
                            .border_color(border)
                            .bg(p.action.hover)
                            .min_h(px(72.))
                            .font_family(fonts::MONO)
                            .text_size(px(code_size::SMALL))
                            .line_height(px(code_size::SMALL * 1.6))
                            .text_color(p.text.primary)
                            .whitespace_nowrap()
                            .children(
                                text_body
                                    .lines()
                                    .map(|line| div().child(line.to_owned()))
                                    .collect::<Vec<_>>(),
                            ),
                    ),
            );
        } else {
            col = col.child(para(
                "Raw ssh_config options are only available when a host is stored in OpenSSH config. The connection settings in the other sections are stored directly by Muxus.",
                body2(),
                p.text.secondary,
            ));
        }
        let _ = (SMALL, FieldBody::Value(SharedString::default()));
        col.into_any_element()
    }
}
