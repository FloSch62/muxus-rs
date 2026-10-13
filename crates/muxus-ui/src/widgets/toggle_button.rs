//! MUI `ToggleButtonGroup exclusive fullWidth size="small"`: equal-width
//! buttons in one outlined row, the chosen one tinted. Labels use the
//! button type at `pxToRem(13)`, weight 550 (`MuiToggleButton` override).

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div,
};

use crate::color::alpha;
use crate::icon::icon;
use crate::label::label;
use crate::theme::ActiveTheme;

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// One button of a [`ToggleButtonGroup`].
pub struct ToggleButton {
    id: ElementId,
    label: SharedString,
    icon: Option<SharedString>,
    selected: bool,
    on_click: Option<Handler>,
    content: Option<AnyElement>,
}

/// The ink of a toggle button's content: text.primary when selected,
/// action.active otherwise (for content passed with [`ToggleButton::content`]).
pub fn toggle_ink(theme: &crate::theme::Theme, selected: bool) -> gpui_kit::Hsla {
    if selected {
        theme.palette.text.primary
    } else {
        theme.palette.action.active
    }
}

impl ToggleButton {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            selected,
            on_click: None,
            content: None,
        }
    }

    /// A 17px icon before the label.
    pub fn icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Any content instead of the icon and label (an icon over a caption);
    /// color it with [`toggle_ink`].
    pub fn content(mut self, content: impl IntoElement) -> Self {
        self.content = Some(content.into_any_element());
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

#[derive(IntoElement)]
pub struct ToggleButtonGroup {
    buttons: Vec<ToggleButton>,
    /// `(py, px, fontSize)` from `& .MuiToggleButton-root` overrides; the
    /// group then sizes to its buttons instead of taking the full width.
    inline: Option<(f32, f32, f32)>,
    /// `(py, px, fontSize)` overrides that keep the full width.
    sizing: Option<(f32, f32, f32)>,
    mono: bool,
    disabled: bool,
    icon_style: Option<(f32, f32)>,
}

impl ToggleButtonGroup {
    pub fn new(buttons: Vec<ToggleButton>) -> Self {
        Self {
            buttons,
            inline: None,
            sizing: None,
            mono: false,
            disabled: false,
            icon_style: None,
        }
    }

    /// A group as wide as its buttons (no `fullWidth`), with the buttons'
    /// `py`, `px` and `fontSize` overridden.
    pub fn inline(mut self, py: f32, px: f32, font_size: f32) -> Self {
        self.inline = Some((py, px, font_size));
        self
    }

    /// `fullWidth` with the buttons' `py`, `px` and `fontSize` overridden.
    pub fn sizing(mut self, py: f32, px: f32, font_size: f32) -> Self {
        self.sizing = Some((py, px, font_size));
        self
    }

    /// Labels in the monospace font.
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// The whole group disabled.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Icon size and the gap between icon and label.
    pub fn icon_style(mut self, size: f32, gap: f32) -> Self {
        self.icon_style = Some((size, gap));
        self
    }
}

impl RenderOnce for ToggleButtonGroup {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let p = &theme.palette;
        let size = self
            .inline
            .or(self.sizing)
            .map_or_else(|| theme.typography.px_to_rem(13.), |(_, _, size)| size);
        let weight = theme.typography.button.weight;
        let family = if self.mono {
            crate::theme::fonts::MONO
        } else {
            crate::theme::fonts::UI
        };
        let (icon_size, icon_gap) = self.icon_style.unwrap_or((17., 6.));
        let border = if self.disabled {
            p.action.disabled_background
        } else {
            p.divider
        };
        let selected_bg = alpha(p.text.primary, p.action.selected_opacity);
        let selected_hover = alpha(
            p.text.primary,
            p.action.selected_opacity + p.action.hover_opacity,
        );
        let hover = alpha(p.text.primary, p.action.hover_opacity);
        let count = self.buttons.len();
        let mut group = div().flex();
        group = if self.inline.is_some() {
            group.flex_none().items_stretch()
        } else {
            group.w_full()
        };
        for (index, button) in self.buttons.into_iter().enumerate() {
            let ink = if self.disabled {
                p.action.disabled
            } else if button.selected {
                p.text.primary
            } else {
                p.action.active
            };
            let mut item = div()
                .id(button.id)
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .border_color(border);
            match button.content {
                Some(custom) => item = item.child(custom),
                None => {
                    // The icon and label sit directly in the button, so the
                    // button's intrinsic width is theirs inside a wrapping row.
                    if let Some(name) = &button.icon {
                        item = item.child(
                            div()
                                .flex_none()
                                .mr(px(icon_gap))
                                .child(icon(name, icon_size, ink)),
                        );
                    }
                    item = item.child(
                        label(button.label)
                            .family(family)
                            .size(size)
                            .weight(weight)
                            .line_height(size * 1.75)
                            .color(ink)
                            .no_truncate(),
                    );
                }
            }
            item = match self.inline {
                Some((py, px_, _)) => item.whitespace_nowrap().py(px(py)).px(px(px_)),
                None => {
                    let item = item.flex_1().flex_basis(px(0.)).min_w_0();
                    match self.sizing {
                        Some((py, px_, _)) => item.py(px(py)).px(px(px_)),
                        None => item.p(px(7.)),
                    }
                }
            };
            if !self.disabled {
                item = item.cursor_pointer();
                if let Some(handler) = button.on_click {
                    item = item.on_click(move |event, window, cx| handler(event, window, cx));
                }
            }
            if index > 0 {
                item = item.ml(px(-1.));
            }
            if index == 0 {
                item = item.rounded_l(px(theme.radius));
            }
            if index + 1 == count {
                item = item.rounded_r(px(theme.radius));
            }
            item = match (button.selected, self.disabled) {
                (true, false) => item.bg(selected_bg).hover(move |s| s.bg(selected_hover)),
                (true, true) => item.bg(selected_bg),
                (false, false) => item.hover(move |s| s.bg(hover)),
                (false, true) => item,
            };
            group = group.child(item);
        }
        group
    }
}

/// One option of an [`ToggleButtonGroup::exclusive`] group.
#[derive(Clone)]
pub struct ToggleOption {
    pub label: SharedString,
    pub icon: Option<SharedString>,
}

impl ToggleOption {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            icon: None,
        }
    }

    pub fn icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.icon = Some(icon.into());
        self
    }
}

/// Options from plain labels.
pub fn toggle_options(labels: &[&str]) -> Vec<ToggleOption> {
    labels
        .iter()
        .map(|label| ToggleOption::new(*label))
        .collect()
}

impl ToggleButtonGroup {
    /// An `exclusive` group from options: the one at `selected` is pressed;
    /// pressing another calls `on_select` with its index (pressing the
    /// pressed one changes nothing, as a handler ignoring `null` does).
    pub fn exclusive(
        id: impl Into<SharedString>,
        options: Vec<ToggleOption>,
        selected: Option<usize>,
        on_select: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        let id: SharedString = id.into();
        let on_select = Rc::new(on_select);
        let buttons = options
            .into_iter()
            .enumerate()
            .map(|(index, option)| {
                let pressed = selected == Some(index);
                let mut button = ToggleButton::new(
                    ElementId::NamedInteger(id.clone(), index as u64),
                    option.label,
                    pressed,
                );
                if let Some(icon) = option.icon {
                    button = button.icon(icon);
                }
                if !pressed {
                    let on_select = on_select.clone();
                    button = button.on_click(move |_, window, cx| on_select(index, window, cx));
                }
                button
            })
            .collect();
        Self::new(buttons)
    }
}

/// A row of toggle buttons as an element (for callers that build the list).
pub fn toggle_buttons(buttons: Vec<ToggleButton>) -> AnyElement {
    ToggleButtonGroup::new(buttons).into_any_element()
}
