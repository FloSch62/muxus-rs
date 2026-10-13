//! Selection controls: MUI `Checkbox`, `Radio` and `Switch`.

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div,
};

use crate::color::alpha;
use crate::icon::{MEDIUM, SMALL, icon};
use crate::theme::ActiveTheme;
use crate::widgets::button::elevation;

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// `Checkbox` (also used for `Radio` via [`Checkbox::radio`]): 9px padding
/// around the box icon, text.secondary unchecked, primary checked.
#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    checked: bool,
    indeterminate: bool,
    small: bool,
    radio: bool,
    disabled: bool,
    padding: f32,
    on_click: Option<Handler>,
    checked_color: Option<gpui_kit::Hsla>,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Self {
            id: id.into(),
            checked,
            indeterminate: false,
            small: false,
            radio: false,
            disabled: false,
            padding: 9.,
            on_click: None,
            checked_color: None,
        }
    }

    /// `color="warning"` etc.: the checked ink (primary by default).
    pub fn checked_color(mut self, color: gpui_kit::Hsla) -> Self {
        self.checked_color = Some(color);
        self
    }

    /// `Radio` with the same geometry.
    pub fn radio(id: impl Into<ElementId>, checked: bool) -> Self {
        Self {
            radio: true,
            ..Self::new(id, checked)
        }
    }

    pub fn indeterminate(mut self, indeterminate: bool) -> Self {
        self.indeterminate = indeterminate;
        self
    }

    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// `sx={{ p }}` in px.
    pub fn padding(mut self, padding: f32) -> Self {
        self.padding = padding;
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

impl RenderOnce for Checkbox {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let p = &theme.palette;
        let on = self.checked || self.indeterminate;
        let checked_ink = self.checked_color.unwrap_or(p.primary.main);
        let ink = if self.disabled {
            p.action.disabled
        } else if on {
            checked_ink
        } else {
            p.text.secondary
        };
        let name = match (self.radio, self.checked, self.indeterminate) {
            (true, true, _) => "RadioButtonChecked",
            (true, false, _) => "RadioButtonUnchecked",
            (false, _, true) => "IndeterminateCheckBox",
            (false, true, _) => "CheckBox",
            (false, false, _) => "CheckBoxOutlineBlank",
        };
        let size = if self.small { SMALL } else { MEDIUM };
        let hover = if on {
            alpha(checked_ink, p.action.hover_opacity)
        } else {
            alpha(p.action.active, p.action.hover_opacity)
        };
        let total = size + 2. * self.padding;
        let mut root = div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(total))
            .rounded(px(total / 2.))
            .child(icon(name, size, ink));
        if !self.disabled {
            root = root.cursor_pointer().hover(move |s| s.bg(hover));
            if let Some(handler) = self.on_click {
                root = root.on_click(move |event, window, cx| handler(event, window, cx));
            }
        }
        root
    }
}

/// `Switch`: a 34×14 track with a 20px thumb (small: 26×10 with 16px).
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    checked: bool,
    small: bool,
    disabled: bool,
    on_click: Option<Handler>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Self {
            id: id.into(),
            checked,
            small: false,
            disabled: false,
            on_click: None,
        }
    }

    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
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

impl RenderOnce for Switch {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let p = &theme.palette;
        let dark = theme.mode.is_dark();
        // MUI: root 58×38 (small 40×24) with 12px (7px) padding around the track;
        // the thumb's switch base has 9px (4px) padding and slides 20px (16px).
        let (width, height, pad, thumb, base_pad, travel) = if self.small {
            (40., 24., 7., 16., 4., 16.)
        } else {
            (58., 38., 12., 20., 9., 20.)
        };
        // MUI: the track is black (white in dark mode) at 0.38 (0.3), the
        // checked one primary at 0.5; disabled tracks drop to 0.12 (0.2).
        let track_base = if self.checked {
            p.primary.main
        } else if dark {
            p.common_white
        } else {
            p.common_black
        };
        let track_opacity = match (self.disabled, self.checked, dark) {
            (true, _, false) => 0.12,
            (true, _, true) => 0.2,
            (false, true, _) => 0.5,
            (false, false, true) => 0.3,
            (false, false, false) => 0.38,
        };
        let track_color = alpha(track_base, track_opacity);
        // The thumb: white (grey 300 in dark mode), primary when checked;
        // disabled grey 100 (grey 600), or the primary lightened by 0.62
        // (darkened by 0.55) when checked.
        let thumb_color = match (self.checked, self.disabled, dark) {
            (true, false, _) => p.primary.main,
            (true, true, false) => crate::color::lighten(p.primary.main, 0.62),
            (true, true, true) => crate::color::darken(p.primary.main, 0.55),
            (false, false, false) => p.common_white,
            (false, false, true) => p.grey.g300,
            (false, true, false) => p.grey.g100,
            (false, true, true) => p.grey.g600,
        };
        let base_size = thumb + 2. * base_pad;
        let left = if self.checked { travel } else { 0. };
        let hover = if self.checked {
            alpha(p.primary.main, p.action.hover_opacity)
        } else {
            alpha(p.action.active, p.action.hover_opacity)
        };
        let mut root = div()
            .id(self.id)
            .relative()
            .flex_none()
            .w(px(width))
            .h(px(height))
            .p(px(pad))
            .child(
                div()
                    .size_full()
                    .rounded(px((height - 2. * pad) / 2.))
                    .bg(track_color),
            )
            .child(
                div()
                    .absolute()
                    .top(px(0.))
                    .left(px(left))
                    .size(px(base_size))
                    .rounded(px(base_size / 2.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when_enabled(!self.disabled, hover)
                    .child(
                        div()
                            .size(px(thumb))
                            .rounded(px(thumb / 2.))
                            .bg(thumb_color)
                            .shadow(elevation(1, theme)),
                    ),
            );
        if !self.disabled {
            root = root.cursor_pointer();
            if let Some(handler) = self.on_click {
                root = root.on_click(move |event, window, cx| handler(event, window, cx));
            }
        }
        root
    }
}

trait EnabledHover: Styled + Sized {
    fn when_enabled(self, enabled: bool, hover: gpui_kit::Hsla) -> Self;
}

impl EnabledHover for gpui_kit::Div {
    fn when_enabled(self, enabled: bool, hover: gpui_kit::Hsla) -> Self {
        if enabled {
            self.hover(move |s| s.bg(hover))
        } else {
            self
        }
    }
}
