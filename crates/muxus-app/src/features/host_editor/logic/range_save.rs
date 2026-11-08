//! Saving a host range (`host-editor/range-save.ts`): create every host of
//! the range, then save each one's settings. ssh_config hosts are written in
//! one request, so they all exist or none do; Muxus profiles are created one
//! by one and stop at the first failure. A settings failure does not stop the
//! rest: the host exists, and the report names it.

use std::future::Future;

use muxus_core::api::{HostUpsertRequest, SavedHostProfileInput};

use super::draft::{HostDraft, Storage, draft_to_request, draft_to_saved_ssh_input};

/// The requests a range save makes; the editor passes the real backend calls.
pub trait RangeSaveSteps {
    /// A saved Muxus profile.
    type Profile: Clone;

    /// Every new ssh_config host in one write, so `.muxus.bak` predates the save.
    fn create_config_hosts(
        &self,
        requests: Vec<HostUpsertRequest>,
    ) -> impl Future<Output = Result<(), String>>;

    fn save_profile(
        &self,
        input: SavedHostProfileInput,
    ) -> impl Future<Output = Result<Self::Profile, String>>;

    /// Folder, colour, logging and the rest, saved next to a host that exists.
    fn save_settings(
        &self,
        draft: &HostDraft,
        profile: Option<&Self::Profile>,
    ) -> impl Future<Output = Result<(), String>>;
}

/// What a range save did.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeSaveReport<P> {
    pub total: usize,
    /// Hosts that exist now, in order.
    pub created: Vec<String>,
    /// The first saved Muxus profile, for Save & connect.
    pub first_profile: Option<P>,
    /// Creating stopped at this host (its name and the error); it and the ones
    /// after it do not exist.
    pub stopped: Option<(String, String)>,
    /// Hosts that exist but whose settings could not be saved, and why.
    pub missing_settings: Vec<(String, String)>,
}

/// `saveHostRange`.
pub async fn save_host_range<S: RangeSaveSteps>(
    drafts: &[HostDraft],
    steps: &S,
    mut on_progress: impl FnMut(String),
) -> RangeSaveReport<S::Profile> {
    let mut report = RangeSaveReport {
        total: drafts.len(),
        created: Vec::new(),
        first_profile: None,
        stopped: None,
        missing_settings: Vec::new(),
    };
    let mut profiles: Vec<Option<S::Profile>> = Vec::new();

    if drafts
        .first()
        .is_some_and(|d| d.storage == Storage::Openssh)
    {
        on_progress(format!("Writing {} hosts to ssh_config…", drafts.len()));
        let requests = drafts
            .iter()
            .map(|draft| draft_to_request(draft, None))
            .collect();
        if let Err(error) = steps.create_config_hosts(requests).await {
            report.stopped = Some((drafts[0].alias_text.clone(), error));
            return report;
        }
        for draft in drafts {
            report.created.push(draft.alias_text.clone());
            profiles.push(None);
        }
    } else {
        for (i, draft) in drafts.iter().enumerate() {
            on_progress(format!(
                "Creating {} ({} of {})…",
                draft.alias_text,
                i + 1,
                drafts.len()
            ));
            match steps
                .save_profile(draft_to_saved_ssh_input(draft, None))
                .await
            {
                Ok(profile) => {
                    report.created.push(draft.alias_text.clone());
                    if report.first_profile.is_none() {
                        report.first_profile = Some(profile.clone());
                    }
                    profiles.push(Some(profile));
                }
                Err(error) => {
                    report.stopped = Some((draft.alias_text.clone(), error));
                    break;
                }
            }
        }
    }

    let created = report.created.clone();
    for (i, name) in created.iter().enumerate() {
        on_progress(format!(
            "Saving settings for {name} ({} of {})…",
            i + 1,
            created.len()
        ));
        if let Err(error) = steps.save_settings(&drafts[i], profiles[i].as_ref()).await {
            report.missing_settings.push((name.clone(), error));
        }
    }
    report
}

/// How the result is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSaveSeverity {
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeSaveSummary {
    pub severity: RangeSaveSeverity,
    pub message: String,
    /// The first error behind a warning, for the toast's details.
    pub detail: Option<String>,
}

/// `summarizeRangeSave`.
pub fn summarize_range_save<P>(report: &RangeSaveReport<P>) -> RangeSaveSummary {
    let created = report.created.len();
    if created == 0 {
        return RangeSaveSummary {
            severity: RangeSaveSeverity::Error,
            message: "No hosts were created.".into(),
            detail: report.stopped.as_ref().map(|(_, error)| error.clone()),
        };
    }
    let mut parts = Vec::new();
    match &report.stopped {
        Some((name, _)) => parts.push(format!(
            "Created {created} of {} hosts, then stopped at {name}.",
            report.total
        )),
        None => parts.push(format!("Created {created} hosts.")),
    }
    if !report.missing_settings.is_empty() {
        let names: Vec<&str> = report
            .missing_settings
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        let listed = if names.len() > 5 {
            format!("{} and {} more", names[..5].join(", "), names.len() - 5)
        } else {
            names.join(", ")
        };
        parts.push(format!(
            "Folder, colour and logging settings were not saved for {listed} \u{2014} edit them to set these."
        ));
    }
    let failure = report
        .stopped
        .as_ref()
        .map(|(_, error)| error)
        .or_else(|| report.missing_settings.first().map(|(_, error)| error));
    match failure {
        None => RangeSaveSummary {
            severity: RangeSaveSeverity::Success,
            message: parts.join(" "),
            detail: None,
        },
        Some(error) => RangeSaveSummary {
            severity: RangeSaveSeverity::Warning,
            message: parts.join(" "),
            detail: Some(error.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    //! Port of `tests/unit/client/host-range-save.test.ts`.

    use std::cell::RefCell;

    use super::super::draft::blank_draft;
    use super::*;

    fn drafts(storage: Storage, names: &[&str]) -> Vec<HostDraft> {
        names
            .iter()
            .map(|name| HostDraft {
                alias_text: (*name).into(),
                hostname: format!("{name}.corp"),
                ..blank_draft("", "", storage)
            })
            .collect()
    }

    /// Records the calls; each step can be made to fail.
    #[derive(Default)]
    struct Steps {
        config_calls: RefCell<Vec<Vec<Vec<String>>>>,
        settings_calls: RefCell<Vec<String>>,
        fail_config: Option<&'static str>,
        fail_profile: Option<(&'static str, &'static str)>,
        fail_settings: Option<(&'static str, &'static str)>,
    }

    impl RangeSaveSteps for Steps {
        type Profile = String;

        async fn create_config_hosts(
            &self,
            requests: Vec<HostUpsertRequest>,
        ) -> Result<(), String> {
            self.config_calls
                .borrow_mut()
                .push(requests.into_iter().map(|r| r.aliases).collect());
            match self.fail_config {
                Some(error) => Err(error.into()),
                None => Ok(()),
            }
        }

        async fn save_profile(&self, input: SavedHostProfileInput) -> Result<String, String> {
            match self.fail_profile {
                Some((name, error)) if input.name == name => Err(error.into()),
                _ => Ok(format!("id-{}", input.name)),
            }
        }

        async fn save_settings(&self, draft: &HostDraft, _: Option<&String>) -> Result<(), String> {
            self.settings_calls
                .borrow_mut()
                .push(draft.alias_text.clone());
            match self.fail_settings {
                Some((name, error)) if draft.alias_text == name => Err(error.into()),
                _ => Ok(()),
            }
        }
    }

    fn save(drafts: &[HostDraft], steps: &Steps) -> RangeSaveReport<String> {
        futures_lite::future::block_on(save_host_range(drafts, steps, |_| {}))
    }

    #[test]
    fn writes_every_ssh_config_host_in_one_request_then_saves_each_hosts_settings() {
        let steps = Steps::default();
        let report = save(&drafts(Storage::Openssh, &["app2", "app3"]), &steps);
        assert_eq!(
            *steps.config_calls.borrow(),
            [vec![vec!["app2".to_string()], vec!["app3".to_string()]]]
        );
        assert_eq!(steps.settings_calls.borrow().len(), 2);
        assert_eq!(report.created, ["app2", "app3"]);
        assert!(report.missing_settings.is_empty());
        assert_eq!(
            summarize_range_save(&report),
            RangeSaveSummary {
                severity: RangeSaveSeverity::Success,
                message: "Created 2 hosts.".into(),
                detail: None
            }
        );
    }

    #[test]
    fn creates_nothing_when_the_ssh_config_write_is_refused() {
        let steps = Steps {
            fail_config: Some("app3 already exists"),
            ..Steps::default()
        };
        let report = save(&drafts(Storage::Openssh, &["app2", "app3"]), &steps);
        assert!(steps.settings_calls.borrow().is_empty());
        assert_eq!(
            summarize_range_save(&report),
            RangeSaveSummary {
                severity: RangeSaveSeverity::Error,
                message: "No hosts were created.".into(),
                detail: Some("app3 already exists".into())
            }
        );
    }

    #[test]
    fn counts_a_host_whose_settings_failed_as_created_and_names_it() {
        let steps = Steps {
            fail_settings: Some(("web3", "metadata rejected")),
            ..Steps::default()
        };
        let report = save(&drafts(Storage::Muxus, &["web2", "web3", "web4"]), &steps);
        assert_eq!(report.created, ["web2", "web3", "web4"]);
        assert_eq!(report.stopped, None);
        assert_eq!(
            summarize_range_save(&report),
            RangeSaveSummary {
                severity: RangeSaveSeverity::Warning,
                message: "Created 3 hosts. Folder, colour and logging settings were not saved for web3 \u{2014} edit them to set these.".into(),
                detail: Some("metadata rejected".into())
            }
        );
    }

    #[test]
    fn stops_creating_muxus_profiles_at_the_first_failure_and_keeps_the_ones_made() {
        let steps = Steps {
            fail_profile: Some(("web3", "name too long")),
            ..Steps::default()
        };
        let report = save(&drafts(Storage::Muxus, &["web2", "web3", "web4"]), &steps);
        assert_eq!(report.created, ["web2"]);
        assert_eq!(report.first_profile.as_deref(), Some("id-web2"));
        assert_eq!(steps.settings_calls.borrow().len(), 1);
        let summary = summarize_range_save(&report);
        assert_eq!(summary.severity, RangeSaveSeverity::Warning);
        assert_eq!(
            summary.message,
            "Created 1 of 3 hosts, then stopped at web3."
        );
        assert_eq!(summary.detail.as_deref(), Some("name too long"));
    }

    #[test]
    fn reports_progress_as_it_goes() {
        let mut progress = Vec::new();
        futures_lite::future::block_on(save_host_range(
            &drafts(Storage::Muxus, &["a1", "a2"]),
            &Steps::default(),
            |message| progress.push(message),
        ));
        assert_eq!(
            progress,
            [
                "Creating a1 (1 of 2)…",
                "Creating a2 (2 of 2)…",
                "Saving settings for a1 (1 of 2)…",
                "Saving settings for a2 (2 of 2)…",
            ]
        );
    }

    #[test]
    fn lists_at_most_five_hosts_whose_settings_were_not_saved() {
        let report = RangeSaveReport::<String> {
            total: 7,
            created: (1..=7).map(|n| format!("h{n}")).collect(),
            first_profile: None,
            stopped: None,
            missing_settings: (1..=7).map(|n| (format!("h{n}"), "x".into())).collect(),
        };
        assert!(
            summarize_range_save(&report)
                .message
                .contains("h1, h2, h3, h4, h5 and 2 more")
        );
    }
}
