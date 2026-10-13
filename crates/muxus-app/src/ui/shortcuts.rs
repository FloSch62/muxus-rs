//! Window-wide keyboard shortcuts: `client/src/shortcuts.ts`.
//!
//! One key-down listener on the window root owns every app shortcut. A key
//! press is translated to the DOM `KeyboardEvent` shape the keymap speaks
//! (`ChordEvent`), looked up in the binding index, and the first command that
//! applies runs. While a text field has focus nothing runs, as in the client.

use muxus_app::keymap::bindings::{BindingIndex, KeybindingOverrides};
use muxus_app::keymap::keystroke::chord_event;
use muxus_app::keymap::run::{CommandContext, CommandOutcome, run_tabs_command};
use muxus_ui::TERMINAL_KEY_CONTEXT;
use muxus_ui::gpui_kit::{Context, KeyDownEvent, Window};

use crate::ui::shell::AppShell;

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
        let index = BindingIndex::new(&self.keybinding_overrides());
        for command in index.commands_for_event(&chord) {
            if self.run_command(&command.id, window, cx) {
                cx.stop_propagation();
                return;
            }
        }
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
