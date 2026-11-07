//! A paced paste's progress, with a way to cancel it: `TabPasteProgress` (a ring on the tab) and
//! `PasteProgressOverlay` (over the terminal) of `client/src/components/PasteProgress.tsx`.

use muxus_app::state::paste_progress::PasteProgress;
use muxus_ui::gpui_kit::{
    AnyElement, ClickEvent, Context, CursorStyle, ElementId, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::{Theme, fonts};
use muxus_ui::widgets::Button;
use muxus_ui::widgets::button::elevation;
use muxus_ui::widgets::progress::{CircularProgress, linear_progress_determinate};
use muxus_ui::widgets::tooltip::Tooltip;

use crate::ui::shell::AppShell;

/// `TabPasteProgress`: a ring on the tab while a paced paste runs; clicking it cancels the
/// paste (`<IconButton size="small" sx={{ p: 0.25 }}>` around a 13px warning ring, thickness 6).
pub fn tab_paste_progress(
    tab_id: &str,
    progress: PasteProgress,
    theme: &Theme,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let p = theme.palette;
    let hover = muxus_ui::color::alpha(p.action.active, p.action.hover_opacity);
    let cancel = tab_id.to_string();
    div()
        .id(ElementId::Name(format!("tab-paste-{tab_id}").into()))
        .flex()
        .flex_none()
        .p(px(2.))
        .rounded_full()
        .cursor(CursorStyle::PointingHand)
        .hover(move |s| s.bg(hover))
        .tooltip(Tooltip::text(format!(
            "{}. Click to cancel.",
            progress.label()
        )))
        .on_click(cx.listener(move |shell, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
            shell.cancel_paste(&cancel, cx);
        }))
        .child(
            CircularProgress::new(
                ElementId::Name(format!("tab-paste-ring-{tab_id}").into()),
                13.,
                p.warning.main,
            )
            .thickness(6.)
            .determinate(progress.percent()),
        )
        .into_any_element()
}

/// `PasteProgressOverlay`: the progress and a cancel button over the terminal while a paced
/// paste runs (a Paper at elevation 8 with a divider border, 13px from the bottom right).
pub fn paste_progress_overlay(
    tab_id: &str,
    progress: PasteProgress,
    theme: &Theme,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let p = theme.palette;
    let caption = theme.typography.caption;
    let cancel = tab_id.to_string();
    div()
        .id(ElementId::Name(format!("paste-progress-{tab_id}").into()))
        .absolute()
        .right(px(13.))
        .bottom(px(13.))
        .flex()
        .items_center()
        .gap(px(10.))
        .pl(px(12.))
        .pr(px(6.))
        .py(px(6.))
        .bg(p.background_paper)
        .border_1()
        .border_color(p.divider)
        .rounded(px(theme.radius))
        .shadow(elevation(8, theme))
        .font_family(fonts::UI)
        .text_color(p.text.primary)
        // Presses stay on the overlay instead of reaching the terminal below.
        .occlude()
        .child(icon("ContentPaste", 17., p.text.secondary))
        .child(
            div()
                .w(px(170.))
                .flex()
                .flex_col()
                .child(
                    div().mb(px(4.)).child(
                        label(progress.label())
                            .size(caption.size)
                            .weight(caption.weight)
                            .line_height(caption.line_height)
                            .color(p.text.primary),
                    ),
                )
                .child(linear_progress_determinate(
                    theme,
                    p.warning.main,
                    progress.percent(),
                    theme.radius,
                )),
        )
        .child(
            Button::new(
                ElementId::Name(format!("paste-progress-cancel-{tab_id}").into()),
                "Cancel",
            )
            .on_click(cx.listener(move |shell, _: &ClickEvent, _, cx| {
                shell.cancel_paste(&cancel, cx);
            })),
        )
        .into_any_element()
}
