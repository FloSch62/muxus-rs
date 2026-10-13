//! MUI outlined `TextField`, size small (`MuiTextField` default): a shorthand
//! for the kit's one outlined field, [`FormField`], with a text input.
//!
//! Geometry: body1 type (14.86px, line height 1.4375em), 8.5px × 14px input
//! padding, 8px corners; the outline is the divider color, the primary color
//! at half strength on hover and a 2px primary ring while focused. The outline
//! is drawn as an overlay, like MUI's fieldset, so focusing never moves text.

use gpui_kit::base::input::{InputEditorStyle, InputState};
use gpui_kit::{
    AnyElement, App, ElementId, Entity, Hsla, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled, Window, div,
};

use crate::color::alpha;
use crate::icon::{SMALL, icon};
use crate::theme::{ActiveTheme, Theme};
use crate::widgets::form_field::FormField;

/// Project the theme's colors onto an input state (placeholder, caret, selection).
pub fn style_input(state: &Entity<InputState>, theme: &Theme, cx: &mut App) {
    let text = theme.palette.text.primary;
    // MUI placeholder: currentColor at 0.42 (light) / 0.5 (dark).
    let placeholder = alpha(text, if theme.mode.is_dark() { 0.5 } else { 0.42 });
    let style = InputEditorStyle {
        foreground: text,
        muted_foreground: placeholder,
        background: Hsla::transparent_black(),
        border: Hsla::transparent_black(),
        selection: theme.components.text_selection,
        caret: text,
        ..Default::default()
    };
    state.update(cx, |state, _| state.set_editor_style(style));
}

#[derive(IntoElement)]
pub struct TextField {
    id: ElementId,
    state: Entity<InputState>,
    start_icon: Option<SharedString>,
    end_text: Option<SharedString>,
    strong_placeholder: bool,
    label: Option<SharedString>,
    full_width: bool,
    floating: bool,
    placeholder: Option<SharedString>,
    start: Option<AnyElement>,
    end: Option<AnyElement>,
    width: Option<f32>,
}

impl TextField {
    pub fn new(id: impl Into<ElementId>, state: &Entity<InputState>) -> Self {
        Self {
            id: id.into(),
            state: state.clone(),
            start_icon: None,
            end_text: None,
            strong_placeholder: false,
            label: None,
            full_width: false,
            floating: false,
            placeholder: None,
            start: None,
            end: None,
            width: None,
        }
    }

    /// MUI's floating label: inside the field while it is empty and not
    /// focused, shrunk into the outline's notch otherwise (needs [`Self::label`]).
    pub fn floating_label(mut self) -> Self {
        self.floating = true;
        self
    }

    /// The input's placeholder, hidden while a floating label sits inside the
    /// field (as MUI hides it).
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }

    /// `InputAdornment position="start"` holding any element (an icon of
    /// another size or color).
    pub fn start(mut self, element: impl IntoElement) -> Self {
        self.start = Some(element.into_any_element());
        self
    }

    /// `InputAdornment position="end"` holding any element (an `edge="end"`
    /// icon button).
    pub fn end(mut self, element: impl IntoElement) -> Self {
        self.end = Some(element.into_any_element());
        self
    }

    /// `sx={{ width }}`.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// `InputAdornment position="start"` holding a small icon.
    pub fn start_icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.start_icon = Some(icon.into());
        self
    }

    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// `InputAdornment position="end"` holding text ("ms").
    pub fn end_text(mut self, text: impl Into<SharedString>) -> Self {
        self.end_text = Some(text.into());
        self
    }

    /// The placeholder in the text color rather than the dimmed one: a date
    /// input's `mm/dd/yyyy` mask, which Chrome draws like a value.
    pub fn strong_placeholder(mut self) -> Self {
        self.strong_placeholder = true;
        self
    }

    /// The outlined field's label, drawn shrunk in the outline's notch (MUI
    /// shrinks it whenever the field has focus or a value; Muxus dialogs open
    /// with the field focused, so it is shown shrunk unless it floats).
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl RenderOnce for TextField {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *cx.theme();
        let mut field = FormField::input(self.id, &self.state);
        if let Some(label) = self.label {
            field = field.label(label).shrink(!self.floating);
        }
        field = match self.placeholder {
            Some(placeholder) => field.placeholder(placeholder),
            None => field.keep_placeholder(),
        };
        if self.strong_placeholder {
            field = field.strong_placeholder();
        }
        if let Some(element) = self.start {
            field = field.start(element);
        } else if let Some(name) = self.start_icon {
            field = field.start(icon(&name, SMALL, theme.palette.action.active));
        }
        if let Some(element) = self.end {
            field = field.end(element);
        } else if let Some(text) = self.end_text {
            field = field.end(div().text_color(theme.palette.text.secondary).child(text));
        }
        if let Some(width) = self.width {
            field = field.width(width);
        } else if !self.full_width {
            field = field.intrinsic();
        }
        field
    }
}
