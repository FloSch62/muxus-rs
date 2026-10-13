//! Display components: `Divider`, `Badge`, `Chip`, `LinearProgress`,
//! `Alert` (and the snackbar it sits in for toasts).

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement,
    MouseButton, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled,
    Window, div, relative,
};

use crate::color::{alpha, darken, lighten};
use crate::icon::icon;
use crate::theme::{ActiveTheme, Status, Theme, fonts};
use crate::widgets::button::elevation;
use crate::widgets::tooltip::{AnchorBounds, Placement, Tooltip, bounds_probe};

type ChipHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Horizontal `<Divider />`.
pub fn divider(theme: &Theme) -> Div {
    div()
        .h(px(1.))
        .flex_none()
        .w(relative(1.))
        .bg(theme.palette.divider)
}

/// `<Divider orientation="vertical" flexItem sx={{ my, mx }} />`.
pub fn vertical_divider(theme: &Theme, my: f32, mx: f32) -> Div {
    div()
        .w(px(1.))
        .flex_none()
        .self_stretch()
        .my(px(my))
        .mx(px(mx))
        .bg(theme.palette.divider)
}

/// `Badge` content as the TopBar draws it: 9px digits on a 14px pill whose
/// centre sits on the top-right corner of the icon (MUI translates it
/// 50% / -50%). `icon_origin` is the icon's offset inside the parent (the
/// IconButton's padding) and `icon_size` its size.
pub fn badge(count: usize, color: Hsla, ink: Hsla, icon_origin: f32, icon_size: f32) -> Div {
    let label = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    // A fixed-width box centred on the corner, so the pill centres itself
    // whatever the width of its digits.
    let slot = 48.;
    div()
        .absolute()
        .top(px(icon_origin - 7.))
        .left(px(icon_origin + icon_size - slot / 2.))
        .w(px(slot))
        .h(px(14.))
        .flex()
        .justify_center()
        .child(
            div()
                .h(px(14.))
                .min_w(px(14.))
                .px(px(6.))
                .rounded(px(10.))
                .bg(color)
                .text_color(ink)
                .font_family(fonts::UI)
                .text_size(px(9.))
                .font_weight(FontWeight(500.))
                .line_height(px(9.))
                .flex()
                .items_center()
                .justify_center()
                .child(label),
        )
}

/// `Chip` (small by default, weight 500 per the theme).
#[derive(IntoElement)]
pub struct Chip {
    label: SharedString,
    outlined: bool,
    color: Option<Hsla>,
    palette: Option<crate::theme::PaletteColor>,
    icon: Option<SharedString>,
    mono: bool,
    width: Option<f32>,
    height: Option<f32>,
    font_size: Option<f32>,
    id: Option<ElementId>,
    on_click: Option<ChipHandler>,
    tooltip: Option<SharedString>,
    min_width: Option<f32>,
    opacity: f32,
    label_padding: Option<f32>,
    icon_size: Option<f32>,
    icon_ink: Option<Hsla>,
    icon_margin: Option<f32>,
    on_delete: Option<(SharedString, Option<SharedString>, ChipHandler)>,
}

impl Chip {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            outlined: false,
            color: None,
            palette: None,
            icon: None,
            mono: false,
            width: None,
            height: None,
            font_size: None,
            id: None,
            on_click: None,
            tooltip: None,
            min_width: None,
            opacity: 1.,
            label_padding: None,
            icon_size: None,
            icon_ink: None,
            icon_margin: None,
            on_delete: None,
        }
    }

    /// A clickable chip (`onClick`): hover tint, pointer, and `handler`.
    pub fn on_click(
        mut self,
        id: impl Into<ElementId>,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.id = Some(id.into());
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// A tooltip on the chip (needs an id for the hover state).
    pub fn tooltip(mut self, id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        self.id.get_or_insert(id.into());
        self.tooltip = Some(text.into());
        self
    }

    /// `sx={{ minWidth }}`.
    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = Some(width);
        self
    }

    /// `sx={{ opacity }}`.
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    /// `& .MuiChip-label { px }`.
    pub fn label_padding(mut self, padding: f32) -> Self {
        self.label_padding = Some(padding);
        self
    }

    /// The start icon at another size (`& .MuiChip-icon { fontSize }`).
    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = Some(size);
        self
    }

    /// The start icon's left margin (`& .MuiChip-icon { ml }`).
    pub fn icon_margin(mut self, margin: f32) -> Self {
        self.icon_margin = Some(margin);
        self
    }

    /// The start icon in another ink (`& .MuiChip-icon { color }`).
    pub fn icon_ink(mut self, ink: Hsla) -> Self {
        self.icon_ink = Some(ink);
        self
    }

    /// `onDelete` with `deleteIcon` (and a tooltip on it).
    pub fn on_delete(
        mut self,
        id: impl Into<ElementId>,
        icon: impl Into<SharedString>,
        tooltip: Option<SharedString>,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.id.get_or_insert(id.into());
        self.on_delete = Some((icon.into(), tooltip, Rc::new(handler)));
        self
    }

    pub fn outlined(mut self) -> Self {
        self.outlined = true;
        self
    }

    /// A palette color for the filled chip (`color="primary"` etc.).
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// `color="warning"` etc. with the palette's own contrast text (filled) or
    /// main ink (outlined).
    pub fn palette(mut self, color: crate::theme::PaletteColor) -> Self {
        self.palette = Some(color);
        self
    }

    /// A start icon (`iconSmall`: 18px, 4px in, 2px for outlined chips,
    /// tucked 4px under the label).
    pub fn icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// The label in the monospace font.
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// `sx={{ width }}`: a fixed width, label centred.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// `sx={{ height }}`.
    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    /// `sx={{ fontSize }}`.
    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = Some(size);
        self
    }
}

impl RenderOnce for Chip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let p = &theme.palette;
        let outlined_default = if theme.mode.is_dark() {
            p.grey.g700
        } else {
            p.grey.g400
        };
        let (bg, fg, border) = match (self.outlined, self.palette, self.color) {
            (false, Some(color), _) => (Some(color.main), color.contrast_text, None),
            (true, Some(color), _) => (None, color.main, Some(alpha(color.main, 0.7))),
            (false, None, None) => (Some(p.action.selected), p.text.primary, None),
            (false, None, Some(color)) => (Some(color), p.common_white, None),
            (true, None, None) => (None, p.text.primary, Some(outlined_default)),
            (true, None, Some(color)) => (None, color, Some(alpha(color, 0.7))),
        };
        // MUI paints a default chip's icon grey 700 (300 in dark mode).
        let icon_ink = if self.palette.is_none() && self.color.is_none() {
            if theme.mode.is_dark() {
                p.grey.g300
            } else {
                p.grey.g700
            }
        } else {
            fg
        };
        let size = self
            .font_size
            .unwrap_or_else(|| theme.typography.px_to_rem(13.));
        let height = self.height.unwrap_or(24.);
        // Clipped and centred, with plain (shrinkable) children: a flex-none
        // icon wrapper inside an unclipped chip collapses outlined chips in a
        // wrapping row (the route path) to the width of their padding.
        let mut chip = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .h(px(height))
            .rounded(px(16.))
            .font_family(if self.mono { fonts::MONO } else { fonts::UI })
            .text_size(px(size))
            .font_weight(FontWeight(500.))
            .text_color(fg)
            .whitespace_nowrap();
        if let Some(width) = self.width {
            chip = chip.w(px(width)).justify_center().overflow_hidden();
        }
        if let Some(bg) = bg {
            chip = chip.bg(bg);
        }
        if let Some(border) = border {
            chip = chip.border_1().border_color(border);
        }
        if let Some(name) = &self.icon {
            chip = chip.child(
                div()
                    .flex()
                    .ml(px(self.icon_margin.unwrap_or(if self.outlined {
                        2.
                    } else {
                        4.
                    })))
                    .mr(px(-4.))
                    .child(icon(
                        name,
                        self.icon_size.unwrap_or(18.),
                        self.icon_ink.unwrap_or(icon_ink),
                    )),
            );
        }
        if let Some(width) = self.min_width {
            chip = chip.min_w(px(width)).justify_center();
        }
        if self.opacity < 1. {
            chip = chip.opacity(self.opacity);
        }
        let pad = self
            .label_padding
            .unwrap_or(if self.outlined { 7. } else { 8. });
        let chip = chip.child(div().px(px(pad)).child(self.label));
        let Some(id) = self.id else {
            return chip.into_any_element();
        };
        let mut chip = chip.id(id).relative();
        if let Some((name, tip, handler)) = self.on_delete {
            // MUI: 16px (small), 26% text ink, 70% of a filled chip's contrast text.
            let ink = if !self.outlined && (self.palette.is_some() || self.color.is_some()) {
                alpha(fg, 0.7)
            } else {
                alpha(p.text.primary, 0.26)
            };
            let anchor = AnchorBounds::default();
            let mut delete = div()
                .id("chip-delete")
                .relative()
                .flex()
                .flex_none()
                .ml(px(-4.))
                .mr(px(if self.outlined { 3. } else { 5. }))
                .cursor_pointer()
                .child(icon(&name, 16., ink))
                .child(bounds_probe(&anchor))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx)
                });
            if let Some(tip) = tip {
                delete = delete.tooltip(Tooltip::anchored(tip, anchor, Placement::Bottom));
            }
            chip = chip.child(delete);
        }
        if let Some(handler) = self.on_click {
            // MUI: outlined chips take the hover tint, filled colored ones their dark tone.
            let hover = match (self.outlined, self.palette) {
                (true, _) => p.action.hover,
                (false, Some(color)) => color.dark,
                (false, None) => alpha(p.text.primary, 0.12),
            };
            chip = chip
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(move |event, window, cx| handler(event, window, cx));
        }
        if let Some(text) = self.tooltip {
            let anchor = AnchorBounds::default();
            chip = chip.child(bounds_probe(&anchor)).tooltip(Tooltip::anchored(
                text,
                anchor,
                Placement::Bottom,
            ));
        }
        chip.into_any_element()
    }
}

/// `LinearProgress` (determinate): a 4px bar with 3px corners.
pub fn linear_progress(theme: &Theme, fraction: f32) -> Div {
    let p = &theme.palette;
    div()
        .h(px(4.))
        .w(relative(1.))
        .rounded(px(3.))
        .overflow_hidden()
        .bg(alpha(p.text.secondary, 0.15))
        .child(
            div()
                .h(relative(1.))
                .w(relative(fraction.clamp(0., 1.)))
                .bg(p.primary.main),
        )
}

/// The text ink of an [`Alert`], for content passed with [`Alert::content`]
/// that sets its own color (labels and paragraphs do).
pub fn alert_ink(theme: &Theme, status: Status, variant: AlertVariant) -> Hsla {
    let p = &theme.palette;
    let color = match status {
        Status::Success => p.success,
        Status::Error => p.error,
        Status::Warning => p.warning,
        Status::Info => p.info,
    };
    match variant {
        AlertVariant::Filled => color.contrast_text,
        AlertVariant::Standard | AlertVariant::Outlined if theme.mode.is_dark() => {
            lighten(color.main, 0.6)
        }
        AlertVariant::Standard | AlertVariant::Outlined => darken(color.main, 0.6),
    }
}

/// `Alert` variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertVariant {
    Standard,
    Filled,
    /// `variant="outlined"`: no fill, a border in the status color
    /// (its light tone in light mode).
    Outlined,
}

/// MUI `Alert` (8px corners per the theme).
#[derive(IntoElement)]
pub struct Alert {
    status: Status,
    variant: AlertVariant,
    message: SharedString,
    content: Option<AnyElement>,
    action: Option<AnyElement>,
    max_width: Option<f32>,
    block_padding: f32,
    square: bool,
    icon: Option<SharedString>,
}

impl Alert {
    pub fn new(status: Status, message: impl Into<SharedString>) -> Self {
        Self {
            status,
            variant: AlertVariant::Standard,
            message: message.into(),
            content: None,
            action: None,
            max_width: None,
            block_padding: 6.,
            square: false,
            icon: None,
        }
    }

    /// `icon={<Icon />}`: another icon than the status's own.
    pub fn icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// `sx={{ borderRadius: 0 }}`: an alert across a dialog's full width.
    pub fn square(mut self) -> Self {
        self.square = true;
        self
    }

    /// Any content in the message slot instead of the text (a message with a
    /// list under it, a row with buttons). It inherits the alert's ink.
    pub fn content(mut self, content: impl IntoElement) -> Self {
        self.content = Some(content.into_any_element());
        self
    }

    /// `sx={{ py: 0 }}`: no block padding around the icon and message.
    pub fn compact(mut self) -> Self {
        self.block_padding = 0.;
        self
    }

    pub fn filled(mut self) -> Self {
        self.variant = AlertVariant::Filled;
        self
    }

    pub fn outlined(mut self) -> Self {
        self.variant = AlertVariant::Outlined;
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }

    pub fn max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self
    }
}

impl RenderOnce for Alert {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let p = &theme.palette;
        let dark = theme.mode.is_dark();
        let color = match self.status {
            Status::Success => p.success,
            Status::Error => p.error,
            Status::Warning => p.warning,
            Status::Info => p.info,
        };
        let icon_name = match self.status {
            Status::Success => "CheckCircleOutlined",
            Status::Error => "ErrorOutlineOutlined",
            Status::Warning => "ReportProblemOutlined",
            Status::Info => "InfoOutlined",
        };
        let (bg, fg, icon_ink) = match self.variant {
            AlertVariant::Standard => {
                if dark {
                    (
                        darken(color.main, 0.9),
                        lighten(color.main, 0.6),
                        color.main,
                    )
                } else {
                    (
                        lighten(color.main, 0.9),
                        darken(color.main, 0.6),
                        color.main,
                    )
                }
            }
            AlertVariant::Filled => {
                let fill = if dark { color.dark } else { color.main };
                (fill, color.contrast_text, color.contrast_text)
            }
            AlertVariant::Outlined => (
                Hsla::transparent_black(),
                alert_ink(theme, self.status, AlertVariant::Outlined),
                color.main,
            ),
        };
        let body = theme.typography.body2;
        // The outline sits inside the padding box, so the content keeps its place.
        let inset = if self.variant == AlertVariant::Outlined {
            1.
        } else {
            0.
        };
        let mut alert = div()
            .flex()
            .items_start()
            .px(px(16. - inset))
            .py(px((self.block_padding - inset).max(0.)))
            .rounded(px(if self.square { 0. } else { 8. }))
            .bg(bg)
            .text_color(fg)
            .font_family(fonts::UI)
            .text_size(px(body.size))
            .line_height(px(body.line_height))
            .font_weight(FontWeight(if self.variant == AlertVariant::Filled {
                500.
            } else {
                400.
            }))
            .child(div().flex().py(px(7.)).mr(px(12.)).child(icon(
                self.icon.as_deref().unwrap_or(icon_name),
                22.,
                icon_ink,
            )))
            .child(match self.content {
                Some(content) => div()
                    .flex()
                    .flex_col()
                    .py(px(8.))
                    .flex_1()
                    .min_w_0()
                    .child(content),
                None => div().py(px(8.)).flex_1().min_w_0().child(self.message),
            });
        if let Some(width) = self.max_width {
            alert = alert.max_w(px(width));
        }
        if self.variant == AlertVariant::Filled {
            alert = alert.shadow(elevation(6, theme));
        }
        if self.variant == AlertVariant::Outlined {
            alert = alert
                .border_1()
                .border_color(if dark { color.main } else { color.light });
        }
        if let Some(action) = self.action {
            alert = alert.child(
                div()
                    .flex()
                    .items_center()
                    .ml(px(16.))
                    .mr(px(-8.))
                    .pt(px(4.))
                    .child(action),
            );
        }
        alert
    }
}
