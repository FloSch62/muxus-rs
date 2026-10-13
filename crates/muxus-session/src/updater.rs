//! In-app updates for the desktop build: `electron/src/updater.ts` (the state machine and its
//! schedule), the parts of electron-updater it relied on (release feed, download with
//! verification, install handoff; see [`transport`]), and the Microsoft Store variant
//! (`store-updater.ts`, `store-updates.ts`; see [`store`]).
//!
//! # Using it
//!
//! [`AppUpdater::for_process`] picks the updater the running build gets, as `createDesktopUpdater`
//! did: Store packages talk to the Store; AppImages, Windows installs and macOS update from the
//! release feed; development builds and Linux package installs get a `disabled` state with its
//! reason (their About page keeps the plain release check, and a `.deb` is updated by the
//! package manager). Every state change goes to the `broadcast` callback; the app pushes it to
//! its windows.
//!
//! Nothing downloads or installs on its own: background checks run only while
//! [`AppUpdater::set_automatic_checks`] is on (the "Notify me" preference), downloading and
//! installing are separate explicit actions, and installing waits for the app to close.
//!
//! The operations return executor-agnostic futures; the work itself runs on the tokio runtime
//! handed in (the backend's, see [`crate::Backend::runtime`]).

pub mod feed;
pub mod http;
pub mod install;
pub mod store;
pub mod transport;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt as _;
use futures_util::future::Shared;
use muxus_core::api::{DesktopUpdateReason, DesktopUpdateState, DesktopUpdateStatus};
use muxus_core::platform::Platform;
use tokio::task::JoinHandle;

pub use feed::{UpdateFileInfo, UpdateInfo};
pub use http::{HttpError, HyperHttp, UpdateHttp};
pub use install::{Installer, RELAUNCH_AFTER_PID_VAR};
pub use store::{DistributionMetadata, StoreUpdater};
pub use transport::{
    FeedTransport, FeedTransportOptions, RELEASE_OWNER, RELEASE_REPO, ReleaseFeed, UpdateError,
    UpdateTransport,
};

/// Let the first window and its sessions settle before network or disk work.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(15);
pub const CHECK_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

/// What the user is told when an update step fails; the details go to the log.
pub const UPDATE_FAILED: &str =
    "The update could not be completed. Check your connection and try again.";

/// Receives every new state.
pub type Broadcast = Arc<dyn Fn(&DesktopUpdateState) + Send + Sync>;

pub(crate) fn log_update_error(message: &str, error: &str) {
    tracing::error!(err = error, "{message}");
}

/// Why the in-app updater is off for an installation (`updateDisabledReason`).
pub fn update_disabled_reason(
    packaged: bool,
    platform: Platform,
    store: bool,
    app_image: bool,
) -> Option<DesktopUpdateReason> {
    if !packaged {
        return Some(DesktopUpdateReason::Development);
    }
    if store {
        return Some(DesktopUpdateReason::Store);
    }
    if platform == Platform::Linux && !app_image {
        return Some(DesktopUpdateReason::PackageManager);
    }
    None
}

/// Background checks, only while the update notifications are on (`UpdateSchedule`): the first
/// one [`FIRST_CHECK_DELAY`] after starting, then every [`CHECK_INTERVAL`].
pub struct UpdateSchedule {
    runtime: tokio::runtime::Handle,
    check: Arc<dyn Fn() + Send + Sync>,
    timers: Mutex<Option<(JoinHandle<()>, JoinHandle<()>)>>,
}

impl UpdateSchedule {
    pub fn new(runtime: tokio::runtime::Handle, check: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            runtime,
            check,
            timers: Mutex::new(None),
        }
    }

    pub fn running(&self) -> bool {
        self.timers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    pub fn start(&self) {
        let mut timers = self.timers.lock().unwrap_or_else(|p| p.into_inner());
        if timers.is_some() {
            return;
        }
        let first = self.check.clone();
        // Deadlines are fixed now, not when the timer tasks first run.
        let first_at = tokio::time::Instant::now() + FIRST_CHECK_DELAY;
        let initial = self.runtime.spawn(async move {
            tokio::time::sleep_until(first_at).await;
            first();
        });
        let every = self.check.clone();
        // Like `setInterval`: counted from now, not from the first check.
        let start = tokio::time::Instant::now() + CHECK_INTERVAL;
        let interval = self.runtime.spawn(async move {
            let mut ticks = tokio::time::interval_at(start, CHECK_INTERVAL);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                every();
            }
        });
        *timers = Some((initial, interval));
    }

    pub fn stop(&self) {
        if let Some((initial, interval)) =
            self.timers.lock().unwrap_or_else(|p| p.into_inner()).take()
        {
            initial.abort();
            interval.abort();
        }
    }
}

impl Drop for UpdateSchedule {
    fn drop(&mut self) {
        self.stop();
    }
}

type SharedState = Shared<Pin<Box<dyn Future<Output = DesktopUpdateState> + Send>>>;

fn ready(state: DesktopUpdateState) -> SharedState {
    (Box::pin(async move { state }) as Pin<Box<dyn Future<Output = DesktopUpdateState> + Send>>)
        .shared()
}

/// Everything a [`DesktopUpdater`] needs.
pub struct DesktopUpdaterOptions {
    /// This build's version.
    pub version: String,
    /// Set when this installation does not update itself; the updater then stays `disabled`.
    pub reason: Option<DesktopUpdateReason>,
    pub transport: Option<Arc<dyn UpdateTransport>>,
    pub broadcast: Broadcast,
    /// Quit the app for an accepted install; [`DesktopUpdater::finish_install`] runs once the
    /// backend has closed.
    pub prepare_install: Arc<dyn Fn() + Send + Sync>,
    /// Start this version again when the installer failed after the app began to quit.
    pub recover_install: Arc<dyn Fn() + Send + Sync>,
    pub runtime: tokio::runtime::Handle,
}

struct Inner {
    version: String,
    transport: Option<Arc<dyn UpdateTransport>>,
    broadcast: Broadcast,
    prepare_install: Arc<dyn Fn() + Send + Sync>,
    recover_install: Arc<dyn Fn() + Send + Sync>,
    runtime: tokio::runtime::Handle,
    state: Mutex<DesktopUpdateState>,
    pending_check: Mutex<Option<SharedState>>,
    pending_download: Mutex<Option<SharedState>>,
}

/// Owns one update operation for the entire app, independent of window lifetimes
/// (`DesktopUpdater`). Cheap to clone; every clone is the same updater.
#[derive(Clone)]
pub struct DesktopUpdater {
    inner: Arc<Inner>,
    schedule: Arc<UpdateSchedule>,
}

impl DesktopUpdater {
    pub fn new(options: DesktopUpdaterOptions) -> Self {
        let status = if options.reason.is_some() {
            DesktopUpdateStatus::Disabled
        } else {
            DesktopUpdateStatus::Idle
        };
        let transport = if options.reason.is_some() {
            None
        } else {
            options.transport
        };
        let inner = Arc::new(Inner {
            state: Mutex::new(DesktopUpdateState {
                status,
                current_version: options.version.clone(),
                source: None,
                version: None,
                percent: None,
                reason: options.reason,
                error: None,
            }),
            version: options.version,
            transport,
            broadcast: options.broadcast,
            prepare_install: options.prepare_install,
            recover_install: options.recover_install,
            runtime: options.runtime.clone(),
            pending_check: Mutex::new(None),
            pending_download: Mutex::new(None),
        });
        let weak = Arc::downgrade(&inner);
        let schedule = Arc::new(UpdateSchedule::new(
            options.runtime,
            Arc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    drop(Inner::check(&inner));
                }
            }),
        ));
        Self { inner, schedule }
    }

    pub fn state(&self) -> DesktopUpdateState {
        self.inner.state()
    }

    /// Background checks follow the "Notify me" preference; none once an update waits for a
    /// restart.
    pub fn set_automatic_checks(&self, enabled: bool) {
        if !enabled {
            self.schedule.stop();
        } else if !matches!(
            self.inner.state().status,
            DesktopUpdateStatus::Disabled
                | DesktopUpdateStatus::Ready
                | DesktopUpdateStatus::Installing
        ) {
            self.schedule.start();
        }
    }

    /// Whether background checks are scheduled.
    pub fn automatic_checks_running(&self) -> bool {
        self.schedule.running()
    }

    pub fn stop(&self) {
        self.schedule.stop();
    }

    /// Ask the feed (shared with a check or download already running).
    pub fn check(&self) -> impl Future<Output = DesktopUpdateState> + Send + 'static {
        Inner::check(&self.inner)
    }

    /// Download the update a check found (shared with a download already running). Waits
    /// for a check in flight, so a click during a check is not lost.
    pub fn download(&self) -> impl Future<Output = DesktopUpdateState> + Send + 'static {
        Inner::download(&self.inner)
    }

    /// The user accepted installing a downloaded update: quit, then
    /// [`finish_install`](Self::finish_install).
    pub fn request_install(&self) -> bool {
        let state = self.inner.state();
        if state.status != DesktopUpdateStatus::Ready {
            return false;
        }
        self.inner.set(DesktopUpdateState {
            status: DesktopUpdateStatus::Installing,
            ..state
        });
        self.stop();
        (self.inner.prepare_install)();
        true
    }

    /// Hand over to the installer. Call only after the backend and saved state have closed.
    pub fn finish_install(&self) -> bool {
        if self.inner.state().status != DesktopUpdateStatus::Installing {
            return false;
        }
        let result = match &self.inner.transport {
            Some(transport) => transport.quit_and_install(),
            None => Err(UpdateError("the update service is unavailable".into())),
        };
        if let Err(error) = result {
            self.inner.fail(&error.0);
        }
        true
    }
}

impl Inner {
    fn state(&self) -> DesktopUpdateState {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn set(&self, state: DesktopUpdateState) {
        let state = DesktopUpdateState {
            current_version: self.version.clone(),
            ..state
        };
        *self.state.lock().unwrap_or_else(|p| p.into_inner()) = state.clone();
        (self.broadcast)(&state);
    }

    fn fresh(&self, status: DesktopUpdateStatus) -> DesktopUpdateState {
        DesktopUpdateState {
            status,
            current_version: self.version.clone(),
            source: None,
            version: None,
            percent: None,
            reason: None,
            error: None,
        }
    }

    fn fail(&self, error: &str) {
        let state = self.state();
        let installing = state.status == DesktopUpdateStatus::Installing;
        log_update_error("desktop update failed", error);
        self.set(DesktopUpdateState {
            version: state.version,
            error: Some(UPDATE_FAILED.into()),
            ..self.fresh(DesktopUpdateStatus::Error)
        });
        // The backend has already shut down; reopen the installed version if its installer
        // fails so the user is not left without Muxus.
        if installing {
            (self.recover_install)();
        }
    }

    /// Run `work` on the runtime; the returned future resolves to the state afterwards.
    fn spawn(
        self: &Arc<Self>,
        work: impl Future<Output = ()> + Send + 'static,
        slot: fn(&Inner) -> &Mutex<Option<SharedState>>,
    ) -> SharedState {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let fallback = self.fresh(DesktopUpdateStatus::Error);
        let shared: SharedState = (Box::pin(async move { rx.await.unwrap_or(fallback) })
            as Pin<Box<dyn Future<Output = DesktopUpdateState> + Send>>)
            .shared();
        let inner = self.clone();
        self.runtime.spawn(async move {
            work.await;
            *slot(&inner).lock().unwrap_or_else(|p| p.into_inner()) = None;
            let _ = tx.send(inner.state());
        });
        shared
    }

    fn check(self: &Arc<Self>) -> SharedState {
        if let Some(download) = self
            .pending_download
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            return download;
        }
        let mut pending = self.pending_check.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(check) = pending.as_ref() {
            return check.clone();
        }
        let state = self.state();
        if matches!(
            state.status,
            DesktopUpdateStatus::Disabled
                | DesktopUpdateStatus::Ready
                | DesktopUpdateStatus::Installing
        ) {
            return ready(state);
        }
        self.set(self.fresh(DesktopUpdateStatus::Checking));
        let inner = self.clone();
        let started = self.spawn(async move { inner.run_check().await }, |inner| {
            &inner.pending_check
        });
        *pending = Some(started.clone());
        started
    }

    async fn run_check(self: &Arc<Self>) {
        let result = match &self.transport {
            Some(transport) => transport.check().await,
            None => Err(UpdateError("The update service is unavailable.".into())),
        };
        match result {
            Ok(Some(version)) => self.set(DesktopUpdateState {
                version: Some(version),
                ..self.fresh(DesktopUpdateStatus::Available)
            }),
            Ok(None) => self.set(self.fresh(DesktopUpdateStatus::UpToDate)),
            Err(error) => self.fail(&error.0),
        }
    }

    fn download(self: &Arc<Self>) -> SharedState {
        let mut pending = self
            .pending_download
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(download) = pending.as_ref() {
            return download.clone();
        }
        let check = self
            .pending_check
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let inner = self.clone();
        let started = self.spawn(async move { inner.run_download(check).await }, |inner| {
            &inner.pending_download
        });
        *pending = Some(started.clone());
        started
    }

    async fn run_download(self: &Arc<Self>, check: Option<SharedState>) {
        // A click can arrive while a check is finishing in another window.
        if let Some(check) = check {
            check.await;
        }
        let state = self.state();
        let Some(version) = state.version.clone() else {
            return;
        };
        if !matches!(
            state.status,
            DesktopUpdateStatus::Available | DesktopUpdateStatus::Error
        ) {
            return;
        }
        self.set(DesktopUpdateState {
            version: Some(version),
            percent: Some(0.0),
            ..self.fresh(DesktopUpdateStatus::Downloading)
        });
        let Some(transport) = self.transport.clone() else {
            self.fail("The update service is unavailable.");
            return;
        };
        let progress_owner = self.clone();
        let progress = move |percent: f64| {
            let state = progress_owner.state();
            if state.status != DesktopUpdateStatus::Downloading {
                return;
            }
            let percent = percent.floor().clamp(0.0, 100.0);
            if state.percent != Some(percent) {
                progress_owner.set(DesktopUpdateState {
                    percent: Some(percent),
                    ..state
                });
            }
        };
        match transport.download(&progress).await {
            Ok(version) => {
                if self.state().status == DesktopUpdateStatus::Downloading {
                    self.set(DesktopUpdateState {
                        version: Some(version),
                        percent: Some(100.0),
                        ..self.fresh(DesktopUpdateStatus::Ready)
                    });
                }
            }
            Err(error) => self.fail(&error.0),
        }
    }
}

/// The updater the running build gets: in-app updates, or the Store's.
#[derive(Clone)]
pub enum AppUpdater {
    Desktop(DesktopUpdater),
    Store(StoreUpdater),
}

/// Process facts and callbacks for [`AppUpdater::for_process`].
pub struct ProcessUpdaterOptions {
    pub version: String,
    /// `false` for development builds (`!app.isPackaged`).
    pub packaged: bool,
    pub platform: Platform,
    /// This process's environment (`APPIMAGE` decides AppImage updates on Linux).
    pub env: HashMap<String, String>,
    pub metadata: DistributionMetadata,
    /// Running from a Store package (`process.windowsStore`).
    pub windows_store: bool,
    pub feed: ReleaseFeed,
    /// Pending downloads live in `<cache_dir>/pending`; see [`default_cache_dir`].
    pub cache_dir: PathBuf,
    pub http: Arc<dyn UpdateHttp>,
    pub broadcast: Broadcast,
    pub prepare_install: Arc<dyn Fn() + Send + Sync>,
    pub recover_install: Arc<dyn Fn() + Send + Sync>,
    /// Store packages: persist state before handing control to the Store.
    pub before_store_install: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    /// Store packages: the owner window's native handle for the Store's dialogs.
    pub store_owner_window: Arc<dyn Fn() -> Result<String, String> + Send + Sync>,
    pub runtime: tokio::runtime::Handle,
}

/// Where pending downloads are kept: the per-user cache folder (`muxus-rs-updater`).
pub fn default_cache_dir(
    platform: Platform,
    env: &HashMap<String, String>,
    home: &std::path::Path,
) -> PathBuf {
    let var = |key: &str| {
        env.get(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let base = match platform {
        Platform::Windows => {
            var("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local"))
        }
        Platform::MacOs => home.join("Library").join("Caches"),
        _ => var("XDG_CACHE_HOME")
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".cache")),
    };
    base.join("muxus-rs-updater")
}

impl AppUpdater {
    /// `createDesktopUpdater`.
    pub fn for_process(options: ProcessUpdaterOptions) -> Self {
        let store = store::is_store_distribution(&options.metadata, options.windows_store);
        let app_image = options
            .env
            .get("APPIMAGE")
            .filter(|path| !path.is_empty())
            .cloned();
        let reason = update_disabled_reason(
            options.packaged,
            options.platform,
            store,
            app_image.is_some(),
        );
        if reason == Some(DesktopUpdateReason::Store) && options.platform == Platform::Windows {
            let executable = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
                .unwrap_or_default()
                .join("store-updater")
                .join("muxus-store-updater.exe");
            return AppUpdater::Store(StoreUpdater::new(store::StoreUpdaterOptions {
                version: options.version,
                bridge: Arc::new(store::HelperStoreBridge {
                    executable,
                    owner_window: options.store_owner_window,
                }),
                broadcast: options.broadcast,
                before_install: options.before_store_install,
                runtime: options.runtime,
            }));
        }
        let launcher = || Box::new(install::DetachedLauncher) as Box<dyn install::Launcher>;
        let installer: Option<Arc<dyn Installer>> = match options.platform {
            Platform::Windows => Some(Arc::new(install::NsisInstaller {
                env: options.env.clone(),
                launcher: launcher(),
            })),
            Platform::MacOs => std::env::current_exe()
                .ok()
                .and_then(|exe| install::app_bundle_of(&exe))
                .map(|bundle| {
                    Arc::new(install::MacZipInstaller {
                        bundle,
                        env: options.env.clone(),
                        launcher: launcher(),
                    }) as Arc<dyn Installer>
                }),
            _ => app_image.map(|app_image| {
                Arc::new(install::AppImageInstaller {
                    app_image: PathBuf::from(app_image),
                    env: options.env.clone(),
                    launcher: launcher(),
                }) as Arc<dyn Installer>
            }),
        };
        let transport = installer.map(|installer| {
            Arc::new(FeedTransport::new(FeedTransportOptions {
                feed: options.feed,
                current_version: options.version.clone(),
                platform: options.platform,
                arch: feed::node_arch().to_string(),
                cache_dir: options.cache_dir,
                http: options.http,
                installer,
            })) as Arc<dyn UpdateTransport>
        });
        AppUpdater::Desktop(DesktopUpdater::new(DesktopUpdaterOptions {
            version: options.version,
            reason,
            transport,
            broadcast: options.broadcast,
            prepare_install: options.prepare_install,
            recover_install: options.recover_install,
            runtime: options.runtime,
        }))
    }

    pub fn state(&self) -> DesktopUpdateState {
        match self {
            AppUpdater::Desktop(updater) => updater.state(),
            AppUpdater::Store(updater) => updater.state(),
        }
    }

    pub fn is_store(&self) -> bool {
        matches!(self, AppUpdater::Store(_))
    }

    pub fn set_automatic_checks(&self, enabled: bool) {
        match self {
            AppUpdater::Desktop(updater) => updater.set_automatic_checks(enabled),
            AppUpdater::Store(updater) => updater.set_automatic_checks(enabled),
        }
    }

    pub fn stop(&self) {
        match self {
            AppUpdater::Desktop(updater) => updater.stop(),
            AppUpdater::Store(updater) => updater.stop(),
        }
    }

    pub fn check(&self) -> Pin<Box<dyn Future<Output = DesktopUpdateState> + Send>> {
        match self {
            AppUpdater::Desktop(updater) => Box::pin(updater.check()),
            AppUpdater::Store(updater) => Box::pin(updater.check()),
        }
    }

    /// In-app downloads only; a Store package has none (`None`).
    pub fn download(&self) -> Option<Pin<Box<dyn Future<Output = DesktopUpdateState> + Send>>> {
        match self {
            AppUpdater::Desktop(updater) => Some(Box::pin(updater.download())),
            AppUpdater::Store(_) => None,
        }
    }

    /// Restart into a downloaded update, or start a Store update.
    pub fn request_install(&self) -> bool {
        match self {
            AppUpdater::Desktop(updater) => updater.request_install(),
            AppUpdater::Store(updater) => updater.request_install(),
        }
    }

    /// After the backend closed: hand over to the installer (in-app updates only).
    pub fn finish_install(&self) -> bool {
        match self {
            AppUpdater::Desktop(updater) => updater.finish_install(),
            AppUpdater::Store(_) => false,
        }
    }
}
