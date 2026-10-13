//! Remote and local file browsing and editing.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SftpEntryType {
    File,
    Dir,
    Link,
    Other,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: SftpEntryType,
    /// Bytes; absent for directories where the server did not report a size.
    pub size: Option<u64>,
    /// Unix mtime in milliseconds.
    pub mtime_ms: Option<i64>,
    /// Permission bits (the low 12 bits of st_mode).
    pub mode: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
    /// Short-lived, single-file capability for native drag-out downloads.
    pub download_ticket: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SftpListResponse {
    pub path: String,
    pub entries: Vec<SftpEntry>,
}

/// UTF-8 text file opened in the editor, regardless of backing filesystem.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorFileResponse {
    pub path: String,
    pub content: String,
    pub size: u64,
    pub mtime_ms: Option<i64>,
    pub mode: Option<u32>,
}

/// Optimistic, text-only save request shared by local and SFTP files.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorFileSaveRequest {
    pub content: String,
    /// Modification time returned by the read/save that produced this buffer.
    pub expected_mtime_ms: Option<i64>,
    /// Explicit conflict recovery chosen by the user.
    pub force: Option<bool>,
}

/// `{ ok: true, size, mtimeMs? }`.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorFileSaveResponse {
    /// Always true.
    pub ok: bool,
    pub size: u64,
    pub mtime_ms: Option<i64>,
}

pub type SftpFileResponse = EditorFileResponse;
pub type SftpFileSaveRequest = EditorFileSaveRequest;
pub type SftpFileSaveResponse = EditorFileSaveResponse;

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpUploadResponse {
    /// Always true.
    pub ok: bool,
    /// The uploaded file's modification time, when the server reported it.
    pub mtime_ms: Option<i64>,
}
