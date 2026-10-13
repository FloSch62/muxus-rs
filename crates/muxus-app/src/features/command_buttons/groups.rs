//! Which command button group the bar and the keyboard menu show (port of
//! `client/src/command-button-groups.ts`, issue #210).
//!
//! A group picked while a session was active wins for that session, then the
//! group its host opens, then the group last picked for sessions without one.
//! The per-session choice is kept per window on purpose ([`SessionGroupChoices`]):
//! it belongs to the tab, and another window's tabs have their own.

use std::collections::HashMap;

use muxus_core::api::{SavedHostKind, SavedHostProfile, SshHostEntry};
use muxus_store::prefs::PrefsState;
use muxus_store::prefs::command_buttons::{
    CommandButton, CommandButtonGroup, command_button_group_of, shown_command_button_group,
};

use crate::hosts::managed::{
    ManagedHost, editable_managed_host_for_profile, managed_host_display_name,
};
use crate::state::tabs::TerminalTab;

/// Hosts with a terminal (`isTerminalHost`): SSH, Telnet and serial. Only
/// those can open a command group.
pub fn is_terminal_host(host: &ManagedHost) -> bool {
    match host {
        ManagedHost::Ssh(_) => true,
        ManagedHost::Profile(entry) => matches!(
            entry.kind,
            SavedHostKind::Ssh | SavedHostKind::Telnet | SavedHostKind::Serial
        ),
    }
}

/// The group picked per tab, for sessions whose host opens a group of its
/// own (`commandButtonGroupByTab` in the window's UI store).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionGroupChoices {
    by_tab: HashMap<String, String>,
}

impl SessionGroupChoices {
    pub fn get(&self, tab_id: &str) -> Option<&str> {
        self.by_tab.get(tab_id).map(String::as_str)
    }

    pub fn set(&mut self, tab_id: &str, group_id: &str) {
        self.by_tab.insert(tab_id.to_owned(), group_id.to_owned());
    }

    /// Forget tabs that closed.
    pub fn retain_tabs(&mut self, alive: impl Fn(&str) -> bool) {
        self.by_tab.retain(|tab_id, _| alive(tab_id));
    }
}

/// The group shown for the active session, and what changing it does
/// (`ActiveCommandButtonGroup`).
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveCommandButtonGroup {
    pub groups: Vec<CommandButtonGroup>,
    /// The group the bar and the keyboard menu show right now.
    pub shown_id: String,
    /// The saved terminal host behind the active session, which can be given
    /// a group of its own. Only looked up once there is more than one group.
    pub host: Option<ManagedHost>,
    /// That host's display name.
    pub host_name: Option<String>,
    /// The group that host opens, when it names one that exists.
    pub host_group_id: Option<String>,
    /// The active tab, for a per-session choice.
    pub tab_id: Option<String>,
}

/// What [`ActiveCommandButtonGroup::select`] changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupSelection {
    /// The choice holds for this session only.
    Session { tab_id: String, group_id: String },
    /// The group for every session without one (`selectedCommandButtonGroup`).
    Preference(String),
}

impl ActiveCommandButtonGroup {
    /// Show another group. With a host that opens its own group, the choice
    /// holds for this session only; otherwise it becomes the group for every
    /// session without one.
    pub fn select(&self, group_id: &str) -> GroupSelection {
        match (&self.host_group_id, &self.tab_id) {
            (Some(_), Some(tab_id)) => GroupSelection::Session {
                tab_id: tab_id.clone(),
                group_id: group_id.to_owned(),
            },
            _ => GroupSelection::Preference(group_id.to_owned()),
        }
    }

    /// The shown group, when it exists.
    pub fn shown_group(&self) -> Option<&CommandButtonGroup> {
        self.groups.iter().find(|group| group.id == self.shown_id)
    }

    /// Whether more than one group exists (group names and the picker only
    /// mean something then).
    pub fn grouped(&self) -> bool {
        self.groups.len() > 1
    }

    /// The buttons of the shown group, in order.
    pub fn shown_buttons<'a>(&self, buttons: &'a [CommandButton]) -> Vec<&'a CommandButton> {
        buttons
            .iter()
            .filter(|button| command_button_group_of(button, &self.groups) == self.shown_id)
            .collect()
    }

    /// How many buttons a group holds.
    pub fn count_in(&self, buttons: &[CommandButton], group_id: &str) -> usize {
        buttons
            .iter()
            .filter(|button| command_button_group_of(button, &self.groups) == group_id)
            .count()
    }

    /// Whether the shown group is the one the active host opens.
    pub fn shown_for_host(&self) -> bool {
        self.host.is_some() && self.host_group_id.as_deref() == Some(self.shown_id.as_str())
    }
}

/// `useActiveCommandButtonGroup` for one window state.
pub fn active_command_button_group(
    prefs: &PrefsState,
    active_tab: Option<&TerminalTab>,
    choices: &SessionGroupChoices,
    ssh_hosts: &[SshHostEntry],
    saved_hosts: &[SavedHostProfile],
) -> ActiveCommandButtonGroup {
    let groups = prefs.command_button_groups.clone();
    // With only the default group no host can open another, so skip the lookups.
    let grouped = groups.len() > 1;
    let host = if grouped {
        active_tab
            .and_then(|tab| tab.profile.as_ref())
            .and_then(|profile| editable_managed_host_for_profile(profile, ssh_hosts, saved_hosts))
            .filter(is_terminal_host)
    } else {
        None
    };
    let requested = host
        .as_ref()
        .and_then(|host| host.metadata())
        .and_then(|metadata| metadata.command_button_group.clone());
    let host_group_id =
        requested.filter(|requested| groups.iter().any(|group| &group.id == requested));
    let tab_id = active_tab.map(|tab| tab.id.clone());
    let session_choice = tab_id
        .as_deref()
        .and_then(|tab_id| choices.get(tab_id))
        .map(str::to_owned);
    let shown_id = shown_command_button_group(
        &groups,
        if host_group_id.is_some() {
            session_choice.as_deref()
        } else {
            None
        },
        host_group_id.as_deref(),
        Some(prefs.selected_command_button_group.as_str()),
    );
    ActiveCommandButtonGroup {
        host_name: host
            .as_ref()
            .map(|host| managed_host_display_name(host).to_owned()),
        groups,
        shown_id,
        host,
        host_group_id,
        tab_id,
    }
}

#[cfg(test)]
mod tests {
    use muxus_core::protocol::{SessionProfile, SshProfile};
    use muxus_store::prefs::command_buttons::default_command_group;

    use super::*;
    use crate::state::tabs::{IdGen, TabsState};

    fn host(alias: &str, group: Option<&str>) -> SshHostEntry {
        SshHostEntry {
            alias: alias.into(),
            aliases: vec![alias.into()],
            file: String::new(),
            resolved: muxus_core::api::ResolvedHostSettings {
                hostname: alias.into(),
                port: 22,
                ..Default::default()
            },
            metadata: Some(crate::model::HostMetadata {
                profile_id: format!("ssh:{alias}"),
                command_button_group: group.map(Into::into),
                ..Default::default()
            }),
            description: None,
            options: Default::default(),
        }
    }

    fn prefs(selected: &str) -> PrefsState {
        PrefsState {
            command_button_groups: vec![
                default_command_group(),
                CommandButtonGroup {
                    id: "juniper".into(),
                    name: "Juniper".into(),
                },
                CommandButtonGroup {
                    id: "huawei".into(),
                    name: "Huawei".into(),
                },
            ],
            selected_command_button_group: selected.into(),
            ..PrefsState::default()
        }
    }

    fn tab_for(target: &str) -> TerminalTab {
        let mut tabs = TabsState::with_pane("pane", IdGen::sequential());
        let id = tabs.open(SessionProfile::Ssh(SshProfile::new(target)), target);
        tabs.tab(&id).unwrap().clone()
    }

    #[test]
    fn a_host_group_wins_over_the_selected_one_and_a_session_choice_over_both() {
        let hosts = [host("edge", Some("juniper"))];
        let tab = tab_for("edge");
        let mut choices = SessionGroupChoices::default();
        let active =
            active_command_button_group(&prefs("huawei"), Some(&tab), &choices, &hosts, &[]);
        assert_eq!(active.shown_id, "juniper");
        assert_eq!(active.host_name.as_deref(), Some("edge"));
        assert!(active.shown_for_host());
        assert_eq!(
            active.select("huawei"),
            GroupSelection::Session {
                tab_id: tab.id.clone(),
                group_id: "huawei".into()
            }
        );
        choices.set(&tab.id, "huawei");
        let active =
            active_command_button_group(&prefs("default"), Some(&tab), &choices, &hosts, &[]);
        assert_eq!(active.shown_id, "huawei");
        assert!(!active.shown_for_host());
    }

    #[test]
    fn sessions_without_a_host_group_follow_the_preference() {
        let hosts = [host("edge", None)];
        let tab = tab_for("edge");
        let mut choices = SessionGroupChoices::default();
        choices.set(&tab.id, "juniper");
        let active =
            active_command_button_group(&prefs("huawei"), Some(&tab), &choices, &hosts, &[]);
        assert_eq!(active.shown_id, "huawei");
        assert_eq!(
            active.select("juniper"),
            GroupSelection::Preference("juniper".into())
        );
    }

    #[test]
    fn a_single_group_skips_the_host_lookup() {
        let hosts = [host("edge", Some("juniper"))];
        let tab = tab_for("edge");
        let prefs = PrefsState::default();
        let active = active_command_button_group(
            &prefs,
            Some(&tab),
            &SessionGroupChoices::default(),
            &hosts,
            &[],
        );
        assert_eq!(active.shown_id, "default");
        assert!(active.host.is_none());
        assert!(!active.grouped());
    }
}
