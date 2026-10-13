//! MUI's outlined form control in full: `FormControl` + `InputLabel` +
//! `OutlinedInput` + `FormHelperText`. It is the one outlined field of the
//! kit: [`super::TextField`] and [`super::Select`] are shorthands that build
//! a [`FormField`]. It covers the floating label that sits inside an empty
//! field and shrinks into the outline's notch, the required mark, helper and
//! error text, the disabled state, the medium size, adornments, and a select
//! body that shows any content (a scheme preview, a dimmed "Multiple values").
//!
//! Geometry (CSS px): body1 input text (14.86px, line height 1.4375em) with
//! 8.5px × 14px padding, so the field is 38.36px tall (medium: +16px); the
//! shrunk label sits at translate(14px, -9px) scaled to 0.75; the outline
//! leaves a notch for it from x = 9 (fieldset border + padding) as wide as
//! the label plus 10px; helper text is a caption 3px below, inset 14px.

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::base::input::{Input, InputState};
use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, Entity, Focusable, FontWeight, Hsla,
    InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement, Styled, Transformation, Window, div, percentage, relative,
};

use crate::icon::icon;
use crate::theme::{ActiveTheme, Theme, fonts};
use crate::widgets::text_field::style_input;
use crate::widgets::tooltip::{AnchorBounds, bounds_probe};
use crate::widgets::typography::{VariantExt, body1, body2, caption, para, rem, text};

/// The outlined small input's height: 1.4375em of body1 plus 2 × 8.5px.
pub fn field_height() -> f32 {
    body1().size * 1.4375 + 17.
}

/// What the field shows inside its outline.
pub enum FieldBody {
    /// An editable text input.
    Input(Entity<InputState>),
    /// A select's rendered value (empty shows the label inside the field).
    Value(SharedString),
    /// A select's value in a dimmer ink (`displayEmpty` placeholders, "Multiple values").
    Muted(SharedString),
    /// Custom content (a scheme preview).
    Element(AnyElement),
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct FormField {
    id: ElementId,
    label: Option<SharedString>,
    required: bool,
    helper: Option<SharedString>,
    error: bool,
    disabled: bool,
    shrink: bool,
    body: FieldBody,
    placeholder: Option<SharedString>,
    select: bool,
    open: bool,
    start: Option<AnyElement>,
    end: Option<AnyElement>,
    width: Option<f32>,
    full_width: bool,
    font: Option<(SharedString, f32)>,
    anchor: Option<AnchorBounds>,
    on_click: Option<ClickHandler>,
    padding_right: Option<f32>,
    no_shrink: bool,
    medium: bool,
    compact: Option<(f32, f32)>,
    notch: Option<Hsla>,
    multiline: bool,
    /// Leave the input's own placeholder alone (it set one itself).
    keep_placeholder: bool,
    /// Neither full width, fixed width nor a flex share.
    intrinsic: bool,
    /// Text rows a multiline field shows at least (`minRows`).
    min_rows: usize,
    /// The end adornment is an `edge="end"` icon button: it reaches 2px from the outline.
    end_edge: bool,
    /// The focus of an [`FieldBody::Element`] editor (a text area): the outline follows it.
    element_focus: Option<gpui_kit::FocusHandle>,
    /// Paint the placeholder in the text color (a date mask Chrome draws like a value).
    strong_placeholder: bool,
}

impl FormField {
    /// A full-width field (`fullWidth`) showing `body`.
    pub fn new(id: impl Into<ElementId>, body: FieldBody) -> Self {
        Self {
            id: id.into(),
            label: None,
            required: false,
            helper: None,
            error: false,
            disabled: false,
            shrink: false,
            body,
            placeholder: None,
            select: false,
            open: false,
            start: None,
            end: None,
            width: None,
            full_width: true,
            font: None,
            anchor: None,
            on_click: None,
            padding_right: None,
            no_shrink: false,
            medium: false,
            compact: None,
            notch: None,
            multiline: false,
            keep_placeholder: false,
            intrinsic: false,
            min_rows: 1,
            end_edge: false,
            element_focus: None,
            strong_placeholder: false,
        }
    }

    /// A text input field.
    pub fn input(id: impl Into<ElementId>, state: &Entity<InputState>) -> Self {
        Self::new(id, FieldBody::Input(state.clone()))
    }

    /// `size="medium"` (16.5px vertical input padding).
    pub fn medium(mut self) -> Self {
        self.medium = true;
        self
    }

    /// A compact field (`'& .MuiInputBase-root': { height, fontSize }` with
    /// `'& .MuiInputLabel-root': { fontSize }`): `height` px tall, text and
    /// label at `font_size`.
    pub fn compact(mut self, height: f32, font_size: f32) -> Self {
        self.compact = Some((height, font_size));
        self
    }

    /// What shows through the outline's notch behind the shrunk label (MUI
    /// leaves a gap in the outline; this paints the surface the field sits on).
    /// `multiline`: at least the field's height, growing with an
    /// [`FieldBody::Element`] body (a text area) that sets its own height.
    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    pub fn notch(mut self, surface: Hsla) -> Self {
        self.notch = Some(surface);
        self
    }

    /// `flexShrink: 0`.
    pub fn no_shrink(mut self) -> Self {
        self.no_shrink = true;
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// `required`: a thin space and an asterisk after the label.
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// `helperText` (an empty string shows none).
    pub fn helper(mut self, helper: impl Into<SharedString>) -> Self {
        let helper = helper.into();
        self.helper = (!helper.is_empty()).then_some(helper);
        self
    }

    pub fn helper_opt(mut self, helper: Option<impl Into<SharedString>>) -> Self {
        self.helper = helper
            .map(Into::into)
            .filter(|h: &SharedString| !h.is_empty());
        self
    }

    /// `error`: outline, label and helper text in the error color.
    pub fn error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// `InputLabel shrink`: keep the label on the outline even when empty.
    pub fn shrink(mut self, shrink: bool) -> Self {
        self.shrink = shrink;
        self
    }

    /// The input's placeholder, hidden while the label sits inside the field.
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }

    /// A `select` field: drop-down arrow (turned while `open`), pointer
    /// cursor, no caret.
    pub fn select(mut self, open: bool) -> Self {
        self.select = true;
        self.open = open;
        self
    }

    /// `InputAdornment position="start"`.
    pub fn start(mut self, element: impl IntoElement) -> Self {
        self.start = Some(element.into_any_element());
        self
    }

    /// `InputAdornment position="end"`.
    pub fn end(mut self, element: impl IntoElement) -> Self {
        self.end = Some(element.into_any_element());
        self
    }

    /// The end adornment is an `edge="end"` icon button (its -12px margin brings it
    /// 2px from the outline).
    pub fn end_edge(mut self) -> Self {
        self.end_edge = true;
        self
    }

    /// The focus of a custom editor body (a text area): the outline turns primary while
    /// it has the keyboard, and a press on the field gives it the keyboard.
    pub fn element_focus(mut self, focus: gpui_kit::FocusHandle) -> Self {
        self.element_focus = Some(focus);
        self
    }

    /// Text rows a [`Self::multiline`] field shows at least (`minRows`).
    pub fn min_rows(mut self, rows: usize) -> Self {
        self.min_rows = rows.max(1);
        self
    }

    /// A fixed width (`sx={{ width }}`) instead of `fullWidth`.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self.full_width = false;
        self
    }

    /// Neither full width nor fixed: the field takes its flex share.
    pub fn flexible(mut self) -> Self {
        self.full_width = false;
        self
    }

    /// No width of its own: the field is as wide as its container gives it (a
    /// plain `TextField` without `fullWidth`).
    pub fn intrinsic(mut self) -> Self {
        self.full_width = false;
        self.intrinsic = true;
        self
    }

    /// Leave the input's own placeholder as it is (the field sets none).
    pub fn keep_placeholder(mut self) -> Self {
        self.keep_placeholder = true;
        self
    }

    /// The placeholder in the text color rather than the dimmed one: a date input's
    /// `mm/dd/yyyy` mask, which Chrome draws like a value.
    pub fn strong_placeholder(mut self) -> Self {
        self.strong_placeholder = true;
        self
    }

    /// Input text in the monospace font at `size`.
    pub fn mono(mut self, size: f32) -> Self {
        self.font = Some((fonts::MONO.into(), size));
        self
    }

    /// Input text in another face at `size` (`fontFamily: monospace` resolves to the
    /// system's monospace face).
    pub fn font(mut self, family: impl Into<SharedString>, size: f32) -> Self {
        self.font = Some((family.into(), size));
        self
    }

    /// Record the field's painted bounds (to open a menu below it).
    pub fn anchor(mut self, anchor: &AnchorBounds) -> Self {
        self.anchor = Some(anchor.clone());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// Extra right padding (an Autocomplete's end buttons).
    pub fn padding_right(mut self, padding: f32) -> Self {
        self.padding_right = Some(padding);
        self
    }
}

fn label_text(label: &str, required: bool) -> SharedString {
    if required {
        format!("{label}\u{2009}*").into()
    } else {
        label.to_owned().into()
    }
}

/// The ink of an outline, label and helper for the field's state.
struct Inks {
    outline: Hsla,
    label: Hsla,
    helper: Hsla,
    text: Hsla,
}

fn inks(theme: &Theme, focused: bool, error: bool, disabled: bool) -> Inks {
    let p = &theme.palette;
    if disabled {
        return Inks {
            outline: p.action.disabled,
            label: p.text.disabled,
            helper: p.text.disabled,
            text: p.text.disabled,
        };
    }
    Inks {
        outline: if error {
            p.error.main
        } else if focused {
            p.primary.main
        } else {
            p.divider
        },
        label: if error {
            p.error.main
        } else if focused {
            p.primary.main
        } else {
            p.text.secondary
        },
        helper: if error {
            p.error.main
        } else {
            p.text.secondary
        },
        text: p.text.primary,
    }
}

impl RenderOnce for FormField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let b1 = match self.compact {
            Some((_, size)) => body1().size(size),
            None => body1(),
        };
        let height = match self.compact {
            Some((height, _)) => height,
            None if self.medium => field_height() + 16.,
            None => field_height(),
        };
        let line = b1.size * 1.4375;

        let input_focus = match &self.body {
            FieldBody::Input(state) => Some(state.read(cx).focus_handle(cx)),
            _ => self.element_focus.clone(),
        };
        let (focused, filled) = match &self.body {
            FieldBody::Input(state) => {
                style_input(state, &theme, cx);
                if self.strong_placeholder {
                    strong_placeholder(state, &theme, cx);
                }
                let focus = state.read(cx).focus_handle(cx);
                let focused = focus.is_focused(window);
                let filled = !state.read(cx).value().is_empty();
                (focused, filled)
            }
            FieldBody::Value(value) => (self.open, !value.is_empty()),
            FieldBody::Muted(_) => (self.open, true),
            FieldBody::Element(_) => (
                self.open
                    || self
                        .element_focus
                        .as_ref()
                        .is_some_and(|focus| focus.is_focused(window)),
                true,
            ),
        };
        let shrunk = self.label.is_some() && (self.shrink || focused || filled);
        let colors = inks(&theme, focused, self.error, self.disabled);

        // MUI hides the placeholder while the label sits inside the field.
        if let FieldBody::Input(state) = &self.body
            && !self.keep_placeholder
        {
            let wanted: SharedString = if self.label.is_some() && !shrunk {
                SharedString::default()
            } else {
                self.placeholder.clone().unwrap_or_default()
            };
            let disabled = self.disabled;
            let presentation = state.read(cx).presentation();
            if presentation.placeholder() != &wanted || presentation.is_disabled() != disabled {
                state.update(cx, |state, cx| {
                    state.set_placeholder(wanted, window, cx);
                    state.set_disabled(disabled, cx);
                });
            }
        }

        let group: SharedString = format!("field-{:?}", self.id).into();
        let border = if focused && !self.disabled { 2. } else { 1. };
        let hover = theme.components.input_hover_border;
        let mut outline = div()
            .absolute()
            .inset_0()
            .rounded(px(theme.radius))
            .border_color(colors.outline);
        outline = if border == 2. {
            outline.border_2()
        } else if !self.error && !self.disabled {
            outline
                .border_1()
                .group_hover(group.clone(), move |s| s.border_color(hover))
        } else {
            outline.border_1()
        };

        let (family, size) = match self.font.clone() {
            Some((family, size)) => (family, size),
            None => (SharedString::from(fonts::UI), b1.size),
        };
        // The select arrow: `fontSize: 1.5rem`.
        let select_arrow = rem(24.);
        let pad_right = self
            .padding_right
            .unwrap_or(if self.select { 32. } else { 14. });
        let mut content = div().flex_1().min_w_0().flex();
        content = if self.multiline {
            content.items_start().py(px(8.5))
        } else {
            content.h_full().items_center()
        };
        let mut content = content
            .pl(px(14.))
            .pr(px(match (self.end.is_some(), self.end_edge) {
                (true, true) => 2.,
                (true, false) => 14.,
                (false, _) => pad_right,
            }));
        if let Some(start) = self.start {
            content = content.child(div().flex().flex_none().mr(px(8.)).child(start));
        }
        let body: AnyElement = match self.body {
            FieldBody::Input(state) => div()
                .flex_1()
                .min_w_0()
                .font_family(family)
                .text_size(px(size))
                .child(Input::new(&state))
                .into_any_element(),
            FieldBody::Value(value) => text(value, b1, colors.text).grow().into_any_element(),
            FieldBody::Muted(value) => text(value, body2(), p.text.secondary)
                .grow()
                .into_any_element(),
            FieldBody::Element(element) => {
                div().flex_1().min_w_0().child(element).into_any_element()
            }
        };
        content = content.child(body);
        if let Some(end) = self.end {
            content = content.child(div().flex().flex_none().ml(px(8.)).child(end));
        }

        let mut input_root = div()
            .id(self.id)
            .group(group)
            .relative()
            .flex()
            .when_multiline(self.multiline, height + line * (self.min_rows - 1) as f32)
            .rounded(px(theme.radius))
            .font_family(fonts::UI)
            .text_size(px(b1.size))
            .font_weight(FontWeight(b1.weight))
            .line_height(px(line))
            .text_color(colors.text)
            .child(content)
            .child(outline);
        if self.select {
            let rotation = if self.open { 0.5 } else { 0. };
            input_root = input_root.child(
                div()
                    .absolute()
                    .right(px(7.))
                    .top(px((height - select_arrow) / 2.))
                    .child(
                        icon(
                            "ArrowDropDown",
                            select_arrow,
                            if self.disabled {
                                p.action.disabled
                            } else {
                                p.action.active
                            },
                        )
                        .with_transformation(Transformation::rotate(percentage(rotation))),
                    ),
            );
        }
        if let Some(label) = &self.label {
            let label = label_text(label, self.required);
            if shrunk {
                let small = b1.size * 0.75;
                // The notch: the paper shows through the outline where the label sits.
                input_root = input_root.child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .left(px(9.))
                        .h(px(border))
                        .px(px(5.))
                        .bg(self.notch.unwrap_or(p.background_paper))
                        .child(text(
                            label.clone(),
                            b1.size(small).line_height(border),
                            Hsla::transparent_black(),
                        )),
                );
                input_root = input_root.child(
                    div()
                        .absolute()
                        .top(px(-9.))
                        .left(px(14.))
                        .max_w(relative(1.33))
                        .child(text(
                            label,
                            b1.size(small)
                                .line_height(line * 0.75)
                                .tracking(b1.letter_spacing * 0.75),
                            colors.label,
                        )),
                );
            } else {
                input_root = input_root.child(
                    div()
                        .absolute()
                        .top(px(if self.medium { 16. } else { 9. }))
                        .left(px(14.))
                        // `max-width: calc(100% - 24px)` from the 14px offset.
                        .right(px(10.))
                        .child(text(label, b1.line_height(line), colors.label)),
                );
            }
        }
        if let Some(anchor) = &self.anchor {
            input_root = input_root.child(bounds_probe(anchor));
        }
        if !self.disabled {
            if self.select || self.on_click.is_some() {
                input_root = input_root.cursor_pointer();
            } else {
                input_root = input_root.cursor_text();
            }
            if let Some(handler) = self.on_click {
                input_root =
                    input_root.on_click(move |event, window, cx| handler(event, window, cx));
            } else if let Some(focus) = input_focus {
                // A press anywhere on the outline puts the caret in the input.
                input_root = input_root.on_click(move |_, window, cx| window.focus(&focus, cx));
            }
        }

        // MUI's FormControl: `width: 100%` (fullWidth) or a fixed width, both
        // free to shrink in a row (min-width 0), as CSS flex items do.
        let mut root = div()
            .flex()
            .flex_col()
            .min_w_0()
            .flex_shrink(if self.no_shrink { 0. } else { 1. });
        if self.full_width {
            root = root.w_full();
        } else if let Some(width) = self.width {
            root = root.w(px(width));
        } else if !self.intrinsic {
            root = root.flex_1();
        }
        root = root.child(input_root);
        if let Some(helper) = self.helper {
            root = root.child(helper_text(helper, colors.helper));
        }
        root
    }
}

/// The input's placeholder in the text color (see [`FormField::strong_placeholder`]).
fn strong_placeholder(state: &Entity<InputState>, theme: &Theme, cx: &mut App) {
    let text = theme.palette.text.primary;
    let style = gpui_kit::base::input::InputEditorStyle {
        foreground: text,
        muted_foreground: text,
        background: Hsla::transparent_black(),
        border: Hsla::transparent_black(),
        selection: theme.components.text_selection,
        caret: text,
        ..Default::default()
    };
    state.update(cx, |state, _| state.set_editor_style(style));
}

/// `InputAdornment position="end"` text: body1 in `action.active`.
pub fn end_adornment(content: impl Into<SharedString>, theme: &Theme) -> impl IntoElement {
    text(content, body1(), theme.palette.action.active).no_truncate()
}

/// `FormHelperText`: a caption 3px under a field, inset 14px.
pub fn helper_text(content: impl Into<SharedString>, color: Hsla) -> impl IntoElement {
    div()
        .mt(px(3.))
        .mx(px(14.))
        .child(para(content, caption(), color))
}

trait HeightExt: Styled + Sized {
    /// A fixed height, or a minimum one for a multiline field.
    fn when_multiline(self, multiline: bool, height: f32) -> Self {
        if multiline {
            self.min_h(px(height))
        } else {
            self.h(px(height))
        }
    }
}

impl<T: Styled> HeightExt for T {}
