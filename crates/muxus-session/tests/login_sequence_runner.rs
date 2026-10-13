//! Port of `tests/unit/server/login-sequence-runner.test.ts`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use muxus_core::api::{LoginSecretStep, LoginSendStep, LoginSequenceStep, LoginWaitStep};
use muxus_core::protocol::LoginSequenceState;
use muxus_session::login_sequence::{
    AbortSignal, LoginSequenceIo, LoginSequenceOutcome, LoginSequenceProgress, LoginSequenceRunner,
    describe_login_step,
};

type SecretFn = Arc<dyn Fn(AbortSignal) -> futures_lite_box::BoxFuture + Send + Sync>;

/// A boxed future without pulling in a futures crate.
mod futures_lite_box {
    pub type BoxFuture =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
}

#[derive(Default)]
struct Record {
    written: Vec<String>,
    typed: Vec<String>,
    progress: Vec<LoginSequenceProgress>,
}

struct Harness {
    record: Arc<Mutex<Record>>,
    writable: bool,
    secret: Option<SecretFn>,
}

#[async_trait]
impl LoginSequenceIo for Harness {
    fn write(&self, data: &[u8]) -> bool {
        if !self.writable {
            return false;
        }
        self.record
            .lock()
            .unwrap()
            .written
            .push(String::from_utf8_lossy(data).into_owned());
        true
    }

    fn typed(&self, data: &[u8]) {
        self.record
            .lock()
            .unwrap()
            .typed
            .push(String::from_utf8_lossy(data).into_owned());
    }

    async fn type_secret(
        &self,
        _secret_id: &str,
        enter: bool,
        abort: AbortSignal,
    ) -> Result<(), String> {
        match &self.secret {
            Some(secret) => secret(abort).await,
            None => {
                self.record
                    .lock()
                    .unwrap()
                    .written
                    .push(format!("<secret>{}", if enter { "\r" } else { "" }));
                Ok(())
            }
        }
    }

    fn progress(&self, update: LoginSequenceProgress) {
        self.record.lock().unwrap().progress.push(update);
    }
}

fn harness(
    steps: Vec<LoginSequenceStep>,
    writable: bool,
    secret: Option<SecretFn>,
) -> (Arc<LoginSequenceRunner>, Arc<Mutex<Record>>) {
    let record = Arc::new(Mutex::new(Record::default()));
    let io = Arc::new(Harness {
        record: record.clone(),
        writable,
        secret,
    });
    let runner = LoginSequenceRunner::new(
        steps,
        io,
        Some(Arc::new(|_: &str| Some("Core enable".to_string()))),
        None,
    );
    (Arc::new(runner), record)
}

fn wait(pattern: &str) -> LoginSequenceStep {
    LoginSequenceStep::Wait(LoginWaitStep {
        id: format!("w-{pattern}"),
        pattern: pattern.into(),
        regex: None,
        timeout_seconds: 5,
    })
}

fn wait_regex(pattern: &str) -> LoginSequenceStep {
    LoginSequenceStep::Wait(LoginWaitStep {
        id: format!("w-{pattern}"),
        pattern: pattern.into(),
        regex: Some(true),
        timeout_seconds: 5,
    })
}

fn wait_timeout(pattern: &str, timeout_seconds: u32) -> LoginSequenceStep {
    LoginSequenceStep::Wait(LoginWaitStep {
        id: format!("w-{pattern}"),
        pattern: pattern.into(),
        regex: None,
        timeout_seconds,
    })
}

fn send(text: &str, enter: bool) -> LoginSequenceStep {
    LoginSequenceStep::Send(LoginSendStep {
        id: format!("s-{text}"),
        text: text.into(),
        enter,
    })
}

fn secret(enter: bool) -> LoginSequenceStep {
    LoginSequenceStep::Secret(LoginSecretStep {
        id: "secret".into(),
        secret_id: "sec-1".into(),
        enter,
    })
}

async fn until(record: &Arc<Mutex<Record>>, check: impl Fn(&Record) -> bool) {
    for _ in 0..500 {
        if check(&record.lock().unwrap()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never held");
}

fn spawn(runner: &Arc<LoginSequenceRunner>) -> tokio::task::JoinHandle<LoginSequenceOutcome> {
    let runner = runner.clone();
    tokio::spawn(async move { runner.run().await })
}

#[tokio::test]
async fn waits_for_each_prompt_before_answering_it_in_order() {
    let (runner, record) = harness(
        vec![
            wait("Press RETURN"),
            send("", true),
            wait("Username:"),
            send("admin", true),
            wait("assword:"),
            secret(true),
        ],
        true,
        None,
    );
    let done = spawn(&runner);
    until(&record, |r| {
        r.progress
            .last()
            .is_some_and(|p| p.detail == "Waiting for “Press RETURN”")
    })
    .await;
    assert!(record.lock().unwrap().written.is_empty());

    runner.output(b"Welcome\r\nPress RE");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(record.lock().unwrap().written.is_empty());
    runner.output(b"TURN to get started");
    until(&record, |r| r.written == ["\r"]).await;

    runner.output(b"\r\nUsername: ");
    until(&record, |r| r.written == ["\r", "admin\r"]).await;
    runner.output(b"admin\r\nPassword: ");

    assert_eq!(done.await.unwrap(), LoginSequenceOutcome::Done);
    let record = record.lock().unwrap();
    assert_eq!(record.written, ["\r", "admin\r", "<secret>\r"]);
    // Plain sends count as typed input; the secret never reaches that path.
    assert_eq!(record.typed, ["\r", "admin\r"]);
    let states: Vec<String> = record
        .progress
        .iter()
        .map(|p| {
            format!(
                "{} {}/{}",
                serde_json::to_value(p.state).unwrap().as_str().unwrap(),
                p.step,
                p.steps
            )
        })
        .collect();
    assert_eq!(
        states,
        [
            "running 1/6",
            "running 2/6",
            "running 3/6",
            "running 4/6",
            "running 5/6",
            "running 6/6",
            "done 6/6"
        ]
    );
}

#[tokio::test]
async fn counts_output_that_arrived_before_the_sequence_started() {
    let (runner, record) = harness(vec![wait("login:"), send("root", true)], true, None);
    runner.output(b"\x1b[1mlogin:\x1b[0m ");
    assert_eq!(runner.run().await, LoginSequenceOutcome::Done);
    assert_eq!(record.lock().unwrap().written, ["root\r"]);
}

#[tokio::test]
async fn matches_regular_expressions() {
    let (runner, record) = harness(
        vec![wait_regex(r"[>#]\s*$"), send("enable", true)],
        true,
        None,
    );
    let done = spawn(&runner);
    runner.output(b"router> ");
    assert_eq!(done.await.unwrap(), LoginSequenceOutcome::Done);
    assert_eq!(record.lock().unwrap().written, ["enable\r"]);
}

#[tokio::test(start_paused = true)]
async fn stops_at_a_step_that_times_out_and_says_which_one() {
    let (runner, record) = harness(
        vec![
            wait("login:"),
            send("admin", true),
            wait_timeout("Password:", 3),
            send("never", true),
        ],
        true,
        None,
    );
    let done = spawn(&runner);
    runner.output(b"login: ");
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(record.lock().unwrap().written, ["admin\r"]);
    runner.output(b"Passcode: ");
    tokio::time::advance(Duration::from_millis(2999)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        record.lock().unwrap().progress.last().unwrap().state,
        LoginSequenceState::Running
    );
    tokio::time::advance(Duration::from_millis(1)).await;

    let message = "Step 3 timed out after 3 s waiting for “Password:”.".to_string();
    assert_eq!(
        done.await.unwrap(),
        LoginSequenceOutcome::Failed {
            step: 3,
            message: message.clone()
        }
    );
    let record = record.lock().unwrap();
    let last = record.progress.last().unwrap();
    assert_eq!(
        (last.state, last.step, last.steps, last.message.clone()),
        (LoginSequenceState::Failed, 3, 4, Some(message))
    );
    assert_eq!(record.written, ["admin\r"]);
}

#[tokio::test]
async fn reports_an_invalid_pattern_instead_of_throwing() {
    let (runner, _record) = harness(vec![wait_regex("([")], true, None);
    match runner.run().await {
        LoginSequenceOutcome::Failed { step: 1, message } => {
            assert!(
                message.contains("Step 1 has an invalid pattern"),
                "{message}"
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn can_be_cancelled_while_it_waits() {
    let (runner, record) = harness(
        vec![send("first", true), wait("never"), send("late", true)],
        true,
        None,
    );
    let done = spawn(&runner);
    until(&record, |r| r.progress.last().is_some_and(|p| p.step == 2)).await;
    runner.cancel();
    assert_eq!(
        done.await.unwrap(),
        LoginSequenceOutcome::Cancelled { step: 2 }
    );
    let last = record.lock().unwrap().progress.last().unwrap().clone();
    assert_eq!(
        (last.state, last.step, last.steps),
        (LoginSequenceState::Cancelled, 2, 3)
    );
    runner.output(b"never");
    assert_eq!(record.lock().unwrap().written, ["first\r"]);
}

#[tokio::test]
async fn cancels_a_vault_step_that_is_waiting_for_the_master_password() {
    let seen: Arc<Mutex<Option<AbortSignal>>> = Arc::default();
    let slot = seen.clone();
    let secret_fn: SecretFn = Arc::new(move |abort: AbortSignal| {
        *slot.lock().unwrap() = Some(abort.clone());
        Box::pin(async move {
            abort.aborted().await;
            Ok(())
        })
    });
    let (runner, record) = harness(
        vec![secret(true), send("after", true)],
        true,
        Some(secret_fn),
    );
    let done = spawn(&runner);
    for _ in 0..500 {
        if seen.lock().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    runner.cancel();
    assert_eq!(
        done.await.unwrap(),
        LoginSequenceOutcome::Cancelled { step: 1 }
    );
    assert!(seen.lock().unwrap().as_ref().unwrap().is_aborted());
    assert!(record.lock().unwrap().written.is_empty());
}

#[tokio::test]
async fn stops_when_a_vault_secret_cannot_be_typed() {
    let secret_fn: SecretFn = Arc::new(|_| {
        Box::pin(async { Err("its secret is no longer in the password vault.".to_string()) })
    });
    let (runner, record) = harness(
        vec![secret(true), send("after", true)],
        true,
        Some(secret_fn),
    );
    assert_eq!(
        runner.run().await,
        LoginSequenceOutcome::Failed {
            step: 1,
            message: "Step 1: its secret is no longer in the password vault.".into()
        }
    );
    let record = record.lock().unwrap();
    assert!(record.written.is_empty());
    assert_eq!(
        record.progress.last().unwrap().state,
        LoginSequenceState::Failed
    );
}

#[tokio::test]
async fn ends_quietly_when_the_session_closes() {
    let (runner, record) = harness(vec![wait("never")], true, None);
    let done = spawn(&runner);
    until(&record, |r| !r.progress.is_empty()).await;
    runner.close();
    assert_eq!(done.await.unwrap(), LoginSequenceOutcome::Closed);
    let states: Vec<LoginSequenceState> = record
        .lock()
        .unwrap()
        .progress
        .iter()
        .map(|p| p.state)
        .collect();
    assert_eq!(states, [LoginSequenceState::Running]);

    let (gone, _) = harness(vec![send("x", true)], false, None);
    assert_eq!(gone.run().await, LoginSequenceOutcome::Closed);
}

#[test]
fn describes_steps_without_showing_secrets() {
    assert_eq!(
        describe_login_step(&wait("Password:"), None),
        "Waiting for “Password:”"
    );
    assert_eq!(
        describe_login_step(&wait_regex("[>#]$"), None),
        "Waiting for a match of “[>#]$”"
    );
    assert_eq!(
        describe_login_step(&send("terminal length 0", true), None),
        "Typing “terminal length 0” and Enter"
    );
    assert_eq!(describe_login_step(&send("", true), None), "Pressing Enter");
    assert_eq!(
        describe_login_step(&secret(true), Some("Core enable")),
        "Typing the secret “Core enable” and Enter"
    );
    let long = describe_login_step(&send(&"x".repeat(200), false), None);
    assert_eq!(long, format!("Typing “{}…”", "x".repeat(59)));
    assert_eq!(
        describe_login_step(&send("a\r\n\tb", false), None),
        "Typing “a b”"
    );
}
