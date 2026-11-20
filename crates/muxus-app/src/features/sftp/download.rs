//! Saving several downloaded files at once, and folders with everything in them (port of
//! `client/src/sftp-download.ts` and the download half of `client/src/api/transfers.ts`). One
//! file asks where to save it; several files, or any folder, ask for a folder once and
//! everything is written into it, without replacing anything there: a name already taken gets
//! a number, as browsers number repeated downloads.
//!
//! A download that stops early keeps what it received, so that downloading the file again
//! asks only for the rest.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use muxus_core::api::{SftpEntry, SftpEntryType, SftpListResponse};
use muxus_session::routes::sftp::SftpDownloadStream;
use muxus_session::{ApiError, ApiResult};
use muxus_ssh::sftp::upload::{TransferPhase, join_remote_path};
use muxus_ssh::sftp::{ByteProgress, DownloadRange, ProgressMeter};

use super::format::format_size;
use super::transfer::{StopReason, TransferDirection, TransferProgressState, TransferStop};

/// Characters Windows refuses in a file name, path separators included.
const UNSAFE_NAME_CHARACTERS: &str = "\\/:*?\"<>|";

/// A remote name that every local file system accepts; browsers clean download names the same
/// way.
pub fn local_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if UNSAFE_NAME_CHARACTERS.contains(character) || (character as u32) < 32 {
                '_'
            } else {
                character
            }
        })
        .collect();
    // Windows cannot keep trailing dots and spaces on a name.
    let cleaned = cleaned.trim_end_matches(['.', ' ']);
    if cleaned.is_empty() {
        "download".into()
    } else {
        cleaned.into()
    }
}

/// "report.txt", then "report (1).txt", "report (2).txt" …; a dotfile keeps its leading dot.
pub fn numbered_file_name(name: &str, copy: usize) -> String {
    if copy == 0 {
        return name.into();
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} ({copy}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({copy})"),
    }
}

/// Write `bytes` into `folder` without replacing anything there: a taken name (a file or a
/// folder) gets a number. Returns the name written.
pub fn save_into_folder(folder: &Path, name: &str, bytes: &[u8]) -> io::Result<String> {
    let base = local_file_name(name);
    refuse_reserved_name(&base)?;
    for copy in 0.. {
        let candidate = numbered_file_name(&base, copy);
        let path = folder.join(&candidate);
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            // Windows answers "access denied", not "already exists", for a name a folder holds.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists || path.exists() => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        return Ok(candidate);
    }
    unreachable!("a free name is always found")
}

/// What a skipped item's reason says of a name the local folder does not take.
const REFUSED_NAME: &str = "name not allowed locally";

/// Whether Windows keeps `name` for a device, whatever follows its first dot: `aux.c` and
/// `con` are the console and the auxiliary port there, not files. Opening one would write to
/// the device, or wait on it.
pub fn reserved_on_windows(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let stem = stem.trim_end_matches(' ').to_ascii_lowercase();
    if matches!(stem.as_str(), "con" | "prn" | "aux" | "nul" | "clock$") {
        return true;
    }
    ["com", "lpt"].iter().any(|port| {
        stem.strip_prefix(port).is_some_and(|digit| {
            matches!(digit, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
    })
}

/// Turns down a name this system keeps for a device, as the local folder would.
fn refuse_reserved_name(name: &str) -> io::Result<()> {
    if cfg!(windows) && reserved_on_windows(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidFilename, REFUSED_NAME));
    }
    Ok(())
}

/// Whether `error` turns the name down rather than the write: such an item is skipped, not
/// renamed, and the download goes on.
pub fn is_refused_name(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::InvalidFilename
}

/// Create a folder in `parent` without merging into anything there: a taken name (a file or a
/// folder) gets a number. Returns the folder created.
pub fn create_folder_in(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let base = local_file_name(name);
    refuse_reserved_name(&base)?;
    for copy in 0.. {
        // A folder name has no extension to keep after the number.
        let candidate = if copy == 0 {
            base.clone()
        } else {
            format!("{base} ({copy})")
        };
        let path = parent.join(candidate);
        if std::fs::symlink_metadata(&path).is_ok() {
            continue;
        }
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!("a free name is always found")
}

// --- continuing interrupted downloads ----------------------------------------------------

/// What downloads that stopped early may keep in memory, in all.
pub const MAX_INTERRUPTED_DOWNLOAD_BYTES: usize = 256 * 1024 * 1024;

/// What a download that stopped early received, and the version of the file it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialDownload {
    pub bytes: Vec<u8>,
    pub etag: String,
}

/// What downloads that stopped early received, by remote path, so that downloading the file
/// again continues them. They are held in memory, so only the newest are kept, up to
/// [`MAX_INTERRUPTED_DOWNLOAD_BYTES`] in all.
#[derive(Debug)]
pub struct InterruptedDownloads {
    /// Oldest first.
    parts: Vec<(String, PartialDownload)>,
    limit: usize,
}

impl Default for InterruptedDownloads {
    fn default() -> Self {
        Self::with_limit(MAX_INTERRUPTED_DOWNLOAD_BYTES)
    }
}

impl InterruptedDownloads {
    pub fn with_limit(limit: usize) -> Self {
        Self {
            parts: Vec::new(),
            limit,
        }
    }

    /// How much of the remote `file` an interrupted download left.
    pub fn kept(&self, file: &str) -> Option<&PartialDownload> {
        self.parts
            .iter()
            .find(|(kept, _)| kept == file)
            .map(|(_, partial)| partial)
    }

    fn take(&mut self, file: &str) -> Option<PartialDownload> {
        let index = self.parts.iter().position(|(kept, _)| kept == file)?;
        Some(self.parts.remove(index).1)
    }

    /// `keepInterruptedDownload`: the newest stay, the oldest go once the limit is passed, and
    /// a part larger than the limit is not kept at all.
    pub fn keep(&mut self, file: &str, partial: PartialDownload) {
        self.forget(file);
        if partial.bytes.len() > self.limit {
            return;
        }
        self.parts.push((file.to_string(), partial));
        let mut kept: usize = self.parts.iter().map(|(_, part)| part.bytes.len()).sum();
        while kept > self.limit && !self.parts.is_empty() {
            kept -= self.parts.remove(0).1.bytes.len();
        }
    }

    /// A download of the remote `file` finished; nothing of it is left to continue.
    pub fn forget(&mut self, file: &str) {
        self.parts.retain(|(kept, _)| kept != file);
    }
}

/// The window's interrupted downloads, shared by its file browsers.
pub type SharedInterruptedDownloads = Rc<RefCell<InterruptedDownloads>>;

/// One download's hold on what is kept of its remote file (`downloadResume`). It takes what an
/// interrupted download received, so the server is asked only for the rest, and when it goes
/// without [`forget`](Self::forget) it leaves what it has for the next download: unless Cancel
/// stopped it, which also drops what an earlier attempt received, even before an answer
/// arrives.
pub struct DownloadResume {
    file: String,
    store: SharedInterruptedDownloads,
    stop: TransferStop,
    etag: Option<String>,
    received: Vec<u8>,
}

impl DownloadResume {
    pub fn new(file: &str, store: &SharedInterruptedDownloads, stop: &TransferStop) -> Self {
        let partial = store.borrow_mut().take(file);
        let (etag, received) = match partial {
            Some(partial) => (Some(partial.etag), partial.bytes),
            None => (None, Vec::new()),
        };
        Self {
            file: file.to_string(),
            store: store.clone(),
            stop: stop.clone(),
            etag,
            received,
        }
    }

    /// The bytes an earlier attempt received: only the rest is asked for, unless the file
    /// changed.
    pub fn range(&self) -> Option<DownloadRange> {
        let etag = self.etag.clone()?;
        (!self.received.is_empty()).then_some(DownloadRange {
            start: self.received.len() as u64,
            etag,
        })
    }

    /// Everything an attempt has when it stops early (`onInterrupted`).
    pub fn interrupted(&mut self, partial: PartialDownload) {
        self.etag = Some(partial.etag);
        self.received = partial.bytes;
    }

    /// The server sends the whole file: what was kept came from another version of it.
    fn start_over(&mut self, etag: Option<String>) {
        self.received.clear();
        self.etag = etag;
    }

    /// The whole file arrived.
    fn complete(&mut self) -> Vec<u8> {
        self.etag = None;
        std::mem::take(&mut self.received)
    }

    /// The download finished; nothing of the file is left to continue.
    pub fn forget(&mut self) {
        self.etag = None;
        self.received.clear();
    }
}

impl Drop for DownloadResume {
    fn drop(&mut self) {
        let Some(etag) = self.etag.take() else {
            return;
        };
        if self.received.is_empty() || self.stop.reason() == Some(StopReason::Cancelled) {
            return;
        }
        let bytes = std::mem::take(&mut self.received);
        self.store
            .borrow_mut()
            .keep(&self.file, PartialDownload { bytes, etag });
    }
}

/// A remote file as the server streams it (the response of `/download`).
pub trait DownloadStream {
    /// The byte the stream starts at: past 0 only where the asked range was honoured (206).
    fn start(&self) -> u64;
    /// The whole file's size, when the server reported one.
    fn size(&self) -> Option<u64>;
    fn etag(&self) -> Option<String>;
    fn mtime_ms(&self) -> Option<i64>;
    /// The next piece of the file; `None` at its end.
    fn next_chunk(&mut self) -> impl Future<Output = Option<ApiResult<Vec<u8>>>>;
}

impl DownloadStream for SftpDownloadStream {
    fn start(&self) -> u64 {
        self.start
    }

    fn size(&self) -> Option<u64> {
        self.size
    }

    fn etag(&self) -> Option<String> {
        self.etag.clone()
    }

    fn mtime_ms(&self) -> Option<i64> {
        self.mtime_ms
    }

    fn next_chunk(&mut self) -> impl Future<Output = Option<ApiResult<Vec<u8>>>> {
        SftpDownloadStream::next_chunk(self)
    }
}

enum Step {
    Chunk(Option<ApiResult<Vec<u8>>>),
    Stopped,
}

/// Stream a download while retaining byte-level progress (`downloadBlobWithProgress`). What
/// `resume` holds of an interrupted download is asked to be continued; the server sends the
/// rest, or the whole file once it changed. `on_head` sees the answer before its first byte.
/// Stopped early, the bytes received stay with `resume`.
pub async fn download_with_progress<S: DownloadStream>(
    open: impl AsyncFnOnce(Option<DownloadRange>) -> ApiResult<S>,
    resume: &mut DownloadResume,
    stop: &TransferStop,
    on_head: impl FnOnce(&S),
    on_progress: &mut dyn FnMut(ByteProgress),
) -> ApiResult<Vec<u8>> {
    stop.check()?;
    let range = resume.range();
    let mut stream = open(range.clone()).await?;
    on_head(&stream);
    let continued = range.is_some_and(|range| stream.start() > 0 && stream.start() == range.start);
    if continued {
        resume.etag = stream.etag();
    } else if stream.start() > 0 {
        return Err(ApiError::internal(
            "the download does not start where it was asked to",
        ));
    } else {
        resume.start_over(stream.etag());
    }
    let offset = resume.received.len() as u64;
    let total = stream.size();
    let mut meter = ProgressMeter::starting_at(offset);
    if let Some(progress) = meter.report(offset, total, true) {
        on_progress(progress);
    }
    loop {
        let step =
            futures_lite::future::or(async { Step::Chunk(stream.next_chunk().await) }, async {
                stop.stopped().await;
                Step::Stopped
            })
            .await;
        match step {
            Step::Chunk(Some(Ok(chunk))) => {
                resume.received.extend_from_slice(&chunk);
                let loaded = resume.received.len() as u64;
                if let Some(progress) = meter.report(loaded, total, false) {
                    on_progress(progress);
                }
            }
            Step::Chunk(Some(Err(error))) => return Err(error),
            Step::Chunk(None) => break,
            Step::Stopped => return stop.check().map(|()| Vec::new()),
        }
    }
    let bytes = resume.complete();
    let loaded = bytes.len() as u64;
    if let Some(progress) = meter.report(loaded, total.or(Some(loaded)), true) {
        on_progress(progress);
    }
    Ok(bytes)
}

// --- folders -----------------------------------------------------------------------------

// A download with folders is listed in full before anything is saved, and refused past these.
pub const MAX_FOLDER_DOWNLOAD_ENTRIES: usize = 10_000;
pub const MAX_FOLDER_DOWNLOAD_BYTES: u64 = 10 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownloadResult {
    /// Files saved, out of `file_count`.
    pub downloaded: usize,
    pub file_count: usize,
    /// Folders created, the selected ones included.
    pub folders: usize,
    /// What was left out, by path below the destination, each with the reason.
    pub skipped: Vec<String>,
    /// Cancel stopped the download; what was saved until then stays.
    pub cancelled: bool,
}

/// The question asked before a download with folders starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadQuestion {
    pub title: String,
    pub description: String,
    pub confirm_label: String,
}

/// A row of the listing, as selected in the file browser, with its remote path.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadTarget {
    pub entry: SftpEntry,
    pub path: String,
}

/// What a download needs from the outside: the remote side, the user and the local folder.
pub trait DownloadDeps {
    fn list(&self, conn_id: &str, path: &str) -> impl Future<Output = ApiResult<SftpListResponse>>;

    /// One whole file. What `resume` holds of an interrupted download of it is continued, and
    /// what this one receives stays with `resume` if it stops early.
    fn download(
        &self,
        conn_id: &str,
        path: &str,
        resume: &mut DownloadResume,
        on_progress: &mut dyn FnMut(ByteProgress),
    ) -> impl Future<Output = ApiResult<Vec<u8>>>;

    /// Asks the user; `true` starts the download.
    fn confirm(&self, question: DownloadQuestion) -> impl Future<Output = bool>;

    /// [`create_folder_in`].
    fn create_folder(
        &self,
        parent: &Path,
        name: &str,
    ) -> impl Future<Output = io::Result<PathBuf>> {
        std::future::ready(create_folder_in(parent, name))
    }

    /// [`save_into_folder`].
    fn save(
        &self,
        folder: &Path,
        name: &str,
        bytes: Vec<u8>,
    ) -> impl Future<Output = io::Result<String>> {
        std::future::ready(save_into_folder(folder, name, &bytes))
    }
}

struct RemoteItem {
    path: String,
    /// Path below the destination, with `/` separators.
    relative_path: String,
    size: u64,
}

fn plural(count: usize, one: &str) -> String {
    format!("{count} {one}{}", if count == 1 { "" } else { "s" })
}

fn parent_of(relative_path: &str) -> &str {
    relative_path
        .rfind('/')
        .map_or("", |slash| &relative_path[..slash])
}

fn name_of(relative_path: &str) -> &str {
    relative_path
        .rfind('/')
        .map_or(relative_path, |slash| &relative_path[slash + 1..])
}

/// A single path element; any other name in a listing could reach outside the folder, here or
/// on the server.
fn is_entry_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

/// The folder's own name, for messages.
pub fn folder_name(folder: &Path) -> Option<String> {
    folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

/// `error`, its message led by what failed and where; a stop stays as is, so the caller can
/// tell why it stopped.
fn could_not(action: &str, relative_path: &str, error: ApiError, stop: &TransferStop) -> ApiError {
    if stop.is_stopped() {
        return error;
    }
    ApiError {
        message: format!("Could not {action} {relative_path}: {}", error.message),
        ..error
    }
}

fn local_failure(
    action: &str,
    relative_path: &str,
    error: &io::Error,
    stop: &TransferStop,
) -> ApiError {
    could_not(
        action,
        relative_path,
        ApiError::internal(error.to_string()),
        stop,
    )
}

/// What a folder download reads and reports with.
pub struct DownloadOptions<'a, D: DownloadDeps> {
    pub stop: &'a TransferStop,
    pub interrupted: &'a SharedInterruptedDownloads,
    pub on_progress: &'a mut dyn FnMut(&TransferProgressState),
    pub deps: &'a D,
}

/// Download files, and folders with everything below them, into `destination`. Folders are
/// listed in full first, refused past the limits and confirmed with their totals. Links,
/// special files and names that are not a single entry or that the local folder refuses are
/// skipped. A file an earlier download left unfinished continues where it stopped. `None`
/// when the user declines, and what was saved after Cancel; stopped otherwise, it fails, a
/// failure naming the item it stopped at.
pub async fn download_from_remote<D: DownloadDeps>(
    conn_id: &str,
    targets: &[DownloadTarget],
    destination: &Path,
    options: DownloadOptions<'_, D>,
) -> ApiResult<Option<DownloadResult>> {
    let stop = options.stop;
    let mut result = DownloadResult::default();
    let run = Download {
        conn_id,
        destination,
        stop,
        interrupted: options.interrupted,
        on_progress: options.on_progress,
        deps: options.deps,
        last: TransferProgressState {
            direction: TransferDirection::Download,
            name: targets.first().map_or_else(
                || "Download".to_string(),
                |target| target.entry.name.clone(),
            ),
            loaded: 0,
            total: None,
            bytes_per_second: 0.,
            phase: TransferPhase::Preparing,
            file_index: Some(0),
            file_count: Some(0),
        },
    };
    match run.run(targets, &mut result).await {
        Ok(true) => Ok(Some(result)),
        Ok(false) => Ok(None),
        // Any other stop fails, so the caller can tell a closed browser from a lost connection.
        Err(_) if stop.reason() == Some(StopReason::Cancelled) => Ok(Some(DownloadResult {
            cancelled: true,
            ..result
        })),
        Err(error) => Err(error),
    }
}

struct Download<'a, D: DownloadDeps> {
    conn_id: &'a str,
    destination: &'a Path,
    stop: &'a TransferStop,
    interrupted: &'a SharedInterruptedDownloads,
    on_progress: &'a mut dyn FnMut(&TransferProgressState),
    deps: &'a D,
    last: TransferProgressState,
}

impl<D: DownloadDeps> Download<'_, D> {
    fn report(&mut self) {
        (self.on_progress)(&self.last);
    }

    /// `false` when the user declined.
    async fn run(
        mut self,
        targets: &[DownloadTarget],
        result: &mut DownloadResult,
    ) -> ApiResult<bool> {
        let mut files: Vec<RemoteItem> = Vec::new();
        // Folders found while listing are appended, so parents come before their children.
        let mut folders: Vec<RemoteItem> = Vec::new();
        let mut directories: Vec<String> = Vec::new();
        let mut total_bytes = 0u64;
        for target in targets {
            let item = |size| RemoteItem {
                path: target.path.clone(),
                relative_path: target.entry.name.clone(),
                size,
            };
            if !is_entry_name(&target.entry.name) {
                result
                    .skipped
                    .push(format!("{} (invalid name)", target.entry.name));
            } else if target.entry.kind == SftpEntryType::File {
                let size = target.entry.size.unwrap_or(0);
                total_bytes += size;
                files.push(item(size));
            } else if target.entry.kind == SftpEntryType::Dir {
                folders.push(item(0));
            }
        }
        self.report();
        let mut entries = targets.len();
        let mut listed: HashSet<String> = HashSet::new();
        let mut index = 0;
        while index < folders.len() {
            let (folder_path, folder_relative) = (
                folders[index].path.clone(),
                folders[index].relative_path.clone(),
            );
            index += 1;
            self.last.name = folder_relative.clone();
            self.report();
            let listing = self
                .deps
                .list(self.conn_id, &folder_path)
                .await
                .map_err(|error| could_not("list", &folder_relative, error, self.stop))?;
            // A server that lists a link as the folder it points to must not lead back into
            // the download.
            if !listed.insert(listing.path.clone()) {
                result.skipped.push(format!("{folder_relative} (link)"));
                continue;
            }
            directories.push(folder_relative.clone());
            for entry in &listing.entries {
                let relative_path = format!("{folder_relative}/{}", entry.name);
                let child = |size| RemoteItem {
                    path: join_remote_path(&listing.path, &entry.name),
                    relative_path: relative_path.clone(),
                    size,
                };
                if !is_entry_name(&entry.name) {
                    result
                        .skipped
                        .push(format!("{relative_path} (invalid name)"));
                    continue;
                }
                match entry.kind {
                    SftpEntryType::Dir => folders.push(child(0)),
                    SftpEntryType::File => {
                        let size = entry.size.unwrap_or(0);
                        total_bytes += size;
                        files.push(child(size));
                    }
                    SftpEntryType::Link => result.skipped.push(format!("{relative_path} (link)")),
                    SftpEntryType::Other => result
                        .skipped
                        .push(format!("{relative_path} (special file)")),
                }
            }
            entries += listing.entries.len();
            if entries > MAX_FOLDER_DOWNLOAD_ENTRIES {
                return Err(ApiError::internal(
                    "More than 10,000 files and folders to download. Download fewer or smaller folders.",
                ));
            }
            if total_bytes > MAX_FOLDER_DOWNLOAD_BYTES {
                return Err(ApiError::internal(format!(
                    "More than {} to download. Download fewer or smaller folders.",
                    format_size(Some(MAX_FOLDER_DOWNLOAD_BYTES))
                )));
            }
        }

        if !folders.is_empty() {
            let (created, skipped) = (directories.len(), result.skipped.len());
            let mut description = format!(
                "{} {} created in {}.",
                plural(created, "folder"),
                if created == 1 { "is" } else { "are" },
                folder_name(self.destination)
                    .unwrap_or_else(|| self.destination.display().to_string()),
            );
            if skipped > 0 {
                description.push_str(&format!(
                    " {} {} skipped.",
                    plural(skipped, "item"),
                    if skipped == 1 { "is" } else { "are" }
                ));
            }
            let confirmed = self
                .deps
                .confirm(DownloadQuestion {
                    title: format!(
                        "Download {} ({})?",
                        plural(files.len(), "file"),
                        format_size(Some(total_bytes))
                    ),
                    description,
                    confirm_label: "Download".into(),
                })
                .await;
            if !confirmed {
                return Ok(false);
            }
            // A stop while the question was open must come before anything is saved.
            self.stop.check()?;
        }

        let mut local: HashMap<String, PathBuf> =
            HashMap::from([(String::new(), self.destination.to_path_buf())]);
        for relative_path in &directories {
            self.stop.check()?;
            // Missing: inside a folder whose name was refused.
            let Some(parent) = local.get(parent_of(relative_path)).cloned() else {
                continue;
            };
            match self
                .deps
                .create_folder(&parent, name_of(relative_path))
                .await
            {
                Ok(created) => {
                    local.insert(relative_path.clone(), created);
                    result.folders += 1;
                }
                Err(error) if is_refused_name(&error) => result
                    .skipped
                    .push(format!("{relative_path} ({REFUSED_NAME})")),
                Err(error) => {
                    return Err(local_failure("create", relative_path, &error, self.stop));
                }
            }
        }
        let transfers: Vec<&RemoteItem> = files
            .iter()
            .filter(|file| local.contains_key(parent_of(&file.relative_path)))
            .collect();
        let transfer_bytes: u64 = transfers.iter().map(|file| file.size).sum();
        result.file_count = transfers.len();

        let mut completed_bytes = 0u64;
        for (index, file) in transfers.iter().enumerate() {
            self.stop.check()?;
            self.last = TransferProgressState {
                direction: TransferDirection::Download,
                name: file.relative_path.clone(),
                loaded: completed_bytes,
                total: Some(transfer_bytes),
                bytes_per_second: 0.,
                phase: TransferPhase::Preparing,
                file_index: Some(index + 1),
                file_count: Some(transfers.len()),
            };
            self.report();
            let mut resume = DownloadResume::new(&file.path, self.interrupted, self.stop);
            let downloaded = {
                let (last, on_progress) = (&mut self.last, &mut self.on_progress);
                let mut progress = |progress: ByteProgress| {
                    last.loaded = completed_bytes + progress.loaded;
                    last.bytes_per_second = progress.bytes_per_second;
                    last.phase = TransferPhase::Transferring;
                    on_progress(last);
                };
                self.deps
                    .download(self.conn_id, &file.path, &mut resume, &mut progress)
                    .await
            };
            // Stopped early, `resume` leaves what the file received for its next download; a
            // Cancel later in the job must leave what another download keeps of this file.
            let bytes = downloaded
                .map_err(|error| could_not("download", &file.relative_path, error, self.stop))?;
            resume.forget();
            drop(resume);
            completed_bytes += bytes.len() as u64;
            let folder = &local[parent_of(&file.relative_path)];
            match self
                .deps
                .save(folder, name_of(&file.relative_path), bytes)
                .await
            {
                Ok(_) => result.downloaded += 1,
                Err(error) if is_refused_name(&error) => result
                    .skipped
                    .push(format!("{} ({REFUSED_NAME})", file.relative_path)),
                Err(error) => {
                    return Err(local_failure(
                        "save",
                        &file.relative_path,
                        &error,
                        self.stop,
                    ));
                }
            }
        }

        self.last = TransferProgressState {
            direction: TransferDirection::Download,
            name: if result.downloaded > 0 || result.folders == 0 {
                plural(result.downloaded, "file")
            } else {
                plural(result.folders, "folder")
            },
            loaded: completed_bytes,
            total: Some(completed_bytes),
            bytes_per_second: 0.,
            phase: TransferPhase::Complete,
            file_index: Some(transfers.len()),
            file_count: Some(transfers.len()),
        };
        self.report();
        Ok(true)
    }
}

/// The toast after a download, e.g. "Downloaded 3 files to Downloads".
pub fn download_summary(result: &DownloadResult, folder: Option<&str>) -> String {
    let DownloadResult {
        downloaded,
        file_count,
        folders,
        skipped,
        cancelled,
    } = result;
    let place = |preposition: &str| {
        folder.map_or(String::new(), |folder| format!(" {preposition} {folder}"))
    };
    if *cancelled {
        return if *downloaded > 0 {
            format!(
                "Download cancelled: {downloaded} of {} saved{}",
                plural(*file_count, "file"),
                place("in")
            )
        } else if *folders > 0 {
            format!(
                "Download cancelled: {} created{}",
                plural(*folders, "folder"),
                place("in")
            )
        } else {
            "Download cancelled".into()
        };
    }
    let done = if *downloaded > 0 {
        format!("Downloaded {}{}", plural(*downloaded, "file"), place("to"))
    } else if *folders > 0 {
        format!("Created {}{}", plural(*folders, "folder"), place("in"))
    } else {
        "Nothing was downloaded".into()
    };
    if skipped.is_empty() {
        done
    } else {
        format!("{done}; {} skipped", plural(skipped.len(), "item"))
    }
}

#[cfg(test)]
mod tests;
