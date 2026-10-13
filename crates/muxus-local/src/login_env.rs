//! The login shell environment of a GUI launch
//! (`electron/src/login-shell-environment.ts` and the `shell-env` package it uses).
//!
//! GUI launches do not inherit shell startup files. Keep PATH aligned on Unix
//! and, on macOS, import the selected SSH agent socket for 1Password,
//! Secretive, and other agents configured by the user's login shell.

use std::collections::HashMap;
use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::platform::Platform;

/// Apply the login shell's PATH (and on macOS its `SSH_AUTH_SOCK`) to
/// `environment`. A broken login shell leaves the launch values in place and
/// reports why through `on_error`. The caller applies the result to the
/// process before it starts other threads.
pub fn import_login_shell_environment(
    platform: Platform,
    environment: &mut HashMap<String, String>,
    read_login_environment: impl FnOnce() -> io::Result<HashMap<String, String>>,
    on_error: Option<&mut dyn FnMut(&io::Error)>,
) {
    if platform == Platform::Windows {
        return;
    }
    let login = match read_login_environment() {
        Ok(login) => login,
        Err(err) => {
            // A broken login shell means PATH and SSH_AUTH_SOCK stay at launch
            // values: connections may fail later; leave a trace of why.
            if let Some(on_error) = on_error {
                on_error(&err);
            }
            return;
        }
    };
    if let Some(path) = login.get("PATH").filter(|path| !path.is_empty()) {
        environment.insert("PATH".into(), path.clone());
    }
    if platform == Platform::MacOs
        && let Some(sock) = login.get("SSH_AUTH_SOCK").filter(|sock| !sock.is_empty())
    {
        environment.insert("SSH_AUTH_SOCK".into(), sock.clone());
    }
}

const DELIMITER: &str = "_SHELL_ENV_DELIMITER_";
/// Use the command builtin to avoid shell aliases or functions named `env`.
const ENV_COMMAND: &str =
    r#"echo -n "_SHELL_ENV_DELIMITER_"; command env; echo -n "_SHELL_ENV_DELIMITER_"; exit"#;
/// A login shell that never finishes must not hold up the launch forever.
const SHELL_TIMEOUT: Duration = Duration::from_secs(10);

/// The user's login shell: the passwd entry first, then `$SHELL`.
pub fn detect_default_shell(platform: Platform) -> String {
    if platform == Platform::Windows {
        return std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
    }
    if let Some(shell) = passwd_shell() {
        return shell;
    }
    let fallback = if platform == Platform::MacOs {
        "/bin/zsh"
    } else {
        "/bin/sh"
    };
    std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| fallback.into())
}

#[cfg(unix)]
fn passwd_shell() -> Option<String> {
    use std::ffi::CStr;
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to live, correctly sized storage owned here.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() || entry.pw_shell.is_null() {
        return None;
    }
    // SAFETY: pw_shell points into `buffer`, NUL-terminated by getpwuid_r.
    let shell = unsafe { CStr::from_ptr(entry.pw_shell) }
        .to_string_lossy()
        .into_owned();
    (!shell.is_empty()).then_some(shell)
}

#[cfg(not(unix))]
fn passwd_shell() -> Option<String> {
    None
}

/// The environment an interactive login shell ends up with (`shellEnvSync`).
/// With an explicit `shell` its failure is returned; otherwise zsh and bash
/// are tried in turn and the process environment is the last resort.
pub fn shell_env(shell: Option<&str>) -> io::Result<HashMap<String, String>> {
    if cfg!(windows) {
        return Ok(std::env::vars().collect());
    }
    let default_shell = detect_default_shell(Platform::current());
    match run_login_shell(shell.unwrap_or(&default_shell)) {
        Ok(env) => Ok(env),
        Err(err) if shell.is_some() => Err(err),
        Err(_) => {
            // Fallback POSIX shells for a default shell that failed (Nushell and the like).
            for fallback in ["/bin/zsh", "/bin/bash"]
                .into_iter()
                .filter(|candidate| *candidate != default_shell)
            {
                if let Ok(env) = run_login_shell(fallback) {
                    return Ok(env);
                }
            }
            Ok(std::env::vars().collect())
        }
    }
}

fn run_login_shell(shell: &str) -> io::Result<HashMap<String, String>> {
    let mut child = Command::new(shell)
        .args(["-ilc", ENV_COMMAND])
        // Disables Oh My Zsh auto-update thing that can block the process.
        .env("DISABLE_AUTO_UPDATE", "true")
        // Prevents the oh-my-zsh tmux plugin from auto-starting.
        .env("ZSH_TMUX_AUTOSTARTED", "true")
        .env("ZSH_TMUX_AUTOSTART", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.read_to_end(&mut output);
        output
    });
    let deadline = Instant::now() + SHELL_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{shell} did not finish"),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(io::Error::other(format!("{shell} exited with {status}")));
    }
    Ok(parse_shell_env(&String::from_utf8_lossy(&output)))
}

static ANSI: LazyLock<Regex> = LazyLock::new(|| {
    // ansi-regex 6: OSC strings, then CSI and related sequences.
    Regex::new(concat!(
        r"(?:(?:\x1b\]|\x{9d})[^\x07\x1b\x{9c}\x{9d}]*(?:\x07|\x1b\\|\x{9c}))",
        r"|[\x1b\x{9b}][\[\]()#;?]*(?:[0-9]{1,4}(?:[;:][0-9]{0,4})*)?[0-9A-PR-TZcf-nq-uy=><~]",
    ))
    .expect("valid regex")
});

/// Remove terminal escape sequences, as `strip-ansi` does.
pub fn strip_ansi(text: &str) -> String {
    if !text.contains('\x1b') && !text.contains('\u{9b}') {
        return text.to_string();
    }
    ANSI.replace_all(text, "").into_owned()
}

/// Read `env` output printed between the two delimiters.
pub fn parse_shell_env(output: &str) -> HashMap<String, String> {
    let section = output.split(DELIMITER).nth(1).unwrap_or("");
    strip_ansi(section)
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(|line| match line.split_once('=') {
            Some((key, value)) => (key.to_string(), value.to_string()),
            None => (line.to_string(), String::new()),
        })
        .collect()
}
