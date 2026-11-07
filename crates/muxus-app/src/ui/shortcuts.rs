//! Window-wide keyboard shortcuts: `client/src/shortcuts.ts`.
//!
//! One key-down listener on the window root owns every app shortcut. A key
//! press is translated to the DOM `KeyboardEvent` shape the keymap speaks
//! (`ChordEvent`), looked up in the binding index, and the first command that
//! applies runs. While a text field has focus nothing runs, as in the client.
//!
//! Command button shortcuts are tried after the app's commands, and only while
//! a terminal has focus: a button types into the focused session.

use std::cell::RefCell;

use muxus_app::features::command_buttons::run::run_command_button;
use muxus_app::features::settings::vault_secrets::{find_vault_secret, vault_secret_missing};
use muxus_app::keymap::bindings::{BindingIndex, KeybindingOverrides};
use muxus_app::keymap::command_buttons::command_buttons_for_event;
use muxus_app::keymap::keystroke::chord_event;
use muxus_app::keymap::run::{CommandContext, CommandOutcome, run_tabs_command};
use muxus_app::state::tabs::is_multi_exec_target;
use muxus_store::prefs::command_buttons::CommandButton;
use muxus_ui::TERMINAL_KEY_CONTEXT;
use muxus_ui::gpui_kit::{Context, KeyDownEvent, Window};

use crate::ui::shell::AppShell;

thread_local! {
    /// The button whose chord is held: a click never repeats, so a held button chord runs
    /// once, and its auto-repeats are swallowed rather than typed into the shell.
    static HELD_BUTTON: RefCell<Option<String>> = const { RefCell::new(None) };
}

impl AppShell {
    /// The user's chord overrides (`keybindings`).
    pub fn keybinding_overrides(&self) -> KeybindingOverrides {
        self.prefs
            .keybindings
            .iter()
            .map(|(id, chords)| (id.clone(), chords.clone()))
            .collect()
    }

    /// Text entry always wins: bindings never fire inside inputs. The
    /// terminal is the exception — it is where terminal chords belong.
    fn typing_in_text_field(window: &Window) -> bool {
        let stack = window.context_stack();
        let terminal = stack
            .iter()
            .any(|context| context.contains(TERMINAL_KEY_CONTEXT));
        !terminal && stack.iter().any(|context| context.contains("Input"))
    }

    pub fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The shortcuts editor is recording a chord: every key belongs to it.
        if muxus_app::features::settings::view::shortcuts::chord_capture_active(cx)
            || self.modal_open()
            || Self::typing_in_text_field(window)
        {
            return;
        }
        if self.sidebar.read(cx).search_focused(window, cx) {
            return;
        }
        let Some(chord) = chord_event(&event.keystroke) else {
            return;
        };
        if !event.is_held {
            HELD_BUTTON.with(|held| held.borrow_mut().take());
        }
        let index = BindingIndex::new(&self.keybinding_overrides());
        let in_terminal = window
            .context_stack()
            .iter()
            .any(|context| context.contains(TERMINAL_KEY_CONTEXT));
        for command in index.commands_for_event(&chord) {
            // A command that types into the active session only answers keys the terminal
            // itself would have taken, never one aimed at a dialog or the sidebar.
            if command.types_into_session && !in_terminal {
                continue;
            }
            if self.run_command(&command.id, window, cx) {
                cx.stop_propagation();
                return;
            }
        }
        // A button types into the focused session, so its shortcut answers only there.
        if !in_terminal {
            return;
        }
        let buttons = self.prefs.command_buttons.clone();
        for button in command_buttons_for_event(&chord, &buttons) {
            let handled = if event.is_held {
                HELD_BUTTON.with(|held| held.borrow().as_deref() == Some(button.id.as_str()))
            } else {
                self.run_command_button_shortcut(button, window, cx)
            };
            if handled {
                HELD_BUTTON.with(|held| *held.borrow_mut() = Some(button.id.clone()));
                cx.stop_propagation();
                return;
            }
        }
    }

    /// `runCommandButtonShortcut`: run a button from its shortcut the way a click does. False,
    /// so the key reaches the shell, unless a connected terminal is active and the button has
    /// something to send, as the Ctrl+Space menu judges it.
    fn run_command_button_shortcut(
        &mut self,
        button: &CommandButton,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab_id) = self
            .tabs
            .active_id
            .clone()
            .filter(|id| self.tabs.tab(id).is_some_and(is_multi_exec_target))
        else {
            return false;
        };
        let vault = self
            .features
            .command_buttons
            .as_ref()
            .and_then(|model| model.read(cx).vault().cloned());
        let secret_name = match button.secret_id.as_deref() {
            None => {
                if muxus_core::js::js_trim(&button.command).is_empty() {
                    return false;
                }
                None
            }
            Some(id) => {
                if id.is_empty() || vault_secret_missing(vault.as_ref(), id) == Some(true) {
                    return false;
                }
                find_vault_secret(vault.as_ref(), id).map(|secret| secret.name.clone())
            }
        };
        run_command_button(Some(&tab_id), button, secret_name.as_deref(), window, cx)
    }

    /// Run one keyboard command (`command.run()`); `false` when it does not apply now.
    pub fn run_command(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let context = CommandContext {
            split_inherits_session: self.prefs.split_inherits_session,
        };
        let handled = match id {
            "app.sidebar" => {
                self.prefs.sidebar_collapsed = !self.prefs.sidebar_collapsed;
                self.prefs_changed(cx);
                true
            }
            "app.focus-mode" => {
                self.focus_mode = !self.focus_mode;
                self.menu = None;
                true
            }
            "app.settings" => {
                self.open_settings(None, cx);
                true
            }
            "app.shortcuts" => {
                self.open_shortcuts(window, cx);
                true
            }
            "app.scripts" => {
                self.open_scripts(None, window, cx);
                true
            }
            "app.quick-launcher" => {
                self.open_launcher(window, cx);
                true
            }
            id => match self.run_ui_command(id, window, cx) {
                Some(handled) => handled,
                None => matches!(
                    run_tabs_command(&mut self.tabs, id, context),
                    CommandOutcome::Ran(true)
                ),
            },
        };
        if handled {
            self.tabs_changed(cx);
        }
        handled
    }
}
