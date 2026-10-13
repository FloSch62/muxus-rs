//! Microsoft Store installations (`electron/src/store-updater.ts`, `store-updates.ts`,
//! `store-bridge.ts`): the Store owns download, consent and installation, so the app only
//! asks a helper program to check or to start the Store's own update, and never downloads
//! anything itself. Only Store packages on Windows build a [`StoreUpdater`]; everywhere else
//! the [`check_store_update`] routing simply answers `None`.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::FutureExt as _;
use futures_util::future::Shared;
use muxus_core::api::{
    DesktopUpdateSource, DesktopUpdateState, DesktopUpdateStatus, UpdateCheckResult,
};

use super::{Broadcast, UpdateSchedule, log_update_error};

/// What a package says about how it is distributed (`package.json` fields in Electron; build
/// environment variables for the Rust build, see [`DistributionMetadata::built_in`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DistributionMetadata {
    /// `store` for Microsoft Store packages.
    pub update_mode: Option<String>,
    pub store_product_id: Option<String>,
}

impl DistributionMetadata {
    /// The metadata compiled into this build (`MUXUS_UPDATE_MODE`, `MUXUS_STORE_PRODUCT_ID`).
    pub fn built_in() -> Self {
        Self {
            update_mode: option_env!("MUXUS_UPDATE_MODE").map(str::to_string),
            store_product_id: option_env!("MUXUS_STORE_PRODUCT_ID").map(str::to_string),
        }
    }
}

pub fn is_store_distribution(metadata: &DistributionMetadata, windows_store: bool) -> bool {
    windows_store || metadata.update_mode.as_deref() == Some("store")
}

/// The Muxus Store page, or the Store's update list when the product is unknown.
pub fn store_page_url(metadata: &DistributionMetadata) -> String {
    match metadata.store_product_id.as_deref() {
        Some(id)
            if id.len() == 12
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()) =>
        {
            format!("ms-windows-store://pdp/?ProductId={id}")
        }
        _ => "ms-windows-store://downloadsandupdates".into(),
    }
}

/// The About page's update check for Store packages: never the release manifest. An explicit
/// check opens the Store page. `None` for every other installation.
pub async fn check_store_update<F, Fut>(
    metadata: &DistributionMetadata,
    windows_store: bool,
    current_version: &str,
    force: bool,
    open_external: F,
) -> Option<UpdateCheckResult>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    if !is_store_distribution(metadata, windows_store) {
        return None;
    }
    let not_available = |reason: &str| UpdateCheckResult::NotAvailable {
        current_version: current_version.to_string(),
        latest_version: None,
        reason: Some(reason.to_string()),
    };
    if force && open_external(store_page_url(metadata)).await.is_err() {
        return Some(not_available("store-open-failed"));
    }
    Some(not_available("store"))
}

/// Whether this process runs from a Store package (`process.windowsStore`).
pub fn running_from_windows_store() -> bool {
    cfg!(windows)
        && std::env::current_exe()
            .map(|path| {
                path.to_string_lossy()
                    .to_lowercase()
                    .contains("\\windowsapps\\")
            })
            .unwrap_or(false)
}

/// The two things the Store helper does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreCommand {
    Check,
    Install,
}

impl StoreCommand {
    fn as_str(self) -> &'static str {
        match self {
            StoreCommand::Check => "check",
            StoreCommand::Install => "install",
        }
    }
}

/// How a Store operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreResult {
    Available,
    UpToDate,
    Updated,
    Canceled,
}

/// Talks to the Microsoft Store.
#[async_trait]
pub trait StoreBridge: Send + Sync {
    async fn run(
        &self,
        command: StoreCommand,
        progress: &(dyn Fn(u8) + Send + Sync),
    ) -> Result<StoreResult, String>;
}

type SharedState = Shared<Pin<Box<dyn Future<Output = DesktopUpdateState> + Send>>>;

/// Everything a [`StoreUpdater`] needs.
pub struct StoreUpdaterOptions {
    pub version: String,
    pub bridge: Arc<dyn StoreBridge>,
    pub broadcast: Broadcast,
    /// Persist state before Windows may close the app to install (may fail).
    pub before_install: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    pub runtime: tokio::runtime::Handle,
}

struct Inner {
    version: String,
    bridge: Arc<dyn StoreBridge>,
    broadcast: Broadcast,
    before_install: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    runtime: tokio::runtime::Handle,
    state: Mutex<DesktopUpdateState>,
    pending: Mutex<Option<SharedState>>,
}

/// The Store updater (`StoreUpdater`). Cheap to clone.
#[derive(Clone)]
pub struct StoreUpdater {
    inner: Arc<Inner>,
    schedule: Arc<UpdateSchedule>,
}

impl StoreUpdater {
    pub fn new(options: StoreUpdaterOptions) -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::new(DesktopUpdateState {
                status: DesktopUpdateStatus::Idle,
                current_version: options.version.clone(),
                source: Some(DesktopUpdateSource::Store),
                version: None,
                percent: None,
                reason: None,
                error: None,
            }),
            version: options.version,
            bridge: options.bridge,
            broadcast: options.broadcast,
            before_install: options.before_install,
            runtime: options.runtime.clone(),
            pending: Mutex::new(None),
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

    pub fn set_automatic_checks(&self, enabled: bool) {
        if enabled {
            self.schedule.start();
        } else {
            self.schedule.stop();
        }
    }

    pub fn stop(&self) {
        self.schedule.stop();
    }

    /// Ask the Store; shared with an operation already running.
    pub fn check(&self) -> impl Future<Output = DesktopUpdateState> + Send + 'static {
        Inner::check(&self.inner)
    }

    /// Start the Store's own update, when one is available and nothing else runs.
    pub fn request_install(&self) -> bool {
        let mut pending = self.inner.lock_pending();
        if pending.is_some() || self.inner.state().status != DesktopUpdateStatus::Available {
            return false;
        }
        *pending = Some(Inner::start(&self.inner, StoreCommand::Install));
        true
    }
}

impl Inner {
    fn state(&self) -> DesktopUpdateState {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, Option<SharedState>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn check(inner: &Arc<Self>) -> SharedState {
        let mut pending = inner.lock_pending();
        if let Some(pending) = pending.as_ref() {
            return pending.clone();
        }
        if inner.state().status == DesktopUpdateStatus::Updated {
            let state = inner.state();
            return (Box::pin(async move { state })
                as Pin<Box<dyn Future<Output = DesktopUpdateState> + Send>>)
                .shared();
        }
        let started = Self::start(inner, StoreCommand::Check);
        *pending = Some(started.clone());
        started
    }

    fn set(&self, status: DesktopUpdateStatus, percent: Option<f64>, error: Option<String>) {
        let state = DesktopUpdateState {
            status,
            current_version: self.version.clone(),
            source: Some(DesktopUpdateSource::Store),
            version: None,
            percent,
            reason: None,
            error,
        };
        *self.state.lock().unwrap_or_else(|p| p.into_inner()) = state.clone();
        (self.broadcast)(&state);
    }

    /// Run one Store operation; the caller stores the returned future as the pending one.
    fn start(inner: &Arc<Self>, command: StoreCommand) -> SharedState {
        inner.set(
            match command {
                StoreCommand::Check => DesktopUpdateStatus::Checking,
                StoreCommand::Install => DesktopUpdateStatus::Installing,
            },
            None,
            None,
        );
        let (tx, rx) = tokio::sync::oneshot::channel();
        let shared: SharedState = (Box::pin(async move {
            rx.await.unwrap_or_else(|_| DesktopUpdateState {
                status: DesktopUpdateStatus::Error,
                current_version: String::new(),
                source: Some(DesktopUpdateSource::Store),
                version: None,
                percent: None,
                reason: None,
                error: None,
            })
        })
            as Pin<Box<dyn Future<Output = DesktopUpdateState> + Send>>)
            .shared();
        let task = inner.clone();
        inner.runtime.spawn(async move {
            task.run(command).await;
            *task.lock_pending() = None;
            let _ = tx.send(task.state());
        });
        shared
    }

    async fn run(self: &Arc<Self>, command: StoreCommand) {
        let outcome = async {
            if command == StoreCommand::Install {
                (self.before_install)()?;
            }
            let progress_owner = self.clone();
            let progress = move |percent: u8| {
                let percent = f64::from(percent);
                if progress_owner.state().percent != Some(percent) {
                    progress_owner.set(DesktopUpdateStatus::Installing, Some(percent), None);
                }
            };
            self.bridge.run(command, &progress).await
        }
        .await;
        match outcome {
            Ok(result) => self.set(
                match result {
                    StoreResult::Available | StoreResult::Canceled => {
                        DesktopUpdateStatus::Available
                    }
                    StoreResult::UpToDate => DesktopUpdateStatus::UpToDate,
                    StoreResult::Updated => DesktopUpdateStatus::Updated,
                },
                None,
                None,
            ),
            Err(error) => {
                log_update_error("Microsoft Store update failed", &error);
                self.set(
                    DesktopUpdateStatus::Error,
                    None,
                    Some(match command {
                        StoreCommand::Check => "Microsoft Store could not check for updates. Try again or open Microsoft Store.".into(),
                        StoreCommand::Install => "Microsoft Store could not complete the update. Check for updates again or open Microsoft Store.".into(),
                    }),
                );
            }
        }
    }
}

/// The Store helper program shipped with Store packages (`store-bridge.ts`): it inherits the
/// package identity, talks to the Store, and reports one JSON object per line.
pub struct HelperStoreBridge {
    pub executable: PathBuf,
    /// The owner window's native handle, as a decimal string.
    pub owner_window: Arc<dyn Fn() -> Result<String, String> + Send + Sync>,
}

/// A check cannot hang forever. Installation may legitimately wait for consent or a slow
/// download, so it has no timeout.
const STORE_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[async_trait]
impl StoreBridge for HelperStoreBridge {
    async fn run(
        &self,
        command: StoreCommand,
        progress: &(dyn Fn(u8) + Send + Sync),
    ) -> Result<StoreResult, String> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
        let owner = (self.owner_window)()?;
        let mut child = tokio::process::Command::new(&self.executable)
            .arg(command.as_str())
            .arg(owner)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?;
        let stdout = child.stdout.take().ok_or("no helper output")?;
        let mut stderr = child.stderr.take().ok_or("no helper output")?;
        let diagnostics = tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            let start = text.len().saturating_sub(4096);
            text[text.ceil_char_boundary(start)..].to_string()
        });
        let work = async {
            let mut lines = BufReader::new(stdout).lines();
            let mut result = None;
            let mut failure: Option<String> = None;
            while let Ok(Some(line)) = lines.next_line().await {
                match parse_helper_line(&line, command) {
                    Ok(HelperLine::Progress(percent)) => progress(percent),
                    Ok(HelperLine::Result(value)) => result = Some(value),
                    Err(error) => failure = Some(error),
                }
            }
            let status = child.wait().await.map_err(|error| error.to_string())?;
            Ok::<_, String>((result, failure, status.code()))
        };
        let outcome = match command {
            StoreCommand::Check => tokio::time::timeout(STORE_CHECK_TIMEOUT, work)
                .await
                .unwrap_or_else(|_| {
                    Ok((
                        None,
                        Some("Microsoft Store update check timed out.".into()),
                        None,
                    ))
                }),
            StoreCommand::Install => work.await,
        }?;
        let diagnostics = diagnostics.await.unwrap_or_default();
        match outcome {
            (Some(result), None, Some(0)) => Ok(result),
            (_, failure, code) => Err(format!(
                "{} {diagnostics}",
                failure.unwrap_or_else(|| format!(
                    "Store helper failed ({}).",
                    code.map_or("killed".into(), |code| code.to_string())
                ))
            )
            .trim()
            .to_string()),
        }
    }
}

enum HelperLine {
    Progress(u8),
    Result(StoreResult),
}

fn parse_helper_line(line: &str, command: StoreCommand) -> Result<HelperLine, String> {
    let message: serde_json::Value =
        serde_json::from_str(line).map_err(|_| "Invalid Store helper response.".to_string())?;
    let status = message.get("status").and_then(|s| s.as_str()).unwrap_or("");
    if status == "installing"
        && command == StoreCommand::Install
        && let Some(percent) = message
            .get("percent")
            .and_then(|p| p.as_f64())
            .filter(|p| p.is_finite())
    {
        return Ok(HelperLine::Progress(percent.floor().clamp(0.0, 100.0) as u8));
    }
    let result = match status {
        "available" => StoreResult::Available,
        "up-to-date" => StoreResult::UpToDate,
        "updated" => StoreResult::Updated,
        "canceled" => StoreResult::Canceled,
        _ => return Err("Microsoft Store helper reported an error.".into()),
    };
    if command == StoreCommand::Check
        && !matches!(result, StoreResult::Available | StoreResult::UpToDate)
    {
        return Err("Unexpected Store check result.".into());
    }
    Ok(HelperLine::Result(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> DistributionMetadata {
        DistributionMetadata {
            update_mode: Some("store".into()),
            store_product_id: Some("9MUXUS000000".into()),
        }
    }

    // `store-updates.test.ts`

    #[tokio::test]
    async fn leaves_normal_desktop_update_checks_to_the_existing_updater() {
        let opened = Mutex::new(Vec::new());
        let result = check_store_update(
            &DistributionMetadata::default(),
            false,
            "0.7.0",
            true,
            |url| {
                opened.lock().unwrap().push(url);
                async { Ok(()) }
            },
        )
        .await;
        assert_eq!(result, None);
        assert!(opened.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn silences_background_checks_without_opening_the_store() {
        let opened = Mutex::new(Vec::new());
        let result = check_store_update(&metadata(), false, "0.7.0", false, |url| {
            opened.lock().unwrap().push(url);
            async { Ok(()) }
        })
        .await;
        assert_eq!(
            result,
            Some(UpdateCheckResult::NotAvailable {
                current_version: "0.7.0".into(),
                latest_version: None,
                reason: Some("store".into())
            })
        );
        assert!(opened.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn opens_only_the_configured_product_when_explicitly_requested() {
        let opened = Mutex::new(Vec::new());
        check_store_update(&metadata(), true, "0.7.0", true, |url| {
            opened.lock().unwrap().push(url);
            async { Ok(()) }
        })
        .await;
        assert_eq!(
            *opened.lock().unwrap(),
            ["ms-windows-store://pdp/?ProductId=9MUXUS000000"]
        );
    }

    #[tokio::test]
    async fn honors_store_detection_even_if_package_metadata_is_absent() {
        let opened = Mutex::new(Vec::new());
        check_store_update(
            &DistributionMetadata::default(),
            true,
            "0.7.0",
            true,
            |url| {
                opened.lock().unwrap().push(url);
                async { Ok(()) }
            },
        )
        .await;
        assert_eq!(
            *opened.lock().unwrap(),
            ["ms-windows-store://downloadsandupdates"]
        );
    }

    #[tokio::test]
    async fn keeps_store_installations_on_the_store_path_when_opening_it_fails() {
        let result = check_store_update(&metadata(), true, "0.7.0", true, |_| async {
            Err("offline".to_string())
        })
        .await;
        assert!(matches!(
            result,
            Some(UpdateCheckResult::NotAvailable { reason: Some(ref reason), .. }) if reason == "store-open-failed"
        ));
    }

    #[test]
    fn reads_helper_lines() {
        assert!(matches!(
            parse_helper_line(
                r#"{"status":"installing","percent":42.7}"#,
                StoreCommand::Install
            ),
            Ok(HelperLine::Progress(42))
        ));
        assert!(matches!(
            parse_helper_line(r#"{"status":"updated"}"#, StoreCommand::Install),
            Ok(HelperLine::Result(StoreResult::Updated))
        ));
        assert!(parse_helper_line(r#"{"status":"updated"}"#, StoreCommand::Check).is_err());
        assert!(parse_helper_line(r#"{"status":"error"}"#, StoreCommand::Check).is_err());
        assert!(parse_helper_line("nonsense", StoreCommand::Check).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_the_store_helper_and_reads_its_answers() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("muxus-store-updater");
        std::fs::write(
            &helper,
            "#!/bin/sh\n[ \"$2\" = 77 ] || exit 3\nif [ \"$1\" = install ]; then echo '{\"status\":\"installing\",\"percent\":50}'; echo '{\"status\":\"updated\"}'; else echo '{\"status\":\"available\"}'; fi\n",
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let bridge = HelperStoreBridge {
            executable: helper.clone(),
            owner_window: Arc::new(|| Ok("77".into())),
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let progress = move |percent: u8| record.lock().unwrap().push(percent);
        assert_eq!(
            bridge.run(StoreCommand::Check, &progress).await,
            Ok(StoreResult::Available)
        );
        assert_eq!(
            bridge.run(StoreCommand::Install, &progress).await,
            Ok(StoreResult::Updated)
        );
        assert_eq!(*seen.lock().unwrap(), [50]);
        let failing = HelperStoreBridge {
            executable: helper,
            owner_window: Arc::new(|| Ok("1".into())),
        };
        assert!(
            failing
                .run(StoreCommand::Check, &progress)
                .await
                .unwrap_err()
                .contains("(3)")
        );
    }
}
