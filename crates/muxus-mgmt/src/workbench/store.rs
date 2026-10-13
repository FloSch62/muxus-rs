//! Everything one NETCONF or gNMI tab remembers: the request being edited,
//! every run with its result, and the explorer tree. It belongs to the tab,
//! not the connection, so a reconnect keeps drafts, results and history
//! (port of `client/src/management/workbench-store.ts`).
//!
//! The state sits behind a lock; every change bumps a revision that views
//! watch, the way React re-renders on a zustand update.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use muxus_core::gnmi_path::GnmiPath;
use muxus_core::management::{ManagementError, ManagementProtocol, ManagementResult};
use serde_json::Value;
use tokio::sync::watch;

use super::data_tree::{DataNode, KeyRegistry, create_root};
use super::live_table::LiveTable;
use super::requests::{ManagementDraft, default_gnmi_draft, default_netconf_draft};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunStatus {
    Running,
    Streaming,
    Ok,
    Error,
    Cancelled,
}

/// One run of a request, with its result.
#[derive(Debug, Clone)]
pub struct RunRecord {
    pub id: String,
    pub draft: ManagementDraft,
    /// Short label of the operation: "Get", "Subscribe", "edit-config", …
    pub label: String,
    pub summary: String,
    /// Milliseconds since the epoch.
    pub started_at: u64,
    pub status: RunStatus,
    pub duration_ms: Option<u64>,
    pub bytes: Option<u64>,
    pub error: Option<ManagementError>,
    pub result: Option<ManagementResult>,
    /// Subscriptions: the live values, updated in place.
    pub live: Option<Arc<Mutex<LiveTable>>>,
    /// Subscriptions: the device finished sending the initial state.
    pub synced: bool,
    /// Set by internal helpers (explorer, candidate checks) that stay out of history.
    pub hidden: bool,
}

impl RunRecord {
    pub fn is_active(&self) -> bool {
        matches!(self.status, RunStatus::Running | RunStatus::Streaming)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NavigatorTab {
    #[default]
    Explore,
    Library,
    History,
    Device,
}

/// What the gNMI workbench shows: data (gNMI), operations (gNOI) or security (gNSI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WorkbenchArea {
    #[default]
    Data,
    Operations,
    Security,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ResultView {
    #[default]
    Tree,
    Table,
    Raw,
}

/// gNMI: the data type to browse. NETCONF: configuration only, or with state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExplorerScope {
    Config,
    All,
}

#[derive(Debug, Clone)]
pub struct ExplorerState {
    pub root: DataNode,
    pub registry: KeyRegistry,
    pub expanded: HashSet<String>,
    pub loading: HashSet<String>,
    pub errors: HashMap<String, String>,
    pub selected: Option<String>,
    pub scope: ExplorerScope,
    /// gNMI: the device ignored or rejected the depth extension.
    pub no_depth: bool,
    /// Bumped on every in-place change of the tree or its sets.
    pub version: u64,
    /// Bumped when the tree is replaced, so work for an old tree stands down.
    pub generation: u64,
}

impl ExplorerState {
    fn fresh(scope: ExplorerScope, generation: u64) -> Self {
        Self {
            root: create_root(GnmiPath::default()),
            registry: KeyRegistry::default(),
            expanded: HashSet::from([String::new()]),
            loading: HashSet::new(),
            errors: HashMap::new(),
            selected: None,
            scope,
            no_depth: false,
            version: 0,
            generation,
        }
    }
}

/// NETCONF candidate datastore: uncommitted changes, as last checked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CandidateState {
    pub dirty: bool,
    pub checking: bool,
    pub checked_at: Option<u64>,
    pub error: Option<String>,
}

/// A NETCONF notification received after create-subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedNotification {
    pub xml: String,
    pub event_time: Option<String>,
    pub received_at: String,
}

/// The tool open in each tools area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTools {
    pub operations: String,
    pub security: String,
}

#[derive(Debug, Clone)]
pub struct WorkbenchState {
    pub protocol: ManagementProtocol,
    pub draft: ManagementDraft,
    pub runs: Vec<RunRecord>,
    pub selected_run_id: Option<String>,
    pub navigator: NavigatorTab,
    pub navigator_open: bool,
    pub result_view: ResultView,
    pub explorer: ExplorerState,
    pub candidate: Option<CandidateState>,
    /// A confirmed commit waiting to be confirmed, with its deadline (ms since the epoch).
    pub confirm_deadline: Option<u64>,
    /// gNMI: the commit-confirmed id the confirmation or rollback must name.
    pub commit_id: Option<String>,
    /// Datastores this session holds a lock on.
    pub locks: Vec<String>,
    /// The SSH login banner the device showed, if any.
    pub banner: Option<String>,
    pub notifications: Vec<ReceivedNotification>,
    pub area: WorkbenchArea,
    pub tool: OpenTools,
    /// Each tool's own state (form, last result), kept while you look at another.
    pub tools: HashMap<String, Value>,
}

const MAX_RUNS: usize = 100;

impl WorkbenchState {
    pub fn new(protocol: ManagementProtocol) -> Self {
        Self {
            protocol,
            draft: match protocol {
                ManagementProtocol::Gnmi => ManagementDraft::Gnmi(default_gnmi_draft()),
                ManagementProtocol::Netconf => ManagementDraft::Netconf(default_netconf_draft()),
            },
            runs: Vec::new(),
            selected_run_id: None,
            navigator: NavigatorTab::Explore,
            navigator_open: true,
            result_view: ResultView::Tree,
            explorer: ExplorerState::fresh(
                match protocol {
                    ManagementProtocol::Gnmi => ExplorerScope::All,
                    ManagementProtocol::Netconf => ExplorerScope::Config,
                },
                0,
            ),
            candidate: None,
            confirm_deadline: None,
            commit_id: None,
            locks: Vec::new(),
            banner: None,
            notifications: Vec::new(),
            area: WorkbenchArea::Data,
            tool: OpenTools {
                operations: "ping".into(),
                security: "authz".into(),
            },
            tools: HashMap::new(),
        }
    }

    pub fn set_tool(&mut self, area: WorkbenchArea, tool: &str) {
        match area {
            WorkbenchArea::Operations => self.tool.operations = tool.to_owned(),
            WorkbenchArea::Security => self.tool.security = tool.to_owned(),
            WorkbenchArea::Data => {}
        }
    }

    pub fn set_tool_state(&mut self, key: &str, update: impl FnOnce(Option<&Value>) -> Value) {
        let next = update(self.tools.get(key));
        self.tools.insert(key.to_owned(), next);
    }

    pub fn add_run(&mut self, run: RunRecord) {
        let hidden = run.hidden;
        let id = run.id.clone();
        self.runs.insert(0, run);
        // Drop the oldest finished runs; live subscriptions stay.
        while self.runs.len() > MAX_RUNS {
            let index = self
                .runs
                .iter()
                .rposition(|candidate| candidate.status == RunStatus::Ok)
                .unwrap_or(self.runs.len() - 1);
            self.runs.remove(index);
        }
        if !hidden {
            self.selected_run_id = Some(id);
        }
    }

    pub fn update_run(&mut self, id: &str, patch: impl FnOnce(&mut RunRecord)) {
        if let Some(run) = self.runs.iter_mut().find(|run| run.id == id) {
            patch(run);
        }
    }

    pub fn run(&self, id: &str) -> Option<&RunRecord> {
        self.runs.iter().find(|run| run.id == id)
    }

    pub fn clear_history(&mut self) {
        self.runs.retain(RunRecord::is_active);
        if !self
            .runs
            .iter()
            .any(|run| Some(&run.id) == self.selected_run_id.as_ref())
        {
            self.selected_run_id = None;
        }
    }

    pub fn set_navigator(&mut self, tab: NavigatorTab) {
        self.navigator = tab;
        self.navigator_open = true;
    }

    /// Change the explorer in place and bump its version.
    pub fn touch_explorer<R>(&mut self, update: impl FnOnce(&mut ExplorerState) -> R) -> R {
        let result = update(&mut self.explorer);
        self.explorer.version += 1;
        result
    }

    pub fn reset_explorer(&mut self, scope: Option<ExplorerScope>) {
        let scope = scope.unwrap_or(self.explorer.scope);
        let generation = self.explorer.generation + 1;
        self.explorer = ExplorerState::fresh(scope, generation);
    }
}

/// The shared state of one tab plus a revision views can watch.
#[derive(Clone)]
pub struct WorkbenchStore {
    state: Arc<Mutex<WorkbenchState>>,
    revision: Arc<watch::Sender<u64>>,
}

impl WorkbenchStore {
    pub fn new(protocol: ManagementProtocol) -> Self {
        Self {
            state: Arc::new(Mutex::new(WorkbenchState::new(protocol))),
            revision: Arc::new(watch::channel(0).0),
        }
    }

    /// Read the state.
    pub fn read<R>(&self, read: impl FnOnce(&WorkbenchState) -> R) -> R {
        read(&self.lock())
    }

    /// Change the state and tell watchers.
    pub fn update<R>(&self, update: impl FnOnce(&mut WorkbenchState) -> R) -> R {
        let result = update(&mut self.lock());
        self.revision.send_modify(|revision| *revision += 1);
        result
    }

    /// Resolves on every change.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
    }

    pub fn revision(&self) -> u64 {
        *self.revision.borrow()
    }

    fn lock(&self) -> MutexGuard<'_, WorkbenchState> {
        self.state.lock().expect("workbench state lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, status: RunStatus) -> RunRecord {
        RunRecord {
            id: id.into(),
            draft: ManagementDraft::Gnmi(default_gnmi_draft()),
            label: "Get".into(),
            summary: "/".into(),
            started_at: 0,
            status,
            duration_ms: None,
            bytes: None,
            error: None,
            result: None,
            live: None,
            synced: false,
            hidden: false,
        }
    }

    #[test]
    fn keeps_a_bounded_history_without_dropping_live_runs() {
        let store = WorkbenchStore::new(ManagementProtocol::Gnmi);
        store.update(|state| {
            state.add_run(run("live", RunStatus::Streaming));
            for index in 0..MAX_RUNS + 5 {
                state.add_run(run(&format!("r{index}"), RunStatus::Ok));
            }
        });
        store.read(|state| {
            assert_eq!(state.runs.len(), MAX_RUNS);
            assert!(state.run("live").is_some());
            assert_eq!(state.selected_run_id.as_deref(), Some("r104"));
        });
        store.update(WorkbenchState::clear_history);
        store.read(|state| {
            assert_eq!(state.runs.len(), 1);
            assert_eq!(state.selected_run_id, None);
        });
        assert!(store.revision() >= 2);
        let generation = store.read(|state| state.explorer.generation);
        store.update(|state| state.reset_explorer(Some(ExplorerScope::Config)));
        store.read(|state| {
            assert_eq!(state.explorer.generation, generation + 1);
            assert_eq!(state.explorer.scope, ExplorerScope::Config);
        });
    }
}
