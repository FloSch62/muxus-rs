//! Typed requests on a connection's shared SFTP session, with pipelined reads and writes.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use russh_sftp::client::error::Error as ClientError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::connection::manager::{ManagedConnection, SftpChannel};

/// OpenSSH's atomic, replacing rename.
pub(crate) const POSIX_RENAME: &str = "posix-rename@openssh.com";
/// Read and write request size. Every SFTP server must accept 32 KiB data packets.
pub(crate) const CHUNK: usize = 32 * 1024;
/// Read requests kept in flight once a transfer is under way.
const READ_WINDOW: usize = 16;
/// Write requests kept in flight.
const WRITE_WINDOW: usize = 8;

/// `ssh2`'s fallback texts for status codes the server sent without a message.
fn status_text(code: StatusCode) -> &'static str {
    match code {
        StatusCode::Ok => "No error",
        StatusCode::Eof => "End of file",
        StatusCode::NoSuchFile => "No such file or directory",
        StatusCode::PermissionDenied => "Permission denied",
        StatusCode::Failure => "Failure",
        StatusCode::BadMessage => "Bad message",
        StatusCode::NoConnection => "No connection",
        StatusCode::ConnectionLost => "Connection lost",
        StatusCode::OpUnsupported => "Operation unsupported",
    }
}

/// A failed SFTP request.
#[derive(Debug, Clone, PartialEq)]
pub enum SftpError {
    /// The server answered with an error status.
    Status { code: StatusCode, message: String },
    /// The session had already ended; the request never left the client.
    Closed,
    /// The session ended while the request was outstanding.
    Lost(String),
    /// Anything else, e.g. a request that timed out.
    Other(String),
}

impl SftpError {
    pub fn code(&self) -> Option<StatusCode> {
        match self {
            Self::Status { code, .. } => Some(*code),
            _ => None,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Status { message, .. } => message.clone(),
            Self::Closed => "SFTP session closed".into(),
            Self::Lost(message) | Self::Other(message) => message.clone(),
        }
    }

    fn is_eof(&self) -> bool {
        self.code() == Some(StatusCode::Eof)
    }

    /// The server does not implement an extension (also what `ssh2` reported locally for an
    /// extension the server never advertised).
    pub fn is_unsupported(&self) -> bool {
        self.code() == Some(StatusCode::OpUnsupported)
    }
}

impl std::fmt::Display for SftpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for SftpError {}

impl From<ClientError> for SftpError {
    fn from(err: ClientError) -> Self {
        match err {
            ClientError::Status(status) => {
                let message = if status.error_message.is_empty() {
                    status_text(status.status_code).to_string()
                } else {
                    status.error_message
                };
                Self::Status {
                    code: status.status_code,
                    message,
                }
            }
            ClientError::UnexpectedBehavior(message) if message == "session closed" => Self::Closed,
            ClientError::UnexpectedBehavior(message)
                if message.starts_with("sender dropped")
                    || message.starts_with("RecvError")
                    || message.starts_with("SendError") =>
            {
                Self::Lost(message)
            }
            ClientError::IO(message) => Self::Lost(message),
            other => Self::Other(other.to_string()),
        }
    }
}

pub(crate) type SftpResult<T> = Result<T, SftpError>;

/// One outstanding read request: the data at its offset.
type ReadTask = JoinHandle<Result<Vec<u8>, ClientError>>;

/// Status-only results (rename, remove, ...) from a raw request.
fn status_only<T>(result: Result<T, ClientError>) -> Result<(), ClientError> {
    result.map(|_| ())
}

/// The connection's SFTP session as one file operation uses it.
///
/// A session whose channel ended (the server's sftp-server exited) is dropped from the
/// connection, so the next operation opens a fresh one, as the TypeScript manager did on the
/// channel's `close`. When an operation's very first request finds the session already ended,
/// nothing reached the server yet and the request is repeated on a fresh session.
#[derive(Clone)]
pub(crate) struct RemoteFs {
    inner: Arc<Inner>,
}

struct Inner {
    connection: Arc<ManagedConnection>,
    channel: Mutex<Arc<SftpChannel>>,
    started: AtomicBool,
}

impl RemoteFs {
    pub(crate) async fn open_on(connection: Arc<ManagedConnection>) -> Result<Self, String> {
        let channel = connection.sftp().await?;
        Ok(Self {
            inner: Arc::new(Inner {
                connection,
                channel: Mutex::new(channel),
                started: AtomicBool::new(false),
            }),
        })
    }

    pub(crate) fn channel(&self) -> Arc<SftpChannel> {
        self.inner
            .channel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn supports(&self, extension: &str) -> bool {
        self.channel().extensions.contains_key(extension)
    }

    /// Runs one request, dropping a dead session from the connection.
    async fn call<T, F, Fut>(&self, request: F) -> SftpResult<T>
    where
        F: Fn(Arc<SftpChannel>) -> Fut,
        Fut: Future<Output = Result<T, ClientError>>,
    {
        let result = request(self.channel()).await.map_err(SftpError::from);
        let first = !self.inner.started.swap(true, Ordering::SeqCst);
        match &result {
            Err(SftpError::Closed) => {
                self.inner.connection.reset_sftp().await;
                if first {
                    let fresh = self
                        .inner
                        .connection
                        .sftp()
                        .await
                        .map_err(SftpError::Other)?;
                    *self.inner.channel.lock().unwrap_or_else(|e| e.into_inner()) = fresh.clone();
                    return request(fresh).await.map_err(SftpError::from);
                }
            }
            Err(SftpError::Lost(_)) => self.inner.connection.reset_sftp().await,
            _ => {}
        }
        result
    }

    pub(crate) async fn realpath(&self, path: &str) -> SftpResult<String> {
        let name = self
            .call(|c| {
                let path = path.to_string();
                async move { c.session.realpath(path).await }
            })
            .await?;
        name.files
            .into_iter()
            .next()
            .map(|file| file.filename)
            .ok_or_else(|| SftpError::Other("no file".into()))
    }

    /// Every entry of a directory, `.` and `..` included, with the server's long names.
    pub(crate) async fn read_dir(&self, path: &str) -> SftpResult<Vec<russh_sftp::protocol::File>> {
        let handle = self
            .call(|c| {
                let path = path.to_string();
                async move { c.session.opendir(path).await }
            })
            .await?
            .handle;
        let session = self.channel();
        let mut entries = Vec::new();
        let result = loop {
            match session.session.readdir(handle.as_str()).await {
                Ok(name) => entries.extend(name.files),
                Err(err) => {
                    let err = SftpError::from(err);
                    break if err.is_eof() { Ok(()) } else { Err(err) };
                }
            }
        };
        let _ = session.session.close(handle).await;
        result.map(|()| entries)
    }

    pub(crate) async fn stat(&self, path: &str) -> SftpResult<FileAttributes> {
        self.call(|c| {
            let path = path.to_string();
            async move { c.session.stat(path).await }
        })
        .await
        .map(|attrs| attrs.attrs)
    }

    pub(crate) async fn lstat(&self, path: &str) -> SftpResult<FileAttributes> {
        self.call(|c| {
            let path = path.to_string();
            async move { c.session.lstat(path).await }
        })
        .await
        .map(|attrs| attrs.attrs)
    }

    /// `lstat`, with a missing file as `None`.
    pub(crate) async fn lstat_if_present(&self, path: &str) -> SftpResult<Option<FileAttributes>> {
        match self.lstat(path).await {
            Ok(attrs) => Ok(Some(attrs)),
            Err(err) if err.code() == Some(StatusCode::NoSuchFile) => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub(crate) async fn mkdir(&self, path: &str) -> SftpResult<()> {
        self.call(|c| {
            let path = path.to_string();
            async move { status_only(c.session.mkdir(path, FileAttributes::empty()).await) }
        })
        .await
    }

    pub(crate) async fn rmdir(&self, path: &str) -> SftpResult<()> {
        self.call(|c| {
            let path = path.to_string();
            async move { status_only(c.session.rmdir(path).await) }
        })
        .await
    }

    pub(crate) async fn remove(&self, path: &str) -> SftpResult<()> {
        self.call(|c| {
            let path = path.to_string();
            async move { status_only(c.session.remove(path).await) }
        })
        .await
    }

    pub(crate) async fn rename(&self, from: &str, to: &str) -> SftpResult<()> {
        self.call(|c| {
            let (from, to) = (from.to_string(), to.to_string());
            async move { status_only(c.session.rename(from, to).await) }
        })
        .await
    }

    /// `posix-rename@openssh.com`: replaces the destination atomically. A server that never
    /// advertised it gets no request; that answers "unsupported", as `ssh2` did.
    pub(crate) async fn posix_rename(&self, from: &str, to: &str) -> SftpResult<()> {
        if !self.supports(POSIX_RENAME) {
            return Err(SftpError::Status {
                code: StatusCode::OpUnsupported,
                message: "Server does not support this extended request".into(),
            });
        }
        let mut data = Vec::with_capacity(8 + from.len() + to.len());
        for part in [from, to] {
            data.extend_from_slice(&(part.len() as u32).to_be_bytes());
            data.extend_from_slice(part.as_bytes());
        }
        let reply = self
            .call(|c| {
                let data = data.clone();
                async move { c.session.extended(POSIX_RENAME, data).await }
            })
            .await?;
        match reply {
            Packet::Status(status) if status.status_code == StatusCode::Ok => Ok(()),
            Packet::Status(status) => Err(ClientError::Status(status).into()),
            _ => Err(SftpError::Other("Unexpected packet".into())),
        }
    }

    /// Opens a file; `mode` (permission bits) applies when the open creates it.
    pub(crate) async fn open(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: Option<u32>,
    ) -> SftpResult<String> {
        let attrs = FileAttributes {
            permissions: mode,
            ..FileAttributes::empty()
        };
        self.call(|c| {
            let (path, attrs) = (path.to_string(), attrs.clone());
            async move { c.session.open(path, flags, attrs).await }
        })
        .await
        .map(|handle| handle.handle)
    }

    pub(crate) async fn close(&self, handle: &str) -> SftpResult<()> {
        self.call(|c| {
            let handle = handle.to_string();
            async move { status_only(c.session.close(handle).await) }
        })
        .await
    }

    /// Streams an open file from its start: chunks in order on the returned channel, ending with
    /// the channel closing at EOF or after an error. The handle is closed and `on_end` runs once
    /// the file is read through or the receiver is dropped.
    pub(crate) fn stream_reader(
        &self,
        handle: String,
        on_end: Box<dyn FnOnce() + Send>,
    ) -> mpsc::Receiver<SftpResult<Vec<u8>>> {
        let (tx, rx) = mpsc::channel(2);
        let fs = self.clone();
        tokio::spawn(async move {
            fs.pump_reads(&handle, &tx).await;
            let _ = fs.close(&handle).await;
            on_end();
        });
        rx
    }

    async fn pump_reads(&self, handle: &str, tx: &mpsc::Sender<SftpResult<Vec<u8>>>) {
        let session = self.channel();
        let mut in_flight: VecDeque<(u64, ReadTask)> = VecDeque::new();
        let mut next = 0u64;
        // Ramp up like a slow start, so small files cost one request plus the EOF probe.
        let mut window = 1usize;
        let abort_all = |in_flight: &mut VecDeque<(u64, ReadTask)>| {
            for (_, task) in in_flight.drain(..) {
                task.abort();
            }
        };
        loop {
            while in_flight.len() < window {
                let (session, handle) = (session.clone(), handle.to_string());
                let offset = next;
                in_flight.push_back((
                    offset,
                    tokio::spawn(async move {
                        session
                            .session
                            .read(handle, offset, CHUNK as u32)
                            .await
                            .map(|data| data.data)
                    }),
                ));
                next += CHUNK as u64;
            }
            let Some((offset, task)) = in_flight.pop_front() else {
                break;
            };
            let result = tokio::select! {
                result = task => result,
                () = tx.closed() => break,
            };
            match result {
                Ok(Ok(data)) if !data.is_empty() => {
                    let len = data.len();
                    if tx.send(Ok(data)).await.is_err() {
                        break;
                    }
                    if len < CHUNK {
                        // A short read: drop the requests beyond it and go on from the gap.
                        abort_all(&mut in_flight);
                        next = offset + len as u64;
                        window = 1;
                    } else {
                        window = (window * 2).min(READ_WINDOW);
                    }
                }
                Ok(Ok(_)) => break,
                Ok(Err(err)) => {
                    let err = SftpError::from(err);
                    if !err.is_eof() {
                        let _ = tx.send(Err(err)).await;
                    }
                    break;
                }
                Err(join) => {
                    let _ = tx.send(Err(SftpError::Other(join.to_string()))).await;
                    break;
                }
            }
        }
        abort_all(&mut in_flight);
    }

    /// Writes `body` into an open file from offset 0, keeping several writes in flight.
    /// `on_written` gets the bytes the server acknowledged so far.
    pub(crate) async fn write_from<R>(
        &self,
        handle: &str,
        body: &mut R,
        on_written: &mut (dyn FnMut(u64) + Send),
    ) -> Result<u64, WriteError>
    where
        R: AsyncRead + Unpin + ?Sized,
    {
        let session = self.channel();
        let mut in_flight: VecDeque<JoinHandle<Result<usize, ClientError>>> = VecDeque::new();
        let mut offset = 0u64;
        let mut acknowledged = 0u64;
        let result: Result<u64, WriteError> = async {
            loop {
                let mut chunk = vec![0u8; CHUNK];
                let mut filled = 0;
                while filled < CHUNK {
                    let n = body
                        .read(&mut chunk[filled..])
                        .await
                        .map_err(WriteError::Body)?;
                    if n == 0 {
                        break;
                    }
                    filled += n;
                }
                if filled == 0 {
                    break;
                }
                chunk.truncate(filled);
                if in_flight.len() >= WRITE_WINDOW {
                    let task = in_flight.pop_front().expect("window is full");
                    acknowledged += await_write(task).await? as u64;
                    on_written(acknowledged);
                }
                let (session, handle) = (session.clone(), handle.to_string());
                let at = offset;
                in_flight.push_back(tokio::spawn(async move {
                    session
                        .session
                        .write(handle, at, chunk)
                        .await
                        .map(|_| filled)
                }));
                offset += filled as u64;
            }
            while let Some(task) = in_flight.pop_front() {
                acknowledged += await_write(task).await? as u64;
                on_written(acknowledged);
            }
            Ok(offset)
        }
        .await;
        for task in in_flight {
            task.abort();
        }
        if let Err(WriteError::Sftp(SftpError::Lost(_))) = &result {
            self.inner.connection.reset_sftp().await;
        }
        result
    }
}

async fn await_write(task: JoinHandle<Result<usize, ClientError>>) -> Result<usize, WriteError> {
    match task.await {
        Ok(result) => result.map_err(|err| WriteError::Sftp(err.into())),
        Err(join) => Err(WriteError::Sftp(SftpError::Other(join.to_string()))),
    }
}

/// Why streaming a body into a remote file stopped.
#[derive(Debug)]
pub(crate) enum WriteError {
    /// Reading the local body failed (an interrupted upload).
    Body(std::io::Error),
    Sftp(SftpError),
}

impl WriteError {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Body(err) => err.to_string(),
            Self::Sftp(err) => err.message(),
        }
    }
}
