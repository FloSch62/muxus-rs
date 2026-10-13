//! The workspace runtime of one window (port of `client/src/state/workspaces.ts`
//! and the `WorkspaceRuntime` of `workspace-persistence.ts`).
//!
//! On start it restores the startup (or latest, or requested) workspace,
//! then autosaves the window's layout to the active named workspace 350 ms
//! after each change; the dialog and the launcher save, open, rename, lock,
//! delete and pick the startup workspace through it. Locked workspaces only
//! change through an explicit save. Every change to the catalog is announced
//! to the other windows over the [`WorkspaceBus`].
//!
//! The window side of it (reading and replacing the pane layout) is the
//! [`WorkspaceLayoutHost`] the app passes in; the app also tells the runtime
//! when the layout changed ([`Workspaces::layout_changed`]).

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use muxus_core::api::{
    WorkspaceLayoutV1, WorkspaceMultiExecGroup, WorkspaceRecord, WorkspaceSaveRequest,
    WorkspaceSummary,
};
use muxus_core::js::js_trim;
use muxus_session::routes::workspaces::WorkspacePatch;
use muxus_session::{ApiError, Backend};
use muxus_ui::gpui_kit::{App, AsyncApp, Context, Task, WeakEntity};

use super::layout::{WorkspaceSnapshot, serialize};
use super::list::next_workspace_name;
use super::sync::{HEARTBEAT, SyncEvent, WorkspaceBus, WorkspaceWindowSync};

/// The autosave waits this long after the last change.
pub const SAVE_DELAY: Duration = Duration::from_millis(350);
/// A failed autosave is retried after this long.
pub const RETRY_DELAY: Duration = Duration::from_millis(2_000);
/// What an unsaved window is called.
pub const UNSAVED_WORKSPACE: &str = "Unsaved workspace";

/// The window's pane layout as the runtime sees it. The app implements it.
pub trait WorkspaceLayoutHost {
    /// The window's layout and multi-exec groups (`currentSnapshot`, see
    /// [`super::layout::window_snapshot`]).
    fn snapshot(&self, cx: &App) -> WorkspaceSnapshot;
    /// No tab is open and the canvas is one pane: a startup restore may
    /// replace it without clobbering a session opened in the meantime.
    fn is_blank(&self, cx: &App) -> bool;
    /// Replace the window's layout with a saved one and set its multi-exec
    /// groups (`tabs.restore` + `multiExec.setGroups`). Must apply before it
    /// returns: the runtime reads the snapshot right after.
    fn restore(
        &self,
        layout: &WorkspaceLayoutV1,
        groups: &[WorkspaceMultiExecGroup],
        connect_remote: bool,
        cx: &mut App,
    );
    /// The workspace the window shows changed (window title, the desktop
    /// shell's reload bookkeeping: `muxusDesktop.setActiveWorkspace`).
    fn active_workspace_changed(
        &self,
        id: Option<&str>,
        name: &str,
        clear_reload_launch: bool,
        cx: &mut App,
    );
    /// Another window asked this one to come forward.
    fn focus_window(&self, cx: &mut App);
    /// `autoReconnectRemote`: restores dial remote sessions too.
    fn connect_remote_on_restore(&self, cx: &App) -> bool;
}

/// Which workspace a new window starts with (`WorkspaceInitialSelection`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceInitialSelection {
    Blank,
    New { id: String, name: String },
    Open { id: String },
    OpenName { name: String },
}

/// The catalog state views read (`useWorkspacesStore`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspacesState {
    pub workspaces: Vec<WorkspaceSummary>,
    pub active_id: Option<String>,
    pub active_name: String,
    pub startup_id: Option<String>,
    pub ready: bool,
    pub busy: bool,
    /// Number of other windows currently showing each workspace.
    pub open_window_counts: HashMap<String, usize>,
    pub error: Option<String>,
}

impl Default for WorkspacesState {
    fn default() -> Self {
        Self {
            workspaces: Vec::new(),
            active_id: None,
            active_name: UNSAVED_WORKSPACE.into(),
            startup_id: None,
            ready: false,
            busy: false,
            open_window_counts: HashMap::new(),
            error: None,
        }
    }
}

impl WorkspacesState {
    pub fn active(&self) -> Option<&WorkspaceSummary> {
        let id = self.active_id.as_deref()?;
        self.workspaces.iter().find(|workspace| workspace.id == id)
    }

    /// Whether the active workspace is locked (only explicit saves change it).
    pub fn active_is_locked(&self) -> bool {
        self.active().is_some_and(|workspace| workspace.is_locked)
    }

    pub fn other_window_count(&self, id: &str) -> usize {
        self.open_window_counts.get(id).copied().unwrap_or(0)
    }
}

fn summary_of(workspace: &WorkspaceRecord) -> WorkspaceSummary {
    WorkspaceSummary::from(workspace)
}

/// The record first, then the rest of the catalog without it.
fn merge_summary(
    summaries: &[WorkspaceSummary],
    workspace: &WorkspaceRecord,
) -> Vec<WorkspaceSummary> {
    let mut merged = vec![summary_of(workspace)];
    merged.extend(
        summaries
            .iter()
            .filter(|candidate| candidate.id != workspace.id)
            .cloned(),
    );
    merged
}

fn startup_of(workspaces: &[WorkspaceSummary]) -> Option<String> {
    workspaces
        .iter()
        .find(|workspace| workspace.is_startup)
        .map(|workspace| workspace.id.clone())
}

fn message(error: &ApiError) -> String {
    error.message.clone()
}

/// The workspace runtime of one window (see the module docs).
pub struct Workspaces {
    backend: Backend,
    host: Rc<dyn WorkspaceLayoutHost>,
    sync: WorkspaceWindowSync,
    state: WorkspacesState,
    initial: Option<WorkspaceInitialSelection>,
    started: bool,
    listening: bool,
    stopped: bool,
    restoring: bool,
    auto_save_paused: bool,
    timer: Option<Task<()>>,
    pending: Option<WorkspaceSnapshot>,
    saving: bool,
    save_waiters: Vec<async_channel::Sender<()>>,
    last_serialized: String,
    refreshing: bool,
    refresh_queued: bool,
    heartbeat: Option<Task<()>>,
    sync_events: Option<Task<()>>,
}

impl Workspaces {
    pub fn new(
        backend: Backend,
        bus: &WorkspaceBus,
        host: Rc<dyn WorkspaceLayoutHost>,
        initial: Option<WorkspaceInitialSelection>,
    ) -> Self {
        Self {
            backend,
            host,
            sync: WorkspaceWindowSync::new(bus),
            state: WorkspacesState::default(),
            initial,
            started: false,
            listening: false,
            stopped: false,
            restoring: false,
            auto_save_paused: false,
            timer: None,
            pending: None,
            saving: false,
            save_waiters: Vec::new(),
            last_serialized: String::new(),
            refreshing: false,
            refresh_queued: false,
            heartbeat: None,
            sync_events: None,
        }
    }

    pub fn state(&self) -> &WorkspacesState {
        &self.state
    }

    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    fn set_state(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut WorkspacesState)) {
        change(&mut self.state);
        cx.notify();
    }

    /// Restore the startup (or latest, or requested) workspace once, then
    /// listen for layout changes (`start`).
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if self.started {
            return;
        }
        self.started = true;
        let before_load = self.host.snapshot(cx).serialized;
        let initial = self.initial.clone();
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let outcome = load_initial(&backend, initial, this.clone(), cx).await;
            let _ = this.update(cx, |this, cx| this.finish_start(outcome, before_load, cx));
        })
        .detach();
    }

    fn finish_start(
        &mut self,
        outcome: Result<StartOutcome, ApiError>,
        before_load: String,
        cx: &mut Context<Self>,
    ) {
        if self.stopped {
            return;
        }
        let mut catalog_changed = false;
        match outcome {
            Ok(outcome) => {
                catalog_changed = outcome.catalog_changed;
                let loaded = self.host.snapshot(cx);
                let locked = outcome.workspace.as_ref().is_some_and(|w| w.is_locked);
                let changed = match &outcome.workspace {
                    Some(_) => outcome.persisted_source != loaded.serialized,
                    None => loaded.serialized != before_load,
                };
                self.last_serialized = if locked {
                    outcome.persisted_source.clone()
                } else {
                    loaded.serialized.clone()
                };
                if !locked && changed {
                    self.pending = Some(loaded);
                    self.schedule(SAVE_DELAY, cx);
                }
            }
            Err(_) => {
                // Connectivity failures are presented elsewhere; the window
                // simply starts unsaved.
                self.last_serialized = self.host.snapshot(cx).serialized;
                self.set_state(cx, |state| state.ready = true);
            }
        }
        self.listening = true;
        self.start_sync(cx);
        if catalog_changed {
            self.sync.invalidate_catalog();
        }
    }

    /// Apply a restored record to the window during start-up.
    fn apply_startup(
        &mut self,
        workspace: &WorkspaceRecord,
        restore: bool,
        cx: &mut Context<Self>,
    ) {
        if restore && self.host.is_blank(cx) {
            self.restoring = true;
            let connect = self.host.connect_remote_on_restore(cx);
            self.host
                .restore(&workspace.layout, &workspace.multi_exec_groups, connect, cx);
            self.restoring = false;
        }
    }

    fn record_startup(
        &mut self,
        summaries: Vec<WorkspaceSummary>,
        opened: Option<&WorkspaceRecord>,
        cx: &mut Context<Self>,
    ) {
        match opened {
            Some(opened) => {
                let merged = merge_summary(&summaries, opened);
                self.set_state(cx, |state| {
                    state.startup_id = startup_of(&merged);
                    state.workspaces = merged;
                    state.active_id = Some(opened.id.clone());
                    state.active_name = opened.name.clone();
                    state.ready = true;
                    state.error = None;
                });
            }
            None => self.set_state(cx, |state| {
                state.startup_id = startup_of(&summaries);
                state.workspaces = summaries;
                state.active_id = None;
                state.active_name = UNSAVED_WORKSPACE.into();
                state.ready = true;
                state.error = None;
            }),
        }
    }

    fn start_sync(&mut self, cx: &mut Context<Self>) {
        let (active_id, active_name) =
            (self.state.active_id.clone(), self.state.active_name.clone());
        self.update_active_workspace(active_id.as_deref(), &active_name, false, cx);
        let (sender, receiver) = async_channel::unbounded::<SyncEvent>();
        self.sync.start(move |event| {
            let _ = sender.try_send(event);
        });
        self.sync_events = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                if this
                    .update(cx, |this, cx| this.handle_sync_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        self.heartbeat = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(HEARTBEAT).await;
                if this.update(cx, |this, _| this.sync.heartbeat()).is_err() {
                    break;
                }
            }
        }));
    }

    fn handle_sync_event(&mut self, event: SyncEvent, cx: &mut Context<Self>) {
        match event {
            SyncEvent::CatalogChanged | SyncEvent::WindowFocus => {
                self.refresh_catalog(cx).detach();
            }
            SyncEvent::OpenWindowCountsChanged(counts) => {
                self.set_state(cx, |state| state.open_window_counts = counts);
            }
            SyncEvent::FocusRequested => self.host.focus_window(cx),
        }
    }

    /// The window closes: flush a pending autosave and leave the bus.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.flush_on_unload(cx);
        self.stopped = true;
        self.timer = None;
        self.heartbeat = None;
        self.sync_events = None;
        self.sync.stop();
    }

    /// The window gained focus: refresh presence and the catalog.
    pub fn window_focused(&mut self) {
        self.sync.window_focused();
    }

    /// The window's tabs or multi-exec groups changed (`handleChange`).
    pub fn layout_changed(&mut self, cx: &mut Context<Self>) {
        if !self.listening
            || self.restoring
            || self.stopped
            || self.auto_save_paused
            || self.state.active_is_locked()
        {
            return;
        }
        let snapshot = self.host.snapshot(cx);
        if snapshot.serialized == self.last_serialized {
            return;
        }
        self.last_serialized = snapshot.serialized.clone();
        self.pending = Some(snapshot);
        self.schedule(SAVE_DELAY, cx);
    }

    fn schedule(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.timer = None;
                this.flush_pending(cx);
            });
        }));
    }

    /// Write the pending snapshot, unless a save is running already
    /// (`flushPending`).
    fn flush_pending(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        if self.state.active_is_locked() {
            self.pending = None;
            return;
        }
        let Some(snapshot) = self.pending.take() else {
            return;
        };
        if self.stopped {
            return;
        }
        self.saving = true;
        let active_id = self.state.active_id.clone();
        let active_name = self.state.active_name.clone();
        let request = WorkspaceSaveRequest {
            id: active_id.clone(),
            name: if active_id.is_some() {
                active_name.clone()
            } else {
                next_workspace_name(&self.state.workspaces)
            },
            layout: snapshot.layout.clone(),
            overwrite_locked: false,
            create_if_missing: false,
            allocate_default_name: active_id.is_none(),
            multi_exec_groups: snapshot.multi_exec_groups.clone(),
        };
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.save_workspace(request).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(saved) => this.record_active(&saved, cx),
                    Err(error) if error.status == 404 && active_id.is_some() => {
                        if this.state.active_id == active_id {
                            let remaining: Vec<WorkspaceSummary> = this
                                .state
                                .workspaces
                                .iter()
                                .filter(|workspace| Some(&workspace.id) != active_id.as_ref())
                                .cloned()
                                .collect();
                            this.detach_active_workspace(
                                remaining,
                                format!(
                                    "Workspace “{active_name}” was deleted in another window. Current sessions remain open as an unsaved workspace."
                                ),
                                cx,
                            );
                        }
                    }
                    Err(error) if error.code.as_deref() == Some("workspace-locked") => {
                        // The snapshot was already recorded as observed. Mark it
                        // dirty again so unlocking requeues it even when no
                        // further edits are made.
                        this.last_serialized.clear();
                        let id = active_id.clone();
                        this.set_state(cx, |state| {
                            for workspace in &mut state.workspaces {
                                if Some(&workspace.id) == id.as_ref() {
                                    workspace.is_locked = true;
                                }
                            }
                            state.error = None;
                        });
                    }
                    Err(_) => {
                        if this.pending.is_none() {
                            this.pending = Some(snapshot);
                        }
                        if !this.stopped {
                            this.schedule(RETRY_DELAY, cx);
                        }
                    }
                }
                this.saving = false;
                for waiter in this.save_waiters.drain(..) {
                    let _ = waiter.try_send(());
                }
                if this.pending.is_some() && this.timer.is_none() && !this.stopped {
                    this.schedule(SAVE_DELAY, cx);
                }
            });
        })
        .detach();
    }

    /// Wait until no save runs and nothing is pending (`flushNow`).
    fn flush_now(
        this: WeakEntity<Self>,
        discard_pending: bool,
        cx: &mut AsyncApp,
    ) -> impl std::future::Future<Output = ()> + 'static {
        let mut cx = cx.clone();
        async move {
            let _ = this.update(&mut cx, |this, _| {
                this.timer = None;
                if discard_pending {
                    this.pending = None;
                }
            });
            loop {
                let waiter = this.update(&mut cx, |this, cx| {
                    if !this.saving && this.pending.is_some() {
                        this.flush_pending(cx);
                    }
                    if this.saving {
                        let (sender, receiver) = async_channel::bounded(1);
                        this.save_waiters.push(sender);
                        Some(receiver)
                    } else {
                        None
                    }
                });
                match waiter {
                    Ok(Some(receiver)) => {
                        let _ = receiver.recv().await;
                    }
                    _ => break,
                }
            }
        }
    }

    fn record_active(&mut self, workspace: &WorkspaceRecord, cx: &mut Context<Self>) {
        let merged = merge_summary(&self.state.workspaces, workspace);
        self.set_state(cx, |state| {
            state.workspaces = merged;
            state.active_id = Some(workspace.id.clone());
            state.active_name = workspace.name.clone();
            if workspace.is_startup {
                state.startup_id = Some(workspace.id.clone());
            }
            state.error = None;
        });
        self.update_active_workspace(Some(&workspace.id), &workspace.name, false, cx);
        self.sync.invalidate_catalog();
    }

    fn record_workspace(&mut self, workspace: &WorkspaceRecord, cx: &mut Context<Self>) {
        let merged = merge_summary(&self.state.workspaces, workspace);
        self.set_state(cx, |state| {
            state.workspaces = merged;
            if workspace.is_startup {
                state.startup_id = Some(workspace.id.clone());
            }
            state.error = None;
        });
        self.sync.invalidate_catalog();
    }

    fn update_active_workspace(
        &mut self,
        id: Option<&str>,
        name: &str,
        clear_reload_launch: bool,
        cx: &mut Context<Self>,
    ) {
        self.sync.set_active_workspace(id);
        self.host
            .active_workspace_changed(id, name, clear_reload_launch, cx);
    }

    fn detach_active_workspace(
        &mut self,
        workspaces: Vec<WorkspaceSummary>,
        error: String,
        cx: &mut Context<Self>,
    ) {
        self.timer = None;
        self.pending = None;
        self.last_serialized = self.host.snapshot(cx).serialized;
        self.set_state(cx, |state| {
            state.startup_id = startup_of(&workspaces);
            state.workspaces = workspaces;
            state.active_id = None;
            state.active_name = UNSAVED_WORKSPACE.into();
            state.error = Some(error);
        });
        self.update_active_workspace(None, UNSAVED_WORKSPACE, true, cx);
    }

    fn reconcile_catalog(&mut self, workspaces: Vec<WorkspaceSummary>, cx: &mut Context<Self>) {
        let previous_active_id = self.state.active_id.clone();
        let active = previous_active_id
            .as_ref()
            .and_then(|id| workspaces.iter().find(|workspace| &workspace.id == id))
            .cloned();
        let previous_locked = self.state.active_is_locked();
        if previous_active_id.is_some() && active.is_none() {
            let name = self.state.active_name.clone();
            self.detach_active_workspace(
                workspaces,
                format!(
                    "Workspace “{name}” was deleted in another window. Current sessions remain open as an unsaved workspace."
                ),
                cx,
            );
            return;
        }
        self.set_state(cx, |state| {
            if let Some(active) = &active {
                state.active_name = active.name.clone();
            }
            state.startup_id = startup_of(&workspaces);
            state.workspaces = workspaces;
            state.error = None;
        });
        if let Some(active) = &active {
            self.update_active_workspace(Some(&active.id), &active.name, false, cx);
            if active.is_locked && !previous_locked {
                self.timer = None;
                self.pending = None;
            } else if !active.is_locked && previous_locked {
                self.layout_changed(cx);
            }
        }
    }

    /// Read the catalog again (another window changed it, this one gained
    /// focus, the dialog opened). Overlapping calls coalesce.
    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if self.stopped {
            return Task::ready(());
        }
        if self.refreshing {
            self.refresh_queued = true;
            return Task::ready(());
        }
        self.refreshing = true;
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some(backend)) = this.update(cx, |this, _| {
                    this.refresh_queued = false;
                    (!this.stopped).then(|| this.backend.clone())
                }) else {
                    break;
                };
                let catalog = backend.workspaces().await;
                let again = this
                    .update(cx, |this, cx| {
                        if let Ok(catalog) = catalog
                            && !this.stopped
                        {
                            this.reconcile_catalog(catalog, cx);
                        }
                        this.refresh_queued && !this.stopped
                    })
                    .unwrap_or(false);
                if !again {
                    break;
                }
            }
            let _ = this.update(cx, |this, _| this.refreshing = false);
        })
    }

    /// Ask the window showing `id` to come forward; false when none does.
    pub fn focus_open_workspace(&self, id: &str) -> bool {
        self.sync.focus_open_workspace(id)
    }

    /// Drop silent windows and ask the others to announce themselves.
    pub fn request_presence(&self) {
        self.sync.request_presence();
    }

    /// Run a catalog action with `busy` set; a failure lands in `error`.
    fn with_busy<T: 'static>(
        &mut self,
        cx: &mut Context<Self>,
        action: impl AsyncFnOnce(WeakEntity<Self>, &mut AsyncApp) -> Result<T, String> + 'static,
    ) -> Task<Result<T, String>> {
        self.set_state(cx, |state| {
            state.busy = true;
            state.error = None;
        });
        cx.spawn(async move |this, cx| {
            let result = action(this.clone(), cx).await;
            let _ = this.update(cx, |this, cx| {
                let failure = result.as_ref().err().cloned();
                this.set_state(cx, |state| {
                    if let Some(failure) = failure {
                        state.error = Some(failure);
                    }
                    state.busy = false;
                });
            });
            result
        })
    }

    /// Pause the autosave for the duration of an explicit change; resume it
    /// (and pick up edits made meanwhile) afterwards.
    fn pause_autosave(this: &WeakEntity<Self>, cx: &mut AsyncApp) -> bool {
        this.update(cx, |this, _| {
            let was = this.auto_save_paused;
            this.auto_save_paused = true;
            was
        })
        .unwrap_or(false)
    }

    fn resume_autosave(this: &WeakEntity<Self>, was_paused: bool, cx: &mut AsyncApp) {
        let _ = this.update(cx, |this, cx| {
            this.auto_save_paused = was_paused;
            if !was_paused && !this.state.active_is_locked() {
                this.layout_changed(cx);
            }
        });
    }

    /// Save the window's layout under a new name and make it the active
    /// workspace (`saveAs`).
    pub fn save_as(
        &mut self,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorkspaceRecord, String>> {
        let name = js_trim(name).to_owned();
        let backend = self.backend.clone();
        self.with_busy(cx, async move |this, cx| {
            let was_paused = Self::pause_autosave(&this, cx);
            let result = async {
                Self::flush_now(this.clone(), false, cx).await;
                let snapshot = this
                    .update(cx, |this, cx| this.host.snapshot(cx))
                    .map_err(|_| "The window closed.".to_owned())?;
                let saved = backend
                    .save_workspace(WorkspaceSaveRequest {
                        id: None,
                        name,
                        layout: snapshot.layout.clone(),
                        overwrite_locked: false,
                        create_if_missing: false,
                        allocate_default_name: false,
                        multi_exec_groups: snapshot.multi_exec_groups.clone(),
                    })
                    .await
                    .map_err(|e| message(&e))?;
                let opened = backend
                    .open_workspace(&saved.id)
                    .await
                    .map_err(|e| message(&e))?;
                let _ = this.update(cx, |this, cx| {
                    this.last_serialized = snapshot.serialized.clone();
                    this.record_active(&opened, cx);
                });
                Ok(opened)
            }
            .await;
            Self::resume_autosave(&this, was_paused, cx);
            result
        })
    }

    /// Save the window's layout over the active workspace, locked or not (`save`).
    pub fn save(&mut self, cx: &mut Context<Self>) -> Task<Result<WorkspaceRecord, String>> {
        let backend = self.backend.clone();
        let active = self
            .state
            .active_id
            .clone()
            .map(|id| (id, self.state.active_name.clone()));
        self.with_busy(cx, async move |this, cx| {
            let Some((id, name)) = active else {
                return Err("Save the workspace with a name first.".to_owned());
            };
            let was_paused = Self::pause_autosave(&this, cx);
            let result = async {
                Self::flush_now(this.clone(), true, cx).await;
                let snapshot = this
                    .update(cx, |this, cx| this.host.snapshot(cx))
                    .map_err(|_| "The window closed.".to_owned())?;
                let saved = backend
                    .save_workspace(WorkspaceSaveRequest {
                        id: Some(id),
                        name,
                        layout: snapshot.layout.clone(),
                        overwrite_locked: true,
                        create_if_missing: false,
                        allocate_default_name: false,
                        multi_exec_groups: snapshot.multi_exec_groups.clone(),
                    })
                    .await
                    .map_err(|e| message(&e))?;
                let _ = this.update(cx, |this, cx| {
                    this.last_serialized = snapshot.serialized.clone();
                    this.record_active(&saved, cx);
                });
                Ok(saved)
            }
            .await;
            Self::resume_autosave(&this, was_paused, cx);
            result
        })
    }

    /// Rename a workspace (`rename`).
    pub fn rename(
        &mut self,
        id: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorkspaceRecord, String>> {
        let backend = self.backend.clone();
        let (id, name) = (id.to_owned(), js_trim(name).to_owned());
        self.with_busy(cx, async move |this, cx| {
            Self::flush_now(this.clone(), false, cx).await;
            let renamed = backend
                .patch_workspace(&id, WorkspacePatch::Rename(name))
                .await
                .map_err(|e| message(&e))?;
            let _ = this.update(cx, |this, cx| {
                if this.state.active_id.as_deref() == Some(renamed.id.as_str()) {
                    this.record_active(&renamed, cx);
                } else {
                    this.record_workspace(&renamed, cx);
                }
            });
            Ok(renamed)
        })
    }

    /// Open a workspace in this window, replacing its layout (`open`).
    pub fn open(
        &mut self,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorkspaceRecord, String>> {
        let backend = self.backend.clone();
        let id = id.to_owned();
        self.with_busy(cx, async move |this, cx| {
            Self::flush_now(this.clone(), false, cx).await;
            let workspace = backend.open_workspace(&id).await.map_err(|e| message(&e))?;
            let _ = this.update(cx, |this, cx| {
                this.restoring = true;
                let connect = this.host.connect_remote_on_restore(cx);
                this.host
                    .restore(&workspace.layout, &workspace.multi_exec_groups, connect, cx);
                this.restoring = false;
                this.pending = None;
                this.last_serialized = this.host.snapshot(cx).serialized;
                this.record_active(&workspace, cx);
            });
            Ok(workspace)
        })
    }

    /// Pick (or with `None` clear) the startup workspace (`setStartup`).
    pub fn set_startup(
        &mut self,
        id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let backend = self.backend.clone();
        let id = id.map(str::to_owned);
        self.with_busy(cx, async move |this, cx| {
            let workspace = backend
                .set_startup_workspace(id.as_deref())
                .await
                .map_err(|e| message(&e))?;
            let _ = this.update(cx, |this, cx| {
                let startup = workspace.as_ref().map(|workspace| workspace.id.clone());
                this.set_state(cx, |state| {
                    state.startup_id = startup.clone();
                    for candidate in &mut state.workspaces {
                        candidate.is_startup = Some(&candidate.id) == startup.as_ref();
                    }
                });
                this.sync.invalidate_catalog();
            });
            Ok(())
        })
    }

    /// Lock or unlock a workspace (`setLocked`). Locking the active one
    /// saves its pending edits first.
    pub fn set_locked(
        &mut self,
        id: &str,
        locked: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorkspaceRecord, String>> {
        let backend = self.backend.clone();
        let id = id.to_owned();
        let is_active = self.state.active_id.as_deref() == Some(id.as_str());
        self.with_busy(cx, async move |this, cx| {
            let was_paused = if is_active {
                Self::pause_autosave(&this, cx)
            } else {
                false
            };
            let result = async {
                if is_active && locked {
                    Self::flush_now(this.clone(), false, cx).await;
                }
                let updated = backend
                    .patch_workspace(&id, WorkspacePatch::SetLocked(locked))
                    .await
                    .map_err(|e| message(&e))?;
                let _ = this.update(cx, |this, cx| {
                    if is_active {
                        this.record_active(&updated, cx);
                    } else {
                        this.record_workspace(&updated, cx);
                    }
                });
                Ok(updated)
            }
            .await;
            if is_active {
                Self::resume_autosave(&this, was_paused, cx);
            }
            result
        })
    }

    /// Delete a workspace (`delete`). Deleting the active one leaves the
    /// window as an unsaved workspace.
    pub fn delete(&mut self, id: &str, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        let backend = self.backend.clone();
        let id = id.to_owned();
        self.with_busy(cx, async move |this, cx| {
            Self::flush_now(this.clone(), false, cx).await;
            let deleted = backend
                .delete_workspace(&id)
                .await
                .map_err(|e| message(&e))?;
            if !deleted {
                return Err("Workspace not found.".to_owned());
            }
            let _ = this.update(cx, |this, cx| {
                let was_active = this.state.active_id.as_deref() == Some(id.as_str());
                if was_active {
                    this.pending = None;
                    this.last_serialized = this.host.snapshot(cx).serialized;
                }
                let id_ref = id.clone();
                this.set_state(cx, |state| {
                    state.workspaces.retain(|workspace| workspace.id != id_ref);
                    if state.active_id.as_deref() == Some(id_ref.as_str()) {
                        state.active_id = None;
                        state.active_name = UNSAVED_WORKSPACE.into();
                    }
                    if state.startup_id.as_deref() == Some(id_ref.as_str()) {
                        state.startup_id = None;
                    }
                    state.error = None;
                });
                let (active_id, active_name) =
                    (this.state.active_id.clone(), this.state.active_name.clone());
                this.update_active_workspace(active_id.as_deref(), &active_name, was_active, cx);
                this.sync.invalidate_catalog();
            });
            Ok(())
        })
    }

    /// The window is closing: hand the pending autosave to the backend
    /// without waiting for it (`flushOnUnload`).
    pub fn flush_on_unload(&mut self, cx: &mut Context<Self>) {
        let _ = cx;
        if self.state.active_is_locked() {
            return;
        }
        let Some(pending) = self.pending.take() else {
            return;
        };
        let active_id = self.state.active_id.clone();
        let request = WorkspaceSaveRequest {
            name: if active_id.is_some() {
                self.state.active_name.clone()
            } else {
                next_workspace_name(&self.state.workspaces)
            },
            allocate_default_name: active_id.is_none(),
            id: active_id,
            layout: pending.layout,
            overwrite_locked: false,
            create_if_missing: false,
            multi_exec_groups: pending.multi_exec_groups,
        };
        // Run on the backend's own runtime, so the save completes after the
        // window (and its executor) are gone.
        let backend = self.backend.clone();
        drop(
            self.backend
                .runtime()
                .spawn(async move { backend.save_workspace(request).await }),
        );
    }
}

/// What start-up found (the first half of `start`).
struct StartOutcome {
    workspace: Option<WorkspaceRecord>,
    persisted_source: String,
    catalog_changed: bool,
}

async fn load_initial(
    backend: &Backend,
    initial: Option<WorkspaceInitialSelection>,
    this: WeakEntity<Workspaces>,
    cx: &mut AsyncApp,
) -> Result<StartOutcome, ApiError> {
    let mut persisted_source = String::new();
    let mut restore_saved = true;
    let mut catalog_changed = false;
    let catalog_request = backend.workspaces();
    let (catalog, workspace) = match initial {
        Some(WorkspaceInitialSelection::New { id, name }) => {
            let catalog = catalog_request.await?;
            let snapshot = this
                .update(cx, |this, cx| this.host.snapshot(cx))
                .map_err(|_| ApiError::internal("the window closed"))?;
            let workspace = backend
                .save_workspace(WorkspaceSaveRequest {
                    id: Some(id),
                    name: js_trim(&name).to_owned(),
                    layout: snapshot.layout.clone(),
                    overwrite_locked: false,
                    create_if_missing: true,
                    allocate_default_name: false,
                    multi_exec_groups: snapshot.multi_exec_groups.clone(),
                })
                .await?;
            catalog_changed = true;
            persisted_source = snapshot.serialized;
            restore_saved = false;
            (catalog, Some(workspace))
        }
        Some(WorkspaceInitialSelection::Open { id }) => {
            let workspace = backend.workspace(&id);
            (catalog_request.await?, Some(workspace.await?))
        }
        Some(WorkspaceInitialSelection::OpenName { name }) => {
            let catalog = catalog_request.await?;
            let target = js_trim(&name).to_lowercase();
            let by_id: Vec<&WorkspaceSummary> = catalog
                .iter()
                .filter(|candidate| candidate.id.to_lowercase() == target)
                .collect();
            let matches = if by_id.is_empty() {
                catalog
                    .iter()
                    .filter(|candidate| js_trim(&candidate.name).to_lowercase() == target)
                    .collect()
            } else {
                by_id
            };
            let workspace = match matches.as_slice() {
                [only] => Some(backend.workspace(&only.id).await?),
                _ => None,
            };
            (catalog, workspace)
        }
        Some(WorkspaceInitialSelection::Blank) => (catalog_request.await?, None),
        None => {
            let startup = backend.startup_workspace();
            let latest = backend.latest_workspace();
            let catalog = catalog_request.await?;
            let startup = startup.await?;
            let latest = latest.await?;
            (catalog, startup.or(latest))
        }
    };

    let mut opened = None;
    if let Some(workspace) = &workspace {
        if persisted_source.is_empty() {
            persisted_source = serialize(&workspace.layout, &workspace.multi_exec_groups);
        }
        let _ = this.update(cx, |this, cx| {
            this.apply_startup(workspace, restore_saved, cx)
        });
        let record = backend.open_workspace(&workspace.id).await?;
        catalog_changed = true;
        opened = Some(record);
    }
    let _ = this.update(cx, |this, cx| {
        this.record_startup(catalog, opened.as_ref(), cx)
    });
    Ok(StartOutcome {
        workspace,
        persisted_source,
        catalog_changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, startup: bool) -> WorkspaceSummary {
        WorkspaceSummary {
            id: id.into(),
            name: id.into(),
            is_locked: false,
            is_startup: startup,
            created_at: "2026-01-01".into(),
            updated_at: "2026-01-01".into(),
            last_opened_at: None,
        }
    }

    #[test]
    fn merging_puts_the_record_first_and_drops_its_old_copy() {
        let record = WorkspaceRecord {
            id: "b".into(),
            name: "Renamed".into(),
            layout: WorkspaceLayoutV1::empty(),
            multi_exec_groups: Vec::new(),
            is_locked: false,
            is_startup: false,
            created_at: "2026-01-01".into(),
            updated_at: "2026-01-02".into(),
            last_opened_at: None,
        };
        let merged = merge_summary(&[summary("a", false), summary("b", false)], &record);
        let ids: Vec<&str> = merged.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
        assert_eq!(merged[0].name, "Renamed");
        assert_eq!(
            startup_of(&[summary("a", false), summary("c", true)]).as_deref(),
            Some("c")
        );
    }

    #[test]
    fn an_unsaved_window_has_the_default_name() {
        let state = WorkspacesState::default();
        assert_eq!(state.active_name, "Unsaved workspace");
        assert!(!state.active_is_locked());
        assert_eq!(state.other_window_count("x"), 0);
    }
}
