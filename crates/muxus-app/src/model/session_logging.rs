//! A session logging policy as the editors hold it, the application default in
//! settings and a host's own in the host editor (`client/src/session-logging-policy.ts`).

use muxus_core::api::{SessionLoggingPolicy, SessionLoggingPolicyInput};

/// `HostSessionLoggingDraft`: every policy field, plus whether the host
/// inherits the application default and whether the effective policy has
/// loaded yet (saving waits for it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostSessionLoggingDraft {
    pub enabled: bool,
    pub capture_input: bool,
    pub max_part_bytes: u64,
    pub max_parts: u32,
    pub log_to_file: bool,
    /// Inherit the application default instead of storing a host override.
    pub inherit: bool,
    /// Prevent saving before the effective server policy has been loaded.
    pub loaded: bool,
}

/// `FALLBACK_SESSION_LOGGING_POLICY`.
pub const FALLBACK_ENABLED: bool = false;
pub const FALLBACK_MAX_PART_BYTES: u64 = 5 * 1024 * 1024;
pub const FALLBACK_MAX_PARTS: u32 = 10;

/// `FALLBACK_SESSION_LOGGING_POLICY` as a draft with the given flags.
pub fn fallback_session_logging_draft(inherit: bool, loaded: bool) -> HostSessionLoggingDraft {
    HostSessionLoggingDraft {
        inherit,
        loaded,
        ..blank_host_session_logging_draft()
    }
}

/// `blankHostSessionLoggingDraft()`.
pub fn blank_host_session_logging_draft() -> HostSessionLoggingDraft {
    HostSessionLoggingDraft {
        enabled: FALLBACK_ENABLED,
        capture_input: false,
        max_part_bytes: FALLBACK_MAX_PART_BYTES,
        max_parts: FALLBACK_MAX_PARTS,
        log_to_file: false,
        inherit: true,
        loaded: false,
    }
}

impl Default for HostSessionLoggingDraft {
    fn default() -> Self {
        blank_host_session_logging_draft()
    }
}

/// `hostSessionLoggingDraft(policy, inherit)`.
pub fn host_session_logging_draft(
    policy: &SessionLoggingPolicy,
    inherit: bool,
) -> HostSessionLoggingDraft {
    HostSessionLoggingDraft {
        enabled: policy.enabled,
        capture_input: policy.capture_input,
        max_part_bytes: policy.max_part_bytes,
        max_parts: policy.max_parts,
        log_to_file: policy.log_to_file,
        inherit,
        loaded: true,
    }
}

/// Whether two drafts would save the same policy; fields of an inherited one do not count.
pub fn same_session_logging_draft(
    a: &HostSessionLoggingDraft,
    b: &HostSessionLoggingDraft,
) -> bool {
    if a.inherit != b.inherit {
        return false;
    }
    a.inherit
        || (a.enabled == b.enabled
            && a.capture_input == b.capture_input
            && a.max_part_bytes == b.max_part_bytes
            && a.max_parts == b.max_parts
            && a.log_to_file == b.log_to_file)
}

/// `sessionLoggingPolicyInput(draft)`.
pub fn session_logging_policy_input(draft: &HostSessionLoggingDraft) -> SessionLoggingPolicyInput {
    SessionLoggingPolicyInput {
        enabled: draft.enabled,
        capture_input: draft.capture_input,
        max_part_bytes: draft.max_part_bytes,
        max_parts: draft.max_parts,
        log_to_file: Some(draft.log_to_file),
    }
}

/// The policy a save writes for the draft: `None` removes the host override.
pub fn policy_to_save(draft: &HostSessionLoggingDraft) -> Option<SessionLoggingPolicyInput> {
    (!draft.inherit).then(|| session_logging_policy_input(draft))
}

/// `clampInteger` of `SessionLoggingPolicyFields`: non-finite input becomes
/// `min`, everything else is rounded and clamped.
pub fn clamp_integer(value: f64, min: i64, max: i64) -> i64 {
    if !value.is_finite() {
        return min;
    }
    (js_round(value) as i64).clamp(min, max)
}

/// `Math.round`: halves round towards +∞.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

/// `formatRetention`: the most raw data one session keeps.
pub fn format_retention(draft: &HostSessionLoggingDraft) -> String {
    let bytes = draft.max_part_bytes as f64 * f64::from(draft.max_parts);
    let gib = 1024. * 1024. * 1024.;
    if bytes >= gib {
        return format!("{} GiB", muxus_core::js::to_fixed(bytes / gib, 1));
    }
    format!("{} MiB", js_round(bytes / 1024. / 1024.) as i64)
}

/// The caption under "Use default session logging settings".
pub fn inherit_caption(draft: &HostSessionLoggingDraft) -> String {
    if draft.inherit {
        format!(
            "Inherited: logging {}{}.",
            if draft.enabled { "enabled" } else { "disabled" },
            if draft.log_to_file {
                ", log files on"
            } else {
                ""
            }
        )
    } else {
        "This host has its own logging and retention policy.".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(enabled: bool, overridden: bool) -> SessionLoggingPolicy {
        SessionLoggingPolicy {
            profile_key: "*".into(),
            enabled,
            capture_input: false,
            max_part_bytes: FALLBACK_MAX_PART_BYTES,
            max_parts: FALLBACK_MAX_PARTS,
            log_to_file: false,
            overridden,
        }
    }

    #[test]
    fn blank_drafts_inherit_and_wait_for_the_policy() {
        let draft = blank_host_session_logging_draft();
        assert!(draft.inherit);
        assert!(!draft.loaded);
        assert_eq!(draft.max_part_bytes, 5 * 1024 * 1024);
        assert_eq!(draft.max_parts, 10);
    }

    #[test]
    fn inherited_drafts_compare_equal_whatever_their_fields() {
        let a = host_session_logging_draft(&policy(true, false), true);
        let b = host_session_logging_draft(&policy(false, false), true);
        assert!(same_session_logging_draft(&a, &b));
        let c = host_session_logging_draft(&policy(true, true), false);
        let d = host_session_logging_draft(&policy(false, true), false);
        assert!(!same_session_logging_draft(&c, &d));
        assert!(!same_session_logging_draft(&a, &c));
    }

    #[test]
    fn formats_retention_like_the_client() {
        let mut draft = blank_host_session_logging_draft();
        assert_eq!(format_retention(&draft), "50 MiB");
        draft.max_part_bytes = 1024 * 1024 * 1024;
        draft.max_parts = 3;
        assert_eq!(format_retention(&draft), "3.0 GiB");
    }

    #[test]
    fn clamps_like_the_number_fields() {
        assert_eq!(clamp_integer(f64::NAN, 1, 1024), 1);
        assert_eq!(clamp_integer(0., 1, 1024), 1);
        assert_eq!(clamp_integer(2.5, 1, 1024), 3);
        assert_eq!(clamp_integer(5000., 1, 1000), 1000);
    }

    #[test]
    fn writes_no_override_when_inheriting() {
        let mut draft = blank_host_session_logging_draft();
        assert_eq!(policy_to_save(&draft), None);
        draft.inherit = false;
        draft.enabled = true;
        assert_eq!(
            policy_to_save(&draft),
            Some(SessionLoggingPolicyInput {
                enabled: true,
                capture_input: false,
                max_part_bytes: 5 * 1024 * 1024,
                max_parts: 10,
                log_to_file: Some(false),
            })
        );
    }
}
