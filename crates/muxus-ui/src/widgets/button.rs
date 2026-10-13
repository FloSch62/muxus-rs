//! MUI `IconButton` and `Button` as the Muxus theme configures them
//! (`MuiButton`: small by default, no uppercase, 7px corners).

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    AnyElement, App, BoxShadow, ClickEvent, ElementId, FontWeight, Hsla, InteractiveElement,
    IntoElement, MouseButton, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement,
    Styled, Window, div, point, relative,
};

use crate::color::alpha;
use crate::icon::icon;
use crate::label::label;
use crate::theme::{ActiveTheme, Theme, fonts};
use crate::widgets::tooltip::{AnchorBounds, Placement, Tooltip, bounds_probe};

pub type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// The palette slot a control is colored with (`color` prop).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonColor {
    /// IconButton's default: `action.active`.
    #[default]
    Default,
    /// Follows the surrounding text color.
    Inherit,
    Primary,
    Secondary,
    Success,
    Warning,
    Error,
    Info,
}

impl ButtonColor {
    /// The ink for this slot, and the base hue its hover tint is derived from.
    fn ink(self, theme: &Theme, inherit: Hsla) -> Hsla {
        let p = &theme.palette;
        match self {
            ButtonColor::Default => p.action.active,
            ButtonColor::Inherit => inherit,
            ButtonColor::Primary => p.primary.main,
            ButtonColor::Secondary => p.secondary.main,
            ButtonColor::Success => p.success.main,
            ButtonColor::Warning => p.warning.main,
            ButtonColor::Error => p.error.main,
            ButtonColor::Info => p.info.main,
        }
    }

    fn palette(self, theme: &Theme) -> Option<crate::theme::PaletteColor> {
        let p = &theme.palette;
        Some(match self {
            ButtonColor::Primary => p.primary,
            ButtonColor::Secondary => p.secondary,
            ButtonColor::Success => p.success,
            ButtonColor::Warning => p.warning,
            ButtonColor::Error => p.error,
            ButtonColor::Info => p.info,
            ButtonColor::Default | ButtonColor::Inherit => return None,
        })
    }
}

/// `IconButton`. Small by default: 5px padding around an 18.57px icon.
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: SharedString,
    icon_size: f32,
    padding: f32,
    color: ButtonColor,
    ink: Option<Hsla>,
    disabled: bool,
    edge_end: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
    badge: Option<AnyElement>,
    selected: bool,
    anchor: Option<AnchorBounds>,
    placement: Placement,
    wash: Option<(Hsla, Hsla)>,
}

impl IconButton {
    pub fn new(id: impl Into<ElementId>, icon: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon: icon.into(),
            icon_size: crate::icon::SMALL,
            padding: 5.,
            color: ButtonColor::Default,
            ink: None,
            disabled: false,
            edge_end: false,
            tooltip: None,
            on_click: None,
            badge: None,
            selected: false,
            anchor: None,
            placement: Placement::Bottom,
            wash: None,
        }
    }

    /// A background tint and its hover form (`sx={{ bgcolor, '&:hover':
    /// { bgcolor } }}`: the multi-exec control while it mirrors).
    pub fn background(mut self, background: Hsla, hover: Hsla) -> Self {
        self.wash = Some((background, hover));
        self
    }

    /// Record the button's painted bounds (menus anchor to them).
    pub fn track_bounds(mut self, anchor: &AnchorBounds) -> Self {
        self.anchor = Some(anchor.clone());
        self
    }

    /// Tooltip placement (`placement` prop); bottom by default.
    pub fn tooltip_placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    /// `sx={{ fontSize }}` on the icon.
    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = size;
        self
    }

    /// `sx={{ p }}` on the button, in px.
    pub fn padding(mut self, padding: f32) -> Self {
        self.padding = padding;
        self
    }

    /// MUI `size="medium"`: 8px padding around a 22.29px icon.
    pub fn medium(mut self) -> Self {
        self.padding = 8.;
        self.icon_size = crate::icon::MEDIUM;
        self
    }

    pub fn color(mut self, color: ButtonColor) -> Self {
        self.color = color;
        self
    }

    /// An explicit icon color (`sx={{ color }}`), overriding the palette slot.
    pub fn ink(mut self, color: Hsla) -> Self {
        self.ink = Some(color);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// `edge="end"`: pulls the button 3px into the padding of its container.
    pub fn edge_end(mut self) -> Self {
        self.edge_end = true;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// A pressed/toggled look, as the rail tabs and toggles use.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// A badge drawn over the icon's top-right corner (see [`super::badge`]).
    pub fn badge(mut self, badge: impl IntoElement) -> Self {
        self.badge = Some(badge.into_any_element());
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

impl RenderOnce for IconButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let inherit = window.text_style().color;
        let ink = if self.disabled {
            theme.palette.action.disabled
        } else {
            self.ink.unwrap_or_else(|| self.color.ink(theme, inherit))
        };
        // MUI: hover is the ink at the hover opacity; default buttons use action.active.
        let hover = match self.color {
            ButtonColor::Default => alpha(
                theme.palette.action.active,
                theme.palette.action.hover_opacity,
            ),
            ButtonColor::Inherit => alpha(inherit, theme.palette.action.hover_opacity),
            _ => alpha(ink, theme.palette.action.hover_opacity),
        };
        let hover = self.wash.map_or(hover, |(_, hover)| hover);
        let size = self.icon_size + 2. * self.padding;
        let mut button = div()
            .id(self.id)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(size))
            .rounded(px(size / 2.))
            .when_some(
                self.selected.then_some(theme.palette.action.selected),
                |b, bg| b.bg(bg),
            )
            .child(icon(&self.icon, self.icon_size, ink));
        if let Some((background, _)) = self.wash {
            button = button.bg(background);
        }
        if self.edge_end {
            button = button.mr(px(-3.));
        }
        if let Some(badge) = self.badge {
            button = button.child(badge);
        }
        let anchor = self.anchor.unwrap_or_default();
        button = button.child(bounds_probe(&anchor));
        if !self.disabled {
            button = button
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default());
            if let Some(handler) = self.on_click {
                button = button.on_click(move |event, window, cx| handler(event, window, cx));
            }
        }
        if let Some(text) = self.tooltip
            && !text.is_empty()
        {
            button = button.tooltip(Tooltip::anchored(text, anchor, self.placement));
        }
        button
    }
}

/// `variant` of a [`Button`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonVariant {
    #[default]
    Text,
    Outlined,
    Contained,
}

/// Colors that replace the palette slot's (`sx` color overrides such as
/// the command bar's accented buttons). The caller picks the disabled form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonTint {
    pub ink: Hsla,
    /// `None` keeps the variant's border (none for text buttons).
    pub border: Option<Hsla>,
    pub background: Option<Hsla>,
    pub hover_border: Option<Hsla>,
    pub hover_background: Option<Hsla>,
}

/// `Button`. Small by default (the theme's `defaultProps`).
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    variant: ButtonVariant,
    color: ButtonColor,
    medium: bool,
    start_icon: Option<SharedString>,
    disabled: bool,
    full_width: bool,
    min_width: Option<f32>,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
    label_element: Option<AnyElement>,
    tint: Option<ButtonTint>,
    end_icon: Option<SharedString>,
    end_icon_gap: f32,
    padding_y: Option<f32>,
    padding_x: Option<f32>,
    start_icon_size: Option<f32>,
    start_icon_gap: f32,
    dashed: bool,
    max_width: Option<f32>,
    font_size: Option<f32>,
    end_icon_turned: bool,
    mono: bool,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant: ButtonVariant::Text,
            color: ButtonColor::Primary,
            medium: false,
            start_icon: None,
            disabled: false,
            full_width: false,
            min_width: None,
            tooltip: None,
            on_click: None,
            label_element: None,
            tint: None,
            end_icon: None,
            end_icon_gap: 8.,
            padding_y: None,
            padding_x: None,
            start_icon_size: None,
            start_icon_gap: 8.,
            dashed: false,
            max_width: None,
            font_size: None,
            end_icon_turned: false,
            mono: false,
        }
    }

    /// `sx={{ fontSize }}` on the label (the line height stays 1.75 of it).
    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = Some(size);
        self
    }

    /// The end icon turned half a turn (an expand chevron while open).
    pub fn end_icon_turned(mut self, turned: bool) -> Self {
        self.end_icon_turned = turned;
        self
    }

    /// The label in the monospace font (a path crumb).
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// Colors replacing the palette slot's (see [`ButtonTint`]).
    pub fn tint(mut self, tint: ButtonTint) -> Self {
        self.tint = Some(tint);
        self
    }

    /// `endIcon` (18px small, 20px medium), `gap` px after the label.
    pub fn end_icon(mut self, icon: impl Into<SharedString>, gap: f32) -> Self {
        self.end_icon = Some(icon.into());
        self.end_icon_gap = gap;
        self
    }

    /// `sx={{ py }}` in px, replacing the size's padding.
    pub fn padding_y(mut self, padding: f32) -> Self {
        self.padding_y = Some(padding);
        self
    }

    /// `sx={{ px }}` in px, replacing the size's padding.
    pub fn padding_x(mut self, padding: f32) -> Self {
        self.padding_x = Some(padding);
        self
    }

    /// `'& .MuiButton-startIcon': { mr, '& svg': { fontSize } }`.
    pub fn start_icon_style(mut self, size: f32, gap: f32) -> Self {
        self.start_icon_size = Some(size);
        self.start_icon_gap = gap;
        self
    }

    /// `borderStyle: 'dashed'` (an outlined button whose target is gone).
    pub fn dashed(mut self, dashed: bool) -> Self {
        self.dashed = dashed;
        self
    }

    /// `sx={{ maxWidth }}`: the label ellipsizes beyond it.
    pub fn max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn outlined(self) -> Self {
        self.variant(ButtonVariant::Outlined)
    }

    pub fn contained(self) -> Self {
        self.variant(ButtonVariant::Contained)
    }

    pub fn color(mut self, color: ButtonColor) -> Self {
        self.color = color;
        self
    }

    pub fn medium(mut self) -> Self {
        self.medium = true;
        self
    }

    pub fn start_icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.start_icon = Some(icon.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// `minWidth` override; MUI's default is 64px.
    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = Some(width);
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// Replace the plain label with custom content (e.g. a body2 Typography).
    pub fn label_element(mut self, element: impl IntoElement) -> Self {
        self.label_element = Some(element.into_any_element());
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

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let typo = theme.typography;
        let inherit = window.text_style().color;
        let palette = self.color.palette(theme);
        let main = self.color.ink(theme, inherit);
        let hover_opacity = theme.palette.action.hover_opacity;
        // MUI sizes: font pxToRem(13) small / pxToRem(14) medium, line height 1.75.
        let font_size = self
            .font_size
            .unwrap_or_else(|| typo.px_to_rem(if self.medium { 14. } else { 13. }));
        let line_height = font_size * 1.75;
        let family = if self.mono { fonts::MONO } else { fonts::UI };
        let (pad_y, pad_x) = match (self.variant, self.medium) {
            (ButtonVariant::Text, false) => (4., 5.),
            (ButtonVariant::Text, true) => (6., 8.),
            (ButtonVariant::Outlined, false) => (3., 9.),
            (ButtonVariant::Outlined, true) => (5., 15.),
            (ButtonVariant::Contained, false) => (4., 10.),
            (ButtonVariant::Contained, true) => (6., 16.),
        };
        let (fg, bg, border, hover_bg, hover_border) = if self.disabled {
            let disabled = theme.palette.action.disabled;
            match self.variant {
                ButtonVariant::Contained => (
                    disabled,
                    Some(theme.palette.action.disabled_background),
                    None,
                    None,
                    None,
                ),
                ButtonVariant::Outlined => (
                    disabled,
                    None,
                    Some(theme.palette.action.disabled_background),
                    None,
                    None,
                ),
                ButtonVariant::Text => (disabled, None, None, None, None),
            }
        } else {
            match self.variant {
                ButtonVariant::Text => (main, None, None, Some(alpha(main, hover_opacity)), None),
                ButtonVariant::Outlined => (
                    main,
                    None,
                    Some(alpha(main, 0.5)),
                    Some(alpha(main, hover_opacity)),
                    Some(main),
                ),
                ButtonVariant::Contained => {
                    let (fill, text, dark) = match palette {
                        Some(p) => (p.main, p.contrast_text, p.dark),
                        None => (
                            theme.palette.action.disabled_background,
                            theme.palette.text.primary,
                            theme.palette.action.selected,
                        ),
                    };
                    (text, Some(fill), None, Some(dark), None)
                }
            }
        };
        let (fg, bg, border, hover_bg, hover_border) = match self.tint {
            Some(tint) => (
                tint.ink,
                tint.background,
                tint.border.or(border),
                tint.hover_background,
                tint.hover_border,
            ),
            None => (fg, bg, border, hover_bg, hover_border),
        };
        let pad_y = self.padding_y.unwrap_or(pad_y);
        let pad_x = self.padding_x.unwrap_or(pad_x);
        let icon_ink = fg;
        let mut button = div()
            .id(self.id)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .min_w(px(self.min_width.unwrap_or(64.)))
            .rounded(px(7.))
            .font_family(family)
            .text_size(px(font_size))
            .font_weight(FontWeight(typo.button.weight))
            .line_height(px(line_height))
            .text_color(fg)
            .whitespace_nowrap()
            .px(px(pad_x))
            .py(px(pad_y));
        if self.full_width {
            button = button.w(relative(1.));
        }
        if let Some(bg) = bg {
            button = button.bg(bg);
        }
        if let Some(border) = border {
            button = button.border_1().border_color(border);
            if self.dashed {
                button = button.border_dashed();
            }
        }
        if let Some(width) = self.max_width {
            button = button.max_w(px(width)).min_w_0();
        }
        if self.variant == ButtonVariant::Contained && !self.disabled {
            button = button.shadow(elevation(2, theme));
        }
        if let Some(icon_name) = &self.start_icon {
            let icon_size = self
                .start_icon_size
                .unwrap_or(if self.medium { 20. } else { 18. });
            button = button.child(
                div()
                    .flex()
                    .ml(px(-2.))
                    .mr(px(self.start_icon_gap))
                    .child(icon(icon_name, icon_size, icon_ink)),
            );
        }
        button = match self.label_element {
            Some(element) => button.child(element),
            None if self.max_width.is_some() => button.child(
                label(self.label)
                    .family(family)
                    .size(font_size)
                    .weight(typo.button.weight)
                    .line_height(line_height)
                    .color(fg),
            ),
            None => button.child(
                label(self.label)
                    .family(family)
                    .size(font_size)
                    .weight(typo.button.weight)
                    .line_height(line_height)
                    .color(fg)
                    .no_truncate(),
            ),
        };
        if let Some(icon_name) = &self.end_icon {
            let icon_size = if self.medium { 20. } else { 18. };
            button = button.child(
                div()
                    .flex()
                    .flex_none()
                    .ml(px(self.end_icon_gap))
                    .mr(px(-2.))
                    .child(if self.end_icon_turned {
                        icon(icon_name, icon_size, icon_ink).with_transformation(
                            gpui_kit::Transformation::rotate(gpui_kit::radians(
                                std::f32::consts::PI,
                            )),
                        )
                    } else {
                        icon(icon_name, icon_size, icon_ink)
                    }),
            );
        }
        if !self.disabled {
            button = button.cursor_pointer().hover(move |mut s| {
                if let Some(bg) = hover_bg {
                    s = s.bg(bg);
                }
                if let Some(border) = hover_border {
                    s = s.border_color(border);
                }
                s
            });
            button =
                button.on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default());
            if let Some(handler) = self.on_click {
                button = button.on_click(move |event, window, cx| handler(event, window, cx));
            }
        }
        if let Some(text) = self.tooltip {
            let anchor = AnchorBounds::default();
            button = button
                .child(bounds_probe(&anchor))
                .tooltip(Tooltip::anchored(text, anchor, Placement::Bottom));
        }
        button
    }
}

/// MUI `theme.shadows[n]` for the elevations Muxus uses, as layered shadows.
pub fn elevation(level: u8, theme: &Theme) -> Vec<BoxShadow> {
    let _ = theme;
    // MUI's three umbra/penumbra/ambient layers at 0.2 / 0.14 / 0.12 black.
    let layers: [(f32, f32, f32, f32); 3] = match level {
        0 => return Vec::new(),
        1 => [(2., 1., -1., 0.2), (1., 1., 0., 0.14), (1., 3., 0., 0.12)],
        2 => [(3., 1., -2., 0.2), (2., 2., 0., 0.14), (1., 5., 0., 0.12)],
        4 => [(2., 4., -1., 0.2), (4., 5., 0., 0.14), (1., 10., 0., 0.12)],
        6 => [(3., 5., -1., 0.2), (6., 10., 0., 0.14), (1., 18., 0., 0.12)],
        8 => [(5., 5., -3., 0.2), (8., 10., 1., 0.14), (3., 14., 2., 0.12)],
        _ => [
            (11., 15., -7., 0.2),
            (24., 38., 3., 0.14),
            (9., 46., 8., 0.12),
        ],
    };
    layers
        .iter()
        .map(|&(y, blur, spread, a)| BoxShadow {
            color: crate::color::rgba8(0, 0, 0, a),
            offset: point(px(0.), px(y)),
            blur_radius: px(blur),
            spread_radius: px(spread),
            inset: false,
        })
        .collect()
}

trait When: Sized {
    fn when_some<T>(self, value: Option<T>, f: impl FnOnce(Self, T) -> Self) -> Self {
        match value {
            Some(value) => f(self, value),
            None => self,
        }
    }
}

impl<T: Styled> When for T {}
