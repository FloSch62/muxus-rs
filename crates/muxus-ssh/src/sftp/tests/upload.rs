//! Port of tests/unit/client/sftp-upload.test.ts ("uploading a drop over SFTP", "reading a
//! drop"), plus the same uploads end to end through the service.

use std::collections::VecDeque;
use std::sync::Mutex;

use super::*;
use crate::Problem;
use crate::connection::io::BoxFuture;
use crate::sftp::progress::{ByteProgress, CancelSignal, UploadStep};
use crate::sftp::upload::{
    ConfirmRequest, DropPayload, DroppedFile, SftpUploadDeps, TransferPhase, UploadDeps,
    UploadError, UploadProgress, UploadResult, collect_drop, upload_summary, upload_to_remote,
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

    fn upload<'a>(
        &'a self,
        _conn_id: &'a str,
        path: &'a str,
        overwrite: bool,
        file: &'a DroppedFile,
        _cancel: &'a CancelSignal,
        on_step: &'a mut (dyn FnMut(UploadStep) + Send),
    ) -> BoxFuture<'a, Result<(), UploadError>> {
        Box::pin(async move {
            if self.existing.iter().any(|existing| existing == path) && !overwrite {
                return Err(Problem::with_code(
                    409,
                    "a file already exists at the upload destination",
                    "SFTP_DESTINATION_EXISTS",
                )
                .into());
            }
            let text = std::fs::read_to_string(&file.source).unwrap();
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
