//! Live session dots: connected and connecting tab counts per sidebar host.
//! Port of `client/src/components/sidebar/useLiveHostCounts.ts` and
//! `tabHostKey` from `client/src/host-sessions.ts`.
//!
//! The TypeScript reduces the tabs to a string signature so the sidebar only
//! re-renders when a count moves; here the counts are an ordered map, which
//! compares by value the same way.

use std::collections::BTreeMap;

use crate::model::SessionProfile;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveCounts {
    pub connected: u32,
    pub connecting: u32,
}

/// The part of a tab's status the dots read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveStatus {
    Connecting,
    Connected,
    /// Interrupted, closed or idle: no dot.
    Other,
}

/// The sidebar host a tab was opened from, keyed like `managed_host_key`.
pub fn tab_host_key(profile: &SessionProfile) -> Option<String> {
    if matches!(profile, SessionProfile::Local(_)) {
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

/// Hosts with live tabs, by host key. Tabs without a profile (empty tabs) are `None`.
pub fn live_host_counts<'a>(
    tabs: impl IntoIterator<Item = (Option<&'a SessionProfile>, LiveStatus)>,
) -> BTreeMap<String, LiveCounts> {
    let mut counts: BTreeMap<String, LiveCounts> = BTreeMap::new();
    for (profile, status) in tabs {
        let Some(profile) = profile else { continue };
        if status == LiveStatus::Other {
            continue;
        }
        let Some(key) = tab_host_key(profile) else {
            continue;
        };
        let entry = counts.entry(key).or_default();
        if status == LiveStatus::Connected {
            entry.connected += 1;
        } else {
            entry.connecting += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::LiveStatus::*;
    use super::*;

    fn with_id(mut profile: SessionProfile, id: &str) -> SessionProfile {
        match &mut profile {
            SessionProfile::Ssh(p) => p.profile_id = Some(id.into()),
            SessionProfile::Telnet(p) => p.profile_id = Some(id.into()),
            _ => unreachable!(),
        }
        profile
    }

    #[test]
    fn counts_connected_and_connecting_tabs_per_host() {
        let web = crate::model::ssh_profile("web");
        let db = with_id(crate::model::ssh_profile("db"), "p1");
        let switch = with_id(crate::model::telnet_profile("sw1", 23), "p2");
        let local = crate::model::local_profile();
        let counts = live_host_counts([
            (Some(&web), Connected),
            (Some(&web), Connecting),
            (Some(&db), Connected),
            (Some(&switch), Connected),
            (Some(&web), Other),
            (Some(&local), Connected),
            (None, Other),
        ]);
        let expected: BTreeMap<String, LiveCounts> = [
            (
                "ssh:web".to_string(),
                LiveCounts {
                    connected: 1,
                    connecting: 1,
                },
            ),
            (
                "profile:p1".to_string(),
                LiveCounts {
                    connected: 1,
                    connecting: 0,
                },
            ),
            (
                "profile:p2".to_string(),
                LiveCounts {
                    connected: 1,
                    connecting: 0,
                },
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(counts, expected);
    }

    #[test]
    fn does_not_change_for_tab_updates_that_leave_the_dots_alone() {
        let web = crate::model::ssh_profile("web");
        let db = crate::model::ssh_profile("db");
        let local = crate::model::local_profile();
        let before = live_host_counts([(Some(&web), Connected), (Some(&local), Connecting)]);
        // A local tab connecting, a reorder and a closed tab.
        let after = live_host_counts([
            (Some(&local), Connected),
            (Some(&web), Connected),
            (Some(&db), Other),
        ]);
        assert_eq!(after, before);
    }
}
