//! Settings search (`client/src/settings-search.ts`): an index of every
//! setting row and every group that holds a list or a form, with extra words
//! to find them by, and the ranking that searches it.
//!
//! The labels match what the sections show, so choosing a result can scroll
//! to it and outline it; a test reads the settings views and fails when a row
//! or group is missing from the index.

use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use super::section::SettingsSection;

/// Whether a result is a row or the group itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EntryKind {
    #[default]
    Row,
    Group,
}

/// Settings that only exist in the desktop app or on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Only {
    Desktop,
    Windows,
}

/// One searchable setting: a row in a Settings section, or a group whose
/// content is a list or a form rather than rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsSearchEntry {
    pub section: SettingsSection,
    /// The row's label, or the group's title (without its "· count" suffix).
    pub label: &'static str,
    /// The group the row sits in, shown beside the result.
    pub group: Option<&'static str>,
    pub kind: EntryKind,
    /// The row's own label where the result needs more context than it gives, e.g. "Items".
    pub row: Option<&'static str>,
    /// Other words people look for it by.
    pub keywords: &'static [&'static str],
    pub only: Option<Only>,
}

impl SettingsSearchEntry {
    /// The label the section shows, which a pick outlines (`entry.row ?? entry.label`).
    pub fn focus_label(&self) -> &'static str {
        self.row.unwrap_or(self.label)
    }

    /// `entryKey`: unique within the index.
    pub fn key(&self) -> String {
        let kind = match self.kind {
            EntryKind::Row => "row",
            EntryKind::Group => "group",
        };
        format!("{}:{kind}:{}", self.section.id(), self.label)
    }
}

const fn row(
    section: SettingsSection,
    group: &'static str,
    label: &'static str,
    keywords: &'static [&'static str],
) -> SettingsSearchEntry {
    SettingsSearchEntry {
        section,
        label,
        group: Some(group),
        kind: EntryKind::Row,
        row: None,
        keywords,
        only: None,
    }
}

/// A row without a group heading above it.
const fn bare_row(
    section: SettingsSection,
    label: &'static str,
    keywords: &'static [&'static str],
) -> SettingsSearchEntry {
    SettingsSearchEntry {
        section,
        label,
        group: None,
        kind: EntryKind::Row,
        row: None,
        keywords,
        only: None,
    }
}

const fn group(
    section: SettingsSection,
    label: &'static str,
    keywords: &'static [&'static str],
) -> SettingsSearchEntry {
    SettingsSearchEntry {
        section,
        label,
        group: None,
        kind: EntryKind::Group,
        row: None,
        keywords,
        only: None,
    }
}

const fn only(entry: SettingsSearchEntry, only: Only) -> SettingsSearchEntry {
    SettingsSearchEntry {
        only: Some(only),
        ..entry
    }
}

use SettingsSection::{
    About, Appearance, Behavior, Data, Debug, Highlighting, Keyboard, LocalShells, Logging,
    Passwords, Terminal, X11,
};

/// `SETTINGS_SEARCH_INDEX`, in Settings order.
pub static SETTINGS_SEARCH_INDEX: &[SettingsSearchEntry] = &[
    // Appearance
    row(
        Appearance,
        "Window",
        "Theme",
        &["dark mode", "light mode", "system", "appearance"],
    ),
    row(
        Appearance,
        "Window",
        "Interface scale",
        &["zoom", "ui size", "scaling", "bigger", "smaller"],
    ),
    row(
        Appearance,
        "Layout",
        "Hosts sidebar",
        &["left", "right", "position", "dock"],
    ),
    row(
        Appearance,
        "Layout",
        "File browser",
        &["sftp", "position", "dock", "sidebar"],
    ),
    row(
        Appearance,
        "Layout",
        "Command bar",
        &["command buttons", "top", "bottom", "position"],
    ),
    row(
        Appearance,
        "Status bar",
        "Show status bar",
        &["cpu", "memory", "disk", "uptime", "statistics"],
    ),
    SettingsSearchEntry {
        row: Some("Items"),
        ..row(
            Appearance,
            "Status bar",
            "Status bar items",
            &[
                "cpu", "memory", "disk", "load", "network", "uptime", "users",
            ],
        )
    },
    row(
        Appearance,
        "Terminal colors",
        "Light terminal theme",
        &["color scheme", "palette", "colours"],
    ),
    row(
        Appearance,
        "Terminal colors",
        "Dark terminal theme",
        &["color scheme", "palette", "colours"],
    ),
    row(
        Appearance,
        "Terminal colors",
        "Text color",
        &["foreground", "font color", "colour"],
    ),
    row(
        Appearance,
        "Terminal colors",
        "Background color",
        &["colour"],
    ),
    group(
        Appearance,
        "Custom color schemes",
        &[
            "import",
            "iterm2",
            "itermcolors",
            "windows terminal",
            "palette",
            "colours",
        ],
    ),
    row(
        Appearance,
        "Terminal font",
        "Font family",
        &["typeface", "monospace", "jetbrains mono"],
    ),
    row(
        Appearance,
        "Terminal font",
        "Font size",
        &["text size", "zoom", "bigger", "smaller"],
    ),
    row(
        Appearance,
        "Terminal font",
        "Line height",
        &["spacing", "line spacing"],
    ),
    row(
        Appearance,
        "Split panes",
        "Dim inactive panes",
        &["fade", "focus", "unfocused"],
    ),
    row(
        Appearance,
        "Split panes",
        "Dimming strength",
        &["fade", "opacity"],
    ),
    row(
        Appearance,
        "Split panes",
        "Outline the focused pane",
        &["border", "highlight", "active pane"],
    ),
    // Terminal
    row(
        Terminal,
        "Cursor",
        "Cursor style",
        &["block", "underline", "bar", "caret"],
    ),
    row(Terminal, "Cursor", "Blinking cursor", &["blink", "caret"]),
    row(
        Terminal,
        "Clipboard & mouse",
        "Right-click",
        &["paste", "context menu", "mouse"],
    ),
    row(
        Terminal,
        "Clipboard & mouse",
        "Open terminal links",
        &["url", "hyperlink", "click", "file paths"],
    ),
    row(
        Terminal,
        "Clipboard & mouse",
        "Copy on select",
        &["selection", "clipboard", "putty"],
    ),
    row(
        Terminal,
        "Clipboard & mouse",
        "Allow terminal clipboard writes (OSC 52)",
        &["tmux", "zellij", "clipboard"],
    ),
    row(
        Terminal,
        "Clipboard & mouse",
        "Confirm multiline pastes",
        &["paste warning", "paste preview"],
    ),
    row(
        Terminal,
        "Clipboard & mouse",
        "Confirm pastes with look-alike characters",
        &["smart quotes", "no-break space", "unicode", "paste warning"],
    ),
    row(
        Terminal,
        "Paste pacing",
        "Delay after each line",
        &["paste delay", "slow paste", "console", "network device"],
    ),
    row(
        Terminal,
        "Paste pacing",
        "Delay after each character",
        &["paste delay", "slow paste", "typing speed"],
    ),
    row(
        Terminal,
        "Buffer",
        "Scrollback lines",
        &["history", "buffer", "lines kept"],
    ),
    // Local shells
    row(
        LocalShells,
        "Default",
        "Default profile",
        &["local terminal", "shell"],
    ),
    row(
        LocalShells,
        "Default",
        "Automatic shell",
        &["bash", "zsh", "powershell", "cmd", "shell"],
    ),
    group(
        LocalShells,
        "Saved profiles",
        &[
            "shell profile",
            "powershell",
            "command prompt",
            "wsl",
            "startup commands",
            "arguments",
        ],
    ),
    only(
        group(
            LocalShells,
            "WSL distributions",
            &["ubuntu", "debian", "linux"],
        ),
        Only::Windows,
    ),
    only(
        row(
            LocalShells,
            "WSL distributions",
            "List installed distributions",
            &["wsl", "ubuntu"],
        ),
        Only::Windows,
    ),
    // Session logging
    group(
        Logging,
        "Default policy",
        &[
            "record",
            "log sessions",
            "session history",
            "input",
            "rotate",
            "retain",
        ],
    ),
    group(Logging, "Local terminals", &["local logging", "record"]),
    group(
        Logging,
        "Log files",
        &["log folder", "file name", "text log", "directory"],
    ),
    row(
        Logging,
        "Log files",
        "Show timestamps (UTC)",
        &["time", "log file"],
    ),
    group(
        Logging,
        "Storage & retention",
        &[
            "history location",
            "quota",
            "disk space",
            "maximum age",
            "cleanup",
        ],
    ),
    row(
        Logging,
        "Storage & retention",
        "Current usage",
        &["disk space", "history size"],
    ),
    // Highlighting
    group(
        Highlighting,
        "Global rules",
        &["keyword", "regex", "color", "highlight"],
    ),
    group(
        Highlighting,
        "Reusable profiles",
        &["nokia", "sr os", "sr linux", "keyword", "highlight"],
    ),
    // Behavior
    row(
        Behavior,
        "Tabs",
        "Confirm before closing a live session",
        &["close tab", "warning", "prompt"],
    ),
    row(
        Behavior,
        "Tabs",
        "Close the tab when its shell exits",
        &["exit", "logout", "ctrl+d", "close tab", "session ended"],
    ),
    row(
        Behavior,
        "Tabs",
        "Show program status on tabs",
        &["osc 7501", "agent", "working", "waiting", "done", "busy"],
    ),
    row(
        Behavior,
        "Hosts sidebar",
        "Open hosts with",
        &["single click", "double-click", "sidebar"],
    ),
    row(
        Behavior,
        "Hosts sidebar",
        "Find containers, clusters and VMs",
        &[
            "docker",
            "podman",
            "kubernetes",
            "containerlab",
            "proxmox",
            "libvirt",
            "virsh",
            "discover",
            "badges",
        ],
    ),
    row(
        Behavior,
        "Hosts sidebar",
        "Refresh containers, clusters and VMs",
        &[
            "docker",
            "kubernetes",
            "containerlab",
            "interval",
            "poll",
            "update",
        ],
    ),
    row(
        Behavior,
        "New hosts",
        "Save new SSH hosts in",
        &["openssh", "ssh config", "storage"],
    ),
    row(
        Behavior,
        "Connecting",
        "Show a summary when an SSH session connects",
        &["cipher", "encryption", "connection info"],
    ),
    row(
        Behavior,
        "Restore & reconnect",
        "SSH keepalive interval",
        &["serveraliveinterval", "timeout", "idle", "firewall", "nat"],
    ),
    row(
        Behavior,
        "Restore & reconnect",
        "Automatically reconnect remote sessions",
        &["auto reconnect", "redial", "workspace restore"],
    ),
    row(
        Behavior,
        "Restore & reconnect",
        "Restore terminal history",
        &["scrollback", "restore", "output"],
    ),
    only(
        row(
            Behavior,
            "Links",
            "SSH links",
            &["ssh://", "url handler", "default app", "protocol"],
        ),
        Only::Desktop,
    ),
    only(
        row(
            Behavior,
            "Links",
            "Telnet links",
            &["telnet://", "url handler", "default app", "protocol"],
        ),
        Only::Desktop,
    ),
    // X11 forwarding
    bare_row(
        X11,
        "Enable X11 forwarding",
        &["x server", "xquartz", "vcxsrv", "display"],
    ),
    row(
        X11,
        "Defaults",
        "Forward X11 by default",
        &["forwardx11", "display"],
    ),
    // Only the X server bundled with the Windows app shares the clipboard.
    only(
        row(
            X11,
            "Defaults",
            "Share the clipboard with X11 apps",
            &["clipboard", "copy", "paste"],
        ),
        Only::Windows,
    ),
    // Keyboard
    row(
        Keyboard,
        "Panes & tabs",
        "New splits continue the current session",
        &["split", "pane", "same host"],
    ),
    row(
        Keyboard,
        "Panes & tabs",
        "Show tab numbers",
        &["numbered tabs", "alt"],
    ),
    group(
        Keyboard,
        "Layout keys",
        &[
            "keyboard shortcuts",
            "keybindings",
            "hotkeys",
            "rebind",
            "chords",
        ],
    ),
    // Passwords
    row(
        Passwords,
        "Vault",
        "Password vault",
        &["master password", "encryption", "credentials", "keychain"],
    ),
    row(
        Passwords,
        "Vault",
        "Remember passwords by default",
        &["save password"],
    ),
    row(
        Passwords,
        "Vault",
        "Ask for the master password",
        &["unlock", "startup"],
    ),
    row(Passwords, "Vault", "Master password", &["change password"]),
    row(
        Passwords,
        "Vault",
        "Delete the vault",
        &["reset", "forgot password", "remove"],
    ),
    group(
        Passwords,
        "Secrets",
        &["enable password", "sudo", "pin", "send secret"],
    ),
    group(
        Passwords,
        "Saved SSH passwords",
        &["credentials", "remembered passwords"],
    ),
    // Backup & data
    row(
        Data,
        "Backup & restore",
        "Back up your setup",
        &["export", "backup", "settings file"],
    ),
    row(
        Data,
        "Backup & restore",
        "Restore a backup",
        &["import", "backup"],
    ),
    row(
        Data,
        "Export",
        "OpenSSH config",
        &["ssh_config", "export hosts"],
    ),
    row(
        Data,
        "Import from other clients",
        "MobaXterm",
        &["import sessions", "migrate"],
    ),
    row(
        Data,
        "Import from other clients",
        "SecureCRT",
        &["import sessions", "migrate", "button bar"],
    ),
    // Debug
    bare_row(
        Debug,
        "Capture verbose diagnostic logs",
        &["debug mode", "logging", "troubleshoot"],
    ),
    bare_row(
        Debug,
        "Application log",
        &["view logs", "export logs", "main.log", "bug report"],
    ),
    // About
    row(
        About,
        "Updates",
        "Notify me when a new version is available",
        &["update", "upgrade", "release"],
    ),
    row(
        About,
        "Updates",
        "Update check",
        &["update", "upgrade", "version"],
    ),
    group(
        About,
        "This installation",
        &["version", "diagnostics", "platform"],
    ),
    row(
        About,
        "Made by",
        "FloSch",
        &["author", "github", "source code"],
    ),
    row(
        About,
        "Made by",
        "Support Muxus",
        &["donate", "sponsor", "coffee"],
    ),
];

/// Where search runs: settings that do not exist there are left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsSearchContext {
    pub desktop: bool,
    /// The backend's platform, e.g. "win32"; unknown until the app info arrives.
    pub platform: Option<String>,
}

static DIACRITIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Diacritic}").unwrap());
static NOT_LETTER_OR_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]+").unwrap());

fn normalize(text: &str) -> String {
    let decomposed: String = text.nfd().collect();
    DIACRITIC.replace_all(&decomposed, "").to_lowercase()
}

/// "OSC 52" and "osc52", "X11" and "x 11" find the same setting.
fn squash(text: &str) -> String {
    NOT_LETTER_OR_NUMBER.replace_all(text, "").into_owned()
}

fn contains(field: &str, word: &str) -> bool {
    if field.contains(word) {
        return true;
    }
    let squashed = squash(word);
    !squashed.is_empty() && squash(field).contains(&squashed)
}

fn starts_word(field: &str, word: &str) -> bool {
    field.starts_with(word)
        || field.contains(&format!(" {word}"))
        || field.contains(&format!("({word}"))
}

/// Lower is better; `None` when some word of the query matches nowhere.
fn rank(entry: &SettingsSearchEntry, query: &str, words: &[&str]) -> Option<u8> {
    let label = normalize(entry.label);
    let mut fields = vec![
        label.clone(),
        normalize(entry.group.unwrap_or_default()),
        normalize(entry.row.unwrap_or_default()),
        normalize(entry.section.label()),
    ];
    fields.extend(entry.keywords.iter().map(|keyword| normalize(keyword)));
    if !words
        .iter()
        .all(|word| fields.iter().any(|field| contains(field, word)))
    {
        return None;
    }
    if label.starts_with(query) {
        return Some(0);
    }
    if words.iter().all(|word| starts_word(&label, word)) {
        return Some(1);
    }
    if words.iter().all(|word| contains(&label, word)) {
        return Some(2);
    }
    Some(3)
}

/// Settings matching every word of `query`, best match first, then in Settings order.
pub fn search_settings(
    query: &str,
    context: &SettingsSearchContext,
) -> Vec<&'static SettingsSearchEntry> {
    search_entries(query, context, SETTINGS_SEARCH_INDEX)
}

/// [`search_settings`] over any index.
pub fn search_entries<'a>(
    query: &str,
    context: &SettingsSearchContext,
    entries: &'a [SettingsSearchEntry],
) -> Vec<&'a SettingsSearchEntry> {
    let normalized = normalize(query)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let words: Vec<&str> = normalized.split(' ').filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut ranked: Vec<(u8, &SettingsSearchEntry)> = entries
        .iter()
        .filter(|entry| match entry.only {
            Some(Only::Desktop) => context.desktop,
            Some(Only::Windows) => context.platform.as_deref() == Some("win32"),
            None => true,
        })
        .filter_map(|entry| rank(entry, &normalized, &words).map(|rank| (rank, entry)))
        .collect();
    // Stable: equal ranks keep Settings order.
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, entry)| entry).collect()
}

/// The label a search result outlines: a group title drops its "· count" suffix.
pub fn settings_title_key(title: &str) -> &str {
    match title.find(" \u{b7} ") {
        Some(at) => &title[..at],
        None => title,
    }
}

/// The setting picked in search: the row label or group title to bring into
/// view (`SettingsFocusTarget`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsFocusTarget {
    pub label: &'static str,
    pub kind: EntryKind,
    /// Changes on every pick, so picking the same setting again outlines it again.
    pub nonce: u64,
}

#[cfg(test)]
mod tests;
