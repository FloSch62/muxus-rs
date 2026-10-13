//! How popups stack over dialogs and what a press outside them does.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Context, InteractiveElement, IntoElement, Modifiers, MouseButton, ParentElement, Render,
    Styled, TestAppContext, Window, div, point, px,
};
use muxus_ui::widgets::layer::DIALOG_LAYER;
use muxus_ui::widgets::menu::{MENU_LAYER, popup};

/// GPUI paints deferred elements by priority but hit-tests a nested one above its parent:
/// a menu of lower priority than the dialog it opens from was drawn under the dialog yet
/// still took the next click.
const _: () = assert!(MENU_LAYER > DIALOG_LAYER);

struct Scene {
    non_modal: bool,
    clicked: Rc<Cell<bool>>,
    dismissed: Rc<Cell<bool>>,
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let clicked = self.clicked.clone();
        let dismissed = self.dismissed.clone();
        let menu = popup(
            point(px(10.), px(10.)),
            move |_, _| dismissed.set(true),
            div().w(px(100.)).h(px(40.)),
        );
        let menu = if self.non_modal {
            menu.non_modal()
        } else {
            menu
        };
        div()
            .size_full()
            .child(
                div()
                    .id("target")
                    .absolute()
                    .left(px(300.))
                    .top(px(300.))
                    .w(px(80.))
                    .h(px(30.))
                    .on_mouse_down(MouseButton::Left, move |_, _, _| clicked.set(true)),
            )
            .child(menu)
    }
}

fn press_outside(non_modal: bool, cx: &mut TestAppContext) -> (bool, bool) {
    let clicked = Rc::new(Cell::new(false));
    let dismissed = Rc::new(Cell::new(false));
    let (_, cx) = cx.add_window_view(|_, _| Scene {
        non_modal,
        clicked: clicked.clone(),
        dismissed: dismissed.clone(),
    });
    cx.run_until_parked();
    cx.simulate_click(point(px(320.), px(310.)), Modifiers::default());
    (clicked.get(), dismissed.get())
}

/// A menu (MUI `Menu`) is modal: the press outside only dismisses it.
#[gpui::test]
fn a_press_outside_a_menu_only_dismisses_it(cx: &mut TestAppContext) {
    assert_eq!(press_outside(false, cx), (false, true));
}

/// An autocomplete's listbox (MUI `Popper`) is not: the press dismisses it and lands, so
/// the field the user clicks next takes the focus (the first click used to be lost).
#[gpui::test]
fn a_press_outside_a_listbox_dismisses_it_and_lands(cx: &mut TestAppContext) {
    assert_eq!(press_outside(true, cx), (true, true));
}
