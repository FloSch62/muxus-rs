//! MUI `Autocomplete` with one value: an outlined text field whose option
//! list opens under it, filtered by what was typed.
//!
//! The behaviour is `useAutocomplete`'s ([`AutocompleteModel`], tested
//! without a window): a click into the field toggles the list (while it is
//! open only an empty field closes it), typing opens and filters it, the
//! arrows move the highlight (wrapping), Page Up/Down jump five options,
//! Home/End go to the ends (not in `freeSolo`, where they move the caret),
//! Enter picks the highlighted option, Escape closes the list without
//! reaching the dialog, leaving the field closes it. The default filter
//! (`createFilterOptions`) keeps options whose label contains the text,
//! ignoring case and accents; a field still showing the picked option lists
//! everything until the user types.
//!
//! The look (`Autocomplete.js` under the theme's `MuiAutocomplete` paper
//! override): the end adornment sits 9px from the right edge, a clear button
//! (4px padding, small `Close` icon) shown while the field is hovered or
//! focused and has text, and outside `freeSolo` the turning `ArrowDropDown`
//! (2px padding); the input makes room for them (39px, 65px for both). The
//! list is a paper as wide as the field right under it (10px corners,
//! divider border, the menu shadow) with 8px of padding, options of body1 at
//! 6px × 16px, at most 40% of the window tall; "Loading…" or "No options"
//! stand in for an empty list (a `freeSolo` field shows no list then).

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::base::input::{InputEvent, InputState};
use gpui_kit::{
    AnyElement, App, ElementId, Entity, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, RenderOnce, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, point,
};
use unicode_normalization::UnicodeNormalization;

use crate::icon::icon;
use crate::theme::{ActiveTheme, Theme};
use crate::widgets::button::IconButton;
use crate::widgets::form_field::FormField;
use crate::widgets::listbox::list_subheader;
use crate::widgets::menu::{self, MenuItem};
use crate::widgets::tooltip::AnchorBounds;
use crate::widgets::typography::{body1, text};

/// `pageSize`: how far Page Up/Down move the highlight.
pub const PAGE_SIZE: usize = 5;

/// One option of an [`Autocomplete`].
#[derive(Clone, Debug, PartialEq)]
pub struct AutocompleteOption {
    /// What a pick hands to the owner (the label unless set apart).
    pub value: String,
    pub label: SharedString,
    /// A second line under the label.
    pub detail: Option<SharedString>,
    /// The group heading it is listed under (`groupBy`); options of a group are adjacent.
    pub group: Option<SharedString>,
    /// An icon before the label.
    pub leading: Option<SharedString>,
}

impl AutocompleteOption {
    pub fn new(label: impl Into<SharedString>) -> Self {
        let label = label.into();
        Self {
            value: label.to_string(),
            label,
            detail: None,
            group: None,
            leading: None,
        }
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn group(mut self, group: impl Into<SharedString>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn leading(mut self, icon: impl Into<SharedString>) -> Self {
        self.leading = Some(icon.into());
        self
    }
}

/// `stripDiacritics` with `toLowerCase`: NFD, combining marks dropped.
pub fn fold(text: &str) -> String {
    text.nfd()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .collect::<String>()
        .to_lowercase()
}

/// MUI's default `filterOptions`: the indices of the labels containing
/// `input`, ignoring case and accents (no trimming); all of them for an
/// empty input.
pub fn filter_labels<'a>(labels: impl IntoIterator<Item = &'a str>, input: &str) -> Vec<usize> {
    let needle = fold(input);
    labels
        .into_iter()
        .enumerate()
        .filter(|(_, label)| needle.is_empty() || fold(label).contains(&needle))
        .map(|(index, _)| index)
        .collect()
}

/// What a key did to the autocomplete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    /// Not ours: let the input have it.
    Ignored,
    /// Handled (highlight moved, list opened or closed): stop it here.
    Handled,
    /// Enter on a highlighted option: pick the shown option at this index.
    Pick(usize),
    /// Enter in a `freeSolo` field with new text: the text is the value now.
    /// The key goes on (a form may submit).
    Commit,
}

/// `useAutocomplete`'s state for one value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutocompleteModel {
    pub open: bool,
    /// The highlighted index among the shown options (`highlightedIndex`, -1 as `None`).
    pub highlighted: Option<usize>,
    /// Nothing typed since the list opened (`inputPristine`).
    pub pristine: bool,
    /// The label of the option picked last (MUI's `value`).
    pub selected: Option<String>,
}

impl AutocompleteModel {
    /// `handleOpen`.
    pub fn open(&mut self) {
        if !self.open {
            self.open = true;
            self.pristine = true;
            self.highlighted = None;
        }
    }

    /// `handleClose`.
    pub fn close(&mut self) {
        self.open = false;
        self.highlighted = None;
    }

    /// The text changed by typing (`handleInputChange`): the list opens and filters.
    pub fn typed(&mut self) {
        self.open();
        self.pristine = false;
    }

    /// A press in the field (`handleInputMouseDown`): toggles the list, except that an
    /// open list with text in the field stays open.
    pub fn pressed(&mut self, input: &str) {
        if !self.open {
            self.open();
        } else if input.is_empty() {
            self.close();
        }
    }

    /// The popup indicator (`handlePopupIndicator`).
    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    /// The text the options are filtered by: none while the field still shows the
    /// picked option untouched.
    pub fn needle<'a>(&self, input: &'a str) -> &'a str {
        if self.pristine && self.selected.as_deref() == Some(input) {
            ""
        } else {
            input
        }
    }

    /// The shown options changed (`syncHighlightedIndex`): highlight the picked
    /// option if it is among them, nothing otherwise.
    pub fn sync(&mut self, shown: &[&str]) {
        self.highlighted = self
            .selected
            .as_deref()
            .and_then(|selected| shown.iter().position(|label| *label == selected));
    }

    /// A key while the field has the keyboard (`handleKeyDown`), with `count` options
    /// shown and `input` in the field.
    pub fn key(&mut self, key: &str, count: usize, free_solo: bool, input: &str) -> KeyOutcome {
        let was_open = self.open;
        match key {
            "down" | "up" | "pagedown" | "pageup" => {
                let diff: isize = match key {
                    "down" => 1,
                    "up" => -1,
                    "pagedown" => PAGE_SIZE as isize,
                    _ => -(PAGE_SIZE as isize),
                };
                // `changeHighlightedIndex` runs before `handleOpen`: a closed list only opens.
                if was_open {
                    self.highlighted = step(self.highlighted, diff, count);
                }
                self.open();
                KeyOutcome::Handled
            }
            "home" | "end" if was_open && !free_solo => {
                self.highlighted = if count == 0 {
                    None
                } else if key == "home" {
                    Some(0)
                } else {
                    Some(count - 1)
                };
                KeyOutcome::Handled
            }
            "enter" => {
                if was_open && let Some(index) = self.highlighted.filter(|&i| i < count) {
                    return KeyOutcome::Pick(index);
                }
                if free_solo && !input.is_empty() && self.selected.as_deref() != Some(input) {
                    self.selected = Some(input.to_owned());
                    self.close();
                    return KeyOutcome::Commit;
                }
                KeyOutcome::Ignored
            }
            "escape" if was_open => {
                self.close();
                KeyOutcome::Handled
            }
            _ => KeyOutcome::Ignored,
        }
    }

    /// An option was picked: it is the value and the list closes.
    pub fn picked(&mut self, label: &str) {
        self.selected = Some(label.to_owned());
        self.close();
    }
}

/// `getNextIndex` for a step of `diff` from `current` among `count` options: single
/// steps wrap around, page steps stop at the ends.
fn step(current: Option<usize>, diff: isize, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let max = count as isize - 1;
    let next = current.map_or(-1, |i| i as isize) + diff;
    let index = if next < 0 {
        if diff.abs() > 1 { 0 } else { max }
    } else if next > max {
        if diff.abs() > 1 { max } else { 0 }
    } else {
        next
    };
    Some(index as usize)
}

/// Called with the picked option (the field already shows its label).
pub type PickHandler = Rc<dyn Fn(&AutocompleteOption, &mut Window, &mut App)>;
/// Picks the shown option at an index.
type IndexHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
/// Called when the clear button empties the field.
pub type ClearHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The keyed state an autocomplete keeps between frames.
struct AutocompleteState {
    model: AutocompleteModel,
    anchor: AnchorBounds,
    scroll: ScrollHandle,
    /// The labels shown last frame (a change re-syncs the highlight).
    shown: Vec<SharedString>,
    /// Each shown option's position among the list's children (group labels count).
    rows: Vec<usize>,
    _input: Subscription,
}

/// See the module docs. The owner keeps the input (its own change handler sees the
/// typing) and builds the [`FormField`] around it.
#[derive(IntoElement)]
pub struct Autocomplete {
    id: ElementId,
    input: Entity<InputState>,
    field: FormField,
    options: Vec<AutocompleteOption>,
    free_solo: bool,
    loading: bool,
    disable_clearable: bool,
    on_pick: Option<PickHandler>,
    on_clear: Option<ClearHandler>,
}

impl Autocomplete {
    /// `field` is the outlined field showing `input` (`renderInput`).
    pub fn new(
        id: impl Into<ElementId>,
        input: &Entity<InputState>,
        field: FormField,
        options: Vec<AutocompleteOption>,
    ) -> Self {
        Self {
            id: id.into(),
            input: input.clone(),
            field,
            options,
            free_solo: false,
            loading: false,
            disable_clearable: false,
            on_pick: None,
            on_clear: None,
        }
    }

    /// `freeSolo`: any text is a value; no popup button, no "No options".
    pub fn free_solo(mut self) -> Self {
        self.free_solo = true;
        self
    }

    /// `loading`: "Loading…" while there are no options to show.
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// `disableClearable`.
    pub fn disable_clearable(mut self) -> Self {
        self.disable_clearable = true;
        self
    }

    pub fn on_pick(
        mut self,
        handler: impl Fn(&AutocompleteOption, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_pick = Some(Rc::new(handler));
        self
    }

    pub fn on_clear(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_clear = Some(Rc::new(handler));
        self
    }
}

/// The end adornment's geometry: where the input text ends (from the outline's
/// right edge) for the buttons shown.
fn input_end(clear: bool, popup: bool) -> f32 {
    // `paddingRight` 26 + 4 + 9 (one button) or 52 + 4 + 9 (both), else the outlined
    // root's 9px; plus the input's own 4px.
    match (clear, popup) {
        (true, true) => 65. + 4.,
        (false, false) => 9. + 4.,
        _ => 39. + 4.,
    }
}

impl RenderOnce for Autocomplete {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let input = self.input.clone();
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| {
            let subscription =
                cx.subscribe(&input, |this: &mut AutocompleteState, _, event, cx| {
                    match event {
                        InputEvent::Change => this.model.typed(),
                        InputEvent::Blur => this.model.close(),
                        _ => return,
                    }
                    cx.notify();
                });
            AutocompleteState {
                model: AutocompleteModel::default(),
                anchor: AnchorBounds::default(),
                scroll: ScrollHandle::new(),
                shown: Vec::new(),
                rows: Vec::new(),
                _input: subscription,
            }
        });
        let theme = *cx.theme();
        let focus = self.input.read(cx).focus_handle(cx);
        let focused = focus.is_focused(window);
        let value = self.input.read(cx).value().to_string();

        // Filter, and keep the highlight in step with what is shown.
        let options = Rc::new(self.options);
        let (shown, model, anchor, scroll) = state.update(cx, |state, _| {
            if state.model.open && !focused {
                state.model.close();
            }
            let needle = state.model.needle(&value).to_owned();
            let shown = filter_labels(options.iter().map(|o| o.label.as_ref()), &needle);
            if state.model.open {
                let labels: Vec<SharedString> =
                    shown.iter().map(|&i| options[i].label.clone()).collect();
                if labels != state.shown {
                    let refs: Vec<&str> = labels.iter().map(|l| l.as_ref()).collect();
                    state.model.sync(&refs);
                    state.shown = labels;
                }
            } else {
                state.shown.clear();
            }
            (
                shown,
                state.model.clone(),
                state.anchor.clone(),
                state.scroll.clone(),
            )
        });
        let shown = Rc::new(shown);

        let has_clear = !self.disable_clearable && !value.is_empty();
        let has_popup = !self.free_solo;
        let group: SharedString = format!("autocomplete-{:?}", self.id).into();

        // The end adornment: clear button and popup indicator.
        let mut field = self.field.anchor(&anchor);
        if has_clear || has_popup {
            let mut end = div().flex().flex_none().items_center().mr(px(7.));
            let mut width = 0.;
            if has_clear {
                width += 4. * 2. + crate::icon::SMALL - 2.;
                let clear_state = state.clone();
                let clear_input = self.input.clone();
                let on_clear = self.on_clear.clone();
                let button = IconButton::new("autocomplete-clear", "Close")
                    .padding(4.)
                    .skip_tab()
                    .on_click(move |_, window, cx| {
                        clear_input.update(cx, |input, cx| input.set_value("", window, cx));
                        clear_state.update(cx, |state, cx| {
                            state.model.selected = None;
                            cx.notify();
                        });
                        let focus = clear_input.read(cx).focus_handle(cx);
                        window.focus(&focus, cx);
                        if let Some(on_clear) = &on_clear {
                            on_clear(window, cx);
                        }
                    });
                let mut slot = div().flex().mr(px(-2.)).child(button);
                if !focused {
                    slot = slot.invisible().group_hover(group.clone(), |s| s.visible());
                }
                end = end.child(slot);
            }
            if has_popup {
                width += 2. * 2. + crate::icon::MEDIUM - 2.;
                let toggle_state = state.clone();
                let toggle_focus = focus.clone();
                end = end.child(
                    div().flex().mr(px(-2.)).child(
                        IconButton::new("autocomplete-popup", "ArrowDropDown")
                            .padding(2.)
                            .skip_tab()
                            .icon_size(crate::icon::MEDIUM)
                            .turned(model.open)
                            .on_click(move |_, window, cx| {
                                toggle_state.update(cx, |state, cx| {
                                    state.model.toggle();
                                    cx.notify();
                                });
                                window.focus(&toggle_focus, cx);
                            }),
                    ),
                );
            }
            // The field puts its end adornment 8px after the input and 2px from the
            // outline (`end_edge`); the margin makes up MUI's padding.
            let gap = input_end(has_clear, has_popup) - 2. - 7. - 8. - width;
            field = field.end(end.ml(px(gap.max(0.)))).end_edge();
        } else {
            field = field.padding_right(input_end(false, false));
        }
        let press_state = state.clone();
        let press_input = self.input.clone();
        let press_focus = focus.clone();
        field = field.on_click(move |_, window, cx| {
            let text = press_input.read(cx).value().to_string();
            press_state.update(cx, |state, cx| {
                state.model.pressed(&text);
                cx.notify();
            });
            window.focus(&press_focus, cx);
        });

        // A pick: the field shows the label, the list closes, the owner hears of it.
        let pick: IndexHandler = {
            let state = state.clone();
            let input = self.input.clone();
            let options = options.clone();
            let shown = shown.clone();
            let on_pick = self.on_pick.clone();
            Rc::new(move |index, window, cx| {
                let Some(option) = shown.get(index).map(|&i| options[i].clone()) else {
                    return;
                };
                input.update(cx, |input, cx| {
                    input.set_value(option.label.to_string(), window, cx)
                });
                state.update(cx, |state, cx| {
                    state.model.picked(&option.label);
                    cx.notify();
                });
                if let Some(on_pick) = &on_pick {
                    on_pick(&option, window, cx);
                }
            })
        };

        let key_state = state.clone();
        let key_input = self.input.clone();
        let key_pick = pick.clone();
        let free_solo = self.free_solo;
        let count = shown.len();
        let mut root = div()
            .id(self.id)
            .group(group)
            .relative()
            .w_full()
            .capture_key_down(move |event: &KeyDownEvent, window, cx| {
                let modifiers = event.keystroke.modifiers;
                if modifiers.control || modifiers.alt || modifiers.platform {
                    return;
                }
                let text = key_input.read(cx).value().to_string();
                let key = event.keystroke.key.clone();
                let (outcome, highlighted, rows, scroll) = key_state.update(cx, |state, cx| {
                    let outcome = state.model.key(&key, count, free_solo, &text);
                    if outcome != KeyOutcome::Ignored {
                        cx.notify();
                    }
                    (
                        outcome,
                        state.model.highlighted,
                        state.rows.clone(),
                        state.scroll.clone(),
                    )
                });
                match outcome {
                    KeyOutcome::Ignored | KeyOutcome::Commit => {}
                    KeyOutcome::Handled => {
                        cx.stop_propagation();
                        if let Some(row) = highlighted.and_then(|i| rows.get(i)) {
                            scroll.scroll_to_item(*row);
                        }
                    }
                    KeyOutcome::Pick(index) => {
                        cx.stop_propagation();
                        key_pick(index, window, cx);
                    }
                }
            })
            .child(field);

        let has_content = !shown.is_empty() || self.loading || !self.free_solo;
        if model.open
            && has_content
            && let Some(bounds) = anchor.get()
        {
            let status = if shown.is_empty() {
                Some(if self.loading {
                    "Loading…"
                } else {
                    "No options"
                })
            } else {
                None
            };
            let mut items = Vec::new();
            let mut rows = Vec::new();
            let mut last_group: Option<SharedString> = None;
            for (index, &option_index) in shown.iter().enumerate() {
                let option = &options[option_index];
                if option.group.is_some() && option.group != last_group {
                    last_group = option.group.clone();
                    items.push(list_subheader(
                        &theme,
                        last_group.clone().unwrap_or_default(),
                    ));
                }
                rows.push(items.len());
                items.push(option_row(
                    &theme,
                    option,
                    index,
                    model.highlighted == Some(index),
                    model.selected.as_deref() == Some(option.label.as_ref()),
                    &state,
                    &pick,
                ));
            }
            state.update(cx, |state, _| state.rows = rows);
            let max_height = crate::zoom::unzoom(window.viewport_size().height) * 0.4;
            let dismiss_state = state.clone();
            let paper = menu::paper(&theme)
                .py(px(0.))
                .min_w(px(0.))
                .w(bounds.size.width)
                .overflow_hidden()
                .child(match status {
                    Some(status) => div()
                        .px(px(16.))
                        .py(px(14.))
                        .child(text(status, body1(), theme.palette.text.secondary))
                        .into_any_element(),
                    None => div()
                        .id("autocomplete-listbox")
                        .flex()
                        .flex_col()
                        .py(px(8.))
                        .max_h(px(max_height))
                        .overflow_y_scroll()
                        .track_scroll(&scroll)
                        .children(items)
                        .into_any_element(),
                });
            let position = point(bounds.origin.x, bounds.origin.y + bounds.size.height);
            root = root.child(
                menu::popup(
                    position,
                    move |_, cx| {
                        dismiss_state.update(cx, |state, cx| {
                            state.model.close();
                            cx.notify();
                        })
                    },
                    paper,
                )
                .non_modal(),
            );
        }
        root
    }
}

/// One option: a menu item (body1, 6px × 16px) that highlights under the pointer.
fn option_row(
    theme: &Theme,
    option: &AutocompleteOption,
    index: usize,
    highlighted: bool,
    selected: bool,
    state: &Entity<AutocompleteState>,
    pick: &IndexHandler,
) -> AnyElement {
    let pick = pick.clone();
    let mut item = MenuItem::new(
        SharedString::from(format!("autocomplete-option-{index}")),
        option.label.clone(),
    )
    .highlighted(highlighted)
    .selected(selected)
    .on_click(move |_, window, cx| pick(index, window, cx));
    if let Some(detail) = &option.detail {
        item = item.detail(detail.clone());
    }
    if let Some(name) = &option.leading {
        item = item.leading(icon(name, 16., theme.palette.text.secondary));
    }
    let hover_state = state.clone();
    div()
        .id(SharedString::from(format!("autocomplete-row-{index}")))
        .flex()
        .flex_col()
        // `handleOptionMouseMove`: the pointer moves the highlight.
        .on_mouse_move(move |_, _, cx| {
            hover_state.update(cx, |state, cx| {
                if state.model.highlighted != Some(index) {
                    state.model.highlighted = Some(index);
                    cx.notify();
                }
            });
        })
        .child(item)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FONTS: [&str; 5] = [
        "JetBrains Mono",
        "Fira Code",
        "Cascadia Code",
        "Ubuntu Mono",
        "Noto Sans Mono",
    ];

    #[test]
    fn filters_by_substring_ignoring_case_and_accents() {
        assert_eq!(filter_labels(FONTS, ""), vec![0, 1, 2, 3, 4]);
        assert_eq!(filter_labels(FONTS, "mono"), vec![0, 3, 4]);
        assert_eq!(filter_labels(FONTS, "CODE"), vec![1, 2]);
        assert_eq!(filter_labels(["Café", "Cafe", "Tea"], "cafe"), vec![0, 1]);
        assert_eq!(filter_labels(["Café", "Cafe"], "CAFÉ"), vec![0, 1]);
        // MUI does not trim: a trailing space is part of the needle.
        assert_eq!(filter_labels(FONTS, "mono "), Vec::<usize>::new());
        assert_eq!(filter_labels(FONTS, "zzz"), Vec::<usize>::new());
    }

    #[test]
    fn a_closed_list_only_opens_on_the_first_arrow() {
        let mut model = AutocompleteModel::default();
        assert_eq!(model.key("down", 3, false, ""), KeyOutcome::Handled);
        assert!(model.open);
        assert_eq!(model.highlighted, None);
        model.key("down", 3, false, "");
        assert_eq!(model.highlighted, Some(0));
    }

    #[test]
    fn arrows_wrap_and_pages_stop_at_the_ends() {
        let mut model = AutocompleteModel::default();
        model.open();
        model.key("up", 3, false, "");
        assert_eq!(model.highlighted, Some(2));
        model.key("down", 3, false, "");
        assert_eq!(model.highlighted, Some(0));
        model.key("up", 3, false, "");
        assert_eq!(model.highlighted, Some(2));
        let mut model = AutocompleteModel::default();
        model.open();
        model.key("pagedown", 12, false, "");
        assert_eq!(model.highlighted, Some(4));
        model.key("pagedown", 12, false, "");
        model.key("pagedown", 12, false, "");
        assert_eq!(model.highlighted, Some(11));
        model.key("pageup", 12, false, "");
        assert_eq!(model.highlighted, Some(6));
        model.key("pageup", 12, false, "");
        model.key("pageup", 12, false, "");
        assert_eq!(model.highlighted, Some(0));
        // No options, no highlight.
        model.key("down", 0, false, "");
        assert_eq!(model.highlighted, None);
    }

    #[test]
    fn home_and_end_belong_to_the_caret_in_free_solo() {
        let mut model = AutocompleteModel::default();
        model.open();
        assert_eq!(model.key("end", 4, false, ""), KeyOutcome::Handled);
        assert_eq!(model.highlighted, Some(3));
        assert_eq!(model.key("home", 4, false, ""), KeyOutcome::Handled);
        assert_eq!(model.highlighted, Some(0));
        let mut model = AutocompleteModel::default();
        model.open();
        assert_eq!(model.key("end", 4, true, "x"), KeyOutcome::Ignored);
        // Closed: Home and End are the caret's too.
        let mut model = AutocompleteModel::default();
        assert_eq!(model.key("home", 4, false, ""), KeyOutcome::Ignored);
    }

    #[test]
    fn enter_picks_the_highlight_or_commits_free_text() {
        let mut model = AutocompleteModel::default();
        model.open();
        model.key("down", 3, true, "Fi");
        assert_eq!(model.key("enter", 3, true, "Fi"), KeyOutcome::Pick(0));
        model.picked("Fira Code");
        assert!(!model.open);
        assert_eq!(model.selected.as_deref(), Some("Fira Code"));
        // Free text: the value becomes what was typed.
        assert_eq!(model.key("enter", 0, true, "My Font"), KeyOutcome::Commit);
        assert_eq!(model.selected.as_deref(), Some("My Font"));
        // Typed text that already is the value: nothing new.
        assert_eq!(model.key("enter", 0, true, "My Font"), KeyOutcome::Ignored);
        // Without freeSolo, Enter without a highlight does nothing.
        let mut model = AutocompleteModel::default();
        model.open();
        assert_eq!(model.key("enter", 3, false, "x"), KeyOutcome::Ignored);
    }

    #[test]
    fn escape_closes_an_open_list_only() {
        let mut model = AutocompleteModel::default();
        assert_eq!(model.key("escape", 3, false, ""), KeyOutcome::Ignored);
        model.open();
        assert_eq!(model.key("escape", 3, false, ""), KeyOutcome::Handled);
        assert!(!model.open);
    }

    #[test]
    fn a_field_showing_the_pick_lists_everything_until_typing() {
        let mut model = AutocompleteModel::default();
        // Nothing picked yet: the field's text filters (as for the font family).
        model.open();
        assert_eq!(model.needle("Fira"), "Fira");
        model.picked("Fira Code");
        model.open();
        assert_eq!(model.needle("Fira Code"), "");
        model.sync(&FONTS);
        assert_eq!(model.highlighted, Some(1));
        model.typed();
        assert_eq!(model.needle("Fira Cod"), "Fira Cod");
        model.sync(&["Fira Code"]);
        assert_eq!(model.highlighted, Some(0));
        model.sync(&["Ubuntu Mono"]);
        assert_eq!(model.highlighted, None);
    }

    #[test]
    fn a_press_toggles_except_an_open_list_with_text() {
        let mut model = AutocompleteModel::default();
        model.pressed("abc");
        assert!(model.open);
        model.pressed("abc");
        assert!(model.open);
        model.pressed("");
        assert!(!model.open);
        model.toggle();
        assert!(model.open);
        model.toggle();
        assert!(!model.open);
    }

    #[test]
    fn the_input_makes_room_for_the_buttons() {
        assert_eq!(input_end(false, false), 13.);
        assert_eq!(input_end(true, false), 43.);
        assert_eq!(input_end(false, true), 43.);
        assert_eq!(input_end(true, true), 69.);
    }
}
