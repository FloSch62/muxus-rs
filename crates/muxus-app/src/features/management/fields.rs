//! The text inputs and code editors of the workbench, created on first use
//! and kept while the view lives. React keeps its inputs controlled by the
//! store; here each render pushes the store's value into the input when the
//! two differ, and every edit goes back into the store right away, so the
//! two only differ when the store changed from elsewhere (a draft loaded
//! from history, a number field that normalized what was typed).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use muxus_ui::gpui_kit::base::input::{EditorState, InputEvent, InputState};
use muxus_ui::gpui_kit::{AppContext, Context, Entity, Focusable, Subscription, Window};

use muxus_ui::theme::ActiveTheme;

use super::view::ManagementView;

/// Which single-line input.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FieldKey {
    ExplorerFilter,
    LibraryFilter,
    DeviceFilter,
    ResultFilter,
    LiveFilter,
    /// A Get or Subscribe path, by position.
    Path(usize),
    /// A Set item's path, by item id.
    SetPath(String),
    Prefix,
    Depth,
    SampleSeconds,
    Heartbeat,
    ConfirmSeconds,
    XpathFilter,
    Persist,
    ConfirmTimeout,
    SchemaIdentifier,
    SchemaVersion,
    SessionId,
    SaveName,
    PromptText,
    /// A field of a gNOI/gNSI tool.
    Tool(&'static str),
    /// A field of a row in a tool's list (path rules, groups), by position.
    ToolRow(&'static str, usize),
}

/// Which code editor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EditorKey {
    /// A Set item's value, by item id.
    SetValue(String),
    Filter,
    Config,
    Rpc,
    RpcPreview,
    /// A read-only result (raw JSON, reply XML, YANG module) of a run.
    Result(String),
    /// A tool's text area.
    Tool(&'static str),
}

struct Field {
    state: Entity<InputState>,
    placeholder: String,
    _subscription: Subscription,
}

struct Editor {
    state: Entity<EditorState>,
    _subscription: Subscription,
    language: &'static str,
    /// The theme mode its colors were set for.
    dark: bool,
    /// The text the view last handed the editor or heard back from it. Rendering compares
    /// the view's value with this, not with the editor's own text, which would copy the
    /// whole document (a large Get result) on every frame.
    synced: Rc<RefCell<String>>,
}

#[derive(Default)]
pub struct Fields {
    inputs: HashMap<FieldKey, Field>,
    editors: HashMap<EditorKey, Editor>,
}

impl Fields {
    /// Drop inputs whose keys are gone (removed paths and Set items).
    pub fn retain(&mut self, keep: impl Fn(&FieldKey) -> bool) {
        self.inputs.retain(|key, _| keep(key));
    }
}

impl ManagementView {
    /// The input for `key`, created with `placeholder` on first use.
    pub(super) fn input(
        &mut self,
        key: FieldKey,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(field) = self.fields.inputs.get_mut(&key) {
            let state = field.state.clone();
            if field.placeholder != placeholder {
                field.placeholder = placeholder.to_owned();
                let placeholder = placeholder.to_owned();
                state.update(cx, |state, cx| {
                    state.set_placeholder(placeholder, window, cx)
                });
            }
            return state;
        }
        let placeholder_text = placeholder.to_owned();
        let placeholder = placeholder.to_owned();
        let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let event_key = key.clone();
        let subscription = cx.subscribe_in(
            &state,
            window,
            move |this, state: &Entity<InputState>, event: &InputEvent, window, cx| {
                let value = state.read(cx).value().to_string();
                this.on_field_event(event_key.clone(), event, value, window, cx);
            },
        );
        self.fields.inputs.insert(
            key,
            Field {
                state: state.clone(),
                placeholder: placeholder_text,
                _subscription: subscription,
            },
        );
        state
    }

    /// The input for `key`, showing `value`.
    pub(super) fn bound_input(
        &mut self,
        key: FieldKey,
        placeholder: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let state = self.input(key, placeholder, window, cx);
        if state.read(cx).value().as_ref() != value {
            let value = value.to_owned();
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        state
    }

    /// Whether the input for `key` has focus.
    pub(super) fn input_focused(
        &self,
        key: &FieldKey,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.fields
            .inputs
            .get(key)
            .is_some_and(|field| field.state.read(cx).focus_handle(cx).is_focused(window))
    }

    /// The current text of an input, if it exists.
    pub(super) fn input_value(&self, key: &FieldKey, cx: &Context<Self>) -> Option<String> {
        self.fields
            .inputs
            .get(key)
            .map(|field| field.state.read(cx).value().to_string())
    }

    /// The input for `key`, if it was created.
    pub(super) fn input_entity(&self, key: &FieldKey) -> Option<Entity<InputState>> {
        self.fields.inputs.get(key).map(|field| field.state.clone())
    }

    /// Focus the input for `key`.
    pub(super) fn focus_input(&self, key: &FieldKey, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(field) = self.fields.inputs.get(key) {
            let handle = field.state.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// The code editor for `key` in `language` (`json`, `html` for XML, `text`).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn editor(
        &mut self,
        key: EditorKey,
        language: &'static str,
        value: &str,
        read_only: bool,
        line_numbers: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<EditorState> {
        // Every code editor takes Monaco's colors and the shared grammars
        // (`util::code_editor`), restyled when the theme mode changes.
        let dark = cx.theme().mode.is_dark();
        if let Some(editor) = self.fields.editors.get_mut(&key)
            && editor.language == language
        {
            let state = editor.state.clone();
            if editor.dark != dark {
                editor.dark = dark;
                state.update(cx, |state, _| {
                    state.set_editor_style(editor_style(dark, language))
                });
            }
            if editor.synced.borrow().as_str() != value {
                *editor.synced.borrow_mut() = value.to_owned();
                let value = value.to_owned();
                state.update(cx, |state, cx| state.set_value(value, window, cx));
            }
            return state;
        }
        let initial = value.to_owned();
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language(language)
                .line_number(line_numbers)
                .folding(line_numbers)
                .soft_wrap(true)
                .default_value(initial);
            state.set_highlighter_factory(
                crate::util::code_editor::highlighter_factory(
                    crate::util::code_editor::BracketPools::Shared,
                ),
                cx,
            );
            state.set_editor_style(editor_style(dark, language));
            state.set_readonly(read_only, cx);
            state
        });
        let event_key = key.clone();
        let synced = Rc::new(RefCell::new(value.to_owned()));
        let heard = synced.clone();
        let subscription = cx.subscribe_in(
            &state,
            window,
            move |this, state: &Entity<EditorState>, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = state.read(cx).value().to_string();
                    *heard.borrow_mut() = value.clone();
                    this.on_editor_change(event_key.clone(), value, window, cx);
                }
            },
        );
        self.fields.editors.insert(
            key,
            Editor {
                state: state.clone(),
                _subscription: subscription,
                language,
                dark,
                synced,
            },
        );
        state
    }
}

/// Monaco's colors and its folding chevrons on hover.
fn editor_style(
    dark: bool,
    language: &'static str,
) -> muxus_ui::gpui_kit::base::input::InputEditorStyle {
    muxus_ui::gpui_kit::base::input::InputEditorStyle {
        fold_icon_renderer: Some(crate::util::code_editor::mouseover_fold_icons(dark)),
        ..muxus_ui::syntax::code_editor_style(dark, language)
    }
}
