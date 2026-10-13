//! A layer that appears under a still pointer (a dialog opened from a button,
//! a menu) must take the hover from what it covers, as a browser re-checks
//! the hover after a layout change. GPUI decides that only on pointer motion,
//! and keeps a tooltip while the pointer stays inside its anchor's bounds:
//! the button's tooltip stayed on top of the dialog the button opened.
//! [`hover_refresh`] re-sends the pointer's position once, on the frame after
//! the layer first shows.

use std::cell::Cell;

use gpui_kit::{
    App, ElementId, IntoElement, MouseMoveEvent, PlatformInput, RenderOnce, Styled, Window, div,
};

/// Mount inside a layer (anywhere); `id` must be stable for the layer's life.
pub fn hover_refresh(id: impl Into<ElementId>) -> HoverRefresh {
    HoverRefresh { id: id.into() }
}

#[derive(IntoElement)]
pub struct HoverRefresh {
    id: ElementId,
}

impl RenderOnce for HoverRefresh {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let shown = window.use_keyed_state(self.id, cx, |_, _| Cell::new(false));
        if !shown.read(cx).replace(true) {
            window.on_next_frame(|window, cx| {
                let event = MouseMoveEvent {
                    position: window.mouse_position(),
                    pressed_button: None,
                    modifiers: window.modifiers(),
                };
                window.dispatch_event(PlatformInput::MouseMove(event), cx);
            });
        }
        div().absolute().size_0()
    }
}
