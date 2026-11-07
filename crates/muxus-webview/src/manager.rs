//! The app's side of the page windows (`openWebWindow`, `focusWebWindow`,
//! `closeAllWebWindows` and `registerWebWindows` in `electron/src/web-windows.ts`): one window
//! per saved host, each a process of its own (see [`crate::protocol`]). A window owns the proxy
//! forward it was opened with; when it closes, [`WebWindows::closed`] names that forward so the
//! app stops it.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;

use crate::protocol::{WEB_WINDOW_ARG, WebWindowCommand, WebWindowEvent, WebWindowLaunch, line};
use crate::request::{WebWindowResult, is_window_key, parse_web_window_request};
use crate::topology::{TopologyTheme, TopologyWindowRequest, is_valid_topology_request};

/// A page window that closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedWebWindow {
    /// The saved host it showed.
    pub key: String,
    /// The forward behind its proxy, to stop now.
    pub forward_id: Option<String>,
    /// Why the window could not be shown, when it could not.
    pub error: Option<String>,
}

/// A node a topology window asks to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedNode {
    /// The window's key.
    pub key: String,
    pub name: String,
}

struct OpenWindow {
    /// Tells this window apart from a later one for the same host.
    serial: u64,
    stdin: ChildStdin,
}

struct Inner {
    program: PathBuf,
    args: Vec<OsString>,
    data_root: PathBuf,
    windows: Mutex<HashMap<String, OpenWindow>>,
    next_serial: AtomicU64,
    closed_tx: async_channel::Sender<ClosedWebWindow>,
    closed_rx: async_channel::Receiver<ClosedWebWindow>,
    nodes_tx: async_channel::Sender<OpenedNode>,
    nodes_rx: async_channel::Receiver<OpenedNode>,
}

/// Every page window of the app, by saved host.
#[derive(Clone)]
pub struct WebWindows {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write one command; false when the window is gone.
fn send(window: &mut OpenWindow, command: WebWindowCommand) -> bool {
    let text = line(&command);
    window.stdin.write_all(text.as_bytes()).is_ok() && window.stdin.flush().is_ok()
}

/// A folder only this user can read: it holds the page's cookies.
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

impl WebWindows {
    /// Windows run by this very program (`muxus --web-window`), keeping each host's cookies
    /// and storage in its own folder under `data_root`.
    pub fn for_current_exe(data_root: impl Into<PathBuf>) -> std::io::Result<Self> {
        Ok(Self::with_command(
            std::env::current_exe()?,
            vec![WEB_WINDOW_ARG.into()],
            data_root,
        ))
    }

    /// Windows run by `program args…` (tests use a script that speaks the protocol).
    pub fn with_command(
        program: impl Into<PathBuf>,
        args: Vec<OsString>,
        data_root: impl Into<PathBuf>,
    ) -> Self {
        let (closed_tx, closed_rx) = async_channel::unbounded();
        let (nodes_tx, nodes_rx) = async_channel::unbounded();
        Self {
            inner: Arc::new(Inner {
                program: program.into(),
                args,
                data_root: data_root.into(),
                windows: Mutex::new(HashMap::new()),
                next_serial: AtomicU64::new(1),
                closed_tx,
                closed_rx,
                nodes_tx,
                nodes_rx,
            }),
        }
    }

    /// Windows that close, with the forward each one leaves behind.
    pub fn closed(&self) -> async_channel::Receiver<ClosedWebWindow> {
        self.inner.closed_rx.clone()
    }

    /// Where a host's page keeps its cookies and storage.
    pub fn data_directory(&self, key: &str) -> PathBuf {
        self.inner.data_root.join(key)
    }

    /// Whether a host's window is open.
    pub fn is_open(&self, key: &str) -> bool {
        lock(&self.inner.windows).contains_key(key)
    }

    /// `focusWebWindow`: bring a host's open window to the front; false when it has none.
    pub fn focus(&self, key: &str) -> bool {
        let mut windows = lock(&self.inner.windows);
        windows
            .get_mut(key)
            .is_some_and(|window| send(window, WebWindowCommand::Focus))
    }

    /// `muxus:web:open`: open the window a request asks for, or focus the host's open one.
    /// On [`WebWindowResult::Focused`] and [`WebWindowResult::Invalid`] the caller stops the
    /// forward it passed along (the open window's proxy serves the page, or nothing will).
    pub fn open(&self, request: &Value) -> Result<WebWindowResult, String> {
        let Some(request) = parse_web_window_request(request) else {
            return Ok(WebWindowResult::Invalid);
        };
        let mut windows = lock(&self.inner.windows);
        if let Some(window) = windows.get_mut(&request.key)
            && send(window, WebWindowCommand::Focus)
        {
            // Another app window got here first; its proxy serves the page.
            return Ok(WebWindowResult::Focused);
        }
        debug_assert!(is_window_key(&request.key));
        let data_directory = self.data_directory(&request.key);
        create_private_dir(&data_directory).map_err(|error| {
            format!(
                "Could not create {} for the page: {error}",
                data_directory.display()
            )
        })?;
        let launch = WebWindowLaunch {
            request: serde_json::to_value(&request).unwrap_or(Value::Null),
            data_directory,
            topology: None,
        };
        self.spawn(windows, request.key, &launch, request.forward_id)
    }

    /// Show a lab's topology in a window of its own (see [`crate::topology`]), or bring the
    /// lab's open window to the front.
    pub fn open_topology(
        &self,
        request: &TopologyWindowRequest,
    ) -> Result<WebWindowResult, String> {
        if !is_valid_topology_request(request) {
            return Ok(WebWindowResult::Invalid);
        }
        let mut windows = lock(&self.inner.windows);
        if let Some(window) = windows.get_mut(&request.key)
            && send(window, WebWindowCommand::Focus)
        {
            return Ok(WebWindowResult::Focused);
        }
        let launch = WebWindowLaunch {
            request: Value::Null,
            data_directory: PathBuf::new(),
            topology: Some(request.clone()),
        };
        self.spawn(windows, request.key.clone(), &launch, None)
    }

    /// Nodes topology windows ask to open, by window key.
    pub fn opened_nodes(&self) -> async_channel::Receiver<OpenedNode> {
        self.inner.nodes_rx.clone()
    }

    /// Tell every open window the app's theme changed (topology windows follow it).
    pub fn set_theme(&self, theme: &TopologyTheme) {
        let mut windows = lock(&self.inner.windows);
        for window in windows.values_mut() {
            send(
                window,
                WebWindowCommand::Theme {
                    theme: Box::new(theme.clone()),
                },
            );
        }
    }

    /// Start the window process, hand it its launch line and watch it until it exits.
    fn spawn(
        &self,
        mut windows: MutexGuard<'_, HashMap<String, OpenWindow>>,
        key: String,
        launch: &WebWindowLaunch,
        forward_id: Option<String>,
    ) -> Result<WebWindowResult, String> {
        let mut child = Command::new(&self.inner.program)
            .args(&self.inner.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("Could not open the page window: {error}"))?;
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Could not open the page window.".into());
        };
        let launch = line(launch);
        if let Err(error) = stdin
            .write_all(launch.as_bytes())
            .and_then(|()| stdin.flush())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Could not open the page window: {error}"));
        }
        let serial = self.inner.next_serial.fetch_add(1, Ordering::Relaxed);
        windows.insert(key.clone(), OpenWindow { serial, stdin });
        drop(windows);

        let inner = Arc::downgrade(&self.inner);
        let closed_tx = self.inner.closed_tx.clone();
        let nodes_tx = self.inner.nodes_tx.clone();
        let watch = move || {
            let mut error = None;
            for text in BufReader::new(stdout).lines() {
                let Ok(text) = text else { break };
                match serde_json::from_str(&text) {
                    Ok(WebWindowEvent::Error { message }) => error = Some(message),
                    Ok(WebWindowEvent::OpenNode { name }) => {
                        let _ = nodes_tx.try_send(OpenedNode {
                            key: key.clone(),
                            name,
                        });
                    }
                    _ => {}
                }
            }
            let status = child.wait();
            if error.is_none() {
                error = match status {
                    Ok(status) if status.success() => None,
                    Ok(status) => Some(format!("The page window closed unexpectedly ({status}).")),
                    Err(err) => Some(format!("The page window closed unexpectedly: {err}")),
                };
            }
            if let Some(inner) = inner.upgrade() {
                let mut windows = lock(&inner.windows);
                if windows.get(&key).is_some_and(|w| w.serial == serial) {
                    windows.remove(&key);
                }
            }
            let _ = closed_tx.try_send(ClosedWebWindow {
                key,
                forward_id,
                error,
            });
        };
        if let Err(error) = std::thread::Builder::new()
            .name("web-window".into())
            .spawn(watch)
        {
            // Without a watcher nobody would notice the window close; it is still open, so
            // leave it be and say so.
            return Err(format!("Could not watch the page window: {error}"));
        }
        Ok(WebWindowResult::Opened)
    }

    /// `closeAllWebWindows`: close every window, as the last app window closes; each one
    /// reports its forward through [`WebWindows::closed`] as it goes.
    pub fn close_all(&self) {
        let mut windows = lock(&self.inner.windows);
        for window in windows.values_mut() {
            send(window, WebWindowCommand::Close);
        }
        // A window whose stdin ends closes too, should it miss the command.
        windows.clear();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    /// A stand-in window: says it opened, logs what it is told, and exits on `close`.
    const WINDOW: &str = r#"
read launch
echo "$launch" > "$0.launch"
echo '{"event":"opened"}'
while read cmd; do
  echo "$cmd" >> "$0.commands"
  case "$cmd" in *close*) exit 0 ;; esac
done
"#;

    fn windows(script: &str, scratch: &Path) -> (WebWindows, PathBuf) {
        let log = scratch.join("window");
        let windows = WebWindows::with_command(
            "/bin/sh",
            vec!["-c".into(), script.into(), log.clone().into()],
            scratch.join("web-pages"),
        );
        (windows, log)
    }

    fn request(forward: Option<&str>) -> Value {
        let mut request =
            json!({ "key": "host_1", "url": "https://grafana.lab", "title": "Grafana" });
        if let Some(id) = forward {
            request["proxy"] = json!("socks5://127.0.0.1:41000");
            request["forwardId"] = json!(id);
        }
        request
    }

    fn next_closed(windows: &WebWindows) -> ClosedWebWindow {
        let closed = windows.closed();
        for _ in 0..200 {
            if let Ok(event) = closed.try_recv() {
                return event;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the window did not close");
    }

    fn wait_for(path: &Path, text: &str) -> String {
        for _ in 0..200 {
            let content = std::fs::read_to_string(path).unwrap_or_default();
            if content.contains(text) {
                return content;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("{} never had {text}", path.display());
    }

    #[test]
    fn opens_one_window_per_host_and_focuses_it_on_reopen() {
        let scratch = tempfile::tempdir().unwrap();
        let (windows, log) = windows(WINDOW, scratch.path());

        assert_eq!(
            windows.open(&request(Some("fwd1"))),
            Ok(WebWindowResult::Opened)
        );
        assert!(windows.is_open("host_1"));
        assert!(scratch.path().join("web-pages/host_1").is_dir());
        let launch: WebWindowLaunch =
            serde_json::from_str(wait_for(&log.with_extension("launch"), "dataDirectory").trim())
                .unwrap();
        assert_eq!(launch.request, request(Some("fwd1")));
        assert_eq!(
            launch.data_directory,
            scratch.path().join("web-pages/host_1")
        );

        assert_eq!(
            windows.open(&request(Some("fwd2"))),
            Ok(WebWindowResult::Focused)
        );
        assert!(windows.focus("host_1"));
        assert!(!windows.focus("other"));
        wait_for(&log.with_extension("commands"), "focus");

        windows.close_all();
        let closed = next_closed(&windows);
        assert_eq!(
            closed,
            ClosedWebWindow {
                key: "host_1".into(),
                forward_id: Some("fwd1".into()),
                error: None,
            }
        );
        assert!(!windows.is_open("host_1"));
    }

    #[test]
    fn refuses_a_request_a_window_must_not_show() {
        let scratch = tempfile::tempdir().unwrap();
        let (windows, _) = windows(WINDOW, scratch.path());
        let mut bad = request(Some("fwd1"));
        bad["url"] = json!("file:///etc/passwd");
        assert_eq!(windows.open(&bad), Ok(WebWindowResult::Invalid));
        assert!(!windows.is_open("host_1"));
    }

    #[test]
    fn reports_why_a_window_could_not_be_shown() {
        let scratch = tempfile::tempdir().unwrap();
        let script = r#"read launch; echo '{"event":"error","message":"no display"}'; exit 1"#;
        let (windows, _) = windows(script, scratch.path());
        assert_eq!(windows.open(&request(None)), Ok(WebWindowResult::Opened));
        let closed = next_closed(&windows);
        assert_eq!(closed.error.as_deref(), Some("no display"));
        assert_eq!(closed.forward_id, None);

        let (windows, _) = windows_crashing(scratch.path());
        assert_eq!(windows.open(&request(None)), Ok(WebWindowResult::Opened));
        let closed = next_closed(&windows);
        assert!(
            closed
                .error
                .as_deref()
                .is_some_and(|error| error.starts_with("The page window closed unexpectedly")),
            "{closed:?}"
        );
    }

    fn windows_crashing(scratch: &Path) -> (WebWindows, PathBuf) {
        windows("read launch; exit 3", scratch)
    }

    /// A stand-in topology window: the user opens a node, and the window closes.
    const TOPOLOGY_WINDOW: &str = r#"
read -r launch
printf '%s\n' "$launch" > "$0.launch"
echo '{"event":"opened"}'
read cmd
echo "$cmd" >> "$0.commands"
echo '{"event":"openNode","name":"srl1"}'
exit 0
"#;

    #[test]
    fn shows_a_topology_and_hears_which_node_to_open() {
        let scratch = tempfile::tempdir().unwrap();
        let (windows, log) = windows(TOPOLOGY_WINDOW, scratch.path());
        let request = crate::topology::tests::request();

        assert_eq!(windows.open_topology(&request), Ok(WebWindowResult::Opened));
        let launch: WebWindowLaunch =
            serde_json::from_str(wait_for(&log.with_extension("launch"), "topology").trim())
                .unwrap();
        assert_eq!(launch.topology.as_ref(), Some(&request));
        // No folder of its own: the viewer keeps nothing.
        assert!(!scratch.path().join("web-pages").exists());

        // The same lab again only focuses its window; a theme change reaches it.
        assert_eq!(
            windows.open_topology(&request),
            Ok(WebWindowResult::Focused)
        );
        wait_for(&log.with_extension("commands"), "focus");

        let opened = windows.opened_nodes();
        let mut node = None;
        for _ in 0..200 {
            if let Ok(found) = opened.try_recv() {
                node = Some(found);
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            node,
            Some(OpenedNode {
                key: "topology-1".into(),
                name: "srl1".into()
            })
        );
        let closed = next_closed(&windows);
        assert_eq!((closed.key.as_str(), closed.error), ("topology-1", None));

        let mut bad = request;
        bad.key = "a b".into();
        assert_eq!(windows.open_topology(&bad), Ok(WebWindowResult::Invalid));
    }

    #[test]
    fn passes_a_theme_change_to_every_window() {
        let scratch = tempfile::tempdir().unwrap();
        let (windows, log) = windows(WINDOW, scratch.path());
        assert_eq!(windows.open(&request(None)), Ok(WebWindowResult::Opened));
        windows.set_theme(&crate::topology::tests::theme());
        let commands = wait_for(&log.with_extension("commands"), "theme");
        let command: WebWindowCommand = serde_json::from_str(commands.trim()).unwrap();
        assert!(matches!(command, WebWindowCommand::Theme { .. }));
        windows.close_all();
        next_closed(&windows);
    }
}
