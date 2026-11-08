//! The keyboard in an open menu: MUI's `MenuList` with the `Menu`'s
//! `autoFocusItem` and the modal's focus handling.
//!
//! When a modal [`super::menu::popup`] opens, the focus moves to its selected
//! item, or to its first one (`variant="selectedMenu"`), unless the menu put
//! it somewhere inside itself already (a search field). The arrow keys move it
//! through the items, wrapping at the ends; Home and End jump to the first and
//! last; typing jumps to the next item whose label starts with what was typed
//! (`textCriteria`); Enter or Space presses the focused item; Escape and Tab
//! close the menu. When the menu closes, the focus goes back to where it was
//! before it opened, unless the chosen item moved it elsewhere.
//!
//! The items are the [`super::MenuItem`]s laid out inside the popup that can
//! be pressed (enabled, with a click handler); they register themselves while
//! the popup's content is laid out ([`MenuScope`]), in paint order.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::{
    AnyElement, App, Bounds, Context, Element, ElementId, FocusHandle, GlobalElementId,
    InspectorElementId, IntoElement, KeyBinding, KeyDownEvent, LayoutId, NoAction, Pixels,
    SharedString, WeakFocusHandle, Window,
};

/// The key context of an open menu's content: Tab belongs to the menu (it
/// closes it) rather than to the window's or a dialog's tab order.
pub const MENU_KEY_CONTEXT: &str = "MuxusMenu";

/// How long typed characters keep adding to one search (`MenuList`'s 500 ms).
const TYPE_AHEAD_WINDOW: Duration = Duration::from_millis(500);

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", NoAction, Some(MENU_KEY_CONTEXT)),
        KeyBinding::new("shift-tab", NoAction, Some(MENU_KEY_CONTEXT)),
    ]);
}

thread_local! {
    /// The registries of the menus being laid out, innermost last.
    static SCOPES: RefCell<Vec<Rc<Registry>>> = const { RefCell::new(Vec::new()) };
    /// Open menus that took the focus: their own focus and what had it before.
    static OPEN: RefCell<Vec<(WeakFocusHandle, FocusHandle)>> = const { RefCell::new(Vec::new()) };
}

/// One pressable item of a menu, in paint order.
struct Entry {
    id: ElementId,
    focus: FocusHandle,
    selected: bool,
    label: SharedString,
}

/// The items of one open menu, rebuilt every frame as they are laid out.
#[derive(Default)]
pub(crate) struct Registry {
    items: RefCell<Vec<Entry>>,
    handles: RefCell<HashMap<ElementId, FocusHandle>>,
}

impl Registry {
    fn handles(&self) -> Vec<FocusHandle> {
        self.items
            .borrow()
            .iter()
            .map(|entry| entry.focus.clone())
            .collect()
    }

    fn labels(&self) -> Vec<SharedString> {
        self.items
            .borrow()
            .iter()
            .map(|entry| entry.label.clone())
            .collect()
    }

    /// `autoFocusItem` on a `selectedMenu`: the selected item, else the first.
    fn initial(&self) -> Option<FocusHandle> {
        let items = self.items.borrow();
        items
            .iter()
            .find(|entry| entry.selected)
            .or_else(|| items.first())
            .map(|entry| entry.focus.clone())
    }
}

/// The focus a [`super::MenuItem`] takes as an item of the menu being laid
/// out, or `None` outside a menu (or in a non-modal list, which leaves the
/// keyboard with its field).
pub(crate) fn register_item(
    id: &ElementId,
    label: &SharedString,
    selected: bool,
    cx: &mut App,
) -> Option<FocusHandle> {
    let registry = SCOPES.with(|scopes| scopes.borrow().last().cloned())?;
    let focus = registry
        .handles
        .borrow_mut()
        .entry(id.clone())
        .or_insert_with(|| cx.focus_handle())
        .clone();
    registry.items.borrow_mut().push(Entry {
        id: id.clone(),
        focus: focus.clone(),
        selected,
        label: label.clone(),
    });
    Some(focus)
}

/// What had the keyboard before the open menus took it: `focused` itself
/// unless it is inside one of them. A dialog opened from a menu item goes back
/// there when it closes, not to the menu that is gone by then.
pub(crate) fn focus_before_menus(
    focused: Option<FocusHandle>,
    window: &Window,
) -> Option<FocusHandle> {
    let focused = focused?;
    OPEN.with(|open| {
        let mut open = open.borrow_mut();
        open.retain(|(menu, _)| menu.upgrade().is_some());
        for (menu, previous) in open.iter().rev() {
            if let Some(menu) = menu.upgrade()
                && (menu == focused || menu.contains(&focused, window))
            {
                return Some(previous.clone());
            }
        }
        Some(focused)
    })
}

/// Lays its content out with `registry` as the menu its items join.
pub(crate) struct MenuScope {
    registry: Rc<Registry>,
    child: AnyElement,
}

impl MenuScope {
    pub(crate) fn new(registry: Rc<Registry>, child: impl IntoElement) -> Self {
        Self {
            registry,
            child: child.into_any_element(),
        }
    }
}

impl IntoElement for MenuScope {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for MenuScope {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.registry.items.borrow_mut().clear();
        SCOPES.with(|scopes| scopes.borrow_mut().push(self.registry.clone()));
        let layout = self.child.request_layout(window, cx);
        SCOPES.with(|scopes| scopes.borrow_mut().pop());
        // Items that are gone take their focus with them.
        let live: HashSet<ElementId> = self
            .registry
            .items
            .borrow()
            .iter()
            .map(|entry| entry.id.clone())
            .collect();
        self.registry
            .handles
            .borrow_mut()
            .retain(|id, _| live.contains(id));
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// `MenuList`'s type-ahead (`textCriteria`): keys typed within 500 ms of each
/// other search together; one key typed again and again steps through the
/// items starting with it.
#[derive(Debug, Default)]
pub struct TypeAhead {
    keys: Vec<char>,
    last: Option<Instant>,
    repeating: bool,
    previous_matched: bool,
}

impl TypeAhead {
    pub fn new() -> Self {
        Self {
            keys: Vec::new(),
            last: None,
            repeating: true,
            previous_matched: true,
        }
    }

    /// `key` typed at `now` over items labelled `labels`, the focus on
    /// `current`: the item to focus (`current` itself to keep it), or `None`.
    pub fn key(
        &mut self,
        key: char,
        now: Instant,
        labels: &[impl AsRef<str>],
        current: Option<usize>,
    ) -> Option<usize> {
        let key = key.to_lowercase().next().unwrap_or(key);
        if !self.keys.is_empty() {
            if self
                .last
                .is_some_and(|last| now.duration_since(last) > TYPE_AHEAD_WINDOW)
            {
                self.keys.clear();
                self.repeating = true;
                self.previous_matched = true;
            } else if self.repeating && Some(&key) != self.keys.first() {
                self.repeating = false;
            }
        }
        self.last = Some(now);
        self.keys.push(key);
        let matches = |index: usize| -> bool {
            let text = labels[index].as_ref().trim().to_lowercase();
            if self.repeating {
                text.chars().next() == self.keys.first().copied()
            } else {
                text.starts_with(&self.keys.iter().collect::<String>())
            }
        };
        if !self.previous_matched {
            return None;
        }
        if let Some(current) = current
            && !self.repeating
            && matches(current)
        {
            return Some(current);
        }
        // From the item after the focused one, around the end, up to the
        // focused one itself.
        let count = labels.len();
        let start = current.map_or(0, |current| current + 1);
        let found = (0..count)
            .map(|step| (start + step) % count)
            .find(|&index| matches(index));
        if found.is_none() {
            self.previous_matched = false;
        }
        found
    }
}

/// Where the arrows, Home and End move the focus among `count` items from
/// `current` (`None`: the menu itself has it). The list wraps.
pub fn step(key: &str, current: Option<usize>, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    match key {
        "down" => Some(current.map_or(0, |current| (current + 1) % count)),
        "up" => Some(current.map_or(count - 1, |current| (current + count - 1) % count)),
        "home" => Some(0),
        "end" => Some(count - 1),
        _ => None,
    }
}

/// An open menu's focus bookkeeping between frames.
pub(crate) struct MenuFocus {
    pub(crate) container: FocusHandle,
    pub(crate) registry: Rc<Registry>,
    type_ahead: TypeAhead,
}

impl MenuFocus {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let container = cx.focus_handle();
        let registry = Rc::new(Registry::default());
        let previous = window.focused(cx);
        let (enter, items) = (container.clone(), registry.clone());
        // Once the menu is laid out (its items are known), as `autoFocusItem` does.
        window.on_next_frame(move |window, cx| {
            if enter.contains_focused(window, cx) {
                return;
            }
            let Some(target) = items.initial() else {
                // A popover without items leaves the keyboard where it is.
                return;
            };
            if let Some(previous) = previous.clone() {
                OPEN.with(|open| open.borrow_mut().push((enter.downgrade(), previous)));
            }
            window.focus(&target, cx);
        });
        let handle = window.window_handle();
        cx.on_release(move |this: &mut MenuFocus, cx| {
            let previous = OPEN.with(|open| {
                let mut open = open.borrow_mut();
                let index = open
                    .iter()
                    .position(|(menu, _)| menu.upgrade().as_ref() == Some(&this.container))?;
                Some(open.remove(index).1)
            });
            let Some(previous) = previous else {
                return;
            };
            let items = this.registry.handles();
            let container = this.container.clone();
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    let stranded = match window.focused(cx) {
                        None => true,
                        Some(focused) => focused == container || items.contains(&focused),
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
            registry,
            type_ahead: TypeAhead::new(),
        }
    }

    /// A key pressed inside the menu. True when the menu used it; `dismiss`
    /// closes the menu (Escape, Tab).
    pub(crate) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        dismiss: &dyn Fn(&mut Window, &mut App),
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Some(focused) = window.focused(cx) else {
            return false;
        };
        let items = self.registry.handles();
        let current = items.iter().position(|item| *item == focused);
        // Only the items' own keys: a search field inside the menu keeps its own.
        if current.is_none() && focused != self.container {
            return false;
        }
        let keystroke = &event.keystroke;
        let key = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;
        if matches!(key, "escape" | "tab") {
            dismiss(window, cx);
            return true;
        }
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return false;
        }
        if let Some(index) = step(key, current, items.len()) {
            window.focus(&items[index], cx);
            return true;
        }
        // The character typed (a named key like "left" is none).
        let text = keystroke
            .key_char
            .clone()
            .or_else(|| (key.chars().count() == 1).then(|| key.to_string()));
        let typed = text
            .as_deref()
            .filter(|text| text.chars().count() == 1 && key != "space")
            .and_then(|text| text.chars().next())
            .filter(|ch| !ch.is_control());
        if let Some(ch) = typed {
            let labels = self.registry.labels();
            if let Some(index) = self.type_ahead.key(ch, Instant::now(), &labels, current) {
                window.focus(&items[index], cx);
            }
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEMS: [&str; 5] = [
        "Split right",
        "Split down",
        "Zoom pane",
        "Even out panes",
        "Close pane",
    ];

    #[test]
    fn arrows_wrap_and_home_end_jump() {
        assert_eq!(step("down", None, 3), Some(0));
        assert_eq!(step("up", None, 3), Some(2));
        assert_eq!(step("down", Some(2), 3), Some(0));
        assert_eq!(step("up", Some(0), 3), Some(2));
        assert_eq!(step("home", Some(1), 3), Some(0));
        assert_eq!(step("end", Some(1), 3), Some(2));
        assert_eq!(step("left", Some(1), 3), None);
        assert_eq!(step("down", None, 0), None);
    }

    #[test]
    fn one_key_again_and_again_steps_through_the_items_starting_with_it() {
        let mut typed = TypeAhead::new();
        let now = Instant::now();
        assert_eq!(typed.key('s', now, &ITEMS, None), Some(0));
        assert_eq!(typed.key('s', now, &ITEMS, Some(0)), Some(1));
        assert_eq!(typed.key('S', now, &ITEMS, Some(1)), Some(0));
    }

    #[test]
    fn keys_typed_together_match_the_start_of_a_label() {
        let mut typed = TypeAhead::new();
        let now = Instant::now();
        assert_eq!(typed.key('z', now, &ITEMS, Some(0)), Some(2));
        // "sp" from "Zoom pane": the next label starting with it, after the focus.
        let mut typed = TypeAhead::new();
        assert_eq!(typed.key('s', now, &ITEMS, Some(2)), Some(0));
        assert_eq!(typed.key('p', now, &ITEMS, Some(0)), Some(0));
        assert_eq!(typed.key('l', now, &ITEMS, Some(0)), Some(0));
    }

    #[test]
    fn a_pause_starts_a_new_search_and_a_miss_stops_the_search() {
        let mut typed = TypeAhead::new();
        let now = Instant::now();
        assert_eq!(typed.key('c', now, &ITEMS, None), Some(4));
        assert_eq!(typed.key('x', now, &ITEMS, Some(4)), None);
        // Still within the window: the search already missed.
        assert_eq!(typed.key('l', now, &ITEMS, Some(4)), None);
        let later = now + Duration::from_millis(600);
        assert_eq!(typed.key('e', later, &ITEMS, Some(4)), Some(3));
    }
}
