//! The workbench's verbs: run a draft (recording it in the tab's history),
//! cancel, load explorer nodes and keep the NETCONF candidate state current.
//! Views call these; nothing here touches the UI (port of
//! `client/src/management/controller.ts`).
//!
//! Explorer nodes are addressed by id: a node is looked up again after
//! every round trip, so work for a tree that was replaced meanwhile finds
//! nothing and stands down.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use muxus_core::gnmi_path::{format_gnmi_path, parse_gnmi_path, strip_module_prefix};
use muxus_core::management::{
    GnmiDataType, GnmiNotification, GnmiSessionInfo, GnmiSubscriptionListMode,
    GnmiSubscriptionMode, ManagementError, ManagementResult, ManagementSessionInfo,
    NetconfSessionInfo,
};
use muxus_core::protocol::{GnmiCommit, GnmiSubscription, ManagementProfile};
use regex::Regex;
use std::sync::LazyLock;
use tokio::runtime::Handle;

use super::connection::{
    Canceller, LiveSubscription, ManagementRequestError, RequestMessage, SubscribeMessage,
    SubscriptionUpdate, WorkbenchTransport,
};
use super::data_tree::{
    DataNode, DataNodeKind, KeyRegistry, create_root, find_node, find_node_mut, merge_json,
    merge_xml, namespace_chain, node_at, schema_path_of,
};
use super::live_table::LiveTable;
use super::requests::{
    Datastore, FilterType, GnmiDraft, GnmiOperation, ManagementDraft, NetconfDraft,
    NetconfOperation, default_gnmi_draft, default_netconf_draft, draft_summary, gnmi_get_message,
    gnmi_set_message, gnmi_subscribe_message, netconf_operation_label, netconf_rpc_body,
    now_base36, now_ms,
};
use super::store::{CandidateState, ExplorerScope, RunRecord, RunStatus, WorkbenchStore};
use super::xml::{parse_rpc_reply, pretty_xml, serialize_element, subtree_filter_for};
use muxus_core::gnmi_path::GnmiPath;

static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

fn run_id() -> String {
    let counter = RUN_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("run{}{counter}", now_base36())
}

pub fn run_label(draft: &ManagementDraft) -> String {
    match draft {
        ManagementDraft::Gnmi(draft) => match draft.operation {
            GnmiOperation::Subscribe => match draft.subscribe.list_mode {
                GnmiSubscriptionListMode::Once => "Subscribe once",
                GnmiSubscriptionListMode::Poll => "Poll",
                GnmiSubscriptionListMode::Stream => "Subscribe",
            },
            GnmiOperation::Set => "Set",
            GnmiOperation::Get => "Get",
        }
        .into(),
        ManagementDraft::Netconf(draft) => netconf_operation_label(draft.operation).into(),
    }
}

/// Explorer requests stop once a response grows past this; the user narrows the path instead.
const EXPLORER_MAX_BYTES: u64 = 24 * 1024 * 1024;
/// Depths the explorer looks at a gNMI node with, in turn; 0 is everything
/// below it. The depth extension returns only leaves at its last level, so a
/// branch whose first values sit deeper does not show at all: SR OS hides its
/// whole top level at depth 2, SR Linux a network instance's protocols.
const EXPLORER_DEPTHS: [u32; 4] = [2, 3, 4, 0];
/// A deeper look at a node gives up past this, keeping what is shown.
const EXPLORER_DEEPER_BYTES: u64 = 4 * 1024 * 1024;
/// A deeper look this large that turns up nothing new is the last one.
const EXPLORER_ENOUGH_BYTES: u64 = 1024 * 1024;
/// How long a key discovery waits for the device.
const DISCOVER_KEYS_TIMEOUT: Duration = Duration::from_secs(8);

static NO_DEPTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)depth|extension|unimplemented|not supported").expect("valid regex")
});

/// Commit-confirmed follow-ups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitAction {
    Confirm,
    Cancel,
}

fn request_error(err: &ManagementRequestError) -> ManagementError {
    err.error.clone()
}

/// The workbench's verbs on one tab's state and connection.
pub struct WorkbenchController<T: WorkbenchTransport> {
    pub store: WorkbenchStore,
    connection: Mutex<Option<Arc<T>>>,
    pub profile: ManagementProfile,
    pub info: Option<ManagementSessionInfo>,
    runtime: Handle,
    pending: Mutex<HashMap<String, Canceller>>,
    live: Mutex<HashMap<String, (Canceller, Canceller)>>,
    /// The latest look at each explorer node, so an older deeper look stands down.
    looks: Mutex<HashMap<String, u64>>,
    candidate_check: AtomicU64,
}

impl<T: WorkbenchTransport> WorkbenchController<T> {
    pub fn new(
        store: WorkbenchStore,
        connection: Option<Arc<T>>,
        profile: ManagementProfile,
        info: Option<ManagementSessionInfo>,
        runtime: Handle,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            connection: Mutex::new(connection),
            profile,
            info,
            runtime,
            pending: Mutex::default(),
            live: Mutex::default(),
            looks: Mutex::default(),
            candidate_check: AtomicU64::new(0),
        })
    }

    fn connection(&self) -> Option<Arc<T>> {
        self.connection
            .lock()
            .expect("connection lock")
            .clone()
            .filter(|connection| !connection.is_closed())
    }

    pub fn connected(&self) -> bool {
        self.connection().is_some() && self.info.is_some()
    }

    /// The connection went away: running requests end with it, history stays.
    pub fn detach(&self) {
        *self.connection.lock().expect("connection lock") = None;
        self.store.update(|state| {
            for run in &mut state.runs {
                if run.is_active() {
                    run.status = RunStatus::Error;
                    if run.error.is_none() {
                        run.error = Some(ManagementError {
                            message: "The session closed.".into(),
                            code: None,
                            cancelled: None,
                        });
                    }
                }
            }
        });
        self.pending.lock().expect("pending lock").clear();
        self.live.lock().expect("live lock").clear();
    }

    pub fn gnmi_info(&self) -> Option<&GnmiSessionInfo> {
        match &self.info {
            Some(ManagementSessionInfo::Gnmi(info)) => Some(info),
            _ => None,
        }
    }

    pub fn netconf_info(&self) -> Option<&NetconfSessionInfo> {
        match &self.info {
            Some(ManagementSessionInfo::Netconf(info)) => Some(info),
            _ => None,
        }
    }

    /// The live connection, for gNOI and gNSI tools; `None` while disconnected.
    pub fn connection_for_tools(&self) -> Option<Arc<T>> {
        if self.connected() {
            self.connection()
        } else {
            None
        }
    }

    /// Whether the device offers a gRPC service. Devices without reflection do
    /// not say, so everything counts as offered and the device answers for itself.
    pub fn has_service(&self, service: &str) -> bool {
        match self.gnmi_info().and_then(|info| info.services.as_ref()) {
            Some(services) => services.iter().any(|name| name == service),
            None => true,
        }
    }

    pub fn has_capability(&self, fragment: &str) -> bool {
        self.netconf_info().is_some_and(|info| {
            info.capabilities
                .iter()
                .any(|capability| capability.contains(fragment))
        })
    }

    fn is_gnmi(&self) -> bool {
        matches!(self.profile, ManagementProfile::Gnmi(_))
    }

    /// Run the editor's draft (or another one) and record it.
    pub fn run(
        self: &Arc<Self>,
        draft: Option<ManagementDraft>,
        hidden: bool,
    ) -> Option<RunRecord> {
        let connection = self.connection()?;
        let draft = draft.unwrap_or_else(|| self.store.read(|state| state.draft.clone()));
        let record = RunRecord {
            id: run_id(),
            draft: draft.clone(),
            label: run_label(&draft),
            summary: draft_summary(&draft),
            started_at: now_ms(),
            status: RunStatus::Running,
            duration_ms: None,
            bytes: None,
            error: None,
            result: None,
            live: None,
            synced: false,
            hidden,
        };
        self.store.update(|state| state.add_run(record.clone()));
        if let ManagementDraft::Gnmi(gnmi) = &draft
            && gnmi.operation == GnmiOperation::Subscribe
        {
            self.start_subscription(&record, gnmi, &connection);
            return Some(record);
        }
        let commit_id = match &draft {
            ManagementDraft::Gnmi(gnmi)
                if gnmi.operation == GnmiOperation::Set
                    && gnmi.confirm_seconds.is_some_and(|s| s > 0) =>
            {
                Some(format!("muxus-{}", record.id))
            }
            _ => None,
        };
        let message = match &draft {
            ManagementDraft::Gnmi(gnmi) if gnmi.operation == GnmiOperation::Set => {
                set_request(gnmi_set_message(gnmi, commit_id.as_deref()))
            }
            ManagementDraft::Gnmi(gnmi) => get_request(gnmi, None),
            ManagementDraft::Netconf(netconf) => RequestMessage::NetconfRpc {
                xml: netconf_rpc_body(netconf),
                timeout_ms: None,
            },
        };
        let pending = connection.request(message);
        self.pending
            .lock()
            .expect("pending lock")
            .insert(record.id.clone(), pending.cancel.clone());
        let controller = self.clone();
        let id = record.id.clone();
        self.runtime.spawn(async move {
            let outcome = pending.outcome.await;
            controller.pending.lock().expect("pending lock").remove(&id);
            match outcome {
                Ok(outcome) => {
                    controller.store.update(|state| {
                        state.update_run(&id, |run| {
                            run.status = RunStatus::Ok;
                            run.result = Some(outcome.result.clone());
                            run.duration_ms = Some(outcome.duration_ms);
                            run.bytes = Some(outcome.bytes);
                        });
                    });
                    match &draft {
                        ManagementDraft::Netconf(netconf) => {
                            controller.after_netconf(netconf, &outcome.result);
                        }
                        ManagementDraft::Gnmi(gnmi) => match &outcome.result {
                            ManagementResult::GnmiGet { notifications } => {
                                controller.learn_keys(notifications);
                            }
                            ManagementResult::GnmiSet { .. } => {
                                if let (Some(commit_id), Some(seconds)) =
                                    (commit_id, gnmi.confirm_seconds)
                                {
                                    controller.store.update(|state| {
                                        state.confirm_deadline =
                                            Some(now_ms() + u64::from(seconds) * 1000);
                                        state.commit_id = Some(commit_id);
                                    });
                                }
                                let paths: Vec<String> = gnmi
                                    .set
                                    .iter()
                                    .map(|item| muxus_core::js::js_trim(&item.path).to_owned())
                                    .filter(|path| !path.is_empty())
                                    .collect();
                                controller.refresh_explorer(&paths);
                            }
                            _ => {}
                        },
                    }
                }
                Err(err) => {
                    controller.store.update(|state| {
                        state.update_run(&id, |run| {
                            run.status = if err.error.cancelled == Some(true) {
                                RunStatus::Cancelled
                            } else {
                                RunStatus::Error
                            };
                            run.error = Some(request_error(&err));
                            run.duration_ms = Some(err.duration_ms);
                        });
                    });
                }
            }
        });
        Some(record)
    }

    /// Confirm a gNMI commit-confirmed Set, or roll it back before its timer does.
    pub fn finish_gnmi_commit(self: &Arc<Self>, action: CommitAction) {
        let Some(connection) = self.connection() else {
            return;
        };
        let (commit_id, confirm_deadline) = self
            .store
            .read(|state| (state.commit_id.clone(), state.confirm_deadline));
        let Some(commit_id) = commit_id else {
            return;
        };
        let draft = GnmiDraft {
            operation: GnmiOperation::Set,
            set: Vec::new(),
            ..default_gnmi_draft()
        };
        let record = RunRecord {
            id: run_id(),
            draft: ManagementDraft::Gnmi(draft),
            label: match action {
                CommitAction::Confirm => "Confirm commit",
                CommitAction::Cancel => "Roll back",
            }
            .into(),
            summary: commit_id.clone(),
            started_at: now_ms(),
            status: RunStatus::Running,
            duration_ms: None,
            bytes: None,
            error: None,
            result: None,
            live: None,
            synced: false,
            hidden: false,
        };
        self.store.update(|state| state.add_run(record.clone()));
        let pending = connection.request(RequestMessage::GnmiSet {
            prefix: None,
            updates: Vec::new(),
            replaces: Vec::new(),
            deletes: Vec::new(),
            commit: Some(match action {
                CommitAction::Confirm => GnmiCommit::Confirm { id: commit_id },
                CommitAction::Cancel => GnmiCommit::Cancel { id: commit_id },
            }),
        });
        let controller = self.clone();
        self.runtime.spawn(async move {
            match pending.outcome.await {
                Ok(outcome) => {
                    controller.store.update(|state| {
                        state.update_run(&record.id, |run| {
                            run.status = RunStatus::Ok;
                            run.result = Some(outcome.result);
                            run.duration_ms = Some(outcome.duration_ms);
                            run.bytes = Some(outcome.bytes);
                        });
                        state.confirm_deadline = None;
                        state.commit_id = None;
                    });
                    if action == CommitAction::Cancel {
                        controller.refresh_explorer(&[]);
                    }
                }
                Err(err) => {
                    controller.store.update(|state| {
                        state.update_run(&record.id, |run| {
                            run.status = RunStatus::Error;
                            run.error = Some(request_error(&err));
                        });
                        // Past the deadline the device rolled back on its own.
                        if confirm_deadline.is_some_and(|deadline| now_ms() > deadline) {
                            state.confirm_deadline = None;
                            state.commit_id = None;
                        }
                    });
                }
            }
        });
    }

    /// Run and wait, outside of history: explorer loads, candidate checks, review fetches.
    pub async fn request(
        &self,
        draft: &ManagementDraft,
        max_bytes: Option<u64>,
    ) -> Result<ManagementResult, ManagementRequestError> {
        let connection = self
            .connection()
            .ok_or_else(|| ManagementRequestError::new("Not connected."))?;
        let message = match draft {
            ManagementDraft::Gnmi(gnmi) if gnmi.operation == GnmiOperation::Set => {
                set_request(gnmi_set_message(gnmi, None))
            }
            ManagementDraft::Gnmi(gnmi) => get_request(gnmi, max_bytes),
            ManagementDraft::Netconf(netconf) => RequestMessage::NetconfRpc {
                xml: netconf_rpc_body(netconf),
                timeout_ms: None,
            },
        };
        Ok(connection.request(message).outcome.await?.result)
    }

    fn start_subscription(
        self: &Arc<Self>,
        record: &RunRecord,
        draft: &GnmiDraft,
        connection: &Arc<T>,
    ) {
        let table = Arc::new(Mutex::new(LiveTable::new()));
        let started = Instant::now();
        let id = record.id.clone();
        self.store.update(|state| {
            state.update_run(&id, |run| {
                run.live = Some(table.clone());
                run.status = RunStatus::Streaming;
            });
        });
        let (request, encoding) = gnmi_subscribe_message(draft);
        let mut subscription = connection.subscribe(SubscribeMessage {
            prefix: request.prefix,
            mode: request.mode,
            encoding,
            updates_only: request.updates_only.then_some(true),
            subscriptions: request.subscriptions,
        });
        self.live.lock().expect("live lock").insert(
            id.clone(),
            (subscription.poll.clone(), subscription.cancel.clone()),
        );
        let once = draft.subscribe.list_mode == GnmiSubscriptionListMode::Once;
        let controller = self.clone();
        self.runtime.spawn(async move {
            while let Some(update) = subscription.updates.recv().await {
                match update {
                    SubscriptionUpdate::Notifications(notifications, bytes) => {
                        table
                            .lock()
                            .expect("table lock")
                            .apply(&notifications, bytes);
                        controller.store.update(|_| {});
                    }
                    SubscriptionUpdate::Sync => {
                        table.lock().expect("table lock").mark_synced();
                        controller.store.update(|state| {
                            state.update_run(&id, |run| {
                                run.synced = true;
                                if once {
                                    // ONCE ends right after the sync; keep the duration for the header.
                                    run.duration_ms = Some(started.elapsed().as_millis() as u64);
                                }
                            });
                        });
                    }
                    SubscriptionUpdate::End(error) => {
                        controller.live.lock().expect("live lock").remove(&id);
                        let bytes = table.lock().expect("table lock").bytes;
                        controller.store.update(|state| {
                            state.update_run(&id, |run| {
                                let failed = error
                                    .as_ref()
                                    .is_some_and(|error| error.cancelled != Some(true));
                                run.status = if failed {
                                    RunStatus::Error
                                } else if run.status == RunStatus::Cancelled {
                                    RunStatus::Cancelled
                                } else {
                                    RunStatus::Ok
                                };
                                if failed {
                                    run.error = error.clone();
                                }
                                if run.duration_ms.is_none() {
                                    run.duration_ms = Some(started.elapsed().as_millis() as u64);
                                }
                                run.bytes = Some(bytes);
                            });
                        });
                        break;
                    }
                }
            }
        });
    }

    pub fn poll(&self, run_id: &str) {
        let live = self.live.lock().expect("live lock").get(run_id).cloned();
        if let Some((poll, _)) = live {
            poll();
        }
    }

    pub fn cancel(&self, run_id: &str) {
        let pending = self
            .pending
            .lock()
            .expect("pending lock")
            .get(run_id)
            .cloned();
        if let Some(cancel) = pending {
            cancel();
            return;
        }
        let live = self.live.lock().expect("live lock").get(run_id).cloned();
        if let Some((_, cancel)) = live {
            self.store.update(|state| {
                state.update_run(run_id, |run| run.status = RunStatus::Cancelled);
            });
            cancel();
        }
    }

    pub fn cancel_all(&self) {
        let ids: Vec<String> = self
            .pending
            .lock()
            .expect("pending lock")
            .keys()
            .chain(self.live.lock().expect("live lock").keys())
            .cloned()
            .collect();
        for id in ids {
            self.cancel(&id);
        }
    }

    fn learn_keys(&self, notifications: &[GnmiNotification]) {
        self.store.update(|state| {
            let registry = &mut state.explorer.registry;
            for notification in notifications {
                for update in &notification.updates {
                    // Unparseable paths teach nothing.
                    if let Ok(path) = parse_gnmi_path(&update.path) {
                        registry.learn(&path);
                    }
                }
            }
        });
    }

    // Explorer

    /// Fetch the children of an explorer node (the root when `node_id` is `""`).
    pub async fn load_node(self: &Arc<Self>, node_id: &str) {
        if !self.connected() {
            return;
        }
        let started = self.store.update(|state| {
            if state.explorer.loading.contains(node_id) {
                return false;
            }
            state.touch_explorer(|explorer| {
                explorer.loading.insert(node_id.to_owned());
                explorer.errors.remove(node_id);
            });
            true
        });
        if !started {
            return;
        }
        let generation = self.store.read(|state| state.explorer.generation);
        let result = if self.is_gnmi() {
            self.load_gnmi_node(node_id).await
        } else {
            self.load_netconf_node(node_id).await
        };
        self.store.update(|state| {
            state.touch_explorer(|explorer| {
                explorer.loading.remove(node_id);
                if explorer.generation != generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        if let Some(node) = find_node_mut(&mut explorer.root, node_id) {
                            node.loaded = true;
                        }
                        explorer.expanded.insert(node_id.to_owned());
                    }
                    Err(message) => {
                        explorer.errors.insert(node_id.to_owned(), message);
                    }
                }
            });
        });
    }

    /// Spawn [`load_node`](Self::load_node) without waiting for it.
    pub fn load_node_later(self: &Arc<Self>, node_id: &str) {
        let controller = self.clone();
        let node_id = node_id.to_owned();
        self.runtime
            .spawn(async move { controller.load_node(&node_id).await });
    }

    fn node_path(&self, node_id: &str) -> Option<GnmiPath> {
        self.store
            .read(|state| find_node(&state.explorer.root, node_id).map(|node| node.path.clone()))
    }

    fn child_count(&self, node_id: &str) -> usize {
        self.store.read(|state| {
            find_node(&state.explorer.root, node_id).map_or(0, |node| node.children.len())
        })
    }

    async fn load_gnmi_node(self: &Arc<Self>, node_id: &str) -> Result<(), String> {
        let look = {
            let mut looks = self.looks.lock().expect("looks lock");
            let look = looks.get(node_id).copied().unwrap_or(0) + 1;
            looks.insert(node_id.to_owned(), look);
            look
        };
        let generation = self.store.read(|state| state.explorer.generation);
        if self.store.read(|state| state.explorer.no_depth) {
            let reply = self.explorer_get(node_id, 0, EXPLORER_MAX_BYTES).await?;
            return self.lay_out_gnmi(node_id, generation, &reply.0, true).await;
        }
        let reply = match self
            .explorer_get_raw(node_id, EXPLORER_DEPTHS[0], EXPLORER_MAX_BYTES)
            .await
        {
            Ok(reply) => reply,
            Err(err) => {
                // Devices without the depth extension reject it; browse whole subtrees instead.
                let text = format!(
                    "{} {}",
                    err.error.code.as_deref().unwrap_or_default(),
                    err.error.message
                );
                if NO_DEPTH.is_match(&text) {
                    self.store
                        .update(|state| state.touch_explorer(|explorer| explorer.no_depth = true));
                    let reply = self.explorer_get(node_id, 0, EXPLORER_MAX_BYTES).await?;
                    return self.lay_out_gnmi(node_id, generation, &reply.0, true).await;
                }
                return Err(err.error.message);
            }
        };
        self.lay_out_gnmi(node_id, generation, &reply.0, false)
            .await?;
        // An empty first look usually means everything was left out, as SR OS
        // does with its top level: look deeper before showing nothing.
        let mut step = 1;
        while self.child_count(node_id) == 0 && step < EXPLORER_DEPTHS.len() {
            let depth = EXPLORER_DEPTHS[step];
            let Ok(reply) = self
                .explorer_get(node_id, depth, EXPLORER_DEEPER_BYTES)
                .await
            else {
                return Ok(());
            };
            self.lay_out_gnmi(node_id, generation, &reply.0, depth == 0)
                .await?;
            step += 1;
        }
        if step < EXPLORER_DEPTHS.len() {
            let controller = self.clone();
            let node_id = node_id.to_owned();
            self.runtime.spawn(async move {
                controller
                    .look_deeper(&node_id, look, generation, step)
                    .await;
            });
        }
        Ok(())
    }

    /// Look below a shown node again, deeper each time. Each look adds the
    /// branches a shallower one left out; once everything below the node fits
    /// in one reply, that reply replaces what is shown, complete. It stops at
    /// the first look that grows too large or fails, and at one that is already
    /// large but turns up nothing new.
    async fn look_deeper(self: &Arc<Self>, node_id: &str, look: u64, generation: u64, from: usize) {
        let current = || {
            self.looks.lock().expect("looks lock").get(node_id) == Some(&look)
                && self.store.read(|state| state.explorer.generation) == generation
        };
        for &depth in &EXPLORER_DEPTHS[from..] {
            let Ok((notifications, bytes)) = self
                .explorer_get(node_id, depth, EXPLORER_DEEPER_BYTES)
                .await
            else {
                return;
            };
            if !current() {
                return;
            }
            if depth == 0 {
                let _ = self
                    .lay_out_gnmi(node_id, generation, &notifications, true)
                    .await;
                return;
            }
            self.learn_keys(&notifications);
            let Ok(reply) = self.reply_node(node_id, &notifications) else {
                return;
            };
            let added = self.store.update(|state| {
                let Some(node) = find_node_mut(&mut state.explorer.root, node_id) else {
                    return 0;
                };
                let known: HashSet<String> =
                    node.children.iter().map(|child| child.id.clone()).collect();
                let added: Vec<DataNode> = reply
                    .children
                    .into_iter()
                    .filter(|child| !known.contains(&child.id))
                    .collect();
                let count = added.len();
                if count > 0 {
                    node.children.extend(added);
                    state.explorer.version += 1;
                }
                count
            });
            if added == 0 && bytes > EXPLORER_ENOUGH_BYTES {
                return;
            }
        }
    }

    async fn explorer_get_raw(
        &self,
        node_id: &str,
        depth: u32,
        max_bytes: u64,
    ) -> Result<(Vec<GnmiNotification>, u64), ManagementRequestError> {
        let connection = self
            .connection()
            .ok_or_else(|| ManagementRequestError::new("Not connected."))?;
        let path = self
            .node_path(node_id)
            .ok_or_else(|| ManagementRequestError::new("The node is gone."))?;
        let scope = self.store.read(|state| state.explorer.scope);
        let draft = GnmiDraft {
            paths: vec![format_gnmi_path(&path)],
            data_type: if scope == ExplorerScope::Config {
                GnmiDataType::Config
            } else {
                GnmiDataType::All
            },
            depth: (depth > 0).then_some(depth),
            ..default_gnmi_draft()
        };
        let outcome = connection
            .request(get_request(&draft, Some(max_bytes)))
            .outcome
            .await?;
        Ok(match outcome.result {
            ManagementResult::GnmiGet { notifications } => (notifications, outcome.bytes),
            _ => (Vec::new(), outcome.bytes),
        })
    }

    async fn explorer_get(
        &self,
        node_id: &str,
        depth: u32,
        max_bytes: u64,
    ) -> Result<(Vec<GnmiNotification>, u64), String> {
        self.explorer_get_raw(node_id, depth, max_bytes)
            .await
            .map_err(|err| err.error.message)
    }

    /// The node as a tree built from `notifications` alone shows it.
    fn reply_node(
        &self,
        node_id: &str,
        notifications: &[GnmiNotification],
    ) -> Result<DataNode, String> {
        let path = self
            .node_path(node_id)
            .ok_or_else(|| "The node is gone.".to_owned())?;
        self.store.update(|state| {
            let registry = &mut state.explorer.registry;
            reply_tree(node_id, &path, notifications, registry)
        })
    }

    /// Show what a reply holds below a node; `complete` when it holds everything there is.
    async fn lay_out_gnmi(
        self: &Arc<Self>,
        node_id: &str,
        generation: u64,
        notifications: &[GnmiNotification],
        complete: bool,
    ) -> Result<(), String> {
        self.learn_keys(notifications);
        let lay_out = || -> Result<(), String> {
            let reply = self.reply_node(node_id, notifications)?;
            self.store.update(|state| {
                if state.explorer.generation != generation {
                    return;
                }
                state.touch_explorer(|explorer| {
                    if let Some(node) = find_node_mut(&mut explorer.root, node_id) {
                        node.children = reply.children;
                        if complete {
                            mark_loaded(node);
                        }
                    }
                });
            });
            Ok(())
        };
        lay_out()?;
        // JSON does not say which leaves key a list. Ask the device for keyed
        // paths of the lists it has not shown us yet, then lay the data out again.
        let unkeyed: Vec<(GnmiPath, String, String)> = self.store.read(|state| {
            find_node(&state.explorer.root, node_id)
                .map(|node| {
                    unkeyed_lists(node, &state.explorer.registry)
                        .into_iter()
                        .take(8)
                        .filter_map(|list| {
                            let entry = list.children.first()?;
                            let leaf = entry
                                .children
                                .iter()
                                .find(|child| child.kind == DataNodeKind::Leaf)?;
                            Some((list.path.clone(), schema_path_of(list), leaf.name.clone()))
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
        if unkeyed.is_empty() {
            return Ok(());
        }
        let learned = join_all(
            unkeyed
                .into_iter()
                .map(|(path, schema, leaf)| self.discover_keys(path, schema, leaf)),
        )
        .await;
        if learned.into_iter().any(|learned| learned) {
            lay_out()?;
        }
        Ok(())
    }

    /// Learn a list's key leaves from the paths a one-shot subscription returns.
    async fn discover_keys(&self, list_path: GnmiPath, schema: String, leaf: String) -> bool {
        let Some(connection) = self.connection() else {
            return false;
        };
        let path = format!(
            "{}/{}",
            format_gnmi_path(&list_path),
            strip_module_prefix(&leaf)
        );
        let LiveSubscription {
            mut updates,
            cancel,
            ..
        } = connection.subscribe(SubscribeMessage {
            prefix: None,
            mode: GnmiSubscriptionListMode::Once,
            encoding: None,
            updates_only: None,
            subscriptions: vec![GnmiSubscription {
                path,
                mode: GnmiSubscriptionMode::TargetDefined,
                sample_interval_ms: None,
                suppress_redundant: None,
                heartbeat_interval_ms: None,
            }],
        });
        let known = || {
            self.store
                .read(|state| state.explorer.registry.lookup(&schema).is_some())
        };
        let wait = async {
            while let Some(update) = updates.recv().await {
                match update {
                    SubscriptionUpdate::Notifications(notifications, _) => {
                        self.learn_keys(&notifications);
                        if known() {
                            return true;
                        }
                    }
                    SubscriptionUpdate::Sync | SubscriptionUpdate::End(_) => return known(),
                }
            }
            known()
        };
        let learned = tokio::time::timeout(DISCOVER_KEYS_TIMEOUT, wait)
            .await
            .unwrap_or(false);
        cancel();
        learned
    }

    async fn load_netconf_node(self: &Arc<Self>, node_id: &str) -> Result<(), String> {
        let root = node_id.is_empty();
        let (filter, config) = self.store.read(|state| {
            let chain = namespace_chain(&state.explorer.root, node_id);
            let filter = if root {
                String::new()
            } else {
                subtree_filter_for(&chain, "")
            };
            (filter, state.explorer.scope == ExplorerScope::Config)
        });
        let draft = NetconfDraft {
            operation: if config || root {
                NetconfOperation::GetConfig
            } else {
                NetconfOperation::Get
            },
            filter_type: if filter.is_empty() {
                FilterType::None
            } else {
                FilterType::Subtree
            },
            filter,
            ..default_netconf_draft()
        };
        let result = self
            .request(&ManagementDraft::Netconf(draft), None)
            .await
            .map_err(|err| err.error.message)?;
        let ManagementResult::NetconfRpc { xml, .. } = result else {
            return Ok(());
        };
        let reply = parse_rpc_reply(&xml)
            .ok_or_else(|| "The device sent something that is not an rpc-reply.".to_owned())?;
        if !reply.errors.is_empty() {
            return Err(reply
                .errors
                .iter()
                .map(|error| {
                    error
                        .message
                        .clone()
                        .or_else(|| error.tag.clone())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("; "));
        }
        let Some(data) = reply.data else {
            return Ok(());
        };
        self.store.update(|state| {
            state.touch_explorer(|explorer| {
                if root {
                    explorer.root.children.clear();
                    merge_xml(&mut explorer.root, &data);
                    // With configuration loaded whole, every node is already complete.
                    if config {
                        mark_loaded(&mut explorer.root);
                    }
                    return;
                }
                // The reply repeats the ancestors; merge from the top so they line up.
                let previous = find_node_mut(&mut explorer.root, node_id)
                    .map(|node| std::mem::take(&mut node.children));
                merge_xml(&mut explorer.root, &data);
                if let Some(node) = find_node_mut(&mut explorer.root, node_id) {
                    if node.children.is_empty()
                        && let Some(previous) = previous
                    {
                        node.children = previous;
                    }
                    mark_loaded(node);
                }
            });
        });
        Ok(())
    }

    /// Show what a change did: reload the explorer where it touched the data.
    /// NETCONF configuration loads whole, so the root reloads; for gNMI the
    /// nearest loaded node above each changed path does.
    pub fn refresh_explorer(self: &Arc<Self>, paths: &[String]) {
        let targets: Vec<String> = self.store.read(|state| {
            let explorer = &state.explorer;
            if !explorer.root.loaded {
                return Vec::new();
            }
            if !self.is_gnmi() || paths.is_empty() {
                return vec![String::new()];
            }
            let mut targets: Vec<String> = Vec::new();
            for text in paths {
                let Ok(path) = parse_gnmi_path(text) else {
                    continue;
                };
                let mut node = &explorer.root;
                let mut deepest = &explorer.root;
                for elem in &path.elems {
                    let name = strip_module_prefix(&elem.name);
                    let next = node.children.iter().find(|child| {
                        child.kind != DataNodeKind::Entry
                            && strip_module_prefix(&child.name) == name
                    });
                    let entry = match (next, &elem.keys) {
                        (Some(next), Some(keys)) => next.children.iter().find(|candidate| {
                            keys.iter().all(|(key, value)| {
                                candidate.keys.as_ref().and_then(|keys| keys.get(key))
                                    == Some(value)
                            })
                        }),
                        (next, _) => next,
                    };
                    let Some(entry) = entry else {
                        break;
                    };
                    node = entry;
                    if entry.loaded {
                        deepest = entry;
                    }
                }
                if !targets.contains(&deepest.id) {
                    targets.push(deepest.id.clone());
                }
            }
            targets
        });
        for target in targets {
            self.load_node_later(&target);
        }
    }

    // NETCONF bookkeeping

    fn after_netconf(self: &Arc<Self>, draft: &NetconfDraft, result: &ManagementResult) {
        let ManagementResult::NetconfRpc { xml, .. } = result else {
            return;
        };
        let ok = parse_rpc_reply(xml).is_some_and(|reply| reply.errors.is_empty());
        let target = draft.target.as_str().to_owned();
        self.store.update(|state| {
            if ok && draft.operation == NetconfOperation::Lock && !state.locks.contains(&target) {
                state.locks.push(target.clone());
            }
            if ok && draft.operation == NetconfOperation::Unlock {
                state.locks.retain(|lock| *lock != target);
            }
            if ok && draft.operation == NetconfOperation::Commit {
                state.confirm_deadline = draft
                    .confirmed
                    .then(|| now_ms() + (draft.confirm_timeout.max(1.0) * 1000.0) as u64);
            }
            if ok && draft.operation == NetconfOperation::CancelCommit {
                state.confirm_deadline = None;
            }
        });
        let changes_running = matches!(
            draft.operation,
            NetconfOperation::Commit | NetconfOperation::CancelCommit
        ) || (matches!(
            draft.operation,
            NetconfOperation::EditConfig | NetconfOperation::CopyConfig
        ) && draft.target == Datastore::Running);
        if ok && changes_running {
            self.refresh_explorer(&[]);
        }
        let touches_candidate = (draft.operation == NetconfOperation::EditConfig
            && draft.target == Datastore::Candidate)
            || matches!(
                draft.operation,
                NetconfOperation::Commit
                    | NetconfOperation::DiscardChanges
                    | NetconfOperation::CancelCommit
            )
            || (draft.operation == NetconfOperation::CopyConfig
                && draft.target == Datastore::Candidate);
        if touches_candidate {
            let controller = self.clone();
            self.runtime.spawn(async move {
                controller.check_candidate().await;
            });
        }
    }

    /// Compare candidate with running and remember whether they differ.
    pub async fn check_candidate(&self) -> Option<(String, String)> {
        if !self.has_capability(":candidate") {
            return None;
        }
        let ticket = self.candidate_check.fetch_add(1, Ordering::AcqRel) + 1;
        self.store.update(|state| {
            let mut candidate = state.candidate.clone().unwrap_or_default();
            candidate.checking = true;
            state.candidate = Some(candidate);
        });
        let fetch = |source: Datastore| async move {
            let draft = NetconfDraft {
                source,
                filter_type: FilterType::None,
                ..default_netconf_draft()
            };
            let result = self
                .request(&ManagementDraft::Netconf(draft), None)
                .await
                .map_err(|err| err.error.message)?;
            let ManagementResult::NetconfRpc { xml, .. } = result else {
                return Ok(String::new());
            };
            let reply = parse_rpc_reply(&xml);
            if let Some(error) = reply.as_ref().and_then(|reply| reply.errors.first()) {
                return Err(error
                    .message
                    .clone()
                    .unwrap_or_else(|| "get-config failed".into()));
            }
            Ok::<_, String>(
                reply
                    .and_then(|reply| reply.data)
                    .map(|data| pretty_xml(&serialize_element(&data)))
                    .unwrap_or_default(),
            )
        };
        let (running, candidate) =
            tokio::join!(fetch(Datastore::Running), fetch(Datastore::Candidate));
        let current = ticket == self.candidate_check.load(Ordering::Acquire);
        match (running, candidate) {
            (Ok(running), Ok(candidate)) => {
                if current {
                    self.store.update(|state| {
                        state.candidate = Some(CandidateState {
                            dirty: running != candidate,
                            checking: false,
                            checked_at: Some(now_ms()),
                            error: None,
                        });
                    });
                }
                Some((running, candidate))
            }
            (Err(error), _) | (_, Err(error)) => {
                if current {
                    self.store.update(|state| {
                        state.candidate = Some(CandidateState {
                            dirty: false,
                            checking: false,
                            checked_at: Some(now_ms()),
                            error: Some(error),
                        });
                    });
                }
                None
            }
        }
    }
}

fn get_request(draft: &GnmiDraft, max_bytes: Option<u64>) -> RequestMessage {
    let (get, encoding) = gnmi_get_message(draft);
    RequestMessage::GnmiGet {
        prefix: get.prefix,
        paths: get.paths,
        data_type: get.data_type,
        encoding,
        depth: get.depth,
        max_bytes,
    }
}

fn set_request(set: crate::gnmi::GnmiSetRequest) -> RequestMessage {
    RequestMessage::GnmiSet {
        prefix: set.prefix,
        updates: set.updates,
        replaces: set.replaces,
        deletes: set.deletes,
        commit: set.commit,
    }
}

/// `node` as a tree built from `notifications` alone shows it.
fn reply_tree(
    node_id: &str,
    node_path: &GnmiPath,
    notifications: &[GnmiNotification],
    registry: &mut KeyRegistry,
) -> Result<DataNode, String> {
    let mut scratch = create_root(GnmiPath::default());
    for notification in notifications {
        for update in &notification.updates {
            let path = parse_gnmi_path(&update.path).map_err(|err| err.message)?;
            let target = node_at(&mut scratch, &path, Some(registry));
            merge_json(target, &update.value.value, Some(registry));
        }
    }
    if let Some(found) = find_node(&scratch, node_id) {
        return Ok(found.clone());
    }
    Ok(node_at(&mut scratch, node_path, None).clone())
}

/// Lists below `node`, within the levels a depth-limited reply shows, whose keys are only guessed.
fn unkeyed_lists<'a>(node: &'a DataNode, registry: &KeyRegistry) -> Vec<&'a DataNode> {
    fn visit<'a>(
        node: &'a DataNode,
        registry: &KeyRegistry,
        depth: usize,
        out: &mut Vec<&'a DataNode>,
    ) {
        if depth > 3 {
            return;
        }
        for child in &node.children {
            match child.kind {
                DataNodeKind::List => {
                    if child
                        .children
                        .iter()
                        .any(|entry| entry.guessed_keys == Some(true))
                        && registry.lookup(&schema_path_of(child)).is_none()
                    {
                        out.push(child);
                    }
                    for entry in &child.children {
                        visit(entry, registry, depth + 2, out);
                    }
                }
                DataNodeKind::Container => visit(child, registry, depth + 1, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    visit(node, registry, 0, &mut out);
    out
}

fn mark_loaded(node: &mut DataNode) {
    node.loaded = true;
    for child in &mut node.children {
        mark_loaded(child);
    }
}
