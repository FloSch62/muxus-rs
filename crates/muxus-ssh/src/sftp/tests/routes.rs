//! Port of tests/unit/server/sftp-upload.test.ts (upload overwrite policy, download transport
//! ownership, remote editor) plus the other operations of server/src/routes/sftp.ts.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

use super::*;
use crate::Problem;
use crate::sftp::progress::{ByteProgress, UploadStep};
use crate::sftp::service::{MAX_EDITOR_BYTES, UploadOptions, delete_tree_with_budget};
use crate::sftp::types::{EditorFileResponse, SftpEntryType, SftpFileSaveRequest};

fn overwrite() -> UploadOptions {
    UploadOptions {
        overwrite: true,
        ..UploadOptions::default()
    }
}

/// `open <path> <flags> <mode>` lines of the server log, split up.
fn opens(fx: &Fixture) -> Vec<(String, String, String)> {
    fx.ops_starting("open ")
        .iter()
        .map(|line| {
            let parts: Vec<&str> = line.split(' ').collect();
            (parts[1].into(), parts[2].into(), parts[3].into())
        })
        .collect()
}

fn renames(fx: &Fixture, kind: &str) -> Vec<(String, String)> {
    fx.ops_starting(&format!("{kind} "))
        .iter()
        .map(|line| {
            let parts: Vec<&str> = line.split(' ').collect();
            (parts[1].into(), parts[2].into())
        })
        .collect()
}

fn file_changed(message: &str) -> Problem {
    Problem::with_code(409, message, "SFTP_FILE_CHANGED")
}

async fn upload(
    fx: &Fixture,
    path: &str,
    options: &UploadOptions,
    body: &[u8],
) -> Result<crate::sftp::SftpUploadResponse, Problem> {
    fx.service
        .upload(&fx.conn_id, path, options, body, &mut |_| {})
        .await
}

fn local_mtime_ms(path: &std::path::Path) -> i64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).unwrap().mtime() * 1000
}

// --- SFTP upload overwrite policy -------------------------------------------------------------

#[tokio::test]
async fn returns_a_conflict_without_opening_an_existing_destination() {
    let fx = Fixture::new().await;
    fx.write("/remote/report.txt", "old");

    let err = upload(
        &fx,
        "/remote/report.txt",
        &UploadOptions::default(),
        b"payload",
    )
    .await
    .unwrap_err();

    assert_eq!(
        err,
        Problem::with_code(
            409,
            "a file already exists at the upload destination",
            "SFTP_DESTINATION_EXISTS"
        )
    );
    assert!(opens(&fx).is_empty());
    assert_eq!(fx.read("/remote/report.txt"), "old");
}

#[tokio::test]
async fn stages_a_new_destination_before_renaming_it_into_place() {
    let fx = Fixture::new().await;
    fx.mkdir("/remote");

    let response = upload(
        &fx,
        "/remote/report.txt",
        &UploadOptions::default(),
        b"payload",
    )
    .await
    .unwrap();

    assert!(response.ok);
    assert_eq!(
        response.mtime_ms,
        Some(local_mtime_ms(&fx.local("/remote/report.txt")))
    );
    let opens = opens(&fx);
    assert_eq!(opens.len(), 1);
    let (temp, flags, mode) = &opens[0];
    assert!(is_staged(temp, "/remote/report.txt", "upload"), "{temp}");
    // `wx` without a mode: a new file the server creates with its defaults.
    assert_eq!((flags.as_str(), mode.as_str()), ("wctx", "-"));
    assert_eq!(
        renames(&fx, "rename"),
        vec![(temp.clone(), "/remote/report.txt".to_string())]
    );
    assert!(renames(&fx, "posix-rename").is_empty());
    assert_eq!(fx.read("/remote/report.txt"), "payload");
    assert!(!fx.exists(temp));
}

#[tokio::test]
async fn creates_clipboard_images_with_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new().await;
    fx.mkdir("/tmp");

    let response = fx
        .service
        .clipboard_image(&fx.conn_id, &b"\x89PNG"[..])
        .await
        .unwrap();

    let name = response.path.strip_prefix("/tmp/muxus-paste-").unwrap();
    let (millis, rest) = name.split_once('-').unwrap();
    assert!(millis.chars().all(|c| c.is_ascii_digit()) && !millis.is_empty());
    let hex = rest.strip_suffix(".png").unwrap();
    assert_eq!(hex.len(), 32);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));

    let opens = opens(&fx);
    let (temp, flags, mode) = &opens[0];
    assert!(is_staged(temp, &response.path, "upload"), "{temp}");
    assert_eq!((flags.as_str(), mode.as_str()), ("wctx", "600"));
    assert_eq!(
        renames(&fx, "rename"),
        vec![(temp.clone(), response.path.clone())]
    );
    let meta = std::fs::metadata(fx.local(&response.path)).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

#[tokio::test]
async fn atomically_replaces_an_existing_regular_file_after_explicit_overwrite_consent() {
    let fx = Fixture::new().await;
    let existing = fx.write("/remote/report.txt", "old");
    set_mode(&existing, 0o640);

    let response = upload(&fx, "/remote/report.txt", &overwrite(), b"payload")
        .await
        .unwrap();

    assert!(response.ok);
    let opens = opens(&fx);
    let (temp, flags, mode) = &opens[0];
    assert!(is_staged(temp, "/remote/report.txt", "upload"), "{temp}");
    // The replacement keeps the permissions of the file it replaces.
    assert_eq!((flags.as_str(), mode.as_str()), ("wctx", "640"));
    assert_eq!(
        renames(&fx, "posix-rename"),
        vec![(temp.clone(), "/remote/report.txt".to_string())]
    );
    assert!(renames(&fx, "rename").is_empty());
    assert_eq!(fx.read("/remote/report.txt"), "payload");
}

#[tokio::test]
async fn replaces_a_file_only_while_it_keeps_the_modification_time_the_client_downloaded() {
    let fx = Fixture::new().await;
    let plan = fx.write("/remote/plan.drawio", "v1");
    set_mtime(&plan, 1_700_000_000);

    let stale = upload(
        &fx,
        "/remote/plan.drawio",
        &UploadOptions::from_query(Some("true"), Some("1699999999000")).unwrap(),
        b"v2",
    )
    .await
    .unwrap_err();
    assert_eq!(
        stale,
        file_changed("the remote file changed since it was downloaded")
    );
    assert!(opens(&fx).is_empty());

    let current = upload(
        &fx,
        "/remote/plan.drawio",
        &UploadOptions::from_query(Some("true"), Some("1700000000000")).unwrap(),
        b"v2",
    )
    .await
    .unwrap();
    assert!(current.ok);
    assert_eq!(current.mtime_ms, Some(local_mtime_ms(&plan)));
    assert_eq!(opens(&fx).len(), 1);
    assert_eq!(fx.read("/remote/plan.drawio"), "v2");
}

#[tokio::test]
async fn treats_a_removed_remote_file_as_changed_when_a_version_was_expected() {
    let fx = Fixture::new().await;
    fx.mkdir("/remote");

    let err = upload(
        &fx,
        "/remote/plan.drawio",
        &UploadOptions::from_query(Some("true"), Some("1700000000000")).unwrap(),
        b"v2",
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, 409);
    assert!(opens(&fx).is_empty());

    let invalid = UploadOptions {
        expected_mtime_ms: Some(f64::NAN),
        ..UploadOptions::default()
    };
    assert_eq!(
        upload(&fx, "/remote/plan.drawio", &invalid, b"v2")
            .await
            .unwrap_err()
            .status,
        400
    );
    assert_eq!(
        UploadOptions::from_query(None, Some("soon"))
            .unwrap_err()
            .status,
        400
    );
}

#[tokio::test]
async fn replaces_a_file_on_servers_without_the_posix_rename_extension() {
    let options = SftpServerOptions {
        posix_rename: false,
        ..SftpServerOptions::default()
    };
    let fx = Fixture::with(options, |_| {}).await;
    fx.write("/remote/report.txt", "old");

    let response = upload(&fx, "/remote/report.txt", &overwrite(), b"payload")
        .await
        .unwrap();

    assert!(response.ok);
    // The extension was never advertised, so it was never asked for.
    assert!(fx.ops_starting("extended").is_empty());
    assert!(renames(&fx, "posix-rename").is_empty());
    // Plain SFTP v3 rename refuses the existing destination, then unlink + rename.
    assert_eq!(renames(&fx, "rename").len(), 2);
    assert_eq!(
        fx.ops_starting("remove "),
        vec!["remove /remote/report.txt"]
    );
    assert_eq!(fx.read("/remote/report.txt"), "payload");
}

#[tokio::test]
async fn refuses_to_follow_a_destination_symlink_even_with_overwrite_consent() {
    let fx = Fixture::new().await;
    let target = fx.write("/elsewhere/real.txt", "keep");
    fx.mkdir("/remote");
    std::os::unix::fs::symlink(&target, fx.local("/remote/report.txt")).unwrap();

    let err = upload(&fx, "/remote/report.txt", &overwrite(), b"payload")
        .await
        .unwrap_err();

    assert_eq!(
        err,
        Problem::with_code(
            409,
            "refusing to overwrite a symbolic link",
            "SFTP_DESTINATION_IS_SYMLINK"
        )
    );
    assert!(opens(&fx).is_empty());
    assert_eq!(fx.read("/elsewhere/real.txt"), "keep");
}

#[tokio::test]
async fn refuses_to_upload_over_a_directory() {
    let fx = Fixture::new().await;
    fx.mkdir("/remote/report.txt");

    let err = upload(&fx, "/remote/report.txt", &overwrite(), b"payload")
        .await
        .unwrap_err();

    assert_eq!(
        err,
        Problem::with_code(
            409,
            "a directory already exists at the upload destination",
            "SFTP_DESTINATION_IS_DIRECTORY"
        )
    );
}

/// A request body that breaks off after its first chunk.
struct InterruptedBody {
    sent: bool,
}

impl AsyncRead for InterruptedBody {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.sent {
            return Poll::Ready(Err(std::io::Error::other("request aborted")));
        }
        self.sent = true;
        buf.put_slice(b"partial");
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn removes_the_staged_file_when_an_upload_is_interrupted() {
    let fx = Fixture::new().await;
    fx.mkdir("/remote");

    let err = fx
        .service
        .upload(
            &fx.conn_id,
            "/remote/report.txt",
            &UploadOptions::default(),
            InterruptedBody { sent: false },
            &mut |_| {},
        )
        .await
        .unwrap_err();

    assert_eq!(err, Problem::internal("request aborted"));
    let (temp, _, _) = opens(&fx).remove(0);
    assert!(is_staged(&temp, "/remote/report.txt", "upload"));
    assert_eq!(fx.ops_starting("remove "), vec![format!("remove {temp}")]);
    assert!(renames(&fx, "rename").is_empty());
    assert!(!fx.exists(&temp));
    assert!(!fx.exists("/remote/report.txt"));
}

#[tokio::test]
async fn reports_upload_progress_and_the_finalizing_step() {
    let fx = Fixture::new().await;
    fx.mkdir("/remote");
    let body = vec![7u8; 300 * 1024];
    let mut steps = Vec::new();
    let options = UploadOptions {
        size: Some(body.len() as u64),
        ..UploadOptions::default()
    };

    fx.service
        .upload(
            &fx.conn_id,
            "/remote/big.bin",
            &options,
            &body[..],
            &mut |step| steps.push(step),
        )
        .await
        .unwrap();

    let total = Some(body.len() as u64);
    assert!(matches!(
        steps.first(),
        Some(UploadStep::Progress(ByteProgress { loaded: 0, total: t, .. })) if *t == total
    ));
    let finalizing = steps
        .iter()
        .position(|step| *step == UploadStep::Finalizing)
        .unwrap();
    assert!(matches!(
        steps[finalizing - 1],
        UploadStep::Progress(ByteProgress { loaded, .. }) if loaded == body.len() as u64
    ));
    assert_eq!(std::fs::read(fx.local("/remote/big.bin")).unwrap(), body);
}

// --- SFTP download transport ownership --------------------------------------------------------

#[tokio::test]
async fn holds_its_connection_lease_until_the_stream_or_the_reader_closes() {
    let fx = Fixture::new().await;
    // Larger than what the download reads ahead, so the stream stays open until it is read.
    let content = "x".repeat(1024 * 1024);
    let report = fx.write("/remote/report.txt", &content);
    set_mtime(&report, 1_760_000_000);

    let download = fx
        .service
        .download(&fx.conn_id, "/remote/report.txt")
        .await
        .unwrap();
    assert_eq!(download.name, "report.txt");
    assert_eq!(download.size, Some(content.len() as u64));
    assert_eq!(download.mtime_ms, Some(1_760_000_000_000));
    assert_eq!(fx.sftp_leases(), 1);

    // The reader went away (the HTTP response closed): the lease goes at once.
    drop(download);
    assert_eq!(fx.sftp_leases(), 0);

    // Reading the stream to its end releases the lease too, before the reader is dropped.
    let mut download = fx
        .service
        .download(&fx.conn_id, "/remote/report.txt")
        .await
        .unwrap();
    let mut read = Vec::new();
    AsyncReadExt::read_to_end(&mut download, &mut read)
        .await
        .unwrap();
    assert_eq!(read.len(), content.len());
    assert_eq!(fx.sftp_leases(), 0);
    drop(download);
    assert_eq!(fx.sftp_leases(), 0);
}

#[tokio::test]
async fn streams_downloads_and_reports_their_final_byte_count() {
    let fx = Fixture::new().await;
    fx.write("/remote/report.txt", "payload");
    let download = fx
        .service
        .download(&fx.conn_id, "/remote/report.txt")
        .await
        .unwrap();
    let mut out = Vec::new();
    let mut progress = Vec::new();

    let copied = download
        .copy_to(&mut out, &mut |update| progress.push(update))
        .await
        .unwrap();

    assert_eq!(copied, 7);
    assert_eq!(out, b"payload");
    let last = progress.last().unwrap();
    assert_eq!((last.loaded, last.total), (7, Some(7)));
}

#[tokio::test]
async fn a_missing_file_fails_before_the_download_starts() {
    let fx = Fixture::new().await;
    let err = fx
        .service
        .download(&fx.conn_id, "/remote/missing.txt")
        .await
        .unwrap_err();
    assert_eq!(err, Problem::internal("No such file"));
    assert_eq!(fx.sftp_leases(), 0);
}

#[tokio::test]
async fn drag_downloads_need_a_live_ticket_for_their_connection() {
    let fx = Fixture::new().await;
    fx.write("/srv/a.txt", "drag me");
    let listing = fx.service.list(&fx.conn_id, "/srv").await.unwrap();
    let ticket = listing.entries[0].download_ticket.clone().unwrap();

    let download = fx
        .service
        .drag_download(&fx.conn_id, &ticket)
        .await
        .unwrap();
    assert_eq!(download.name, "a.txt");
    assert_eq!(download.read_all().await.unwrap(), b"drag me");

    // Tickets stay valid for their lifetime, but only for the connection that listed them; a
    // mismatch burns the ticket.
    let expired = Problem::new(401, "download ticket is missing or expired");
    assert_eq!(
        fx.service
            .drag_download("other", &ticket)
            .await
            .unwrap_err(),
        expired
    );
    assert_eq!(
        fx.service
            .drag_download(&fx.conn_id, &ticket)
            .await
            .unwrap_err(),
        expired
    );
    assert_eq!(
        fx.service
            .drag_download(&fx.conn_id, "made-up")
            .await
            .unwrap_err(),
        expired
    );

    let short_lived = SftpService::with_ticket_ttl(fx.manager.clone(), Duration::ZERO);
    let listing = short_lived.list(&fx.conn_id, "/srv").await.unwrap();
    let ticket = listing.entries[0].download_ticket.clone().unwrap();
    assert_eq!(
        short_lived
            .drag_download(&fx.conn_id, &ticket)
            .await
            .unwrap_err(),
        expired
    );
}

// --- SFTP remote editor -----------------------------------------------------------------------

#[tokio::test]
async fn reads_bounded_utf8_text_with_its_optimistic_concurrency_metadata() {
    let fx = Fixture::new().await;
    let conf = fx.write("/etc/example.conf", "hello remote");
    set_mode(&conf, 0o640);
    set_mtime(&conf, 123);

    let response = fx
        .service
        .read_file(&fx.conn_id, "/etc/example.conf")
        .await
        .unwrap();

    assert_eq!(
        response,
        EditorFileResponse {
            path: "/etc/example.conf".into(),
            content: "hello remote".into(),
            size: 12,
            mtime_ms: Some(123_000),
            mode: Some(0o640),
        }
    );
}

#[tokio::test]
async fn refuses_files_the_editor_cannot_show() {
    let fx = Fixture::new().await;
    fx.mkdir("/srv/dir");
    let target = fx.write("/srv/real.txt", "text");
    std::os::unix::fs::symlink(&target, fx.local("/srv/link.txt")).unwrap();
    let huge = std::fs::File::create(fx.local("/srv/huge.log")).unwrap();
    huge.set_len(MAX_EDITOR_BYTES + 1).unwrap();
    std::fs::write(fx.local("/srv/blob.bin"), b"ab\0cd").unwrap();
    std::fs::write(fx.local("/srv/latin1.txt"), b"caf\xe9").unwrap();

    let read = |path: &'static str| fx.service.read_file(&fx.conn_id, path);
    assert_eq!(
        read("/srv/dir").await.unwrap_err(),
        Problem::bad_request("cannot edit a directory")
    );
    assert_eq!(
        read("/srv/link.txt").await.unwrap_err(),
        Problem::with_code(
            409,
            "open the symbolic link target explicitly",
            "SFTP_EDITOR_SYMLINK"
        )
    );
    assert_eq!(
        read("/srv/huge.log").await.unwrap_err(),
        Problem::new(
            413,
            "files larger than 8 MiB cannot be opened in the editor"
        )
    );
    assert_eq!(
        read("/srv/blob.bin").await.unwrap_err(),
        Problem::new(
            415,
            "this appears to be a binary file and cannot be opened as text"
        )
    );
    assert_eq!(
        read("/srv/latin1.txt").await.unwrap_err(),
        Problem::new(415, "the remote editor currently supports UTF-8 text files")
    );
}

#[tokio::test]
async fn drops_a_leading_byte_order_mark_but_counts_its_bytes() {
    let fx = Fixture::new().await;
    std::fs::create_dir_all(fx.local("/srv")).unwrap();
    std::fs::write(fx.local("/srv/bom.txt"), "\u{feff}text").unwrap();

    let response = fx
        .service
        .read_file(&fx.conn_id, "/srv/bom.txt")
        .await
        .unwrap();

    assert_eq!(response.content, "text");
    assert_eq!(response.size, 7);
}

#[tokio::test]
async fn rejects_a_save_when_the_remote_file_changed() {
    let fx = Fixture::new().await;
    let conf = fx.write("/etc/example.conf", "old value");
    set_mtime(&conf, 124);

    let err = fx
        .service
        .save_file(
            &fx.conn_id,
            "/etc/example.conf",
            &SftpFileSaveRequest {
                content: "new value".into(),
                expected_mtime_ms: Some(123_000),
                force: None,
            },
        )
        .await
        .unwrap_err();

    assert_eq!(
        err,
        file_changed("the remote file changed since it was opened")
    );
    assert!(opens(&fx).is_empty());
    assert_eq!(fx.read("/etc/example.conf"), "old value");

    // An explicit force saves anyway.
    let forced = fx
        .service
        .save_file(
            &fx.conn_id,
            "/etc/example.conf",
            &SftpFileSaveRequest {
                content: "new value".into(),
                expected_mtime_ms: Some(123_000),
                force: Some(true),
            },
        )
        .await
        .unwrap();
    assert!(forced.ok);
    assert_eq!(fx.read("/etc/example.conf"), "new value");
}

#[tokio::test]
async fn atomically_replaces_the_remote_text_when_the_version_still_matches() {
    let fx = Fixture::new().await;
    let conf = fx.write("/etc/example.conf", "old value");
    set_mode(&conf, 0o640);
    set_mtime(&conf, 123);

    let response = fx
        .service
        .save_file(
            &fx.conn_id,
            "/etc/example.conf",
            &SftpFileSaveRequest {
                content: "new value".into(),
                expected_mtime_ms: Some(123_000),
                force: None,
            },
        )
        .await
        .unwrap();

    assert!(response.ok);
    assert_eq!(response.size, 9);
    assert_eq!(response.mtime_ms, Some(local_mtime_ms(&conf)));
    assert_eq!(fx.read("/etc/example.conf"), "new value");
    let opens = opens(&fx);
    let (temp, flags, mode) = &opens[0];
    assert!(is_staged(temp, "/etc/example.conf", "tmp"), "{temp}");
    assert_eq!((flags.as_str(), mode.as_str()), ("wctx", "640"));
    assert_eq!(
        renames(&fx, "posix-rename"),
        vec![(temp.clone(), "/etc/example.conf".to_string())]
    );
}

#[tokio::test]
async fn refuses_saves_over_directories_links_and_oversized_text() {
    let fx = Fixture::new().await;
    fx.mkdir("/srv/dir");
    let target = fx.write("/srv/real.txt", "text");
    std::os::unix::fs::symlink(&target, fx.local("/srv/link.txt")).unwrap();
    let save = |path: &'static str, content: String| {
        let service = fx.service.clone();
        let conn_id = fx.conn_id.clone();
        async move {
            service
                .save_file(
                    &conn_id,
                    path,
                    &SftpFileSaveRequest {
                        content,
                        expected_mtime_ms: None,
                        force: None,
                    },
                )
                .await
                .unwrap_err()
        }
    };

    assert_eq!(
        save("/srv/dir", "x".into()).await,
        Problem::bad_request("cannot overwrite a directory")
    );
    assert_eq!(
        save("/srv/link.txt", "x".into()).await,
        Problem::with_code(
            409,
            "refusing to overwrite a symbolic link",
            "SFTP_DESTINATION_IS_SYMLINK"
        )
    );
    assert_eq!(
        save("/srv/real.txt", "x".repeat(MAX_EDITOR_BYTES as usize + 1)).await,
        Problem::new(
            413,
            "files larger than 8 MiB cannot be saved from the editor"
        )
    );
    assert_eq!(fx.read("/srv/real.txt"), "text");
}

// --- Listing, folders, renames and deletes ----------------------------------------------------

#[tokio::test]
async fn lists_a_directory_with_types_permissions_owners_and_drag_tickets() {
    let options = SftpServerOptions {
        home: "/home/tester".into(),
        ..SftpServerOptions::default()
    };
    let fx = Fixture::with(options, |_| {}).await;
    let notes = fx.write("/home/tester/notes.txt", "12345");
    set_mode(&notes, 0o640);
    set_mtime(&notes, 1_700_000_000);
    fx.mkdir("/home/tester/src");
    std::os::unix::fs::symlink("notes.txt", fx.local("/home/tester/latest")).unwrap();

    assert_eq!(
        fx.service.home(&fx.conn_id).await.unwrap().path,
        "/home/tester"
    );
    let listing = fx.service.list(&fx.conn_id, ".").await.unwrap();

    assert_eq!(listing.path, "/home/tester");
    let names: Vec<(&str, SftpEntryType)> = listing
        .entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry.kind))
        .collect();
    assert_eq!(
        names,
        vec![
            ("latest", SftpEntryType::Link),
            ("notes.txt", SftpEntryType::File),
            ("src", SftpEntryType::Dir),
        ]
    );
    let notes = &listing.entries[1];
    assert_eq!(notes.size, Some(5));
    assert_eq!(notes.mode, Some(0o640));
    assert_eq!(notes.mtime_ms, Some(1_700_000_000_000));
    assert_eq!(notes.owner.as_deref(), Some("tester"));
    assert_eq!(notes.group.as_deref(), Some("staff"));
    let ticket = notes.download_ticket.as_deref().unwrap();
    assert_eq!(ticket.len(), 24);
    assert!(listing.entries[0].download_ticket.is_none());
    assert!(listing.entries[2].download_ticket.is_none());

    let json = serde_json::to_value(notes).unwrap();
    assert_eq!(json["type"], "file");
    assert_eq!(json["mtimeMs"], 1_700_000_000_000_i64);
    assert_eq!(json["downloadTicket"], ticket);
    // Every operation hands its lease back when it is done.
    assert_eq!(fx.sftp_leases(), 0);
}

#[tokio::test]
async fn creates_folders_one_level_or_with_their_parents() {
    let fx = Fixture::new().await;
    fx.mkdir("/srv");
    fx.write("/srv/file", "x");

    fx.service
        .mkdir(&fx.conn_id, "/srv/one", false)
        .await
        .unwrap();
    assert!(fx.local("/srv/one").is_dir());
    assert_eq!(
        fx.service
            .mkdir(&fx.conn_id, "/srv/one", false)
            .await
            .unwrap_err()
            .status,
        500
    );

    let ok = fx
        .service
        .mkdir(&fx.conn_id, "/srv/one/two/../three/four", true)
        .await
        .unwrap();
    assert!(ok.ok);
    assert!(fx.local("/srv/one/three/four").is_dir());
    assert!(!fx.exists("/srv/one/two"));
    // Existing folders along the way are fine, a file in the way is not.
    fx.service
        .mkdir(&fx.conn_id, "/srv/one/three", true)
        .await
        .unwrap();
    assert_eq!(
        fx.service
            .mkdir(&fx.conn_id, "/srv/file/sub", true)
            .await
            .unwrap_err(),
        Problem::internal("Failure")
    );
}

#[tokio::test]
async fn renames_entries_and_requires_both_names() {
    let fx = Fixture::new().await;
    fx.write("/srv/a.txt", "a");

    fx.service
        .rename(&fx.conn_id, "/srv/a.txt", "/srv/b.txt")
        .await
        .unwrap();
    assert_eq!(fx.read("/srv/b.txt"), "a");
    assert_eq!(
        fx.service
            .rename(&fx.conn_id, "", "/srv/c.txt")
            .await
            .unwrap_err(),
        Problem::bad_request("from and to are required")
    );
}

#[tokio::test]
async fn deletes_files_and_whole_trees_without_following_links() {
    let fx = Fixture::new().await;
    fx.write("/srv/single.txt", "x");
    fx.write("/srv/tree/a.txt", "a");
    fx.write("/srv/tree/deep/b.txt", "b");
    fx.mkdir("/srv/tree/empty");
    let outside = fx.write("/keep/outside.txt", "keep");
    std::os::unix::fs::symlink(outside.parent().unwrap(), fx.local("/srv/tree/link")).unwrap();

    fx.service
        .delete(&fx.conn_id, "/srv/single.txt")
        .await
        .unwrap();
    assert!(!fx.exists("/srv/single.txt"));

    fx.service.delete(&fx.conn_id, "/srv/tree").await.unwrap();
    assert!(!fx.exists("/srv/tree"));
    assert_eq!(fx.read("/keep/outside.txt"), "keep");
}

#[tokio::test]
async fn stops_a_runaway_tree_delete_at_its_entry_budget() {
    let fx = Fixture::new().await;
    for name in ["a", "b", "c"] {
        fx.write(&format!("/srv/tree/{name}"), name);
    }

    let err = delete_tree_with_budget(&fx.service, &fx.conn_id, "/srv/tree", 2)
        .await
        .unwrap_err();

    assert_eq!(
        err,
        Problem::bad_request("refusing to delete more than 2 entries")
    );
    assert!(fx.exists("/srv/tree"));
}

// --- Requests the routes rejected up front ----------------------------------------------------

#[tokio::test]
async fn answers_unknown_connections_missing_paths_and_disabled_sftp() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.service.list("stale", "/").await.unwrap_err(),
        Problem::not_found("connection not found")
    );
    assert_eq!(
        fx.service.list(&fx.conn_id, "").await.unwrap_err(),
        Problem::bad_request("path is required")
    );
    assert_eq!(fx.sftp_leases(), 0);

    let disabled = Fixture::build(
        SftpServerOptions::default(),
        |_| {},
        |options| options.disable_sftp_for_host = Some(Arc::new(|_: &str| true)),
    )
    .await;
    assert_eq!(
        disabled
            .service
            .list(&disabled.conn_id, "/")
            .await
            .unwrap_err(),
        Problem::internal("SFTP is disabled for this host.")
    );
    assert_eq!(disabled.sftp_leases(), 0);
}

// --- One shared SFTP session per connection ---------------------------------------------------

#[tokio::test]
async fn operations_share_one_sftp_session_and_reopen_it_after_it_ended() {
    let fx = Fixture::new().await;
    fx.write("/srv/a.txt", "a");
    let (first, second) = tokio::join!(
        fx.service.list(&fx.conn_id, "/srv"),
        fx.service.read_file(&fx.conn_id, "/srv/a.txt"),
    );
    first.unwrap();
    second.unwrap();
    fx.service.home(&fx.conn_id).await.unwrap();
    let subsystems = || {
        fx.server
            .events()
            .iter()
            .filter(|event| *event == "subsystem sftp")
            .count()
    };
    assert_eq!(subsystems(), 1);

    // The server's SFTP side goes away while the SSH transport stays up.
    let connection = fx.manager.get(&fx.conn_id).unwrap();
    connection
        .sftp()
        .await
        .unwrap()
        .session
        .close_session()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let listing = fx.service.list(&fx.conn_id, "/srv").await.unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(subsystems(), 2);
}
