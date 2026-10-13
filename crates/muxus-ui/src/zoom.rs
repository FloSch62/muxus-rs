//! Interface scale (`applyInterfaceZoom`): the preference that scales the whole window the
//! way Electron's zoom factor scales the page, terminal text included.
//!
//! GPUI has no page zoom, and every Muxus size is written in CSS pixels through [`px`]. So
//! `px` itself carries the scale: the [`crate::gpui_kit`] every crate builds its UI with
//! hands out this `px`, which multiplies by the current zoom, and the rem size the component
//! library measures in follows it ([`apply_to_window`]). At 100% nothing changes (a
//! multiplication by 1.0 is exact).
//!
//! Lengths measured from the laid-out window are already scaled. Turning one back into a
//! CSS length (a dragged sidebar width the preferences keep) goes through [`unzoom`], and a
//! length that must not scale (the window's own size, as Electron's zoom leaves the window
//! alone) through [`window_px`].

use std::sync::atomic::{AtomicU32, Ordering};

use ::gpui_kit::{Pixels, Window};

/// Electron's limits (`MIN_INTERFACE_ZOOM`, `MAX_INTERFACE_ZOOM`).
pub const MIN_ZOOM: f32 = 0.5;
pub const MAX_ZOOM: f32 = 2.0;

/// The browser's default root font size, which the component library's rems are based on.
const BASE_REM: f32 = 16.0;

static ZOOM: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0

/// The interface scale in effect.
pub fn zoom() -> f32 {
    f32::from_bits(ZOOM.load(Ordering::Relaxed))
}

/// Set the interface scale (clamped; anything not finite is 1). Returns whether it changed;
/// the caller redraws its windows ([`apply_to_window`] in each).
pub fn set_zoom(zoom: f32) -> bool {
    let zoom = if zoom.is_finite() {
        zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    };
    ZOOM.swap(zoom.to_bits(), Ordering::Relaxed) != zoom.to_bits()
}

/// A CSS length at the current interface scale.
pub fn px(value: f32) -> Pixels {
    ::gpui_kit::px(value * zoom())
}

/// A length that does not scale with the interface: the window's own geometry.
pub fn window_px(value: f32) -> Pixels {
    ::gpui_kit::px(value)
}

/// A length in layout space (measured from the laid-out window, a pointer position, a
/// glyph offset) as `Pixels`: no further scaling.
pub fn layout_px(value: f32) -> Pixels {
    ::gpui_kit::px(value)
}

/// A CSS length in layout space, as a number to combine with measured ones.
pub fn css(value: f32) -> f32 {
    value * zoom()
}

/// A length measured from the scaled layout, back in CSS pixels (what [`px`] took).
pub fn unzoom(measured: Pixels) -> f32 {
    f32::from(measured) / zoom()
}

/// Scale the component library's rems with the interface. Call from each window's root
/// render.
pub fn apply_to_window(window: &mut Window) {
    let rem = ::gpui_kit::px(BASE_REM * zoom());
    if window.rem_size() != rem {
        window.set_rem_size(rem);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_css_lengths_and_measures_them_back() {
        assert_eq!(px(13.), ::gpui_kit::px(13.));
        set_zoom(1.25);
        assert_eq!(px(16.), ::gpui_kit::px(20.));
        assert_eq!(unzoom(::gpui_kit::px(20.)), 16.);
        assert_eq!(window_px(800.), ::gpui_kit::px(800.));
        assert!(!set_zoom(1.25));
        set_zoom(9.0);
        assert_eq!(zoom(), MAX_ZOOM);
        set_zoom(f32::NAN);
        assert_eq!(zoom(), 1.0);
    }
}
