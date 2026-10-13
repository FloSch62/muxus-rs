//! Editing many hosts at once (`HostBulkEditDialog.tsx`). Each field starts
//! from what the hosts share, or reads "Multiple values" where they differ,
//! and only the fields the user touches are written.

use std::collections::HashMap;
use std::rc::Rc;

use muxus_core::api::SessionLoggingPolicy;
use muxus_core::protocol::StrictHostKeyChecking;
use muxus_session::Backend;
use muxus_session::login_sequence::editor::{
    LoginSequenceDraft, LoginSequenceMode, LoginStepKind, new_login_step,
};
use muxus_term::paste::PasteDelayKind;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, ScrollHandle,
    SharedString, Styled, Task, Window, div, px,
};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::ButtonColor;
use muxus_ui::widgets::dialog::modal;

use crate::features::host_editor::logic::bulk_edit::{
    BulkEditPlan, BulkFieldSummary, BulkHostChanges, BulkHostField, BulkSummary, BulkTriState,
    BulkValue, bulk_changes_problem, bulk_edit_plan, describe, host_logging_key, is_terminal_host,
    summarize_bulk_values,
};
use crate::features::host_editor::logic::js::digits_only;
use crate::features::host_editor::logic::session_logging::{
    HostSessionLoggingDraft, host_session_logging_draft,
};
use crate::features::host_editor::services::{self, EditorData};
use crate::features::host_editor::view::form::{self, Choice, Form};
use crate::features::host_editor::view::shared::{
    color_override, logging_section, login_sequence_editor, paste_delay_field,
};
use crate::features::host_editor::widgets::inputs::{InputCache, Menus};
use crate::features::host_editor::widgets::section::intro;
use crate::features::host_editor::widgets::shell::{
    DIALOG_MD, SectionTab, ShellProps, editor_shell,
};
use crate::features::host_editor::widgets::visuals::host_color_picker;
use crate::hosts::managed::{ManagedHost, managed_host_display_name, managed_host_key};
use muxus_ui::widgets::Alert;
use muxus_ui::widgets::Button;
use muxus_ui::widgets::display::AlertVariant;
use muxus_ui::widgets::display::alert_ink;
use muxus_ui::widgets::display::divider;
use muxus_ui::widgets::form_control::FormControl;
use muxus_ui::widgets::form_control::FormControlLabel;
use muxus_ui::widgets::form_control::label_with_caption;
use muxus_ui::widgets::form_field::{FieldBody, FormField};
use muxus_ui::widgets::typography::{body2, caption, para, text};
use muxus_ui::widgets::typography::{button, rem};

#[derive(Debug, Clone, PartialEq)]
pub enum BulkEditEvent {
    Close,
    Toast { status: Status, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Organize,
    Connection,
    Appearance,
    Highlighting,
    Logging,
    Login,
}

const SECTIONS: [(Section, &str, &str, &str, &[BulkHostField]); 6] = [
    (
        Section::Organize,
        "organize",
        "Folder & color",
        "FolderOutlined",
        &[BulkHostField::Group, BulkHostField::Color],
    ),
    (
        Section::Connection,
        "connection",
        "SSH connection",
        "DnsOutlined",
        &[
            BulkHostField::User,
            BulkHostField::Port,
            BulkHostField::StrictHostKeyChecking,
            BulkHostField::ForwardAgent,
            BulkHostField::ForwardX11,
            BulkHostField::ConsoleCompatibility,
            BulkHostField::DisableSftp,
        ],
    ),
    (
        Section::Appearance,
        "appearance",
        "Terminal appearance",
        "PaletteOutlined",
        &[
            BulkHostField::TerminalScheme,
            BulkHostField::TerminalFontColor,
            BulkHostField::TerminalBackgroundColor,
            BulkHostField::CommandButtonGroup,
            BulkHostField::PasteLineDelayMs,
            BulkHostField::PasteCharDelayMs,
        ],
    ),
    (
        Section::Highlighting,
        "highlighting",
        "Highlighting",
        "HighlightOutlined",
        &[
            BulkHostField::HighlightProfileId,
            BulkHostField::HighlightInheritGlobal,
        ],
    ),
    (
        Section::Logging,
        "logging",
        "Session logging",
        "HistoryOutlined",
        &[BulkHostField::SessionLogging],
    ),
    (
        Section::Login,
        "login",
        "Login sequence",
        "LoginOutlined",
        &[BulkHostField::LoginSequence],
    ),
];

const MULTIPLE_VALUES: &str = "Multiple values";

pub struct BulkEditDialog {
    backend: Backend,
    host_keys: Vec<String>,
    data: EditorData,
    loaded: bool,
    policies: HashMap<String, SessionLoggingPolicy>,
    default_policy: Option<SessionLoggingPolicy>,
    changes: BulkHostChanges,
    section: Section,
    busy: bool,
    inputs: InputCache,
    menus: Menus,
    scroll: ScrollHandle,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<BulkEditEvent> for BulkEditDialog {}

impl Focusable for BulkEditDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Form for BulkEditDialog {
    fn inputs(&mut self) -> &mut InputCache {
        &mut self.inputs
    }

    fn menus(&mut self) -> &mut Menus {
        &mut self.menus
    }
}

/// Open the bulk editor on hosts by their managed host key (`ssh:alias`, `profile:id`).
pub fn open_bulk_edit(
    window: &mut Window,
    cx: &mut App,
    backend: Backend,
    host_keys: Vec<String>,
) -> Entity<BulkEditDialog> {
    cx.new(|cx: &mut Context<BulkEditDialog>| {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut dialog = BulkEditDialog {
            backend,
            host_keys,
            data: EditorData::default(),
            loaded: false,
            policies: HashMap::new(),
            default_policy: None,
            changes: BulkHostChanges::default(),
            section: Section::Organize,
            busy: false,
            inputs: InputCache::default(),
            menus: Menus::default(),
            scroll: ScrollHandle::new(),
            focus,
            _tasks: Vec::new(),
        };
        dialog.reload(cx);
        dialog
    })
}

/// What one field's control shows.
struct FieldState<T> {
    value: Option<T>,
    mixed: bool,
    changed: bool,
    count: usize,
    summary: BulkFieldSummary<()>,
}

impl BulkEditDialog {
    fn reload(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let keys = self.host_keys.clone();
        let task = cx.spawn(async move |this, cx| {
            let data = services::load_editor_data(&backend).await;
            let hosts = resolve(&data, &keys);
            let terminal: Vec<ManagedHost> = hosts
                .iter()
                .filter(|h| is_terminal_host(h))
                .cloned()
                .collect();
            let policies = services::logging_policies(&backend, &terminal).await;
            let default_policy = if terminal.is_empty() {
                None
            } else {
                services::logging_policy(&backend, "*").await
            };
            let _ = this.update(cx, |this, cx| {
                this.data = data;
                this.policies = policies;
                this.default_policy = default_policy;
                this.loaded = true;
                cx.notify();
            });
        });
        self._tasks.push(task);
    }

    /// The hosts, resolved live, so a host deleted meanwhile simply drops out.
    fn hosts(&self) -> Vec<ManagedHost> {
        resolve(&self.data, &self.host_keys)
    }

    fn summary(&self) -> BulkSummary {
        summarize_bulk_values(&self.hosts(), &self.policies)
    }

    fn plan(&self) -> BulkEditPlan {
        bulk_edit_plan(&self.hosts(), &self.changes, &self.policies)
    }

    fn field<T: BulkValue>(
        &self,
        field: BulkHostField,
        summary: &BulkFieldSummary<T>,
        change: &Option<T>,
        plan: &BulkEditPlan,
    ) -> FieldState<T> {
        let changed = change.is_some();
        FieldState {
            value: match change {
                Some(value) => Some(value.clone()),
                None => match summary {
                    BulkFieldSummary::Same { value, .. } => Some(value.clone()),
                    _ => None,
                },
            },
            mixed: !changed && matches!(summary, BulkFieldSummary::Mixed { .. }),
            changed,
            count: plan.field_counts.get(&field).copied().unwrap_or(0),
            summary: match summary {
                BulkFieldSummary::None => BulkFieldSummary::None,
                BulkFieldSummary::Loading { count } => BulkFieldSummary::Loading { count: *count },
                BulkFieldSummary::Same { count, .. } => BulkFieldSummary::Same {
                    count: *count,
                    value: (),
                },
                BulkFieldSummary::Mixed { count } => BulkFieldSummary::Mixed { count: *count },
            },
        }
    }

    /// Set a field; choosing what every host already has is no change at all.
    fn set<T: BulkValue>(
        &mut self,
        field: BulkHostField,
        value: T,
        get: fn(&BulkSummary) -> &BulkFieldSummary<T>,
        slot: fn(&mut BulkHostChanges) -> &mut Option<T>,
        cx: &mut Context<Self>,
    ) {
        let summary = self.summary();
        let same = matches!(get(&summary), BulkFieldSummary::Same { value: current, .. } if current.same(&value));
        if same {
            self.changes.remove(field);
        } else {
            *slot(&mut self.changes) = Some(value);
        }
        cx.notify();
    }

    fn reset(&mut self, field: BulkHostField, cx: &mut Context<Self>) {
        self.changes.remove(field);
        cx.notify();
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let plan = self.plan();
        if plan.hosts.is_empty() || bulk_changes_problem(&self.changes).is_some() {
            return;
        }
        self.busy = true;
        let backend = self.backend.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = services::apply_bulk_edit(&backend, &plan).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(result) => {
                        let (ok, message) = result.message();
                        cx.emit(BulkEditEvent::Toast {
                            status: if ok { Status::Success } else { Status::Error },
                            message,
                        });
                        if result.failed == 0 {
                            cx.emit(BulkEditEvent::Close);
                        } else {
                            // A partial failure keeps the dialog: once the hosts
                            // reload, the plan holds what is still missing.
                            this.reload(cx);
                        }
                    }
                    Err(message) => cx.emit(BulkEditEvent::Toast {
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
                cx.emit(BulkEditEvent::Close);
            }
        }
    }

    /// `FieldStatus`: what applying would do with a field.
    #[allow(clippy::too_many_arguments)]
    fn status<T>(
        &self,
        theme: &Theme,
        field: BulkHostField,
        state: &FieldState<T>,
        inset: bool,
        empty: Option<&str>,
        cleared: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let p = &theme.palette;
        let ml = if inset { 14. } else { 0. };
        if state.changed {
            let mut message = String::new();
            if cleared && let Some(empty) = empty {
                message.push_str(empty);
                message.push(' ');
            }
            if state.count == 0 {
                message.push_str("Every host already has this.");
            } else {
                message.push_str(&format!(
                    "Changes {} host{}.",
                    state.count,
                    if state.count == 1 { "" } else { "s" }
                ));
            }
            let entity = cx.entity().downgrade();
            return Some(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(Theme::spacing(1.))
                    .mt(Theme::spacing(0.5))
                    .ml(px(ml))
                    .min_h(px(26.))
                    .child(text(message, caption(), p.primary.main).no_truncate())
                    .child(
                        Button::new(SharedString::from(format!("undo-{field:?}")), "Undo")
                            .on_click(move |_, _, cx| {
                                let _ = entity.update(cx, |this, cx| this.reset(field, cx));
                            })
                            .min_width(0.),
                    )
                    .into_any_element(),
            );
        }
        if state.mixed {
            return Some(
                div()
                    .mt(Theme::spacing(0.5))
                    .ml(px(ml))
                    .child(text(
                        "Multiple values — each host keeps its own.",
                        caption(),
                        p.text.secondary,
                    ))
                    .into_any_element(),
            );
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn bulk_select<T: Clone + PartialEq + 'static>(
        &mut self,
        key: &str,
        label: &str,
        value: Option<T>,
        mixed: bool,
        options: Vec<(T, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_select: impl Fn(&mut Self, T, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let current = value.clone();
        let display = if mixed {
            Some(FieldBody::Muted(MULTIPLE_VALUES.into()))
        } else {
            current
                .as_ref()
                .and_then(|v| options.iter().find(|(o, _)| o == v))
                .map(|(_, label)| FieldBody::Value(label.clone().into()))
        };
        let choices: Vec<Choice<Option<T>>> = options
            .into_iter()
            .map(|(v, l)| Choice::new(Some(v), l))
            .collect();
        let label = label.to_owned();
        form::select(
            self,
            key,
            move |body| {
                FormField::new(SharedString::from(format!("{label}-bulk")), body)
                    .label(label.clone())
                    .shrink(true)
            },
            &if mixed { None } else { current },
            choices,
            display,
            window,
            cx,
            move |this, value, cx| {
                if let Some(value) = value {
                    on_select(this, value, cx);
                }
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn bulk_checkbox(
        &self,
        theme: &Theme,
        id: &'static str,
        state: &FieldState<bool>,
        label: &str,
        description: &str,
        on_toggle: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let checked = !state.mixed && state.value == Some(true);
        let entity = cx.entity().downgrade();
        let on_toggle = Rc::new(on_toggle);
        FormControlLabel::new(id, FormControl::Checkbox(checked), "")
            .label_element(label_with_caption(
                theme,
                label.to_owned(),
                description.to_owned(),
                2.,
                false,
            ))
            .indeterminate(state.mixed)
            .on_click(move |_, _, cx| {
                let on_toggle = on_toggle.clone();
                let _ = entity.update(cx, |this, cx| on_toggle(this, !checked, cx));
            })
            .into_any_element()
    }

    fn content(
        &mut self,
        section: Section,
        total: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let summary = self.summary();
        let plan = self.plan();
        let col = || div().flex().flex_col().gap(Theme::spacing(2.5));
        match section {
            Section::Organize => {
                let group = self.field(
                    BulkHostField::Group,
                    &summary.group,
                    &self.changes.group,
                    &plan,
                );
                let color = self.field(
                    BulkHostField::Color,
                    &summary.color,
                    &self.changes.color,
                    &plan,
                );
                let known = self.data.known_folders();
                let folder = form::folder_field(
                    self,
                    "bulk-group",
                    "Folder",
                    &group.value.clone().unwrap_or_default(),
                    known,
                    Some("Use / to nest; leave empty for no folder.".into()),
                    group.mixed.then_some(MULTIPLE_VALUES),
                    false,
                    window,
                    cx,
                    |this, value, cx| {
                        this.set(
                            BulkHostField::Group,
                            value,
                            |s| &s.group,
                            |c| &mut c.group,
                            cx,
                        )
                    },
                );
                let entity = cx.entity().downgrade();
                let picker = host_color_picker(
                    &theme,
                    "bulk-color",
                    "Color",
                    color.value.clone().flatten().as_deref(),
                    color.mixed,
                    30.,
                    Rc::new(move |value, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.set(
                                BulkHostField::Color,
                                value,
                                |s| &s.color,
                                |c| &mut c.color,
                                cx,
                            )
                        });
                    }),
                );
                col()
                    .child(intro(&theme, "Folder & color", "Display name, folder and color are local to Muxus — they never touch your ssh config."))
                    .child(div().child(folder).children(self.status(&theme, BulkHostField::Group, &group, true, None, false, cx)))
                    .child(div().child(picker).children(self.status(&theme, BulkHostField::Color, &color, false, None, false, cx)))
                    .into_any_element()
            }
            Section::Connection => {
                let user = self.field(
                    BulkHostField::User,
                    &summary.user,
                    &self.changes.user,
                    &plan,
                );
                let port = self.field(
                    BulkHostField::Port,
                    &summary.port,
                    &self.changes.port,
                    &plan,
                );
                let strict = self.field(
                    BulkHostField::StrictHostKeyChecking,
                    &summary.strict_host_key_checking,
                    &self.changes.strict_host_key_checking,
                    &plan,
                );
                let agent = self.field(
                    BulkHostField::ForwardAgent,
                    &summary.forward_agent,
                    &self.changes.forward_agent,
                    &plan,
                );
                let x11 = self.field(
                    BulkHostField::ForwardX11,
                    &summary.forward_x11,
                    &self.changes.forward_x11,
                    &plan,
                );
                let console = self.field(
                    BulkHostField::ConsoleCompatibility,
                    &summary.console_compatibility,
                    &self.changes.console_compatibility,
                    &plan,
                );
                let sftp = self.field(
                    BulkHostField::DisableSftp,
                    &summary.disable_sftp,
                    &self.changes.disable_sftp,
                    &plan,
                );
                let ssh_count = user.summary.count();
                let hosts = self.hosts();
                let open_ssh = hosts
                    .iter()
                    .filter(|h| matches!(h, ManagedHost::Ssh(_)))
                    .count();
                let muxus = ssh_count.saturating_sub(open_ssh);
                let storage = [
                    (open_ssh > 0).then(|| format!("{open_ssh} in ssh_config")),
                    (muxus > 0).then(|| format!("{muxus} in Muxus app data")),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" and ");
                let user_input = form::text_input(
                    self,
                    "bulk-user",
                    window,
                    cx,
                    user.value.clone().unwrap_or_default(),
                    |this, v, cx| {
                        this.set(BulkHostField::User, v, |s| &s.user, |c| &mut c.user, cx);
                        None
                    },
                );
                let port_input = form::text_input(
                    self,
                    "bulk-port",
                    window,
                    cx,
                    port.value.clone().unwrap_or_default(),
                    |this, v, cx| {
                        let digits = digits_only(&v);
                        let fix = (digits != v).then(|| digits.clone());
                        this.set(
                            BulkHostField::Port,
                            digits,
                            |s| &s.port,
                            |c| &mut c.port,
                            cx,
                        );
                        fix
                    },
                );
                let strict_value = strict
                    .value
                    .map(|v| v.map(strict_name).unwrap_or("inherit"));
                let strict_select = self.bulk_select(
                    "bulk-strict",
                    "Host verification",
                    strict_value.map(str::to_owned),
                    strict.mixed,
                    vec![
                        ("inherit".into(), "Use the default".into()),
                        ("ask".into(), "Ask before trusting a new host".into()),
                        ("accept-new".into(), "Trust new hosts automatically".into()),
                        ("yes".into(), "Require a saved host key".into()),
                        ("no".into(), "Disable strict checking (no)".into()),
                    ],
                    window,
                    cx,
                    |this, v: String, cx| {
                        let value = parse_strict(&v);
                        this.set(
                            BulkHostField::StrictHostKeyChecking,
                            value,
                            |s| &s.strict_host_key_checking,
                            |c| &mut c.strict_host_key_checking,
                            cx,
                        );
                    },
                );
                let tri = |label: &str| {
                    vec![
                        (BulkTriState::Inherit, "Use the default".to_owned()),
                        (BulkTriState::Yes, label.to_owned()),
                        (BulkTriState::No, format!("Do not {}", label.to_lowercase())),
                    ]
                };
                let agent_select = self.bulk_select(
                    "bulk-agent",
                    "Agent forwarding",
                    agent.value,
                    agent.mixed,
                    tri("Forward agent"),
                    window,
                    cx,
                    |this, v, cx| {
                        this.set(
                            BulkHostField::ForwardAgent,
                            v,
                            |s| &s.forward_agent,
                            |c| &mut c.forward_agent,
                            cx,
                        )
                    },
                );
                let x11_select = self.bulk_select(
                    "bulk-x11",
                    "X11 forwarding",
                    x11.value,
                    x11.mixed,
                    tri("Forward X11"),
                    window,
                    cx,
                    |this, v, cx| {
                        this.set(
                            BulkHostField::ForwardX11,
                            v,
                            |s| &s.forward_x11,
                            |c| &mut c.forward_x11,
                            cx,
                        )
                    },
                );
                let user_cleared = user.value.as_deref() == Some("");
                let port_cleared = port.value.as_deref() == Some("");
                col()
                    .child(intro(
                        &theme,
                        "SSH connection",
                        describe(ssh_count, total, "SSH host", &format!("Each host keeps its storage ({storage}), and only the changed options are rewritten.")),
                    ))
                    .child(
                        div()
                            .flex()
                            .gap(Theme::spacing(1.5))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        FormField::input("bulk-user-field", &user_input)
                                            .label("User")
                                            .shrink(true)
                                            .placeholder(if user.mixed { MULTIPLE_VALUES } else { "Not set" }),
                                    )
                                    .children(self.status(&theme, BulkHostField::User, &user, true, Some("Removes the user, so each host falls back to its defaults."), user_cleared, cx)),
                            )
                            .child(
                                div()
                                    .w(px(200.))
                                    .flex_none()
                                    .child(
                                        FormField::input("bulk-port-field", &port_input)
                                            .label("Port")
                                            .shrink(true)
                                            .placeholder(if port.mixed { MULTIPLE_VALUES } else { "22" }),
                                    )
                                    .children(self.status(&theme, BulkHostField::Port, &port, true, Some("Removes the port."), port_cleared, cx)),
                            ),
                    )
                    .child(div().child(strict_select).children(self.status(&theme, BulkHostField::StrictHostKeyChecking, &strict, false, None, false, cx)))
                    .child(div().child(agent_select).children(self.status(&theme, BulkHostField::ForwardAgent, &agent, false, None, false, cx)))
                    .child(div().child(x11_select).children(self.status(&theme, BulkHostField::ForwardX11, &x11, false, None, false, cx)))
                    .child(divider(&theme))
                    .child(
                        div()
                            .child(self.bulk_checkbox(
                                &theme,
                                "bulk-console",
                                &console,
                                "Enable console compatibility mode",
                                "Skips SFTP, shell integration, and SendEnv/SetEnv requests. TTY settings still apply.",
                                |this, v, cx| this.set(BulkHostField::ConsoleCompatibility, v, |s| &s.console_compatibility, |c| &mut c.console_compatibility, cx),
                                cx,
                            ))
                            .children(self.status(&theme, BulkHostField::ConsoleCompatibility, &console, false, None, false, cx)),
                    )
                    .child(
                        div()
                            .child(self.bulk_checkbox(
                                &theme,
                                "bulk-sftp",
                                &sftp,
                                "Disable SFTP and shell integration only",
                                "Keeps SendEnv/SetEnv and normal TTY behavior.",
                                |this, v, cx| this.set(BulkHostField::DisableSftp, v, |s| &s.disable_sftp, |c| &mut c.disable_sftp, cx),
                                cx,
                            ))
                            .children(self.status(&theme, BulkHostField::DisableSftp, &sftp, false, None, false, cx)),
                    )
                    .into_any_element()
            }
            Section::Appearance => {
                let scheme = self.field(
                    BulkHostField::TerminalScheme,
                    &summary.terminal_scheme,
                    &self.changes.terminal_scheme,
                    &plan,
                );
                let font = self.field(
                    BulkHostField::TerminalFontColor,
                    &summary.terminal_font_color,
                    &self.changes.terminal_font_color,
                    &plan,
                );
                let background = self.field(
                    BulkHostField::TerminalBackgroundColor,
                    &summary.terminal_background_color,
                    &self.changes.terminal_background_color,
                    &plan,
                );
                let group = self.field(
                    BulkHostField::CommandButtonGroup,
                    &summary.command_button_group,
                    &self.changes.command_button_group,
                    &plan,
                );
                let line = self.field(
                    BulkHostField::PasteLineDelayMs,
                    &summary.paste_line_delay_ms,
                    &self.changes.paste_line_delay_ms,
                    &plan,
                );
                let ch = self.field(
                    BulkHostField::PasteCharDelayMs,
                    &summary.paste_char_delay_ms,
                    &self.changes.paste_char_delay_ms,
                    &plan,
                );
                let prefs = self.data.prefs.clone();
                let custom = crate::features::host_editor::view::custom_schemes(&prefs);
                let scheme_value = scheme.value.clone().flatten();
                let valid = scheme_value
                    .as_deref()
                    .is_some_and(|id| muxus_term::palette::is_terminal_scheme_id(id, &custom));
                let defaults = crate::features::host_editor::view::terminal_color_defaults(
                    &prefs,
                    &custom,
                    if scheme.mixed {
                        None
                    } else {
                        scheme_value.as_deref()
                    },
                    theme.mode.is_dark(),
                );
                let scheme_select = crate::features::host_editor::view::scheme_select(
                    self,
                    "bulk-scheme",
                    if valid {
                        scheme_value.unwrap_or_default()
                    } else {
                        String::new()
                    },
                    scheme.mixed,
                    &custom,
                    window,
                    cx,
                    |this, id, cx| {
                        this.set(
                            BulkHostField::TerminalScheme,
                            (!id.is_empty()).then_some(id),
                            |s| &s.terminal_scheme,
                            |c| &mut c.terminal_scheme,
                            cx,
                        )
                    },
                );
                let font_el = color_override(
                    self,
                    "bulk-font",
                    "Text color",
                    font.value.clone().flatten().as_deref(),
                    &defaults.0,
                    font.mixed,
                    cx,
                    |this, v, cx| {
                        this.set(
                            BulkHostField::TerminalFontColor,
                            v,
                            |s| &s.terminal_font_color,
                            |c| &mut c.terminal_font_color,
                            cx,
                        )
                    },
                );
                let bg_el = color_override(
                    self,
                    "bulk-background",
                    "Background color",
                    background.value.clone().flatten().as_deref(),
                    &defaults.1,
                    background.mixed,
                    cx,
                    |this, v, cx| {
                        this.set(
                            BulkHostField::TerminalBackgroundColor,
                            v,
                            |s| &s.terminal_background_color,
                            |c| &mut c.terminal_background_color,
                            cx,
                        )
                    },
                );
                let groups = prefs.command_button_groups.clone();
                let current_group = group.value.clone().flatten();
                let mut group_options = vec![(String::new(), "Group chosen in the bar".to_owned())];
                if let Some(v) = &current_group
                    && !groups.iter().any(|g| &g.id == v)
                {
                    group_options.push((v.clone(), "Deleted group".into()));
                }
                for g in &groups {
                    let name = g.name.trim();
                    group_options.push((
                        g.id.clone(),
                        if name.is_empty() {
                            "Untitled group".into()
                        } else {
                            name.to_owned()
                        },
                    ));
                }
                let group_select = self.bulk_select(
                    "bulk-command-group",
                    "Command button group",
                    if group.mixed {
                        None
                    } else {
                        Some(current_group.unwrap_or_default())
                    },
                    group.mixed,
                    group_options,
                    window,
                    cx,
                    |this, v: String, cx| {
                        this.set(
                            BulkHostField::CommandButtonGroup,
                            (!v.is_empty()).then_some(v),
                            |s| &s.command_button_group,
                            |c| &mut c.command_button_group,
                            cx,
                        )
                    },
                );
                let line_field = paste_delay_field(
                    self,
                    "bulk-paste-line",
                    PasteDelayKind::Line,
                    line.value.flatten(),
                    prefs.paste_line_delay_ms,
                    line.mixed,
                    window,
                    cx,
                    |this, v, cx| {
                        this.set(
                            BulkHostField::PasteLineDelayMs,
                            v,
                            |s| &s.paste_line_delay_ms,
                            |c| &mut c.paste_line_delay_ms,
                            cx,
                        )
                    },
                );
                let char_field = paste_delay_field(
                    self,
                    "bulk-paste-char",
                    PasteDelayKind::Char,
                    ch.value.flatten(),
                    prefs.paste_char_delay_ms,
                    ch.mixed,
                    window,
                    cx,
                    |this, v, cx| {
                        this.set(
                            BulkHostField::PasteCharDelayMs,
                            v,
                            |s| &s.paste_char_delay_ms,
                            |c| &mut c.paste_char_delay_ms,
                            cx,
                        )
                    },
                );
                col()
                    .child(intro(
                        &theme,
                        "Terminal appearance",
                        describe(
                            scheme.summary.count(),
                            total,
                            "terminal host",
                            "Unset values follow the application settings.",
                        ),
                    ))
                    .child(div().child(scheme_select).children(self.status(
                        &theme,
                        BulkHostField::TerminalScheme,
                        &scheme,
                        false,
                        None,
                        false,
                        cx,
                    )))
                    .child(div().child(font_el).children(self.status(
                        &theme,
                        BulkHostField::TerminalFontColor,
                        &font,
                        false,
                        None,
                        false,
                        cx,
                    )))
                    .child(div().child(bg_el).children(self.status(
                        &theme,
                        BulkHostField::TerminalBackgroundColor,
                        &background,
                        false,
                        None,
                        false,
                        cx,
                    )))
                    .child(div().child(group_select).children(self.status(
                        &theme,
                        BulkHostField::CommandButtonGroup,
                        &group,
                        true,
                        None,
                        false,
                        cx,
                    )))
                    .child(
                        div()
                            .flex()
                            .gap(Theme::spacing(1.5))
                            .child(div().flex_1().min_w_0().child(line_field).children(
                                self.status(
                                    &theme,
                                    BulkHostField::PasteLineDelayMs,
                                    &line,
                                    true,
                                    None,
                                    false,
                                    cx,
                                ),
                            ))
                            .child(div().flex_1().min_w_0().child(char_field).children(
                                self.status(
                                    &theme,
                                    BulkHostField::PasteCharDelayMs,
                                    &ch,
                                    true,
                                    None,
                                    false,
                                    cx,
                                ),
                            )),
                    )
                    .into_any_element()
            }
            Section::Highlighting => {
                let profile = self.field(
                    BulkHostField::HighlightProfileId,
                    &summary.highlight_profile_id,
                    &self.changes.highlight_profile_id,
                    &plan,
                );
                let inherit = self.field(
                    BulkHostField::HighlightInheritGlobal,
                    &summary.highlight_inherit_global,
                    &self.changes.highlight_inherit_global,
                    &plan,
                );
                let profiles = self.data.prefs.keyword_highlight_profiles.clone();
                let current = profile.value.clone().flatten();
                let mut options = vec![(String::new(), "No reusable profile".to_owned())];
                if let Some(id) = &current
                    && !profiles.iter().any(|p| &p.id == id)
                {
                    options.push((id.clone(), format!("Missing profile ({id})")));
                }
                for profile in &profiles {
                    options.push((profile.id.clone(), profile.name.clone()));
                }
                let select = self.bulk_select(
                    "bulk-highlight",
                    "Highlighting profile",
                    if profile.mixed {
                        None
                    } else {
                        Some(current.unwrap_or_default())
                    },
                    profile.mixed,
                    options,
                    window,
                    cx,
                    |this, v: String, cx| {
                        this.set(
                            BulkHostField::HighlightProfileId,
                            (!v.is_empty()).then_some(v),
                            |s| &s.highlight_profile_id,
                            |c| &mut c.highlight_profile_id,
                            cx,
                        )
                    },
                );
                let checkbox = self.bulk_checkbox(
                    &theme,
                    "bulk-inherit-global",
                    &inherit,
                    "Include global highlighting rules",
                    "Turn this off when these hosts should use only their profile and host rules.",
                    |this, v, cx| {
                        this.set(
                            BulkHostField::HighlightInheritGlobal,
                            v,
                            |s| &s.highlight_inherit_global,
                            |c| &mut c.highlight_inherit_global,
                            cx,
                        )
                    },
                    cx,
                );
                col()
                    .child(intro(
                        &theme,
                        "Keyword highlighting",
                        describe(profile.summary.count(), total, "terminal host", "Each host keeps its own keyword rules; only the profile and the global rules switch change."),
                    ))
                    .child(div().child(select).children(self.status(&theme, BulkHostField::HighlightProfileId, &profile, false, None, false, cx)))
                    .child(div().child(checkbox).children(self.status(&theme, BulkHostField::HighlightInheritGlobal, &inherit, false, None, false, cx)))
                    .into_any_element()
            }
            Section::Logging => {
                let logging = self.field(
                    BulkHostField::SessionLogging,
                    &summary.session_logging,
                    &self.changes.session_logging,
                    &plan,
                );
                let description = describe(
                    logging.summary.count(),
                    total,
                    "terminal host",
                    "New sessions use the chosen policy; existing logs are kept.",
                );
                if matches!(logging.summary, BulkFieldSummary::Loading { .. }) && !logging.changed {
                    return col()
                        .gap(Theme::spacing(2.))
                        .child(intro(&theme, "Session logging", description))
                        .child(para(
                            "Loading session logging settings…",
                            body2(),
                            p.text.secondary,
                        ))
                        .into_any_element();
                }
                let Some(value) = logging.value else {
                    let start: Option<HostSessionLoggingDraft> = self
                        .default_policy
                        .as_ref()
                        .map(|policy| host_session_logging_draft(policy, true));
                    let entity = cx.entity().downgrade();
                    let action = Button::new("bulk-logging-one", "Set one policy")
                        .color(ButtonColor::Inherit)
                        .label_element(
                            text(
                                "Set one policy",
                                button(rem(13.)),
                                alert_ink(&theme, Status::Info, AlertVariant::Standard),
                            )
                            .no_truncate(),
                        )
                        .disabled(start.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(start) = start {
                                let _ = entity.update(cx, |this, cx| {
                                    this.set(
                                        BulkHostField::SessionLogging,
                                        start,
                                        |s| &s.session_logging,
                                        |c| &mut c.session_logging,
                                        cx,
                                    )
                                });
                            }
                        });
                    return col()
                        .gap(Theme::spacing(2.))
                        .child(intro(&theme, "Session logging", description))
                        .child(Alert::new(Status::Info, "").outlined().content(para("These hosts log sessions differently. Each keeps its own policy until you set one for all of them.", body2(), alert_ink(&theme, Status::Info, AlertVariant::Outlined))).action(action.into_any_element()))
                        .into_any_element();
                };
                let section = logging_section(
                    self,
                    "bulk-logging",
                    &value,
                    &description,
                    window,
                    cx,
                    move |this, patch, cx| {
                        this.set(
                            BulkHostField::SessionLogging,
                            patch,
                            |s| &s.session_logging,
                            |c| &mut c.session_logging,
                            cx,
                        )
                    },
                );
                col()
                    .gap(Theme::spacing(2.))
                    .child(section)
                    .children(self.status(
                        &theme,
                        BulkHostField::SessionLogging,
                        &logging,
                        false,
                        None,
                        false,
                        cx,
                    ))
                    .into_any_element()
            }
            Section::Login => {
                let sequence = self.field(
                    BulkHostField::LoginSequence,
                    &summary.login_sequence,
                    &self.changes.login_sequence,
                    &plan,
                );
                let intro_el = intro(
                    &theme,
                    "Login sequence",
                    describe(
                        sequence.summary.count(),
                        total,
                        "terminal host",
                        "Steps each session runs after it connects. Hosts that use their folder’s sequence follow the folder they are in.",
                    ),
                );
                let Some(value) = sequence.value.clone() else {
                    let (folders, one) = (cx.entity().downgrade(), cx.entity().downgrade());
                    let actions = div()
                        .flex()
                        .gap(Theme::spacing(0.5))
                        .child(
                            Button::new("bulk-login-folders", "Use folders")
                                .color(ButtonColor::Inherit)
                                .label_element(
                                    text(
                                        "Use folders",
                                        button(rem(13.)),
                                        alert_ink(&theme, Status::Info, AlertVariant::Standard),
                                    )
                                    .no_truncate(),
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = folders.update(cx, |this, cx| {
                                        this.set(
                                            BulkHostField::LoginSequence,
                                            LoginSequenceDraft {
                                                mode: LoginSequenceMode::Inherit,
                                                steps: Vec::new(),
                                            },
                                            |s| &s.login_sequence,
                                            |c| &mut c.login_sequence,
                                            cx,
                                        )
                                    });
                                }),
                        )
                        .child(
                            Button::new("bulk-login-one", "Set one sequence")
                                .color(ButtonColor::Inherit)
                                .label_element(
                                    text(
                                        "Set one sequence",
                                        button(rem(13.)),
                                        alert_ink(&theme, Status::Info, AlertVariant::Standard),
                                    )
                                    .no_truncate(),
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = one.update(cx, |this, cx| {
                                        this.set(
                                            BulkHostField::LoginSequence,
                                            LoginSequenceDraft {
                                                mode: LoginSequenceMode::Custom,
                                                steps: vec![new_login_step(LoginStepKind::Wait)],
                                            },
                                            |s| &s.login_sequence,
                                            |c| &mut c.login_sequence,
                                            cx,
                                        )
                                    });
                                }),
                        );
                    return col()
                        .gap(Theme::spacing(2.))
                        .child(intro_el)
                        .child(Alert::new(Status::Info, "").outlined().content(para("These hosts log in differently. Each keeps its own login sequence until you set one for all of them.", body2(), alert_ink(&theme, Status::Info, AlertVariant::Outlined))).action(actions.into_any_element()))
                        .into_any_element();
                };
                let vault = self.data.vault.clone();
                let editor = login_sequence_editor(
                    self,
                    "bulk-login",
                    &value,
                    None,
                    "Use the folder's login sequence",
                    "Each host runs the sequence of the folder it is in, if that folder sets one.",
                    vault.as_ref(),
                    window,
                    cx,
                    |this, draft, cx| {
                        this.set(
                            BulkHostField::LoginSequence,
                            draft,
                            |s| &s.login_sequence,
                            |c| &mut c.login_sequence,
                            cx,
                        )
                    },
                );
                col()
                    .gap(Theme::spacing(2.))
                    .child(intro_el)
                    .child(editor)
                    .children(self.status(
                        &theme,
                        BulkHostField::LoginSequence,
                        &sequence,
                        false,
                        None,
                        false,
                        cx,
                    ))
                    .into_any_element()
            }
        }
    }
}

fn strict_name(value: StrictHostKeyChecking) -> &'static str {
    match value {
        StrictHostKeyChecking::Yes => "yes",
        StrictHostKeyChecking::No => "no",
        StrictHostKeyChecking::AcceptNew => "accept-new",
        StrictHostKeyChecking::Ask => "ask",
    }
}

fn parse_strict(value: &str) -> Option<StrictHostKeyChecking> {
    match value {
        "yes" => Some(StrictHostKeyChecking::Yes),
        "no" => Some(StrictHostKeyChecking::No),
        "accept-new" => Some(StrictHostKeyChecking::AcceptNew),
        "ask" => Some(StrictHostKeyChecking::Ask),
        _ => None,
    }
}

/// The selected hosts that still exist, in selection order.
fn resolve(data: &EditorData, keys: &[String]) -> Vec<ManagedHost> {
    let mut all: HashMap<String, ManagedHost> = HashMap::new();
    if let Some(config) = &data.ssh_config {
        for host in &config.hosts {
            let host = ManagedHost::ssh(host.clone());
            all.insert(managed_host_key(&host), host);
        }
    }
    for profile in &data.saved_hosts {
        let host = ManagedHost::profile(profile.clone());
        all.insert(managed_host_key(&host), host);
    }
    keys.iter()
        .filter_map(|key| all.get(key).cloned())
        .collect()
}

impl Render for BulkEditDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let hosts = self.hosts();
        let entity = cx.entity().downgrade();
        if self.loaded && hosts.is_empty() {
            let close = entity.clone();
            let backdrop = entity.clone();
            return div()
                .id("bulk-edit")
                .absolute()
                .inset_0()
                .track_focus(&self.focus)
                .on_key_down(cx.listener(Self::on_key_down))
                .child(modal(
                    crate::features::host_editor::widgets::shell::dialog_frame(
                        &theme,
                        window,
                        "bulk-empty",
                        DIALOG_MD,
                        "Edit hosts",
                        16.,
                        para(
                            "The selected hosts no longer exist.",
                            body2(),
                            theme.palette.text.secondary,
                        )
                        .into_any_element(),
                        vec![
                            Button::new("bulk-close", "Close")
                                .on_click(move |_, _, cx| {
                                    let _ = close.update(cx, |_, cx| cx.emit(BulkEditEvent::Close));
                                })
                                .into_any_element(),
                        ],
                        true,
                    ),
                    Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                        let _ = backdrop.update(cx, |_, cx| cx.emit(BulkEditEvent::Close));
                    })),
                ))
                .into_any_element();
        }
        let summary = self.summary();
        let visible: Vec<_> = SECTIONS
            .iter()
            .filter(|(_, _, _, _, fields)| {
                fields
                    .iter()
                    .any(|f| summary.state(*f) != BulkFieldSummary::None)
            })
            .collect();
        let section = if visible.iter().any(|(s, ..)| *s == self.section) {
            self.section
        } else {
            Section::Organize
        };
        let tabs: Vec<SectionTab> = visible
            .iter()
            .map(|(_, id, label, icon, fields)| {
                let count = fields.iter().filter(|f| self.changes.has(**f)).count();
                SectionTab::new(id, label, icon, (count > 0).then_some(count))
            })
            .collect();
        let total = hosts.len();
        let content = self.content(section, total, window, cx);
        let plan = self.plan();
        let changed = self.changes.fields().len();
        let idle = if changed == 0 {
            Some("Change a setting to apply it to every selected host.".to_owned())
        } else if plan.hosts.is_empty() {
            Some("The selected hosts already have these settings.".to_owned())
        } else {
            None
        };
        let names: Vec<&str> = hosts
            .iter()
            .take(3)
            .map(managed_host_display_name)
            .collect();
        let storage = if total > 3 {
            format!("{} and {} more", names.join(", "), total - 3)
        } else {
            names.join(", ")
        };
        let (on_section, close, save, backdrop) =
            (entity.clone(), entity.clone(), entity.clone(), entity);
        let props = ShellProps {
            title: format!("Edit {total} host{}", if total == 1 { "" } else { "s" }).into(),
            storage: storage.into(),
            kind: None,
            on_kind: None,
            sections: tabs,
            section: SharedString::from(
                SECTIONS
                    .iter()
                    .find(|s| s.0 == section)
                    .map(|s| s.1)
                    .unwrap_or("organize"),
            ),
            on_section: Rc::new(move |id: SharedString, _, cx: &mut App| {
                let _ = on_section.update(cx, |this, cx| {
                    if let Some((s, ..)) = SECTIONS.iter().find(|s| s.1 == id.as_ref()) {
                        this.section = *s;
                        this.menus.close();
                        cx.notify();
                    }
                });
            }),
            problem: bulk_changes_problem(&self.changes),
            loading: idle,
            busy: self.busy,
            on_delete: None,
            delete_pending: false,
            on_close: Rc::new(move |_, cx: &mut App| {
                let _ = close.update(cx, |_, cx| cx.emit(BulkEditEvent::Close));
            }),
            on_save: Rc::new(move |_, _, cx: &mut App| {
                let _ = save.update(cx, |this, cx| this.apply(cx));
            }),
            save_label: if plan.hosts.is_empty() {
                "Apply".into()
            } else {
                format!(
                    "Apply to {} host{}",
                    plan.hosts.len(),
                    if plan.hosts.len() == 1 { "" } else { "s" }
                )
                .into()
            },
            connect_action: false,
            scroll: self.scroll.clone(),
        };
        div()
            .id("bulk-edit")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(modal(
                editor_shell(&theme, window, props, content),
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = backdrop.update(cx, |_, cx| cx.emit(BulkEditEvent::Close));
                })),
            ))
            .into_any_element()
    }
}

/// The logging key of every terminal host, for callers that preload policies.
pub fn logging_keys(hosts: &[ManagedHost]) -> Vec<String> {
    hosts
        .iter()
        .filter(|h| is_terminal_host(h))
        .map(host_logging_key)
        .collect()
}
