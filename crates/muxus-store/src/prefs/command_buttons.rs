//! Saved command buttons and their groups (port of `client/src/command-buttons.ts`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use muxus_core::js;

/// The built-in group. It cannot be deleted and holds every unfiled button.
pub const DEFAULT_COMMAND_GROUP_ID: &str = "default";
pub const DEFAULT_COMMAND_GROUP_NAME: &str = "Default";
pub const MAX_COMMAND_BUTTONS: usize = 500;
pub const MAX_COMMAND_BUTTON_GROUPS: usize = 50;
pub const MAX_COMMAND_BUTTON_GROUP_NAME: usize = 80;

/// The default group, `{ id: 'default', name: 'Default' }`.
pub fn default_command_group() -> CommandButtonGroup {
    CommandButtonGroup {
        id: DEFAULT_COMMAND_GROUP_ID.to_owned(),
        name: DEFAULT_COMMAND_GROUP_NAME.to_owned(),
    }
}

/// Accents a button can wear. Each has a darker ink for light mode and a
/// lighter one for dark mode, so labels stay readable on either bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandButtonColor {
    Red,
    Orange,
    Amber,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
    Gray,
}

/// `COMMAND_BUTTON_COLORS`: id, name, light ink, dark ink.
pub const COMMAND_BUTTON_COLORS: [(CommandButtonColor, &str, &str, &str); 9] = [
    (CommandButtonColor::Red, "Red", "#b91c1c", "#f87171"),
    (CommandButtonColor::Orange, "Orange", "#c2410c", "#fb923c"),
    (CommandButtonColor::Amber, "Amber", "#a16207", "#fbbf24"),
    (CommandButtonColor::Green, "Green", "#15803d", "#4ade80"),
    (CommandButtonColor::Teal, "Teal", "#0f766e", "#2dd4bf"),
    (CommandButtonColor::Blue, "Blue", "#1d4ed8", "#60a5fa"),
    (CommandButtonColor::Purple, "Purple", "#7e22ce", "#c084fc"),
    (CommandButtonColor::Pink, "Pink", "#be185d", "#f472b6"),
    (CommandButtonColor::Gray, "Gray", "#52525b", "#a1a1aa"),
];

impl CommandButtonColor {
    pub fn id(self) -> &'static str {
        match self {
            CommandButtonColor::Red => "red",
            CommandButtonColor::Orange => "orange",
            CommandButtonColor::Amber => "amber",
            CommandButtonColor::Green => "green",
            CommandButtonColor::Teal => "teal",
            CommandButtonColor::Blue => "blue",
            CommandButtonColor::Purple => "purple",
            CommandButtonColor::Pink => "pink",
            CommandButtonColor::Gray => "gray",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        COMMAND_BUTTON_COLORS
            .iter()
            .map(|entry| entry.0)
            .find(|color| color.id() == value)
    }

    pub fn name(self) -> &'static str {
        COMMAND_BUTTON_COLORS
            .iter()
            .find(|entry| entry.0 == self)
            .map(|entry| entry.1)
            .unwrap_or_default()
    }

    /// The text color of an accented button in the given theme.
    pub fn ink(self, dark: bool) -> &'static str {
        let entry = COMMAND_BUTTON_COLORS
            .iter()
            .find(|entry| entry.0 == self)
            .expect("every color is listed");
        if dark { entry.3 } else { entry.2 }
    }
}

/// Whether a stored value names one of the accents.
pub fn is_command_button_color(value: &Value) -> bool {
    value.as_str().and_then(CommandButtonColor::parse).is_some()
}

/// One saved command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandButton {
    pub id: String,
    pub label: String,
    pub command: String,
    /// Append an Enter keystroke after sending the saved command.
    pub send_enter: bool,
    /// Group the button is filed under; absent means the default group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// Accent from the command button palette; absent keeps the plain look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<CommandButtonColor>,
    /// Named vault secret the button types instead of `command`. Only the id
    /// is stored; the backend types the value, and `send_enter` adds Enter after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_id: Option<String>,
}

/// A named set of command buttons the bar shows one at a time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommandButtonGroup {
    pub id: String,
    pub name: String,
}

/// A button that types a vault secret rather than its command text.
pub fn is_secret_command_button(button: &CommandButton) -> bool {
    button.secret_id.as_deref().is_some_and(|id| !id.is_empty())
}

/// Match every search word against a saved command's label or command text,
/// or for a secret button, the secret's name.
pub fn filter_command_buttons<'a>(
    buttons: &'a [CommandButton],
    query: &str,
    secret_name: impl Fn(&str) -> Option<String>,
) -> Vec<&'a CommandButton> {
    let lowered = js::js_trim(query).to_lowercase();
    let words: Vec<&str> = lowered
        .split(js::is_js_whitespace)
        .filter(|word| !word.is_empty())
        .collect();
    if words.is_empty() {
        return buttons.iter().collect();
    }
    buttons
        .iter()
        .filter(|button| {
            let text = if is_secret_command_button(button) {
                secret_name(button.secret_id.as_deref().unwrap_or_default()).unwrap_or_default()
            } else {
                button.command.clone()
            };
            let searchable = format!("{} {}", button.label, text).to_lowercase();
            words.iter().all(|word| searchable.contains(word))
        })
        .collect()
}

/// Convert saved, possibly multiline text to terminal Enter characters.
pub fn command_button_input(button: &CommandButton) -> String {
    let command = button.command.replace("\r\n", "\r").replace('\n', "\r");
    if !button.send_enter || command.ends_with('\r') {
        return command;
    }
    format!("{command}\r")
}

/// Send a saved command and return keyboard focus to its terminal.
pub fn activate_command_button(
    send_input: impl FnOnce(&str) -> bool,
    focus: impl FnOnce(),
    button: &CommandButton,
) -> bool {
    let sent = send_input(&command_button_input(button));
    focus();
    sent
}

/// What a button shows: its label, else its command, never nothing.
pub fn command_button_label(button: &CommandButton) -> String {
    let label = js::js_trim(&button.label);
    if is_secret_command_button(button) {
        return if label.is_empty() {
            "Secret".to_owned()
        } else {
            label.to_owned()
        };
    }
    let command = js::js_trim(&button.command);
    if !label.is_empty() {
        label.to_owned()
    } else if !command.is_empty() {
        command.to_owned()
    } else {
        "Command".to_owned()
    }
}

/// `${prefix}-${crypto.randomUUID()}`.
pub fn new_preference_id(prefix: &str) -> String {
    let mut bytes = [0u8; 16];
    crate::ids::random_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{prefix}-{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// `typeof value === 'string' && value.length > 0 && value.length <= max`.
pub(crate) fn bounded_string(value: &Value, max: usize) -> Option<&str> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && js::utf16_len(text) <= max)
}

/// Stored groups, repaired: malformed entries dropped, ids unique, and the
/// default group first and exactly once. Anything but an array is `None`.
pub fn normalize_command_button_groups(value: &Value) -> Option<Vec<CommandButtonGroup>> {
    let entries = value.as_array()?;
    let mut ids = std::collections::HashSet::new();
    let mut groups: Vec<CommandButtonGroup> = Vec::new();
    for entry in entries {
        let Some(record) = entry.as_object() else {
            continue;
        };
        let Some(id) = bounded_string(record.get("id").unwrap_or(&Value::Null), 200) else {
            continue;
        };
        if ids.contains(id) {
            continue;
        }
        let Some(name) = record.get("name").and_then(Value::as_str) else {
            continue;
        };
        ids.insert(id.to_owned());
        let name = js::utf16_prefix(js::js_trim(name), MAX_COMMAND_BUTTON_GROUP_NAME);
        let name = if !name.is_empty() {
            name.to_owned()
        } else if id == DEFAULT_COMMAND_GROUP_ID {
            DEFAULT_COMMAND_GROUP_NAME.to_owned()
        } else {
            "Untitled group".to_owned()
        };
        groups.push(CommandButtonGroup {
            id: id.to_owned(),
            name,
        });
    }
    let default_group = groups
        .iter()
        .find(|group| group.id == DEFAULT_COMMAND_GROUP_ID)
        .cloned()
        .unwrap_or_else(default_command_group);
    let mut normalized = vec![default_group];
    normalized.extend(
        groups
            .into_iter()
            .filter(|group| group.id != DEFAULT_COMMAND_GROUP_ID)
            .take(MAX_COMMAND_BUTTON_GROUPS - 1),
    );
    Some(normalized)
}

/// Stored buttons, repaired against their groups: malformed entries dropped,
/// unknown colors cleared, and a button whose group is gone (or is the
/// default one) left unfiled. Anything but an array is `None`.
pub fn normalize_command_buttons(
    value: &Value,
    groups: &[CommandButtonGroup],
) -> Option<Vec<CommandButton>> {
    let entries = value.as_array()?;
    let mut ids = std::collections::HashSet::new();
    let mut buttons = Vec::new();
    for entry in entries {
        if buttons.len() >= MAX_COMMAND_BUTTONS {
            break;
        }
        let Some(record) = entry.as_object() else {
            continue;
        };
        let Some(id) = bounded_string(record.get("id").unwrap_or(&Value::Null), 200) else {
            continue;
        };
        if ids.contains(id) {
            continue;
        }
        let Some(label) = record
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| js::utf16_len(label) <= 200)
        else {
            continue;
        };
        let Some(command) = record
            .get("command")
            .and_then(Value::as_str)
            .filter(|command| js::utf16_len(command) <= 100_000)
        else {
            continue;
        };
        ids.insert(id.to_owned());
        let group_id = record
            .get("groupId")
            .and_then(Value::as_str)
            .filter(|group| {
                *group != DEFAULT_COMMAND_GROUP_ID && groups.iter().any(|known| known.id == *group)
            })
            .filter(|group| !group.is_empty());
        buttons.push(CommandButton {
            id: id.to_owned(),
            label: label.to_owned(),
            command: command.to_owned(),
            send_enter: record.get("sendEnter") != Some(&Value::Bool(false)),
            group_id: group_id.map(str::to_owned),
            color: record
                .get("color")
                .and_then(Value::as_str)
                .and_then(CommandButtonColor::parse),
            secret_id: bounded_string(record.get("secretId").unwrap_or(&Value::Null), 200)
                .map(str::to_owned),
        });
    }
    Some(buttons)
}

/// The group a button is filed under; unfiled buttons are in the default group.
pub fn command_button_group_of<'a>(
    button: &'a CommandButton,
    groups: &[CommandButtonGroup],
) -> &'a str {
    match button.group_id.as_deref() {
        Some(id) if !id.is_empty() && groups.iter().any(|group| group.id == id) => id,
        _ => DEFAULT_COMMAND_GROUP_ID,
    }
}

pub fn command_buttons_in_group(
    buttons: &[CommandButton],
    groups: &[CommandButtonGroup],
    group_id: &str,
) -> Vec<CommandButton> {
    buttons
        .iter()
        .filter(|button| command_button_group_of(button, groups) == group_id)
        .cloned()
        .collect()
}

/// Swap a button with its neighbor in its own group; other groups keep their places.
pub fn move_command_button(
    buttons: &[CommandButton],
    groups: &[CommandButtonGroup],
    id: &str,
    offset: isize,
) -> Vec<CommandButton> {
    let Some(index) = buttons.iter().position(|button| button.id == id) else {
        return buttons.to_vec();
    };
    let group_id = command_button_group_of(&buttons[index], groups);
    let mut target = index as isize + offset;
    while target >= 0
        && (target as usize) < buttons.len()
        && command_button_group_of(&buttons[target as usize], groups) != group_id
    {
        target += offset;
    }
    if target < 0 || target as usize >= buttons.len() {
        return buttons.to_vec();
    }
    let mut next = buttons.to_vec();
    next.swap(index, target as usize);
    next
}

/// File a button under another group, after that group's last button.
pub fn move_command_button_to_group(
    buttons: &[CommandButton],
    groups: &[CommandButtonGroup],
    id: &str,
    group_id: &str,
) -> Vec<CommandButton> {
    let Some(button) = buttons.iter().find(|candidate| candidate.id == id) else {
        return buttons.to_vec();
    };
    if command_button_group_of(button, groups) == group_id {
        return buttons.to_vec();
    }
    let mut moved = button.clone();
    moved.group_id = (group_id != DEFAULT_COMMAND_GROUP_ID).then(|| group_id.to_owned());
    let mut remaining: Vec<CommandButton> = buttons
        .iter()
        .filter(|candidate| candidate.id != id)
        .cloned()
        .collect();
    let at = remaining
        .iter()
        .rposition(|candidate| command_button_group_of(candidate, groups) == group_id)
        .map_or(remaining.len(), |last| last + 1);
    remaining.insert(at, moved);
    remaining
}

/// A copy of a button right below the original, as a start for a similar command.
pub fn duplicate_command_button(
    buttons: &[CommandButton],
    id: &str,
    new_id: &str,
) -> Vec<CommandButton> {
    let Some(index) = buttons.iter().position(|button| button.id == id) else {
        return buttons.to_vec();
    };
    let original = &buttons[index];
    let label = js::js_trim(&original.label);
    let mut copy = original.clone();
    copy.id = new_id.to_owned();
    copy.label = if label.is_empty() {
        String::new()
    } else {
        js::utf16_prefix(&format!("{label} copy"), 80).to_owned()
    };
    let mut next = buttons.to_vec();
    next.insert(index + 1, copy);
    next
}

/// Move a group one step; the default group always stays first.
pub fn move_command_button_group(
    groups: &[CommandButtonGroup],
    id: &str,
    offset: isize,
) -> Vec<CommandButtonGroup> {
    let index = groups
        .iter()
        .position(|group| group.id == id)
        .map_or(-1, |index| index as isize);
    let target = index + offset;
    if index < 1 || target < 1 || target as usize >= groups.len() {
        return groups.to_vec();
    }
    let mut next = groups.to_vec();
    next.swap(index as usize, target as usize);
    next
}

/// Remove a group and every button filed under it. The default group stays.
pub fn without_command_button_group(
    buttons: &[CommandButton],
    groups: &[CommandButtonGroup],
    id: &str,
) -> (Vec<CommandButton>, Vec<CommandButtonGroup>) {
    if id == DEFAULT_COMMAND_GROUP_ID {
        return (buttons.to_vec(), groups.to_vec());
    }
    (
        buttons
            .iter()
            .filter(|button| command_button_group_of(button, groups) != id)
            .cloned()
            .collect(),
        groups
            .iter()
            .filter(|group| group.id != id)
            .cloned()
            .collect(),
    )
}

/// "New group", or the first numbered variant no group is called yet.
pub fn unused_command_button_group_name(groups: &[CommandButtonGroup]) -> String {
    let names: std::collections::HashSet<String> = groups
        .iter()
        .map(|group| js::js_trim(&group.name).to_lowercase())
        .collect();
    if !names.contains("new group") {
        return "New group".to_owned();
    }
    let mut suffix = 2;
    while names.contains(&format!("new group {suffix}")) {
        suffix += 1;
    }
    format!("New group {suffix}")
}

/// The group the command bar and the keyboard menu show. A group picked
/// while a session was active wins for that session, then the group its host
/// opens, then the group last picked for sessions without one. Groups that no
/// longer exist are skipped.
pub fn shown_command_button_group(
    groups: &[CommandButtonGroup],
    session: Option<&str>,
    host: Option<&str>,
    selected: Option<&str>,
) -> String {
    [session, host, selected]
        .into_iter()
        .flatten()
        .find(|id| !id.is_empty() && groups.iter().any(|group| group.id == *id))
        .unwrap_or(DEFAULT_COMMAND_GROUP_ID)
        .to_owned()
}
