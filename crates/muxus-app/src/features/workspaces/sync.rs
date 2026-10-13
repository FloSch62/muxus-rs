//! Keeping several windows' workspace catalogs in step (port of
//! `client/src/workspace-sync.ts`).
//!
//! Each Electron renderer joined a `BroadcastChannel`: it announced which
//! workspace it shows, told the others when the catalog changed, and asked
//! the window showing a workspace to come forward instead of opening it a
//! second time. Every window of this app runs in one process, so the channel
//! is a [`WorkspaceBus`] the windows share; the messages, the presence
//! heartbeat and its expiry are the TypeScript's.
//!
//! Delivery is synchronous but never re-entrant: a message posted while
//! another is being delivered waits in the bus's queue. What a window learns
//! is reported through the callback given to [`WorkspaceWindowSync::start`]
//! once the window's own state is no longer borrowed.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

/// How often a window re-announces itself and drops silent peers.
pub const HEARTBEAT: Duration = Duration::from_secs(5);
/// A peer not heard from for this long is gone.
pub const PRESENCE_TTL: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    CatalogChanged {
        sender: String,
    },
    PresenceRequest {
        sender: String,
    },
    Presence {
        sender: String,
        workspace_id: Option<String>,
    },
    Focus {
        sender: String,
        target: String,
        workspace_id: String,
    },
    Goodbye {
        sender: String,
    },
}

impl Message {
    fn sender(&self) -> &str {
        match self {
            Message::CatalogChanged { sender }
            | Message::PresenceRequest { sender }
            | Message::Presence { sender, .. }
            | Message::Focus { sender, .. }
            | Message::Goodbye { sender } => sender,
        }
    }
}

/// What a window learns from the others (`WorkspaceSyncCallbacks`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncEvent {
    /// Another window changed the catalog: read it again.
    CatalogChanged,
    /// How many other windows show each workspace.
    OpenWindowCountsChanged(HashMap<String, usize>),
    /// This window gained focus ([`WorkspaceWindowSync::window_focused`]).
    WindowFocus,
    /// Another window asked this one to come forward.
    FocusRequested,
}

pub type Clock = Rc<dyn Fn() -> Instant>;

struct Bus {
    members: Vec<Weak<RefCell<SyncCore>>>,
    queue: VecDeque<Message>,
    dispatching: bool,
    clock: Clock,
}

/// The channel every window of the process shares (`BroadcastChannel` named
/// `muxus-workspaces-v1`). Cheap to clone.
#[derive(Clone)]
pub struct WorkspaceBus {
    inner: Rc<RefCell<Bus>>,
}

impl Default for WorkspaceBus {
    fn default() -> Self {
        Self::new(Rc::new(Instant::now))
    }
}

impl WorkspaceBus {
    /// A bus whose presence timestamps come from `clock` (tests pass a fake one).
    pub fn new(clock: Clock) -> Self {
        Self {
            inner: Rc::new(RefCell::new(Bus {
                members: Vec::new(),
                queue: VecDeque::new(),
                dispatching: false,
                clock,
            })),
        }
    }

    fn now(&self) -> Instant {
        let clock = self.inner.borrow().clock.clone();
        clock()
    }

    fn join(&self, core: &Rc<RefCell<SyncCore>>) {
        self.inner.borrow_mut().members.push(Rc::downgrade(core));
    }

    fn leave(&self, core: &Rc<RefCell<SyncCore>>) {
        let target = Rc::downgrade(core);
        self.inner
            .borrow_mut()
            .members
            .retain(|member| member.strong_count() > 0 && !member.ptr_eq(&target));
    }

    /// Deliver `messages` to every member but their sender, then whatever
    /// those deliveries post in turn.
    fn post(&self, messages: Vec<Message>) {
        {
            let mut bus = self.inner.borrow_mut();
            bus.queue.extend(messages);
            if bus.dispatching {
                return;
            }
            bus.dispatching = true;
        }
        loop {
            let (next, members) = {
                let mut bus = self.inner.borrow_mut();
                let Some(next) = bus.queue.pop_front() else {
                    bus.dispatching = false;
                    break;
                };
                bus.members.retain(|member| member.strong_count() > 0);
                let members: Vec<_> = bus.members.iter().filter_map(Weak::upgrade).collect();
                (next, members)
            };
            let now = self.now();
            for member in members {
                let (outgoing, events, sink) = {
                    let Ok(mut core) = member.try_borrow_mut() else {
                        continue;
                    };
                    if core.window_id == next.sender() || !core.started {
                        continue;
                    }
                    let outgoing = core.handle(&next, now);
                    (
                        outgoing,
                        std::mem::take(&mut core.events),
                        core.sink.clone(),
                    )
                };
                self.inner.borrow_mut().queue.extend(outgoing);
                if let Some(sink) = sink {
                    for event in events {
                        sink(event);
                    }
                }
            }
        }
    }
}

struct RemotePresence {
    workspace_id: Option<String>,
    last_seen: Instant,
}

struct SyncCore {
    window_id: String,
    remotes: HashMap<String, RemotePresence>,
    active_workspace_id: Option<String>,
    started: bool,
    last_counts: Option<BTreeMap<String, usize>>,
    events: Vec<SyncEvent>,
    sink: Option<Rc<dyn Fn(SyncEvent)>>,
}

impl SyncCore {
    fn presence(&self) -> Message {
        Message::Presence {
            sender: self.window_id.clone(),
            workspace_id: self.active_workspace_id.clone(),
        }
    }

    fn open_window_counts(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for presence in self.remotes.values() {
            if let Some(id) = &presence.workspace_id {
                *counts.entry(id.clone()).or_insert(0) += 1;
            }
        }
        counts
    }

    fn publish_counts(&mut self) {
        let counts = self.open_window_counts();
        let sorted: BTreeMap<String, usize> = counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
        // Nothing published yet counts as a change, even to an empty map: the
        // TypeScript compares against an empty serialization.
        if self.last_counts.as_ref() == Some(&sorted) {
            return;
        }
        self.last_counts = Some(sorted);
        self.events.push(SyncEvent::OpenWindowCountsChanged(counts));
    }

    fn remove_stale_presence(&mut self, now: Instant) {
        let before = self.remotes.len();
        self.remotes.retain(|_, presence| {
            now.checked_duration_since(presence.last_seen)
                .is_none_or(|age| age <= PRESENCE_TTL)
        });
        if self.remotes.len() != before {
            self.publish_counts();
        }
    }

    fn handle(&mut self, message: &Message, now: Instant) -> Vec<Message> {
        match message {
            Message::CatalogChanged { .. } => {
                self.events.push(SyncEvent::CatalogChanged);
                Vec::new()
            }
            Message::PresenceRequest { .. } => vec![self.presence()],
            Message::Goodbye { sender } => {
                if self.remotes.remove(sender).is_some() {
                    self.publish_counts();
                }
                Vec::new()
            }
            Message::Focus {
                target,
                workspace_id,
                ..
            } => {
                if *target == self.window_id
                    && Some(workspace_id) == self.active_workspace_id.as_ref()
                {
                    self.events.push(SyncEvent::FocusRequested);
                }
                Vec::new()
            }
            Message::Presence {
                sender,
                workspace_id,
            } => {
                self.remotes.insert(
                    sender.clone(),
                    RemotePresence {
                        workspace_id: workspace_id.clone(),
                        last_seen: now,
                    },
                );
                self.publish_counts();
                Vec::new()
            }
        }
    }
}

/// One window's end of the bus (`WorkspaceWindowSync`). Each window owns
/// exactly one.
pub struct WorkspaceWindowSync {
    bus: WorkspaceBus,
    core: Rc<RefCell<SyncCore>>,
}

fn random_id() -> String {
    muxus_store::prefs::command_buttons::new_preference_id("window")
}

impl WorkspaceWindowSync {
    pub fn new(bus: &WorkspaceBus) -> Self {
        Self {
            bus: bus.clone(),
            core: Rc::new(RefCell::new(SyncCore {
                window_id: random_id(),
                remotes: HashMap::new(),
                active_workspace_id: None,
                started: false,
                last_counts: None,
                events: Vec::new(),
                sink: None,
            })),
        }
    }

    pub fn window_id(&self) -> String {
        self.core.borrow().window_id.clone()
    }

    /// Run `change` on the core, then hand its events to the sink and its
    /// messages to the bus, with the core no longer borrowed.
    fn with_core<R>(&self, change: impl FnOnce(&mut SyncCore) -> (R, Vec<Message>)) -> R {
        let (result, outgoing, events, sink) = {
            let mut core = self.core.borrow_mut();
            let (result, outgoing) = change(&mut core);
            let outgoing = if core.started { outgoing } else { Vec::new() };
            (
                result,
                outgoing,
                std::mem::take(&mut core.events),
                core.sink.clone(),
            )
        };
        if let Some(sink) = sink {
            for event in events {
                sink(event);
            }
        }
        if !outgoing.is_empty() {
            self.bus.post(outgoing);
        }
        result
    }

    /// Join the bus: ask who is there and say what this window shows.
    pub fn start(&self, sink: impl Fn(SyncEvent) + 'static) {
        if self.core.borrow().started {
            return;
        }
        {
            let mut core = self.core.borrow_mut();
            core.started = true;
            core.sink = Some(Rc::new(sink));
        }
        self.bus.join(&self.core);
        self.with_core(|core| {
            let request = Message::PresenceRequest {
                sender: core.window_id.clone(),
            };
            ((), vec![request, core.presence()])
        });
    }

    /// Leave the bus (the window closes): the others forget this window.
    pub fn stop(&self) {
        if !self.core.borrow().started {
            return;
        }
        self.with_core(|core| {
            (
                (),
                vec![Message::Goodbye {
                    sender: core.window_id.clone(),
                }],
            )
        });
        self.bus.leave(&self.core);
        self.with_core(|core| {
            core.started = false;
            core.remotes.clear();
            core.publish_counts();
            ((), Vec::new())
        });
    }

    /// Which workspace this window shows now. Announced when it changed.
    pub fn set_active_workspace(&self, workspace_id: Option<&str>) {
        self.with_core(|core| {
            let changed = workspace_id != core.active_workspace_id.as_deref();
            core.active_workspace_id = workspace_id.map(Into::into);
            let outgoing = if changed {
                vec![core.presence()]
            } else {
                Vec::new()
            };
            ((), outgoing)
        });
    }

    /// Tell the other windows the catalog changed.
    pub fn invalidate_catalog(&self) {
        self.with_core(|core| {
            (
                (),
                vec![Message::CatalogChanged {
                    sender: core.window_id.clone(),
                }],
            )
        });
    }

    /// Drop silent peers and ask everyone to announce themselves again.
    pub fn request_presence(&self) {
        let now = self.bus.now();
        self.with_core(|core| {
            core.remove_stale_presence(now);
            let request = Message::PresenceRequest {
                sender: core.window_id.clone(),
            };
            ((), vec![request, core.presence()])
        });
    }

    /// The heartbeat (every [`HEARTBEAT`]): drop silent peers, announce.
    pub fn heartbeat(&self) {
        let now = self.bus.now();
        self.with_core(|core| {
            core.remove_stale_presence(now);
            ((), vec![core.presence()])
        });
    }

    /// The window gained focus: refresh presence and report it.
    pub fn window_focused(&self) {
        self.request_presence();
        self.with_core(|core| {
            core.events.push(SyncEvent::WindowFocus);
            ((), Vec::new())
        });
    }

    /// Ask the window showing `workspace_id` (the one heard from most
    /// recently) to come forward. False when no other window shows it.
    pub fn focus_open_workspace(&self, workspace_id: &str) -> bool {
        let now = self.bus.now();
        self.with_core(|core| {
            core.remove_stale_presence(now);
            let target = core
                .remotes
                .iter()
                .filter(|(_, presence)| presence.workspace_id.as_deref() == Some(workspace_id))
                .max_by_key(|(_, presence)| presence.last_seen)
                .map(|(id, _)| id.clone());
            match target {
                Some(target) => (
                    true,
                    vec![Message::Focus {
                        sender: core.window_id.clone(),
                        target,
                        workspace_id: workspace_id.to_owned(),
                    }],
                ),
                None => (false, Vec::new()),
            }
        })
    }

    /// How many other windows show each workspace.
    pub fn open_window_counts(&self) -> HashMap<String, usize> {
        self.core.borrow().open_window_counts()
    }
}

impl Drop for WorkspaceWindowSync {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Default)]
    struct Recorded {
        events: RefCell<Vec<SyncEvent>>,
    }

    impl Recorded {
        fn sink(self: &Rc<Self>) -> impl Fn(SyncEvent) + 'static {
            let this = self.clone();
            move |event| this.events.borrow_mut().push(event)
        }

        fn count(&self, wanted: &SyncEvent) -> usize {
            self.events
                .borrow()
                .iter()
                .filter(|event| *event == wanted)
                .count()
        }

        fn last_counts(&self) -> Option<HashMap<String, usize>> {
            self.events
                .borrow()
                .iter()
                .rev()
                .find_map(|event| match event {
                    SyncEvent::OpenWindowCountsChanged(counts) => Some(counts.clone()),
                    _ => None,
                })
        }
    }

    fn counts(entries: &[(&str, usize)]) -> HashMap<String, usize> {
        entries.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    }

    #[test]
    fn shares_catalog_invalidations_and_active_workspace_presence() {
        let bus = WorkspaceBus::default();
        let (first_events, second_events) =
            (Rc::new(Recorded::default()), Rc::new(Recorded::default()));
        let first = WorkspaceWindowSync::new(&bus);
        let second = WorkspaceWindowSync::new(&bus);

        first.set_active_workspace(Some("operations"));
        first.start(first_events.sink());
        second.set_active_workspace(Some("development"));
        second.start(second_events.sink());

        assert_eq!(first.open_window_counts(), counts(&[("development", 1)]));
        assert_eq!(second.open_window_counts(), counts(&[("operations", 1)]));

        first.invalidate_catalog();
        assert_eq!(second_events.count(&SyncEvent::CatalogChanged), 1);
        assert_eq!(first_events.count(&SyncEvent::CatalogChanged), 0);

        first.stop();
        second.stop();
    }

    #[test]
    fn focuses_one_existing_owner_and_removes_presence_when_its_window_closes() {
        let bus = WorkspaceBus::default();
        let (first_events, second_events) =
            (Rc::new(Recorded::default()), Rc::new(Recorded::default()));
        let first = WorkspaceWindowSync::new(&bus);
        let second = WorkspaceWindowSync::new(&bus);

        first.set_active_workspace(Some("operations"));
        first.start(first_events.sink());
        second.set_active_workspace(Some("development"));
        second.start(second_events.sink());

        assert!(first.focus_open_workspace("development"));
        assert_eq!(second_events.count(&SyncEvent::FocusRequested), 1);
        assert!(!first.focus_open_workspace("missing"));

        second.stop();
        assert_eq!(first.open_window_counts(), HashMap::new());
        assert_eq!(first_events.last_counts(), Some(HashMap::new()));

        first.stop();
    }

    #[test]
    fn forgets_windows_that_stop_answering() {
        let now = Rc::new(Cell::new(Instant::now()));
        let clock = now.clone();
        let bus = WorkspaceBus::new(Rc::new(move || clock.get()));
        let first = WorkspaceWindowSync::new(&bus);
        let second = WorkspaceWindowSync::new(&bus);
        first.start(|_| {});
        second.set_active_workspace(Some("lab"));
        second.start(|_| {});
        assert_eq!(first.open_window_counts(), counts(&[("lab", 1)]));

        // The second window hangs: its heartbeat stops arriving.
        let hung = std::mem::ManuallyDrop::new(second);
        bus.leave(&hung.core);
        now.set(now.get() + PRESENCE_TTL + Duration::from_secs(1));
        first.heartbeat();
        assert_eq!(first.open_window_counts(), HashMap::new());
    }
}
