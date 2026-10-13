//! Local terminal sessions on a pseudo-terminal.
//!
//! Port of `server/src/local/pty-manager.ts` plus the local branch of
//! `server/src/ws/terminal-socket.ts` (startup command, exit reporting).

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::Duration;

use muxus_core::stream::{
    TerminalSize, TransportChannels, TransportEvent, TransportFeedback, TransportInput,
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use regex::Regex;

use crate::platform::Platform;
use crate::profile::LocalProfile;
use crate::shell_integration::{integration_root, shell_integration};

/// The terminal type local terminals (and SSH sessions) announce.
pub const DEFAULT_TERM: &str = "xterm-256color";

/// How much recent output the startup-command prompt detector keeps.
const STARTUP_OUTPUT_WINDOW: usize = 8192;
/// Queue depth of the transport channels a local session hands out.
const CHANNEL_CAPACITY: usize = 256;
/// After the shell exits, how long trailing output may take to drain before
/// the exit is reported anyway (a background job can hold the pty open).
const EXIT_DRAIN_GRACE: Duration = Duration::from_secs(1);

/// The environment a local terminal is launched from: the variables it
/// inherits, where the shell integration shims live, and the platform.
#[derive(Debug, Clone)]
pub struct LocalEnvironment {
    pub vars: HashMap<String, String>,
    /// Shim directory from [`integration_root`]; `None` disables integration.
    pub integration_root: Option<PathBuf>,
    pub platform: Platform,
}

impl LocalEnvironment {
    /// This process's environment, with the shims written to the cache directory.
    pub fn current() -> Self {
        Self {
            vars: std::env::vars().collect(),
            integration_root: integration_root(),
            platform: Platform::current(),
        }
    }
}

/// The user's home directory for an environment: `$HOME` (`%USERPROFILE%` on
/// Windows) first, as Node's `os.homedir()` does.
pub fn home_dir(env: &HashMap<String, String>) -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env.get(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
}

/// The login shell a local terminal spawns when the profile has no override.
pub fn default_shell(env: &HashMap<String, String>, platform: Platform) -> String {
    default_shell_with(env, platform, |path| Path::new(path).exists())
}

fn default_shell_with(
    env: &HashMap<String, String>,
    platform: Platform,
    exists: impl Fn(&str) -> bool,
) -> String {
    if platform == Platform::Windows {
        return env
            .get("COMSPEC")
            .cloned()
            .unwrap_or_else(|| "powershell.exe".to_string());
    }
    if let Some(shell) = env
        .get("SHELL")
        .filter(|shell| !shell.is_empty() && exists(shell))
    {
        return shell.clone();
    }
    if exists("/bin/zsh") {
        "/bin/zsh"
    } else {
        "/bin/bash"
    }
    .to_string()
}

/// Vars describing whatever terminal launched Muxus itself: wrong and
/// misleading inside PTYs that Muxus owns.
static HOST_TERMINAL_ENV: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(TERMINFO|TERM_PROGRAM|TERM_PROGRAM_VERSION|VTE_VERSION|WT_SESSION|WT_PROFILE_ID|KONSOLE_VERSION|KONSOLE_DBUS_\w+|ITERM_SESSION_ID|GNOME_TERMINAL_\w+|KITTY_\w+|WEZTERM_\w+|ALACRITTY_\w+)$",
    )
    .expect("valid regex")
});

/// Whether an inherited variable describes the terminal that launched Muxus.
pub fn is_host_terminal_env(key: &str) -> bool {
    HOST_TERMINAL_ENV.is_match(key)
}

/// Shell integration arguments first, then the profile's own.
pub fn local_pty_args(profile: &LocalProfile, integration_args: &[String]) -> Vec<String> {
    integration_args
        .iter()
        .chain(profile.args.iter().flatten())
        .cloned()
        .collect()
}

/// Normalize a saved startup action to terminal input. A PTY Enter is CR on
/// every supported platform; embedded newlines become separate Enter presses.
pub fn local_startup_input(command: Option<&str>) -> Option<String> {
    let trimmed = command?.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!(
        "{}\r",
        trimmed.replace("\r\n", "\r").replace('\n', "\r")
    ))
}

static SHELL_PROMPT_OSC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b\](?:133|633);A(?:;[^\x07\x1b]*)?(?:\x07|\x1b\\)").expect("valid regex")
});
static ANSI_OSC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").expect("valid regex"));
static ANSI_CSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").expect("valid regex"));
static COMMON_PROMPT_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:[$#>]|❯|➜)\s*$").expect("valid regex"));

/// Detect the first interactive prompt without feeding startup text into shell
/// initialization. OSC 133/633 is authoritative; the visible fallback covers
/// default cmd, PowerShell, POSIX, fish, and common themed prompts.
pub fn local_shell_prompt_ready(output: &str) -> bool {
    if SHELL_PROMPT_OSC.is_match(output) {
        return true;
    }
    let without_osc = ANSI_OSC.replace_all(output, "");
    let visible = ANSI_CSI.replace_all(&without_osc, "");
    let line = visible.rsplit(['\r', '\n']).next().unwrap_or("");
    COMMON_PROMPT_END.is_match(line)
}

/// Everything needed to start a local shell, resolved from a profile and an
/// environment. Kept separate from the spawn so it can be inspected and tested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPtyPlan {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The complete environment of the shell.
    pub env: HashMap<String, String>,
}

/// Resolve the shell, arguments, working directory and environment of a local terminal.
pub fn plan_local_pty(profile: &LocalProfile, environment: &LocalEnvironment) -> LocalPtyPlan {
    let program = profile
        .shell
        .as_deref()
        .map(str::trim)
        .filter(|shell| !shell.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default_shell(&environment.vars, environment.platform));
    let integration = shell_integration(
        &program,
        &environment.vars,
        environment.integration_root.as_deref(),
        environment.platform,
    );
    let args = local_pty_args(profile, &integration.args);
    let mut env: HashMap<String, String> = environment
        .vars
        .iter()
        .filter(|(key, _)| !is_host_terminal_env(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    env.extend(integration.env);
    env.insert("TERM".into(), DEFAULT_TERM.into());
    // Muxus renders 24-bit color; advertise it independently of terminfo.
    env.insert("COLORTERM".into(), "truecolor".into());
    env.insert("TERM_PROGRAM".into(), "muxus".into());
    let cwd = profile
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir(&environment.vars))
        .unwrap_or_else(|| PathBuf::from("/"));
    LocalPtyPlan {
        program,
        args,
        cwd,
        env,
    }
}

/// A running local terminal.
#[derive(Debug)]
pub struct LocalPty {
    /// The terminal-facing byte stream. Data, Resize and Close are honoured;
    /// Break has no meaning on a local pty and is ignored.
    pub channels: TransportChannels,
    /// The shell that was started.
    pub shell: String,
    pub process_id: Option<u32>,
}

fn pty_size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows.max(1),
        cols: size.cols.max(1),
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}

/// Start a local shell on a new pseudo-terminal.
///
/// The returned channels follow `muxus_core::stream`: output arrives as
/// `Data`, the shell's end as `Exit` (code and signal). After the terminal
/// sends `Close` (or drops its input sender) the shell is hung up and no
/// further events arrive. The profile's startup command is typed once the
/// first interactive prompt shows. Runs on its own threads; no async runtime
/// is required.
pub fn spawn_local_pty(
    profile: &LocalProfile,
    size: TerminalSize,
    environment: &LocalEnvironment,
) -> io::Result<LocalPty> {
    let plan = plan_local_pty(profile, environment);
    let pair = native_pty_system()
        .openpty(pty_size(size))
        .map_err(io::Error::other)?;

    let mut command = CommandBuilder::new(&plan.program);
    command.args(&plan.args);
    command.cwd(&plan.cwd);
    command.env_clear();
    for (key, value) in &plan.env {
        command.env(key, value);
    }
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(io::Error::other)?;
    // The master only sees end-of-file once every slave handle is gone.
    drop(pair.slave);
    let process_id = child.process_id();
    let mut killer = child.clone_killer();
    let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
    let writer = pair.master.take_writer().map_err(io::Error::other)?;

    let (channels, ends) = TransportChannels::pair(CHANNEL_CAPACITY);
    let ended = Arc::new(AtomicBool::new(false));
    // Terminal input is counted for the stream's feedback once written; the
    // startup command typed on the user's behalf is not terminal input.
    let (write_tx, write_rx) = async_channel::bounded::<(Vec<u8>, bool)>(CHANNEL_CAPACITY);
    ends.feedback.track_written_input();
    let (reader_done_tx, reader_done_rx) = std::sync::mpsc::channel::<()>();

    spawn_writer(writer, write_rx, ends.feedback.clone());
    spawn_reader(
        reader,
        ends.events.clone(),
        write_tx.clone(),
        local_startup_input(profile.startup_command.as_deref()),
        ended.clone(),
        reader_done_tx,
    );

    // Exit: report once the shell is gone and its last output has drained.
    {
        let events = ends.events.clone();
        let ended = ended.clone();
        thread::Builder::new()
            .name("muxus-local-wait".into())
            .spawn(move || {
                let status = child.wait();
                let _ = reader_done_rx.recv_timeout(EXIT_DRAIN_GRACE);
                if ended.swap(true, Ordering::SeqCst) {
                    return;
                }
                let event = match status {
                    Ok(status) => TransportEvent::Exit {
                        code: Some(status.exit_code() as i32),
                        signal: status.signal().map(str::to_string),
                    },
                    Err(err) => TransportEvent::Error(err.to_string()),
                };
                let _ = events.send_blocking(event);
                events.close();
            })?;
    }

    // Input: keystrokes go to the writer thread, resizes to the master.
    {
        let master: Mutex<Box<dyn MasterPty + Send>> = Mutex::new(pair.master);
        let events = ends.events;
        let input = ends.input;
        thread::Builder::new()
            .name("muxus-local-input".into())
            .spawn(move || {
                loop {
                    match input.recv_blocking() {
                        Ok(TransportInput::Data(data)) => {
                            if write_tx.send_blocking((data, true)).is_err() {
                                break;
                            }
                        }
                        Ok(TransportInput::Resize(size)) => {
                            if let Ok(master) = master.lock() {
                                let _ = master.resize(pty_size(size));
                            }
                        }
                        Ok(TransportInput::Break { .. }) => {}
                        Ok(TransportInput::Close) | Err(_) => {
                            ended.store(true, Ordering::SeqCst);
                            let _ = killer.kill();
                            events.close();
                            break;
                        }
                    }
                }
                write_tx.close();
            })?;
    }

    Ok(LocalPty {
        channels,
        shell: plan.program,
        process_id,
    })
}

fn spawn_writer(
    mut writer: Box<dyn Write + Send>,
    queue: async_channel::Receiver<(Vec<u8>, bool)>,
    feedback: TransportFeedback,
) {
    let _ = thread::Builder::new()
        .name("muxus-local-write".into())
        .spawn(move || {
            while let Ok((data, terminal_input)) = queue.recv_blocking() {
                if writer
                    .write_all(&data)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
                if terminal_input {
                    feedback.input_written();
                }
            }
            feedback.close();
        });
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    events: async_channel::Sender<TransportEvent>,
    writes: async_channel::Sender<(Vec<u8>, bool)>,
    mut startup_input: Option<String>,
    ended: Arc<AtomicBool>,
    done: std::sync::mpsc::Sender<()>,
) {
    let _ = thread::Builder::new()
        .name("muxus-local-read".into())
        .spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            let mut startup_output = String::new();
            let mut decoder = Utf8Tail::default();
            loop {
                let read = match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                let chunk = &buffer[..read];
                if startup_input.is_some() {
                    startup_output.push_str(&decoder.decode(chunk));
                    trim_to_last_chars(&mut startup_output, STARTUP_OUTPUT_WINDOW);
                    if local_shell_prompt_ready(&startup_output) {
                        let input = (startup_input.take().unwrap_or_default().into_bytes(), false);
                        startup_output = String::new();
                        if let Err(async_channel::TrySendError::Full(input)) =
                            writes.try_send(input)
                        {
                            let writes = writes.clone();
                            thread::spawn(move || writes.send_blocking(input));
                        }
                    }
                }
                if ended.load(Ordering::SeqCst)
                    || events
                        .send_blocking(TransportEvent::Data(chunk.to_vec()))
                        .is_err()
                {
                    break;
                }
            }
            let _ = done.send(());
        });
}

/// Keep only the newest `limit` characters.
fn trim_to_last_chars(text: &mut String, limit: usize) {
    let count = text.chars().count();
    if count > limit {
        let cut = text
            .char_indices()
            .nth(count - limit)
            .map(|(index, _)| index)
            .unwrap_or(0);
        text.drain(..cut);
    }
}

/// UTF-8 decoding that keeps a character split across reads for the next one.
#[derive(Default)]
struct Utf8Tail {
    pending: Vec<u8>,
}

impl Utf8Tail {
    fn decode(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let keep = incomplete_suffix_len(&self.pending);
        let complete = self.pending.len() - keep;
        let text = String::from_utf8_lossy(&self.pending[..complete]).into_owned();
        self.pending.drain(..complete);
        text
    }
}

/// Length of a trailing, possibly valid but unfinished UTF-8 sequence.
fn incomplete_suffix_len(bytes: &[u8]) -> usize {
    for back in 1..=3.min(bytes.len()) {
        let byte = bytes[bytes.len() - back];
        if byte & 0b1100_0000 == 0b1000_0000 {
            continue;
        }
        let needed = match byte {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return 0,
        };
        return if needed > back { back } else { 0 };
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shell_prefers_an_existing_shell_variable() {
        let env = HashMap::from([("SHELL".to_string(), "/usr/bin/fish".to_string())]);
        assert_eq!(
            default_shell_with(&env, Platform::Linux, |_| true),
            "/usr/bin/fish"
        );
        assert_eq!(
            default_shell_with(&env, Platform::Linux, |path| path == "/bin/zsh"),
            "/bin/zsh"
        );
        assert_eq!(
            default_shell_with(&env, Platform::Linux, |_| false),
            "/bin/bash"
        );
        assert_eq!(
            default_shell_with(&HashMap::new(), Platform::Windows, |_| true),
            "powershell.exe"
        );
    }

    #[test]
    fn utf8_tail_keeps_split_characters() {
        let mut decoder = Utf8Tail::default();
        let check = "✓".as_bytes();
        assert_eq!(decoder.decode(&check[..1]), "");
        assert_eq!(decoder.decode(&check[1..]), "✓");
        assert_eq!(decoder.decode(b"a\xffb"), "a\u{fffd}b");
    }

    #[test]
    fn host_terminal_variables_are_recognized() {
        assert!(is_host_terminal_env("TERM_PROGRAM"));
        assert!(is_host_terminal_env("KITTY_WINDOW_ID"));
        assert!(is_host_terminal_env("GNOME_TERMINAL_SCREEN"));
        assert!(!is_host_terminal_env("TERM"));
        assert!(!is_host_terminal_env("PATH"));
    }
}
