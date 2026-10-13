//! The bars above the request editor (`CandidateBar.tsx`): the NETCONF
//! candidate at a glance (uncommitted changes, a confirmed commit counting
//! down, locks held) and the countdown of a gNMI commit-confirmed Set.

use super::workbench::ListenerForApp;
use muxus_mgmt::workbench::controller::CommitAction;
use muxus_mgmt::workbench::requests::{
    Datastore, NetconfDraft, NetconfOperation, default_netconf_draft,
};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::ActiveTheme;

use super::logic::format::countdown;
use super::ui::{ConfirmAction, Dialog, PromptAction, ReviewApply};
use super::view::ManagementView;
use muxus_ui::widgets::button::Button;
use muxus_ui::widgets::button::ButtonColor;
use muxus_ui::widgets::progress::CircularProgress;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl ManagementView {
    /// Run a NETCONF operation built on the default draft.
    pub(super) fn run_netconf(&mut self, patch: impl FnOnce(&mut NetconfDraft)) {
        let mut draft = default_netconf_draft();
        patch(&mut draft);
        self.controller.run(
            Some(muxus_mgmt::workbench::requests::ManagementDraft::Netconf(
                draft,
            )),
            false,
        );
    }

    pub(super) fn render_candidate_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = *cx.theme();
        let p = &theme.palette;
        if !self.controller.has_capability(":candidate") || !self.connected() {
            return None;
        }
        let (candidate, deadline, locks) = self.store.read(|state| {
            (
                state.candidate.clone(),
                state.confirm_deadline,
                state.locks.clone(),
            )
        });
        let now = now_ms();
        let pending = deadline.is_some_and(|deadline| deadline as i64 > now);
        let dirty = candidate.as_ref().is_some_and(|candidate| candidate.dirty);
        let checking = candidate
            .as_ref()
            .is_some_and(|candidate| candidate.checking);
        if !dirty && !pending && locks.is_empty() && !checking {
            return None;
        }
        let body2 = theme.typography.body2;
        let caption = theme.typography.caption;
        let mut bar = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(4.))
            .min_h(px(38.))
            .border_b_1()
            .border_color(p.divider);
        if pending {
            bar = bar.bg(alpha(p.warning.main, 0.14));
        } else if dirty {
            bar = bar.bg(alpha(p.info.main, 0.1));
        }
        let small = |text: &str, weight: f32| {
            label(text.to_owned())
                .size(body2.size)
                .weight(weight)
                .line_height(body2.line_height)
                .no_truncate()
        };
        if pending {
            let left = deadline.unwrap_or_default() as i64 - now;
            bar = bar
                .child(icon("TimerOutlined", 18., p.warning.main))
                .child(small(&format!("Rolls back in {}", countdown(left)), 600.))
                .child(
                    label("unless you confirm the commit")
                        .size(caption.size)
                        .line_height(caption.line_height)
                        .color(p.text.secondary)
                        .no_truncate(),
                )
                .child(div().flex_1())
                .child(
                    Button::new("confirm-commit", "Confirm")
                        .color(ButtonColor::Warning)
                        .contained()
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, _cx: &mut Context<Self>| {
                                this.run_netconf(|draft| {
                                    draft.operation = NetconfOperation::Commit
                                });
                            },
                        )),
                )
                .child(
                    Button::new("roll-back", "Roll back now")
                        .color(ButtonColor::Inherit)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, _cx: &mut Context<Self>| {
                                this.run_netconf(|draft| {
                                    draft.operation = NetconfOperation::CancelCommit
                                });
                            },
                        )),
                );
        } else if dirty {
            bar =
                bar.child(div().size(px(8.)).rounded_full().bg(p.info.main))
                    .child(small("Uncommitted changes in the candidate", 600.))
                    .child(div().flex_1())
                    .child(Button::new("review-candidate", "Review").on_click(
                        cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                            this.open_candidate_review(cx);
                        }),
                    ));
            if self.controller.has_capability(":validate") {
                bar = bar.child(Button::new("validate-candidate", "Validate").on_click(
                    cx.listener_for_app(|this: &mut Self, _cx: &mut Context<Self>| {
                        this.run_netconf(|draft| {
                            draft.operation = NetconfOperation::Validate;
                            draft.source = Datastore::Candidate;
                        });
                    }),
                ));
            }
            bar = bar.child(
                Button::new("discard-candidate", "Discard").color(ButtonColor::Inherit).on_click(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.ui.dialog = Some(Dialog::Confirm {
                            confirmation: super::logic::editor::Confirmation {
                                title: "Discard the candidate?".into(),
                                description: "Every uncommitted change in the candidate is lost, including other sessions’.".into(),
                                confirm_label: "Discard".into(),
                                destructive: true,
                            },
                            action: ConfirmAction::Discard,
                        });
                        cx.notify();
                    },
                )),
            );
            if self.controller.has_capability(":confirmed-commit") {
                bar = bar.child(
                    Button::new("commit-confirmed", "Commit confirmed…")
                        .outlined()
                        .tooltip("Commit with an automatic rollback unless confirmed")
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| {
                                this.open_commit_confirmed(cx);
                            },
                        )),
                );
            }
            bar = bar.child(Button::new("commit", "Commit").contained().on_click(
                cx.listener_for_app(|this: &mut Self, _cx: &mut Context<Self>| {
                    this.run_netconf(|draft| draft.operation = NetconfOperation::Commit);
                }),
            ));
        } else if checking {
            bar = bar
                .child(CircularProgress::new(
                    ("bars-progress", 1usize),
                    12.,
                    p.primary.main,
                ))
                .child(
                    label("Checking the candidate …")
                        .size(caption.size)
                        .line_height(caption.line_height)
                        .color(p.text.secondary)
                        .no_truncate(),
                )
                .child(div().flex_1());
        } else {
            bar = bar.child(div().flex_1());
        }
        for lock in locks {
            let target = match lock.as_str() {
                "candidate" => Datastore::Candidate,
                "startup" => Datastore::Startup,
                _ => Datastore::Running,
            };
            bar = bar.child(
                Button::new(
                    SharedString::from(format!("unlock-{lock}")),
                    format!("Unlock {lock}"),
                )
                .color(ButtonColor::Inherit)
                .font_size(12.)
                .start_icon("LockOutlined")
                .start_icon_style(15., 11.)
                .tooltip(format!("This session holds the {lock} lock"))
                .on_click(cx.listener_for_app(
                    move |this: &mut Self, _cx: &mut Context<Self>| {
                        this.run_netconf(|draft| {
                            draft.operation = NetconfOperation::Unlock;
                            draft.target = target;
                        });
                    },
                )),
            );
        }
        Some(bar.into_any_element())
    }

    fn open_candidate_review(&mut self, cx: &mut Context<Self>) {
        self.ui.dialog = Some(Dialog::Review {
            title: "Uncommitted changes".into(),
            description: Some("What a commit would change in the running configuration.".into()),
            sections: Vec::new(),
            loading: true,
            error: None,
            apply: Some((ReviewApply::Commit, "Commit".into())),
            tab: 0,
        });
        cx.notify();
        let controller = self.controller.clone();
        let texts = self.on_runtime(async move { controller.check_candidate().await });
        cx.spawn(async move |this, cx| {
            let texts = texts.await.flatten();
            let _ = this.update(cx, |this, cx| {
                if let Some(Dialog::Review {
                    sections,
                    loading,
                    error,
                    ..
                }) = &mut this.ui.dialog
                {
                    *loading = false;
                    match texts {
                        Some((running, candidate)) => {
                            *sections = vec![super::ui::ReviewSection::new(
                                "running → candidate".into(),
                                running,
                                candidate,
                                "xml",
                                None,
                            )];
                        }
                        None => {
                            *error = Some(
                                "Could not read the running and candidate configurations.".into(),
                            );
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_commit_confirmed(&mut self, cx: &mut Context<Self>) {
        self.ui.dialog = Some(Dialog::Prompt {
            title: "Confirmed commit".into(),
            description: Some(
                "The change goes live now and rolls back on its own unless you confirm it in time — a safety net for changes that could cut you off.".into(),
            ),
            label: "Roll back after (seconds)".into(),
            confirm_label: "Commit".into(),
            action: PromptAction::CommitConfirmed,
            error: None,
        });
        self.ui.prompt_initial = Some("120".into());
        cx.notify();
    }

    pub(super) fn render_gnmi_commit_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = *cx.theme();
        let p = &theme.palette;
        let (deadline, commit_id) = self
            .store
            .read(|state| (state.confirm_deadline, state.commit_id.clone()));
        let (deadline, _commit) = (deadline?, commit_id?);
        if !self.connected() {
            return None;
        }
        let now = now_ms();
        let expired = deadline as i64 <= now;
        let body2 = theme.typography.body2;
        let caption = theme.typography.caption;
        let mut bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(4.))
            .min_h(px(38.))
            .border_b_1()
            .border_color(p.divider)
            .bg(alpha(p.warning.main, 0.14))
            .child(icon("TimerOutlined", 18., p.warning.main))
            .child(
                label(if expired {
                    "The rollback timer ran out".to_owned()
                } else {
                    format!("Rolls back in {}", countdown(deadline as i64 - now))
                })
                .size(body2.size)
                .weight(600.)
                .line_height(body2.line_height)
                .no_truncate(),
            )
            .child(
                label(if expired {
                    "the device has undone the change"
                } else {
                    "unless you confirm the change"
                })
                .size(caption.size)
                .line_height(caption.line_height)
                .color(p.text.secondary)
                .no_truncate(),
            )
            .child(div().flex_1());
        if expired {
            bar = bar.child(
                Button::new("dismiss-commit", "Dismiss")
                    .color(ButtonColor::Inherit)
                    .on_click(
                        cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                            this.store.update(|state| {
                                state.confirm_deadline = None;
                                state.commit_id = None;
                            });
                            cx.notify();
                        }),
                    ),
            );
        } else {
            bar = bar
                .child(
                    Button::new("confirm-set", "Confirm")
                        .color(ButtonColor::Warning)
                        .contained()
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, _cx: &mut Context<Self>| {
                                this.controller.finish_gnmi_commit(CommitAction::Confirm);
                            },
                        )),
                )
                .child(
                    Button::new("roll-back-set", "Roll back now")
                        .color(ButtonColor::Inherit)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, _cx: &mut Context<Self>| {
                                this.controller.finish_gnmi_commit(CommitAction::Cancel);
                            },
                        )),
                );
        }
        Some(bar.into_any_element())
    }
}
