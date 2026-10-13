//! gRPC authorization (`tools/AuthzTool.tsx`): probe a user and RPC
//! against the policy in force, and rotate the policy.

use muxus_core::management::{ManagementError, grpc_methods};
use muxus_ui::gpui_kit::{
    AnyElement, Context, Div, IntoElement, ParentElement, Styled, Window, div, px,
};
use muxus_ui::theme::{ActiveTheme, Theme};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::fields::EditorKey;
use super::super::logic::format::locale_time;
use super::super::logic::node_actions::pretty_json;
use super::super::logic::tools::{device_time, rotation_version};
use super::super::navigator::ListenerIndex;
use super::super::ui::{Dialog, ReviewApply, ReviewSection};
use super::super::view::ManagementView;
use super::super::widgets::MONO;
use super::super::workbench::ListenerForApp;
use super::kit::{ToolField, body_text, caption_text, default_user, error_alert};
use super::rotation::{Rotation, RotationState, RotationText};
use super::{contained, field_row, section, spacer, tool_header};
use muxus_ui::widgets::button::Button;
use muxus_ui::widgets::button::ButtonColor;
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::toggle_button::ToggleButtonGroup;
use muxus_ui::widgets::toggle_button::ToggleOption;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProbeRecord {
    pub user: String,
    pub rpc: String,
    pub action: String,
    pub version: Option<String>,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AuthzState {
    pub loading: bool,
    pub version: Option<String>,
    pub created_on: Option<Value>,
    pub policy: Option<String>,
    pub error: Option<ManagementError>,
    /// `rules` or `json`.
    pub view: String,
    pub editing: bool,
    pub draft: String,
    /// `None` until edited: the session's user.
    pub probe_user: Option<String>,
    pub probe_rpc: String,
    pub probes: Vec<ProbeRecord>,
    pub rotation: RotationState,
}

impl Default for AuthzState {
    fn default() -> Self {
        Self {
            loading: false,
            version: None,
            created_on: None,
            policy: None,
            error: None,
            view: "rules".into(),
            editing: false,
            draft: String::new(),
            probe_user: None,
            probe_rpc: "/gnmi.gNMI/Set".into(),
            probes: Vec::new(),
            rotation: RotationState::default(),
        }
    }
}

/// The RPCs worth probing: the gNMI ones and every gNOI/gNSI call Muxus knows.
pub fn probe_rpcs() -> Vec<String> {
    [
        "/gnmi.gNMI/Capabilities",
        "/gnmi.gNMI/Get",
        "/gnmi.gNMI/Set",
        "/gnmi.gNMI/Subscribe",
    ]
    .into_iter()
    .chain(grpc_methods::ALL.iter().copied())
    .map(str::to_owned)
    .collect()
}

/// The policy text indented, or as it is when it is not JSON.
pub fn pretty_policy(text: Option<&str>) -> String {
    match text {
        None | Some("") => String::new(),
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => pretty_json(&value),
            Err(_) => text.to_owned(),
        },
    }
}

/// Why the draft does not parse, if it does not.
pub fn draft_problem(draft: &str) -> Option<String> {
    serde_json::from_str::<Value>(draft)
        .err()
        .map(|error| error.to_string())
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| match item {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One rule of the policy (`RuleRow`).
fn rule_row(rule: &Value, allow: bool, theme: &Theme) -> Div {
    let p = &theme.palette;
    let body2 = theme.typography.body2;
    let mut principals = strings(
        rule.get("source")
            .and_then(|source| source.get("principals")),
    );
    if principals.is_empty() {
        principals.push("anyone".into());
    }
    let mut paths = strings(rule.get("request").and_then(|request| request.get("paths")));
    if paths.is_empty() {
        paths.push("every RPC".into());
    }
    let mut who = div().flex().flex_wrap().gap(px(4.)).mb(px(4.));
    for principal in principals {
        let shown = if principal == "*" {
            "everyone".to_owned()
        } else {
            principal
        };
        who = who.child(Chip::new(shown).outlined().height(20.).font_size(11.));
    }
    let mut what = div().flex().flex_wrap().gap(px(4.));
    for path in paths {
        let shown = if path == "/*" {
            "every RPC".to_owned()
        } else if path.is_empty() {
            "(empty)".to_owned()
        } else {
            path
        };
        what = what.child(Chip::new(shown).height(20.).font_size(11.).mono());
    }
    let mut chip = Chip::new(if allow { "Allow" } else { "Deny" })
        .height(22.)
        .min_width(58.);
    chip = if allow {
        chip.palette(p.success)
    } else {
        chip.palette(p.error)
    };
    div()
        .flex()
        .items_start()
        .gap(px(12.))
        .py(px(8.))
        .border_b_1()
        .border_color(p.divider)
        .child(chip)
        .child(
            div().w(px(180.)).flex_none().min_w_0().child(
                muxus_ui::label::label(
                    rule.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unnamed")
                        .to_owned(),
                )
                .size(body2.size)
                .line_height(body2.line_height)
                .weight(600.),
            ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(caption_text("Who", p.text.secondary, theme))
                .child(who)
                .child(caption_text("What", p.text.secondary, theme))
                .child(what),
        )
}

impl ManagementView {
    pub(super) fn render_authz(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let can_run = self.grpc().is_some();
        let state: AuthzState = self.tool("authz");
        let rotating = state.rotation.rotating();
        let probe_user = state
            .probe_user
            .clone()
            .unwrap_or_else(|| default_user(self.session_address().as_deref()));
        let reload = Button::new("authz-reload", "Reload")
            .disabled(!can_run || rotating)
            .on_click(
                cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| this.load_authz(cx)),
            )
            .into_any_element();
        let mut column = div().child(tool_header(
            "AdminPanelSettingsOutlined",
            "Authorization",
            "gnsi.authz.v1.Authz",
            "Who may call which gRPC methods on this device. Probe a user and RPC against the policy in force, and replace the policy safely: it takes effect, you test it, then make it final.",
            Some(reload),
            &theme,
        ));
        if let Some(bar) = self.rotation_bar(
            "authz",
            RotationText {
                what: "policy",
                plural: false,
                test_hint: "Probe it now: probes run against the new policy.",
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
                        this.update_tool::<AuthzState>("authz", |state| state.error = None);
                        cx.notify();
                    },
                ))),
            ));
        }

        // Probe
        let mut probe = div().child(
            field_row(8.)
                .child(self.tool_field(
                    ToolField::new("authz.user", "User", probe_user).width(160.),
                    window,
                    cx,
                ))
                .child(
                    self.tool_field(
                        ToolField::new("authz.rpc", "RPC", state.probe_rpc.clone())
                            .basis(300.)
                            .mono()
                            .options(probe_rpcs()),
                        window,
                        cx,
                    ),
                )
                .child(
                    contained("authz-probe", "Probe", ButtonColor::Primary)
                        .disabled(!can_run)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| this.probe_authz(cx),
                        )),
                ),
        );
        if !state.probes.is_empty() {
            let mut list = div().flex().flex_col().gap(px(4.)).mt(px(12.));
            for record in &state.probes {
                let permit = record.action == "ACTION_PERMIT";
                let chip = Chip::new(if permit {
                    "Permit"
                } else if record.action == "ACTION_DENY" {
                    "Deny"
                } else {
                    "Unspecified"
                })
                .height(20.)
                .min_width(64.);
                let chip = if permit {
                    chip.palette(p.success)
                } else {
                    chip.palette(p.error)
                };
                let version = record
                    .version
                    .clone()
                    .filter(|version| !version.is_empty())
                    .unwrap_or_else(|| "\u{2014}".into());
                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(chip)
                        .child(
                            body_text(
                                format!("{} \u{2192} {}", record.user, record.rpc),
                                p.text.primary,
                                &theme,
                            )
                            .font_family(MONO)
                            .text_size(px(12.)),
                        )
                        .child(spacer())
                        .child(caption_text(
                            format!("policy {version} \u{b7} {}", locale_time(record.at)),
                            p.text.secondary,
                            &theme,
                        )),
                );
            }
            probe = probe.child(list);
        }
        column = column.child(section("Probe", None, probe, &theme));

        // Policy in force
        let draft_error = if state.editing {
            draft_problem(&state.draft)
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
        if !state.editing {
            let selected = if state.view == "json" { 1 } else { 0 };
            actions = actions
                .child(
                    ToggleButtonGroup::exclusive(
                        "authz-view",
                        vec![ToggleOption::new("Rules"), ToggleOption::new("JSON")],
                        Some(selected),
                        cx.listener_index(|this: &mut Self, index, cx: &mut Context<Self>| {
                            this.update_tool::<AuthzState>("authz", |state| {
                                state.view = if index == 1 { "json" } else { "rules" }.into();
                            });
                            cx.notify();
                        }),
                    )
                    .inline(1.6, 8., 11.5),
                )
                .child(
                    Button::new("authz-edit", "Edit\u{2026}")
                        .disabled(!can_run || rotating || state.policy.is_none())
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| {
                                this.update_tool::<AuthzState>("authz", |state| {
                                    state.editing = true;
                                    state.draft = pretty_policy(state.policy.as_deref());
                                });
                                cx.notify();
                            },
                        )),
                );
        } else {
            actions = actions
                .child(
                    Button::new("authz-cancel", "Cancel")
                        .color(ButtonColor::Inherit)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| {
                                this.update_tool::<AuthzState>("authz", |state| {
                                    state.editing = false
                                });
                                cx.notify();
                            },
                        )),
                )
                .child(
                    contained(
                        "authz-review",
                        "Review & rotate\u{2026}",
                        ButtonColor::Primary,
                    )
                    .disabled(draft_error.is_some())
                    .on_click(cx.listener_for_app(
                        |this: &mut Self, cx: &mut Context<Self>| this.review_authz(cx),
                    )),
                );
        }
        let policy: Option<Value> = state
            .policy
            .as_deref()
            .filter(|policy| !policy.is_empty())
            .and_then(|policy| serde_json::from_str(policy).ok());
        let body: AnyElement = if state.loading {
            CircularProgress::new(("authz-progress", 1usize), 20., p.primary.main)
                .into_any_element()
        } else if state.editing {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(self.code_editor(EditorKey::Tool("authz.draft"), "json", &state.draft, false, Some((12, 32)), true, "", window, cx))
                .child(caption_text(
                    draft_error.clone().unwrap_or_else(|| {
                        "A gRPC authorization policy: allow_rules and deny_rules naming principals and RPC paths.".into()
                    }),
                    if draft_error.is_some() { p.error.main } else { p.text.secondary },
                    &theme,
                ))
                .into_any_element()
        } else if state.view == "json" || policy.is_none() {
            let text = pretty_policy(state.policy.as_deref());
            self.code_editor(
                EditorKey::Tool("authz.policy"),
                "json",
                &text,
                true,
                Some((4, 28)),
                true,
                "",
                window,
                cx,
            )
            .into_any_element()
        } else {
            let policy = policy.unwrap_or_default();
            let mut rules = div();
            if let Some(name) = policy
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
            {
                rules = rules.child(
                    body_text(name.to_owned(), p.text.primary, &theme)
                        .font_weight(muxus_ui::gpui_kit::FontWeight(700.))
                        .mb(px(8.)),
                );
            }
            for rule in policy
                .get("deny_rules")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                rules = rules.child(rule_row(rule, false, &theme));
            }
            for rule in policy
                .get("allow_rules")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                rules = rules.child(rule_row(rule, true, &theme));
            }
            rules
                .child(
                    caption_text(
                        "Deny rules win over allow rules; anything no rule allows is denied.",
                        p.text.secondary,
                        &theme,
                    )
                    .mt(px(8.)),
                )
                .into_any_element()
        };
        column
            .child(section(
                "Policy in force",
                Some(actions.into_any_element()),
                body,
                &theme,
            ))
            .into_any_element()
    }

    pub(super) fn load_authz(&mut self, cx: &mut Context<Self>) {
        if self.grpc().is_none() {
            return;
        }
        self.update_tool::<AuthzState>("authz", |state| {
            state.loading = true;
            state.error = None;
        });
        self.grpc_call(grpc_methods::AUTHZ_GET, json!({}), cx, |this, result, _| {
            this.update_tool::<AuthzState>("authz", |state| {
                state.loading = false;
                match result {
                    Ok(response) => {
                        state.version = response
                            .get("version")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        state.created_on = response.get("created_on").cloned();
                        state.policy = Some(
                            response
                                .get("policy")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        );
                    }
                    Err(error) => state.error = Some(error),
                }
            })
        });
        cx.notify();
    }

    fn probe_authz(&mut self, cx: &mut Context<Self>) {
        let state: AuthzState = self.tool("authz");
        let user = state
            .probe_user
            .clone()
            .unwrap_or_else(|| default_user(self.session_address().as_deref()));
        let user = muxus_core::js::js_trim(&user).to_owned();
        let rpc = muxus_core::js::js_trim(&state.probe_rpc).to_owned();
        if self.grpc().is_none() || user.is_empty() || rpc.is_empty() {
            return;
        }
        let request = json!({ "user": user, "rpc": rpc });
        self.grpc_call(
            grpc_methods::AUTHZ_PROBE,
            request,
            cx,
            move |this, result, _| match result {
                Ok(response) => {
                    let record = ProbeRecord {
                        user,
                        rpc,
                        action: response
                            .get("action")
                            .and_then(Value::as_str)
                            .unwrap_or("ACTION_UNSPECIFIED")
                            .to_owned(),
                        version: response
                            .get("version")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        at: chrono::Utc::now().timestamp_millis(),
                    };
                    this.update_tool::<AuthzState>("authz", |state| {
                        state.probes.insert(0, record);
                        state.probes.truncate(12);
                    });
                }
                Err(error) => {
                    this.update_tool::<AuthzState>("authz", |state| state.error = Some(error))
                }
            },
        );
    }

    fn review_authz(&mut self, cx: &mut Context<Self>) {
        let state: AuthzState = self.tool("authz");
        self.ui.dialog = Some(Dialog::Review {
            title: "Rotate the authorization policy".into(),
            description: Some(
                "The new policy takes effect as soon as it is uploaded, but stays provisional until you finalize it. Test it with a probe first; if anything is off, roll back."
                    .into(),
            ),
            sections: vec![ReviewSection::new(
                "policy".into(),
                pretty_policy(state.policy.as_deref()),
                pretty_policy(Some(&state.draft)),
                "json",
                None,
            )],
            loading: false,
            error: None,
            apply: Some((ReviewApply::Rotate("authz"), "Upload".into())),
            tab: 0,
        });
        cx.notify();
    }

    pub(super) fn rotate_authz(&mut self, cx: &mut Context<Self>) {
        let state: AuthzState = self.tool("authz");
        let Ok(policy) = serde_json::from_str::<Value>(&state.draft) else {
            return;
        };
        let version = rotation_version(chrono::Utc::now());
        let upload = json!({
            "upload_request": {
                "version": version,
                "created_on": chrono::Utc::now().timestamp(),
                "policy": serde_json::to_string(&policy).unwrap_or_default(),
            }
        });
        self.start_rotation(
            "authz",
            Rotation {
                method: grpc_methods::AUTHZ_ROTATE,
                upload,
                finalize_field: "finalize_rotation",
                version,
            },
            cx,
        );
        self.update_tool::<AuthzState>("authz", |state| state.editing = false);
    }

    pub(super) fn authz_field(&mut self, name: &str, value: String) {
        self.update_tool::<AuthzState>("authz", |state| match name {
            "authz.user" => state.probe_user = Some(value),
            "authz.rpc" => state.probe_rpc = value,
            "authz.draft" => state.draft = value,
            _ => {}
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_prints_policies() {
        assert_eq!(
            pretty_policy(Some("{\"name\":\"p\",\"allow_rules\":[]}")),
            "{\n  \"name\": \"p\",\n  \"allow_rules\": []\n}"
        );
        assert_eq!(pretty_policy(Some("not json")), "not json");
        assert_eq!(pretty_policy(None), "");
        assert!(draft_problem("{").is_some());
        assert!(draft_problem("{}").is_none());
    }

    #[test]
    fn offers_every_known_rpc_to_probe() {
        let rpcs = probe_rpcs();
        assert_eq!(rpcs[2], "/gnmi.gNMI/Set");
        assert!(rpcs.contains(&"/gnoi.system.System/Ping".to_owned()));
    }
}
