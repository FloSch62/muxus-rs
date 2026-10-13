//! Client-side window decorations, as the frameless Electron windows have them
//! on Linux, for every Muxus window (the main window, settings):
//!
//! - [`title_bar`] makes a bar the title bar: a press drags the window, a
//!   double press maximizes or restores it. Controls on the bar claim their
//!   own presses (`prevent_default`) and are left alone.
//! - [`window_controls`] are minimize, maximize/restore and close for the
//!   right end of that bar, in the bar's own colors (`titleBarColors`).
//! - [`decorate_window`] wraps the window's content: a floating window gets
//!   Chromium's frame (a soft shadow in a transparent margin, rounded top
//!   corners, no border line) and resize edges; maximized and fullscreen
//!   windows fill the screen edge to edge.
//! - [`window_corner_radius`] is the radius the title bar rounds its own fill
//!   with.
//!
//! Fullscreen windows show no controls, which is also how the docs
//! screenshots (a browser viewport) look.

use std::collections::HashMap;
use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, App, BoxShadow, CursorStyle, Decorations, Div, ElementId, Hsla, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, ParentElement, ResizeEdge, SharedString, Size,
    Stateful, StatefulInteractiveElement, Styled, TitlebarOptions, Window, WindowBounds,
    WindowDecorations, WindowOptions, div, point,
};
use gpui_kit::{Global, WindowId};

/// The options of every Muxus window (`appWindowOptions` in `electron/src/main.ts`):
/// frameless, with the app's own title bar, frame and controls, the traffic lights where the
/// top bar leaves room for them on macOS, and the desktop file's app id.
///
/// A window opened with the platform's default decorations gets none at all under GNOME on
/// Wayland (it offers no server-side decorations), so it could not even be closed: every
/// window goes through here and draws [`window_controls`] in its title bar.
pub fn app_window_options(
    title: impl Into<SharedString>,
    bounds: WindowBounds,
    min_size: Size<gpui_kit::Pixels>,
    app_id: &str,
) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(bounds),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(16.), px(18.))),
        }),
        window_decorations: Some(WindowDecorations::Client),
        window_min_size: Some(min_size),
        app_id: Some(app_id.into()),
        // [`title_bar`] moves the window itself; AppKit must not drag it too.
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

/// Room the title bar leaves at its left end for the macOS traffic lights (placed at
/// `traffic_light_position`, 16px in), as `TopBar.tsx` pads for them; `None` elsewhere and
/// in fullscreen, where macOS hides them.
pub fn traffic_light_inset(window: &Window) -> Option<f32> {
    (cfg!(target_os = "macos") && !window.is_fullscreen()).then_some(80.)
}

use crate::color::alpha;
use crate::icon::icon;
use crate::theme::ActiveTheme;

/// How far inside the window edge a press starts a resize.
const RESIZE_EDGE: f32 = 5.;
/// The transparent margin a floating window keeps around its frame for the
/// shadow, as Chromium's client-side frame on Wayland does. It stays set while
/// the window is maximized (the compositor reports every edge tiled then and
/// the margin collapses); changing it between states would make the first
/// resize after a restore count it twice.
const FRAME_INSET: f32 = 10.;
/// Chromium rounds a floating frameless window's top corners (8px, measured
/// from Electron Muxus in the same GNOME session). Bottom corners stay square.
const FRAME_RADIUS: f32 = 8.;

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{Bounds, size};

    #[test]
    fn app_windows_draw_their_own_frame() {
        let options = app_window_options(
            "Settings — Muxus",
            WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size(px(800.), px(600.)))),
            size(px(560.), px(420.)),
            "muxus",
        );
        assert_eq!(options.window_decorations, Some(WindowDecorations::Client));
        let titlebar = options.titlebar.expect("a title");
        assert!(titlebar.appears_transparent);
        assert_eq!(titlebar.title.as_deref(), Some("Settings — Muxus"));
        assert_eq!(options.app_id.as_deref(), Some("muxus"));
        assert_eq!(options.window_min_size, Some(size(px(560.), px(420.))));
    }
}

type CloseHook = Rc<dyn Fn(&mut Window, &mut App) -> bool>;

/// What each window runs before it closes, whichever way it closes.
#[derive(Default)]
struct CloseHooks(HashMap<WindowId, Vec<CloseHook>>);

impl Global for CloseHooks {}

/// Run `hook` before the window closes, by the desktop's close (the title bar of a
/// server-decorated window, Alt+F4) or by [`window_controls`]' close button; `false` keeps
/// the window open (a question about unsaved edits). GPUI keeps one close callback per
/// window and its own close does not run it, so every hook of a window goes through here.
pub fn on_close_request(
    window: &Window,
    cx: &mut App,
    hook: impl Fn(&mut Window, &mut App) -> bool + 'static,
) {
    let id = window.window_handle().window_id();
    let hooks = &mut cx.default_global::<CloseHooks>().0;
    let first = !hooks.contains_key(&id);
    hooks.entry(id).or_default().push(Rc::new(hook));
    if first {
        window.on_window_should_close(cx, move |window, cx| close_hooks_allow(id, window, cx));
    }
}

/// Every hook of the window, in order; the first that says no keeps the window.
fn close_hooks_allow(id: WindowId, window: &mut Window, cx: &mut App) -> bool {
    let hooks = cx
        .try_global::<CloseHooks>()
        .and_then(|hooks| hooks.0.get(&id).cloned())
        .unwrap_or_default();
    let allowed = hooks.iter().all(|hook| hook(window, cx));
    if allowed && cx.has_global::<CloseHooks>() {
        cx.global_mut::<CloseHooks>().0.remove(&id);
    }
    allowed
}

/// Close the window as the desktop's close does: its hooks first.
pub fn request_close(window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    if close_hooks_allow(id, window, cx) {
        window.remove_window();
    }
}

/// The app draws the window's frame (client decorations, not fullscreen).
pub fn client_decorated(window: &Window) -> bool {
    matches!(window.window_decorations(), Decorations::Client { .. }) && !window.is_fullscreen()
}

/// The title bar carries [`window_controls`]: on a client-decorated Linux window, and on
/// Windows, where GPUI reports server decorations but hides the native title bar (and its
/// buttons) for a transparent title bar. macOS keeps its traffic lights.
pub fn draws_window_controls(window: &Window) -> bool {
    if cfg!(target_os = "windows") {
        !window.is_fullscreen()
    } else {
        client_decorated(window)
    }
}

/// The radius of the window's top corners: rounded while it floats, square
/// when maximized, tiled to the top or fullscreen.
pub fn window_corner_radius(window: &Window) -> f32 {
    match window.window_decorations() {
        Decorations::Client { tiling } if !tiling.top && !window.is_fullscreen() => FRAME_RADIUS,
        _ => 0.,
    }
}

/// Make `bar` the window's title bar: a press drags the window, a double
/// press maximizes or restores it.
///
/// On Windows the bar is a caption area instead (`start_window_move` does nothing there):
/// Windows drags, double-click maximizes and snaps it, unless the app claims the press,
/// which is how the controls on the bar keep theirs.
pub fn title_bar<E: InteractiveElement>(bar: E) -> E {
    #[cfg(target_os = "windows")]
    let bar = bar.window_control_area(gpui_kit::WindowControlArea::Drag);
    bar.on_mouse_down(
        MouseButton::Left,
        |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
            // Controls on the bar claim their own presses (prevent_default).
            if window.default_prevented() {
                cx.stop_propagation();
                return;
            }
            if cfg!(target_os = "windows") {
                return;
            }
            if event.click_count == 2 {
                if cfg!(target_os = "macos") {
                    // Zooms or minimizes, as the user's Dock setting says.
                    window.titlebar_double_click();
                } else {
                    window.zoom_window();
                }
            } else {
                window.start_window_move();
            }
            cx.stop_propagation();
        },
    )
}

/// Minimize, maximize/restore and close, for the right end of the title bar;
/// `None` when the platform draws them ([`draws_window_controls`]).
///
/// Measured from the Electron build in the same headless GNOME session: 24px
/// circles 13px apart, 10px from the window edge, tinted ~9% toward the
/// symbol color, with 8px glyphs drawn in the symbol color (the bar's ink,
/// `titleBarColors`). The overlay sits 16px past the last toolbar button, as
/// the toolbar's padding reserves it.
pub fn window_controls(window: &Window, cx: &App) -> Option<AnyElement> {
    if !draws_window_controls(window) {
        return None;
    }
    let theme = cx.theme();
    let (_, symbol) = theme.title_bar_colors(0.);
    let dark = theme.mode.is_dark();
    let tint = |amount: f32| {
        if dark {
            alpha(theme.palette.common_white, amount)
        } else {
            alpha(theme.palette.common_black, amount)
        }
    };
    let (rest, hover, pressed) = (
        tint(if dark { 0.09 } else { 0.086 }),
        tint(0.15),
        tint(0.22),
    );
    let maximized = window.is_maximized();
    let button = |id: &'static str, glyph: &'static str| -> Stateful<Div> {
        div()
            .id(ElementId::Name(id.into()))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(24.))
            .rounded(px(12.))
            .bg(rest)
            .hover(move |s| s.bg(hover))
            .active(move |s| s.bg(pressed))
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .child(icon(glyph, 8., symbol))
    };
    Some(
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(13.))
            .pl(px(5.7))
            .pr(px(10.))
            .child(
                button("window-minimize", "WindowMinimize")
                    .on_click(|_, window, _| window.minimize_window()),
            )
            .child(
                button(
                    "window-maximize",
                    if maximized {
                        "WindowRestore"
                    } else {
                        "WindowMaximize"
                    },
                )
                .on_click(|_, window, _| window.zoom_window()),
            )
            .child(
                button("window-close", "WindowClose")
                    .on_click(|_, window, cx| request_close(window, cx)),
            )
            .into_any_element(),
    )
}

/// Wrap the window's content in its frame when the app decorates the window
/// itself: a soft shadow in a transparent margin, rounded top corners and
/// resize edges. Electron's frameless window shows no border line, in either
/// theme. Call it from the window's root view on every render.
pub fn decorate_window(content: Stateful<Div>, window: &mut Window, cx: &App) -> AnyElement {
    let Decorations::Client { tiling } = window.window_decorations() else {
        window.set_client_inset(px(0.));
        return content.into_any_element();
    };
    window.set_client_inset(px(FRAME_INSET));
    let floating = !(tiling.top && tiling.bottom && tiling.left && tiling.right);
    if !floating || window.is_fullscreen() {
        return content.into_any_element();
    }
    let radius = window_corner_radius(window);
    let shadow = cx.theme().components.window_shadow;
    // An inactive window's shadow is lighter, as Chromium draws it.
    let shade = if window.is_window_active() {
        shadow
    } else {
        alpha(shadow, shadow.a * 0.7)
    };
    let frame = content
        .when(radius > 0., |frame| frame.rounded_t(px(radius)))
        .shadow(vec![
            BoxShadow {
                color: shade,
                offset: point(px(0.), px(2.)),
                blur_radius: px(FRAME_INSET),
                spread_radius: px(-1.),
                inset: false,
            },
            BoxShadow {
                color: shade,
                offset: point(px(0.), px(1.)),
                blur_radius: px(3.),
                spread_radius: px(0.),
                inset: false,
            },
        ]);
    let inset = px(FRAME_INSET);
    let mut backdrop = div()
        .id("window-backdrop")
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .bg(Hsla::transparent_black())
        .when(!tiling.top, |d| d.pt(inset))
        .when(!tiling.bottom, |d| d.pb(inset))
        .when(!tiling.left, |d| d.pl(inset))
        .when(!tiling.right, |d| d.pr(inset))
        .child(frame);

    // Resize from the margin and a few pixels into the frame.
    let reach = px(FRAME_INSET + RESIZE_EDGE);
    let edge = |id: &'static str, resize: ResizeEdge, cursor: CursorStyle| {
        div()
            .id(ElementId::Name(id.into()))
            .absolute()
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.start_window_resize(resize);
                cx.stop_propagation();
            })
    };
    let (top, bottom, left, right) = (!tiling.top, !tiling.bottom, !tiling.left, !tiling.right);
    if top {
        backdrop = backdrop.child(
            edge("resize-top", ResizeEdge::Top, CursorStyle::ResizeUpDown)
                .top_0()
                .left(reach)
                .right(reach)
                .h(reach),
        );
    }
    if bottom {
        backdrop = backdrop.child(
            edge(
                "resize-bottom",
                ResizeEdge::Bottom,
                CursorStyle::ResizeUpDown,
            )
            .bottom_0()
            .left(reach)
            .right(reach)
            .h(reach),
        );
    }
    if left {
        backdrop = backdrop.child(
            edge(
                "resize-left",
                ResizeEdge::Left,
                CursorStyle::ResizeLeftRight,
            )
            .left_0()
            .top(reach)
            .bottom(reach)
            .w(reach),
        );
    }
    if right {
        backdrop = backdrop.child(
            edge(
                "resize-right",
                ResizeEdge::Right,
                CursorStyle::ResizeLeftRight,
            )
            .right_0()
            .top(reach)
            .bottom(reach)
            .w(reach),
        );
    }
    let corners = [
        (
            top && left,
            "resize-tl",
            ResizeEdge::TopLeft,
            CursorStyle::ResizeUpLeftDownRight,
        ),
        (
            top && right,
            "resize-tr",
            ResizeEdge::TopRight,
            CursorStyle::ResizeUpRightDownLeft,
        ),
        (
            bottom && left,
            "resize-bl",
            ResizeEdge::BottomLeft,
            CursorStyle::ResizeUpRightDownLeft,
        ),
        (
            bottom && right,
            "resize-br",
            ResizeEdge::BottomRight,
            CursorStyle::ResizeUpLeftDownRight,
        ),
    ];
    for (shown, id, resize, cursor) in corners {
        if !shown {
            continue;
        }
        let corner = edge(id, resize, cursor).size(reach);
        let corner = match resize {
            ResizeEdge::TopLeft => corner.top_0().left_0(),
            ResizeEdge::TopRight => corner.top_0().right_0(),
            ResizeEdge::BottomLeft => corner.bottom_0().left_0(),
            _ => corner.bottom_0().right_0(),
        };
        backdrop = backdrop.child(corner);
    }
    backdrop.into_any_element()
}
