//! SFTP file operations on a live SSH connection (`server/src/routes/sftp.ts`) and the detached
//! file browser's lease (`ws/sftp-lease-socket.ts`). The logic lives in
//! [`muxus_ssh::sftp::SftpService`]; these methods run it on the backend runtime.

use std::path::PathBuf;
use std::sync::Arc;

use muxus_core::api::{
    SftpFileResponse, SftpFileSaveRequest, SftpFileSaveResponse, SftpListResponse,
    SftpUploadResponse,
};
use muxus_ssh::connection::BoxFuture;
use muxus_ssh::sftp::upload::{
    ConfirmRequest, DropPayload, SftpUploadDeps, UploadError, UploadProgress, UploadResult,
    upload_to_remote,
};
use muxus_ssh::sftp::{
    CancelSignal, SftpClipboardImageResponse, SftpDownload, SftpHomeResponse, SftpOkResponse,
    SftpWindowLease, UploadOptions, UploadStep,
};

use crate::backend::Backend;
use crate::error::{ApiError, ApiResult};

const DOWNLOAD_QUEUE: usize = 8;

/// A remote file being read; chunks arrive as the backend reads them. Dropping it stops the
/// transfer and releases its hold on the connection.
pub struct SftpDownloadStream {
    pub name: String,
    pub size: Option<u64>,
    pub mtime_ms: Option<i64>,
    chunks: async_channel::Receiver<ApiResult<Vec<u8>>>,
}

impl std::fmt::Debug for SftpDownloadStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpDownloadStream")
            .field("name", &self.name)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl SftpDownloadStream {
    fn start(download: SftpDownload, runtime: &tokio::runtime::Handle) -> Self {
        let (tx, rx) = async_channel::bounded(DOWNLOAD_QUEUE);
        let header = (download.name.clone(), download.size, download.mtime_ms);
        runtime.spawn(async move {
            let mut download = download;
            loop {
                let chunk = download.next_chunk().await;
                let done = !matches!(chunk, Ok(Some(_)));
                let item = match chunk {
                    Ok(Some(bytes)) => Ok(bytes),
                    Ok(None) => break,
                    Err(problem) => Err(ApiError::from(problem)),
                };
                if tx.send(item).await.is_err() || done {
                    break;
                }
            }
        });
        Self {
            name: header.0,
            size: header.1,
            mtime_ms: header.2,
            chunks: rx,
        }
    }

    /// The next chunk; `None` once the file is read through.
    pub async fn next_chunk(&self) -> Option<ApiResult<Vec<u8>>> {
        self.chunks.recv().await.ok()
    }

    /// The rest of the file.
    pub async fn read_all(self) -> ApiResult<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(chunk) = self.next_chunk().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(out)
    }
}

/// A failed file operation leaves a record, as the server's request log has the failed
/// request (`statusCode` and error): server-side failures as warnings, refusals the user can
/// act on (missing file, existing destination) as information. A cancelled upload is not a
/// failure.
fn logged<T>(op: &str, conn_id: &str, path: &str, result: ApiResult<T>) -> ApiResult<T> {
    if let Err(error) = &result {
        let status = error.status;
        let code = error.code.as_deref().unwrap_or("");
        if status == 499 {
            return result;
        }
        if status >= 500 {
            tracing::warn!(op, conn_id, path, status, code, err = %error.message, "sftp request failed");
        } else {
            tracing::info!(op, conn_id, path, status, code, err = %error.message, "sftp request refused");
        }
    }
    result
}

fn upload_error(error: UploadError) -> ApiError {
    match error {
        UploadError::Cancelled => ApiError::with_code(499, "Upload cancelled", "upload-cancelled"),
        UploadError::Failed(problem) => problem.into(),
    }
}

impl Backend {
    /// `GET /api/sftp/:connId/home`.
    pub async fn sftp_home(&self, conn_id: &str) -> ApiResult<SftpHomeResponse> {
        let log_target = (conn_id.to_string(), ".".to_string());
        let conn_id = conn_id.to_string();
        let result = self
            .run(move |shared| async move { Ok(shared.sftp.home(&conn_id).await?) })
            .await;
        logged("home", &log_target.0, &log_target.1, result)
    }

    /// `GET /api/sftp/:connId/list`; `.` is the home directory.
    pub async fn sftp_list(&self, conn_id: &str, path: &str) -> ApiResult<SftpListResponse> {
        let log_target = (conn_id.to_string(), path.to_string());
        let (conn_id, path) = (conn_id.to_string(), path.to_string());
        let result = self
            .run(move |shared| async move { Ok(shared.sftp.list(&conn_id, &path).await?) })
            .await;
        logged("list", &log_target.0, &log_target.1, result)
    }

    /// `GET /api/sftp/:connId/download`.
    pub async fn sftp_download(&self, conn_id: &str, path: &str) -> ApiResult<SftpDownloadStream> {
        let log_target = (conn_id.to_string(), path.to_string());
        let (conn_id, path) = (conn_id.to_string(), path.to_string());
        let result = self
            .run(move |shared| async move {
                let download = shared.sftp.download(&conn_id, &path).await?;
                Ok(SftpDownloadStream::start(download, &shared.runtime))
            })
            .await;
        logged("download", &log_target.0, &log_target.1, result)
    }

    /// `GET /api/sftp/:connId/drag-download`: a file named by a listing's drag-out ticket.
    pub async fn sftp_drag_download(
        &self,
        conn_id: &str,
        ticket: &str,
    ) -> ApiResult<SftpDownloadStream> {
        let log_target = (conn_id.to_string(), "".to_string());
        let (conn_id, ticket) = (conn_id.to_string(), ticket.to_string());
        let result = self
            .run(move |shared| async move {
                let download = shared.sftp.drag_download(&conn_id, &ticket).await?;
                Ok(SftpDownloadStream::start(download, &shared.runtime))
            })
            .await;
        logged("drag download", &log_target.0, &log_target.1, result)
    }

    /// `GET /api/sftp/:connId/file`: a text file for the remote editor.
    pub async fn sftp_read_file(&self, conn_id: &str, path: &str) -> ApiResult<SftpFileResponse> {
        let log_target = (conn_id.to_string(), path.to_string());
        let (conn_id, path) = (conn_id.to_string(), path.to_string());
        let result = self
            .run(move |shared| async move { Ok(shared.sftp.read_file(&conn_id, &path).await?) })
            .await;
        logged("read", &log_target.0, &log_target.1, result)
    }

    /// `PUT /api/sftp/:connId/file`.
    pub async fn sftp_save_file(
        &self,
        conn_id: &str,
        path: &str,
        request: SftpFileSaveRequest,
    ) -> ApiResult<SftpFileSaveResponse> {
        let log_target = (conn_id.to_string(), path.to_string());
        let (conn_id, path) = (conn_id.to_string(), path.to_string());
        let result = self
            .run(move |shared| async move {
                Ok(shared.sftp.save_file(&conn_id, &path, &request).await?)
            })
            .await;
        logged("save", &log_target.0, &log_target.1, result)
    }

    /// `POST /api/sftp/:connId/upload` from a local file. `on_step` reports progress (smoothed)
    /// and the move into place.
    pub async fn sftp_upload(
        &self,
        conn_id: &str,
        path: &str,
        options: UploadOptions,
        source: PathBuf,
        on_step: impl FnMut(UploadStep) + Send + 'static,
    ) -> ApiResult<SftpUploadResponse> {
        let log_target = (conn_id.to_string(), path.to_string());
        let (conn_id, path) = (conn_id.to_string(), path.to_string());
        let result = self
            .run(move |shared| async move {
                let mut on_step = on_step;
                let body = tokio::fs::File::open(&source).await?;
                Ok(shared
                    .sftp
                    .upload(&conn_id, &path, &options, body, &mut on_step)
                    .await?)
            })
            .await;
        logged("upload", &log_target.0, &log_target.1, result)
    }

    /// Upload a drop of local files and folders into `directory` (`client/src/sftp-upload.ts`):
    /// folders first, then each file; an existing file is replaced only when `confirm` says so.
    /// A cancelled upload fails with code `upload-cancelled`.
    pub async fn sftp_upload_drop(
        &self,
        conn_id: &str,
        directory: &str,
        payload: DropPayload,
        cancel: CancelSignal,
        on_progress: impl FnMut(&UploadProgress) + Send + 'static,
        confirm: impl Fn(ConfirmRequest) -> BoxFuture<'static, bool> + Send + Sync + 'static,
    ) -> ApiResult<UploadResult> {
        let log_target = (conn_id.to_string(), directory.to_string());
        let (conn_id, directory) = (conn_id.to_string(), directory.to_string());
        let result = self
            .run(move |shared| async move {
                let mut on_progress = on_progress;
                let deps = SftpUploadDeps {
                    service: shared.sftp.clone(),
                    confirm,
                };
                upload_to_remote(
                    &conn_id,
                    &directory,
                    &payload,
                    &cancel,
                    &mut on_progress,
                    &deps,
                )
                .await
                .map_err(upload_error)
            })
            .await;
        logged("upload", &log_target.0, &log_target.1, result)
    }

    /// `POST /api/sftp/:connId/clipboard-image`: a pasted image in the remote `/tmp`, mode 600.
    pub async fn sftp_clipboard_image(
        &self,
        conn_id: &str,
        image: Vec<u8>,
    ) -> ApiResult<SftpClipboardImageResponse> {
        let log_target = (conn_id.to_string(), "/tmp".to_string());
        let conn_id = conn_id.to_string();
        let result = self
            .run(move |shared| async move {
                Ok(shared
                    .sftp
                    .clipboard_image(&conn_id, std::io::Cursor::new(image))
                    .await?)
            })
            .await;
        logged("clipboard image", &log_target.0, &log_target.1, result)
    }

    /// `POST /api/sftp/:connId/mkdir`.
    pub async fn sftp_mkdir(
        &self,
        conn_id: &str,
        path: &str,
        recursive: bool,
    ) -> ApiResult<SftpOkResponse> {
        let log_target = (conn_id.to_string(), path.to_string());
        let (conn_id, path) = (conn_id.to_string(), path.to_string());
        let result =
            self.run(move |shared| async move {
                Ok(shared.sftp.mkdir(&conn_id, &path, recursive).await?)
            })
            .await;
        logged("mkdir", &log_target.0, &log_target.1, result)
    }

    /// `POST /api/sftp/:connId/rename`.
    pub async fn sftp_rename(
        &self,
        conn_id: &str,
        from: &str,
        to: &str,
    ) -> ApiResult<SftpOkResponse> {
        let log_target = (conn_id.to_string(), from.to_string());
        let (conn_id, from, to) = (conn_id.to_string(), from.to_string(), to.to_string());
        let result = self
            .run(move |shared| async move { Ok(shared.sftp.rename(&conn_id, &from, &to).await?) })
            .await;
        logged("rename", &log_target.0, &log_target.1, result)
    }

    /// `DELETE /api/sftp/:connId`: a file or a whole tree.
    pub async fn sftp_delete(&self, conn_id: &str, path: &str) -> ApiResult<SftpOkResponse> {
        let log_target = (conn_id.to_string(), path.to_string());
        let (conn_id, path) = (conn_id.to_string(), path.to_string());
        let result = self
            .run(move |shared| async move { Ok(shared.sftp.delete(&conn_id, &path).await?) })
            .await;
        logged("delete", &log_target.0, &log_target.1, result)
    }

    /// `/ws/sftp/:connId/lease`: a detached file browser window holds the connection open
    /// while it shows it. Drop the lease when the window closes; `closed()` resolves when the
    /// connection goes away first.
    pub fn hold_sftp_window(&self, conn_id: &str) -> ApiResult<Arc<SftpWindowLease>> {
        Ok(Arc::new(self.shared.sftp.hold_window(conn_id)?))
    }
}
