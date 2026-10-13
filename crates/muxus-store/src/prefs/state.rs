//! The preferences (`PrefsState` in `client/src/state/prefs.ts`): every field,
//! its default, and the value types they hold.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use muxus_core::api::{KeywordHighlightProfile, KeywordHighlightRule};
use muxus_core::js;
use muxus_core::protocol::DEFAULT_SSH_KEEPALIVE_INTERVAL_SECONDS;

use super::builtin_highlight_profiles::builtin_highlight_profiles;
use super::command_buttons::{
    CommandButton, CommandButtonGroup, DEFAULT_COMMAND_GROUP_ID, default_command_group,
};
use super::custom_schemes::CustomTerminalScheme;
use super::layout::{
    DEFAULT_INACTIVE_PANE_DIM_STRENGTH, DEFAULT_SFTP_PANEL_WIDTH, DEFAULT_SIDEBAR_WIDTH,
    STATUS_BAR_ITEMS, StatusBarItem,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    Dark,
    /// Follow the operating system.
    #[default]
    Os,
}

/// The appearance in effect once `os` is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectiveThemeMode {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    #[default]
    Block,
    Underline,
    Bar,
}

/// Right-click: copy selection / paste (terminal convention), always paste, or context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RightClickAction {
    #[default]
    CopyPaste,
    Paste,
    Menu,
}

/// Mouse gesture that opens a detected terminal file path or web URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalFileLinkActivation {
    Direct,
    #[default]
    Alt,
    Ctrl,
    Meta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabNumberVisibility {
    #[default]
    Shortcut,
    Always,
}

/// Window edge the hosts sidebar docks against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarPosition {
    #[default]
    Left,
    Right,
}

/// Where the saved-command bar sits relative to the pane canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandBarPosition {
    #[default]
    Top,
    Bottom,
}

/// Beside each SSH terminal, or behind a tab in the window sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileBrowserPosition {
    #[default]
    Pane,
    Sidebar,
}

/// Click that opens a sidebar entry; with double-click, a single click selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarOpenGesture {
    #[default]
    Click,
    DoubleClick,
}

/// Where the host editor saves a new SSH host unless told otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NewSshHostStorage {
    #[default]
    Openssh,
    Muxus,
}

/// Presentation of one sidebar folder. Folders are paths, not records, so
/// their looks cannot hang off a host and live here instead.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FolderStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// A reusable local terminal launch configuration. Arguments stay structured
/// so paths and values containing spaces are never reparsed as a command line.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalShellProfileConfig {
    pub id: String,
    pub name: String,
    pub shell: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub startup_command: String,
}

macro_rules! prefs_state {
    ($( $(#[$meta:meta])* $field:ident : $ty:ty = $key:literal => $default:expr; )*) => {
        /// Every preference, in the order the TypeScript store declares them
        /// (which is the order they are written in).
        #[derive(Debug, Clone, PartialEq, Serialize)]
        #[serde(rename_all = "camelCase")]
        pub struct PrefsState {
            $( $(#[$meta])* pub $field: $ty, )*
            /// Persisted keys this build does not model, kept in place so the
            /// Electron build never loses them.
            #[serde(flatten)]
            pub extra: Map<String, Value>,
        }

        impl Default for PrefsState {
            fn default() -> Self {
                Self { $( $field: $default, )* extra: Map::new() }
            }
        }

        impl PrefsState {
            /// The stored key of every field, in field order.
            pub const KEYS: &'static [&'static str] = &[$($key),*];

            /// Read a stored state object. A value of the wrong type keeps the
            /// field's default (the stored value itself stays in the document);
            /// unknown keys land in `extra`.
            pub fn from_json_map(map: &Map<String, Value>) -> Self {
                let mut state = Self::default();
                for (key, value) in map {
                    match key.as_str() {
                        $( $key => {
                            if let Ok(parsed) = serde_json::from_value::<$ty>(value.clone()) {
                                state.$field = parsed;
                            }
                        } )*
                        // The store's own setter; never a preference.
                        "set" => {}
                        _ => {
                            state.extra.insert(key.clone(), value.clone());
                        }
                    }
                }
                state
            }
        }
    };
}

prefs_state! {
    theme_mode: ThemeMode = "themeMode" => ThemeMode::Os;
    /// Base font size for the terminal.
    #[serde(with = "js::number")]
    mono_font_size: f64 = "monoFontSize" => 14.0;
    /// Terminal font family; bundled text and symbol fonts remain as fallbacks.
    font_family: String = "fontFamily" => "JetBrains Mono".to_owned();
    /// Terminal line height multiplier (1.0 = font metrics).
    #[serde(with = "js::number")]
    line_height: f64 = "lineHeight" => 1.0;
    /// Terminal color scheme id used while the effective application theme is light.
    light_terminal_scheme: String = "lightTerminalScheme" => "vscode-light".to_owned();
    /// Terminal color scheme id used while the effective application theme is dark.
    dark_terminal_scheme: String = "darkTerminalScheme" => "vscode-dark".to_owned();
    /// Color schemes the user created or imported, offered next to the built-in ones.
    custom_terminal_schemes: Vec<CustomTerminalScheme> = "customTerminalSchemes" => Vec::new();
    /// Terminal text color; empty follows the color scheme's foreground.
    font_color: String = "fontColor" => String::new();
    /// Terminal background color; empty follows the color scheme's background.
    background_color: String = "backgroundColor" => String::new();
    /// Scrollback lines kept per terminal.
    #[serde(with = "js::number")]
    scrollback: f64 = "scrollback" => 10_000.0;
    cursor_blink: bool = "cursorBlink" => true;
    cursor_style: CursorStyle = "cursorStyle" => CursorStyle::Block;
    /// Render terminals on the GPU; off keeps the simpler renderer.
    webgl_renderer: bool = "webglRenderer" => false;
    /// Local terminal shell; `auto` lets the backend pick the login shell.
    local_shell: String = "localShell" => "auto".to_owned();
    /// Named alternatives offered wherever a local terminal can be launched.
    local_shell_profiles: Vec<LocalShellProfileConfig> = "localShellProfiles" => Vec::new();
    /// Empty selects the legacy automatic/custom `local_shell` preference.
    default_local_shell_profile_id: String = "defaultLocalShellProfileId" => String::new();
    /// Offer installed WSL distributions next to the saved profiles (Windows).
    show_wsl_distributions: bool = "showWslDistributions" => true;
    /// Copy the selection to the clipboard as soon as it is made.
    copy_on_select: bool = "copyOnSelect" => false;
    /// Let terminal applications replace the system clipboard via OSC 52.
    allow_osc52_clipboard_write: bool = "allowOsc52ClipboardWrite" => true;
    right_click_action: RightClickAction = "rightClickAction" => RightClickAction::CopyPaste;
    terminal_file_link_activation: TerminalFileLinkActivation = "terminalFileLinkActivation" => TerminalFileLinkActivation::Alt;
    /// Preview multiline pastes before they can run several shell commands.
    paste_warn_multiline: bool = "pasteWarnMultiline" => true;
    /// Wait after each pasted line, in milliseconds; hosts may set their own.
    #[serde(with = "js::number")]
    paste_line_delay_ms: f64 = "pasteLineDelayMs" => 0.0;
    /// Wait after each pasted character, in milliseconds; hosts may set their own.
    #[serde(with = "js::number")]
    paste_char_delay_ms: f64 = "pasteCharDelayMs" => 0.0;
    /// Ask before closing a tab with a live session.
    confirm_close_connected: bool = "confirmCloseConnected" => true;
    /// Dial remote sessions on workspace restore and retry dropped connections.
    auto_reconnect_remote: bool = "autoReconnectRemote" => true;
    /// SSH keepalive fallback in seconds; zero relies entirely on ssh_config.
    #[serde(with = "js::number")]
    ssh_keepalive_interval_seconds: f64 = "sshKeepaliveIntervalSeconds" => f64::from(DEFAULT_SSH_KEEPALIVE_INTERVAL_SECONDS);
    /// Print the route, encryption and active features when an SSH session connects.
    ssh_session_summary: bool = "sshSessionSummary" => false;
    /// Pre-select "Remember this password" on password prompts.
    remember_passwords_by_default: bool = "rememberPasswordsByDefault" => false;
    /// Storage the host editor starts with for a new SSH host.
    new_ssh_host_storage: NewSshHostStorage = "newSshHostStorage" => NewSshHostStorage::Openssh;
    /// X11 forwarding master switch; `None` follows the platform default (off on macOS).
    x11_enabled: Option<bool> = "x11Enabled" => None;
    /// Forward X11 for hosts without ForwardX11; `None` follows the platform default.
    x11_forward_by_default: Option<bool> = "x11ForwardByDefault" => None;
    /// Bridge the bundled Windows X server to the system clipboard (read and write).
    x11_clipboard_sharing: bool = "x11ClipboardSharing" => false;
    /// Show a notification at startup when a newer release is available.
    notify_on_new_version: bool = "notifyOnNewVersion" => true;
    /// Persist recent terminal output and replay it on restore and reconnect.
    restore_scrollback: bool = "restoreScrollback" => true;
    /// Scale of the whole interface, 1 = 100%.
    #[serde(with = "js::number")]
    interface_zoom: f64 = "interfaceZoom" => 1.0;
    /// Splitting a pane opens a second session on the same host.
    split_inherits_session: bool = "splitInheritsSession" => true;
    /// When window-wide shortcut numbers are shown on terminal tabs.
    tab_number_visibility: TabNumberVisibility = "tabNumberVisibility" => TabNumberVisibility::Shortcut;
    /// Draw a theme-accented border around focused and multi-exec panes in a split layout.
    active_pane_border: bool = "activePaneBorder" => false;
    /// Reduce the presentation opacity of panes that do not receive keyboard input.
    dim_inactive_panes: bool = "dimInactivePanes" => false;
    /// Fraction removed from inactive-pane opacity while dimming is enabled.
    #[serde(with = "js::number")]
    inactive_pane_dim_strength: f64 = "inactivePaneDimStrength" => DEFAULT_INACTIVE_PANE_DIM_STRENGTH;
    /// Chords per command id, replacing that command's defaults. An empty
    /// list unbinds the command; commands absent from the map keep their defaults.
    keybindings: IndexMap<String, Vec<String>> = "keybindings" => IndexMap::new();
    /// One-click commands shown in the action bar, in order within each group.
    command_buttons: Vec<CommandButton> = "commandButtons" => Vec::new();
    /// Command button groups in menu order; the default group is always first.
    command_button_groups: Vec<CommandButtonGroup> = "commandButtonGroups" => vec![default_command_group()];
    /// Group the bar shows for sessions whose host does not choose one.
    selected_command_button_group: String = "selectedCommandButtonGroup" => DEFAULT_COMMAND_GROUP_ID.to_owned();
    /// Show saved commands as buttons beside the terminals in addition to the keyboard menu.
    show_command_bar: bool = "showCommandBar" => true;
    /// Dock the command bar above or below the pane canvas.
    command_bar_position: CommandBarPosition = "commandBarPosition" => CommandBarPosition::Top;
    /// Show the active session's host statistics along the bottom of the window.
    show_status_bar: bool = "showStatusBar" => true;
    /// What the status bar shows, in bar order.
    status_bar_items: Vec<StatusBarItem> = "statusBarItems" => STATUS_BAR_ITEMS.to_vec();
    /// Rules applied to every terminal; hosts may add to or replace these.
    keyword_highlights: Vec<KeywordHighlightRule> = "keywordHighlights" => Vec::new();
    /// Named rule sets referenced by saved-host highlighting metadata.
    keyword_highlight_profiles: Vec<KeywordHighlightProfile> = "keywordHighlightProfiles" => builtin_highlight_profiles().to_vec();
    /// Whether the whole hosts sidebar is hidden — not to be confused with
    /// `sidebar_collapsed_folders`, which collapses individual folders inside it.
    sidebar_collapsed: bool = "sidebarCollapsed" => false;
    /// Which side of the window the hosts sidebar docks on.
    sidebar_position: SidebarPosition = "sidebarPosition" => SidebarPosition::Left;
    /// Where the remote file browser opens: beside its terminal, or in the sidebar.
    file_browser_position: FileBrowserPosition = "fileBrowserPosition" => FileBrowserPosition::Pane;
    /// Whether a sidebar entry opens on a single click or on a double-click.
    sidebar_open_gesture: SidebarOpenGesture = "sidebarOpenGesture" => SidebarOpenGesture::Click;
    /// Width of the sessions and hosts sidebar.
    #[serde(with = "js::number")]
    sidebar_width: f64 = "sidebarWidth" => DEFAULT_SIDEBAR_WIDTH;
    /// Folder keys the user collapsed. Absent means expanded, so a new folder
    /// shows its contents the first time it appears.
    sidebar_collapsed_folders: Vec<String> = "sidebarCollapsedFolders" => Vec::new();
    /// Colour and icon per folder key.
    sidebar_folder_styles: IndexMap<String, FolderStyle> = "sidebarFolderStyles" => IndexMap::new();
    /// Manual sibling order per parent folder key: parent → ordered child
    /// keys. Folders missing from a list fall back to alphabetical, after the
    /// ranked ones — the same rule hosts already follow with sortOrder.
    sidebar_folder_order: IndexMap<String, Vec<String>> = "sidebarFolderOrder" => IndexMap::new();
    /// Folders the user created that hold no host yet; canonical paths, since
    /// an empty folder has no host to carry its label.
    sidebar_empty_folders: Vec<String> = "sidebarEmptyFolders" => Vec::new();
    /// Width of the per-session remote file browser.
    #[serde(with = "js::number")]
    sftp_panel_width: f64 = "sftpPanelWidth" => DEFAULT_SFTP_PANEL_WIDTH;
    /// Verbose diagnostic logging plus access to the log viewer and export.
    debug_mode: bool = "debugMode" => false;
}

impl PrefsState {
    /// The scheme id for the effective appearance (`terminalSchemeIdForMode`).
    pub fn terminal_scheme_id_for_mode(&self, mode: EffectiveThemeMode) -> &str {
        match mode {
            EffectiveThemeMode::Light => &self.light_terminal_scheme,
            EffectiveThemeMode::Dark => &self.dark_terminal_scheme,
        }
    }

    /// Remove a custom scheme. A terminal theme that was set to it returns to
    /// its default; hosts assigned to it fall back to the application setting
    /// on their own (`withoutCustomTerminalScheme`).
    pub fn without_custom_terminal_scheme(&mut self, id: &str) {
        let defaults = PrefsState::default();
        self.custom_terminal_schemes
            .retain(|scheme| scheme.id != id);
        if self.light_terminal_scheme == id {
            self.light_terminal_scheme = defaults.light_terminal_scheme;
        }
        if self.dark_terminal_scheme == id {
            self.dark_terminal_scheme = defaults.dark_terminal_scheme;
        }
    }

    /// The SSH keepalive fallback a connect sends: `None` when the preference
    /// leaves it to the OpenSSH configuration (`sshKeepalivePrefField`).
    pub fn ssh_keepalive_field(&self) -> Option<u32> {
        (self.ssh_keepalive_interval_seconds > 0.0)
            .then_some(self.ssh_keepalive_interval_seconds as u32)
    }

    /// The state as a stored JSON object, fields in order, unknown keys last.
    pub fn to_json_map(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        }
    }
}

/// `isLocalShellProfileArray`: at most 100 complete profiles with unique ids.
pub fn is_local_shell_profile_array(value: &Value) -> bool {
    let Some(entries) = value.as_array() else {
        return false;
    };
    if entries.len() > 100 {
        return false;
    }
    let mut ids = std::collections::HashSet::new();
    entries.iter().all(|entry| {
        let Some(profile) = entry.as_object() else {
            return false;
        };
        let text = |key: &str| profile.get(key).and_then(Value::as_str);
        let (Some(id), Some(name), Some(shell), Some(cwd), Some(startup), Some(args)) = (
            text("id"),
            text("name"),
            text("shell"),
            text("cwd"),
            text("startupCommand"),
            profile.get("args").and_then(Value::as_array),
        ) else {
            return false;
        };
        let valid = !id.is_empty()
            && js::utf16_len(id) <= 200
            && !ids.contains(id)
            && !js::js_trim(name).is_empty()
            && js::utf16_len(name) <= 200
            && js::utf16_len(shell) <= 4096
            && js::utf16_len(cwd) <= 4096
            && js::utf16_len(startup) <= 32_768
            && args.len() <= 64
            && args
                .iter()
                .all(|arg| arg.as_str().is_some_and(|arg| js::utf16_len(arg) <= 4096));
        if valid {
            ids.insert(id.to_owned());
        }
        valid
    })
}
