//! The keyboard in an open menu (MUI `MenuList`): the focus moves to the
//! selected item as the menu opens, the arrows walk the items and wrap,
//! typing jumps to an item, Enter presses it, Escape closes the menu and the
//! focus goes back. And a window root with a tab order: Tab walks its
//! controls, except inside a terminal, which keeps Tab for the shell.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, FocusHandle, InteractiveElement, IntoElement, KeyUpEvent, Keystroke, ParentElement,
    Render, Styled, TestAppContext, VisualTestContext, Window, div, point, px,
};
use muxus_ui::theme::{ActiveTheme, Mode};
use muxus_ui::widgets::focus::tab_order;
use muxus_ui::widgets::menu::{paper, popup};
use muxus_ui::widgets::{Button, MenuItem};

type Log = Rc<RefCell<Vec<String>>>;

const ITEMS: [&str; 4] = ["Light", "Dark", "Match system", "Disabled"];

struct Scene {
    open: bool,
    selected: usize,
    log: Log,
    outside: FocusHandle,
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let mut root = div().size_full().child(
            div()
                .id("outside-field")
                .track_focus(&self.outside)
                .w(px(40.))
                .h(px(20.)),
        );
        if self.open {
            let mut menu = paper(&theme);
            for (index, item) in ITEMS.iter().enumerate() {
                let log = self.log.clone();
                let this = cx.entity().downgrade();
                menu = menu.child(
                    MenuItem::new(*item, *item)
                        .selected(index == self.selected)
                        .disabled(*item == "Disabled")
                        .on_click(move |_, _, cx| {
                            log.borrow_mut().push(item.to_string());
                            let _ = this.update(cx, |scene, cx| {
                                scene.open = false;
                                cx.notify();
                            });
                        }),
                );
            }
            let log = self.log.clone();
            let this = cx.entity().downgrade();
            root = root.child(popup(
                point(px(100.), px(100.)),
                move |_, cx| {
                    log.borrow_mut().push("dismissed".into());
                    let _ = this.update(cx, |scene, cx| {
                        scene.open = false;
                        cx.notify();
                    });
                },
                menu,
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

fn open(view: &gpui::Entity<Scene>, cx: &mut VisualTestContext) {
    view.update(cx, |scene, cx| {
        scene.open = true;
        cx.notify();
    });
    cx.run_until_parked();
    // Tests have no frame loop: deliver the frame after the one that drew the menu.
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
}

fn scene(cx: &mut TestAppContext) -> (gpui::Entity<Scene>, &mut VisualTestContext, Log) {
    cx.update(|cx| {
        muxus_ui::init(Mode::Light, cx);
    });
    let log: Log = Rc::default();
    let scene_log = log.clone();
    let (view, cx) = cx.add_window_view(move |_, cx| Scene {
        open: false,
        selected: 1,
        log: scene_log,
        outside: cx.focus_handle(),
    });
    cx.run_until_parked();
    let outside = view.read_with(cx, |scene, _| scene.outside.clone());
    cx.update(|window, cx| window.focus(&outside, cx));
    (view, cx, log)
}

#[gpui::test]
fn the_arrows_walk_an_open_menu_from_its_selected_item(cx: &mut TestAppContext) {
    let (view, cx, log) = scene(cx);
    let outside = view.read_with(cx, |scene, _| scene.outside.clone());
    open(&view, cx);
    let inside = cx.update(|window, _| !outside.is_focused(window));
    assert!(inside, "the menu took the keyboard as it opened");
    // From "Dark" (selected): down to "Match system", past the disabled item to
    // "Light" (wrapping), and Enter presses it.
    press(cx, "down down enter");
    assert_eq!(log.borrow().as_slice(), ["Light"]);
    // The menu closed: the keyboard is back where it was.
    cx.run_until_parked();
    cx.update(|_, _| {});
    cx.run_until_parked();
    assert!(cx.update(|window, _| outside.is_focused(window)));

    // Up wraps to the last item; End and Home jump.
    open(&view, cx);
    press(cx, "home up enter");
    assert_eq!(log.borrow().as_slice(), ["Light", "Match system"]);
}

#[gpui::test]
fn typing_jumps_to_an_item_and_escape_closes_the_menu(cx: &mut TestAppContext) {
    let (view, cx, log) = scene(cx);
    let outside = view.read_with(cx, |scene, _| scene.outside.clone());
    open(&view, cx);
    press(cx, "m enter");
    assert_eq!(log.borrow().as_slice(), ["Match system"]);

    open(&view, cx);
    press(cx, "escape");
    assert_eq!(log.borrow().as_slice(), ["Match system", "dismissed"]);
    cx.run_until_parked();
    cx.update(|_, _| {});
    cx.run_until_parked();
    assert!(cx.update(|window, _| outside.is_focused(window)));
}

/// A window root with a tab order and a "terminal" that wants Tab for itself.
struct Page {
    log: Log,
    /// What has the keyboard when no control does (the page's body).
    body: FocusHandle,
    terminal: FocusHandle,
}

impl Render for Page {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let log = self.log.clone();
        let pressed = |name: &'static str| {
            let log = self.log.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, _: &mut gpui::App| {
                log.borrow_mut().push(name.to_string())
            }
        };
        tab_order(div().id("page"))
            .size_full()
            .flex()
            .child(div().id("body").track_focus(&self.body).size_0())
            .child(Button::new("one", "One").on_click(pressed("one")))
            .child(Button::new("two", "Two").on_click(pressed("two")))
            .child(
                div()
                    .id("terminal")
                    .key_context(muxus_ui::TERMINAL_KEY_CONTEXT)
                    .track_focus(&self.terminal)
                    .w(px(100.))
                    .h(px(40.))
                    .on_key_down(move |event, _, _| {
                        log.borrow_mut()
                            .push(format!("terminal {}", event.keystroke.key))
                    }),
            )
    }
}

#[gpui::test]
fn tab_walks_the_window_but_a_terminal_keeps_it(cx: &mut TestAppContext) {
    cx.update(|cx| {
        muxus_ui::init(Mode::Light, cx);
    });
    let log: Log = Rc::default();
    let page_log = log.clone();
    let (view, cx) = cx.add_window_view(move |_, cx| Page {
        log: page_log,
        body: cx.focus_handle(),
        terminal: cx.focus_handle(),
    });
    cx.run_until_parked();
    let body = view.read_with(cx, |page, _| page.body.clone());
    cx.update(|window, cx| window.focus(&body, cx));
    // No control has the keyboard: Tab goes to the first control, then the next.
    press(cx, "tab enter tab enter");
    assert_eq!(log.borrow().as_slice(), ["one", "two"]);
    // Shift+Tab walks back.
    press(cx, "shift-tab enter");
    assert_eq!(log.borrow().as_slice(), ["one", "two", "one"]);
    // In the terminal, Tab is the shell's.
    let terminal = view.read_with(cx, |page, _| page.terminal.clone());
    cx.update(|window, cx| window.focus(&terminal, cx));
    press(cx, "tab");
    assert_eq!(
        log.borrow().as_slice(),
        ["one", "two", "one", "terminal tab"]
    );
    assert!(cx.update(|window, _| terminal.is_focused(window)));
}
