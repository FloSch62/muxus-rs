//! A host's anti-idle setting as the editors edit it (port of
//! `client/src/anti-idle.ts`). Shared here because the host editors, the
//! bulk editor and backups all check it the same way.

use crate::api::{
    ANTI_IDLE_MAX_INTERVAL_SECONDS, ANTI_IDLE_MIN_INTERVAL_SECONDS, ANTI_IDLE_TEXT_MAX_LENGTH,
    AntiIdle,
};
use crate::js::{js_number, js_trim, number_from_text, utf16_len};

/// The interval offered when anti-idle is first switched on.
const DEFAULT_ANTI_IDLE_INTERVAL_SECONDS: u32 = 60;

/// A host's anti-idle setting in the shape the editors edit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AntiIdleDraft {
    pub enabled: bool,
    /// Seconds as typed; kept with the text while switched off, so switching back loses nothing.
    pub interval_seconds: String,
    pub text: String,
    pub enter: bool,
}

impl Default for AntiIdleDraft {
    fn default() -> Self {
        anti_idle_draft(None)
    }
}

pub fn anti_idle_draft(setting: Option<&AntiIdle>) -> AntiIdleDraft {
    match setting {
        None => AntiIdleDraft {
            enabled: false,
            interval_seconds: DEFAULT_ANTI_IDLE_INTERVAL_SECONDS.to_string(),
            text: String::new(),
            enter: false,
        },
        Some(setting) => AntiIdleDraft {
            enabled: true,
            interval_seconds: setting.interval_seconds.to_string(),
            text: setting.text.clone(),
            enter: setting.enter,
        },
    }
}

/// The stored form: `None` is off. Only a draft without a problem has an
/// interval that fits; any other reads as 0, which the API refuses.
pub fn anti_idle_from_draft(draft: &AntiIdleDraft) -> Option<AntiIdle> {
    if !draft.enabled {
        return None;
    }
    let seconds = number_from_text(&draft.interval_seconds);
    Some(AntiIdle {
        interval_seconds: if seconds.is_finite() && seconds.fract() == 0. && seconds >= 0. {
            seconds.min(f64::from(u32::MAX)) as u32
        } else {
            0
        },
        text: draft.text.clone(),
        enter: draft.enter,
    })
}

/// Why the interval cannot be saved, or `None`.
pub fn anti_idle_interval_problem(draft: &AntiIdleDraft) -> Option<String> {
    let seconds = number_from_text(&draft.interval_seconds);
    if js_trim(&draft.interval_seconds).is_empty()
        || !(seconds.is_finite() && seconds.fract() == 0.)
        || seconds < f64::from(ANTI_IDLE_MIN_INTERVAL_SECONDS)
        || seconds > f64::from(ANTI_IDLE_MAX_INTERVAL_SECONDS)
    {
        return Some(format!(
            "Between {ANTI_IDLE_MIN_INTERVAL_SECONDS} and {ANTI_IDLE_MAX_INTERVAL_SECONDS} seconds."
        ));
    }
    None
}

/// The first reason a draft cannot be saved, or `None`.
pub fn anti_idle_problem(draft: &AntiIdleDraft) -> Option<String> {
    if !draft.enabled {
        return None;
    }
    if let Some(interval) = anti_idle_interval_problem(draft) {
        return Some(format!("Anti-idle interval: {interval}"));
    }
    if draft.text.is_empty() && !draft.enter {
        return Some("Anti-idle: enter text to type, or press Enter.".into());
    }
    if utf16_len(&draft.text) > ANTI_IDLE_TEXT_MAX_LENGTH {
        return Some(format!(
            "Anti-idle text can be up to {ANTI_IDLE_TEXT_MAX_LENGTH} characters."
        ));
    }
    // Enter is the toggle's job; a hidden one, a tab or a Ctrl+C would not show in the field.
    if draft.text.chars().any(char::is_control) {
        return Some("Anti-idle text cannot contain control characters.".into());
    }
    None
}

/// Whether two drafts store the same setting (`sameAntiIdleDraft`: their
/// stored forms as JSON, where an unreadable interval is `null`).
pub fn same_anti_idle_draft(a: &AntiIdleDraft, b: &AntiIdleDraft) -> bool {
    fn stored(draft: &AntiIdleDraft) -> Option<(serde_json::Value, &str, bool)> {
        draft.enabled.then(|| {
            (
                js_number(number_from_text(&draft.interval_seconds)),
                draft.text.as_str(),
                draft.enter,
            )
        })
    }
    stored(a) == stored(b)
}

/// Shape and range check for a setting read from a backup (`isAntiIdle`);
/// the store's caller validates it again.
pub fn is_anti_idle(value: &serde_json::Value) -> bool {
    let Some(setting) = value.as_object() else {
        return false;
    };
    let (Some(interval), Some(text), Some(enter)) = (
        setting.get("intervalSeconds").filter(|v| v.is_number()),
        setting.get("text").and_then(serde_json::Value::as_str),
        setting.get("enter").and_then(serde_json::Value::as_bool),
    ) else {
        return false;
    };
    // `String(setting.intervalSeconds)`, as the draft holds it.
    let interval_seconds = interval
        .as_f64()
        .map_or_else(String::new, |seconds| js_number(seconds).to_string());
    anti_idle_problem(&AntiIdleDraft {
        enabled: true,
        interval_seconds,
        text: text.to_owned(),
        enter,
    })
    .is_none()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn starts_switched_off_with_a_minute() {
        let draft = anti_idle_draft(None);
        assert_eq!(
            draft,
            AntiIdleDraft {
                enabled: false,
                interval_seconds: "60".into(),
                text: String::new(),
                enter: false,
            }
        );
        assert_eq!(anti_idle_from_draft(&draft), None);
        assert_eq!(anti_idle_problem(&draft), None);
    }

    #[test]
    fn round_trips_a_stored_setting() {
        let setting = AntiIdle {
            interval_seconds: 240,
            text: " ".into(),
            enter: false,
        };
        let draft = anti_idle_draft(Some(&setting));
        assert_eq!(draft.interval_seconds, "240");
        assert_eq!(anti_idle_from_draft(&draft), Some(setting));
        // Switched off, the typed values stay for switching back.
        let off = AntiIdleDraft {
            enabled: false,
            ..draft.clone()
        };
        assert_eq!(off.text, " ");
        assert!(!same_anti_idle_draft(&draft, &off));
        assert!(same_anti_idle_draft(&off, &anti_idle_draft(None)));
        let spaced = AntiIdleDraft {
            interval_seconds: " 240 ".into(),
            ..draft.clone()
        };
        assert!(same_anti_idle_draft(&draft, &spaced));
    }

    #[test]
    fn explains_what_stops_a_draft_from_being_saved() {
        let draft = |interval: &str, text: &str, enter: bool| AntiIdleDraft {
            enabled: true,
            interval_seconds: interval.into(),
            text: text.into(),
            enter,
        };
        assert_eq!(
            anti_idle_problem(&draft("9", " ", false)).as_deref(),
            Some("Anti-idle interval: Between 10 and 3600 seconds.")
        );
        assert!(anti_idle_problem(&draft("", " ", false)).is_some());
        assert!(anti_idle_problem(&draft("60.5", " ", false)).is_some());
        assert!(anti_idle_problem(&draft("3601", " ", false)).is_some());
        assert_eq!(
            anti_idle_problem(&draft("60", "", false)).as_deref(),
            Some("Anti-idle: enter text to type, or press Enter.")
        );
        assert_eq!(anti_idle_problem(&draft("60", "", true)), None);
        assert_eq!(
            anti_idle_problem(&draft("60", &"x".repeat(257), true)).as_deref(),
            Some("Anti-idle text can be up to 256 characters.")
        );
        assert_eq!(
            anti_idle_problem(&draft("60", "a\u{3}", false)).as_deref(),
            Some("Anti-idle text cannot contain control characters.")
        );
    }

    #[test]
    fn checks_a_setting_read_from_a_backup() {
        assert!(is_anti_idle(
            &json!({ "intervalSeconds": 120, "text": " ", "enter": false })
        ));
        assert!(!is_anti_idle(
            &json!({ "intervalSeconds": 1, "text": " ", "enter": false })
        ));
        assert!(!is_anti_idle(
            &json!({ "intervalSeconds": 120, "text": "", "enter": false })
        ));
        // A hidden Enter or Ctrl+C in the text is not restored.
        assert!(!is_anti_idle(
            &json!({ "intervalSeconds": 120, "text": "a\rb\u{3}", "enter": false })
        ));
        assert!(!is_anti_idle(
            &json!({ "intervalSeconds": "120", "text": " ", "enter": false })
        ));
        assert!(!is_anti_idle(&json!([])));
    }
}
