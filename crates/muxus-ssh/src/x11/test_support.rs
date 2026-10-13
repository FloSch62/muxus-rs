//! Shared fixtures for the X11 tests: setup requests as clients send them and stand-in X servers.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::proxy::rewrite_x11_setup;
use super::xauthority::{FAMILY_WILD, MIT_MAGIC_COOKIE, X11Auth, XauthEntry, serialize_xauthority};

/// An X11 connection setup request as a client would send it.
pub fn setup_request(little_endian: bool, name: &str, data: &[u8]) -> Vec<u8> {
    let mut header = vec![if little_endian { b'l' } else { b'B' }, 0, 0, 0, 0, 0];
    let version = if little_endian {
        11u16.to_le_bytes()
    } else {
        11u16.to_be_bytes()
    };
    header[2..4].copy_from_slice(&version);
    header.resize(12, 0);
    rewrite_x11_setup(
        &header,
        little_endian,
        Some(&X11Auth {
            name: name.to_string(),
            data: data.to_vec(),
        }),
    )
}

/// Expected FamilyInternet6 bytes, built independently of the code under test.
pub fn ipv6(address: &str) -> Vec<u8> {
    let (head, tail) = address.split_once("::").unwrap_or((address, ""));
    let left: Vec<&str> = if head.is_empty() {
        Vec::new()
    } else {
        head.split(':').collect()
    };
    let right: Vec<&str> = if tail.is_empty() {
        Vec::new()
    } else {
        tail.split(':').collect()
    };
    let mut groups = left.clone();
    groups.extend(std::iter::repeat_n("0", 8 - left.len() - right.len()));
    groups.extend(right);
    groups
        .iter()
        .flat_map(|group| u16::from_str_radix(group, 16).unwrap().to_be_bytes())
        .collect()
}

/// An Xauthority file with one FamilyWild MIT-MAGIC-COOKIE-1 record for `number`.
pub fn write_xauthority(path: &Path, number: &str, cookie: &[u8]) {
    let bytes = serialize_xauthority(&[XauthEntry {
        family: FAMILY_WILD,
        address: Vec::new(),
        number: number.to_string(),
        name: MIT_MAGIC_COOKIE.to_string(),
        data: cookie.to_vec(),
    }])
    .unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A local X server stand-in on a Unix socket: it records the first chunk of every connection
/// (the setup request) and answers it with `reply`.
#[cfg(unix)]
pub struct FakeXServer {
    /// `$DISPLAY` naming the socket: `<dir>/xsock:<number>`.
    pub display: String,
    pub setups: Arc<Mutex<Vec<Vec<u8>>>>,
    task: tokio::task::JoinHandle<()>,
}

#[cfg(unix)]
impl FakeXServer {
    pub fn start(dir: &Path, number: &str, reply: &'static [u8]) -> Self {
        let base = dir.join("xsock");
        let socket = PathBuf::from(format!("{}:{number}", base.display()));
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let setups = Arc::new(Mutex::new(Vec::new()));
        let seen = setups.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let seen = seen.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let Ok(read) = stream.read(&mut buf).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    seen.lock().unwrap().push(buf[..read].to_vec());
                    let _ = stream.write_all(reply).await;
                    // Echo whatever else arrives until the client goes away.
                    let mut rest = vec![0u8; 4096];
                    while let Ok(read) = stream.read(&mut rest).await {
                        if read == 0 || stream.write_all(&rest[..read]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        Self {
            display: format!("{}:{number}", base.display()),
            setups,
            task,
        }
    }
}

#[cfg(unix)]
impl Drop for FakeXServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Stand-ins for spawned VcXsrv processes. A spawned server "listens" on its display port a few
/// milliseconds after launch and stops listening when it exits; killing it makes it exit.
#[derive(Clone)]
pub struct FakeChildren {
    listening: Arc<Mutex<std::collections::HashSet<u16>>>,
    raced: Arc<std::collections::HashSet<u16>>,
    spawns: Arc<Mutex<Vec<FakeChild>>>,
}

/// One spawn as the fake recorded it.
#[derive(Clone)]
pub struct FakeChild {
    pub executable: PathBuf,
    pub args: Vec<String>,
    port: u16,
    killed: Arc<std::sync::atomic::AtomicBool>,
    exit: Arc<Mutex<Option<tokio::sync::oneshot::Sender<String>>>>,
    listening: Arc<Mutex<std::collections::HashSet<u16>>>,
}

impl FakeChild {
    pub fn killed(&self) -> bool {
        self.killed.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The process ends (once) with `reason`.
    pub fn exit(&self, reason: &str) {
        if let Some(exit) = self.exit.lock().unwrap().take() {
            self.listening.lock().unwrap().remove(&self.port);
            let _ = exit.send(reason.to_string());
        }
    }
}

impl FakeChildren {
    /// `busy` ports are taken before the scan; `raced` ports are grabbed by some other X server
    /// between the scan and VcXsrv binding them.
    pub fn new(busy: &[u16], raced: &[u16]) -> Self {
        Self {
            listening: Arc::new(Mutex::new(busy.iter().copied().collect())),
            raced: Arc::new(raced.iter().copied().collect()),
            spawns: Arc::default(),
        }
    }

    pub fn options(&self, auth_directory: &Path) -> super::vcxsrv::BundledXServerOptions {
        use super::vcxsrv::{BundledXServerOptions, XServerProbe, XServerProcess};

        let listening = self.listening.clone();
        let port_in_use = move |port: u16| -> super::BoxFuture<bool> {
            let busy = listening.lock().unwrap().contains(&port);
            Box::pin(async move { busy })
        };
        let listening = self.listening.clone();
        let raced = self.raced.clone();
        let probe = move |port: u16, _auth: X11Auth| -> super::BoxFuture<XServerProbe> {
            let state = if raced.contains(&port) {
                XServerProbe::Foreign
            } else if listening.lock().unwrap().contains(&port) {
                XServerProbe::Ours
            } else {
                XServerProbe::Down
            };
            Box::pin(async move { state })
        };
        let children = self.clone();
        let spawn = move |executable: &Path, args: &[String], _cwd: &Path| {
            let port = 6000 + args[0][1..].parse::<u16>().unwrap();
            let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
            let child = FakeChild {
                executable: executable.to_path_buf(),
                args: args.to_vec(),
                port,
                killed: Arc::default(),
                exit: Arc::new(Mutex::new(Some(exit_tx))),
                listening: children.listening.clone(),
            };
            children.spawns.lock().unwrap().push(child.clone());
            // The server starts listening on its display port shortly after launch.
            let listening = children.listening.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                listening.lock().unwrap().insert(port);
            });
            let killed = child.clone();
            Ok(XServerProcess::new(
                move || {
                    killed
                        .killed
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    killed.exit("exited with SIGTERM");
                },
                exit_rx,
            ))
        };
        BundledXServerOptions {
            spawn_server: Some(Arc::new(spawn)),
            port_in_use: Some(Arc::new(port_in_use)),
            probe_server: Some(Arc::new(probe)),
            auth_directory: Some(auth_directory.to_path_buf()),
            claimed_displays: None,
        }
    }

    pub fn count(&self) -> usize {
        self.spawns.lock().unwrap().len()
    }

    pub fn get(&self, index: usize) -> FakeChild {
        self.spawns.lock().unwrap()[index].clone()
    }

    /// The display argument of every spawn, in order.
    pub fn displays(&self) -> Vec<String> {
        self.spawns
            .lock()
            .unwrap()
            .iter()
            .map(|child| child.args[0].clone())
            .collect()
    }

    /// Let exit notifications reach the servers that watch them.
    pub async fn settle(&self) {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
