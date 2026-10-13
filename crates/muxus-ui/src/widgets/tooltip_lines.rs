//! Tooltips beyond one line of text, in the `MuiTooltip` look: several
//! plain lines then a monospace one (a certificate's subject, issuer and
//! fingerprint), or one block of monospace text kept as written (a login
//! banner). Also [`TooltipExt`], a plain tooltip on any element with an id.

use crate::zoom::{css, layout_px, px};
use gpui_kit::{
    AnyView, App, AppContext, Bounds, Context, Div, FontWeight, IntoElement, ParentElement, Pixels,
    Point, Render, SharedString, Stateful, StatefulInteractiveElement, Styled, Window, div,
};

use crate::icon::icon;
use crate::theme::{ActiveTheme, fonts};
use crate::widgets::tooltip::{AnchorBounds, Placement, Tooltip, bounds_probe};

const GAP: f32 = 14.;
const ARROW_W: f32 = 12.;
const ARROW_H: f32 = 6.;
const SLOT: f32 = 900.;

/// A tooltip of several lines, centred under the control that recorded `anchor`.
pub struct LinesTooltip {
    lines: Vec<SharedString>,
    mono: Option<SharedString>,
    mono_size: f32,
    max_width: Option<f32>,
    anchor: Option<Bounds<Pixels>>,
    pointer: Point<Pixels>,
}

impl LinesTooltip {
    /// Plain lines, then an optional line in the mono font (a fingerprint);
    /// MUI's 300px width cap.
    pub fn builder(
        lines: Vec<String>,
        mono: Option<String>,
        anchor: AnchorBounds,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        Self::build(lines, mono, 10.5, Some(300.), anchor)
    }

    /// One block of mono text kept as written (a login banner); no width cap.
    pub fn pre(
        text: String,
        anchor: AnchorBounds,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        Self::build(Vec::new(), Some(text), 10.5, None, anchor)
    }

    fn build(
        lines: Vec<String>,
        mono: Option<String>,
        mono_size: f32,
        max_width: Option<f32>,
        anchor: AnchorBounds,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let lines: Vec<SharedString> = lines.into_iter().map(Into::into).collect();
        let mono: Option<SharedString> = mono.map(Into::into);
        move |window, cx| {
            let pointer = window.mouse_position();
            let anchor = anchor.get();
            let (lines, mono) = (lines.clone(), mono.clone());
            cx.new(|_| LinesTooltip {
                lines,
                mono,
                mono_size,
                max_width,
                anchor,
                pointer,
            })
            .into()
        }
    }
}

impl Render for LinesTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bg = theme.components.tooltip_bg;
        let mut bubble = div()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(6.))
            .bg(bg)
            .text_color(theme.palette.common_white)
            .font_family(fonts::UI)
            .text_size(px(12.))
            .font_weight(FontWeight(500.))
            .line_height(px(18.))
            .flex()
            .flex_col();
        if let Some(width) = self.max_width {
            bubble = bubble.max_w(px(width));
        }
        bubble = bubble.children(self.lines.iter().map(|line| div().child(line.clone())));
        if let Some(mono) = &self.mono {
            let mut block = div()
                .font_family(fonts::MONO)
                .text_size(px(self.mono_size))
                .line_height(px(self.mono_size * 1.5));
            if !self.lines.is_empty() {
                block = block.mt(px(4.));
            }
            if self.max_width.is_none() {
                block = block.whitespace_nowrap();
                for line in mono.split('\n') {
                    block = block.child(div().child(SharedString::from(line.to_owned())));
                }
            } else {
                block = block.child(mono.clone());
            }
            bubble = bubble.child(block);
        }
        let Some(anchor) = self.anchor else {
            return div().mt(px(GAP)).child(bubble);
        };
        // GPUI puts this view's origin 1px right of and below the pointer.
        let origin = self.pointer + gpui_kit::point(layout_px(1.), layout_px(1.));
        let center_x = f32::from(anchor.center().x - origin.x);
        div().size(px(0.)).child(
            div()
                .absolute()
                .left(layout_px(center_x - css(SLOT) / 2.))
                .top(layout_px(
                    f32::from(anchor.bottom() - origin.y) + css(GAP - ARROW_H),
                ))
                .w(px(SLOT))
                .flex()
                .flex_col()
                .items_center()
                .child(icon("TooltipArrow", ARROW_W, bg).h(px(ARROW_H)))
                .child(bubble),
        )
    }
}

/// A plain `Tooltip` under any element with an id, centred on it as MUI places it.
pub trait TooltipExt: Sized {
    fn mui_tooltip(self, text: impl Into<SharedString>) -> Self;
}

impl TooltipExt for Stateful<Div> {
    fn mui_tooltip(self, text: impl Into<SharedString>) -> Self {
        let text: SharedString = text.into();
        if text.is_empty() {
            return self;
        }
        let anchor = AnchorBounds::default();
        self.child(bounds_probe(&anchor)).tooltip(Tooltip::anchored(
            text,
            anchor,
            Placement::Bottom,
        ))
    }
}
