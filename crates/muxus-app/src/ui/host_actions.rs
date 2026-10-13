//! What the sidebar's host and folder actions do in the window (`SessionSidebar.tsx`,
//! `HostContextMenu.tsx`, `FolderContextMenu.tsx`): the host editors and dialogs of
//! `features::host_editor`, deletes behind a confirmation, and order writes.

use std::time::Instant;

use muxus_app::features::host_editor::dialogs::bulk::{BulkEditEvent, open_bulk_edit};
use muxus_app::features::host_editor::dialogs::folder::{FolderDialogEvent, open_folder_dialog};
use muxus_app::features::host_editor::dialogs::launch_group::{
    LaunchGroupEvent, LaunchTarget, launch_toast, open_launch_group,
};
use muxus_app::features::host_editor::dialogs::organize::{OrganizeEvent, open_organize};
use muxus_app::features::host_editor::logic::folder::FolderDialogState;
use muxus_app::features::host_editor::services::{
    self, ConnectTarget, delete_folder_question, delete_hosts_question, delete_question,
};
use muxus_app::features::host_editor::view::{
    HostEditorEvent, HostEditorTarget, NewHost, open_host_editor,
};
use muxus_app::features::settings::{SettingsSection, SettingsTarget};
use muxus_app::hosts::ManagedHost;
use muxus_app::hosts::managed::{alphabetize_managed_hosts, managed_host_ref};
use muxus_app::hosts::tree::{all_containers, is_descendant_path, is_same_path, sibling_host_keys};
use muxus_app::sidebar::folder_mutations::{delete_folder_plan, move_hosts_plan};
use muxus_app::state::session_actions::{connect_managed_host, connect_target, session_set_entry};
use muxus_app::state::tabs::TabStatus;
use muxus_core::api::HostOrderRequest;
use muxus_ui::gpui_kit::{Context, Window};
use muxus_ui::theme::Status;

use crate::ui::confirm::{ConfirmAction, ConfirmRequest};
use crate::ui::shell::AppShell;
use crate::ui::sidebar::SidebarAction;

impl AppShell {
    /// Every host the sidebar lists, in both catalogs.
    pub fn all_managed_hosts(&self) -> Vec<ManagedHost> {
        self.catalog
            .ssh
            .hosts
            .iter()
            .cloned()
            .map(ManagedHost::ssh)
            .chain(
                self.catalog
                    .profiles
                    .iter()
                    .cloned()
                    .map(ManagedHost::profile),
            )
            .collect()
    }

    fn hosts_by_keys(&self, keys: &[String]) -> Vec<ManagedHost> {
        let all = self.all_managed_hosts();
        keys.iter()
            .filter_map(|key| all.iter().find(|host| &host.key() == key).cloned())
            .collect()
    }

    /// `setHostEditor`: the host editor over this window.
    pub fn open_host_editor(
        &mut self,
        target: HostEditorTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.data.backend() else {
            self.show_toast(Status::Info, "Hosts need the Muxus backend.", cx);
            return;
        };
        let editor = open_host_editor(window, cx, backend, target);
        self.mount_overlay(
            "host-editor",
            editor,
            window,
            cx,
            |this, _, event: &HostEditorEvent, window, cx| match event {
                HostEditorEvent::Close => this.close_overlay("host-editor", window, cx),
                HostEditorEvent::Deleted => {
                    this.load_catalog(cx);
                    this.close_overlay("host-editor", window, cx);
                }
                HostEditorEvent::Saved { connect } => {
                    this.load_catalog(cx);
                    this.close_overlay("host-editor", window, cx);
                    let now = Instant::now();
                    match connect {
                        Some(ConnectTarget::Alias(alias)) => {
                            connect_target(
                                &mut this.tabs,
                                &mut this.launch_guard,
                                alias,
                                alias,
                                None,
                                now,
                            );
                        }
                        Some(ConnectTarget::SavedHost(profile)) => {
                            connect_managed_host(
                                &mut this.tabs,
                                &mut this.launch_guard,
                                &ManagedHost::profile((**profile).clone()),
                                None,
                                now,
                            );
                        }
                        None => {}
                    }
                    this.tabs_changed(cx);
                }
                HostEditorEvent::Toast { status, message } => {
                    this.show_toast(*status, message.clone(), cx)
                }
            },
        );
    }

    /// `editHosts`: one host opens its own editor; several open the bulk editor.
    fn edit_hosts(&mut self, hosts: Vec<ManagedHost>, window: &mut Window, cx: &mut Context<Self>) {
        match hosts.as_slice() {
            [] => {}
            [host] => self.open_host_editor(HostEditorTarget::Edit(host.clone()), window, cx),
            _ => {
                let Some(backend) = self.data.backend() else {
                    return;
                };
                let keys = hosts.iter().map(ManagedHost::key).collect();
                let dialog = open_bulk_edit(window, cx, backend, keys);
                self.mount_overlay(
                    "bulk-edit",
                    dialog,
                    window,
                    cx,
                    |this, _, event: &BulkEditEvent, window, cx| match event {
                        BulkEditEvent::Close => {
                            this.load_catalog(cx);
                            this.close_overlay("bulk-edit", window, cx);
                        }
                        BulkEditEvent::Toast { status, message } => {
                            this.load_catalog(cx);
                            this.show_toast(*status, message.clone(), cx);
                        }
                    },
                );
            }
        }
    }

    fn open_folder_dialog(
        &mut self,
        state: FolderDialogState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let prefs = self.sidebar.read(cx).folders.clone();
        let dialog = open_folder_dialog(window, cx, backend, state, prefs);
        self.mount_overlay(
            "folder-dialog",
            dialog,
            window,
            cx,
            |this, _, event: &FolderDialogEvent, window, cx| match event {
                FolderDialogEvent::Close => this.close_overlay("folder-dialog", window, cx),
                FolderDialogEvent::Submitted { prefs, toast } => {
                    let changes = prefs.clone();
                    this.sidebar.update(cx, |sidebar, cx| {
                        for change in &changes {
                            change.apply(&mut sidebar.folders);
                        }
                        sidebar.rebuild();
                        cx.notify();
                    });
                    this.load_catalog(cx);
                    if let Some(toast) = toast {
                        this.show_toast(Status::Success, toast.clone(), cx);
                    }
                    this.close_overlay("folder-dialog", window, cx);
                }
                FolderDialogEvent::Toast { status, message } => {
                    this.show_toast(*status, message.clone(), cx)
                }
            },
        );
    }

    /// `setLaunchTarget`: start a folder's hosts in a chosen layout.
    fn open_launch_group(
        &mut self,
        label: String,
        hosts: Vec<ManagedHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let live = self
            .tabs
            .tabs
            .iter()
            .any(|tab| tab.status == TabStatus::Connected);
        let dialog = open_launch_group(window, cx, LaunchTarget { label, hosts }, live);
        self.mount_overlay(
            "launch-group",
            dialog,
            window,
            cx,
            |this, _, event: &LaunchGroupEvent, window, cx| match event {
                LaunchGroupEvent::Close => this.close_overlay("launch-group", window, cx),
                LaunchGroupEvent::Launch {
                    hosts,
                    layout,
                    label,
                } => {
                    let entries: Vec<_> = hosts.iter().map(session_set_entry).collect();
                    let started = this.tabs.launch_set(&entries, *layout);
                    this.clear_multi_exec_groups(cx);
                    this.close_overlay("launch-group", window, cx);
                    if !started.is_empty() {
                        this.show_toast(
                            Status::Info,
                            launch_toast(started.len(), label, *layout),
                            cx,
                        );
                    }
                    this.tabs_changed(cx);
                }
            },
        );
    }

    /// Write one sibling order (`reorder.mutate`): both catalogs must be complete.
    fn commit_order(&mut self, keys: Vec<String>, cx: &mut Context<Self>) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let hosts = self.hosts_by_keys(&keys);
        let request = HostOrderRequest {
            hosts: hosts
                .iter()
                .map(|host| match managed_host_ref(host) {
                    muxus_app::hosts::managed::ManagedHostRef::Ssh { alias } => {
                        muxus_core::api::ManagedHostRef::Ssh { alias }
                    }
                    muxus_app::hosts::managed::ManagedHostRef::Profile { id } => {
                        muxus_core::api::ManagedHostRef::Profile { id }
                    }
                })
                .collect(),
        };
        cx.spawn(async move |this, cx| {
            let result = backend.put_host_order(request).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.load_catalog(cx),
                Err(error) => this.show_toast(
                    Status::Error,
                    format!("Could not save the host order: {}", error.message),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Reorder a host among its siblings (folders are always alphabetical).
    fn move_host_by_key(&mut self, key: &str, delta: isize, cx: &mut Context<Self>) {
        let order = {
            let sidebar = self.sidebar.read(cx);
            if !sidebar.reorder_enabled() {
                return;
            }
            let tree = &sidebar.tree;
            all_containers(tree).into_iter().find_map(|container| {
                let mut keys = sibling_host_keys(tree, container);
                let from = keys.iter().position(|candidate| candidate == key)?;
                let to = from as isize + delta;
                if to < 0 || to as usize >= keys.len() {
                    return Some(None);
                }
                keys.swap(from, to as usize);
                Some(Some(keys))
            })
        };
        if let Some(Some(keys)) = order {
            self.commit_order(keys, cx);
        }
    }

    fn request_delete_hosts(
        &mut self,
        hosts: Vec<ManagedHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, description, confirm_label) = match hosts.as_slice() {
            [] => return,
            [host] => {
                let (title, description) = delete_question(host);
                (title, description, "Delete".to_string())
            }
            _ => delete_hosts_question(&hosts),
        };
        self.open_confirm(
            ConfirmRequest {
                title,
                description: Some(description),
                confirm_label,
                destructive: true,
                checkbox: None,
                action: ConfirmAction::DeleteHosts(hosts.iter().map(ManagedHost::key).collect()),
            },
            window,
            cx,
        );
    }

    /// The confirmed delete of hosts (one by one, as `useDeleteManagedHosts` does).
    pub fn delete_hosts_now(&mut self, keys: &[String], cx: &mut Context<Self>) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let hosts = self.hosts_by_keys(keys);
        let several = hosts.len() > 1;
        cx.spawn(async move |this, cx| {
            let mut failed = Vec::new();
            for host in &hosts {
                if let Err(message) = services::delete_host(&backend, host).await {
                    failed.push(message);
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.load_catalog(cx);
                if several {
                    this.sidebar.update(cx, |sidebar, cx| {
                        sidebar.selection.clear();
                        cx.notify();
                    });
                }
                if let Some(message) = failed.first() {
                    this.show_toast(Status::Error, message.clone(), cx);
                }
            });
        })
        .detach();
    }

    fn request_delete_folder(
        &mut self,
        key: String,
        path: String,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let moves = delete_folder_plan(&self.all_managed_hosts(), &path).len();
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let has_credentials = backend.folder_settings().await.ok().map(|settings| {
                settings.folders.iter().any(|folder| {
                    is_same_path(&folder.path, &path) || is_descendant_path(&folder.path, &path)
                })
            });
            let (title, description) =
                delete_folder_question(&label, &path, moves, has_credentials);
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.open_confirm(
                        ConfirmRequest {
                            title,
                            description: Some(description),
                            confirm_label: "Delete folder".into(),
                            destructive: true,
                            checkbox: None,
                            action: ConfirmAction::DeleteFolder { key, path, label },
                        },
                        window,
                        cx,
                    )
                });
            });
        })
        .detach();
    }

    /// The confirmed folder delete: its hosts move up, its style and shared credentials go.
    pub fn delete_folder_now(
        &mut self,
        key: &str,
        path: &str,
        label: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let moves = delete_folder_plan(&self.all_managed_hosts(), path);
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.folders.remove_empty_folder(path);
            sidebar.folders.set_folder_style(key, None);
            sidebar.rebuild();
            cx.notify();
        });
        let (path, label) = (path.to_string(), label.to_string());
        cx.spawn(async move |this, cx| {
            if !moves.is_empty() {
                let (_, failed) = services::apply_folder_moves(&backend, &moves).await;
                if failed > 0 {
                    let _ = this.update(cx, |this, cx| {
                        this.show_toast(
                            Status::Error,
                            format!("Some hosts could not leave “{label}”."),
                            cx,
                        )
                    });
                }
            }
            // Idempotent, and must not depend on folder settings having loaded.
            let _ = backend.delete_folder_settings(&path).await;
            let _ = this.update(cx, |this, cx| this.load_catalog(cx));
        })
        .detach();
    }

    /// `moveHosts`: pick a folder for hosts, starting from the one they share.
    fn move_hosts(&mut self, hosts: Vec<ManagedHost>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(first) = hosts.first() else {
            return;
        };
        let group = |host: &ManagedHost| host.group().unwrap_or("").to_string();
        let path = group(first);
        let shared = hosts.iter().all(|host| is_same_path(&group(host), &path));
        self.open_folder_dialog(
            FolderDialogState::MoveHosts {
                host_keys: hosts.iter().map(ManagedHost::key).collect(),
                host_name: (hosts.len() == 1)
                    .then(|| muxus_app::hosts::managed::managed_host_display_name(first).into()),
                current_path: if shared { path } else { String::new() },
            },
            window,
            cx,
        );
    }

    /// `openManagedHostSftp`: the file browser on a usable tab for this host, dialling a
    /// session only when there is nothing to reuse.
    fn open_host_sftp(&mut self, host: &ManagedHost, cx: &mut Context<Self>) {
        if !muxus_app::hosts::managed::managed_host_supports_sftp(host) {
            return;
        }
        let same_host = |tab: &muxus_app::state::tabs::TerminalTab| match (&tab.profile, host) {
            (Some(muxus_app::model::SessionProfile::Ssh(profile)), ManagedHost::Ssh(entry)) => {
                profile.profile_id.is_none() && profile.target == entry.alias
            }
            (Some(muxus_app::model::SessionProfile::Ssh(profile)), ManagedHost::Profile(entry)) => {
                profile.profile_id.as_deref() == Some(entry.id.as_str())
            }
            _ => false,
        };
        let matching: Vec<_> = self
            .tabs
            .tabs
            .iter()
            .filter(|tab| {
                same_host(tab)
                    && matches!(tab.status, TabStatus::Connecting | TabStatus::Connected)
                    && tab.sftp_available != Some(false)
            })
            .cloned()
            .collect();
        let active = self.tabs.active_id.clone();
        let reusable = matching
            .iter()
            .find(|tab| Some(&tab.id) == active.as_ref() && tab.conn_id.is_some())
            .or_else(|| matching.iter().find(|tab| tab.conn_id.is_some()))
            .or_else(|| matching.iter().find(|tab| Some(&tab.id) == active.as_ref()))
            .or_else(|| matching.first())
            .map(|tab| tab.id.clone());
        let id = reusable.unwrap_or_else(|| {
            connect_managed_host(
                &mut self.tabs,
                &mut self.launch_guard,
                host,
                None,
                Instant::now(),
            )
        });
        self.show_file_browser(&id, cx);
        self.tabs.activate(&id);
        self.tabs_changed(cx);
    }

    /// Carry out a sidebar menu action.
    pub fn on_sidebar_action(
        &mut self,
        action: &SidebarAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action.clone() {
            SidebarAction::OpenInNewWindow(host) => {
                crate::ui::open_main_window(crate::ui::MainWindowLaunch::Host(Box::new(host)), cx)
            }
            SidebarAction::OpenSftp(host) => self.open_host_sftp(&host, cx),
            SidebarAction::MoveHost { key, delta } => self.move_host_by_key(&key, delta, cx),
            SidebarAction::MoveHosts(hosts) => self.move_hosts(hosts, window, cx),
            SidebarAction::Organize(host) => {
                let Some(backend) = self.data.backend() else {
                    return;
                };
                let dialog = open_organize(window, cx, backend, host);
                self.mount_overlay(
                    "organize",
                    dialog,
                    window,
                    cx,
                    |this, _, event: &OrganizeEvent, window, cx| match event {
                        OrganizeEvent::Close => this.close_overlay("organize", window, cx),
                        OrganizeEvent::Saved => {
                            this.load_catalog(cx);
                            this.close_overlay("organize", window, cx);
                        }
                        OrganizeEvent::Toast { status, message } => {
                            this.show_toast(*status, message.clone(), cx)
                        }
                    },
                );
            }
            SidebarAction::EditHosts(hosts) => self.edit_hosts(hosts, window, cx),
            SidebarAction::Duplicate(host) => {
                self.open_host_editor(HostEditorTarget::Duplicate(host), window, cx)
            }
            SidebarAction::NewHost(new) => {
                self.open_host_editor(HostEditorTarget::New(new), window, cx)
            }
            SidebarAction::DeleteHosts(hosts) => self.request_delete_hosts(hosts, window, cx),
            SidebarAction::LaunchHosts { label, hosts } => {
                if !hosts.is_empty() {
                    self.open_launch_group(label, hosts, window, cx)
                }
            }
            SidebarAction::SortHosts(hosts) => {
                if self.sidebar.read(cx).reorder_enabled() && hosts.len() > 1 {
                    let keys = alphabetize_managed_hosts(&hosts)
                        .iter()
                        .map(ManagedHost::key)
                        .collect();
                    self.commit_order(keys, cx);
                }
            }
            SidebarAction::DropHosts(drop) => {
                self.drop_hosts(drop.host_keys, drop.path, drop.order, cx)
            }
            SidebarAction::DropFolder(drop) => self.drop_folder(drop, cx),
            SidebarAction::NewFolder { parent } => self.open_folder_dialog(
                FolderDialogState::New {
                    parent_path: parent,
                },
                window,
                cx,
            ),
            SidebarAction::EditFolder { path } => {
                self.open_folder_dialog(FolderDialogState::Edit { path }, window, cx)
            }
            SidebarAction::DeleteFolder { key, path, label } => {
                self.request_delete_folder(key, path, label, window, cx)
            }
            SidebarAction::LocalShellSettings => self.open_settings(
                Some(SettingsTarget::section(SettingsSection::LocalShells)),
                cx,
            ),
        }
    }

    /// Hosts dropped into a folder: change the folder of every host that moved, then write
    /// their order (`dropHosts`).
    pub fn drop_hosts(
        &mut self,
        host_keys: Vec<String>,
        path: Option<String>,
        order: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let moves = match &path {
            Some(path) => move_hosts_plan(&self.hosts_by_keys(&host_keys), path),
            None => Vec::new(),
        };
        if moves.is_empty() {
            self.commit_order(order, cx);
            return;
        }
        let Some(backend) = self.data.backend() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let (_, failed) = services::apply_folder_moves(&backend, &moves).await;
            let _ = this.update(cx, |this, cx| {
                this.load_catalog(cx);
                if failed == 0 {
                    this.commit_order(order, cx);
                }
            });
        })
        .detach();
    }

    /// A folder dragged elsewhere (`dropFolder`): its key changes with its path, so its
    /// colour, collapse state and sibling order move first; then its hosts and shared
    /// credentials follow.
    pub fn drop_folder(
        &mut self,
        drop: crate::ui::sidebar_dnd::FolderDrop,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let moves = if drop.from != drop.to {
            muxus_app::sidebar::folder_mutations::folder_rewrite_plan(
                &self.all_managed_hosts(),
                &drop.from,
                &drop.to,
            )
        } else {
            Vec::new()
        };
        let (from, to) = (drop.from.clone(), drop.to.clone());
        self.sidebar.update(cx, |sidebar, cx| {
            if from != to {
                sidebar.folders.rename_folder_prefs(&from, &to);
                if moves.is_empty() {
                    sidebar.folders.add_empty_folder(&to);
                }
                sidebar.folders.remove_empty_folder(&from);
            }
            if let Some((parent, keys)) = &drop.placement {
                sidebar.folders.set_folder_order(parent, keys);
            }
            sidebar.rebuild();
            cx.notify();
        });
        if from == to {
            return;
        }
        cx.spawn(async move |this, cx| {
            if !moves.is_empty() {
                services::apply_folder_moves(&backend, &moves).await;
            }
            // Idempotent: credentials must never be stranded on the old path.
            let _ = backend.move_folder_settings(&from, &to).await;
            let _ = this.update(cx, |this, cx| this.load_catalog(cx));
        })
        .detach();
    }

    /// A blank tab's "Add host" card or the sidebar's add button.
    pub fn add_host(
        &mut self,
        prefill: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_host_editor(
            HostEditorTarget::New(NewHost {
                prefill_target: prefill.filter(|text| !text.trim().is_empty()),
                ..NewHost::default()
            }),
            window,
            cx,
        );
    }
}
