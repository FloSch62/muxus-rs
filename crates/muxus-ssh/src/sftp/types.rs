//! Request and response shapes of the SFTP operations.
//!
//! The DTOs of `shared/src/api-types.ts` (`SftpEntry`, `SftpListResponse`, the editor file
//! types, `SftpUploadResponse`) already live in `muxus_core::api` and are re-exported here; the
//! route bodies the TypeScript server built inline are defined below.

pub use muxus_core::api::{
    EditorFileResponse, EditorFileSaveRequest, EditorFileSaveResponse, SftpEntry, SftpEntryType,
    SftpFileResponse, SftpFileSaveRequest, SftpFileSaveResponse, SftpListResponse,
    SftpUploadResponse,
};
use serde::{Deserialize, Serialize};

/// `GET /api/sftp/:connId/home`: `{ path }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SftpHomeResponse {
    pub path: String,
}

/// `{ ok: true }` from mkdir, rename and delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SftpOkResponse {
    /// Always true.
    pub ok: bool,
}

impl SftpOkResponse {
    pub const OK: Self = Self { ok: true };
}

/// `POST /api/sftp/:connId/clipboard-image`: where the pasted image landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SftpClipboardImageResponse {
    pub path: String,
}
