//! Port of tests/unit/client/sftp-upload.test.ts ("uploading a drop over SFTP", "reading a
//! drop"), plus the same uploads end to end through the service.

use std::collections::VecDeque;
use std::sync::Mutex;

use super::*;
use crate::Problem;
use crate::connection::io::BoxFuture;
use crate::sftp::UploadSource;
use crate::sftp::progress::{ByteProgress, CancelSignal, UploadStep};
#[cfg(unix)]
use crate::sftp::upload::collect_drop;
use crate::sftp::upload::{
    ConfirmRequest, DropPayload, DroppedFile, SftpUploadDeps, TransferPhase, UploadDeps,
    UploadError, UploadProgress, UploadResult, fingerprint_windows, upload_fingerprint,
    upload_source, upload_summary, upload_to_remote,
};

/// Local files for a drop: `(relative path, content)`.
fn payload(files: &[(&str, &str)], directories: &[&str]) -> (tempfile::TempDir, DropPayload) {
    let dir = tempfile::tempdir().unwrap();
    let files = files
        .iter()
        .enumerate()
        .map(|(index, (relative_path, text))| {
            let source = dir.path().join(format!("{index}"));
            std::fs::create_dir_all(&source).unwrap();
            let source = source.join(relative_path.rsplit('/').next().unwrap());
            std::fs::write(&source, text).unwrap();
            DroppedFile {
                source,
                relative_path: relative_path.to_string(),
                size: text.len() as u64,
            }
        })
        .collect();
    let directories = directories.iter().map(|dir| dir.to_string()).collect();
    (dir, DropPayload { files, directories })
}

/// Remote files that already exist answer the first, non-overwriting upload with a conflict.
#[derive(Default)]
struct Harness {
    existing: Vec<String>,
    answers: Mutex<VecDeque<bool>>,
    calls: Mutex<Vec<String>>,
    asked: Mutex<Vec<ConfirmRequest>>,
    cancel_on_confirm: Option<CancelSignal>,
    /// The source each upload request described its file with.
    sources: Mutex<Vec<Option<UploadSource>>>,
    /// What the server kept of every described file is another file's.
    mismatch: bool,
    /// Cancelled while a file is read for its fingerprint, which then never finishes.
    cancel_on_source: Option<CancelSignal>,
}

impl Harness {
    fn new(existing: &[&str], answers: &[bool]) -> Self {
        Self {
            existing: existing.iter().map(|path| path.to_string()).collect(),
            answers: Mutex::new(answers.iter().copied().collect()),
            ..Self::default()
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl UploadDeps for Harness {
    fn mkdir<'a>(
        &'a self,
        conn_id: &'a str,
        path: &'a str,
    ) -> BoxFuture<'a, Result<(), UploadError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(format!("mkdir {conn_id} {path}"));
            Ok(())
        })
    }

    fn source<'a>(
        &'a self,
        file: &'a DroppedFile,
    ) -> BoxFuture<'a, Result<Option<UploadSource>, UploadError>> {
        Box::pin(async move {
            if let Some(cancel) = &self.cancel_on_source {
                cancel.cancel();
                std::future::pending::<()>().await;
            }
            Ok(upload_source(file).await.map_err(Problem::from)?)
        })
    }

    fn upload<'a>(
        &'a self,
        _conn_id: &'a str,
        path: &'a str,
        overwrite: bool,
        source: Option<&'a UploadSource>,
        file: &'a DroppedFile,
        _cancel: &'a CancelSignal,
        on_step: &'a mut (dyn FnMut(UploadStep) + Send),
    ) -> BoxFuture<'a, Result<(), UploadError>> {
        Box::pin(async move {
            self.sources.lock().unwrap().push(source.copied());
            if self.mismatch && source.is_some() {
                return Err(Problem::with_code(
                    409,
                    "the upload kept on the server does not match this file",
                    "SFTP_UPLOAD_MISMATCH",
                )
                .into());
            }
            if self.existing.iter().any(|existing| existing == path) && !overwrite {
                return Err(Problem::with_code(
                    409,
                    "a file already exists at the upload destination",
                    "SFTP_DESTINATION_EXISTS",
                )
                .into());
            }
            let bytes = std::fs::read(&file.source).unwrap();
            // A megabyte of zeros would only hide the call.
            let text = match std::str::from_utf8(&bytes) {
                Ok(text) if bytes.len() < 1024 => text.to_string(),
                _ => format!("<{} bytes>", bytes.len()),
            };
            let replace = if overwrite { " (replace)" } else { "" };
            self.calls
                .lock()
                .unwrap()
                .push(format!("upload {path}{replace} {text}"));
            on_step(UploadStep::Progress(ByteProgress {
                loaded: file.size,
                total: Some(file.size),
                bytes_per_second: 10.0,
            }));
            on_step(UploadStep::Finalizing);
            Ok(())
        })
    }

    fn confirm(&self, request: ConfirmRequest) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(request);
            if let Some(cancel) = &self.cancel_on_confirm {
                cancel.cancel();
                return true;
            }
            self.answers.lock().unwrap().pop_front().unwrap_or(false)
        })
    }
}

async fn run(
    directory: &str,
    payload: &DropPayload,
    deps: &dyn UploadDeps,
    cancel: &CancelSignal,
) -> (Result<UploadResult, UploadError>, Vec<UploadProgress>) {
    let mut progress = Vec::new();
    let result = upload_to_remote(
        "conn-1",
        directory,
        payload,
        cancel,
        &mut |update| progress.push(update.clone()),
        deps,
    )
    .await;
    (result, progress)
}

#[tokio::test]
async fn creates_folders_shallowest_first_then_uploads_every_file_below_the_directory() {
    let harness = Harness::new(&[], &[]);
    let (_dir, drop) = payload(
        &[
            ("notes.txt", "n"),
            ("site/css/main.css", "body{}"),
            ("site/index.html", "<p>"),
        ],
        &["site/empty"],
    );

    let (result, progress) = run("/srv/app", &drop, &harness, &CancelSignal::new()).await;

    assert_eq!(
        harness.calls(),
        vec![
            "mkdir conn-1 /srv/app/site",
            "mkdir conn-1 /srv/app/site/empty",
            "mkdir conn-1 /srv/app/site/css",
            "upload /srv/app/notes.txt n",
            "upload /srv/app/site/css/main.css body{}",
            "upload /srv/app/site/index.html <p>",
        ]
    );
    assert_eq!(
        result.unwrap(),
        UploadResult {
            uploaded: 3,
            directories: 3
        }
    );
    let first = &progress[0];
    assert_eq!(
        (first.phase, first.file_index, first.file_count, first.total),
        (TransferPhase::Preparing, 0, 3, Some(10))
    );
    assert!(
        progress
            .iter()
            .any(|update| update.phase == TransferPhase::Transferring
                && update.name == "site/css/main.css"
                && update.file_index == 2)
    );
    let last = progress.last().unwrap();
    assert_eq!(
        (last.phase, last.loaded, last.file_index, last.file_count),
        (TransferPhase::Complete, 10, 3, 3)
    );
}

#[tokio::test]
async fn uploads_into_the_root_directory_without_doubling_the_slash() {
    let harness = Harness::new(&[], &[]);
    let (_dir, drop) = payload(&[("a.txt", "a")], &[]);
    run("/", &drop, &harness, &CancelSignal::new())
        .await
        .0
        .unwrap();
    assert_eq!(harness.calls(), vec!["upload /a.txt a"]);
}

#[tokio::test]
async fn replaces_an_existing_file_only_after_confirmation_and_skips_it_otherwise() {
    let harness = Harness::new(&["/srv/a.txt", "/srv/b.txt"], &[true, false]);
    let (_dir, drop) = payload(&[("a.txt", "A"), ("b.txt", "B"), ("c.txt", "C")], &[]);

    let (result, _) = run("/srv", &drop, &harness, &CancelSignal::new()).await;

    let asked = harness.asked.lock().unwrap().clone();
    assert_eq!(
        asked
            .iter()
            .map(|request| request.description.as_str())
            .collect::<Vec<_>>(),
        vec![
            "a.txt already exists on the remote host.",
            "b.txt already exists on the remote host.",
        ]
    );
    assert_eq!(asked[0].title, "Replace the existing file?");
    assert_eq!(asked[0].confirm_label, "Replace");
    assert!(asked[0].destructive);
    assert_eq!(
        harness.calls(),
        vec!["upload /srv/a.txt (replace) A", "upload /srv/c.txt C"]
    );
    assert_eq!(
        result.unwrap(),
        UploadResult {
            uploaded: 2,
            directories: 0
        }
    );
}

#[tokio::test]
async fn stops_at_the_next_file_once_cancelled_while_asking_to_replace() {
    let cancel = CancelSignal::new();
    let harness = Harness {
        cancel_on_confirm: Some(cancel.clone()),
        ..Harness::new(&["/srv/a.txt"], &[])
    };
    let (_dir, drop) = payload(&[("a.txt", "A"), ("b.txt", "B")], &[]);

    let (result, _) = run("/srv", &drop, &harness, &cancel).await;

    assert_eq!(result.unwrap_err(), UploadError::Cancelled);
    assert!(harness.calls().is_empty());
}

const MIB: usize = 1024 * 1024;

/// A local file of `bytes`, as one item of a drop.
fn dropped(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> DroppedFile {
    let source = dir.path().join(name);
    std::fs::write(&source, bytes).unwrap();
    DroppedFile {
        source,
        relative_path: name.to_string(),
        size: bytes.len() as u64,
    }
}

fn fingerprint_of(bytes: &[u8]) -> [u8; 32] {
    upload_fingerprint(&mut std::io::Cursor::new(bytes), bytes.len() as u64).unwrap()
}

#[tokio::test]
async fn describes_a_file_of_1_mib_or_more_to_the_server_so_it_can_continue_an_interrupted_upload()
{
    let harness = Harness::new(&[], &[]);
    let dir = tempfile::tempdir().unwrap();
    let image = dropped(&dir, "image.bin", &vec![0u8; MIB]);
    set_mtime(&image.source, 1_760_000_000);
    let note = dropped(&dir, "note.txt", b"payload");
    let drop = DropPayload {
        files: vec![image, note],
        directories: Vec::new(),
    };

    run("/srv", &drop, &harness, &CancelSignal::new())
        .await
        .0
        .unwrap();

    assert_eq!(
        *harness.sources.lock().unwrap(),
        vec![
            Some(UploadSource {
                size: MIB as u64,
                last_modified: 1_760_000_000_000,
                fingerprint: fingerprint_of(&vec![0u8; MIB]),
            }),
            None,
        ]
    );
    assert_eq!(
        harness.calls(),
        vec![
            "upload /srv/image.bin <1048576 bytes>",
            "upload /srv/note.txt payload"
        ]
    );
}

#[tokio::test]
async fn sends_a_file_again_as_a_new_upload_when_what_the_server_kept_of_it_does_not_match() {
    let harness = Harness {
        mismatch: true,
        ..Harness::new(&[], &[])
    };
    let dir = tempfile::tempdir().unwrap();
    let drop = DropPayload {
        files: vec![dropped(&dir, "image.bin", &vec![0u8; MIB])],
        directories: Vec::new(),
    };

    let (result, _) = run("/srv", &drop, &harness, &CancelSignal::new()).await;

    let described: Vec<bool> = harness
        .sources
        .lock()
        .unwrap()
        .iter()
        .map(Option::is_some)
        .collect();
    assert_eq!(described, vec![true, false]);
    assert_eq!(
        result.unwrap(),
        UploadResult {
            uploaded: 1,
            directories: 0
        }
    );
}

#[tokio::test]
async fn sends_nothing_for_a_file_cancelled_while_it_is_being_fingerprinted() {
    let cancel = CancelSignal::new();
    let harness = Harness {
        cancel_on_source: Some(cancel.clone()),
        ..Harness::new(&[], &[])
    };
    let dir = tempfile::tempdir().unwrap();
    let drop = DropPayload {
        files: vec![dropped(&dir, "image.bin", &vec![0u8; MIB])],
        directories: Vec::new(),
    };

    let (result, _) = run("/srv", &drop, &harness, &cancel).await;

    assert_eq!(result.unwrap_err(), UploadError::Cancelled);
    assert!(harness.calls().is_empty());
    assert!(harness.sources.lock().unwrap().is_empty());
}

#[test]
fn fingerprints_64_kib_windows_spread_over_the_whole_file_at_most_1_mib_apart() {
    let base = vec![0u8; 3 * MIB];
    let changed_at = |offset: usize| {
        let mut copy = base.clone();
        copy[offset] = 1;
        fingerprint_of(&copy)
    };
    let fingerprint = fingerprint_of(&base);

    assert_eq!(fingerprint_of(&base.clone()), fingerprint);
    for offset in [0, MIB, 2 * MIB, 3 * MIB - 1] {
        assert_ne!(changed_at(offset), fingerprint, "{offset}");
    }
    // Between windows a change goes unseen; the server checks the bytes before where it
    // continues.
    assert_eq!(changed_at(MIB + MIB / 2), fingerprint);
    // A file shorter than a window is read once, whole.
    assert_eq!(fingerprint_windows(7), vec![0]);
    assert_ne!(fingerprint_of(b"payload"), fingerprint_of(b"payloae"));
}

#[test]
fn reads_at_most_256_windows_the_first_and_the_last_among_them() {
    let size = 10 * 1024 * 1024 * 1024u64;
    let starts = fingerprint_windows(size);

    assert_eq!(starts.len(), 256);
    assert_eq!(starts[0], 0);
    assert_eq!(starts.last(), Some(&(size - 64 * 1024)));
    assert!(starts.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn tells_apart_full_flash_images_that_share_their_boot_loader_and_padding() {
    // 8 MiB: the same 512 KiB boot loader, a different 2 MiB firmware, then erased flash.
    let image = |firmware: usize| {
        let mut bytes = vec![0xffu8; 8 * MIB];
        for (index, byte) in bytes.iter_mut().take(512 * 1024).enumerate() {
            *byte = (index % 251) as u8;
        }
        for index in 0..2 * MIB {
            bytes[512 * 1024 + index] = ((index + firmware) % 253) as u8;
        }
        bytes
    };

    assert_ne!(fingerprint_of(&image(1)), fingerprint_of(&image(2)));
}

#[tokio::test]
async fn sends_small_files_whole_without_reading_them_for_a_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let small = dropped(&dir, "small.bin", &vec![7u8; MIB - 1]);
    assert_eq!(upload_source(&small).await.unwrap(), None);
    let gone = DroppedFile {
        source: dir.path().join("gone.bin"),
        relative_path: "gone.bin".into(),
        size: MIB as u64,
    };
    assert!(upload_source(&gone).await.is_err());
}

#[tokio::test]
async fn keeps_file_names_from_climbing_out_of_the_directory() {
    let harness = Harness::new(&[], &[]);
    let (_dir, drop) = payload(&[("../../etc/passwd", "x")], &[]);
    run("/srv", &drop, &harness, &CancelSignal::new())
        .await
        .0
        .unwrap();
    assert_eq!(
        harness.calls(),
        vec!["mkdir conn-1 /srv/etc", "upload /srv/etc/passwd x"]
    );
}

#[test]
fn summarizes_what_happened() {
    let result = |uploaded, directories| UploadResult {
        uploaded,
        directories,
    };
    assert_eq!(upload_summary(result(1, 0), None), "Uploaded 1 file");
    assert_eq!(
        upload_summary(result(3, 1), Some("/srv/app")),
        "Uploaded 3 files to /srv/app"
    );
    assert_eq!(
        upload_summary(result(0, 2), Some("/srv")),
        "Created 2 folders in /srv"
    );
    assert_eq!(upload_summary(result(0, 0), None), "Nothing was uploaded");
}

#[cfg(unix)]
#[test]
fn walks_dropped_folders_keeping_empty_ones() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::write(base.join("report.pdf"), "pdf").unwrap();
    std::fs::create_dir_all(base.join("project/src")).unwrap();
    std::fs::create_dir_all(base.join("project/empty")).unwrap();
    std::fs::write(base.join("project/README.md"), "readme").unwrap();
    std::fs::write(base.join("project/src/main.go"), "package main").unwrap();
    // A link back up the tree is walked once, not forever.
    std::os::unix::fs::symlink(base.join("project"), base.join("project/src/loop")).unwrap();

    let dropped = collect_drop(&[base.join("report.pdf"), base.join("project")]).unwrap();

    let mut files: Vec<&str> = dropped
        .files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect();
    files.sort();
    assert_eq!(
        files,
        vec!["project/README.md", "project/src/main.go", "report.pdf"]
    );
    let mut directories = dropped.directories.clone();
    directories.sort();
    assert_eq!(directories, vec!["project", "project/empty", "project/src"]);
    assert_eq!(dropped.files.iter().map(|file| file.size).sum::<u64>(), 21);
    assert!(collect_drop(&[]).unwrap().is_empty());
}

fn deps(
    fx: &Fixture,
    answer: bool,
) -> SftpUploadDeps<impl Fn(ConfirmRequest) -> BoxFuture<'static, bool> + Send + Sync> {
    SftpUploadDeps {
        service: fx.service.clone(),
        confirm: move |_request: ConfirmRequest| -> BoxFuture<'static, bool> {
            Box::pin(async move { answer })
        },
    }
}

#[tokio::test]
async fn uploads_a_dropped_folder_over_the_connection() {
    let fx = Fixture::new().await;
    fx.write("/srv/notes.txt", "old notes");
    let (_dir, drop) = payload(
        &[("notes.txt", "new notes"), ("site/index.html", "<p>")],
        &["site/empty"],
    );
    let deps = deps(&fx, true);
    let mut phases = Vec::new();

    let result = upload_to_remote(
        &fx.conn_id,
        "/srv",
        &drop,
        &CancelSignal::new(),
        &mut |update| {
            if phases.last() != Some(&update.phase) {
                phases.push(update.phase);
            }
        },
        &deps,
    )
    .await
    .unwrap();

    assert_eq!(
        result,
        UploadResult {
            uploaded: 2,
            directories: 2
        }
    );
    assert_eq!(fx.read("/srv/notes.txt"), "new notes");
    assert_eq!(fx.read("/srv/site/index.html"), "<p>");
    assert!(fx.local("/srv/site/empty").is_dir());
    assert_eq!(phases.first(), Some(&TransferPhase::Preparing));
    assert!(phases.contains(&TransferPhase::Transferring));
    assert!(phases.contains(&TransferPhase::Finalizing));
    assert_eq!(phases.last(), Some(&TransferPhase::Complete));
    assert_eq!(fx.sftp_leases(), 0);
}

#[tokio::test]
async fn a_declined_replacement_keeps_the_remote_file() {
    let fx = Fixture::new().await;
    fx.write("/srv/notes.txt", "old notes");
    let (_dir, drop) = payload(&[("notes.txt", "new notes")], &[]);

    let result = upload_to_remote(
        &fx.conn_id,
        "/srv",
        &drop,
        &CancelSignal::new(),
        &mut |_| {},
        &deps(&fx, false),
    )
    .await
    .unwrap();

    assert_eq!(result.uploaded, 0);
    assert_eq!(fx.read("/srv/notes.txt"), "old notes");
}

#[tokio::test]
async fn cancelling_mid_transfer_removes_the_staged_file() {
    let fx = Fixture::new().await;
    fx.mkdir("/srv");
    let big = "z".repeat(4 * 1024 * 1024);
    let (_dir, drop) = payload(&[("big.bin", &big)], &[]);
    let cancel = CancelSignal::new();

    let result = upload_to_remote(
        &fx.conn_id,
        "/srv",
        &drop,
        &cancel,
        &mut |update| {
            if update.phase == TransferPhase::Transferring {
                cancel.cancel();
            }
        },
        &deps(&fx, true),
    )
    .await;

    assert_eq!(result.unwrap_err(), UploadError::Cancelled);
    let staged = || {
        std::fs::read_dir(fx.local("/srv"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains(".muxus-"))
    };
    assert!(eventually(|| !staged()).await, "the staged file is removed");
    assert!(!fx.exists("/srv/big.bin"));
    assert!(eventually(|| fx.sftp_leases() == 0).await);
}
