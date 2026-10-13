//! Window state behind the command bar, the keyboard menu, the editor and
//! the multi-exec control.
//!
//! [`CommandButtons`] is what `useActiveCommandButtonGroup` reads in the
//! client: the active tab, the per-tab group choices (kept in this window on
//! purpose, issue #210), the saved hosts (a host may open its own group) and
//! the vault's secret names. [`MultiExec`] holds the window's mirrored-input
//! selection; terminal panes route their input through it.
//!
//! The window creates one of each and tells them when its tabs change.

use muxus_core::api::{OpenSshMetadataPatch, PasswordVaultStatus, SavedHostProfile, SshHostEntry};
use muxus_core::js::Patch;
use muxus_session::Backend;
use muxus_store::prefs::PrefsState;
use muxus_ui::gpui_kit::{App, Context};
use muxus_ui::theme::Status;

use super::groups::{
    ActiveCommandButtonGroup, GroupSelection, SessionGroupChoices, active_command_button_group,
};
use super::multi_exec::{MirrorTargets, MultiExecState, multi_exec_targets, toggle_multi_exec};
use crate::features::launcher::kit::show_toast;
use crate::features::settings::SettingsPrefs;
use crate::hosts::managed::ManagedHost;
use crate::state::tabs::{TabsState, TerminalTab};

/// The window's preferences (the shared [`SettingsPrefs`], else defaults).
pub fn prefs(cx: &App) -> PrefsState {
    SettingsPrefs::global(cx)
        .map(|prefs| prefs.read(cx).prefs().clone())
        .unwrap_or_default()
}

/// Change the preferences through the shared [`SettingsPrefs`].
pub fn update_prefs(cx: &mut App, change: impl FnOnce(&mut PrefsState)) {
    if let Some(prefs) = SettingsPrefs::global(cx) {
        prefs.update(cx, |prefs, cx| prefs.update(cx, change));
    }
}

/// See the module docs.
pub struct CommandButtons {
    backend: Backend,
    tabs: Vec<TerminalTab>,
    active_id: Option<String>,
    choices: SessionGroupChoices,
    ssh_hosts: Vec<SshHostEntry>,
    saved_hosts: Vec<SavedHostProfile>,
    vault: Option<PasswordVaultStatus>,
}

impl CommandButtons {
    /// Read the hosts (and the vault, when a saved command types a secret).
    pub fn new(backend: Backend, cx: &mut Context<Self>) -> Self {
        let model = Self {
            backend,
            tabs: Vec::new(),
            active_id: None,
            choices: SessionGroupChoices::default(),
            ssh_hosts: Vec::new(),
            saved_hosts: Vec::new(),
            vault: None,
        };
        model.refresh_hosts(cx);
        model.refresh_vault(cx);
        model
    }

    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    /// The window's tabs changed.
    pub fn set_tabs(&mut self, tabs: &TabsState, cx: &mut Context<Self>) {
        self.tabs = tabs.tabs.clone();
        self.active_id = tabs.active_id.clone();
        let alive: Vec<String> = self.tabs.iter().map(|tab| tab.id.clone()).collect();
        self.choices
            .retain_tabs(|id| alive.iter().any(|tab| tab == id));
        cx.notify();
    }

    pub fn active_tab(&self) -> Option<&TerminalTab> {
        let id = self.active_id.as_deref()?;
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn active_id(&self) -> Option<&str> {
        self.active_id.as_deref()
    }

    /// The active tab's terminal is connected.
    pub fn connected(&self) -> bool {
        self.active_tab()
            .is_some_and(|tab| tab.status == crate::state::tabs::TabStatus::Connected)
    }

    pub fn vault(&self) -> Option<&PasswordVaultStatus> {
        self.vault.as_ref()
    }

    /// The saved hosts changed (an editor saved one, metadata moved).
    pub fn refresh_hosts(&self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let ssh = backend.ssh_config().await;
            let saved = backend.saved_hosts().await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(ssh) = ssh {
                    this.ssh_hosts = ssh.hosts;
                }
                if let Ok(saved) = saved {
                    this.saved_hosts = saved.profiles;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Read the vault's secret names again (only needed while a saved
    /// command types a secret).
    pub fn refresh_vault(&self, cx: &mut Context<Self>) {
        if !prefs(cx)
            .command_buttons
            .iter()
            .any(|button| button.secret_id.is_some())
        {
            return;
        }
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let vault = backend.vault_status().await.ok();
            let _ = this.update(cx, |this, cx| {
                this.vault = vault;
                cx.notify();
            });
        })
        .detach();
    }

    /// `useActiveCommandButtonGroup`.
    pub fn active_group(&self, cx: &App) -> ActiveCommandButtonGroup {
        active_command_button_group(
            &prefs(cx),
            self.active_tab(),
            &self.choices,
            &self.ssh_hosts,
            &self.saved_hosts,
        )
    }

    /// Show another group: for this session when its host opens one of its
    /// own, else for every session without one.
    pub fn select_group(&mut self, group_id: &str, cx: &mut Context<Self>) {
        match self.active_group(cx).select(group_id) {
            GroupSelection::Session { tab_id, group_id } => {
                self.choices.set(&tab_id, &group_id);
                cx.notify();
            }
            GroupSelection::Preference(group_id) => {
                update_prefs(cx, |prefs| prefs.selected_command_button_group = group_id);
                cx.notify();
            }
        }
    }

    /// "Always show <group> for <host>": the host setting of Terminal
    /// appearance → Command button group, toggled from the bar.
    pub fn toggle_host_group(&mut self, cx: &mut Context<Self>) {
        let active = self.active_group(cx);
        let Some(host) = active.host.clone() else {
            return;
        };
        let shown_for_host = active.shown_for_host();
        let patch = OpenSshMetadataPatch {
            command_button_group: if shown_for_host {
                Patch::Clear
            } else {
                Patch::Set(active.shown_id.clone())
            },
            ..OpenSshMetadataPatch::default()
        };
        let host_name = active.host_name.clone().unwrap_or_default();
        let group_name = active
            .shown_group()
            .map(|group| group.name.clone())
            .unwrap_or_else(|| "this group".into());
        show_toast(
            Status::Success,
            if shown_for_host {
                format!("Sessions to {host_name} no longer switch the command group.")
            } else {
                format!("Sessions to {host_name} now show {group_name}.")
            },
            cx,
        );
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = match &host {
                ManagedHost::Ssh(entry) => backend
                    .patch_ssh_host_metadata(&entry.alias, patch)
                    .await
                    .map(drop),
                ManagedHost::Profile(entry) => backend
                    .patch_saved_host_metadata(&entry.id, patch)
                    .await
                    .map(drop),
            };
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.refresh_hosts(cx),
                Err(error) => show_toast(Status::Error, error.message, cx),
            });
        })
        .detach();
    }

    /// How many saved hosts open each group (the editor's captions).
    pub fn hosts_opening(&self, group_id: &str) -> usize {
        self.ssh_hosts
            .iter()
            .filter_map(|host| host.metadata.as_ref())
            .chain(self.saved_hosts.iter().map(|host| &host.metadata))
            .filter(|metadata| metadata.command_button_group.as_deref() == Some(group_id))
            .count()
    }
}

/// The window's mirrored input (`useMultiExecStore`), and what its terminals
/// need to route input through it.
#[derive(Default)]
pub struct MultiExec {
    state: MultiExecState,
    /// Connected terminal tabs, in window order.
    connected: Vec<String>,
}

impl MultiExec {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> &MultiExecState {
        &self.state
    }

    /// The window's tabs changed: drop targets that disconnected.
    pub fn set_tabs(&mut self, tabs: &TabsState, cx: &mut Context<Self>) {
        let connected = multi_exec_targets(tabs);
        if connected != self.connected {
            self.connected = connected;
            self.state.reconcile(&self.connected);
            cx.notify();
        }
    }

    /// Change the store (the control's buttons and checkboxes).
    pub fn update(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut MultiExecState)) {
        let before = self.state.clone();
        change(&mut self.state);
        if self.state != before {
            cx.notify();
        }
    }

    /// The keyboard toggle (`terminal.multi-exec`); shows why nothing
    /// happened when there was nothing to mirror.
    pub fn toggle(&mut self, tabs: &TabsState, cx: &mut Context<Self>) {
        if let Some(message) = toggle_multi_exec(&mut self.state, tabs) {
            show_toast(Status::Info, message, cx);
        }
        cx.notify();
    }

    /// Mirror input typed into `source_tab_id` (see [`MultiExecState::broadcast_input`]).
    pub fn broadcast_input(
        &self,
        source_tab_id: &str,
        data: &[u8],
        targets: &dyn MirrorTargets,
    ) -> usize {
        self.state.broadcast_input(source_tab_id, data, targets)
    }

    /// Mirror a paste (see [`MultiExecState::broadcast_paste`]).
    pub fn broadcast_paste(
        &self,
        source_tab_id: &str,
        text: &str,
        targets: &dyn MirrorTargets,
    ) -> usize {
        self.state.broadcast_paste(source_tab_id, text, targets)
    }

    /// Panes whose visible terminal mirrors input (their border turns warning-colored).
    pub fn pane_ids(&self, tabs: &TabsState) -> std::collections::HashSet<String> {
        super::multi_exec::multi_exec_pane_ids(&tabs.root, &self.state.selected_ids)
    }
}
