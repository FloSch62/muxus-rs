//! Which update notice to show (`client/src/update-notice.ts`): the in-app
//! updater's decisions, and the words of the release-manifest notice that
//! development builds and Linux packages fall back to.

use muxus_core::api::{DesktopUpdateSource, DesktopUpdateState, DesktopUpdateStatus};

/// `client-state.json` item holding the release version whose notice was
/// dismissed (`muxus-dismissed-update-version`).
pub const DISMISSED_UPDATE_KEY: &str = "muxus-dismissed-update-version";
/// `client-state.json` item holding the in-app updater decision that was
/// dismissed (`muxus-dismissed-desktop-update`).
pub const DISMISSED_DESKTOP_UPDATE_KEY: &str = "muxus-dismissed-desktop-update";

/// The snackbar's tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeTone {
    Info,
    Warning,
}

/// The in-app updater owns updates; otherwise the release-manifest check does.
pub fn is_in_app_updater(state: Option<&DesktopUpdateState>) -> bool {
    state.is_some_and(|state| state.status != DesktopUpdateStatus::Disabled)
}

/// One in-app updater decision worth a notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopUpdateNotice {
    /// Identifies one decision, so dismissing it never hides a later step.
    pub key: String,
    pub message: String,
    pub tone: NoticeTone,
    /// Store updates do not reveal the target version, so their dismissal
    /// lasts only for this run and cannot hide a different future update.
    pub persist_dismissal: bool,
}

/// The status as the TypeScript spells it (`up-to-date`, `ready`, …).
pub fn status_name(status: DesktopUpdateStatus) -> &'static str {
    match status {
        DesktopUpdateStatus::Disabled => "disabled",
        DesktopUpdateStatus::Idle => "idle",
        DesktopUpdateStatus::Checking => "checking",
        DesktopUpdateStatus::UpToDate => "up-to-date",
        DesktopUpdateStatus::Available => "available",
        DesktopUpdateStatus::Downloading => "downloading",
        DesktopUpdateStatus::Ready => "ready",
        DesktopUpdateStatus::Installing => "installing",
        DesktopUpdateStatus::Updated => "updated",
        DesktopUpdateStatus::Error => "error",
    }
}

/// `desktopUpdateNotice`.
pub fn desktop_update_notice(state: Option<&DesktopUpdateState>) -> Option<DesktopUpdateNotice> {
    let state = state.filter(|state| is_in_app_updater(Some(state)))?;
    let tone = if state.status == DesktopUpdateStatus::Error {
        NoticeTone::Warning
    } else {
        NoticeTone::Info
    };
    if state.source == Some(DesktopUpdateSource::Store) {
        let message = match state.status {
            DesktopUpdateStatus::Error => state
                .error
                .clone()
                .unwrap_or_else(|| "Microsoft Store could not check for updates.".into()),
            DesktopUpdateStatus::Updated => {
                "Microsoft Store completed the update. Reopen Muxus to use the installed version."
                    .into()
            }
            DesktopUpdateStatus::Available => {
                "A new version of Muxus is available in Microsoft Store.".into()
            }
            _ => return None,
        };
        return Some(DesktopUpdateNotice {
            key: format!(
                "store:{}:{}",
                state.current_version,
                status_name(state.status)
            ),
            message,
            tone,
            persist_dismissal: false,
        });
    }
    // Availability and a completed download are separate decisions, and a
    // background check failing before any update was found is not worth a notice.
    let version = state.version.as_deref()?;
    let message = match state.status {
        DesktopUpdateStatus::Error => state
            .error
            .clone()
            .unwrap_or_else(|| "The update could not be completed.".into()),
        DesktopUpdateStatus::Ready => {
            format!("Muxus {version} is downloaded and ready to install.")
        }
        DesktopUpdateStatus::Available => format!(
            "Muxus {version} is available. You are running {}.",
            state.current_version
        ),
        _ => return None,
    };
    Some(DesktopUpdateNotice {
        key: format!("{version}:{}", status_name(state.status)),
        message,
        tone,
        persist_dismissal: true,
    })
}

/// The release-manifest notice (`ReleaseUpdateNotification`).
pub fn release_update_message(latest_version: &str, current_version: &str) -> String {
    format!("Muxus {latest_version} is available. You are running {current_version}.")
}

/// What a button of the in-app updater's notice does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopUpdateAction {
    /// "Update now" (Store, available).
    InstallStore,
    /// "Open Microsoft Store" (Store, error).
    OpenStore,
    /// "Restart to update" (downloaded).
    Restart,
    /// "Download update".
    Download,
}

impl DesktopUpdateAction {
    pub fn label(self) -> &'static str {
        match self {
            DesktopUpdateAction::InstallStore => "Update now",
            DesktopUpdateAction::OpenStore => "Open Microsoft Store",
            DesktopUpdateAction::Restart => "Restart to update",
            DesktopUpdateAction::Download => "Download update",
        }
    }

    /// Every action but the restart also dismisses the notice (`act(…)`).
    pub fn dismisses(self) -> bool {
        self != DesktopUpdateAction::Restart
    }
}

/// The action buttons before "Later", in the TypeScript's order.
pub fn desktop_update_actions(state: &DesktopUpdateState) -> Vec<DesktopUpdateAction> {
    let store = state.source == Some(DesktopUpdateSource::Store);
    let mut actions = Vec::new();
    if store && state.status == DesktopUpdateStatus::Available {
        actions.push(DesktopUpdateAction::InstallStore);
    }
    if store && state.status == DesktopUpdateStatus::Error {
        actions.push(DesktopUpdateAction::OpenStore);
    }
    if !store && state.status == DesktopUpdateStatus::Ready {
        actions.push(DesktopUpdateAction::Restart);
    }
    if !store && state.status != DesktopUpdateStatus::Ready {
        actions.push(DesktopUpdateAction::Download);
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(status: DesktopUpdateStatus) -> DesktopUpdateState {
        DesktopUpdateState {
            status,
            current_version: "0.8.0".into(),
            source: None,
            version: None,
            percent: None,
            reason: None,
            error: None,
        }
    }

    fn with_version(status: DesktopUpdateStatus, version: &str) -> DesktopUpdateState {
        DesktopUpdateState {
            version: Some(version.into()),
            ..state(status)
        }
    }

    fn store(status: DesktopUpdateStatus) -> DesktopUpdateState {
        DesktopUpdateState {
            source: Some(DesktopUpdateSource::Store),
            ..state(status)
        }
    }

    #[test]
    fn falls_back_to_the_release_check_for_browsers_development_and_linux_packages() {
        use muxus_core::api::DesktopUpdateReason;
        assert!(!is_in_app_updater(None));
        let disabled = DesktopUpdateState {
            reason: Some(DesktopUpdateReason::PackageManager),
            ..state(DesktopUpdateStatus::Disabled)
        };
        assert!(!is_in_app_updater(Some(&disabled)));
        assert!(is_in_app_updater(Some(&state(DesktopUpdateStatus::Idle))));
        let development = DesktopUpdateState {
            reason: Some(DesktopUpdateReason::Development),
            ..state(DesktopUpdateStatus::Disabled)
        };
        assert_eq!(desktop_update_notice(Some(&development)), None);
    }

    #[test]
    fn treats_availability_and_a_finished_download_as_separate_decisions() {
        let available =
            desktop_update_notice(Some(&with_version(DesktopUpdateStatus::Available, "0.9.0")))
                .unwrap();
        let ready = desktop_update_notice(Some(&DesktopUpdateState {
            percent: Some(100.),
            ..with_version(DesktopUpdateStatus::Ready, "0.9.0")
        }))
        .unwrap();
        assert_eq!(
            available.message,
            "Muxus 0.9.0 is available. You are running 0.8.0."
        );
        assert!(available.persist_dismissal);
        assert_eq!(
            ready.message,
            "Muxus 0.9.0 is downloaded and ready to install."
        );
        assert_eq!(ready.tone, NoticeTone::Info);
        assert_ne!(ready.key, available.key);
    }

    #[test]
    fn stays_quiet_while_checking_or_downloading_and_for_failed_background_checks() {
        for status in [
            DesktopUpdateStatus::Idle,
            DesktopUpdateStatus::Checking,
            DesktopUpdateStatus::UpToDate,
            DesktopUpdateStatus::Downloading,
            DesktopUpdateStatus::Installing,
        ] {
            assert_eq!(
                desktop_update_notice(Some(&with_version(status, "0.9.0"))),
                None
            );
        }
        let offline = DesktopUpdateState {
            error: Some("offline".into()),
            ..state(DesktopUpdateStatus::Error)
        };
        assert_eq!(desktop_update_notice(Some(&offline)), None);
        let checksum = desktop_update_notice(Some(&DesktopUpdateState {
            error: Some("bad checksum".into()),
            ..with_version(DesktopUpdateStatus::Error, "0.9.0")
        }))
        .unwrap();
        assert_eq!(checksum.message, "bad checksum");
        assert_eq!(checksum.tone, NoticeTone::Warning);
    }

    #[test]
    fn keeps_store_dismissals_to_this_run_since_the_store_hides_the_target_version() {
        let notice = desktop_update_notice(Some(&store(DesktopUpdateStatus::Available))).unwrap();
        assert!(!notice.persist_dismissal);
        assert_eq!(
            notice.message,
            "A new version of Muxus is available in Microsoft Store."
        );
        assert!(
            desktop_update_notice(Some(&store(DesktopUpdateStatus::Updated)))
                .unwrap()
                .message
                .contains("Reopen Muxus")
        );
        assert_eq!(
            desktop_update_notice(Some(&store(DesktopUpdateStatus::UpToDate))),
            None
        );
    }

    #[test]
    fn offers_the_actions_of_each_updater_step() {
        assert_eq!(
            desktop_update_actions(&with_version(DesktopUpdateStatus::Available, "0.9.0")),
            [DesktopUpdateAction::Download]
        );
        assert_eq!(
            desktop_update_actions(&with_version(DesktopUpdateStatus::Ready, "0.9.0")),
            [DesktopUpdateAction::Restart]
        );
        assert_eq!(
            desktop_update_actions(&store(DesktopUpdateStatus::Available)),
            [DesktopUpdateAction::InstallStore]
        );
        assert_eq!(
            desktop_update_actions(&store(DesktopUpdateStatus::Error)),
            [DesktopUpdateAction::OpenStore]
        );
        assert!(!DesktopUpdateAction::Restart.dismisses());
        assert!(DesktopUpdateAction::Download.dismisses());
        assert_eq!(
            release_update_message("0.9.0", "0.8.0"),
            "Muxus 0.9.0 is available. You are running 0.8.0."
        );
    }
}
