//! One Muxus per data folder (`app.requestSingleInstanceLock` and the `second-instance` event
//! in `electron/src/main.ts`).
//!
//! The first process takes an exclusive lock on a per-user lock file and listens on a
//! per-user Unix socket (a named pipe on Windows). A later launch finds the lock taken, hands
//! its command line to the running instance and exits; the running instance turns it into a
//! [`DesktopEvent`]. The launch is parsed by the process that was started (structured data,
//! as Electron forwards it, because argument order may differ in a re-parse), and validated
//! again by the receiver ([`launch_from_payload`]), since anything on the socket is untrusted.
//!
//! The lock is keyed by the data folder, so a sandboxed run (`MUXUS_DATA_DIR`) or a
//! development build never hands its launch to the installed app.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use muxus_core::api::CommandLineLaunch;
use muxus_import::launch::{parse_command_line_launch, parse_command_line_launch_data};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use super::events::{DesktopEvent, DesktopEventSender, DesktopEvents, desktop_events};

/// What a later launch sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstancePayload {
    /// Its full command line, executable first.
    pub argv: Vec<String>,
    /// The launch it parsed itself.
    pub launch: Option<CommandLineLaunch>,
}

/// The largest message a later launch may send.
const MAX_PAYLOAD_BYTES: u64 = 64 * 1024;
/// How long a later launch keeps trying to reach a running instance that is still starting.
const CONNECT_PATIENCE: Duration = Duration::from_secs(5);
/// How long a relaunched update waits for the old process to exit.
const RELAUNCH_PATIENCE: Duration = Duration::from_secs(60);
const REPLY: &str = "ok";

/// The launch a payload asks for: the structured launch when it validates, else the command
/// line parsed again. `None` means "just show the window".
pub fn launch_from_payload(payload: &InstancePayload) -> Option<CommandLineLaunch> {
    payload
        .launch
        .as_ref()
        .and_then(|launch| serde_json::to_value(launch).ok())
        .and_then(|value| parse_command_line_launch_data(&value))
        .or_else(|| parse_command_line_launch(&payload.argv))
}

/// The event a payload turns into.
pub fn event_for_payload(payload: &InstancePayload) -> DesktopEvent {
    match launch_from_payload(payload) {
        Some(launch) => DesktopEvent::Launch(launch),
        None => DesktopEvent::Focus,
    }
}

/// Lock and socket names for one data folder: `muxus-rs-<hash>`.
pub fn instance_name(data_dir: &Path) -> String {
    let canonical = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    let digest = sha2::Sha256::digest(canonical.to_string_lossy().as_bytes());
    let hex: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("muxus-rs-{hex}")
}

/// The per-user folder for the lock and the socket: `$XDG_RUNTIME_DIR`, else a private
/// folder in the temporary directory.
pub fn runtime_dir() -> PathBuf {
    #[cfg(unix)]
    {
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute() && dir.is_dir())
        {
            return dir;
        }
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        std::env::temp_dir().join(format!("muxus-rs-{uid}"))
    }
    #[cfg(not(unix))]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("muxus-rs")
    }
}

/// How [`acquire`] ended.
pub enum Acquired {
    /// This is the running instance: keep the guard alive for the whole run and subscribe to
    /// [`PrimaryInstance::events`].
    Primary(PrimaryInstance),
    /// Another instance runs; the launch was handed to it (`true`) or could not be delivered
    /// (`false`, the other instance is still starting or hung). Either way, exit.
    Secondary { delivered: bool },
}

/// The running instance: holds the lock and listens for later launches.
pub struct PrimaryInstance {
    _lock: File,
    events: DesktopEvents,
    sender: DesktopEventSender,
    #[cfg(unix)]
    socket: PathBuf,
}

impl PrimaryInstance {
    /// Later launches, focus requests and (through [`PrimaryInstance::sender`]) anything else
    /// the desktop shell reports.
    pub fn events(&self) -> DesktopEvents {
        self.events.clone()
    }

    /// For other sources of the same events (macOS `open-url`, tests).
    pub fn sender(&self) -> DesktopEventSender {
        self.sender.clone()
    }
}

#[cfg(unix)]
impl Drop for PrimaryInstance {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Wait for a process this one replaces (an update relaunch) to exit.
pub fn wait_for_exit(pid: u32, patience: Duration) {
    let deadline = Instant::now() + patience;
    while process_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 only checks that the process exists.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Become the running instance for `data_dir`, or hand `argv` to the one that is.
pub fn acquire(data_dir: &Path, argv: &[String]) -> std::io::Result<Acquired> {
    acquire_in(&runtime_dir(), data_dir, argv)
}

/// [`acquire`] with an explicit folder for the lock and the socket (tests).
pub fn acquire_in(runtime: &Path, data_dir: &Path, argv: &[String]) -> std::io::Result<Acquired> {
    if let Some(pid) = std::env::var(muxus_session::updater::RELAUNCH_AFTER_PID_VAR)
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
    {
        wait_for_exit(pid, RELAUNCH_PATIENCE);
    }
    create_private_dir(runtime)?;
    let name = instance_name(data_dir);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(runtime.join(format!("{name}.lock")))?;
    let payload = InstancePayload {
        argv: argv.to_vec(),
        launch: parse_command_line_launch(argv),
    };
    match lock.try_lock() {
        Ok(()) => listen(runtime, &name, lock),
        Err(std::fs::TryLockError::WouldBlock) => Ok(Acquired::Secondary {
            delivered: hand_over(runtime, &name, &payload),
        }),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = std::fs::metadata(dir)?;
        // SAFETY: getuid has no preconditions.
        if meta.uid() != unsafe { libc::getuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{} belongs to another user", dir.display()),
            ));
        }
        if meta.mode() & 0o077 != 0 && dir.starts_with(std::env::temp_dir()) {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// Read one payload line and answer it.
fn serve_connection<S: Read + Write>(stream: S, sender: &DesktopEventSender) {
    let mut reader = BufReader::new(stream.take(MAX_PAYLOAD_BYTES));
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let Ok(payload) = serde_json::from_str::<InstancePayload>(line.trim_end()) else {
        return;
    };
    sender.send(event_for_payload(&payload));
    let mut stream = reader.into_inner().into_inner();
    let _ = stream.write_all(format!("{REPLY}\n").as_bytes());
    let _ = stream.flush();
}

#[cfg(unix)]
fn listen(runtime: &Path, name: &str, lock: File) -> std::io::Result<Acquired> {
    use std::os::unix::net::UnixListener;
    let socket = runtime.join(format!("{name}.sock"));
    // A socket left by a crashed instance; the lock says nobody serves it.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    }
    let (sender, events) = desktop_events();
    let serving = sender.clone();
    std::thread::Builder::new()
        .name("muxus-instance".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                serve_connection(stream, &serving);
                if serving.is_closed() {
                    break;
                }
            }
        })?;
    Ok(Acquired::Primary(PrimaryInstance {
        _lock: lock,
        events,
        sender,
        socket,
    }))
}

#[cfg(unix)]
fn hand_over(runtime: &Path, name: &str, payload: &InstancePayload) -> bool {
    use std::os::unix::net::UnixStream;
    let socket = runtime.join(format!("{name}.sock"));
    let deadline = Instant::now() + CONNECT_PATIENCE;
    loop {
        if let Ok(stream) = UnixStream::connect(&socket) {
            let _ = stream.set_read_timeout(Some(CONNECT_PATIENCE));
            return send_payload(stream, payload);
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn send_payload(mut stream: impl Read + Write, payload: &InstancePayload) -> bool {
    let Ok(mut line) = serde_json::to_string(payload) else {
        return false;
    };
    line.push('\n');
    if stream.write_all(line.as_bytes()).is_err() || stream.flush().is_err() {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).is_ok() && reply.trim() == REPLY
}

#[cfg(windows)]
fn pipe_name(name: &str) -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let user: String = user
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect();
    format!(r"\\.\pipe\{name}-{user}")
}

#[cfg(windows)]
fn listen(_runtime: &Path, name: &str, lock: File) -> std::io::Result<Acquired> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::ServerOptions;
    let pipe = pipe_name(name);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;
    let first = runtime.block_on(async {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&pipe)
    })?;
    let (sender, events) = desktop_events();
    let serving = sender.clone();
    std::thread::Builder::new()
        .name("muxus-instance".into())
        .spawn(move || {
            runtime.block_on(async move {
                let mut server = first;
                loop {
                    if server.connect().await.is_err() {
                        break;
                    }
                    let Ok(next) = ServerOptions::new()
                        .reject_remote_clients(true)
                        .create(&pipe)
                    else {
                        break;
                    };
                    let mut client = std::mem::replace(&mut server, next);
                    let mut line = String::new();
                    {
                        let mut reader =
                            tokio::io::BufReader::new((&mut client).take(MAX_PAYLOAD_BYTES));
                        if reader.read_line(&mut line).await.is_err() {
                            continue;
                        }
                    }
                    if let Ok(payload) = serde_json::from_str::<InstancePayload>(line.trim_end()) {
                        serving.send(event_for_payload(&payload));
                        let _ = client.write_all(format!("{REPLY}\n").as_bytes()).await;
                    }
                    if serving.is_closed() {
                        break;
                    }
                }
            });
        })?;
    Ok(Acquired::Primary(PrimaryInstance {
        _lock: lock,
        events,
        sender,
    }))
}

#[cfg(windows)]
fn hand_over(_runtime: &Path, name: &str, payload: &InstancePayload) -> bool {
    let pipe = pipe_name(name);
    let deadline = Instant::now() + CONNECT_PATIENCE;
    loop {
        if let Ok(stream) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pipe)
        {
            return send_payload(stream, payload);
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("muxus")
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn trusts_the_structured_launch_only_after_validating_it() {
        let payload = InstancePayload {
            argv: argv(&["--host", "web"]),
            launch: Some(CommandLineLaunch::Connect {
                name: "  root@db.example:2222 ".into(),
            }),
        };
        assert_eq!(
            launch_from_payload(&payload),
            Some(CommandLineLaunch::Connect {
                name: "root@db.example:2222".into()
            })
        );
        // An invalid structured launch falls back to the command line.
        let invalid = InstancePayload {
            argv: argv(&["--host", "web"]),
            launch: Some(CommandLineLaunch::Connect {
                name: "bad host!".into(),
            }),
        };
        assert_eq!(
            launch_from_payload(&invalid),
            Some(CommandLineLaunch::Host { name: "web".into() })
        );
        let link = InstancePayload {
            argv: argv(&["ssh://admin@10.0.0.1:2222"]),
            launch: None,
        };
        assert!(matches!(
            event_for_payload(&link),
            DesktopEvent::Launch(CommandLineLaunch::Url { .. })
        ));
        let bare = InstancePayload {
            argv: argv(&[]),
            launch: None,
        };
        assert_eq!(event_for_payload(&bare), DesktopEvent::Focus);
    }

    #[test]
    fn names_one_instance_per_data_folder() {
        let one = instance_name(Path::new("/scratch/one"));
        assert!(one.starts_with("muxus-rs-") && one.len() == "muxus-rs-".len() + 16);
        assert_eq!(one, instance_name(Path::new("/scratch/one")));
        assert_ne!(one, instance_name(Path::new("/scratch/two")));
    }

    #[cfg(unix)]
    #[test]
    fn a_second_launch_hands_its_command_line_to_the_running_instance() {
        let runtime = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let Acquired::Primary(primary) =
            acquire_in(runtime.path(), data.path(), &argv(&[])).unwrap()
        else {
            panic!("the first launch runs");
        };
        let events = primary.events();
        let second = acquire_in(
            runtime.path(),
            data.path(),
            &argv(&["--connect", "admin@10.0.0.1", "--port", "2222"]),
        )
        .unwrap();
        assert!(matches!(second, Acquired::Secondary { delivered: true }));
        assert_eq!(
            events.recv_blocking(),
            Some(DesktopEvent::Launch(CommandLineLaunch::Connect {
                name: "admin@10.0.0.1:2222".into()
            }))
        );
        let third = acquire_in(runtime.path(), data.path(), &argv(&[])).unwrap();
        assert!(matches!(third, Acquired::Secondary { delivered: true }));
        assert_eq!(events.recv_blocking(), Some(DesktopEvent::Focus));

        // Another data folder is another instance.
        let other = tempfile::tempdir().unwrap();
        assert!(matches!(
            acquire_in(runtime.path(), other.path(), &argv(&[])).unwrap(),
            Acquired::Primary(_)
        ));

        // Once the running instance is gone, the next launch takes over (the stale socket
        // file does not get in the way).
        let socket = runtime
            .path()
            .join(format!("{}.sock", instance_name(data.path())));
        drop(primary);
        std::fs::write(&socket, b"stale").unwrap();
        assert!(matches!(
            acquire_in(runtime.path(), data.path(), &argv(&[])).unwrap(),
            Acquired::Primary(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn ignores_garbage_on_the_socket() {
        use std::os::unix::net::UnixStream;
        let runtime = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let Acquired::Primary(primary) =
            acquire_in(runtime.path(), data.path(), &argv(&[])).unwrap()
        else {
            panic!("the first launch runs");
        };
        let socket = runtime
            .path()
            .join(format!("{}.sock", instance_name(data.path())));
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream.write_all(b"not json\n").unwrap();
        let mut reply = String::new();
        let _ = BufReader::new(&stream).read_line(&mut reply);
        assert_eq!(reply, "", "no answer to garbage");
        // A real launch still gets through afterwards.
        assert!(hand_over(
            runtime.path(),
            &instance_name(data.path()),
            &InstancePayload {
                argv: argv(&["--workspace", "Lab"]),
                launch: None
            }
        ));
        assert_eq!(
            primary.events().recv_blocking(),
            Some(DesktopEvent::Launch(CommandLineLaunch::Workspace {
                name: "Lab".into()
            }))
        );
    }

    #[cfg(unix)]
    #[test]
    fn waits_for_a_replaced_process_to_exit() {
        let mut child = std::process::Command::new("sleep")
            .arg("0.3")
            .spawn()
            .unwrap();
        let started = Instant::now();
        let pid = child.id();
        let waiter = std::thread::spawn(move || wait_for_exit(pid, Duration::from_secs(5)));
        let _ = child.wait();
        waiter.join().unwrap();
        assert!(started.elapsed() >= Duration::from_millis(250));
        // A process that is gone does not hold anything up.
        let started = Instant::now();
        wait_for_exit(pid, Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
