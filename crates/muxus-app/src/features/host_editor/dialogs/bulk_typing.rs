//! The bulk editor's prompt waiting, anti-idle and keyboard and echo fields (the parts of
//! `HostBulkEditDialog.tsx` that edit `pastePromptWait`, `antiIdle`, `backspaceKey`,
//! `enterKey`, `implicitCr` and `localEcho`).

use muxus_core::anti_idle::anti_idle_draft;
use muxus_core::paste_prompt::paste_prompt_wait_draft;
use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div,
};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::{Alert, Button, ButtonColor};

use super::BulkEditDialog;
use crate::features::host_editor::logic::bulk_edit::{
    BulkFieldSummary, BulkHostChanges, BulkHostField, BulkSummary, BulkValue,
};
use crate::features::host_editor::view::session_input::{
    DraftAccess, anti_idle_fields, anti_idle_intro, backspace_choices, enter_choices,
    input_modes_intro, paste_prompt_fields,
};

/// The value a field shows: the change made here, else what every host shares.
fn current<T: Clone>(change: &Option<T>, summary: &BulkFieldSummary<T>) -> Option<T> {
    change.clone().or_else(|| match summary {
        BulkFieldSummary::Same { value, .. } => Some(value.clone()),
        _ => None,
    })
}

impl BulkEditDialog {
    /// A draft field: its editor, or, while the hosts differ, a note with "Set one for all".
    #[allow(clippy::too_many_arguments)]
    fn draft_field<T: BulkValue + 'static>(
        &mut self,
        field: BulkHostField,
        get: fn(&BulkSummary) -> &BulkFieldSummary<T>,
        slot: fn(&mut BulkHostChanges) -> &mut Option<T>,
        fresh: fn() -> T,
        mixed_note: &'static str,
        editor: impl FnOnce(
            &mut Self,
            &T,
            DraftAccess<Self, T>,
            &mut Window,
            &mut Context<Self>,
        ) -> AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let summary = self.summary();
        let plan = self.plan();
        let change = slot(&mut self.changes).clone();
        let state = self.field(field, get(&summary), &change, &plan);
        let body = match &state.value {
            Some(value) => editor(
                self,
                value,
                DraftAccess::new(
                    move |this: &Self| {
                        current(
                            &slot(&mut this.changes.clone()).clone(),
                            get(&this.summary()),
                        )
                    },
                    move |this, value, cx| this.set(field, value, get, slot, cx),
                ),
                window,
                cx,
            ),
            None => {
                let entity = cx.entity().downgrade();
                Alert::new(Status::Info, mixed_note)
                    .outlined()
                    .action(
                        Button::new(
                            SharedString::from(format!("set-all-{field:?}")),
                            "Set one for all",
                        )
                        .color(ButtonColor::Inherit)
                        .on_click(move |_, _, cx| {
                            let _ = entity
                                .update(cx, |this, cx| this.set(field, fresh(), get, slot, cx));
                        }),
                    )
                    .into_any_element()
            }
        };
        div()
            .child(body)
            .children(self.status(&theme, field, &state, false, None, false, cx))
            .into_any_element()
    }

    /// Prompt waiting, anti-idle and the keys, for the Terminal appearance section.
    pub(super) fn typing_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let prompt = self.draft_field(
            BulkHostField::PastePromptWait,
            |s| &s.paste_prompt_wait,
            |c| &mut c.paste_prompt_wait,
            || paste_prompt_wait_draft(None),
            "These hosts wait for the prompt differently when pasting. Each keeps its own setting until you set one for all of them.",
            |this, value, access, window, cx| {
                paste_prompt_fields(this, "bulk", value, access, window, cx)
            },
            window,
            cx,
        );
        let anti_idle = self.draft_field(
            BulkHostField::AntiIdle,
            |s| &s.anti_idle,
            |c| &mut c.anti_idle,
            || anti_idle_draft(None),
            "These hosts handle idle sessions differently. Each keeps its own setting until you set one for all of them.",
            |this, value, access, window, cx| {
                anti_idle_fields(this, "bulk", value, access, window, cx)
            },
            window,
            cx,
        );
        let summary = self.summary();
        let plan = self.plan();
        let backspace = self.field(
            BulkHostField::BackspaceKey,
            &summary.backspace_key,
            &self.changes.backspace_key,
            &plan,
        );
        let enter = self.field(
            BulkHostField::EnterKey,
            &summary.enter_key,
            &self.changes.enter_key,
            &plan,
        );
        let implicit_cr = self.field(
            BulkHostField::ImplicitCr,
            &summary.implicit_cr,
            &self.changes.implicit_cr,
            &plan,
        );
        let local_echo = self.field(
            BulkHostField::LocalEcho,
            &summary.local_echo,
            &self.changes.local_echo,
            &plan,
        );
        let backspace_select = self.bulk_select(
            "bulk-backspace-key",
            "Backspace sends",
            backspace.value,
            backspace.mixed,
            backspace_choices(),
            window,
            cx,
            |this, key, cx| {
                this.set(
                    BulkHostField::BackspaceKey,
                    key,
                    |s| &s.backspace_key,
                    |c| &mut c.backspace_key,
                    cx,
                )
            },
        );
        let enter_select = self.bulk_select(
            "bulk-enter-key",
            "Enter sends",
            enter.value,
            enter.mixed,
            enter_choices(),
            window,
            cx,
            |this, key, cx| {
                this.set(
                    BulkHostField::EnterKey,
                    key,
                    |s| &s.enter_key,
                    |c| &mut c.enter_key,
                    cx,
                )
            },
        );
        let implicit_cr_box = self.bulk_checkbox(
            &theme,
            "bulk-implicit-cr",
            &implicit_cr,
            "Implicit CR in every received LF",
            "For devices that end output lines with LF alone.",
            |this, on, cx| {
                this.set(
                    BulkHostField::ImplicitCr,
                    on,
                    |s| &s.implicit_cr,
                    |c| &mut c.implicit_cr,
                    cx,
                )
            },
            cx,
        );
        let local_echo_box = self.bulk_checkbox(
            &theme,
            "bulk-local-echo",
            &local_echo,
            "Local echo",
            "Shows what is typed, pasted or sent with a button, for devices that do not echo it.",
            |this, on, cx| {
                this.set(
                    BulkHostField::LocalEcho,
                    on,
                    |s| &s.local_echo,
                    |c| &mut c.local_echo,
                    cx,
                )
            },
            cx,
        );
        div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(2.5))
            .child(prompt)
            .child(
                div()
                    .child(div().mb(Theme::spacing(1.)).child(anti_idle_intro(&theme)))
                    .child(anti_idle),
            )
            .child(input_modes_intro(&theme))
            .child(
                div()
                    .flex()
                    .gap(Theme::spacing(1.5))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(backspace_select)
                            .children(self.status(
                                &theme,
                                BulkHostField::BackspaceKey,
                                &backspace,
                                true,
                                None,
                                false,
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(enter_select)
                            .children(self.status(
                                &theme,
                                BulkHostField::EnterKey,
                                &enter,
                                true,
                                None,
                                false,
                                cx,
                            )),
                    ),
            )
            .child(div().child(implicit_cr_box).children(self.status(
                &theme,
                BulkHostField::ImplicitCr,
                &implicit_cr,
                false,
                None,
                false,
                cx,
            )))
            .child(div().child(local_echo_box).children(self.status(
                &theme,
                BulkHostField::LocalEcho,
                &local_echo,
                false,
                None,
                false,
                cx,
            )))
            .into_any_element()
    }
}
