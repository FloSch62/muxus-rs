//! The keyboard command catalog (port of `client/src/keymap/commands.ts`).
//!
//! The TypeScript commands carry their own `run` closures. Here a command is
//! data, and running one is a dispatch on its id: [`super::run`] performs the
//! ones that only touch the tabs store, and the UI performs the rest
//! (dialogs, the terminal, preferences).

use std::collections::HashMap;
use std::sync::LazyLock;

use super::chords::IS_MAC;
use crate::state::workspace_layout::PaneDirection;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommandCategory {
    Panes,
    Tabs,
    Terminal,
    App,
}

impl CommandCategory {
    pub fn label(self) -> &'static str {
        match self {
            CommandCategory::Panes => "Panes",
            CommandCategory::Tabs => "Tabs",
            CommandCategory::Terminal => "Terminal",
            CommandCategory::App => "Application",
        }
    }
}

/// Category order for the shortcut list.
pub const COMMAND_CATEGORIES: [CommandCategory; 4] = [
    CommandCategory::Panes,
    CommandCategory::Tabs,
    CommandCategory::Terminal,
    CommandCategory::App,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyCommand {
    pub id: String,
    pub title: String,
    pub category: CommandCategory,
    /// Chords applied when the user has not rebound the command.
    pub default_chords: Vec<String>,
    /// Extra words the command palette should match on.
    pub keywords: Vec<String>,
    /// False for commands too numerous or too situational to list in the palette.
    pub palette: bool,
    /// Also answers while the file editor has focus, where other bindings stay quiet.
    pub in_editor: bool,
}

/// Chords only offered where Command is the modifier: Ctrl belongs to the shell.
fn mac_only(chord: String) -> Vec<String> {
    if IS_MAC { vec![chord] } else { Vec::new() }
}

fn arrow(direction: PaneDirection) -> &'static str {
    match direction {
        PaneDirection::Left => "ArrowLeft",
        PaneDirection::Right => "ArrowRight",
        PaneDirection::Up => "ArrowUp",
        PaneDirection::Down => "ArrowDown",
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn command(
    id: &str,
    title: &str,
    category: CommandCategory,
    chords: &[&str],
    keywords: &[&str],
) -> KeyCommand {
    KeyCommand {
        id: id.into(),
        title: title.into(),
        category,
        default_chords: strings(chords),
        keywords: strings(keywords),
        palette: true,
        in_editor: false,
    }
}

fn hidden(mut command: KeyCommand) -> KeyCommand {
    command.palette = false;
    command
}

fn split_command(direction: PaneDirection, extra_chords: &[&str]) -> KeyCommand {
    let name = direction.as_str();
    let mut chords = vec![format!("Mod+Shift+{}", arrow(direction))];
    chords.extend(strings(extra_chords));
    KeyCommand {
        id: format!("pane.split.{name}"),
        title: format!("Split pane {name}"),
        category: CommandCategory::Panes,
        default_chords: chords,
        keywords: strings(&["split", "divide", "new pane", name]),
        palette: true,
        in_editor: false,
    }
}

fn focus_command(direction: PaneDirection) -> KeyCommand {
    let name = direction.as_str();
    KeyCommand {
        id: format!("pane.focus.{name}"),
        title: format!("Focus pane {name}"),
        category: CommandCategory::Panes,
        default_chords: vec![format!("Alt+{}", arrow(direction))],
        keywords: strings(&["focus", "move", "navigate", name]),
        palette: true,
        in_editor: false,
    }
}

fn move_tab_command(direction: PaneDirection) -> KeyCommand {
    let name = direction.as_str();
    KeyCommand {
        id: format!("tab.to-pane.{name}"),
        title: format!("Move tab to pane {name}"),
        category: CommandCategory::Tabs,
        default_chords: vec![format!("Alt+Shift+{}", arrow(direction))],
        keywords: strings(&["move", "send", "detach", "split off", name]),
        palette: true,
        in_editor: false,
    }
}

fn select_tab_command(position: usize, id: Option<&str>) -> KeyCommand {
    let id = id.map_or_else(|| position.to_string(), str::to_owned);
    // Ctrl+2…Ctrl+8 are control characters (Ctrl+3 is Escape); leave them
    // to the terminal and take the Alt row that every terminal app uses.
    let mut chords = vec![format!("Alt+Digit{position}")];
    chords.extend(mac_only(format!("Mod+Digit{position}")));
    KeyCommand {
        id: format!("tab.select.{id}"),
        title: format!("Go to tab {position}"),
        category: CommandCategory::Tabs,
        default_chords: chords,
        keywords: vec![
            "switch".into(),
            "jump".into(),
            "numbered tab".into(),
            format!("tab {position}"),
        ],
        palette: false,
        in_editor: false,
    }
}

fn build_commands() -> Vec<KeyCommand> {
    use CommandCategory::{App, Panes, Tabs, Terminal};
    use PaneDirection::{Down, Left, Right, Up};

    let mut commands = vec![
        // Panes
        split_command(Right, &["Alt+Shift+Equal"]),
        split_command(Down, &["Alt+Shift+Minus"]),
        split_command(Left, &[]),
        split_command(Up, &[]),
        focus_command(Left),
        focus_command(Right),
        focus_command(Up),
        focus_command(Down),
        command(
            "pane.focus.next",
            "Focus next pane",
            Panes,
            &["Mod+Shift+O"],
            &["cycle", "rotate", "other pane"],
        ),
        command(
            "pane.focus.previous",
            "Focus previous pane",
            Panes,
            &[],
            &["cycle", "rotate"],
        ),
        command(
            "pane.zoom",
            "Zoom pane / restore layout",
            Panes,
            &["Mod+Shift+Z"],
            &["maximize", "fullscreen", "expand", "tmux"],
        ),
        command(
            "pane.grow",
            "Grow pane",
            Panes,
            &["Mod+Shift+Period"],
            &["resize", "wider", "taller"],
        ),
        command(
            "pane.shrink",
            "Shrink pane",
            Panes,
            &["Mod+Shift+Comma"],
            &["resize", "narrower", "shorter"],
        ),
        command(
            "pane.equalize",
            "Even out panes",
            Panes,
            &[],
            &["balance", "reset", "equal", "layout"],
        ),
        command(
            "pane.close",
            "Close pane",
            Panes,
            &["Mod+Shift+X"],
            &["remove", "kill"],
        ),
        // Tabs
        command(
            "tab.new",
            "New tab",
            Tabs,
            &["Mod+Shift+T"],
            &["open", "session"],
        ),
        command(
            "tab.duplicate",
            "Duplicate tab",
            Tabs,
            &["Mod+Shift+D"],
            &["clone", "second session"],
        ),
    ];
    // Ctrl+W deletes a word in every shell, so only Command takes it.
    let mut close = command(
        "tab.close",
        "Close tab",
        Tabs,
        &["Mod+Shift+W"],
        &["end session"],
    );
    close.default_chords.extend(mac_only("Mod+W".into()));
    commands.push(close);
    commands.extend([
        command(
            "tab.next",
            "Next tab",
            Tabs,
            &["Ctrl+PageDown", "Mod+Shift+BracketRight"],
            &["cycle", "switch"],
        ),
        command(
            "tab.previous",
            "Previous tab",
            Tabs,
            &["Ctrl+PageUp", "Mod+Shift+BracketLeft"],
            &["cycle", "switch"],
        ),
        command(
            "tab.move.previous",
            "Move tab left in the strip",
            Tabs,
            &["Ctrl+Shift+PageUp"],
            &["reorder", "rearrange"],
        ),
        command(
            "tab.move.next",
            "Move tab right in the strip",
            Tabs,
            &["Ctrl+Shift+PageDown"],
            &["reorder", "rearrange"],
        ),
        move_tab_command(Left),
        move_tab_command(Right),
        move_tab_command(Up),
        move_tab_command(Down),
    ]);
    for position in 1..=8 {
        commands.push(select_tab_command(position, None));
    }
    // Keep the legacy command id so existing custom bindings survive, while
    // making 9 select the tab visibly labelled 9 instead of an unrelated last tab.
    commands.push(select_tab_command(9, Some("last")));

    // Terminal
    commands.extend([
        hidden(command(
            "terminal.copy",
            "Copy selection",
            Terminal,
            &["Mod+Shift+C"],
            &[],
        )),
        hidden(command(
            "terminal.paste",
            "Paste into terminal",
            Terminal,
            &["Mod+Shift+V"],
            &[],
        )),
        // Ctrl+Space deliberately takes NUL from the shell: it matches the
        // MobaXterm macro picker and remains Control (not Command) on macOS.
        command(
            "terminal.command-menu",
            "Show saved command menu",
            Terminal,
            &["Ctrl+Space"],
            &["command buttons", "commands", "macros", "MobaXterm"],
        ),
        command(
            "terminal.send-secret",
            "Send secret…",
            Terminal,
            &["Mod+Shift+P"],
            &[
                "password",
                "vault",
                "enable",
                "sudo",
                "pin",
                "type secret",
                "credential",
            ],
        ),
    ]);
    // VS Code's terminal toggle. It only claims the key while the session has
    // files open, so the shell keeps it otherwise.
    let mut toggle_editor = command(
        "terminal.toggle-editor",
        "Switch between terminal and open files",
        Terminal,
        &["Ctrl+Backquote"],
        &["editor", "monaco", "files", "show terminal"],
    );
    toggle_editor.in_editor = true;
    commands.push(toggle_editor);
    commands.extend([
        command(
            "terminal.find",
            "Find in terminal",
            Terminal,
            &["Mod+Shift+F"],
            &["search", "scrollback"],
        ),
        hidden(command(
            "terminal.select-all",
            "Select all terminal output",
            Terminal,
            &["Mod+Shift+A"],
            &[],
        )),
        command(
            "terminal.clear",
            "Clear scrollback",
            Terminal,
            &["Mod+Shift+K"],
            &["erase", "reset"],
        ),
        command(
            "terminal.multi-exec",
            "Toggle multi-execution",
            Terminal,
            &["Mod+Shift+M"],
            &[
                "multi-exec",
                "mirror",
                "broadcast",
                "sync input",
                "all sessions",
            ],
        ),
        command(
            "terminal.force-reconnect-all",
            "Force reconnect all remote sessions",
            Terminal,
            &[],
            &[
                "ssh", "telnet", "serial", "restart", "replace", "stuck", "hung",
            ],
        ),
        command(
            "terminal.send-break",
            "Send BREAK",
            Terminal,
            &[],
            &[
                "break", "serial", "telnet", "ssh", "console", "rommon", "sysrq", "openboot",
            ],
        ),
        command(
            "terminal.cancel-login-sequence",
            "Cancel login sequence",
            Terminal,
            &[],
            &["login", "sequence", "expect", "stop", "abort"],
        ),
        command(
            "terminal.zoom-in",
            "Increase terminal font size",
            Terminal,
            &["Mod+Shift+Equal", "Mod+Equal"],
            &["zoom", "bigger"],
        ),
        command(
            "terminal.zoom-out",
            "Decrease terminal font size",
            Terminal,
            &["Mod+Shift+Minus", "Mod+Minus"],
            &["zoom", "smaller"],
        ),
        command(
            "terminal.zoom-reset",
            "Reset terminal font size",
            Terminal,
            &["Mod+Shift+Digit0", "Mod+Digit0"],
            &["zoom", "default"],
        ),
        // Application
        hidden(command(
            "app.quick-launcher",
            "Open quick launcher",
            App,
            &["Mod+K"],
            &[],
        )),
        command(
            "app.sidebar",
            "Toggle hosts sidebar",
            App,
            &["Mod+B"],
            &["hosts", "sessions", "sidebar", "hide", "show"],
        ),
        command(
            "app.status-bar",
            "Toggle status bar",
            App,
            &[],
            &[
                "status",
                "statistics",
                "cpu",
                "memory",
                "disk",
                "uptime",
                "hide",
                "show",
            ],
        ),
        command(
            "app.focus-mode",
            "Toggle focus mode",
            App,
            &["Mod+Shift+B"],
            &["distraction free", "zen", "fullscreen", "hide chrome"],
        ),
        hidden(command(
            "app.settings",
            "Open settings",
            App,
            &["Mod+Comma"],
            &[],
        )),
        command(
            "app.shortcuts",
            "Show keyboard shortcuts",
            App,
            &["Mod+Shift+Slash"],
            &["keys", "bindings", "help", "cheat sheet"],
        ),
    ]);
    commands
}

/// Every command, in catalog order.
pub static KEY_COMMANDS: LazyLock<Vec<KeyCommand>> = LazyLock::new(build_commands);

static BY_ID: LazyLock<HashMap<&'static str, &'static KeyCommand>> = LazyLock::new(|| {
    KEY_COMMANDS
        .iter()
        .map(|command| (command.id.as_str(), command))
        .collect()
});

pub fn key_commands() -> &'static [KeyCommand] {
    &KEY_COMMANDS
}

pub fn key_command(id: &str) -> Option<&'static KeyCommand> {
    BY_ID.get(id).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        assert_eq!(BY_ID.len(), KEY_COMMANDS.len());
    }

    #[test]
    fn catalog_matches_the_typescript_table() {
        assert_eq!(KEY_COMMANDS.len(), 56);
        let first = &KEY_COMMANDS[0];
        assert_eq!(first.id, "pane.split.right");
        assert_eq!(first.title, "Split pane right");
        assert_eq!(
            first.default_chords,
            vec!["Mod+Shift+ArrowRight", "Alt+Shift+Equal"]
        );
        let last = key_command("tab.select.last").unwrap();
        assert_eq!(last.title, "Go to tab 9");
        assert!(!last.palette);
        assert_eq!(
            key_command("app.shortcuts").unwrap().category.label(),
            "Application"
        );
    }
}
