//! Port of `tests/unit/server/login-sequence-session.test.ts`: a login
//! sequence on a live Telnet session against a fake console server, with the
//! session recorder and a log file attached. The TypeScript test drove the
//! whole backend over its WebSocket; here the parts are wired by hand the way
//! the session layer wires them.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use muxus_core::api::{
    LoginSecretStep, LoginSendStep, LoginSequence, LoginSequenceStep, LoginWaitStep,
    SessionEndStatus, SessionLogFileSettings, SessionLoggingPolicy,
};
use muxus_core::protocol::{
    AuthPromptPurpose, AuthPromptResponse, LoginSequenceState, TelnetProfile,
    TerminalClientMessage, TerminalProfile, TerminalServerMessage,
};
use muxus_core::stream::{TerminalSize, TransportEvent, TransportInput};
use muxus_session::logging::{
    MemoryHistory, RecorderConfig, SessionRecorder, SystemClock, session_profile_identity,
};
use muxus_session::login_sequence::{
    HostLoginSequence, LoginSequenceHost, LoginSequenceSession, LoginSequenceSessionOptions,
    LoginSequenceSource, SecretVault, VaultError, resolve_login_sequence,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MASTER: &str = "master-pass-12";
const SECRET: &str = "enable-Secret-4711";
const SECRET_ID: &str = "vault-secret-1";

/// A console server behind Telnet: a banner that must be answered with
/// RETURN, a user name, then a password prompt that comes a beat later and,
/// like a real one, does not echo. Telnet negotiation from the client is ignored.
async fn start_fake_console() -> (u16, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(Mutex::new(0));
    let counter = connections.clone();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            *counter.lock().unwrap() += 1;
            tokio::spawn(console(socket));
        }
    });
    (port, connections)
}

fn digest(text: &str) -> String {
    // A stand-in for the TypeScript's sha256 prefix: proves the line arrived
    // whole without echoing it.
    format!(
        "{:016x}",
        text.bytes()
            .fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(byte))
                .wrapping_mul(0x100000001b3))
    )
}

async fn console(socket: tokio::net::TcpStream) {
    let (mut reader, writer) = socket.into_split();
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let send = move |text: String| {
        let writer = writer.clone();
        async move {
            let _ = writer.lock().await.write_all(text.as_bytes()).await;
        }
    };
    send("\x1b[1mcore-sw1 console\x1b[0m\r\nPress RET".into()).await;
    {
        let send = send.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            send("URN to get started.".into()).await;
        });
    }
    #[derive(PartialEq)]
    enum Stage {
        Banner,
        User,
        Password,
        Shell,
    }
    let mut stage = Stage::Banner;
    let mut line = String::new();
    let mut iac = 0;
    let mut buffer = [0u8; 1024];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        for &byte in &buffer[..read] {
            // IAC WILL/WONT/DO/DONT <option> and IAC SB ... IAC SE.
            if iac > 0 {
                iac = match iac {
                    1 if byte == 250 => 3,
                    1 if byte >= 251 => 2,
                    3 if byte != 240 => 3,
                    _ => 0,
                };
                continue;
            }
            if byte == 255 {
                iac = 1;
                continue;
            }
            if byte == 0 || byte == 10 {
                continue;
            }
            if byte != 13 {
                line.push(char::from(byte));
                if stage != Stage::Password {
                    send(char::from(byte).to_string()).await;
                }
                continue;
            }
            let entered = std::mem::take(&mut line);
            match stage {
                Stage::Banner => {
                    stage = Stage::User;
                    send("\r\n\r\nUsername: ".into()).await;
                }
                Stage::User => {
                    stage = Stage::Password;
                    send("\r\n".into()).await;
                    let send = send.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(60)).await;
                        send("Pass".into()).await;
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        send("word: ".into()).await;
                    });
                }
                Stage::Password => {
                    if entered == SECRET {
                        stage = Stage::Shell;
                        send("\r\ncore-sw1>".into()).await;
                    } else {
                        send("\r\n% Access denied\r\nPassword: ".into()).await;
                    }
                }
                Stage::Shell => {
                    send(format!("\r\nreceived {}\r\ncore-sw1>", digest(&entered))).await
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum UnlockPolicy {
    /// The vault hands out secrets without asking.
    Never,
    /// Every use needs the master password.
    Credential,
}

struct FakeVault(UnlockPolicy);

#[async_trait]
impl SecretVault for FakeVault {
    fn secret_name(&self, secret_id: &str) -> Option<String> {
        (secret_id == SECRET_ID).then(|| "Console password".into())
    }

    async fn secret_value(
        &self,
        secret_id: &str,
        master_password: Option<&str>,
    ) -> Result<Option<String>, VaultError> {
        if secret_id != SECRET_ID {
            return Ok(None);
        }
        match (self.0, master_password) {
            (UnlockPolicy::Never, _) | (_, Some(MASTER)) => Ok(Some(SECRET.into())),
            (UnlockPolicy::Credential, None) => Err(VaultError::UnlockRequired),
            (UnlockPolicy::Credential, Some(_)) => Err(VaultError::InvalidMasterPassword(
                "The master password is incorrect.".into(),
            )),
        }
    }
}

fn wait(id: &str, pattern: &str, regex: bool, timeout_seconds: u32) -> LoginSequenceStep {
    LoginSequenceStep::Wait(LoginWaitStep {
        id: id.into(),
        pattern: pattern.into(),
        regex: regex.then_some(true),
        timeout_seconds,
    })
}

fn send(id: &str, text: &str) -> LoginSequenceStep {
    LoginSequenceStep::Send(LoginSendStep {
        id: id.into(),
        text: text.into(),
        enter: true,
    })
}

fn login_steps() -> LoginSequence {
    LoginSequence {
        steps: vec![
            wait("1", "Press RETURN", false, 5),
            send("2", ""),
            wait("3", "Username:", false, 5),
            send("4", "admin"),
            // The prompt arrives in two chunks, and only after a pause.
            wait("5", r"[Pp]assword:\s*$", true, 5),
            LoginSequenceStep::Secret(LoginSecretStep {
                id: "6".into(),
                secret_id: SECRET_ID.into(),
                enter: true,
            }),
        ],
    }
}

struct Live {
    input: async_channel::Sender<TransportInput>,
    session: Arc<LoginSequenceSession>,
    recorder: SessionRecorder,
    messages: Arc<Mutex<Vec<TerminalServerMessage>>>,
    output: Arc<Mutex<Vec<u8>>>,
}

impl Live {
    fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    fn progress(&self) -> Vec<(LoginSequenceState, u32, u32, String, Option<String>)> {
        self.messages
            .lock()
            .unwrap()
            .iter()
            .filter_map(|message| match message {
                TerminalServerMessage::LoginSequence {
                    state,
                    step,
                    steps,
                    detail,
                    message,
                } => Some((*state, *step, *steps, detail.clone(), message.clone())),
                _ => None,
            })
            .collect()
    }

    fn prompts(&self) -> Vec<TerminalServerMessage> {
        self.messages
            .lock()
            .unwrap()
            .iter()
            .filter(|message| matches!(message, TerminalServerMessage::AuthPrompt(_)))
            .cloned()
            .collect()
    }

    /// The user types: the recorder sees it, then the transport.
    async fn type_text(&self, text: &str) {
        self.recorder.input(text.as_bytes());
        self.input
            .send(TransportInput::Data(text.as_bytes().to_vec()))
            .await
            .unwrap();
    }

    async fn until(&self, what: &str, check: impl Fn(&Live) -> bool) {
        for _ in 0..1000 {
            if check(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "{what} never happened; output {:?}, progress {:?}",
            self.output(),
            self.progress()
        );
    }

    async fn close(self) {
        self.session.close();
        self.recorder.end(SessionEndStatus::Disconnected);
        let _ = self.input.send(TransportInput::Close).await;
    }
}

async fn connect(
    port: u16,
    sequence: Option<LoginSequence>,
    vault: UnlockPolicy,
    history: &MemoryHistory,
    log_directory: PathBuf,
) -> Live {
    let channels = muxus_telnet::connect(
        &TelnetProfile {
            profile_id: None,
            host: "127.0.0.1".into(),
            port,
        },
        TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        },
    )
    .await
    .unwrap();
    let profile = TerminalProfile::Telnet(TelnetProfile {
        profile_id: Some("core-sw1".into()),
        host: "127.0.0.1".into(),
        port,
    });
    let directory = log_directory.to_string_lossy().into_owned();
    let recorder = SessionRecorder::start(RecorderConfig {
        history: Arc::new(history.clone()),
        policy: SessionLoggingPolicy {
            profile_key: "profile:core-sw1".into(),
            enabled: true,
            capture_input: true,
            max_part_bytes: 1024 * 1024,
            max_parts: 2,
            log_to_file: true,
            overridden: true,
        },
        file_settings: Arc::new(move || SessionLogFileSettings {
            directory: Some(directory.clone()),
            filename_pattern: "{title}.log".into(),
            timestamps: false,
        }),
        home: PathBuf::from("/nonexistent/home"),
        identity: session_profile_identity(&profile),
        title: Some("core-sw1".into()),
        clock: Arc::new(SystemClock),
    });
    let messages: Arc<Mutex<Vec<TerminalServerMessage>>> = Arc::default();
    let sink = messages.clone();
    let writer = channels.input.clone();
    let session = Arc::new(LoginSequenceSession::new(LoginSequenceSessionOptions {
        sequence: sequence.unwrap_or_default(),
        vault: Arc::new(FakeVault(vault)),
        write: Arc::new(move |data: &[u8]| {
            writer.try_send(TransportInput::Data(data.to_vec())).is_ok()
        }),
        recorder: Arc::new(recorder.clone()),
        send: Arc::new(move |message| sink.lock().unwrap().push(message)),
    }));
    let output: Arc<Mutex<Vec<u8>>> = Arc::default();
    {
        let (session, recorder, output, events) = (
            session.clone(),
            recorder.clone(),
            output.clone(),
            channels.events.clone(),
        );
        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                match event {
                    TransportEvent::Data(data) => {
                        recorder.output(&data);
                        session.output(&data);
                        output.lock().unwrap().extend_from_slice(&data);
                    }
                    _ => break,
                }
            }
        });
    }
    session.start();
    Live {
        input: channels.input,
        session,
        recorder,
        messages,
        output,
    }
}

/// Host and folder metadata: the host sits in Network/Consoles.
struct Source {
    host: Option<LoginSequence>,
    network: Option<LoginSequence>,
}

impl LoginSequenceSource for Source {
    fn host_login_sequence(&self, host: &LoginSequenceHost) -> Option<HostLoginSequence> {
        (host == &LoginSequenceHost::Profile("core-sw1".into())).then(|| HostLoginSequence {
            login_sequence: self.host.clone(),
            group: Some("Network/Consoles".into()),
        })
    }

    fn folder_login_sequence(&self, path: &str) -> Option<LoginSequence> {
        (path == "Network").then(|| self.network.clone()).flatten()
    }
}

#[tokio::test]
async fn answers_the_prompts_with_a_folder_sequence_and_keeps_the_vault_secret_out_of_every_record()
{
    let (port, _connections) = start_fake_console().await;
    let scratch = tempfile::tempdir().unwrap();
    let history = MemoryHistory::new();
    let host = LoginSequenceHost::Profile("core-sw1".into());
    let sequence = resolve_login_sequence(
        &Source {
            host: None,
            network: Some(login_steps()),
        },
        &host,
    );
    assert!(sequence.is_some());

    let live = connect(
        port,
        sequence,
        UnlockPolicy::Never,
        &history,
        scratch.path().to_path_buf(),
    )
    .await;
    live.until("the sequence finishing", |live| {
        live.progress()
            .last()
            .is_some_and(|p| p.0 == LoginSequenceState::Done)
    })
    .await;
    let steps: Vec<String> = live
        .progress()
        .iter()
        .map(|(state, step, _, detail, _)| {
            format!(
                "{} {step} {detail}",
                serde_json::to_value(state).unwrap().as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            "running 1 Waiting for “Press RETURN”",
            "running 2 Pressing Enter",
            "running 3 Waiting for “Username:”",
            "running 4 Typing “admin” and Enter",
            r"running 5 Waiting for a match of “[Pp]assword:\s*$”",
            "running 6 Typing the secret “Console password” and Enter",
            "done 6 Typing the secret “Console password” and Enter",
        ]
    );
    live.until("the shell prompt", |live| {
        live.output().contains("core-sw1>")
    })
    .await;
    assert!(!live.output().contains("Access denied"));
    assert!(!live.output().contains(SECRET));

    // The session is the user's from here on.
    live.type_text("show version\r").await;
    let expected = format!("received {}", digest("show version"));
    live.until("the command", |live| live.output().contains(&expected))
        .await;

    let session_id = live.recorder.state().session_id.unwrap();
    live.close().await;
    let recorded = history.session(&session_id).unwrap();
    assert!(recorded.transcript().contains("admin"));
    assert!(!recorded.transcript().contains(SECRET));
    let raw: Vec<u8> = recorded
        .events
        .iter()
        .flat_map(|event| event.raw.clone())
        .collect();
    let raw = String::from_utf8_lossy(&raw);
    assert!(raw.contains("admin\r"));
    assert!(raw.contains("core-sw1>"));
    assert!(!raw.contains(SECRET));

    let log_file = scratch.path().join("core-sw1.log");
    let mut text = String::new();
    for _ in 0..500 {
        text = std::fs::read_to_string(&log_file).unwrap_or_default();
        if text.contains("Logging to file ended") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(text.contains("core-sw1>"), "{text}");
    assert!(text.contains("admin"));
    assert!(!text.contains(SECRET));
}

#[tokio::test]
async fn stops_at_a_step_that_times_out_and_leaves_the_session_open() {
    let (port, _connections) = start_fake_console().await;
    let scratch = tempfile::tempdir().unwrap();
    let history = MemoryHistory::new();
    let sequence = LoginSequence {
        steps: vec![wait("1", "Login:", false, 1), send("2", "admin")],
    };
    let live = connect(
        port,
        Some(sequence),
        UnlockPolicy::Never,
        &history,
        scratch.path().to_path_buf(),
    )
    .await;
    live.until("the timeout", |live| {
        live.progress()
            .last()
            .is_some_and(|p| p.0 == LoginSequenceState::Failed)
    })
    .await;
    let last = live.progress().last().cloned().unwrap();
    assert_eq!((last.1, last.2), (1, 2));
    assert_eq!(
        last.4.as_deref(),
        Some("Step 1 timed out after 1 s waiting for “Login:”.")
    );
    live.type_text("\r").await;
    live.until("the user prompt", |live| {
        live.output().contains("Username: ")
    })
    .await;
    assert!(!live.output().contains("admin"));
    let transcript = history.sessions()[0].transcript();
    assert!(
        transcript.contains("Login sequence stopped: Step 1 timed out"),
        "{transcript}"
    );
    live.close().await;
}

#[tokio::test]
async fn cancels_on_request_and_runs_again_on_a_new_connect() {
    let (port, connections) = start_fake_console().await;
    let scratch = tempfile::tempdir().unwrap();
    let history = MemoryHistory::new();
    let sequence = LoginSequence {
        steps: vec![
            wait("1", "Press RETURN", false, 5),
            send("2", ""),
            wait("3", "never shown", false, 60),
        ],
    };
    let first = connect(
        port,
        Some(sequence.clone()),
        UnlockPolicy::Never,
        &history,
        scratch.path().to_path_buf(),
    )
    .await;
    first
        .until("step 3", |live| {
            live.progress().last().is_some_and(|p| p.1 == 3)
        })
        .await;
    first
        .until("the user prompt", |live| {
            live.output().contains("Username: ")
        })
        .await;

    assert!(
        first
            .session
            .handle_control(&TerminalClientMessage::CancelLoginSequence)
    );
    first
        .until("the cancel", |live| {
            live.progress()
                .last()
                .is_some_and(|p| (p.0, p.1, p.2) == (LoginSequenceState::Cancelled, 3, 3))
        })
        .await;
    first.type_text("operator\r").await;
    first
        .until("the typed name", |live| live.output().contains("operator"))
        .await;

    // A reconnect is a new session, and the sequence starts over.
    let second = connect(
        port,
        Some(sequence),
        UnlockPolicy::Never,
        &history,
        scratch.path().to_path_buf(),
    )
    .await;
    second
        .until("step 3 again", |live| {
            live.progress().last().is_some_and(|p| p.1 == 3)
        })
        .await;
    assert_eq!(*connections.lock().unwrap(), 2);
    assert_eq!(
        (second.progress()[0].0, second.progress()[0].1),
        (LoginSequenceState::Running, 1)
    );
    first.close().await;
    second.close().await;
}

#[tokio::test]
async fn asks_for_the_master_password_when_the_vault_prompts_for_every_use() {
    let (port, _connections) = start_fake_console().await;
    let scratch = tempfile::tempdir().unwrap();
    let history = MemoryHistory::new();
    let live = connect(
        port,
        Some(login_steps()),
        UnlockPolicy::Credential,
        &history,
        scratch.path().to_path_buf(),
    )
    .await;
    live.until("the unlock prompt", |live| !live.prompts().is_empty())
        .await;
    let TerminalServerMessage::AuthPrompt(prompt) = &live.prompts()[0] else {
        unreachable!()
    };
    assert_eq!(prompt.purpose, Some(AuthPromptPurpose::VaultUnlock));
    assert_eq!(prompt.skip_label.as_deref(), Some("Stop login sequence"));
    // Other control messages are not taken as the answer.
    assert!(
        !live.session.handle_control(&TerminalClientMessage::Resize {
            cols: 100,
            rows: 30
        })
    );
    assert!(
        live.session
            .handle_control(&TerminalClientMessage::AuthResponse(AuthPromptResponse {
                answers: vec!["wrong-master".into()],
                ..AuthPromptResponse::default()
            }))
    );
    live.until("the second prompt", |live| live.prompts().len() == 2)
        .await;
    let TerminalServerMessage::AuthPrompt(again) = &live.prompts()[1] else {
        unreachable!()
    };
    assert!(
        again
            .instructions
            .as_deref()
            .unwrap()
            .contains("master password")
    );
    assert!(
        again
            .instructions
            .as_deref()
            .unwrap()
            .starts_with("The master password is incorrect.\n\n")
    );
    live.session
        .handle_control(&TerminalClientMessage::AuthResponse(AuthPromptResponse {
            answers: vec![MASTER.into()],
            ..AuthPromptResponse::default()
        }));
    live.until("the sequence finishing", |live| {
        live.progress()
            .last()
            .is_some_and(|p| p.0 == LoginSequenceState::Done)
    })
    .await;
    live.until("the shell prompt", |live| {
        live.output().contains("core-sw1>")
    })
    .await;
    live.close().await;
}

#[tokio::test]
async fn stops_when_the_user_declines_to_unlock_the_vault() {
    let (port, _connections) = start_fake_console().await;
    let scratch = tempfile::tempdir().unwrap();
    let history = MemoryHistory::new();
    let live = connect(
        port,
        Some(login_steps()),
        UnlockPolicy::Credential,
        &history,
        scratch.path().to_path_buf(),
    )
    .await;
    live.until("the unlock prompt", |live| !live.prompts().is_empty())
        .await;
    live.session
        .handle_control(&TerminalClientMessage::AuthResponse(AuthPromptResponse {
            answers: vec![],
            skipped: Some(true),
            ..AuthPromptResponse::default()
        }));
    live.until("the failure", |live| {
        live.progress()
            .last()
            .is_some_and(|p| p.0 == LoginSequenceState::Failed)
    })
    .await;
    let last = live.progress().last().cloned().unwrap();
    assert_eq!(
        (last.1, last.4.as_deref()),
        (6, Some("Step 6: the password vault stayed locked."))
    );
    live.close().await;
}

#[test]
fn runs_nothing_for_a_host_that_switches_the_folder_sequence_off() {
    let host = LoginSequenceHost::Profile("core-sw1".into());
    let folder = LoginSequence {
        steps: vec![send("1", "from-folder")],
    };
    let off = Source {
        host: Some(LoginSequence::default()),
        network: Some(folder.clone()),
    };
    assert_eq!(resolve_login_sequence(&off, &host), None);
    // An empty folder sequence stops the search as well.
    let folder_off = Source {
        host: None,
        network: Some(LoginSequence::default()),
    };
    assert_eq!(resolve_login_sequence(&folder_off, &host), None);
    let inherited = Source {
        host: None,
        network: Some(folder.clone()),
    };
    assert_eq!(resolve_login_sequence(&inherited, &host), Some(folder));
    assert_eq!(
        resolve_login_sequence(&inherited, &LoginSequenceHost::Alias("other".into())),
        None
    );
}
