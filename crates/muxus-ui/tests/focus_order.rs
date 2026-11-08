//! Keyboard focus between the kit's controls: Tab walks the controls of a
//! modal and wraps around inside it, Shift+Tab walks back, Enter presses the
//! focused control, and closing the modal hands the focus back.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, FocusHandle, InteractiveElement, IntoElement, KeyUpEvent, Keystroke, ParentElement,
    Render, Styled, TestAppContext, VisualTestContext, Window, div,
};
use muxus_ui::theme::Mode;
use muxus_ui::widgets::dialog::modal;
use muxus_ui::widgets::{Button, Checkbox};

type Log = Rc<RefCell<Vec<&'static str>>>;

struct Scene {
    open: bool,
    log: Log,
    outside: FocusHandle,
}

fn pressed(
    log: &Log,
    name: &'static str,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + use<> {
    let log = log.clone();
    move |_, _, _| log.borrow_mut().push(name)
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let log = &self.log;
        let mut root = div()
            .size_full()
            .child(
                div()
                    .id("outside-field")
                    .track_focus(&self.outside)
                    .w(gpui::px(40.))
                    .h(gpui::px(20.)),
            )
            .child(
                div()
                    .absolute()
                    .left(gpui::px(200.))
                    .top(gpui::px(200.))
                    .child(Button::new("outside", "Behind").on_click(pressed(log, "behind"))),
            );
        if self.open {
            root = root.child(modal(
                div()
                    .flex()
                    .gap(gpui::px(8.))
                    .child(Button::new("first", "First").on_click(pressed(log, "first")))
                    .child(Checkbox::new("check", false).on_click(pressed(log, "check")))
                    .child(Button::new("last", "Last").on_click(pressed(log, "last"))),
                None,
            ));
        }
        root
    }
}

fn press(cx: &mut VisualTestContext, keys: &'static str) {
    for key in keys.split(' ') {
        cx.simulate_keystrokes(key);
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse(key).unwrap(),
        });
        cx.run_until_parked();
    }
}

fn scene(cx: &mut TestAppContext) -> (gpui::Entity<Scene>, &mut VisualTestContext, Log) {
    cx.update(|cx| {
        muxus_ui::init(Mode::Light, cx);
    });
    let log: Log = Rc::default();
    let scene_log = log.clone();
    let (view, cx) = cx.add_window_view(move |_, cx| Scene {
        open: false,
        log: scene_log,
        outside: cx.focus_handle(),
    });
    cx.run_until_parked();
    (view, cx, log)
}

#[gpui::test]
fn tab_walks_a_modals_controls_and_wraps_inside_it(cx: &mut TestAppContext) {
    let (view, cx, log) = scene(cx);
    // Something behind the modal has the keyboard when it opens.
    let outside = view.read_with(cx, |scene, _| scene.outside.clone());
    cx.update(|window, cx| window.focus(&outside, cx));
    view.update(cx, |scene, cx| {
        scene.open = true;
        cx.notify();
    });
    cx.run_until_parked();
    // Tests have no frame loop: deliver the frame after the one that drew the modal.
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
    let inside = cx.update(|window, _| !outside.is_focused(window));
    assert!(inside, "the modal took the keyboard as it opened");
    // The modal took the keyboard: Tab goes to its first control.
    press(cx, "tab enter");
    press(cx, "tab space");
    press(cx, "tab enter");
    // Past the last control, Tab wraps to the first, never to what is behind.
    press(cx, "tab enter");
    // Shift+Tab walks back, wrapping to the last.
    press(cx, "shift-tab enter");
    assert_eq!(
        log.borrow().as_slice(),
        ["first", "check", "last", "first", "last"]
    );

    // Closing hands the keyboard back to what had it before.
    view.update(cx, |scene, cx| {
        scene.open = false;
        cx.notify();
    });
    // The frame without the modal is drawn on the next update; the restore follows it.
    cx.run_until_parked();
    cx.update(|_, _| {});
    cx.run_until_parked();
    let focused = cx.update(|window, _| outside.is_focused(window));
    assert!(focused, "the focus went back behind the closed modal");
}

#[gpui::test]
fn a_pointer_press_leaves_the_focus_where_it_was(cx: &mut TestAppContext) {
    let (view, cx, log) = scene(cx);
    let outside = view.read_with(cx, |scene, _| scene.outside.clone());
    cx.update(|window, cx| window.focus(&outside, cx));
    cx.run_until_parked();
    // A button pressed with the pointer runs its action without taking the focus.
    cx.simulate_click(
        gpui::point(gpui::px(215.), gpui::px(210.)),
        gpui::Modifiers::default(),
    );
    assert_eq!(log.borrow().as_slice(), ["behind"]);
    let focused = cx.update(|window, _| outside.is_focused(window));
    assert!(focused);
    // Enter does not press it either: it never had the keyboard.
    press(cx, "enter");
    assert_eq!(log.borrow().as_slice(), ["behind"]);
}
