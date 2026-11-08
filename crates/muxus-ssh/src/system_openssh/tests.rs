//! Ported from `tests/unit/server/system-openssh.test.ts`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_core::api::{ConfigForward, ForwardType, saved_host_hop};
use muxus_core::environments::{ContainerRuntime, ContainerShell, EnvironmentTarget};
use muxus_core::platform::Platform;
use muxus_core::protocol::SshProfile;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

fn s(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

fn alias() -> SshProfile {
    SshProfile::new("build-box")
}

fn saved() -> SshProfile {
    SshProfile {
        profile_id: Some("p1".into()),
        use_config: Some(false),
        ..SshProfile::new("alice@build.example.com:2200")
    }
}

fn contains_all(args: &[String], expected: &[&str]) {
    for item in expected {
        assert!(
            args.iter().any(|a| a == item),
            "{item} missing from {args:?}"
        );
    }
}

#[test]
fn leaves_an_ssh_config_alias_to_its_config() {
    assert_eq!(
        ssh_destination(&alias()),
        SshDestination {
            options: Vec::new(),
            destination: "build-box".into()
        }
    );
    assert!(saved_host_options(&alias()).is_empty());
    assert_eq!(
        terminal_ssh_args(&alias(), None),
        s(&[
            "-o",
            "EscapeChar=none",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "--",
            "build-box"
        ])
    );
}

#[test]
fn takes_the_user_and_port_from_a_saved_target_with_its_fields_winning() {
    assert_eq!(
        ssh_destination(&saved()),
        SshDestination {
            options: s(&["-l", "alice", "-p", "2200"]),
            destination: "build.example.com".into()
        }
    );
    let explicit = SshProfile {
        user: Some("bob".into()),
        port: Some(22),
        ..saved()
    };
    assert_eq!(
        ssh_destination(&explicit),
        SshDestination {
            options: s(&["-l", "bob", "-p", "22"]),
            destination: "build.example.com".into()
        }
    );
}

#[test]
fn passes_a_saved_hosts_settings_on_the_command_line_and_asks_for_gssapi() {
    let forward = |kind, bind_port, host: Option<&str>, port| ConfigForward {
        kind,
        bind_port,
        target_host: host.map(str::to_string),
        target_port: port,
    };
    let profile = SshProfile {
        identity_files: Some(s(&["~/.ssh/a key"])),
        certificate_files: Some(s(&["/certs/a b-cert.pub"])),
        identities_only: Some(true),
        forward_agent: Some(false),
        proxy_jump: Some(s(&["jump1", "jump2"])),
        proxy_command: Some("nc \"%h\" %p".into()),
        remote_command: Some("date +%s".into()),
        forwards: Some(vec![
            forward(ForwardType::Local, 8080, Some("::1"), Some(80)),
            forward(ForwardType::Remote, 9090, Some("db"), Some(5432)),
            forward(ForwardType::Dynamic, 1080, None, None),
        ]),
        ..saved()
    };
    assert_eq!(
        saved_host_options(&profile),
        s(&[
            "-i",
            "~/.ssh/a key",
            "-o",
            "CertificateFile=\"/certs/a b-cert.pub\"",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "ForwardAgent=no",
            "-J",
            "jump1,jump2",
            "-o",
            "ProxyCommand=nc \"%h\" %p",
            "-o",
            "RemoteCommand=date +%%s",
            "-L",
            "8080:[::1]:80",
            "-R",
            "9090:db:5432",
            "-D",
            "1080",
            "-o",
            "GSSAPIAuthentication=yes",
        ])
    );
}

#[test]
fn does_not_ask_for_gssapi_on_a_password_only_saved_host() {
    let profile = SshProfile {
        password_only: Some(true),
        ..saved()
    };
    assert_eq!(
        saved_host_options(&profile),
        s(&[
            "-o",
            "PubkeyAuthentication=no",
            "-o",
            "PreferredAuthentications=keyboard-interactive,password",
        ])
    );
}

#[test]
fn shares_the_login_with_the_file_browser_and_leaves_the_config_out_of_its_session() {
    let control = Path::new("/tmp/d/c");
    contains_all(
        &terminal_ssh_args(&alias(), Some(control)),
        &[
            "ControlMaster=yes",
            "ControlPath=/tmp/d/c",
            "ControlPersist=60",
        ],
    );
    let args = sftp_ssh_args(&alias(), control);
    contains_all(
        &args,
        &[
            "-s",
            "ControlMaster=no",
            "ControlPath=/tmp/d/c",
            "RemoteCommand=none",
            "BatchMode=yes",
        ],
    );
    assert_eq!(args[args.len() - 3..], s(&["--", "build-box", "sftp"]));
}

#[test]
fn feeds_scripts_over_the_shared_login() {
    let args = script_ssh_args(&alias(), Path::new("/tmp/d/c"));
    contains_all(
        &args,
        &["ControlMaster=no", "ControlPath=/tmp/d/c", "BatchMode=yes"],
    );
    assert_eq!(args[args.len() - 3..], s(&["--", "build-box", "sh -s"]));
}

#[test]
fn asks_whether_the_host_runs_commands_on_a_connection_of_its_own() {
    let profile = SshProfile {
        forwards: Some(vec![ConfigForward {
            kind: ForwardType::Dynamic,
            bind_port: 1080,
            target_host: None,
            target_port: None,
        }]),
        ..saved()
    };
    let args = command_probe_args(&profile);
    contains_all(
        &args,
        &[
            "ControlMaster=no",
            "ControlPath=none",
            "BatchMode=yes",
            "ClearAllForwardings=yes",
        ],
    );
    assert!(!args.join(" ").contains("muxus-ssh"));
    // A saved host's own settings can only be reached through its options.
    contains_all(
        &args,
        &["-l", "alice", "-p", "2200", "GSSAPIAuthentication=yes"],
    );
    assert_eq!(
        args.last().map(String::as_str),
        Some("echo \"muxus$((40+2))\"")
    );
}

#[test]
fn enters_a_container_with_a_forced_terminal_whatever_the_hosts_settings_say() {
    let profile = SshProfile {
        remote_command: Some("tmux".into()),
        environment: Some(EnvironmentTarget::Container {
            runtime: ContainerRuntime::Docker,
            container: "web".into(),
            shell: Some(ContainerShell::Bash),
        }),
        ..saved()
    };
    let args = terminal_ssh_args(&profile, None);
    let at = |value: &str| args.iter().position(|a| a == value).unwrap();
    assert!(at("RemoteCommand=none") < at("RemoteCommand=tmux"));
    assert!(args.iter().any(|a| a == "RequestTTY=force"));
    let last = args.last().unwrap();
    assert!(last.contains("docker") && last.contains("exec"), "{last}");
    assert_eq!(args[args.len() - 2], "build.example.com");
}

#[test]
fn finds_ssh_on_path() {
    let dir = tempfile::tempdir().unwrap();
    let platform = Platform::current();
    let program = dir.path().join(if platform == Platform::Windows {
        "ssh.exe"
    } else {
        "ssh"
    });
    std::fs::write(&program, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let missing = dir.path().join("nonexistent");
    let path = std::env::join_paths([missing.as_path(), dir.path()]).unwrap();
    let env = HashMap::from([("PATH".to_string(), path.to_string_lossy().into_owned())]);
    assert_eq!(find_ssh_binary(&env, platform), Some(program));
}

#[test]
fn reports_nothing_when_there_is_no_program() {
    let env = HashMap::from([("SystemRoot".to_string(), "/nonexistent".to_string())]);
    assert_eq!(find_ssh_binary(&env, Platform::Windows), None);
}

#[test]
fn refuses_a_jump_through_a_host_saved_in_muxus() {
    let profile = SshProfile {
        proxy_jump: Some(vec![saved_host_hop("other")]),
        ..saved()
    };
    let err = SystemOpenSshSessions::default()
        .start(&profile, true, &SessionTools::default())
        .unwrap_err();
    assert!(err.contains("ssh program cannot reach"), "{err}");
}

#[test]
fn refuses_a_link_that_pins_a_host_key() {
    let profile = SshProfile {
        host_key_fingerprint: Some("SHA256:abc".into()),
        ..alias()
    };
    let err = SystemOpenSshSessions::default()
        .start(&profile, true, &SessionTools::default())
        .unwrap_err();
    assert!(err.contains("pins a host key"), "{err}");
}

// --- A session through the real ssh program ---------------------------------------------

/// The `ssh` program, when this computer has one that runs.
fn ssh_program() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let env: HashMap<String, String> = std::env::vars().collect();
    let program = find_ssh_binary(&env, Platform::current())?;
    std::process::Command::new(&program)
        .arg("-V")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|_| program)
}

/// Everything the real `ssh` reads stays in `dir`: its config, a home without keys, and
/// known hosts that are never written.
struct Sandbox {
    dir: tempfile::TempDir,
    tools: SessionTools,
}

fn sandbox(program: PathBuf) -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    std::fs::write(
        &config,
        format!(
            "Host *\n  StrictHostKeyChecking no\n  UserKnownHostsFile /dev/null\n  GlobalKnownHostsFile /dev/null\n  LogLevel ERROR\n  IdentityAgent none\n  IdentitiesOnly yes\n  IdentityFile {}\n  GSSAPIAuthentication no\n",
            dir.path().join("no-key").display()
        ),
    )
    .unwrap();
    let mut env = HashMap::new();
    env.insert("HOME".to_string(), dir.path().display().to_string());
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }
    let tools = SessionTools {
        binary: Some(program),
        extra_args: vec!["-F".into(), config.display().to_string()],
        env,
        control_root: None,
    };
    Sandbox { dir, tools }
}

fn profile(port: u16) -> SshProfile {
    SshProfile {
        profile_id: Some("p1".into()),
        use_config: Some(false),
        port: Some(port),
        ..SshProfile::new("127.0.0.1")
    }
}

/// The terminal's `ssh`, run with pipes instead of a PTY: the shell gets no tty, but the login
/// and its control socket work the same.
struct Terminal {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    output: Arc<Mutex<String>>,
}

fn start_terminal(session: &Arc<SystemOpenSshSession>) -> Terminal {
    let mut child = tokio::process::Command::new(session.binary())
        .args(session.terminal_args())
        .env_clear()
        .envs(session.env())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let output = Arc::new(Mutex::new(String::new()));
    for mut stream in [
        Box::new(child.stdout.take().unwrap()) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
        Box::new(child.stderr.take().unwrap()),
    ] {
        let output = output.clone();
        tokio::spawn(async move {
            let mut buffer = vec![0u8; 4096];
            while let Ok(n) = stream.read(&mut buffer).await {
                if n == 0 {
                    break;
                }
                output
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buffer[..n]));
            }
        });
    }
    Terminal {
        child,
        stdin,
        output,
    }
}

impl Terminal {
    async fn wait_for(&self, expected: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if self.output.lock().unwrap().contains(expected) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no {expected:?} in {:?}",
                self.output.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn wait_until(mut check: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !check() {
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn server(
    exec: Option<crate::testing::server::ExecHandler>,
    sftp_root: Option<PathBuf>,
) -> crate::testing::server::TestServer {
    let mut spec = crate::testing::server::ServerSpec::new();
    spec.accept_none = true;
    spec.greeting = "hello from the shell\n".into();
    spec.exec_handler = exec;
    spec.sftp_root = sftp_root;
    crate::testing::server::start(spec).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_the_terminal_shares_its_login_with_sftp_and_cleans_up() {
    let Some(program) = ssh_program() else { return };
    let files = tempfile::tempdir().unwrap();
    let server = server(None, Some(files.path().to_path_buf())).await;
    let sandbox = sandbox(program);
    let registry = SystemOpenSshSessions::default();
    let session = registry
        .start(&profile(server.port()), true, &sandbox.tools)
        .unwrap();
    assert!(session.sftp_available);
    let control = session.control_path().unwrap().to_path_buf();
    let control_dir = control.parent().unwrap().to_path_buf();
    let name = control_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("muxus-ssh-") && name.len() == "muxus-ssh-".len() + 6,
        "{name}"
    );

    let mut terminal = start_terminal(&session);
    terminal.wait_for("hello from the shell").await;
    terminal.stdin.write_all(b"typed text\n").await.unwrap();
    terminal.wait_for("typed text").await;

    let lease = registry.acquire(&session.id).expect("a lease");
    let sftp = lease.connection().sftp().await.unwrap();
    let home = sftp.session.realpath(".").await.unwrap();
    assert!(!home.files.is_empty());
    // One SFTP session serves every file request.
    assert!(Arc::ptr_eq(
        &sftp,
        &lease.connection().sftp().await.unwrap()
    ));

    // A running transfer outlives the tab, as it does on a built-in connection.
    let _ = terminal.child.start_kill();
    let _ = terminal.child.wait().await;
    session.terminal_ended();
    assert!(!session.is_finished());
    sftp.session.realpath("after-tab").await.unwrap();

    lease.release();
    tokio::time::timeout(Duration::from_secs(10), session.closed())
        .await
        .unwrap();
    assert!(registry.acquire(&session.id).is_none());
    wait_until(|| !control_dir.exists()).await;
    drop(sandbox.dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaves_nothing_behind_when_the_backend_shuts_down() {
    let Some(program) = ssh_program() else { return };
    let server = server(None, None).await;
    let sandbox = sandbox(program);
    let registry = SystemOpenSshSessions::default();
    let session = registry
        .start(&profile(server.port()), true, &sandbox.tools)
        .unwrap();
    let control_dir = session
        .control_path()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let terminal = start_terminal(&session);
    terminal.wait_for("hello from the shell").await;
    wait_until(|| session.control_path().unwrap().exists()).await;

    registry.close_all();
    // Already gone on return: a shutting-down backend gets no later turn.
    assert!(!control_dir.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_scripts_over_the_shared_login() {
    let Some(program) = ssh_program() else { return };
    let scripts: Arc<Mutex<Vec<String>>> = Arc::default();
    let probes: Arc<Mutex<usize>> = Arc::default();
    let handler: crate::testing::server::ExecHandler = {
        let scripts = scripts.clone();
        let probes = probes.clone();
        Arc::new(move |command: &str, input: &[u8]| {
            if command.contains("muxus$((40+2))") {
                *probes.lock().unwrap() += 1;
                return Some(("muxus42\n".to_string(), 0));
            }
            if command != "sh -s" {
                return None;
            }
            scripts
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(input).into_owned());
            Some(("banner\nnot statistics\n".to_string(), 0))
        })
    };
    let server = server(Some(handler), None).await;
    let sandbox = sandbox(program);
    let registry = SystemOpenSshSessions::default();
    let session = registry
        .start(&profile(server.port()), true, &sandbox.tools)
        .unwrap();
    let mut terminal = start_terminal(&session);
    terminal.wait_for("hello from the shell").await;

    let lease = registry.acquire(&session.id).unwrap();
    assert!(lease.connection().runs_commands().await.unwrap());
    // One question per session, however often the status bar polls.
    assert!(lease.connection().runs_commands().await.unwrap());
    assert_eq!(*probes.lock().unwrap(), 1);
    let output = lease.connection().run_script("echo hi\n").await.unwrap();
    assert!(output.contains("not statistics"), "{output}");
    assert_eq!(*scripts.lock().unwrap(), vec!["echo hi\n".to_string()]);

    terminal.stdin.write_all(b"still here\n").await.unwrap();
    terminal.wait_for("still here").await;
    lease.release();
    registry.close_all();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_count_a_device_cli_that_echoes_the_question_as_running_commands() {
    let Some(program) = ssh_program() else { return };
    let handler: crate::testing::server::ExecHandler =
        Arc::new(|command: &str, _: &[u8]| Some((format!("% Invalid input: {command}\n"), 0)));
    let server = server(Some(handler), None).await;
    let sandbox = sandbox(program);
    let registry = SystemOpenSshSessions::default();
    let session = registry
        .start(&profile(server.port()), true, &sandbox.tools)
        .unwrap();
    let terminal = start_terminal(&session);
    terminal.wait_for("hello from the shell").await;
    let lease = registry.acquire(&session.id).unwrap();
    assert!(!lease.connection().runs_commands().await.unwrap());
    lease.release();
    registry.close_all();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn has_no_commands_or_shared_login_in_a_session_without_sftp() {
    let Some(program) = ssh_program() else { return };
    let server = server(None, None).await;
    let sandbox = sandbox(program);
    let registry = SystemOpenSshSessions::default();
    let session = registry
        .start(&profile(server.port()), false, &sandbox.tools)
        .unwrap();
    assert!(!session.sftp_available);
    assert!(session.control_path().is_none());
    let terminal = start_terminal(&session);
    terminal.wait_for("hello from the shell").await;
    let lease = registry.acquire(&session.id).unwrap();
    assert!(!lease.connection().runs_commands().await.unwrap());
    let err = lease.connection().run_script("true\n").await.unwrap_err();
    assert!(err.contains("not available"), "{err}");
    let err = lease.connection().sftp().await.unwrap_err();
    assert!(err.contains("not available"), "{err}");
    lease.release();
    registry.close_all();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passes_on_the_remote_shells_exit_and_its_own_failures() {
    let Some(program) = ssh_program() else { return };
    let server = server(None, None).await;
    let sandbox = sandbox(program);
    let registry = SystemOpenSshSessions::default();
    let session = registry
        .start(&profile(server.port()), true, &sandbox.tools)
        .unwrap();
    let mut terminal = start_terminal(&session);
    terminal.wait_for("hello from the shell").await;
    // The fake shell ends with status 3 on `exit`.
    terminal.stdin.write_all(b"exit\r").await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(15), terminal.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(3));
    session.terminal_ended();

    // Nothing listens on port 1: ssh's own failure.
    let failed = registry.start(&profile(1), true, &sandbox.tools).unwrap();
    let mut terminal = start_terminal(&failed);
    let status = tokio::time::timeout(Duration::from_secs(15), terminal.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(SSH_OWN_FAILURE));
    failed.terminal_ended();
    for dir in [session.control_path(), failed.control_path()] {
        let dir = dir.unwrap().parent().unwrap().to_path_buf();
        wait_until(|| !dir.exists()).await;
    }
}
