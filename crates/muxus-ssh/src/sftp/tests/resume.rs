//! Port of the "SFTP upload resume", "SFTP download resume" and "SFTP file modes" suites of
//! tests/unit/server/sftp-upload.test.ts: uploads staged under a name made from the local
//! file, continued after a lost connection; downloads continued at a byte; and the modes new
//! files are created with.

use std::pin::Pin;
use std::task::{Context, Poll};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, ReadBuf};

#[cfg(unix)]
use super::modes::{mode_of, umask};
use super::*;
use crate::Problem;
use crate::sftp::service::{DownloadRange, UploadOptions, UploadSource};

const MIB: usize = 1024 * 1024;
const LAST_MODIFIED: i64 = 1_760_000_000_000;
const FINGERPRINT: [u8; 32] = [0xab; 32];
const FILE: &str = "/remote/image.bin";

/// Bytes that never repeat within the 64 KiB the server compares.
fn source_bytes() -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..MIB + 300)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u8
        })
        .collect()
}

fn describing(bytes: &[u8]) -> UploadSource {
    UploadSource {
        size: bytes.len() as u64,
        last_modified: LAST_MODIFIED,
        fingerprint: FINGERPRINT,
    }
}

fn options(source: UploadSource) -> UploadOptions {
    UploadOptions {
        source: Some(source),
        ..UploadOptions::default()
    }
}

/// The name the server stages an upload of `source` to `file` under.
pub(super) fn staged_name(file: &str, source: &UploadSource) -> String {
    let described = format!(
        "{}:{}:{}",
        source.size,
        source.last_modified,
        hex::encode(source.fingerprint)
    );
    let key = hex::encode(Sha256::digest(described.as_bytes()));
    format!("{file}.muxus-{}.upload", &key[..12])
}

async fn upload(
    fx: &Fixture,
    options: &UploadOptions,
    body: impl AsyncRead + Unpin + Send,
) -> Result<crate::sftp::SftpUploadResponse, Problem> {
    fx.service
        .upload(&fx.conn_id, FILE, options, body, &mut |_| {})
        .await
}

fn write_bytes(fx: &Fixture, remote: &str, bytes: &[u8]) {
    let path = fx.local(remote);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn read_bytes(fx: &Fixture, remote: &str) -> Vec<u8> {
    std::fs::read(fx.local(remote)).unwrap()
}

/// What was written into `path`: where the writes start and how many bytes they are.
fn written(fx: &Fixture, path: &str) -> Option<(u64, usize)> {
    let writes: Vec<_> = fx
        .sftp
        .writes()
        .into_iter()
        .filter(|(written, _, _)| written == path)
        .collect();
    let start = writes.iter().map(|(_, offset, _)| *offset).min()?;
    Some((start, writes.iter().map(|(_, _, len)| len).sum()))
}

/// Opens of `path` that did not create it: the staged file of an earlier attempt.
fn reopened(fx: &Fixture, path: &str) -> usize {
    fx.ops_starting(&format!("open {path} rw ")).len()
}

fn mismatch() -> Problem {
    Problem::with_code(
        409,
        "the upload kept on the server does not match this file",
        "SFTP_UPLOAD_MISMATCH",
    )
}

fn names_in(fx: &Fixture, remote_dir: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fx.local(remote_dir))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A body that fails after `limit` bytes, as an upload aborted half way.
struct Failing<'a> {
    bytes: &'a [u8],
    position: usize,
    limit: usize,
}

impl AsyncRead for Failing<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.position >= self.limit {
            return Poll::Ready(Err(std::io::Error::other("aborted by the client")));
        }
        let end = (self.position + buf.remaining()).min(self.limit);
        buf.put_slice(&self.bytes[self.position..end]);
        self.position = end;
        Poll::Ready(Ok(()))
    }
}

/// A body that waits after its first megabyte until it is released.
struct Held {
    bytes: Vec<u8>,
    position: usize,
    release: tokio::sync::watch::Receiver<bool>,
    waiting: Option<Pin<Box<dyn std::future::Future<Output = ()> + Send>>>,
}

impl AsyncRead for Held {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.position >= MIB && !*self.release.borrow() {
            if self.waiting.is_none() {
                let mut release = self.release.clone();
                self.waiting = Some(Box::pin(async move {
                    let _ = release.wait_for(|released| *released).await;
                }));
            }
            let waiting = self.waiting.as_mut().expect("just set");
            if waiting.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
        }
        let limit = if *self.release.borrow() {
            self.bytes.len()
        } else {
            MIB
        };
        let end = (self.position + buf.remaining()).min(limit);
        let (from, bytes) = (self.position, self.bytes.clone());
        buf.put_slice(&bytes[from..end]);
        self.position = end;
        Poll::Ready(Ok(()))
    }
}

// --- SFTP upload resume -----------------------------------------------------------------------

#[tokio::test]
async fn stages_an_upload_under_a_name_made_of_the_local_files_size_time_and_fingerprint() {
    let fx = Fixture::new().await;
    fx.mkdir("/remote");
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);

    assert!(
        upload(&fx, &options(described), &source[..])
            .await
            .unwrap()
            .ok
    );
    let later = UploadSource {
        last_modified: LAST_MODIFIED + 1,
        ..described
    };
    let other = UploadSource {
        fingerprint: [0xcd; 32],
        ..described
    };
    for source_of in [later, other] {
        let replace = UploadOptions {
            overwrite: true,
            ..options(source_of)
        };
        upload(&fx, &replace, &source[..]).await.unwrap();
    }

    let names: Vec<String> = fx
        .ops_starting("open ")
        .iter()
        .map(|line| line.split(' ').nth(1).unwrap().to_string())
        .collect();
    assert_eq!(names[0], staged);
    assert_eq!(
        names.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    for name in &names {
        assert!(is_staged(name, FILE, "upload"), "{name}");
    }
    assert_eq!(read_bytes(&fx, FILE), source);
    assert_eq!(names_in(&fx, "/remote"), vec!["image.bin"]);
}

#[tokio::test]
async fn continues_a_staged_file_whose_last_bytes_match_the_upload_sending_only_the_rest() {
    let fx = Fixture::new().await;
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    write_bytes(&fx, &staged, &source[..700_000]);
    let mut progress = Vec::new();

    let response = fx
        .service
        .upload(
            &fx.conn_id,
            FILE,
            &UploadOptions {
                size: Some(source.len() as u64),
                ..options(described)
            },
            &source[..],
            &mut |step| {
                if let crate::sftp::UploadStep::Progress(progress_now) = step {
                    progress.push(progress_now.loaded);
                }
            },
        )
        .await
        .unwrap();

    assert!(response.ok);
    assert_eq!(
        written(&fx, &staged),
        Some((700_000, source.len() - 700_000))
    );
    assert_eq!(read_bytes(&fx, FILE), source);
    assert!(!fx.exists(&staged));
    // The bar starts where the earlier attempt stopped.
    assert_eq!(progress.first(), Some(&700_000));
    assert_eq!(progress.last(), Some(&(source.len() as u64)));
    assert!(progress.windows(2).all(|pair| pair[0] <= pair[1]));
}

#[tokio::test]
async fn continues_a_staged_file_that_already_holds_every_byte() {
    let fx = Fixture::new().await;
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    write_bytes(&fx, &staged, &source);

    assert!(
        upload(&fx, &options(described), &source[..])
            .await
            .unwrap()
            .ok
    );

    assert_eq!(written(&fx, &staged), None);
    assert_eq!(read_bytes(&fx, FILE), source);
    assert!(!fx.exists(&staged));
}

#[tokio::test]
async fn writes_nothing_into_a_staged_file_that_differs_and_has_the_file_sent_again() {
    let fx = Fixture::new().await;
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    let mut other = source[..600_000].to_vec();
    other.extend(std::iter::repeat_n(0x5a, 100_000));
    write_bytes(&fx, &staged, &other);

    let err = upload(&fx, &options(described), &source[..])
        .await
        .unwrap_err();

    assert_eq!(err, mismatch());
    assert!(fx.sftp.writes().is_empty());
    assert!(names_in(&fx, "/remote").is_empty());
}

#[tokio::test]
async fn refuses_a_body_that_ends_before_the_bytes_the_server_kept() {
    let fx = Fixture::new().await;
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    write_bytes(&fx, &staged, &source[..700_000]);

    let err = upload(&fx, &options(described), &source[..500_000])
        .await
        .unwrap_err();

    assert_eq!(err, mismatch());
    assert!(fx.sftp.writes().is_empty());
    assert!(names_in(&fx, "/remote").is_empty());
}

#[tokio::test]
async fn refuses_a_continued_upload_that_does_not_end_at_the_described_size() {
    let fx = Fixture::new().await;
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    write_bytes(&fx, &staged, &source[..700_000]);
    let mut longer = source.clone();
    longer.extend_from_slice(b"more than was described");

    let err = upload(&fx, &options(described), &longer[..])
        .await
        .unwrap_err();

    assert_eq!(err, mismatch());
    assert!(names_in(&fx, "/remote").is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn never_opens_what_holds_the_staged_name_unless_it_is_a_regular_file() {
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);

    let fx = Fixture::new().await;
    fx.mkdir("/remote");
    let fifo = std::ffi::CString::new(fx.local(&staged).to_string_lossy().as_bytes()).unwrap();
    // SAFETY: `fifo` is a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);

    assert!(
        upload(&fx, &options(described), &source[..])
            .await
            .unwrap()
            .ok
    );
    // Only the staged file it creates itself is opened.
    assert_eq!(reopened(&fx, &staged), 0);
    assert_eq!(fx.ops_starting(&format!("open {staged} ")).len(), 1);
    assert_eq!(read_bytes(&fx, FILE), source);

    let linked = Fixture::new().await;
    let keys = "ssh-ed25519 AAAA admin@laptop\n";
    let authorized = linked.write("/home/admin/.ssh/authorized_keys", keys);
    linked.mkdir("/remote");
    std::os::unix::fs::symlink(&authorized, linked.local(&staged)).unwrap();

    assert!(
        upload(&linked, &options(described), &source[..])
            .await
            .unwrap()
            .ok
    );
    assert_eq!(reopened(&linked, &staged), 0);
    assert_eq!(linked.read("/home/admin/.ssh/authorized_keys"), keys);
    assert_eq!(read_bytes(&linked, FILE), source);
}

#[cfg(unix)]
#[tokio::test]
async fn never_writes_through_a_staged_name_swapped_for_a_link_just_before_it_is_opened() {
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    let fx = Fixture::with(
        SftpServerOptions {
            link_before_open: Some((staged.clone(), "/home/admin/.ssh/authorized_keys".into())),
            ..SftpServerOptions::default()
        },
        |_| {},
    )
    .await;
    let keys = "ssh-ed25519 AAAA admin@laptop\n";
    fx.write("/home/admin/.ssh/authorized_keys", keys);
    write_bytes(&fx, &staged, &source[..700_000]);

    let err = upload(&fx, &options(described), &source[..])
        .await
        .unwrap_err();

    assert_eq!(err, mismatch());
    assert_eq!(fx.read("/home/admin/.ssh/authorized_keys"), keys);
    assert!(fx.sftp.writes().is_empty());
    assert!(!fx.exists(&staged));
    assert!(!fx.exists(FILE));
}

#[tokio::test]
async fn closes_the_staged_file_once_however_a_continued_upload_ends() {
    // A second close could close another file: OpenSSH hands a freed handle out again.
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    let mut other = source[..600_000].to_vec();
    other.extend(std::iter::repeat_n(0x5a, 100_000));
    let failing = SftpServerOptions {
        fail_writes: true,
        ..SftpServerOptions::default()
    };
    for (server, partial, cut) in [
        (SftpServerOptions::default(), &source[..700_000], None),
        (SftpServerOptions::default(), &other[..], None),
        (
            SftpServerOptions::default(),
            &source[..700_000],
            Some(800_000),
        ),
        (failing, &source[..700_000], None),
    ] {
        let fx = Fixture::with(server, |_| {}).await;
        write_bytes(&fx, &staged, partial);

        let _ = match cut {
            Some(limit) => {
                let body = Failing {
                    bytes: &source,
                    position: 0,
                    limit,
                };
                upload(&fx, &options(described), body).await
            }
            None => upload(&fx, &options(described), &source[..]).await,
        };

        assert_eq!(reopened(&fx, &staged), 1);
        let closes = fx.sftp.closes();
        assert_eq!(
            closes.iter().filter(|path| **path == staged).count(),
            1,
            "{closes:?}"
        );
    }
}

#[tokio::test]
async fn starts_over_from_a_staged_file_it_cannot_continue() {
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    // The server empties what it opens for writing.
    let truncating = SftpServerOptions {
        truncate_on_open: true,
        ..SftpServerOptions::default()
    };
    let longer = vec![0x33u8; source.len() + 1];
    for (server, partial) in [
        (truncating, &source[..700_000]),
        (SftpServerOptions::default(), &longer[..]),
        (SftpServerOptions::default(), &[][..]),
    ] {
        let fx = Fixture::with(server, |_| {}).await;
        write_bytes(&fx, &staged, partial);

        assert!(
            upload(&fx, &options(described), &source[..])
                .await
                .unwrap()
                .ok
        );

        assert_eq!(written(&fx, &staged), Some((0, source.len())));
        assert_eq!(read_bytes(&fx, FILE), source);
        assert_eq!(names_in(&fx, "/remote"), vec!["image.bin"]);
    }
}

#[tokio::test]
async fn uploads_under_a_random_name_when_what_holds_the_staged_name_cannot_be_removed() {
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    let fx = Fixture::with(
        SftpServerOptions {
            stuck: vec![staged.clone()],
            ..SftpServerOptions::default()
        },
        |_| {},
    )
    .await;
    write_bytes(&fx, &staged, b"another user's file");

    assert!(
        upload(&fx, &options(described), &source[..])
            .await
            .unwrap()
            .ok
    );

    let (path, _, _) = fx.sftp.writes().remove(0);
    assert!(is_staged(&path, FILE, "upload"), "{path}");
    assert_ne!(path, staged);
    assert_eq!(read_bytes(&fx, &staged), b"another user's file");
    assert_eq!(read_bytes(&fx, FILE), source);
}

#[tokio::test]
async fn sends_the_same_file_a_second_time_at_once_under_a_name_of_its_own() {
    let fx = Arc::new(Fixture::new().await);
    fx.mkdir("/remote");
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    let (release, released) = tokio::sync::watch::channel(false);
    let slow = Held {
        bytes: source.clone(),
        position: 0,
        release: released,
        waiting: None,
    };

    let first = tokio::spawn({
        let fx = fx.clone();
        async move { upload(&fx, &options(described), slow).await }
    });
    assert!(eventually(|| written(&fx, &staged).is_some()).await);
    let second = UploadOptions {
        overwrite: true,
        ..options(described)
    };
    assert!(upload(&fx, &second, &source[..]).await.unwrap().ok);
    release.send_replace(true);
    // The second upload put the file there first, and this one did not agree to replace it.
    let err = first.await.unwrap().unwrap_err();
    assert_eq!(err.code.as_deref(), Some("SFTP_DESTINATION_EXISTS"));

    let paths: Vec<String> = fx
        .ops_starting("open ")
        .iter()
        .map(|line| line.split(' ').nth(1).unwrap().to_string())
        .collect();
    assert_eq!(paths[0], staged);
    assert!(is_staged(&paths[1], FILE, "upload"), "{}", paths[1]);
    assert_ne!(paths[1], staged);
    assert_eq!(read_bytes(&fx, FILE), source);
    assert!(eventually(|| names_in(&fx, "/remote") == ["image.bin"]).await);

    // Once it is over, the name is free again.
    let third = UploadOptions {
        overwrite: true,
        ..options(described)
    };
    upload(&fx, &third, &source[..]).await.unwrap();
    assert_eq!(fx.ops_starting(&format!("open {staged} ")).len(), 2);
}

#[tokio::test]
async fn leaves_an_upload_that_does_not_describe_the_local_file_as_it_was() {
    let fx = Fixture::new().await;
    fx.mkdir("/remote");
    let source = source_bytes();
    let staged = staged_name(FILE, &describing(&source));

    assert!(
        upload(&fx, &UploadOptions::default(), &source[..])
            .await
            .unwrap()
            .ok
    );

    assert_eq!(
        fx.ops_starting("lstat "),
        vec![format!("lstat {FILE}"), format!("lstat {FILE}")]
    );
    assert!(fx.ops_starting(&format!("open {staged} ")).is_empty());
    assert_eq!(read_bytes(&fx, FILE), source);
}

#[test]
fn rejects_a_size_time_or_fingerprint_that_cannot_describe_a_file() {
    let fingerprint = "ab".repeat(32);
    let invalid =
        Problem::bad_request("size, lastModified and fingerprint must describe the local file");
    let parse = |size, last_modified, fingerprint| {
        UploadSource::from_query(size, last_modified, fingerprint)
    };

    assert_eq!(parse(None, None, None), Ok(None));
    assert_eq!(
        parse(Some("1048876"), Some("1760000000000"), Some(&fingerprint)),
        Ok(Some(UploadSource {
            size: 1_048_876,
            last_modified: LAST_MODIFIED,
            fingerprint: FINGERPRINT,
        }))
    );
    // A file last changed before 1970 has a negative time.
    assert!(
        parse(Some("0"), Some("-1"), Some(&fingerprint))
            .unwrap()
            .is_some()
    );
    for (size, last_modified, fingerprint) in [
        (
            Some("-1"),
            Some("1760000000000"),
            Some(fingerprint.as_str()),
        ),
        (Some("1048876"), Some("1760000000000"), None),
        (
            Some("1048876"),
            Some("yesterday"),
            Some(fingerprint.as_str()),
        ),
        (
            Some("1.5"),
            Some("1760000000000"),
            Some(fingerprint.as_str()),
        ),
        (
            Some("1048876"),
            Some("1760000000000"),
            Some("../../etc/passwd"),
        ),
        (
            Some("1048876"),
            Some("1760000000000"),
            Some("AB".repeat(32).as_str()),
        ),
    ] {
        assert_eq!(
            parse(size, last_modified, fingerprint),
            Err(invalid.clone()),
            "{size:?} {last_modified:?} {fingerprint:?}"
        );
    }
}

// --- SFTP file modes of staged uploads ---------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn creates_a_new_staged_file_with_0644_and_does_not_chmod_it() {
    let fx = Fixture::new().await;
    fx.mkdir("/remote");
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);

    upload(&fx, &options(described), &source[..]).await.unwrap();

    assert_eq!(
        fx.ops_starting("open "),
        vec![format!("open {staged} wctx 644")]
    );
    assert_eq!(mode_of(&fx, FILE), 0o644 & !umask());
}

#[cfg(unix)]
#[tokio::test]
async fn keeps_the_mode_a_continued_staged_file_was_created_with() {
    let fx = Fixture::new().await;
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    write_bytes(&fx, &staged, &source[..700_000]);
    set_mode(&fx.local(&staged), 0o600);

    upload(&fx, &options(described), &source[..]).await.unwrap();

    assert_eq!(mode_of(&fx, FILE), 0o600);
    assert_eq!(read_bytes(&fx, FILE), source);
}

// --- SFTP download resume ---------------------------------------------------------------------

async fn start_download(fx: &Fixture, range: Option<&DownloadRange>) -> crate::sftp::SftpDownload {
    fx.service
        .download_from(&fx.conn_id, "/srv/image.bin", range)
        .await
        .unwrap()
}

fn seven_bytes(fx: &Fixture) {
    let path = fx.write("/srv/image.bin", "payload");
    set_mtime(&path, 1_760_000_000);
}

#[tokio::test]
async fn sends_only_the_rest_of_a_file_that_is_unchanged_since_the_first_attempt() {
    let fx = Fixture::new().await;
    seven_bytes(&fx);

    let first = start_download(&fx, None).await;
    let etag = first.etag.clone().expect("a validator");
    assert_eq!(first.start, 0);
    assert_eq!(first.size, Some(7));
    assert!(
        etag.starts_with('"') && etag.ends_with('"') && etag.len() == 29,
        "{etag}"
    );
    assert!(
        etag[1..28]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    );
    assert_eq!(first.read_all().await.unwrap(), b"payload");

    let range = DownloadRange { start: 3, etag };
    let rest = start_download(&fx, Some(&range)).await;
    assert_eq!(rest.start, 3);
    assert_eq!(rest.size, Some(7));
    assert_eq!(rest.etag.as_deref(), Some(range.etag.as_str()));
    assert_eq!(rest.read_all().await.unwrap(), b"load");
}

#[tokio::test]
async fn sends_the_whole_file_once_it_changed_without_a_validator_or_past_its_end() {
    let fx = Fixture::new().await;
    seven_bytes(&fx);
    let first = start_download(&fx, None).await;
    let etag = first.etag.clone().unwrap();
    drop(first);

    for range in [
        DownloadRange {
            start: 3,
            etag: "\"an older version\"".into(),
        },
        DownloadRange {
            start: 7,
            etag: etag.clone(),
        },
        DownloadRange {
            start: 70,
            etag: etag.clone(),
        },
    ] {
        let whole = start_download(&fx, Some(&range)).await;
        assert_eq!(whole.start, 0, "{range:?}");
        assert_eq!(whole.read_all().await.unwrap(), b"payload");
    }

    // Changed on the server: another size or another time is another file.
    let path = fx.write("/srv/image.bin", "payloads");
    set_mtime(&path, 1_760_000_000);
    let range = DownloadRange {
        start: 3,
        etag: etag.clone(),
    };
    let changed = start_download(&fx, Some(&range)).await;
    assert_eq!(changed.start, 0);
    assert_ne!(changed.etag.as_deref(), Some(etag.as_str()));
    drop(changed);
    let path = fx.write("/srv/image.bin", "payload");
    set_mtime(&path, 1_760_000_001);
    let touched = start_download(&fx, Some(&range)).await;
    assert_eq!(touched.start, 0);
    assert_ne!(touched.etag.as_deref(), Some(etag.as_str()));
}

#[tokio::test]
async fn gives_the_same_file_on_another_host_and_another_path_another_validator() {
    let here = Fixture::new().await;
    let there = Fixture::new().await;
    seven_bytes(&here);
    seven_bytes(&there);
    let copy = here.write("/srv/copy.bin", "payload");
    set_mtime(&copy, 1_760_000_000);

    let on_here = start_download(&here, None).await;
    let on_there = start_download(&there, None).await;
    let other_path = here
        .service
        .download(&here.conn_id, "/srv/copy.bin")
        .await
        .unwrap();

    // The two servers listen on different ports, so they are different hosts.
    assert_ne!(on_here.etag, on_there.etag);
    assert_ne!(on_here.etag, other_path.etag);
    // The same file answers the same validator again, also on another request.
    let again = start_download(&here, None).await;
    assert_eq!(on_here.etag, again.etag);
}

#[tokio::test]
async fn reports_progress_of_a_continued_download_from_where_it_starts() {
    let fx = Fixture::new().await;
    seven_bytes(&fx);
    let first = start_download(&fx, None).await;
    let range = DownloadRange {
        start: 3,
        etag: first.etag.clone().unwrap(),
    };
    drop(first);

    let rest = start_download(&fx, Some(&range)).await;
    let mut sink = Vec::new();
    let mut progress = Vec::new();
    let copied = rest
        .copy_to(&mut sink, &mut |update| {
            progress.push((update.loaded, update.total))
        })
        .await
        .unwrap();

    assert_eq!(copied, 4);
    assert_eq!(sink, b"load");
    assert_eq!(progress.first(), Some(&(3, Some(7))));
    assert_eq!(progress.last(), Some(&(7, Some(7))));
}

#[cfg(unix)]
#[tokio::test]
async fn answers_a_file_that_cannot_be_opened_with_the_error_before_any_byte() {
    let fx = Fixture::new().await;
    let secret = fx.write("/srv/secret.key", "secret");
    set_mode(&secret, 0o000);
    // Root reads anything; there is nothing to refuse then.
    if std::fs::File::open(&secret).is_ok() {
        return;
    }

    let err = fx
        .service
        .download(&fx.conn_id, "/srv/secret.key")
        .await
        .unwrap_err();

    assert_eq!(err, Problem::internal("Permission denied"));
    assert!(eventually(|| fx.sftp_leases() == 0).await);
}

// --- A lost connection ------------------------------------------------------------------------

#[tokio::test]
async fn fails_an_upload_when_its_connection_drops_and_continues_it_on_the_next() {
    let fx = Arc::new(Fixture::new().await);
    fx.mkdir("/remote");
    let source = source_bytes();
    let described = describing(&source);
    let staged = staged_name(FILE, &described);
    let (release, released) = tokio::sync::watch::channel(false);
    let slow = Held {
        bytes: source.clone(),
        position: 0,
        release: released,
        waiting: None,
    };
    let first = tokio::spawn({
        let fx = fx.clone();
        async move { upload(&fx, &options(described), slow).await }
    });
    let staged_len = || std::fs::metadata(fx.local(&staged)).map_or(0, |meta| meta.len());
    assert!(eventually(|| staged_len() == MIB as u64).await);

    // The connection drops with the upload half way; the rest of the body then finds no
    // server to write to, and neither does the removal of the staged file.
    fx.server.disconnect_all().await;
    release.send_replace(true);
    let failed = tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .expect("the upload ends instead of waiting for an answer")
        .unwrap();
    assert!(failed.is_err());
    assert_eq!(staged_len(), MIB as u64);
    assert!(!fx.exists(FILE));

    let again = fx
        .manager
        .connect(
            &SshProfile::new("lab"),
            TestIo::accepting(PASSWORD),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .unwrap();
    let conn_id = again.connection().id.clone();
    assert_ne!(conn_id, fx.conn_id);
    let before = fx.sftp.writes().len();

    let response = fx
        .service
        .upload(
            &conn_id,
            FILE,
            &options(described),
            &source[..],
            &mut |_| {},
        )
        .await
        .unwrap();

    assert!(response.ok);
    let rest = &fx.sftp.writes()[before..];
    assert_eq!(
        rest.iter().map(|(_, offset, _)| *offset).min(),
        Some(MIB as u64)
    );
    assert_eq!(
        rest.iter().map(|(_, _, len)| len).sum::<usize>(),
        source.len() - MIB
    );
    assert_eq!(read_bytes(&fx, FILE), source);
    assert!(!fx.exists(&staged));
}

#[tokio::test]
async fn fails_a_download_when_its_connection_drops() {
    let fx = Fixture::new().await;
    let big = vec![0x42u8; 4 * MIB];
    write_bytes(&fx, "/srv/image.bin", &big);
    let mut download = start_download(&fx, None).await;
    let mut received = download
        .next_chunk()
        .await
        .unwrap()
        .expect("a first chunk")
        .len();

    fx.server.disconnect_all().await;
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match download.next_chunk().await {
                Ok(Some(chunk)) => received += chunk.len(),
                other => break other,
            }
        }
    })
    .await
    .expect("the download ends instead of waiting for an answer");

    assert!(ended.is_err(), "{ended:?}");
    assert!(received < big.len());
}
