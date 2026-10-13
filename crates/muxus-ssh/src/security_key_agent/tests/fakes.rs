//! Security key fixtures and stand-ins for OpenSSH's ssh-agent and ssh-add.
//!
//! The stand-in tools are shell scripts written per test. The signing ssh-agent re-runs this test
//! binary as `fake_ssh_agent_process`, which serves the agent protocol on the socket the agent was
//! asked for and, like ssh-agent, runs $SSH_ASKPASS itself for a PIN or a touch notification.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine;
use russh::keys::ssh_key::PublicKey;
use russh::keys::{Algorithm, EcdsaCurve, PrivateKey};

use crate::security_keys::{public_key_blob, read_field, ssh_string};

pub(super) const ED25519_SK: &str = "sk-ssh-ed25519@openssh.com";
pub(super) const ECDSA_SK: &str = "sk-ecdsa-sha2-nistp256@openssh.com";

/// The key fields of a plain key blob, after its type.
fn key_fields(blob: &[u8]) -> &[u8] {
    let (_, after_type) = read_field(blob, 0).unwrap();
    &blob[after_type..]
}

/// A FIDO key the way an authenticator holds it: an Ed25519 key bound to the "ssh:" application.
pub(crate) fn ed25519_sk() -> PublicKey {
    let key = crate::testing::agent::ed25519();
    let blob = public_key_blob(key.public_key());
    let mut sk = ssh_string(ED25519_SK.as_bytes());
    sk.extend_from_slice(key_fields(&blob));
    sk.extend(ssh_string(b"ssh:"));
    PublicKey::from_bytes(&sk).unwrap()
}

/// A FIDO key on P-256, bound to the "ssh:" application.
pub(crate) fn ecdsa_sk() -> PublicKey {
    let key = PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        },
    )
    .unwrap();
    let blob = public_key_blob(key.public_key());
    let mut sk = ssh_string(ECDSA_SK.as_bytes());
    sk.extend_from_slice(key_fields(&blob));
    sk.extend(ssh_string(b"ssh:"));
    PublicKey::from_bytes(&sk).unwrap()
}

/// An OpenSSH private key file for a security key, as ssh-keygen writes it. With a cipher other
/// than "none" only the header is meaningful.
pub(crate) fn sk_key_file(key: &PublicKey, cipher: &str) -> String {
    let blob = public_key_blob(key);
    let check = 0x1234_5678u32.to_be_bytes();
    let mut private = Vec::new();
    private.extend_from_slice(&check);
    private.extend_from_slice(&check);
    private.extend(ssh_string(key.algorithm().as_str().as_bytes()));
    private.extend_from_slice(key_fields(&blob));
    private.push(0x01);
    private.extend(ssh_string(&[9; 64]));
    private.extend(ssh_string(b""));
    private.extend(ssh_string(b"test-key"));
    let mut pad = 1u8;
    while private.len() % 8 != 0 {
        private.push(pad);
        pad += 1;
    }
    let mut body = b"openssh-key-v1\0".to_vec();
    body.extend(ssh_string(cipher.as_bytes()));
    if cipher == "none" {
        body.extend(ssh_string(b"none"));
        body.extend(ssh_string(b""));
    } else {
        body.extend(ssh_string(b"bcrypt"));
        let mut kdf = ssh_string(&[0; 16]);
        kdf.extend_from_slice(&16u32.to_be_bytes());
        body.extend(ssh_string(&kdf));
    }
    body.extend_from_slice(&1u32.to_be_bytes());
    body.extend(ssh_string(&blob));
    body.extend(ssh_string(&private));
    let encoded = base64::engine::general_purpose::STANDARD.encode(body);
    let lines: Vec<&str> = encoded
        .as_bytes()
        .chunks(70)
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect();
    format!(
        "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
        lines.join("\n")
    )
}

pub(crate) fn shell_quote(value: impl AsRef<std::ffi::OsStr>) -> String {
    let value = value.as_ref().to_str().unwrap();
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Writes an executable shell script and waits until it can run: a process forked elsewhere in
/// this test binary may hold the new file open for a moment (ETXTBSY).
pub(crate) fn write_tool(path: &Path, body: &str) -> PathBuf {
    let script = format!("#!/bin/sh\n[ -n \"$MUXUS_FAKE_PROBE\" ] && exit 0\n{body}");
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    for _ in 0..200 {
        let probe = Command::new(path)
            .env("MUXUS_FAKE_PROBE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match probe {
            Ok(status) if status.success() => return path.to_path_buf(),
            Err(err) if err.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => panic!("{} does not run: {other:?}", path.display()),
        }
    }
    panic!("{} stayed busy", path.display());
}

/// Records how the tool was started, under `<dir>/<name>-args` and `<dir>/<name>-env`.
fn record_start(dir: &Path, name: &str) -> String {
    format!(
        "printf '%s\\n' \"$@\" > {}\nenv > {}\n",
        shell_quote(dir.join(format!("{name}-args"))),
        shell_quote(dir.join(format!("{name}-env")))
    )
}

/// An ssh-agent that serves the agent protocol (see [`fake_ssh_agent_process`]). `config` lines:
/// `refuse` fails every signature, `pin=<pin>` asks for a PIN first (`presence` with a touch),
/// `notify` sends a touch notification, `log=<line>` is logged when a signature fails.
pub(crate) fn fake_ssh_agent(dir: &Path, config: &[&str]) -> PathBuf {
    std::fs::write(dir.join("agent-config"), config.join("\n")).unwrap();
    let exe = std::env::current_exe().unwrap();
    let module = module_path!().split_once("::").unwrap().1;
    write_tool(
        &dir.join("ssh-agent"),
        &format!(
            "{}MUXUS_FAKE_SSH_AGENT={}\nexport MUXUS_FAKE_SSH_AGENT\nexec {} --exact {module}::fake_ssh_agent_process --ignored --nocapture --test-threads=1 -q\n",
            record_start(dir, "agent"),
            shell_quote(dir),
            shell_quote(exe),
        ),
    )
}

/// An ssh-agent that starts and then does nothing.
pub(crate) fn idle_ssh_agent(dir: &Path) -> PathBuf {
    write_tool(
        &dir.join("ssh-agent"),
        &format!(
            "{}echo \"SSH_AUTH_SOCK=$3; export SSH_AUTH_SOCK;\"\necho \"echo Agent pid $$;\"\nexec sleep 600\n",
            record_start(dir, "agent")
        ),
    )
}

/// An ssh-agent that runs `body` instead of starting.
pub(crate) fn failing_ssh_agent(dir: &Path, body: &str) -> PathBuf {
    write_tool(
        &dir.join("ssh-agent"),
        &format!("{}{body}", record_start(dir, "agent")),
    )
}

/// An ssh-add that runs `prelude`, then asks for `passphrase` through $SSH_ASKPASS the way
/// ssh-add does (until it is right, or a prompt fails), or loads the key straight away.
pub(crate) fn fake_ssh_add(dir: &Path, passphrase: Option<&str>, prelude: &str) -> PathBuf {
    let load = match passphrase {
        Some(passphrase) => format!(
            r#"prompt="Enter passphrase for $2: "
while :; do
  pass=$("$SSH_ASKPASS" "$prompt") || exit 1
  [ -n "$pass" ] || exit 1
  if [ "$pass" = {} ]; then echo "Identity added: $2 (test-key)" >&2; exit 0; fi
  prompt="Bad passphrase, try again for $2: "
done
"#,
            shell_quote(passphrase)
        ),
        None => "echo \"Identity added: $2 (test-key)\" >&2\n".to_string(),
    };
    write_tool(
        &dir.join("ssh-add"),
        &format!("{}{prelude}\n{load}", record_start(dir, "add")),
    )
}

#[test]
#[ignore = "started by the other tests as a stand-in ssh-agent"]
fn fake_ssh_agent_process() {
    if let Some(dir) = std::env::var_os("MUXUS_FAKE_SSH_AGENT") {
        serve_fake_agent(Path::new(&dir));
    }
}

fn serve_fake_agent(dir: &Path) {
    let args = std::fs::read_to_string(dir.join("agent-args")).unwrap();
    let args: Vec<&str> = args.lines().collect();
    let socket = args[args.iter().position(|arg| *arg == "-a").unwrap() + 1];
    let config = std::fs::read_to_string(dir.join("agent-config")).unwrap_or_default();
    let listener = UnixListener::bind(socket).unwrap();
    let mut stdout = std::io::stdout();
    write!(
        stdout,
        "SSH_AUTH_SOCK={socket}; export SSH_AUTH_SOCK;\necho Agent pid {};\n",
        std::process::id()
    )
    .unwrap();
    stdout.flush().unwrap();
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        while let Some(message) = read_message(&mut stream) {
            let reply = match message.first() {
                Some(13) => fake_sign(dir, &config, &message),
                _ => vec![5],
            };
            if write_message(&mut stream, &reply).is_err() {
                break;
            }
        }
    }
}

fn fake_sign(dir: &Path, config: &str, message: &[u8]) -> Vec<u8> {
    let setting = |name: &str| {
        config
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
    };
    let flag = |name: &str| config.lines().any(|line| line == name);
    let askpass = std::env::var_os("SSH_ASKPASS").unwrap();
    let (key_blob, _) = read_field(message, 1).unwrap();
    let (key_type, _) = read_field(key_blob, 0).unwrap();
    record(dir, &format!("sign {}", String::from_utf8_lossy(key_type)));
    if flag("notify") {
        // ssh-agent announces a touch this way and expects no answer.
        let status = Command::new(&askpass)
            .arg("Confirm user presence for key ED25519-SK SHA256:fake")
            .env("SSH_ASKPASS_PROMPT", "none")
            .stdout(Stdio::null())
            .status()
            .unwrap();
        record(dir, &format!("notify {}", status.code().unwrap_or(-1)));
    }
    let mut signed = !flag("refuse");
    if let Some(pin) = setting("pin") {
        let prompt = if flag("presence") {
            "Enter PIN and confirm user presence for ED25519-SK key SHA256:fake: "
        } else {
            "Enter PIN for ED25519-SK key SHA256:fake: "
        };
        let output = Command::new(&askpass)
            .arg(prompt)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .unwrap();
        let answer = String::from_utf8_lossy(&output.stdout);
        let answer = answer.trim_end_matches('\n');
        record(
            dir,
            &format!("pin {} {answer}", output.status.code().unwrap_or(-1)),
        );
        signed = signed && output.status.success() && answer == pin;
    }
    if !signed {
        if let Some(log) = setting("log") {
            let _ = writeln!(std::io::stderr(), "{log}");
        }
        return vec![5];
    }
    // A security key signature: the signature itself, then the flags and the counter.
    let mut signature = ssh_string(key_type);
    signature.extend(ssh_string(&[9; 64]));
    signature.push(0x01);
    signature.extend_from_slice(&0x0102_0304u32.to_be_bytes());
    let mut reply = vec![14];
    reply.extend(ssh_string(&signature));
    reply
}

fn record(dir: &Path, event: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("agent-events"))
        .unwrap();
    writeln!(file, "{event}").unwrap();
}

fn read_message(stream: &mut UnixStream) -> Option<Vec<u8>> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).ok()?;
    let mut message = vec![0; u32::from_be_bytes(length) as usize];
    stream.read_exact(&mut message).ok()?;
    Some(message)
}

fn write_message(stream: &mut UnixStream, message: &[u8]) -> std::io::Result<()> {
    let length = u32::try_from(message.len()).unwrap();
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(message)
}
