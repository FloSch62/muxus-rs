//! MUI `Link component="button"`: underlined text that acts like a button.
//! `color="inherit"` follows the surrounding text color; the variant sets the
//! type (caption by default, as the update notice's mute link uses it).

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    App, ClickEvent, ElementId, FontWeight, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div,
};

use crate::theme::{ActiveTheme, TextVariant};

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Link {
    id: ElementId,
    label: SharedString,
    variant: Option<TextVariant>,
    opacity: f32,
    on_click: Option<Handler>,
}

impl Link {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant: None,
            opacity: 1.,
            on_click: None,
        }
    }

    /// The typography variant (caption when unset).
    pub fn variant(mut self, variant: TextVariant) -> Self {
        self.variant = Some(variant);
        self
    }

    /// `sx={{ opacity }}`.
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Link {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let variant = self.variant.unwrap_or(cx.theme().typography.caption);
        let ink = window.text_style().color;
        let mut link = div()
            .id(self.id)
            .text_size(px(variant.size))
            .font_weight(FontWeight(variant.weight))
            .line_height(px(variant.line_height))
            .text_color(ink)
            .opacity(self.opacity)
            .underline()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .child(self.label);
        if let Some(handler) = self.on_click {
            link = link.on_click(move |event, window, cx| handler(event, window, cx));
        }
        link
    }
}
