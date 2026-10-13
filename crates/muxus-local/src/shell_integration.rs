//! Shell integration for local terminals: rc shims that report the command
//! lifecycle via OSC 133 (FinalTerm): `D;<exit code>` when a command finishes,
//! `C` when one starts. The terminal turns them into failed-command marks in
//! the scrollbar, the same mechanism VS Code uses. The shims source the user's
//! own startup files first, so prompts and plugins are untouched.
//!
//! The generated file contents are byte-for-byte those of the TypeScript
//! implementation (`server/src/local/shell-integration.ts`). The shim texts live in
//! `muxus_core::shell_integration`, shared with the SSH integration.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::platform::Platform;

pub use muxus_core::shell_integration::{
    LOCAL_BASH_INIT as BASH_INIT, SHELL_CWD_REPORT, ZSHENV, ZSHRC,
};

const FILES: [(&str, &str); 3] = [
    ("zsh/.zshenv", ZSHENV),
    ("zsh/.zshrc", ZSHRC),
    ("bash-init.bash", BASH_INIT),
];

/// Write the shim files under `root` (idempotent).
pub fn write_integration_files(root: &Path) -> io::Result<()> {
    let zsh = root.join("zsh");
    create_private_dir(&zsh)?;
    for (rel, content) in FILES {
        let path = root.join(rel);
        fs::write(&path, content)?;
        set_mode(&path, 0o644)?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Where the shim files live for an environment: `$XDG_CACHE_HOME/muxus/shell-integration`,
/// or `~/.cache/muxus/shell-integration`.
pub fn integration_dir(env: &HashMap<String, String>, home: &Path) -> PathBuf {
    let base = env
        .get("XDG_CACHE_HOME")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"));
    base.join("muxus").join("shell-integration")
}

/// Shim directory for this process (written once per process); `None` when
/// it could not be written.
pub fn integration_root() -> Option<PathBuf> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let env: HashMap<String, String> = std::env::vars().collect();
        let home = crate::pty::home_dir(&env)?;
        let root = integration_dir(&env, &home);
        write_integration_files(&root).ok()?;
        Some(root)
    })
    .clone()
}

/// Spawn arguments and environment that switch integration on for one shell.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellIntegration {
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

// cmd.exe expands $E to ESC and $P to its current drive and directory every
// time it draws a prompt. Prefixing the user's prompt with this invisible OSC
// property gives Windows local terminals the same live cwd reports as the
// bash/zsh shims without replacing the visible prompt.
const CMD_CWD_PROMPT_PREFIX: &str = r"$E]133;P;CwdRaw=$P$E\";

/// The last path component, split the way the platform splits paths.
fn shell_basename(shell: &str, platform: Platform) -> &str {
    let separators: &[char] = if platform == Platform::Windows {
        &['\\', '/']
    } else {
        &['/']
    };
    let trimmed = shell.trim_end_matches(separators);
    trimmed.rsplit(separators).next().unwrap_or(trimmed)
}

/// Spawn arguments and env that switch integration on for a given shell.
/// Unrecognized shells spawn untouched (fish emits OSC 133 on its own).
pub fn shell_integration(
    shell: &str,
    base_env: &HashMap<String, String>,
    root: Option<&Path>,
    platform: Platform,
) -> ShellIntegration {
    let none = ShellIntegration::default();
    if platform == Platform::Windows {
        let name = shell_basename(shell, platform).to_lowercase();
        if name != "cmd" && name != "cmd.exe" {
            return none;
        }
        let prompt = base_env
            .get("PROMPT")
            .filter(|prompt| !prompt.is_empty())
            .map(String::as_str)
            .unwrap_or("$P$G");
        return ShellIntegration {
            args: Vec::new(),
            env: HashMap::from([(
                "PROMPT".to_string(),
                format!("{CMD_CWD_PROMPT_PREFIX}{prompt}"),
            )]),
        };
    }
    let Some(root) = root else { return none };
    match shell_basename(shell, platform) {
        "zsh" => {
            let mut env = HashMap::from([(
                "ZDOTDIR".to_string(),
                root.join("zsh").to_string_lossy().into_owned(),
            )]);
            if let Some(user) = base_env
                .get("ZDOTDIR")
                .map(|value| value.trim())
                .filter(|v| !v.is_empty())
            {
                env.insert("MUXUS_USER_ZDOTDIR".to_string(), user.to_string());
            }
            ShellIntegration {
                args: Vec::new(),
                env,
            }
        }
        "bash" => ShellIntegration {
            args: vec![
                "--init-file".to_string(),
                root.join("bash-init.bash").to_string_lossy().into_owned(),
            ],
            env: HashMap::new(),
        },
        _ => none,
    }
}
