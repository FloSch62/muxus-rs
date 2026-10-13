//! Port of `tests/unit/client/login-sequence.test.ts`.

use muxus_core::api::{
    FolderAuthSettings, FolderSettingsRecord, LoginSecretStep, LoginSendStep, LoginSequence,
    LoginSequenceStep, LoginWaitStep,
};
use muxus_session::login_sequence::editor::{
    LoginSequenceDraft, LoginSequenceMode, LoginSequenceOwner, LoginStepKind,
    change_login_step_kind, inherited_login_sequence, is_login_sequence, login_sequence_draft,
    login_sequence_from_draft, login_sequence_problem, login_sequences_using_secret,
    login_step_summary, move_login_step, new_login_step, same_login_sequence,
};
use serde_json::json;

fn wait() -> LoginWaitStep {
    LoginWaitStep {
        id: "w".into(),
        pattern: "Password:".into(),
        regex: None,
        timeout_seconds: 10,
    }
}

fn send() -> LoginSendStep {
    LoginSendStep {
        id: "s".into(),
        text: "enable".into(),
        enter: true,
    }
}

fn secret() -> LoginSecretStep {
    LoginSecretStep {
        id: "k".into(),
        secret_id: "vault-1".into(),
        enter: true,
    }
}

fn w(step: LoginWaitStep) -> LoginSequenceStep {
    LoginSequenceStep::Wait(step)
}

fn s(step: LoginSendStep) -> LoginSequenceStep {
    LoginSequenceStep::Send(step)
}

fn k(step: LoginSecretStep) -> LoginSequenceStep {
    LoginSequenceStep::Secret(step)
}

fn sequence(steps: Vec<LoginSequenceStep>) -> LoginSequence {
    LoginSequence { steps }
}

fn folder(path: &str, login_sequence: Option<LoginSequence>) -> FolderSettingsRecord {
    FolderSettingsRecord {
        id: path.into(),
        path: path.into(),
        auth: serde_json::from_value::<FolderAuthSettings>(json!({})).unwrap(),
        has_password: false,
        login_sequence,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    }
}

fn ids(steps: &[LoginSequenceStep]) -> Vec<&str> {
    steps.iter().map(LoginSequenceStep::id).collect()
}

#[test]
fn maps_the_stored_form_to_a_mode_and_back() {
    assert_eq!(
        login_sequence_draft(None),
        LoginSequenceDraft {
            mode: LoginSequenceMode::Inherit,
            steps: vec![]
        }
    );
    assert_eq!(
        login_sequence_draft(Some(&sequence(vec![]))),
        LoginSequenceDraft {
            mode: LoginSequenceMode::None,
            steps: vec![]
        }
    );
    assert_eq!(
        login_sequence_draft(Some(&sequence(vec![w(wait()), s(send())]))),
        LoginSequenceDraft {
            mode: LoginSequenceMode::Custom,
            steps: vec![w(wait()), s(send())]
        }
    );
    let draft = |mode| LoginSequenceDraft {
        mode,
        steps: vec![w(wait())],
    };
    assert_eq!(
        login_sequence_from_draft(&draft(LoginSequenceMode::Inherit)),
        None
    );
    assert_eq!(
        login_sequence_from_draft(&draft(LoginSequenceMode::None)),
        Some(sequence(vec![]))
    );
    assert_eq!(
        login_sequence_from_draft(&draft(LoginSequenceMode::Custom)),
        Some(sequence(vec![w(wait())]))
    );
}

#[test]
fn starts_new_steps_with_sensible_defaults_and_unique_ids() {
    let (a, b) = (
        new_login_step(LoginStepKind::Wait),
        new_login_step(LoginStepKind::Wait),
    );
    let LoginSequenceStep::Wait(first) = &a else {
        panic!()
    };
    assert_eq!((first.pattern.as_str(), first.timeout_seconds), ("", 10));
    assert!(first.id.starts_with("step-"));
    assert_ne!(a.id(), b.id());
    assert!(
        matches!(new_login_step(LoginStepKind::Send), LoginSequenceStep::Send(LoginSendStep { ref text, enter: true, .. }) if text.is_empty())
    );
    assert!(
        matches!(new_login_step(LoginStepKind::Secret), LoginSequenceStep::Secret(LoginSecretStep { ref secret_id, enter: true, .. }) if secret_id.is_empty())
    );
}

#[test]
fn keeps_the_id_and_the_text_when_a_step_changes_kind() {
    assert!(
        matches!(change_login_step_kind(&w(wait()), LoginStepKind::Send),
        LoginSequenceStep::Send(LoginSendStep { ref id, ref text, .. }) if id == "w" && text == "Password:")
    );
    assert!(
        matches!(change_login_step_kind(&s(send()), LoginStepKind::Wait),
        LoginSequenceStep::Wait(LoginWaitStep { ref id, ref pattern, .. }) if id == "s" && pattern == "enable")
    );
    assert!(
        matches!(change_login_step_kind(&s(send()), LoginStepKind::Secret),
        LoginSequenceStep::Secret(LoginSecretStep { ref id, ref secret_id, .. }) if id == "s" && secret_id.is_empty())
    );
    assert_eq!(
        change_login_step_kind(&s(send()), LoginStepKind::Send),
        s(send())
    );
}

#[test]
fn reorders_steps_and_ignores_moves_past_either_end() {
    let three = [w(wait()), s(send()), k(secret())];
    assert_eq!(ids(&move_login_step(&three, 0, 1)), ["s", "w", "k"]);
    assert_eq!(ids(&move_login_step(&three, 2, -1)), ["w", "k", "s"]);
    let two = [w(wait()), s(send())];
    assert_eq!(ids(&move_login_step(&two, 0, -1)), ["w", "s"]);
    assert_eq!(ids(&move_login_step(&two, 1, 1)), ["w", "s"]);
}

#[test]
fn names_the_first_problem_of_a_custom_draft() {
    let problem = |step: LoginSequenceStep| {
        login_sequence_problem(&LoginSequenceDraft {
            mode: LoginSequenceMode::Custom,
            steps: vec![step],
        })
    };
    assert_eq!(problem(w(wait())), None);
    assert_eq!(
        problem(w(LoginWaitStep {
            pattern: String::new(),
            ..wait()
        }))
        .as_deref(),
        Some("Login sequence step 1: Enter the text to wait for.")
    );
    assert!(
        problem(w(LoginWaitStep {
            pattern: "(".into(),
            regex: Some(true),
            ..wait()
        }))
        .unwrap()
        .contains("Invalid regular expression")
    );
    // Literal text is never compiled, so regex syntax there is fine.
    assert_eq!(
        problem(w(LoginWaitStep {
            pattern: "(".into(),
            ..wait()
        })),
        None
    );
    for timeout_seconds in [0, 3601] {
        assert_eq!(
            problem(w(LoginWaitStep {
                timeout_seconds,
                ..wait()
            }))
            .as_deref(),
            Some("Login sequence step 1: Wait between 1 and 3600 seconds.")
        );
    }
    assert_eq!(
        problem(s(LoginSendStep {
            text: String::new(),
            enter: true,
            ..send()
        })),
        None
    );
    assert_eq!(
        problem(s(LoginSendStep {
            text: String::new(),
            enter: false,
            ..send()
        }))
        .as_deref(),
        Some("Login sequence step 1: Enter text to send, or press Enter.")
    );
    assert_eq!(
        problem(k(LoginSecretStep {
            secret_id: String::new(),
            ..secret()
        }))
        .as_deref(),
        Some("Login sequence step 1: Choose a secret from the password vault.")
    );
}

#[test]
fn only_checks_a_draft_that_runs_its_own_steps() {
    let broken = vec![w(LoginWaitStep {
        pattern: String::new(),
        ..wait()
    })];
    for mode in [LoginSequenceMode::Inherit, LoginSequenceMode::None] {
        assert_eq!(
            login_sequence_problem(&LoginSequenceDraft {
                mode,
                steps: broken.clone()
            }),
            None
        );
    }
    let empty = login_sequence_problem(&LoginSequenceDraft {
        mode: LoginSequenceMode::Custom,
        steps: vec![],
    });
    assert!(empty.unwrap().contains("Add a login step"));
    let many = (0..33)
        .map(|_| new_login_step(LoginStepKind::Send))
        .collect();
    let too_many = login_sequence_problem(&LoginSequenceDraft {
        mode: LoginSequenceMode::Custom,
        steps: many,
    });
    assert!(too_many.unwrap().contains("up to 32 steps"));
}

#[test]
fn ignores_step_ids_and_an_unset_regex_flag() {
    let other = w(LoginWaitStep {
        id: "other".into(),
        regex: Some(false),
        ..wait()
    });
    assert!(same_login_sequence(
        Some(&sequence(vec![w(wait())])),
        Some(&sequence(vec![other]))
    ));
    let different = w(LoginWaitStep {
        pattern: "login:".into(),
        ..wait()
    });
    assert!(!same_login_sequence(
        Some(&sequence(vec![w(wait())])),
        Some(&sequence(vec![different]))
    ));
    assert!(same_login_sequence(None, None));
    assert!(!same_login_sequence(None, Some(&sequence(vec![]))));
}

fn folders() -> Vec<FolderSettingsRecord> {
    vec![
        folder("Network", Some(sequence(vec![s(send())]))),
        folder("network/consoles", Some(sequence(vec![w(wait())]))),
        folder("Network/Lab", Some(sequence(vec![]))),
        folder("Servers", None),
    ]
}

#[test]
fn takes_the_nearest_folder_that_sets_a_sequence() {
    let folders = folders();
    assert_eq!(
        inherited_login_sequence(Some(&folders), Some("Network/Consoles")),
        Some((sequence(vec![w(wait())]), "network/consoles".into()))
    );
    assert_eq!(
        inherited_login_sequence(Some(&folders), Some("Network / EU / Edge")),
        Some((sequence(vec![s(send())]), "Network".into()))
    );
}

#[test]
fn stops_at_a_folder_that_switches_it_off_and_finds_nothing_outside() {
    let folders = folders();
    assert_eq!(
        inherited_login_sequence(Some(&folders), Some("Network/Lab/Bench")),
        None
    );
    assert_eq!(
        inherited_login_sequence(Some(&folders), Some("Servers")),
        None
    );
    assert_eq!(inherited_login_sequence(Some(&folders), Some("")), None);
    assert_eq!(inherited_login_sequence(None, Some("Network")), None);
}

#[test]
fn accepts_well_formed_sequences_only() {
    let all = json!({ "steps": [
        { "id": "w", "kind": "wait", "pattern": "Password:", "timeoutSeconds": 10 },
        { "id": "s", "kind": "send", "text": "enable", "enter": true },
        { "id": "k", "kind": "secret", "secretId": "vault-1", "enter": true },
    ]});
    assert!(is_login_sequence(&all));
    assert!(is_login_sequence(&json!({ "steps": [] })));
    assert!(!is_login_sequence(&json!(null)));
    assert!(!is_login_sequence(&json!({ "steps": "nope" })));
    assert!(!is_login_sequence(
        &json!({ "steps": [{ "id": "w", "kind": "type", "pattern": "x", "timeoutSeconds": 10 }] })
    ));
    assert!(!is_login_sequence(
        &json!({ "steps": [{ "id": "w", "kind": "wait", "pattern": "(", "regex": true, "timeoutSeconds": 10 }] })
    ));
    assert!(!is_login_sequence(
        &json!({ "steps": [{ "id": "", "kind": "send", "text": "enable", "enter": true }] })
    ));
}

#[test]
fn describes_each_kind_of_step() {
    assert_eq!(login_step_summary(&w(wait()), None), "Wait for “Password:”");
    assert_eq!(
        login_step_summary(
            &w(LoginWaitStep {
                pattern: r"#\s*$".into(),
                regex: Some(true),
                ..wait()
            }),
            None
        ),
        r"Wait for a match of /#\s*$/"
    );
    assert_eq!(
        login_step_summary(&s(send()), None),
        "Send “enable” and Enter"
    );
    assert_eq!(
        login_step_summary(
            &s(LoginSendStep {
                text: String::new(),
                ..send()
            }),
            None
        ),
        "Press Enter"
    );
    assert_eq!(
        login_step_summary(&k(secret()), Some("Core enable")),
        "Send the secret “Core enable” and Enter"
    );
    assert_eq!(
        login_step_summary(
            &k(LoginSecretStep {
                enter: false,
                ..secret()
            }),
            None
        ),
        "Send the secret from the vault"
    );
}

#[test]
fn names_the_hosts_and_folders_whose_own_sequence_types_the_secret() {
    let owner = |label: &str, steps: Option<Vec<LoginSequenceStep>>| LoginSequenceOwner {
        label: label.into(),
        login_sequence: steps.map(sequence),
    };
    let owners = [
        owner("core-sw1", Some(vec![w(wait()), k(secret())])),
        owner(
            "Network",
            Some(vec![k(LoginSecretStep {
                secret_id: "other".into(),
                ..secret()
            })]),
        ),
        owner("web-01", None),
        owner("Lab", Some(vec![k(secret())])),
    ];
    assert_eq!(
        login_sequences_using_secret(&owners, "vault-1"),
        ["core-sw1", "Lab"]
    );
}
