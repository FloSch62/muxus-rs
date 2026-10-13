//! Text editing for files a local terminal can see, and pasted clipboard images
//! (`server/src/routes/local-files.ts`).

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use muxus_core::api::{EditorFileResponse, EditorFileSaveRequest, EditorFileSaveResponse};

use crate::backend::Backend;
use crate::error::{ApiError, ApiResult};

const MAX_EDITOR_BYTES: u64 = 8 * 1024 * 1024;
const CLIPBOARD_IMAGE_MAX_BYTES: usize = 18 * 1024 * 1024;

fn format_bytes(bytes: u64) -> String {
    format!("{} MiB", (bytes as f64 / (1024.0 * 1024.0)).round())
}

fn too_large_to_open() -> ApiError {
    ApiError::new(
        413,
        format!(
            "files larger than {} cannot be opened in the editor",
            format_bytes(MAX_EDITOR_BYTES)
        ),
    )
}

fn symlink_problem() -> ApiError {
    ApiError::with_code(
        409,
        "open the symbolic link target explicitly",
        "LOCAL_EDITOR_SYMLINK",
    )
}

/// `localFileError`: OS errors as the editor's problems.
fn local_file_error(error: io::Error) -> ApiError {
    match error.kind() {
        io::ErrorKind::NotFound => ApiError::not_found("local file not found"),
        io::ErrorKind::PermissionDenied => {
            ApiError::new(403, "permission denied while accessing the local file")
        }
        _ if error.raw_os_error() == Some(libc_eloop()) => symlink_problem(),
        _ => ApiError::internal(error.to_string()),
    }
}

#[cfg(unix)]
fn libc_eloop() -> i32 {
    libc::ELOOP
}

#[cfg(not(unix))]
fn libc_eloop() -> i32 {
    -1
}

/// The query's path: required, absolute, normalized (`path.normalize`).
fn require_absolute_path(value: &str) -> ApiResult<PathBuf> {
    if value.is_empty() || value.contains('\0') {
        return Err(ApiError::bad_request("path is required"));
    }
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(ApiError::bad_request("path must be absolute"));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

fn require_regular_file(metadata: &Metadata) -> ApiResult<()> {
    let kind = metadata.file_type();
    if kind.is_dir() {
        return Err(ApiError::bad_request("cannot edit a directory"));
    }
    if kind.is_symlink() {
        return Err(symlink_problem());
    }
    if !kind.is_file() {
        return Err(ApiError::new(
            415,
            "only regular files can be opened in the editor",
        ));
    }
    Ok(())
}

fn mtime_ms(metadata: &Metadata) -> Option<i64> {
    let modified = metadata.modified().ok()?;
    let since = modified.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

#[cfg(unix)]
fn mode_bits(metadata: &Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_bits(_metadata: &Metadata) -> Option<u32> {
    None
}

fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Read a UTF-8 text file for the editor (`readLocalTextFile`).
pub fn read_local_text_file(file: &Path) -> ApiResult<EditorFileResponse> {
    let initial = fs::symlink_metadata(file).map_err(local_file_error)?;
    require_regular_file(&initial)?;
    if initial.len() > MAX_EDITOR_BYTES {
        return Err(too_large_to_open());
    }
    let mut handle = open_no_follow(file).map_err(local_file_error)?;
    let opened = handle.metadata().map_err(local_file_error)?;
    require_regular_file(&opened)?;
    if opened.len() > MAX_EDITOR_BYTES {
        return Err(too_large_to_open());
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    (&mut handle)
        .take(opened.len())
        .read_to_end(&mut bytes)
        .map_err(local_file_error)?;
    let after = handle.metadata().map_err(local_file_error)?;
    if bytes.len() as u64 != opened.len()
        || after.len() != opened.len()
        || mtime_ms(&after) != mtime_ms(&opened)
    {
        return Err(ApiError::new(
            409,
            "the local file changed while it was being opened",
        ));
    }
    if bytes.contains(&0) {
        return Err(ApiError::new(
            415,
            "this appears to be a binary file and cannot be opened as text",
        ));
    }
    let content = String::from_utf8(bytes)
        .map_err(|_| ApiError::new(415, "the file editor currently supports UTF-8 text files"))?;
    // TextDecoder drops a leading byte order mark.
    let content = content
        .strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(content);
    Ok(EditorFileResponse {
        path: file.to_string_lossy().into_owned(),
        size: opened.len(),
        content,
        mtime_ms: mtime_ms(&opened),
        mode: mode_bits(&opened),
    })
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    muxus_store::ids::random_bytes(&mut buffer);
    buffer.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Write through a temporary file and rename it over the original, keeping its mode.
fn atomic_local_text_save(file: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let mut name = file.as_os_str().to_owned();
    name.push(format!(".muxus-{}.tmp", random_hex(6)));
    let temporary = PathBuf::from(name);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        let mut handle = options.open(&temporary)?;
        handle.write_all(bytes)?;
        // open() applies the process umask even when a mode is supplied. Restore the original
        // file's permissions before the temporary file is published.
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            handle.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        handle.sync_all()?;
        drop(handle);
        fs::rename(&temporary, file)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Save editor text atomically (`PUT /api/local-files/file`).
pub fn save_local_text_file(
    file: &Path,
    request: &EditorFileSaveRequest,
) -> ApiResult<EditorFileSaveResponse> {
    let bytes = request.content.as_bytes();
    if bytes.len() as u64 > MAX_EDITOR_BYTES {
        return Err(ApiError::new(
            413,
            format!(
                "files larger than {} cannot be saved from the editor",
                format_bytes(MAX_EDITOR_BYTES)
            ),
        ));
    }
    let current = fs::symlink_metadata(file).map_err(local_file_error)?;
    require_regular_file(&current)?;
    if request.force != Some(true)
        && let Some(expected) = request.expected_mtime_ms
        && mtime_ms(&current) != Some(expected)
    {
        return Err(ApiError::with_code(
            409,
            "the local file changed since it was opened",
            "LOCAL_FILE_CHANGED",
        ));
    }
    atomic_local_text_save(file, bytes, mode_bits(&current)).map_err(local_file_error)?;
    let saved = fs::metadata(file).map_err(local_file_error)?;
    Ok(EditorFileSaveResponse {
        ok: true,
        size: saved.len(),
        mtime_ms: mtime_ms(&saved),
    })
}

/// Keep a pasted clipboard image where local shells can read it, owner-only.
pub fn write_clipboard_image(directory: &Path, image: &[u8]) -> ApiResult<PathBuf> {
    if image.len() > CLIPBOARD_IMAGE_MAX_BYTES {
        return Err(ApiError::new(
            413,
            format!(
                "clipboard images larger than {} cannot be pasted",
                format_bytes(CLIPBOARD_IMAGE_MAX_BYTES as u64)
            ),
        ));
    }
    let file = directory.join(format!(
        "muxus-paste-{}-{}.png",
        chrono::Utc::now().timestamp_millis(),
        random_hex(16)
    ));
    // create_new refuses an existing name, so a planted file or symlink is never reused.
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut handle = options.open(&file).map_err(local_file_error)?;
    if let Err(error) = handle.write_all(image) {
        drop(handle);
        let _ = fs::remove_file(&file);
        return Err(local_file_error(error));
    }
    Ok(file)
}

impl Backend {
    /// `GET /api/local-files/file`.
    pub async fn read_local_file(&self, path: &str) -> ApiResult<EditorFileResponse> {
        let file = require_absolute_path(path)?;
        self.blocking(move |_| read_local_text_file(&file)).await
    }

    /// `PUT /api/local-files/file`.
    pub async fn save_local_file(
        &self,
        path: &str,
        request: EditorFileSaveRequest,
    ) -> ApiResult<EditorFileSaveResponse> {
        let file = require_absolute_path(path)?;
        self.blocking(move |_| save_local_text_file(&file, &request))
            .await
    }

    /// `POST /api/local-files/clipboard-image`: returns the new file's path.
    pub async fn local_clipboard_image(&self, image: Vec<u8>) -> ApiResult<String> {
        self.blocking(move |_| {
            write_clipboard_image(&std::env::temp_dir(), &image)
                .map(|file| file.to_string_lossy().into_owned())
        })
        .await
    }
}
