//! Forms that save explicitly (`client/src/settings-draft.ts`) and the
//! session logging policy drafts they hold (`client/src/session-logging-policy.ts`).

use super::js::{number_to_string, to_fixed};

/// The draft once the stored value moves from `previous` to `stored`. A draft
/// without unsaved edits follows it, one with edits keeps them, and a draft
/// that was just saved takes what the server kept.
pub fn follow_stored_draft<T: Clone>(
    current: &T,
    previous: Option<&T>,
    stored: &T,
    just_saved: bool,
    same: impl Fn(&T, &T) -> bool,
) -> T {
    match previous {
        Some(previous) if !just_saved && !same(current, previous) => current.clone(),
        _ => stored.clone(),
    }
}

/// A settings form that saves explicitly (`useSettingsDraft`). Feed it every
/// stored value as it arrives with [`SettingsDraft::observe`]; `same` compares
/// the way the server normalizes, e.g. ignoring surrounding whitespace.
#[derive(Clone, Debug)]
pub struct SettingsDraft<T> {
    draft: T,
    baseline: Option<T>,
    just_saved: bool,
    same: fn(&T, &T) -> bool,
}

impl<T: Clone + PartialEq> SettingsDraft<T> {
    pub fn new(initial: T, same: fn(&T, &T) -> bool) -> Self {
        Self {
            draft: initial,
            baseline: None,
            just_saved: false,
            same,
        }
    }

    /// The stored value as last read. A value equal to the baseline changes
    /// nothing (the TypeScript compares a memoized identity that only moves
    /// when the stored value really changes).
    pub fn observe(&mut self, stored: &T) {
        if self.baseline.as_ref() == Some(stored) {
            return;
        }
        let next = follow_stored_draft(
            &self.draft,
            self.baseline.as_ref(),
            stored,
            self.just_saved,
            self.same,
        );
        self.baseline = Some(stored.clone());
        self.just_saved = false;
        self.draft = next;
    }

    pub fn draft(&self) -> &T {
        &self.draft
    }

    /// The draft differs from the stored value, so undoing an edit clears it.
    pub fn dirty(&self) -> bool {
        self.baseline
            .as_ref()
            .is_some_and(|baseline| !(self.same)(&self.draft, baseline))
    }

    pub fn edit(&mut self, patch: impl FnOnce(&mut T)) {
        patch(&mut self.draft);
    }

    /// Call after a successful save: the next stored value replaces the draft.
    pub fn saved(&mut self) {
        self.just_saved = true;
    }

    /// Whether a stored value has arrived yet.
    pub fn loaded(&self) -> bool {
        self.baseline.is_some()
    }
}

/// A session logging policy being edited: the shared model
/// (`crate::model::session_logging`).
pub use crate::model::session_logging::{
    HostSessionLoggingDraft as SessionLoggingDraft, fallback_session_logging_draft,
    format_retention, host_session_logging_draft, same_session_logging_draft,
    session_logging_policy_input,
};

/// `clampInteger` of the policy fields: non-finite input takes the minimum.
pub fn clamp_integer(value: f64, min: f64, max: f64) -> f64 {
    if !value.is_finite() {
        return min;
    }
    max.min(min.max(muxus_store::prefs::layout::js_round(value)))
}

/// The log file form (`LogFileDraft` in `SettingsDialog.tsx`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogFileDraft {
    pub directory: String,
    pub filename_pattern: String,
    pub timestamps: bool,
}

pub fn same_log_file_draft(a: &LogFileDraft, b: &LogFileDraft) -> bool {
    muxus_core::js::js_trim(&a.directory) == muxus_core::js::js_trim(&b.directory)
        && muxus_core::js::js_trim(&a.filename_pattern)
            == muxus_core::js::js_trim(&b.filename_pattern)
        && a.timestamps == b.timestamps
}

/// The history storage form (`HistoryStorageDraft`), every field as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryStorageDraft {
    pub storage_location: String,
    pub max_total_gib: String,
    pub min_free_gib: String,
    pub min_free_percent: String,
    pub max_age_days: String,
}

impl Default for HistoryStorageDraft {
    /// `EMPTY_HISTORY_STORAGE_DRAFT`.
    fn default() -> Self {
        Self {
            storage_location: String::new(),
            max_total_gib: "5".into(),
            min_free_gib: "2".into(),
            min_free_percent: "5".into(),
            max_age_days: String::new(),
        }
    }
}

pub fn same_history_storage_draft(a: &HistoryStorageDraft, b: &HistoryStorageDraft) -> bool {
    let t = muxus_core::js::js_trim;
    t(&a.storage_location) == t(&b.storage_location)
        && t(&a.max_total_gib) == t(&b.max_total_gib)
        && t(&a.min_free_gib) == t(&b.min_free_gib)
        && t(&a.min_free_percent) == t(&b.min_free_percent)
        && t(&a.max_age_days) == t(&b.max_age_days)
}

const GIB: f64 = 1024. * 1024. * 1024.;

/// JavaScript `Number(text)` for a form field: blank is 0, junk is NaN.
pub fn js_number(text: &str) -> f64 {
    let trimmed = muxus_core::js::js_trim(text);
    if trimmed.is_empty() {
        return 0.;
    }
    match trimmed {
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => {
            let lower = trimmed.to_ascii_lowercase();
            if let Some(hex) = lower.strip_prefix("0x") {
                return u64::from_str_radix(hex, 16).map_or(f64::NAN, |v| v as f64);
            }
            // Rust accepts "inf"/"nan" spellings JavaScript does not.
            if lower.contains("inf") || lower.contains("nan") {
                return f64::NAN;
            }
            trimmed.parse::<f64>().unwrap_or(f64::NAN)
        }
    }
}

/// `gibToBytes`: -1 for anything that is not a finite, non-negative number.
pub fn gib_to_bytes(value: &str) -> f64 {
    let number = js_number(value);
    if number.is_finite() && number >= 0. {
        muxus_store::prefs::layout::js_round(number * GIB)
    } else {
        -1.
    }
}

/// `bytesToGiB`: three decimals, trailing zeros dropped.
pub fn bytes_to_gib(value: u64) -> String {
    let rounded: f64 = to_fixed(value as f64 / GIB, 3).parse().unwrap_or(0.);
    number_to_string(rounded)
}

/// `formatStorageBytes`.
pub fn format_storage_bytes(value: u64) -> String {
    let value = value as f64;
    if value < 1024. * 1024. {
        format!("{} KiB", to_fixed(value / 1024., 1))
    } else if value < GIB {
        format!("{} MiB", to_fixed(value / 1024. / 1024., 1))
    } else {
        format!("{} GiB", to_fixed(value / GIB, 2))
    }
}

/// The storage form's values once parsed, and whether they may be saved.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryStorageValues {
    pub max_total_bytes: f64,
    pub min_free_bytes: f64,
    pub min_free_percent: f64,
    pub max_age_days: Option<f64>,
    pub valid: bool,
}

pub fn history_storage_values(draft: &HistoryStorageDraft) -> HistoryStorageValues {
    let max_total_bytes = gib_to_bytes(&draft.max_total_gib);
    let min_free_bytes = gib_to_bytes(&draft.min_free_gib);
    let min_free_percent = js_number(&draft.min_free_percent);
    let max_age_days = (!draft.max_age_days.is_empty()).then(|| js_number(&draft.max_age_days));
    let valid = max_total_bytes >= 64. * 1024. * 1024.
        && min_free_bytes >= 0.
        && (0. ..=100.).contains(&min_free_percent)
        && max_age_days.is_none_or(|days| days.fract() == 0. && days.is_finite() && days >= 1.);
    HistoryStorageValues {
        max_total_bytes,
        min_free_bytes,
        min_free_percent,
        max_age_days,
        valid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Value {
        value: String,
    }

    fn v(text: &str) -> Value {
        Value { value: text.into() }
    }

    fn same(a: &Value, b: &Value) -> bool {
        a.value.trim() == b.value.trim()
    }

    #[test]
    fn starts_from_the_first_stored_value() {
        assert_eq!(
            follow_stored_draft(&v("initial"), None, &v("stored"), false, same),
            v("stored")
        );
    }

    #[test]
    fn follows_a_refetch_while_the_draft_holds_no_edits_in_any_spacing() {
        assert_eq!(
            follow_stored_draft(&v(" old "), Some(&v("old")), &v("new"), false, same),
            v("new")
        );
    }

    #[test]
    fn keeps_unsaved_edits_across_a_refetch() {
        assert_eq!(
            follow_stored_draft(&v("edited"), Some(&v("old")), &v("new"), false, same),
            v("edited")
        );
    }

    #[test]
    fn takes_what_the_server_kept_right_after_a_save() {
        assert_eq!(
            follow_stored_draft(&v("Edited "), Some(&v("old")), &v("edited"), true, same),
            v("edited")
        );
    }

    #[test]
    fn the_draft_hook_tracks_dirtiness_and_saves() {
        let mut draft = SettingsDraft::new(v(""), same);
        assert!(!draft.dirty());
        assert!(!draft.loaded());
        draft.observe(&v("stored"));
        assert_eq!(draft.draft(), &v("stored"));
        draft.edit(|value| value.value = "stored ".into());
        assert!(!draft.dirty(), "whitespace is normalized away");
        draft.edit(|value| value.value = "changed".into());
        assert!(draft.dirty());
        // A refetch keeps the edit.
        draft.observe(&v("other"));
        assert_eq!(draft.draft(), &v("changed"));
        // The save lands: the next stored value replaces the draft.
        draft.saved();
        draft.observe(&v("Changed"));
        assert_eq!(draft.draft(), &v("Changed"));
        assert!(!draft.dirty());
        // The same value again changes nothing.
        draft.edit(|value| value.value = "x".into());
        draft.observe(&v("Changed"));
        assert_eq!(draft.draft(), &v("x"));
    }

    fn policy_draft(patch: impl FnOnce(&mut SessionLoggingDraft)) -> SessionLoggingDraft {
        let mut draft = fallback_session_logging_draft(false, true);
        patch(&mut draft);
        draft
    }

    #[test]
    fn counts_every_saved_field_and_ignores_the_loading_flag() {
        let base = policy_draft(|_| {});
        assert!(same_session_logging_draft(
            &base,
            &policy_draft(|d| d.loaded = false)
        ));
        assert!(!same_session_logging_draft(
            &base,
            &policy_draft(|d| d.log_to_file = true)
        ));
        assert!(!same_session_logging_draft(
            &base,
            &policy_draft(|d| d.max_parts = 3)
        ));
    }

    #[test]
    fn treats_inherited_drafts_as_equal_whatever_their_fields_show() {
        assert!(same_session_logging_draft(
            &policy_draft(|d| d.inherit = true),
            &policy_draft(|d| {
                d.inherit = true;
                d.enabled = true;
            })
        ));
        assert!(!same_session_logging_draft(
            &policy_draft(|d| d.inherit = true),
            &policy_draft(|_| {})
        ));
    }

    #[test]
    fn retention_and_clamping_follow_the_form() {
        assert_eq!(format_retention(&policy_draft(|_| {})), "50 MiB");
        assert_eq!(
            format_retention(&policy_draft(|d| {
                d.max_part_bytes = 1024 * 1024 * 1024;
                d.max_parts = 3;
            })),
            "3.0 GiB"
        );
        assert_eq!(clamp_integer(f64::NAN, 1., 1024.), 1.);
        assert_eq!(clamp_integer(2.5, 1., 1024.), 3.);
        assert_eq!(clamp_integer(5000., 1., 1024.), 1024.);
        assert_eq!(clamp_integer(-3., 1., 1000.), 1.);
    }

    #[test]
    fn history_storage_fields_parse_like_the_form() {
        assert_eq!(bytes_to_gib(5 * 1024 * 1024 * 1024), "5");
        assert_eq!(bytes_to_gib(64 * 1024 * 1024), "0.063");
        assert_eq!(bytes_to_gib(0), "0");
        assert_eq!(gib_to_bytes("0.25"), 268_435_456.);
        assert_eq!(gib_to_bytes(""), 0.);
        assert_eq!(gib_to_bytes("abc"), -1.);
        assert_eq!(gib_to_bytes("-1"), -1.);
        assert_eq!(format_storage_bytes(512), "0.5 KiB");
        assert_eq!(format_storage_bytes(3 * 1024 * 1024), "3.0 MiB");
        assert_eq!(format_storage_bytes(5 * 1024 * 1024 * 1024), "5.00 GiB");

        let mut draft = HistoryStorageDraft::default();
        assert!(history_storage_values(&draft).valid);
        draft.max_age_days = "1.5".into();
        assert!(!history_storage_values(&draft).valid);
        draft.max_age_days = "30".into();
        assert_eq!(history_storage_values(&draft).max_age_days, Some(30.));
        draft.max_total_gib = "0.01".into();
        assert!(!history_storage_values(&draft).valid, "below 64 MiB");
        draft.max_total_gib = "1".into();
        draft.min_free_percent = "101".into();
        assert!(!history_storage_values(&draft).valid);
        assert!(same_history_storage_draft(
            &HistoryStorageDraft::default(),
            &HistoryStorageDraft {
                max_total_gib: " 5 ".into(),
                ..HistoryStorageDraft::default()
            }
        ));
    }

    #[test]
    fn log_file_drafts_compare_trimmed() {
        let a = LogFileDraft {
            directory: " /logs ".into(),
            filename_pattern: "{host}.log".into(),
            timestamps: false,
        };
        let b = LogFileDraft {
            directory: "/logs".into(),
            ..a.clone()
        };
        assert!(same_log_file_draft(&a, &b));
        assert!(!same_log_file_draft(
            &a,
            &LogFileDraft {
                timestamps: true,
                ..b
            }
        ));
    }
}
