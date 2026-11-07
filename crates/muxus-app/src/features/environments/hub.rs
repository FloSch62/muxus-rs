//! Reading hosts (`client/src/environments.ts` with `useEnvironmentDiscovery.ts`):
//! one hub per window. It reads this computer when the window opens and each
//! SSH host when a tab to it connects, reads them again on the refresh
//! interval while they stay connected, and forgets hosts nobody is connected
//! to. Hosts are read over the tab's own connection, and automatically only
//! when their server runs commands. While the panel is open it may dial a
//! host without a terminal, asking for passwords like a session would; those
//! connections are let go when the panel closes.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use muxus_core::environments::EnvironmentsResponse;
use muxus_core::protocol::{TerminalClientMessage, TerminalServerMessage};
use muxus_session::environments::EnvironmentSource;
use muxus_session::{Backend, SessionEvent, SessionHandle};
use muxus_ui::gpui_kit::{AsyncApp, Context, EventEmitter, Task, WeakEntity};

use super::model::{
    EntryPatch, EnvironmentCount, EnvironmentHost, EnvironmentStatus, EnvironmentsStore,
    HostEnvironmentsEntry, LOCAL_ENVIRONMENT_KEY, LiveTab, connected_hosts, disconnected_keys,
    discovery_enabled_for, environment_badges, live_connection_for,
};
use crate::features::forwarding::start::{DialAnswer, DialPrompt};
use crate::model::{SavedHostProfile, SshHostEntry, SshProfile};

/// What the hub reads of its window.
#[derive(Clone, Debug, PartialEq)]
pub struct HubInput {
    pub tabs: Vec<LiveTab>,
    /// Settings → Behavior → Find containers, clusters and VMs.
    pub discover: bool,
    /// Read connected hosts again this often; 0 only on connect.
    pub refresh_seconds: f64,
    /// The SSH keepalive preference, sent with the panel's own dials.
    pub keepalive_seconds: f64,
    /// A hidden window does not read again.
    pub visible: bool,
}

impl Default for HubInput {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            discover: true,
            refresh_seconds: 30.,
            keepalive_seconds: 0.,
            visible: true,
        }
    }
}

/// Which hosts the panel shows (`environmentsPanel`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PanelScope {
    #[default]
    Closed,
    /// This computer and every connected host.
    All,
    /// One host, by key.
    Host(String),
}

impl PanelScope {
    pub fn is_open(&self) -> bool {
        *self != PanelScope::Closed
    }

    pub fn host_key(&self) -> Option<&str> {
        match self {
            PanelScope::Host(key) => Some(key),
            _ => None,
        }
    }
}

/// What the hub asks its window to do.
pub enum EnvironmentsHubEvent {
    /// A dial of the panel asks a sign-in question; answer on `reply` (dropping it cancels).
    Prompt {
        prompt: DialPrompt,
        reply: async_channel::Sender<DialAnswer>,
    },
}

/// A shell-less transport the panel dialed.
struct Dial {
    conn_id: String,
    handle: Arc<SessionHandle>,
}

#[derive(Clone, Copy, Debug)]
struct ReadOptions {
    automatic: bool,
    interactive: bool,
    /// A background refresh: keep showing the last result until the new one arrives.
    quiet: bool,
}

pub struct EnvironmentsHub {
    backend: Backend,
    store: EnvironmentsStore,
    /// Hosts being read now.
    reading: HashSet<String>,
    dials: HashMap<String, Dial>,
    input: HubInput,
    ssh_hosts: Vec<SshHostEntry>,
    saved_profiles: Vec<SavedHostProfile>,
    catalog_loaded: bool,
    /// The hosts a connected tab of the window can run commands on, by key.
    live_keys: BTreeSet<String>,
    panel: PanelScope,
    refresh: Option<Task<()>>,
    kubus: Option<bool>,
}

impl EventEmitter<EnvironmentsHubEvent> for EnvironmentsHub {}

impl EnvironmentsHub {
    pub fn new(backend: Backend) -> Self {
        Self {
            backend,
            store: EnvironmentsStore::default(),
            reading: HashSet::new(),
            dials: HashMap::new(),
            input: HubInput {
                // Nothing is read until the window says what it shows.
                discover: false,
                ..HubInput::default()
            },
            ssh_hosts: Vec::new(),
            saved_profiles: Vec::new(),
            catalog_loaded: false,
            live_keys: BTreeSet::new(),
            panel: PanelScope::Closed,
            refresh: None,
            kubus: None,
        }
    }

    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    pub fn store(&self) -> &EnvironmentsStore {
        &self.store
    }

    pub fn entry(&self, key: &str) -> Option<&HostEnvironmentsEntry> {
        self.store.get(key)
    }

    pub fn panel(&self) -> &PanelScope {
        &self.panel
    }

    pub fn ssh_hosts(&self) -> &[SshHostEntry] {
        &self.ssh_hosts
    }

    pub fn saved_profiles(&self) -> &[SavedHostProfile] {
        &self.saved_profiles
    }

    /// The marks the sidebar shows at the end of host rows, by host key.
    pub fn marks(&self) -> HashMap<String, Vec<EnvironmentCount>> {
        self.store
            .entries()
            .map(|entry| {
                (
                    entry.key().to_string(),
                    environment_badges(&entry.environments),
                )
            })
            .filter(|(_, badges)| !badges.is_empty())
            .collect()
    }

    /// Hosts a launcher search may open things on, with what they run (`readyEntries`).
    pub fn ready_entries(&self) -> Vec<HostEnvironmentsEntry> {
        self.store
            .entries()
            .filter(|entry| !entry.environments.is_empty())
            .cloned()
            .collect()
    }

    /// The sidebar's hosts changed.
    pub fn set_catalog(
        &mut self,
        ssh_hosts: Vec<SshHostEntry>,
        saved_profiles: Vec<SavedHostProfile>,
        cx: &mut Context<Self>,
    ) {
        if self.catalog_loaded
            && self.ssh_hosts == ssh_hosts
            && self.saved_profiles == saved_profiles
        {
            return;
        }
        self.ssh_hosts = ssh_hosts;
        self.saved_profiles = saved_profiles;
        self.catalog_loaded = true;
        self.follow_tabs(cx);
    }

    /// The window's tabs or preferences changed.
    pub fn sync(&mut self, input: HubInput, cx: &mut Context<Self>) {
        if input == self.input {
            return;
        }
        let previous = std::mem::replace(&mut self.input, input);
        if previous.discover != self.input.discover {
            self.follow_setting(cx);
        }
        if previous.refresh_seconds != self.input.refresh_seconds || self.refresh.is_none() {
            self.restart_refresh(cx);
        }
        if previous.tabs != self.input.tabs || previous.discover != self.input.discover {
            self.follow_tabs(cx);
        }
    }

    /// This computer, at startup and when the setting comes back on.
    fn follow_setting(&mut self, cx: &mut Context<Self>) {
        let local = EnvironmentHost::local();
        match self.store.get(LOCAL_ENVIRONMENT_KEY) {
            Some(entry) if !self.input.discover && entry.automatic => {
                self.store.remove(LOCAL_ENVIRONMENT_KEY);
                cx.notify();
            }
            None if self.input.discover => self.discover(
                local,
                ReadOptions {
                    automatic: true,
                    interactive: false,
                    quiet: false,
                },
                cx,
            ),
            _ => {}
        }
    }

    /// SSH hosts, as their tabs connect and disconnect.
    fn follow_tabs(&mut self, cx: &mut Context<Self>) {
        if !self.catalog_loaded {
            return;
        }
        if self.input.discover && self.store.get(LOCAL_ENVIRONMENT_KEY).is_none() {
            self.follow_setting(cx);
        }
        let connected = connected_hosts(&self.input.tabs, &self.ssh_hosts, &self.saved_profiles);
        let live_keys: BTreeSet<String> = connected.iter().map(|host| host.key.clone()).collect();
        if live_keys != self.live_keys {
            self.live_keys = live_keys;
            cx.notify();
        }
        for host in connected {
            if let Some(entry) = self.store.get(&host.key) {
                // The host editor may have changed the host's own choice.
                if entry.host.discover != host.discover
                    || entry.host.system_open_ssh != host.system_open_ssh
                {
                    self.store.update(&host, EntryPatch::default());
                    cx.notify();
                }
                if self
                    .store
                    .get(&host.key)
                    .is_some_and(|entry| entry.automatic)
                    && !discovery_enabled_for(&host, self.input.discover)
                {
                    self.store.remove(&host.key);
                    cx.notify();
                }
                continue;
            }
            if discovery_enabled_for(&host, self.input.discover) {
                self.discover(
                    host,
                    ReadOptions {
                        automatic: true,
                        interactive: false,
                        quiet: false,
                    },
                    cx,
                );
            }
        }
        let keep = self.panel.host_key().map(str::to_string);
        self.forget_disconnected_hosts(keep.as_deref(), cx);
    }

    fn restart_refresh(&mut self, cx: &mut Context<Self>) {
        let seconds = self.input.refresh_seconds;
        if seconds <= 0. {
            self.refresh = None;
            return;
        }
        self.refresh = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs_f64(seconds))
                    .await;
                let alive = this.update(cx, |this, cx| {
                    if this.input.visible {
                        this.refresh_connected_hosts(cx);
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
    }

    /// The hosts a connected tab of the window can run commands on, by key.
    pub fn live_keys(&self) -> &BTreeSet<String> {
        &self.live_keys
    }

    /// Whether Muxus can read the host now without connecting: this computer, a connected tab,
    /// or the panel's own connection (`hostConnected`).
    pub fn host_connected(&self, host: &EnvironmentHost) -> bool {
        host.profile.is_none()
            || self.dials.contains_key(&host.key)
            || live_connection_for(&self.input.tabs, host).is_some()
    }

    /// Drop what is known about hosts nobody is connected to, so nothing is shown for them.
    /// `keep` is the host the panel is open on, which may be connecting.
    fn forget_disconnected_hosts(&mut self, keep: Option<&str>, cx: &mut Context<Self>) {
        let keys = disconnected_keys(
            &self.store,
            keep,
            |host| self.host_connected(host),
            |key| self.reading.contains(key),
        );
        if keys.is_empty() {
            return;
        }
        for key in keys {
            self.store.remove(&key);
        }
        cx.notify();
    }

    /// Read every host that can be read without connecting again, quietly. Hosts that run no
    /// commands, and hosts found automatically whose discovery is now off, are left alone
    /// (`refreshConnectedHosts`).
    pub fn refresh_connected_hosts(&mut self, cx: &mut Context<Self>) {
        let due: Vec<(EnvironmentHost, bool)> = self
            .store
            .entries()
            .filter(|entry| {
                !matches!(
                    entry.status,
                    EnvironmentStatus::Unsupported | EnvironmentStatus::Loading
                ) && self.host_connected(&entry.host)
                    && !(entry.automatic
                        && !discovery_enabled_for(&entry.host, self.input.discover))
            })
            .map(|entry| (entry.host.clone(), entry.automatic))
            .collect();
        for (host, automatic) in due {
            self.discover(
                host,
                ReadOptions {
                    automatic,
                    interactive: false,
                    quiet: true,
                },
                cx,
            );
        }
    }

    /// Read a host again over whatever transport it has, dialing if the panel can ask
    /// (`refreshHost`).
    pub fn refresh_host(&mut self, host: EnvironmentHost, cx: &mut Context<Self>) {
        self.discover(
            host,
            ReadOptions {
                automatic: false,
                interactive: true,
                quiet: false,
            },
            cx,
        );
    }

    /// Remove a host from the list (the panel's "Remove from this list").
    pub fn forget(&mut self, key: &str, cx: &mut Context<Self>) {
        self.store.remove(key);
        if self.panel.host_key() == Some(key) {
            self.panel = PanelScope::All;
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // The panel
    // -----------------------------------------------------------------------

    /// Open the panel on `scope`, or close it.
    pub fn set_panel(&mut self, scope: PanelScope, cx: &mut Context<Self>) {
        if scope == self.panel {
            return;
        }
        let was_open = self.panel.is_open();
        self.panel = scope;
        if !self.panel.is_open() {
            // Closing it lets go of the connections it dialed.
            self.release_dials(cx);
            cx.notify();
            return;
        }
        if !was_open && self.kubus.is_none() {
            let backend = self.backend.clone();
            cx.spawn(async move |this, cx| {
                let available = backend.kubus_available().await;
                let _ = this.update(cx, |this, cx| {
                    this.kubus = Some(available);
                    cx.notify();
                });
            })
            .detach();
        }
        // A host opened from the sidebar is read now if nothing is known about it yet; the
        // overview always includes this computer.
        match self.panel.clone() {
            PanelScope::Host(key) => {
                if let Some(entry) = self.store.get(&key)
                    && (entry.status == EnvironmentStatus::Offline
                        || (entry.status != EnvironmentStatus::Loading
                            && entry.environments.is_empty()))
                {
                    let host = entry.host.clone();
                    self.discover(
                        host,
                        ReadOptions {
                            automatic: false,
                            interactive: true,
                            quiet: false,
                        },
                        cx,
                    );
                }
            }
            PanelScope::All => {
                if self.store.get(LOCAL_ENVIRONMENT_KEY).is_none() {
                    self.discover(
                        EnvironmentHost::local(),
                        ReadOptions {
                            automatic: false,
                            interactive: false,
                            quiet: false,
                        },
                        cx,
                    );
                }
            }
            PanelScope::Closed => {}
        }
        cx.notify();
    }

    /// Toggle the panel from a sidebar mark: open on `key`, or close when it already shows it.
    pub fn toggle_host(&mut self, key: &str, cx: &mut Context<Self>) {
        let scope = if self.panel.host_key() == Some(key) {
            PanelScope::Closed
        } else {
            PanelScope::Host(key.to_string())
        };
        self.set_panel(scope, cx);
    }

    /// Open the panel on one host, reading it (and connecting, asking as needed) if nothing is
    /// known yet (`showHostEnvironments`).
    pub fn show_host(&mut self, host: EnvironmentHost, cx: &mut Context<Self>) {
        let patch = if self.store.get(&host.key).is_some() {
            EntryPatch {
                automatic: Some(false),
                ..EntryPatch::default()
            }
        } else {
            EntryPatch {
                status: Some(EnvironmentStatus::Offline),
                automatic: Some(false),
                ..EntryPatch::default()
            }
        };
        self.store.update(&host, patch);
        self.set_panel(PanelScope::Host(host.key), cx);
        cx.notify();
    }

    /// Whether the Kubus desktop app is installed (asked once, when the panel first opens).
    pub fn kubus(&self) -> bool {
        self.kubus == Some(true)
    }

    fn release_dials(&mut self, cx: &mut Context<Self>) {
        for (_, dial) in self.dials.drain() {
            dial.handle.close();
        }
        self.forget_disconnected_hosts(None, cx);
    }

    // -----------------------------------------------------------------------
    // Reading
    // -----------------------------------------------------------------------

    /// Where to run a host's tools. `interactive` may dial the host, asking for passwords
    /// through the window; otherwise only an existing transport counts (`environmentSource`).
    pub fn source(
        &mut self,
        host: &EnvironmentHost,
        interactive: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<Option<EnvironmentSource>, String>> {
        let Some(profile) = host.profile.clone() else {
            return Task::ready(Ok(Some(EnvironmentSource::Local)));
        };
        if let Some(conn_id) = live_connection_for(&self.input.tabs, host) {
            return Task::ready(Ok(Some(EnvironmentSource::ssh(conn_id))));
        }
        if let Some(dial) = self.dials.get(&host.key) {
            return Task::ready(Ok(Some(EnvironmentSource::ssh(dial.conn_id.clone()))));
        }
        if !interactive || !self.panel.is_open() {
            return Task::ready(Ok(None));
        }
        let key = host.key.clone();
        let keepalive = self.input.keepalive_seconds;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let (handle, conn_id) = dial(&backend, &profile, keepalive, &this, cx).await?;
            let handle = Arc::new(handle);
            this.update(cx, |this, cx| {
                this.dials.insert(
                    key.clone(),
                    Dial {
                        conn_id: conn_id.clone(),
                        handle: handle.clone(),
                    },
                );
                // When the transport goes away, so does the dial.
                let watched = handle.clone();
                let closed_key = key.clone();
                let closed_conn = conn_id.clone();
                cx.spawn(async move |this, cx| {
                    while let Some(event) = watched.next_event().await {
                        if matches!(event, SessionEvent::Closed { .. }) {
                            break;
                        }
                    }
                    let _ = this.update(cx, |this, _| {
                        if this
                            .dials
                            .get(&closed_key)
                            .is_some_and(|dial| dial.conn_id == closed_conn)
                        {
                            this.dials.remove(&closed_key);
                        }
                    });
                })
                .detach();
            })
            .map_err(|_| "the window closed".to_string())?;
            Ok(Some(EnvironmentSource::ssh(conn_id)))
        })
    }

    /// Forget a dial whose transport went away, so the next request dials again.
    fn drop_dial(&mut self, key: &str, conn_id: &str) {
        if self
            .dials
            .get(key)
            .is_some_and(|dial| dial.conn_id == conn_id)
            && let Some(dial) = self.dials.remove(key)
        {
            dial.handle.close();
        }
    }

    /// Read what a host runs and store it. Concurrent requests for one host share a reading
    /// (`discoverHost`).
    fn discover(&mut self, host: EnvironmentHost, options: ReadOptions, cx: &mut Context<Self>) {
        if !self.reading.insert(host.key.clone()) {
            return;
        }
        let existing = self.store.get(&host.key).map(|entry| entry.automatic);
        // An explicit request keeps the host listed even if it was found automatically.
        let keep_automatic = options.automatic && existing.unwrap_or(true);
        let patch = if options.quiet && existing.is_some() {
            EntryPatch {
                automatic: Some(keep_automatic),
                ..EntryPatch::default()
            }
        } else {
            EntryPatch {
                automatic: Some(keep_automatic),
                ..EntryPatch::status(EnvironmentStatus::Loading, None)
            }
        };
        self.store.update(&host, patch);
        cx.notify();
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let patch = read_host(&backend, &host, options, &this, cx).await;
            let _ = this.update(cx, |this, cx| {
                this.reading.remove(&host.key);
                // A host forgotten while it was read stays forgotten.
                if this.store.get(&host.key).is_some() {
                    this.store.update(&host, patch);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// Dial `profile` without a terminal, answering its questions through the window
/// (`dialProfile`).
async fn dial(
    backend: &Backend,
    profile: &SshProfile,
    keepalive_seconds: f64,
    this: &WeakEntity<EnvironmentsHub>,
    cx: &mut AsyncApp,
) -> Result<(SessionHandle, String), String> {
    let mut profile = SshProfile {
        environment: None,
        ..profile.clone()
    };
    // A disabled keepalive is left out, so a stale value can never outvote it.
    profile.keepalive_interval_seconds =
        (keepalive_seconds > 0.).then(|| keepalive_seconds.round() as u32);
    let handle = backend.open_session(TerminalClientMessage::Dial { profile });
    let mut last_status = String::new();
    let failed = |last: &str, otherwise: &str| {
        if last.is_empty() {
            otherwise.to_string()
        } else {
            last.to_string()
        }
    };
    loop {
        let Some(event) = handle.next_event().await else {
            return Err(failed(
                &last_status,
                "connection closed before it was ready",
            ));
        };
        match event {
            SessionEvent::Message(TerminalServerMessage::Status { message, .. }) => {
                last_status = message;
            }
            SessionEvent::Message(TerminalServerMessage::AuthPrompt(info)) => {
                match ask(this, cx, DialPrompt::Auth(info)).await {
                    Some(DialAnswer::Auth(Some(response))) => {
                        handle.send(TerminalClientMessage::AuthResponse(response))
                    }
                    _ => {
                        handle.close();
                        return Err(failed(
                            &last_status,
                            "connection closed before it was ready",
                        ));
                    }
                }
            }
            SessionEvent::Message(TerminalServerMessage::HostKey(challenge)) => {
                let accept = matches!(
                    ask(this, cx, DialPrompt::HostKey(challenge)).await,
                    Some(DialAnswer::HostKey(true))
                );
                handle.send(TerminalClientMessage::HostKeyResponse { accept });
            }
            SessionEvent::Message(TerminalServerMessage::Ready { conn_id, .. }) => {
                return Ok((handle, conn_id));
            }
            SessionEvent::Message(TerminalServerMessage::Exit { message, .. }) => {
                let message = message.filter(|message| !message.is_empty());
                return Err(message.unwrap_or_else(|| failed(&last_status, "connection failed")));
            }
            SessionEvent::Closed { .. } => {
                return Err(failed(
                    &last_status,
                    "connection closed before it was ready",
                ));
            }
            _ => {}
        }
    }
}

/// Ask the window a dial's question and wait for the answer; `None` when nobody answers.
async fn ask(
    this: &WeakEntity<EnvironmentsHub>,
    cx: &mut AsyncApp,
    prompt: DialPrompt,
) -> Option<DialAnswer> {
    let (reply, answer) = async_channel::bounded(1);
    this.update(cx, |_, cx| {
        cx.emit(EnvironmentsHubEvent::Prompt { prompt, reply })
    })
    .ok()?;
    answer.recv().await.ok()
}

/// One reading of a host, as the patch to store (`readHost`).
async fn read_host(
    backend: &Backend,
    host: &EnvironmentHost,
    options: ReadOptions,
    this: &WeakEntity<EnvironmentsHub>,
    cx: &mut AsyncApp,
) -> EntryPatch {
    for _attempt in 0..2 {
        let source = match this.update(cx, |this, cx| this.source(host, options.interactive, cx)) {
            Ok(task) => task.await,
            Err(_) => return EntryPatch::default(),
        };
        let source = match source {
            Err(message) => return EntryPatch::status(EnvironmentStatus::Offline, Some(message)),
            Ok(None) => {
                return EntryPatch::status(
                    EnvironmentStatus::Offline,
                    Some("Not connected.".into()),
                );
            }
            Ok(Some(source)) => source,
        };
        match backend
            .environments(source.clone(), options.automatic)
            .await
        {
            Ok(EnvironmentsResponse::Unsupported { message }) => {
                return EntryPatch::status(EnvironmentStatus::Unsupported, Some(message));
            }
            Ok(EnvironmentsResponse::Ok {
                sampled_at,
                environments,
                problems,
            }) => {
                return EntryPatch {
                    status: Some(EnvironmentStatus::Ready),
                    environments: Some(environments),
                    problems: Some(problems),
                    sampled_at: Some(Some(sampled_at)),
                    ..EntryPatch::default()
                };
            }
            // The transport closed under us: try once more over another one.
            Err(error) if error.status == 404 => {
                if let EnvironmentSource::Ssh { conn_id } = &source {
                    let key = host.key.clone();
                    let conn_id = conn_id.clone();
                    let _ = this.update(cx, |this, _| this.drop_dial(&key, &conn_id));
                    continue;
                }
                return EntryPatch::status(EnvironmentStatus::Error, Some(error.message));
            }
            Err(error) => return EntryPatch::status(EnvironmentStatus::Error, Some(error.message)),
        }
    }
    EntryPatch::status(
        EnvironmentStatus::Offline,
        Some("The connection closed.".into()),
    )
}
