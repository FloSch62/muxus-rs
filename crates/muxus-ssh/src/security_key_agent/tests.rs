use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex as StdMutex;

use russh::keys::ssh_key::certificate;

use super::*;
use crate::responsive_agent::identity_blob;
use crate::security_keys::{
    SecurityKeyError, describe_security_key_error, public_key_blob, read_openssh_key_file,
    security_key_file,
};
use fakes::*;

pub(crate) mod fakes;

#[derive(Default)]
struct Recorder(StdMutex<Vec<String>>);

impl AgentHooks for Recorder {
    fn pause_deadline(&self) {
        self.0.lock().unwrap().push("pause".into());
    }
    fn resume_deadline(&self) {
        self.0.lock().unwrap().push("resume".into());
    }
    fn on_security_key(&self, _key: &PublicKey) {
        self.0.lock().unwrap().push("touch".into());
    }
    fn on_signature(&self, algorithm: &str) {
        self.0.lock().unwrap().push(format!("signed {algorithm}"));
    }
}

/// A question as it was asked, with the runtime directory's entries (and their modes) then.
struct Asked {
    question: SecurityKeyQuestion,
    private_dirs: Vec<(String, u32)>,
}

/// A temp directory short enough that the private directory stays in it.
fn short_tempdir() -> tempfile::TempDir {
    let temp = std::env::temp_dir();
    let root = if temp.as_os_str().len() > 40 {
        PathBuf::from("/tmp")
    } else {
        temp
    };
    tempfile::Builder::new()
        .prefix("mxsk")
        .tempdir_in(root)
        .unwrap()
}

/// A security key file, a runtime directory, and what the agent did with them.
struct Setup {
    dir: tempfile::TempDir,
    runtime: tempfile::TempDir,
    file: PathBuf,
    key: PublicKey,
    hooks: Arc<Recorder>,
    asked: Arc<StdMutex<Vec<Asked>>>,
}

impl Setup {
    fn new() -> Self {
        Self::with_key("id_ed25519_sk", ed25519_sk())
    }

    fn with_key(name: &str, key: PublicKey) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(name);
        std::fs::write(&file, sk_key_file(&key, "none")).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            dir,
            runtime: short_tempdir(),
            file,
            key,
            hooks: Arc::new(Recorder::default()),
            asked: Arc::default(),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Answers the questions in turn, repeating the last answer.
    fn ask(&self, answers: &[Option<&str>]) -> Arc<dyn SecurityKeyAsk> {
        let answers: Vec<Option<String>> = answers
            .iter()
            .map(|answer| answer.map(str::to_string))
            .collect();
        let asked = self.asked.clone();
        let runtime = self.runtime.path().to_path_buf();
        Arc::new(move |question: SecurityKeyQuestion| {
            let mut asked = asked.lock().unwrap();
            let answer = answers
                .get(asked.len())
                .or(answers.last())
                .cloned()
                .flatten();
            asked.push(Asked {
                question,
                private_dirs: listing(&runtime),
            });
            async move { Ok::<_, AskCancelled>(answer) }
        })
    }

    fn options(&self, ask: Arc<dyn SecurityKeyAsk>, tools: OpenSshTools) -> SecurityKeyFileOptions {
        SecurityKeyFileOptions {
            file: self.file.clone(),
            public_key: self.key.clone(),
            identity: None,
            provider: None,
            ask,
            agent_options: ResponsiveAgentOptions {
                hooks: self.hooks.clone(),
                ..ResponsiveAgentOptions::default()
            },
            tools,
            runtime_dir: Some(self.runtime.path().to_path_buf()),
        }
    }

    fn tools(&self, ssh_agent: PathBuf, ssh_add: PathBuf) -> OpenSshTools {
        OpenSshTools { ssh_agent, ssh_add }
    }

    /// `kind:retry` of every question asked.
    fn kinds(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|asked| {
                let kind = match asked.question.kind {
                    SecurityKeyQuestionKind::Passphrase => "passphrase",
                    SecurityKeyQuestionKind::Pin => "pin",
                    SecurityKeyQuestionKind::Other => "other",
                };
                format!("{kind}:{}", asked.question.retry)
            })
            .collect()
    }

    fn hooks(&self) -> Vec<String> {
        self.hooks.0.lock().unwrap().clone()
    }

    /// A record the fake tools wrote, by line.
    fn record(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.path().join(name))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn runtime_entries(&self) -> Vec<(String, u32)> {
        listing(self.runtime.path())
    }
}

fn listing(dir: &Path) -> Vec<(String, u32)> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
            (entry.file_name().to_string_lossy().into_owned(), mode)
        })
        .collect()
}

async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Disposes the agent and checks that nothing of it is left.
async fn dispose_and_check(agent: &SecurityKeyFileAgent, setup: &Setup) {
    agent.dispose();
    assert_eq!(setup.runtime_entries(), Vec::<(String, u32)>::new());
    eventually("the OpenSSH processes to end", || {
        agent.running_children() == 0
    })
    .await;
}

fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

// --- helpers (security key helpers in security-keys.test.ts) ---

#[test]
fn reads_why_the_private_agent_could_not_sign_from_its_log() {
    assert_eq!(
        agent_failure_reason("process_sign_request2: sshkey_sign: device not found\n"),
        Some("no security key is plugged in")
    );
    assert_eq!(
        agent_failure_reason(
            "process_sign_request2: sshkey_sign: incorrect passphrase supplied to decrypt private key"
        ),
        Some("the PIN was not accepted")
    );
    assert!(
        agent_failure_reason("error: internal security key support not enabled")
            .unwrap()
            .starts_with(
                "this OpenSSH has no built-in security key support: set SecurityKeyProvider"
            )
    );
    assert_eq!(
        agent_failure_reason("process_sign_request2: sshkey_sign: invalid format"),
        None
    );
    assert_eq!(
        agent_failure_reason("SSHKEY_SIGN: DEVICE NOT FOUND"),
        Some("no security key is plugged in")
    );
}

#[test]
fn describes_signing_failures_in_terms_of_the_key_and_the_hop() {
    let key = ed25519_sk();
    let failed = |message: &str| SecurityKeyError {
        message: message.into(),
        key: Some(key.clone()),
    };
    let described = describe_security_key_error(&failed("Agent responded with failure"), "bastion");
    let rest = described
        .strip_prefix("the security key ED25519-SK SHA256:")
        .unwrap();
    let (fingerprint, rest) = rest.split_once(' ').unwrap();
    assert!(!fingerprint.is_empty());
    assert_eq!(
        rest,
        "did not sign the login to bastion: it was not touched in time, its PIN was not accepted, or it is not plugged in"
    );
    assert!(
        describe_security_key_error(
            &failed("the PIN was wrong, or it was not touched in time"),
            "web"
        )
        .ends_with(
            "did not sign the login to web: the PIN was wrong, or it was not touched in time"
        )
    );
    assert_eq!(
        describe_security_key_error(
            &SecurityKeyError {
                message: "could not load the security key x: y".into(),
                key: None
            },
            "web"
        ),
        "could not load the security key x: y"
    );
}

#[test]
fn reads_the_public_key_from_a_key_file_header_without_its_passphrase() {
    let key = ed25519_sk();
    let plain = read_openssh_key_file(sk_key_file(&key, "none").as_bytes()).unwrap();
    assert_eq!(plain.key_type, ED25519_SK);
    assert!(!plain.encrypted);
    assert_eq!(plain.public_blob, public_key_blob(&key));
    let encrypted = read_openssh_key_file(sk_key_file(&key, "aes256-ctr").as_bytes()).unwrap();
    assert_eq!(encrypted.key_type, ED25519_SK);
    assert!(encrypted.encrypted);
    assert_eq!(
        security_key_file(sk_key_file(&key, "none").as_bytes())
            .unwrap()
            .algorithm()
            .as_str(),
        ED25519_SK
    );
    assert_eq!(
        security_key_file(sk_key_file(&ecdsa_sk(), "none").as_bytes())
            .unwrap()
            .algorithm()
            .as_str(),
        ECDSA_SK
    );
    assert!(
        security_key_file(
            b"-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----\n"
        )
        .is_none()
    );
}

#[test]
fn keeps_only_the_complaint_of_an_openssh_tool() {
    assert_eq!(
        process_reason(
            "@@@@@@@@@@@@@@@@@@@@\n@    WARNING: UNPROTECTED PRIVATE KEY FILE!    @\n@@@@@@@@@@@@@@@@@@@@\nPermissions 0644 for 'k' are too open.\r\n  This private key will be ignored.\n\n"
        ),
        "Permissions 0644 for 'k' are too open. This private key will be ignored."
    );
    assert_eq!(process_reason("\n@@@\n"), "");
}

#[test]
fn reads_the_askpass_relay_protocol() {
    assert_eq!(
        parse_askpass_request(b"ask 42 456e746572\n"),
        Some((42, Some("Enter".to_string())))
    );
    assert_eq!(
        parse_askpass_request(b"ask 7 \n"),
        Some((7, Some(String::new())))
    );
    assert_eq!(parse_askpass_request(b"ask 7 zz\n"), Some((7, None)));
    assert_eq!(parse_askpass_request(b"ask x 41\n"), None);
    assert_eq!(parse_askpass_request(b"hello\n"), None);
    assert_eq!(first_line("pin\nmore"), "pin");
    assert_eq!(first_line(" spaced \\ pass "), " spaced \\ pass ");
    assert!(agent_ready(
        "SSH_AUTH_SOCK=/x/agent.sock; export SSH_AUTH_SOCK;\necho Agent pid 1234;\n"
    ));
    assert!(!agent_ready("echo Agent pid ;\n"));
}

#[test]
fn keeps_the_private_directory_in_the_runtime_directory_when_it_can() {
    let runtime = tempfile::tempdir().unwrap();
    let temp = Path::new("/var/tmp");
    if runtime.path().join("muxus-sk-XXXXXX").as_os_str().len() <= MAX_SOCKET_DIR_LENGTH {
        assert_eq!(
            private_directory_root(Some(runtime.path()), temp),
            runtime.path()
        );
    }
    assert_eq!(private_directory_root(None, temp), temp);
    assert_eq!(
        private_directory_root(Some(Path::new("relative/run")), temp),
        temp
    );
    assert_eq!(
        private_directory_root(Some(&runtime.path().join("missing")), temp),
        temp
    );
    // Too long for the agent's socket path: /tmp instead.
    let long = PathBuf::from(format!("/{}", "r".repeat(80)));
    assert_eq!(private_directory_root(None, &long), PathBuf::from("/tmp"));
    let dir = create_private_directory(runtime.path()).unwrap();
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("muxus-sk-") && name.len() == "muxus-sk-XXXXXX".len());
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn keeps_the_users_own_agent_away_from_openssh() {
    let envs = |provider: Option<&str>| -> Vec<(String, Option<String>)> {
        let command = openssh_command(Path::new("ssh-add"), Path::new("/x/askpass"), provider);
        command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    };
    let with_provider = envs(Some("/lib/sk.so"));
    for expected in [
        ("SSH_AUTH_SOCK", None),
        ("SSH_AGENT_PID", None),
        ("SSH_SK_PROVIDER", Some("/lib/sk.so")),
        ("SSH_ASKPASS", Some("/x/askpass")),
        ("SSH_ASKPASS_REQUIRE", Some("force")),
    ] {
        let expected = (expected.0.to_string(), expected.1.map(str::to_string));
        assert!(with_provider.contains(&expected), "{expected:?}");
    }
    assert!(envs(None).contains(&("SSH_SK_PROVIDER".to_string(), None)));
}

// --- the private agent (security key IdentityFile in security-keys.test.ts) ---

#[tokio::test]
async fn explains_what_to_do_when_ssh_agent_cannot_be_found() {
    let setup = Setup::new();
    let tools = setup.tools(
        PathBuf::from("muxus-test-no-such-ssh-agent"),
        setup.path().join("empty-path/ssh-add"),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[]), tools));
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(
        err.message,
        format!(
            "id_ed25519_sk is a security key (ED25519-SK). Muxus signs with it through OpenSSH's ssh-agent, which was not found: install the OpenSSH client, or load it into your SSH agent with \"ssh-add {}\"",
            setup.file.display()
        )
    );
    assert_eq!(err.key, None);
    // No passphrase prompt for the key handle file, and no second attempt.
    assert_eq!(agent.sign(None, b"data".to_vec()).await.unwrap_err(), err);
    assert!(setup.kinds().is_empty());
    assert_eq!(setup.hooks(), ["pause", "resume", "pause", "resume"]);
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn explains_what_to_do_when_ssh_add_cannot_be_found() {
    let setup = Setup::new();
    let tools = setup.tools(
        idle_ssh_agent(setup.path()),
        setup.path().join("empty-path/ssh-add"),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[]), tools));
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(
        err.message,
        format!(
            "id_ed25519_sk is a security key (ED25519-SK). Muxus signs with it through OpenSSH's ssh-add, which was not found: install the OpenSSH client, or load it into your SSH agent with \"ssh-add {}\"",
            setup.file.display()
        )
    );
    // The agent that did start ends with the login.
    assert_eq!(agent.running_children(), 2);
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn loads_the_key_into_a_private_agent_relaying_the_passphrase_and_cleans_up() {
    let setup = Setup::new();
    let tools = setup.tools(
        fake_ssh_agent(setup.path(), &[]),
        fake_ssh_add(setup.path(), Some("k3y-pass"), ""),
    );
    let agent = SecurityKeyFileAgent::new(
        setup.options(setup.ask(&[Some("wrong"), Some("k3y-pass")]), tools),
    );
    let signed = agent.sign(None, b"data".to_vec()).await.unwrap();
    // ssh-add accepted the second passphrase and loaded the key.
    assert_eq!(setup.kinds(), ["passphrase:false", "passphrase:true"]);
    assert!(signed.starts_with(b"data"));
    assert!(signed.ends_with(&[0x01, 1, 2, 3, 4]));
    assert_eq!(
        setup.hooks(),
        [
            "pause",
            "resume",
            "touch",
            "pause",
            "resume",
            "signed sk-ssh-ed25519@openssh.com"
        ]
    );

    // The private agent lived in the user's runtime directory, open to the user alone.
    let private = {
        let asked = setup.asked.lock().unwrap();
        let [(private, mode)] = asked[0].private_dirs.as_slice() else {
            panic!("{:?}", asked[0].private_dirs);
        };
        assert!(private.starts_with("muxus-sk-"));
        assert_eq!(*mode, 0o700);
        private.clone()
    };
    let socket = setup.runtime.path().join(private).join("agent.sock");
    assert_eq!(
        setup.record("agent-args"),
        ["-D", "-a", socket.to_str().unwrap()]
    );
    let agent_env = setup.record("agent-env");
    assert!(agent_env.contains(&"SSH_ASKPASS_REQUIRE=force".to_string()));
    for scrubbed in ["SSH_AUTH_SOCK=", "SSH_AGENT_PID=", "SSH_SK_PROVIDER="] {
        assert!(!agent_env.iter().any(|line| line.starts_with(scrubbed)));
    }
    assert_eq!(
        setup.record("add-args"),
        ["--", setup.file.to_str().unwrap()]
    );
    assert!(
        setup
            .record("add-env")
            .contains(&format!("SSH_AUTH_SOCK={}", socket.display()))
    );

    // The next signature uses the same agent and asks nothing.
    agent.sign(None, b"more".to_vec()).await.unwrap();
    assert_eq!(setup.kinds().len(), 2);
    assert_eq!(
        setup.record("agent-events"),
        [
            "sign sk-ssh-ed25519@openssh.com",
            "sign sk-ssh-ed25519@openssh.com"
        ]
    );
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn relays_prompts_and_answers_byte_for_byte() {
    let setup = Setup::with_key("id 'odd' \\ key ü", ed25519_sk());
    let passphrase = "  k3y \\ 'pass' ü $HOME ";
    let tools = setup.tools(
        fake_ssh_agent(setup.path(), &[]),
        fake_ssh_add(setup.path(), Some(passphrase), ""),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[Some(passphrase)]), tools));
    agent.sign(None, b"data".to_vec()).await.unwrap();
    {
        let asked = setup.asked.lock().unwrap();
        assert_eq!(
            asked[0].question.text,
            format!("Enter passphrase for {}", setup.file.display())
        );
        assert_eq!(asked.len(), 1);
    }
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn stops_asking_for_the_passphrase_after_three_tries() {
    let setup = Setup::new();
    let tools = setup.tools(
        idle_ssh_agent(setup.path()),
        fake_ssh_add(setup.path(), Some("right"), ""),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[Some("wrong")]), tools));
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(
        err.message,
        "could not load the security key id_ed25519_sk: the passphrase was not accepted"
    );
    assert_eq!(
        setup.kinds(),
        ["passphrase:false", "passphrase:true", "passphrase:true"]
    );
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn skipping_or_cancelling_the_passphrase_skips_the_key() {
    let setup = Setup::new();
    let tools = setup.tools(
        idle_ssh_agent(setup.path()),
        fake_ssh_add(setup.path(), Some("right"), ""),
    );
    let skipped = SecurityKeyFileAgent::new(setup.options(setup.ask(&[None]), tools.clone()));
    let err = skipped.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(
        err.message,
        "skipped the security key id_ed25519_sk: no passphrase was entered"
    );
    assert_eq!(setup.kinds(), ["passphrase:false"]);
    dispose_and_check(&skipped, &setup).await;

    let cancel: Arc<dyn SecurityKeyAsk> =
        Arc::new(|_question: SecurityKeyQuestion| async { Err::<Option<String>, _>(AskCancelled) });
    let cancelled = SecurityKeyFileAgent::new(setup.options(cancel, tools));
    let err = cancelled.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(
        err.message,
        "skipped the security key id_ed25519_sk: no passphrase was entered"
    );
    dispose_and_check(&cancelled, &setup).await;
}

#[tokio::test]
async fn explains_why_ssh_add_could_not_load_the_key() {
    let setup = Setup::new();
    let banner = r#"printf '%s\n' '@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@' '@         WARNING: UNPROTECTED PRIVATE KEY FILE!          @' '@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@' "Permissions 0644 for '$2' are too open." 'This private key will be ignored.' >&2
exit 1"#;
    let tools = setup.tools(
        idle_ssh_agent(setup.path()),
        fake_ssh_add(setup.path(), None, banner),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[]), tools));
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(
        err.message,
        format!(
            "could not load the security key id_ed25519_sk: Permissions 0644 for '{}' are too open. This private key will be ignored.",
            setup.file.display()
        )
    );
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn explains_why_ssh_agent_did_not_start() {
    let setup = Setup::new();
    let tools = setup.tools(
        failing_ssh_agent(
            setup.path(),
            "printf '%s\\n' '@@@@@@' 'unix_listener: cannot bind to path /x/agent.sock: Address already in use' >&2\nexit 2\n",
        ),
        fake_ssh_add(setup.path(), None, ""),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[]), tools));
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(
        err.message,
        "could not start an ssh-agent for the security key id_ed25519_sk: unix_listener: cannot bind to path /x/agent.sock: Address already in use"
    );
    assert!(setup.record("add-args").is_empty());
    dispose_and_check(&agent, &setup).await;

    let quiet = Setup::new();
    let tools = quiet.tools(
        failing_ssh_agent(quiet.path(), "exit 3\n"),
        fake_ssh_add(quiet.path(), None, ""),
    );
    let agent = SecurityKeyFileAgent::new(quiet.options(quiet.ask(&[]), tools));
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(
        err.message,
        "could not start an ssh-agent for the security key id_ed25519_sk: ssh-agent exited with status 3"
    );
    dispose_and_check(&agent, &quiet).await;
}

#[tokio::test]
async fn allows_the_resolved_provider_and_hands_it_to_both_tools() {
    let setup = Setup::new();
    let library = setup.path().join("lib/libsk.so");
    std::fs::create_dir_all(library.parent().unwrap()).unwrap();
    std::fs::write(&library, b"").unwrap();
    let link = setup.path().join("sk-link.so");
    std::os::unix::fs::symlink(&library, &link).unwrap();
    let tools = setup.tools(
        idle_ssh_agent(setup.path()),
        fake_ssh_add(setup.path(), None, ""),
    );
    let mut options = setup.options(setup.ask(&[]), tools);
    options.provider = Some(link.to_str().unwrap().to_string());
    let agent = SecurityKeyFileAgent::new(options);
    // The idle agent never listens, so the signature itself fails, naming the key.
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert!(
        err.message.starts_with("Failed to connect to SSH agent"),
        "{}",
        err.message
    );
    assert_eq!(err.key.as_ref(), Some(&setup.key));
    let args = setup.record("agent-args");
    assert_eq!(
        args[3..],
        [
            "-P".to_string(),
            std::fs::canonicalize(&library)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        ]
    );
    let provider = format!("SSH_SK_PROVIDER={}", link.display());
    assert!(setup.record("agent-env").contains(&provider));
    assert!(setup.record("add-env").contains(&provider));
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn explains_a_failed_signature_from_the_agent_log() {
    let setup = Setup::new();
    let tools = setup.tools(
        fake_ssh_agent(
            setup.path(),
            &[
                "refuse",
                "log=process_sign_request2: sshkey_sign: device not found",
            ],
        ),
        fake_ssh_add(setup.path(), None, ""),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[]), tools));
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(err.message, "no security key is plugged in");
    assert_eq!(err.key.as_ref(), Some(&setup.key));
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn keeps_the_agent_reply_when_its_log_says_nothing() {
    let setup = Setup::new();
    let tools = setup.tools(
        fake_ssh_agent(setup.path(), &["refuse"]),
        fake_ssh_add(setup.path(), None, ""),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[]), tools));
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(err.message, "Agent responded with failure");
    assert_eq!(err.key.as_ref(), Some(&setup.key));
    assert!(
        describe_security_key_error(&err, "bastion").ends_with(
            "did not sign the login to bastion: it was not touched in time, its PIN was not accepted, or it is not plugged in"
        )
    );
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn relays_the_pin_and_blames_it_when_the_signature_fails() {
    let setup = Setup::new();
    let tools = setup.tools(
        fake_ssh_agent(setup.path(), &["pin=0000", "presence"]),
        fake_ssh_add(setup.path(), None, ""),
    );
    let agent =
        SecurityKeyFileAgent::new(setup.options(setup.ask(&[Some("1234"), Some("0000")]), tools));
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(
        err.message,
        "the PIN was wrong, or it was not touched in time"
    );
    assert_eq!(err.key.as_ref(), Some(&setup.key));
    {
        let asked = setup.asked.lock().unwrap();
        let question = &asked[0].question;
        assert_eq!(question.kind, SecurityKeyQuestionKind::Pin);
        assert!(question.presence);
        assert_eq!(
            question.text,
            "Enter PIN and confirm user presence for ED25519-SK key SHA256:fake"
        );
    }
    // Entering a PIN that also needs a touch announces the touch again.
    assert_eq!(
        setup.hooks(),
        ["pause", "resume", "touch", "pause", "touch", "resume"]
    );

    agent.sign(None, b"data".to_vec()).await.unwrap();
    assert_eq!(
        setup.record("agent-events")[1..],
        [
            "pin 0 1234".to_string(),
            "sign sk-ssh-ed25519@openssh.com".to_string(),
            "pin 0 0000".to_string()
        ]
    );
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn touch_notifications_need_no_answer() {
    let setup = Setup::new();
    let tools = setup.tools(
        fake_ssh_agent(setup.path(), &["notify"]),
        fake_ssh_add(setup.path(), None, ""),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[]), tools));
    agent.sign(None, b"data".to_vec()).await.unwrap();
    assert_eq!(
        setup.record("agent-events"),
        ["sign sk-ssh-ed25519@openssh.com", "notify 0"]
    );
    assert!(setup.kinds().is_empty());
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn offers_a_certificate_and_signs_it_with_the_key_file() {
    let setup = Setup::with_key("id_cert_ecdsa_sk", ecdsa_sk());
    let mut builder =
        certificate::Builder::new(vec![7; 32], setup.key.key_data().clone(), 0, u64::MAX).unwrap();
    builder.valid_principal("tester").unwrap();
    builder.key_id("cert-id").unwrap();
    let certificate = builder.sign(&crate::testing::agent::ed25519()).unwrap();
    let identity = AgentIdentity::Certificate {
        certificate,
        comment: "cert".into(),
    };
    let tools = setup.tools(
        fake_ssh_agent(setup.path(), &[]),
        fake_ssh_add(setup.path(), None, ""),
    );
    let mut options = setup.options(setup.ask(&[]), tools);
    options.identity = Some(identity.clone());
    let agent = SecurityKeyFileAgent::new(options);
    let offered = agent.identities();
    assert_eq!(offered.len(), 1);
    assert_eq!(identity_blob(&offered[0]), identity_blob(&identity));

    agent.sign(None, b"data".to_vec()).await.unwrap();
    // The agent signed with the plain key; the signature counts as the certificate's.
    assert_eq!(
        setup.record("agent-events"),
        ["sign sk-ecdsa-sha2-nistp256@openssh.com"]
    );
    assert_eq!(
        setup.hooks().last().unwrap(),
        "signed sk-ecdsa-sha2-nistp256-cert-v01@openssh.com"
    );
    dispose_and_check(&agent, &setup).await;
}

#[test]
fn offers_the_key_itself_without_a_certificate() {
    let setup = Setup::new();
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[]), OpenSshTools::default()));
    let offered = agent.identities();
    assert_eq!(identity_blob(&offered[0]), public_key_blob(&setup.key));
    assert_eq!(agent.file(), setup.file.as_path());
    assert_eq!(agent.public_key(), &setup.key);
}

#[tokio::test]
async fn a_disposed_agent_starts_nothing() {
    let setup = Setup::new();
    let tools = setup.tools(
        fake_ssh_agent(setup.path(), &[]),
        fake_ssh_add(setup.path(), None, ""),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[]), tools));
    agent.dispose();
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert_eq!(err.message, "the login ended");
    assert!(setup.record("agent-args").is_empty());
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn disposing_during_a_prompt_ends_the_login_and_cleans_up() {
    let setup = Setup::new();
    let tools = setup.tools(
        idle_ssh_agent(setup.path()),
        fake_ssh_add(setup.path(), Some("k3y-pass"), ""),
    );
    let asked = Arc::new(Notify::new());
    let ask: Arc<dyn SecurityKeyAsk> = {
        let asked = asked.clone();
        Arc::new(move |_question: SecurityKeyQuestion| {
            asked.notify_one();
            std::future::pending::<Result<Option<String>, AskCancelled>>()
        })
    };
    let agent = SecurityKeyFileAgent::new(setup.options(ask, tools));
    let (result, ()) = tokio::join!(agent.sign(None, b"data".to_vec()), async {
        asked.notified().await;
        agent.dispose();
    });
    assert_eq!(result.unwrap_err().message, "the login ended");
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn the_watchdog_ends_the_agent_once_muxus_is_gone() {
    let setup = Setup::new();
    let tools = setup.tools(
        fake_ssh_agent(setup.path(), &[]),
        fake_ssh_add(setup.path(), None, ""),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[]), tools));
    agent.sign(None, b"data".to_vec()).await.unwrap();
    assert!(!agent.agent_exited());
    assert!(agent.close_watchdog_pipe());
    eventually("the watchdog to end the agent", || agent.agent_exited()).await;
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    assert!(
        err.message.starts_with("Failed to connect to SSH agent"),
        "{}",
        err.message
    );
    dispose_and_check(&agent, &setup).await;
}

#[tokio::test]
async fn signs_login_challenges_as_a_russh_signer() {
    use russh::Signer;
    let setup = Setup::new();
    let tools = setup.tools(
        fake_ssh_agent(setup.path(), &[]),
        fake_ssh_add(setup.path(), None, ""),
    );
    let agent = SecurityKeyFileAgent::new(setup.options(setup.ask(&[]), tools));
    let identity = agent.identities().remove(0);
    let signed = SecurityKeySigner(&agent)
        .auth_sign(&identity, None, b"challenge".to_vec())
        .await
        .unwrap();
    assert!(signed.starts_with(b"challenge"));
    dispose_and_check(&agent, &setup).await;
}

/// The TypeScript test runs the real OpenSSH tools when they are installed; so does this one.
#[tokio::test]
async fn loads_an_encrypted_key_with_the_real_openssh_tools() {
    let (Some(ssh_agent), Some(ssh_add), Some(ssh_keygen)) = (
        find_program("ssh-agent"),
        find_program("ssh-add"),
        find_program("ssh-keygen"),
    ) else {
        eprintln!("OpenSSH is not installed; skipping");
        return;
    };
    let setup = Setup::with_key("id_encrypted_sk", ed25519_sk());
    // The real tools, with a scratch HOME.
    let home = setup.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let wrap = |name: &str, program: &Path| {
        write_tool(
            &setup.path().join(name),
            &format!(
                "HOME={} exec {} \"$@\"\n",
                shell_quote(&home),
                shell_quote(program)
            ),
        )
    };
    let tools = setup.tools(wrap("ssh-agent", &ssh_agent), wrap("ssh-add", &ssh_add));
    let status = std::process::Command::new(ssh_keygen)
        .args(["-q", "-p", "-P", "", "-N", "k3y-pass", "-f"])
        .arg(&setup.file)
        .env("HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let mut options = setup.options(setup.ask(&[Some("wrong"), Some("k3y-pass")]), tools);
    // A middleware that cannot load keeps the signature away from any authenticator that
    // happens to be plugged in.
    options.provider = Some("/dev/null".into());
    let agent = SecurityKeyFileAgent::new(options);
    let err = agent.sign(None, b"data".to_vec()).await.unwrap_err();
    // Signing fails without an authenticator, but only after ssh-add accepted the second
    // passphrase and loaded the key.
    assert_eq!(
        setup.kinds(),
        ["passphrase:false", "passphrase:true"],
        "{}",
        err.message
    );
    assert_eq!(err.key.as_ref(), Some(&setup.key), "{}", err.message);
    // The private agent itself refused: it was listening and held the key.
    assert!(
        !err.message.starts_with("Failed to connect"),
        "{}",
        err.message
    );
    {
        let asked = setup.asked.lock().unwrap();
        assert!(
            matches!(asked[0].private_dirs.as_slice(), [(name, 0o700)] if name.starts_with("muxus-sk-"))
        );
    }
    dispose_and_check(&agent, &setup).await;
}
