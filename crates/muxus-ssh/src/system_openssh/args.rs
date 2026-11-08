//! The `ssh` command lines of a system OpenSSH session (`system-openssh.ts`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use muxus_core::api::ForwardType;
use muxus_core::environments::environment_command_line;
use muxus_core::platform::Platform;
use muxus_core::protocol::{RequestTty, SshProfile};

use crate::config::parse_host_spec;
use crate::connection::profile::uses_config;

/// Keeps the shared connection alive through a crash; a normal close ends it at once.
pub const CONTROL_PERSIST_SECONDS: u32 = 60;
/// A shell prints the sum; a device CLI that echoes what it was sent does not.
pub const COMMAND_PROBE: &str = "echo \"muxus$((40+2))\"";
pub const COMMAND_PROBE_ANSWER: &str = "muxus42";

/// The `ssh` program: the first on PATH, then the platform's usual place (`findSshBinary`).
pub fn find_ssh_binary(env: &HashMap<String, String>, platform: Platform) -> Option<PathBuf> {
    let windows = platform == Platform::Windows;
    let (separator, delimiter) = if windows { ('\\', ';') } else { ('/', ':') };
    let join = |directory: &str, name: &str| {
        let trimmed = directory.trim_end_matches(['/', '\\']);
        format!("{trimmed}{separator}{name}")
    };
    // Windows spells it Path, and a copied environment is case-sensitive.
    let path = env
        .iter()
        .find(|(key, _)| {
            if windows {
                key.eq_ignore_ascii_case("PATH")
            } else {
                key.as_str() == "PATH"
            }
        })
        .map(|(_, value)| value.as_str())
        .unwrap_or("");
    let program = if windows { "ssh.exe" } else { "ssh" };
    let mut candidates: Vec<String> = path
        .split(delimiter)
        .filter(|directory| !directory.is_empty())
        .map(|directory| join(directory, program))
        .collect();
    if windows {
        if let Some(root) = env.get("SystemRoot") {
            candidates.push(join(root, "System32\\OpenSSH\\ssh.exe"));
        }
    } else {
        candidates.push("/usr/bin/ssh".into());
    }
    candidates
        .into_iter()
        .map(PathBuf::from)
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

fn option(keyword: &str, value: impl std::fmt::Display) -> [String; 2] {
    ["-o".into(), format!("{keyword}={value}")]
}

/// Quote a value for options ssh splits into words, such as paths with spaces.
fn quoted_option(keyword: &str, value: &str) -> [String; 2] {
    let needs_quotes = value
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\\');
    if needs_quotes {
        let escaped: String = value
            .chars()
            .flat_map(|c| {
                let escape = (c == '"' || c == '\\').then_some('\\');
                escape.into_iter().chain(std::iter::once(c))
            })
            .collect();
        option(keyword, format!("\"{escaped}\""))
    } else {
        option(keyword, value)
    }
}

/// The destination `ssh` is given, with the user and port that override its config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshDestination {
    pub options: Vec<String>,
    pub destination: String,
}

/// `sshDestination`: a saved host's target may carry its own user and port; an alias is passed
/// as is.
pub fn ssh_destination(profile: &SshProfile) -> SshDestination {
    let (host, spec_user, spec_port) = if uses_config(profile) {
        (profile.target.clone(), None, None)
    } else {
        let spec = parse_host_spec(&profile.target);
        (spec.host, spec.user, spec.port)
    };
    let user = profile.user.clone().or(spec_user);
    let port = profile.port.or(spec_port);
    let mut options = Vec::new();
    if let Some(user) = user.filter(|user| !user.is_empty()) {
        options.extend(["-l".to_string(), user]);
    }
    if let Some(port) = port.filter(|port| *port != 0) {
        options.extend(["-p".to_string(), port.to_string()]);
    }
    SshDestination {
        options,
        destination: host,
    }
}

fn forward_target(host: Option<&str>, port: Option<u16>) -> String {
    let name = match host {
        Some(host) if host.contains(':') => format!("[{host}]"),
        Some(host) => host.to_string(),
        None => "localhost".into(),
    };
    format!("{name}:{}", port.unwrap_or(0))
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn request_tty(value: RequestTty) -> &'static str {
    match value {
        RequestTty::No => "no",
        RequestTty::Yes => "yes",
        RequestTty::Force => "force",
        RequestTty::Auto => "auto",
    }
}

/// Options for a saved host's own settings (`savedHostOptions`). It has no ssh_config block, so
/// they go on the command line, where they outrank anything a `Host *` block says. GSSAPI is
/// requested too, since a saved host has nowhere else to ask for it.
pub fn saved_host_options(profile: &SshProfile) -> Vec<String> {
    if uses_config(profile) {
        return Vec::new();
    }
    let mut args: Vec<String> = Vec::new();
    for file in profile.identity_files.iter().flatten() {
        args.extend(["-i".to_string(), file.clone()]);
    }
    for file in profile.certificate_files.iter().flatten() {
        args.extend(quoted_option("CertificateFile", file));
    }
    if let Some(only) = profile.identities_only {
        args.extend(option("IdentitiesOnly", yes_no(only)));
    }
    if let Some(agent) = profile.identity_agent.as_deref().filter(|a| !a.is_empty()) {
        args.extend(quoted_option("IdentityAgent", agent));
    }
    if let Some(forward) = profile.forward_agent {
        args.extend(option("ForwardAgent", yes_no(forward)));
    }
    if let Some(forward) = profile.forward_x11 {
        args.extend(option("ForwardX11", yes_no(forward)));
    }
    if let Some(hops) = profile.proxy_jump.as_ref().filter(|hops| !hops.is_empty()) {
        args.extend(["-J".to_string(), hops.join(",")]);
    }
    // These two take the rest of the line, so quotes would become part of the command.
    if let Some(command) = profile.proxy_command.as_deref().filter(|c| !c.is_empty()) {
        args.extend(option("ProxyCommand", command));
    }
    if let Some(command) = profile.remote_command.as_deref().filter(|c| !c.is_empty()) {
        args.extend(option("RemoteCommand", command.replace('%', "%%")));
    }
    if let Some(tty) = profile.request_tty {
        args.extend(option("RequestTTY", request_tty(tty)));
    }
    if let Some(seconds) = profile.keepalive_interval_seconds.filter(|s| *s > 0) {
        args.extend(option("ServerAliveInterval", seconds));
    }
    for forward in profile.forwards.iter().flatten() {
        match forward.kind {
            ForwardType::Dynamic => args.extend(["-D".to_string(), forward.bind_port.to_string()]),
            kind => args.extend([
                if kind == ForwardType::Local {
                    "-L"
                } else {
                    "-R"
                }
                .to_string(),
                format!(
                    "{}:{}",
                    forward.bind_port,
                    forward_target(forward.target_host.as_deref(), forward.target_port)
                ),
            ]),
        }
    }
    if profile.password_only == Some(true) {
        args.extend(option("PubkeyAuthentication", "no"));
        args.extend(option(
            "PreferredAuthentications",
            "keyboard-interactive,password",
        ));
    } else {
        args.extend(option("GSSAPIAuthentication", "yes"));
    }
    args
}

/// Everything but the program name for the terminal's `ssh` (`terminalSshArgs`).
pub fn terminal_ssh_args(profile: &SshProfile, control_path: Option<&Path>) -> Vec<String> {
    let SshDestination {
        options,
        destination,
    } = ssh_destination(profile);
    let mut args: Vec<String> = Vec::new();
    // Pasted text must never be read as an `~.` disconnect.
    args.extend(option("EscapeChar", "none"));
    match control_path {
        Some(path) => {
            args.extend(option("ControlMaster", "yes"));
            args.extend(option("ControlPath", path.display()));
            // Backgrounds the master after login, so the terminal's `ssh` ends with the remote
            // shell instead of waiting for the file browser to leave.
            args.extend(option("ControlPersist", CONTROL_PERSIST_SECONDS));
        }
        None => {
            // A config shared with another computer must not turn on what this one lacks.
            args.extend(option("ControlMaster", "no"));
            args.extend(option("ControlPath", "none"));
        }
    }
    let environment = profile
        .environment
        .as_ref()
        .and_then(|target| environment_command_line(target).ok());
    if environment.is_some() {
        // The first value wins, so these go before anything the host's settings add.
        args.extend(option("RemoteCommand", "none"));
        args.extend(option("RequestTTY", "force"));
    }
    args.extend(saved_host_options(profile));
    args.extend(options);
    args.push("--".into());
    args.push(destination);
    args.extend(environment);
    args
}

/// Options of an `ssh` that runs one background job rather than the user's session.
fn background_job_options() -> Vec<String> {
    let mut args = vec!["-T".to_string()];
    args.extend(option("BatchMode", "yes"));
    args.extend(option("LogLevel", "ERROR"));
    // A config's values for these belong to the terminal, and would clash with a command.
    args.extend(option("RemoteCommand", "none"));
    args.extend(option("RequestTTY", "no"));
    args.extend(option("ClearAllForwardings", "yes"));
    args.extend(option("ForwardAgent", "no"));
    args.extend(option("ForwardX11", "no"));
    args.extend(option("PermitLocalCommand", "no"));
    args
}

fn shared_login_args(profile: &SshProfile, control_path: &Path, command: &[&str]) -> Vec<String> {
    let SshDestination {
        options,
        destination,
    } = ssh_destination(profile);
    let mut args = background_job_options();
    args.extend(option("ControlMaster", "no"));
    args.extend(option("ControlPath", control_path.display()));
    args.extend(options);
    args.push("--".into());
    args.push(destination);
    args.extend(command.iter().map(|part| part.to_string()));
    args
}

/// Everything but the program name for the `ssh` that carries SFTP over the shared login.
pub fn sftp_ssh_args(profile: &SshProfile, control_path: &Path) -> Vec<String> {
    let mut args = vec!["-s".to_string()];
    args.extend(shared_login_args(profile, control_path, &["sftp"]));
    args
}

/// Everything but the program name for the `ssh` that feeds a script to the host's sh.
pub fn script_ssh_args(profile: &SshProfile, control_path: &Path) -> Vec<String> {
    shared_login_args(profile, control_path, &["sh -s"])
}

/// Everything but the program name for the `ssh` that asks whether the host runs commands.
/// This one logs in on its own connection: some devices drop the whole connection when asked
/// to exec (issue #267), which must not take the terminal with it. BatchMode means a host that
/// needs a person to log in just answers no.
pub fn command_probe_args(profile: &SshProfile) -> Vec<String> {
    let SshDestination {
        options,
        destination,
    } = ssh_destination(profile);
    let mut args = background_job_options();
    args.extend(option("ControlMaster", "no"));
    args.extend(option("ControlPath", "none"));
    args.extend(option("ConnectTimeout", 10));
    args.extend(saved_host_options(profile));
    args.extend(options);
    args.push("--".into());
    args.push(destination);
    args.push(COMMAND_PROBE.into());
    args
}

/// The `ssh` that ends the shared connection (`ssh -O exit`).
pub fn control_exit_args(profile: &SshProfile, control_path: &Path) -> Vec<String> {
    let SshDestination {
        options,
        destination,
    } = ssh_destination(profile);
    let mut args: Vec<String> = Vec::new();
    args.extend(option("ControlPath", control_path.display()));
    args.extend(option("LogLevel", "ERROR"));
    args.extend(["-O".to_string(), "exit".to_string()]);
    args.extend(options);
    args.push("--".into());
    args.push(destination);
    args
}
