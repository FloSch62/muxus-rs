//! MUI `Dialog` (12px corners and a divider border per the theme),
//! `DialogTitle`, `DialogContent`, `DialogActions`, and the 50% black backdrop.
//!
//! The owning view mounts [`modal`] while the dialog is open; the backdrop
//! swallows pointer events and reports clicks outside the paper.

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    AnyElement, App, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, Styled, Window, deferred, div, relative,
};

use crate::color::rgba8;
use crate::theme::{ActiveTheme, Theme, fonts};
use crate::widgets::button::elevation;

/// `maxWidth` of a dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogWidth {
    Xs,
    Sm,
    Md,
    Lg,
}

impl DialogWidth {
    /// The `maxWidth` breakpoint in px.
    pub fn px(self) -> f32 {
        match self {
            DialogWidth::Xs => 444.,
            DialogWidth::Sm => 600.,
            DialogWidth::Md => 900.,
            DialogWidth::Lg => 1200.,
        }
    }
}

/// Called when the backdrop is clicked or Escape is pressed.
pub type CloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Dialog {
    title: Option<SharedString>,
    subtitle: Option<SharedString>,
    content: Vec<AnyElement>,
    actions: Vec<AnyElement>,
    width: DialogWidth,
    full_width: bool,
    on_close: Option<CloseHandler>,
}

impl Dialog {
    pub fn new() -> Self {
        Self {
            title: None,
            subtitle: None,
            content: Vec::new(),
            actions: Vec::new(),
            width: DialogWidth::Sm,
            full_width: false,
            on_close: None,
        }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// A caption under the title, inside the title block (`DialogTitle`
    /// holding a caption `Typography`).
    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn content(mut self, element: impl IntoElement) -> Self {
        self.content.push(element.into_any_element());
        self
    }

    pub fn action(mut self, element: impl IntoElement) -> Self {
        self.actions.push(element.into_any_element());
        self
    }

    pub fn max_width(mut self, width: DialogWidth) -> Self {
        self.width = width;
        self
    }

    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// Backdrop click and Escape.
    pub fn on_close(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }
}

impl Default for Dialog {
    fn default() -> Self {
        Self::new()
    }
}

/// The paper itself, for callers that lay out their own dialog body.
pub fn dialog_paper(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .max_h(relative(1.))
        .bg(theme.palette.background_paper)
        .border_1()
        .border_color(theme.palette.divider)
        .rounded(px(12.))
        .shadow(elevation(24, theme))
        .font_family(fonts::UI)
        .text_color(theme.palette.text.primary)
}

impl RenderOnce for Dialog {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let h6 = theme.typography.h6;
        let mut paper = dialog_paper(theme).m(px(32.)).max_w(px(self.width.px()));
        if self.full_width {
            // `fullWidth`: calc(100% - 64px) of the window, up to `maxWidth`.
            // The paper sits in a shrink-wrapped box, so the window gives the 100%.
            let available = crate::zoom::unzoom(window.viewport_size().width) - 64.;
            paper = paper.w(px(self.width.px().min(available.max(0.))));
        }
        let has_title = self.title.is_some();
        if let Some(title) = self.title {
            let caption = theme.typography.caption;
            let mut block = div()
                .px(px(24.))
                .py(px(16.))
                .text_size(px(h6.size))
                .font_weight(FontWeight(h6.weight))
                .line_height(px(h6.line_height))
                .child(title);
            if let Some(subtitle) = self.subtitle {
                block = block.child(
                    div()
                        .text_size(px(caption.size))
                        .font_weight(FontWeight(caption.weight))
                        .line_height(px(caption.line_height))
                        .text_color(theme.palette.text.secondary)
                        .child(subtitle),
                );
            }
            paper = paper.child(block);
        }
        let body = theme.typography.body1;
        paper = paper.child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .px(px(24.))
                .pt(px(if has_title { 0. } else { 20. }))
                .pb(px(20.))
                .text_size(px(body.size))
                .line_height(px(body.line_height))
                .children(self.content),
        );
        if !self.actions.is_empty() {
            paper = paper.child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .p(px(8.))
                    .children(self.actions),
            );
        }
        modal(paper, self.on_close)
    }
}

/// A full-window backdrop with `content` centred on it.
pub fn modal(content: impl IntoElement, on_close: Option<CloseHandler>) -> impl IntoElement {
    let backdrop = div()
        .id("modal-backdrop")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba8(0, 0, 0, 0.5))
        // MUI's backdrop takes the pointer: nothing under it hovers or scrolls.
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            if let Some(close) = &on_close {
                close(window, cx);
            }
        })
        .child(crate::widgets::hover::hover_refresh("modal-hover"))
        .child(
            div()
                .id("modal-content")
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(content),
        );
    deferred(backdrop).with_priority(3)
}

/// Like [`modal`], with `content` near the top instead of centred: the
/// paper's top edge `top` px below the window's (`position: absolute;
/// top: 10vh` papers such as the quick launcher's).
pub fn modal_at_top(
    content: impl IntoElement,
    on_close: Option<CloseHandler>,
    top: f32,
) -> impl IntoElement {
    let backdrop = div()
        .id("modal-backdrop")
        .absolute()
        .inset_0()
        .flex()
        .items_start()
        .justify_center()
        .pt(px(top))
        .bg(rgba8(0, 0, 0, 0.5))
        // MUI's backdrop takes the pointer: nothing under it hovers or scrolls.
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            if let Some(close) = &on_close {
                close(window, cx);
            }
        })
        .child(crate::widgets::hover::hover_refresh("modal-hover"))
        .child(
            div()
                .id("modal-content")
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(content),
        );
    deferred(backdrop).with_priority(3)
}

/// `DialogTitle` for a dialog that lays out its own title row (an icon, a
/// count chip): h6 type, 16px × 24px padding, children in a row.
pub fn dialog_title(theme: &Theme) -> Div {
    let h6 = theme.typography.h6;
    div()
        .flex()
        .flex_none()
        .items_center()
        .px(px(24.))
        .py(px(16.))
        .text_size(px(h6.size))
        .font_weight(FontWeight(h6.weight))
        .line_height(px(h6.line_height))
        .text_color(theme.palette.text.primary)
}

/// `DialogActions`: 8px around the buttons and between them, right-aligned.
pub fn dialog_actions() -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_end()
        .gap(px(8.))
        .p(px(8.))
}

/// `DialogContent`: body1 type, 20px × 24px padding (no top padding right
/// under a title); with `dividers` 16px × 24px between two divider lines.
/// It grows to fill a paper of fixed height and clips what overflows.
pub fn dialog_content(theme: &Theme, under_title: bool, dividers: bool) -> Div {
    let body = theme.typography.body1;
    let content = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .px(px(24.))
        .text_size(px(body.size))
        .line_height(px(body.line_height));
    if dividers {
        content
            .py(px(16.))
            .border_t_1()
            .border_b_1()
            .border_color(theme.palette.divider)
    } else {
        content
            .pt(px(if under_title { 0. } else { 20. }))
            .pb(px(20.))
    }
}
