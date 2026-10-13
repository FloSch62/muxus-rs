//! Right-click behaviour (port of `client/src/terminal/right-click.ts`).

use serde::{Deserialize, Serialize};

/// The right-click preference: the terminal-emulator convention (copy the selection when there is
/// one, otherwise paste), always paste, or a context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RightClickAction {
    #[default]
    CopyPaste,
    Paste,
    Menu,
}

/// Keep the platform convention (word selection on macOS) only when it is useful to Muxus's
/// context menu; every other path must leave the selection alone.
pub fn right_click_selects_word(
    platform_default: bool,
    action: RightClickAction,
    has_selection: bool,
) -> bool {
    platform_default && action == RightClickAction::Menu && !has_selection
}

/// What one right click does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RightClickIntent {
    Copy { selection: String },
    Paste,
    Menu { selection: String },
}

/// Resolve one right click from the selection that existed when it occurred.
pub fn terminal_right_click_intent(action: RightClickAction, selection: &str) -> RightClickIntent {
    match action {
        RightClickAction::Menu => RightClickIntent::Menu {
            selection: selection.into(),
        },
        RightClickAction::CopyPaste if !selection.is_empty() => RightClickIntent::Copy {
            selection: selection.into(),
        },
        _ => RightClickIntent::Paste,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RightClickAction::*;

    #[test]
    fn preserves_the_macos_word_selection_convention_only_for_an_empty_context_menu() {
        assert!(right_click_selects_word(true, Menu, false));
        assert!(!right_click_selects_word(true, Menu, true));
        assert!(!right_click_selects_word(true, CopyPaste, false));
        assert!(!right_click_selects_word(true, Paste, false));
    }

    #[test]
    fn keeps_word_selection_disabled_on_linux_and_windows() {
        assert!(!right_click_selects_word(false, Menu, false));
        assert!(!right_click_selects_word(false, Menu, true));
        assert!(!right_click_selects_word(false, CopyPaste, false));
        assert!(!right_click_selects_word(false, Paste, false));
    }

    #[test]
    fn copies_the_same_existing_selection_regardless_of_the_click_position() {
        let selection = "the original\nselection";
        assert_eq!(
            terminal_right_click_intent(CopyPaste, selection),
            RightClickIntent::Copy {
                selection: selection.into()
            }
        );
    }

    #[test]
    fn pastes_when_copy_paste_mode_starts_without_a_selection() {
        assert_eq!(
            terminal_right_click_intent(CopyPaste, ""),
            RightClickIntent::Paste
        );
    }

    #[test]
    fn always_pastes_in_paste_mode_even_when_text_is_selected() {
        assert_eq!(
            terminal_right_click_intent(Paste, "leave this selected"),
            RightClickIntent::Paste
        );
    }

    #[test]
    fn snapshots_the_selection_for_context_menu_copy() {
        assert_eq!(
            terminal_right_click_intent(Menu, "selected when opened"),
            RightClickIntent::Menu {
                selection: "selected when opened".into()
            }
        );
        assert_eq!(
            terminal_right_click_intent(Menu, ""),
            RightClickIntent::Menu {
                selection: String::new()
            }
        );
    }

    #[test]
    fn reads_the_stored_preference() {
        assert_eq!(
            serde_json::from_str::<RightClickAction>("\"copy-paste\"").unwrap(),
            CopyPaste
        );
        assert_eq!(serde_json::to_string(&Menu).unwrap(), "\"menu\"");
    }
}
