//! The sidebar's context menus: `components/sidebar/{HostContextMenu,
//! FolderContextMenu,PanelContextMenu,LocalShellContextMenu}.tsx`, item for
//! item. Actions that open dialogs or write to the store go to the window as
//! [`SidebarEvent::Action`].

use muxus_app::hosts::ManagedHost;
use muxus_app::hosts::managed::{managed_host_copy_command, managed_host_supports_sftp};
use muxus_app::hosts::tree::{
    ContainerRef, TreeChild, all_containers, folder_siblings, sibling_host_keys,
};
use muxus_ui::gpui_kit::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, IntoElement, ParentElement, Pixels, Point,
    Window,
};
use muxus_ui::theme::Theme;
use muxus_ui::widgets::menu::{self, MenuItem, menu_divider};

use crate::ui::sidebar::{SessionSidebar, SidebarAction, SidebarEvent};
use muxus_app::features::host_editor::view::NewHost;
use muxus_app::hosts::tree::collect_hosts;

/// Which context menu is showing, and where.
#[derive(Clone, Debug)]
pub enum SidebarMenu {
    Host {
        host: ManagedHost,
        position: Point<Pixels>,
    },
    Folder {
        key: String,
        position: Point<Pixels>,
    },
    Panel {
        position: Point<Pixels>,
    },
    LocalShell {
        position: Point<Pixels>,
    },
}

impl SessionSidebar {
    /// A menu item handler that hands `action` to the window.
    fn action(
        action: SidebarAction,
        cx: &mut Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.menu = None;
            cx.emit(SidebarEvent::Action(action.clone()));
            cx.notify();
        })
    }

    /// The hosts of the selection, resolved against every host (a host the search hides
    /// is still selected; a deleted one drops out).
    pub(crate) fn selected_hosts(&self) -> Vec<ManagedHost> {
        let all = self.all_hosts();
        self.selection
            .iter()
            .filter_map(|key| all.iter().find(|host| &host.key() == key).cloned())
            .collect()
    }

    /// Where a host sits among its siblings: (index, total).
    fn host_position(&self, key: &str) -> (Option<usize>, usize) {
        for container in all_containers(&self.tree) {
            let keys = sibling_host_keys(&self.tree, container);
            if let Some(index) = keys.iter().position(|candidate| candidate == key) {
                return (Some(index), keys.len());
            }
        }
        (None, 0)
    }

    /// Alt+Arrow on a folder, or Move up/down in its menu: swap it with the
    /// sibling folder next to it.
    pub fn move_folder(&mut self, key: &str, delta: isize, cx: &mut Context<Self>) {
        if !self.reorder_enabled() {
            return;
        }
        let Some(siblings) = folder_siblings(&self.tree, key) else {
            return;
        };
        let mut keys = siblings.keys.clone();
        let Some(from) = keys.iter().position(|candidate| candidate == key) else {
            return;
        };
        let to = from as isize + delta;
        if to < 0 || to as usize >= keys.len() {
            return;
        }
        keys.swap(from, to as usize);
        self.folders.set_folder_order(&siblings.parent_key, &keys);
        self.rebuild();
        cx.notify();
    }

    /// Collapse a folder and everything nested inside it, in one write.
    fn collapse_subtree(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(root) = self.tree.find_container(key) else {
            return;
        };
        let mut keys = Vec::new();
        let mut stack = vec![root];
        while let Some(container) = stack.pop() {
            if let ContainerRef::Folder(id) = container {
                let folder = self.tree.folder(id);
                keys.push(folder.key.clone());
                for child in &folder.children {
                    if let TreeChild::Folder(child) = child {
                        stack.push(ContainerRef::Folder(*child));
                    }
                }
            }
        }
        for key in keys {
            self.set_collapsed_quietly(&key, true);
        }
        self.rebuild();
        cx.notify();
    }

    pub(crate) fn render_menu(
        &self,
        menu: SidebarMenu,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (position, paper) = match menu {
            SidebarMenu::Host { host, position } => {
                let key = host.key();
                let (index, total) = self.host_position(&key);
                let reorder = self.reorder_enabled();
                let can_up = reorder && index.is_some_and(|index| index > 0);
                let can_down = reorder && index.is_some_and(|index| index + 1 < total);
                let selected = if self.selection.contains(&key) {
                    self.selection.len()
                } else {
                    0
                };
                let for_selection = selected > 1;
                let targets = if for_selection {
                    self.selected_hosts()
                } else {
                    vec![host.clone()]
                };
                let copy = managed_host_copy_command(&host, &self.catalog.profiles);
                let ssh = match &host {
                    ManagedHost::Ssh(_) => true,
                    ManagedHost::Profile(entry) => {
                        entry.kind == muxus_app::model::SavedHostKind::Ssh
                    }
                };
                let connect = host.clone();
                let mut paper = menu::paper(theme)
                    .child(
                        MenuItem::new("host-connect", "Connect")
                            .icon("PlayArrowOutlined")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.menu = None;
                                cx.emit(SidebarEvent::OpenHost {
                                    host: connect.clone(),
                                    new_session: true,
                                    repeat: false,
                                    anchor: None,
                                });
                                cx.notify();
                            })),
                    )
                    .child(
                        MenuItem::new("host-new-window", "Open in new window")
                            .icon("OpenInNewOutlined")
                            .on_click(Self::action(
                                SidebarAction::OpenInNewWindow(host.clone()),
                                cx,
                            )),
                    );
                if managed_host_supports_sftp(&host) {
                    paper = paper.child(
                        MenuItem::new("host-sftp", "Open SFTP file browser")
                            .icon("FolderOpenOutlined")
                            .on_click(Self::action(SidebarAction::OpenSftp(host.clone()), cx)),
                    );
                }
                paper = paper
                    .child(
                        MenuItem::new("host-up", "Move up")
                            .icon("KeyboardArrowUp")
                            .disabled(!can_up)
                            .on_click(Self::action(
                                SidebarAction::MoveHost {
                                    key: key.clone(),
                                    delta: -1,
                                },
                                cx,
                            )),
                    )
                    .child(
                        MenuItem::new("host-down", "Move down")
                            .icon("KeyboardArrowDown")
                            .disabled(!can_down)
                            .on_click(Self::action(
                                SidebarAction::MoveHost {
                                    key: key.clone(),
                                    delta: 1,
                                },
                                cx,
                            )),
                    )
                    .child(menu_divider(theme))
                    .child(
                        MenuItem::new(
                            "host-move",
                            if for_selection {
                                format!("Move {selected} selected hosts…")
                            } else {
                                "Move to folder…".into()
                            },
                        )
                        .icon("DriveFileMoveOutlined")
                        .on_click(Self::action(SidebarAction::MoveHosts(targets.clone()), cx)),
                    )
                    .child(
                        MenuItem::new("host-organize", "Organize & color…")
                            .icon("PaletteOutlined")
                            .on_click(Self::action(SidebarAction::Organize(host.clone()), cx)),
                    )
                    .child(
                        MenuItem::new("host-edit", "Edit host")
                            .icon("EditOutlined")
                            .on_click(Self::action(
                                SidebarAction::EditHosts(vec![host.clone()]),
                                cx,
                            )),
                    );
                if for_selection {
                    paper = paper.child(
                        MenuItem::new(
                            "host-edit-selected",
                            format!("Edit {selected} selected hosts…"),
                        )
                        .icon("EditNoteOutlined")
                        .on_click(Self::action(SidebarAction::EditHosts(targets.clone()), cx)),
                    );
                }
                paper = paper.child(
                    MenuItem::new("host-duplicate", "Duplicate")
                        .icon("LibraryAddOutlined")
                        .on_click(Self::action(SidebarAction::Duplicate(host.clone()), cx)),
                );
                if ssh && !for_selection {
                    paper = paper
                        .child(
                            MenuItem::new("host-gnmi", "Add gNMI host for it…")
                                .icon("SensorsOutlined")
                                .on_click(Self::action(
                                    SidebarAction::NewHost(management_host_for(&host, true)),
                                    cx,
                                )),
                        )
                        .child(
                            MenuItem::new("host-netconf", "Add NETCONF host for it…")
                                .icon("AccountTreeOutlined")
                                .on_click(Self::action(
                                    SidebarAction::NewHost(management_host_for(&host, false)),
                                    cx,
                                )),
                        );
                }
                let text = copy.text.clone();
                paper = paper
                    .child(
                        MenuItem::new("host-copy", copy.label)
                            .icon("ContentCopy")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.menu = None;
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                cx.emit(SidebarEvent::Copied(text.clone()));
                                cx.notify();
                            })),
                    )
                    .child(menu_divider(theme))
                    .child(
                        MenuItem::new(
                            "host-delete",
                            if for_selection {
                                format!("Delete {selected} selected hosts")
                            } else {
                                "Delete host".into()
                            },
                        )
                        .icon("DeleteOutlined")
                        .destructive()
                        .on_click(Self::action(SidebarAction::DeleteHosts(targets), cx)),
                    );
                (position, paper)
            }
            SidebarMenu::Folder { key, position } => {
                let count = self
                    .tree
                    .find_container(&key)
                    .map(|container| self.tree.descendant_host_count(container))
                    .unwrap_or(0);
                let siblings = folder_siblings(&self.tree, &key);
                let index = siblings
                    .as_ref()
                    .and_then(|s| s.keys.iter().position(|k| k == &key));
                let total = siblings.as_ref().map_or(0, |s| s.keys.len());
                let reorder = self.reorder_enabled();
                let can_up = reorder && index.is_some_and(|index| index > 0);
                let can_down = reorder && index.is_some_and(|index| index + 1 < total);
                let direct_hosts = self
                    .tree
                    .find_container(&key)
                    .map(|container| sibling_host_keys(&self.tree, container).len())
                    .unwrap_or(0);
                let launch = if count > 0 {
                    format!("Launch {count} host{}…", if count == 1 { "" } else { "s" })
                } else {
                    "Launch hosts…".into()
                };
                let edit = match count {
                    0 => "Edit hosts…".to_string(),
                    1 => "Edit host…".to_string(),
                    n => format!("Edit {n} hosts…"),
                };
                let (up_key, down_key, collapse_key) = (key.clone(), key.clone(), key.clone());
                let folder = self.tree.folder_by_key(&key).cloned();
                let (path, label) = folder
                    .as_ref()
                    .map(|folder| (folder.path.clone(), folder.label.clone()))
                    .unwrap_or_default();
                let hosts = self
                    .tree
                    .find_container(&key)
                    .map(|container| collect_hosts(&self.tree, container))
                    .unwrap_or_default();
                let direct: Vec<ManagedHost> = self
                    .tree
                    .find_container(&key)
                    .map(|container| {
                        let keys = sibling_host_keys(&self.tree, container);
                        keys.iter()
                            .filter_map(|key| {
                                self.tree
                                    .hosts
                                    .iter()
                                    .find(|host| &host.key() == key)
                                    .cloned()
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let paper = menu::paper(theme)
                    .child(
                        MenuItem::new("folder-launch", launch)
                            .icon("PlayArrowOutlined")
                            .disabled(count == 0)
                            .on_click(Self::action(
                                SidebarAction::LaunchHosts {
                                    label: label.clone(),
                                    hosts: hosts.clone(),
                                },
                                cx,
                            )),
                    )
                    .child(
                        MenuItem::new("folder-edit-hosts", edit)
                            .icon("EditNoteOutlined")
                            .disabled(count == 0)
                            .on_click(Self::action(SidebarAction::EditHosts(hosts.clone()), cx)),
                    )
                    .child(menu_divider(theme))
                    .child(
                        MenuItem::new("folder-up", "Move up")
                            .icon("KeyboardArrowUp")
                            .disabled(!can_up)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.menu = None;
                                this.move_folder(&up_key, -1, cx);
                            })),
                    )
                    .child(
                        MenuItem::new("folder-down", "Move down")
                            .icon("KeyboardArrowDown")
                            .disabled(!can_down)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.menu = None;
                                this.move_folder(&down_key, 1, cx);
                            })),
                    )
                    .child(
                        MenuItem::new("folder-sort", "Sort hosts alphabetically")
                            .icon("SortByAlpha")
                            .disabled(!(reorder && direct_hosts > 1))
                            .on_click(Self::action(SidebarAction::SortHosts(direct.clone()), cx)),
                    )
                    .child(menu_divider(theme))
                    .child(
                        MenuItem::new("folder-new-host", "New host…")
                            .icon("DnsOutlined")
                            .on_click(Self::action(
                                SidebarAction::NewHost(NewHost {
                                    group: Some(path.clone()),
                                    ..NewHost::default()
                                }),
                                cx,
                            )),
                    )
                    .child(
                        MenuItem::new("folder-new-folder", "New folder inside")
                            .icon("CreateNewFolderOutlined")
                            .on_click(Self::action(
                                SidebarAction::NewFolder {
                                    parent: Some(path.clone()),
                                },
                                cx,
                            )),
                    )
                    .child(
                        MenuItem::new("folder-edit", "Rename, move & style…")
                            .icon("EditOutlined")
                            .on_click(Self::action(
                                SidebarAction::EditFolder { path: path.clone() },
                                cx,
                            )),
                    )
                    .child(
                        MenuItem::new("folder-collapse", "Collapse all inside")
                            .icon("UnfoldLess")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.menu = None;
                                this.collapse_subtree(&collapse_key, cx);
                            })),
                    )
                    .child(menu_divider(theme))
                    .child(
                        MenuItem::new("folder-delete", "Delete folder")
                            .icon("DeleteOutlined")
                            .destructive()
                            .on_click(Self::action(
                                SidebarAction::DeleteFolder {
                                    key: key.clone(),
                                    path: path.clone(),
                                    label: label.clone(),
                                },
                                cx,
                            )),
                    );
                (position, paper)
            }
            SidebarMenu::Panel { position } => {
                let folder_edits = !self.filtering();
                let sortable = self.reorder_enabled() && self.tree.hosts.len() > 1;
                let move_label = if self.on_right {
                    "Move sidebar to the left"
                } else {
                    "Move sidebar to the right"
                };
                let paper = menu::paper(theme)
                    .child(
                        MenuItem::new("panel-new-host", "New host…")
                            .icon("DnsOutlined")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                cx.emit(SidebarEvent::AddHost { prefill: None });
                                cx.notify();
                            })),
                    )
                    .child(
                        MenuItem::new("panel-new-folder", "New folder…")
                            .icon("CreateNewFolderOutlined")
                            .disabled(!folder_edits)
                            .on_click(Self::action(SidebarAction::NewFolder { parent: None }, cx)),
                    )
                    .child(menu_divider(theme))
                    .child(
                        MenuItem::new("panel-sort", "Sort all hosts alphabetically")
                            .icon("SortByAlpha")
                            .disabled(!sortable)
                            .on_click(Self::action(SidebarAction::SortHosts(self.all_hosts()), cx)),
                    )
                    .child(menu_divider(theme))
                    .child(
                        MenuItem::new("panel-move-sidebar", move_label)
                            .icon("ViewSidebarOutlined")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                cx.emit(SidebarEvent::MoveSidebar);
                                cx.notify();
                            })),
                    );
                (position, paper)
            }
            SidebarMenu::LocalShell { position } => {
                let paper = menu::paper(theme)
                    .child(
                        MenuItem::new("shell-open", "Open")
                            .icon("Terminal")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                cx.emit(SidebarEvent::OpenLocalTerminal);
                                cx.notify();
                            })),
                    )
                    .child(menu_divider(theme))
                    .child(
                        MenuItem::new("shell-settings", "Local shell settings…")
                            .icon("SettingsOutlined")
                            .on_click(Self::action(SidebarAction::LocalShellSettings, cx)),
                    );
                (position, paper)
            }
        };
        let sidebar = cx.entity().downgrade();
        let dismiss = move |_: &mut Window, cx: &mut App| {
            let _ = sidebar.update(cx, |this, cx| this.close_menu(cx));
        };
        menu::popup(position, dismiss, paper).into_any_element()
    }
}

/// `managementHostFor`: a new gNMI (`gnmi`) or NETCONF host for an SSH host, its address,
/// user, folder and a name saying which protocol it speaks.
fn management_host_for(host: &ManagedHost, gnmi: bool) -> NewHost {
    let protocol = if gnmi { "gNMI" } else { "NETCONF" };
    let kind = if gnmi {
        muxus_app::model::SavedHostKind::Gnmi
    } else {
        muxus_app::model::SavedHostKind::Netconf
    };
    match host {
        ManagedHost::Ssh(entry) => {
            let target = if gnmi {
                entry.resolved.hostname.clone()
            } else {
                entry.alias.clone()
            };
            let user = entry
                .resolved
                .user
                .as_deref()
                .map(|user| format!("{user}@"))
                .unwrap_or_default();
            NewHost {
                prefill_target: Some(format!("{user}{target}")),
                prefill_name: Some(format!(
                    "{} ({protocol})",
                    muxus_app::hosts::managed::managed_host_display_name(host)
                )),
                group: entry.metadata.as_ref().and_then(|m| m.group.clone()),
                kind: Some(kind),
            }
        }
        ManagedHost::Profile(entry) => {
            let (address, user) = match &entry.profile {
                muxus_app::model::SavedHostSessionProfile::Ssh(profile) => {
                    let address = match profile.target.split_once('@') {
                        Some((_, rest)) => rest.to_string(),
                        None => profile.target.clone(),
                    };
                    let user = profile.user.clone().or_else(|| {
                        profile
                            .target
                            .split_once('@')
                            .map(|(user, _)| user.to_string())
                    });
                    (address, user)
                }
                _ => (String::new(), None),
            };
            let parts: Vec<&str> = address.split(':').collect();
            let target = if parts.len() == 2 {
                parts[0].to_string()
            } else {
                address.clone()
            };
            let user = user.map(|user| format!("{user}@")).unwrap_or_default();
            NewHost {
                prefill_target: Some(format!("{user}{target}")),
                prefill_name: Some(format!(
                    "{} ({protocol})",
                    muxus_app::hosts::managed::managed_host_display_name(host)
                )),
                group: entry.metadata.group.clone(),
                kind: Some(kind),
            }
        }
    }
}
