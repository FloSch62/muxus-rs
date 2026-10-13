//! MUI `FormControlLabel` (a radio, checkbox or switch with its label, pulled
//! 11px into the gutter and 16px apart from the next one) and `FormLabel`
//! (the secondary body1 line above a radio group).

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div,
};

use crate::label::label;
use crate::theme::{ActiveTheme, Theme};
use crate::widgets::controls::{Checkbox, Switch};
use crate::widgets::tooltip::Tooltip;
use crate::widgets::typography::{body2, inline_caption, para};

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Which control a [`FormControlLabel`] carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormControl {
    Radio(bool),
    Checkbox(bool),
    Switch(bool),
}

#[derive(IntoElement)]
pub struct FormControlLabel {
    id: ElementId,
    control: FormControl,
    label: SharedString,
    disabled: bool,
    on_click: Option<Handler>,
    label_element: Option<AnyElement>,
    small: bool,
    indeterminate: bool,
    flush: bool,
    align_start: bool,
    tooltip: Option<SharedString>,
}

impl FormControlLabel {
    pub fn new(
        id: impl Into<ElementId>,
        control: FormControl,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            control,
            label: label.into(),
            disabled: false,
            on_click: None,
            label_element: None,
            small: false,
            indeterminate: false,
            flush: false,
            align_start: false,
            tooltip: None,
        }
    }

    /// Any content as the label (a title over a caption, see
    /// [`label_with_caption`]); it fills the row and may wrap, and inherits
    /// the label ink (disabled ink while disabled).
    pub fn label_element(mut self, element: impl IntoElement) -> Self {
        self.label_element = Some(element.into_any_element());
        self
    }

    /// `size="small"` on the control.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// A checkbox's `indeterminate` state.
    pub fn indeterminate(mut self, indeterminate: bool) -> Self {
        self.indeterminate = indeterminate;
        self
    }

    /// Inside a MUI `Stack`, which resets its children's margins: no -11px
    /// pull into the gutter and no 16px after.
    pub fn flush(mut self) -> Self {
        self.flush = true;
        self
    }

    /// `alignItems: flex-start`: the control beside the label's first line.
    pub fn align_start(mut self) -> Self {
        self.align_start = true;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// The whole row (control and label) is the click target.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for FormControlLabel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let p = &theme.palette;
        let body = theme.typography.body1;
        let control_id: SharedString = format!("{:?}-control", self.id).into();
        let control: AnyElement = match self.control {
            FormControl::Switch(checked) => {
                let mut switch = Switch::new(control_id, checked).disabled(self.disabled);
                if self.small {
                    switch = switch.small();
                }
                switch.into_any_element()
            }
            FormControl::Radio(checked) => {
                let radio = Checkbox::radio(control_id, checked).disabled(self.disabled);
                if self.small { radio.small() } else { radio }.into_any_element()
            }
            FormControl::Checkbox(checked) => {
                let checkbox = Checkbox::new(control_id, checked)
                    .indeterminate(self.indeterminate)
                    .disabled(self.disabled);
                if self.small {
                    checkbox.small()
                } else {
                    checkbox
                }
                .into_any_element()
            }
        };
        let ink = if self.disabled {
            p.text.disabled
        } else {
            p.text.primary
        };
        let mut row = div()
            .id(self.id)
            .flex()
            .ml(px(if self.flush { 0. } else { -11. }))
            .mr(px(if self.flush { 0. } else { 16. }))
            .child(div().flex().flex_none().child(control));
        row = match self.label_element {
            Some(element) => row.child(div().flex_1().min_w_0().text_color(ink).child(element)),
            None => row.flex_none().child(
                label(self.label)
                    .size(body.size)
                    .weight(body.weight)
                    .line_height(body.line_height)
                    .color(ink)
                    .no_truncate(),
            ),
        };
        row = if self.align_start {
            row.items_start()
        } else {
            row.items_center()
        };
        if !self.disabled {
            row = row.cursor_pointer();
            if let Some(handler) = self.on_click {
                row = row.on_click(move |event, window, cx| handler(event, window, cx));
            }
        }
        if let Some(tip) = self.tooltip {
            row = row.tooltip(Tooltip::text(tip));
        }
        row
    }
}

/// A labelled control's two-line label: a body2 title over a caption, with
/// `py` above and below. While disabled the title takes the disabled ink,
/// the caption keeps its own.
pub fn label_with_caption(
    theme: &Theme,
    title: impl Into<SharedString>,
    caption: impl Into<SharedString>,
    py: f32,
    disabled: bool,
) -> Div {
    let title_ink = if disabled {
        theme.palette.text.disabled
    } else {
        theme.palette.text.primary
    };
    div()
        .flex()
        .flex_col()
        .py(px(py))
        .child(para(title, body2(), title_ink))
        .child(inline_caption(caption, theme.palette.text.secondary))
}

/// `FormLabel`: the question above a radio group, body1 in secondary ink
/// (line height 1.4375em, as MUI's `FormLabel` sets it).
pub fn form_label(content: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let body = theme.typography.body1;
    label(content)
        .size(body.size)
        .weight(body.weight)
        .line_height(body.size * 1.4375)
        .color(theme.palette.text.secondary)
        .no_truncate()
}
