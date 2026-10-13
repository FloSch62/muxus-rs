//! Shared fixture of the UI regression tests: an in-process SSH/SFTP server on
//! loopback, a sandboxed `Backend` on its own tokio runtime, and one signed-in
//! terminal session whose connection the views under test use. Nothing touches
//! the real home directory.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::TestAppContext;
use muxus_core::protocol::{AuthPromptResponse, TerminalClientMessage, TerminalServerMessage};
use muxus_session::{Backend, BackendOptions, RuntimeChoice, SessionEvent, SessionHandle};
use muxus_ssh::testing::server::{ServerSpec, TestServer, start as start_sshd};
use muxus_ssh::testing::sftp_server::{SftpServerOptions, register};
use muxus_ui::gpui_kit::App;

pub struct Fixture {
    pub backend: Backend,
    pub conn_id: String,
    pub remote: PathBuf,
    pub home: PathBuf,
    pub server: TestServer,
    pub session: SessionHandle,
    pub runtime: tokio::runtime::Runtime,
    pub scratch: tempfile::TempDir,
}

impl Fixture {
    /// Start the server with `seed` filling its root (served as `/home/tester`), the backend,
    /// and sign in.
    pub fn start(seed: impl FnOnce(&Path)) -> Self {
        Self::start_with(seed, |spec| spec)
    }

    pub fn start_with(
        seed: impl FnOnce(&Path),
        spec: impl FnOnce(ServerSpec) -> ServerSpec,
    ) -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let remote = scratch.path().join("remote");
        std::fs::create_dir_all(&remote).unwrap();
        seed(&remote);
        register(
            &remote,
            SftpServerOptions {
                home: "/".into(),
                ..SftpServerOptions::default()
            },
        );
        let server = runtime.block_on(start_sshd(spec(ServerSpec {
            sftp_root: Some(remote.clone()),
            ..ServerSpec::new()
                .password("pw")
                .methods(&[russh::MethodKind::Password])
        })));
        let home = scratch.path().join("home");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(
            home.join(".ssh/config"),
            format!(
                "Host box\n  HostName 127.0.0.1\n  User tester\n  Port {}\n",
                server.port()
            ),
        )
        .unwrap();
        let mut options = BackendOptions::sandbox(scratch.path().join("data"), &home);
        options.runtime = RuntimeChoice::Handle(runtime.handle().clone());
        let backend = runtime
            .block_on(Backend::start(options))
            .expect("the backend starts");
        let (session, conn_id) = connect(&runtime, &backend, "box");
        Self {
            backend,
            conn_id,
            remote,
            home,
            server,
            session,
            runtime,
            scratch,
        }
    }
}

/// Open a terminal session to `target`, answer its prompts, and wait for `ready`.
pub fn connect(
    runtime: &tokio::runtime::Runtime,
    backend: &Backend,
    target: &str,
) -> (SessionHandle, String) {
    let connect: TerminalClientMessage = serde_json::from_value(serde_json::json!({
        "op": "connect",
        "profile": { "kind": "ssh", "target": target },
        "cols": 80,
        "rows": 24
    }))
    .unwrap();
    let session = backend.open_session(connect);
    let conn_id = runtime.block_on(async {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(20), session.next_event())
                .await
                .expect("the session answers")
                .expect("the session stays open");
            if let SessionEvent::Message(message) = event {
                match message {
                    TerminalServerMessage::HostKey(_) => {
                        session.send(TerminalClientMessage::HostKeyResponse { accept: true })
                    }
                    TerminalServerMessage::AuthPrompt(_) => {
                        session.send(TerminalClientMessage::AuthResponse(AuthPromptResponse {
                            answers: vec!["pw".into()],
                            remember_password: None,
                            skipped: None,
                        }))
                    }
                    TerminalServerMessage::Ready { conn_id, .. } => break conn_id,
                    TerminalServerMessage::Exit { message, .. } => {
                        panic!("the session ended: {message:?}")
                    }
                    _ => {}
                }
            }
        }
    });
    (session, conn_id)
}

/// Let the backend's own threads wake the test's foreground tasks (its replies arrive on
/// tokio threads), which the deterministic scheduler otherwise reports as an error.
pub fn allow_backend_threads(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
}

/// The UI's shared parts: theme, widgets, SFTP services (notifications collected).
pub fn init_ui(backend: &Backend, cx: &mut App) -> Rc<std::cell::RefCell<Vec<String>>> {
    muxus_ui::init(muxus_ui::theme::Mode::Light, cx);
    let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = seen.clone();
    muxus_app::features::sftp::services::init(
        backend.clone(),
        Rc::new(move |_, message, _, _| sink.borrow_mut().push(message.to_string())),
        cx,
    );
    seen
}

/// Let the foreground executor and the backend's threads run until `done` holds (or fail
/// after `timeout`).
pub fn wait_until(
    cx: &mut TestAppContext,
    timeout: Duration,
    what: &str,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = Instant::now() + timeout;
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Let everything settle for `duration` (no condition).
pub fn settle(cx: &mut TestAppContext, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    cx.run_until_parked();
}
