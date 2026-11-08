//! The themed scrollbar (`MuiCssBaseline`): 10px wide, a thumb with 8px
//! corners inset by a 2px transparent border, `scrollThumb` color
//! (`scrollThumbHover` under the pointer and while dragged).
//!
//! GPUI scroll containers draw no scrollbar of their own; [`vertical`] paints
//! the thumb for a [`ScrollHandle`] as an overlay along the container's right
//! edge, and works it as Chrome's scrollbar does: dragging the thumb scrolls
//! the content with it (the point grabbed stays under the pointer), a press
//! on the track above or below the thumb pages by 87.5% of the viewport
//! toward it. The wheel still reaches the content through the overlay.

use crate::zoom::{css, layout_px, px};
use gpui_kit::{
    App, Div, ElementId, EmptyView, EntityId, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, ScrollHandle, StatefulInteractiveElement, Styled, Window, div,
    point, relative,
};

use crate::theme::{Theme, layout::SCROLLBAR_SIZE};

/// How much of the viewport a press on the track scrolls (Chromium's
/// `kMinFractionToStepWhenPaging`).
pub const PAGE_FRACTION: f32 = 0.875;

/// The thumb for a viewport `viewport` tall over content scrolled `scrolled` of
/// at most `max` (layout px): its length and its top, or `None` when the content
/// fits. The thumb is never shorter than `min_thumb`.
pub fn thumb_geometry(
    viewport: f32,
    max: f32,
    scrolled: f32,
    min_thumb: f32,
) -> Option<(f32, f32)> {
    if max <= 0.5 || viewport <= 0. {
        return None;
    }
    let content = viewport + max;
    let ratio = (viewport / content).clamp(0.05, 1.);
    let fraction = (scrolled / max).clamp(0., 1.);
    let thumb = (viewport * ratio).max(min_thumb).min(viewport);
    Some((thumb, (viewport - thumb) * fraction))
}

/// How far the content scrolls for the thumb's top at `top` (the inverse of
/// [`thumb_geometry`]).
pub fn scrolled_for_thumb_top(viewport: f32, max: f32, thumb: f32, top: f32) -> f32 {
    let travel = viewport - thumb;
    if travel <= 0. {
        return 0.;
    }
    (top / travel).clamp(0., 1.) * max
}

/// A press on the track at `press` (from the track's top): a page toward it, or no
/// change on the thumb itself.
pub fn page_toward(viewport: f32, max: f32, scrolled: f32, thumb: (f32, f32), press: f32) -> f32 {
    let (length, top) = thumb;
    let page = viewport * PAGE_FRACTION;
    if press < top {
        (scrolled - page).max(0.)
    } else if press > top + length {
        (scrolled + page).min(max)
    } else {
        scrolled
    }
}

/// The scrollbar's state between frames: where the thumb was grabbed.
struct ScrollbarState {
    /// The pointer's distance from the thumb's top when it was pressed.
    grab: f32,
    dragging: bool,
}

/// What a thumb drag carries: which scrollbar it belongs to.
struct ThumbDrag {
    owner: EntityId,
}

/// An absolutely positioned overlay; put it inside a `relative()` parent that
/// wraps the scroll container. Nothing is drawn when the content fits.
pub fn vertical(handle: &ScrollHandle, theme: &Theme) -> VerticalScrollbar {
    VerticalScrollbar {
        id: "scrollbar".into(),
        handle: handle.clone(),
        thumb: theme.components.scroll_thumb,
        thumb_hover: theme.components.scroll_thumb_hover,
    }
}

/// See [`vertical`].
#[derive(IntoElement)]
pub struct VerticalScrollbar {
    id: ElementId,
    handle: ScrollHandle,
    thumb: gpui_kit::Hsla,
    thumb_hover: gpui_kit::Hsla,
}

impl VerticalScrollbar {
    /// An id of its own, for two scrollbars under one parent.
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = id.into();
        self
    }
}

/// The handle's geometry in layout px: (viewport, max, scrolled).
fn measure(handle: &ScrollHandle) -> (f32, f32, f32) {
    (
        f32::from(handle.bounds().size.height),
        f32::from(handle.max_offset().y),
        -f32::from(handle.offset().y),
    )
}

fn scroll_to(handle: &ScrollHandle, scrolled: f32) {
    let offset = handle.offset();
    handle.set_offset(point(offset.x, layout_px(-scrolled)));
}

impl RenderOnce for VerticalScrollbar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let overlay = div()
            .absolute()
            .top(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .w(px(SCROLLBAR_SIZE));
        let (viewport, max, scrolled) = measure(&self.handle);
        // In layout space (the viewport is measured), the minimum scaled.
        let min_thumb = css(20.);
        let Some((length, top)) = thumb_geometry(viewport, max, scrolled, min_thumb) else {
            return overlay.into_any_element();
        };
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| ScrollbarState {
            grab: 0.,
            dragging: false,
        });
        let owner = state.entity_id();
        let dragging = state.read(cx).dragging;
        let thumb_color = if dragging {
            self.thumb_hover
        } else {
            self.thumb
        };
        let hover = self.thumb_hover;
        let thumb = div()
            .id("scrollbar-thumb")
            .absolute()
            .top(layout_px(top))
            .left(px(2.))
            .w(px(SCROLLBAR_SIZE - 4.))
            .h(layout_px(length))
            .rounded(px(8.))
            .bg(thumb_color)
            .hover(move |s| s.bg(hover))
            .on_drag(ThumbDrag { owner }, |_, _, _, cx| {
                gpui_kit::AppContext::new(cx, |_| EmptyView)
            })
            .on_drag_move::<ThumbDrag>({
                let handle = self.handle.clone();
                let state = state.clone();
                move |event, window, cx| {
                    if event.drag(cx).owner != owner {
                        return;
                    }
                    let (viewport, max, scrolled) = measure(&handle);
                    let Some((length, _)) = thumb_geometry(viewport, max, scrolled, min_thumb)
                    else {
                        return;
                    };
                    let grab = state.read(cx).grab;
                    let track_top = f32::from(handle.bounds().origin.y);
                    let top = f32::from(event.event.position.y) - track_top - grab;
                    scroll_to(&handle, scrolled_for_thumb_top(viewport, max, length, top));
                    window.refresh();
                }
            });
        let press_handle = self.handle.clone();
        let press_state = state.clone();
        let release_state = state.clone();
        let release_out_state = state.clone();
        let end_drag = |state: &gpui_kit::Entity<ScrollbarState>, cx: &mut App| {
            state.update(cx, |state, cx| {
                if state.dragging {
                    state.dragging = false;
                    cx.notify();
                }
            });
        };
        overlay
            .id(self.id)
            // Presses belong to the scrollbar; the wheel still scrolls the content.
            .block_mouse_except_scroll()
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                cx.stop_propagation();
                let (viewport, max, scrolled) = measure(&press_handle);
                let Some(thumb) = thumb_geometry(viewport, max, scrolled, min_thumb) else {
                    return;
                };
                let press = f32::from(event.position.y) - f32::from(press_handle.bounds().origin.y);
                if press >= thumb.1 && press <= thumb.1 + thumb.0 {
                    press_state.update(cx, |state, cx| {
                        state.grab = press - thumb.1;
                        state.dragging = true;
                        cx.notify();
                    });
                } else {
                    scroll_to(
                        &press_handle,
                        page_toward(viewport, max, scrolled, thumb, press),
                    );
                    window.refresh();
                }
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                end_drag(&release_state, cx)
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                end_drag(&release_out_state, cx)
            })
            .child(thumb)
            .into_any_element()
    }
}

/// Keep the fraction helper available for horizontal strips too.
pub fn fraction_width(fraction: f32) -> Div {
    div().h(relative(1.)).w(relative(fraction))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumb_is_the_viewport_share_of_the_content() {
        // 200px showing 800px: a quarter of the track, at the top, then at the bottom.
        assert_eq!(thumb_geometry(200., 600., 0., 20.), Some((50., 0.)));
        assert_eq!(thumb_geometry(200., 600., 600., 20.), Some((50., 150.)));
        assert_eq!(thumb_geometry(200., 600., 300., 20.), Some((50., 75.)));
        // Content that fits has no thumb.
        assert_eq!(thumb_geometry(200., 0., 0., 20.), None);
        // A very long list keeps the minimum thumb.
        assert_eq!(thumb_geometry(200., 100_000., 0., 20.).unwrap().0, 20.);
    }

    #[test]
    fn dragging_the_thumb_maps_back_to_the_scroll_offset() {
        let (viewport, max) = (200., 600.);
        let (thumb, _) = thumb_geometry(viewport, max, 0., 20.).unwrap();
        assert_eq!(scrolled_for_thumb_top(viewport, max, thumb, 75.), 300.);
        assert_eq!(scrolled_for_thumb_top(viewport, max, thumb, -10.), 0.);
        assert_eq!(scrolled_for_thumb_top(viewport, max, thumb, 500.), 600.);
        // A thumb as long as the track cannot move.
        assert_eq!(scrolled_for_thumb_top(200., 600., 200., 50.), 0.);
        // Round trip.
        for scrolled in [0., 123., 450., 600.] {
            let (thumb, top) = thumb_geometry(viewport, max, scrolled, 20.).unwrap();
            let back = scrolled_for_thumb_top(viewport, max, thumb, top);
            assert!((back - scrolled).abs() < 1e-3, "{scrolled} → {back}");
        }
    }

    #[test]
    fn a_press_on_the_track_pages_toward_it() {
        let thumb = (50., 75.);
        assert_eq!(page_toward(200., 600., 300., thumb, 10.), 125.);
        assert_eq!(page_toward(200., 600., 300., thumb, 190.), 475.);
        assert_eq!(page_toward(200., 600., 300., thumb, 100.), 300.);
        assert_eq!(page_toward(200., 600., 100., thumb, 10.), 0.);
        assert_eq!(page_toward(200., 600., 550., thumb, 190.), 600.);
    }
}
