//! Chord labels for tooltips and menus (port of `client/src/keymap/hints.ts`).

use super::bindings::{KeybindingOverrides, command_chords};
use super::chords::format_chord_string;
use super::commands::key_command;

/// Every chord bound to a command, rendered for the current platform.
pub fn chord_labels(command_id: &str, overrides: &KeybindingOverrides) -> Vec<String> {
    key_command(command_id).map_or_else(Vec::new, |command| {
        command_chords(command, overrides)
            .iter()
            .map(|chord| format_chord_string(chord))
            .collect()
    })
}

/// The chord to show next to a button or menu item, if the command has one
/// (`useChordLabel` without the store subscription).
pub fn chord_label(command_id: &str, overrides: &KeybindingOverrides) -> Option<String> {
    chord_labels(command_id, overrides).into_iter().next()
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_overrides_and_platform() {
        let none = KeybindingOverrides::new();
        assert_eq!(chord_label("app.sidebar", &none).as_deref(), Some("Ctrl+B"));
        assert_eq!(
            chord_labels("terminal.zoom-in", &none),
            vec!["Ctrl+Shift+=", "Ctrl+="]
        );
        assert_eq!(chord_label("pane.equalize", &none), None);
        assert_eq!(chord_label("no.such.command", &none), None);
        let rebound: KeybindingOverrides =
            [("app.sidebar".to_owned(), vec!["Alt+KeyS".to_owned()])].into();
        assert_eq!(
            chord_label("app.sidebar", &rebound).as_deref(),
            Some("Alt+S")
        );
    }
}
