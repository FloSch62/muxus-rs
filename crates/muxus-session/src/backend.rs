//! [`Backend`]: the in-process server (`server/src/app.ts`, `server/src/server.ts`).

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use muxus_local::pty::LocalEnvironment;
use muxus_ssh::LocalEnv;
use muxus_ssh::connection::{ManagerOptions, SshConnectionManager};
use muxus_ssh::folder_auth::{folder_auth_resolver, saved_profile_folder_auth_resolver};
use muxus_ssh::forwards::{ForwardManager, ForwardManagerOptions};
use muxus_ssh::sftp::SftpService;
use muxus_ssh::x11::local::{LocalX11, LocalX11Options};
use muxus_store::Database;
use muxus_store::db::DEFAULT_SNAPSHOT_GRACE_SECONDS;
use muxus_store::history::{HistoryOpenOptions, SessionHistoryStore};
use muxus_store::paths::history_root;
use muxus_store::vault::{
    MemoryVaultKeyStore, PasswordVault, PasswordVaultKdf, SystemVaultKeyStore, VaultKeyStore,
    VaultOptions,
};
use tokio::runtime::{Handle, Runtime};

use crate::adapters::{StoreFolderAuth, StoreHistory, StoreVault};
use crate::data_dir::{DataLocation, DataSource};
use crate::diagnostics::DiagnosticProbes;
use crate::error::{ApiError, ApiResult};
use crate::stats::SharedReadings;
use crate::terminal::{SessionRegistry, TerminalInputs};

/// The manifest the update check reads (`UPDATE_MANIFEST_URL`).
pub const UPDATE_MANIFEST_URL: &str = "https://flosch62.github.io/muxus/latest.json";
/// How long a detached terminal waits for a window to attach again
/// (`TERMINAL_REATTACH_GRACE_MS`).
pub const TERMINAL_REATTACH_GRACE: Duration = Duration::from_secs(15);

/// Environment variable lookups (`SSH_AUTH_SOCK`, `$VAR` indirections).
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Which tokio runtime the backend runs on.
#[derive(Debug, Clone)]
pub enum RuntimeChoice {
    /// Its own multi-thread runtime with this many worker threads (shut down with the backend).
    Own { worker_threads: usize },
    /// A runtime the caller owns (tests, an embedding that already runs tokio).
    Handle(Handle),
}

impl Default for RuntimeChoice {
    fn default() -> Self {
        RuntimeChoice::Own { worker_threads: 4 }
    }
}

/// Everything [`Backend::start`] needs. [`BackendOptions::from_process`] reads the running
/// process; [`BackendOptions::sandbox`] keeps every file and secret inside scratch folders.
#[derive(Clone)]
pub struct BackendOptions {
    /// The data folder (database, preferences, session history).
    pub data: DataLocation,
    /// The user's home directory: `~/.ssh`, known_hosts, the default session log folder.
    pub home: PathBuf,
    /// Environment lookups for SSH (`SSH_AUTH_SOCK`, `$VAR` indirections).
    pub env_lookup: EnvLookup,
    /// The environment local terminals start from; `None` uses this process's (and writes the
    /// shell integration shims to the user's cache folder on first use).
    pub local_environment: Option<LocalEnvironment>,
    /// Where the vault key of an automatic vault lives; `None` is the OS credential store.
    pub vault_key_store: Option<Arc<dyn VaultKeyStore>>,
    /// scrypt parameters for new vaults; `None` is the production default.
    pub vault_kdf: Option<PasswordVaultKdf>,
    pub runtime: RuntimeChoice,
    /// How long a detached terminal stays alive for a window to attach again.
    pub reattach_grace: Duration,
    /// This build's version, as the update check compares it.
    pub app_version: String,
    /// The update manifest URL (an `http://` URL is allowed for tests).
    pub update_manifest_url: String,
    /// Directory of the X server bundled with the Windows app.
    pub x11_server_directory: Option<PathBuf>,
    /// Start the saved tunnels marked "start with Muxus".
    pub autostart_tunnels: bool,
    /// What connection diagnostics probe with; `None` is the real network, `ping` and agent.
    pub diagnostic_probes: Option<Arc<dyn DiagnosticProbes>>,
}

impl std::fmt::Debug for BackendOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendOptions")
            .field("data", &self.data)
            .field("home", &self.home)
            .field("runtime", &self.runtime)
            .finish_non_exhaustive()
    }
}

/// Names the release folder updates come from (`<folder>/latest.json` for the manifest check,
/// the `latest*.yml` feeds for the in-app updater). Unset, this build offers no updates:
/// [`UPDATE_MANIFEST_URL`] lists the Electron releases, which must never reach a Rust build.
pub const UPDATE_FEED_VAR: &str = "MUXUS_UPDATE_FEED";

/// The manifest the running process checks: empty (no update check) unless
/// [`UPDATE_FEED_VAR`] names a release folder.
pub fn process_update_manifest_url() -> String {
    std::env::var(UPDATE_FEED_VAR)
        .ok()
        .map(|feed| feed.trim().trim_end_matches('/').to_string())
        .filter(|feed| !feed.is_empty())
        .map(|feed| format!("{feed}/latest.json"))
        .unwrap_or_default()
}

impl BackendOptions {
    /// The running process: its data folder (see [`crate::data_dir`]), home, environment and the
    /// OS credential store.
    pub fn from_process() -> Self {
        Self {
            data: DataLocation::from_process(),
            home: LocalEnv::current().home,
            env_lookup: Arc::new(|name: &str| std::env::var(name).ok()),
            local_environment: None,
            vault_key_store: None,
            vault_kdf: None,
            runtime: RuntimeChoice::default(),
            reattach_grace: TERMINAL_REATTACH_GRACE,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            update_manifest_url: process_update_manifest_url(),
            x11_server_directory: std::env::var_os("MUXUS_X11_SERVER_DIR").map(PathBuf::from),
            autostart_tunnels: true,
            diagnostic_probes: None,
        }
    }

    /// A backend that touches nothing outside `data_dir` and `home`: an in-memory vault key
    /// store, no SSH agent, local shells started with `HOME=home` and no shell integration, and
    /// a cheap vault KDF. For tests and sandboxed runs.
    pub fn sandbox(data_dir: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let mut vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        vars.insert("HOME".into(), home.to_string_lossy().into_owned());
        for key in ["PATH", "LANG", "TERM", "USER", "LOGNAME"] {
            if let Ok(value) = std::env::var(key) {
                vars.insert(key.into(), value);
            }
        }
        vars.insert("SHELL".into(), "/bin/sh".into());
        Self {
            data: DataLocation {
                paths: muxus_store::paths::AppPaths::at(data_dir),
                source: DataSource::Override,
                seed_from: None,
                history_root: None,
            },
            home,
            env_lookup: Arc::new(|_: &str| None),
            local_environment: Some(LocalEnvironment {
                vars,
                integration_root: None,
                platform: muxus_core::platform::Platform::current(),
            }),
            vault_key_store: Some(Arc::new(MemoryVaultKeyStore::new())),
            vault_kdf: Some(PasswordVaultKdf {
                cost: 1024,
                block_size: 8,
                parallelism: 1,
            }),
            runtime: RuntimeChoice::default(),
            reattach_grace: TERMINAL_REATTACH_GRACE,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            update_manifest_url: UPDATE_MANIFEST_URL.to_string(),
            x11_server_directory: None,
            autostart_tunnels: true,
            diagnostic_probes: None,
        }
    }
}

/// Facts the routes need about where things are.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub paths: muxus_store::paths::AppPaths,
    pub home: PathBuf,
    pub database_path: PathBuf,
    /// The forced history root (`historyPath` in the TypeScript server config).
    pub history_path: Option<PathBuf>,
    pub reattach_grace: Duration,
    pub app_version: String,
    pub update_manifest_url: String,
}

/// The state every backend clone shares (`AppContext`).
pub(crate) struct Shared {
    pub runtime: Handle,
    owned_runtime: Mutex<Option<Runtime>>,
    pub config: Config,
    pub local_env: LocalEnv,
    pub env_lookup: EnvLookup,
    local_environment: Mutex<Option<LocalEnvironment>>,
    pub database: Arc<Database>,
    pub vault: Arc<PasswordVault>,
    pub history: Arc<SessionHistoryStore>,
    pub prefs: Mutex<muxus_store::prefs::PrefsStore>,
    pub connections: SshConnectionManager,
    pub forwards: ForwardManager,
    pub sftp: SftpService,
    pub x11: Arc<LocalX11>,
    pub terminal_inputs: TerminalInputs,
    pub sessions: SessionRegistry,
    pub host_stats: SharedReadings,
    pub diagnostic_probes: Arc<dyn DiagnosticProbes>,
    pub update_check: Mutex<Arc<tokio::sync::OnceCell<muxus_core::api::UpdateCheckResult>>>,
    closed: AtomicBool,
}

impl Shared {
    /// The local terminals' environment (built once; it writes the integration shims).
    pub fn local_environment(&self) -> LocalEnvironment {
        let mut slot = self
            .local_environment
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        slot.get_or_insert_with(LocalEnvironment::current).clone()
    }

    /// The ssh_config root file (`~/.ssh/config`).
    pub fn ssh_config_path(&self) -> PathBuf {
        self.local_env.default_ssh_config_path()
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Some(runtime) = self
            .owned_runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            runtime.shutdown_background();
        }
    }
}

/// The Muxus backend. See the crate documentation for the threading model.
#[derive(Clone)]
pub struct Backend {
    pub(crate) shared: Arc<Shared>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
            .field("database", &self.shared.config.database_path)
            .finish_non_exhaustive()
    }
}

static PROCESS_START: OnceLock<()> = OnceLock::new();

impl Backend {
    /// Open the data folder and start the backend (`buildApp` + `startServer`). Can be awaited
    /// from any executor.
    pub async fn start(options: BackendOptions) -> ApiResult<Backend> {
        PROCESS_START.get_or_init(|| ());
        let (handle, owned) = match &options.runtime {
            RuntimeChoice::Handle(handle) => (handle.clone(), None),
            RuntimeChoice::Own { worker_threads } => {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads((*worker_threads).max(1))
                    .thread_name("muxus-backend")
                    .enable_all()
                    .build()
                    .map_err(|error| ApiError::internal(error.to_string()))?;
                (runtime.handle().clone(), Some(runtime))
            }
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let init_handle = handle.clone();
        handle.spawn(async move {
            let _ = tx.send(initialize(options, init_handle).await);
        });
        let result = rx
            .await
            .unwrap_or_else(|_| Err(ApiError::internal("the backend failed to start")));
        match result {
            Ok(parts) => Ok(Backend {
                shared: Arc::new(parts.into_shared(handle, owned)),
            }),
            Err(error) => {
                if let Some(runtime) = owned {
                    runtime.shutdown_background();
                }
                Err(error)
            }
        }
    }

    /// [`Backend::start`] for a plain thread (not inside an async executor).
    pub fn start_blocking(options: BackendOptions) -> ApiResult<Backend> {
        let (tx, rx) = async_channel::bounded(1);
        std::thread::Builder::new()
            .name("muxus-backend-start".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("a start-up runtime");
                let _ = tx.send_blocking(runtime.block_on(Backend::start(options)));
            })
            .map_err(|error| ApiError::internal(error.to_string()))?;
        rx.recv_blocking()
            .unwrap_or_else(|_| Err(ApiError::internal("the backend failed to start")))
    }

    /// Stop everything (`onClose`): forwards, SSH transports, terminal sessions, the X server,
    /// the vault key, and session history (finishing open parts). Later calls fail.
    pub async fn shutdown(&self) {
        if self.shared.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        // Spawned directly: `spawn` refuses new work once the backend is closed.
        let shared = self.shared.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        self.shared.runtime.spawn(async move {
            shared.sessions.close_all();
            shared.forwards.stop_all();
            shared.connections.close_all();
            shared.x11.close();
            shared.vault.dispose();
            let _ = shared.history.close().await;
            let _ = tx.send(());
        });
        let _ = rx.await;
        if let Some(runtime) = self
            .shared
            .owned_runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            runtime.shutdown_background();
        }
    }

    /// The runtime backend work runs on.
    pub fn runtime(&self) -> &Handle {
        &self.shared.runtime
    }

    /// The application database, for the layers that keep their own records in it (saved
    /// management requests, desktop identities).
    pub fn database(&self) -> &Arc<Database> {
        &self.shared.database
    }

    /// The password vault, for layers that keep their own secrets in it (desktop and
    /// management passwords).
    pub fn vault(&self) -> &Arc<PasswordVault> {
        &self.shared.vault
    }

    /// The session history store, for layers that record sessions of their own.
    pub fn history(&self) -> &Arc<SessionHistoryStore> {
        &self.shared.history
    }

    /// The SSH connection manager, for layers that open their own channels (management
    /// sessions, remote desktops over SSH gateways).
    pub fn connections(&self) -> &SshConnectionManager {
        &self.shared.connections
    }

    /// The live forward list, republished after every change.
    pub fn subscribe_forwards(
        &self,
    ) -> tokio::sync::watch::Receiver<Vec<muxus_core::api::ForwardInfo>> {
        self.shared.forwards.subscribe()
    }

    /// Run `future` on the backend runtime; the returned future only waits for its answer.
    pub(crate) fn spawn<T, F>(
        &self,
        future: F,
    ) -> impl Future<Output = ApiResult<T>> + Send + 'static
    where
        T: Send + 'static,
        F: Future<Output = ApiResult<T>> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let closed = self.shared.closed.load(Ordering::SeqCst);
        if !closed {
            self.shared.runtime.spawn(async move {
                let _ = tx.send(future.await);
            });
        }
        async move {
            rx.await
                .unwrap_or_else(|_| Err(ApiError::internal("the backend has shut down")))
        }
    }

    /// Run `work` with the shared state on the backend runtime.
    pub(crate) fn run<T, F, Fut>(
        &self,
        work: F,
    ) -> impl Future<Output = ApiResult<T>> + Send + 'static
    where
        T: Send + 'static,
        F: FnOnce(Arc<Shared>) -> Fut,
        Fut: Future<Output = ApiResult<T>> + Send + 'static,
    {
        self.spawn(work(self.shared.clone()))
    }

    /// Run blocking store work on the runtime's blocking pool.
    pub(crate) fn blocking<T, F>(
        &self,
        work: F,
    ) -> impl Future<Output = ApiResult<T>> + Send + 'static
    where
        T: Send + 'static,
        F: FnOnce(&Shared) -> ApiResult<T> + Send + 'static,
    {
        let shared = self.shared.clone();
        self.spawn(async move {
            tokio::task::spawn_blocking(move || work(&shared))
                .await
                .unwrap_or_else(|error| Err(ApiError::internal(error.to_string())))
        })
    }
}

/// What start-up builds before the shared state exists.
struct Parts {
    config: Config,
    local_env: LocalEnv,
    env_lookup: EnvLookup,
    local_environment: Option<LocalEnvironment>,
    database: Arc<Database>,
    vault: Arc<PasswordVault>,
    history: Arc<SessionHistoryStore>,
    prefs: muxus_store::prefs::PrefsStore,
    connections: SshConnectionManager,
    forwards: ForwardManager,
    x11: Arc<LocalX11>,
    diagnostic_probes: Arc<dyn DiagnosticProbes>,
}

impl Parts {
    fn into_shared(self, runtime: Handle, owned: Option<Runtime>) -> Shared {
        Shared {
            runtime,
            owned_runtime: Mutex::new(owned),
            sftp: SftpService::new(self.connections.clone()),
            config: self.config,
            local_env: self.local_env,
            env_lookup: self.env_lookup,
            local_environment: Mutex::new(self.local_environment),
            database: self.database,
            vault: self.vault,
            history: self.history,
            prefs: Mutex::new(self.prefs),
            connections: self.connections,
            forwards: self.forwards,
            x11: self.x11,
            terminal_inputs: TerminalInputs::default(),
            sessions: SessionRegistry::default(),
            host_stats: SharedReadings::new(),
            diagnostic_probes: self.diagnostic_probes,
            update_check: Mutex::new(Arc::new(tokio::sync::OnceCell::new())),
            closed: AtomicBool::new(false),
        }
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| Err(ApiError::internal(error.to_string())))
}

/// `buildApp`: open everything in the order the TypeScript server does.
async fn initialize(options: BackendOptions, runtime: Handle) -> ApiResult<Parts> {
    let BackendOptions {
        data,
        home,
        env_lookup,
        local_environment,
        vault_key_store,
        vault_kdf,
        reattach_grace,
        app_version,
        update_manifest_url,
        x11_server_directory,
        autostart_tunnels,
        diagnostic_probes,
        runtime: _,
    } = options;
    let key_store: Arc<dyn VaultKeyStore> =
        vault_key_store.unwrap_or_else(|| Arc::new(SystemVaultKeyStore::new()));

    let prepare_store = key_store.clone();
    let prepare_data = data.clone();
    blocking(move || {
        prepare_data
            .prepare(Some(prepare_store.as_ref()))
            .map(drop)
            .map_err(ApiError::internal)
    })
    .await?;

    let database_path = data.paths.database();
    let open_path = database_path.clone();
    let vault_store = key_store.clone();
    let (database, vault) = blocking(move || {
        let database = Arc::new(Database::open(&open_path)?);
        let vault = Arc::new(PasswordVault::new(
            database.clone(),
            VaultOptions {
                kdf: vault_kdf,
                key_store: Some(vault_store),
            },
        ));
        vault.initialize()?;
        Ok((database, vault))
    })
    .await?;

    // Preferences and other UI state (`client-state.json`), as the Electron main process kept
    // them. A snapshot that needed migrating is written back; a failed write is only logged.
    let prefs_path = data.paths.client_state_file();
    let prefs = blocking(move || {
        let (store, error) = muxus_store::prefs::PrefsStore::open(prefs_path);
        if let Some(error) = error {
            tracing::warn!(%error, "could not write the migrated preferences");
        }
        Ok(store)
    })
    .await?;

    let local_env = LocalEnv::with_home(&home);
    let x11 = Arc::new(LocalX11::new(LocalX11Options {
        bundled_server_directory: x11_server_directory,
        ..LocalX11Options::default()
    }));
    let connections = SshConnectionManager::new(manager_options(
        &local_env,
        env_lookup.clone(),
        database.clone(),
        vault.clone(),
        x11.clone(),
    ));
    let tunnels_db = database.clone();
    let forwards = ForwardManager::new(
        connections.clone(),
        ForwardManagerOptions::with_tunnels(move |id| tunnels_db.tunnel(id).ok().flatten()),
    );

    // Session history: the forced root, else the saved location, else beside the database.
    let history_db = database.clone();
    let (settings, had_legacy) = blocking(move || {
        Ok((
            history_db.session_history_settings()?,
            history_db.has_legacy_session_history()?,
        ))
    })
    .await?;
    let root = history_root(
        data.history_root.as_deref(),
        settings.storage_location.as_deref(),
        &database_path,
    )
    .map(|root| std::path::absolute(&root).unwrap_or(root));
    let history = SessionHistoryStore::open(HistoryOpenOptions {
        root,
        settings,
        legacy_database_path: had_legacy.then(|| database_path.clone()),
    })
    .await?;
    let finalize_db = database.clone();
    blocking(move || {
        finalize_db.finalize_session_history_separation(had_legacy)?;
        finalize_db.prune_terminal_snapshots(DEFAULT_SNAPSHOT_GRACE_SECONDS)?;
        Ok(())
    })
    .await?;

    if autostart_tunnels {
        let tunnels_db = database.clone();
        let tunnels = blocking(move || Ok(tunnels_db.list_tunnels()?)).await?;
        let _guard = runtime.enter();
        forwards.autostart(&tunnels);
    }

    let diagnostic_probes = diagnostic_probes.unwrap_or_else(|| {
        Arc::new(crate::routes::diagnostics::BackendProbes {
            env: env_lookup.clone(),
            home: home.clone(),
        })
    });
    Ok(Parts {
        config: Config {
            paths: data.paths.clone(),
            home,
            database_path,
            history_path: data.history_root.clone(),
            reattach_grace,
            app_version,
            update_manifest_url,
        },
        local_env,
        env_lookup,
        local_environment,
        database,
        vault,
        history: Arc::new(history),
        prefs,
        connections,
        forwards,
        x11,
        diagnostic_probes,
    })
}

/// The connection manager's view of the store (`new SshConnectionManager(...)` in `app.ts`).
fn manager_options(
    local_env: &LocalEnv,
    env_lookup: EnvLookup,
    database: Arc<Database>,
    vault: Arc<PasswordVault>,
    x11: Arc<LocalX11>,
) -> ManagerOptions {
    let mut options = ManagerOptions::new(local_env.clone());
    options.env_lookup = env_lookup;
    options.vault = Some(Arc::new(StoreVault(vault)));
    let saved_db = database.clone();
    options.saved_ssh_profile = Some(Arc::new(move |id: &str| {
        match saved_db.saved_host_profile(id).ok().flatten()?.profile {
            muxus_core::protocol::SavedHostSessionProfile::Ssh(profile) => Some(profile),
            _ => None,
        }
    }));
    let folders = Arc::new(StoreFolderAuth(database.clone()));
    let by_alias = folder_auth_resolver(folders.clone());
    options.folder_auth = Some(Arc::new(move |alias: &str| by_alias(alias)));
    let by_profile = saved_profile_folder_auth_resolver(folders);
    options.profile_folder_auth = Some(Arc::new(move |id: &str| by_profile(id)));
    let db = database.clone();
    options.disable_sftp_for_host = Some(Arc::new(move |alias: &str| {
        db.sftp_disabled_for_alias(alias).unwrap_or(false)
    }));
    let db = database.clone();
    options.console_compatibility_for_host = Some(Arc::new(move |alias: &str| {
        db.console_compatibility_for_alias(alias).unwrap_or(false)
    }));
    let db = database.clone();
    options.disable_sftp_for_profile = Some(Arc::new(move |id: &str| {
        db.sftp_disabled_for_saved_host(id).unwrap_or(false)
    }));
    let db = database;
    options.console_compatibility_for_profile = Some(Arc::new(move |id: &str| {
        db.console_compatibility_for_saved_host(id).unwrap_or(false)
    }));
    options.x11 = Some(x11);
    options
}

/// History adapter for recorders.
pub(crate) fn history_sink(shared: &Shared) -> Arc<dyn crate::logging::HistorySink> {
    Arc::new(StoreHistory(shared.history.clone()))
}
