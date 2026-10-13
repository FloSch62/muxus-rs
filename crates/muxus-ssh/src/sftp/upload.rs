//! Files and folders dropped or picked for upload, and the upload itself (port of
//! `client/src/sftp-upload.ts`). The file browser and a drop onto an SSH terminal both go through
//! here, so both create folders, report progress and ask before replacing a file alike.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::progress::{ByteProgress, CancelSignal, UploadStep};
use super::service::{SFTP_DESTINATION_EXISTS, SftpService, UploadOptions};
use crate::Problem;
use crate::connection::io::BoxFuture;

/// One local file of a drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedFile {
    pub source: PathBuf,
    /// Path below the upload directory, with `/` separators.
    pub relative_path: String,
    pub size: u64,
}

impl DroppedFile {
    fn file_name(&self) -> String {
        self.source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// What a drop holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DropPayload {
    pub files: Vec<DroppedFile>,
    /// Folders to create, even when empty.
    pub directories: Vec<String>,
}

impl DropPayload {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.directories.is_empty()
    }
}

/// Phases of a transfer as the progress bar shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferPhase {
    Preparing,
    Transferring,
    Finalizing,
    Cancelling,
    Complete,
}

/// An upload's progress (`UploadProgress`: `TransferProgressState` with `direction: 'upload'`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadProgress {
    pub name: String,
    pub loaded: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub bytes_per_second: f64,
    pub phase: TransferPhase,
    pub file_index: usize,
    pub file_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UploadResult {
    pub uploaded: usize,
    pub directories: usize,
}

/// The question asked before replacing a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmRequest {
    pub title: String,
    pub description: String,
    pub confirm_label: String,
    pub destructive: bool,
}

/// Why an upload stopped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UploadError {
    /// The user cancelled (`AbortError`).
    #[error("Upload cancelled")]
    Cancelled,
    #[error(transparent)]
    Failed(#[from] Problem),
}

/// What an upload needs from the outside: the remote side and the user.
pub trait UploadDeps: Send + Sync {
    /// Creates `path` and its missing parents.
    fn mkdir<'a>(
        &'a self,
        conn_id: &'a str,
        path: &'a str,
    ) -> BoxFuture<'a, Result<(), UploadError>>;

    /// Uploads one file. Without `overwrite`, an existing destination fails with a 409
    /// `SFTP_DESTINATION_EXISTS` problem.
    fn upload<'a>(
        &'a self,
        conn_id: &'a str,
        path: &'a str,
        overwrite: bool,
        file: &'a DroppedFile,
        cancel: &'a CancelSignal,
        on_step: &'a mut (dyn FnMut(UploadStep) + Send),
    ) -> BoxFuture<'a, Result<(), UploadError>>;

    /// Asks the user; `true` replaces the file.
    fn confirm(&self, request: ConfirmRequest) -> BoxFuture<'_, bool>;
}

/// `dir/name` without doubling the root's slash.
pub fn join_remote_path(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// A relative path that cannot climb out of the upload directory.
pub fn clean_relative_path(value: &str) -> String {
    value
        .replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
        .collect::<Vec<_>>()
        .join("/")
}

/// Reads the local files and folders of a drop: folders are walked (empty ones kept), files keep
/// their path below the dropped item. Symbolic links are followed, each folder once.
pub fn collect_drop(paths: &[PathBuf]) -> std::io::Result<DropPayload> {
    let mut payload = DropPayload::default();
    let mut seen = HashSet::new();
    for path in paths {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        visit(path, &name, &mut payload, &mut seen)?;
    }
    Ok(payload)
}

fn visit(
    path: &Path,
    relative: &str,
    payload: &mut DropPayload,
    seen: &mut HashSet<PathBuf>,
) -> std::io::Result<()> {
    let relative_path = clean_relative_path(relative);
    let meta = std::fs::metadata(path)?;
    if meta.is_file() {
        payload.files.push(DroppedFile {
            source: path.to_path_buf(),
            relative_path,
            size: meta.len(),
        });
        return Ok(());
    }
    if !meta.is_dir() || !seen.insert(std::fs::canonicalize(path)?) {
        return Ok(());
    }
    payload.directories.push(relative_path.clone());
    let mut children: Vec<_> = std::fs::read_dir(path)?.collect::<Result<_, _>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let name = child.file_name().to_string_lossy().into_owned();
        visit(
            &child.path(),
            &format!("{relative_path}/{name}"),
            payload,
            seen,
        )?;
    }
    Ok(())
}

fn is_destination_exists(error: &UploadError) -> bool {
    matches!(error, UploadError::Failed(problem)
        if problem.status == 409 && problem.code.as_deref() == Some(SFTP_DESTINATION_EXISTS))
}

fn check(cancel: &CancelSignal) -> Result<(), UploadError> {
    if cancel.is_cancelled() {
        Err(UploadError::Cancelled)
    } else {
        Ok(())
    }
}

/// Uploads a drop into `directory` on the connection: folders first, shallowest first, then each
/// file. A file that already exists is replaced only once the user confirms; a declined one is
/// skipped. Progress ends in a `complete` state.
pub async fn upload_to_remote(
    conn_id: &str,
    directory: &str,
    payload: &DropPayload,
    cancel: &CancelSignal,
    on_progress: &mut (dyn FnMut(&UploadProgress) + Send),
    deps: &dyn UploadDeps,
) -> Result<UploadResult, UploadError> {
    let file_count = payload.files.len();
    let parent_directories = payload.files.iter().filter_map(|file| {
        let cleaned = clean_relative_path(&file.relative_path);
        let parts: Vec<&str> = cleaned.split('/').collect();
        let parent = parts[..parts.len() - 1].join("/");
        (!parent.is_empty()).then_some(parent)
    });
    let mut unique = HashSet::new();
    let mut directories: Vec<String> = payload
        .directories
        .iter()
        .cloned()
        .chain(parent_directories)
        .filter(|dir| !dir.is_empty() && unique.insert(dir.clone()))
        .collect();
    directories.sort_by_key(|dir| dir.split('/').count());
    let total_bytes: u64 = payload.files.iter().map(|file| file.size).sum();
    let total_or_none = (total_bytes > 0).then_some(total_bytes);
    let mut completed_bytes = 0u64;
    let mut uploaded = 0usize;
    let mut file_index = 0usize;
    let mut last = UploadProgress {
        name: directories
            .first()
            .cloned()
            .or_else(|| payload.files.first().map(|file| file.relative_path.clone()))
            .unwrap_or_else(|| "Preparing upload".into()),
        loaded: 0,
        total: total_or_none,
        bytes_per_second: 0.0,
        phase: TransferPhase::Preparing,
        file_index: 0,
        file_count,
    };
    on_progress(&last);

    for relative in &directories {
        check(cancel)?;
        last.name = relative.clone();
        last.loaded = completed_bytes;
        last.bytes_per_second = 0.0;
        last.phase = TransferPhase::Preparing;
        on_progress(&last);
        deps.mkdir(conn_id, &join_remote_path(directory, relative))
            .await?;
    }
    for item in &payload.files {
        // A cancel while the replace question was open must stop the next request too.
        check(cancel)?;
        file_index += 1;
        let fallback = item.file_name();
        let relative = clean_relative_path(if item.relative_path.is_empty() {
            &fallback
        } else {
            &item.relative_path
        });
        let send = FileSend {
            deps,
            conn_id,
            destination: join_remote_path(directory, &relative),
            relative: &relative,
            item,
            cancel,
            completed_bytes,
            total_bytes,
            file_index,
        };
        match send.run(false, &mut last, on_progress).await {
            Ok(()) => uploaded += 1,
            Err(error) if !is_destination_exists(&error) => return Err(error),
            Err(_) => {
                let replace = deps
                    .confirm(ConfirmRequest {
                        title: "Replace the existing file?".into(),
                        description: format!("{relative} already exists on the remote host."),
                        confirm_label: "Replace".into(),
                        destructive: true,
                    })
                    .await;
                if replace {
                    check(cancel)?;
                    send.run(true, &mut last, on_progress).await?;
                    uploaded += 1;
                }
            }
        }
        completed_bytes += item.size;
    }

    last.name = payload
        .files
        .last()
        .map(|file| file.relative_path.clone())
        .or_else(|| directories.last().cloned())
        .unwrap_or_else(|| "Upload".into());
    last.loaded = total_bytes;
    last.total = total_or_none;
    last.bytes_per_second = 0.0;
    last.phase = TransferPhase::Complete;
    last.file_index = file_count;
    on_progress(&last);
    Ok(UploadResult {
        uploaded,
        directories: directories.len(),
    })
}

/// One file's upload request, reporting into the drop's progress.
struct FileSend<'a> {
    deps: &'a dyn UploadDeps,
    conn_id: &'a str,
    destination: String,
    relative: &'a str,
    item: &'a DroppedFile,
    cancel: &'a CancelSignal,
    completed_bytes: u64,
    total_bytes: u64,
    file_index: usize,
}

impl FileSend<'_> {
    async fn run(
        &self,
        overwrite: bool,
        last: &mut UploadProgress,
        on_progress: &mut (dyn FnMut(&UploadProgress) + Send),
    ) -> Result<(), UploadError> {
        let mut on_step = |step: UploadStep| {
            match step {
                UploadStep::Progress(ByteProgress {
                    loaded,
                    bytes_per_second,
                    ..
                }) => {
                    last.name = self.relative.to_string();
                    last.loaded = self.completed_bytes + loaded;
                    last.total = Some(self.total_bytes);
                    last.bytes_per_second = bytes_per_second;
                    last.phase = TransferPhase::Transferring;
                    last.file_index = self.file_index;
                }
                UploadStep::Finalizing => {
                    last.loaded = self.completed_bytes + self.item.size;
                    last.phase = TransferPhase::Finalizing;
                }
            }
            on_progress(last);
        };
        self.deps
            .upload(
                self.conn_id,
                &self.destination,
                overwrite,
                self.item,
                self.cancel,
                &mut on_step,
            )
            .await
    }
}

/// The toast after an upload, e.g. "Uploaded 3 files" or "Uploaded 1 file to /srv/app".
pub fn upload_summary(result: UploadResult, directory: Option<&str>) -> String {
    let place =
        |preposition: &str| directory.map_or(String::new(), |dir| format!(" {preposition} {dir}"));
    if result.uploaded > 0 {
        let files = if result.uploaded == 1 {
            "1 file".to_string()
        } else {
            format!("{} files", result.uploaded)
        };
        return format!("Uploaded {files}{}", place("to"));
    }
    if result.directories > 0 {
        let folders = if result.directories == 1 {
            "1 folder".to_string()
        } else {
            format!("{} folders", result.directories)
        };
        return format!("Created {folders}{}", place("in"));
    }
    "Nothing was uploaded".into()
}

/// [`UploadDeps`] over a live connection: folders through [`SftpService::mkdir`], files through
/// [`SftpService::upload`], and the replace question through `confirm`.
pub struct SftpUploadDeps<C> {
    pub service: SftpService,
    pub confirm: C,
}

impl<C> UploadDeps for SftpUploadDeps<C>
where
    C: Fn(ConfirmRequest) -> BoxFuture<'static, bool> + Send + Sync,
{
    fn mkdir<'a>(
        &'a self,
        conn_id: &'a str,
        path: &'a str,
    ) -> BoxFuture<'a, Result<(), UploadError>> {
        Box::pin(async move {
            self.service.mkdir(conn_id, path, true).await?;
            Ok(())
        })
    }

    fn upload<'a>(
        &'a self,
        conn_id: &'a str,
        path: &'a str,
        overwrite: bool,
        file: &'a DroppedFile,
        cancel: &'a CancelSignal,
        on_step: &'a mut (dyn FnMut(UploadStep) + Send),
    ) -> BoxFuture<'a, Result<(), UploadError>> {
        Box::pin(async move {
            let body = tokio::fs::File::open(&file.source)
                .await
                .map_err(Problem::from)?;
            let options = UploadOptions {
                overwrite,
                expected_mtime_ms: None,
                size: Some(file.size),
            };
            // Dropping the upload on cancel removes its staged temporary file.
            tokio::select! {
                result = self.service.upload(conn_id, path, &options, body, on_step) => {
                    result.map(|_| ()).map_err(UploadError::from)
                }
                () = cancel.cancelled() => Err(UploadError::Cancelled),
            }
        })
    }

    fn confirm(&self, request: ConfirmRequest) -> BoxFuture<'_, bool> {
        (self.confirm)(request)
    }
}
