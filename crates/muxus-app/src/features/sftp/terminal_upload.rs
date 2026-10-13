//! `client/src/terminal-upload.ts`: files dropped onto an SSH terminal upload
//! over the session's own connection. Progress shows in the file browser when
//! one is on screen for the connection, otherwise in a corner of the window.
//! One upload per connection at a time.
//!
//! [`start_terminal_upload`] holds the rules; [`TerminalUploadEnv`] is the
//! store, the notifications and the backend it works with, so the rules run
//! the same in the app and in tests.

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use futures_lite::FutureExt;
use muxus_session::ApiError;
use muxus_ssh::sftp::CancelSignal;
use muxus_ssh::sftp::upload::{DropPayload, UploadProgress, UploadResult, upload_summary};
use muxus_ui::theme::Status;

use super::services::{TerminalUpload, error_details};
use super::transfer::{TransferDirection, TransferPhase, TransferProgressState};

pub type LocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// What the upload needs from the app.
pub trait TerminalUploadEnv {
    /// The connection's upload on screen, if any.
    fn current(&self, conn_id: &str) -> Option<TerminalUpload>;
    fn show(&self, upload: TerminalUpload);
    fn clear(&self, conn_id: &str, id: u64);
    /// Clear the upload after `delay`, unless a later one replaced it.
    fn clear_after(&self, conn_id: &str, id: u64, delay: Duration);
    fn notify(&self, status: Status, message: String, details: Option<String>);
    /// Re-read file browser listings of the connection.
    fn refresh(&self, conn_id: &str);
    /// Run the upload (`uploadToRemote`), reporting progress on the UI side.
    fn upload(
        &self,
        conn_id: &str,
        directory: &str,
        payload: DropPayload,
        cancel: CancelSignal,
        on_progress: Box<dyn FnMut(&UploadProgress)>,
    ) -> LocalFuture<Result<UploadResult, ApiError>>;
}

/// `TerminalUploadRequest`.
pub struct TerminalUploadRequest {
    pub conn_id: String,
    pub host: Option<String>,
    /// Reads the drop and settles where it goes; the upload shows as
    /// preparing meanwhile. Fails with a message.
    pub prepare: LocalFuture<Result<(String, DropPayload), ApiError>>,
    /// Delay before a finished upload's progress disappears.
    pub settle: Duration,
}

/// The default delay before a finished bar goes away.
pub const SETTLE: Duration = Duration::from_millis(1_200);

thread_local! {
    static NEXT_UPLOAD_ID: Cell<u64> = const { Cell::new(1) };
}

/// The cancel code `Backend::sftp_upload_drop` fails with (`AbortError`).
pub const UPLOAD_CANCELLED: &str = "upload-cancelled";

fn cancelled() -> ApiError {
    ApiError::with_code(499, "Upload cancelled", UPLOAD_CANCELLED)
}

fn is_cancelled(error: &ApiError) -> bool {
    error.code.as_deref() == Some(UPLOAD_CANCELLED)
}

/// Upload files dropped onto an SSH terminal. Resolves once the upload is
/// over (its bar may linger for [`TerminalUploadRequest::settle`]).
pub async fn start_terminal_upload<E: TerminalUploadEnv + 'static>(
    request: TerminalUploadRequest,
    env: Rc<E>,
) {
    let TerminalUploadRequest {
        conn_id,
        host,
        prepare,
        settle,
    } = request;
    // A finished upload only lingers on screen; the next one replaces it.
    if env
        .current(&conn_id)
        .is_some_and(|running| running.transfer.phase != TransferPhase::Complete)
    {
        env.notify(
            Status::Warning,
            "Wait for the current upload to finish.".into(),
            None,
        );
        return;
    }
    let id = NEXT_UPLOAD_ID.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    });
    let cancel = CancelSignal::new();
    let upload = TerminalUpload {
        id,
        conn_id: conn_id.clone(),
        host,
        transfer: TransferProgressState {
            direction: TransferDirection::Upload,
            name: "files".into(),
            loaded: 0,
            total: None,
            bytes_per_second: 0.,
            phase: TransferPhase::Preparing,
            file_index: None,
            file_count: None,
        },
        cancel: cancel.clone(),
    };
    env.show(upload.clone());

    let run = async {
        let stop = cancel.clone();
        let prepared = async { Some(prepare.await) }
            .or(async move {
                stop.cancelled().await;
                None
            })
            .await;
        let (directory, payload) = match prepared {
            None => return Err(cancelled()),
            Some(result) => result?,
        };
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        if payload.is_empty() {
            env.clear(&conn_id, id);
            env.notify(
                Status::Info,
                "Nothing was uploaded: the drop held no files or folders.".into(),
                None,
            );
            return Ok(None);
        }
        let progress_env = env.clone();
        let progress_conn = conn_id.clone();
        let template = upload.clone();
        let on_progress = Box::new(move |update: &UploadProgress| {
            // A cancel stays on screen until the request has actually stopped.
            let current = progress_env.current(&progress_conn);
            if current
                .as_ref()
                .is_some_and(|current| current.transfer.phase == TransferPhase::Cancelling)
            {
                return;
            }
            let mut shown = current
                .filter(|current| current.id == template.id)
                .unwrap_or_else(|| template.clone());
            shown.transfer = TransferProgressState::from_upload(update);
            progress_env.show(shown);
        });
        let result = env
            .upload(&conn_id, &directory, payload, cancel.clone(), on_progress)
            .await?;
        Ok(Some((result, directory)))
    };

    match run.await {
        Ok(None) => {}
        Ok(Some((result, directory))) => {
            env.refresh(&conn_id);
            env.notify(
                Status::Success,
                upload_summary(result, Some(&directory)),
                None,
            );
            env.clear_after(&conn_id, id, settle);
        }
        Err(error) => {
            env.clear(&conn_id, id);
            if is_cancelled(&error) {
                env.notify(Status::Info, "Upload cancelled".into(), None);
                env.refresh(&conn_id);
            } else {
                env.notify(Status::Error, error.message.clone(), error_details(&error));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use futures_lite::future::block_on;
    use muxus_ssh::sftp::upload::DroppedFile;

    use super::*;

    #[derive(Default)]
    struct Env {
        uploads: RefCell<HashMap<String, TerminalUpload>>,
        seen: RefCell<Vec<TransferPhase>>,
        notes: RefCell<Vec<(Status, String, Option<String>)>>,
        refreshed: RefCell<Vec<String>>,
        calls: RefCell<Vec<String>>,
        /// `clear_after` runs at once for delays under this.
        immediate: Cell<bool>,
        fail: RefCell<Option<ApiError>>,
    }

    impl TerminalUploadEnv for Env {
        fn current(&self, conn_id: &str) -> Option<TerminalUpload> {
            self.uploads.borrow().get(conn_id).cloned()
        }
        fn show(&self, upload: TerminalUpload) {
            let mut seen = self.seen.borrow_mut();
            if seen.last() != Some(&upload.transfer.phase) {
                seen.push(upload.transfer.phase);
            }
            self.uploads
                .borrow_mut()
                .insert(upload.conn_id.clone(), upload);
        }
        fn clear(&self, conn_id: &str, id: u64) {
            let mut uploads = self.uploads.borrow_mut();
            if uploads.get(conn_id).is_some_and(|upload| upload.id == id) {
                uploads.remove(conn_id);
            }
        }
        fn clear_after(&self, conn_id: &str, id: u64, delay: Duration) {
            if delay.is_zero() || self.immediate.get() {
                self.clear(conn_id, id);
            }
        }
        fn notify(&self, status: Status, message: String, details: Option<String>) {
            self.notes.borrow_mut().push((status, message, details));
        }
        fn refresh(&self, conn_id: &str) {
            self.refreshed.borrow_mut().push(conn_id.into());
        }
        fn upload(
            &self,
            _conn_id: &str,
            directory: &str,
            payload: DropPayload,
            _cancel: CancelSignal,
            mut on_progress: Box<dyn FnMut(&UploadProgress)>,
        ) -> LocalFuture<Result<UploadResult, ApiError>> {
            if let Some(error) = self.fail.borrow().clone() {
                return Box::pin(async move { Err(error) });
            }
            let mut report = |name: &str, phase, loaded| {
                on_progress(&UploadProgress {
                    name: name.into(),
                    loaded,
                    total: Some(1),
                    bytes_per_second: 10.,
                    phase,
                    file_index: 1,
                    file_count: payload.files.len(),
                })
            };
            for file in &payload.files {
                report(&file.relative_path, TransferPhase::Preparing, 0);
                report(&file.relative_path, TransferPhase::Transferring, 1);
                report(&file.relative_path, TransferPhase::Finalizing, 1);
                self.calls
                    .borrow_mut()
                    .push(format!("upload {directory}/{}", file.relative_path));
            }
            report("done", TransferPhase::Complete, 1);
            let uploaded = payload.files.len();
            Box::pin(async move {
                Ok(UploadResult {
                    uploaded,
                    directories: 0,
                })
            })
        }
    }

    fn payload(names: &[&str]) -> DropPayload {
        DropPayload {
            files: names
                .iter()
                .map(|name| DroppedFile {
                    source: PathBuf::from(format!("/tmp/{name}")),
                    relative_path: name.to_string(),
                    size: 1,
                })
                .collect(),
            directories: Vec::new(),
        }
    }

    fn request(directory: &str, names: &[&str], settle: Duration) -> TerminalUploadRequest {
        let prepared = (directory.to_string(), payload(names));
        TerminalUploadRequest {
            conn_id: "conn-1".into(),
            host: Some("web-01".into()),
            prepare: Box::pin(async move { Ok(prepared) }),
            settle,
        }
    }

    #[test]
    fn shows_progress_for_the_connection_then_reports_where_the_files_went() {
        let env = Rc::new(Env::default());
        let settle = Duration::from_secs(60);
        block_on(start_terminal_upload(
            request("/home/admin", &["a.txt"], settle),
            env.clone(),
        ));
        assert_eq!(*env.calls.borrow(), ["upload /home/admin/a.txt"]);
        assert_eq!(
            *env.seen.borrow(),
            [
                TransferPhase::Preparing,
                TransferPhase::Transferring,
                TransferPhase::Finalizing,
                TransferPhase::Complete
            ]
        );
        let shown = env.current("conn-1").unwrap();
        assert_eq!(shown.host.as_deref(), Some("web-01"));
        assert_eq!(shown.transfer.phase, TransferPhase::Complete);
        assert_eq!(*env.refreshed.borrow(), ["conn-1"]);
        assert_eq!(
            env.notes.borrow().last().unwrap(),
            &(
                Status::Success,
                "Uploaded 1 file to /home/admin".into(),
                None
            )
        );
        let env = Rc::new(Env::default());
        block_on(start_terminal_upload(
            request("/home/admin", &["a.txt"], Duration::ZERO),
            env.clone(),
        ));
        assert!(env.uploads.borrow().is_empty());
    }

    #[test]
    fn runs_one_upload_per_connection_at_a_time() {
        let env = Rc::new(Env::default());
        let (release, released) = async_channel::bounded::<()>(1);
        let first = TerminalUploadRequest {
            conn_id: "conn-1".into(),
            host: None,
            prepare: Box::pin(async move {
                let _ = released.recv().await;
                Ok(("/srv".to_string(), payload(&["a.txt"])))
            }),
            settle: Duration::ZERO,
        };
        let mut first = Box::pin(start_terminal_upload(first, env.clone()));
        // Run the first upload until it waits on its drop.
        assert!(block_on(futures_lite::future::poll_once(&mut first)).is_none());
        block_on(start_terminal_upload(
            request("/srv", &["b.txt"], Duration::ZERO),
            env.clone(),
        ));
        assert_eq!(
            env.notes.borrow().last().unwrap(),
            &(
                Status::Warning,
                "Wait for the current upload to finish.".into(),
                None
            )
        );
        release.try_send(()).unwrap();
        block_on(first);
        assert_eq!(
            env.notes.borrow().last().unwrap(),
            &(Status::Success, "Uploaded 1 file to /srv".into(), None)
        );
    }

    #[test]
    fn does_not_wait_for_a_finished_upload_that_is_still_on_screen() {
        let env = Rc::new(Env::default());
        let settle = Duration::from_secs(60);
        block_on(start_terminal_upload(
            request("/srv", &["a.txt"], settle),
            env.clone(),
        ));
        assert_eq!(
            env.current("conn-1").unwrap().transfer.phase,
            TransferPhase::Complete
        );
        block_on(start_terminal_upload(
            request("/srv", &["b.txt"], settle),
            env.clone(),
        ));
        assert_eq!(
            *env.calls.borrow(),
            ["upload /srv/a.txt", "upload /srv/b.txt"]
        );
        assert!(
            env.notes
                .borrow()
                .iter()
                .all(|(status, ..)| *status != Status::Warning)
        );
    }

    #[test]
    fn can_be_cancelled_while_the_drop_is_still_being_read() {
        let env = Rc::new(Env::default());
        let never = TerminalUploadRequest {
            conn_id: "conn-1".into(),
            host: None,
            prepare: Box::pin(futures_lite::future::pending()),
            settle: Duration::ZERO,
        };
        let mut upload = Box::pin(start_terminal_upload(never, env.clone()));
        assert!(block_on(futures_lite::future::poll_once(&mut upload)).is_none());
        // The bar's Cancel.
        {
            let mut uploads = env.uploads.borrow_mut();
            let current = uploads.get_mut("conn-1").unwrap();
            current.transfer.phase = TransferPhase::Cancelling;
            current.cancel.cancel();
        }
        assert_eq!(
            env.current("conn-1").unwrap().transfer.phase,
            TransferPhase::Cancelling
        );
        block_on(upload);
        assert_eq!(
            env.notes.borrow().last().unwrap(),
            &(Status::Info, "Upload cancelled".into(), None)
        );
        assert!(env.uploads.borrow().is_empty());
    }

    #[test]
    fn reports_a_failed_upload_as_an_error_and_clears_its_progress() {
        let env = Rc::new(Env::default());
        *env.fail.borrow_mut() = Some(ApiError::new(404, "connection not found"));
        block_on(start_terminal_upload(
            request("/srv", &["a.txt"], Duration::ZERO),
            env.clone(),
        ));
        let notes = env.notes.borrow();
        let (status, message, details) = notes.last().unwrap();
        assert_eq!(*status, Status::Error);
        assert_eq!(message, "connection not found");
        assert!(details.as_deref().unwrap().contains("HTTP 404"));
        assert!(env.uploads.borrow().is_empty());
    }

    #[test]
    fn an_empty_drop_uploads_nothing() {
        let env = Rc::new(Env::default());
        block_on(start_terminal_upload(
            request("/srv", &[], Duration::ZERO),
            env.clone(),
        ));
        assert_eq!(
            env.notes.borrow().last().unwrap().1,
            "Nothing was uploaded: the drop held no files or folders."
        );
        assert!(env.uploads.borrow().is_empty());
    }
}
