//! The texts the compare dialog compares (`client/src/compare-sources.ts`):
//! a terminal's output or selection, a session history record as its clean
//! log, a local file or a file read over SFTP.

use std::path::Path;

use muxus_core::api::{SessionLogSummary, session_log_name};
use muxus_session::Backend;

use crate::features::launcher::time_format::short_date_medium_time;

/// One side of the compare dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareText {
    pub label: String,
    pub text: String,
    /// A file read over SFTP, so a second file on that connection starts from its path.
    pub remote: Option<RemoteFile>,
}

/// Where a remote side was read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFile {
    pub conn_id: String,
    pub host: String,
    pub path: String,
}

/// Texts above this many characters are refused, as files above 8 MiB are.
pub const MAX_COMPARE_CHARS: usize = 8 * 1024 * 1024;
/// Local files above this size are refused, as the editor refuses them.
pub const MAX_LOCAL_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The text's length as JavaScript counts it (UTF-16 units).
fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The side, or why it is too large to compare.
pub fn comparable(side: CompareText) -> Result<CompareText, String> {
    // A cheap bound first: UTF-16 never has more units than UTF-8 has bytes.
    if side.text.len() > MAX_COMPARE_CHARS && js_length(&side.text) > MAX_COMPARE_CHARS {
        return Err("Texts larger than 8 MiB cannot be compared.".into());
    }
    Ok(side)
}

/// A history record's name, followed by when it started.
pub fn history_compare_label(session: &SessionLogSummary) -> String {
    let name = session_log_name(&session.title, session.label.as_deref());
    let started = muxus_core::js::parse_timestamp(&session.started_at)
        .map(|at| short_date_medium_time(&at.with_timezone(&chrono::Local)))
        .unwrap_or_else(|| "Invalid Date".into());
    format!("{name} \u{b7} {started}")
}

/// A session history record as its clean log, without timestamps.
pub async fn history_compare_text(
    backend: &Backend,
    session: &SessionLogSummary,
) -> Result<CompareText, String> {
    let export = backend
        .export_session_log_clean(&session.id, false)
        .await
        .map_err(|error| error.message)?;
    Ok(CompareText {
        label: history_compare_label(session),
        text: String::from_utf8_lossy(&export.body).into_owned(),
        remote: None,
    })
}

/// A file read over SFTP, labelled `host:path`.
pub async fn remote_file_compare_text(
    backend: &Backend,
    conn_id: &str,
    host: &str,
    path: &str,
) -> Result<CompareText, String> {
    let file = backend
        .sftp_read_file(conn_id, path)
        .await
        .map_err(|error| error.message)?;
    Ok(CompareText {
        label: format!("{host}:{}", file.path),
        text: file.content,
        remote: Some(RemoteFile {
            conn_id: conn_id.into(),
            host: host.into(),
            path: file.path,
        }),
    })
}

/// A terminal's output, or its selection.
pub fn terminal_compare_text(title: &str, text: String, selection: bool) -> CompareText {
    CompareText {
        label: format!(
            "{title} ({})",
            if selection {
                "selection"
            } else {
                "terminal output"
            }
        ),
        text,
        remote: None,
    }
}

/// A local file, labelled by its name.
pub fn local_file_compare_text(path: &Path) -> Result<CompareText, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .len();
    if size > MAX_LOCAL_FILE_BYTES {
        return Err("Files larger than 8 MiB cannot be compared.".into());
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    Ok(CompareText {
        label: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        text: String::from_utf8_lossy(&bytes).into_owned(),
        remote: None,
    })
}

#[cfg(test)]
mod tests {
    //! Port of `tests/unit/client/compare-sources.test.ts` (the request URLs
    //! are `Backend` calls here; the backend's own tests cover them).

    use super::*;

    fn session() -> SessionLogSummary {
        serde_json::from_value(serde_json::json!({
            "id": "log-1",
            "profileKey": "ssh:core-1",
            "title": "admin@core-1",
            "kind": "ssh",
            "host": "core-1",
            "startedAt": "2026-10-14T12:00:00.000Z",
            "status": "completed",
            "paused": false,
            "captureInput": false,
            "eventCount": 3,
            "rawBytes": 10,
            "normalizedBytes": 10,
            "partCount": 1,
            "pinned": false,
        }))
        .unwrap()
    }

    #[test]
    fn names_a_record_by_its_label_else_its_title_followed_by_the_start_time() {
        let at = muxus_core::js::parse_timestamp("2026-10-14T12:00:00.000Z")
            .unwrap()
            .with_timezone(&chrono::Local);
        let time = short_date_medium_time(&at);
        assert_eq!(
            history_compare_label(&session()),
            format!("admin@core-1 \u{b7} {time}")
        );
        let labelled = SessionLogSummary {
            label: Some("core-1 before".into()),
            ..session()
        };
        assert_eq!(
            history_compare_label(&labelled),
            format!("core-1 before \u{b7} {time}")
        );
    }

    #[test]
    fn labels_a_terminal_output_and_a_selection_apart() {
        assert_eq!(
            terminal_compare_text("core-1", "show version\n".into(), false),
            CompareText {
                label: "core-1 (terminal output)".into(),
                text: "show version\n".into(),
                remote: None,
            }
        );
        assert_eq!(
            terminal_compare_text("core-1", "Gi0/1 up".into(), true).label,
            "core-1 (selection)"
        );
    }

    #[test]
    fn refuses_texts_above_the_limit() {
        let side = CompareText {
            label: "big".into(),
            text: "x".repeat(MAX_COMPARE_CHARS),
            remote: None,
        };
        assert_eq!(comparable(side.clone()), Ok(side.clone()));
        let bigger = CompareText {
            text: format!("{}x", side.text),
            ..side
        };
        assert!(
            comparable(bigger)
                .unwrap_err()
                .contains("larger than 8 MiB")
        );
    }

    #[test]
    fn reads_local_files_up_to_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("running.cfg");
        std::fs::write(&path, "hostname r1\n").unwrap();
        let side = local_file_compare_text(&path).unwrap();
        assert_eq!(side.label, "running.cfg");
        assert_eq!(side.text, "hostname r1\n");
        let big = dir.path().join("big.log");
        std::fs::write(&big, vec![b'x'; MAX_LOCAL_FILE_BYTES as usize + 1]).unwrap();
        assert!(
            local_file_compare_text(&big)
                .unwrap_err()
                .contains("larger than 8 MiB")
        );
    }
}
