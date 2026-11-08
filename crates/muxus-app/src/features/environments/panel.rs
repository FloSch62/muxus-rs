//! The containers and VMs panel (`components/environments/EnvironmentsPanel.tsx`):
//! the containers, clusters, labs and VMs of this computer and of SSH hosts,
//! as one tree per host the way XPipe's connection hub lays them out: host,
//! then each tool it has (Docker, Containerlab, Kubernetes, Proxmox, libvirt),
//! then what runs there, each line with its own actions. Opened for one host
//! from its sidebar mark or menu, or for all of them from the top bar.
//!
//! The window mounts it between the sidebar and the panes while the hub's
//! [`PanelScope`] is open; sessions it opens and hosts it adds go to the window
//! as [`EnvironmentsPanelEvent`]s.

use std::collections::HashMap;
use std::time::Duration;

use muxus_core::environments::{
    ContainerlabLab, EnvironmentAction, EnvironmentTarget, HostEnvironment, KubernetesPod,
    LibvirtUri, ProxmoxGuestType,
};
use muxus_session::environments::EnvironmentSource;
use muxus_store::prefs::layout::{
    DEFAULT_ENVIRONMENTS_PANEL_WIDTH, MIN_ENVIRONMENTS_PANEL_WIDTH, clamp_environments_panel_width,
};
use muxus_store::prefs::state::SidebarPosition;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, ClickEvent, ClipboardItem, Context, CursorStyle, ElementId,
    Entity, EventEmitter, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, canvas, div, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts, layout, weight};
use muxus_ui::widgets::menu::{self, MenuItem, menu_divider};
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::search_field::search_field;
use muxus_ui::widgets::{Button, ButtonColor, IconButton, Switch};
use muxus_webview::{TopologyWindowRequest, WebWindowResult};

use super::hosts::{
    HostDirectory, JumpHostOptions, enter_host_draft, jump_host_draft, local_enter_profile,
};
use super::hub::{EnvironmentsHub, PanelScope};
use super::logos::environment_logo;
use super::model::{
    EnvironmentHost, EnvironmentKind, EnvironmentStatus, HostEnvironmentsEntry,
    LOCAL_ENVIRONMENT_KEY, age,
};
use super::sessions::{
    containerlab_node_entry, environment_entry, forwarded_port_url, local_port_url,
    ssh_to_address_entry, vnc_entry,
};
use super::topology::{TopologyDialog, TopologyEvent};
use super::tree::{LineSpec, note_line, tree_line};
use super::viewer;
use crate::features::host_editor::logic::draft::HostDraft;
use crate::features::launcher::kit::{show_error_toast, show_toast};
use crate::features::settings::SettingsPrefs;
use crate::features::sftp::dialogs::{ConfirmOptions, DialogQueue, PromptOptions};
use crate::state::tabs::SessionSetEntry;

/// Room the panel leaves for the host sidebar beside it and a usable workspace when widened.
const WIDENED_RESERVE: f32 = 560.;

/// What the panel asks its window to do.
pub enum EnvironmentsPanelEvent {
    /// Open a session in the window's focused pane.
    Open(Box<SessionSetEntry>),
    /// Open the host editor with a new host filled in, to name and file before saving.
    EditHost(Box<HostDraft>),
}

/// A guest whose addresses the hypervisor knows.
#[derive(Clone, Debug, PartialEq)]
pub enum GuestRef {
    Proxmox(ProxmoxGuestType, u32),
    Libvirt(Option<LibvirtUri>, String),
}

/// What a line's button or menu entry does.
#[derive(Clone, Debug)]
pub enum PanelAction {
    Open(SessionSetEntry),
    Target(EnvironmentHost, EnvironmentTarget),
    /// SSH to an address, asking for the user first.
    SshAs {
        host: EnvironmentHost,
        address: String,
        name: String,
    },
    SshGuest {
        host: EnvironmentHost,
        guest: GuestRef,
        name: String,
    },
    Vnc {
        host: EnvironmentHost,
        uri: Option<LibvirtUri>,
        domain: String,
    },
    Act {
        host: EnvironmentHost,
        action: EnvironmentAction,
        confirm: Option<(String, String)>,
    },
    Port {
        host: EnvironmentHost,
        address: Option<String>,
        port: u16,
        scheme: &'static str,
    },
    Copy(String),
    AddEnter {
        host: EnvironmentHost,
        name: String,
        target: EnvironmentTarget,
    },
    AddAddress {
        host: EnvironmentHost,
        options: JumpHostOptions,
    },
    AddGuest {
        host: EnvironmentHost,
        name: String,
        guest: GuestRef,
        console: EnvironmentTarget,
    },
    Topology {
        host: EnvironmentHost,
        lab: ContainerlabLab,
    },
    Kubus(String),
    Refresh(EnvironmentHost),
    Forget(String),
    ReloadPods(String),
    Namespace(String, String),
    Toggle(String, bool),
}

/// One entry of a line's "…" menu (`MenuEntry`).
#[derive(Clone, Debug)]
pub struct MenuEntry {
    pub label: String,
    pub action: PanelAction,
    pub disabled: bool,
    pub destructive: bool,
    pub divider: bool,
}

impl MenuEntry {
    pub fn new(label: impl Into<String>, action: PanelAction) -> Self {
        Self {
            label: label.into(),
            action,
            disabled: false,
            destructive: false,
            divider: false,
        }
    }

    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    pub fn divider(mut self) -> Self {
        self.divider = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

struct OpenMenu {
    position: Point<Pixels>,
    entries: Vec<MenuEntry>,
}

/// The pods of one opened context.
pub(super) enum PodsState {
    Loading,
    Ready(Vec<KubernetesPod>),
    Error(String),
}

pub(super) struct ContextPods {
    pub namespace: String,
    pub namespaces: Vec<String>,
    pub namespaces_asked: bool,
    pub state: PodsState,
    pub generation: u64,
    /// The list, read and host reading the shown list is of.
    pub requested: Option<(String, u64, Option<u64>)>,
    pub _task: Option<Task<()>>,
}

impl Default for ContextPods {
    fn default() -> Self {
        Self {
            namespace: "default".into(),
            namespaces: Vec::new(),
            namespaces_asked: false,
            state: PodsState::Loading,
            generation: 0,
            requested: None,
            _task: None,
        }
    }
}

struct WidthDrag {
    start_x: f32,
    start_width: f32,
    width: f32,
}

pub struct EnvironmentsPanel {
    pub(super) hub: Entity<EnvironmentsHub>,
    filter_input: Entity<InputState>,
    pub(super) filter: String,
    pub(super) show_stopped: bool,
    widened: bool,
    expansion: HashMap<String, bool>,
    pub(super) pods: HashMap<String, ContextPods>,
    menu: Option<OpenMenu>,
    dialogs: DialogQueue,
    topology: Option<(Entity<TopologyDialog>, Subscription)>,
    /// Remembered per host: the user each SSH to a guest last logged in as.
    last_users: HashMap<String, String>,
    drag: Option<WidthDrag>,
    now: u64,
    _subscriptions: Vec<Subscription>,
    _clock: Task<()>,
}

impl EventEmitter<EnvironmentsPanelEvent> for EnvironmentsPanel {}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

impl EnvironmentsPanel {
    pub fn new(hub: Entity<EnvironmentsHub>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Filter by name, image or address"));
        let subscriptions = vec![
            cx.observe(&hub, |_, _, cx| cx.notify()),
            cx.subscribe(&filter_input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filter = input.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            }),
        ];
        let clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.now = now_ms();
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        Self {
            hub,
            filter_input,
            filter: String::new(),
            show_stopped: false,
            widened: false,
            expansion: HashMap::new(),
            pods: HashMap::new(),
            menu: None,
            dialogs: DialogQueue::new(),
            topology: None,
            last_users: HashMap::new(),
            drag: None,
            now: now_ms(),
            _subscriptions: subscriptions,
            _clock: clock,
        }
    }

    /// Which lines are open. Each line has its own default, so lists the user never touched
    /// open the way their kind suggests (`useExpansion`).
    pub(super) fn is_open(&self, key: &str, fallback: bool) -> bool {
        self.expansion.get(key).copied().unwrap_or(fallback)
    }

    fn toggle(&mut self, key: &str, fallback: bool, cx: &mut Context<Self>) {
        let open = self.is_open(key, fallback);
        self.expansion.insert(key.to_string(), !open);
        cx.notify();
    }

    /// A click handler that runs `action`.
    pub(super) fn on(
        &self,
        action: PanelAction,
        cx: &mut Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        let entity = cx.entity().downgrade();
        move |_, window, cx| {
            let action = action.clone();
            let _ = entity.update(cx, |this, cx| this.run(action, window, cx));
        }
    }

    /// A line's click: a single click toggles an expandable line, a double click runs `open`.
    pub(super) fn line_click(
        &self,
        toggle: Option<(String, bool)>,
        open: Option<PanelAction>,
        cx: &mut Context<Self>,
    ) -> Option<super::tree::Click> {
        if toggle.is_none() && open.is_none() {
            return None;
        }
        let entity = cx.entity().downgrade();
        Some(Box::new(move |event, window, cx| {
            let action = if event.click_count() >= 2 {
                open.clone()
            } else {
                toggle
                    .clone()
                    .map(|(key, fallback)| PanelAction::Toggle(key, fallback))
            };
            if let Some(action) = action {
                let _ = entity.update(cx, |this, cx| this.run(action, window, cx));
            }
        }))
    }

    /// A row action: an icon button with its name as tooltip (`LineAction`).
    pub(super) fn line_action(
        &self,
        id: impl Into<String>,
        label: impl Into<SharedString>,
        icon_name: &'static str,
        action: PanelAction,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        IconButton::new(ElementId::Name(id.into().into()), icon_name)
            .icon_size(16.)
            .padding(4.)
            .tooltip(label)
            .disabled(disabled)
            .on_click(self.on(action, cx))
            .into_any_element()
    }

    /// The "…" button with the rest of a line's actions (`LineMenu`).
    pub(super) fn line_menu(
        &self,
        id: impl Into<String>,
        label: impl Into<SharedString>,
        entries: Vec<MenuEntry>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if entries.is_empty() {
            return None;
        }
        let entity = cx.entity().downgrade();
        Some(
            IconButton::new(ElementId::Name(id.into().into()), "MoreHoriz")
                .icon_size(16.)
                .padding(4.)
                .tooltip(label)
                .on_click(move |event: &ClickEvent, window, cx| {
                    let position = event
                        .mouse_position()
                        .unwrap_or_else(|| window.mouse_position());
                    let entries = entries.clone();
                    let _ = entity.update(cx, |this, cx| {
                        this.menu = Some(OpenMenu { position, entries });
                        cx.notify();
                    });
                })
                .into_any_element(),
        )
    }

    /// Open a menu of `entries` at `position`.
    pub(super) fn open_menu(
        &mut self,
        position: Point<Pixels>,
        entries: Vec<MenuEntry>,
        cx: &mut Context<Self>,
    ) {
        self.menu = Some(OpenMenu { position, entries });
        cx.notify();
    }

    fn source_task(
        &mut self,
        host: &EnvironmentHost,
        cx: &mut Context<Self>,
    ) -> Task<Result<Option<EnvironmentSource>, String>> {
        self.hub.update(cx, |hub, cx| hub.source(host, true, cx))
    }

    /// Run `action`.
    pub(super) fn run(&mut self, action: PanelAction, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match action {
            PanelAction::Open(entry) => cx.emit(EnvironmentsPanelEvent::Open(Box::new(entry))),
            PanelAction::Target(host, target) => cx.emit(EnvironmentsPanelEvent::Open(Box::new(
                environment_entry(&host, target),
            ))),
            PanelAction::SshAs {
                host,
                address,
                name,
            } => self.ssh_as(host, address, name, window, cx),
            PanelAction::SshGuest { host, guest, name } => {
                let lookup = self.addresses(&host, guest, cx);
                cx.spawn_in(window, async move |this, cx| {
                    let Some(addresses) = lookup.await else {
                        return;
                    };
                    let _ = this.update_in(cx, |this, window, cx| match addresses.first() {
                        Some(address) => this.ssh_as(host, address.clone(), name, window, cx),
                        None => {
                            show_toast(Status::Warning, format!("{name} reports no address."), cx)
                        }
                    });
                })
                .detach();
            }
            PanelAction::Vnc { host, uri, domain } => {
                let source = self.source_task(&host, cx);
                let backend = self.hub.read(cx).backend().clone();
                cx.spawn(async move |this, cx| {
                    let result = match source.await {
                        Ok(Some(source)) => backend
                            .environment_libvirt_display(source, uri, domain.clone())
                            .await
                            .map_err(|error| error.message),
                        Ok(None) => Err(format!("{} is not connected", host.label)),
                        Err(message) => Err(message),
                    };
                    let _ = this.update(cx, |_, cx| match result {
                        Ok(display) => match display.vnc {
                            Some(vnc) => cx.emit(EnvironmentsPanelEvent::Open(Box::new(
                                vnc_entry(&host, &vnc.host, vnc.port, &domain),
                            ))),
                            None => show_toast(
                                Status::Warning,
                                format!("{domain} has no VNC display."),
                                cx,
                            ),
                        },
                        Err(message) => show_error_toast(message, cx),
                    });
                })
                .detach();
            }
            PanelAction::Act {
                host,
                action,
                confirm,
            } => self.act(host, action, confirm, window, cx),
            PanelAction::Port {
                host,
                address,
                port,
                scheme,
            } => self.open_port(host, address, port, scheme, cx),
            PanelAction::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                show_toast(Status::Success, format!("Copied \"{text}\""), cx);
            }
            PanelAction::AddEnter { host, name, target } => self.add_enter(host, name, target, cx),
            PanelAction::AddAddress { host, options } => {
                let draft = {
                    let hub = self.hub.read(cx);
                    let directory = HostDirectory {
                        ssh_hosts: hub.ssh_hosts(),
                        saved_profiles: hub.saved_profiles(),
                    };
                    jump_host_draft(&host, directory, &options, new_host_storage(cx))
                };
                cx.emit(EnvironmentsPanelEvent::EditHost(Box::new(draft)));
            }
            PanelAction::AddGuest {
                host,
                name,
                guest,
                console,
            } => {
                // Keep a guest as a sidebar host: SSH to the address it reports, through the
                // hypervisor, or its console when it reports none.
                let lookup = self.addresses(&host, guest, cx);
                cx.spawn_in(window, async move |this, cx| {
                    let Some(addresses) = lookup.await else {
                        return;
                    };
                    let _ = this.update_in(cx, |this, window, cx| match addresses.first() {
                        Some(address) => this.run(
                            PanelAction::AddAddress {
                                host,
                                options: JumpHostOptions {
                                    name,
                                    address: address.clone(),
                                    ..JumpHostOptions::default()
                                },
                            },
                            window,
                            cx,
                        ),
                        None => this.add_enter(host, name, console, cx),
                    });
                })
                .detach();
            }
            PanelAction::Topology { host, lab } => self.show_topology(host, lab, window, cx),
            PanelAction::Kubus(route) => {
                if let Some(url) = muxus_session::environments::kubus_url(&route) {
                    cx.open_url(&url);
                }
            }
            PanelAction::Refresh(host) => self.hub.update(cx, |hub, cx| hub.refresh_host(host, cx)),
            PanelAction::Forget(key) => self.hub.update(cx, |hub, cx| hub.forget(&key, cx)),
            PanelAction::ReloadPods(key) => {
                if let Some(pods) = self.pods.get_mut(&key) {
                    pods.generation += 1;
                }
            }
            PanelAction::Namespace(key, namespace) => {
                let pods = self.pods.entry(key).or_default();
                pods.namespace = namespace;
            }
            PanelAction::Toggle(key, fallback) => self.toggle(&key, fallback, cx),
        }
        cx.notify();
    }

    /// Ask which user to log in as, offering the last one used for this host, then open SSH to
    /// `address` through the host (`openSshToAddress`).
    fn ssh_as(
        &mut self,
        host: EnvironmentHost,
        address: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let asked = self.dialogs.prompt(
            PromptOptions {
                title: format!("SSH to {address}"),
                description: host
                    .profile
                    .as_ref()
                    .map(|_| format!("Through {}.", host.label)),
                label: "User".into(),
                initial_value: self.last_users.get(&host.key).cloned().unwrap_or_default(),
                placeholder: Some("Your local user name".into()),
                confirm_label: "Connect".into(),
                allow_empty: true,
                ..PromptOptions::default()
            },
            |this: &mut Self| &mut this.dialogs,
            window,
            cx,
        );
        cx.spawn(async move |this, cx| {
            let Some(user) = asked.await else {
                return;
            };
            let user = user.trim().to_string();
            let _ = this.update(cx, |this, cx| {
                this.last_users.insert(host.key.clone(), user.clone());
                cx.emit(EnvironmentsPanelEvent::Open(Box::new(
                    ssh_to_address_entry(
                        &host,
                        &address,
                        &name,
                        (!user.is_empty()).then_some(user.as_str()),
                    ),
                )));
            });
        })
        .detach();
    }

    /// A guest's addresses, or `None` after a toast saying why there are none.
    fn addresses(
        &mut self,
        host: &EnvironmentHost,
        guest: GuestRef,
        cx: &mut Context<Self>,
    ) -> Task<Option<Vec<String>>> {
        let source = self.source_task(host, cx);
        let backend = self.hub.read(cx).backend().clone();
        let label = host.label.clone();
        cx.spawn(async move |_, cx| {
            let result = match source.await {
                Ok(Some(source)) => match guest {
                    GuestRef::Proxmox(guest_type, vmid) => {
                        backend
                            .environment_proxmox_addresses(source, guest_type, vmid)
                            .await
                    }
                    GuestRef::Libvirt(uri, domain) => {
                        backend
                            .environment_libvirt_addresses(source, uri, domain)
                            .await
                    }
                }
                .map(|response| response.addresses)
                .map_err(|error| error.message),
                Ok(None) => Err(format!("{label} is not connected")),
                Err(message) => Err(message),
            };
            match result {
                Ok(addresses) => Some(addresses),
                Err(message) => {
                    cx.update(|cx| show_error_toast(message, cx));
                    None
                }
            }
        })
    }

    /// Run a lifecycle action, asking first when `confirm` says so, then read the host again.
    fn act(
        &mut self,
        host: EnvironmentHost,
        action: EnvironmentAction,
        confirm: Option<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let asked = confirm.map(|(title, description)| {
            self.dialogs.confirm(
                ConfirmOptions {
                    title,
                    description: Some(description),
                    confirm_label: "Continue".into(),
                    destructive: true,
                    ..ConfirmOptions::default()
                },
                window,
                cx,
            )
        });
        let backend = self.hub.read(cx).backend().clone();
        cx.spawn(async move |this, cx| {
            if let Some(asked) = asked
                && !asked.await.confirmed
            {
                return;
            }
            let Ok(source) = this.update(cx, |this, cx| this.source_task(&host, cx)) else {
                return;
            };
            let result = match source.await {
                Ok(Some(source)) => backend
                    .environment_action(source, action)
                    .await
                    .map_err(|error| error.message),
                Ok(None) => Err(format!("{} is not connected", host.label)),
                Err(message) => Err(message),
            };
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(response) if !response.ok => show_error_toast(
                        if response.output.is_empty() {
                            "The action failed.".to_string()
                        } else {
                            response.output
                        },
                        cx,
                    ),
                    Err(message) => show_error_toast(message, cx),
                    Ok(_) => {}
                }
                this.hub.update(cx, |hub, cx| hub.refresh_host(host, cx));
            });
        })
        .detach();
    }

    /// Open a port the host serves in the browser: directly on this computer, through an SSH
    /// forward otherwise. The forward shows in the forwarding panel and stops with the
    /// connection (`openHostPort`).
    fn open_port(
        &mut self,
        host: EnvironmentHost,
        address: Option<String>,
        port: u16,
        scheme: &'static str,
        cx: &mut Context<Self>,
    ) {
        if host.profile.is_none() {
            cx.open_url(&local_port_url(scheme, address.as_deref(), port));
            return;
        }
        let source = self.source_task(&host, cx);
        let backend = self.hub.read(cx).backend().clone();
        cx.spawn(async move |_, cx| {
            let result = match source.await {
                Ok(Some(EnvironmentSource::Ssh { conn_id })) => backend
                    .environment_port(
                        &conn_id,
                        muxus_core::environments::EnvironmentPortRequest {
                            host: super::sessions::loopback_for(address.as_deref()),
                            port,
                        },
                    )
                    .await
                    .map_err(|error| error.message),
                Ok(_) => Err(format!("{} is not connected", host.label)),
                Err(message) => Err(message),
            };
            cx.update(|cx| match result {
                Ok(forward) => cx.open_url(&forwarded_port_url(scheme, forward.local_port)),
                Err(message) => {
                    show_error_toast(format!("Could not open port {port}: {message}"), cx)
                }
            });
        })
        .detach();
    }

    /// Keep something found on a host as a sidebar entry. On this computer it becomes a local
    /// terminal profile; on an SSH host the host editor opens with a new host filled in
    /// (`addEnvironmentToHosts`).
    fn add_enter(
        &mut self,
        host: EnvironmentHost,
        name: String,
        target: EnvironmentTarget,
        cx: &mut Context<Self>,
    ) {
        if host.profile.is_none() {
            let Some(prefs) = SettingsPrefs::global(cx) else {
                return;
            };
            let profile = muxus_store::prefs::state::LocalShellProfileConfig {
                id: muxus_local::profile::new_local_shell_profile_id(),
                ..local_enter_profile(&name, &target)
            };
            prefs.update(cx, |prefs, cx| {
                prefs.update(cx, |state| state.local_shell_profiles.push(profile))
            });
            show_toast(
                Status::Success,
                format!("{name} was added to the local terminals."),
                cx,
            );
            return;
        }
        let draft = {
            let hub = self.hub.read(cx);
            let directory = HostDirectory {
                ssh_hosts: hub.ssh_hosts(),
                saved_profiles: hub.saved_profiles(),
            };
            enter_host_draft(&host, directory, &name, &target, new_host_storage(cx))
        };
        match draft {
            Some(draft) => cx.emit(EnvironmentsPanelEvent::EditHost(Box::new(draft))),
            None => show_toast(
                Status::Warning,
                format!(
                    "{} is not a saved host, so {name} cannot be added.",
                    host.label
                ),
                cx,
            ),
        }
    }

    fn show_topology(
        &mut self,
        host: EnvironmentHost,
        lab: ContainerlabLab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = self.source_task(&host, cx);
        let backend = self.hub.read(cx).backend().clone();
        let dialog = cx.new(|cx| TopologyDialog::new(host, lab, source, backend, window, cx));
        let subscription = cx.subscribe_in(
            &dialog,
            window,
            |this, _, event: &TopologyEvent, window, cx| match event {
                TopologyEvent::Close => {
                    this.topology = None;
                    cx.notify();
                }
                TopologyEvent::Open(entry) => {
                    this.topology = None;
                    this.run(PanelAction::Open((**entry).clone()), window, cx);
                }
                TopologyEvent::Window(request) => this.show_topology_window(request, window, cx),
            },
        );
        self.topology = Some((dialog, subscription));
    }

    /// The topology in the containerlab viewer's window, shown where the dialog is: the
    /// dialog closes once the window shows (or at once, when the lab's window was already
    /// open); should the window not come up, the dialog draws the topology natively.
    fn show_topology_window(
        &mut self,
        request: &TopologyWindowRequest,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some((dialog, _)) = &self.topology else {
            return;
        };
        let dialog = dialog.clone();
        let (host, lab) = {
            let dialog = dialog.read(cx);
            (dialog.host().clone(), dialog.lab().clone())
        };
        let panel = cx.entity().downgrade();
        // "SSH as admin" or "Open shell" in the window (openContainerlabNode).
        let open_node = move |name: String, cx: &mut App| {
            let Some(node) = lab.nodes.iter().find(|node| node.name == name) else {
                return;
            };
            let entry = containerlab_node_entry(&host, &lab, node);
            let _ = panel.update(cx, |_, cx| {
                cx.emit(EnvironmentsPanelEvent::Open(Box::new(entry)));
            });
        };
        let panel = cx.entity().downgrade();
        let waiting = dialog.downgrade();
        let on_shown = move |shown: bool, cx: &mut App| {
            let _ = panel.update(cx, |this, cx| {
                // Only the dialog that asked, should another have replaced it meanwhile.
                if !this
                    .topology
                    .as_ref()
                    .is_some_and(|(dialog, _)| dialog.entity_id() == waiting.entity_id())
                {
                    return;
                }
                if shown {
                    this.topology = None;
                } else if let Some(dialog) = waiting.upgrade() {
                    dialog.update(cx, |dialog, cx| dialog.stop_handover(cx));
                }
                cx.notify();
            });
        };
        let request = TopologyWindowRequest {
            placement: Some(viewer::dialog_placement(window)),
            ..request.clone()
        };
        match viewer::open(&request, open_node, on_shown, cx) {
            // The dialog waits for the new window to show.
            Some(Ok(WebWindowResult::Opened)) => {}
            Some(Ok(WebWindowResult::Focused)) => {
                self.topology = None;
                cx.notify();
            }
            Some(Err(error)) => {
                show_toast(Status::Error, error, cx);
                dialog.update(cx, |dialog, cx| dialog.stop_handover(cx));
            }
            Some(Ok(WebWindowResult::Invalid)) | None => {
                dialog.update(cx, |dialog, cx| dialog.stop_handover(cx));
            }
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.hub
            .update(cx, |hub, cx| hub.set_panel(PanelScope::Closed, cx));
    }

    fn panel_width(&self, window: &Window, cx: &App) -> f32 {
        let container = muxus_ui::zoom::unzoom(window.viewport_size().width);
        if self.widened {
            // Expanded, it takes what the sidebar and a usable workspace leave.
            return (container - WIDENED_RESERVE)
                .min(container * 0.75)
                .max(MIN_ENVIRONMENTS_PANEL_WIDTH as f32);
        }
        if let Some(drag) = &self.drag {
            return drag.width;
        }
        let stored = SettingsPrefs::global(cx)
            .map(|prefs| prefs.read(cx).prefs().environments_panel_width)
            .unwrap_or(DEFAULT_ENVIRONMENTS_PANEL_WIDTH);
        clamp_environments_panel_width(stored, container as f64) as f32
    }

    fn sidebar_on_right(cx: &App) -> bool {
        SettingsPrefs::global(cx)
            .is_some_and(|prefs| prefs.read(cx).prefs().sidebar_position == SidebarPosition::Right)
    }

    /// The edge grip: the panel sits between the sidebar and the panes and grows toward the
    /// panes (`PanelResizeHandle`).
    fn render_grip(
        &self,
        width: f32,
        on_right: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group: SharedString = "environments-resize".into();
        let dragging = self.drag.is_some();
        let line = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(2.5))
            .w(px(3.))
            .bg(theme.palette.primary.main);
        let grip = div()
            .id("environments-resize")
            .group(group.clone())
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(8.))
            .cursor(CursorStyle::ResizeLeftRight)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count == 2 {
                        let default = DEFAULT_ENVIRONMENTS_PANEL_WIDTH;
                        if let Some(prefs) = SettingsPrefs::global(cx) {
                            prefs.update(cx, |prefs, cx| {
                                prefs.update(cx, |state| state.environments_panel_width = default)
                            });
                        }
                    } else {
                        this.drag = Some(WidthDrag {
                            start_x: muxus_ui::zoom::unzoom(event.position.x),
                            start_width: width,
                            width,
                        });
                    }
                    cx.notify();
                }),
            )
            .child(if dragging {
                line
            } else {
                line.invisible().group_hover(group, |s| s.visible())
            });
        if on_right {
            grip.left(px(-4.)).into_any_element()
        } else {
            grip.right(px(-4.)).into_any_element()
        }
    }

    /// While resizing, follow the pointer anywhere in the window.
    fn render_drag_tracker(&self, on_right: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.drag.as_ref()?;
        let entity = cx.entity().downgrade();
        Some(
            canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    let moving = entity.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if !phase.bubble() {
                            return;
                        }
                        let _ = moving.update(cx, |this, cx| {
                            let container = muxus_ui::zoom::unzoom(window.viewport_size().width);
                            if let Some(drag) = &mut this.drag {
                                let delta = muxus_ui::zoom::unzoom(event.position.x) - drag.start_x;
                                let direction = if on_right { -1. } else { 1. };
                                drag.width = clamp_environments_panel_width(
                                    (drag.start_width + delta * direction) as f64,
                                    container as f64,
                                ) as f32;
                                cx.notify();
                            }
                        });
                    });
                    let ending = entity.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                        if !phase.bubble() {
                            return;
                        }
                        let _ = ending.update(cx, |this, cx| {
                            if let Some(drag) = this.drag.take() {
                                let width = f64::from(drag.width);
                                if let Some(prefs) = SettingsPrefs::global(cx) {
                                    prefs.update(cx, |prefs, cx| {
                                        prefs.update(cx, |state| {
                                            state.environments_panel_width = width
                                        })
                                    });
                                }
                                cx.notify();
                            }
                        });
                    });
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
    }

    fn render_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.menu.as_ref()?;
        let entity = cx.entity().downgrade();
        let mut content = menu::paper(theme);
        for (index, entry) in open.entries.iter().enumerate() {
            let mut item = MenuItem::new(
                SharedString::from(format!("environment-menu-{index}")),
                entry.label.clone(),
            )
            .dense()
            .disabled(entry.disabled);
            if entry.destructive {
                item = item.destructive();
            }
            let (entity, action) = (entity.clone(), entry.action.clone());
            content = content.child(item.on_click(move |_, window, cx| {
                let action = action.clone();
                let _ = entity.update(cx, |this, cx| this.run(action, window, cx));
            }));
            if entry.divider && index + 1 < open.entries.len() {
                content = content.child(menu_divider(theme));
            }
        }
        let dismiss = entity;
        Some(
            menu::popup(
                open.position,
                move |_, cx| {
                    let _ = dismiss.update(cx, |this, cx| {
                        this.menu = None;
                        cx.notify();
                    });
                },
                content,
            )
            .into_any_element(),
        )
    }

    fn render_header(
        &self,
        scoped: Option<&HostEnvironmentsEntry>,
        hosts: &[HostEnvironmentsEntry],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let entity = cx.entity().downgrade();
        let mut title = div().flex_1().min_w_0().child(
            label(
                scoped
                    .map(|entry| entry.host.label.clone())
                    .unwrap_or_else(|| "Containers & VMs".into()),
            )
            .variant(theme.typography.subtitle2)
            .weight(weight::BOLD)
            .color(p.text.primary),
        );
        if scoped.is_some() {
            let hub = self.hub.clone();
            title = title.child(
                div()
                    .id("environments-all-hosts")
                    .cursor_pointer()
                    .child(
                        label("All hosts")
                            .size(11.)
                            .line_height(13.)
                            .color(p.primary.main),
                    )
                    .on_click(move |_, _, cx| {
                        hub.update(cx, |hub, cx| hub.set_panel(PanelScope::All, cx));
                    }),
            );
        }
        let refresh_hosts: Vec<EnvironmentHost> =
            hosts.iter().map(|entry| entry.host.clone()).collect();
        let hub = self.hub.clone();
        let (widen, close) = (entity.clone(), entity);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::spacing(1.))
            // As tall as the tab strip beside it, so their bottom lines meet.
            .h(px(layout::TAB_STRIP_HEIGHT))
            .px(Theme::spacing(1.5))
            .border_b_1()
            .border_color(p.divider)
            .child(icon("ViewInArOutlined", SMALL, p.primary.main))
            .child(title)
            .child(
                IconButton::new("environments-refresh", "RefreshOutlined")
                    .tooltip(match scoped {
                        Some(entry) => format!("Read {} again", entry.host.label),
                        None => "Read every host again".into(),
                    })
                    .on_click(move |_, _, cx| {
                        let hosts = refresh_hosts.clone();
                        hub.update(cx, |hub, cx| {
                            for host in hosts {
                                hub.refresh_host(host, cx);
                            }
                        });
                    }),
            )
            .child(
                IconButton::new(
                    "environments-widen",
                    if self.widened {
                        "CloseFullscreen"
                    } else {
                        "OpenInFull"
                    },
                )
                .tooltip(if self.widened {
                    "Back to the set width"
                } else {
                    "Widen to see more"
                })
                .on_click(move |_, _, cx| {
                    let _ = widen.update(cx, |this, cx| {
                        this.widened = !this.widened;
                        cx.notify();
                    });
                }),
            )
            .child(
                IconButton::new("environments-close", "Close")
                    .tooltip("Close containers and VMs panel")
                    .on_click(move |_, _, cx| {
                        let _ = close.update(cx, |this, cx| this.close(cx));
                    }),
            )
            .into_any_element()
    }

    fn render_filter(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = theme.palette;
        let stopped = cx.entity().downgrade();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::spacing(1.))
            .px(Theme::spacing(1.5))
            .py(Theme::spacing(1.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(search_field(&self.filter_input, 32., None, cx)),
            )
            .child(
                div()
                    .id("environments-stopped")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .cursor_pointer()
                    .child(Switch::new("environments-stopped-switch", self.show_stopped).small())
                    .child(label("Stopped").size(12.).color(p.text.secondary))
                    .on_click(move |_, _, cx| {
                        let _ = stopped.update(cx, |this, cx| {
                            this.show_stopped = !this.show_stopped;
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    /// One host and what it runs (`HostCard`).
    fn render_host(
        &mut self,
        entry: &HostEnvironmentsEntry,
        scoped: bool,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let host = &entry.host;
        let key = format!("host:{}", host.key);
        let open = scoped || self.is_open(&key, true);
        let local = host.key == LOCAL_ENVIRONMENT_KEY;
        let mut tools: Vec<&'static str> = Vec::new();
        for environment in &entry.environments {
            let label = EnvironmentKind::of(environment).label();
            if !tools.contains(&label) {
                tools.push(label);
            }
        }
        let status: AnyElement = match entry.status {
            EnvironmentStatus::Loading => CircularProgress::new(
                SharedString::from(format!("env-loading-{}", host.key)),
                12.,
                p.primary.main,
            )
            .into_any_element(),
            EnvironmentStatus::Ready => {
                super::tree::small_text(age(entry.sampled_at, self.now).unwrap_or_default(), theme)
                    .into_any_element()
            }
            EnvironmentStatus::Offline => {
                super::tree::small_text("not connected", theme).into_any_element()
            }
            EnvironmentStatus::Unsupported => {
                super::tree::small_text("runs no commands", theme).into_any_element()
            }
            EnvironmentStatus::Error => super::tree::small_text("failed", theme).into_any_element(),
        };
        let host_icon = icon(
            if local {
                "ComputerOutlined"
            } else {
                "DnsOutlined"
            },
            17.,
            host.color
                .as_deref()
                .and_then(muxus_ui::color::try_hex)
                .unwrap_or(p.text.secondary),
        );
        let mut spec = LineSpec::new(key.clone(), 0, host.label.clone())
            .icon(host_icon)
            .maybe_detail((!tools.is_empty()).then(|| tools.join(" · ")))
            .status(status)
            .action(self.line_action(
                format!("env-refresh-{}", host.key),
                format!("Read {} again", host.label),
                "RefreshOutlined",
                PanelAction::Refresh(host.clone()),
                false,
                cx,
            ));
        spec.emphasis = true;
        if !scoped {
            spec = spec.open(open);
        }
        if !local
            && let Some(menu) = self.line_menu(
                format!("env-host-menu-{}", host.key),
                format!("More actions for {}", host.label),
                vec![MenuEntry::new(
                    "Remove from this list",
                    PanelAction::Forget(host.key.clone()),
                )],
                cx,
            )
        {
            spec = spec.action(menu);
        }
        let click = self.line_click((!scoped).then(|| (key.clone(), true)), None, cx);
        let mut card = div()
            .flex()
            .flex_col()
            .py(px(2.))
            .border_b_1()
            .border_color(p.divider)
            .child(tree_line(spec, theme, click));
        if !open {
            return card.into_any_element();
        }
        for problem in &entry.problems {
            let provider = match problem.provider {
                muxus_core::environments::EnvironmentProvider::Docker => "Docker",
                muxus_core::environments::EnvironmentProvider::Podman => "Podman",
                muxus_core::environments::EnvironmentProvider::Kubernetes => "Kubernetes",
                muxus_core::environments::EnvironmentProvider::Proxmox => "Proxmox VE",
                muxus_core::environments::EnvironmentProvider::Libvirt => "libvirt",
            };
            card = card.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(6.))
                    .pl(px(30.))
                    .pr(px(8.))
                    .py(px(2.))
                    .child(icon(
                        "WarningAmberOutlined",
                        14.,
                        theme.status_text_color(Status::Warning),
                    ))
                    .child(
                        div().flex_1().min_w_0().child(
                            paragraph(format!("{provider}: {}", problem.message))
                                .size(11.)
                                .color(theme.status_text_color(Status::Warning)),
                        ),
                    ),
            );
        }
        match entry.status {
            EnvironmentStatus::Offline => {
                let refresh = host.clone();
                card = card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(Theme::spacing(1.))
                        .pl(px(30.))
                        .pr(px(8.))
                        .py(px(4.))
                        .child(
                            div().flex_1().min_w_0().child(
                                paragraph(
                                    entry
                                        .message
                                        .clone()
                                        .unwrap_or_else(|| "Not connected.".into()),
                                )
                                .size(12.)
                                .color(p.text.secondary),
                            ),
                        )
                        .child(
                            Button::new(
                                SharedString::from(format!("env-connect-{}", host.key)),
                                "Connect",
                            )
                            .color(ButtonColor::Primary)
                            .on_click(self.on(PanelAction::Refresh(refresh), cx)),
                        ),
                );
            }
            EnvironmentStatus::Error | EnvironmentStatus::Unsupported => {
                let color = if entry.status == EnvironmentStatus::Error {
                    theme.status_text_color(Status::Error)
                } else {
                    p.text.secondary
                };
                card = card.child(note_line(
                    0,
                    entry.message.clone().unwrap_or_default(),
                    color,
                ));
            }
            EnvironmentStatus::Ready if entry.environments.is_empty() => {
                card = card.child(note_line(
                    0,
                    format!(
                        "No Docker, Podman, Kubernetes, Proxmox or libvirt found{}.",
                        if local { " on this computer" } else { "" }
                    ),
                    p.text.secondary,
                ));
            }
            _ => {}
        }
        for environment in &entry.environments {
            let section = match environment {
                HostEnvironment::Containers(runtime) => {
                    self.containers_section(entry, runtime, theme, window, cx)
                }
                HostEnvironment::Containerlab(labs) => {
                    self.containerlab_section(entry, labs, theme, window, cx)
                }
                HostEnvironment::Kubernetes(kube) => {
                    self.kubernetes_section(entry, kube, theme, window, cx)
                }
                HostEnvironment::Proxmox(pve) => {
                    self.proxmox_section(entry, pve, theme, window, cx)
                }
                HostEnvironment::Libvirt(libvirt) => {
                    self.libvirt_section(entry, libvirt, theme, window, cx)
                }
            };
            card = card.children(section);
        }
        card.into_any_element()
    }

    /// A tool's logo for a line's icon.
    pub(super) fn logo(&self, kind: EnvironmentKind, window: &Window, cx: &mut App) -> AnyElement {
        environment_logo(kind, 16., window, cx).into_any_element()
    }
}

/// Where new SSH hosts are saved by default (Settings → Behavior).
fn new_host_storage(cx: &App) -> muxus_store::prefs::state::NewSshHostStorage {
    SettingsPrefs::global(cx)
        .map(|prefs| prefs.read(cx).prefs().new_ssh_host_storage)
        .unwrap_or(muxus_store::prefs::state::NewSshHostStorage::Openssh)
}

impl Render for EnvironmentsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let (scope, entries): (PanelScope, Vec<HostEnvironmentsEntry>) = {
            let hub = self.hub.read(cx);
            (
                hub.panel().clone(),
                hub.store().entries().cloned().collect(),
            )
        };
        let scoped_entry = scope
            .host_key()
            .and_then(|key| entries.iter().find(|entry| entry.host.key == key))
            .cloned();
        let hosts: Vec<HostEnvironmentsEntry> = match scope.host_key() {
            Some(_) => scoped_entry.iter().cloned().collect(),
            None => entries
                .into_iter()
                // Hosts found on connect that run nothing listable stay out of the overview.
                .filter(|entry| {
                    entry.host.key == LOCAL_ENVIRONMENT_KEY
                        || !entry.automatic
                        || !entry.environments.is_empty()
                })
                .collect(),
        };
        let on_right = Self::sidebar_on_right(cx);
        let width = self.panel_width(window, cx);
        let header = self.render_header(scoped_entry.as_ref(), &hosts, &theme, cx);
        let filter = self.render_filter(&theme, cx);
        let mut tree = div()
            .id("environments-tree")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb(Theme::spacing(2.));
        if hosts.is_empty() {
            tree = tree.child(
                div().px(Theme::spacing(1.5)).py(Theme::spacing(2.)).child(
                    paragraph(
                        "Nothing found yet. Hosts appear here once Muxus connects to them and finds Docker, Podman, Containerlab, Kubernetes, Proxmox or libvirt. To look at a host now, right-click it in the sidebar and choose Containers & VMs.",
                    )
                    .variant(theme.typography.body2)
                    .color(p.text.secondary),
                ),
            );
        }
        let scoped = scope.host_key().is_some();
        for entry in &hosts {
            let card = self.render_host(entry, scoped, &theme, window, cx);
            tree = tree.child(card);
        }
        let menu = self.render_menu(&theme, cx);
        let dialogs = self
            .dialogs
            .render(|this: &mut Self| &mut this.dialogs, window, cx);
        let grip = (!self.widened).then(|| self.render_grip(width, on_right, &theme, cx));
        let tracker = self.render_drag_tracker(on_right, cx);
        div()
            .id("environments-panel")
            .relative()
            .w(px(width))
            .min_w(px(MIN_ENVIRONMENTS_PANEL_WIDTH as f32))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(p.sidebar)
            .border_l_1()
            .border_r_1()
            .border_color(p.divider)
            .font_family(fonts::UI)
            .child(header)
            .child(filter)
            .child(tree)
            .children(grip)
            .children(tracker)
            .children(menu)
            .children(
                self.topology
                    .as_ref()
                    .map(|(dialog, _)| dialog.clone().into_any_element()),
            )
            .children(dialogs)
    }
}

/// Create the panel for a window's hub.
pub fn environments_panel(
    hub: Entity<EnvironmentsHub>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<EnvironmentsPanel> {
    cx.new(|cx| EnvironmentsPanel::new(hub, window, cx))
}
