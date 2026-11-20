//! The "SFTP file modes" suite of tests/unit/server/sftp-upload.test.ts: a new file is created
//! 0644 less the server's umask, and a file that replaces another keeps that file's mode. Modes
//! are POSIX, so these run on Unix only.

use super::*;
use crate::sftp::service::UploadOptions;
use crate::sftp::types::SftpFileSaveRequest;

const FILE: &str = "/remote/image.bin";

async fn upload(fx: &Fixture, options: &UploadOptions, body: &[u8]) {
    fx.service
        .upload(&fx.conn_id, FILE, options, body, &mut |_| {})
        .await
        .unwrap();
}

pub(super) fn mode_of(fx: &Fixture, remote: &str) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(fx.local(remote))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

/// The process umask, which the test server creates files under.
// `mode_t` is 16 bits wide on macOS.
#[allow(clippy::useless_conversion)]
pub(super) fn umask() -> u32 {
    // Linux says it without changing it, which other tests creating files would notice.
    if let Ok(status) = std::fs::read_to_string("/proc/self/status")
        && let Some(mask) = status
            .lines()
            .find_map(|line| line.strip_prefix("Umask:"))
            .and_then(|mask| u32::from_str_radix(mask.trim(), 8).ok())
    {
        return mask;
    }
    // SAFETY: `umask` only swaps a per-process number; it is put back at once.
    let mask = unsafe { libc::umask(0o022) };
    // SAFETY: as above.
    unsafe { libc::umask(mask) };
    u32::from(mask)
}

#[tokio::test]
async fn opens_a_new_file_with_0644_less_the_umask_and_does_not_chmod_it() {
    let fx = Fixture::new().await;
    fx.mkdir("/remote");

    upload(&fx, &UploadOptions::default(), b"payload").await;

    let opens = fx.ops_starting("open ");
    assert_eq!(opens.len(), 1);
    assert!(opens[0].ends_with(" wctx 644"), "{}", opens[0]);
    assert_eq!(mode_of(&fx, FILE), 0o644 & !umask());
}

#[tokio::test]
async fn gives_a_replaced_file_the_mode_it_had() {
    let fx = Fixture::new().await;
    let existing = fx.write(FILE, "old");
    set_mode(&existing, 0o750);

    let replace = UploadOptions {
        overwrite: true,
        ..UploadOptions::default()
    };
    upload(&fx, &replace, b"payload").await;

    assert!(fx.ops_starting("open ")[0].ends_with(" wctx 750"));
    assert_eq!(mode_of(&fx, FILE), 0o750);
}

#[tokio::test]
async fn saves_from_the_editor_through_a_file_with_the_mode_of_the_one_it_replaces() {
    let fx = Fixture::new().await;
    let motd = fx.write("/etc/motd", "old");
    set_mode(&motd, 0o640);

    fx.service
        .save_file(
            &fx.conn_id,
            "/etc/motd",
            &SftpFileSaveRequest {
                content: "hello".into(),
                expected_mtime_ms: None,
                force: None,
            },
        )
        .await
        .unwrap();

    let opens = fx.ops_starting("open ");
    assert!(opens[0].ends_with(".tmp wctx 640"), "{}", opens[0]);
    assert_eq!(mode_of(&fx, "/etc/motd"), 0o640);
}
