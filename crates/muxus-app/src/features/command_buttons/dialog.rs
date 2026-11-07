//! The command button editor (`client/src/components/CommandButtonsDialog.tsx`):
//! whether and where the command bar shows, the groups (issue #210) down the
//! left, and the selected group's buttons as cards: label, color, command or
//! vault secret, Enter, group, order, duplicate and delete. Every change is
//! written to the preferences at once, as the client's store does.

use std::collections::HashMap;
use std::rc::Rc;

use muxus_core::api::PasswordVaultStatus;
use muxus_store::prefs::command_buttons::{
    COMMAND_BUTTON_COLORS, CommandButton, CommandButtonColor, CommandButtonGroup,
    DEFAULT_COMMAND_GROUP_ID, DEFAULT_COMMAND_GROUP_NAME, MAX_COMMAND_BUTTON_GROUPS,
    MAX_COMMAND_BUTTONS, command_button_group_of, command_buttons_in_group,
    duplicate_command_button, move_command_button, move_command_button_group,
    move_command_button_to_group, new_preference_id, unused_command_button_group_name,
    without_command_button_group,
};
use muxus_store::prefs::state::CommandBarPosition;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState, Textarea, TextareaState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, ScrollHandle,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::theme::Status;
use muxus_ui::theme::{ActiveTheme, TextVariant, Theme};
use muxus_ui::widgets::dialog::{dialog_actions, dialog_paper, dialog_title, modal};
use muxus_ui::widgets::form_control::{FormControl, FormControlLabel, label_with_caption};
use muxus_ui::widgets::form_field::{FieldBody, FormField};
use muxus_ui::widgets::menu::{self, MenuItem, popover_paper};
use muxus_ui::widgets::tabs::{self, Tab};
use muxus_ui::widgets::toggle_button::{ToggleButton, ToggleButtonGroup};
use muxus_ui::widgets::tooltip::{AnchorBounds, Placement, Tooltip, bounds_probe};
use muxus_ui::widgets::{Alert, Button, ButtonColor, Chip, IconButton, Select, TextField};

use super::model::{CommandButtons, prefs, update_prefs};
use super::palette::command_button_ink;
use crate::features::launcher::kit::{AskAnswer, AskDialog, AskRequest, text};
use crate::features::settings::vault_secrets::vault_secret_missing;
use crate::features::settings::view::appearance::keybinding_overrides;
use crate::features::settings::view::fields::style_textarea;
use crate::features::settings::view::shortcuts::set_chord_capture;
use crate::features::vault::secret_dialog::{
    VaultSecretDialog, VaultSecretEvent, open_vault_secret_dialog,
};
use crate::keymap::chords::{
    chord_from_event, chord_to_string, format_chord_string, is_bindable_chord, is_modifier_code,
};
use crate::keymap::command_buttons::{command_button_shortcut_conflict, shell_owns};
use crate::keymap::keystroke::chord_event;

/// What the editor asks the window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandButtonsEvent {
    Close,
    /// "Create it under Settings → Passwords".
    OpenPasswordSettings,
}

/// An open drop-down or popover of the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Popup {
    Color(String),
    Group(String),
    Secret(String),
}

#[derive(Clone, Debug)]
enum Pending {
    DeleteGroup(String),
    DeleteButton(String),
}

pub struct CommandButtonsDialog {
    model: Entity<CommandButtons>,
    selected_group: String,
    inputs: HashMap<String, Entity<InputState>>,
    /// Multi-line fields (a card's Command), by the same keys as `inputs`.
    textareas: HashMap<String, Entity<TextareaState>>,
    anchors: HashMap<String, AnchorBounds>,
    popup: Option<Popup>,
    ask: Option<(Entity<AskDialog>, Pending)>,
    adding_secret: Option<(Entity<VaultSecretDialog>, String)>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    subscriptions: Vec<Subscription>,
    /// The button whose shortcut is being recorded; one card at a time.
    recording_shortcut: Option<String>,
    /// Why the last chord pressed was refused, while recording.
    shortcut_problem: Option<String>,
}

impl EventEmitter<CommandButtonsEvent> for CommandButtonsDialog {}

impl Focusable for CommandButtonsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Open the editor on the group the bar shows (so "Manage" edits what you
/// were looking at).
pub fn open_command_buttons(
    model: Entity<CommandButtons>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<CommandButtonsDialog> {
    cx.new(|cx| CommandButtonsDialog::new(model, window, cx))
}

/// The caption under the group name (`groupCaption`).
pub fn group_caption(is_default: bool, host_count: usize) -> String {
    if host_count > 0 {
        return format!(
            "The bar switches to this group in sessions to {host_count} host{}.",
            if host_count == 1 { "" } else { "s" }
        );
    }
    if is_default {
        "Holds every command not filed under another group.".into()
    } else {
        "To switch to it for a host, choose it under Terminal appearance in the host editor.".into()
    }
}

/// What deleting a group says (`deleteGroup`'s description).
pub fn delete_group_description(count: usize, host_count: usize) -> String {
    let buttons = if count == 0 {
        "The group holds no command buttons.".to_owned()
    } else {
        format!(
            "Its {count} command button{} deleted with it.",
            if count == 1 { " is" } else { "s are" }
        )
    };
    let hosts = (host_count > 0).then(|| {
        format!(
            "{host_count} host{} that open{} it will show the group chosen in the bar instead.",
            if host_count == 1 { "" } else { "s" },
            if host_count == 1 { "s" } else { "" }
        )
    });
    match hosts {
        Some(hosts) => format!("{buttons} {hosts}"),
        None => buttons,
    }
}

/// A group's name as the tabs and menus show it.
fn group_label(group: &CommandButtonGroup) -> String {
    let name = muxus_core::js::js_trim(&group.name);
    if name.is_empty() {
        "Untitled group".into()
    } else {
        name.to_owned()
    }
}

impl CommandButtonsDialog {
    pub fn new(model: Entity<CommandButtons>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let selected_group = model.read(cx).active_group(cx).shown_id;
        let mut subscriptions = vec![cx.observe(&model, |_, _, cx| cx.notify())];
        if let Some(prefs) = crate::features::settings::SettingsPrefs::global(cx) {
            subscriptions.push(cx.observe(&prefs, |_, _, cx| cx.notify()));
        }
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            model,
            selected_group,
            inputs: HashMap::new(),
            textareas: HashMap::new(),
            anchors: HashMap::new(),
            popup: None,
            ask: None,
            adding_secret: None,
            scroll: ScrollHandle::new(),
            focus,
            subscriptions,
            recording_shortcut: None,
            shortcut_problem: None,
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.stop_recording_shortcut(cx);
        cx.emit(CommandButtonsEvent::Close);
    }

    fn record_shortcut(&mut self, button_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.shortcut_problem = None;
        self.recording_shortcut = Some(button_id.to_string());
        set_chord_capture(true, cx);
        // Keys go to the dialog itself while it records, not to a field.
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn stop_recording_shortcut(&mut self, cx: &mut Context<Self>) {
        if self.recording_shortcut.take().is_some() {
            set_chord_capture(false, cx);
        }
        self.shortcut_problem = None;
        cx.notify();
    }

    /// A key while a shortcut is recorded (`useChordCapture`): Esc cancels, a chord that is
    /// not bindable, belongs to the shell or is already in use is refused, any other is saved.
    fn capture_shortcut(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(button_id) = self.recording_shortcut.clone() else {
            return;
        };
        cx.stop_propagation();
        let Some(pressed) = chord_event(&event.keystroke) else {
            return;
        };
        if is_modifier_code(&pressed.code) {
            return;
        }
        if pressed.code == "Escape" {
            self.stop_recording_shortcut(cx);
            return;
        }
        let chord = chord_from_event(&pressed);
        if !is_bindable_chord(&chord) {
            self.shortcut_problem = Some(
                "Add Ctrl, Alt, or Cmd \u{2014} Shift alone would swallow ordinary typing.".into(),
            );
            cx.notify();
            return;
        }
        let shortcut = chord_to_string(&chord);
        let prefs = prefs(cx);
        let keybindings = keybinding_overrides(&prefs);
        if let Some(conflict) = command_button_shortcut_conflict(
            &shortcut,
            &button_id,
            &keybindings,
            &prefs.command_buttons,
        ) {
            self.shortcut_problem = Some(conflict);
            cx.notify();
            return;
        }
        Self::update_button(&button_id, cx, |button| button.shortcut = Some(shortcut));
        self.stop_recording_shortcut(cx);
    }

    fn anchor(&mut self, key: &str) -> AnchorBounds {
        self.anchors.entry(key.to_owned()).or_default().clone()
    }

    /// The input for `key`, created with `value` the first time; edits go
    /// through `apply`.
    fn input(
        &mut self,
        key: String,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl Fn(&str, &mut App) + 'static,
    ) -> Entity<InputState> {
        if let Some(input) = self.inputs.get(&key) {
            return input.clone();
        }
        let value = value.to_owned();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(value, window, cx);
            state
        });
        self.subscriptions.push(
            cx.subscribe(&input, move |_, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    apply(&value, cx);
                }
            }),
        );
        self.inputs.insert(key, input.clone());
        input
    }

    /// A multi-line field for `key` that grows from one row to three and scrolls after
    /// (`multiline maxRows={3}`), created with `value` the first time.
    fn textarea(
        &mut self,
        key: String,
        value: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl Fn(&str, &mut App) + 'static,
    ) -> Entity<TextareaState> {
        if let Some(input) = self.textareas.get(&key) {
            return input.clone();
        }
        let value = value.to_owned();
        let placeholder = placeholder.to_owned();
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                .auto_grow(1, 3)
                .placeholder(placeholder);
            state.set_value(value, window, cx);
            state
        });
        self.subscriptions.push(
            cx.subscribe(&input, move |_, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    apply(&value, cx);
                }
            }),
        );
        self.textareas.insert(key, input.clone());
        input
    }

    /// Focus a field with its text selected (`useSelectWhen`): a name the
    /// user is expected to overwrite right away.
    fn select_field(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.inputs.get(key).cloned() {
            input.update(cx, |state, cx| {
                state.focus(window, cx);
                state.select_all(window, cx);
            });
        }
    }

    fn set_buttons(
        cx: &mut App,
        change: impl FnOnce(&mut Vec<CommandButton>, &[CommandButtonGroup]),
    ) {
        update_prefs(cx, |prefs| {
            let groups = prefs.command_button_groups.clone();
            change(&mut prefs.command_buttons, &groups);
        });
    }

    fn update_button(id: &str, cx: &mut App, change: impl FnOnce(&mut CommandButton)) {
        let id = id.to_owned();
        Self::set_buttons(cx, move |buttons, _| {
            if let Some(button) = buttons.iter_mut().find(|button| button.id == id) {
                change(button);
            }
        });
    }

    fn add_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prefs = prefs(cx);
        let created = CommandButtonGroup {
            id: new_preference_id("command-group"),
            name: unused_command_button_group_name(&prefs.command_button_groups),
        };
        let id = created.id.clone();
        update_prefs(cx, |prefs| prefs.command_button_groups.push(created));
        self.selected_group = id.clone();
        // Render once so the field exists, then select its name.
        cx.notify();
        let key = format!("group:{id}");
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |this, cx| this.select_field(&key, window, cx));
        });
    }

    fn add_button(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let is_default = self.selected_group == DEFAULT_COMMAND_GROUP_ID;
        let created = CommandButton {
            id: new_preference_id("command"),
            label: "New command".into(),
            command: String::new(),
            send_enter: true,
            group_id: (!is_default).then(|| self.selected_group.clone()),
            color: None,
            secret_id: None,
            shortcut: None,
        };
        let key = format!("label:{}", created.id);
        Self::set_buttons(cx, |buttons, _| buttons.push(created));
        cx.notify();
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |this, cx| this.select_field(&key, window, cx));
        });
    }

    fn ask(
        &mut self,
        request: AskRequest,
        pending: Pending,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = cx.new(|cx| AskDialog::new(request, window, cx));
        cx.subscribe_in(
            &dialog,
            window,
            |this, _, answer: &AskAnswer, window, cx| {
                let Some((_, pending)) = this.ask.take() else {
                    return;
                };
                window.focus(&this.focus, cx);
                if *answer == AskAnswer::Confirmed {
                    match pending {
                        Pending::DeleteGroup(id) => {
                            let groups = prefs(cx).command_button_groups;
                            let index = groups.iter().position(|group| group.id == id).unwrap_or(0);
                            update_prefs(cx, |prefs| {
                                let (buttons, groups) = without_command_button_group(
                                    &prefs.command_buttons,
                                    &prefs.command_button_groups,
                                    &id,
                                );
                                prefs.command_buttons = buttons;
                                prefs.command_button_groups = groups;
                            });
                            this.selected_group = index
                                .checked_sub(1)
                                .and_then(|previous| groups.get(previous))
                                .map(|group| group.id.clone())
                                .unwrap_or_else(|| DEFAULT_COMMAND_GROUP_ID.to_owned());
                        }
                        Pending::DeleteButton(id) => {
                            Self::set_buttons(cx, |buttons, _| {
                                buttons.retain(|button| button.id != id)
                            });
                        }
                    }
                }
                cx.notify();
            },
        )
        .detach();
        self.ask = Some((dialog, pending));
        cx.notify();
    }

    fn request_delete_group(
        &mut self,
        group: &CommandButtonGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prefs = prefs(cx);
        let count = command_buttons_in_group(
            &prefs.command_buttons,
            &prefs.command_button_groups,
            &group.id,
        )
        .len();
        let hosts = self.model.read(cx).hosts_opening(&group.id);
        let name = muxus_core::js::js_trim(&group.name);
        self.ask(
            AskRequest::confirm(
                format!(
                    "Delete “{}”?",
                    if name.is_empty() { "this group" } else { name }
                ),
                delete_group_description(count, hosts),
                "Delete group",
            )
            .destructive(),
            Pending::DeleteGroup(group.id.clone()),
            window,
            cx,
        );
    }

    fn request_delete_button(
        &mut self,
        button: &CommandButton,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = muxus_core::js::js_trim(&button.label);
        self.ask(
            AskRequest::confirm(
                format!(
                    "Delete “{}”?",
                    if label.is_empty() {
                        "this command button"
                    } else {
                        label
                    }
                ),
                "The saved command is removed from the action bar.",
                "Delete",
            )
            .destructive(),
            Pending::DeleteButton(button.id.clone()),
            window,
            cx,
        );
    }

    fn open_new_secret(&mut self, button_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let backend = self.model.read(cx).backend().clone();
        let dialog = open_vault_secret_dialog(backend, None, "", window, cx);
        let button_id = button_id.to_owned();
        cx.subscribe(&dialog, move |this, _, event: &VaultSecretEvent, cx| {
            if let VaultSecretEvent::Saved(result) = event {
                let secret = result.secret.id.clone();
                let id = button_id.clone();
                Self::update_button(&id, cx, |button| button.secret_id = Some(secret));
                this.model.update(cx, |model, cx| model.refresh_vault(cx));
            }
            this.adding_secret = None;
            cx.notify();
        })
        .detach();
        self.adding_secret = Some((dialog, String::new()));
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.recording_shortcut.is_some() {
            self.capture_shortcut(event, cx);
            return;
        }
        if event.keystroke.key == "escape" && self.ask.is_none() && self.adding_secret.is_none() {
            cx.stop_propagation();
            if self.popup.take().is_some() {
                cx.notify();
            } else {
                self.close(cx);
            }
        }
    }
}

/// The bar switch and its Top/Bottom choice.
fn bar_settings(show: bool, position: CommandBarPosition, theme: &Theme) -> impl IntoElement {
    let p = &theme.palette;
    let t = &theme.typography;
    let caption = format!(
        "Display saved commands {} the terminals. The Ctrl+Space menu always remains available.",
        if position == CommandBarPosition::Bottom {
            "below"
        } else {
            "above"
        }
    );
    let pick = |value: CommandBarPosition| {
        move |_: &muxus_ui::gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
            update_prefs(cx, |prefs| prefs.command_bar_position = value);
        }
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(Theme::spacing(2.))
        .p(Theme::spacing(1.25))
        .rounded(px(theme.radius))
        .border_1()
        .border_color(p.divider)
        .child(
            div().flex_1().min_w_0().child(
                FormControlLabel::new("command-bar-switch", FormControl::Switch(show), "")
                    .small()
                    .flush()
                    .label_element(label_with_caption(
                        theme,
                        "Show command bar",
                        caption,
                        0.,
                        false,
                    ))
                    .on_click(move |_, _, cx| {
                        update_prefs(cx, |prefs| prefs.show_command_bar = !show)
                    }),
            ),
        )
        .child(
            ToggleButtonGroup::new(vec![
                ToggleButton::new(
                    "command-bar-top",
                    "Top",
                    position == CommandBarPosition::Top,
                )
                .icon("VerticalAlignTop")
                .on_click(pick(CommandBarPosition::Top)),
                ToggleButton::new(
                    "command-bar-bottom",
                    "Bottom",
                    position == CommandBarPosition::Bottom,
                )
                .icon("VerticalAlignBottom")
                .on_click(pick(CommandBarPosition::Bottom)),
            ])
            // `size="small"` keeps its 7px padding; `sx` sets `px: 1.25`.
            .inline(
                Theme::spacing_css(0.875),
                Theme::spacing_css(1.25),
                t.px_to_rem(13.),
            )
            .icon_style(SMALL, Theme::spacing_css(0.5))
            .disabled(!show),
        )
}

impl CommandButtonsDialog {
    /// The group column: one vertical tab per group and "New group".
    fn group_tabs(
        &mut self,
        groups: &[CommandButtonGroup],
        buttons: &[CommandButton],
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = &theme.palette;
        let t = &theme.typography;
        // The label's line is the tab's own (`lineHeight: 1.25`).
        let label = TextVariant {
            line_height: t.body2.size * tabs::LINE_HEIGHT,
            ..t.body2
        };
        let mut tabs = div().flex().flex_col();
        for (index, group) in groups.iter().enumerate() {
            let count = buttons
                .iter()
                .filter(|button| command_button_group_of(button, groups) == group.id)
                .count();
            let id = group.id.clone();
            let select = cx.entity().downgrade();
            let selected = group.id == self.selected_group;
            tabs = tabs.child(
                Tab::new(("command-group-tab", index), group_label(group), selected)
                    .vertical()
                    .min_height(38.)
                    .padding(
                        Theme::spacing_css(0.5),
                        Theme::spacing_css(1.5),
                        Theme::spacing_css(1.5),
                    )
                    .font_size(t.body2.size)
                    .content(
                        div()
                            .flex()
                            .gap(Theme::spacing(1.))
                            .child(
                                div().flex_1().min_w_0().child(
                                    text(
                                        group_label(group),
                                        label,
                                        if selected {
                                            p.text.primary
                                        } else {
                                            p.text.secondary
                                        },
                                    )
                                    .weight(t.button.weight),
                                ),
                            )
                            .child(
                                text(count.to_string(), label, p.text.secondary)
                                    .weight(t.button.weight),
                            ),
                    )
                    .on_click(move |_, _, cx| {
                        let _ = select.update(cx, |this, cx| {
                            this.selected_group = id.clone();
                            this.popup = None;
                            cx.notify();
                        });
                    }),
            );
        }
        let add = cx.entity().downgrade();
        let _ = window;
        div()
            .flex()
            .flex_col()
            .w(px(190.))
            .flex_none()
            .min_h_0()
            .border_r_1()
            .border_color(p.divider)
            .child(tabs)
            .child(
                // A flex box, so the button keeps its own width at the start.
                div()
                    .flex()
                    .pt(Theme::spacing(0.5))
                    .pr(Theme::spacing(1.5))
                    .child(
                        Button::new("command-new-group", "New group")
                            .start_icon("Add")
                            .disabled(groups.len() >= MAX_COMMAND_BUTTON_GROUPS)
                            .on_click(move |_, window, cx| {
                                let _ = add.update(cx, |this, cx| this.add_group(window, cx));
                            }),
                    ),
            )
            .into_any_element()
    }

    /// The selected group's name, order and delete button.
    fn group_header(
        &mut self,
        group: &CommandButtonGroup,
        groups: &[CommandButtonGroup],
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = &theme.palette;
        let t = &theme.typography;
        let is_default = group.id == DEFAULT_COMMAND_GROUP_ID;
        let index = groups
            .iter()
            .position(|candidate| candidate.id == group.id)
            .unwrap_or(0);
        let id = group.id.clone();
        let name_input = self.input(
            format!("group:{}", group.id),
            &group.name,
            window,
            cx,
            move |value, cx| {
                let value: String = value
                    .chars()
                    .take(muxus_store::prefs::command_buttons::MAX_COMMAND_BUTTON_GROUP_NAME)
                    .collect();
                let id = id.clone();
                update_prefs(cx, |prefs| {
                    if let Some(group) = prefs
                        .command_button_groups
                        .iter_mut()
                        .find(|group| group.id == id)
                    {
                        group.name = value;
                    }
                });
            },
        );
        let mut row = div().flex().items_center().gap(Theme::spacing(1.)).child(
            TextField::new(("command-group-name", index), &name_input)
                .label("Group name")
                .width(260.),
        );
        if !is_default {
            let (up, down, delete) = (group.id.clone(), group.id.clone(), group.clone());
            let remove = cx.entity().downgrade();
            row = row
                .child(
                    IconButton::new("command-group-up", "ArrowUpward")
                        .tooltip("Move group up")
                        .disabled(index <= 1)
                        .on_click(move |_, _, cx| {
                            let id = up.clone();
                            update_prefs(cx, |prefs| {
                                prefs.command_button_groups = move_command_button_group(
                                    &prefs.command_button_groups,
                                    &id,
                                    -1,
                                );
                            });
                        }),
                )
                .child(
                    IconButton::new("command-group-down", "ArrowDownward")
                        .tooltip("Move group down")
                        .disabled(index + 1 == groups.len())
                        .on_click(move |_, _, cx| {
                            let id = down.clone();
                            update_prefs(cx, |prefs| {
                                prefs.command_button_groups =
                                    move_command_button_group(&prefs.command_button_groups, &id, 1);
                            });
                        }),
                )
                .child(div().flex_1())
                .child(
                    Button::new("command-group-delete", "Delete group")
                        .color(ButtonColor::Error)
                        .start_icon("DeleteOutlined")
                        .on_click(move |_, window, cx| {
                            let group = delete.clone();
                            let _ = remove.update(cx, |this, cx| {
                                this.request_delete_group(&group, window, cx)
                            });
                        }),
                );
        }
        let hosts = self.model.read(cx).hosts_opening(&group.id);
        div()
            .flex()
            .flex_col()
            .mb(Theme::spacing(2.))
            .child(row)
            .child(
                div()
                    .mt(Theme::spacing(0.75))
                    .ml(Theme::spacing(1.75))
                    .child(text(
                        group_caption(is_default, hosts),
                        t.caption,
                        p.text.secondary,
                    )),
            )
            .into_any_element()
    }

    /// One button's card.
    #[allow(clippy::too_many_arguments)]
    fn card(
        &mut self,
        index: usize,
        count: usize,
        button: &CommandButton,
        groups: &[CommandButtonGroup],
        total: usize,
        vault: Option<&PasswordVaultStatus>,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = &theme.palette;
        let t = &theme.typography;
        let sends_secret = button.secret_id.is_some();
        let id = button.id.clone();

        let label_id = id.clone();
        let label_input = self.input(
            format!("label:{id}"),
            &button.label,
            window,
            cx,
            move |value, cx| {
                let value: String = value.chars().take(80).collect();
                Self::update_button(&label_id, cx, |button| button.label = value);
            },
        );
        let color_anchor = self.anchor(&format!("color:{id}"));
        let color_key = id.clone();
        let current = button.color;
        let dot_size = Theme::spacing_css(1.5);
        let dot = match current {
            Some(color) => div()
                .size(px(dot_size))
                .rounded_full()
                .bg(command_button_ink(color, theme)),
            None => div()
                .size(px(dot_size))
                .rounded_full()
                .border_1()
                .border_color(p.text.disabled),
        };
        let picker = cx.entity().downgrade();
        let color_button = div()
            .id(("command-color", index))
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .size(px(dot_size + 2. * Theme::spacing_css(0.75)))
            // `edge="start"` on a small IconButton pulls it 3px into the
            // field's padding; the adornment's `mr: 0.5` replaces its usual 8px.
            .ml(px(-3.))
            .mr(-Theme::spacing(0.5))
            .rounded_full()
            .cursor_pointer()
            .hover(move |s| s.bg(p.action.hover))
            .child(dot)
            .child(bounds_probe(&color_anchor))
            .tooltip(Tooltip::anchored(
                match current {
                    Some(color) => format!("Color: {}", color.name()),
                    None => "Button color".into(),
                },
                color_anchor.clone(),
                Placement::Bottom,
            ))
            .on_click(move |_, _, cx| {
                let key = color_key.clone();
                let _ = picker.update(cx, |this, cx| {
                    this.popup = Some(Popup::Color(key));
                    cx.notify();
                });
            });
        let label_field = TextField::new(("command-label", index), &label_input)
            .label("Button label")
            .start(color_button)
            .width(220.);

        let value_field: AnyElement = if sends_secret {
            let secret_id = button.secret_id.clone().unwrap_or_default();
            let missing =
                !secret_id.is_empty() && vault_secret_missing(vault, &secret_id) == Some(true);
            let value = if missing {
                "Missing secret".to_owned()
            } else {
                vault
                    .and_then(|vault| vault.secrets.iter().find(|secret| secret.id == secret_id))
                    .map(|secret| match &secret.username {
                        Some(user) => format!("{} · {user}", secret.name),
                        None => secret.name.clone(),
                    })
                    .unwrap_or_default()
            };
            let anchor = self.anchor(&format!("secret:{id}"));
            let open = cx.entity().downgrade();
            let key = id.clone();
            div()
                .flex_1()
                .min_w_0()
                .child(
                    Select::new(("command-secret", index), value)
                        .label("Secret")
                        .open(self.popup == Some(Popup::Secret(id.clone())))
                        .full_width()
                        .track_bounds(&anchor)
                        .on_click(move |_, _, cx| {
                            let key = key.clone();
                            let _ = open.update(cx, |this, cx| {
                                this.popup = Some(Popup::Secret(key));
                                cx.notify();
                            });
                        }),
                )
                .into_any_element()
        } else {
            let command_id = id.clone();
            // `multiline maxRows={3}`: one row that grows to three, then scrolls.
            let command_input = self.textarea(
                format!("command:{id}"),
                &button.command,
                "systemctl status nginx",
                window,
                cx,
                move |value, cx| {
                    let value = value.to_owned();
                    Self::update_button(&command_id, cx, |button| button.command = value);
                },
            );
            style_textarea(&command_input, theme, cx);
            let focus = command_input.read(cx).focus_handle(cx);
            div()
                .flex_1()
                .min_w_0()
                .child(
                    FormField::new(
                        ("command-text", index),
                        FieldBody::Element(Textarea::new(&command_input).into_any_element()),
                    )
                    .multiline()
                    .element_focus(focus)
                    .label("Command")
                    .shrink(true)
                    .keep_placeholder(),
                )
                .into_any_element()
        };

        let weak = cx.entity().downgrade();
        let (up, down, copy, delete) = (id.clone(), id.clone(), id.clone(), button.clone());
        let recording = self.recording_shortcut.as_deref() == Some(id.as_str());
        // Saved by an earlier build or a hand edit; the dispatcher skips it.
        let shell_shortcut = button
            .shortcut
            .as_deref()
            .filter(|shortcut| !recording && shell_owns(shortcut));
        let shortcut_control: AnyElement = if button.shortcut.is_some() || recording {
            let record = weak.clone();
            let record_id = id.clone();
            let mut chip = Chip::new(match button.shortcut.as_deref() {
                Some(shortcut) if !recording => format_chord_string(shortcut),
                _ => "Press keys\u{2026}".to_string(),
            })
            .outlined()
            .mono()
            .height(24.)
            .on_click(("command-shortcut", index), move |_, window, cx| {
                let id = record_id.clone();
                let _ = record.update(cx, |this, cx| this.record_shortcut(&id, window, cx));
            })
            .tooltip(
                ("command-shortcut", index),
                if recording {
                    "Press the new shortcut"
                } else {
                    "Change the shortcut"
                },
            );
            if recording {
                chip = chip.color(p.primary.main);
            } else if shell_shortcut.is_some() {
                chip = chip.palette(p.warning);
            }
            if button.shortcut.is_some() && !recording {
                let clear_id = id.clone();
                chip = chip.on_delete(
                    ("command-shortcut-delete", index),
                    "CancelRounded",
                    None,
                    move |_, _, cx| {
                        let id = clear_id.clone();
                        Self::update_button(&id, cx, |button| button.shortcut = None);
                    },
                );
            }
            div().mr(Theme::spacing(0.5)).child(chip).into_any_element()
        } else {
            let record = weak.clone();
            let record_id = id.clone();
            IconButton::new(("command-shortcut-add", index), "KeyboardOutlined")
                .tooltip("Add shortcut")
                .on_click(move |_, window, cx| {
                    let id = record_id.clone();
                    let _ = record.update(cx, |this, cx| this.record_shortcut(&id, window, cx));
                })
                .into_any_element()
        };
        let shortcut_note: Option<AnyElement> = if recording {
            Some(
                Alert::new(
                    Status::Info,
                    self.shortcut_problem.clone().unwrap_or_else(|| {
                        "Press the key combination you want. Esc cancels.".into()
                    }),
                )
                .icon("KeyboardOutlined")
                .into_any_element(),
            )
        } else {
            shell_shortcut.map(|shortcut| {
                Alert::new(
                    Status::Warning,
                    format!(
                        "{} is kept for the shell, so it never runs this button. Replace or remove it.",
                        format_chord_string(shortcut)
                    ),
                )
                .into_any_element()
            })
        };
        let tools = div()
            .flex()
            .items_center()
            .pt(Theme::spacing(0.25))
            .child(shortcut_control)
            .child(
                IconButton::new(("command-up", index), "ArrowUpward")
                    .tooltip("Move up")
                    .disabled(index == 0)
                    .on_click(move |_, _, cx| {
                        let id = up.clone();
                        Self::set_buttons(cx, move |buttons, groups| {
                            *buttons = move_command_button(buttons, groups, &id, -1)
                        });
                    }),
            )
            .child(
                IconButton::new(("command-down", index), "ArrowDownward")
                    .tooltip("Move down")
                    .disabled(index + 1 == count)
                    .on_click(move |_, _, cx| {
                        let id = down.clone();
                        Self::set_buttons(cx, move |buttons, groups| {
                            *buttons = move_command_button(buttons, groups, &id, 1)
                        });
                    }),
            )
            .child(
                IconButton::new(("command-copy", index), "ContentCopyOutlined")
                    .tooltip("Duplicate button")
                    .disabled(total >= MAX_COMMAND_BUTTONS)
                    .on_click({
                        let weak = weak.clone();
                        move |_, window, cx| {
                            let id = copy.clone();
                            let new_id = new_preference_id("command");
                            let key = format!("label:{new_id}");
                            Self::set_buttons(cx, |buttons, _| {
                                *buttons = duplicate_command_button(buttons, &id, &new_id)
                            });
                            let weak = weak.clone();
                            window.on_next_frame(move |window, cx| {
                                let _ =
                                    weak.update(cx, |this, cx| this.select_field(&key, window, cx));
                            });
                        }
                    }),
            )
            .child(
                IconButton::new(("command-delete", index), "DeleteOutlined")
                    .color(ButtonColor::Error)
                    .tooltip("Delete button")
                    .on_click(move |_, window, cx| {
                        let button = delete.clone();
                        let _ = weak.update(cx, |this, cx| {
                            this.request_delete_button(&button, window, cx)
                        });
                    }),
            );

        let (to_command, to_secret, enter_id) = (id.clone(), id.clone(), id.clone());
        let send_enter = button.send_enter;
        let mut second = div()
            .flex()
            .items_center()
            .gap(Theme::spacing(2.))
            .child(
                ToggleButtonGroup::new(vec![
                    ToggleButton::new(("command-kind-command", index), "Command", !sends_secret)
                        .icon("TerminalOutlined")
                        .on_click(move |_, _, cx| {
                            let id = to_command.clone();
                            Self::update_button(&id, cx, |button| button.secret_id = None);
                        }),
                    ToggleButton::new(("command-kind-secret", index), "Secret", sends_secret)
                        .icon("KeyOutlined")
                        .on_click(move |_, _, cx| {
                            let id = to_secret.clone();
                            // Clear the text, so a password typed there before is not kept.
                            Self::update_button(&id, cx, |button| {
                                if button.secret_id.is_none() {
                                    button.secret_id = Some(String::new());
                                    button.command = String::new();
                                }
                            });
                        }),
                ])
                // `size="small"` keeps its 7px padding; `sx` sets `px: 1.25`.
                .inline(
                    Theme::spacing_css(0.875),
                    Theme::spacing_css(1.25),
                    t.px_to_rem(13.),
                )
                .icon_style(SMALL, Theme::spacing_css(0.5)),
            )
            .child(
                div().flex_1().min_w_0().child(
                    FormControlLabel::new(
                        ("command-enter", index),
                        FormControl::Switch(send_enter),
                        "",
                    )
                    .small()
                    .flush()
                    .label_element(if sends_secret {
                        label_with_caption(
                            theme,
                            "Press Enter after it",
                            "Answers a password prompt. Turn off to type the secret without Enter.",
                            0.,
                            false,
                        )
                    } else {
                        label_with_caption(
                            theme,
                            "Run immediately",
                            "Send Enter after the command. Turn off to insert it for review first.",
                            0.,
                            false,
                        )
                    })
                    .on_click(move |_, _, cx| {
                        let id = enter_id.clone();
                        Self::update_button(&id, cx, |button| button.send_enter = !send_enter);
                    }),
                ),
            );
        if groups.len() > 1 {
            let anchor = self.anchor(&format!("group-of:{id}"));
            let open = cx.entity().downgrade();
            let key = id.clone();
            let group_id = command_button_group_of(button, groups);
            let current = groups
                .iter()
                .find(|group| group.id == group_id)
                .map(group_label)
                .unwrap_or_default();
            second = second.child(
                Select::new(("command-group", index), current)
                    .label("Group")
                    .width(180.)
                    .open(self.popup == Some(Popup::Group(id.clone())))
                    .track_bounds(&anchor)
                    .on_click(move |_, _, cx| {
                        let key = key.clone();
                        let _ = open.update(cx, |this, cx| {
                            this.popup = Some(Popup::Group(key));
                            cx.notify();
                        });
                    }),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(1.25))
            .p(Theme::spacing(1.5))
            .rounded(px(theme.radius))
            .border_1()
            .border_color(p.divider)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(Theme::spacing(1.))
                    .child(label_field)
                    .child(value_field)
                    .child(tools),
            )
            .child(second)
            .children(shortcut_note)
            .into_any_element()
    }

    /// The open color picker, group menu or secret menu.
    fn popup_view(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let popup = self.popup.clone()?;
        let p = &theme.palette;
        let t = &theme.typography;
        let close = cx.entity().downgrade();
        let dismiss = move |_: &mut Window, cx: &mut App| {
            let _ = close.update(cx, |this, cx| {
                this.popup = None;
                cx.notify();
            });
        };
        let weak = cx.entity().downgrade();
        match popup {
            Popup::Color(id) => {
                let bounds = self.anchors.get(&format!("color:{id}"))?.get()?;
                let prefs = prefs(cx);
                let current = prefs
                    .command_buttons
                    .iter()
                    .find(|button| button.id == id)?
                    .color;
                let swatch = Theme::spacing_css(2.75);
                let check = |selected: bool, ink| {
                    selected.then(|| icon("Check", t.px_to_rem(14.) * 14. / 13., ink))
                };
                let choose = |color: Option<CommandButtonColor>| {
                    let id = id.clone();
                    let weak = weak.clone();
                    move |_: &muxus_ui::gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                        let id = id.clone();
                        Self::update_button(&id, cx, |button| button.color = color);
                        let _ = weak.update(cx, |this, cx| {
                            this.popup = None;
                            cx.notify();
                        });
                    }
                };
                let none_anchor = AnchorBounds::default();
                let mut row = div().flex().gap(Theme::spacing(0.75)).child(
                    div()
                        .id("command-color-none")
                        .relative()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(swatch))
                        .rounded_full()
                        .border_1()
                        .border_color(if current.is_none() {
                            p.text.secondary
                        } else {
                            p.divider
                        })
                        .cursor_pointer()
                        .children(check(current.is_none(), p.text.secondary))
                        .child(bounds_probe(&none_anchor))
                        .tooltip(Tooltip::anchored(
                            "No color",
                            none_anchor,
                            Placement::Bottom,
                        ))
                        .on_click(choose(None)),
                );
                for (index, (color, name, _, _)) in COMMAND_BUTTON_COLORS.iter().enumerate() {
                    let anchor = AnchorBounds::default();
                    let selected = current == Some(*color);
                    row = row.child(
                        div()
                            .id(("command-color-swatch", index))
                            .relative()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(swatch))
                            .rounded_full()
                            .bg(command_button_ink(*color, theme))
                            .cursor_pointer()
                            .children(check(selected, p.background_paper))
                            .child(bounds_probe(&anchor))
                            .tooltip(Tooltip::anchored(*name, anchor, Placement::Bottom))
                            .on_click(choose(Some(*color))),
                    );
                }
                let paper = popover_paper(theme)
                    .p(Theme::spacing(1.25))
                    .child(div().mb(Theme::spacing(1.)).child(text(
                        "Button color",
                        t.caption,
                        p.text.secondary,
                    )))
                    .child(row);
                Some(menu::popup(bounds.bottom_left(), dismiss, paper).into_any_element())
            }
            Popup::Group(id) => {
                let bounds = self.anchors.get(&format!("group-of:{id}"))?.get()?;
                let prefs = prefs(cx);
                let button = prefs
                    .command_buttons
                    .iter()
                    .find(|button| button.id == id)?
                    .clone();
                let current =
                    command_button_group_of(&button, &prefs.command_button_groups).to_owned();
                let mut items = menu::paper(theme).min_w(bounds.size.width);
                for (index, group) in prefs.command_button_groups.iter().enumerate() {
                    let (id, group_id, weak) = (id.clone(), group.id.clone(), weak.clone());
                    items = items.child(
                        MenuItem::new(("command-group-option", index), group_label(group))
                            .selected(group.id == current)
                            .on_click(move |_, _, cx| {
                                let (id, group_id) = (id.clone(), group_id.clone());
                                Self::set_buttons(cx, move |buttons, groups| {
                                    *buttons = move_command_button_to_group(
                                        buttons, groups, &id, &group_id,
                                    )
                                });
                                let _ = weak.update(cx, |this, cx| {
                                    this.popup = None;
                                    cx.notify();
                                });
                            }),
                    );
                }
                Some(menu::popup(bounds.bottom_left(), dismiss, items).into_any_element())
            }
            Popup::Secret(id) => {
                let bounds = self.anchors.get(&format!("secret:{id}"))?.get()?;
                let vault = self.model.read(cx).vault().cloned();
                let prefs = prefs(cx);
                let button = prefs
                    .command_buttons
                    .iter()
                    .find(|button| button.id == id)?
                    .clone();
                let selected = button.secret_id.clone().unwrap_or_default();
                let mut items = menu::paper(theme).min_w(bounds.size.width);
                for (index, secret) in vault
                    .iter()
                    .flat_map(|vault| vault.secrets.iter())
                    .enumerate()
                {
                    let (id, secret_id, weak) = (id.clone(), secret.id.clone(), weak.clone());
                    let mut item =
                        MenuItem::new(("command-secret-option", index), secret.name.clone())
                            .selected(secret.id == selected)
                            .on_click(move |_, _, cx| {
                                let (id, secret_id) = (id.clone(), secret_id.clone());
                                Self::update_button(&id, cx, |button| {
                                    button.secret_id = Some(secret_id)
                                });
                                let _ = weak.update(cx, |this, cx| {
                                    this.popup = None;
                                    cx.notify();
                                });
                            });
                    if let Some(user) = &secret.username {
                        item = item.trailing(div().ml(Theme::spacing(1.5)).child(text(
                            user.clone(),
                            t.body2,
                            p.text.secondary,
                        )));
                    }
                    items = items.child(item);
                }
                let (new_id, new_weak) = (id.clone(), weak.clone());
                items = items.child(
                    MenuItem::new("command-secret-new", "New secret…")
                        .icon("Add")
                        .on_click(move |_, window, cx| {
                            let id = new_id.clone();
                            let _ = new_weak.update(cx, |this, cx| {
                                this.popup = None;
                                this.open_new_secret(&id, window, cx);
                            });
                        }),
                );
                Some(menu::popup(bounds.bottom_left(), dismiss, items).into_any_element())
            }
        }
    }
}

impl Render for CommandButtonsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let t = theme.typography;
        let prefs = prefs(cx);
        let groups = prefs.command_button_groups.clone();
        let buttons = prefs.command_buttons.clone();
        let vault = self.model.read(cx).vault().cloned();
        let group = groups
            .iter()
            .find(|candidate| candidate.id == self.selected_group)
            .or_else(|| groups.first())
            .cloned()
            .unwrap_or_else(|| CommandButtonGroup {
                id: DEFAULT_COMMAND_GROUP_ID.into(),
                name: DEFAULT_COMMAND_GROUP_NAME.into(),
            });
        let group_buttons = command_buttons_in_group(&buttons, &groups, &group.id);
        // Recording ends once its card leaves the view: another group shown, the button moved
        // to another group, or deleted. Keys are never taken unseen.
        if let Some(recording) = &self.recording_shortcut
            && !group_buttons.iter().any(|button| &button.id == recording)
        {
            self.stop_recording_shortcut(cx);
        }
        let grouped = groups.len() > 1;
        let weak = cx.entity().downgrade();

        let mut cards = div().flex().flex_col().gap(Theme::spacing(1.25));
        if group_buttons.is_empty() {
            cards = cards.child(
                div()
                    .p(Theme::spacing(3.))
                    .flex()
                    .justify_center()
                    .rounded(px(theme.radius))
                    .border_1()
                    .border_color(p.divider)
                    .child(text(
                        if grouped {
                            "No command buttons in this group yet."
                        } else {
                            "No command buttons yet."
                        },
                        t.body2,
                        p.text.secondary,
                    )),
            );
        }
        let count = group_buttons.len();
        for (index, button) in group_buttons.iter().enumerate() {
            cards = cards.child(self.card(
                index,
                count,
                button,
                &groups,
                buttons.len(),
                vault.as_ref(),
                &theme,
                window,
                cx,
            ));
        }
        let add = weak.clone();
        cards = cards.child(
            div().child(
                Button::new("command-add", "Add button")
                    .start_icon("Add")
                    .disabled(buttons.len() >= MAX_COMMAND_BUTTONS)
                    .on_click(move |_, window, cx| {
                        let _ = add.update(cx, |this, cx| this.add_button(window, cx));
                    }),
            ),
        );
        let mut editor = div()
            .id("command-buttons-editor")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .pt(Theme::spacing(1.))
            .pr(Theme::spacing(0.5))
            .pb(Theme::spacing(1.));
        if grouped {
            editor = editor.child(self.group_header(&group, &groups, &theme, window, cx));
        }
        editor = editor.child(cards);

        let content = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .px(Theme::spacing(3.))
            .pb(Theme::spacing(0.5))
            .child(
                div().mb(Theme::spacing(2.)).child(
                    muxus_ui::label::paragraph(
                        "Save commands you use often and sort them into groups, such as one per vendor or task; the bar shows one group at a time. Open them with Ctrl+Space or keep the optional one-click bar above or below the terminals.",
                    )
                    .size(t.body2.size)
                    .line_height(t.body2.line_height)
                    .color(p.text.secondary),
                ),
            )
            .child(bar_settings(prefs.show_command_bar, prefs.command_bar_position, &theme))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(Theme::spacing(2.5))
                    .mt(Theme::spacing(2.))
                    .child(self.group_tabs(&groups, &buttons, &theme, window, cx))
                    .child(editor),
            );

        let viewport = window.viewport_size();
        let (vw, vh) = (
            muxus_ui::zoom::unzoom(viewport.width),
            muxus_ui::zoom::unzoom(viewport.height),
        );
        let done = weak.clone();
        // `maxWidth="md" fullWidth`, `height: min(720px, calc(100% - 64px))`.
        let paper = dialog_paper(&theme)
            .w(px(900f32.min(vw - 64.)))
            .h(px(720f32.min(vh - 64.)))
            .child(dialog_title(&theme).child(text("Command buttons", t.h6, p.text.primary)))
            .child(content)
            .child(
                dialog_actions()
                    .px(Theme::spacing(3.))
                    .pb(Theme::spacing(2.))
                    .child(Button::new("command-done", "Done").contained().on_click(
                        move |_, _, cx| {
                            let _ = done.update(cx, |this, cx| this.close(cx));
                        },
                    )),
            );
        let close = weak.clone();
        let mut root = div()
            .id("command-buttons-dialog")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(modal(
                paper,
                Some(Rc::new(move |_, cx: &mut App| {
                    let _ = close.update(cx, |this, cx| this.close(cx));
                })),
            ));
        if let Some(popup) = self.popup_view(&theme, cx) {
            root = root.child(popup);
        }
        if let Some((ask, _)) = &self.ask {
            root = root.child(ask.clone());
        }
        if let Some((dialog, _)) = &self.adding_secret {
            root = root.child(dialog.clone());
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_what_a_group_does_and_what_deleting_it_takes() {
        assert_eq!(
            group_caption(true, 0),
            "Holds every command not filed under another group."
        );
        assert_eq!(
            group_caption(false, 2),
            "The bar switches to this group in sessions to 2 hosts."
        );
        assert_eq!(
            delete_group_description(1, 1),
            "Its 1 command button is deleted with it. 1 host that opens it will show the group chosen in the bar instead."
        );
        assert_eq!(
            delete_group_description(0, 0),
            "The group holds no command buttons."
        );
        assert_eq!(
            delete_group_description(3, 2),
            "Its 3 command buttons are deleted with it. 2 hosts that open it will show the group chosen in the bar instead."
        );
    }
}
