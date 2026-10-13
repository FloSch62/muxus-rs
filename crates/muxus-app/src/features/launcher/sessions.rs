//! Sessions the launcher and the host pickers open or jump to:
//! `client/src/host-sessions.ts` (which tabs belong to a sidebar host) and the
//! local shell half of `client/src/session-actions.ts`
//! (`openLocalShellProfile`, `openLocalTerminal`). The repeat-launch guard
//! itself is the app's [`crate::state::session_actions::LaunchGuard`]; its
//! TypeScript tests (`session-launch-guard.test.ts`) are ported below.

use std::time::Instant;

use muxus_core::js::js_trim;
use muxus_core::protocol::{LocalProfile, SessionProfile};
use muxus_local::profile::local_shell_launch_arguments;
use muxus_store::prefs::PrefsState;
use muxus_store::prefs::state::LocalShellProfileConfig;

use crate::state::session_actions::{LaunchGuard, open_local_terminal};
use crate::state::tabs::{TabsState, TerminalTab, tabs_in_order};
use crate::state::workspace_layout::PaneNode;

/// The sidebar host a tab was opened from, keyed like `managed_host_key`.
/// Local shells and ad-hoc remote sessions without a saved profile belong to
/// no host.
pub fn tab_host_key(tab: &TerminalTab) -> Option<String> {
    let profile = tab.profile.as_ref()?;
    if let SessionProfile::Local(_) = profile {
        return None;
    }
    if let Some(id) = profile.profile_id() {
        return Some(format!("profile:{id}"));
    }
    match profile {
        SessionProfile::Ssh(ssh) => Some(format!("ssh:{}", ssh.target)),
        _ => None,
    }
}

/// Every tab in this window opened from one sidebar host, in the window-wide
/// order the tab numbers follow: live, connecting and ended alike, since an
/// ended tab is still somewhere to jump back to.
pub fn host_session_tabs<'a>(
    root: &PaneNode,
    tabs: &'a [TerminalTab],
    host_key: &str,
) -> Vec<&'a TerminalTab> {
    tabs_in_order(root, tabs)
        .into_iter()
        .filter(|tab| tab.profile.is_some() && tab_host_key(tab).as_deref() == Some(host_key))
        .collect()
}

fn trimmed(value: &str) -> Option<String> {
    let value = js_trim(value);
    (!value.is_empty()).then(|| value.to_owned())
}

/// The session profile and tab title a saved local shell opens with.
pub fn local_shell_launch(saved: &LocalShellProfileConfig) -> (SessionProfile, String) {
    let profile = LocalProfile {
        shell: trimmed(&saved.shell),
        args: local_shell_launch_arguments(&saved.args),
        cwd: trimmed(&saved.cwd),
        startup_command: trimmed(&saved.startup_command),
    };
    let title = trimmed(&saved.name).unwrap_or_else(|| "Local".to_owned());
    (SessionProfile::Local(profile), title)
}

/// The user's default local terminal: the saved profile named as default,
/// else the automatic (or custom) shell. A named profile can replace the
/// automatic shell without changing callers such as Ctrl+Shift+T or splits.
pub fn default_local_launch(prefs: &PrefsState) -> (SessionProfile, String) {
    if let Some(saved) = prefs
        .local_shell_profiles
        .iter()
        .find(|profile| profile.id == prefs.default_local_shell_profile_id)
    {
        return local_shell_launch(saved);
    }
    let shell = js_trim(&prefs.local_shell);
    let shell = (prefs.local_shell != "auto" && !shell.is_empty()).then(|| shell.to_owned());
    (
        SessionProfile::Local(LocalProfile {
            shell,
            ..LocalProfile::default()
        }),
        "Local".to_owned(),
    )
}

/// `openLocalShellProfile`: open one explicitly selected saved shell.
pub fn open_local_shell_profile(
    tabs: &mut TabsState,
    guard: &mut LaunchGuard,
    saved: &LocalShellProfileConfig,
    replace: Option<&str>,
    now: Instant,
) -> String {
    let (profile, title) = local_shell_launch(saved);
    open_local_terminal(tabs, guard, profile, &title, replace, now)
}

/// `openLocalTerminal`: open the default local terminal from the preferences.
pub fn open_default_local_terminal(
    tabs: &mut TabsState,
    guard: &mut LaunchGuard,
    prefs: &PrefsState,
    replace: Option<&str>,
    now: Instant,
) -> String {
    let (profile, title) = default_local_launch(prefs);
    open_local_terminal(tabs, guard, profile, &title, replace, now)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use muxus_core::protocol::{SerialProfile, SshProfile, TelnetProfile};

    use super::*;
    use crate::hosts::managed::{ManagedHost, managed_host_key};
    use crate::model::SavedHostProfile;
    use crate::state::session_actions::{connect_managed_host, connect_target};
    use crate::state::tabs::{IdGen, TabPatch, TabStatus};
    use crate::state::workspace_layout::PaneDirection;

    fn tab(profile: Option<SessionProfile>) -> TerminalTab {
        let mut tabs = TabsState::with_pane("pane-test", IdGen::sequential());
        let id = match profile {
            Some(profile) => tabs.open(profile, "t"),
            None => tabs.open_empty(),
        };
        tabs.tab(&id).unwrap().clone()
    }

    fn ssh(target: &str, profile_id: Option<&str>) -> SessionProfile {
        let mut profile = SshProfile::new(target);
        profile.profile_id = profile_id.map(Into::into);
        SessionProfile::Ssh(profile)
    }

    // host-sessions.test.ts

    #[test]
    fn keys_tabs_the_way_the_sidebar_keys_the_hosts_they_came_from() {
        let ssh_host = crate::model::SshHostEntry {
            alias: "web".into(),
            aliases: vec!["web".into()],
            file: String::new(),
            resolved: Default::default(),
            metadata: None,
            description: None,
            options: Default::default(),
        };
        assert_eq!(
            tab_host_key(&tab(Some(ssh("web", None)))),
            Some(managed_host_key(&ManagedHost::ssh(ssh_host)))
        );
        assert_eq!(
            tab_host_key(&tab(Some(ssh("db", Some("p1"))))).as_deref(),
            Some("profile:p1")
        );
        let mut serial = SerialProfile::new("/dev/ttyUSB0");
        serial.profile_id = Some("p1".into());
        assert_eq!(
            tab_host_key(&tab(Some(SessionProfile::Serial(serial)))).as_deref(),
            Some("profile:p1")
        );
    }

    #[test]
    fn leaves_local_shells_blank_tabs_and_unsaved_non_ssh_sessions_without_a_host() {
        assert_eq!(
            tab_host_key(&tab(Some(SessionProfile::Local(LocalProfile::default())))),
            None
        );
        assert_eq!(tab_host_key(&tab(None)), None);
        assert_eq!(
            tab_host_key(&tab(Some(SessionProfile::Telnet(TelnetProfile {
                profile_id: None,
                host: "sw1".into(),
                port: 23,
            })))),
            None
        );
    }

    #[test]
    fn lists_every_tab_of_one_host_in_window_order_ended_ones_included() {
        let mut tabs = TabsState::with_pane("pane-test", IdGen::sequential());
        let first = tabs.open(ssh("web", None), "web");
        tabs.open(ssh("db", None), "db");
        tabs.open(SessionProfile::Local(LocalProfile::default()), "Local");
        assert!(tabs.split("pane-test", PaneDirection::Right).is_some());
        let second = tabs.open(ssh("web", None), "web (2)");
        tabs.update(
            &first,
            TabPatch {
                status: Some(TabStatus::Closed),
                ..Default::default()
            },
        );
        let ids: Vec<&str> = host_session_tabs(&tabs.root, &tabs.tabs, "ssh:web")
            .into_iter()
            .map(|tab| tab.id.as_str())
            .collect();
        assert_eq!(ids, [first.as_str(), second.as_str()]);
        assert!(host_session_tabs(&tabs.root, &tabs.tabs, "ssh:nowhere").is_empty());
    }

    // session-launch-guard.test.ts

    struct Clock(Instant);

    impl Clock {
        /// Move past the repeat window so the next launch is deliberate.
        fn settle(&mut self) {
            self.0 += Duration::from_millis(1000);
        }

        fn advance(&mut self, ms: u64) {
            self.0 += Duration::from_millis(ms);
        }
    }

    fn fixture() -> (TabsState, LaunchGuard, Clock) {
        (
            TabsState::with_pane("pane-test", IdGen::sequential()),
            LaunchGuard::default(),
            Clock(Instant::now()),
        )
    }

    fn saved_host(name: &str) -> SavedHostProfile {
        serde_json::from_value(serde_json::json!({
            "id": "core-router",
            "kind": "telnet",
            "name": name,
            "profile": {
                "kind": "telnet",
                "profileId": "core-router",
                "host": "router.example.test",
                "port": 2323,
            },
            "metadata": { "profileId": "core-router", "connectCount": 0 },
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    }

    fn titles(tabs: &TabsState) -> Vec<&str> {
        tabs.tabs.iter().map(|tab| tab.title.as_str()).collect()
    }

    #[test]
    fn opens_one_session_when_a_host_row_is_double_clicked() {
        let (mut tabs, mut guard, clock) = fixture();
        let first = connect_target(
            &mut tabs,
            &mut guard,
            "edge-router",
            "edge-router",
            None,
            clock.0,
        );
        let second = connect_target(
            &mut tabs,
            &mut guard,
            "edge-router",
            "edge-router",
            None,
            clock.0,
        );
        assert_eq!(second, first);
        assert_eq!(tabs.tabs.len(), 1);
    }

    #[test]
    fn keeps_swallowing_repeats_while_the_row_is_being_mashed() {
        let (mut tabs, mut guard, mut clock) = fixture();
        let first = connect_target(
            &mut tabs,
            &mut guard,
            "edge-router",
            "edge-router",
            None,
            clock.0,
        );
        for _ in 0..5 {
            clock.advance(400);
            assert_eq!(
                connect_target(
                    &mut tabs,
                    &mut guard,
                    "edge-router",
                    "edge-router",
                    None,
                    clock.0
                ),
                first
            );
        }
        assert_eq!(tabs.tabs.len(), 1);
    }

    #[test]
    fn opens_a_second_session_once_the_repeat_window_has_passed() {
        let (mut tabs, mut guard, mut clock) = fixture();
        let first = connect_target(
            &mut tabs,
            &mut guard,
            "edge-router",
            "edge-router",
            None,
            clock.0,
        );
        clock.settle();
        let second = connect_target(
            &mut tabs,
            &mut guard,
            "edge-router",
            "edge-router",
            None,
            clock.0,
        );
        assert_ne!(second, first);
        assert_eq!(tabs.tabs.len(), 2);
    }

    #[test]
    fn does_not_swallow_a_different_host_clicked_right_after() {
        let (mut tabs, mut guard, clock) = fixture();
        connect_target(
            &mut tabs,
            &mut guard,
            "edge-router",
            "edge-router",
            None,
            clock.0,
        );
        connect_target(
            &mut tabs,
            &mut guard,
            "core-switch",
            "core-switch",
            None,
            clock.0,
        );
        assert_eq!(titles(&tabs), ["edge-router", "core-switch"]);
    }

    #[test]
    fn guards_saved_hosts_by_profile_not_by_title() {
        let (mut tabs, mut guard, clock) = fixture();
        let first = connect_managed_host(
            &mut tabs,
            &mut guard,
            &ManagedHost::profile(saved_host("Core router")),
            None,
            clock.0,
        );
        let second = connect_managed_host(
            &mut tabs,
            &mut guard,
            &ManagedHost::profile(saved_host("Core router (renamed)")),
            None,
            clock.0,
        );
        assert_eq!(second, first);
        assert_eq!(tabs.tabs.len(), 1);
    }

    #[test]
    fn guards_the_local_terminal_button() {
        let (mut tabs, mut guard, clock) = fixture();
        let prefs = PrefsState::default();
        let first = open_default_local_terminal(&mut tabs, &mut guard, &prefs, None, clock.0);
        let second = open_default_local_terminal(&mut tabs, &mut guard, &prefs, None, clock.0);
        assert_eq!(second, first);
        assert_eq!(tabs.tabs.len(), 1);
    }

    #[test]
    fn opens_the_selected_saved_local_shell_with_arguments_and_startup_actions() {
        let (mut tabs, mut guard, clock) = fixture();
        let saved = LocalShellProfileConfig {
            id: "ubuntu".into(),
            name: "Ubuntu".into(),
            shell: "wsl.exe".into(),
            args: vec!["-d".into(), "Ubuntu".into()],
            cwd: "C:\\work".into(),
            startup_command: "cd project\nnpm run dev".into(),
        };
        let id = open_local_shell_profile(&mut tabs, &mut guard, &saved, None, clock.0);
        let opened = tabs.tab(&id).unwrap();
        assert_eq!(opened.title, "Ubuntu");
        assert_eq!(
            opened.profile,
            Some(SessionProfile::Local(LocalProfile {
                shell: Some("wsl.exe".into()),
                args: Some(vec!["-d".into(), "Ubuntu".into()]),
                cwd: Some("C:\\work".into()),
                startup_command: Some("cd project\nnpm run dev".into()),
            }))
        );
    }

    #[test]
    fn uses_the_configured_default_saved_local_shell() {
        let (mut tabs, mut guard, clock) = fixture();
        let prefs = PrefsState {
            local_shell_profiles: vec![LocalShellProfileConfig {
                id: "powershell".into(),
                name: "PowerShell".into(),
                shell: "pwsh.exe".into(),
                args: vec!["-NoLogo".into()],
                cwd: String::new(),
                startup_command: String::new(),
            }],
            default_local_shell_profile_id: "powershell".into(),
            ..PrefsState::default()
        };
        let id = open_default_local_terminal(&mut tabs, &mut guard, &prefs, None, clock.0);
        let opened = tabs.tab(&id).unwrap();
        assert_eq!(opened.title, "PowerShell");
        assert_eq!(
            opened.profile,
            Some(SessionProfile::Local(LocalProfile {
                shell: Some("pwsh.exe".into()),
                args: Some(vec!["-NoLogo".into()]),
                ..LocalProfile::default()
            }))
        );
    }

    #[test]
    fn reopens_a_host_whose_fresh_tab_was_closed_inside_the_window() {
        let (mut tabs, mut guard, clock) = fixture();
        let first = connect_target(
            &mut tabs,
            &mut guard,
            "edge-router",
            "edge-router",
            None,
            clock.0,
        );
        tabs.close(&first);
        let second = connect_target(
            &mut tabs,
            &mut guard,
            "edge-router",
            "edge-router",
            None,
            clock.0,
        );
        assert_ne!(second, first);
        let ids: Vec<&str> = tabs.tabs.iter().map(|tab| tab.id.as_str()).collect();
        assert_eq!(ids, [second.as_str()]);
    }

    #[test]
    fn fills_a_blank_tab_once_instead_of_leaving_a_stray_session_behind() {
        let (mut tabs, mut guard, clock) = fixture();
        let blank = tabs.open_empty();
        let first = connect_target(
            &mut tabs,
            &mut guard,
            "edge-router",
            "edge-router",
            Some(&blank),
            clock.0,
        );
        let second = connect_target(
            &mut tabs,
            &mut guard,
            "edge-router",
            "edge-router",
            Some(&blank),
            clock.0,
        );
        assert_eq!(first, blank);
        assert_eq!(second, blank);
        assert_eq!(tabs.tabs.len(), 1);
    }

    #[test]
    fn the_automatic_shell_has_no_override_and_a_custom_one_is_trimmed() {
        let prefs = PrefsState {
            local_shell: "  /bin/zsh ".into(),
            ..PrefsState::default()
        };
        let (profile, title) = default_local_launch(&prefs);
        assert_eq!(title, "Local");
        assert_eq!(
            profile,
            SessionProfile::Local(LocalProfile {
                shell: Some("/bin/zsh".into()),
                ..LocalProfile::default()
            })
        );
        let (profile, _) = default_local_launch(&PrefsState::default());
        assert_eq!(profile, SessionProfile::Local(LocalProfile::default()));
    }
}
