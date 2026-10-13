//! `components/ShortcutsDialog.tsx`: every keyboard command with its chords,
//! searchable, rebindable by recording a new chord, plus the mouse gestures
//! and file editor keys no keymap entry can express.
//!
//! Any window can mount the dialog: create it with [`ShortcutsDialog::new`],
//! render the entity as an overlay, and drop it on [`ShortcutsEvent::Close`].
//! While it records a chord, [`chord_capture_active`] is true, and the
//! window's own shortcut dispatcher must stand down (`setChordCaptureActive`)
//! so the chord being captured cannot fire the command it replaces.

use std::collections::HashMap;

use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, Global,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, px, relative,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts};
use muxus_ui::widgets::{Alert, Button, ButtonColor, IconButton, Tooltip};

use super::super::prefs_model::SettingsPrefs;
use super::appearance::keybinding_overrides;
use super::fields::field;
use super::kit::{BODY2, CAPTION, snap};
use crate::keymap::bindings::{
    chords_are_default, command_chords, conflicting_command_ids, is_command_customized,
};
use crate::keymap::chords::{
    IS_MAC, chord_from_event, chord_signature, chord_to_string, format_chord_string,
    is_bindable_chord, is_modifier_code, parse_chord,
};
use crate::keymap::commands::{COMMAND_CATEGORIES, KeyCommand, key_commands};
use crate::keymap::keystroke::chord_event;

pub enum ShortcutsEvent {
    Close,
}

/// A chord being recorded for a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recording {
    pub command_id: String,
    /// Replaces this chord when set, otherwise the chord is added.
    pub index: Option<usize>,
}

#[derive(Default)]
struct ChordCapture(bool);

impl Global for ChordCapture {}

/// True while a shortcuts editor records a chord (`setChordCaptureActive`).
pub fn chord_capture_active(cx: &App) -> bool {
    cx.try_global::<ChordCapture>()
        .is_some_and(|capture| capture.0)
}

fn set_chord_capture(active: bool, cx: &mut App) {
    cx.set_global(ChordCapture(active));
}

/// Interactions worth documenting that no keymap entry can express.
pub fn extras() -> Vec<(&'static str, String)> {
    vec![
        (
            "Zoom the terminal with the mouse",
            format!("{}Scroll wheel", if IS_MAC { "\u{2303} " } else { "Ctrl+" }),
        ),
        (
            "Open a terminal path in the editor",
            "Click the underlined path".into(),
        ),
        (
            "Resize a split / reset it to half",
            "Drag the divider \u{b7} double-click".into(),
        ),
        (
            "Pane actions (split, zoom, close)",
            "Right-click the tab strip".into(),
        ),
        (
            "Rename a tab / close a tab",
            "Double-click it \u{b7} middle-click it".into(),
        ),
        (
            "New session to a host with one open",
            format!(
                "{}Click \u{b7} middle-click",
                if IS_MAC { "\u{21e7} " } else { "Shift+" }
            ),
        ),
        (
            "Search next / previous match",
            format!("Enter / {}", format_chord_string("Shift+Enter")),
        ),
    ]
}

/// Monaco's own bindings, in the platform's notation.
pub fn editor_shortcuts() -> Vec<(&'static str, String)> {
    vec![
        (
            "Save file / all files",
            format!(
                "{} \u{b7} {}, S",
                format_chord_string("Mod+S"),
                format_chord_string("Mod+K")
            ),
        ),
        (
            "Find / replace",
            format!(
                "{} \u{b7} {}",
                format_chord_string("Mod+F"),
                format_chord_string(if IS_MAC { "Alt+Mod+F" } else { "Mod+H" })
            ),
        ),
        (
            "Command palette",
            format!("F1 \u{b7} {}", format_chord_string("Mod+Shift+P")),
        ),
        (
            "Go to line",
            format_chord_string(if IS_MAC { "Ctrl+G" } else { "Mod+G" }),
        ),
        ("Format document", format_chord_string("Shift+Alt+F")),
    ]
}

/// The commands a search matches, by category in catalog order.
pub fn matching_commands(
    query: &str,
    overrides: &crate::keymap::bindings::KeybindingOverrides,
) -> Vec<(
    crate::keymap::commands::CommandCategory,
    Vec<&'static KeyCommand>,
)> {
    let needle = muxus_core::js::js_trim(query).to_lowercase();
    let mut buckets: HashMap<crate::keymap::commands::CommandCategory, Vec<&'static KeyCommand>> =
        HashMap::new();
    for command in key_commands() {
        if !needle.is_empty() {
            let mut haystack = vec![command.title.clone(), command.category.label().to_string()];
            haystack.extend(command.keywords.iter().cloned());
            haystack.extend(
                command_chords(command, overrides)
                    .iter()
                    .map(|chord| format_chord_string(chord)),
            );
            if !haystack.join(" ").to_lowercase().contains(&needle) {
                continue;
            }
        }
        buckets.entry(command.category).or_default().push(command);
    }
    COMMAND_CATEGORIES
        .iter()
        .filter_map(|category| {
            buckets
                .remove(category)
                .map(|commands| (*category, commands))
        })
        .collect()
}

/// The chord list once `text` is recorded (`onKeyDown` of the recorder):
/// an existing equal chord is dropped, a replaced one is swapped in place.
pub fn chords_after_recording(current: &[String], text: &str, index: Option<usize>) -> Vec<String> {
    let signature = parse_chord(text).map(|chord| chord_signature(&chord));
    let kept: Vec<String> = current
        .iter()
        .enumerate()
        .filter(|(position, existing)| {
            if index == Some(*position) {
                return false;
            }
            parse_chord(existing).map(|chord| chord_signature(&chord)) != signature
        })
        .map(|(_, chord)| chord.clone())
        .collect();
    match index {
        None => {
            let mut chords = kept;
            chords.push(text.to_string());
            chords
        }
        Some(index) => {
            let at = index.min(kept.len());
            let mut chords = kept[..at].to_vec();
            chords.push(text.to_string());
            chords.extend_from_slice(&kept[at..]);
            chords
        }
    }
}

/// The keyboard shortcuts editor.
pub struct ShortcutsDialog {
    prefs: Entity<SettingsPrefs>,
    search: Entity<InputState>,
    query: String,
    recording: Option<Recording>,
    capture_error: Option<String>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ShortcutsEvent> for ShortcutsDialog {}

impl Focusable for ShortcutsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ShortcutsDialog {
    pub fn new(prefs: Entity<SettingsPrefs>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search commands"));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().to_string();
                    cx.notify();
                }
            }),
            cx.observe(&prefs, |_, _, cx| cx.notify()),
        ];
        search.update(cx, |state, cx| state.focus(window, cx));
        Self {
            prefs,
            search,
            query: String::new(),
            recording: None,
            capture_error: None,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Whether a chord is being recorded right now.
    pub fn recording(&self) -> bool {
        self.recording.is_some()
    }

    fn overrides(&self, cx: &App) -> crate::keymap::bindings::KeybindingOverrides {
        keybinding_overrides(self.prefs.read(cx).prefs())
    }

    fn apply_chords(&self, command: &KeyCommand, chords: Vec<String>, cx: &mut App) {
        let id = command.id.clone();
        let default = chords_are_default(command, &chords);
        self.prefs.update(cx, |model, cx| {
            model.update(cx, |prefs| {
                if default {
                    prefs.keybindings.shift_remove(&id);
                } else {
                    prefs.keybindings.insert(id, chords);
                }
            })
        });
    }

    fn start_recording(
        &mut self,
        command_id: &str,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.capture_error = None;
        self.recording = Some(Recording {
            command_id: command_id.to_string(),
            index,
        });
        set_chord_capture(true, cx);
        // Keys go to the dialog itself while it records, not to the search field.
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        set_chord_capture(false, cx);
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        self.capture_error = None;
        set_chord_capture(false, cx);
        cx.emit(ShortcutsEvent::Close);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(recording) = self.recording.clone() else {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                self.close(cx);
            }
            return;
        };
        cx.stop_propagation();
        let Some(chord_event) = chord_event(&event.keystroke) else {
            return;
        };
        if is_modifier_code(&chord_event.code) {
            return;
        }
        if chord_event.code == "Escape" {
            self.stop_recording(cx);
            return;
        }
        let chord = chord_from_event(&chord_event);
        if !is_bindable_chord(&chord) {
            self.capture_error = Some(
                "Add Ctrl, Alt, or Cmd \u{2014} Shift alone would swallow ordinary typing.".into(),
            );
            cx.notify();
            return;
        }
        let Some(command) = key_commands()
            .iter()
            .find(|command| command.id == recording.command_id)
        else {
            self.stop_recording(cx);
            return;
        };
        let overrides = self.overrides(cx);
        let current = command_chords(command, &overrides).to_vec();
        let chords = chords_after_recording(&current, &chord_to_string(&chord), recording.index);
        self.apply_chords(command, chords, cx);
        self.capture_error = None;
        self.stop_recording(cx);
    }

    fn command_row(
        &self,
        theme: &Theme,
        command: &'static KeyCommand,
        overrides: &crate::keymap::bindings::KeybindingOverrides,
        conflicting: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let chords = command_chords(command, overrides).to_vec();
        let customized = is_command_customized(command, overrides);
        let recording = self
            .recording
            .as_ref()
            .filter(|recording| recording.command_id == command.id)
            .cloned();
        let group_name: SharedString = format!("shortcut-row-{}", command.id).into();
        let mut chips = div().flex().flex_wrap().items_center().gap(px(4.));
        for (index, chord) in chords.iter().enumerate() {
            let pressing = recording.as_ref().is_some_and(|r| r.index == Some(index));
            let text = if pressing {
                "Press keys\u{2026}".to_string()
            } else {
                format_chord_string(chord)
            };
            let id = command.id.clone();
            let remove_id = command.id.clone();
            chips = chips.child(chord_chip(
                theme,
                SharedString::from(format!("chip-{}-{index}", command.id)),
                text,
                pressing,
                group_name.clone(),
                cx.listener(move |this, _, window, cx| {
                    this.start_recording(&id, Some(index), window, cx)
                }),
                cx.listener(move |this, _, _, cx| {
                    if let Some(command) = key_commands().iter().find(|c| c.id == remove_id) {
                        let overrides = this.overrides(cx);
                        let chords: Vec<String> = command_chords(command, &overrides)
                            .iter()
                            .enumerate()
                            .filter(|(position, _)| *position != index)
                            .map(|(_, chord)| chord.clone())
                            .collect();
                        this.apply_chords(command, chords, cx);
                    }
                }),
            ));
        }
        if chords.is_empty() && recording.is_none() {
            chips = chips.child(
                label("Unbound")
                    .size(CAPTION)
                    .line_height(snap(CAPTION * 1.66))
                    .color(p.text.disabled),
            );
        }
        if recording.as_ref().is_some_and(|r| r.index.is_none()) {
            chips = chips.child(
                div()
                    .flex()
                    .items_center()
                    .h(px(24.))
                    .px(px(8.))
                    .rounded(px(16.))
                    .border_1()
                    .border_color(p.primary.main)
                    .child(
                        label("Press keys\u{2026}")
                            .family(fonts::MONO)
                            .size(11.)
                            .weight(500.)
                            .line_height(16.5)
                            .color(p.primary.main),
                    ),
            );
        }
        let add_id = command.id.clone();
        let reset_id = command.id.clone();
        let actions_visible = recording.is_some();
        let hover = alpha(p.text.primary, 0.04);
        div()
            .id(SharedString::from(format!("shortcut-{}", command.id)))
            .group(group_name.clone())
            .flex()
            .items_center()
            .gap(px(12.))
            // 26px of controls between 4.8px paddings, held to the device grid.
            .h(px(35.5))
            .px(px(8.))
            .mx(px(-8.))
            .rounded(px(8.))
            .hover(move |s| s.bg(hover))
            .child(
                label(command.title.clone())
                    .size(BODY2)
                    .line_height(snap(BODY2 * 1.43))
                    .color(p.text.primary)
                    .grow(),
            )
            .when(conflicting, |row| {
                row.child(
                    div()
                        .id(SharedString::from(format!("conflict-{}", command.id)))
                        .tooltip(Tooltip::text("Another command uses this chord"))
                        .child(icon("WarningAmberOutlined", 16., p.warning.main)),
                )
            })
            .child(chips)
            .child(
                div()
                    .flex()
                    .when(!actions_visible, |actions| {
                        actions.invisible().group_hover(group_name, |s| s.visible())
                    })
                    .child(
                        IconButton::new(
                            SharedString::from(format!("add-chord-{}", command.id)),
                            "Add",
                        )
                        .icon_size(16.)
                        .tooltip("Add a chord")
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.start_recording(&add_id, None, window, cx)
                            },
                        )),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("reset-chord-{}", command.id)),
                            "ReplayOutlined",
                        )
                        .icon_size(15.)
                        .disabled(!customized)
                        .tooltip(if customized {
                            "Restore the default"
                        } else {
                            "Default chord"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let id = reset_id.clone();
                            this.prefs.update(cx, |model, cx| {
                                model.update(cx, |prefs| {
                                    prefs.keybindings.shift_remove(&id);
                                })
                            });
                        })),
                    ),
            )
            .into_any_element()
    }
}

/// An outlined small `Chip` in the chord tier, with a delete icon that shows
/// while the row is hovered.
#[allow(clippy::too_many_arguments)]
fn chord_chip(
    theme: &Theme,
    id: SharedString,
    text: String,
    recording: bool,
    row_group: SharedString,
    on_click: impl Fn(&muxus_ui::gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
    on_delete: impl Fn(&muxus_ui::gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let p = theme.palette;
    let border = if recording {
        p.primary.main
    } else if theme.mode.is_dark() {
        p.grey.g700
    } else {
        p.grey.g400
    };
    let ink = if recording {
        p.primary.main
    } else {
        p.text.primary
    };
    let delete_ink = alpha(p.text.primary, 0.26);
    let paper = p.background_paper;
    div()
        .id(id.clone())
        .flex()
        .items_center()
        .h(px(24.))
        .rounded(px(16.))
        .border_1()
        .border_color(border)
        .cursor_pointer()
        .on_click(on_click)
        .child(
            div().px(px(7.)).child(
                label(text)
                    .family(fonts::MONO)
                    .size(11.)
                    .weight(500.)
                    .line_height(16.5)
                    .color(ink)
                    .no_truncate(),
            ),
        )
        .child(
            div()
                .id(SharedString::from(format!("{id}-delete")))
                .ml(px(-6.))
                .mr(px(5.))
                .size(px(16.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .bg(delete_ink)
                .invisible()
                .group_hover(row_group, |s| s.visible())
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    on_delete(event, window, cx);
                })
                .child(icon("Close", 10., paper)),
        )
        .into_any_element()
}

impl Render for ShortcutsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let overrides = self.overrides(cx);
        let conflicts = conflicting_command_ids(&overrides);
        let customized = key_commands()
            .iter()
            .any(|command| is_command_customized(command, &overrides));
        let groups = matching_commands(&self.query, &overrides);
        let mut content = div().flex().flex_col();
        if self.recording.is_some() {
            content =
                content.child(div().mb(px(12.)).child(Alert::new(
                    Status::Info,
                    self.capture_error.clone().unwrap_or_else(|| {
                        "Press the key combination you want. Esc cancels.".into()
                    }),
                )));
        } else if !conflicts.is_empty() {
            content = content.child(div().mb(px(12.)).child(Alert::new(
                Status::Warning,
                "Some commands share a chord. The first one that applies wins.",
            )));
        }
        let overline = |text: &str| {
            label(text.to_uppercase())
                .size(CAPTION)
                .line_height(snap(CAPTION * 2.66))
                .tracking(0.8)
                .color(p.text.secondary)
        };
        let any_match = !groups.is_empty();
        for (category, commands) in groups {
            let mut section = div()
                .flex()
                .flex_col()
                .mb(px(20.))
                .child(overline(category.label()));
            for command in commands {
                section = section.child(self.command_row(
                    &theme,
                    command,
                    &overrides,
                    conflicts.contains(&command.id),
                    cx,
                ));
            }
            content = content.child(section);
        }
        if !any_match {
            content = content.child(
                div().py(px(24.)).flex().justify_center().child(
                    label(format!(
                        "No command matches \u{201c}{}\u{201d}.",
                        muxus_core::js::js_trim(&self.query)
                    ))
                    .size(BODY2)
                    .line_height(snap(BODY2 * 1.43))
                    .color(p.text.secondary),
                ),
            );
        }
        for (title, rows) in [
            ("Mouse and other gestures", extras()),
            ("File editor", editor_shortcuts()),
        ] {
            let mut section = div().flex().flex_col().mb(px(16.)).child(overline(title));
            for (text, keys) in rows {
                section = section.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(16.))
                        .h(px(27.5))
                        .child(
                            label(text)
                                .size(BODY2)
                                .line_height(snap(BODY2 * 1.43))
                                .color(p.text.primary)
                                .grow(),
                        )
                        .child(super::kit::chord_text(keys, p.text.secondary)),
                );
            }
            content = content.child(section);
        }
        let viewport = window.viewport_size();
        let width = (muxus_ui::zoom::unzoom(viewport.width) - 64.).min(900.);
        let height = muxus_ui::zoom::unzoom(viewport.height) - 64.;
        let title = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(24.))
            .pt(px(16.))
            .pb(px(8.))
            .child(icon("KeyboardOutlined", 22.2857, p.primary.main))
            .child(super::kit::dialog_title(&theme, "Keyboard"))
            .child(div().flex_1())
            .child(
                field("shortcuts-search", &self.search)
                    .width(260.)
                    .start_icon("Search", 17.)
                    .render(&theme, window, cx),
            );
        let body = div()
            .id("shortcuts-content")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(px(24.))
            .py(px(16.))
            .border_t_1()
            .border_b_1()
            .border_color(p.divider)
            .child(content);
        let actions = div()
            .flex()
            .items_center()
            .p(px(8.))
            .child(
                Button::new("shortcuts-restore", "Restore defaults")
                    .color(ButtonColor::Inherit)
                    .start_icon("ReplayOutlined")
                    .disabled(!customized)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.prefs.update(cx, |model, cx| {
                            model.update(cx, |prefs| prefs.keybindings.clear())
                        });
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("shortcuts-done", "Done")
                    .contained()
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            );
        let paper = muxus_ui::widgets::dialog::dialog_paper(&theme)
            .w(px(width))
            .h(px(height))
            .max_h(relative(1.))
            .child(title)
            .child(body)
            .child(actions);
        let weak = cx.entity().downgrade();
        div()
            .id("shortcuts-dialog")
            .absolute()
            .inset_0()
            .key_context("ShortcutsDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(muxus_ui::widgets::dialog::modal(
                paper,
                Some(std::rc::Rc::new(move |_, cx: &mut App| {
                    let _ = weak.update(cx, |this, cx| this.close(cx));
                })),
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn recording_adds_replaces_and_deduplicates() {
        let current = strings(&["Ctrl+Shift+ArrowRight", "Alt+Shift+Equal"]);
        assert_eq!(
            chords_after_recording(&current, "Alt+KeyR", None),
            strings(&["Ctrl+Shift+ArrowRight", "Alt+Shift+Equal", "Alt+KeyR"])
        );
        assert_eq!(
            chords_after_recording(&current, "Alt+KeyR", Some(0)),
            strings(&["Alt+KeyR", "Alt+Shift+Equal"])
        );
        // Recording a chord the command already has keeps one copy.
        assert_eq!(
            chords_after_recording(&current, "Alt+Shift+Equal", None),
            strings(&["Ctrl+Shift+ArrowRight", "Alt+Shift+Equal"])
        );
    }

    #[test]
    fn search_matches_titles_categories_and_chords() {
        let none = Default::default();
        let all: usize = matching_commands("", &none)
            .iter()
            .map(|(_, c)| c.len())
            .sum();
        assert_eq!(all, key_commands().len());
        let panes = matching_commands("split pane", &none);
        assert!(
            panes
                .iter()
                .all(|(category, _)| category.label() == "Panes")
        );
        assert!(!panes.is_empty());
        assert!(matching_commands("no such command at all", &none).is_empty());
        assert_eq!(extras().len(), 7);
        assert_eq!(editor_shortcuts().len(), 5);
    }
}
