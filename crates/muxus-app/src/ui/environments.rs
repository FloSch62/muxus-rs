//! The window's containers and VMs (`AppShell.tsx`'s `useEnvironmentDiscovery` and
//! `EnvironmentsPanel` placement, the top bar control of `TopBar.tsx`): the hub that reads
//! hosts as this window's tabs connect, the panel between the sidebar and the panes, and what
//! they ask of the window (sessions, the host editor, sign-in questions).
//!
//! Each window's hub and panel live in a global keyed by the window's shell, so the shell's own
//! state stays as it was.

use std::collections::HashMap;
use std::time::Instant;

use muxus_app::features::environments::hub::{
    EnvironmentsHub, EnvironmentsHubEvent, HubInput, PanelScope,
};
use muxus_app::features::environments::launcher::{EnvironmentLaunch, EnvironmentLaunchItem};
use muxus_app::features::environments::model::{
    EnvironmentCount, LiveTab, environment_host_for_managed,
};
use muxus_app::features::environments::panel::{
    EnvironmentsPanel, EnvironmentsPanelEvent, environments_panel,
};
use muxus_app::features::host_editor::view::{HostEditorTarget, NewHost};
use muxus_app::hosts::ManagedHost;
use muxus_app::state::tabs::{SessionSetEntry, TabPatch, TabStatus};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EntityId, Global, IntoElement, Subscription,
    Window,
};
use muxus_ui::widgets::{ButtonColor, IconButton};

use crate::ui::shell::AppShell;

/// One window's hub and panel.
struct Slot {
    hub: Entity<EnvironmentsHub>,
    panel: Option<Entity<EnvironmentsPanel>>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
struct WindowEnvironments(HashMap<EntityId, Slot>);

impl Global for WindowEnvironments {}

/// What the sidebar shows at the end of host rows: each host's marks and the host the panel is
/// open on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SidebarMarks {
    pub marks: HashMap<String, Vec<EnvironmentCount>>,
    pub open: Option<String>,
}

/// What a sidebar row asks of the panel.
#[derive(Clone, Debug)]
pub enum EnvironmentsRequest {
    /// The mark at the end of a row: open the panel on that host, or close it.
    Toggle(String),
    /// "Containers & VMs" in a host's menu.
    Show(ManagedHost),
}

impl AppShell {
    /// This window's hub, created on first use (`None` without the backend).
    pub fn environments_hub(&mut self, cx: &mut Context<Self>) -> Option<Entity<EnvironmentsHub>> {
        let id = cx.entity_id();
        if let Some(slot) = cx
            .try_global::<WindowEnvironments>()
            .and_then(|windows| windows.0.get(&id))
        {
            return Some(slot.hub.clone());
        }
        let backend = self.data.backend()?;
        let hub = cx.new(|_| EnvironmentsHub::new(backend));
        let subscriptions = vec![
            cx.observe(&hub, |this, _, cx| {
                this.push_environment_marks(cx);
                cx.notify();
            }),
            cx.subscribe(
                &hub,
                |this, _, event: &EnvironmentsHubEvent, cx| match event {
                    EnvironmentsHubEvent::Prompt { prompt, reply } => {
                        use muxus_app::features::forwarding::start::DialPrompt;
                        let prompt = match prompt.clone() {
                            DialPrompt::Auth(info) => crate::ui::prompts::SessionPrompt::Auth(info),
                            DialPrompt::HostKey(challenge) => {
                                crate::ui::prompts::SessionPrompt::HostKey(challenge)
                            }
                        };
                        this.prompt_seq += 1;
                        let key = format!("environment-prompt:{}", this.prompt_seq);
                        this.queue_prompt(
                            key,
                            prompt,
                            Some(crate::ui::prompts::PromptReply::Tunnel(reply.clone())),
                            cx,
                        );
                    }
                },
            ),
        ];
        cx.on_release(move |_, cx| {
            if let Some(windows) = cx.try_global::<WindowEnvironments>()
                && windows.0.contains_key(&id)
            {
                cx.global_mut::<WindowEnvironments>().0.remove(&id);
            }
        })
        .detach();
        // Hosts map to sidebar entries once the catalog is known; an empty one may not be
        // loaded yet.
        if self.catalog != muxus_app::model::Catalog::default() {
            let catalog = self.catalog.clone();
            hub.update(cx, |hub, cx| {
                hub.set_catalog(catalog.ssh.hosts, catalog.profiles, cx)
            });
        }
        cx.default_global::<WindowEnvironments>().0.insert(
            id,
            Slot {
                hub: hub.clone(),
                panel: None,
                _subscriptions: subscriptions,
            },
        );
        Some(hub)
    }

    fn environments_slot<'a>(&self, cx: &'a App, id: EntityId) -> Option<&'a Slot> {
        cx.try_global::<WindowEnvironments>()
            .and_then(|windows| windows.0.get(&id))
    }

    /// Keep the hub up to date with this window's tabs and preferences. Cheap when nothing
    /// changed; called on every frame of the window.
    pub fn sync_environments(&mut self, cx: &mut Context<Self>) {
        let Some(hub) = self.environments_hub(cx) else {
            return;
        };
        let input = HubInput {
            tabs: self
                .tabs
                .tabs
                .iter()
                .filter(|tab| matches!(tab.profile, Some(muxus_app::model::SessionProfile::Ssh(_))))
                .map(|tab| LiveTab {
                    profile: tab.profile.clone(),
                    connected: tab.status == TabStatus::Connected,
                    conn_id: tab.conn_id.clone(),
                })
                .collect(),
            discover: self.prefs.discover_environments,
            refresh_seconds: self.prefs.environment_refresh_seconds,
            keepalive_seconds: self.prefs.ssh_keepalive_interval_seconds,
            visible: self.window_visible,
        };
        hub.update(cx, |hub, cx| hub.sync(input, cx));
    }

    /// The sidebar's hosts were (re)loaded.
    pub fn environments_catalog_loaded(
        &mut self,
        catalog: &muxus_app::model::Catalog,
        cx: &mut Context<Self>,
    ) {
        let Some(hub) = self.environments_hub(cx) else {
            return;
        };
        let catalog = catalog.clone();
        hub.update(cx, |hub, cx| {
            hub.set_catalog(catalog.ssh.hosts, catalog.profiles, cx)
        });
    }

    fn push_environment_marks(&mut self, cx: &mut Context<Self>) {
        let Some(hub) = self
            .environments_slot(cx, cx.entity_id())
            .map(|slot| slot.hub.clone())
        else {
            return;
        };
        let marks = {
            let hub = hub.read(cx);
            SidebarMarks {
                marks: hub.marks(),
                open: hub.panel().host_key().map(str::to_string),
            }
        };
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_environment_marks(marks, cx));
    }

    /// Open the panel on everything, or close it (the top bar control and the launcher action).
    pub fn toggle_environments_panel(&mut self, cx: &mut Context<Self>) {
        let Some(hub) = self.environments_hub(cx) else {
            return;
        };
        hub.update(cx, |hub, cx| {
            let scope = if hub.panel().is_open() {
                PanelScope::Closed
            } else {
                PanelScope::All
            };
            hub.set_panel(scope, cx);
        });
    }

    /// A sidebar row's request.
    pub fn on_environments_request(
        &mut self,
        request: EnvironmentsRequest,
        cx: &mut Context<Self>,
    ) {
        let Some(hub) = self.environments_hub(cx) else {
            return;
        };
        match request {
            EnvironmentsRequest::Toggle(key) => hub.update(cx, |hub, cx| hub.toggle_host(&key, cx)),
            EnvironmentsRequest::Show(host) => {
                if let Some(host) = environment_host_for_managed(&host) {
                    hub.update(cx, |hub, cx| hub.show_host(host, cx));
                }
            }
        }
    }

    /// The panel while it is open (not in focus mode), created on first use.
    pub fn render_environments_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.focus_mode {
            return None;
        }
        let id = cx.entity_id();
        let hub = self.environments_hub(cx)?;
        if !hub.read(cx).panel().is_open() {
            return None;
        }
        let existing = self
            .environments_slot(cx, id)
            .and_then(|slot| slot.panel.clone());
        let panel = match existing {
            Some(panel) => panel,
            None => {
                let panel = environments_panel(hub, window, cx);
                let subscription = cx.subscribe_in(
                    &panel,
                    window,
                    |this, _, event: &EnvironmentsPanelEvent, window, cx| {
                        this.on_environments_panel_event(event, window, cx)
                    },
                );
                if let Some(slot) = cx.default_global::<WindowEnvironments>().0.get_mut(&id) {
                    slot.panel = Some(panel.clone());
                    slot._subscriptions.push(subscription);
                }
                panel
            }
        };
        Some(panel.into_any_element())
    }

    fn on_environments_panel_event(
        &mut self,
        event: &EnvironmentsPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            EnvironmentsPanelEvent::Open(entry) => {
                self.open_environment_session((**entry).clone(), cx)
            }
            EnvironmentsPanelEvent::EditHost(draft) => self.open_host_editor(
                HostEditorTarget::New(NewHost {
                    draft: Some(draft.clone()),
                    ..NewHost::default()
                }),
                window,
                cx,
            ),
        }
    }

    /// Open a session found on a host in the focused pane, with the host's color
    /// (`openSessionProfile`).
    pub fn open_environment_session(&mut self, entry: SessionSetEntry, cx: &mut Context<Self>) {
        let SessionSetEntry {
            profile,
            title,
            color,
        } = entry;
        let key = format!("environment:{profile:?}");
        let id = self
            .launch_guard
            .launch_once(&mut self.tabs, key, Instant::now(), |tabs| {
                tabs.open(profile, title)
            });
        if let Some(color) = color {
            self.tabs.update(
                &id,
                TabPatch {
                    color: Some(Some(color)),
                    ..Default::default()
                },
            );
        }
        self.tabs_changed(cx);
    }

    /// Run a quick launcher entry.
    pub fn launch_environment(&mut self, launch: EnvironmentLaunch, cx: &mut Context<Self>) {
        match launch {
            EnvironmentLaunch::Open(entry) => self.open_environment_session(*entry, cx),
            EnvironmentLaunch::ShowHost(host) => {
                if let Some(hub) = self.environments_hub(cx) {
                    hub.update(cx, |hub, cx| hub.show_host(*host, cx));
                }
            }
        }
    }

    /// Everything running on the hosts read so far, for the quick launcher.
    pub fn environment_launch_items(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Vec<EnvironmentLaunchItem> {
        let Some(hub) = self.environments_hub(cx) else {
            return Vec::new();
        };
        let entries = hub.read(cx).ready_entries();
        muxus_app::features::environments::launcher::environment_launch_items(&entries)
    }

    /// The top bar's containers and VMs control.
    pub fn environments_control(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let hub = self.environments_hub(cx)?;
        let open = hub.read(cx).panel().is_open();
        Some(
            IconButton::new("environments", "ViewInArOutlined")
                .tooltip("Containers & VMs")
                .color(if open {
                    ButtonColor::Primary
                } else {
                    ButtonColor::Default
                })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_environments_panel(cx)))
                .into_any_element(),
        )
    }
}
