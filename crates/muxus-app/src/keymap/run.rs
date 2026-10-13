//! Running a command by id.
//!
//! `runKeyCommand` in the TypeScript calls each command's own closure. Here the
//! commands whose effect is confined to the tabs store run against a
//! [`TabsState`]; the rest (dialogs, the terminal, preferences, windows) are
//! reported back as [`CommandOutcome::NeedsUi`] for the UI to perform.

use super::commands::key_command;
use crate::state::tabs::{PANE_RESIZE_STEP, TabsState};
use crate::state::workspace_layout::PaneDirection;

/// What dispatching a command did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    /// The command ran: `true` when it applied. `false` means "not applicable
    /// right now" and leaves the key to whatever would have received it: the
    /// terminal keeps Alt+← for word motion until a pane actually sits to the left.
    Ran(bool),
    /// The command needs the UI: a dialog, the terminal, preferences or a window.
    NeedsUi,
    /// No command has this id.
    Unknown,
}

/// Preferences the tabs-only commands read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandContext {
    /// `splitInheritsSession`: a split carries the current session into the new pane.
    pub split_inherits_session: bool,
}

fn direction(name: &str) -> Option<PaneDirection> {
    Some(match name {
        "left" => PaneDirection::Left,
        "right" => PaneDirection::Right,
        "up" => PaneDirection::Up,
        "down" => PaneDirection::Down,
        _ => return None,
    })
}

/// Run a command whose effect is confined to the tabs store.
pub fn run_tabs_command(tabs: &mut TabsState, id: &str, context: CommandContext) -> CommandOutcome {
    if key_command(id).is_none() {
        return CommandOutcome::Unknown;
    }
    if let Some(name) = id.strip_prefix("pane.split.")
        && let Some(direction) = direction(name)
    {
        return CommandOutcome::Ran(
            tabs.split_active_pane(direction, context.split_inherits_session),
        );
    }
    if let Some(name) = id.strip_prefix("pane.focus.")
        && let Some(direction) = direction(name)
    {
        return CommandOutcome::Ran(tabs.focus_pane_direction(direction));
    }
    if let Some(name) = id.strip_prefix("tab.to-pane.")
        && let Some(direction) = direction(name)
    {
        return CommandOutcome::Ran(tabs.move_tab_to_direction(direction));
    }
    if let Some(position) = id.strip_prefix("tab.select.") {
        let index = if position == "last" {
            8
        } else {
            position.parse::<usize>().unwrap_or(1) - 1
        };
        return CommandOutcome::Ran(tabs.activate_tab_index(index));
    }
    CommandOutcome::Ran(match id {
        "pane.focus.next" => tabs.cycle_pane(false),
        "pane.focus.previous" => tabs.cycle_pane(true),
        "pane.zoom" => tabs.toggle_zoom(None),
        "pane.grow" => tabs.resize_active_pane(PANE_RESIZE_STEP),
        "pane.shrink" => tabs.resize_active_pane(-PANE_RESIZE_STEP),
        "pane.equalize" => tabs.equalize_panes(),
        "tab.new" => {
            tabs.open_empty();
            true
        }
        "tab.duplicate" => {
            let active = tabs.active_id.clone().unwrap_or_default();
            tabs.duplicate_tab(&active)
        }
        "tab.next" => {
            tabs.cycle(false);
            true
        }
        "tab.previous" => {
            tabs.cycle(true);
            true
        }
        "tab.move.previous" => tabs.move_tab_within_pane(-1),
        "tab.move.next" => tabs.move_tab_within_pane(1),
        "terminal.toggle-editor" => tabs.toggle_editor(),
        "terminal.find" => {
            if tabs.active_id.is_none() {
                false
            } else {
                tabs.request_search();
                true
            }
        }
        _ => return CommandOutcome::NeedsUi,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tabs::IdGen;

    #[test]
    fn runs_tab_commands_and_leaves_the_rest_to_the_ui() {
        let mut tabs = TabsState::with_pane("pane-test", IdGen::sequential());
        let context = CommandContext {
            split_inherits_session: true,
        };
        let first = tabs.open(crate::model::local_profile(), "One");
        tabs.open(crate::model::local_profile(), "Two");

        assert_eq!(
            run_tabs_command(&mut tabs, "tab.select.1", context),
            CommandOutcome::Ran(true)
        );
        assert_eq!(tabs.active_id, Some(first));
        assert_eq!(
            run_tabs_command(&mut tabs, "tab.select.last", context),
            CommandOutcome::Ran(false)
        );
        // A single pane has nothing to zoom, so the key passes through.
        assert_eq!(
            run_tabs_command(&mut tabs, "pane.zoom", context),
            CommandOutcome::Ran(false)
        );
        assert_eq!(
            run_tabs_command(&mut tabs, "pane.split.right", context),
            CommandOutcome::Ran(true)
        );
        assert_eq!(tabs.tabs.len(), 3);
        assert_eq!(
            run_tabs_command(&mut tabs, "pane.focus.left", context),
            CommandOutcome::Ran(true)
        );
        assert_eq!(
            run_tabs_command(&mut tabs, "terminal.find", context),
            CommandOutcome::Ran(true)
        );
        assert_eq!(
            run_tabs_command(&mut tabs, "tab.close", context),
            CommandOutcome::NeedsUi
        );
        assert_eq!(
            run_tabs_command(&mut tabs, "app.settings", context),
            CommandOutcome::NeedsUi
        );
        assert_eq!(
            run_tabs_command(&mut tabs, "no.such.command", context),
            CommandOutcome::Unknown
        );
    }
}
