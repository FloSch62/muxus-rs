//! Keyboard focus for the kit's controls.
//!
//! Every control (buttons, icon buttons, checkboxes, radios, switches,
//! sliders, tabs, toggle buttons, select fields, sort labels) is a tab
//! stop, so Tab and Shift+Tab walk them in paint order: in a window with the
//! component library's root (settings, SFTP), and inside any dialog, whose
//! [`super::dialog::modal`] keeps the walk within it (MUI's `FocusTrap`).
//! Enter and Space press the focused control (GPUI turns them into a click).
//!
//! MUI marks keyboard focus with the focus ripple (`TouchRipple`
//! `pulsate`): the control's ink at 0.3 over its shape, shown only for
//! keyboard focus (`:focus-visible`). It is drawn here without the slow
//! 0.92 ↔ 1 pulse. Buttons keep the focus where it was when pressed with the
//! pointer (they did before keyboard focus existed); form controls take it,
//! as Chrome gives a clicked checkbox the focus.

use gpui_kit::{
    App, Div, ElementId, FocusHandle, Hsla, InteractiveElement, IntoElement, KeyBinding,
    MouseButton, RenderOnce, Stateful, StatefulInteractiveElement, Styled, Window,
};

use crate::color::{alpha, over};

/// The focus ripple's opacity (`.MuiTouchRipple-ripplePulsate`).
pub const RIPPLE_OPACITY: f32 = 0.3;

/// The focus ripple of a control inked `ink`, over its resting `background`
/// (a contained button's fill), as one color.
pub fn ripple(ink: Hsla, background: Option<Hsla>) -> Hsla {
    let ripple = alpha(ink, RIPPLE_OPACITY);
    match background {
        Some(background) => over(ripple, background),
        None => ripple,
    }
}

/// The tab stop a control keeps between frames, keyed by its id (for controls
/// that style a part of themselves by focus, which `focus_visible` cannot).
pub fn keyed_focus(id: &ElementId, window: &mut Window, cx: &mut App) -> FocusHandle {
    window
        .use_keyed_state(
            ElementId::NamedChild(std::sync::Arc::new(id.clone()), "focus".into()),
            cx,
            |_, cx| cx.focus_handle().tab_stop(true),
        )
        .read(cx)
        .clone()
}

/// The control has the keyboard focus and got it from the keyboard (`:focus-visible`).
pub fn focus_visible(focus: &FocusHandle, window: &Window) -> bool {
    focus.is_focused(window) && window.last_input_was_keyboard()
}

/// Controls as tab stops.
pub trait TabStop: StatefulInteractiveElement + Sized {
    /// A tab stop whose keyboard focus shows `ripple` as its background. With
    /// `keep_focus` a press with the pointer leaves the focus where it was.
    fn tab_stop_with_ripple(self, ripple: Hsla, keep_focus: bool) -> Self {
        let this = self
            .tab_stop(true)
            .focusable()
            .focus_visible(move |s| s.bg(ripple));
        if keep_focus {
            this.on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        } else {
            this
        }
    }
}

impl<T: StatefulInteractiveElement> TabStop for T {}

/// The key context of a focus trap (an open modal): Tab and Shift+Tab walk its controls.
pub const TRAP_KEY_CONTEXT: &str = "MuxusFocusTrap";

gpui_kit::actions!(muxus_focus, [FocusNext, FocusPrevious]);

/// How many tab stops a Tab press passes at most looking for the next one
/// inside the trap (the window's others come first when it wraps around).
const MAX_TAB_STEPS: usize = 1024;

/// Bind Tab and Shift+Tab inside focus traps.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, Some(TRAP_KEY_CONTEXT)),
        KeyBinding::new("shift-tab", FocusPrevious, Some(TRAP_KEY_CONTEXT)),
    ]);
}

/// MUI's `FocusTrap` around a modal's paper: when it opens, the focus moves
/// into it unless something in it took the focus already; Tab and Shift+Tab
/// cycle through its controls only; when it closes, the focus goes back to
/// where it was before, unless the owner moved it elsewhere meanwhile.
///
/// `element` is the paper's container itself (no wrapper is added, so the
/// layout stays as it was); `key` names the trap's state among its siblings.
#[derive(IntoElement)]
pub struct FocusTrap {
    key: ElementId,
    element: Stateful<Div>,
}

impl FocusTrap {
    pub fn new(key: impl Into<ElementId>, element: Stateful<Div>) -> Self {
        Self {
            key: key.into(),
            element,
        }
    }
}

/// A trap's bookkeeping between frames.
struct TrapState {
    /// The trap's own focus (not a tab stop): what has the keyboard when
    /// none of its controls does.
    container: FocusHandle,
    /// What had the keyboard before the trap opened.
    previous: Option<FocusHandle>,
    /// What had the keyboard inside the trap last frame.
    inside: Option<FocusHandle>,
}

impl TrapState {
    fn new(window: &mut Window, cx: &mut gpui_kit::Context<Self>) -> Self {
        let container = cx.focus_handle();
        let previous = window.focused(cx);
        let this = cx.weak_entity();
        let enter = container.clone();
        window.on_next_frame(move |window, cx| {
            if enter.contains_focused(window, cx) {
                // The dialog focused a field of its own as it opened: what had the
                // focus before is that field, not something to go back to.
                let _ = this.update(cx, |this, _| this.previous = None);
            } else {
                window.focus(&enter, cx);
            }
        });
        let handle = window.window_handle();
        cx.on_release(move |this: &mut TrapState, cx| {
            let container = this.container.clone();
            let inside = this.inside.take();
            let Some(previous) = this.previous.take() else {
                return;
            };
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    let stranded = match window.focused(cx) {
                        None => true,
                        Some(focused) => focused == container || Some(&focused) == inside.as_ref(),
                    };
                    if stranded {
                        window.focus(&previous, cx);
                    }
                });
            });
        })
        .detach();
        Self {
            container,
            previous,
            inside: None,
        }
    }
}

/// Move the focus to the next (or previous) tab stop inside `container`,
/// wrapping around within it.
fn cycle(container: &FocusHandle, forward: bool, window: &mut Window, cx: &mut App) {
    let start = window.focused(cx);
    for _ in 0..MAX_TAB_STEPS {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
        let Some(now) = window.focused(cx) else {
            break;
        };
        if now != *container && container.contains_focused(window, cx) {
            return;
        }
        if Some(&now) == start.as_ref() {
            break;
        }
    }
    // Nothing to tab to inside: the focus stays.
    window.focus(start.as_ref().unwrap_or(container), cx);
}

impl RenderOnce for FocusTrap {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.key, cx, TrapState::new);
        let container = state.read(cx).container.clone();
        if container.contains_focused(window, cx) {
            let inside = window.focused(cx);
            state.update(cx, |state, _| state.inside = inside);
        }
        let (next, previous) = (container.clone(), container.clone());
        self.element
            .key_context(TRAP_KEY_CONTEXT)
            .track_focus(&container)
            .on_action(move |_: &FocusNext, window, cx| cycle(&next, true, window, cx))
            .on_action(move |_: &FocusPrevious, window, cx| cycle(&previous, false, window, cx))
    }
}
