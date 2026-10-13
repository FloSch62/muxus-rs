//! Local shell profiles: the session profile a local terminal is opened with,
//! the saved launch profiles from the preferences, and the helpers the shell
//! profile editor and launcher use (`client/src/local-shell-profile.ts`).

pub use muxus_core::api::WslDistribution;
pub use muxus_core::protocol::LocalProfile;
use serde::{Deserialize, Serialize};

/// A saved local shell launch profile (`LocalShellProfileConfig` in the client prefs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalShellProfileConfig {
    pub id: String,
    pub name: String,
    pub shell: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub startup_command: String,
}

impl LocalShellProfileConfig {
    /// The session profile this saved profile launches.
    pub fn launch_profile(&self) -> LocalProfile {
        let text = |value: &str| (!value.trim().is_empty()).then(|| value.to_string());
        LocalProfile {
            shell: text(&self.shell),
            args: local_shell_launch_arguments(&self.args),
            cwd: text(&self.cwd),
            startup_command: text(&self.startup_command),
        }
    }
}

const MAX_LOCAL_SHELL_ARGUMENTS: usize = 64;
const MAX_LOCAL_SHELL_ARGUMENT_LENGTH: usize = 4096;

/// A fresh id for a saved local shell profile.
pub fn new_local_shell_profile_id() -> String {
    format!("local-{}", uuid::Uuid::new_v4())
}

/// Preserve blank rows while the arguments field is being edited.
pub fn parse_local_shell_argument_text(value: &str) -> Vec<String> {
    // JavaScript's split limit truncates the list rather than joining the rest.
    value
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .take(MAX_LOCAL_SHELL_ARGUMENTS)
        .map(|argument| truncate_utf16(argument, MAX_LOCAL_SHELL_ARGUMENT_LENGTH))
        .collect()
}

/// The first `limit` UTF-16 code units, never splitting a character.
fn truncate_utf16(text: &str, limit: usize) -> String {
    let mut units = 0;
    let mut end = text.len();
    for (index, ch) in text.char_indices() {
        units += ch.len_utf16();
        if units > limit {
            end = index;
            break;
        }
    }
    text[..end].to_string()
}

/// Empty editor rows describe layout, not arguments passed to the executable.
pub fn local_shell_launch_arguments(arguments: &[String]) -> Option<Vec<String>> {
    let compact: Vec<String> = arguments
        .iter()
        .filter(|argument| !argument.is_empty())
        .cloned()
        .collect();
    (!compact.is_empty()).then_some(compact)
}

/// Launch settings for an installed WSL distribution. `--cd ~` starts in the
/// Linux home directory, as Windows Terminal does, rather than the Windows one.
pub fn wsl_shell_profile(distribution: &WslDistribution) -> LocalShellProfileConfig {
    LocalShellProfileConfig {
        id: format!("wsl:{}", distribution.name),
        name: distribution.name.clone(),
        shell: "wsl.exe".to_string(),
        args: vec![
            "-d".into(),
            distribution.name.clone(),
            "--cd".into(),
            "~".into(),
        ],
        cwd: String::new(),
        startup_command: String::new(),
    }
}

/// Whether a shell launch (a saved profile or an open tab's) starts the named distribution.
pub fn opens_wsl_distribution(shell: Option<&str>, args: Option<&[String]>, name: &str) -> bool {
    let executable = shell
        .map(str::trim)
        .and_then(|shell| shell.rsplit(['\\', '/']).next())
        .map(str::to_lowercase);
    if !matches!(executable.as_deref(), Some("wsl" | "wsl.exe")) {
        return false;
    }
    let args = args.unwrap_or_default();
    let wanted = name.to_lowercase();
    args.iter().enumerate().any(|(index, argument)| {
        (argument == "-d" || argument == "--distribution")
            && args
                .get(index + 1)
                .is_some_and(|next| next.to_lowercase() == wanted)
    })
}

/// Installed distributions no saved profile covers yet, ready to launch.
pub fn wsl_shell_profiles(
    distributions: &[WslDistribution],
    saved: &[LocalShellProfileConfig],
) -> Vec<LocalShellProfileConfig> {
    distributions
        .iter()
        .filter(|distribution| {
            !saved.iter().any(|profile| {
                opens_wsl_distribution(
                    Some(&profile.shell),
                    Some(&profile.args),
                    &distribution.name,
                )
            })
        })
        .map(wsl_shell_profile)
        .collect()
}

/// A copy of an automatically listed distribution profile with its own id,
/// to be added to the saved profiles where it can be customized.
pub fn saved_wsl_shell_profile(profile: &LocalShellProfileConfig) -> LocalShellProfileConfig {
    LocalShellProfileConfig {
        id: new_local_shell_profile_id(),
        ..profile.clone()
    }
}
