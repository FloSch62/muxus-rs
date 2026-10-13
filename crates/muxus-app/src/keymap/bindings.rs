//! Chord → command lookup with user overrides (port of `client/src/keymap/bindings.ts`).

use std::collections::{HashMap, HashSet};

use super::chords::{ChordEvent, chord_signature, event_signature, parse_chord};
use super::commands::{KEY_COMMANDS, KeyCommand};

/// Command id → chords, replacing that command's defaults. An empty list
/// leaves the command unbound.
pub type KeybindingOverrides = HashMap<String, Vec<String>>;

/// The chords a command answers to, honoring the user's overrides.
pub fn command_chords<'a>(
    command: &'a KeyCommand,
    overrides: &'a KeybindingOverrides,
) -> &'a [String] {
    overrides
        .get(&command.id)
        .unwrap_or(&command.default_chords)
}

pub fn is_command_customized(command: &KeyCommand, overrides: &KeybindingOverrides) -> bool {
    overrides
        .get(&command.id)
        .is_some_and(|chords| !chords_are_default(command, chords))
}

/// Whether a chord list is exactly what the command ships with.
pub fn chords_are_default(command: &KeyCommand, chords: &[String]) -> bool {
    chords.len() == command.default_chords.len()
        && chords
            .iter()
            .zip(&command.default_chords)
            .all(|(left, right)| same_chord(left, right))
}

fn same_chord(left: &str, right: &str) -> bool {
    match (parse_chord(left), parse_chord(right)) {
        (Some(left), Some(right)) => chord_signature(&left) == chord_signature(&right),
        _ => left == right,
    }
}

/// Signature → commands, in catalog order. Several commands can share a
/// signature after rebinding; the dispatcher tries each until one applies and
/// the shortcut settings surface them as a conflict.
#[derive(Clone, Debug, Default)]
pub struct BindingIndex {
    by_signature: HashMap<String, Vec<&'static KeyCommand>>,
}

impl BindingIndex {
    pub fn new(overrides: &KeybindingOverrides) -> Self {
        let mut by_signature: HashMap<String, Vec<&'static KeyCommand>> = HashMap::new();
        for command in KEY_COMMANDS.iter() {
            for text in command_chords(command, overrides) {
                let Some(chord) = parse_chord(text) else {
                    continue;
                };
                by_signature
                    .entry(chord_signature(&chord))
                    .or_default()
                    .push(command);
            }
        }
        BindingIndex { by_signature }
    }

    /// Commands bound to a chord signature, most relevant first.
    pub fn get(&self, signature: &str) -> &[&'static KeyCommand] {
        self.by_signature
            .get(signature)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The hot keydown path: commands a key press would trigger. Keep one
    /// index per preference change rather than building it per key.
    pub fn commands_for_event(&self, event: &ChordEvent) -> &[&'static KeyCommand] {
        self.get(&event_signature(event))
    }

    pub fn signatures(&self) -> impl Iterator<Item = (&String, &Vec<&'static KeyCommand>)> {
        self.by_signature.iter()
    }
}

pub fn build_binding_index(overrides: &KeybindingOverrides) -> BindingIndex {
    BindingIndex::new(overrides)
}

/// Commands that a chord would trigger, most relevant first.
pub fn commands_for_chord(text: &str, overrides: &KeybindingOverrides) -> Vec<&'static KeyCommand> {
    let Some(chord) = parse_chord(text) else {
        return Vec::new();
    };
    BindingIndex::new(overrides)
        .get(&chord_signature(&chord))
        .to_vec()
}

/// Command ids that share at least one chord with another command.
pub fn conflicting_command_ids(overrides: &KeybindingOverrides) -> HashSet<String> {
    let mut conflicts = HashSet::new();
    for (_, commands) in BindingIndex::new(overrides).signatures() {
        if commands.len() < 2 {
            continue;
        }
        for command in commands {
            conflicts.insert(command.id.clone());
        }
    }
    conflicts
}

/// Commands a key press would trigger. Builds the index on every call; the UI
/// keeps a [`BindingIndex`] instead.
pub fn commands_for_event(
    event: &ChordEvent,
    overrides: &KeybindingOverrides,
) -> Vec<&'static KeyCommand> {
    BindingIndex::new(overrides)
        .commands_for_event(event)
        .to_vec()
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::super::chords::key_event;
    use super::super::commands::key_command;
    use super::*;

    fn none() -> KeybindingOverrides {
        KeybindingOverrides::new()
    }

    fn ids(commands: &[&KeyCommand]) -> Vec<String> {
        commands.iter().map(|command| command.id.clone()).collect()
    }

    fn for_chord(text: &str) -> Vec<String> {
        ids(&commands_for_chord(text, &none()))
    }

    fn overrides(entries: &[(&str, &[&str])]) -> KeybindingOverrides {
        entries
            .iter()
            .map(|(id, chords)| {
                (
                    (*id).to_owned(),
                    chords.iter().map(|chord| (*chord).to_owned()).collect(),
                )
            })
            .collect()
    }

    // default bindings

    #[test]
    fn parses_every_default_chord() {
        for command in KEY_COMMANDS.iter() {
            for chord in &command.default_chords {
                assert!(parse_chord(chord).is_some(), "{} → {chord}", command.id);
            }
        }
    }

    #[test]
    fn ships_without_a_single_chord_collision() {
        assert!(conflicting_command_ids(&none()).is_empty());
    }

    #[test]
    fn leaves_the_shell_the_keys_it_owns() {
        // Ctrl+2…Ctrl+8 are control characters (Ctrl+3 is Escape) and Ctrl+W
        // deletes a word, so away from macOS neither can be a shortcut.
        assert!(for_chord("Ctrl+Digit3").is_empty());
        assert!(for_chord("Ctrl+KeyW").is_empty());
        assert_eq!(for_chord("Alt+Digit3"), vec!["tab.select.3"]);
        assert_eq!(for_chord("Ctrl+Shift+KeyW"), vec!["tab.close"]);
    }

    #[test]
    fn binds_multi_execution_to_a_chord_of_its_own() {
        assert_eq!(for_chord("Mod+Shift+M"), vec!["terminal.multi-exec"]);
    }

    #[test]
    fn opens_the_saved_command_menu_with_control_space() {
        assert_eq!(for_chord("Ctrl+Space"), vec!["terminal.command-menu"]);
    }

    #[test]
    fn opens_the_send_secret_picker_with_its_own_chord() {
        let commands = commands_for_chord("Mod+Shift+P", &none());
        assert_eq!(ids(&commands), vec!["terminal.send-secret"]);
        assert_eq!(commands[0].title, "Send secret…");
    }

    #[test]
    fn switches_between_the_terminal_and_open_files_with_control_backquote() {
        let commands = commands_for_chord("Ctrl+Backquote", &none());
        assert_eq!(ids(&commands), vec!["terminal.toggle-editor"]);
        assert!(commands[0].in_editor);
    }

    #[test]
    fn toggles_focus_mode_with_its_default_chord() {
        assert_eq!(for_chord("Mod+Shift+B"), vec!["app.focus-mode"]);
    }

    #[test]
    fn gives_every_direction_its_own_split_focus_and_move_tab_chord() {
        for direction in ["left", "right", "up", "down"] {
            for prefix in ["pane.split", "pane.focus", "tab.to-pane"] {
                let command = key_command(&format!("{prefix}.{direction}")).unwrap();
                assert!(!command_chords(command, &none()).is_empty());
            }
        }
        assert_eq!(for_chord("Mod+Shift+ArrowRight"), vec!["pane.split.right"]);
        assert_eq!(for_chord("Alt+ArrowRight"), vec!["pane.focus.right"]);
        assert_eq!(for_chord("Alt+Shift+ArrowRight"), vec!["tab.to-pane.right"]);
    }

    #[test]
    fn keeps_numbered_tab_selection_on_the_alt_row() {
        assert_eq!(for_chord("Alt+Digit3"), vec!["tab.select.3"]);
        assert_eq!(for_chord("Alt+Digit9"), vec!["tab.select.last"]);
        assert!(for_chord("Alt+Shift+Digit3").is_empty());
    }

    // user overrides

    fn user() -> KeybindingOverrides {
        overrides(&[("pane.zoom", &["Ctrl+Shift+KeyJ"]), ("pane.equalize", &[])])
    }

    #[test]
    fn replaces_the_defaults_of_the_rebound_command_only() {
        assert_eq!(
            ids(&commands_for_chord("Ctrl+Shift+KeyJ", &user())),
            vec!["pane.zoom"]
        );
        assert!(commands_for_chord("Mod+Shift+Z", &user()).is_empty());
        assert_eq!(
            ids(&commands_for_chord("Mod+Shift+ArrowDown", &user())),
            vec!["pane.split.down"]
        );
    }

    #[test]
    fn treats_an_empty_chord_list_as_unbound() {
        let index = build_binding_index(&user());
        assert!(
            !index
                .signatures()
                .flat_map(|(_, commands)| commands.iter())
                .any(|command| command.id == "pane.equalize")
        );
    }

    #[test]
    fn reports_a_chord_shared_with_another_command() {
        let clashing = overrides(&[("pane.zoom", &["Mod+Shift+T"])]);
        let mut conflicts: Vec<_> = conflicting_command_ids(&clashing).into_iter().collect();
        conflicts.sort();
        assert_eq!(conflicts, vec!["pane.zoom", "tab.new"]);
    }

    #[test]
    fn leaves_send_break_unbound_until_a_chord_is_recorded_for_it() {
        assert!(
            key_command("terminal.send-break")
                .unwrap()
                .default_chords
                .is_empty()
        );
        let bound = overrides(&[("terminal.send-break", &["Ctrl+Alt+KeyB"])]);
        assert_eq!(
            ids(&commands_for_chord("Ctrl+Alt+KeyB", &bound)),
            vec!["terminal.send-break"]
        );
        assert!(!conflicting_command_ids(&bound).contains("terminal.send-break"));
    }

    #[test]
    fn recognizes_an_override_that_only_restates_the_default() {
        let zoom = key_command("pane.zoom").unwrap();
        assert!(chords_are_default(zoom, &["Ctrl+Shift+KeyZ".to_owned()]));
        assert!(!is_command_customized(
            zoom,
            &overrides(&[("pane.zoom", &["Ctrl+Shift+KeyZ"])])
        ));
        assert!(is_command_customized(zoom, &user()));
    }

    // keyboard layouts

    fn event_ids(event: ChordEvent) -> Vec<String> {
        ids(&commands_for_event(&event, &none()))
    }

    fn ctrl(key: &str, code: &str) -> ChordEvent {
        key_event(key, code, true, false, false, false)
    }

    fn ctrl_shift(key: &str, code: &str) -> ChordEvent {
        key_event(key, code, true, false, true, false)
    }

    fn alt(key: &str, code: &str) -> ChordEvent {
        key_event(key, code, false, true, false, false)
    }

    #[test]
    fn takes_letters_from_the_key_cap_so_qwertz_presses_the_key_printed_z() {
        // German layout: the cap printed Z sits where QWERTY has Y.
        assert_eq!(event_ids(ctrl_shift("z", "KeyY")), vec!["pane.zoom"]);
        // ...and the cap printed Y must not zoom just because it sits on KeyZ.
        assert!(event_ids(ctrl_shift("y", "KeyZ")).is_empty());
    }

    #[test]
    fn falls_back_to_the_physical_key_when_the_character_is_mangled() {
        // AZERTY types "&" where QWERTY has 1; macOS turns Option+1 into "¡".
        assert_eq!(event_ids(alt("&", "Digit1")), vec!["tab.select.1"]);
        assert_eq!(event_ids(alt("¡", "Digit1")), vec!["tab.select.1"]);
        // Digits typed as digits resolve the same way.
        assert_eq!(event_ids(alt("1", "Digit1")), vec!["tab.select.1"]);
    }

    #[test]
    fn keeps_german_punctuation_on_the_printed_cap() {
        // Ctrl+Shift+? opens the sheet where the layout prints "?" (over ß)...
        assert_eq!(event_ids(ctrl_shift("?", "Minus")), vec!["app.shortcuts"]);
        // ...and the key printed "-" still zooms the terminal out.
        assert_eq!(
            event_ids(ctrl_shift("_", "Slash")),
            vec!["terminal.zoom-out"]
        );
        // Shrinking a pane rides the comma key, which German types as ";".
        assert_eq!(event_ids(ctrl_shift(";", "Comma")), vec!["pane.shrink"]);
    }

    #[test]
    fn zooms_from_the_printed_plus_and_minus_keys_of_either_layout() {
        // German types "+" unshifted on another key; US needs Shift on "=".
        assert_eq!(
            event_ids(ctrl("+", "BracketRight")),
            vec!["terminal.zoom-in"]
        );
        assert_eq!(
            event_ids(ctrl_shift("+", "Equal")),
            vec!["terminal.zoom-in"]
        );
        assert_eq!(event_ids(ctrl("=", "Equal")), vec!["terminal.zoom-in"]);
        assert_eq!(event_ids(ctrl("+", "NumpadAdd")), vec!["terminal.zoom-in"]);
        // German "-" sits on the slash key, US on its own.
        assert_eq!(event_ids(ctrl("-", "Slash")), vec!["terminal.zoom-out"]);
        assert_eq!(event_ids(ctrl("-", "Minus")), vec!["terminal.zoom-out"]);
        assert_eq!(
            event_ids(ctrl_shift("_", "Slash")),
            vec!["terminal.zoom-out"]
        );
    }

    #[test]
    fn leaves_keys_without_a_character_on_their_layout_independent_code() {
        assert_eq!(
            event_ids(ctrl_shift("ArrowRight", "ArrowRight")),
            vec!["pane.split.right"]
        );
        assert_eq!(
            event_ids(alt("ArrowLeft", "ArrowLeft")),
            vec!["pane.focus.left"]
        );
    }
}
