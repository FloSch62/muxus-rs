//! Sidebar width limits (port of `client/src/sidebar-width.ts`).

/// The search box is what sets this floor: it has to hold `user@host` without
/// eliding the placeholder that says so, and still leave room for Add host.
pub const DEFAULT_SIDEBAR_WIDTH: f64 = 288.0;
/// Host names still fit here; long addresses fall back to the row hover card.
pub const MIN_SIDEBAR_WIDTH: f64 = 200.0;
pub const MAX_SIDEBAR_WIDTH: f64 = 520.0;
pub const MIN_MAIN_WORKSPACE_WIDTH: f64 = 320.0;

/// The widest the sidebar may get in a window `container_width` wide.
pub fn max_sidebar_width(container_width: f64) -> f64 {
    MIN_SIDEBAR_WIDTH.max(
        MAX_SIDEBAR_WIDTH
            .min(container_width * 0.45)
            .min(container_width - MIN_MAIN_WORKSPACE_WIDTH)
            .floor(),
    )
}

/// A dragged or typed width, rounded and kept within the limits.
pub fn clamp_sidebar_width(width: f64, container_width: f64) -> f64 {
    MIN_SIDEBAR_WIDTH.max(max_sidebar_width(container_width).min(js_round(width)))
}

/// `Math.round`: halves round up, towards positive infinity.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_room_for_the_main_workspace_and_respects_its_fixed_maximum() {
        assert_eq!(max_sidebar_width(1_600.0), 520.0);
        assert_eq!(max_sidebar_width(900.0), 405.0);
        assert_eq!(max_sidebar_width(600.0), 270.0);
        assert_eq!(max_sidebar_width(400.0), MIN_SIDEBAR_WIDTH);
    }

    #[test]
    fn rounds_and_clamps_dragged_widths() {
        assert_eq!(clamp_sidebar_width(100.0, 1_200.0), MIN_SIDEBAR_WIDTH);
        assert_eq!(clamp_sidebar_width(341.6, 1_200.0), 342.0);
        assert_eq!(clamp_sidebar_width(800.0, 1_200.0), 520.0);
        assert_eq!(clamp_sidebar_width(300.5, 1_200.0), 301.0);
    }
}
