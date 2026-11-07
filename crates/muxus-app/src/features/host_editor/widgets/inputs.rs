//! Text inputs a form keeps by key, and the open state of its menus.
//!
//! A form view asks for the input of a field every render; the first call
//! creates it from the draft's value and subscribes to its changes, later
//! calls hand back the same entity. Edits flow into the draft through the
//! field's change handler, which may also correct the text (the port fields
//! keep digits only).

use std::collections::HashMap;
use std::rc::Rc;

use muxus_ui::gpui_kit::base::input::{InputEvent, InputState, TextareaState};
use muxus_ui::gpui_kit::{AppContext, Context, Entity, SharedString, Subscription, Window};
use muxus_ui::widgets::tooltip::AnchorBounds;

use crate::features::host_editor::view::form::Form;

/// What a change handler returns: `Some(text)` to replace what was typed.
pub type Correction = Option<String>;

struct Entry {
    state: Entity<InputState>,
    _subscription: Subscription,
}

/// A list being edited as text (the keyword rules' JSON mode): its editor and the error of
/// the last attempt to apply it.
struct TextEdit {
    state: Entity<TextareaState>,
    error: Option<String>,
    _subscription: Subscription,
}

#[derive(Default)]
pub struct InputCache {
    entries: HashMap<SharedString, Entry>,
    text_edits: HashMap<SharedString, TextEdit>,
}

impl InputCache {
    /// The input of `key`, created from `initial` on first use.
    pub fn get<V: 'static>(
        &mut self,
        key: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<V>,
        initial: impl FnOnce() -> String,
        on_change: impl Fn(&mut V, String, &mut Context<V>) -> Correction + 'static,
    ) -> Entity<InputState> {
        self.get_with_enter(key, window, cx, initial, on_change, |_, _, _| {})
    }

    /// Like [`Self::get`], with a handler for Enter.
    pub fn get_with_enter<V: 'static>(
        &mut self,
        key: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<V>,
        initial: impl FnOnce() -> String,
        on_change: impl Fn(&mut V, String, &mut Context<V>) -> Correction + 'static,
        on_enter: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    ) -> Entity<InputState> {
        let key = key.into();
        if let Some(entry) = self.entries.get(&key) {
            return entry.state.clone();
        }
        let value = initial();
        let state = cx.new(|cx| InputState::new(window, cx).default_value(value));
        let subscription = cx.subscribe_in(
            &state,
            window,
            move |this: &mut V, input: &Entity<InputState>, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => {
                        let value = input.read(cx).value().to_string();
                        if let Some(fixed) = on_change(this, value, cx) {
                            input.update(cx, |state, cx| state.set_value(fixed, window, cx));
                        }
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {
                        on_enter(this, window, cx);
                        cx.notify();
                    }
                    InputEvent::Focus | InputEvent::Blur => cx.notify(),
                }
            },
        );
        self.entries.insert(
            key,
            Entry {
                state: state.clone(),
                _subscription: subscription,
            },
        );
        state
    }

    /// Replace a field's text from code (no change event).
    pub fn set<V: 'static>(
        &self,
        key: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        if let Some(entry) = self.entries.get(key) {
            let value = value.to_owned();
            entry
                .state
                .update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    pub fn existing(&self, key: &str) -> Option<Entity<InputState>> {
        self.entries.get(key).map(|entry| entry.state.clone())
    }

    pub fn remove(&mut self, key: &str) {
        self.entries.remove(key);
    }

    /// Drop every field whose key starts with `prefix` (rows that went away).
    pub fn remove_prefix(&mut self, prefix: &str) {
        self.entries.retain(|key, _| !key.starts_with(prefix));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.text_edits.clear();
    }

    /// The multi-line editor of `key` and its error, while one is open.
    pub fn text_edit(&self, key: &str) -> Option<(Entity<TextareaState>, Option<String>)> {
        self.text_edits
            .get(key)
            .map(|edit| (edit.state.clone(), edit.error.clone()))
    }

    /// Open a multi-line editor on `text` that grows from `rows.0` to `rows.1` rows; typing
    /// clears its error.
    pub fn open_text_edit<V: Form>(
        &mut self,
        key: &str,
        text: String,
        rows: (usize, usize),
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let state = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).auto_grow(rows.0, rows.1);
            state.set_value(text, window, cx);
            state
        });
        let edit_key = SharedString::from(key.to_owned());
        let subscription = cx.subscribe(&state, move |this: &mut V, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && let Some(edit) = this.inputs().text_edits.get_mut(&edit_key)
                && edit.error.take().is_some()
            {
                cx.notify();
            }
        });
        self.text_edits.insert(
            SharedString::from(key.to_owned()),
            TextEdit {
                state,
                error: None,
                _subscription: subscription,
            },
        );
    }

    pub fn set_text_edit_error(&mut self, key: &str, error: String) {
        if let Some(edit) = self.text_edits.get_mut(key) {
            edit.error = Some(error);
        }
    }

    pub fn close_text_edit(&mut self, key: &str) {
        self.text_edits.remove(key);
    }
}

/// Which menu or popup of a form is open, and where each control painted.
#[derive(Default)]
pub struct Menus {
    pub open: Option<SharedString>,
    anchors: HashMap<SharedString, AnchorBounds>,
    /// The highlighted option of an open list (keyboard navigation).
    pub highlighted: Option<usize>,
    /// Whether something was typed since the open list opened (an untouched
    /// autocomplete lists every suggestion).
    pub typed: bool,
}

impl Menus {
    pub fn anchor(&mut self, key: &str) -> AnchorBounds {
        self.anchors
            .entry(SharedString::from(key.to_owned()))
            .or_insert_with(|| Rc::new(std::cell::Cell::new(None)))
            .clone()
    }

    pub fn is_open(&self, key: &str) -> bool {
        self.open.as_deref() == Some(key)
    }

    pub fn toggle(&mut self, key: &str) {
        if self.is_open(key) {
            self.open = None;
        } else {
            self.open = Some(SharedString::from(key.to_owned()));
        }
        self.highlighted = None;
        self.typed = false;
    }

    pub fn open(&mut self, key: &str) {
        if !self.is_open(key) {
            self.open = Some(SharedString::from(key.to_owned()));
            self.highlighted = None;
            self.typed = false;
        }
    }

    pub fn close(&mut self) {
        self.open = None;
        self.highlighted = None;
    }
}
