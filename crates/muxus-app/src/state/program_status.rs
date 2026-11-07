//! What programs reported with OSC 7501, per tab (port of `client/src/state/program-status.ts`).
//!
//! The records live here rather than in the terminal, so a replaced view (a reconnect) keeps
//! them. A finished result (done or error) shows until the tab is looked at: activating it in a
//! focused window, or focusing the window with it active. Closed tabs drop their records.

use std::collections::{HashMap, HashSet};

use muxus_term::program_status::{
    ProgramStatusRecord, ProgramStatusReport, apply_program_status, drop_transient_program_status,
    summarize_program_status,
};

/// One tab's records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabProgramStatus {
    pub records: Vec<ProgramStatusRecord>,
    /// A done or error report arrived that nobody has looked at yet.
    pub unseen: bool,
}

/// Every tab's records in one window.
#[derive(Debug, Clone, Default)]
pub struct ProgramStatusStore {
    by_tab: HashMap<String, TabProgramStatus>,
    /// Monotonic report order across the window's terminals.
    next_seq: u64,
}

fn newly_finished(before: &[ProgramStatusRecord], after: &[ProgramStatusRecord]) -> bool {
    after.iter().any(|record| {
        record.state.is_finished()
            && before
                .iter()
                .find(|previous| previous.id == record.id)
                .is_none_or(|previous| previous.seq != record.seq)
    })
}

impl ProgramStatusStore {
    pub fn tab(&self, tab_id: &str) -> Option<&TabProgramStatus> {
        self.by_tab.get(tab_id)
    }

    pub fn is_empty(&self) -> bool {
        self.by_tab.is_empty()
    }

    /// Change one tab's records. `watched` says the user is looking at the tab right now (it is
    /// active in a focused window): a result finishing there counts as seen. Returns whether
    /// anything changed.
    fn update(
        &mut self,
        tab_id: &str,
        watched: bool,
        change: impl FnOnce(&mut Vec<ProgramStatusRecord>) -> bool,
    ) -> bool {
        let current = self.by_tab.get(tab_id);
        let before = current
            .map(|status| status.records.clone())
            .unwrap_or_default();
        let mut records = before.clone();
        if !change(&mut records) {
            return false;
        }
        if records.is_empty() {
            return self.by_tab.remove(tab_id).is_some();
        }
        let unseen = if newly_finished(&before, &records) {
            !watched
        } else {
            current.is_some_and(|status| status.unseen)
        };
        self.by_tab
            .insert(tab_id.to_string(), TabProgramStatus { records, unseen });
        true
    }

    /// A report or a clear from the tab's terminal.
    pub fn apply(&mut self, tab_id: &str, report: &ProgramStatusReport, watched: bool) -> bool {
        if matches!(report, ProgramStatusReport::Query) {
            return false;
        }
        self.next_seq += 1;
        let seq = self.next_seq;
        self.update(tab_id, watched, |records| {
            let before = records.clone();
            apply_program_status(records, report, seq);
            *records != before
        })
    }

    /// A new shell prompt, or the end of the session: the programs reporting work are gone.
    /// Finished results stay.
    pub fn drop_transient(&mut self, tab_id: &str) -> bool {
        self.update(tab_id, false, drop_transient_program_status)
    }

    /// A new session started in the tab (or the terminal was reset): nothing from before
    /// applies.
    pub fn reset(&mut self, tab_id: &str) -> bool {
        self.by_tab.remove(tab_id).is_some()
    }

    /// The tab was looked at: its finished results stop showing.
    pub fn mark_seen(&mut self, tab_id: &str) -> bool {
        match self.by_tab.get_mut(tab_id) {
            Some(status) if status.unseen => {
                status.unseen = false;
                true
            }
            _ => false,
        }
    }

    /// Closed tabs drop their records.
    pub fn retain_tabs(&mut self, open: &HashSet<&str>) -> bool {
        let before = self.by_tab.len();
        self.by_tab
            .retain(|tab_id, _| open.contains(tab_id.as_str()));
        self.by_tab.len() != before
    }

    /// The record a tab shows, if any.
    pub fn summary(&self, tab_id: &str) -> Option<ProgramStatusRecord> {
        let status = self.by_tab.get(tab_id)?;
        summarize_program_status(&status.records, status.unseen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_term::program_status::{ProgramState, parse_program_status};

    fn apply(store: &mut ProgramStatusStore, tab_id: &str, line: &str, watched: bool) {
        let parsed = parse_program_status(line).expect(line);
        store.apply(tab_id, &parsed, watched);
    }

    #[test]
    fn keeps_a_finished_result_unseen_until_the_tab_is_looked_at() {
        let mut store = ProgramStatusStore::default();
        apply(&mut store, "tab-1", "state=working", false);
        assert!(!store.tab("tab-1").unwrap().unseen);
        apply(&mut store, "tab-1", "state=done", false);
        assert!(store.tab("tab-1").unwrap().unseen);
        assert_eq!(store.summary("tab-1").unwrap().state, ProgramState::Done);
        assert!(store.mark_seen("tab-1"));
        assert!(!store.tab("tab-1").unwrap().unseen);
        assert_eq!(store.summary("tab-1"), None);
        assert!(!store.mark_seen("tab-1"));
    }

    #[test]
    fn counts_a_result_as_seen_when_it_finishes_in_the_tab_being_watched() {
        let mut store = ProgramStatusStore::default();
        apply(&mut store, "tab-2", "state=done", true);
        assert!(!store.tab("tab-2").unwrap().unseen);
        assert_eq!(store.summary("tab-2"), None);
    }

    #[test]
    fn a_later_report_keeps_an_unseen_result_unseen() {
        let mut store = ProgramStatusStore::default();
        apply(&mut store, "tab", "state=error:id=a", false);
        apply(&mut store, "tab", "state=working:id=b", true);
        assert!(store.tab("tab").unwrap().unseen);
        // The failure outranks the work in progress until it is seen.
        assert_eq!(store.summary("tab").unwrap().id, "a");
    }

    #[test]
    fn forgets_a_tab_once_its_records_are_empty_or_the_tab_closes() {
        let mut store = ProgramStatusStore::default();
        apply(&mut store, "tab-4", "state=working", false);
        apply(&mut store, "tab-4", "state=clear", false);
        assert!(store.tab("tab-4").is_none());

        apply(&mut store, "tab-5", "state=done", false);
        apply(&mut store, "tab-6", "state=done", false);
        assert!(store.retain_tabs(&HashSet::from(["tab-6"])));
        assert!(store.tab("tab-5").is_none());
        assert!(store.tab("tab-6").is_some());
    }

    #[test]
    fn a_prompt_or_the_end_of_the_session_drops_work_in_progress_only() {
        let mut store = ProgramStatusStore::default();
        apply(&mut store, "tab", "state=working:id=a", false);
        apply(&mut store, "tab", "state=error:id=b", false);
        assert!(store.drop_transient("tab"));
        let ids: Vec<_> = store
            .tab("tab")
            .unwrap()
            .records
            .iter()
            .map(|record| record.id.clone())
            .collect();
        assert_eq!(ids, ["b"]);
        assert!(!store.drop_transient("tab"));
        assert!(store.tab("tab").unwrap().unseen);
    }

    #[test]
    fn a_new_session_clears_everything() {
        let mut store = ProgramStatusStore::default();
        apply(&mut store, "tab", "state=done", false);
        assert!(store.reset("tab"));
        assert!(store.is_empty());
        assert!(!store.reset("tab"));
    }

    #[test]
    fn the_query_changes_nothing() {
        let mut store = ProgramStatusStore::default();
        assert!(!store.apply("tab", &ProgramStatusReport::Query, false));
        assert!(store.is_empty());
    }
}
