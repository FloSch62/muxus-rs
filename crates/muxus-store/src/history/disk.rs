//! Disk usage of the history root and free space on its file system.

use std::io;
use std::path::Path;

/// What the quota checks measure (`diskStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiskStatus {
    /// Bytes allocated under the history root, SQLite WAL/SHM files included.
    pub usage_bytes: u64,
    /// Bytes available to an unprivileged user on the root's file system.
    pub free_bytes: u64,
    /// Size of the root's file system.
    pub total_bytes: u64,
}

pub(crate) fn disk_status(root: &Path) -> io::Result<DiskStatus> {
    let (free_bytes, total_bytes) = file_system_space(root)?;
    Ok(DiskStatus {
        usage_bytes: directory_size(root)?,
        free_bytes,
        total_bytes,
    })
}

#[cfg(unix)]
fn file_system_space(root: &Path) -> io::Result<(u64, u64)> {
    let stats = rustix::fs::statvfs(root)?;
    // f_blocks and f_bavail count f_frsize units (equal to f_bsize, which the
    // TypeScript multiplies by, on every common file system).
    let unit = if stats.f_frsize > 0 {
        stats.f_frsize
    } else {
        stats.f_bsize
    };
    Ok((
        stats.f_bavail.saturating_mul(unit),
        stats.f_blocks.saturating_mul(unit),
    ))
}

#[cfg(windows)]
fn file_system_space(root: &Path) -> io::Result<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = root
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available = 0u64;
    let mut total = 0u64;
    let mut free = 0u64;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path and the out pointers are valid u64s.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((available, total))
}

#[cfg(not(any(unix, windows)))]
fn file_system_space(_root: &Path) -> io::Result<(u64, u64)> {
    Ok((u64::MAX, u64::MAX))
}

/// Bytes allocated by regular files under `directory` (`directorySize`).
/// POSIX `st_blocks` measures allocated 512-byte units, sparse files
/// included; other platforms fall back to the logical size. Symbolic links
/// are neither followed nor counted.
pub(crate) fn directory_size(directory: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            total += directory_size(&entry.path())?;
        } else if kind.is_file() {
            let metadata = entry.metadata()?;
            total += allocated_bytes(&metadata);
        }
    }
    Ok(total)
}

#[cfg(unix)]
fn allocated_bytes(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks() * 512
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &std::fs::Metadata) -> u64 {
    metadata.len()
}
