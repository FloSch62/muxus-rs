//! Opening remote files with local programs: the client half
//! (`client/src/local-open.ts`) and the desktop half the Electron main
//! process did (`electron/src/local-open.ts`).
//!
//! Remote files opened with a local program are written to a private folder
//! first. Each copy gets a folder of its own so the program sees the remote
//! file's real name, and copies nothing has touched for a day are removed.
//! Saves the program makes are reported to [`super::local_copies`], which
//! offers to upload them.

use std::collections::HashMap;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use muxus_core::api::{LocalCopyChange, LocalOpenTarget, SftpUploadResponse};
use muxus_session::ApiError;
use muxus_ssh::sftp::{CancelSignal, UploadOptions, UploadStep};
use muxus_ui::gpui_kit::{App, AsyncApp};
use muxus_ui::theme::Status;
use sha2::{Digest, Sha256};

use super::dialogs::{ConfirmAnswer, ConfirmOptions, DialogHost};
use super::local_copies::{
    LocalCopySyncEnv, LocalCopyUpload, SyncQueue, TrackedLocalCopy, local_copy_changed,
};
use super::services::{SftpServices, error_details, notify_error, services};
use super::terminal_upload::LocalFuture;

/// How "Open with…" picks the program on this platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenWithChooser {
    /// Windows' own "How do you want to open this file?" dialog.
    System,
    /// The native file picker, starting in Applications (macOS).
    ProgramPicker,
    /// Muxus' list of installed programs (Linux).
    ApplicationList,
}

/// Remote files can be opened with local programs: the native app always can.
pub fn can_open_locally() -> bool {
    true
}

/// The chooser this platform offers; `None` hides "Open with…".
pub fn open_with_chooser() -> Option<OpenWithChooser> {
    if cfg!(windows) {
        Some(OpenWithChooser::System)
    } else if cfg!(target_os = "linux") {
        Some(OpenWithChooser::ApplicationList)
    } else {
        // macOS's program picker is not ported yet.
        None
    }
}

pub const LOCAL_COPY_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const LOCAL_COPY_PREFIX: &str = "open-";
const MAX_FILE_NAME_LENGTH: usize = 180;
/// Programs keep files open for hours; watching is cheap, but not unbounded.
pub const MAX_WATCHED_LOCAL_COPIES: usize = 64;

fn is_windows_reserved(name: &str) -> bool {
    // `/^(con|prn|aux|nul|com[1-9¹²³]|lpt[1-9¹²³])(\..*)?$/i`
    let lower = name.to_lowercase();
    let stem = lower.split('.').next().unwrap_or("");
    if ["con", "prn", "aux", "nul"].contains(&stem) {
        return true;
    }
    let digits = ['1', '2', '3', '4', '5', '6', '7', '8', '9', '¹', '²', '³'];
    ["com", "lpt"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|rest| {
            let mut chars = rest.chars();
            matches!((chars.next(), chars.next()), (Some(digit), None) if digits.contains(&digit))
        })
    })
}

/// A remote file name that is safe to create on every desktop platform.
pub fn local_copy_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut safe: String = base
        .chars()
        .map(|c| {
            if c < ' ' || c == '\u{7f}' || "<>:\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Windows drops trailing dots and spaces, which would change the name.
    let trimmed = safe.trim_end_matches(['.', ' ']).len();
    safe.truncate(trimmed);
    if safe.is_empty() || safe == "." || safe == ".." {
        safe = "file".into();
    }
    if is_windows_reserved(&safe) {
        safe = format!("_{safe}");
    }
    let length = safe.encode_utf16().count();
    if length > MAX_FILE_NAME_LENGTH {
        // `path.extname`: from the last dot that is not the first character.
        let extension: String = match safe.rfind('.') {
            Some(index) if index > 0 => safe[index..].chars().take(20).collect(),
            _ => String::new(),
        };
        let keep = MAX_FILE_NAME_LENGTH - extension.encode_utf16().count();
        let mut head = String::new();
        let mut units = 0;
        for c in safe.chars() {
            if units + c.len_utf16() > keep {
                break;
            }
            units += c.len_utf16();
            head.push(c);
        }
        safe = format!("{head}{extension}");
    }
    safe
}

/// A copy's private folder and its file, created empty.
#[derive(Debug)]
pub struct LocalCopy {
    pub directory: PathBuf,
    pub file: PathBuf,
    pub handle: File,
}

#[cfg(unix)]
fn private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn private_dir(path: &Path) -> std::io::Result<()> {
    DirBuilder::new().create(path)
}

/// Create an owner-only folder under `root` and an empty file named after the remote one.
pub fn create_local_copy(root: &Path, name: &str) -> std::io::Result<LocalCopy> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        DirBuilder::new().recursive(true).mode(0o700).create(root)?;
        // Under a shared /tmp someone else may have created the root first; their
        // folder would let them swap the copy before the program reads it.
        use std::os::unix::fs::MetadataExt;
        let info = fs::symlink_metadata(root)?;
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        if !info.is_dir() || info.uid() != uid || info.mode() & 0o022 != 0 {
            return Err(std::io::Error::other(format!(
                "{} is not a private folder of this user",
                root.display()
            )));
        }
    }
    #[cfg(not(unix))]
    fs::create_dir_all(root)?;
    let directory = make_temp_dir(root)?;
    let file = directory.join(local_copy_file_name(name));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&file) {
        Ok(handle) => Ok(LocalCopy {
            directory,
            file,
            handle,
        }),
        Err(error) => {
            let _ = fs::remove_dir_all(&directory);
            Err(error)
        }
    }
}

/// `mkdtemp(root/open-)`.
fn make_temp_dir(root: &Path) -> std::io::Result<PathBuf> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        ^ u128::from(std::process::id());
    for attempt in 0u128..100 {
        let mut value = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(attempt * 1_442_695_040_888_963_407);
        let suffix: String = (0..6)
            .map(|_| {
                let c = ALPHABET[(value % ALPHABET.len() as u128) as usize] as char;
                value /= ALPHABET.len() as u128;
                c
            })
            .collect();
        let directory = root.join(format!("{LOCAL_COPY_PREFIX}{suffix}"));
        match private_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other("could not create a private folder"))
}

/// Remove copies whose folder and file have both been left alone for `max_age`.
pub fn purge_stale_local_copies(root: &Path, now: SystemTime, max_age: Duration) {
    let Ok(names) = fs::read_dir(root) else {
        return;
    };
    for entry in names.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(LOCAL_COPY_PREFIX)
        {
            continue;
        }
        let directory = entry.path();
        let Ok(info) = fs::symlink_metadata(&directory) else {
            continue;
        };
        if !info.is_dir() {
            continue;
        }
        let mut newest = info.modified().unwrap_or(now);
        if let Ok(children) = fs::read_dir(&directory) {
            for child in children.flatten() {
                if let Ok(modified) = child.metadata().and_then(|meta| meta.modified()) {
                    newest = newest.max(modified);
                }
            }
        }
        // Still in use or already gone; the next purge tries again.
        if now.duration_since(newest).is_ok_and(|age| age > max_age) {
            let _ = fs::remove_dir_all(&directory);
        }
    }
}

/// Where copies live. Strict snaps (Ubuntu's Firefox among them) get a
/// private /tmp and cannot read hidden folders at the top of the home
/// directory, so Linux copies live in a hidden folder inside Downloads.
/// Without a Downloads folder the shared /tmp then needs a per-user name.
pub fn local_copy_root() -> PathBuf {
    if !cfg!(target_os = "linux") {
        return std::env::temp_dir().join("muxus-open");
    }
    let downloads = super::panel::downloads_dir();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    if downloads == home {
        #[cfg(unix)]
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() }.to_string();
        #[cfg(not(unix))]
        let uid = "user".to_string();
        std::env::temp_dir().join(format!("muxus-open-{uid}"))
    } else {
        downloads.join(".muxus-open")
    }
}

/// The environment for programs Muxus starts: without the variables a
/// launcher sets for itself, which would make a launched program group under
/// the Muxus icon or pick the wrong display backend.
pub fn launch_environment(environment: &HashMap<String, String>) -> HashMap<String, String> {
    let mut launched = environment.clone();
    if let Some(original) = launched.remove("ORIGINAL_XDG_CURRENT_DESKTOP") {
        launched.insert("XDG_CURRENT_DESKTOP".into(), original);
    }
    for name in ["CHROME_DESKTOP", "GDK_BACKEND", "NO_AT_BRIDGE"] {
        launched.remove(name);
    }
    launched
}

/// Run an opener (xdg-open, open) that hands the file to another program and
/// exits. A non-zero exit is a failure; an opener still running after
/// `timeout` is assumed to be waiting on the program it started.
pub fn run_opener(command: &str, args: &[&str], timeout: Duration) -> Result<(), String> {
    let environment: HashMap<String, String> = std::env::vars().collect();
    let mut child = Command::new(command)
        .args(args)
        .env_clear()
        .envs(launch_environment(&environment))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{command}: {error}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                let name = Path::new(command)
                    .file_name()
                    .map_or_else(|| command.to_string(), |n| n.to_string_lossy().into_owned());
                return Err(match status.code() {
                    Some(code) => format!("{name} exited with {code}"),
                    None => format!("{name} exited with a signal"),
                });
            }
            Ok(None) if started.elapsed() >= timeout => return Ok(()),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// Open a local copy with the desktop's default program, or the one picked.
pub fn open_local_copy(file: &Path, target: &LocalOpenTarget) -> Result<(), String> {
    let path = file.to_string_lossy().into_owned();
    match target {
        LocalOpenTarget::Default => {
            if cfg!(target_os = "macos") {
                run_opener("open", &[&path], Duration::from_secs(10))
            } else if cfg!(windows) {
                run_opener("cmd", &["/C", "start", "", &path], Duration::from_secs(10))
            } else {
                run_opener("xdg-open", &[&path], Duration::from_secs(10))
            }
        }
        LocalOpenTarget::SystemChooser => {
            if !cfg!(windows) {
                return Err("No system chooser on this platform".into());
            }
            // OpenAs_RunDLL takes the rest of the command line, unquoted, as the path.
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
            let rundll32 = format!("{root}\\System32\\rundll32.exe");
            Command::new(rundll32)
                .arg(format!("shell32.dll,OpenAs_RunDLL {path}"))
                .spawn()
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
        LocalOpenTarget::Application { id } => {
            if let Some(program) = id.strip_prefix(super::open_with::PICKED_PREFIX) {
                return Command::new(program)
                    .arg(&path)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map(|_| ())
                    .map_err(|error| error.to_string());
            }
            let environment: HashMap<String, String> = std::env::vars().collect();
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let catalog = super::linux_apps::LinuxApplicationCatalog::read(&environment, &home);
            let application = catalog
                .find(id)
                .ok_or_else(|| "The program is no longer installed".to_string())?;
            super::linux_apps::launch(application, &path)
        }
    }
}

/// Size, modification time and content hash of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSignature {
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// SHA-256 of the contents, hex.
    pub hash: String,
}

pub fn file_signature(file: &Path) -> std::io::Result<FileSignature> {
    let info = fs::metadata(file)?;
    let mut hasher = Sha256::new();
    let mut reader = File::open(file)?;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let hash = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(FileSignature {
        size: info.len(),
        modified: info.modified().ok(),
        hash,
    })
}

/// Whether a copy's file changed contents since `signature`; updates it.
/// Saves that leave the contents as they were (a touch, a rewrite of the
/// same bytes) do not count; a file missing in the middle of a rename-style
/// save is looked at again on the next check.
pub fn check_copy(file: &Path, signature: &mut FileSignature) -> bool {
    let Ok(info) = fs::metadata(file) else {
        return false;
    };
    if !info.is_file() {
        return false;
    }
    if info.len() == signature.size && info.modified().ok() == signature.modified {
        return false;
    }
    let Ok(next) = file_signature(file) else {
        return false;
    };
    let changed = next.hash != signature.hash;
    *signature = next;
    changed
}

/// A remote file fetched for opening locally.
pub struct DownloadedFile {
    pub conn_id: String,
    pub remote_path: String,
    pub name: String,
    pub host: Option<String>,
    pub remote_mtime_ms: Option<i64>,
    pub bytes: Vec<u8>,
}

thread_local! {
    static NEXT_COPY: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// `openDownloadedFile` and the desktop's `openLocalCopy`: write the copy,
/// open it, and offer later saves for upload.
pub fn open_downloaded_file(download: DownloadedFile, target: LocalOpenTarget, cx: &mut App) {
    let root = local_copy_root();
    let name = download.name.clone();
    let task = cx.background_executor().spawn(async move {
        purge_stale_local_copies(&root, SystemTime::now(), LOCAL_COPY_MAX_AGE);
        let mut copy = create_local_copy(&root, &download.name).map_err(|error| {
            format!(
                "Could not create a local copy of {}: {error}",
                download.name
            )
        })?;
        if let Err(error) = copy.handle.write_all(&download.bytes) {
            let _ = fs::remove_dir_all(&copy.directory);
            return Err(format!(
                "Could not write the local copy of {}: {error}",
                download.name
            ));
        }
        drop(copy.handle);
        let signature = file_signature(&copy.file).map_err(|error| error.to_string())?;
        open_local_copy(&copy.file, &target)
            .map_err(|message| format!("Could not open {}: {message}", download.name))?;
        Ok((copy.file, signature, download))
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok((file, signature, download)) => {
                let id = NEXT_COPY.with(|next| {
                    let id = next.get();
                    next.set(id + 1);
                    format!("copy-{id}")
                });
                let copy = TrackedLocalCopy {
                    id: id.clone(),
                    conn_id: download.conn_id,
                    remote_path: download.remote_path,
                    name: download.name,
                    host: download.host,
                    remote_mtime_ms: download.remote_mtime_ms,
                    auto_upload: false,
                };
                services(cx).local_copies.update(cx, |copies, cx| {
                    copies.track(copy, file.clone());
                    cx.notify();
                });
                watch_local_copy(id, file, signature, cx);
            }
            Err(message) => super::services::notify(Status::Error, message, cx),
        });
    })
    .detach();
    let _ = name;
}

/// Report saves into the copy (polling its size and time, then its contents).
fn watch_local_copy(id: String, file: PathBuf, signature: FileSignature, cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut signature = signature;
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(700))
                .await;
            let tracked =
                cx.update(|cx| services(cx).local_copies.read(cx).copies.contains_key(&id));
            if !tracked || !file.exists() {
                break;
            }
            let check_file = file.clone();
            let current = signature.clone();
            let (changed, next) = cx
                .background_executor()
                .spawn(async move {
                    let mut next = current;
                    let changed = check_copy(&check_file, &mut next);
                    (changed, next)
                })
                .await;
            signature = next;
            if changed {
                let change = LocalCopyChange {
                    id: id.clone(),
                    file: file.display().to_string(),
                };
                let env = Rc::new(AppLocalCopies { cx: cx.clone() });
                cx.spawn(async move |_| local_copy_changed(change, env).await)
                    .detach();
            }
        }
    })
    .detach();
}

/// [`LocalCopySyncEnv`] on the app's services.
struct AppLocalCopies {
    cx: AsyncApp,
}

impl AppLocalCopies {
    fn read<T>(&self, f: impl FnOnce(&super::local_copies::LocalCopies) -> T) -> Option<T> {
        Some(self.cx.update(|cx| f(services(cx).local_copies.read(cx))))
    }

    fn write<T>(&self, f: impl FnOnce(&mut super::local_copies::LocalCopies) -> T) -> Option<T> {
        Some(self.cx.update(|cx| {
            services(cx).local_copies.update(cx, |copies, cx| {
                cx.notify();
                f(copies)
            })
        }))
    }
}

/// The app's window-level question host, installed by [`install_dialog_host`].
struct GlobalDialogHost(muxus_ui::gpui_kit::Entity<DialogHost>);

impl muxus_ui::gpui_kit::Global for GlobalDialogHost {}

/// Where questions that outlive their view are asked (see [`DialogHost`]).
pub fn install_dialog_host(host: muxus_ui::gpui_kit::Entity<DialogHost>, cx: &mut App) {
    cx.set_global(GlobalDialogHost(host));
}

/// Ask through the installed [`DialogHost`]; unanswered (declined) without one.
pub fn ask(options: ConfirmOptions, cx: &mut App) -> LocalFuture<ConfirmAnswer> {
    match cx.try_global::<GlobalDialogHost>() {
        Some(host) => {
            let host = host.0.clone();
            DialogHost::ask(&host, options, cx)
        }
        None => Box::pin(async { ConfirmAnswer::default() }),
    }
}

impl LocalCopySyncEnv for AppLocalCopies {
    fn copy(&self, id: &str) -> Option<TrackedLocalCopy> {
        self.read(|copies| copies.copies.get(id).cloned()).flatten()
    }

    fn set_auto_upload(&self, id: &str) {
        self.write(|copies| {
            if let Some(copy) = copies.copies.get_mut(id) {
                copy.auto_upload = true;
            }
        });
    }

    fn set_remote_mtime(&self, id: &str, mtime_ms: Option<i64>) {
        self.write(|copies| {
            if let Some(copy) = copies.copies.get_mut(id) {
                copy.remote_mtime_ms = mtime_ms;
            }
        });
    }

    fn upload_state(&self) -> Option<LocalCopyUpload> {
        self.read(|copies| copies.upload.clone()).flatten()
    }

    fn set_upload(&self, upload: Option<LocalCopyUpload>) {
        self.write(|copies| copies.upload = upload);
    }

    fn with_queue<T>(&self, f: impl FnOnce(&mut SyncQueue) -> T) -> T {
        let mut f = Some(f);
        let mut result = None;
        self.write(|copies| result = f.take().map(|f| f(copies.queue())));
        match result {
            Some(result) => result,
            None => f.expect("the app is gone")(&mut SyncQueue::default()),
        }
    }

    fn confirm(&self, options: ConfirmOptions) -> LocalFuture<ConfirmAnswer> {
        self.cx.update(|cx| ask(options, cx))
    }

    fn upload(
        &self,
        conn_id: &str,
        remote_path: &str,
        expected_mtime_ms: Option<i64>,
        source: PathBuf,
        _cancel: CancelSignal,
        mut on_step: Box<dyn FnMut(UploadStep)>,
    ) -> LocalFuture<Result<SftpUploadResponse, ApiError>> {
        let SftpServices { backend, .. } = self.cx.update(|cx| services(cx));
        let size = fs::metadata(&source).map(|meta| meta.len()).ok();
        let (conn_id, remote_path) = (conn_id.to_string(), remote_path.to_string());
        let (steps_tx, steps_rx) = async_channel::unbounded::<UploadStep>();
        let cx = self.cx.clone();
        Box::pin(async move {
            let forward = cx.spawn(async move |_| {
                while let Ok(step) = steps_rx.recv().await {
                    on_step(step);
                }
            });
            let result = backend
                .sftp_upload(
                    &conn_id,
                    &remote_path,
                    UploadOptions {
                        overwrite: true,
                        expected_mtime_ms: expected_mtime_ms.map(|ms| ms as f64),
                        size,
                    },
                    source,
                    move |step| {
                        let _ = steps_tx.try_send(step);
                    },
                )
                .await;
            forward.await;
            result
        })
    }

    fn refresh(&self, conn_id: &str) {
        let conn = conn_id.to_string();
        self.cx.update(|cx| {
            services(cx)
                .listings
                .update(cx, |listings, cx| listings.refresh(&conn, cx))
        });
    }

    fn notify(&self, status: Status, message: String, details: Option<String>) {
        self.cx.update(|cx| {
            let notify = cx.global::<SftpServices>().notify.clone();
            notify(status, message.into(), details.map(Into::into), cx);
        });
    }

    fn hide_after(&self, attempt: u64, delay: Duration) {
        self.cx.update(|cx| {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(delay).await;
                cx.update(|cx| {
                    services(cx).local_copies.update(cx, |copies, cx| {
                        if copies
                            .upload
                            .as_ref()
                            .is_some_and(|upload| upload.attempt == attempt)
                        {
                            copies.upload = None;
                            cx.notify();
                        }
                    })
                });
            })
            .detach();
        });
    }
}

// Keep the error helpers in use for platforms whose openers report through them.
#[allow(dead_code)]
fn report(error: &ApiError, cx: &mut App) {
    let _ = error_details(error);
    notify_error(error, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_ordinary_remote_names_intact() {
        assert_eq!(local_copy_file_name("plan.drawio"), "plan.drawio");
        assert_eq!(
            local_copy_file_name("Quarterly report (final).pdf"),
            "Quarterly report (final).pdf"
        );
        assert_eq!(local_copy_file_name("übersicht.html"), "übersicht.html");
    }

    #[test]
    fn never_lets_a_name_leave_its_folder_or_break_on_windows() {
        assert_eq!(local_copy_file_name("../../etc/passwd"), "passwd");
        assert_eq!(local_copy_file_name("..\\..\\boot.ini"), "boot.ini");
        assert_eq!(local_copy_file_name(".."), "file");
        assert_eq!(local_copy_file_name(""), "file");
        assert_eq!(
            local_copy_file_name("a<b>c:d\"e|f?g*h.txt"),
            "a_b_c_d_e_f_g_h.txt"
        );
        assert_eq!(local_copy_file_name("tab\there\u{1}.txt"), "tab_here_.txt");
        assert_eq!(local_copy_file_name("notes. . "), "notes");
        assert_eq!(local_copy_file_name("CON"), "_CON");
        assert_eq!(local_copy_file_name("lpt1.log"), "_lpt1.log");
        assert_eq!(local_copy_file_name("console.log"), "console.log");
    }

    #[test]
    fn shortens_long_names_but_keeps_the_extension_that_picks_the_program() {
        let name = local_copy_file_name(&format!("{}.drawio", "x".repeat(300)));
        assert_eq!(name.len(), 180);
        assert!(name.ends_with(".drawio"));
    }

    #[test]
    fn gives_every_copy_a_private_folder_so_the_remote_name_can_repeat() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("muxus-open");
        let mut first = create_local_copy(&root, "index.html").unwrap();
        let second = create_local_copy(&root, "index.html").unwrap();
        first.handle.write_all(b"one").unwrap();
        assert_ne!(first.directory, second.directory);
        assert_eq!(first.file.file_name().unwrap(), "index.html");
        assert_eq!(first.file.parent().unwrap(), first.directory);
        assert_eq!(fs::read_to_string(&first.file).unwrap(), "one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode(&root) & 0o777, 0o700);
            assert_eq!(mode(&first.directory) & 0o777, 0o700);
            assert_eq!(mode(&first.file) & 0o077, 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_root_others_can_write_to() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().unwrap();
        let shared = scratch.path().join("muxus-open");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        let error = create_local_copy(&shared, "a.txt").unwrap_err();
        assert!(error.to_string().contains("is not a private folder"));
    }

    #[test]
    fn removes_only_copies_left_alone_for_longer_than_the_limit() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("muxus-open");
        let old = create_local_copy(&root, "old.txt").unwrap();
        let fresh = create_local_copy(&root, "fresh.txt").unwrap();
        let long_ago = SystemTime::now() - LOCAL_COPY_MAX_AGE - Duration::from_secs(60);
        drop(old.handle);
        File::options()
            .write(true)
            .open(&old.file)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        File::open(&old.directory)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        purge_stale_local_copies(&root, SystemTime::now(), LOCAL_COPY_MAX_AGE);
        assert!(!old.directory.exists());
        assert!(fresh.directory.exists());
        fs::write(root.join("open-not-a-dir"), "x").unwrap();
        purge_stale_local_copies(
            &root,
            SystemTime::now() + LOCAL_COPY_MAX_AGE * 2,
            LOCAL_COPY_MAX_AGE,
        );
        assert!(root.join("open-not-a-dir").exists());
    }

    #[test]
    fn tolerates_a_missing_root() {
        purge_stale_local_copies(
            Path::new("/nonexistent/muxus-open"),
            SystemTime::now(),
            LOCAL_COPY_MAX_AGE,
        );
    }

    #[test]
    fn drops_launcher_variables_from_launched_programs() {
        let environment: HashMap<String, String> = [
            ("ORIGINAL_XDG_CURRENT_DESKTOP", "GNOME"),
            ("XDG_CURRENT_DESKTOP", "Unity"),
            ("CHROME_DESKTOP", "muxus.desktop"),
            ("GDK_BACKEND", "x11"),
            ("NO_AT_BRIDGE", "1"),
            ("PATH", "/usr/bin"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let launched = launch_environment(&environment);
        assert_eq!(
            launched.get("XDG_CURRENT_DESKTOP").map(String::as_str),
            Some("GNOME")
        );
        assert!(!launched.contains_key("ORIGINAL_XDG_CURRENT_DESKTOP"));
        assert!(!launched.contains_key("CHROME_DESKTOP"));
        assert!(!launched.contains_key("GDK_BACKEND"));
        assert!(!launched.contains_key("NO_AT_BRIDGE"));
        assert_eq!(launched.get("PATH").map(String::as_str), Some("/usr/bin"));
    }

    #[cfg(unix)]
    #[test]
    fn reports_how_an_opener_exited() {
        assert_eq!(run_opener("true", &[], Duration::from_secs(5)), Ok(()));
        assert_eq!(
            run_opener("sh", &["-c", "exit 4"], Duration::from_secs(5)),
            Err("sh exited with 4".into())
        );
        let started = Instant::now();
        assert_eq!(
            run_opener("sleep", &["5"], Duration::from_millis(200)),
            Ok(())
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn reports_a_save_with_new_contents_once() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("plan.drawio");
        fs::write(&file, "one").unwrap();
        let mut signature = file_signature(&file).unwrap();
        assert!(!check_copy(&file, &mut signature));
        fs::write(&file, "two!").unwrap();
        assert!(check_copy(&file, &mut signature));
        assert!(!check_copy(&file, &mut signature));
        // A rename-style save: a new file moved over the copy.
        let staged = scratch.path().join(".plan.drawio.tmp");
        fs::write(&staged, "three").unwrap();
        fs::rename(&staged, &file).unwrap();
        assert!(check_copy(&file, &mut signature));
        // The same contents again is no change.
        fs::write(&file, "three").unwrap();
        assert!(!check_copy(&file, &mut signature));
    }

    #[test]
    fn hashes_the_contents_into_a_signature() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("a.txt");
        fs::write(&file, "abc").unwrap();
        let signature = file_signature(&file).unwrap();
        assert_eq!(signature.size, 3);
        assert_eq!(
            signature.hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
