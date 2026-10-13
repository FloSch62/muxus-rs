//! Remote files opened with a local program (`state/local-copies.ts`) and the
//! upload of their saves (`client/src/local-copy-sync.ts`).
//!
//! When a program saves a file Muxus opened for it, ask whether to upload the
//! save (or upload straight away once the user said so for that file), then
//! upload it with progress, refusing to silently replace a remote file that
//! changed in the meantime.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use muxus_core::api::{LocalCopyChange, SftpUploadResponse};
use muxus_session::ApiError;
use muxus_ssh::sftp::service::SFTP_FILE_CHANGED;
use muxus_ssh::sftp::{CancelSignal, UploadStep};
use muxus_ui::theme::Status;

use super::dialogs::{ConfirmAnswer, ConfirmOptions};
use super::services::error_details;
use super::terminal_upload::{LocalFuture, UPLOAD_CANCELLED};
use super::transfer::{TransferDirection, TransferPhase, TransferProgressState};

/// A remote file opened with a local program, whose saves can be uploaded back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedLocalCopy {
    pub id: String,
    pub conn_id: String,
    pub remote_path: String,
    pub name: String,
    /// The tab's title, to say where an upload goes.
    pub host: Option<String>,
    /// The remote file's modification time the copy started from or was last uploaded as.
    pub remote_mtime_ms: Option<i64>,
    /// Upload later saves without asking.
    pub auto_upload: bool,
}

/// The upload of a save, as its bar shows it (`LocalCopyUpload`).
#[derive(Clone, Debug)]
pub struct LocalCopyUpload {
    /// Distinguishes attempts so a finished one clears only itself.
    pub attempt: u64,
    pub copy_id: String,
    pub host: Option<String>,
    pub transfer: TransferProgressState,
    pub cancel: CancelSignal,
}

/// `useLocalCopiesStore`, plus the queue that serialises saves.
#[derive(Default)]
pub struct LocalCopies {
    pub copies: HashMap<String, TrackedLocalCopy>,
    pub upload: Option<LocalCopyUpload>,
    /// Where each copy lives on disk.
    pub files: HashMap<String, PathBuf>,
    queue: SyncQueue,
}

impl LocalCopies {
    pub fn track(&mut self, copy: TrackedLocalCopy, file: PathBuf) {
        self.files.insert(copy.id.clone(), file);
        self.copies.insert(copy.id.clone(), copy);
    }

    /// The bar's Cancel.
    pub fn cancel_upload(&mut self) {
        if let Some(upload) = &mut self.upload {
            upload.transfer.phase = TransferPhase::Cancelling;
            upload.transfer.bytes_per_second = 0.;
            upload.cancel.cancel();
        }
    }

    pub fn queue(&mut self) -> &mut SyncQueue {
        &mut self.queue
    }
}

/// Saves reported while the same copy is still waiting for its turn, or
/// waiting for the user's answer, fold into the one already queued.
#[derive(Default)]
pub struct SyncQueue {
    queued: HashSet<String>,
    pending: VecDeque<LocalCopyChange>,
    running: bool,
    next_attempt: u64,
}

/// What the sync needs from the app.
pub trait LocalCopySyncEnv {
    fn copy(&self, id: &str) -> Option<TrackedLocalCopy>;
    fn set_auto_upload(&self, id: &str);
    fn set_remote_mtime(&self, id: &str, mtime_ms: Option<i64>);
    fn upload_state(&self) -> Option<LocalCopyUpload>;
    fn set_upload(&self, upload: Option<LocalCopyUpload>);
    fn with_queue<T>(&self, f: impl FnOnce(&mut SyncQueue) -> T) -> T;
    fn confirm(&self, options: ConfirmOptions) -> LocalFuture<ConfirmAnswer>;
    /// Upload the local file over `remote_path`, replacing it when its
    /// modification time is still `expected_mtime_ms`.
    fn upload(
        &self,
        conn_id: &str,
        remote_path: &str,
        expected_mtime_ms: Option<i64>,
        source: PathBuf,
        cancel: CancelSignal,
        on_step: Box<dyn FnMut(UploadStep)>,
    ) -> LocalFuture<Result<SftpUploadResponse, ApiError>>;
    /// Re-read the listing of the connection the file lives on.
    fn refresh(&self, conn_id: &str);
    fn notify(&self, status: Status, message: String, details: Option<String>);
    /// Hide a finished attempt's bar after a moment.
    fn hide_after(&self, attempt: u64, delay: Duration);
}

/// Delay before a finished upload's bar disappears.
pub const SETTLE: Duration = Duration::from_millis(1_200);

/// Handle a program saving new contents into a tracked copy. Resolves once
/// every save queued meanwhile is dealt with.
pub async fn local_copy_changed<E: LocalCopySyncEnv + 'static>(
    change: LocalCopyChange,
    env: Rc<E>,
) {
    if env.copy(&change.id).is_none() {
        return;
    }
    let first = env.with_queue(|queue| {
        if !queue.queued.insert(change.id.clone()) {
            return None;
        }
        queue.pending.push_back(change);
        Some(!std::mem::replace(&mut queue.running, true))
    });
    // Already queued, or another call is working through the queue.
    if first != Some(true) {
        return;
    }
    while let Some(change) = env.with_queue(|queue| queue.pending.pop_front()) {
        sync_local_copy(change, &env).await;
    }
    env.with_queue(|queue| queue.running = false);
}

fn release<E: LocalCopySyncEnv>(env: &Rc<E>, id: &str) {
    env.with_queue(|queue| queue.queued.remove(id));
}

async fn sync_local_copy<E: LocalCopySyncEnv + 'static>(change: LocalCopyChange, env: &Rc<E>) {
    let LocalCopyChange { id, file } = change;
    let Some(copy) = env.copy(&id) else {
        release(env, &id);
        return;
    };
    if !copy.auto_upload {
        let on_host = copy
            .host
            .as_ref()
            .map_or_else(String::new, |host| format!(" on {host}"));
        let answer = env
            .confirm(ConfirmOptions {
                title: format!("Upload changes to {}?", copy.name),
                description: Some(format!(
                    "{} was saved in another program. Uploading replaces {}{on_host}.",
                    copy.name, copy.remote_path
                )),
                confirm_label: "Upload".into(),
                cancel_label: Some("Don't upload".into()),
                destructive: false,
                checkbox: Some("Upload later changes to this file without asking".into()),
            })
            .await;
        if !answer.confirmed {
            release(env, &id);
            return;
        }
        if answer.checked {
            env.set_auto_upload(&id);
        }
    }
    // From here on, a new save is newer than what gets uploaded and asks again.
    release(env, &id);
    let copy = env.copy(&id).unwrap_or(copy);
    upload_local_copy(copy, PathBuf::from(file), env).await;
}

fn is_remote_changed(error: &ApiError) -> bool {
    error.status == 409 && error.code.as_deref() == Some(SFTP_FILE_CHANGED)
}

fn is_connection_gone(error: &ApiError) -> bool {
    error.status == 404 && error.message == "connection not found"
}

fn is_abort(error: &ApiError) -> bool {
    error.code.as_deref() == Some(UPLOAD_CANCELLED)
}

/// The attempt's bar: shown while visible, and cleared only while it is still
/// this attempt's.
struct Bar<E: LocalCopySyncEnv> {
    env: Rc<E>,
    progress: RefCell<LocalCopyUpload>,
    visible: std::cell::Cell<bool>,
}

impl<E: LocalCopySyncEnv> Bar<E> {
    fn show(&self, patch: impl FnOnce(&mut TransferProgressState)) {
        // The bar's Cancel marks the shared state; keep that.
        if let Some(current) = self.env.upload_state()
            && current.attempt == self.progress.borrow().attempt
            && current.transfer.phase == TransferPhase::Cancelling
        {
            self.progress.borrow_mut().transfer.phase = TransferPhase::Cancelling;
            return;
        }
        patch(&mut self.progress.borrow_mut().transfer);
        if self.visible.get() {
            self.env.set_upload(Some(self.progress.borrow().clone()));
        }
    }

    fn hide(&self) {
        self.visible.set(false);
        if self
            .env
            .upload_state()
            .is_some_and(|upload| upload.attempt == self.progress.borrow().attempt)
        {
            self.env.set_upload(None);
        }
    }
}

async fn upload_local_copy<E: LocalCopySyncEnv + 'static>(
    copy: TrackedLocalCopy,
    file: PathBuf,
    env: &Rc<E>,
) {
    let attempt = env.with_queue(|queue| {
        queue.next_attempt += 1;
        queue.next_attempt
    });
    let cancel = CancelSignal::new();
    let where_to = copy
        .host
        .as_ref()
        .map_or_else(String::new, |host| format!(" to {host}"));
    let bar = Rc::new(Bar {
        env: env.clone(),
        progress: RefCell::new(LocalCopyUpload {
            attempt,
            copy_id: copy.id.clone(),
            host: copy.host.clone(),
            transfer: TransferProgressState {
                direction: TransferDirection::Upload,
                name: copy.name.clone(),
                loaded: 0,
                total: None,
                bytes_per_second: 0.,
                phase: TransferPhase::Preparing,
                file_index: None,
                file_count: None,
            },
            cancel: cancel.clone(),
        }),
        visible: std::cell::Cell::new(true),
    });
    env.set_upload(Some(bar.progress.borrow().clone()));

    let result: Result<Option<SftpUploadResponse>, ApiError> = async {
        let size = read_copy_size(&file)?;
        bar.show(|transfer| transfer.total = Some(size));
        let send = |expected: Option<i64>| {
            let progress = bar.clone();
            env.upload(
                &copy.conn_id,
                &copy.remote_path,
                expected,
                file.clone(),
                cancel.clone(),
                Box::new(move |step| match step {
                    UploadStep::Progress(bytes) => progress.show(|transfer| {
                        transfer.loaded = bytes.loaded;
                        if bytes.total.is_some() {
                            transfer.total = bytes.total;
                        }
                        transfer.bytes_per_second = bytes.bytes_per_second;
                        transfer.phase = TransferPhase::Transferring;
                    }),
                    UploadStep::Finalizing => progress.show(|transfer| {
                        transfer.loaded = size;
                        transfer.phase = TransferPhase::Finalizing;
                    }),
                }),
            )
        };
        match send(copy.remote_mtime_ms).await {
            Ok(response) => Ok(Some(response)),
            Err(error) if is_remote_changed(&error) => {
                bar.hide();
                let on_host = copy
                    .host
                    .as_ref()
                    .map_or_else(String::new, |host| format!(" on {host}"));
                let replace = env
                    .confirm(ConfirmOptions {
                        title: format!("{} also changed on the server", copy.name),
                        description: Some(format!(
                            "{}{on_host} was changed or removed after you opened it. Uploading replaces that version with yours.",
                            copy.remote_path
                        )),
                        confirm_label: "Replace".into(),
                        cancel_label: Some("Keep the server version".into()),
                        destructive: true,
                        checkbox: None,
                    })
                    .await;
                if !replace.confirmed {
                    env.notify(
                        Status::Info,
                        format!(
                            "{} was not uploaded. Your copy is at {}",
                            copy.name,
                            file.display()
                        ),
                        None,
                    );
                    return Ok(None);
                }
                bar.visible.set(true);
                bar.show(|transfer| {
                    transfer.loaded = 0;
                    transfer.bytes_per_second = 0.;
                    transfer.phase = TransferPhase::Preparing;
                });
                send(None).await.map(Some)
            }
            Err(error) => Err(error),
        }
    }
    .await;

    match result {
        Ok(None) => {}
        Ok(Some(response)) => {
            env.set_remote_mtime(&copy.id, response.mtime_ms);
            env.refresh(&copy.conn_id);
            let size = bar.progress.borrow().transfer.total.unwrap_or(0);
            bar.show(|transfer| {
                transfer.loaded = size;
                transfer.total = Some(size);
                transfer.bytes_per_second = 0.;
                transfer.phase = TransferPhase::Complete;
            });
            env.notify(
                Status::Success,
                format!("Uploaded {}{where_to}", copy.name),
                None,
            );
            env.hide_after(attempt, SETTLE);
        }
        Err(error) => {
            bar.hide();
            if is_abort(&error) {
                env.notify(
                    Status::Info,
                    format!(
                        "Cancelled the upload of {}. Your copy is at {}",
                        copy.name,
                        file.display()
                    ),
                    None,
                );
            } else if is_connection_gone(&error) {
                env.notify(
                    Status::Error,
                    format!(
                        "Could not upload {}: the connection{} is closed. Reconnect and upload your copy from {}",
                        copy.name,
                        where_to,
                        file.display()
                    ),
                    None,
                );
            } else {
                env.notify(
                    Status::Error,
                    format!("Could not upload {}: {}", copy.name, error.message),
                    error_details(&error),
                );
            }
        }
    }
}

fn read_copy_size(file: &Path) -> Result<u64, ApiError> {
    std::fs::metadata(file)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
        .ok_or_else(|| ApiError::new(0, "The local copy can no longer be read."))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use futures_lite::future::block_on;

    use super::*;

    struct Env {
        store: RefCell<LocalCopies>,
        answers: RefCell<VecDeque<ConfirmAnswer>>,
        asked: RefCell<Vec<ConfirmOptions>>,
        uploads: RefCell<Vec<(String, Option<i64>, String)>>,
        refreshed: RefCell<Vec<String>>,
        notes: RefCell<Vec<(Status, String)>>,
        fail_first: RefCell<Option<ApiError>>,
        cancel_during_upload: Cell<bool>,
        /// Holds the next question open until it receives.
        gate: RefCell<Option<async_channel::Receiver<()>>>,
    }

    impl Env {
        fn new(answers: &[(bool, bool)]) -> Rc<Self> {
            Rc::new(Self {
                store: RefCell::default(),
                answers: RefCell::new(
                    answers
                        .iter()
                        .map(|(confirmed, checked)| ConfirmAnswer {
                            confirmed: *confirmed,
                            checked: *checked,
                        })
                        .collect(),
                ),
                asked: RefCell::default(),
                uploads: RefCell::default(),
                refreshed: RefCell::default(),
                notes: RefCell::default(),
                fail_first: RefCell::default(),
                cancel_during_upload: Cell::new(false),
                gate: RefCell::default(),
            })
        }
    }

    impl LocalCopySyncEnv for Env {
        fn copy(&self, id: &str) -> Option<TrackedLocalCopy> {
            self.store.borrow().copies.get(id).cloned()
        }
        fn set_auto_upload(&self, id: &str) {
            if let Some(copy) = self.store.borrow_mut().copies.get_mut(id) {
                copy.auto_upload = true;
            }
        }
        fn set_remote_mtime(&self, id: &str, mtime_ms: Option<i64>) {
            if let Some(copy) = self.store.borrow_mut().copies.get_mut(id) {
                copy.remote_mtime_ms = mtime_ms;
            }
        }
        fn upload_state(&self) -> Option<LocalCopyUpload> {
            self.store.borrow().upload.clone()
        }
        fn set_upload(&self, upload: Option<LocalCopyUpload>) {
            self.store.borrow_mut().upload = upload;
        }
        fn with_queue<T>(&self, f: impl FnOnce(&mut SyncQueue) -> T) -> T {
            f(self.store.borrow_mut().queue())
        }
        fn confirm(&self, options: ConfirmOptions) -> LocalFuture<ConfirmAnswer> {
            self.asked.borrow_mut().push(options);
            let answer = self.answers.borrow_mut().pop_front().unwrap_or_default();
            let gate = self.gate.borrow_mut().take();
            Box::pin(async move {
                if let Some(gate) = gate {
                    let _ = gate.recv().await;
                }
                answer
            })
        }
        fn upload(
            &self,
            _conn_id: &str,
            remote_path: &str,
            expected_mtime_ms: Option<i64>,
            source: PathBuf,
            cancel: CancelSignal,
            mut on_step: Box<dyn FnMut(UploadStep)>,
        ) -> LocalFuture<Result<SftpUploadResponse, ApiError>> {
            if let Some(error) = self.fail_first.borrow_mut().take() {
                return Box::pin(async move { Err(error) });
            }
            if self.cancel_during_upload.get() {
                self.store.borrow_mut().cancel_upload();
                assert!(cancel.is_cancelled());
                return Box::pin(async move {
                    Err(ApiError::with_code(
                        499,
                        "Upload cancelled",
                        UPLOAD_CANCELLED,
                    ))
                });
            }
            let text = std::fs::read_to_string(&source).unwrap();
            let size = text.len() as u64;
            self.uploads
                .borrow_mut()
                .push((remote_path.to_string(), expected_mtime_ms, text));
            on_step(UploadStep::Progress(muxus_ssh::sftp::ByteProgress {
                loaded: size,
                total: Some(size),
                bytes_per_second: 100.,
            }));
            on_step(UploadStep::Finalizing);
            Box::pin(async move {
                Ok(SftpUploadResponse {
                    ok: true,
                    mtime_ms: Some(1_700_000_100_000),
                })
            })
        }
        fn refresh(&self, conn_id: &str) {
            self.refreshed.borrow_mut().push(conn_id.into());
        }
        fn notify(&self, status: Status, message: String, _details: Option<String>) {
            self.notes.borrow_mut().push((status, message));
        }
        fn hide_after(&self, attempt: u64, _delay: Duration) {
            let mut store = self.store.borrow_mut();
            if store
                .upload
                .as_ref()
                .is_some_and(|upload| upload.attempt == attempt)
            {
                store.upload = None;
            }
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        file: String,
    }

    fn track(env: &Env, id: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plan.drawio");
        std::fs::write(&file, "<mxfile>edited</mxfile>").unwrap();
        env.store.borrow_mut().track(
            TrackedLocalCopy {
                id: id.into(),
                conn_id: "conn-1".into(),
                remote_path: "/home/deploy/plan.drawio".into(),
                name: "plan.drawio".into(),
                host: Some("web-01".into()),
                remote_mtime_ms: Some(1_700_000_000_000),
                auto_upload: false,
            },
            file.clone(),
        );
        Fixture {
            file: file.display().to_string(),
            _dir: dir,
        }
    }

    fn change(id: &str, fixture: &Fixture) -> LocalCopyChange {
        LocalCopyChange {
            id: id.into(),
            file: fixture.file.clone(),
        }
    }

    #[test]
    fn asks_then_uploads_only_over_the_version_that_was_downloaded() {
        let env = Env::new(&[(true, false)]);
        let fixture = track(&env, "copy-1");
        block_on(local_copy_changed(change("copy-1", &fixture), env.clone()));
        let asked = env.asked.borrow();
        assert_eq!(asked[0].title, "Upload changes to plan.drawio?");
        assert_eq!(
            asked[0].description.as_deref(),
            Some(
                "plan.drawio was saved in another program. Uploading replaces /home/deploy/plan.drawio on web-01."
            )
        );
        assert_eq!(asked[0].confirm_label, "Upload");
        assert_eq!(
            *env.uploads.borrow(),
            [(
                "/home/deploy/plan.drawio".to_string(),
                Some(1_700_000_000_000),
                "<mxfile>edited</mxfile>".to_string()
            )]
        );
        assert_eq!(
            env.copy("copy-1").unwrap().remote_mtime_ms,
            Some(1_700_000_100_000)
        );
        assert_eq!(*env.refreshed.borrow(), ["conn-1"]);
        assert_eq!(
            env.notes.borrow().last().unwrap(),
            &(Status::Success, "Uploaded plan.drawio to web-01".into())
        );
        assert!(env.store.borrow().upload.is_none());
    }

    #[test]
    fn uploads_nothing_when_the_user_declines() {
        let env = Env::new(&[(false, false)]);
        let fixture = track(&env, "copy-1");
        block_on(local_copy_changed(change("copy-1", &fixture), env.clone()));
        assert!(env.uploads.borrow().is_empty());
    }

    #[test]
    fn stops_asking_once_the_user_ticks_the_box() {
        let env = Env::new(&[(true, true)]);
        let fixture = track(&env, "copy-1");
        block_on(local_copy_changed(change("copy-1", &fixture), env.clone()));
        block_on(local_copy_changed(change("copy-1", &fixture), env.clone()));
        assert_eq!(env.asked.borrow().len(), 1);
        let uploads = env.uploads.borrow();
        assert_eq!(uploads.len(), 2);
        assert_eq!(uploads[1].1, Some(1_700_000_100_000));
    }

    #[test]
    fn folds_saves_made_while_the_question_is_open_into_it() {
        let env = Env::new(&[(true, false)]);
        let fixture = track(&env, "copy-1");
        let (open, gate) = async_channel::bounded::<()>(1);
        *env.gate.borrow_mut() = Some(gate);
        let mut first = Box::pin(local_copy_changed(change("copy-1", &fixture), env.clone()));
        // The first save waits on its question while two more arrive.
        assert!(block_on(futures_lite::future::poll_once(&mut first)).is_none());
        block_on(local_copy_changed(change("copy-1", &fixture), env.clone()));
        block_on(local_copy_changed(change("copy-1", &fixture), env.clone()));
        open.try_send(()).unwrap();
        block_on(first);
        assert_eq!(env.asked.borrow().len(), 1);
        assert_eq!(env.uploads.borrow().len(), 1);
    }

    #[test]
    fn asks_before_replacing_a_server_file_that_changed_too() {
        let env = Env::new(&[(true, false), (true, false)]);
        let fixture = track(&env, "copy-1");
        *env.fail_first.borrow_mut() = Some(ApiError::with_code(
            409,
            "the remote file changed since it was downloaded",
            SFTP_FILE_CHANGED,
        ));
        block_on(local_copy_changed(change("copy-1", &fixture), env.clone()));
        let asked = env.asked.borrow();
        assert_eq!(asked[1].title, "plan.drawio also changed on the server");
        assert_eq!(asked[1].confirm_label, "Replace");
        assert!(asked[1].destructive);
        let uploads = env.uploads.borrow();
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].1, None);
        assert_eq!(
            env.notes.borrow().last().unwrap(),
            &(Status::Success, "Uploaded plan.drawio to web-01".into())
        );
    }

    #[test]
    fn keeps_the_servers_version_when_the_user_says_so() {
        let env = Env::new(&[(true, false), (false, false)]);
        let fixture = track(&env, "copy-1");
        *env.fail_first.borrow_mut() = Some(ApiError::with_code(409, "changed", SFTP_FILE_CHANGED));
        block_on(local_copy_changed(change("copy-1", &fixture), env.clone()));
        assert!(env.uploads.borrow().is_empty());
        assert_eq!(
            env.notes.borrow().last().unwrap(),
            &(
                Status::Info,
                format!(
                    "plan.drawio was not uploaded. Your copy is at {}",
                    fixture.file
                )
            )
        );
        assert!(env.store.borrow().upload.is_none());
    }

    #[test]
    fn points_at_the_copy_when_the_connection_has_closed() {
        let env = Env::new(&[(true, false)]);
        let fixture = track(&env, "copy-1");
        *env.fail_first.borrow_mut() = Some(ApiError::new(404, "connection not found"));
        block_on(local_copy_changed(change("copy-1", &fixture), env.clone()));
        assert_eq!(
            env.notes.borrow().last().unwrap(),
            &(
                Status::Error,
                format!(
                    "Could not upload plan.drawio: the connection to web-01 is closed. Reconnect and upload your copy from {}",
                    fixture.file
                )
            )
        );
    }

    #[test]
    fn can_be_cancelled_while_it_uploads() {
        let env = Env::new(&[(true, false)]);
        let fixture = track(&env, "copy-1");
        env.cancel_during_upload.set(true);
        block_on(local_copy_changed(change("copy-1", &fixture), env.clone()));
        assert_eq!(
            env.notes.borrow().last().unwrap(),
            &(
                Status::Info,
                format!(
                    "Cancelled the upload of plan.drawio. Your copy is at {}",
                    fixture.file
                )
            )
        );
        assert!(env.store.borrow().upload.is_none());
    }

    #[test]
    fn ignores_copies_it_was_never_told_about() {
        let env = Env::new(&[(true, false)]);
        block_on(local_copy_changed(
            LocalCopyChange {
                id: "unknown".into(),
                file: "/tmp/x".into(),
            },
            env.clone(),
        ));
        assert!(env.asked.borrow().is_empty());
    }
}
