//! SFTP file operations for the file browser and remote editor, with leases shared over one
//! connection.
//!
//! Port of `server/src/routes/sftp.ts` ([`SftpService`]), `server/src/ws/sftp-lease-socket.ts`
//! ([`SftpWindowLease`]) and the upload orchestration of `client/src/sftp-upload.ts`
//! ([`upload::upload_to_remote`]). Every operation runs on the connection's single SFTP session
//! (`ManagedConnection::sftp`) while holding an SFTP lease, so a closed terminal tab never tears
//! the transport out from under a running file operation.

mod fs;
pub mod lease;
pub mod paths;
pub mod progress;
pub mod service;
pub mod types;
pub mod upload;

pub use fs::SftpError;
pub use lease::SftpWindowLease;
pub use progress::{ByteProgress, CancelSignal, ProgressMeter, UploadStep};
pub use service::{SftpDownload, SftpService, UploadOptions};
pub use types::*;

#[cfg(test)]
mod tests;
