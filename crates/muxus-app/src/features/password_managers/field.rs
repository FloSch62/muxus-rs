//! `PasswordEntryField.tsx`: the password-manager entry a host or folder logs in with. It
//! shows the linked entry, opens the picker to choose one, and unlinks it. It holds a
//! reference only, never a password.

use std::rc::Rc;

use muxus_core::api::ExternalPasswordRef;
use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::typography::{body2, caption, para, text};
use muxus_ui::widgets::{Button, IconButton};

use super::logic::{linked_entry_caption, linked_entry_title};
use super::store::{self, PickRequest, request_password_entry};

/// The field for `value`. `key` tells its buttons from another field's in the same view,
/// `suggestion` is what the entry is for (offered as a search in the picker), and
/// `on_change` gets the entry picked, or `None` when it is unlinked.
pub fn password_entry_field<V: 'static>(
    key: &str,
    value: Option<&ExternalPasswordRef>,
    suggestion: &str,
    helper: Option<String>,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, Option<ExternalPasswordRef>, &mut Context<V>) + 'static,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let on_change = Rc::new(on_change);
    let suggestion = (!suggestion.is_empty()).then(|| suggestion.to_owned());

    // "Choose…" and "Change": the picker, over the window this field is in.
    let choose = {
        let view = cx.entity().downgrade();
        let on_change = on_change.clone();
        let value = value.cloned();
        move |cx: &mut muxus_ui::gpui_kit::App| {
            let view = view.clone();
            let on_change = on_change.clone();
            request_password_entry(
                PickRequest {
                    value: value.clone(),
                    suggestion: suggestion.clone(),
                    on_pick: Box::new(move |picked, cx| {
                        let _ = view.update(cx, |this, cx| {
                            on_change(this, Some(picked), cx);
                            cx.notify();
                        });
                    }),
                },
                cx,
            );
        }
    };

    let mut field = div().flex().flex_col();
    field = match value {
        Some(value) => {
            let (said, orphaned) = linked_entry_caption(value, store::listed_sources(cx));
            let mut title = div().flex().items_baseline().min_w_0().child(text(
                linked_entry_title(value).to_owned(),
                body2(),
                p.text.primary,
            ));
            if let Some(username) = value.username.as_ref().filter(|name| !name.is_empty()) {
                title = title.child(div().ml(Theme::spacing(1.)).min_w_0().child(text(
                    username.clone(),
                    body2(),
                    p.text.secondary,
                )));
            }
            let unlink = cx.entity().downgrade();
            let on_unlink = on_change.clone();
            field.child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::spacing(1.25))
                    .pl(Theme::spacing(1.5))
                    .pr(Theme::spacing(0.75))
                    .py(Theme::spacing(0.75))
                    .border_1()
                    .border_color(if orphaned { p.warning.main } else { p.divider })
                    .rounded(px(theme.radius * 1.5))
                    .child(
                        div()
                            .flex_none()
                            .child(icon("KeyOutlined", 18., p.text.secondary)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(title)
                            .child(text(
                                said,
                                caption(),
                                if orphaned {
                                    p.warning.main
                                } else {
                                    p.text.secondary
                                },
                            )),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{key}-change")), "Change")
                            .on_click(move |_, _, cx| choose(cx)),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("{key}-unlink")),
                            "LinkOffOutlined",
                        )
                        .icon_size(18.)
                        .tooltip("Stop using this entry")
                        .on_click(move |_, _, cx| {
                            let _ = unlink.update(cx, |this, cx| {
                                on_unlink(this, None, cx);
                                cx.notify();
                            });
                        }),
                    ),
            )
        }
        None => field.child(
            div().flex().child(
                Button::new(
                    SharedString::from(format!("{key}-choose")),
                    "Choose from a password manager…",
                )
                .outlined()
                .start_icon("KeyOutlined")
                .on_click(move |_, _, cx| choose(cx)),
            ),
        ),
    };
    if let Some(helper) = helper {
        field = field.child(div().mt(Theme::spacing(0.75)).child(para(
            helper,
            caption(),
            p.text.secondary,
        )));
    }
    field.into_any_element()
}
