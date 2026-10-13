//! Form plumbing shared by every editor view: text inputs bound to a draft,
//! selects with their menus, and free-solo autocompletes.

use std::rc::Rc;

use muxus_ui::gpui_kit::base::input::InputState;
use muxus_ui::gpui_kit::{
    AnyElement, Context, Entity, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, SharedString, Styled, Window, div,
};
use muxus_ui::theme::ActiveTheme;

use muxus_ui::widgets::MenuItem;
use muxus_ui::widgets::form_field::{FieldBody, FormField};
use muxus_ui::widgets::listbox::{list_subheader, listbox_popup};

use crate::features::host_editor::widgets::inputs::{Correction, InputCache, Menus};

/// How tall an open select menu may grow before it scrolls.
const SELECT_MENU_MAX_HEIGHT: f32 = 390.;
/// How tall an autocomplete's list may grow before it scrolls.
const AUTOCOMPLETE_MAX_HEIGHT: f32 = 352.;

/// A view that keeps form inputs and menus.
pub trait Form: Sized + 'static {
    fn inputs(&mut self) -> &mut InputCache;
    fn menus(&mut self) -> &mut Menus;
    /// The vault changed (a secret was created from a field): show its new secrets.
    fn vault_changed(&mut self, _status: muxus_core::api::PasswordVaultStatus) {}
}

/// The input of `key`, created with `initial` the first time.
pub fn text_input<V: Form>(
    view: &mut V,
    key: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<V>,
    initial: impl Into<String>,
    on_change: impl Fn(&mut V, String, &mut Context<V>) -> Correction + 'static,
) -> Entity<InputState> {
    let initial = initial.into();
    view.inputs()
        .get(key, window, cx, move || initial, on_change)
}

/// Like [`text_input`], with Enter handled.
pub fn text_input_enter<V: Form>(
    view: &mut V,
    key: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<V>,
    initial: impl Into<String>,
    on_change: impl Fn(&mut V, String, &mut Context<V>) -> Correction + 'static,
    on_enter: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> Entity<InputState> {
    let initial = initial.into();
    view.inputs()
        .get_with_enter(key, window, cx, move || initial, on_change, on_enter)
}

/// One choice of a select.
pub struct Choice<T> {
    pub value: T,
    pub label: SharedString,
    /// Kept selectable as the current value but not listed (MUI's hidden
    /// "Multiple values" item).
    pub hidden: bool,
    pub disabled: bool,
    /// A group heading instead of a choice.
    pub header: bool,
}

impl<T> Choice<T> {
    pub fn new(value: T, label: impl Into<SharedString>) -> Self {
        Self {
            value,
            label: label.into(),
            hidden: false,
            disabled: false,
            header: false,
        }
    }

    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    pub fn header(value: T, label: impl Into<SharedString>) -> Self {
        Self {
            value,
            label: label.into(),
            hidden: false,
            disabled: true,
            header: true,
        }
    }
}

/// A `TextField select`: the field showing the current choice and, while
/// open, its menu. `display` overrides what the closed field shows.
#[allow(clippy::too_many_arguments)]
pub fn select<V: Form, T: Clone + PartialEq + 'static>(
    view: &mut V,
    key: &str,
    field: impl FnOnce(FieldBody) -> FormField,
    value: &T,
    choices: Vec<Choice<T>>,
    display: Option<FieldBody>,
    _window: &mut Window,
    cx: &mut Context<V>,
    on_select: impl Fn(&mut V, T, &mut Context<V>) + 'static,
) -> AnyElement {
    let anchor = view.menus().anchor(key);
    let open = view.menus().is_open(key);
    let current = choices
        .iter()
        .find(|choice| !choice.header && &choice.value == value)
        .map(|choice| choice.label.clone())
        .unwrap_or_default();
    let body = display.unwrap_or(FieldBody::Value(current));
    let entity = cx.entity().downgrade();
    let toggle_key = key.to_owned();
    let field = field(body)
        .select(open)
        .anchor(&anchor)
        .on_click(move |_, _, cx| {
            let _ = entity.update(cx, |this, cx| {
                this.menus().toggle(&toggle_key);
                cx.notify();
            });
        });
    let mut root = div().w_full().child(field);
    if open {
        let theme = *cx.theme();
        let on_select = Rc::new(on_select);
        let options: Vec<AnyElement> = choices
            .into_iter()
            .filter(|choice| !choice.hidden)
            .enumerate()
            .map(|(index, choice)| {
                if choice.header {
                    return list_subheader(&theme, choice.label);
                }
                let selected = &choice.value == value;
                let entity = cx.entity().downgrade();
                let on_select = on_select.clone();
                let picked = choice.value.clone();
                MenuItem::new(
                    SharedString::from(format!("menu-option-{index}")),
                    choice.label,
                )
                .selected(selected)
                .disabled(choice.disabled)
                .on_click(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.menus().close();
                        on_select(this, picked.clone(), cx);
                        cx.notify();
                    });
                })
                .into_any_element()
            })
            .collect();
        let entity = cx.entity().downgrade();
        if let Some(popup) = listbox_popup(
            &theme,
            &anchor,
            options,
            SELECT_MENU_MAX_HEIGHT,
            move |_, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.menus().close();
                    cx.notify();
                });
            },
        ) {
            root = root.child(popup);
        }
    }
    root.into_any_element()
}

/// One suggestion of an autocomplete.
#[derive(Clone)]
pub struct Suggestion {
    /// What a pick hands to the handler.
    pub value: String,
    pub label: SharedString,
    pub detail: Option<SharedString>,
    /// The group heading it is listed under (`groupBy`).
    pub group: Option<SharedString>,
    pub leading: Option<&'static str>,
}

impl Suggestion {
    pub fn new(value: impl Into<String>, label: impl Into<SharedString>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            detail: None,
            group: None,
            leading: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn group(mut self, group: impl Into<SharedString>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn leading(mut self, icon: &'static str) -> Self {
        self.leading = Some(icon);
        self
    }
}

/// Called with an input's new text; may correct it.
pub type InputHandler<V> = Rc<dyn Fn(&mut V, String, &mut Context<V>) -> Correction>;
/// Called with the value of a picked suggestion.
pub type PickHandler<V> = Rc<dyn Fn(&mut V, String, &mut Window, &mut Context<V>)>;
/// Called on Enter.
pub type EnterHandler<V> = Rc<dyn Fn(&mut V, &mut Window, &mut Context<V>)>;

/// How an autocomplete reports what happens in it.
pub struct AutocompleteHandlers<V> {
    /// The text changed (typing).
    pub on_input: InputHandler<V>,
    /// A suggestion was picked.
    pub on_pick: PickHandler<V>,
    /// Enter with no suggestion highlighted (free solo submit).
    pub on_enter: Option<EnterHandler<V>>,
}

/// A free-solo `Autocomplete`: a text field whose list of suggestions opens
/// while typing (or on a click into it) and filters by what was typed. The
/// list of an untouched field shows every suggestion.
#[allow(clippy::too_many_arguments)]
pub fn autocomplete<V: Form>(
    view: &mut V,
    key: &str,
    initial: String,
    suggestions: Vec<Suggestion>,
    field: impl FnOnce(Entity<InputState>) -> FormField,
    handlers: AutocompleteHandlers<V>,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let menu_key = format!("{key}-list");
    let anchor = view.menus().anchor(&menu_key);
    let on_input = handlers.on_input.clone();
    let open_key = menu_key.clone();
    let input = {
        let on_enter = handlers.on_enter.clone();
        view.inputs().get_with_enter(
            SharedString::from(key.to_owned()),
            window,
            cx,
            move || initial,
            move |this: &mut V, value, cx| {
                this.menus().open(&open_key);
                this.menus().typed = true;
                on_input(this, value, cx)
            },
            move |this, window, cx| {
                if let Some(on_enter) = &on_enter {
                    on_enter(this, window, cx);
                }
            },
        )
    };
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    let mut open = view.menus().is_open(&menu_key);
    if open && !focused {
        view.menus().close();
        open = false;
    }

    let needle = if view.menus().typed {
        input.read(cx).value().trim().to_lowercase()
    } else {
        String::new()
    };
    let shown: Vec<Suggestion> = suggestions
        .into_iter()
        .filter(|s| needle.is_empty() || s.label.to_lowercase().contains(&needle))
        .collect();
    let highlighted = view.menus().highlighted;

    let click_key = menu_key.clone();
    let entity = cx.entity().downgrade();
    let field = field(input.clone())
        .anchor(&anchor)
        .on_click(move |_, _, cx| {
            let _ = entity.update(cx, |this, cx| {
                this.menus().open(&click_key);
                cx.notify();
            });
        });

    let count = shown.len();
    let nav_key = menu_key.clone();
    let picks: Vec<String> = shown.iter().map(|s| s.value.clone()).collect();
    let on_pick = handlers.on_pick.clone();
    let mut root = div()
        .w_full()
        .capture_key_down(
            cx.listener(move |this: &mut V, event: &KeyDownEvent, window, cx| {
                let menus = this.menus();
                let open = menus.is_open(&nav_key);
                match event.keystroke.key.as_str() {
                    "down" => {
                        if !open {
                            menus.open(&nav_key);
                        }
                        if count > 0 {
                            menus.highlighted =
                                Some(menus.highlighted.map_or(0, |i| (i + 1) % count));
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }
                    "up" if open && count > 0 => {
                        menus.highlighted = Some(
                            menus
                                .highlighted
                                .map_or(count - 1, |i| (i + count - 1) % count),
                        );
                        cx.stop_propagation();
                        cx.notify();
                    }
                    "escape" if open => {
                        menus.close();
                        cx.stop_propagation();
                        cx.notify();
                    }
                    "enter" if open => {
                        if let Some(index) = menus.highlighted
                            && let Some(value) = picks.get(index).cloned()
                        {
                            menus.close();
                            on_pick(this, value, window, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }
                    _ => {}
                }
            }),
        )
        .child(field);

    if open && count > 0 {
        let theme = *cx.theme();
        let p = theme.palette;
        let mut options = Vec::new();
        let mut last_group: Option<SharedString> = None;
        for (index, suggestion) in shown.into_iter().enumerate() {
            if suggestion.group.is_some() && suggestion.group != last_group {
                last_group = suggestion.group.clone();
                options.push(list_subheader(
                    &theme,
                    suggestion.group.clone().unwrap_or_default(),
                ));
            }
            let entity = cx.entity().downgrade();
            let on_pick = handlers.on_pick.clone();
            let value = suggestion.value.clone();
            let mut option = MenuItem::new(
                SharedString::from(format!("menu-option-{index}")),
                suggestion.label,
            )
            .highlighted(highlighted == Some(index))
            .on_click(move |_, window, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.menus().close();
                    on_pick(this, value.clone(), window, cx);
                    cx.notify();
                });
            });
            if let Some(detail) = suggestion.detail {
                option = option.detail(detail);
            }
            if let Some(icon) = suggestion.leading {
                option = option.leading(muxus_ui::icon::icon(icon, 16., p.text.secondary));
            }
            options.push(option.into_any_element());
        }
        let entity = cx.entity().downgrade();
        if let Some(popup) = listbox_popup(
            &theme,
            &anchor,
            options,
            AUTOCOMPLETE_MAX_HEIGHT,
            move |_, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.menus().close();
                    cx.notify();
                });
            },
        ) {
            root = root.child(popup);
        }
    }
    root.into_any_element()
}

/// `FolderPathField`: every existing folder path (ancestors included) as a
/// suggestion, and a new path typed with `/` between levels.
#[allow(clippy::too_many_arguments)]
pub fn folder_field<V: Form>(
    view: &mut V,
    key: &str,
    label: &str,
    value: &str,
    known: Vec<String>,
    helper: Option<String>,
    placeholder: Option<&str>,
    error: bool,
    window: &mut Window,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, String, &mut Context<V>) + 'static,
) -> AnyElement {
    use crate::hosts::tree::{folder_label, folder_parent_path};
    let suggestions: Vec<Suggestion> = known
        .into_iter()
        .map(|path| {
            let parent = folder_parent_path(&path);
            let label = if parent.is_empty() {
                folder_label(&path)
            } else {
                format!(
                    "{} / {}",
                    parent.split('/').collect::<Vec<_>>().join(" / "),
                    folder_label(&path)
                )
            };
            Suggestion::new(path, label)
        })
        .collect();
    let on_change = Rc::new(on_change);
    let (typed, picked) = (on_change.clone(), on_change);
    let pick_key = key.to_owned();
    let label = label.to_owned();
    let shrink = placeholder.is_some();
    let placeholder = placeholder.unwrap_or("e.g. Production/EU").to_owned();
    let helper = helper
        .unwrap_or_else(|| "Use / to nest, e.g. Production/EU. Leave empty for no folder.".into());
    autocomplete(
        view,
        key,
        value.to_owned(),
        suggestions,
        move |input| {
            FormField::input(SharedString::from(format!("{label}-field")), &input)
                .label(label.clone())
                .placeholder(placeholder.clone())
                .shrink(shrink)
                .helper(helper.clone())
                .error(error)
                .padding_right(39.)
        },
        AutocompleteHandlers {
            on_input: Rc::new(move |this: &mut V, text, cx| {
                typed(this, text, cx);
                None
            }),
            on_pick: Rc::new(move |this: &mut V, path, window, cx| {
                this.inputs().set(&pick_key, &path, window, cx);
                picked(this, path, cx);
            }),
            on_enter: None,
        },
        window,
        cx,
    )
}

/// A password input: like [`text_input`], with the text masked.
pub fn password_input<V: Form>(
    view: &mut V,
    key: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<V>,
    initial: impl Into<String>,
    on_change: impl Fn(
        &mut V,
        String,
        &mut Context<V>,
    ) -> crate::features::host_editor::widgets::inputs::Correction
    + 'static,
) -> Entity<InputState> {
    let input = text_input(view, key, window, cx, initial, on_change);
    if !input.read(cx).presentation().is_masked() {
        input.update(cx, |state, cx| state.set_masked(true, window, cx));
    }
    input
}
