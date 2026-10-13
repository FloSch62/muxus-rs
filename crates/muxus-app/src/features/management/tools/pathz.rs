//! gNMI path authorization (`tools/PathzTool.tsx`): probe a path, edit the
//! rules and groups, and rotate them through the sandbox.

use std::collections::HashSet;

use muxus_core::gnmi_path::try_parse_gnmi_path;
use muxus_core::management::{ManagementError, grpc_methods};
use muxus_mgmt::workbench::set_review::canonical_json;
use muxus_ui::gpui_kit::{
    AnyElement, Context, Div, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::IconButton;
use muxus_ui::widgets::display::Alert;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::super::fields::FieldKey;
use super::super::logic::tools::{device_time, gnoi_path, rotation_version};
use super::super::navigator::ListenerIndex;
use super::super::path_text::path_text;
use super::super::ui::{Dialog, ReviewApply, ReviewSection, SelectField};
use super::super::view::ManagementView;
use super::super::widgets::MONO;
use super::super::workbench::ListenerForApp;
use super::kit::{
    ToolField, body_text, caption_text, default_user, error_alert, json_path_text, message_error,
};
use super::rotation::{Rotation, RotationState, RotationText};
use super::{contained, field_row, section, spacer, tool_header};
use muxus_ui::widgets::button::Button;
use muxus_ui::widgets::button::ButtonColor;
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::toggle_button::ToggleButtonGroup;
use muxus_ui::widgets::toggle_button::ToggleOption;

/// A rule as the editor holds it: the path as text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuleDraft {
    pub id: String,
    /// `user` or `group`.
    pub principal: String,
    pub name: String,
    pub path: String,
    pub mode: String,
    pub action: String,
}

impl Default for RuleDraft {
    fn default() -> Self {
        Self {
            id: String::new(),
            principal: "user".into(),
            name: String::new(),
            path: "/".into(),
            mode: "MODE_WRITE".into(),
            action: "ACTION_PERMIT".into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GroupDraft {
    pub name: String,
    pub users: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PathzProbe {
    pub action: String,
    pub version: Option<String>,
    pub user: String,
    pub path: String,
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PathzState {
    pub loading: bool,
    pub no_policy: bool,
    pub version: Option<String>,
    pub created_on: Option<Value>,
    pub rules: Vec<Value>,
    pub groups: Vec<Value>,
    pub error: Option<ManagementError>,
    pub editing: bool,
    pub rule_drafts: Vec<RuleDraft>,
    pub group_drafts: Vec<GroupDraft>,
    /// `None` until edited: the session's user.
    pub probe_user: Option<String>,
    pub probe_path: String,
    pub probe_mode: String,
    pub probe_instance: String,
    pub probe: Option<PathzProbe>,
    pub rotation: RotationState,
}

impl Default for PathzState {
    fn default() -> Self {
        Self {
            loading: false,
            no_policy: false,
            version: None,
            created_on: None,
            rules: Vec::new(),
            groups: Vec::new(),
            error: None,
            editing: false,
            rule_drafts: Vec::new(),
            group_drafts: Vec::new(),
            probe_user: None,
            probe_path: "/interface".into(),
            probe_mode: "MODE_WRITE".into(),
            probe_instance: "POLICY_INSTANCE_ACTIVE".into(),
            probe: None,
            rotation: RotationState::default(),
        }
    }
}

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// The policy in force as editable drafts (`toDrafts`).
pub fn to_drafts(rules: &[Value], groups: &[Value]) -> (Vec<RuleDraft>, Vec<GroupDraft>) {
    let rule_drafts = rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            let group = rule
                .get("group")
                .and_then(Value::as_str)
                .filter(|group| !group.is_empty());
            RuleDraft {
                id: rule
                    .get("id")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("rule-{}", index + 1), str::to_owned),
                principal: if group.is_some() { "group" } else { "user" }.into(),
                name: group
                    .map(str::to_owned)
                    .or_else(|| rule.get("user").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_default(),
                path: json_path_text(rule.get("path")),
                mode: if text(rule.get("mode")) == "MODE_WRITE" {
                    "MODE_WRITE"
                } else {
                    "MODE_READ"
                }
                .into(),
                action: if text(rule.get("action")) == "ACTION_DENY" {
                    "ACTION_DENY"
                } else {
                    "ACTION_PERMIT"
                }
                .into(),
            }
        })
        .collect();
    let group_drafts = groups
        .iter()
        .map(|group| GroupDraft {
            name: text(group.get("name")),
            users: group
                .get("users")
                .and_then(Value::as_array)
                .map(|users| {
                    users
                        .iter()
                        .map(|user| text(user.get("name")))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default(),
        })
        .collect();
    (rule_drafts, group_drafts)
}

/// The drafts as a policy (`policyFromDrafts`).
pub fn policy_from_drafts(rules: &[RuleDraft], groups: &[GroupDraft]) -> Value {
    let rules: Vec<Value> = rules
        .iter()
        .map(|rule| {
            let parsed = try_parse_gnmi_path(&rule.path);
            let path = match &parsed {
                Some(parsed) => gnoi_path(&parsed.elems, parsed.origin.as_deref()),
                None => gnoi_path(&[], None),
            };
            let mut out = Map::new();
            out.insert("id".into(), json!(rule.id));
            out.insert(rule.principal.clone(), json!(rule.name));
            out.insert("path".into(), path);
            out.insert("mode".into(), json!(rule.mode));
            out.insert("action".into(), json!(rule.action));
            Value::Object(out)
        })
        .collect();
    let groups: Vec<Value> = groups
        .iter()
        .filter(|group| !muxus_core::js::js_trim(&group.name).is_empty())
        .map(|group| {
            json!({
                "name": muxus_core::js::js_trim(&group.name),
                "users": group
                    .users
                    .split(',')
                    .map(muxus_core::js::js_trim)
                    .filter(|user| !user.is_empty())
                    .map(|name| json!({ "name": name }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "rules": rules, "groups": groups })
}

/// What keeps the drafts from being uploaded (`draftProblem`).
pub fn draft_problem(rules: &[RuleDraft]) -> Option<String> {
    for rule in rules {
        if muxus_core::js::js_trim(&rule.id).is_empty() {
            return Some("Every rule needs an id.".into());
        }
        if muxus_core::js::js_trim(&rule.name).is_empty() {
            return Some(format!("Rule {} needs a {}.", rule.id, rule.principal));
        }
        if try_parse_gnmi_path(&rule.path).is_none() {
            return Some(format!("The path of rule {} does not parse.", rule.id));
        }
    }
    let ids: HashSet<&str> = rules
        .iter()
        .map(|rule| muxus_core::js::js_trim(&rule.id))
        .collect();
    (ids.len() != rules.len()).then(|| "Rule ids must be unique.".into())
}

fn toggle(labels: &[&str]) -> Vec<ToggleOption> {
    labels.iter().map(|text| ToggleOption::new(*text)).collect()
}

fn verdict_chip(action: &str, height: f32, theme: &Theme) -> Chip {
    let p = &theme.palette;
    let permit = action == "ACTION_PERMIT";
    let chip = Chip::new(if permit {
        "Permit"
    } else if action == "ACTION_DENY" {
        "Deny"
    } else {
        "Unspecified"
    })
    .height(height);
    if permit {
        chip.palette(p.success)
    } else {
        chip.palette(p.error)
    }
}

impl ManagementView {
    pub(super) fn render_pathz(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let can_run = self.grpc().is_some();
        let state: PathzState = self.tool("pathz");
        let rotating = state.rotation.rotating();
        let probe_user = state
            .probe_user
            .clone()
            .unwrap_or_else(|| default_user(self.session_address().as_deref()));
        let reload = Button::new("pathz-reload", "Reload")
            .disabled(!can_run || rotating)
            .on_click(
                cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| this.load_pathz(cx)),
            )
            .into_any_element();
        let mut column = div().child(tool_header(
            "RuleOutlined",
            "Path authorization",
            "gnsi.pathz.v1.Pathz",
            "Which users and groups may read or write which gNMI paths. Probe a path, or upload new rules, test them in the sandbox and make them final.",
            Some(reload),
            &theme,
        ));
        if let Some(bar) = self.rotation_bar(
            "pathz",
            RotationText {
                what: "path rules",
                plural: true,
                test_hint: "Probe the sandbox instance to see what they allow.",
            },
            &theme,
            cx,
        ) {
            column = column.child(bar);
        }
        if let Some(error) = &state.error {
            column = column.child(error_alert(
                error,
                &theme,
                Some(std::rc::Rc::new(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<PathzState>("pathz", |state| state.error = None);
                        cx.notify();
                    },
                ))),
            ));
        }

        // Probe
        let path_field = self.render_path_field(
            FieldKey::Tool("pathz.path"),
            &state.probe_path,
            "/interface[name=ethernet-1/1]",
            window,
            cx,
        );
        let mode = if state.probe_mode == "MODE_READ" {
            0
        } else {
            1
        };
        let instance = if state.probe_instance == "POLICY_INSTANCE_SANDBOX" {
            1
        } else {
            0
        };
        let mut probe = div().child(
            field_row(8.)
                .child(self.tool_field(
                    ToolField::new("pathz.user", "User", probe_user).width(140.),
                    window,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .flex_grow(1.)
                        .flex_shrink(1.)
                        .flex_basis(px(260.))
                        .min_w(px(200.))
                        .child(path_field),
                )
                .child(
                    ToggleButtonGroup::exclusive(
                        "pathz-mode",
                        toggle(&["Read", "Write"]),
                        Some(mode),
                        cx.listener_index(|this: &mut Self, index, cx: &mut Context<Self>| {
                            this.update_tool::<PathzState>("pathz", |state| {
                                state.probe_mode = if index == 0 {
                                    "MODE_READ"
                                } else {
                                    "MODE_WRITE"
                                }
                                .into();
                            });
                            cx.notify();
                        }),
                    )
                    .inline(3.2, 8.8, 12.),
                )
                .child(
                    ToggleButtonGroup::exclusive(
                        "pathz-instance",
                        toggle(&["Active", "Sandbox"]),
                        Some(instance),
                        cx.listener_index(|this: &mut Self, index, cx: &mut Context<Self>| {
                            this.update_tool::<PathzState>("pathz", |state| {
                                state.probe_instance = if index == 0 {
                                    "POLICY_INSTANCE_ACTIVE"
                                } else {
                                    "POLICY_INSTANCE_SANDBOX"
                                }
                                .into();
                            });
                            cx.notify();
                        }),
                    )
                    .inline(3.2, 8.8, 12.),
                )
                .child(
                    contained("pathz-probe", "Probe", ButtonColor::Primary)
                        .disabled(!can_run)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| this.probe_pathz(cx),
                        )),
                ),
        );
        if let Some(result) = &state.probe {
            let version = result
                .version
                .clone()
                .filter(|version| !version.is_empty())
                .unwrap_or_else(|| "\u{2014}".into());
            probe = probe.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .mt(px(12.))
                    .child(verdict_chip(&result.action, 22., &theme))
                    .child(
                        body_text(
                            format!(
                                "{} {} {}",
                                result.user,
                                if result.mode == "MODE_WRITE" {
                                    "writes"
                                } else {
                                    "reads"
                                },
                                result.path
                            ),
                            p.text.primary,
                            &theme,
                        )
                        .font_family(MONO)
                        .text_size(px(12.)),
                    )
                    .child(spacer())
                    .child(caption_text(
                        format!("policy {version}"),
                        p.text.secondary,
                        &theme,
                    )),
            );
        }
        column = column.child(section("Probe", None, probe, &theme));

        // Rules in force
        let problem = if state.editing {
            draft_problem(&state.rule_drafts)
        } else {
            None
        };
        let mut actions = div().flex().items_center().gap(px(8.));
        if let Some(version) = &state.version {
            actions = actions.child(caption_text(
                format!(
                    "{version} \u{b7} {}",
                    device_time(state.created_on.as_ref(), true)
                ),
                p.text.secondary,
                &theme,
            ));
        }
        if state.editing {
            actions = actions
                .child(
                    Button::new("pathz-cancel", "Cancel")
                        .color(ButtonColor::Inherit)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| {
                                this.update_tool::<PathzState>("pathz", |state| {
                                    state.editing = false
                                });
                                cx.notify();
                            },
                        )),
                )
                .child(
                    contained(
                        "pathz-review",
                        "Review & rotate\u{2026}",
                        ButtonColor::Primary,
                    )
                    .disabled(problem.is_some())
                    .on_click(cx.listener_for_app(
                        |this: &mut Self, cx: &mut Context<Self>| this.review_pathz(cx),
                    )),
                );
        } else {
            actions = actions.child(
                Button::new(
                    "pathz-edit",
                    if state.no_policy {
                        "Create rules\u{2026}"
                    } else {
                        "Edit\u{2026}"
                    },
                )
                .disabled(!can_run || rotating || state.loading)
                .on_click(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<PathzState>("pathz", |state| {
                            let (rules, groups) = to_drafts(&state.rules, &state.groups);
                            state.editing = true;
                            state.rule_drafts = rules;
                            state.group_drafts = groups;
                        });
                        cx.notify();
                    },
                )),
            );
        }
        let body: AnyElement = if state.loading {
            CircularProgress::new(("pathz-progress", 1usize), 20., p.primary.main)
                .into_any_element()
        } else if state.editing {
            self.pathz_editor(&state, problem, window, cx)
                .into_any_element()
        } else if state.no_policy {
            Alert::new(
                Status::Info,
                "No path authorization policy is active. gNMI access is only limited by the authorization policy and the device's own roles.",
            )
            .into_any_element()
        } else if state.rules.is_empty() {
            body_text("The policy has no rules.", p.text.secondary, &theme).into_any_element()
        } else {
            rules_view(&state, &theme).into_any_element()
        };
        column
            .child(section(
                "Rules in force",
                Some(actions.into_any_element()),
                body,
                &theme,
            ))
            .into_any_element()
    }

    fn pathz_editor(
        &mut self,
        state: &PathzState,
        problem: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let default_name = default_user(self.session_address().as_deref());
        let mut editor = div().flex().flex_col().gap(px(8.));
        for (index, rule) in state.rule_drafts.iter().enumerate() {
            let who = if rule.principal == "group" {
                "Group"
            } else {
                "User"
            };
            editor = editor.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(self.tool_field(
                        ToolField::row("pathz.rule.id", index, "Id", rule.id.clone()).width(110.),
                        window,
                        cx,
                    ))
                    .child(self.select_field(
                        SelectField::ToolRow("pathz.rule.principal", index),
                        "Who",
                        who.into(),
                        90.,
                        false,
                        cx,
                    ))
                    .child(
                        self.tool_field(
                            ToolField::row("pathz.rule.name", index, who, rule.name.clone())
                                .width(120.),
                            window,
                            cx,
                        ),
                    )
                    .child(
                        self.tool_field(
                            ToolField::row("pathz.rule.path", index, "Path", rule.path.clone())
                                .grow()
                                .mono(),
                            window,
                            cx,
                        ),
                    )
                    .child(
                        self.select_field(
                            SelectField::ToolRow("pathz.rule.mode", index),
                            "Mode",
                            if rule.mode == "MODE_WRITE" {
                                "Write"
                            } else {
                                "Read"
                            }
                            .into(),
                            96.,
                            false,
                            cx,
                        ),
                    )
                    .child(
                        self.select_field(
                            SelectField::ToolRow("pathz.rule.action", index),
                            "Action",
                            if rule.action == "ACTION_DENY" {
                                "Deny"
                            } else {
                                "Permit"
                            }
                            .into(),
                            104.,
                            false,
                            cx,
                        ),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("remove-rule-{index}")),
                            "Close",
                        )
                        .icon_size(16.)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_tool::<PathzState>("pathz", |state| {
                                if index < state.rule_drafts.len() {
                                    state.rule_drafts.remove(index);
                                }
                            });
                            cx.notify();
                        })),
                    ),
            );
        }
        editor = editor.child(
            div().flex().child(
                Button::new("pathz-add-rule", "Rule")
                    .start_icon("Add")
                    .on_click(cx.listener_for_app(
                        move |this: &mut Self, cx: &mut Context<Self>| {
                            let name = default_name.clone();
                            this.update_tool::<PathzState>("pathz", |state| {
                                let id = format!("rule-{}", state.rule_drafts.len() + 1);
                                state.rule_drafts.push(RuleDraft {
                                    id,
                                    name,
                                    ..RuleDraft::default()
                                });
                            });
                            cx.notify();
                        },
                    )),
            ),
        );
        let subtitle2 = theme.typography.subtitle2;
        editor = editor.child(
            div()
                .pt(px(8.))
                .text_size(px(subtitle2.size))
                .font_weight(muxus_ui::gpui_kit::FontWeight(subtitle2.weight))
                .line_height(px(subtitle2.line_height))
                .child("Groups"),
        );
        for (index, group) in state.group_drafts.iter().enumerate() {
            editor = editor.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        self.tool_field(
                            ToolField::row("pathz.group.name", index, "Group", group.name.clone())
                                .width(160.),
                            window,
                            cx,
                        ),
                    )
                    .child(
                        self.tool_field(
                            ToolField::row(
                                "pathz.group.users",
                                index,
                                "Users (comma separated)",
                                group.users.clone(),
                            )
                            .grow(),
                            window,
                            cx,
                        ),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("remove-group-{index}")),
                            "Close",
                        )
                        .icon_size(16.)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_tool::<PathzState>("pathz", |state| {
                                if index < state.group_drafts.len() {
                                    state.group_drafts.remove(index);
                                }
                            });
                            cx.notify();
                        })),
                    ),
            );
        }
        editor = editor.child(
            div().flex().child(
                Button::new("pathz-add-group", "Group")
                    .start_icon("Add")
                    .on_click(
                        cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                            this.update_tool::<PathzState>("pathz", |state| {
                                state.group_drafts.push(GroupDraft::default())
                            });
                            cx.notify();
                        }),
                    ),
            ),
        );
        if let Some(problem) = problem {
            editor = editor.child(caption_text(problem, p.error.main, &theme));
        }
        editor
    }

    pub(super) fn load_pathz(&mut self, cx: &mut Context<Self>) {
        if self.grpc().is_none() {
            return;
        }
        self.update_tool::<PathzState>("pathz", |state| {
            state.loading = true;
            state.error = None;
        });
        let request = json!({ "policy_instance": "POLICY_INSTANCE_ACTIVE" });
        self.grpc_call(grpc_methods::PATHZ_GET, request, cx, |this, result, _| {
            this.update_tool::<PathzState>("pathz", |state| {
                state.loading = false;
                match result {
                    Ok(response) => {
                        state.no_policy = false;
                        state.version = response
                            .get("version")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        state.created_on = response.get("created_on").cloned();
                        let policy = response.get("policy");
                        state.rules = policy
                            .and_then(|policy| policy.get("rules"))
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        state.groups = policy
                            .and_then(|policy| policy.get("groups"))
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                    }
                    Err(error) => {
                        if matches!(
                            error.code.as_deref(),
                            Some("FAILED_PRECONDITION" | "NOT_FOUND")
                        ) {
                            state.no_policy = true;
                            state.rules.clear();
                            state.groups.clear();
                            state.version = None;
                        } else {
                            state.error = Some(error);
                        }
                    }
                }
            })
        });
        cx.notify();
    }

    pub(super) fn probe_pathz(&mut self, cx: &mut Context<Self>) {
        if self.grpc().is_none() {
            return;
        }
        let state: PathzState = self.tool("pathz");
        let user = state
            .probe_user
            .clone()
            .unwrap_or_else(|| default_user(self.session_address().as_deref()));
        let Some(parsed) = try_parse_gnmi_path(&state.probe_path) else {
            self.update_tool::<PathzState>("pathz", |state| {
                state.error = Some(message_error("The path does not parse."))
            });
            cx.notify();
            return;
        };
        let user = muxus_core::js::js_trim(&user).to_owned();
        let request = json!({
            "user": user,
            "path": gnoi_path(&parsed.elems, parsed.origin.as_deref()),
            "mode": state.probe_mode,
            "policy_instance": state.probe_instance,
        });
        let path = state.probe_path.clone();
        let mode = state.probe_mode.clone();
        self.grpc_call(
            grpc_methods::PATHZ_PROBE,
            request,
            cx,
            move |this, result, _| match result {
                Ok(response) => this.update_tool::<PathzState>("pathz", |state| {
                    state.probe = Some(PathzProbe {
                        action: response
                            .get("action")
                            .and_then(Value::as_str)
                            .unwrap_or("ACTION_UNSPECIFIED")
                            .to_owned(),
                        version: response
                            .get("version")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        user,
                        path,
                        mode,
                    })
                }),
                Err(error) => {
                    this.update_tool::<PathzState>("pathz", |state| state.error = Some(error))
                }
            },
        );
    }

    fn review_pathz(&mut self, cx: &mut Context<Self>) {
        let state: PathzState = self.tool("pathz");
        let current = json!({ "rules": state.rules, "groups": state.groups });
        let proposed = policy_from_drafts(&state.rule_drafts, &state.group_drafts);
        self.ui.dialog = Some(Dialog::Review {
            title: "Rotate the path rules".into(),
            description: Some(
                "The rules are uploaded to the sandbox and take effect provisionally. Probe them, then finalize; rolling back restores the current rules."
                    .into(),
            ),
            sections: vec![ReviewSection::new(
                "policy".into(),
                if state.no_policy { String::new() } else { canonical_json(Some(&current)) },
                canonical_json(Some(&proposed)),
                "json",
                None,
            )],
            loading: false,
            error: None,
            apply: Some((ReviewApply::Rotate("pathz"), "Upload".into())),
            tab: 0,
        });
        cx.notify();
    }

    pub(super) fn rotate_pathz(&mut self, cx: &mut Context<Self>) {
        let state: PathzState = self.tool("pathz");
        let proposed = policy_from_drafts(&state.rule_drafts, &state.group_drafts);
        let version = rotation_version(chrono::Utc::now());
        let upload = json!({
            "upload_request": {
                "version": version,
                "created_on": chrono::Utc::now().timestamp(),
                "policy": proposed,
            }
        });
        self.start_rotation(
            "pathz",
            Rotation {
                method: grpc_methods::PATHZ_ROTATE,
                upload,
                finalize_field: "finalize_rotation",
                version,
            },
            cx,
        );
        self.update_tool::<PathzState>("pathz", |state| {
            state.editing = false;
            state.probe_instance = "POLICY_INSTANCE_SANDBOX".into();
        });
    }

    pub(super) fn pathz_field(&mut self, name: &str, value: String) {
        self.update_tool::<PathzState>("pathz", |state| match name {
            "pathz.user" => state.probe_user = Some(value),
            "pathz.path" => state.probe_path = value,
            _ => {}
        });
    }

    pub(super) fn pathz_row_field(&mut self, name: &str, index: usize, value: String) {
        self.update_tool::<PathzState>("pathz", |state| {
            if let Some(rule) = state.rule_drafts.get_mut(index) {
                match name {
                    "pathz.rule.id" => rule.id = value.clone(),
                    "pathz.rule.name" => rule.name = value.clone(),
                    "pathz.rule.path" => rule.path = value.clone(),
                    _ => {}
                }
            }
            if let Some(group) = state.group_drafts.get_mut(index) {
                match name {
                    "pathz.group.name" => group.name = value,
                    "pathz.group.users" => group.users = value,
                    _ => {}
                }
            }
        });
    }

    pub(super) fn pathz_row_select(&mut self, name: &str, index: usize, choice: usize) {
        self.update_tool::<PathzState>("pathz", |state| {
            let Some(rule) = state.rule_drafts.get_mut(index) else {
                return;
            };
            match name {
                "pathz.rule.principal" => {
                    rule.principal = if choice == 0 { "user" } else { "group" }.into()
                }
                "pathz.rule.mode" => {
                    rule.mode = if choice == 0 {
                        "MODE_READ"
                    } else {
                        "MODE_WRITE"
                    }
                    .into()
                }
                "pathz.rule.action" => {
                    rule.action = if choice == 0 {
                        "ACTION_PERMIT"
                    } else {
                        "ACTION_DENY"
                    }
                    .into()
                }
                _ => {}
            }
        });
    }
}

/// Options of the row selects.
pub fn row_select_options(name: &str) -> Option<Vec<(String, Option<String>)>> {
    let labels: &[&str] = match name {
        "pathz.rule.principal" => &["User", "Group"],
        "pathz.rule.mode" => &["Read", "Write"],
        "pathz.rule.action" => &["Permit", "Deny"],
        _ => return None,
    };
    Some(
        labels
            .iter()
            .map(|label| ((*label).to_owned(), None))
            .collect(),
    )
}

/// The rules in force, and the groups.
fn rules_view(state: &PathzState, theme: &Theme) -> Div {
    let p = &theme.palette;
    let mut list = div();
    for rule in &state.rules {
        let deny = text(rule.get("action")) == "ACTION_DENY";
        let mut chip = Chip::new(if deny { "Deny" } else { "Permit" })
            .height(22.)
            .min_width(58.);
        chip = if deny {
            chip.palette(p.error)
        } else {
            chip.palette(p.success)
        };
        let who = match rule
            .get("group")
            .and_then(Value::as_str)
            .filter(|group| !group.is_empty())
        {
            Some(group) => format!("group {group}"),
            None => text(rule.get("user")),
        };
        list = list.child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .py(px(6.))
                .border_b_1()
                .border_color(p.divider)
                .child(chip)
                .child(
                    Chip::new(if text(rule.get("mode")) == "MODE_WRITE" {
                        "write"
                    } else {
                        "read"
                    })
                    .outlined()
                    .height(20.)
                    .font_size(11.),
                )
                .child(
                    div().w(px(140.)).flex_none().min_w_0().child(
                        muxus_ui::label::label(who)
                            .size(theme.typography.body2.size)
                            .line_height(theme.typography.body2.line_height),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.5))
                        .line_height(px(12.5 * 1.5))
                        .child(path_text(
                            &json_path_text(rule.get("path")),
                            0,
                            false,
                            true,
                            theme,
                        )),
                )
                .child(
                    caption_text(text(rule.get("id")), p.text.secondary, theme).font_family(MONO),
                ),
        );
    }
    if !state.groups.is_empty() {
        let mut groups = div().flex().flex_wrap().gap(px(4.)).mt(px(12.));
        for group in &state.groups {
            let users = group
                .get("users")
                .and_then(Value::as_array)
                .map(|users| {
                    users
                        .iter()
                        .map(|user| text(user.get("name")))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            groups =
                groups.child(Chip::new(format!("{}: {users}", text(group.get("name")))).outlined());
        }
        list = list.child(groups);
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_rules_through_drafts() {
        let rules = vec![json!({
            "id": "r1",
            "group": "ops",
            "path": {"elem": [{"name": "interface", "key": {"name": "ethernet-1/1"}}]},
            "mode": "MODE_WRITE",
            "action": "ACTION_DENY"
        })];
        let groups = vec![json!({"name": "ops", "users": [{"name": "alice"}, {"name": "bob"}]})];
        let (rule_drafts, group_drafts) = to_drafts(&rules, &groups);
        assert_eq!(rule_drafts[0].principal, "group");
        assert_eq!(rule_drafts[0].path, "/interface[name=ethernet-1/1]");
        assert_eq!(group_drafts[0].users, "alice, bob");
        let policy = policy_from_drafts(&rule_drafts, &group_drafts);
        assert_eq!(policy["rules"][0]["group"], "ops");
        assert_eq!(
            policy["rules"][0]["path"]["elem"][0]["key"]["name"],
            "ethernet-1/1"
        );
        assert_eq!(policy["groups"][0]["users"][1]["name"], "bob");
    }

    #[test]
    fn finds_problems_in_drafts() {
        let rule = RuleDraft {
            id: "a".into(),
            name: "alice".into(),
            ..RuleDraft::default()
        };
        assert_eq!(draft_problem(std::slice::from_ref(&rule)), None);
        assert_eq!(
            draft_problem(&[rule.clone(), rule.clone()]).as_deref(),
            Some("Rule ids must be unique.")
        );
        let nameless = RuleDraft {
            name: " ".into(),
            ..rule.clone()
        };
        assert_eq!(
            draft_problem(&[nameless]).as_deref(),
            Some("Rule a needs a user.")
        );
        let broken = RuleDraft {
            path: "/x[".into(),
            ..rule
        };
        assert_eq!(
            draft_problem(&[broken]).as_deref(),
            Some("The path of rule a does not parse.")
        );
    }
}
