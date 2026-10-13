//! A window-sized layer for overlays owned by a view that sits somewhere
//! inside the window (a side panel's dialog, a menu's prompt). MUI portals
//! its modals to the document body; GPUI lays an element out where it is
//! mounted, so [`window_layer`] lifts it to the window's corner at the
//! window's size, above the regular tree. A [`super::Dialog`] (whose backdrop
//! is `absolute inset_0`) then covers the whole window wherever its owner is.

use crate::zoom::px;
use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Styled, Window, anchored, deferred, div, point,
};

use crate::color::rgba8;
use crate::widgets::dialog::CloseHandler;

/// Draw order of a [`window_layer`] relative to other deferred elements:
/// dialogs 3, menus [`super::menu::MENU_LAYER`] above them.
pub const DIALOG_LAYER: usize = 3;

/// Notifications (MUI's snackbar, z-index 1400) draw above dialogs and menus
/// (modal, 1300), as a failure a dialog caused shows over it.
pub const TOAST_LAYER: usize = 5;

/// `content` laid out over the whole window.
pub fn window_layer(window: &Window, priority: usize, content: impl IntoElement) -> AnyElement {
    let size = window.viewport_size();
    deferred(
        anchored()
            .position(point(px(0.), px(0.)))
            .child(div().relative().w(size.width).h(size.height).child(content)),
    )
    .with_priority(priority)
    .into_any_element()
}

/// MUI's modal over the whole window from anywhere in the tree: the 50% black
/// backdrop (a press on it calls `on_close`) with `paper` centred and kept
/// 32px from the window's edges. Unlike [`super::dialog::modal`] the backdrop
/// is part of the layer itself, so menus a dialog opens (deferred popups)
/// draw above it.
pub fn modal_layer(
    window: &Window,
    id: impl Into<ElementId>,
    paper: impl IntoElement,
    on_close: Option<CloseHandler>,
) -> AnyElement {
    let size = window.viewport_size();
    window_layer(
        window,
        DIALOG_LAYER,
        div()
            .id(id)
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba8(0, 0, 0, 0.5))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                move |_, window: &mut Window, cx: &mut App| {
                    if let Some(close) = &on_close {
                        close(window, cx);
                    }
                },
            )
            .child(crate::widgets::hover::hover_refresh("modal-layer-hover"))
            .child(
                div()
                    .id("modal-layer-paper")
                    .flex()
                    .max_h(size.height - px(64.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(paper),
            ),
    )
}
