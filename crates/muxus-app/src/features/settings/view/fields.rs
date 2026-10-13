//! The settings forms' text fields: a builder over the kit's one outlined
//! field ([`FormField`]) with what the settings forms need: a label in the
//! outline's notch over the section's surface, helper or error text,
//! adornments, monospace input, multi-line input and a disabled state.
//!
//! Geometry (size small, measured in the Electron build): 38.36px tall for
//! one line of body1, 8.5px × 14px input padding, 8px corners; the label is
//! body1 scaled to 0.75 (11.14px) sitting 9px above the field with a 5px gap
//! cut into the outline on either side; helper text is the caption tier 3px
//! below, 14px in.

use muxus_ui::gpui_kit::base::input::{InputState, Textarea, TextareaState};
use muxus_ui::gpui_kit::{
    AnyElement, App, Div, ElementId, Entity, Focusable, Hsla, IntoElement, SharedString, Styled,
    Window, div, px, relative,
};
use muxus_ui::label::label;
use muxus_ui::theme::Theme;
use muxus_ui::widgets::form_field::{FieldBody, FormField};

use super::kit::{BODY1, snap};

/// The editor behind a field.
#[derive(Clone)]
pub enum FieldInput {
    Line(Entity<InputState>),
    Multi(Entity<TextareaState>),
}

impl From<&Entity<InputState>> for FieldInput {
    fn from(state: &Entity<InputState>) -> Self {
        FieldInput::Line(state.clone())
    }
}

impl From<&Entity<TextareaState>> for FieldInput {
    fn from(state: &Entity<TextareaState>) -> Self {
        FieldInput::Multi(state.clone())
    }
}

/// An outlined text field (see the module docs).
pub struct Field {
    id: ElementId,
    input: FieldInput,
    label: Option<SharedString>,
    helper: Option<SharedString>,
    error: bool,
    end_text: Option<SharedString>,
    end: Option<AnyElement>,
    start_icon: Option<(SharedString, f32)>,
    width: Option<f32>,
    mono: Option<(SharedString, f32)>,
    disabled: bool,
    notch: Option<Hsla>,
    min_rows: usize,
}

pub fn field(id: impl Into<ElementId>, input: impl Into<FieldInput>) -> Field {
    Field {
        id: id.into(),
        input: input.into(),
        label: None,
        helper: None,
        error: false,
        end_text: None,
        end: None,
        start_icon: None,
        width: None,
        mono: None,
        disabled: false,
        notch: None,
        min_rows: 1,
    }
}

impl Field {
    /// A floating label, always shrunk into the outline's notch. `notch` is
    /// the surface the field sits on, which the notch shows through.
    pub fn label(mut self, label: impl Into<SharedString>, notch: Hsla) -> Self {
        self.label = Some(label.into());
        self.notch = Some(notch);
        self
    }

    pub fn helper(mut self, helper: impl Into<SharedString>) -> Self {
        self.helper = Some(helper.into());
        self
    }

    pub fn helper_opt(mut self, helper: Option<impl Into<SharedString>>) -> Self {
        self.helper = helper.map(Into::into);
        self
    }

    pub fn error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }

    /// Text after the input (`InputAdornment position="end"`: "ms", "MiB").
    pub fn end_text(mut self, text: impl Into<SharedString>) -> Self {
        self.end_text = Some(text.into());
        self
    }

    /// An element after the input (`InputAdornment position="end"` holding a
    /// button, e.g. show/hide password).
    /// An icon before the input (`InputAdornment position="start"`): 14px
    /// in, 8px before the text, in the action color.
    pub fn start_icon(mut self, name: impl Into<SharedString>, size: f32) -> Self {
        self.start_icon = Some((name.into(), size));
        self
    }

    pub fn end(mut self, element: impl IntoElement) -> Self {
        self.end = Some(element.into_any_element());
        self
    }

    /// A fixed width; full width otherwise.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// The typed text in another face (paths and commands use `monospace`).
    pub fn font(mut self, family: impl Into<SharedString>, size: f32) -> Self {
        self.mono = Some((family.into(), size));
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Rows a multi-line field shows at least (`minRows`).
    pub fn min_rows(mut self, rows: usize) -> Self {
        self.min_rows = rows.max(1);
        self
    }

    pub fn render(self, theme: &Theme, _window: &mut Window, cx: &mut App) -> AnyElement {
        let p = &theme.palette;
        let mut field = match &self.input {
            FieldInput::Line(state) => FormField::input(self.id, state),
            FieldInput::Multi(state) => {
                style_textarea(state, theme, cx);
                let focus = state.read(cx).focus_handle(cx);
                FormField::new(
                    self.id,
                    FieldBody::Element(Textarea::new(state).into_any_element()),
                )
                .multiline()
                .min_rows(self.min_rows)
                .element_focus(focus)
            }
        };
        field = field
            .keep_placeholder()
            .error(self.error)
            .disabled(self.disabled);
        if let (Some(text), Some(notch)) = (self.label, self.notch) {
            field = field.label(text).shrink(true).notch(notch);
        }
        if let Some(helper) = self.helper {
            field = field.helper(helper);
        }
        if let Some((name, size)) = &self.start_icon {
            field = field.start(muxus_ui::icon::icon(name, *size, p.action.active));
        }
        if let Some(text) = self.end_text {
            field = field.end(
                label(text)
                    .size(BODY1)
                    .line_height(snap(BODY1 * 1.5))
                    .color(p.text.secondary)
                    .no_truncate(),
            );
        } else if let Some(end) = self.end {
            field = field.end(end).end_edge();
        }
        if let Some((family, size)) = self.mono {
            field = field.font(family, size);
        }
        if let Some(width) = self.width {
            field = field.width(width);
        }
        field.into_any_element()
    }
}

/// Project the theme's colors onto a multi-line editor.
pub fn style_textarea(state: &Entity<TextareaState>, theme: &Theme, cx: &mut App) {
    use muxus_ui::color::alpha;
    use muxus_ui::gpui_kit::base::input::InputEditorStyle;
    let text = theme.palette.text.primary;
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

/// A small search/plain field without a label (rule rows, the shortcuts search).
pub fn bare(id: impl Into<ElementId>, input: impl Into<FieldInput>) -> Field {
    field(id, input)
}

/// Body1 text in a field's place while its value loads.
pub fn placeholder_box(theme: &Theme, height: f32) -> Div {
    div()
        .h(px(height))
        .w(relative(1.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme.palette.divider)
}
