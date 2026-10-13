//! MUI `Select` (outlined, small) as a closed field: the current value and the
//! drop-down arrow, a shorthand for the kit's one outlined field
//! ([`FormField`] with a value body). Opening it is the owner's job: mount a
//! [`super::menu`] popup below the field with the options as menu items.

use std::rc::Rc;

use gpui_kit::{App, ClickEvent, ElementId, IntoElement, RenderOnce, SharedString, Window};

use crate::widgets::form_field::{FieldBody, FormField};

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Select {
    id: ElementId,
    value: SharedString,
    open: bool,
    full_width: bool,
    on_click: Option<Handler>,
    label: Option<SharedString>,
    width: Option<f32>,
    anchor: Option<crate::widgets::tooltip::AnchorBounds>,
}

impl Select {
    pub fn new(id: impl Into<ElementId>, value: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            open: false,
            full_width: false,
            on_click: None,
            label: None,
            width: None,
            anchor: None,
        }
    }

    /// `TextField select label=…`: the label in the outline's notch, or
    /// inside the field while the value is empty and the menu closed.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// `sx={{ width }}`.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Record the field's painted bounds, so the owner can open its menu below it.
    pub fn track_bounds(mut self, anchor: &crate::widgets::tooltip::AnchorBounds) -> Self {
        self.anchor = Some(anchor.clone());
        self
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn full_width(mut self) -> Self {
        self.full_width = true;
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

impl RenderOnce for Select {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut field = FormField::new(self.id, FieldBody::Value(self.value)).select(self.open);
        if let Some(label) = self.label {
            field = field.label(label);
        }
        if let Some(width) = self.width {
            field = field.width(width).no_shrink();
        } else if !self.full_width {
            field = field.intrinsic();
        }
        if let Some(anchor) = &self.anchor {
            field = field.anchor(anchor);
        }
        if let Some(handler) = self.on_click {
            field = field.on_click(move |event, window, cx| handler(event, window, cx));
        }
        field
    }
}
