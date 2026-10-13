//! Making Muxus the system handler for ssh:// and telnet:// links (`electron/src/link-handlers.ts`),
//! behind Settings → Behavior → Links.
//!
//! Registering is always the user's choice: nothing here runs until Settings asks, because it
//! changes which program the whole system uses for a scheme. [`Backend::link_handlers`] reads
//! the current handlers, [`Backend::register_link_handler`] claims one scheme.
//!
//! - **Linux** ([`LinuxLinkHandlers`]): xdg-mime and mimeapps.list. An installed package uses
//!   the desktop file it installed (`muxus.desktop`); an AppImage or a development build gets a
//!   hidden desktop file that points at itself, since nothing else knows where it lives.
//! - **macOS and Windows** ([`SystemLinkHandlers`]): Launch Services and the per-user URL
//!   associations. Windows keeps a user's explicit choice of default app out of reach of
//!   programs, so when that choice wins, or for a Store package, its Default apps page opens.
//!
//! Muxus has no link scheme of its own; the two connection schemes are the whole list
//! (`muxus_core::connection_links::CONNECTION_LINK_SCHEMES`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use muxus_core::api::{LinkHandlerRegistration, LinkHandlerState, LinkHandlerStatus};
use muxus_core::connection_links::{CONNECTION_LINK_SCHEMES, ConnectionLinkScheme};
use muxus_core::platform::Platform;

use crate::backend::Backend;
use crate::error::ApiResult;

/// The desktop file the Linux packages install.
pub const INSTALLED_DESKTOP_ID: &str = "muxus.desktop";
/// Written for an AppImage or a development build, which install no desktop file.
pub const GENERATED_DESKTOP_ID: &str = "muxus-url-handler.desktop";
/// The macOS bundle identifier (`appId` of the Electron build).
pub const BUNDLE_ID: &str = "io.github.flosch62.muxus";

const XDG_MIME_MISSING: &str =
    "xdg-mime was not found. Install xdg-utils so Muxus can be chosen for links.";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

fn not_default() -> LinkHandlerStatus {
    LinkHandlerStatus {
        is_default: false,
        current_handler: None,
    }
}

fn mime_type(scheme: ConnectionLinkScheme) -> String {
    format!("x-scheme-handler/{}", scheme.as_str())
}

/// An Exec argument that needs no quoting.
fn is_plain_argument(argument: &str) -> bool {
    !argument.is_empty()
        && argument
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_/.,:=@+-".contains(&byte))
}

/// One Exec argument, quoted per the Desktop Entry spec: inside quotes `"`, `` ` ``, `$` and
/// `\` take a backslash, the string escape doubles every backslash again, and `%` is written
/// `%%` so it is not a field code.
pub fn desktop_exec_argument(argument: &str) -> Result<String, String> {
    if argument.contains(['\n', '\r', '\t']) {
        return Err("A launch path cannot contain line breaks.".into());
    }
    let quoted = if is_plain_argument(argument) {
        argument.to_string()
    } else {
        let mut escaped = String::with_capacity(argument.len() + 2);
        escaped.push('"');
        for ch in argument.chars() {
            if matches!(ch, '"' | '`' | '$' | '\\') {
                escaped.push('\\');
            }
            escaped.push(ch);
        }
        escaped.push('"');
        escaped
    };
    Ok(quoted.replace('\\', "\\\\").replace('%', "%%"))
}

/// A hidden launcher that hands links to this Muxus, for the schemes it should claim.
pub fn generated_desktop_entry(
    command: &[String],
    schemes: &[ConnectionLinkScheme],
) -> Result<String, String> {
    let mut exec = command
        .iter()
        .map(|argument| desktop_exec_argument(argument))
        .collect::<Result<Vec<_>, _>>()?;
    exec.push("%u".into());
    let mime_types: String = schemes
        .iter()
        .map(|scheme| format!("{};", mime_type(*scheme)))
        .collect();
    Ok([
        "[Desktop Entry]".to_string(),
        "Type=Application".into(),
        "Name=Muxus".into(),
        "Comment=Opens ssh:// and telnet:// links in Muxus".into(),
        format!("Exec={}", exec.join(" ")),
        "Icon=muxus".into(),
        "Terminal=false".into(),
        "NoDisplay=true".into(),
        format!("MimeType={mime_types}"),
        "StartupWMClass=muxus".into(),
        String::new(),
    ]
    .join("\n"))
}

/// An AppImage puts its own mount first in PATH, XDG_DATA_DIRS and LD_LIBRARY_PATH. Those
/// entries must not reach xdg-mime (the mount has nothing to say about desktop defaults, and a
/// space in its name breaks the script's directory loops), nor a relaunched update.
pub fn without_app_image_paths(env: &HashMap<String, String>) -> HashMap<String, String> {
    let (Some(_), Some(mount)) = (
        env.get("APPIMAGE").filter(|value| !value.is_empty()),
        env.get("APPDIR").filter(|value| !value.is_empty()),
    ) else {
        return env.clone();
    };
    let mut cleaned = env.clone();
    for name in ["PATH", "XDG_DATA_DIRS", "LD_LIBRARY_PATH"] {
        let Some(value) = env.get(name) else { continue };
        let kept: Vec<&str> = value
            .split(':')
            .filter(|entry| {
                !entry.is_empty() && *entry != mount && !entry.starts_with(&format!("{mount}/"))
            })
            .collect();
        if kept.is_empty() {
            cleaned.remove(name);
        } else {
            cleaned.insert(name.into(), kept.join(":"));
        }
    }
    cleaned
}

// --- Desktop Entry helpers (`linux-applications.ts`) -------------------------------------------

/// The keys of one group of a freedesktop key file; the first value of a key wins, and a
/// repeated group is ignored (`parseKeyFileGroup`).
pub fn parse_key_file_group(text: &str, wanted: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    let mut seen_groups: Vec<String> = Vec::new();
    let mut in_group = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let group = &line[1..line.len() - 1];
            in_group = group == wanted && !seen_groups.iter().any(|seen| seen == group);
            seen_groups.push(group.to_string());
            continue;
        }
        if !in_group {
            continue;
        }
        let Some(equals) = line.find('=').filter(|index| *index > 0) else {
            continue;
        };
        let key = line[..equals].trim().to_string();
        values
            .entry(key)
            .or_insert_with(|| line[equals + 1..].trim().to_string());
    }
    values
}

/// Undo the escapes a Desktop Entry `string` value may contain.
pub fn unescape_desktop_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(&next) = chars.peek()
        {
            let replacement = match next {
                's' => Some(' '),
                'n' => Some('\n'),
                't' => Some('\t'),
                'r' => Some('\r'),
                '\\' => Some('\\'),
                _ => None,
            };
            if let Some(replacement) = replacement {
                out.push(replacement);
                chars.next();
                continue;
            }
        }
        out.push(ch);
    }
    out
}

/// Split an (already unescaped) Exec value into arguments; `None` when it is malformed.
pub fn split_exec(exec: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = exec.chars().collect();
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_argument = false;
    let mut quoted = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if quoted {
            let next = chars.get(index + 1).copied();
            if ch == '\\' && next.is_some_and(|next| "\"`$\\".contains(next)) {
                current.push(next.unwrap_or_default());
                index += 1;
            } else if ch == '"' {
                quoted = false;
            } else {
                current.push(ch);
            }
            index += 1;
            continue;
        }
        if ch == '"' {
            quoted = true;
            in_argument = true;
        } else if matches!(ch, ' ' | '\t' | '\n') {
            if in_argument {
                args.push(std::mem::take(&mut current));
            }
            current.clear();
            in_argument = false;
        } else if ch == '\\' && index + 1 < chars.len() {
            index += 1;
            current.push(chars[index]);
            in_argument = true;
        } else {
            current.push(ch);
            in_argument = true;
        }
        index += 1;
    }
    if quoted {
        return None;
    }
    if in_argument {
        args.push(current);
    }
    (!args.is_empty()).then_some(args)
}

/// XDG data directories, most important first.
pub fn data_directories(env: &HashMap<String, String>, home: &Path) -> Vec<PathBuf> {
    let data_home = env
        .get("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".local").join("share"));
    let mut dirs: Vec<PathBuf> = env
        .get("XDG_DATA_DIRS")
        .map(|value| {
            value
                .split(':')
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    if dirs.is_empty() {
        dirs = vec!["/usr/local/share".into(), "/usr/share".into()];
    }
    let mut unique: Vec<PathBuf> = Vec::new();
    for dir in std::iter::once(data_home).chain(dirs) {
        if !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    unique
}

// --- Running programs ------------------------------------------------------------------------

/// Why a helper program failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The program is not installed.
    NotFound,
    Failed(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::NotFound => f.write_str("not found"),
            RunError::Failed(message) => f.write_str(message),
        }
    }
}

/// Runs a program without a shell and returns its standard output.
#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(
        &self,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<String, RunError>;
}

/// The real runner: exactly `env`, a 10 second limit.
pub struct ProcessRunner;

#[async_trait]
impl CommandRunner for ProcessRunner {
    async fn run(
        &self,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<String, RunError> {
        let mut process = tokio::process::Command::new(command);
        process
            .args(args)
            .env_clear()
            .envs(env)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let output = tokio::time::timeout(COMMAND_TIMEOUT, process.output())
            .await
            .map_err(|_| RunError::Failed(format!("{command} did not finish in time")))?
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    RunError::NotFound
                } else {
                    RunError::Failed(error.to_string())
                }
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(RunError::Failed(if stderr.is_empty() {
                format!("{command} failed ({})", output.status)
            } else {
                stderr
            }));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

// --- Linux -----------------------------------------------------------------------------------

/// Everything [`LinuxLinkHandlers`] needs.
#[derive(Clone)]
pub struct LinuxLinkHandlerOptions {
    /// Environment for xdg-mime (see [`without_app_image_paths`]).
    pub env: HashMap<String, String>,
    pub home: PathBuf,
    /// The arguments that start this Muxus, ahead of the link: the AppImage file or the
    /// executable.
    pub command: Vec<String>,
    /// A packaged installation may use the desktop file its package installed.
    pub installed: bool,
    /// Where a link to a launch path that needs quoting is kept. xdg-utils takes the first
    /// space-separated word of Exec and ignores quotes, so an AppImage named
    /// "Muxus 0.10.0.AppImage" is registered through this link instead.
    pub alias_directory: Option<PathBuf>,
    pub run: Arc<dyn CommandRunner>,
}

/// Linux registration through xdg-mime and mimeapps.list.
pub struct LinuxLinkHandlers {
    options: LinuxLinkHandlerOptions,
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn desktop_command(text: &str) -> Option<Vec<String>> {
    let exec = parse_key_file_group(text, "Desktop Entry")
        .get("Exec")
        .cloned()
        .unwrap_or_default();
    split_exec(&unescape_desktop_string(&exec))
}

impl LinuxLinkHandlers {
    pub fn new(options: LinuxLinkHandlerOptions) -> Self {
        Self { options }
    }

    fn application_directories(&self) -> Vec<PathBuf> {
        data_directories(&self.options.env, &self.options.home)
            .into_iter()
            .map(|dir| dir.join("applications"))
            .collect()
    }

    fn generated_path(&self) -> PathBuf {
        self.application_directories()
            .into_iter()
            .next()
            .unwrap_or_default()
            .join(GENERATED_DESKTOP_ID)
    }

    fn find_desktop_file(&self, id: &str) -> Option<String> {
        self.application_directories()
            .iter()
            .find_map(|dir| read_text(&dir.join(id)))
    }

    fn desktop_id(&self) -> &'static str {
        if self.options.installed && self.find_desktop_file(INSTALLED_DESKTOP_ID).is_some() {
            INSTALLED_DESKTOP_ID
        } else {
            GENERATED_DESKTOP_ID
        }
    }

    fn alias_path(&self) -> Option<PathBuf> {
        let [single] = self.options.command.as_slice() else {
            return None;
        };
        if is_plain_argument(single) {
            return None;
        }
        let directory = self.options.alias_directory.as_ref()?;
        is_plain_argument(&directory.to_string_lossy())
            .then(|| directory.join("muxus-link-handler"))
    }

    /// Whether the generated entry still starts this copy (an AppImage may have moved).
    fn generated_entry_is_current(&self) -> bool {
        let Some(text) = read_text(&self.generated_path()) else {
            return false;
        };
        let exec = desktop_command(&text);
        let alias = self.alias_path();
        let command: Vec<String> = match &alias {
            Some(alias) => vec![alias.to_string_lossy().into_owned()],
            None => self.options.command.clone(),
        };
        let mut expected: Vec<String> = command.iter().map(|arg| arg.replace('%', "%%")).collect();
        expected.push("%u".into());
        if exec.as_ref() != Some(&expected) {
            return false;
        }
        let Some(alias) = alias else { return true };
        let linked = std::fs::canonicalize(&alias).ok();
        let target = std::fs::canonicalize(&self.options.command[0]).ok();
        linked.is_some() && linked == target
    }

    /// The launch command to write into the entry, pointing the alias at this copy first.
    fn launch_command(&self) -> Result<Vec<String>, String> {
        let Some(alias) = self.alias_path() else {
            return Ok(self.options.command.clone());
        };
        if let Some(parent) = alias.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let temporary =
            alias.with_file_name(format!("muxus-link-handler.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&temporary);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&self.options.command[0], &temporary)
            .map_err(|error| error.to_string())?;
        #[cfg(not(unix))]
        std::fs::copy(&self.options.command[0], &temporary).map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, &alias).map_err(|error| error.to_string())?;
        Ok(vec![alias.to_string_lossy().into_owned()])
    }

    fn handler_name(&self, id: &str) -> String {
        self.find_desktop_file(id)
            .and_then(|text| {
                parse_key_file_group(&text, "Desktop Entry")
                    .get("Name")
                    .map(|name| unescape_desktop_string(name))
            })
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| id.strip_suffix(".desktop").unwrap_or(id).to_string())
    }

    async fn query_default(&self, scheme: ConnectionLinkScheme) -> Result<String, RunError> {
        let args = vec!["query".into(), "default".into(), mime_type(scheme)];
        Ok(self
            .options
            .run
            .run("xdg-mime", &args, &self.options.env)
            .await?
            .trim()
            .to_string())
    }

    pub async fn state(&self) -> LinkHandlerState {
        let id = self.desktop_id();
        let current = id == INSTALLED_DESKTOP_ID || self.generated_entry_is_current();
        let mut statuses = Vec::new();
        for scheme in CONNECTION_LINK_SCHEMES {
            let handler = match self.query_default(scheme).await {
                Ok(handler) => handler,
                Err(error) => {
                    return LinkHandlerState {
                        unavailable: Some(if error == RunError::NotFound {
                            XDG_MIME_MISSING.into()
                        } else {
                            "The desktop’s default applications could not be read.".into()
                        }),
                        ssh: not_default(),
                        telnet: not_default(),
                    };
                }
            };
            if handler.is_empty() {
                statuses.push(not_default());
                continue;
            }
            let is_default = current && handler == id;
            statuses.push(LinkHandlerStatus {
                is_default,
                // An entry from an AppImage that has since moved, or from another build.
                current_handler: Some(if handler == GENERATED_DESKTOP_ID && !is_default {
                    "Another copy of Muxus".into()
                } else {
                    self.handler_name(&handler)
                }),
            });
        }
        let telnet = statuses.pop().unwrap_or_else(not_default);
        let ssh = statuses.pop().unwrap_or_else(not_default);
        LinkHandlerState {
            unavailable: None,
            ssh,
            telnet,
        }
    }

    pub async fn register(&self, scheme: ConnectionLinkScheme) -> LinkHandlerRegistration {
        let result: Result<(), RunError> = async {
            let id = self.desktop_id();
            if id == GENERATED_DESKTOP_ID {
                // Claim only the schemes the user chose, so the entry never becomes a fallback
                // handler for the other one.
                let before = self.state().await;
                let schemes: Vec<ConnectionLinkScheme> = CONNECTION_LINK_SCHEMES
                    .into_iter()
                    .filter(|candidate| {
                        *candidate == scheme || status_of(&before, *candidate).is_default
                    })
                    .collect();
                let file = self.generated_path();
                if let Some(parent) = file.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|error| RunError::Failed(error.to_string()))?;
                }
                let entry = generated_desktop_entry(
                    &self.launch_command().map_err(RunError::Failed)?,
                    &schemes,
                )
                .map_err(RunError::Failed)?;
                let temporary = file
                    .with_file_name(format!("{GENERATED_DESKTOP_ID}.{}.tmp", std::process::id()));
                write_private_file(&temporary, &entry)
                    .map_err(|error| RunError::Failed(error.to_string()))?;
                std::fs::rename(&temporary, &file)
                    .map_err(|error| RunError::Failed(error.to_string()))?;
            }
            let args = vec!["default".into(), id.into(), mime_type(scheme)];
            self.options
                .run
                .run("xdg-mime", &args, &self.options.env)
                .await
                .map(|_| ())
        }
        .await;
        if let Err(error) = result {
            return LinkHandlerRegistration {
                state: self.state().await,
                error: Some(match error {
                    RunError::NotFound => XDG_MIME_MISSING.into(),
                    RunError::Failed(message) => format!(
                        "Could not make Muxus the handler for {}:// links: {message}",
                        scheme.as_str()
                    ),
                }),
                opened_system_settings: None,
            };
        }
        let state = self.state().await;
        let accepted = status_of(&state, scheme).is_default;
        LinkHandlerRegistration {
            error: (!accepted).then(|| {
                format!(
                    "The desktop did not accept Muxus as the handler for {}:// links.",
                    scheme.as_str()
                )
            }),
            state,
            opened_system_settings: None,
        }
    }
}

fn write_private_file(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

fn status_of(state: &LinkHandlerState, scheme: ConnectionLinkScheme) -> &LinkHandlerStatus {
    match scheme {
        ConnectionLinkScheme::Ssh => &state.ssh,
        ConnectionLinkScheme::Telnet => &state.telnet,
    }
}

// --- macOS and Windows -----------------------------------------------------------------------

/// The system calls macOS and Windows registration needs (Electron's protocol-client API).
#[async_trait]
pub trait ProtocolClientApi: Send + Sync {
    /// macOS: whether Launch Services opens `scheme` with this app.
    async fn is_default_protocol_client(&self, scheme: &str) -> bool;
    /// Register this app for `scheme`; false when the system refused.
    async fn set_as_default_protocol_client(&self, scheme: &str) -> bool;
    /// The name of the program that opens `url` now ("" when none).
    async fn application_name_for_protocol(&self, url: &str) -> String;
    /// Windows: the executable the system resolves for `url`, honouring the user's choice.
    async fn application_path_for_protocol(&self, url: &str) -> Option<String>;
    /// Windows: open Settings → Default apps.
    async fn open_default_apps_settings(&self) -> Result<(), String>;
}

/// Everything [`SystemLinkHandlers`] needs.
pub struct SystemLinkHandlerOptions {
    /// macOS or Windows.
    pub platform: Platform,
    pub api: Arc<dyn ProtocolClientApi>,
    /// Microsoft Store packages declare their schemes in the manifest instead.
    pub windows_store: bool,
    /// The running executable.
    pub executable: String,
}

/// macOS (Launch Services) and Windows registration.
pub struct SystemLinkHandlers {
    options: SystemLinkHandlerOptions,
}

/// Windows paths compare case-insensitively, `/` and `\` alike.
fn same_windows_path(left: &str, right: &str) -> bool {
    let normalize = |path: &str| path.trim_matches('"').replace('/', "\\").to_lowercase();
    normalize(left) == normalize(right)
}

impl SystemLinkHandlers {
    pub fn new(options: SystemLinkHandlerOptions) -> Self {
        Self { options }
    }

    async fn status(&self, scheme: ConnectionLinkScheme) -> LinkHandlerStatus {
        let url = format!("{}://", scheme.as_str());
        let api = &self.options.api;
        let name = api.application_name_for_protocol(&url).await;
        let current_handler = (!name.is_empty()).then_some(name);
        if self.options.platform == Platform::MacOs {
            return LinkHandlerStatus {
                is_default: api.is_default_protocol_client(scheme.as_str()).await,
                current_handler,
            };
        }
        // The handler Windows resolves, which honours the user's choice in Settings.
        let handler = api.application_path_for_protocol(&url).await;
        LinkHandlerStatus {
            is_default: handler
                .is_some_and(|path| same_windows_path(&path, &self.options.executable)),
            current_handler,
        }
    }

    pub async fn state(&self) -> LinkHandlerState {
        LinkHandlerState {
            unavailable: None,
            ssh: self.status(ConnectionLinkScheme::Ssh).await,
            telnet: self.status(ConnectionLinkScheme::Telnet).await,
        }
    }

    pub async fn register(&self, scheme: ConnectionLinkScheme) -> LinkHandlerRegistration {
        let windows = self.options.platform == Platform::Windows;
        let refused = || {
            format!(
                "The system did not accept Muxus as the handler for {}:// links.",
                scheme.as_str()
            )
        };
        if windows && self.options.windows_store {
            let _ = self.options.api.open_default_apps_settings().await;
            return LinkHandlerRegistration {
                state: self.state().await,
                error: None,
                opened_system_settings: Some(true),
            };
        }
        if !self
            .options
            .api
            .set_as_default_protocol_client(scheme.as_str())
            .await
        {
            return LinkHandlerRegistration {
                state: self.state().await,
                error: Some(refused()),
                opened_system_settings: None,
            };
        }
        let state = self.state().await;
        if status_of(&state, scheme).is_default {
            return LinkHandlerRegistration {
                state,
                error: None,
                opened_system_settings: None,
            };
        }
        if windows {
            let _ = self.options.api.open_default_apps_settings().await;
            return LinkHandlerRegistration {
                state,
                error: None,
                opened_system_settings: Some(true),
            };
        }
        LinkHandlerRegistration {
            state,
            error: Some(refused()),
            opened_system_settings: None,
        }
    }
}

/// Windows: the per-user URL association through `reg.exe` (`setAsDefaultProtocolClient`
/// writes the same keys).
pub struct WindowsProtocolClient {
    pub executable: String,
    pub run: Arc<dyn CommandRunner>,
    pub env: HashMap<String, String>,
}

impl WindowsProtocolClient {
    async fn reg(&self, args: &[&str]) -> Result<String, RunError> {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        self.run.run("reg", &args, &self.env).await
    }

    /// The default value of a key, from `reg query` output.
    fn default_value(output: &str) -> Option<String> {
        output.lines().find_map(|line| {
            let line = line.trim();
            let rest = line
                .strip_prefix("(Default)")
                .or_else(|| line.strip_prefix("(Standard)"))?;
            let value = rest.trim_start().strip_prefix("REG_SZ")?.trim();
            (!value.is_empty()).then(|| value.to_string())
        })
    }

    fn named_value(output: &str, name: &str) -> Option<String> {
        output.lines().find_map(|line| {
            let rest = line.trim().strip_prefix(name)?;
            let value = rest.trim_start().strip_prefix("REG_SZ")?.trim();
            (!value.is_empty()).then(|| value.to_string())
        })
    }

    /// The program of a `shell\open\command` line.
    fn command_program(command: &str) -> Option<String> {
        let command = command.trim();
        if let Some(rest) = command.strip_prefix('"') {
            return rest.split('"').next().map(str::to_string);
        }
        command.split_whitespace().next().map(str::to_string)
    }

    async fn handler_command(&self, scheme: &str) -> Option<String> {
        let choice = self
            .reg(&[
                "query",
                &format!(
                    "HKCU\\Software\\Microsoft\\Windows\\Shell\\Associations\\UrlAssociations\\{scheme}\\UserChoice"
                ),
                "/v",
                "ProgId",
            ])
            .await
            .ok()
            .and_then(|output| Self::named_value(&output, "ProgId"));
        let class = choice.unwrap_or_else(|| scheme.to_string());
        let output = self
            .reg(&[
                "query",
                &format!("HKCR\\{class}\\shell\\open\\command"),
                "/ve",
            ])
            .await
            .ok()?;
        Self::default_value(&output)
    }
}

#[async_trait]
impl ProtocolClientApi for WindowsProtocolClient {
    async fn is_default_protocol_client(&self, scheme: &str) -> bool {
        self.application_path_for_protocol(&format!("{scheme}://"))
            .await
            .is_some_and(|path| same_windows_path(&path, &self.executable))
    }

    async fn set_as_default_protocol_client(&self, scheme: &str) -> bool {
        let key = format!("HKCU\\Software\\Classes\\{scheme}");
        let description = format!("URL:{scheme}");
        let open = format!("{key}\\shell\\open\\command");
        let command = format!("\"{}\" \"%1\"", self.executable);
        for args in [
            vec!["add", key.as_str(), "/ve", "/d", description.as_str(), "/f"],
            vec!["add", key.as_str(), "/v", "URL Protocol", "/d", "", "/f"],
            vec!["add", open.as_str(), "/ve", "/d", command.as_str(), "/f"],
        ] {
            if self.reg(&args).await.is_err() {
                return false;
            }
        }
        true
    }

    async fn application_name_for_protocol(&self, url: &str) -> String {
        let scheme = url.trim_end_matches("://");
        self.handler_command(scheme)
            .await
            .and_then(|command| Self::command_program(&command))
            .and_then(|program| {
                Path::new(&program.replace('\\', "/"))
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .unwrap_or_default()
    }

    async fn application_path_for_protocol(&self, url: &str) -> Option<String> {
        let scheme = url.trim_end_matches("://");
        Self::command_program(&self.handler_command(scheme).await?)
    }

    async fn open_default_apps_settings(&self) -> Result<(), String> {
        let args: Vec<String> = ["/c", "start", "", "ms-settings:defaultapps"]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
        self.run
            .run("cmd", &args, &self.env)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// macOS: Launch Services through `osascript` (JavaScript for Automation reaches AppKit and
/// CoreServices without linking them).
pub struct MacProtocolClient {
    pub bundle_id: String,
    pub run: Arc<dyn CommandRunner>,
    pub env: HashMap<String, String>,
}

const MAC_HANDLER_SCRIPT: &str = "ObjC.import('AppKit');
function run(argv) {
  const app = $.NSWorkspace.sharedWorkspace.URLForApplicationToOpenURL($.NSURL.URLWithString(argv[0]));
  if (!app || app.isNil()) return '';
  const bundle = $.NSBundle.bundleWithURL(app);
  const id = bundle.isNil() ? '' : ObjC.unwrap(bundle.bundleIdentifier);
  const name = ObjC.unwrap($.NSFileManager.defaultManager.displayNameAtPath(app.path)).replace(/\\.app$/, '');
  return id + '\\n' + name;
}";

const MAC_REGISTER_SCRIPT: &str = "ObjC.import('CoreServices');
function run(argv) {
  return String($.LSSetDefaultHandlerForURLScheme($(argv[0]), $(argv[1])));
}";

impl MacProtocolClient {
    async fn handler(&self, url: &str) -> Option<(String, String)> {
        let args = vec![
            "-l".to_string(),
            "JavaScript".into(),
            "-e".into(),
            MAC_HANDLER_SCRIPT.into(),
            url.into(),
        ];
        let output = self.run.run("osascript", &args, &self.env).await.ok()?;
        let mut lines = output.trim_end_matches('\n').splitn(2, '\n');
        let id = lines.next()?.to_string();
        let name = lines.next().unwrap_or_default().to_string();
        (!id.is_empty() || !name.is_empty()).then_some((id, name))
    }
}

#[async_trait]
impl ProtocolClientApi for MacProtocolClient {
    async fn is_default_protocol_client(&self, scheme: &str) -> bool {
        self.handler(&format!("{scheme}://"))
            .await
            .is_some_and(|(id, _)| id.eq_ignore_ascii_case(&self.bundle_id))
    }

    async fn set_as_default_protocol_client(&self, scheme: &str) -> bool {
        let args = vec![
            "-l".to_string(),
            "JavaScript".into(),
            "-e".into(),
            MAC_REGISTER_SCRIPT.into(),
            scheme.into(),
            self.bundle_id.clone(),
        ];
        self.run
            .run("osascript", &args, &self.env)
            .await
            .is_ok_and(|status| status.trim() == "0")
    }

    async fn application_name_for_protocol(&self, url: &str) -> String {
        self.handler(url)
            .await
            .map(|(_, name)| name)
            .unwrap_or_default()
    }

    async fn application_path_for_protocol(&self, _url: &str) -> Option<String> {
        None
    }

    async fn open_default_apps_settings(&self) -> Result<(), String> {
        Err("macOS has no such page".into())
    }
}

/// The registration this platform uses.
pub enum LinkHandlers {
    Linux(LinuxLinkHandlers),
    System(SystemLinkHandlers),
}

impl LinkHandlers {
    pub async fn state(&self) -> LinkHandlerState {
        match self {
            LinkHandlers::Linux(handlers) => handlers.state().await,
            LinkHandlers::System(handlers) => handlers.state().await,
        }
    }

    pub async fn register(&self, scheme: ConnectionLinkScheme) -> LinkHandlerRegistration {
        match self {
            LinkHandlers::Linux(handlers) => handlers.register(scheme).await,
            LinkHandlers::System(handlers) => handlers.register(scheme).await,
        }
    }

    /// For a running Muxus: `env` is its environment, `user_data` its data folder (the alias
    /// link lives there). `None` on platforms without link registration.
    pub fn for_process(
        platform: Platform,
        env: &HashMap<String, String>,
        home: &Path,
        user_data: &Path,
        packaged: bool,
    ) -> Option<Self> {
        let executable = std::env::current_exe()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "muxus".into());
        let run: Arc<dyn CommandRunner> = Arc::new(ProcessRunner);
        match platform {
            Platform::Linux | Platform::FreeBsd => {
                // An AppImage runs from a temporary mount; the file itself is what to start.
                let app_image = env.get("APPIMAGE").filter(|path| !path.is_empty()).cloned();
                Some(LinkHandlers::Linux(LinuxLinkHandlers::new(
                    LinuxLinkHandlerOptions {
                        env: without_app_image_paths(env),
                        home: home.to_path_buf(),
                        installed: packaged && app_image.is_none(),
                        command: vec![app_image.unwrap_or(executable)],
                        alias_directory: Some(user_data.to_path_buf()),
                        run,
                    },
                )))
            }
            Platform::Windows => {
                let windows_store = crate::updater::store::running_from_windows_store();
                Some(LinkHandlers::System(SystemLinkHandlers::new(
                    SystemLinkHandlerOptions {
                        platform,
                        api: Arc::new(WindowsProtocolClient {
                            executable: executable.clone(),
                            run,
                            env: env.clone(),
                        }),
                        windows_store,
                        executable,
                    },
                )))
            }
            Platform::MacOs => Some(LinkHandlers::System(SystemLinkHandlers::new(
                SystemLinkHandlerOptions {
                    platform,
                    api: Arc::new(MacProtocolClient {
                        bundle_id: BUNDLE_ID.into(),
                        run,
                        env: env.clone(),
                    }),
                    windows_store: false,
                    executable,
                },
            ))),
            Platform::Other => None,
        }
    }
}

impl Backend {
    fn link_handlers_for_process(&self) -> Option<LinkHandlers> {
        let environment = self.shared.local_environment();
        LinkHandlers::for_process(
            environment.platform,
            &environment.vars,
            &self.shared.config.home,
            &self.shared.config.paths.user_data,
            !cfg!(debug_assertions),
        )
    }

    /// Whether ssh:// and telnet:// links open in Muxus (`muxus:link-handlers:get`); `None`
    /// where the platform has no registration.
    pub async fn link_handlers(&self) -> ApiResult<Option<LinkHandlerState>> {
        let handlers = self.link_handlers_for_process();
        self.spawn(async move {
            Ok(match handlers {
                Some(handlers) => Some(handlers.state().await),
                None => None,
            })
        })
        .await
    }

    /// Make Muxus the system handler for one scheme (`muxus:link-handlers:register`).
    pub async fn register_link_handler(
        &self,
        scheme: ConnectionLinkScheme,
    ) -> ApiResult<Option<LinkHandlerRegistration>> {
        let handlers = self.link_handlers_for_process();
        self.spawn(async move {
            let Some(handlers) = handlers else {
                return Ok(None);
            };
            let result = handlers.register(scheme).await;
            match &result.error {
                Some(error) => tracing::warn!("{error}"),
                None => tracing::info!(
                    "registered as the {}:// handler{}",
                    scheme.as_str(),
                    if result.opened_system_settings == Some(true) {
                        " (system settings opened)"
                    } else {
                        ""
                    }
                ),
            }
            Ok(Some(result))
        })
        .await
    }
}

#[cfg(test)]
mod tests;
