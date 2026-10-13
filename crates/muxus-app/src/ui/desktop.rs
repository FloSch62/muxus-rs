//! Launches handed over by later processes and the command line this one started with
//! (`deliverCommandLineLaunch` in `electron/src/main.ts`, `CommandLineLaunchHandler.tsx`).

use std::time::Instant;

use muxus_app::features::desktop::events::{DesktopEvent, DesktopEvents};
use muxus_app::features::desktop::launch::{LaunchHost, PlannedAction, PlannedLaunch, plan_launch};
use muxus_app::hosts::ManagedHost;
use muxus_app::state::session_actions::{connect_managed_host, connect_target};
use muxus_app::state::tabs::TabPatch;
use muxus_core::api::{CommandLineLaunch, ManagedHostRef};
use muxus_ui::gpui_kit::{App, AsyncApp, Context};
use muxus_ui::theme::Status;

use crate::ui::features::{active_shell, focus_shell_window};
use crate::ui::shell::AppShell;

/// Handle the desktop events of the whole run.
pub fn listen(events: DesktopEvents, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Some(event) = events.recv().await {
            cx.update(|cx| {
                let Some(shell) = active_shell(cx) else {
                    return;
                };
                focus_shell_window(&shell, cx);
                if let DesktopEvent::Launch(launch) = event {
                    shell.update(cx, |shell, cx| shell.run_launch(launch, cx));
                }
            });
        }
    })
    .detach();
}

impl AppShell {
    /// What a further main window was opened for.
    pub fn start_window_launch(
        &mut self,
        launch: crate::ui::MainWindowLaunch,
        cx: &mut Context<Self>,
    ) {
        match launch {
            crate::ui::MainWindowLaunch::Host(host) => {
                connect_managed_host(
                    &mut self.tabs,
                    &mut self.launch_guard,
                    &host,
                    None,
                    Instant::now(),
                );
                self.tabs_changed(cx);
            }
            crate::ui::MainWindowLaunch::Session {
                profile,
                title,
                color,
                transfer,
            } => {
                let id = self.tabs.open(*profile, title);
                if let Some(transfer) = transfer {
                    self.transfers.insert(id.clone(), transfer);
                }
                if color.is_some() {
                    self.tabs.update(
                        &id,
                        TabPatch {
                            color: Some(color),
                            ..Default::default()
                        },
                    );
                }
                self.tabs_changed(cx);
            }
            // The workspace runtime restores or creates it (its initial selection).
            crate::ui::MainWindowLaunch::Workspace { name, .. } => self.workspace_name = name,
        }
    }

    /// Resolve a launch against the backend's catalogs, then perform it here.
    pub fn run_launch(&mut self, launch: CommandLineLaunch, cx: &mut Context<Self>) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let planned = plan_launch(&backend, &launch).await;
            let _ = this.update(cx, |this, cx| this.perform_launch(planned, cx));
        })
        .detach();
    }

    fn launch_host(&self, host: &LaunchHost) -> Option<ManagedHost> {
        match &host.host {
            ManagedHostRef::Ssh { alias } => self
                .catalog
                .ssh
                .hosts
                .iter()
                .find(|entry| &entry.alias == alias)
                .cloned()
                .map(ManagedHost::ssh),
            ManagedHostRef::Profile { id } => self
                .catalog
                .profiles
                .iter()
                .find(|profile| &profile.id == id)
                .cloned()
                .map(ManagedHost::profile),
        }
    }

    pub fn perform_launch(&mut self, planned: PlannedLaunch, cx: &mut Context<Self>) {
        let now = Instant::now();
        let mut failed = false;
        match planned.action {
            PlannedAction::ConnectTarget { target } => {
                connect_target(
                    &mut self.tabs,
                    &mut self.launch_guard,
                    &target,
                    &target,
                    None,
                    now,
                );
            }
            PlannedAction::OpenProfile {
                profile,
                title,
                color,
            } => {
                let id = self.tabs.open(*profile, title);
                if color.is_some() {
                    self.tabs.update(
                        &id,
                        TabPatch {
                            color: Some(color),
                            ..Default::default()
                        },
                    );
                }
            }
            PlannedAction::ConnectHost { host } => match self.launch_host(&host) {
                Some(host) => {
                    connect_managed_host(&mut self.tabs, &mut self.launch_guard, &host, None, now);
                }
                None => failed = true,
            },
            PlannedAction::LaunchFolder { hosts, .. } => {
                let hosts: Vec<ManagedHost> = hosts
                    .iter()
                    .filter_map(|host| self.launch_host(host))
                    .collect();
                for host in hosts {
                    connect_managed_host(&mut self.tabs, &mut self.launch_guard, &host, None, now);
                }
            }
            PlannedAction::OpenWorkspace { workspace } => {
                self.open_workspace_by_id(&workspace.id, &workspace.name, cx);
            }
            PlannedAction::Fail => failed = true,
        }
        if let Some(toast) = planned.toast {
            let status = if failed { Status::Error } else { Status::Info };
            self.show_toast(status, toast, cx);
        }
        self.tabs_changed(cx);
    }
}
