//! The themed scrollbar (`MuiCssBaseline`): 10px wide, a thumb with 8px
//! corners inset by a 2px transparent border, `scrollThumb` color.
//!
//! GPUI scroll containers draw no scrollbar of their own; [`vertical`] paints
//! the thumb for a [`ScrollHandle`] as an overlay along the container's right
//! edge. It is display-only (wheel and keyboard scroll the content).

use crate::zoom::{css, layout_px, px};
use gpui_kit::{Div, ParentElement, ScrollHandle, Styled, div, relative};

use crate::theme::{Theme, layout::SCROLLBAR_SIZE};

/// An absolutely positioned overlay; put it inside a `relative()` parent that
/// wraps the scroll container. Nothing is drawn when the content fits.
pub fn vertical(handle: &ScrollHandle, theme: &Theme) -> Div {
    let bounds = handle.bounds();
    let max = handle.max_offset().y;
    let viewport = bounds.size.height;
    let overlay = div()
        .absolute()
        .top(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .w(px(SCROLLBAR_SIZE));
    if max <= px(0.5) || viewport <= px(0.) {
        return overlay;
    }
    let content = viewport + max;
    let ratio = (viewport / content).clamp(0.05, 1.);
    let scrolled = (-handle.offset().y / max).clamp(0., 1.);
    // In layout space (the viewport is measured), the minimum scaled.
    let thumb = (f32::from(viewport) * ratio).max(css(20.));
    let top = (f32::from(viewport) - thumb) * scrolled;
    overlay.child(
        div()
            .absolute()
            .top(layout_px(top))
            .left(px(2.))
            .w(px(SCROLLBAR_SIZE - 4.))
            .h(layout_px(thumb))
            .rounded(px(8.))
            .bg(theme.components.scroll_thumb),
    )
}

/// Keep the fraction helper available for horizontal strips too.
pub fn fraction_width(fraction: f32) -> Div {
    div().h(relative(1.)).w(relative(fraction))
}
