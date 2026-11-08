//! Command buttons run from their own keyboard shortcuts (port of
//! `client/src/keymap/command-buttons.ts`, and the shortcut half of `normalizeCommandButtons`
//! in `client/src/command-buttons.ts`, which needs the chord parser).
//!
//! A button with a shortcut is a keyboard command of the `Buttons` category: it shares the
//! conflict checks and the keyboard sheet with the app's own commands, and the dispatcher tries
//! it after them.

use std::collections::HashSet;

use muxus_store::prefs::command_buttons::{CommandButton, command_button_label};

use super::bindings::{KeybindingOverrides, commands_for_chord};
use super::chords::{
    ChordEvent, chord_signature, event_signature, format_chord_string, is_bindable_chord,
    parse_chord, sends_control_character,
};
use super::commands::{CommandCategory, KeyCommand};

/// The keyboard command id a button's shortcut is registered under.
pub fn command_button_command_id(button_id: &str) -> String {
    format!("command-button:{button_id}")
}

/// The button a `command-button:` command id names.
pub fn command_button_id(command_id: &str) -> Option<&str> {
    command_id.strip_prefix("command-button:")
}

/// A stored chord the shell keeps, which no button runs from.
pub fn shell_owns(text: &str) -> bool {
    parse_chord(text).is_some_and(|chord| sends_control_character(&chord))
}

/// The chord that runs `button`, and so the one the bar and the Ctrl+Space menu show. A saved
/// chord the shell owns is left out, whatever saved it.
pub fn command_button_shortcut(button: &CommandButton) -> Option<&str> {
    button
        .shortcut
        .as_deref()
        .filter(|shortcut| !shell_owns(shortcut))
}

/// Buttons that carry a shortcut, as keyboard commands.
pub fn command_button_commands(buttons: &[CommandButton]) -> Vec<KeyCommand> {
    buttons
        .iter()
        .filter_map(|button| {
            let shortcut = command_button_shortcut(button)?;
            Some(KeyCommand {
                id: command_button_command_id(&button.id),
                title: command_button_label(button),
                category: CommandCategory::Buttons,
                default_chords: vec![shortcut.to_string()],
                keywords: Vec::new(),
                palette: false,
                in_editor: false,
                // Buttons answer only in a terminal by their category already.
                types_into_session: false,
            })
        })
        .collect()
}

/// The buttons a key press would run, after the app's own commands.
pub fn command_buttons_for_event<'a>(
    event: &ChordEvent,
    buttons: &'a [CommandButton],
) -> Vec<&'a CommandButton> {
    let signature = event_signature(event);
    buttons
        .iter()
        .filter(|button| {
            command_button_shortcut(button)
                .and_then(parse_chord)
                .is_some_and(|chord| chord_signature(&chord) == signature)
        })
        .collect()
}

/// Why a button cannot take `chord`: the shell owns it, or a command or another button already
/// answers to it, named. `None` when the chord is free.
pub fn command_button_shortcut_conflict(
    chord: &str,
    button_id: &str,
    keybindings: &KeybindingOverrides,
    buttons: &[CommandButton],
) -> Option<String> {
    if shell_owns(chord) {
        return Some(format!(
            "{} is kept for the shell.",
            format_chord_string(chord)
        ));
    }
    if let Some(command) = commands_for_chord(chord, keybindings).first() {
        return Some(format!(
            "{} already runs “{}”.",
            format_chord_string(chord),
            command.title
        ));
    }
    let signature = parse_chord(chord).map(|chord| chord_signature(&chord))?;
    let taken = buttons
        .iter()
        .filter(|button| button.id != button_id)
        .find(|button| {
            command_button_shortcut(button)
                .and_then(parse_chord)
                .is_some_and(|chord| chord_signature(&chord) == signature)
        })?;
    Some(format!(
        "{} already runs the command button “{}”.",
        format_chord_string(chord),
        command_button_label(taken)
    ))
}

/// Command ids that share a chord with another command or button (`conflictingCommandIds`
/// with the buttons as extra commands).
pub fn conflicting_ids_with_buttons(
    keybindings: &KeybindingOverrides,
    buttons: &[CommandButton],
) -> HashSet<String> {
    let mut conflicts = super::bindings::conflicting_command_ids(keybindings);
    let index = super::bindings::BindingIndex::new(keybindings);
    let extra = command_button_commands(buttons);
    for command in &extra {
        let Some(signature) = command
            .default_chords
            .first()
            .and_then(|chord| parse_chord(chord))
            .map(|chord| chord_signature(&chord))
        else {
            continue;
        };
        let commands = index.get(&signature);
        let buttons_sharing: Vec<&KeyCommand> = extra
            .iter()
            .filter(|other| {
                other
                    .default_chords
                    .first()
                    .and_then(|chord| parse_chord(chord))
                    .is_some_and(|chord| chord_signature(&chord) == signature)
            })
            .collect();
        if commands.len() + buttons_sharing.len() < 2 {
            continue;
        }
        conflicts.extend(commands.iter().map(|command| command.id.clone()));
        conflicts.extend(buttons_sharing.iter().map(|command| command.id.clone()));
    }
    conflicts
}

/// The shortcut half of `normalizeCommandButtons`: a shortcut that is not a bindable chord,
/// belongs to the shell, or that an earlier button already holds, is dropped.
pub fn keep_bindable_shortcuts(buttons: &mut [CommandButton]) {
    let mut taken = HashSet::new();
    for button in buttons {
        let Some(shortcut) = button.shortcut.as_deref() else {
            continue;
        };
        let signature = parse_chord(shortcut)
            .filter(|chord| is_bindable_chord(chord) && !sends_control_character(chord))
            .map(|chord| chord_signature(&chord));
        let free = signature
            .as_ref()
            .is_some_and(|signature| !taken.contains(signature));
        if let Some(signature) = signature {
            taken.insert(signature);
        }
        if !free {
            button.shortcut = None;
        }
    }
}

/// Port of the pure cases of `tests/unit/client/command-button-shortcuts.test.ts`, and the
/// shortcut cases of `command-buttons.test.ts` and `keymap.test.ts`.
#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::super::chords::key_event;
    use super::*;

    fn saved(id: &str, label: &str, shortcut: Option<&str>) -> CommandButton {
        CommandButton {
            id: id.into(),
            label: label.into(),
            command: format!("{id}\r"),
            send_enter: true,
            group_id: None,
            color: None,
            secret_id: None,
            shortcut: shortcut.map(str::to_owned),
        }
    }

    fn none() -> KeybindingOverrides {
        KeybindingOverrides::new()
    }

    #[test]
    fn knows_the_chords_the_shell_owns() {
        let owned = |text: &str| sends_control_character(&parse_chord(text).unwrap());
        for chord in [
            "Ctrl+KeyC",
            "Ctrl+KeyW",
            "Ctrl+Space",
            // xterm.js sends nothing for it, but the keymap leaves Ctrl+2…8 to the terminal.
            "Ctrl+Digit2",
            "Ctrl+Digit3",
            "Ctrl+Digit8",
            "Ctrl+BracketLeft",
            "Ctrl+Backslash",
            "Ctrl+Slash",
            "Ctrl+Backspace",
            "Ctrl+Shift+Backspace",
            "Ctrl+Shift+Digit2",
            "Ctrl+Shift+Digit6",
            "Ctrl+Shift+Minus",
        ] {
            assert!(owned(chord), "{chord}");
        }
        // Ctrl+Shift with a letter is where the defaults live; Alt, Cmd and other keys stay free.
        for chord in [
            "Ctrl+Shift+KeyC",
            "Ctrl+Alt+KeyC",
            "Ctrl+Alt+Backspace",
            "Meta+KeyC",
            "Ctrl+Digit1",
            "Ctrl+PageDown",
            "Alt+KeyC",
        ] {
            assert!(!owned(chord), "{chord}");
        }
    }

    #[test]
    fn registers_only_the_buttons_that_carry_a_shortcut() {
        let buttons = [
            saved("disk", "Disk usage", Some("Ctrl+Alt+KeyD")),
            saved("plain", "Plain", None),
            saved("shell", "Interrupt", Some("Ctrl+KeyC")),
        ];
        let commands = command_button_commands(&buttons);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].id, "command-button:disk");
        assert_eq!(commands[0].title, "Disk usage");
        assert_eq!(commands[0].category, CommandCategory::Buttons);
        assert_eq!(commands[0].default_chords, ["Ctrl+Alt+KeyD"]);
        assert!(!commands[0].palette);
        assert_eq!(command_button_id("command-button:disk"), Some("disk"));
    }

    #[test]
    fn finds_the_button_a_pressed_chord_runs() {
        let buttons = [
            saved("disk", "Disk usage", Some("Ctrl+Alt+KeyD")),
            saved("shell", "Interrupt", Some("Ctrl+KeyC")),
        ];
        let pressed = key_event("d", "KeyD", true, true, false, false);
        let found = command_buttons_for_event(&pressed, &buttons);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "disk");
        // A chord the shell owns stays with the shell, even when saved prefs still hold it.
        let interrupt = key_event("c", "KeyC", true, false, false, false);
        assert!(command_buttons_for_event(&interrupt, &buttons).is_empty());
    }

    #[test]
    fn accepts_a_free_chord_and_the_chord_the_button_already_has() {
        let buttons = [saved("disk", "Disk usage", Some("Ctrl+Alt+KeyD"))];
        assert_eq!(
            command_button_shortcut_conflict("Ctrl+Alt+KeyU", "disk", &none(), &buttons),
            None
        );
        assert_eq!(
            command_button_shortcut_conflict("Ctrl+Alt+KeyD", "disk", &none(), &buttons),
            None
        );
    }

    #[test]
    fn refuses_a_chord_an_app_command_answers_to_and_names_the_command() {
        let conflict =
            command_button_shortcut_conflict("Ctrl+Shift+KeyC", "disk", &none(), &[]).unwrap();
        assert!(
            conflict.starts_with("Ctrl+Shift+C already runs “"),
            "{conflict}"
        );
        // A rebound command frees its default chord.
        let mut rebound = none();
        rebound.insert("terminal.copy".into(), vec!["Ctrl+Alt+KeyY".into()]);
        assert_eq!(
            command_button_shortcut_conflict("Ctrl+Shift+KeyC", "disk", &rebound, &[]),
            None
        );
    }

    #[test]
    fn refuses_a_chord_another_button_holds_and_names_the_button() {
        let buttons = [
            saved("disk", "Disk usage", Some("Ctrl+Alt+KeyD")),
            saved("up", "Uptime", None),
        ];
        assert_eq!(
            command_button_shortcut_conflict("Ctrl+Alt+KeyD", "up", &none(), &buttons).as_deref(),
            Some("Ctrl+Alt+D already runs the command button “Disk usage”.")
        );
    }

    #[test]
    fn refuses_the_chords_the_shell_keeps_and_says_so() {
        assert_eq!(
            command_button_shortcut_conflict("Ctrl+KeyC", "disk", &none(), &[]).as_deref(),
            Some("Ctrl+C is kept for the shell.")
        );
        assert_eq!(
            command_button_shortcut_conflict("Ctrl+Space", "disk", &none(), &[]).as_deref(),
            Some("Ctrl+Space is kept for the shell.")
        );
    }

    #[test]
    fn marks_buttons_and_commands_that_share_a_chord() {
        let buttons = [
            saved("one", "One", Some("Ctrl+Alt+KeyO")),
            saved("two", "Two", Some("Ctrl+Alt+KeyO")),
            saved("three", "Three", Some("Ctrl+Alt+KeyT")),
        ];
        let conflicts = conflicting_ids_with_buttons(&none(), &buttons);
        assert!(conflicts.contains("command-button:one"));
        assert!(conflicts.contains("command-button:two"));
        assert!(!conflicts.contains("command-button:three"));
    }

    #[test]
    fn keeps_a_shortcut_only_while_it_is_a_bindable_chord_the_shell_does_not_own_and_no_earlier_button_holds()
     {
        let mut buttons = vec![
            saved("a", "A", Some("Ctrl+Alt+KeyS")),
            saved("b", "B", Some("Alt+Ctrl+S")),
            saved("c", "C", Some("Shift+KeyR")),
            saved("typo", "Typo", Some("Hyper+KeyR")),
            saved("interrupt", "Interrupt", Some("Ctrl+KeyC")),
            saved("f5", "F5", Some("F5")),
        ];
        keep_bindable_shortcuts(&mut buttons);
        let shortcuts: Vec<Option<&str>> = buttons
            .iter()
            .map(|button| button.shortcut.as_deref())
            .collect();
        assert_eq!(
            shortcuts,
            [Some("Ctrl+Alt+KeyS"), None, None, None, None, Some("F5")]
        );
    }
}
