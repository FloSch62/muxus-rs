//! OSC 133 shell integration for remote bash/zsh hosts: probe the login shell, install the rc
//! shims once per content hash under `~/.cache/muxus/shell-integration/<hash>` over SFTP, and
//! start the shell through them. Every other host gets a plain shell.

pub mod scripts;

use sha2::{Digest, Sha256};

pub use scripts::{BASH_INIT, REMOTE_ZSHRC, SHELL_PROBE, ZPROFILE, ZSHENV};

pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub const MAX_PROBE_OUTPUT: usize = 8 * 1024;
const INSTALL_COMPLETE: &str = ".complete";

/// A content-addressed directory makes the installed scripts immutable. Once the completion
/// marker exists, future connections need only one SFTP stat instead of re-checking every parent
/// and rewriting every file over the WAN.
pub fn integration_version() -> String {
    let mut hash = Sha256::new();
    hash.update(ZSHENV);
    hash.update(ZPROFILE);
    hash.update(REMOTE_ZSHRC);
    hash.update(BASH_INIT);
    hex::encode(hash.finalize())[..12].to_string()
}

/// A login shell Muxus can integrate with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportedShell {
    pub path: String,
    pub kind: ShellKind,
    pub home: String,
    pub zdotdir: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
}

/// The value of a `NAME=value` marker line (`^NAME=(…)$`, multiline).
fn marker<'a>(output: &'a str, name: &str) -> Option<&'a str> {
    output
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .find_map(|line| {
            line.strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
        })
}

pub fn parse_shell_probe(output: &str) -> Option<SupportedShell> {
    let shell_path = marker(output, "__MUXUS_SHELL__")
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    if shell_path.contains('\0') {
        return None;
    }
    let name = shell_path.rsplit('/').next().unwrap_or(shell_path);
    let kind = match name {
        "bash" => ShellKind::Bash,
        "zsh" => ShellKind::Zsh,
        _ => return None,
    };
    let home = marker(output, "__MUXUS_HOME__")
        .filter(|h| h.starts_with('/'))
        .map(str::trim)?;
    if home.is_empty() || home.contains('\0') {
        return None;
    }
    let zdotdir = marker(output, "__MUXUS_ZDOTDIR__")
        .map(str::trim)
        .filter(|z| !z.is_empty());
    Some(SupportedShell {
        path: shell_path.to_string(),
        kind,
        home: home.to_string(),
        zdotdir: zdotdir.map(str::to_string),
    })
}

pub fn quote_shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn posix_join(parts: &[&str]) -> String {
    let mut out = String::new();
    for part in parts {
        if out.is_empty() {
            out.push_str(part);
        } else if out.ends_with('/') {
            out.push_str(part.trim_start_matches('/'));
        } else {
            out.push('/');
            out.push_str(part.trim_start_matches('/'));
        }
    }
    out
}

/// The exec command for an integrated shell. `cwd` is the directory a reconnecting tab was last
/// in; the startup files change into it.
pub fn remote_shell_command(shell: &SupportedShell, root: &str, cwd: Option<&str>) -> String {
    let executable = quote_shell_word(&shell.path);
    let restore_cwd = match cwd {
        Some(cwd) if cwd.starts_with('/') && !cwd.contains('\0') => {
            format!("export MUXUS_RESTORE_CWD={}; ", quote_shell_word(cwd))
        }
        _ => String::new(),
    };
    match shell.kind {
        ShellKind::Bash => format!(
            "{restore_cwd}exec {executable} --noprofile --rcfile {} -i",
            quote_shell_word(&posix_join(&[root, "bash-init.bash"]))
        ),
        ShellKind::Zsh => {
            let user_zdotdir = match &shell.zdotdir {
                Some(dir) => format!("export MUXUS_USER_ZDOTDIR={}; ", quote_shell_word(dir)),
                None => "unset MUXUS_USER_ZDOTDIR; ".to_string(),
            };
            format!(
                "{restore_cwd}{user_zdotdir}export ZDOTDIR={}; exec {executable} -l",
                quote_shell_word(&posix_join(&[root, "zsh"]))
            )
        }
    }
}

/// What a remote path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteFileKind {
    File,
    Directory,
    Other,
}

/// The server dropped the whole SSH transport while Muxus was attempting the optional Unix
/// shell-integration setup or transitioning to its plain-shell fallback. The connection manager
/// uses this narrow signal to redial once as a plain console session; ordinary channel rejections
/// remain ordinary errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("SSH transport was lost during remote shell integration setup.")]
pub struct RemoteShellTransportLost {
    pub transport_error: Option<String>,
}

/// Failure to open a remote shell.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenShellError {
    #[error(transparent)]
    TransportLost(#[from] RemoteShellTransportLost),
    #[error("{0}")]
    Channel(String),
}

/// What opening a shell needs from the SSH connection; implemented over a live transport and by
/// test stubs.
pub trait RemoteShellHost: Send + Sync {
    type Channel: Send;
    type Sftp: RemoteShellSftp;

    /// Runs `command` without a tty and returns its output (bounded by size and time).
    fn probe(&self, command: &str) -> impl Future<Output = Result<String, String>> + Send;
    /// Opens (or reuses) the connection's SFTP session.
    fn sftp(&self) -> impl Future<Output = Result<Self::Sftp, String>> + Send;
    /// Starts `command` with the session's tty, environment and X11 settings.
    fn open_exec(
        &self,
        command: &str,
    ) -> impl Future<Output = Result<Self::Channel, String>> + Send;
    /// Starts a plain login shell with the session's tty, environment and X11 settings.
    fn open_shell(&self) -> impl Future<Output = Result<Self::Channel, String>> + Send;
    /// Whether the transport has been lost since the attempt began, and why.
    fn transport_lost(&self) -> Option<Option<String>>;
}

/// The SFTP operations the install uses.
pub trait RemoteShellSftp: Send + Sync {
    fn stat(&self, path: &str) -> impl Future<Output = Result<RemoteFileKind, String>> + Send;
    fn mkdir(&self, path: &str, mode: u32) -> impl Future<Output = Result<(), String>> + Send;
    fn write_file(
        &self,
        path: &str,
        content: &str,
        mode: u32,
    ) -> impl Future<Output = Result<(), String>> + Send;
}

fn is_disconnected_error(message: &str) -> bool {
    message.starts_with("Not connected")
        || message.starts_with("Connection lost")
        || message.contains("Disconnected")
}

/// Open a remote shell, adding OSC 133 integration for supported zsh/bash hosts and falling back
/// to a normal shell everywhere else. Transport-loss observation spans both phases: some console
/// servers close the probe channel first and only disconnect the SSH transport while the plain
/// shell opens.
pub async fn open_remote_shell<H: RemoteShellHost>(
    host: &H,
    cwd: Option<&str>,
) -> Result<H::Channel, OpenShellError> {
    if let Some(channel) = try_open_integrated_remote_shell(host, cwd).await {
        return Ok(channel);
    }
    if let Some(error) = host.transport_lost() {
        return Err(RemoteShellTransportLost {
            transport_error: error,
        }
        .into());
    }
    match host.open_shell().await {
        Ok(channel) => Ok(channel),
        Err(error) => {
            if let Some(transport_error) = host.transport_lost() {
                return Err(RemoteShellTransportLost {
                    transport_error: transport_error.or(Some(error)),
                }
                .into());
            }
            if is_disconnected_error(&error) {
                return Err(RemoteShellTransportLost {
                    transport_error: Some(error),
                }
                .into());
            }
            Err(OpenShellError::Channel(error))
        }
    }
}

async fn try_open_integrated_remote_shell<H: RemoteShellHost>(
    host: &H,
    cwd: Option<&str>,
) -> Option<H::Channel> {
    // Do not speculatively open SFTP against console appliances. Many expose only a plain shell
    // channel and some disconnect the whole transport when an unsupported subsystem is
    // requested.
    let output = host.probe(SHELL_PROBE).await.ok()?;
    let shell = parse_shell_probe(&output)?;
    let sftp = host.sftp().await.ok()?;
    let root = install_integration(&sftp, &shell).await.ok()?;
    host.open_exec(&remote_shell_command(&shell, &root, cwd))
        .await
        .ok()
}

async fn install_integration<S: RemoteShellSftp>(
    sftp: &S,
    shell: &SupportedShell,
) -> Result<String, String> {
    let version = integration_version();
    let root = posix_join(&[
        &shell.home,
        ".cache",
        "muxus",
        "shell-integration",
        &version,
    ]);
    let complete_path = posix_join(&[&root, INSTALL_COMPLETE]);
    if sftp.stat(&complete_path).await == Ok(RemoteFileKind::File) {
        return Ok(root);
    }
    let zsh_dir = posix_join(&[&root, "zsh"]);
    let directories: Vec<&str> = match shell.kind {
        ShellKind::Zsh => vec![&root, &zsh_dir],
        ShellKind::Bash => vec![&root],
    };
    for directory in directories {
        ensure_directory(sftp, directory).await?;
    }
    let files: Vec<(String, &str)> = match shell.kind {
        ShellKind::Zsh => vec![
            (posix_join(&[&root, "zsh", ".zshenv"]), ZSHENV),
            (posix_join(&[&root, "zsh", ".zprofile"]), ZPROFILE),
            (posix_join(&[&root, "zsh", ".zshrc"]), REMOTE_ZSHRC),
        ],
        ShellKind::Bash => vec![(posix_join(&[&root, "bash-init.bash"]), BASH_INIT)],
    };
    for (path, content) in &files {
        sftp.write_file(path, content, 0o600).await?;
    }
    // Written last so interrupted installs are repaired on the next attempt.
    sftp.write_file(&complete_path, &version, 0o600).await?;
    Ok(root)
}

async fn ensure_directory<S: RemoteShellSftp>(sftp: &S, directory: &str) -> Result<(), String> {
    let mut current = String::from("/");
    for part in directory.split('/').filter(|p| !p.is_empty() && *p != ".") {
        current = posix_join(&[&current, part]);
        match sftp.stat(&current).await {
            Ok(RemoteFileKind::Directory) => continue,
            Ok(_) => return Err(format!("{current} is not a directory")),
            Err(_) => {}
        }
        if let Err(error) = sftp.mkdir(&current, 0o700).await {
            // Another terminal may have created it between stat and mkdir.
            if sftp.stat(&current).await != Ok(RemoteFileKind::Directory) {
                return Err(error);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
