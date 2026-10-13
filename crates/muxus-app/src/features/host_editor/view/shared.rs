//! Form parts several editors share: the login sequence editor
//! (`LoginSequenceEditor.tsx`), the session logging fields
//! (`SessionLoggingPolicyFields.tsx`), the keyword rule list
//! (`KeywordHighlightRulesEditor.tsx`), the vault secret picker
//! (`VaultSecretField.tsx`), paste delay fields and color overrides.

use std::rc::Rc;

use muxus_core::api::{
    KeywordHighlightRule, LOGIN_SEQUENCE_MAX_STEPS, LOGIN_SEQUENCE_MAX_TIMEOUT_SECONDS,
    LoginSequence, LoginSequenceStep, PasswordVaultStatus,
};
use muxus_session::login_sequence::editor::{
    LoginSequenceDraft, LoginSequenceMode, LoginStepKind, change_login_step_kind, login_step_kind,
    login_step_summary, move_login_step, new_login_step, wait_pattern_error,
};
use muxus_term::keywords::keyword_pattern_error;
use muxus_term::paste::{PasteDelayKind, parse_paste_delay};
use muxus_ui::color::{alpha, try_hex};
use muxus_ui::gpui_kit::{
    AnyElement, App, Context, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::{ButtonColor, IconButton, Tooltip};

use super::form::{self, Choice, Form};
use super::ssh::{row, stack};
use crate::features::host_editor::logic::session_logging::{
    HostSessionLoggingDraft, clamp_integer, format_retention, inherit_caption,
};
use crate::features::host_editor::widgets::visuals::{color_box, color_choices};
use muxus_ui::widgets::Button;
use muxus_ui::widgets::form_control::FormControl;
use muxus_ui::widgets::form_control::FormControlLabel;
use muxus_ui::widgets::form_control::label_with_caption;
use muxus_ui::widgets::form_field::{FieldBody, FormField, end_adornment};
use muxus_ui::widgets::typography::{body1, body2, caption, mono, para, text};
use muxus_ui::widgets::typography::{code_size, weight};

/// Called with a field's new value.
type ChangeHandler<V, T> = Rc<dyn Fn(&mut V, T, &mut Context<V>)>;
/// A keyword rule's toggle: tooltip, glyph, state and how it flips the rule.
type RuleFlag = (
    &'static str,
    &'static str,
    bool,
    fn(&mut KeywordHighlightRule),
);

const ROW_HEIGHT: f32 = 40.;

const STEP_KINDS: [(LoginStepKind, &str); 3] = [
    (LoginStepKind::Wait, "Wait for"),
    (LoginStepKind::Send, "Send text"),
    (LoginStepKind::Secret, "Send secret"),
];

/// A compact on/off option with its name as tooltip (`FlagToggle`).
fn flag_toggle(
    theme: &Theme,
    id: SharedString,
    label: &'static str,
    selected: bool,
    content: AnyElement,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let p = &theme.palette;
    let selected_bg = alpha(p.text.primary, p.action.selected_opacity);
    let hover = alpha(p.text.primary, p.action.hover_opacity);
    let mut toggle = div()
        .id(id)
        .w(px(34.))
        .h(px(ROW_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.radius))
        .border_1()
        .border_color(p.divider)
        .cursor_pointer()
        .text_color(if selected {
            p.text.primary
        } else {
            p.action.active
        })
        .tooltip(Tooltip::text(label))
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(content);
    toggle = if selected {
        toggle.bg(selected_bg)
    } else {
        toggle.hover(move |s| s.bg(hover))
    };
    toggle.into_any_element()
}

fn mono_glyph(theme: &Theme, glyph: &'static str, selected: bool) -> AnyElement {
    let p = &theme.palette;
    mono(
        glyph,
        13.,
        18.,
        if selected {
            p.text.primary
        } else {
            p.action.active
        },
    )
    .no_truncate()
    .into_any_element()
}

/// `LoginSequenceEditor`: where the sequence comes from, and its own steps.
#[allow(clippy::too_many_arguments)]
pub fn login_sequence_editor<V: Form>(
    view: &mut V,
    prefix: &str,
    value: &LoginSequenceDraft,
    inherited: Option<(LoginSequence, String)>,
    inherit_label: &str,
    inherit_empty_text: &str,
    vault: Option<&PasswordVaultStatus>,
    window: &mut Window,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, LoginSequenceDraft, &mut Context<V>) + 'static,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let on_change = Rc::new(on_change);
    let mode_change = on_change.clone();
    let steps = value.steps.clone();
    let mut col = stack(2.).child(form::select(
        view,
        &format!("{prefix}-mode"),
        |body| {
            FormField::new(SharedString::from(format!("{prefix}-mode-field")), body)
                .label("Login sequence")
        },
        &value.mode,
        vec![
            Choice::new(LoginSequenceMode::Inherit, inherit_label.to_owned()),
            Choice::new(LoginSequenceMode::None, "No login sequence"),
            Choice::new(LoginSequenceMode::Custom, "Run these steps"),
        ],
        None,
        window,
        cx,
        move |this, mode, cx| {
            // A first switch to custom starts with the most common opening step.
            let steps = if mode == LoginSequenceMode::Custom && steps.is_empty() {
                vec![new_login_step(LoginStepKind::Wait)]
            } else {
                steps.clone()
            };
            mode_change(this, LoginSequenceDraft { mode, steps }, cx);
        },
    ));
    if value.mode == LoginSequenceMode::Inherit {
        col = col.child(match inherited {
            Some((sequence, folder)) => {
                let mut list = div()
                    .flex()
                    .flex_col()
                    .mt(Theme::spacing(0.5))
                    .pl(Theme::spacing(2.5));
                for (index, step) in sequence.steps.iter().enumerate() {
                    let secret = match step {
                        LoginSequenceStep::Secret(secret) => vault
                            .and_then(|v| v.secrets.iter().find(|s| s.id == secret.secret_id))
                            .map(|s| s.name.clone()),
                        _ => None,
                    };
                    list = list.child(
                        row(0.5)
                            .child(
                                div().w(px(16.)).ml(-Theme::spacing(2.5)).child(
                                    text(format!("{}.", index + 1), body2(), p.text.primary)
                                        .no_truncate(),
                                ),
                            )
                            .child(div().flex_1().min_w_0().child(para(
                                login_step_summary(step, secret.as_deref()),
                                body2(),
                                p.text.primary,
                            ))),
                    );
                }
                div()
                    .px(Theme::spacing(1.5))
                    .py(Theme::spacing(1.25))
                    .rounded(px(theme.radius))
                    .border_1()
                    .border_color(p.divider)
                    .child(text(
                        format!("From the folder “{folder}”"),
                        caption(),
                        p.text.secondary,
                    ))
                    .child(list)
                    .into_any_element()
            }
            None => {
                para(inherit_empty_text.to_owned(), body2(), p.text.secondary).into_any_element()
            }
        });
    }
    if value.mode == LoginSequenceMode::Custom {
        col = col.child(step_list(view, prefix, value, vault, window, cx, on_change));
    }
    col.into_any_element()
}

fn step_list<V: Form>(
    view: &mut V,
    prefix: &str,
    value: &LoginSequenceDraft,
    vault: Option<&PasswordVaultStatus>,
    window: &mut Window,
    cx: &mut Context<V>,
    on_change: ChangeHandler<V, LoginSequenceDraft>,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let mut list = stack(1.);
    let count = value.steps.len();
    for (index, step) in value.steps.iter().enumerate() {
        let number = index + 1;
        let id = step.id().to_owned();
        let key = format!("{prefix}-step-{id}");
        let update = {
            let on_change = on_change.clone();
            let id = id.clone();
            move |this: &mut V,
                  cx: &mut Context<V>,
                  draft: &LoginSequenceDraft,
                  f: &dyn Fn(&LoginSequenceStep) -> LoginSequenceStep| {
                let steps = draft
                    .steps
                    .iter()
                    .map(|s| if s.id() == id { f(s) } else { s.clone() })
                    .collect();
                on_change(
                    this,
                    LoginSequenceDraft {
                        mode: draft.mode,
                        steps,
                    },
                    cx,
                );
            }
        };
        let update = Rc::new(update);
        let draft_now = value.clone();

        let kind_change = {
            let update = update.clone();
            let draft = draft_now.clone();
            move |this: &mut V, kind: LoginStepKind, cx: &mut Context<V>| {
                update(this, cx, &draft, &|s| change_login_step_kind(s, kind));
            }
        };
        let kind_select = form::select(
            view,
            &format!("{key}-kind"),
            |body| {
                FormField::new(SharedString::from(format!("{key}-kind-field")), body).width(140.)
            },
            &login_step_kind(step),
            STEP_KINDS
                .iter()
                .map(|(k, l)| Choice::new(*k, *l))
                .collect(),
            None,
            window,
            cx,
            kind_change,
        );

        let fields: AnyElement = match step {
            LoginSequenceStep::Wait(wait) => {
                let error = (wait.regex == Some(true) && !wait.pattern.is_empty())
                    .then(|| wait_pattern_error(&wait.pattern))
                    .flatten();
                let (u1, u2, u3) = (update.clone(), update.clone(), update.clone());
                let (d1, d2, d3) = (draft_now.clone(), draft_now.clone(), draft_now.clone());
                let pattern = form::text_input(
                    view,
                    format!("{key}-pattern"),
                    window,
                    cx,
                    wait.pattern.clone(),
                    move |this, v, cx| {
                        u1(this, cx, &d1, &|s| match s {
                            LoginSequenceStep::Wait(w) => {
                                LoginSequenceStep::Wait(muxus_core::api::LoginWaitStep {
                                    pattern: v.clone(),
                                    ..w.clone()
                                })
                            }
                            other => other.clone(),
                        });
                        None
                    },
                );
                let timeout = form::text_input(
                    view,
                    format!("{key}-timeout"),
                    window,
                    cx,
                    wait.timeout_seconds.to_string(),
                    move |this, v, cx| {
                        let digits: String =
                            v.chars().filter(char::is_ascii_digit).take(4).collect();
                        let fix = (digits != v).then(|| digits.clone());
                        let seconds = digits.parse::<u32>().unwrap_or(0);
                        u2(this, cx, &d2, &|s| match s {
                            LoginSequenceStep::Wait(w) => {
                                LoginSequenceStep::Wait(muxus_core::api::LoginWaitStep {
                                    timeout_seconds: seconds,
                                    ..w.clone()
                                })
                            }
                            other => other.clone(),
                        });
                        fix
                    },
                );
                let regex = wait.regex == Some(true);
                let entity = cx.entity().downgrade();
                let toggle = flag_toggle(
                    &theme,
                    SharedString::from(format!("{key}-regex")),
                    "Regular expression",
                    regex,
                    mono_glyph(&theme, ".*", regex),
                    move |_, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            u3(this, cx, &d3, &|s| match s {
                                LoginSequenceStep::Wait(w) => {
                                    LoginSequenceStep::Wait(muxus_core::api::LoginWaitStep {
                                        regex: if regex { None } else { Some(true) },
                                        ..w.clone()
                                    })
                                }
                                other => other.clone(),
                            });
                        });
                    },
                );
                let bad_timeout = wait.timeout_seconds < 1
                    || wait.timeout_seconds > LOGIN_SEQUENCE_MAX_TIMEOUT_SECONDS;
                row(0.75)
                    .items_start()
                    .child(
                        FormField::input(
                            SharedString::from(format!("{key}-pattern-field")),
                            &pattern,
                        )
                        .placeholder(if regex { "[>#]\\s*$" } else { "Password:" })
                        .error(error.is_some())
                        .helper_opt(error)
                        .mono(13.)
                        .flexible(),
                    )
                    .child(toggle)
                    .child(
                        div()
                            .id(SharedString::from(format!("{key}-timeout-tip")))
                            .tooltip(Tooltip::text(
                                "Stop the sequence if nothing matches within this time",
                            ))
                            .child(
                                FormField::input(
                                    SharedString::from(format!("{key}-timeout-field")),
                                    &timeout,
                                )
                                .error(bad_timeout)
                                .end(end_adornment("s", &theme))
                                .width(84.)
                                .no_shrink(),
                            ),
                    )
                    .into_any_element()
            }
            LoginSequenceStep::Send(send) => {
                let (u1, u2) = (update.clone(), update.clone());
                let (d1, d2) = (draft_now.clone(), draft_now.clone());
                let input = form::text_input(
                    view,
                    format!("{key}-text"),
                    window,
                    cx,
                    send.text.clone(),
                    move |this, v, cx| {
                        u1(this, cx, &d1, &|s| match s {
                            LoginSequenceStep::Send(w) => {
                                LoginSequenceStep::Send(muxus_core::api::LoginSendStep {
                                    text: v.clone(),
                                    ..w.clone()
                                })
                            }
                            other => other.clone(),
                        });
                        None
                    },
                );
                let enter = send.enter;
                let entity = cx.entity().downgrade();
                row(0.75)
                    .items_start()
                    .child(
                        FormField::input(SharedString::from(format!("{key}-text-field")), &input)
                            .placeholder(if enter {
                                "Nothing — only press Enter"
                            } else {
                                "enable"
                            })
                            .mono(13.)
                            .flexible(),
                    )
                    .child(flag_toggle(
                        &theme,
                        SharedString::from(format!("{key}-enter")),
                        "Press Enter afterwards",
                        enter,
                        icon(
                            "KeyboardReturn",
                            17.,
                            if enter {
                                p.text.primary
                            } else {
                                p.action.active
                            },
                        )
                        .into_any_element(),
                        move |_, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                u2(this, cx, &d2, &|s| match s {
                                    LoginSequenceStep::Send(w) => {
                                        LoginSequenceStep::Send(muxus_core::api::LoginSendStep {
                                            enter: !enter,
                                            ..w.clone()
                                        })
                                    }
                                    other => other.clone(),
                                });
                            });
                        },
                    ))
                    .into_any_element()
            }
            LoginSequenceStep::Secret(secret) => {
                let (u1, u2) = (update.clone(), update.clone());
                let (d1, d2) = (draft_now.clone(), draft_now.clone());
                let enter = secret.enter;
                let field = vault_secret_field(
                    view,
                    &format!("{key}-secret"),
                    "Secret",
                    (!secret.secret_id.is_empty()).then(|| secret.secret_id.clone()),
                    vault,
                    window,
                    cx,
                    move |this, id, cx| {
                        u1(this, cx, &d1, &|s| match s {
                            LoginSequenceStep::Secret(w) => {
                                LoginSequenceStep::Secret(muxus_core::api::LoginSecretStep {
                                    secret_id: id.clone(),
                                    ..w.clone()
                                })
                            }
                            other => other.clone(),
                        });
                    },
                );
                let entity = cx.entity().downgrade();
                row(0.75)
                    .items_start()
                    .child(div().flex_1().min_w_0().child(field))
                    .child(flag_toggle(
                        &theme,
                        SharedString::from(format!("{key}-enter")),
                        "Press Enter afterwards",
                        enter,
                        icon(
                            "KeyboardReturn",
                            17.,
                            if enter {
                                p.text.primary
                            } else {
                                p.action.active
                            },
                        )
                        .into_any_element(),
                        move |_, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                u2(this, cx, &d2, &|s| match s {
                                    LoginSequenceStep::Secret(w) => LoginSequenceStep::Secret(
                                        muxus_core::api::LoginSecretStep {
                                            enter: !enter,
                                            ..w.clone()
                                        },
                                    ),
                                    other => other.clone(),
                                });
                            });
                        },
                    ))
                    .into_any_element()
            }
        };

        let (up, down, delete) = (on_change.clone(), on_change.clone(), on_change.clone());
        let (draft_up, draft_down, draft_delete) =
            (draft_now.clone(), draft_now.clone(), draft_now.clone());
        let (e1, e2, e3) = (
            cx.entity().downgrade(),
            cx.entity().downgrade(),
            cx.entity().downgrade(),
        );
        let delete_id = id.clone();
        let buttons = row(0.)
            .flex_none()
            .h(px(ROW_HEIGHT))
            .items_center()
            .ml_auto()
            .child(
                IconButton::new(SharedString::from(format!("{key}-up")), "ArrowUpward")
                    .tooltip("Move up")
                    .disabled(index == 0)
                    .on_click(move |_, _, cx| {
                        let _ = e1.update(cx, |this, cx| {
                            up(
                                this,
                                LoginSequenceDraft {
                                    mode: draft_up.mode,
                                    steps: move_login_step(&draft_up.steps, index, -1),
                                },
                                cx,
                            );
                        });
                    }),
            )
            .child(
                IconButton::new(SharedString::from(format!("{key}-down")), "ArrowDownward")
                    .tooltip("Move down")
                    .disabled(index + 1 == count)
                    .on_click(move |_, _, cx| {
                        let _ = e2.update(cx, |this, cx| {
                            down(
                                this,
                                LoginSequenceDraft {
                                    mode: draft_down.mode,
                                    steps: move_login_step(&draft_down.steps, index, 1),
                                },
                                cx,
                            );
                        });
                    }),
            )
            .child(
                IconButton::new(
                    SharedString::from(format!("{key}-delete")),
                    "DeleteOutlined",
                )
                .tooltip("Delete step")
                .color(ButtonColor::Error)
                .on_click(move |_, _, cx| {
                    let _ = e3.update(cx, |this, cx| {
                        let steps = draft_delete
                            .steps
                            .iter()
                            .filter(|s| s.id() != delete_id)
                            .cloned()
                            .collect();
                        delete(
                            this,
                            LoginSequenceDraft {
                                mode: draft_delete.mode,
                                steps,
                            },
                            cx,
                        );
                    });
                }),
            );
        list = list.child(
            row(0.75)
                .items_start()
                .flex_wrap()
                .child(
                    div()
                        .w(px(18.))
                        .h(px(ROW_HEIGHT))
                        .flex()
                        .items_center()
                        .justify_end()
                        .child(
                            text(number.to_string(), body2(), p.text.secondary)
                                .no_truncate()
                                .tabular_nums(),
                        ),
                )
                .child(div().w(px(140.)).flex_none().child(kind_select))
                .child(div().flex_1().min_w(px(180.)).child(fields))
                .child(buttons),
        );
    }
    let full = count >= LOGIN_SEQUENCE_MAX_STEPS;
    let mut adds = row(1.).flex_wrap().pt(Theme::spacing(0.5));
    for (kind, label) in STEP_KINDS {
        let on_change = on_change.clone();
        let draft = value.clone();
        let entity = cx.entity().downgrade();
        adds = adds.child(
            Button::new(SharedString::from(format!("{prefix}-add-{label}")), label)
                .disabled(full)
                .start_icon("Add")
                .on_click(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        let mut steps = draft.steps.clone();
                        steps.push(new_login_step(kind));
                        on_change(
                            this,
                            LoginSequenceDraft {
                                mode: draft.mode,
                                steps,
                            },
                            cx,
                        );
                    });
                }),
        );
    }
    list.child(adds).into_any_element()
}

/// `VaultSecretField`: a named secret from the password vault, by id. A
/// reference whose secret was deleted shows as missing until another is picked.
#[allow(clippy::too_many_arguments)]
pub fn vault_secret_field<V: Form>(
    view: &mut V,
    key: &str,
    label: &str,
    value: Option<String>,
    vault: Option<&PasswordVaultStatus>,
    window: &mut Window,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, String, &mut Context<V>) + 'static,
) -> AnyElement {
    let on_change = Rc::new(on_change);
    const MISSING: &str = "\u{0}missing-secret";
    const NEW: &str = "\u{0}new-secret";
    let theme = *cx.theme();
    let configured = vault.map(|v| v.configured).unwrap_or(true);
    let secrets = vault.map(|v| v.secrets.clone()).unwrap_or_default();
    let missing = value
        .as_ref()
        .is_some_and(|id| vault.is_some() && !secrets.iter().any(|s| &s.id == id));
    let current = if missing {
        MISSING.to_owned()
    } else {
        value.clone().unwrap_or_default()
    };
    let mut choices = Vec::new();
    if missing {
        choices.push(Choice::new(MISSING.to_owned(), "Missing secret").disabled());
    }
    for secret in &secrets {
        let label = match &secret.username {
            Some(user) if !user.is_empty() => format!("{}   {user}", secret.name),
            _ => secret.name.clone(),
        };
        choices.push(Choice::new(secret.id.clone(), label));
    }
    choices.push(Choice::new(NEW.to_owned(), "New secret…"));
    let display = if missing {
        FieldBody::Value("Missing secret".into())
    } else {
        match secrets.iter().find(|s| Some(&s.id) == value.as_ref()) {
            Some(secret) => FieldBody::Value(match &secret.username {
                Some(user) if !user.is_empty() => format!("{} · {user}", secret.name).into(),
                _ => secret.name.clone().into(),
            }),
            None => FieldBody::Muted("Choose a secret".into()),
        }
    };
    let helper: Option<String> = if !configured {
        Some("Secrets live in the password vault. Create it under Settings → Passwords.".into())
    } else if missing {
        Some("This secret was deleted from the vault. Pick another one.".into())
    } else {
        None
    };
    let label = label.to_owned();
    let _ = theme;
    form::select(
        view,
        key,
        move |body| {
            FormField::new(SharedString::from(format!("{label}-secret-field")), body)
                .label(label.clone())
                .shrink(true)
                .error(missing)
                .disabled(!configured)
                .helper_opt(helper)
        },
        &current,
        choices,
        Some(display),
        window,
        cx,
        move |this, id, cx| {
            if id == NEW {
                // "New secret…": the vault dialog; the field takes the secret it saves.
                let view = cx.entity().downgrade();
                let on_change = on_change.clone();
                cx.defer(move |cx| {
                    crate::features::vault::secret_dialog::request_new_secret(
                        Box::new(move |saved, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.vault_changed(saved.status);
                                on_change(this, saved.secret.id, cx);
                                cx.notify();
                            });
                        }),
                        cx,
                    );
                });
            } else if id != MISSING {
                on_change(this, id, cx);
            }
        },
    )
}

/// `SessionLoggingPolicyFields` (with the inherit switch when `allow_inherit`).
pub fn logging_fields<V: Form>(
    view: &mut V,
    prefix: &str,
    value: &HostSessionLoggingDraft,
    allow_inherit: bool,
    window: &mut Window,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, HostSessionLoggingDraft, &mut Context<V>) + 'static,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let disabled = !value.loaded || (allow_inherit && value.inherit);
    let on_change = Rc::new(on_change);
    let draft = *value;
    let switch_row = |id: &str,
                      checked: bool,
                      row_disabled: bool,
                      title: &str,
                      sub: &str,
                      apply: fn(&mut HostSessionLoggingDraft)| {
        let entity = cx.entity().downgrade();
        let on_change = on_change.clone();
        FormControlLabel::new(
            SharedString::from(format!("{prefix}-{id}")),
            FormControl::Switch(checked),
            "",
        )
        .label_element(label_with_caption(
            &theme,
            title.to_owned(),
            sub.to_owned(),
            0.,
            row_disabled,
        ))
        .flush()
        .disabled(row_disabled)
        .on_click(move |_, _, cx| {
            let _ = entity.update(cx, |this, cx| {
                let mut next = draft;
                apply(&mut next);
                on_change(this, next, cx);
            });
        })
    };
    let mut col = stack(2.25);
    if allow_inherit {
        col = col.child(switch_row(
            "inherit",
            value.inherit,
            !value.loaded,
            "Use default session logging settings",
            &inherit_caption(value),
            |d| d.inherit = !d.inherit,
        ));
    }
    col = col
        .child(switch_row(
            "enabled",
            value.enabled,
            disabled,
            "Log terminal sessions",
            "Store timestamped raw activity and a searchable normalized transcript.",
            |d| d.enabled = !d.enabled,
        ))
        .child(switch_row(
            "files",
            value.log_to_file,
            disabled,
            "Write log files",
            "Save each new session to a plain-text log file as it runs, independent of history.",
            |d| d.log_to_file = !d.log_to_file,
        ))
        .child(
            switch_row(
                "input",
                value.capture_input,
                disabled || (!value.enabled && !value.log_to_file),
                "Record terminal input",
                "Off is safer: typed commands may contain passwords or tokens.",
                |d| d.capture_input = !d.capture_input,
            )
            .tooltip("When disabled, client input bytes are never persisted. Commands echoed by the remote shell are still output."),
        );
    let (part_change, parts_change) = (on_change.clone(), on_change.clone());
    let part = form::text_input(
        view,
        format!("{prefix}-part"),
        window,
        cx,
        ((value.max_part_bytes as f64 / 1024. / 1024.).round() as u64).to_string(),
        move |this, v, cx| {
            let mut next = draft;
            next.max_part_bytes =
                clamp_integer(super::super::logic::js::js_number(&v), 1, 1024) as u64 * 1024 * 1024;
            part_change(this, next, cx);
            None
        },
    );
    let parts = form::text_input(
        view,
        format!("{prefix}-parts"),
        window,
        cx,
        value.max_parts.to_string(),
        move |this, v, cx| {
            let mut next = draft;
            next.max_parts = clamp_integer(super::super::logic::js::js_number(&v), 1, 1000) as u32;
            parts_change(this, next, cx);
            None
        },
    );
    col = col.child(
        row(1.5)
            .child(
                FormField::input(SharedString::from(format!("{prefix}-part-field")), &part)
                    .label("Rotate part at")
                    .disabled(disabled || !value.enabled)
                    .end(text("MiB", body1(), p.text.secondary).no_truncate())
                    .width(190.),
            )
            .child(
                FormField::input(SharedString::from(format!("{prefix}-parts-field")), &parts)
                    .label("Retain newest parts")
                    .disabled(disabled || !value.enabled)
                    .width(190.),
            ),
    );
    if value.enabled && !disabled {
        col = col.child(text(
            format!(
                "Maximum retained raw data per session: approximately {}.",
                format_retention(value)
            ),
            caption(),
            p.text.secondary,
        ));
    }
    col.into_any_element()
}

/// `LoggingSection`: the heading, the fields and the input-recording warning.
pub fn logging_section<V: Form>(
    view: &mut V,
    prefix: &str,
    value: &HostSessionLoggingDraft,
    description: &str,
    window: &mut Window,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, HostSessionLoggingDraft, &mut Context<V>) + 'static,
) -> AnyElement {
    let theme = *cx.theme();
    let mut col = stack(2.)
        .child(crate::features::host_editor::widgets::section::intro(
            &theme,
            "Session logging",
            description.to_owned(),
        ))
        .child(logging_fields(
            view, prefix, value, true, window, cx, on_change,
        ));
    if !value.inherit && (value.enabled || value.log_to_file) && value.capture_input {
        col = col.child(muxus_ui::widgets::Alert::new(
            Status::Warning,
            "Input recording can retain commands containing credentials. Output echoed by the remote shell may be retained regardless of this switch.",
        ));
    }
    col.into_any_element()
}

/// One color input: the box, and its choices while open.
#[allow(clippy::too_many_arguments)]
pub fn color_override<V: Form>(
    view: &mut V,
    key: &'static str,
    label: &str,
    value: Option<&str>,
    default_value: &str,
    mixed: bool,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, Option<String>, &mut Context<V>) + 'static,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let on_change = Rc::new(on_change);
    let anchor = view.menus().anchor(key);
    let open = view.menus().is_open(key);
    let shown = value.unwrap_or(default_value).to_owned();
    let entity = cx.entity().downgrade();
    let toggle_key = key;
    let mut boxed = color_box(&theme, format!("{key}-box"), &shown, 34., 30.)
        .on_click(move |_, _, cx| {
            let _ = entity.update(cx, |this, cx| {
                this.menus().toggle(toggle_key);
                cx.notify();
            });
        })
        .child(muxus_ui::widgets::tooltip::bounds_probe(&anchor));
    boxed = boxed.relative();
    let mut row_el = row(1.5)
        .items_center()
        .child(
            div()
                .min_w(px(118.))
                .child(text(label.to_owned(), body2(), p.text.secondary).no_truncate()),
        )
        .child(boxed);
    if mixed {
        row_el = row_el.child(text("Multiple values", caption(), p.text.secondary).no_truncate());
    }
    if value.is_some() || mixed {
        let entity = cx.entity().downgrade();
        let reset = on_change.clone();
        row_el = row_el.child(
            Button::new(
                SharedString::from(format!("{key}-reset")),
                "Use application default",
            )
            .on_click(move |_, _, cx| {
                let _ = entity.update(cx, |this, cx| reset(this, None, cx));
            }),
        );
    } else {
        row_el = row_el.child(
            text("Following application default", caption(), p.text.secondary).no_truncate(),
        );
    }
    let mut root = div().child(row_el);
    if open && let Some(bounds) = anchor.get() {
        let entity = cx.entity().downgrade();
        let pick = on_change.clone();
        let picker = color_choices(
            &theme,
            &shown,
            Rc::new(move |color, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.menus().close();
                    pick(this, Some(color), cx);
                });
            }),
        );
        let close = cx.entity().downgrade();
        root = root.child(muxus_ui::widgets::menu::popup(
            muxus_ui::gpui_kit::point(bounds.origin.x, bounds.origin.y + bounds.size.height),
            move |_, cx| {
                let _ = close.update(cx, |this, cx| {
                    this.menus().close();
                    cx.notify();
                });
            },
            muxus_ui::widgets::menu::paper(&theme)
                .p(Theme::spacing(1.5))
                .child(picker),
        ));
    }
    root.into_any_element()
}

/// `PasteDelayField`: a host's own paste delay; empty follows the Terminal settings.
#[allow(clippy::too_many_arguments)]
pub fn paste_delay_field<V: Form>(
    view: &mut V,
    key: &str,
    kind: PasteDelayKind,
    value: Option<u32>,
    default_value: f64,
    mixed: bool,
    window: &mut Window,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, Option<u32>, &mut Context<V>) + 'static,
) -> AnyElement {
    let theme = *cx.theme();
    let label = match kind {
        PasteDelayKind::Line => "Delay after each line",
        PasteDelayKind::Char => "Delay after each character",
    };
    let input = form::text_input(
        view,
        key.to_owned(),
        window,
        cx,
        value.map(|v| v.to_string()).unwrap_or_default(),
        move |this, v, cx| {
            on_change(this, parse_paste_delay(&v, kind), cx);
            None
        },
    );
    let default_text = muxus_core::js::js_number(default_value).to_string();
    FormField::input(SharedString::from(format!("{key}-field")), &input)
        .label(label)
        .shrink(true)
        .placeholder(if mixed {
            "Multiple values".to_owned()
        } else {
            format!("{default_text} (Terminal settings)")
        })
        .end(end_adornment("ms", &theme))
        .into_any_element()
}

/// The style preview of a keyword rule ("Ab" in its colors).
fn rule_style(theme: &Theme, rule: &KeywordHighlightRule) -> impl IntoElement {
    let fg = try_hex(&rule.foreground).unwrap_or(theme.palette.text.primary);
    let bg = rule
        .background
        .as_deref()
        .and_then(try_hex)
        .unwrap_or(Hsla::transparent_black());
    div()
        .w(px(44.))
        .h(px(ROW_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.radius))
        .border_1()
        .border_color(theme.palette.divider)
        .bg(theme.palette.background_default)
        .child(
            div()
                .px(Theme::spacing(0.5))
                .rounded(px(theme.radius * 0.5))
                .bg(bg)
                .child(
                    mono("Ab", code_size::BODY, 18., fg)
                        .weight(weight::SEMIBOLD)
                        .no_truncate(),
                ),
        )
}

/// `KeywordHighlightRulesEditor` (the rule rows; JSON editing is not offered yet).
pub fn keyword_rules_editor<V: Form>(
    view: &mut V,
    prefix: &str,
    rules: &[KeywordHighlightRule],
    empty_message: &str,
    window: &mut Window,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, Vec<KeywordHighlightRule>, &mut Context<V>) + 'static,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let on_change: ChangeHandler<V, Vec<KeywordHighlightRule>> = Rc::new(on_change);
    let mut col = stack(0.75);
    if rules.is_empty() {
        col = col.child(para(empty_message.to_owned(), body2(), p.text.secondary));
    }
    let count = rules.len();
    for (index, rule) in rules.iter().enumerate() {
        let id = rule.id.clone();
        let key = format!("{prefix}-rule-{id}");
        let all = rules.to_vec();
        let set = {
            let on_change = on_change.clone();
            let id = id.clone();
            let all = all.clone();
            Rc::new(
                move |this: &mut V, cx: &mut Context<V>, f: &dyn Fn(&mut KeywordHighlightRule)| {
                    let next = all
                        .iter()
                        .map(|r| {
                            let mut r = r.clone();
                            if r.id == id {
                                f(&mut r);
                            }
                            r
                        })
                        .collect();
                    on_change(this, next, cx);
                },
            )
        };
        let (s1, s2) = (set.clone(), set.clone());
        let name = form::text_input(
            view,
            format!("{key}-name"),
            window,
            cx,
            rule.name.clone().unwrap_or_default(),
            move |this, v, cx| {
                s1(this, cx, &|r| r.name = (!v.is_empty()).then(|| v.clone()));
                None
            },
        );
        let keyword = form::text_input(
            view,
            format!("{key}-keyword"),
            window,
            cx,
            rule.keyword.clone(),
            move |this, v, cx| {
                s2(this, cx, &|r| r.keyword = v.clone());
                None
            },
        );
        let error = keyword_pattern_error(&rule.keyword, rule.regex == Some(true));
        let regex = rule.regex == Some(true);
        let flags: [RuleFlag; 3] = [
            ("Match case", "Aa", rule.case_sensitive, |r| {
                r.case_sensitive = !r.case_sensitive
            }),
            ("Whole word", "ab", rule.whole_word, |r| {
                r.whole_word = !r.whole_word
            }),
            ("Regular expression", ".*", regex, |r| {
                r.regex = if r.regex == Some(true) {
                    None
                } else {
                    Some(true)
                }
            }),
        ];
        let mut toggles = row(0.25).flex_none();
        for (label, glyph, selected, flip) in flags {
            let entity = cx.entity().downgrade();
            let set = set.clone();
            toggles = toggles.child(flag_toggle(
                &theme,
                SharedString::from(format!("{key}-{glyph}")),
                label,
                selected,
                mono_glyph(&theme, glyph, selected),
                move |_, cx| {
                    let _ = entity.update(cx, |this, cx| set(this, cx, &|r| flip(r)));
                },
            ));
        }
        let all_moves = all.clone();
        let (mv_up, mv_down, rm) = (on_change.clone(), on_change.clone(), on_change.clone());
        let (e1, e2, e3) = (
            cx.entity().downgrade(),
            cx.entity().downgrade(),
            cx.entity().downgrade(),
        );
        let (a1, a2, a3) = (all_moves.clone(), all_moves.clone(), all_moves);
        let remove_id = id.clone();
        let label = rule
            .name
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                if rule.keyword.is_empty() {
                    "rule".into()
                } else {
                    rule.keyword.clone()
                }
            });
        let _ = label;
        col = col.child(
            row(0.75)
                .items_start()
                .child(rule_style(&theme, rule))
                .child(
                    FormField::input(SharedString::from(format!("{key}-name-field")), &name)
                        .placeholder("Name (optional)")
                        .width(170.)
                        .no_shrink(),
                )
                .child(
                    FormField::input(SharedString::from(format!("{key}-keyword-field")), &keyword)
                        .placeholder(if regex {
                            "Regular expression"
                        } else {
                            "Keyword"
                        })
                        .error(rule.keyword.is_empty() || error.is_some())
                        .helper_opt(error)
                        .flexible(),
                )
                .child(toggles)
                .child(
                    row(0.)
                        .flex_none()
                        .h(px(ROW_HEIGHT))
                        .items_center()
                        .child(
                            IconButton::new(SharedString::from(format!("{key}-up")), "ArrowUpward")
                                .tooltip("Move up")
                                .disabled(index == 0)
                                .on_click(move |_, _, cx| {
                                    let _ = e1.update(cx, |this, cx| {
                                        let mut next = a1.clone();
                                        next.swap(index, index - 1);
                                        mv_up(this, next, cx);
                                    });
                                }),
                        )
                        .child(
                            IconButton::new(
                                SharedString::from(format!("{key}-down")),
                                "ArrowDownward",
                            )
                            .tooltip("Move down")
                            .disabled(index + 1 == count)
                            .on_click(move |_, _, cx| {
                                let _ = e2.update(cx, |this, cx| {
                                    let mut next = a2.clone();
                                    next.swap(index, index + 1);
                                    mv_down(this, next, cx);
                                });
                            }),
                        )
                        .child(
                            IconButton::new(
                                SharedString::from(format!("{key}-delete")),
                                "DeleteOutlined",
                            )
                            .tooltip("Delete rule")
                            .color(ButtonColor::Error)
                            .on_click(move |_, _, cx| {
                                let _ = e3.update(cx, |this, cx| {
                                    let next =
                                        a3.iter().filter(|r| r.id != remove_id).cloned().collect();
                                    rm(this, next, cx);
                                });
                            }),
                        ),
                ),
        );
    }
    let entity = cx.entity().downgrade();
    let add = on_change.clone();
    let all = rules.to_vec();
    col = col.child(
        row(1.).pt(Theme::spacing(0.5)).child(
            Button::new(SharedString::from(format!("{prefix}-add-rule")), "Add rule")
                .start_icon("Add")
                .on_click(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        let mut next = all.clone();
                        next.push(KeywordHighlightRule {
                            id: muxus_store::prefs::command_buttons::new_preference_id("highlight"),
                            name: None,
                            keyword: "ERROR".into(),
                            foreground: KeywordHighlightRule::DEFAULT_FOREGROUND.into(),
                            background: Some(KeywordHighlightRule::DEFAULT_BACKGROUND.into()),
                            case_sensitive: false,
                            whole_word: true,
                            regex: None,
                        });
                        add(this, next, cx);
                    });
                }),
        ),
    );
    col.into_any_element()
}
