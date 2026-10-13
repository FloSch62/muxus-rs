//! SFTP file operations on a live SSH connection (port of `server/src/routes/sftp.ts`).
//!
//! The connection id comes from the terminal's `ready` message, so the file panel shares the
//! session's single SSH connection (MobaXterm-style) instead of dialing a second one. Each
//! operation holds an SFTP lease on that connection while it runs, and every operation shares the
//! connection's one SFTP session. The HTTP routes became methods; their failures are [`Problem`]s
//! carrying the status the route answered with.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::mpsc;

use super::fs::{RemoteFs, SftpError, SftpResult, WriteError};
use super::lease::SftpWindowLease;
use super::paths;
use super::progress::{ByteProgress, ProgressMeter, UploadStep};
use super::types::{
    SftpClipboardImageResponse, SftpEntry, SftpEntryType, SftpFileResponse, SftpFileSaveRequest,
    SftpFileSaveResponse, SftpHomeResponse, SftpListResponse, SftpOkResponse, SftpUploadResponse,
};
use crate::Problem;
use crate::connection::leases::ConnectionLeaseOwner;
use crate::connection::manager::{ConnectionLease, SshConnectionManager};

/// Directory-tree deletes stop after this many entries — runaway guard.
pub const MAX_RECURSIVE_DELETE: usize = 10_000;
/// The editor is a text editor, not a way to accidentally buffer huge remote artifacts.
pub const MAX_EDITOR_BYTES: u64 = 8 * 1024 * 1024;
/// How long a drag-out download ticket stays valid.
pub const DOWNLOAD_TICKET_TTL: Duration = Duration::from_secs(2 * 60);
/// Pasted clipboard images are readable by their owner only.
pub const CLIPBOARD_IMAGE_MODE: u32 = 0o600;
pub const CLIPBOARD_IMAGE_TEMP_DIR: &str = "/tmp";

/// Problem codes the client acts on.
pub const SFTP_DESTINATION_EXISTS: &str = "SFTP_DESTINATION_EXISTS";
pub const SFTP_DESTINATION_IS_DIRECTORY: &str = "SFTP_DESTINATION_IS_DIRECTORY";
pub const SFTP_DESTINATION_IS_SYMLINK: &str = "SFTP_DESTINATION_IS_SYMLINK";
pub const SFTP_EDITOR_SYMLINK: &str = "SFTP_EDITOR_SYMLINK";
pub const SFTP_FILE_CHANGED: &str = "SFTP_FILE_CHANGED";

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

/// Upload request options (the `overwrite` and `expectedMtimeMs` query parameters).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UploadOptions {
    /// Replace an existing regular file; the user agreed to it.
    pub overwrite: bool,
    /// Upload only if the remote file still has this modification time (ms).
    pub expected_mtime_ms: Option<f64>,
    /// Size of the body, for progress only.
    pub size: Option<u64>,
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
    expires_at: Instant,
}

/// The SFTP operations of the file browser and the remote editor.
#[derive(Clone)]
pub struct SftpService {
    manager: SshConnectionManager,
    tickets: Arc<Mutex<HashMap<String, DownloadTicket>>>,
    ticket_ttl: Duration,
}

impl SftpService {
    pub fn new(manager: SshConnectionManager) -> Self {
        Self::with_ticket_ttl(manager, DOWNLOAD_TICKET_TTL)
    }

    pub(crate) fn with_ticket_ttl(manager: SshConnectionManager, ticket_ttl: Duration) -> Self {
        Self {
            manager,
            tickets: Arc::new(Mutex::new(HashMap::new())),
            ticket_ttl,
        }
    }

    pub fn manager(&self) -> &SshConnectionManager {
        &self.manager
    }

    /// Holds the connection open while a file browser window shows it (`/ws/sftp/:connId/lease`).
    pub fn hold_window(&self, conn_id: &str) -> Result<SftpWindowLease, Problem> {
        SftpWindowLease::acquire(&self.manager, conn_id)
    }

    /// An SFTP lease for one operation, plus the connection's shared session.
    async fn acquire(&self, conn_id: &str) -> Result<(ConnectionLease, RemoteFs), Problem> {
        let lease = self
            .manager
            .acquire(conn_id, ConnectionLeaseOwner::Sftp)
            .ok_or_else(|| Problem::not_found("connection not found"))?;
        // A failed session open drops (releases) the lease again.
        let fs = RemoteFs::open_on(lease.connection().clone())
            .await
            .map_err(Problem::internal)?;
        Ok((lease, fs))
    }

    fn issue_download_ticket(&self, conn_id: &str, file: String) -> String {
        let now = Instant::now();
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|_, ticket| ticket.expires_at > now);
        let ticket =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 18]>());
        tickets.insert(
            ticket.clone(),
            DownloadTicket {
                conn_id: conn_id.to_string(),
                path: file,
                expires_at: now + self.ticket_ttl,
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
    /// carry a short-lived drag-out download ticket.
    pub async fn list(&self, conn_id: &str, path: &str) -> Result<SftpListResponse, Problem> {
        let (_lease, fs) = self.acquire(conn_id).await?;
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
                    self.issue_download_ticket(conn_id, paths::join(&resolved, &item.filename))
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
        let (lease, fs) = self.acquire(conn_id).await?;
        let file = require_path(path)?;
        let stat = fs.stat(file).await?;
        let handle = fs.open(file, OpenFlags::READ, None).await?;
        Ok(SftpDownload::start(
            lease,
            &fs,
            handle,
            paths::basename(file),
            stat.size,
            mtime_ms(stat.mtime),
        ))
    }

    /// Streams the file a listing's download ticket names (native drag-out downloads).
    pub async fn drag_download(
        &self,
        conn_id: &str,
        ticket: &str,
    ) -> Result<SftpDownload, Problem> {
        let file = {
            let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
            match tickets.get(ticket) {
                Some(value) if value.conn_id == conn_id && value.expires_at > Instant::now() => {
                    value.path.clone()
                }
                _ => {
                    tickets.remove(ticket);
                    return Err(Problem::new(401, "download ticket is missing or expired"));
                }
            }
        };
        let (lease, fs) = self.acquire(conn_id).await?;
        let stat = fs.stat(&file).await?;
        let handle = fs.open(&file, OpenFlags::READ, None).await?;
        Ok(SftpDownload::start(
            lease,
            &fs,
            handle,
            paths::basename(&file),
            stat.size,
            None,
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
        let (_lease, fs) = self.acquire(conn_id).await?;
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
        let upload = StreamUpload {
            fs: &fs,
            lease: self.manager.acquire(conn_id, ConnectionLeaseOwner::Sftp),
            file,
            overwrite,
            mode,
            size: options.size,
        };
        if let Err(err) = upload.run(&mut body, on_progress).await {
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
            lease: self.manager.acquire(conn_id, ConnectionLeaseOwner::Sftp),
            file: &file,
            overwrite: false,
            mode: Some(CLIPBOARD_IMAGE_MODE),
            size: None,
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
    chunks: mpsc::Receiver<SftpResult<Vec<u8>>>,
    buffer: Vec<u8>,
    position: usize,
    lease: Arc<ConnectionLease>,
}

impl std::fmt::Debug for SftpDownload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpDownload")
            .field("name", &self.name)
            .field("size", &self.size)
            .field("mtime_ms", &self.mtime_ms)
            .finish()
    }
}

impl SftpDownload {
    fn start(
        lease: ConnectionLease,
        fs: &RemoteFs,
        handle: String,
        name: String,
        size: Option<u64>,
        mtime_ms: Option<i64>,
    ) -> Self {
        let lease = Arc::new(lease);
        // The lease outlives this call: it goes when the remote stream ends or the download is
        // dropped, whichever comes first.
        let release = lease.clone();
        let chunks = fs.stream_reader(handle, Box::new(move || release.release()));
        Self {
            name,
            size,
            mtime_ms,
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
        let mut meter = ProgressMeter::new();
        if let Some(progress) = meter.report(0, self.size, true) {
            on_progress(progress);
        }
        let mut loaded = 0u64;
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
        Ok(loaded)
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
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
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
            .open(&temp, write_exclusive(), mode.map(|mode| mode & 0o7777))
            .await?;
        let written = fs.write_from(&handle, &mut &bytes[..], &mut |_| {}).await;
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

/// `wx`: create a new file for writing, failing if it exists.
fn write_exclusive() -> OpenFlags {
    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::EXCLUDE
}

/// Uploads into a sibling temporary file. If the body fails, the SFTP stream fails, or the
/// upload is dropped half way, only that temporary file is removed; an existing destination is
/// never left truncated.
struct StreamUpload<'a> {
    fs: &'a RemoteFs,
    /// Keeps the transport up for the clean-up of a dropped upload.
    lease: Option<ConnectionLease>,
    file: &'a str,
    overwrite: bool,
    mode: Option<u32>,
    size: Option<u64>,
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
        let temp = format!("{}.muxus-{}.upload", self.file, random_hex::<6>());
        let mut guard = TempFileGuard {
            fs: Some(self.fs.clone()),
            path: temp.clone(),
            lease: self.lease,
        };
        let result: Result<(), Problem> = async {
            let handle = self
                .fs
                .open(
                    &temp,
                    write_exclusive(),
                    self.mode.map(|mode| mode & 0o7777),
                )
                .await?;
            let total = self.size;
            let mut meter = ProgressMeter::new();
            if let Some(progress) = meter.report(0, total, true) {
                on_progress(UploadStep::Progress(progress));
            }
            let written = self
                .fs
                .write_from(&handle, body, &mut |acknowledged| {
                    let done = total.is_some_and(|total| acknowledged >= total);
                    if let Some(progress) = meter.report(acknowledged, total, done) {
                        on_progress(UploadStep::Progress(progress));
                    }
                })
                .await;
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
}

/// Removes a staged temporary file unless disarmed, also when its upload is dropped.
struct TempFileGuard {
    fs: Option<RemoteFs>,
    path: String,
    lease: Option<ConnectionLease>,
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
