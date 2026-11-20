//! SFTP file operations on a live SSH connection (port of `server/src/routes/sftp.ts`).
//!
//! The connection id comes from the terminal's `ready` message, so the file panel shares the
//! session's single SSH connection (MobaXterm-style) instead of dialing a second one. Each
//! operation holds an SFTP lease on that connection while it runs, and every operation shares the
//! connection's one SFTP session. The HTTP routes became methods; their failures are [`Problem`]s
//! carrying the status the route answered with.

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::mpsc;

use super::fs::{CHUNK, RemoteFs, SftpError, SftpResult, WriteError};
use super::lease::{SftpLease, SftpWindowLease};
use super::paths;
use super::progress::{ByteProgress, ProgressMeter, UploadStep};
use super::types::{
    SftpClipboardImageResponse, SftpEntry, SftpEntryType, SftpFileResponse, SftpFileSaveRequest,
    SftpFileSaveResponse, SftpHomeResponse, SftpListResponse, SftpOkResponse, SftpUploadResponse,
};
use crate::Problem;
use crate::connection::leases::ConnectionLeaseOwner;
use crate::connection::manager::SshConnectionManager;

/// Directory-tree deletes stop after this many entries — runaway guard.
pub const MAX_RECURSIVE_DELETE: usize = 10_000;
/// The editor is a text editor, not a way to accidentally buffer huge remote artifacts.
pub const MAX_EDITOR_BYTES: u64 = 8 * 1024 * 1024;
/// A new file's mode, less the server's umask: what `sftp put` gives a typical file.
pub const NEW_FILE_MODE: u32 = 0o644;
/// Pasted clipboard images are readable by their owner only.
pub const CLIPBOARD_IMAGE_MODE: u32 = 0o600;
pub const CLIPBOARD_IMAGE_TEMP_DIR: &str = "/tmp";
/// How many bytes just before where an upload continues must match the staged file.
pub const RESUME_CHECK_BYTES: u64 = 64 * 1024;

/// Problem codes the client acts on.
pub const SFTP_DESTINATION_EXISTS: &str = "SFTP_DESTINATION_EXISTS";
pub const SFTP_DESTINATION_IS_DIRECTORY: &str = "SFTP_DESTINATION_IS_DIRECTORY";
pub const SFTP_DESTINATION_IS_SYMLINK: &str = "SFTP_DESTINATION_IS_SYMLINK";
pub const SFTP_EDITOR_SYMLINK: &str = "SFTP_EDITOR_SYMLINK";
pub const SFTP_FILE_CHANGED: &str = "SFTP_FILE_CHANGED";
/// What the server kept of an interrupted upload is not this file's: send it again, whole.
pub const SFTP_UPLOAD_MISMATCH: &str = "SFTP_UPLOAD_MISMATCH";

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

impl From<SftpError> for Problem {
    fn from(err: SftpError) -> Self {
        Problem::internal(err.message())
    }
}

impl From<WriteError> for Problem {
    fn from(err: WriteError) -> Self {
        Problem::internal(err.message())
    }
}

/// The local file an upload sends (the `size`, `lastModified` and `fingerprint` query
/// parameters): an interrupted upload of the same file continues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadSource {
    pub size: u64,
    /// The local modification time (ms).
    pub last_modified: i64,
    /// SHA-256 of samples spread over the file ([`upload_fingerprint`](super::upload::upload_fingerprint)).
    pub fingerprint: [u8; 32],
}

/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `Number(value)` of a query value, when it is a safe integer.
fn safe_integer(value: Option<&str>) -> Option<f64> {
    let text = value?.trim();
    let number = if text.is_empty() {
        0.0
    } else {
        text.parse::<f64>().ok()?
    };
    (number.is_finite() && number.fract() == 0.0 && number.abs() <= MAX_SAFE_INTEGER)
        .then_some(number)
}

impl UploadSource {
    /// Parses the query values the TypeScript route accepted; none of them is no source.
    pub fn from_query(
        size: Option<&str>,
        last_modified: Option<&str>,
        fingerprint: Option<&str>,
    ) -> Result<Option<Self>, Problem> {
        if size.is_none() && last_modified.is_none() && fingerprint.is_none() {
            return Ok(None);
        }
        let invalid = || {
            Problem::bad_request("size, lastModified and fingerprint must describe the local file")
        };
        let size = safe_integer(size)
            .filter(|size| *size >= 0.0)
            .ok_or_else(invalid)?;
        let last_modified = safe_integer(last_modified).ok_or_else(invalid)?;
        let digits = fingerprint
            .filter(|text| {
                text.len() == 64
                    && text
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or_else(invalid)?;
        let mut fingerprint = [0u8; 32];
        hex::decode_to_slice(digits, &mut fingerprint).map_err(|_| invalid())?;
        Ok(Some(Self {
            size: size as u64,
            last_modified: last_modified as i64,
            fingerprint,
        }))
    }

    /// Names the staged file of this local file's uploads.
    fn key(&self) -> String {
        let described = format!(
            "{}:{}:{}",
            self.size,
            self.last_modified,
            hex::encode(self.fingerprint)
        );
        hex::encode(Sha256::digest(described.as_bytes()))[..12].to_string()
    }
}

/// Upload request options (the `overwrite` and `expectedMtimeMs` query parameters).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UploadOptions {
    /// Replace an existing regular file; the user agreed to it.
    pub overwrite: bool,
    /// Upload only if the remote file still has this modification time (ms).
    pub expected_mtime_ms: Option<f64>,
    /// Size of the body, for progress only.
    pub size: Option<u64>,
    /// The local file being sent, when an interrupted upload of it may be continued.
    pub source: Option<UploadSource>,
}

impl UploadOptions {
    /// Parses the query values the TypeScript route accepted.
    pub fn from_query(
        overwrite: Option<&str>,
        expected_mtime_ms: Option<&str>,
    ) -> Result<Self, Problem> {
        let overwrite = match overwrite {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => return Err(Problem::bad_request("overwrite must be true or false")),
        };
        let expected_mtime_ms = match expected_mtime_ms {
            None => None,
            // `Number(value)`: blank text is 0, anything unparsable is NaN and rejected below.
            Some(text) if text.trim().is_empty() => Some(0.0),
            Some(text) => Some(text.trim().parse::<f64>().unwrap_or(f64::NAN)),
        };
        let options = Self {
            overwrite,
            expected_mtime_ms,
            size: None,
            source: None,
        };
        options.validate()?;
        Ok(options)
    }

    fn validate(&self) -> Result<(), Problem> {
        match self.expected_mtime_ms {
            Some(value) if !value.is_finite() || value < 0.0 => {
                Err(Problem::bad_request("expectedMtimeMs must be a timestamp"))
            }
            _ => Ok(()),
        }
    }
}

struct DownloadTicket {
    conn_id: String,
    path: String,
}

/// Drag-out download tickets. A listing can stay on screen long before a file is dragged out of
/// it, and a drag whose download fails leaves the file manager with an empty file. So a ticket
/// lasts as long as its connection: each remote file keeps one ticket across listings, and all
/// of them go when the connection closes.
#[derive(Default)]
struct DownloadTickets {
    by_ticket: HashMap<String, DownloadTicket>,
    /// Connection id → remote path → ticket.
    by_connection: HashMap<String, HashMap<String, String>>,
}

/// Where a download continues: `Range: bytes=<start>-` under an `If-Range` with the validator
/// the interrupted attempt was answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRange {
    pub start: u64,
    pub etag: String,
}

/// Staged uploads in progress, by connection: the same file sent twice at once goes under a
/// random name the second time instead of into the same file.
type StagedUploads = Arc<Mutex<HashSet<String>>>;

/// One staged upload's entry; it goes when the upload ends or is dropped.
struct StagedUpload {
    uploads: StagedUploads,
    key: String,
}

impl StagedUpload {
    /// `None` while another upload of the same file is staged under that name.
    fn claim(uploads: &StagedUploads, key: String) -> Option<Self> {
        let claimed = uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone());
        claimed.then(|| Self {
            uploads: uploads.clone(),
            key,
        })
    }
}

impl Drop for StagedUpload {
    fn drop(&mut self) {
        self.uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

/// The SFTP operations of the file browser and the remote editor.
#[derive(Clone)]
pub struct SftpService {
    manager: SshConnectionManager,
    tickets: Arc<Mutex<DownloadTickets>>,
    staged_uploads: StagedUploads,
}

impl SftpService {
    pub fn new(manager: SshConnectionManager) -> Self {
        Self {
            manager,
            tickets: Arc::new(Mutex::new(DownloadTickets::default())),
            staged_uploads: StagedUploads::default(),
        }
    }

    pub fn manager(&self) -> &SshConnectionManager {
        &self.manager
    }

    /// Holds the connection open while a file browser window shows it (`/ws/sftp/:connId/lease`).
    pub fn hold_window(&self, conn_id: &str) -> Result<SftpWindowLease, Problem> {
        SftpWindowLease::acquire(&self.manager, conn_id)
    }

    /// A further lease on the connection, for work that outlives the operation's own.
    fn lease(&self, conn_id: &str) -> Option<SftpLease> {
        SftpLease::acquire(&self.manager, conn_id, ConnectionLeaseOwner::Sftp)
    }

    /// An SFTP lease for one operation, plus the connection's shared session. A system OpenSSH
    /// session's connId reaches the login its terminal shares.
    async fn acquire(&self, conn_id: &str) -> Result<(SftpLease, RemoteFs), Problem> {
        let lease = self
            .lease(conn_id)
            .ok_or_else(|| Problem::not_found("connection not found"))?;
        // A failed session open drops (releases) the lease again.
        let fs = RemoteFs::open_on(lease.source())
            .await
            .map_err(Problem::internal)?;
        Ok((lease, fs))
    }

    fn issue_download_ticket(&self, connection: &SftpLease, file: String) -> String {
        let conn_id = connection.connection_id().to_string();
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        if !tickets.by_connection.contains_key(&conn_id) {
            tickets
                .by_connection
                .insert(conn_id.clone(), HashMap::new());
            // Every ticket of the connection goes when it closes.
            let all = self.tickets.clone();
            let closed = connection.connection_closed();
            let closing = conn_id.clone();
            tokio::spawn(async move {
                closed.await;
                let mut tickets = all.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(issued) = tickets.by_connection.remove(&closing) {
                    for ticket in issued.values() {
                        tickets.by_ticket.remove(ticket);
                    }
                }
            });
        }
        if let Some(existing) = tickets
            .by_connection
            .get(&conn_id)
            .and_then(|issued| issued.get(&file))
        {
            return existing.clone();
        }
        let ticket =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 18]>());
        tickets
            .by_connection
            .entry(conn_id.clone())
            .or_default()
            .insert(file.clone(), ticket.clone());
        tickets.by_ticket.insert(
            ticket.clone(),
            DownloadTicket {
                conn_id,
                path: file,
            },
        );
        ticket
    }

    /// The remote user's home directory. Kept for API compatibility; the client resolves `.`
    /// through [`list`](Self::list) so its initial directory load needs one round trip.
    pub async fn home(&self, conn_id: &str) -> Result<SftpHomeResponse, Problem> {
        let (_lease, fs) = self.acquire(conn_id).await?;
        Ok(SftpHomeResponse {
            path: fs.realpath(".").await?,
        })
    }

    /// A directory listing; `path` may be relative (`.` is the home directory). Regular files
    /// carry a drag-out download ticket, valid while the connection lasts.
    pub async fn list(&self, conn_id: &str, path: &str) -> Result<SftpListResponse, Problem> {
        let (lease, fs) = self.acquire(conn_id).await?;
        let dir = require_path(path)?;
        let resolved = fs.realpath(dir).await?;
        let listing = fs.read_dir(&resolved).await?;
        let entries = listing
            .into_iter()
            .filter(|item| item.filename != "." && item.filename != "..")
            .map(|item| {
                let mode = item.attrs.permissions.unwrap_or(0);
                let kind = entry_type(mode);
                let (owner, group) = owner_from_longname(&item.longname);
                let download_ticket = (kind == SftpEntryType::File).then(|| {
                    self.issue_download_ticket(&lease, paths::join(&resolved, &item.filename))
                });
                SftpEntry {
                    name: item.filename,
                    kind,
                    size: item.attrs.size,
                    mtime_ms: mtime_ms(item.attrs.mtime),
                    mode: Some(mode & 0o7777),
                    owner,
                    group,
                    download_ticket,
                }
            })
            .collect();
        Ok(SftpListResponse {
            path: resolved,
            entries,
        })
    }

    /// Streams a remote file. The download keeps its SFTP lease until it is read through or
    /// dropped, so closing the terminal tab cannot tear the shared transport out from under it.
    pub async fn download(&self, conn_id: &str, path: &str) -> Result<SftpDownload, Problem> {
        self.download_from(conn_id, path, None).await
    }

    /// [`download`](Self::download), continuing at `range` while the file is still the one the
    /// interrupted attempt read: its validator (host, path, size and modification time) must
    /// match, or the whole file is sent. [`SftpDownload::start`] says which it is.
    pub async fn download_from(
        &self,
        conn_id: &str,
        path: &str,
        range: Option<&DownloadRange>,
    ) -> Result<SftpDownload, Problem> {
        let (lease, fs) = self.acquire(conn_id).await?;
        let file = require_path(path)?;
        let stat = fs.stat(file).await?;
        let etag = download_etag(lease.mux_key(), file, &stat);
        let start = match (&etag, stat.size) {
            (Some(etag), Some(size)) => range_start(range, etag, size),
            _ => 0,
        };
        // Opened before anything is answered, so a file the user may not read fails with that
        // error instead of a download that never delivers a byte.
        let handle = fs.open(file, OpenFlags::READ, None).await?;
        Ok(SftpDownload::begin(
            lease,
            &fs,
            handle,
            DownloadHead {
                name: paths::basename(file),
                size: stat.size,
                mtime_ms: mtime_ms(stat.mtime),
                etag,
                start,
            },
        ))
    }

    /// Streams the file a listing's download ticket names (native drag-out downloads).
    pub async fn drag_download(
        &self,
        conn_id: &str,
        ticket: &str,
    ) -> Result<SftpDownload, Problem> {
        let file = {
            let tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
            match tickets.by_ticket.get(ticket) {
                Some(value) if value.conn_id == conn_id => value.path.clone(),
                _ => return Err(Problem::new(401, "download ticket is missing or expired")),
            }
        };
        let (lease, fs) = self.acquire(conn_id).await?;
        let stat = fs.stat(&file).await?;
        let handle = fs.open(&file, OpenFlags::READ, None).await?;
        Ok(SftpDownload::begin(
            lease,
            &fs,
            handle,
            DownloadHead {
                name: paths::basename(&file),
                size: stat.size,
                mtime_ms: None,
                etag: None,
                start: 0,
            },
        ))
    }

    /// Opens a UTF-8 text file for the editor, with the metadata a later save checks against.
    pub async fn read_file(&self, conn_id: &str, path: &str) -> Result<SftpFileResponse, Problem> {
        let (_lease, fs) = self.acquire(conn_id).await?;
        let file = require_path(path)?;
        let attrs = fs.lstat(file).await?;
        if is_dir(&attrs) {
            return Err(Problem::bad_request("cannot edit a directory"));
        }
        if is_symlink(&attrs) {
            return Err(Problem::with_code(
                409,
                "open the symbolic link target explicitly",
                SFTP_EDITOR_SYMLINK,
            ));
        }
        if attrs.size.unwrap_or(0) > MAX_EDITOR_BYTES {
            return Err(too_large_to_open());
        }
        let bytes = read_bounded(&fs, file, MAX_EDITOR_BYTES).await?;
        if bytes.contains(&0) {
            return Err(Problem::new(
                415,
                "this appears to be a binary file and cannot be opened as text",
            ));
        }
        let size = bytes.len() as u64;
        let mut content = String::from_utf8(bytes).map_err(|_| {
            Problem::new(415, "the remote editor currently supports UTF-8 text files")
        })?;
        // TextDecoder drops a leading byte order mark.
        if content.starts_with('\u{feff}') {
            content.drain(..'\u{feff}'.len_utf8());
        }
        Ok(SftpFileResponse {
            path: file.to_string(),
            content,
            size,
            mtime_ms: mtime_ms(attrs.mtime),
            mode: attrs.permissions.map(|mode| mode & 0o7777),
        })
    }

    /// Saves editor text over an existing file, atomically, unless the file changed since it was
    /// opened (`expectedMtimeMs`) and the user did not force it.
    pub async fn save_file(
        &self,
        conn_id: &str,
        path: &str,
        request: &SftpFileSaveRequest,
    ) -> Result<SftpFileSaveResponse, Problem> {
        let (_lease, fs) = self.acquire(conn_id).await?;
        let file = require_path(path)?;
        let bytes = request.content.as_bytes();
        if bytes.len() as u64 > MAX_EDITOR_BYTES {
            return Err(Problem::new(
                413,
                format!(
                    "files larger than {} cannot be saved from the editor",
                    format_bytes(MAX_EDITOR_BYTES)
                ),
            ));
        }
        let attrs = fs.lstat(file).await?;
        if is_dir(&attrs) {
            return Err(Problem::bad_request("cannot overwrite a directory"));
        }
        if is_symlink(&attrs) {
            return Err(Problem::with_code(
                409,
                "refusing to overwrite a symbolic link",
                SFTP_DESTINATION_IS_SYMLINK,
            ));
        }
        let current = mtime_ms(attrs.mtime);
        if request.force != Some(true)
            && let Some(expected) = request.expected_mtime_ms
            && current != Some(expected)
        {
            return Err(Problem::with_code(
                409,
                "the remote file changed since it was opened",
                SFTP_FILE_CHANGED,
            ));
        }
        atomic_text_save(&fs, file, bytes, attrs.permissions).await?;
        let saved = fs.stat(file).await?;
        Ok(SftpFileSaveResponse {
            ok: true,
            size: saved.size.unwrap_or(bytes.len() as u64),
            mtime_ms: mtime_ms(saved.mtime),
        })
    }

    /// Uploads `body` to `path` through a sibling temporary file, so an existing destination is
    /// never left truncated. Without `overwrite` an existing file is a 409
    /// `SFTP_DESTINATION_EXISTS` the client turns into a "replace?" question.
    pub async fn upload<R>(
        &self,
        conn_id: &str,
        path: &str,
        options: &UploadOptions,
        mut body: R,
        on_progress: &mut (dyn FnMut(UploadStep) + Send),
    ) -> Result<SftpUploadResponse, Problem>
    where
        R: AsyncRead + Unpin + Send,
    {
        let (lease, fs) = self.acquire(conn_id).await?;
        let file = require_path(path)?;
        options.validate()?;
        let overwrite = options.overwrite;

        let existing = fs.lstat_if_present(file).await?;
        if let Some(expected) = options.expected_mtime_ms {
            let current = existing
                .as_ref()
                .and_then(|attrs| attrs.mtime)
                .map(|mtime| f64::from(mtime) * 1000.0);
            if current != Some(expected) {
                return Err(Problem::with_code(
                    409,
                    "the remote file changed since it was downloaded",
                    SFTP_FILE_CHANGED,
                ));
            }
        }
        if existing.as_ref().is_some_and(is_dir) {
            return Err(Problem::with_code(
                409,
                "a directory already exists at the upload destination",
                SFTP_DESTINATION_IS_DIRECTORY,
            ));
        }
        if existing.as_ref().is_some_and(is_symlink) {
            return Err(Problem::with_code(
                409,
                "refusing to overwrite a symbolic link",
                SFTP_DESTINATION_IS_SYMLINK,
            ));
        }
        if existing.is_some() && !overwrite {
            return Err(destination_exists());
        }

        let mode = existing.and_then(|attrs| attrs.permissions);
        // The staged file is named after the local file, unless an upload of the same file is
        // already going into it.
        let staging = options.source.and_then(|source| {
            let key = format!("{}\0{file}\0{}", lease.connection_id(), source.key());
            StagedUpload::claim(&self.staged_uploads, key).map(|claim| (source, claim))
        });
        let upload = StreamUpload {
            fs: &fs,
            lease: self.lease(conn_id),
            file,
            overwrite,
            mode,
            size: options.size,
            source: staging.as_ref().map(|(source, _)| *source),
        };
        let uploaded = upload.run(&mut body, on_progress).await;
        drop(staging);
        if let Err(err) = uploaded {
            // A different client may have created the destination while this upload streamed
            // into its temporary file: answer with the same actionable conflict instead of a
            // generic rename error.
            if !overwrite && matches!(fs.lstat_if_present(file).await, Ok(Some(_))) {
                return Err(destination_exists());
            }
            return Err(err);
        }
        let mtime_ms = fs
            .stat(file)
            .await
            .ok()
            .and_then(|saved| mtime_ms(saved.mtime));
        Ok(SftpUploadResponse { ok: true, mtime_ms })
    }

    /// Stores a pasted clipboard image as a fresh owner-only file in `/tmp` and answers its path,
    /// which the terminal then types.
    pub async fn clipboard_image<R>(
        &self,
        conn_id: &str,
        mut body: R,
    ) -> Result<SftpClipboardImageResponse, Problem>
    where
        R: AsyncRead + Unpin + Send,
    {
        let (_lease, fs) = self.acquire(conn_id).await?;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let file = format!(
            "{CLIPBOARD_IMAGE_TEMP_DIR}/muxus-paste-{now_ms}-{}.png",
            random_hex::<16>()
        );
        let upload = StreamUpload {
            fs: &fs,
            lease: self.lease(conn_id),
            file: &file,
            overwrite: false,
            mode: Some(CLIPBOARD_IMAGE_MODE),
            size: None,
            source: None,
        };
        upload.run(&mut body, &mut |_| {}).await?;
        Ok(SftpClipboardImageResponse { path: file })
    }

    /// Creates a directory; `recursive` creates missing parents and accepts existing ones.
    pub async fn mkdir(
        &self,
        conn_id: &str,
        path: &str,
        recursive: bool,
    ) -> Result<SftpOkResponse, Problem> {
        let (_lease, fs) = self.acquire(conn_id).await?;
        let target = require_path(path)?;
        if recursive {
            mkdir_recursive(&fs, target).await?;
        } else {
            fs.mkdir(target).await?;
        }
        Ok(SftpOkResponse::OK)
    }

    pub async fn rename(
        &self,
        conn_id: &str,
        from: &str,
        to: &str,
    ) -> Result<SftpOkResponse, Problem> {
        let (_lease, fs) = self.acquire(conn_id).await?;
        if from.is_empty() || to.is_empty() {
            return Err(Problem::bad_request("from and to are required"));
        }
        fs.rename(from, to).await?;
        Ok(SftpOkResponse::OK)
    }

    /// Deletes a file or link, or a directory with everything below it (symbolic links inside
    /// are removed, never followed).
    pub async fn delete(&self, conn_id: &str, path: &str) -> Result<SftpOkResponse, Problem> {
        let (_lease, fs) = self.acquire(conn_id).await?;
        let target = require_path(path)?;
        let attrs = fs.lstat(target).await?;
        if is_dir(&attrs) {
            let mut budget = MAX_RECURSIVE_DELETE;
            delete_tree(&fs, target, &mut budget, MAX_RECURSIVE_DELETE).await?;
        } else {
            fs.remove(target).await?;
        }
        Ok(SftpOkResponse::OK)
    }
}

/// A remote file being streamed. Read it through [`AsyncRead`], [`next_chunk`](Self::next_chunk)
/// or [`copy_to`](Self::copy_to).
pub struct SftpDownload {
    /// File name for the save dialog (`content-disposition`).
    pub name: String,
    /// Size from `stat` when the server reported one (`content-length`).
    pub size: Option<u64>,
    /// Modification time (`last-modified`): a client that opens the file locally can upload it
    /// back with a check that nobody changed the remote file in between.
    pub mtime_ms: Option<i64>,
    /// The file's validator (`etag`); a download that stops early can continue under it.
    pub etag: Option<String>,
    /// The byte the stream starts at: past 0 only where the request's range was honoured
    /// (the route's 206).
    pub start: u64,
    chunks: mpsc::Receiver<SftpResult<Vec<u8>>>,
    buffer: Vec<u8>,
    position: usize,
    lease: Arc<SftpLease>,
}

impl std::fmt::Debug for SftpDownload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpDownload")
            .field("name", &self.name)
            .field("size", &self.size)
            .field("mtime_ms", &self.mtime_ms)
            .field("etag", &self.etag)
            .field("start", &self.start)
            .finish()
    }
}

/// What a download answers with before its first byte (the route's response headers).
struct DownloadHead {
    name: String,
    size: Option<u64>,
    mtime_ms: Option<i64>,
    etag: Option<String>,
    start: u64,
}

impl SftpDownload {
    fn begin(lease: SftpLease, fs: &RemoteFs, handle: String, head: DownloadHead) -> Self {
        let lease = Arc::new(lease);
        // The lease outlives this call: it goes when the remote stream ends or the download is
        // dropped, whichever comes first.
        let release = lease.clone();
        let chunks = fs.stream_reader_from(handle, head.start, Box::new(move || release.release()));
        Self {
            name: head.name,
            size: head.size,
            mtime_ms: head.mtime_ms,
            etag: head.etag,
            start: head.start,
            chunks,
            buffer: Vec::new(),
            position: 0,
            lease,
        }
    }

    /// The next piece of the file; `None` at its end.
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, Problem> {
        if self.position < self.buffer.len() {
            let rest = self.buffer.split_off(self.position);
            self.buffer.clear();
            self.position = 0;
            return Ok(Some(rest));
        }
        match self.chunks.recv().await {
            Some(Ok(chunk)) => Ok(Some(chunk)),
            Some(Err(err)) => Err(err.into()),
            None => Ok(None),
        }
    }

    /// Writes the whole file into `writer`, reporting progress like the client's download did
    /// (forced first and last updates, throttled in between). Answers the byte count.
    pub async fn copy_to<W>(
        mut self,
        writer: &mut W,
        on_progress: &mut (dyn FnMut(ByteProgress) + Send),
    ) -> Result<u64, Problem>
    where
        W: AsyncWrite + Unpin + ?Sized,
    {
        let start = self.start;
        let mut meter = ProgressMeter::starting_at(start);
        if let Some(progress) = meter.report(start, self.size, true) {
            on_progress(progress);
        }
        let mut loaded = start;
        while let Some(chunk) = self.next_chunk().await? {
            writer.write_all(&chunk).await?;
            loaded += chunk.len() as u64;
            if let Some(progress) = meter.report(loaded, self.size, false) {
                on_progress(progress);
            }
        }
        writer.flush().await?;
        if let Some(progress) = meter.report(loaded, self.size.or(Some(loaded)), true) {
            on_progress(progress);
        }
        Ok(loaded - start)
    }

    /// The whole file in memory.
    pub async fn read_all(mut self) -> Result<Vec<u8>, Problem> {
        let mut out = Vec::new();
        while let Some(chunk) = self.next_chunk().await? {
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }
}

impl AsyncRead for SftpDownload {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        loop {
            if this.position < this.buffer.len() {
                let len = buf.remaining().min(this.buffer.len() - this.position);
                buf.put_slice(&this.buffer[this.position..this.position + len]);
                this.position += len;
                return Poll::Ready(Ok(()));
            }
            match ready!(this.chunks.poll_recv(cx)) {
                Some(Ok(chunk)) => {
                    this.buffer = chunk;
                    this.position = 0;
                }
                Some(Err(err)) => return Poll::Ready(Err(std::io::Error::other(err))),
                None => return Poll::Ready(Ok(())),
            }
        }
    }
}

impl Drop for SftpDownload {
    fn drop(&mut self) {
        self.lease.release();
    }
}

fn require_path(path: &str) -> Result<&str, Problem> {
    if path.is_empty() {
        Err(Problem::bad_request("path is required"))
    } else {
        Ok(path)
    }
}

/// A download's validator: the host, path, size and modification time. A download continues
/// from a byte only while all of them still match.
fn download_etag(mux_key: &str, file: &str, stat: &FileAttributes) -> Option<String> {
    let (size, mtime) = (stat.size?, stat.mtime?);
    let described = serde_json::to_string(&(mux_key, file, size, mtime)).ok()?;
    let digest = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(described.as_bytes()));
    Some(format!("\"{}\"", &digest[..27]))
}

/// Where a download starts: a range counts only under the file's current validator, and
/// inside the file.
fn range_start(range: Option<&DownloadRange>, etag: &str, size: u64) -> u64 {
    match range {
        Some(range) if range.etag == etag && range.start < size => range.start,
        _ => 0,
    }
}

fn upload_mismatch() -> Problem {
    Problem::with_code(
        409,
        "the upload kept on the server does not match this file",
        SFTP_UPLOAD_MISMATCH,
    )
}

fn destination_exists() -> Problem {
    Problem::with_code(
        409,
        "a file already exists at the upload destination",
        SFTP_DESTINATION_EXISTS,
    )
}

fn too_large_to_open() -> Problem {
    Problem::new(
        413,
        format!(
            "files larger than {} cannot be opened in the editor",
            format_bytes(MAX_EDITOR_BYTES)
        ),
    )
}

fn format_bytes(bytes: u64) -> String {
    format!("{} MiB", (bytes as f64 / (1024.0 * 1024.0)).round())
}

fn random_hex<const N: usize>() -> String {
    let mut bytes = [0u8; N];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut bytes);
    hex::encode(bytes)
}

/// Unix mtime in ms; a zero or missing mtime is unknown.
fn mtime_ms(mtime: Option<u32>) -> Option<i64> {
    mtime
        .filter(|mtime| *mtime != 0)
        .map(|mtime| i64::from(mtime) * 1000)
}

fn file_format(attrs: &FileAttributes) -> Option<u32> {
    attrs.permissions.map(|mode| mode & S_IFMT)
}

fn is_dir(attrs: &FileAttributes) -> bool {
    file_format(attrs) == Some(S_IFDIR)
}

fn is_symlink(attrs: &FileAttributes) -> bool {
    file_format(attrs) == Some(S_IFLNK)
}

fn is_regular(attrs: &FileAttributes) -> bool {
    file_format(attrs) == Some(S_IFREG)
}

fn entry_type(mode: u32) -> SftpEntryType {
    match mode & S_IFMT {
        S_IFDIR => SftpEntryType::Dir,
        S_IFREG => SftpEntryType::File,
        S_IFLNK => SftpEntryType::Link,
        _ => SftpEntryType::Other,
    }
}

/// "drwxr-xr-x  2 alice staff 4096 …" → owner/group display hints.
fn owner_from_longname(longname: &str) -> (Option<String>, Option<String>) {
    let fields: Vec<&str> = longname.split_whitespace().collect();
    if fields.len() >= 4 {
        (Some(fields[2].to_string()), Some(fields[3].to_string()))
    } else {
        (None, None)
    }
}

async fn read_bounded(fs: &RemoteFs, file: &str, max_bytes: u64) -> Result<Vec<u8>, Problem> {
    let handle = fs.open(file, OpenFlags::READ, None).await?;
    let mut chunks = fs.stream_reader(handle, Box::new(|| {}));
    let mut bytes = Vec::new();
    while let Some(chunk) = chunks.recv().await {
        bytes.extend_from_slice(&chunk?);
        if bytes.len() as u64 > max_bytes {
            // Dropping the receiver stops the reads and closes the handle.
            return Err(too_large_to_open());
        }
    }
    Ok(bytes)
}

/// Writes a complete temporary file before replacing the destination. The shared commit path
/// prefers OpenSSH's atomic POSIX rename extension.
async fn atomic_text_save(
    fs: &RemoteFs,
    file: &str,
    bytes: &[u8],
    mode: Option<u32>,
) -> Result<(), Problem> {
    let temp = format!("{file}.muxus-{}.tmp", random_hex::<6>());
    let result: Result<(), Problem> = async {
        let handle = fs
            .open(&temp, write_exclusive(), Some(temp_file_mode(mode)))
            .await?;
        let written = fs.write_from(&handle, &mut &bytes[..], &mut |_| {}).await;
        if written.is_ok() {
            restore_mode(fs, &handle, &temp, mode).await;
        }
        let closed = fs.close(&handle).await;
        written?;
        closed?;
        commit_temp_file(fs, &temp, file, true).await
    }
    .await;
    if result.is_err() {
        let _ = fs.remove(&temp).await;
    }
    result
}

/// The mode a new temporary file is opened with: that of the file it replaces, else
/// [`NEW_FILE_MODE`], which the server's umask may narrow.
fn temp_file_mode(mode: Option<u32>) -> u32 {
    mode.unwrap_or(NEW_FILE_MODE) & 0o7777
}

/// Gives a temporary file the exact mode of the file it replaces. The mode passed to `open` goes
/// through the server's umask, which would turn a 0664 file into 0644 on save. A server that
/// refuses keeps the umask result, as `sftp put -p` does after its warning. A new file keeps
/// what the umask left of [`NEW_FILE_MODE`].
async fn restore_mode(fs: &RemoteFs, handle: &str, temp: &str, mode: Option<u32>) {
    let Some(mode) = mode.map(|mode| mode & 0o7777) else {
        return;
    };
    if let Err(err) = fs.set_mode(handle, mode).await {
        tracing::warn!(path = temp, mode = %format!("{mode:o}"), %err, "could not set the mode of a saved file");
    }
}

/// `wx`: create a new file for writing, failing if it exists.
fn write_exclusive() -> OpenFlags {
    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::EXCLUDE
}

/// Uploads into a sibling temporary file. If the body fails, the SFTP stream fails, or the
/// upload is dropped half way, only that temporary file is removed; an existing destination is
/// never left truncated.
///
/// With a `source`, the temporary file is named after the local file, so that the next upload
/// of it continues one a lost connection cut short (the removal never reached the server). A
/// staged file that does not match the body is never written to: the upload fails with
/// `SFTP_UPLOAD_MISMATCH` and the client sends the file again as a new upload. Anything else
/// under that name is removed, or left alone when it cannot be, and the upload goes under a
/// random name.
struct StreamUpload<'a> {
    fs: &'a RemoteFs,
    /// Keeps the transport up for the clean-up of a dropped upload.
    lease: Option<SftpLease>,
    file: &'a str,
    overwrite: bool,
    mode: Option<u32>,
    size: Option<u64>,
    source: Option<UploadSource>,
}

/// What an interrupted attempt left in a staged file: open where it ends, with its last bytes.
struct StagedPart {
    handle: String,
    offset: u64,
    tail: Vec<u8>,
}

impl StreamUpload<'_> {
    async fn run<R>(
        self,
        body: &mut R,
        on_progress: &mut (dyn FnMut(UploadStep) + Send),
    ) -> Result<(), Problem>
    where
        R: AsyncRead + Unpin + Send + ?Sized,
    {
        let (temp, part) = match self.source {
            Some(source) => self.staged_file(&source).await?,
            None => (self.random_temp(), None),
        };
        let mut guard = TempFileGuard {
            fs: Some(self.fs.clone()),
            path: temp.clone(),
            lease: self.lease,
        };
        let result: Result<(), Problem> = async {
            let total = self.size;
            let (handle, offset, tail) = match part {
                Some(part) => (part.handle, part.offset, part.tail),
                None => {
                    let handle = self
                        .fs
                        .open(&temp, write_exclusive(), Some(temp_file_mode(self.mode)))
                        .await?;
                    (handle, 0, Vec::new())
                }
            };
            let continued = offset > 0;
            let mut meter = ProgressMeter::starting_at(offset);
            if let Some(progress) = meter.report(offset, total, true) {
                on_progress(UploadStep::Progress(progress));
            }
            let written = async {
                if continued {
                    skip_staged_bytes(body, offset, &tail).await?;
                }
                let written = self
                    .fs
                    .write_from_at(&handle, offset, body, &mut |acknowledged| {
                        let done = total.is_some_and(|total| acknowledged >= total);
                        if let Some(progress) = meter.report(acknowledged, total, done) {
                            on_progress(UploadStep::Progress(progress));
                        }
                    })
                    .await?;
                if let Some(source) = self.source.filter(|_| continued) {
                    // A server that wrote anywhere but where it was asked shows in the size.
                    if self.fs.fstat(&handle).await?.size != Some(source.size) {
                        return Err(upload_mismatch());
                    }
                }
                Ok(written)
            }
            .await;
            // A continued upload keeps the mode its staged file was created with.
            if written.is_ok() && !continued {
                restore_mode(self.fs, &handle, &temp, self.mode).await;
            }
            let closed = self.fs.close(&handle).await;
            let written = written?;
            closed?;
            on_progress(UploadStep::Finalizing);
            if let Some(progress) = meter.report(written, total.or(Some(written)), true) {
                on_progress(UploadStep::Progress(progress));
            }
            commit_temp_file(self.fs, &temp, self.file, self.overwrite).await
        }
        .await;
        match result {
            Ok(()) => guard.disarm(),
            Err(_) => guard.remove().await,
        }
        result
    }

    fn random_temp(&self) -> String {
        format!("{}.muxus-{}.upload", self.file, random_hex::<6>())
    }

    /// The temporary file of an upload that can be continued, and what an interrupted attempt
    /// left in it.
    async fn staged_file(
        &self,
        source: &UploadSource,
    ) -> Result<(String, Option<StagedPart>), Problem> {
        let staged = format!("{}.muxus-{}.upload", self.file, source.key());
        let found = self.fs.lstat_if_present(&staged).await?;
        // Only a regular file is opened: opening a FIFO or a device can block, or act on it.
        let part = match &found {
            Some(attrs) if is_regular(attrs) => open_staged(self.fs, &staged, source.size).await,
            _ => None,
        };
        let temp = if found.is_none() || part.is_some() || removed(self.fs, &staged).await {
            staged
        } else {
            self.random_temp()
        };
        Ok((temp, part))
    }
}

/// Opens the staged file of an interrupted attempt, if it is a regular file no larger than the
/// upload. Its type, size and last bytes are read again through the handle that is written to,
/// whatever the name pointed to by then.
async fn open_staged(fs: &RemoteFs, staged: &str, size: u64) -> Option<StagedPart> {
    let handle = fs
        .open(staged, OpenFlags::READ | OpenFlags::WRITE, None)
        .await
        .ok()?;
    let part = async {
        let attrs = fs.fstat(&handle).await.ok()?;
        let offset = attrs.size.unwrap_or(0);
        if !is_regular(&attrs) || offset == 0 || offset > size {
            return None;
        }
        let tail_start = offset.saturating_sub(RESUME_CHECK_BYTES);
        // Unreadable: it starts over, like anything else that cannot be continued.
        let tail = fs.read_range(&handle, tail_start, offset).await.ok()??;
        Some((offset, tail))
    }
    .await;
    match part {
        Some((offset, tail)) => Some(StagedPart {
            handle,
            offset,
            tail,
        }),
        None => {
            let _ = fs.close(&handle).await;
            None
        }
    }
}

/// Reads past the bytes of `body` the staged file already holds. The last of them must equal
/// the file's `tail`: if they differ, or the body ends before them, what the server kept is not
/// this file's and nothing is written to it.
async fn skip_staged_bytes<R>(body: &mut R, offset: u64, tail: &[u8]) -> Result<(), Problem>
where
    R: AsyncRead + Unpin + ?Sized,
{
    let tail_start = offset - tail.len() as u64;
    let mut buffer = vec![0u8; CHUNK];
    let mut position = 0u64;
    while position < offset {
        let want = (offset - position).min(CHUNK as u64) as usize;
        let read = body.read(&mut buffer[..want]).await?;
        if read == 0 {
            return Err(upload_mismatch());
        }
        let start = position;
        position += read as u64;
        let from = start.max(tail_start);
        if from < position {
            let bytes = &buffer[(from - start) as usize..read];
            let kept = &tail[(from - tail_start) as usize..(position - tail_start) as usize];
            if bytes != kept {
                return Err(upload_mismatch());
            }
        }
    }
    Ok(())
}

/// Whether nothing is left at `file`: removed now, or never there.
async fn removed(fs: &RemoteFs, file: &str) -> bool {
    match fs.remove(file).await {
        Ok(()) => true,
        Err(err) => err.code() == Some(StatusCode::NoSuchFile),
    }
}

/// Removes a staged temporary file unless disarmed, also when its upload is dropped.
struct TempFileGuard {
    fs: Option<RemoteFs>,
    path: String,
    lease: Option<SftpLease>,
}

impl TempFileGuard {
    fn disarm(&mut self) {
        self.fs = None;
    }

    async fn remove(&mut self) {
        if let Some(fs) = self.fs.take() {
            let _ = fs.remove(&self.path).await;
        }
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let Some(fs) = self.fs.take() else { return };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let path = std::mem::take(&mut self.path);
        let lease = self.lease.take();
        runtime.spawn(async move {
            let _ = fs.remove(&path).await;
            drop(lease);
        });
    }
}

async fn commit_temp_file(
    fs: &RemoteFs,
    temp: &str,
    destination: &str,
    overwrite: bool,
) -> Result<(), Problem> {
    if !overwrite {
        if fs.lstat_if_present(destination).await?.is_some() {
            return Err(destination_exists());
        }
        fs.rename(temp, destination).await?;
        return Ok(());
    }

    match fs.posix_rename(temp, destination).await {
        Ok(()) => return Ok(()),
        Err(err) if !err.is_unsupported() => return Err(err.into()),
        Err(_) => {}
    }

    // Base SFTP v3 rename commonly refuses an existing destination. Try it first for servers
    // that replace, then use a short unlink/rename fallback.
    if fs.rename(temp, destination).await.is_err() {
        fs.remove(destination).await?;
        fs.rename(temp, destination).await?;
    }
    Ok(())
}

async fn mkdir_recursive(fs: &RemoteFs, target: &str) -> Result<(), Problem> {
    let normalized = paths::normalize(target);
    let absolute = normalized.starts_with('/');
    let mut current = if absolute {
        "/".to_string()
    } else {
        ".".to_string()
    };
    for part in normalized.split('/').filter(|part| !part.is_empty()) {
        current = paths::join(&current, part);
        if let Err(err) = fs.mkdir(&current).await {
            let existing = fs.lstat_if_present(&current).await?;
            if !existing.as_ref().is_some_and(is_dir) {
                return Err(err.into());
            }
        }
    }
    Ok(())
}

/// Removes a directory tree, `budget` entries at most.
fn delete_tree<'a>(
    fs: &'a RemoteFs,
    dir: &'a str,
    budget: &'a mut usize,
    limit: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Problem>> + Send + 'a>> {
    Box::pin(async move {
        let listing = fs.read_dir(dir).await?;
        for item in listing {
            if item.filename == "." || item.filename == ".." {
                continue;
            }
            if *budget == 0 {
                return Err(Problem::bad_request(format!(
                    "refusing to delete more than {limit} entries"
                )));
            }
            *budget -= 1;
            let child = paths::join(dir, &item.filename);
            if entry_type(item.attrs.permissions.unwrap_or(0)) == SftpEntryType::Dir {
                delete_tree(fs, &child, budget, limit).await?;
            } else {
                fs.remove(&child).await?;
            }
        }
        fs.rmdir(dir).await?;
        Ok(())
    })
}

#[cfg(test)]
pub(crate) async fn delete_tree_with_budget(
    service: &SftpService,
    conn_id: &str,
    dir: &str,
    limit: usize,
) -> Result<(), Problem> {
    let (_lease, fs) = service.acquire(conn_id).await?;
    let mut budget = limit;
    delete_tree(&fs, dir, &mut budget, limit).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_upload_query_values_like_the_route() {
        assert_eq!(
            UploadOptions::from_query(None, None).unwrap(),
            UploadOptions::default()
        );
        let options = UploadOptions::from_query(Some("true"), Some("1700000000000")).unwrap();
        assert!(options.overwrite);
        assert_eq!(options.expected_mtime_ms, Some(1_700_000_000_000.0));
        assert_eq!(
            UploadOptions::from_query(Some("yes"), None).unwrap_err(),
            Problem::bad_request("overwrite must be true or false")
        );
        for bad in ["soon", "-1", "inf"] {
            assert_eq!(
                UploadOptions::from_query(None, Some(bad)).unwrap_err(),
                Problem::bad_request("expectedMtimeMs must be a timestamp"),
                "{bad}"
            );
        }
    }

    #[test]
    fn reads_owner_and_group_from_long_names_and_types_from_modes() {
        assert_eq!(
            owner_from_longname("drwxr-xr-x  2 alice staff 4096 Jan 1 00:00 src"),
            (Some("alice".into()), Some("staff".into()))
        );
        assert_eq!(owner_from_longname(""), (None, None));
        assert_eq!(entry_type(0o040755), SftpEntryType::Dir);
        assert_eq!(entry_type(0o100644), SftpEntryType::File);
        assert_eq!(entry_type(0o120777), SftpEntryType::Link);
        assert_eq!(entry_type(0o140755), SftpEntryType::Other);
        assert_eq!(entry_type(0), SftpEntryType::Other);
        assert_eq!(mtime_ms(Some(0)), None);
        assert_eq!(mtime_ms(Some(123)), Some(123_000));
        assert_eq!(format_bytes(MAX_EDITOR_BYTES), "8 MiB");
    }
}
