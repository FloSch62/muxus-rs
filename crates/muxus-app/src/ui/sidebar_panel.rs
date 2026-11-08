//! `client/src/layout/Sidebar.tsx` and `components/PanelResizeHandle.tsx`:
//! the sidebar panel (fixed width, chrome background, divider toward the
//! panes) with its resize grip.

use muxus_app::sidebar_width::{DEFAULT_SIDEBAR_WIDTH, clamp_sidebar_width};
use muxus_ui::gpui_kit::{
    AnyElement, Context, CursorStyle, ElementId, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, SharedString, Styled, Window, div, px,
};
use muxus_ui::theme::ActiveTheme;
use muxus_ui::widgets::focus::{focus_visible, keyed_focus};

use crate::ui::prefs::Side;
use crate::ui::shell::{AppShell, ShellMenu, SidebarDrag};

/// How far an arrow key moves the grip.
const KEYBOARD_STEP: f64 = 20.;

/// The width a key on the grip asks for: arrows move the edge facing the panes by
/// 20px (toward the panes widens), Home goes back to the default. `None` for any
/// other key.
pub fn keyboard_width(key: &str, width: f64, on_right: bool) -> Option<f64> {
    // The grip sits on the panel's right edge when the sidebar is on the left.
    let direction = if on_right { -1. } else { 1. };
    match key {
        "left" => Some(width - KEYBOARD_STEP * direction),
        "right" => Some(width + KEYBOARD_STEP * direction),
        "home" => Some(DEFAULT_SIDEBAR_WIDTH),
        _ => None,
    }
}

impl AppShell {
    pub fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let on_right = self.prefs.sidebar_position == Side::Right;
        let container = muxus_ui::zoom::unzoom(window.viewport_size().width);
        // `maxWidth: 45%` of the row, and the shared clamp for everything else.
        let width = (clamp_sidebar_width(self.prefs.sidebar_width, container as f64) as f32)
            .min(container * 0.45);
        let dragging = self.sidebar_drag.is_some();
        let group: SharedString = "sidebar-resize".into();
        // The host whose open-sessions menu shows stays marked in the tree.
        let sessions_key = match &self.menu {
            Some(ShellMenu::HostSessions { host, .. }) => Some(host.key()),
            _ => None,
        };
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_sessions_menu_key(sessions_key, cx)
        });
        // An `<hr tabIndex=0>`: focused, the arrow keys resize the panel.
        let id = ElementId::Name("sidebar-resize".into());
        let focus = keyed_focus(&id, window, cx);
        let keyboard = focus_visible(&focus, window);

        let grip = div()
            .id(id)
            .track_focus(&focus)
            .group(group.clone())
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(8.))
            .cursor(CursorStyle::ResizeLeftRight)
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                let modifiers = event.keystroke.modifiers;
                if modifiers.control || modifiers.alt || modifiers.platform {
                    return;
                }
                let Some(next) =
                    keyboard_width(&event.keystroke.key, this.prefs.sidebar_width, on_right)
                else {
                    return;
                };
                cx.stop_propagation();
                this.prefs.sidebar_width = clamp_sidebar_width(next, container as f64);
                this.prefs_changed(cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let focus = focus.clone();
                    move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        window.focus(&focus, cx);
                        if event.click_count == 2 {
                            this.sidebar_drag = None;
                            this.prefs.sidebar_width = DEFAULT_SIDEBAR_WIDTH;
                            this.prefs_changed(cx);
                        } else {
                            this.sidebar_drag = Some(SidebarDrag {
                                start_x: muxus_ui::zoom::unzoom(event.position.x),
                                start_width: width,
                            });
                        }
                        cx.notify();
                    }
                }),
            )
            .child({
                let line = div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(2.5))
                    .w(px(3.))
                    .bg(p.primary.main);
                // Shown on hover, while dragging and with keyboard focus (`:focus-visible`).
                if dragging || keyboard {
                    line
                } else {
                    line.invisible().group_hover(group, |s| s.visible())
                }
            });
        let grip = if on_right {
            grip.left(px(-4.))
        } else {
            grip.right(px(-4.))
        };

        let (rail, content) = self.render_sidebar_views(cx);
        let mut panel = div()
            .id("sidebar-panel")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .h_full()
            .w(px(width))
            .bg(p.sidebar)
            .border_color(p.divider)
            .child(content);
        panel = if on_right {
            panel.border_l_1()
        } else {
            panel.border_r_1()
        };
        let panel = panel.child(grip);
        // The rail keeps to the window edge, outside the resizable width.
        match rail {
            None => panel.into_any_element(),
            Some(rail) => {
                let row = div().flex().flex_none().h_full();
                if on_right {
                    row.child(panel).child(rail).into_any_element()
                } else {
                    row.child(rail).child(panel).into_any_element()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_move_the_edge_that_faces_the_panes() {
        // Sidebar on the left: the grip is its right edge.
        assert_eq!(keyboard_width("right", 300., false), Some(320.));
        assert_eq!(keyboard_width("left", 300., false), Some(280.));
        // Sidebar on the right: the grip is its left edge.
        assert_eq!(keyboard_width("right", 300., true), Some(280.));
        assert_eq!(keyboard_width("left", 300., true), Some(320.));
        assert_eq!(
            keyboard_width("home", 412., true),
            Some(DEFAULT_SIDEBAR_WIDTH)
        );
        assert_eq!(keyboard_width("up", 300., false), None);
    }
}
