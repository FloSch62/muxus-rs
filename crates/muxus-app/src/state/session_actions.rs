//! Opening sessions from the chrome: the tab-side half of
//! `client/src/session-actions.ts` (`connectManagedHost`, `connectTarget`,
//! `openLocalTerminal`, `isQuickConnectTarget` and the repeat-launch guard).

use std::time::{Duration, Instant};

use crate::hosts::ManagedHost;
use crate::model::SessionProfile;
use crate::state::tabs::{TabPatch, TabsState};

/// How long a launch swallows an identical repeat. A tab appears instantly but
/// spends its first second connecting, so a double-click on a host reads as "the
/// first click missed" long before there is anything on screen to say otherwise.
pub const REPEAT_LAUNCH: Duration = Duration::from_millis(500);

/// The last launch, for collapsing a burst of identical launches into one session.
#[derive(Clone, Debug, Default)]
pub struct LaunchGuard {
    last: Option<(String, String, Instant)>,
}

impl LaunchGuard {
    /// Collapse a burst of identical launches into one session, handing every
    /// caller in the burst the same tab. The window slides with each suppressed
    /// click, so mashing a row still yields one tab; a deliberate second session
    /// is one short pause away.
    pub fn launch_once(
        &mut self,
        tabs: &mut TabsState,
        key: String,
        now: Instant,
        launch: impl FnOnce(&mut TabsState) -> String,
    ) -> String {
        // A guard that outlives its tab would swallow the very click meant to
        // bring the session back, so a closed tab ends the window early.
        if let Some((last_key, id, at)) = &mut self.last
            && *last_key == key
            && tabs.tab(id).is_some()
            && now
                .checked_duration_since(*at)
                .is_some_and(|elapsed| elapsed < REPEAT_LAUNCH)
        {
            *at = now;
            return id.clone();
        }
        let id = launch(tabs);
        self.last = Some((key, id.clone(), now));
        id
    }
}

/// Turn the active (or requested) blank tab into this session, when there is one.
fn replace_active_empty(
    tabs: &mut TabsState,
    profile: &SessionProfile,
    title: &str,
    requested: Option<&str>,
) -> Option<String> {
    let id = match requested {
        Some(id) => Some(id.to_string()),
        None => tabs
            .active_tab()
            .filter(|tab| tab.profile.is_none())
            .map(|tab| tab.id.clone()),
    }?;
    tabs.replace_empty(&id, profile.clone(), title)
        .then_some(id)
}

fn open_or_replace(
    tabs: &mut TabsState,
    profile: SessionProfile,
    title: &str,
    replace: Option<&str>,
) -> String {
    replace_active_empty(tabs, &profile, title, replace)
        .unwrap_or_else(|| tabs.open(profile, title))
}

/// `connectTarget`: an ssh_config alias or an ad-hoc `[user@]host[:port]`.
pub fn connect_target(
    tabs: &mut TabsState,
    guard: &mut LaunchGuard,
    target: &str,
    title: &str,
    replace: Option<&str>,
    now: Instant,
) -> String {
    let profile = crate::model::ssh_profile(target);
    let key = format!("ssh:{target}:{}", replace.unwrap_or(""));
    guard.launch_once(tabs, key, now, |tabs| {
        open_or_replace(tabs, profile, title, replace)
    })
}

/// `connectManagedHost`: connect a listed host with its display name and color
/// carried into the tab, preserving the visual cue after the sidebar is hidden.
pub fn connect_managed_host(
    tabs: &mut TabsState,
    guard: &mut LaunchGuard,
    host: &ManagedHost,
    replace: Option<&str>,
    now: Instant,
) -> String {
    let color = host.metadata().and_then(|metadata| metadata.color.clone());
    let id = match host {
        ManagedHost::Ssh(entry) => {
            let title = entry
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.display_name.clone())
                .unwrap_or_else(|| entry.alias.clone());
            connect_target(tabs, guard, &entry.alias, &title, replace, now)
        }
        ManagedHost::Profile(entry) => {
            let profile = with_profile_id(entry.profile.clone().into(), &entry.id);
            let title = host.display_name().to_string();
            let key = format!("saved:{}:{}", entry.id, replace.unwrap_or(""));
            guard.launch_once(tabs, key, now, |tabs| {
                open_or_replace(tabs, profile, &title, replace)
            })
        }
    };
    if let Some(color) = color {
        tabs.update(
            &id,
            TabPatch {
                color: Some(Some(color)),
                ..Default::default()
            },
        );
    }
    id
}

/// One host of `launchManagedHostGroup`: its profile, title and color for
/// [`TabsState::launch_set`].
pub fn session_set_entry(host: &ManagedHost) -> crate::state::tabs::SessionSetEntry {
    let color = host.metadata().and_then(|metadata| metadata.color.clone());
    match host {
        ManagedHost::Ssh(entry) => crate::state::tabs::SessionSetEntry {
            profile: crate::model::ssh_profile(entry.alias.clone()),
            title: entry
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.display_name.clone())
                .unwrap_or_else(|| entry.alias.clone()),
            color,
        },
        ManagedHost::Profile(entry) => crate::state::tabs::SessionSetEntry {
            profile: with_profile_id(entry.profile.clone().into(), &entry.id),
            title: host.display_name().to_string(),
            color,
        },
    }
}

/// `launchLocalTerminal`: open a local shell (`openLocalTerminal` resolves
/// the profile and title from the preferences).
pub fn open_local_terminal(
    tabs: &mut TabsState,
    guard: &mut LaunchGuard,
    profile: SessionProfile,
    title: &str,
    replace: Option<&str>,
    now: Instant,
) -> String {
    let key = format!("local:{profile:?}:{}", replace.unwrap_or(""));
    guard.launch_once(tabs, key, now, |tabs| {
        open_or_replace(tabs, profile, title, replace)
    })
}

/// A saved host's profile carries its own id, so sessions can find it again.
fn with_profile_id(mut profile: SessionProfile, id: &str) -> SessionProfile {
    match &mut profile {
        SessionProfile::Local(_) => {}
        SessionProfile::Ssh(p) => p.profile_id = Some(id.to_string()),
        SessionProfile::Telnet(p) => p.profile_id = Some(id.to_string()),
        SessionProfile::Serial(p) => p.profile_id = Some(id.to_string()),
        SessionProfile::Rdp(p) => p.profile_id = Some(id.to_string()),
        SessionProfile::Vnc(p) => p.profile_id = Some(id.to_string()),
        SessionProfile::Gnmi(p) => p.profile_id = Some(id.to_string()),
        SessionProfile::Netconf(p) => p.profile_id = Some(id.to_string()),
    }
    profile
}

/// `isQuickConnectTarget`: whether the search box text reads as `[user@]host[:port]`.
pub fn is_quick_connect_target(input: &str) -> bool {
    let s = input.trim();
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return false;
    }
    let at = s.rfind('@');
    if at == Some(0) {
        return false;
    }
    let rest = match at {
        Some(index) => &s[index + 1..],
        None => s,
    };
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (rest, None),
    };
    if host.is_empty() {
        return false;
    }
    match port {
        None => true,
        Some(port) => {
            !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit())
                && port
                    .parse::<u32>()
                    .is_ok_and(|port| (1..=65535).contains(&port))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tabs::IdGen;

    #[test]
    fn quick_connect_targets() {
        for ok in [
            "host",
            "user@host",
            "ops@staging-07:2222",
            "a@b@c",
            "10.0.0.1:22",
        ] {
            assert!(is_quick_connect_target(ok), "{ok}");
        }
        for bad in [
            "",
            "  ",
            "two words",
            "@host",
            "host:",
            "host:0",
            "host:70000",
            "host:2x",
            ":22",
            "user@",
        ] {
            assert!(!is_quick_connect_target(bad), "{bad}");
        }
    }

    #[test]
    fn a_repeat_inside_the_window_returns_the_same_tab() {
        let mut tabs = TabsState::with_pane("pane-1", IdGen::sequential());
        let mut guard = LaunchGuard::default();
        let start = Instant::now();
        let first = connect_target(&mut tabs, &mut guard, "web-01", "web-01", None, start);
        let again = connect_target(
            &mut tabs,
            &mut guard,
            "web-01",
            "web-01",
            None,
            start + Duration::from_millis(300),
        );
        assert_eq!(first, again);
        assert_eq!(tabs.tabs.len(), 1);
        let later = connect_target(
            &mut tabs,
            &mut guard,
            "web-01",
            "web-01",
            None,
            start + Duration::from_millis(900),
        );
        assert_ne!(first, later);
        assert_eq!(tabs.tabs.len(), 2);
    }

    #[test]
    fn a_closed_tab_ends_the_window_early() {
        let mut tabs = TabsState::with_pane("pane-1", IdGen::sequential());
        let mut guard = LaunchGuard::default();
        let start = Instant::now();
        let first = connect_target(&mut tabs, &mut guard, "web-01", "web-01", None, start);
        tabs.close(&first);
        let again = connect_target(
            &mut tabs,
            &mut guard,
            "web-01",
            "web-01",
            None,
            start + Duration::from_millis(100),
        );
        assert_ne!(first, again);
    }

    #[test]
    fn launching_replaces_the_active_blank_tab() {
        let mut tabs = TabsState::with_pane("pane-1", IdGen::sequential());
        let mut guard = LaunchGuard::default();
        let blank = tabs.open_empty();
        let id = open_local_terminal(
            &mut tabs,
            &mut guard,
            crate::model::local_profile(),
            "Local",
            None,
            Instant::now(),
        );
        assert_eq!(id, blank);
        assert_eq!(tabs.tabs.len(), 1);
        assert_eq!(tabs.tabs[0].title, "Local");
    }

    #[test]
    fn managed_hosts_carry_their_color_into_the_tab() {
        let catalog = crate::model::demo::demo_catalog();
        let entry = catalog
            .ssh
            .hosts
            .iter()
            .find(|host| host.alias == "web-01")
            .unwrap()
            .clone();
        let host = ManagedHost::ssh(entry);
        let mut tabs = TabsState::with_pane("pane-1", IdGen::sequential());
        let id = connect_managed_host(
            &mut tabs,
            &mut LaunchGuard::default(),
            &host,
            None,
            Instant::now(),
        );
        let tab = tabs.tab(&id).unwrap();
        assert_eq!(tab.title, "web-01");
        assert_eq!(tab.color.as_deref(), Some("#3b82f6"));
    }

    #[test]
    fn saved_hosts_open_with_their_profile_id() {
        let catalog = crate::model::demo::demo_catalog();
        let entry = catalog
            .profiles
            .iter()
            .find(|p| p.name == "lab-console")
            .unwrap()
            .clone();
        let id_expected = entry.id.clone();
        let host = ManagedHost::profile(entry);
        let mut tabs = TabsState::with_pane("pane-1", IdGen::sequential());
        let id = connect_managed_host(
            &mut tabs,
            &mut LaunchGuard::default(),
            &host,
            None,
            Instant::now(),
        );
        let tab = tabs.tab(&id).unwrap();
        assert_eq!(
            tab.profile.as_ref().and_then(|p| p.profile_id()),
            Some(id_expected.as_str())
        );
    }
}
