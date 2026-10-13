//! `client/src/layout/Sidebar.tsx` and `components/PanelResizeHandle.tsx`:
//! the sidebar panel (fixed width, chrome background, divider toward the
//! panes) with its resize grip.

use muxus_app::sidebar_width::{DEFAULT_SIDEBAR_WIDTH, clamp_sidebar_width};
use muxus_ui::gpui_kit::{
    AnyElement, Context, CursorStyle, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, SharedString, Styled, Window, div, px,
};
use muxus_ui::theme::ActiveTheme;

use crate::ui::prefs::Side;
use crate::ui::shell::{AppShell, SidebarDrag};

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

        let grip = div()
            .id("sidebar-resize")
            .group(group.clone())
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(8.))
            .cursor(CursorStyle::ResizeLeftRight)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
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
                if dragging {
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
