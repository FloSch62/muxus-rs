//! Duplicate into a numbered range (`HostEditorDialog.tsx`): new and
//! duplicated SSH hosts may name a range, e.g. `app[2-10]`, to create one
//! host per number in a single save. The name field's help shows what the
//! save will create, then its progress; the dialog cannot be closed while a
//! range saves.

use std::collections::HashSet;

use muxus_core::api::SavedHostKind;
use muxus_ui::gpui_kit::Context;
use muxus_ui::theme::Status;

use super::super::logic::draft::{HostDraft, Storage, draft_problem};
use super::super::logic::name_range::{
    DraftRange, describe_draft_range, expand_draft_range, range_example, range_problem,
};
use super::super::logic::range_save::{RangeSaveSeverity, save_host_range, summarize_range_save};
use super::super::range_services::BackendRangeSteps;
use super::super::services::ConnectTarget;
use super::{HostEditor, HostEditorEvent, HostEditorTarget};
use crate::hosts::managed::ManagedHost;

impl HostEditor {
    /// A new or duplicated SSH host, which may name a range.
    fn creates_hosts(&self) -> bool {
        self.kind == SavedHostKind::Ssh && !self.editing()
    }

    /// The duplicated host's name, which the HostName follows.
    fn range_source_name(&self) -> Option<&str> {
        match &self.target {
            HostEditorTarget::Duplicate(ManagedHost::Ssh(entry)) => Some(&entry.alias),
            HostEditorTarget::Duplicate(ManagedHost::Profile(entry)) => Some(&entry.name),
            _ => None,
        }
    }

    /// Names a new host may not take, lowercase.
    fn existing_names(&self) -> HashSet<String> {
        if self.ssh.storage == Storage::Openssh {
            self.data
                .ssh_config
                .iter()
                .flat_map(|config| &config.hosts)
                .flat_map(|host| &host.aliases)
                .map(|alias| alias.to_lowercase())
                .collect()
        } else {
            self.data
                .saved_hosts
                .iter()
                .map(|profile| profile.name.to_lowercase())
                .collect()
        }
    }

    /// The range the name asks for, when it asks for one.
    pub(super) fn ssh_range(&self) -> Option<DraftRange> {
        if !self.creates_hosts() {
            return None;
        }
        expand_draft_range(&self.ssh, self.range_source_name(), &self.existing_names())
    }

    /// Why the SSH draft cannot be saved: its range, or the draft itself.
    pub(super) fn ssh_problem(&self) -> Option<String> {
        match self.ssh_range() {
            Some(range) => range_problem(&range),
            None => draft_problem(&self.ssh),
        }
    }

    /// The draft the Host block preview shows: a range's first host.
    pub(super) fn preview_draft(&self) -> HostDraft {
        self.ssh_range()
            .and_then(|range| range.drafts.into_iter().next())
            .unwrap_or_else(|| self.ssh.clone())
    }

    /// Replaces the name field's help: the save's progress, what a range will
    /// create, or a tip when duplicating.
    pub(super) fn range_helper(&self) -> Option<String> {
        if let Some(progress) = &self.range_progress {
            return Some(progress.clone());
        }
        if let Some(range) = self.ssh_range()
            && range.problem.is_none()
        {
            return Some(describe_draft_range(&range.drafts));
        }
        matches!(self.target, HostEditorTarget::Duplicate(_)).then(|| {
            format!(
                "Tip: {} creates one copy per number",
                range_example(self.range_source_name())
            )
        })
    }

    /// Save every host of a range; `false` when the draft names no range.
    pub(super) fn save_range(&mut self, connect: bool, cx: &mut Context<Self>) -> bool {
        let Some(range) = self.ssh_range() else {
            return false;
        };
        if range.problem.is_some() || range.drafts.is_empty() {
            return true;
        }
        let drafts = range.drafts;
        self.busy = true;
        self.range_progress = Some(String::new());
        cx.notify();
        let backend = self.backend.clone();
        let task = cx.spawn(async move |this, cx| {
            let steps = BackendRangeSteps(&backend);
            let report = save_host_range(&drafts, &steps, |message| {
                let _ = this.update(cx, |this, cx| {
                    this.range_progress = Some(message);
                    cx.notify();
                });
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.range_progress = None;
                let summary = summarize_range_save(&report);
                let message = match &summary.detail {
                    Some(detail) => format!("{} {detail}", summary.message),
                    None => summary.message.clone(),
                };
                let status = match summary.severity {
                    RangeSaveSeverity::Success => Status::Success,
                    RangeSaveSeverity::Warning => Status::Warning,
                    RangeSaveSeverity::Error => Status::Error,
                };
                if report.created.is_empty() {
                    // Nothing changed: keep the dialog so the range can be fixed.
                    cx.emit(HostEditorEvent::Toast { status, message });
                    cx.notify();
                    return;
                }
                cx.emit(HostEditorEvent::Toast { status, message });
                let connect = (connect && summary.severity == RangeSaveSeverity::Success)
                    .then(|| match report.first_profile {
                        Some(profile) => Some(ConnectTarget::SavedHost(Box::new(profile))),
                        None => drafts
                            .first()
                            .map(|draft| ConnectTarget::Alias(draft.alias_text.clone())),
                    })
                    .flatten();
                cx.emit(HostEditorEvent::Saved { connect });
            });
        });
        self._tasks.push(task);
        true
    }
}
