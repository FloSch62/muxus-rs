//! The terminal's scrollbar as xterm.js 6 builds it: VS Code's scrollable element
//! (`ScrollbarState`, `AbstractScrollbar`) over the viewport, without arrows. It differs from the
//! kit's scrollbar (`muxus_ui::widgets::scrollbar`, Chrome's): the slider is at least 20px and
//! sized from the floored share of the content, and a press on the track jumps the slider's
//! middle to the pointer and goes on as a drag, instead of paging. Adapted from VS Code's
//! `scrollbarState.ts` and `abstractScrollbar.ts` as bundled in xterm.js (MIT, © Microsoft
//! Corporation).

/// VS Code's `MINIMUM_SLIDER_SIZE`, in CSS px.
pub(crate) const MINIMUM_SLIDER_SIZE: f32 = 20.0;

/// How far the pointer may stray sideways from a dragged slider on Windows before the drag
/// snaps back (`POINTER_DRAG_RESET_DISTANCE`), in CSS px.
pub(crate) const POINTER_DRAG_RESET_DISTANCE: f32 = 140.0;

/// The slider for a viewport `visible` tall over content `scroll_size` tall, scrolled to
/// `position` (all in px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScrollbarState {
    pub needed: bool,
    pub slider_size: f32,
    pub slider_ratio: f32,
    pub slider_position: f32,
}

impl ScrollbarState {
    /// `ScrollbarState._computeValues` with no arrows and no opposite scrollbar; `minimum` is
    /// [`MINIMUM_SLIDER_SIZE`] in the caller's units.
    pub fn new(visible: f32, scroll_size: f32, position: f32, minimum: f32) -> Self {
        let representable = visible.max(0.0);
        let needed = scroll_size > 0.0 && scroll_size > visible;
        if !needed {
            return Self {
                needed,
                slider_size: representable.round(),
                slider_ratio: 0.0,
                slider_position: 0.0,
            };
        }
        let slider_size = minimum
            .max((visible * representable / scroll_size).floor())
            .round();
        let slider_ratio = (representable - slider_size) / (scroll_size - visible);
        Self {
            needed,
            slider_size,
            slider_ratio,
            slider_position: (position * slider_ratio).round(),
        }
    }

    /// Whether a press `offset` px below the track's top is on the slider.
    pub fn on_slider(&self, offset: f32) -> bool {
        self.needed
            && offset >= self.slider_position
            && offset <= self.slider_position + self.slider_size
    }

    /// Where the content scrolls for a press on the track `offset` px below its top: the
    /// slider's middle under the pointer (`getDesiredScrollPositionFromOffset`).
    pub fn position_for_offset(&self, offset: f32) -> f32 {
        if !self.needed || self.slider_ratio <= 0.0 {
            return 0.0;
        }
        ((offset - self.slider_size / 2.0) / self.slider_ratio).round()
    }

    /// Where the content scrolls when the slider is dragged `delta` px from where it was
    /// grabbed (`getDesiredScrollPositionFromDelta`).
    pub fn position_for_delta(&self, delta: f32) -> f32 {
        if !self.needed || self.slider_ratio <= 0.0 {
            return 0.0;
        }
        ((self.slider_position + delta) / self.slider_ratio).round()
    }
}

/// The viewport's top line for a scroll position: the scrollable clamps it to the content,
/// and xterm's viewport rounds it to a whole row (`Viewport._handleScroll`).
pub(crate) fn line_for_position(
    position: f32,
    visible: f32,
    scroll_size: f32,
    row_height: f32,
) -> usize {
    if row_height <= 0.0 {
        return 0;
    }
    let max = (scroll_size - visible).max(0.0);
    (position.clamp(0.0, max) / row_height).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_the_slider_from_the_floored_share_with_a_minimum() {
        // 24 rows of 17px over 124 lines.
        let state = ScrollbarState::new(408.0, 2108.0, 0.0, 20.0);
        assert!(state.needed);
        assert_eq!(state.slider_size, 78.0);
        assert_eq!(state.slider_position, 0.0);
        // At the bottom the slider reaches the track's end.
        let bottom = ScrollbarState::new(408.0, 2108.0, 1700.0, 20.0);
        assert_eq!(bottom.slider_position + bottom.slider_size, 408.0);
        // A long scrollback keeps the minimum.
        let long = ScrollbarState::new(408.0, 170_408.0, 0.0, 20.0);
        assert_eq!(long.slider_size, 20.0);
        // Content that fits needs none.
        assert!(!ScrollbarState::new(408.0, 408.0, 0.0, 20.0).needed);
    }

    #[test]
    fn a_press_on_the_track_centres_the_slider_on_the_pointer() {
        let state = ScrollbarState::new(400.0, 2000.0, 0.0, 20.0);
        // Slider 80px, ratio 320 / 1600 = 0.2.
        assert_eq!(state.slider_size, 80.0);
        assert_eq!(state.position_for_offset(240.0), 1000.0);
        assert!(state.on_slider(10.0));
        assert!(!state.on_slider(240.0));
        // Past the ends the scrollable clamps it.
        assert_eq!(state.position_for_offset(0.0), -200.0);
        assert_eq!(line_for_position(-200.0, 400.0, 2000.0, 20.0), 0);
        assert_eq!(line_for_position(5000.0, 400.0, 2000.0, 20.0), 80);
        assert_eq!(line_for_position(1009.0, 400.0, 2000.0, 20.0), 50);
    }

    #[test]
    fn a_drag_moves_the_content_by_the_slider_distance() {
        let state = ScrollbarState::new(400.0, 2000.0, 500.0, 20.0);
        assert_eq!(state.slider_position, 100.0);
        assert_eq!(state.position_for_delta(0.0), 500.0);
        assert_eq!(state.position_for_delta(20.0), 600.0);
        assert_eq!(state.position_for_delta(-100.0), 0.0);
    }
}
