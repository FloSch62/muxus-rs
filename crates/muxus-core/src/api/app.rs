//! Application-level types: app info, X11 switches, logs, updates, windows,
//! command-line launches and the desktop shell's link handlers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::protocol::SessionProfile;

/// Shape of every REST error body (`{ message, code? }`).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub message: String,
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    /// Node's `process.platform` naming (`linux`, `win32`, `darwin`, …).
    pub platform: String,
    /// Home directory of the backend user (initial cwd suggestions).
    pub home_dir: String,
    /// Default login shell the backend would spawn for local terminals.
    pub default_shell: String,
    /// Algorithm names the SSH engine can negotiate, per ssh_config keyword
    /// ("Ciphers", "KexAlgorithms", "HostKeyAlgorithms", "MACs") — the editor
    /// warns about entries the dialer would have to skip.
    pub ssh_algorithms: BTreeMap<String, Vec<String>>,
}

/// Application-wide X11 forwarding switches; unset values follow the platform default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct X11Settings {
    /// Master switch: off never requests X11 and shows no X11 hints. Default off on macOS.
    pub enabled: bool,
    /// Whether hosts without a ForwardX11 setting forward X11. Default on with the bundled server.
    pub forward_by_default: bool,
    /// Bridge the bundled Windows X server to the system clipboard (read and write).
    pub clipboard: bool,
}

/// Body of PUT /api/x11/settings; omitted switches revert to their platform default.
#[skip_serializing_none]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct X11SettingsUpdate {
    pub enabled: Option<bool>,
    pub forward_by_default: Option<bool>,
    pub clipboard: bool,
}

/// Where forwarded X11 windows open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum X11Source {
    /// The X server the Windows app ships (one display per SSH connection).
    Bundled,
    /// The user's own `$DISPLAY` (XQuartz on macOS).
    Display,
    /// Nothing to forward to.
    None,
}

/// Platform defaults for the X11 switches a user may leave unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct X11Defaults {
    pub enabled: bool,
    pub forward_by_default: bool,
}

/// Where forwarded X11 windows open, plus the effective settings.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct X11Status {
    pub enabled: bool,
    pub forward_by_default: bool,
    pub clipboard: bool,
    pub source: X11Source,
    /// The `$DISPLAY` in use for `display`.
    pub display: Option<String>,
    pub defaults: X11Defaults,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppLogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

/// Which process wrote a log record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppLogSource {
    /// The embedded backend.
    Server,
    /// The desktop shell's main process.
    App,
}

/// One diagnostic log record from the in-memory app log buffer.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLogEntry {
    /// Unix epoch milliseconds.
    pub ts: i64,
    pub level: AppLogLevel,
    pub source: AppLogSource,
    pub msg: String,
    /// Structured fields attached to the record (host, err, …).
    pub context: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLogsResponse {
    pub entries: Vec<AppLogEntry>,
    /// True while the backend logs at debug level or below.
    pub debug_enabled: bool,
    /// Ring-buffer size; older entries beyond it are dropped.
    pub capacity: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLogSettingsInput {
    pub debug_enabled: bool,
}

/// Raw bookmark data discovered from a local Windows MobaXterm installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MobaXtermSessionSource {
    /// Human-readable origin shown before the user confirms the import.
    pub source: String,
    /// Reconstructed MobaXterm INI bookmark sections. Never contains passwords.
    pub content: String,
}

/// A local program the desktop app can open a downloaded remote file with.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalOpenApplication {
    /// Opaque to the UI; the shell resolves it again before launching.
    pub id: String,
    pub name: String,
    /// `data:` URL of the program's icon, when one was found.
    pub icon: Option<String>,
    /// The program declares support for the file's type.
    pub recommended: Option<bool>,
    /// The desktop's configured default program for the file's type.
    pub is_default: Option<bool>,
}

/// How the desktop app opens a downloaded remote file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LocalOpenTarget {
    Default,
    /// Windows' own "How do you want to open this file?" dialog.
    SystemChooser,
    Application {
        id: String,
    },
}

/// `{ ok: true } | { ok: false; message }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalOpenResult {
    Ok,
    Failed { message: String },
}

impl Serialize for LocalOpenResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = match self {
            LocalOpenResult::Ok => serde_json::json!({ "ok": true }),
            LocalOpenResult::Failed { message } => {
                serde_json::json!({ "ok": false, "message": message })
            }
        };
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LocalOpenResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            ok: bool,
            message: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(if raw.ok {
            LocalOpenResult::Ok
        } else {
            LocalOpenResult::Failed {
                message: raw.message.unwrap_or_default(),
            }
        })
    }
}

/// A program picked with the native file picker, or why the pick cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LocalProgramChoice {
    Application { application: LocalOpenApplication },
    Message { message: String },
}

/// A local copy whose contents a program changed after it was opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalCopyChange {
    pub id: String,
    /// Where the copy lives, for messages that point the user at it.
    pub file: String,
}

/// ssh_config keywords the dialer applies even though they have no editor
/// field of their own (they are edited under Advanced). Everything else in
/// Advanced is preserved for OpenSSH but never used by Muxus itself.
/// Lowercased for comparison, per ssh_config's case-insensitive keywords.
pub const DIAL_TIME_KEYWORDS: &[&str] = &[
    "ciphers",
    "kexalgorithms",
    "hostkeyalgorithms",
    "macs",
    "compression",
    "connecttimeout",
    "serveraliveinterval",
    "serveralivecountmax",
    "identityagent",
    "passwordauthentication",
    "kbdinteractiveauthentication",
    "challengeresponseauthentication",
    "preferredauthentications",
    "userknownhostsfile",
    "globalknownhostsfile",
    "setenv",
    "sendenv",
    "remotecommand",
    "requesttty",
    "stricthostkeychecking",
    "forwardx11",
    "securitykeyprovider",
];

/// Whether an ssh_config keyword (any case) is one the dialer applies.
pub fn is_dial_time_keyword(keyword: &str) -> bool {
    let lower = keyword.to_ascii_lowercase();
    DIAL_TIME_KEYWORDS.contains(&lower.as_str())
}

/// Result of a release check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckResult {
    Available {
        current_version: String,
        latest_version: String,
        release_name: Option<String>,
        release_url: String,
        published_at: Option<String>,
    },
    NotAvailable {
        current_version: String,
        latest_version: Option<String>,
        reason: Option<String>,
    },
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateCheckWire {
    available: bool,
    current_version: String,
    latest_version: Option<String>,
    release_name: Option<String>,
    release_url: Option<String>,
    published_at: Option<String>,
    reason: Option<String>,
}

impl Serialize for UpdateCheckResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = match self.clone() {
            UpdateCheckResult::Available {
                current_version,
                latest_version,
                release_name,
                release_url,
                published_at,
            } => UpdateCheckWire {
                available: true,
                current_version,
                latest_version: Some(latest_version),
                release_name,
                release_url: Some(release_url),
                published_at,
                reason: None,
            },
            UpdateCheckResult::NotAvailable {
                current_version,
                latest_version,
                reason,
            } => UpdateCheckWire {
                available: false,
                current_version,
                latest_version,
                release_name: None,
                release_url: None,
                published_at: None,
                reason,
            },
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for UpdateCheckResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = UpdateCheckWire::deserialize(deserializer)?;
        if wire.available {
            Ok(UpdateCheckResult::Available {
                current_version: wire.current_version,
                latest_version: wire
                    .latest_version
                    .ok_or_else(|| serde::de::Error::missing_field("latestVersion"))?,
                release_name: wire.release_name,
                release_url: wire
                    .release_url
                    .ok_or_else(|| serde::de::Error::missing_field("releaseUrl"))?,
                published_at: wire.published_at,
            })
        } else {
            Ok(UpdateCheckResult::NotAvailable {
                current_version: wire.current_version,
                latest_version: wire.latest_version,
                reason: wire.reason,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesktopUpdateStatus {
    Disabled,
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Ready,
    Installing,
    Updated,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesktopUpdateReason {
    Development,
    Store,
    PackageManager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopUpdateSource {
    Store,
}

/// In-app update state owned by the desktop shell. `disabled` builds
/// (development, Linux packages) keep using [`UpdateCheckResult`].
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopUpdateState {
    pub status: DesktopUpdateStatus,
    pub current_version: String,
    pub source: Option<DesktopUpdateSource>,
    pub version: Option<String>,
    pub percent: Option<f64>,
    pub reason: Option<DesktopUpdateReason>,
    pub error: Option<String>,
}

/// One serial device reported by the host OS.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialPortInfo {
    /// OS-native path (COM3, /dev/ttyUSB0, /dev/tty.usbserial-…, etc.).
    pub path: String,
    pub manufacturer: Option<String>,
    pub serial_number: Option<String>,
    pub pnp_id: Option<String>,
    pub location_id: Option<String>,
    pub product_id: Option<String>,
    pub vendor_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialPortsResponse {
    pub ports: Vec<SerialPortInfo>,
}

/// One Windows Subsystem for Linux distribution registered for the user.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WslDistribution {
    /// Name `wsl.exe -d` accepts (Ubuntu, Debian, …).
    pub name: String,
    /// The distribution plain `wsl.exe` starts.
    pub is_default: bool,
    /// The distribution's own icon as a `data:` URL, when it ships one.
    pub icon: Option<String>,
}

/// Installed WSL distributions, default first; empty off Windows or without WSL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WslDistributionsResponse {
    pub distributions: Vec<WslDistribution>,
}

/// One target supplied when the desktop executable is launched from a
/// command line. Names stay unresolved until the UI has loaded the host and
/// workspace catalogs. A `connect` name is an already validated ad-hoc
/// "[user@]host[:port]" SSH target. A `url` name is an ssh:// or telnet://
/// link as received, parsed by the UI so it can report a malformed one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CommandLineLaunch {
    Host { name: String },
    Folder { name: String },
    Workspace { name: String },
    Connect { name: String },
    Url { name: String },
}

/// One extra application window. Workspace windows either create a named
/// workspace or open an existing one; session windows start a fresh shell;
/// SFTP windows stay attached to an existing SSH transport. There is at most
/// one settings window; asking for it again brings it to the front on the
/// requested section.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AppWindowLaunch {
    #[serde(rename_all = "camelCase")]
    Workspace {
        /// Omitted when the new window should create a workspace named `title`.
        workspace_id: Option<String>,
        title: String,
    },
    Session {
        profile: SessionProfile,
        title: String,
        color: Option<String>,
    },
    /// One live tab claimed from another window through an opaque token.
    #[serde(rename_all = "camelCase")]
    TabTransfer { transfer_id: String, title: String },
    #[serde(rename_all = "camelCase")]
    Sftp {
        conn_id: String,
        title: String,
        path: Option<String>,
    },
    Settings {
        /// Settings section to show; the window keeps its current one otherwise.
        section: Option<String>,
        /// Entry within that section to bring into view.
        item: Option<String>,
    },
}

/// Whether the operating system opens one link scheme with this Muxus.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkHandlerStatus {
    pub is_default: bool,
    /// The program that opens these links now, when the system names one.
    pub current_handler: Option<String>,
}

/// Desktop only: which ssh:// and telnet:// links open in Muxus.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHandlerState {
    /// Why Muxus cannot register itself here, when it cannot.
    pub unavailable: Option<String>,
    pub ssh: LinkHandlerStatus,
    pub telnet: LinkHandlerStatus,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkHandlerRegistration {
    pub state: LinkHandlerState,
    /// Set when registering failed.
    pub error: Option<String>,
    /// The system keeps this choice itself (Windows default apps, a Store
    /// installation), so its settings were opened for the user to pick Muxus.
    pub opened_system_settings: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_results_keep_their_wire_shape() {
        let json = r#"{"available":true,"currentVersion":"0.11.0","latestVersion":"0.11.1","releaseUrl":"https://github.com/FloSch62/muxus/releases/tag/v0.11.1"}"#;
        let parsed: UpdateCheckResult = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
        let none = r#"{"available":false,"currentVersion":"0.11.1","reason":"offline"}"#;
        let parsed: UpdateCheckResult = serde_json::from_str(none).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), none);
    }

    #[test]
    fn window_launches_use_kebab_kinds() {
        let launch = AppWindowLaunch::TabTransfer {
            transfer_id: "t1".into(),
            title: "x".into(),
        };
        assert_eq!(
            serde_json::to_string(&launch).unwrap(),
            r#"{"kind":"tab-transfer","transferId":"t1","title":"x"}"#
        );
        let open: LocalOpenTarget = serde_json::from_str(r#"{"kind":"system-chooser"}"#).unwrap();
        assert_eq!(open, LocalOpenTarget::SystemChooser);
        assert_eq!(
            serde_json::to_string(&LocalOpenResult::Ok).unwrap(),
            r#"{"ok":true}"#
        );
    }
}
