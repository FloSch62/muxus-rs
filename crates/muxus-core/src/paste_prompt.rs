//! A host's prompt waiting as the editors edit it (port of
//! `client/src/terminal/paste-prompt.ts`). Shared here because the host
//! editors, the bulk editor and backups all check it the same way.

use crate::js::{js_number, js_trim, number_from_text};
pub use crate::paste_pacing::is_paste_prompt_wait;
use crate::paste_pacing::{
    MAX_PASTE_PROMPT_TIMEOUT_MS, MIN_PASTE_PROMPT_TIMEOUT_MS, PastePromptWaitSetting,
    paste_pattern_problem, valid_paste_prompt_timeout,
};

/// A host's prompt waiting in the shape the editors edit; an empty field uses the default.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct PastePromptWaitDraft {
    pub enabled: bool,
    /// Kept while switched off, so switching back loses nothing.
    pub prompt_pattern: String,
    /// Milliseconds as typed.
    pub timeout_ms: String,
    pub error_pattern: String,
}

pub fn paste_prompt_wait_draft(setting: Option<&PastePromptWaitSetting>) -> PastePromptWaitDraft {
    PastePromptWaitDraft {
        enabled: setting.is_some(),
        prompt_pattern: setting
            .and_then(|s| s.prompt_pattern.clone())
            .unwrap_or_default(),
        timeout_ms: setting
            .and_then(|s| s.timeout_ms)
            .map(|ms| ms.to_string())
            .unwrap_or_default(),
        error_pattern: setting
            .and_then(|s| s.error_pattern.clone())
            .unwrap_or_default(),
    }
}

/// The typed timeout as a whole number of milliseconds, when it is one.
fn typed_timeout(text: &str) -> Option<u32> {
    let ms = number_from_text(text);
    (ms.is_finite() && ms.fract() == 0. && ms >= 0. && ms <= f64::from(u32::MAX))
        .then_some(ms as u32)
}

/// The stored form: `None` is off, and an empty field is left out so it
/// follows the default. Only a draft without a problem has a timeout that
/// fits; any other is left out too.
pub fn paste_prompt_wait_from_draft(
    draft: &PastePromptWaitDraft,
) -> Option<PastePromptWaitSetting> {
    if !draft.enabled {
        return None;
    }
    Some(PastePromptWaitSetting {
        prompt_pattern: (!draft.prompt_pattern.is_empty()).then(|| draft.prompt_pattern.clone()),
        timeout_ms: if js_trim(&draft.timeout_ms).is_empty() {
            None
        } else {
            typed_timeout(&draft.timeout_ms)
        },
        error_pattern: (!draft.error_pattern.is_empty()).then(|| draft.error_pattern.clone()),
    })
}

/// Why a typed timeout cannot be saved, or `None`; empty uses the default.
pub fn paste_prompt_timeout_problem(text: &str) -> Option<String> {
    if js_trim(text).is_empty() || valid_paste_prompt_timeout(&js_number(number_from_text(text))) {
        return None;
    }
    Some(format!(
        "Between {MIN_PASTE_PROMPT_TIMEOUT_MS} and {MAX_PASTE_PROMPT_TIMEOUT_MS} ms."
    ))
}

/// The first reason a draft cannot be saved, or `None`.
pub fn paste_prompt_wait_problem(draft: &PastePromptWaitDraft) -> Option<String> {
    if !draft.enabled {
        return None;
    }
    let pattern = |pattern: &str| {
        if pattern.is_empty() {
            None
        } else {
            paste_pattern_problem(pattern)
        }
    };
    if let Some(problem) = pattern(&draft.prompt_pattern) {
        return Some(format!("Prompt pattern: {problem}"));
    }
    if let Some(problem) = paste_prompt_timeout_problem(&draft.timeout_ms) {
        return Some(format!("Prompt timeout: {problem}"));
    }
    if let Some(problem) = pattern(&draft.error_pattern) {
        return Some(format!("Error pattern: {problem}"));
    }
    None
}

/// Whether two drafts store the same setting (`samePastePromptWaitDraft`:
/// their stored forms as JSON, where an unreadable timeout is `null`).
pub fn same_paste_prompt_wait_draft(a: &PastePromptWaitDraft, b: &PastePromptWaitDraft) -> bool {
    type Stored<'a> = (Option<&'a str>, Option<serde_json::Value>, Option<&'a str>);
    fn stored(draft: &PastePromptWaitDraft) -> Option<Stored<'_>> {
        draft.enabled.then(|| {
            (
                (!draft.prompt_pattern.is_empty()).then_some(draft.prompt_pattern.as_str()),
                (!js_trim(&draft.timeout_ms).is_empty())
                    .then(|| js_number(number_from_text(&draft.timeout_ms))),
                (!draft.error_pattern.is_empty()).then_some(draft.error_pattern.as_str()),
            )
        })
    }
    stored(a) == stored(b)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn is_off_for_a_host_without_it_and_stores_only_the_fields_that_differ_from_the_defaults() {
        let off = paste_prompt_wait_draft(None);
        assert_eq!(off, PastePromptWaitDraft::default());
        assert_eq!(paste_prompt_wait_from_draft(&off), None);
        let on = PastePromptWaitDraft {
            enabled: true,
            ..off.clone()
        };
        assert_eq!(
            paste_prompt_wait_from_draft(&on),
            Some(PastePromptWaitSetting::default())
        );

        let own = PastePromptWaitSetting {
            prompt_pattern: Some("[>#\\]]\\s*$".into()),
            timeout_ms: Some(20_000),
            error_pattern: Some("^Error:".into()),
        };
        let draft = paste_prompt_wait_draft(Some(&own));
        assert_eq!(
            draft,
            PastePromptWaitDraft {
                enabled: true,
                prompt_pattern: "[>#\\]]\\s*$".into(),
                timeout_ms: "20000".into(),
                error_pattern: "^Error:".into(),
            }
        );
        assert_eq!(paste_prompt_wait_from_draft(&draft), Some(own));
        // Switched off, the fields are kept for switching back but not saved.
        let switched_off = PastePromptWaitDraft {
            enabled: false,
            ..draft.clone()
        };
        assert_eq!(paste_prompt_wait_from_draft(&switched_off), None);
        assert!(same_paste_prompt_wait_draft(&switched_off, &off));
        assert!(!same_paste_prompt_wait_draft(&draft, &off));
    }

    #[test]
    fn names_the_field_that_keeps_a_draft_from_saving() {
        let on = paste_prompt_wait_draft(Some(&PastePromptWaitSetting::default()));
        assert_eq!(paste_prompt_wait_problem(&on), None);
        let with = |change: fn(&mut PastePromptWaitDraft)| {
            let mut draft = on.clone();
            change(&mut draft);
            paste_prompt_wait_problem(&draft)
        };
        let prompt = with(|d| d.prompt_pattern = "[".into()).unwrap();
        assert!(prompt.starts_with("Prompt pattern: "), "{prompt}");
        assert!(prompt.contains("Invalid regular expression"), "{prompt}");
        assert_eq!(
            with(|d| d.error_pattern = ".*".into()).as_deref(),
            Some("Error pattern: This pattern matches empty text, so it would match any output.")
        );
        assert_eq!(
            with(|d| d.timeout_ms = "70000".into()).as_deref(),
            Some("Prompt timeout: Between 100 and 60000 ms.")
        );
        // A problem in a switched-off draft does not count.
        assert_eq!(
            with(|d| {
                d.enabled = false;
                d.prompt_pattern = "[".into();
            }),
            None
        );
        assert_eq!(paste_prompt_timeout_problem(""), None);
        assert_eq!(paste_prompt_timeout_problem("100"), None);
    }

    #[test]
    fn checks_a_setting_read_from_a_backup() {
        assert!(is_paste_prompt_wait(&json!({})));
        assert!(is_paste_prompt_wait(
            &json!({ "promptPattern": "#\\s*$", "timeoutMs": 100, "errorPattern": "^%" })
        ));
        assert!(!is_paste_prompt_wait(&json!(null)));
        assert!(!is_paste_prompt_wait(&json!("[>#$]")));
        assert!(!is_paste_prompt_wait(&json!({ "promptPattern": 5 })));
        assert!(!is_paste_prompt_wait(&json!({ "errorPattern": "(" })));
        assert!(!is_paste_prompt_wait(&json!({ "timeoutMs": "5000" })));
    }
}
