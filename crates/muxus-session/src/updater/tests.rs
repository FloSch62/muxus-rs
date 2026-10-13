//! `tests/unit/electron/updater.test.ts` and `store-updater.test.ts`, plus the release-feed
//! transport against a fake HTTP layer and a loopback server. Nothing here reaches the
//! network or installs anything.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use muxus_core::api::{
    DesktopUpdateReason, DesktopUpdateSource, DesktopUpdateState, DesktopUpdateStatus,
};
use muxus_core::platform::Platform;
use sha2::Digest as _;

use super::store::{StoreBridge, StoreCommand, StoreResult, StoreUpdater, StoreUpdaterOptions};
use super::*;

/// Let spawned work run (a current-thread runtime only runs it while the test awaits).
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

async fn advance(duration: Duration) {
    tokio::time::advance(duration).await;
    settle().await;
}

#[derive(Default)]
struct Counter(AtomicUsize);

impl Counter {
    fn bump(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// What the fake transport does on its next call.
enum Step<T> {
    Ok(T),
    Err(&'static str),
    /// Report these percentages, then succeed.
    Progress(Vec<f64>, T),
}

/// electron-updater stand-in: `1.0.0` is available unless a step says otherwise.
#[derive(Default)]
struct FakeTransport {
    checks: Counter,
    downloads: Counter,
    installs: Counter,
    check_steps: Mutex<VecDeque<Step<Option<String>>>>,
    download_steps: Mutex<VecDeque<Step<String>>>,
    install_fails: Mutex<bool>,
}

#[async_trait]
impl UpdateTransport for FakeTransport {
    async fn check(&self) -> Result<Option<String>, UpdateError> {
        self.checks.bump();
        tokio::task::yield_now().await;
        match self.check_steps.lock().unwrap().pop_front() {
            None => Ok(Some("1.0.0".into())),
            Some(Step::Ok(value)) | Some(Step::Progress(_, value)) => Ok(value),
            Some(Step::Err(message)) => Err(UpdateError(message.into())),
        }
    }

    async fn download(
        &self,
        progress: &(dyn Fn(f64) + Send + Sync),
    ) -> Result<String, UpdateError> {
        self.downloads.bump();
        tokio::task::yield_now().await;
        let step = self.download_steps.lock().unwrap().pop_front();
        match step {
            None => Ok("1.0.0".into()),
            Some(Step::Ok(version)) => Ok(version),
            Some(Step::Err(message)) => Err(UpdateError(message.into())),
            Some(Step::Progress(percents, version)) => {
                for percent in percents {
                    progress(percent);
                }
                Ok(version)
            }
        }
    }

    fn quit_and_install(&self) -> Result<(), UpdateError> {
        self.installs.bump();
        if *self.install_fails.lock().unwrap() {
            Err(UpdateError("installer failed".into()))
        } else {
            Ok(())
        }
    }
}

struct Setup {
    updater: DesktopUpdater,
    transport: Arc<FakeTransport>,
    broadcasts: Arc<Mutex<Vec<DesktopUpdateState>>>,
    prepare: Arc<Counter>,
    recover: Arc<Counter>,
}

fn setup_with(reason: Option<DesktopUpdateReason>) -> Setup {
    let transport = Arc::new(FakeTransport::default());
    let broadcasts = Arc::new(Mutex::new(Vec::new()));
    let prepare = Arc::new(Counter::default());
    let recover = Arc::new(Counter::default());
    let seen = broadcasts.clone();
    let prepared = prepare.clone();
    let recovered = recover.clone();
    let updater = DesktopUpdater::new(DesktopUpdaterOptions {
        version: "0.9.0".into(),
        reason,
        transport: Some(transport.clone()),
        broadcast: Arc::new(move |state| seen.lock().unwrap().push(state.clone())),
        prepare_install: Arc::new(move || prepared.bump()),
        recover_install: Arc::new(move || recovered.bump()),
        runtime: tokio::runtime::Handle::current(),
    });
    Setup {
        updater,
        transport,
        broadcasts,
        prepare,
        recover,
    }
}

fn setup() -> Setup {
    setup_with(None)
}

// --- `updater.test.ts` ---------------------------------------------------------------------------

#[tokio::test]
async fn notifies_without_downloading_and_requires_separate_download_and_install_actions() {
    let Setup {
        updater,
        transport,
        prepare,
        ..
    } = setup();
    let first = updater.check();
    let second = updater.check();
    assert_eq!(first.await, second.await);
    assert_eq!(transport.checks.get(), 1, "one check for both callers");
    assert_eq!(transport.downloads.get(), 0);
    let state = updater.state();
    assert_eq!(state.status, DesktopUpdateStatus::Available);
    assert_eq!(state.version.as_deref(), Some("1.0.0"));
    assert!(!updater.request_install());
    assert!(!updater.finish_install());
    assert_eq!(transport.installs.get(), 0);

    let download = updater.download();
    let again = updater.download();
    download.await;
    again.await;
    assert_eq!(transport.downloads.get(), 1);
    let state = updater.state();
    assert_eq!(state.status, DesktopUpdateStatus::Ready);
    assert_eq!(state.version.as_deref(), Some("1.0.0"));
    assert_eq!(prepare.get(), 0);
    updater.check().await;
    assert_eq!(
        transport.checks.get(),
        1,
        "a ready update is not checked again"
    );
    assert!(updater.request_install());
    assert!(!updater.request_install());
    assert_eq!(prepare.get(), 1);
    assert_eq!(
        transport.installs.get(),
        0,
        "installing waits for the app to close"
    );
    assert!(updater.finish_install());
    assert_eq!(transport.installs.get(), 1);
}

#[tokio::test]
async fn serializes_a_download_request_behind_an_in_flight_check_without_losing_consent() {
    let Setup {
        updater, transport, ..
    } = setup();
    let check = updater.check();
    let download = updater.download();
    let download_again = updater.download();
    // A check while a download is pending joins the download.
    let joined = updater.check();
    let (_, state, _, joined) = tokio::join!(check, download, download_again, joined);
    assert_eq!(transport.checks.get(), 1);
    assert_eq!(transport.downloads.get(), 1);
    assert_eq!(state.status, DesktopUpdateStatus::Ready);
    assert_eq!(joined.status, DesktopUpdateStatus::Ready);
}

#[tokio::test]
async fn recovers_from_an_installer_error_during_explicit_installation() {
    let Setup {
        updater,
        transport,
        recover,
        ..
    } = setup();
    updater.check().await;
    updater.download().await;
    assert_eq!(transport.installs.get(), 0);
    assert!(updater.request_install());
    *transport.install_fails.lock().unwrap() = true;
    updater.finish_install();
    assert_eq!(updater.state().status, DesktopUpdateStatus::Error);
    assert_eq!(recover.get(), 1);
}

#[tokio::test]
async fn recovers_from_check_and_download_failures_without_automatically_retrying_downloads() {
    let Setup {
        updater,
        transport,
        broadcasts,
        ..
    } = setup();
    transport
        .check_steps
        .lock()
        .unwrap()
        .push_back(Step::Err("offline"));
    updater.check().await;
    assert_eq!(updater.state().status, DesktopUpdateStatus::Error);
    assert_eq!(updater.state().error.as_deref(), Some(UPDATE_FAILED));
    updater.download().await;
    assert_eq!(transport.downloads.get(), 0, "nothing to download yet");
    updater.check().await;
    assert_eq!(updater.state().status, DesktopUpdateStatus::Available);
    transport
        .download_steps
        .lock()
        .unwrap()
        .push_back(Step::Err("bad checksum"));
    updater.download().await;
    let state = updater.state();
    assert_eq!(state.status, DesktopUpdateStatus::Error);
    assert_eq!(state.version.as_deref(), Some("1.0.0"));
    assert!(!updater.request_install());

    // 10.3% and 10.8% are both 10%: one broadcast.
    transport
        .download_steps
        .lock()
        .unwrap()
        .push_back(Step::Progress(vec![10.3, 10.8], "1.0.0".into()));
    let before = broadcasts.lock().unwrap().len();
    updater.download().await;
    let states: Vec<DesktopUpdateState> = broadcasts.lock().unwrap()[before..].to_vec();
    let percents: Vec<Option<f64>> = states.iter().map(|state| state.percent).collect();
    assert_eq!(percents, [Some(0.0), Some(10.0), Some(100.0)]);
    let state = updater.state();
    assert_eq!(state.status, DesktopUpdateStatus::Ready);
    assert_eq!(state.percent, Some(100.0));
}

#[tokio::test]
async fn shows_up_to_date_handles_an_unavailable_service_and_reopens_after_an_installer_error() {
    let Setup {
        updater,
        transport,
        recover,
        ..
    } = setup();
    transport
        .check_steps
        .lock()
        .unwrap()
        .push_back(Step::Ok(None));
    updater.check().await;
    assert_eq!(updater.state().status, DesktopUpdateStatus::UpToDate);
    transport
        .check_steps
        .lock()
        .unwrap()
        .push_back(Step::Err("The update service is unavailable."));
    updater.check().await;
    assert_eq!(updater.state().status, DesktopUpdateStatus::Error);
    updater.check().await;
    updater.download().await;
    updater.request_install();
    *transport.install_fails.lock().unwrap() = true;
    updater.finish_install();
    assert_eq!(recover.get(), 1);
}

#[tokio::test(start_paused = true)]
async fn checks_after_startup_and_periodically_while_automatic_checks_are_on() {
    let Setup {
        updater, transport, ..
    } = setup();
    advance(Duration::from_secs(60)).await;
    assert_eq!(transport.checks.get(), 0);
    updater.set_automatic_checks(true);
    updater.set_automatic_checks(true);
    advance(Duration::from_millis(14_999)).await;
    assert_eq!(transport.checks.get(), 0);
    advance(Duration::from_millis(1)).await;
    assert_eq!(transport.checks.get(), 1);
    advance(CHECK_INTERVAL).await;
    assert_eq!(transport.checks.get(), 2);
    assert_eq!(transport.downloads.get(), 0);
    assert_eq!(updater.state().status, DesktopUpdateStatus::Available);
    updater.stop();
    advance(CHECK_INTERVAL).await;
    assert_eq!(transport.checks.get(), 2);
}

#[tokio::test(start_paused = true)]
async fn stops_background_checks_when_notifications_are_turned_off() {
    let Setup {
        updater, transport, ..
    } = setup();
    updater.set_automatic_checks(true);
    updater.set_automatic_checks(false);
    advance(Duration::from_secs(5 * 60 * 60)).await;
    assert_eq!(transport.checks.get(), 0);
    // An explicit check from Settings still works.
    updater.check().await;
    assert_eq!(transport.checks.get(), 1);
}

#[tokio::test(start_paused = true)]
async fn does_not_schedule_checks_once_an_update_waits_for_a_restart() {
    let Setup {
        updater, transport, ..
    } = setup();
    updater.check().await;
    updater.download().await;
    updater.set_automatic_checks(true);
    assert!(!updater.automatic_checks_running());
    advance(Duration::from_secs(5 * 60 * 60)).await;
    assert_eq!(transport.checks.get(), 1);
}

#[tokio::test]
async fn leaves_development_store_and_system_package_installs_to_their_update_owner() {
    let reason = |packaged, platform, store, app_image| {
        update_disabled_reason(packaged, platform, store, app_image)
    };
    assert_eq!(reason(true, Platform::Windows, false, false), None);
    assert_eq!(
        reason(false, Platform::Windows, false, false),
        Some(DesktopUpdateReason::Development)
    );
    assert_eq!(
        reason(true, Platform::Windows, true, false),
        Some(DesktopUpdateReason::Store)
    );
    // macOS releases are universal: both Intel and Apple Silicon update in-app.
    assert_eq!(reason(true, Platform::MacOs, false, false), None);
    assert_eq!(
        reason(true, Platform::Linux, false, false),
        Some(DesktopUpdateReason::PackageManager)
    );
    assert_eq!(reason(true, Platform::Linux, false, true), None);

    let Setup {
        updater,
        transport,
        broadcasts,
        ..
    } = setup_with(Some(DesktopUpdateReason::Store));
    updater.set_automatic_checks(true);
    assert!(!updater.automatic_checks_running());
    assert_eq!(updater.check().await.status, DesktopUpdateStatus::Disabled);
    assert_eq!(
        updater.download().await.status,
        DesktopUpdateStatus::Disabled
    );
    assert_eq!(updater.state().reason, Some(DesktopUpdateReason::Store));
    assert!(broadcasts.lock().unwrap().is_empty());
    assert_eq!(transport.checks.get(), 0);
}

// --- `store-updater.test.ts` ---------------------------------------------------------------------

enum StoreStep {
    Return(StoreResult),
    Fail(&'static str),
    /// Report progress twice, then wait for the test to finish the operation.
    Hold(tokio::sync::oneshot::Receiver<StoreResult>),
}

#[derive(Default)]
struct FakeBridge {
    commands: Mutex<Vec<StoreCommand>>,
    steps: Mutex<VecDeque<StoreStep>>,
    before_install_calls: Arc<Counter>,
    broadcasts: Arc<Mutex<Vec<DesktopUpdateState>>>,
}

#[async_trait]
impl StoreBridge for FakeBridge {
    async fn run(
        &self,
        command: StoreCommand,
        progress: &(dyn Fn(u8) + Send + Sync),
    ) -> Result<StoreResult, String> {
        self.commands.lock().unwrap().push(command);
        let step = self.steps.lock().unwrap().pop_front();
        match step {
            None => Ok(StoreResult::Available),
            Some(StoreStep::Return(result)) => Ok(result),
            Some(StoreStep::Fail(message)) => Err(message.into()),
            Some(StoreStep::Hold(finish)) => {
                assert_eq!(self.before_install_calls.get(), 1);
                progress(42);
                let count = self.broadcasts.lock().unwrap().len();
                progress(42);
                assert_eq!(self.broadcasts.lock().unwrap().len(), count);
                Ok(finish.await.unwrap_or(StoreResult::Canceled))
            }
        }
    }
}

struct StoreSetup {
    updater: StoreUpdater,
    bridge: Arc<FakeBridge>,
    before_install: Arc<Counter>,
    fail_before_install: Arc<Mutex<bool>>,
}

fn store_setup() -> StoreSetup {
    let bridge = Arc::new(FakeBridge::default());
    let before_install = bridge.before_install_calls.clone();
    let fail_before_install = Arc::new(Mutex::new(false));
    let broadcasts = bridge.broadcasts.clone();
    let counted = before_install.clone();
    let failing = fail_before_install.clone();
    let updater = StoreUpdater::new(StoreUpdaterOptions {
        version: "0.9.1".into(),
        bridge: bridge.clone(),
        broadcast: Arc::new(move |state| broadcasts.lock().unwrap().push(state.clone())),
        before_install: Arc::new(move || {
            counted.bump();
            if std::mem::take(&mut *failing.lock().unwrap()) {
                Err("disk full".into())
            } else {
                Ok(())
            }
        }),
        runtime: tokio::runtime::Handle::current(),
    });
    StoreSetup {
        updater,
        bridge,
        before_install,
        fail_before_install,
    }
}

#[tokio::test]
async fn store_checks_without_downloading_deduplicates_checks_and_requires_an_available_update() {
    let StoreSetup {
        updater,
        bridge,
        before_install,
        ..
    } = store_setup();
    assert!(!updater.request_install());
    let first = updater.check();
    let second = updater.check();
    assert!(!updater.request_install());
    first.await;
    second.await;
    assert_eq!(*bridge.commands.lock().unwrap(), [StoreCommand::Check]);
    assert_eq!(before_install.get(), 0);
    assert_eq!(
        updater.state(),
        DesktopUpdateState {
            status: DesktopUpdateStatus::Available,
            current_version: "0.9.1".into(),
            source: Some(DesktopUpdateSource::Store),
            version: None,
            percent: None,
            reason: None,
            error: None,
        }
    );
}

#[tokio::test]
async fn store_persists_before_installation_broadcasts_progress_and_reports_completion() {
    let StoreSetup {
        updater,
        bridge,
        before_install,
        ..
    } = store_setup();
    updater.check().await;
    let (finish, finished) = tokio::sync::oneshot::channel();
    bridge
        .steps
        .lock()
        .unwrap()
        .push_back(StoreStep::Hold(finished));
    assert!(updater.request_install());
    assert!(!updater.request_install());
    let pending = updater.check();
    settle().await;
    assert_eq!(bridge.commands.lock().unwrap().len(), 2);
    assert_eq!(before_install.get(), 1);
    let state = updater.state();
    assert_eq!(state.status, DesktopUpdateStatus::Installing);
    assert_eq!(state.percent, Some(42.0));
    finish.send(StoreResult::Updated).unwrap();
    pending.await;
    let state = updater.state();
    assert_eq!(state.status, DesktopUpdateStatus::Updated);
    assert_eq!(state.percent, None);
    updater.check().await;
    assert_eq!(bridge.commands.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn store_leaves_cancellation_available_and_never_retries_installation_automatically() {
    let StoreSetup {
        updater, bridge, ..
    } = store_setup();
    updater.check().await;
    bridge
        .steps
        .lock()
        .unwrap()
        .push_back(StoreStep::Return(StoreResult::Canceled));
    updater.request_install();
    updater.check().await;
    assert_eq!(updater.state().status, DesktopUpdateStatus::Available);
    assert_eq!(bridge.commands.lock().unwrap().len(), 2);
    bridge
        .steps
        .lock()
        .unwrap()
        .push_back(StoreStep::Fail("offline"));
    updater.request_install();
    updater.check().await;
    let state = updater.state();
    assert_eq!(state.status, DesktopUpdateStatus::Error);
    assert!(state.error.unwrap().contains("could not complete"));
    assert!(!updater.request_install());
    bridge
        .steps
        .lock()
        .unwrap()
        .push_back(StoreStep::Return(StoreResult::UpToDate));
    updater.check().await;
    assert_eq!(updater.state().status, DesktopUpdateStatus::UpToDate);
}

#[tokio::test]
async fn store_surfaces_failed_checks_and_failed_persistence_without_installing() {
    let StoreSetup {
        updater,
        bridge,
        fail_before_install,
        ..
    } = store_setup();
    bridge
        .steps
        .lock()
        .unwrap()
        .push_back(StoreStep::Fail("unavailable"));
    updater.check().await;
    let state = updater.state();
    assert_eq!(state.status, DesktopUpdateStatus::Error);
    assert!(state.error.unwrap().contains("could not check"));
    updater.check().await;
    *fail_before_install.lock().unwrap() = true;
    updater.request_install();
    updater.check().await;
    assert_eq!(bridge.commands.lock().unwrap().len(), 2);
    assert_eq!(updater.state().status, DesktopUpdateStatus::Error);
}

#[tokio::test(start_paused = true)]
async fn store_runs_delayed_and_periodic_checks_without_opening_the_store_or_installing() {
    let StoreSetup {
        updater, bridge, ..
    } = store_setup();
    updater.set_automatic_checks(true);
    updater.set_automatic_checks(true);
    advance(Duration::from_millis(14_999)).await;
    assert!(bridge.commands.lock().unwrap().is_empty());
    advance(Duration::from_millis(1)).await;
    assert_eq!(bridge.commands.lock().unwrap().len(), 1);
    advance(CHECK_INTERVAL).await;
    assert_eq!(bridge.commands.lock().unwrap().len(), 2);
    assert!(
        bridge
            .commands
            .lock()
            .unwrap()
            .iter()
            .all(|command| *command == StoreCommand::Check)
    );
    updater.stop();
    advance(CHECK_INTERVAL).await;
    assert_eq!(bridge.commands.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn store_stops_background_checks_when_notifications_are_turned_off() {
    let StoreSetup {
        updater, bridge, ..
    } = store_setup();
    updater.set_automatic_checks(true);
    updater.set_automatic_checks(false);
    advance(Duration::from_secs(5 * 60 * 60)).await;
    assert!(bridge.commands.lock().unwrap().is_empty());
}

// --- The release-feed transport --------------------------------------------------------------------

fn sha512(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(bytes))
}

/// An in-memory web: documents and downloads by URL; every request is recorded.
#[derive(Default)]
struct FakeHttp {
    documents: Mutex<HashMap<String, Result<Vec<u8>, HttpError>>>,
    requests: Mutex<Vec<String>>,
}

impl FakeHttp {
    fn serve(&self, url: &str, body: impl Into<Vec<u8>>) {
        self.documents
            .lock()
            .unwrap()
            .insert(url.into(), Ok(body.into()));
    }

    fn answer(&self, url: &str) -> Result<Vec<u8>, HttpError> {
        self.requests.lock().unwrap().push(url.to_string());
        self.documents
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .unwrap_or(Err(HttpError::Status(404)))
    }
}

#[async_trait]
impl UpdateHttp for FakeHttp {
    async fn get(&self, url: &str, _accept: &str, _limit: usize) -> Result<Vec<u8>, HttpError> {
        self.answer(url)
    }

    async fn download(
        &self,
        url: &str,
        file: &Path,
        progress: http::Progress<'_>,
    ) -> Result<(), HttpError> {
        let body = self.answer(url)?;
        let total = body.len() as u64;
        let half = body.len() / 2;
        std::fs::write(file, &body[..half]).map_err(|error| HttpError::Io(error.to_string()))?;
        progress(half as u64, Some(total));
        std::fs::write(file, &body).map_err(|error| HttpError::Io(error.to_string()))?;
        progress(total, Some(total));
        Ok(())
    }
}

#[derive(Default)]
struct RecordingInstaller(Mutex<Vec<(PathBuf, Vec<u8>)>>);

impl Installer for RecordingInstaller {
    fn install(&self, downloaded: &Path) -> Result<(), String> {
        let bytes = std::fs::read(downloaded).map_err(|error| error.to_string())?;
        self.0
            .lock()
            .unwrap()
            .push((downloaded.to_path_buf(), bytes));
        Ok(())
    }
}

const PAYLOAD: &[u8] = b"the next Muxus, packed as an AppImage";

fn linux_feed(version: &str, payload: &[u8]) -> String {
    format!(
        "version: {version}\nfiles:\n  - url: muxus-{version}-linux-x86_64.AppImage\n    sha512: {}\n    size: {}\n    blockMapSize: 10\n  - url: muxus-{version}-linux-amd64.deb\n    sha512: abc\n    size: 3\npath: muxus-{version}-linux-x86_64.AppImage\nsha512: {}\nreleaseDate: '2026-10-12T10:00:00.000Z'\n",
        sha512(payload),
        payload.len(),
        sha512(payload),
    )
}

fn feed_transport(
    feed: ReleaseFeed,
    http: Arc<dyn UpdateHttp>,
    cache: &Path,
) -> (Arc<FeedTransport>, Arc<RecordingInstaller>) {
    let installer = Arc::new(RecordingInstaller::default());
    let transport = Arc::new(FeedTransport::new(FeedTransportOptions {
        feed,
        current_version: "1.1.0".into(),
        platform: Platform::Linux,
        arch: "x64".into(),
        cache_dir: cache.to_path_buf(),
        http,
        installer: installer.clone(),
    }));
    (transport, installer)
}

const RELEASES: &str = "https://github.com/FloSch62/muxus/releases";

#[tokio::test]
async fn finds_downloads_verifies_and_installs_the_newest_github_release() {
    let cache = tempfile::tempdir().unwrap();
    let http = Arc::new(FakeHttp::default());
    http.serve(&format!("{RELEASES}/latest"), r#"{"tag_name":"v1.2.0"}"#);
    http.serve(
        &format!("{RELEASES}/download/v1.2.0/latest-linux.yml"),
        linux_feed("1.2.0", PAYLOAD),
    );
    http.serve(
        &format!("{RELEASES}/download/v1.2.0/muxus-1.2.0-linux-x86_64.AppImage"),
        PAYLOAD,
    );
    let (transport, installer) = feed_transport(ReleaseFeed::muxus(), http.clone(), cache.path());
    assert_eq!(transport.check().await, Ok(Some("1.2.0".into())));

    let percents = Arc::new(Mutex::new(Vec::new()));
    let seen = percents.clone();
    let progress = move |percent: f64| seen.lock().unwrap().push(percent.floor());
    assert_eq!(transport.download(&progress).await, Ok("1.2.0".into()));
    assert_eq!(*percents.lock().unwrap(), [48.0, 100.0, 100.0]);
    let pending = cache.path().join("pending");
    let files: Vec<String> = std::fs::read_dir(&pending)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, ["muxus-1.2.0-linux-x86_64.AppImage"]);

    transport.quit_and_install().unwrap();
    let installs = installer.0.lock().unwrap();
    assert_eq!(installs.len(), 1);
    assert_eq!(
        installs[0].0,
        pending.join("muxus-1.2.0-linux-x86_64.AppImage")
    );
    assert_eq!(installs[0].1, PAYLOAD);
    assert!(
        !http
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|url| url.ends_with(".deb")),
        "an AppImage never downloads the .deb"
    );
}

#[tokio::test]
async fn reports_up_to_date_and_refuses_feeds_without_an_appimage() {
    let cache = tempfile::tempdir().unwrap();
    let http = Arc::new(FakeHttp::default());
    let base = "http://127.0.0.1:9/feed";
    http.serve(
        &format!("{base}/latest-linux.yml"),
        linux_feed("1.1.0", PAYLOAD),
    );
    let (transport, _) = feed_transport(
        ReleaseFeed::Generic {
            base_url: base.into(),
        },
        http.clone(),
        cache.path(),
    );
    assert_eq!(transport.check().await, Ok(None));
    assert!(
        transport.download(&|_| {}).await.is_err(),
        "nothing was found"
    );

    http.serve(
        &format!("{base}/latest-linux.yml"),
        "version: 1.3.0\nfiles:\n  - url: muxus-1.3.0-linux-amd64.deb\n    sha512: abc\n    size: 3\n",
    );
    let error = transport.check().await.unwrap_err();
    assert!(error.0.contains(".AppImage"), "{error}");

    let missing = feed_transport(
        ReleaseFeed::Generic {
            base_url: "http://127.0.0.1:9/elsewhere".into(),
        },
        http.clone(),
        cache.path(),
    )
    .0;
    assert!(
        missing
            .check()
            .await
            .unwrap_err()
            .0
            .contains("Cannot find latest-linux.yml")
    );
}

#[tokio::test]
async fn throws_away_a_download_that_does_not_match_its_checksum() {
    let cache = tempfile::tempdir().unwrap();
    let http = Arc::new(FakeHttp::default());
    let base = "http://127.0.0.1:9/feed";
    http.serve(
        &format!("{base}/latest-linux.yml"),
        linux_feed("1.2.0", PAYLOAD),
    );
    http.serve(
        &format!("{base}/muxus-1.2.0-linux-x86_64.AppImage"),
        b"tampered!".to_vec(),
    );
    let (transport, installer) = feed_transport(
        ReleaseFeed::Generic {
            base_url: base.into(),
        },
        http,
        cache.path(),
    );
    transport.check().await.unwrap();
    let error = transport.download(&|_| {}).await.unwrap_err();
    assert!(error.0.contains("mismatch"), "{error}");
    assert_eq!(
        std::fs::read_dir(cache.path().join("pending"))
            .unwrap()
            .count(),
        0
    );
    assert!(transport.quit_and_install().is_err());
    assert!(installer.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reuses_a_verified_download_from_an_earlier_run() {
    let cache = tempfile::tempdir().unwrap();
    let http = Arc::new(FakeHttp::default());
    let base = "http://127.0.0.1:9/feed";
    http.serve(
        &format!("{base}/latest-linux.yml"),
        linux_feed("1.2.0", PAYLOAD),
    );
    let pending = cache.path().join("pending");
    std::fs::create_dir_all(&pending).unwrap();
    std::fs::write(pending.join("muxus-1.2.0-linux-x86_64.AppImage"), PAYLOAD).unwrap();
    std::fs::write(pending.join("muxus-1.1.5-linux-x86_64.AppImage"), b"old").unwrap();
    let (transport, _) = feed_transport(
        ReleaseFeed::Generic {
            base_url: base.into(),
        },
        http.clone(),
        cache.path(),
    );
    transport.check().await.unwrap();
    assert_eq!(transport.download(&|_| {}).await, Ok("1.2.0".into()));
    assert!(
        !http
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|url| url.ends_with(".AppImage"))
    );
}

#[tokio::test]
async fn drives_the_whole_update_through_the_state_machine() {
    let cache = tempfile::tempdir().unwrap();
    let http = Arc::new(FakeHttp::default());
    let base = "http://127.0.0.1:9/feed";
    http.serve(
        &format!("{base}/latest-linux.yml"),
        linux_feed("1.2.0", PAYLOAD),
    );
    http.serve(
        &format!("{base}/muxus-1.2.0-linux-x86_64.AppImage"),
        PAYLOAD,
    );
    let (transport, installer) = feed_transport(
        ReleaseFeed::Generic {
            base_url: base.into(),
        },
        http,
        cache.path(),
    );
    let quit = Arc::new(Counter::default());
    let quitting = quit.clone();
    let updater = DesktopUpdater::new(DesktopUpdaterOptions {
        version: "1.1.0".into(),
        reason: None,
        transport: Some(transport),
        broadcast: Arc::new(|_| {}),
        prepare_install: Arc::new(move || quitting.bump()),
        recover_install: Arc::new(|| panic!("the install must not fail")),
        runtime: tokio::runtime::Handle::current(),
    });
    let checked = updater.check().await;
    assert_eq!(checked.status, DesktopUpdateStatus::Available);
    assert_eq!(checked.version.as_deref(), Some("1.2.0"));
    assert_eq!(checked.current_version, "1.1.0");
    assert_eq!(updater.download().await.status, DesktopUpdateStatus::Ready);
    assert!(updater.request_install());
    assert_eq!(quit.get(), 1);
    assert!(updater.finish_install());
    assert_eq!(installer.0.lock().unwrap().len(), 1);
}

/// Real HTTP against loopback: the feed, a redirect, the download.
#[tokio::test]
async fn downloads_over_real_http_from_a_loopback_mirror() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let feed = linux_feed("1.2.0", PAYLOAD);
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = requests.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buffer = vec![0u8; 8192];
            let read = socket.read(&mut buffer).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let path = request
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            seen.lock().unwrap().push(request);
            let (status, headers, body): (&str, String, Vec<u8>) = match path.as_str() {
                "/feed/latest-linux.yml" => ("200 OK", String::new(), feed.clone().into_bytes()),
                "/feed/muxus-1.2.0-linux-x86_64.AppImage" => (
                    "302 Found",
                    "location: /assets/payload\r\n".into(),
                    Vec::new(),
                ),
                "/assets/payload" => ("200 OK", String::new(), PAYLOAD.to_vec()),
                _ => ("404 Not Found", String::new(), Vec::new()),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&body).await;
        }
    });
    let cache = tempfile::tempdir().unwrap();
    let (transport, installer) = feed_transport(
        ReleaseFeed::Generic {
            base_url: format!("http://{address}/feed"),
        },
        Arc::new(HyperHttp::new("1.1.0")),
        cache.path(),
    );
    assert_eq!(transport.check().await, Ok(Some("1.2.0".into())));
    assert_eq!(transport.download(&|_| {}).await, Ok("1.2.0".into()));
    transport.quit_and_install().unwrap();
    assert_eq!(installer.0.lock().unwrap()[0].1, PAYLOAD);
    let requests = requests.lock().unwrap();
    assert!(
        requests[0]
            .to_lowercase()
            .contains("user-agent: muxus/1.1.0")
    );
    assert_eq!(requests.len(), 3);
}

#[test]
fn keeps_pending_downloads_in_the_user_cache() {
    let env: HashMap<String, String> =
        [("XDG_CACHE_HOME".to_string(), "/scratch/cache".to_string())].into();
    assert_eq!(
        default_cache_dir(Platform::Linux, &env, Path::new("/home/me")),
        Path::new("/scratch/cache/muxus-rs-updater")
    );
    assert_eq!(
        default_cache_dir(Platform::Linux, &HashMap::new(), Path::new("/home/me")),
        Path::new("/home/me/.cache/muxus-rs-updater")
    );
    assert_eq!(
        default_cache_dir(Platform::MacOs, &HashMap::new(), Path::new("/Users/me")),
        Path::new("/Users/me/Library/Caches/muxus-rs-updater")
    );
}

#[tokio::test]
async fn gives_each_installation_its_updater() {
    let base =
        |env: HashMap<String, String>, packaged: bool, platform: Platform| ProcessUpdaterOptions {
            version: "1.1.0".into(),
            packaged,
            platform,
            env,
            metadata: DistributionMetadata::default(),
            windows_store: false,
            feed: ReleaseFeed::Generic {
                base_url: "http://127.0.0.1:9/feed".into(),
            },
            cache_dir: PathBuf::from("/nonexistent/cache"),
            http: Arc::new(FakeHttp::default()),
            broadcast: Arc::new(|_| {}),
            prepare_install: Arc::new(|| {}),
            recover_install: Arc::new(|| {}),
            before_store_install: Arc::new(|| Ok(())),
            store_owner_window: Arc::new(|| Ok("0".into())),
            runtime: tokio::runtime::Handle::current(),
        };
    let deb = AppUpdater::for_process(base(HashMap::new(), true, Platform::Linux));
    assert_eq!(deb.state().status, DesktopUpdateStatus::Disabled);
    assert_eq!(
        deb.state().reason,
        Some(DesktopUpdateReason::PackageManager)
    );
    let development = AppUpdater::for_process(base(HashMap::new(), false, Platform::Linux));
    assert_eq!(
        development.state().reason,
        Some(DesktopUpdateReason::Development)
    );
    let app_image = AppUpdater::for_process(base(
        [("APPIMAGE".to_string(), "/apps/Muxus.AppImage".to_string())].into(),
        true,
        Platform::Linux,
    ));
    assert_eq!(app_image.state().status, DesktopUpdateStatus::Idle);
    assert!(!app_image.is_store());
    let mut store = base(HashMap::new(), true, Platform::Windows);
    store.metadata.update_mode = Some("store".into());
    let store = AppUpdater::for_process(store);
    assert!(store.is_store());
    assert_eq!(store.state().source, Some(DesktopUpdateSource::Store));
    assert!(store.download().is_none());
}
