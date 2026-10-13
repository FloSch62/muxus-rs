//! Login sequence editing helpers shared by the host and folder editors
//! (`client/src/login-sequence.ts`).

use muxus_core::api::{
    FolderSettingsRecord, LOGIN_SEQUENCE_DEFAULT_TIMEOUT_SECONDS, LOGIN_SEQUENCE_MAX_STEPS,
    LOGIN_SEQUENCE_MAX_TIMEOUT_SECONDS, LOGIN_SEQUENCE_TEXT_MAX_LENGTH, LoginSecretStep,
    LoginSendStep, LoginSequence, LoginSequenceStep, LoginWaitStep,
};
use muxus_core::folder_paths::{folder_chain, folder_path_key};
use muxus_core::js::utf16_len;
use serde_json::Value;

use super::matcher::compile_wait_pattern;

/// How a host or folder gets its login sequence: from the folder above it,
/// none at all (switching an inherited one off), or its own steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginSequenceMode {
    Inherit,
    None,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginSequenceDraft {
    pub mode: LoginSequenceMode,
    /// Kept while another mode is picked, so switching back loses nothing.
    pub steps: Vec<LoginSequenceStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStepKind {
    Wait,
    Send,
    Secret,
}

/// `newPreferenceId('step')`.
fn new_step_id() -> String {
    format!("step-{}", uuid::Uuid::new_v4())
}

pub fn new_login_step(kind: LoginStepKind) -> LoginSequenceStep {
    let id = new_step_id();
    match kind {
        LoginStepKind::Wait => LoginSequenceStep::Wait(LoginWaitStep {
            id,
            pattern: String::new(),
            regex: None,
            timeout_seconds: LOGIN_SEQUENCE_DEFAULT_TIMEOUT_SECONDS,
        }),
        LoginStepKind::Send => LoginSequenceStep::Send(LoginSendStep {
            id,
            text: String::new(),
            enter: true,
        }),
        LoginStepKind::Secret => LoginSequenceStep::Secret(LoginSecretStep {
            id,
            secret_id: String::new(),
            enter: true,
        }),
    }
}

pub fn login_step_kind(step: &LoginSequenceStep) -> LoginStepKind {
    match step {
        LoginSequenceStep::Wait(_) => LoginStepKind::Wait,
        LoginSequenceStep::Send(_) => LoginStepKind::Send,
        LoginSequenceStep::Secret(_) => LoginStepKind::Secret,
    }
}

/// A step switched to another kind keeps its id, and its text where that still fits.
pub fn change_login_step_kind(step: &LoginSequenceStep, kind: LoginStepKind) -> LoginSequenceStep {
    if login_step_kind(step) == kind {
        return step.clone();
    }
    let text = match step {
        LoginSequenceStep::Wait(wait) => wait.pattern.clone(),
        LoginSequenceStep::Send(send) => send.text.clone(),
        LoginSequenceStep::Secret(_) => String::new(),
    };
    let id = step.id().to_string();
    match new_login_step(kind) {
        LoginSequenceStep::Wait(wait) => LoginSequenceStep::Wait(LoginWaitStep {
            id,
            pattern: text,
            ..wait
        }),
        LoginSequenceStep::Send(send) => {
            LoginSequenceStep::Send(LoginSendStep { id, text, ..send })
        }
        LoginSequenceStep::Secret(secret) => {
            LoginSequenceStep::Secret(LoginSecretStep { id, ..secret })
        }
    }
}

pub fn login_sequence_draft(sequence: Option<&LoginSequence>) -> LoginSequenceDraft {
    match sequence {
        None => LoginSequenceDraft {
            mode: LoginSequenceMode::Inherit,
            steps: Vec::new(),
        },
        Some(sequence) => LoginSequenceDraft {
            mode: if sequence.steps.is_empty() {
                LoginSequenceMode::None
            } else {
                LoginSequenceMode::Custom
            },
            steps: sequence.steps.clone(),
        },
    }
}

/// The stored form: `None` inherits, an empty list is none.
pub fn login_sequence_from_draft(draft: &LoginSequenceDraft) -> Option<LoginSequence> {
    match draft.mode {
        LoginSequenceMode::Inherit => None,
        LoginSequenceMode::None => Some(LoginSequence { steps: Vec::new() }),
        LoginSequenceMode::Custom => Some(LoginSequence {
            steps: draft.steps.clone(),
        }),
    }
}

/// Moves a step up (-1) or down (1); out-of-range moves change nothing.
pub fn move_login_step(
    steps: &[LoginSequenceStep],
    index: usize,
    offset: isize,
) -> Vec<LoginSequenceStep> {
    let mut next = steps.to_vec();
    if let Some(target) = index.checked_add_signed(offset)
        && index < steps.len()
        && target < steps.len()
    {
        next.swap(index, target);
    }
    next
}

/// Why a regular expression cannot be used, or `None` when it compiles.
pub fn wait_pattern_error(pattern: &str) -> Option<String> {
    compile_wait_pattern(pattern, true)
        .err()
        .map(|reason| format!("Invalid regular expression: /{pattern}/: {reason}"))
}

/// What is wrong with one step, phrased for the field it belongs to.
pub fn login_step_problem(step: &LoginSequenceStep) -> Option<String> {
    match step {
        LoginSequenceStep::Wait(wait) => {
            if wait.pattern.is_empty() {
                return Some("Enter the text to wait for.".into());
            }
            if utf16_len(&wait.pattern) > LOGIN_SEQUENCE_TEXT_MAX_LENGTH {
                return Some("This text is too long.".into());
            }
            if wait.regex == Some(true)
                && let Some(error) = wait_pattern_error(&wait.pattern)
            {
                return Some(error);
            }
            if !(1..=LOGIN_SEQUENCE_MAX_TIMEOUT_SECONDS).contains(&wait.timeout_seconds) {
                return Some(format!(
                    "Wait between 1 and {LOGIN_SEQUENCE_MAX_TIMEOUT_SECONDS} seconds."
                ));
            }
            None
        }
        LoginSequenceStep::Send(send) => {
            if send.text.is_empty() && !send.enter {
                return Some("Enter text to send, or press Enter.".into());
            }
            if utf16_len(&send.text) > LOGIN_SEQUENCE_TEXT_MAX_LENGTH {
                return Some("This text is too long.".into());
            }
            None
        }
        LoginSequenceStep::Secret(secret) => secret
            .secret_id
            .is_empty()
            .then(|| "Choose a secret from the password vault.".into()),
    }
}

/// The first reason a draft cannot be saved, or `None`.
pub fn login_sequence_problem(draft: &LoginSequenceDraft) -> Option<String> {
    if draft.mode != LoginSequenceMode::Custom {
        return None;
    }
    if draft.steps.is_empty() {
        return Some("Add a login step, or choose no login sequence.".into());
    }
    if draft.steps.len() > LOGIN_SEQUENCE_MAX_STEPS {
        return Some(format!(
            "A login sequence can have up to {LOGIN_SEQUENCE_MAX_STEPS} steps."
        ));
    }
    draft.steps.iter().enumerate().find_map(|(index, step)| {
        login_step_problem(step)
            .map(|problem| format!("Login sequence step {}: {problem}", index + 1))
    })
}

/// A step without its id, with an unset regex flag read as false.
fn comparable_step(step: &LoginSequenceStep) -> (LoginStepKind, String, bool, u32, bool) {
    match step {
        LoginSequenceStep::Wait(wait) => (
            LoginStepKind::Wait,
            wait.pattern.clone(),
            wait.regex == Some(true),
            wait.timeout_seconds,
            false,
        ),
        LoginSequenceStep::Send(send) => {
            (LoginStepKind::Send, send.text.clone(), false, 0, send.enter)
        }
        LoginSequenceStep::Secret(secret) => (
            LoginStepKind::Secret,
            secret.secret_id.clone(),
            false,
            0,
            secret.enter,
        ),
    }
}

/// Whether two stored sequences do the same thing; step ids do not count.
pub fn same_login_sequence(a: Option<&LoginSequence>, b: Option<&LoginSequence>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.steps.len() == b.steps.len()
                && a.steps
                    .iter()
                    .zip(&b.steps)
                    .all(|(x, y)| comparable_step(x) == comparable_step(y))
        }
        _ => false,
    }
}

pub fn same_login_sequence_draft(a: &LoginSequenceDraft, b: &LoginSequenceDraft) -> bool {
    same_login_sequence(
        login_sequence_from_draft(a).as_ref(),
        login_sequence_from_draft(b).as_ref(),
    )
}

/// The sequence a host in `group` would take from its folders when it has
/// none of its own: the nearest folder that sets one wins, and an empty one
/// there means none. Returns the sequence and the folder it comes from.
pub fn inherited_login_sequence(
    folders: Option<&[FolderSettingsRecord]>,
    group: Option<&str>,
) -> Option<(LoginSequence, String)> {
    let folders = folders?;
    for candidate in folder_chain(group.unwrap_or("")) {
        let key = folder_path_key(&candidate);
        let folder = folders
            .iter()
            .find(|folder| folder_path_key(&folder.path) == key);
        if let Some(sequence) = folder.and_then(|folder| folder.login_sequence.as_ref()) {
            return (!sequence.steps.is_empty()).then(|| {
                (
                    sequence.clone(),
                    folder.map(|f| f.path.clone()).unwrap_or_default(),
                )
            });
        }
    }
    None
}

/// A step as one line of text, as the editors and the tab describe it.
pub fn login_step_summary(step: &LoginSequenceStep, secret_name: Option<&str>) -> String {
    match step {
        LoginSequenceStep::Wait(wait) => {
            if wait.regex == Some(true) {
                format!("Wait for a match of /{}/", wait.pattern)
            } else {
                format!("Wait for “{}”", wait.pattern)
            }
        }
        LoginSequenceStep::Send(send) => {
            if send.text.is_empty() {
                return "Press Enter".into();
            }
            format!(
                "Send “{}”{}",
                send.text,
                if send.enter { " and Enter" } else { "" }
            )
        }
        LoginSequenceStep::Secret(secret) => format!(
            "Send the secret {}{}",
            secret_name.map_or("from the vault".to_string(), |name| format!("“{name}”")),
            if secret.enter { " and Enter" } else { "" }
        ),
    }
}

/// Shape check for sequences read from a backup; the backend validates the values again.
pub fn is_login_sequence(value: &Value) -> bool {
    let Some(steps) = value
        .as_object()
        .and_then(|object| object.get("steps"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    steps.len() <= LOGIN_SEQUENCE_MAX_STEPS
        && steps.iter().all(|step| {
            is_login_step(step)
                && serde_json::from_value::<LoginSequenceStep>(step.clone())
                    .is_ok_and(|step| login_step_problem(&step).is_none())
        })
}

fn is_login_step(value: &Value) -> bool {
    let Some(step) = value.as_object() else {
        return false;
    };
    let id_ok = step
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty() && utf16_len(id) <= 100);
    if !id_ok {
        return false;
    }
    let is_bool = |key: &str| step.get(key).is_some_and(Value::is_boolean);
    let is_string = |key: &str| step.get(key).is_some_and(Value::is_string);
    match step.get("kind").and_then(Value::as_str) {
        Some("wait") => {
            is_string("pattern")
                && step.get("regex").is_none_or(Value::is_boolean)
                && step.get("timeoutSeconds").is_some_and(Value::is_number)
        }
        Some("send") => is_string("text") && is_bool("enter"),
        Some("secret") => {
            step.get("secretId")
                .and_then(Value::as_str)
                .is_some_and(|id| utf16_len(id) <= 200)
                && is_bool("enter")
        }
        _ => false,
    }
}

/// Where a login sequence is set: a host or a folder, by the name the user knows it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginSequenceOwner {
    pub label: String,
    pub login_sequence: Option<LoginSequence>,
}

/// The hosts and folders whose own login sequence types this vault secret.
pub fn login_sequences_using_secret(owners: &[LoginSequenceOwner], secret_id: &str) -> Vec<String> {
    owners
        .iter()
        .filter(|owner| {
            owner.login_sequence.as_ref().is_some_and(|sequence| {
                sequence
                    .steps
                    .iter()
                    .any(|step| matches!(step, LoginSequenceStep::Secret(secret) if secret.secret_id == secret_id))
            })
        })
        .map(|owner| owner.label.clone())
        .collect()
}
