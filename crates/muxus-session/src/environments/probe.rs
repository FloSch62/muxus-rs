//! Running the host's own tools (`server/src/environments/probe-runner.ts`).
//!
//! A batch of probes becomes one POSIX sh script, so a remote host is asked
//! over a single exec channel of its live connection. The program of each
//! probe is looked up on the host's PATH first, so a missing tool reads as
//! "not installed" rather than as a failure.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use muxus_core::environments::shell_quote;
use regex::Regex;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::stats::RemoteCommand;

/// One command a discovery or lookup runs on a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub id: String,
    pub argv: Vec<String>,
}

impl Probe {
    pub fn new(id: impl Into<String>, argv: &[&str]) -> Self {
        Self {
            id: id.into(),
            argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeResult {
    Missing,
    Ran {
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
}

impl ProbeResult {
    pub fn ran(stdout: &str, exit_code: i32, stderr: &str) -> Self {
        ProbeResult::Ran {
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }
}

pub type ProbeResults = HashMap<String, ProbeResult>;

/// Runs a batch of probes on one host and reports each by id. Fails when the
/// host could not be asked at all (the channel was refused).
pub type ProbeFuture = Pin<Box<dyn Future<Output = Result<ProbeResults, String>> + Send>>;
pub type ProbeRunner = Arc<dyn Fn(Vec<Probe>) -> ProbeFuture + Send + Sync>;

/// One probe may wait on a slow cluster or daemon this long before it is stopped.
pub const PROBE_TIMEOUT_SECONDS: u64 = 12;
/// The whole batch, should the host have no `timeout` to stop one probe.
const BATCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Pod lists and topology files can be large, but not this large.
const MAX_OUTPUT: usize = 16 * 1024 * 1024;

const SECTION: &str = "@@muxus-env:";
const EXIT: &str = "@@muxus-env-exit:";
const MISSING: &str = "@@muxus-env-missing";

/// One POSIX sh script for the whole batch. stderr goes to a temporary file
/// and follows the exit marker, which keeps JSON on stdout clean of warnings.
/// Nothing stays behind.
pub fn probe_script(probes: &[Probe]) -> String {
    let mut lines: Vec<String> = vec![
        "LC_ALL=C".into(),
        "export LC_ALL".into(),
        r#"PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin:/snap/bin""#
            .into(),
        "export PATH".into(),
        r#"muxus_limit="""#.into(),
        format!(
            r#"if command -v timeout >/dev/null 2>&1; then muxus_limit="timeout {PROBE_TIMEOUT_SECONDS}"; fi"#
        ),
        r#"muxus_err=$(mktemp 2>/dev/null) || muxus_err="${TMPDIR:-/tmp}/muxus-env-$$""#.into(),
        "muxus_probe() {".into(),
        format!(r#"  printf '\n{SECTION}%s\n' "$1""#),
        "  shift".into(),
        r#"  if command -v "$1" >/dev/null 2>&1; then"#.into(),
        r#"    $muxus_limit "$@" </dev/null 2>"$muxus_err""#.into(),
        format!(r#"    printf '\n{EXIT}%s\n' "$?""#),
        r#"    cat "$muxus_err" 2>/dev/null"#.into(),
        "  else".into(),
        format!(r#"    printf '\n{MISSING}\n'"#),
        "  fi".into(),
        "}".into(),
    ];
    for probe in probes {
        let mut words = vec!["muxus_probe".to_string(), shell_quote(&probe.id)];
        words.extend(probe.argv.iter().map(|arg| shell_quote(arg)));
        lines.push(words.join(" "));
    }
    lines.push(r#"rm -f "$muxus_err""#.into());
    lines.push(String::new());
    lines.join("\n")
}

/// `Number.parseInt` of the leading integer of a line.
fn parse_exit_code(text: &str) -> Option<i32> {
    let text = muxus_core::js::js_trim_start(text);
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end].parse::<i32>().ok().map(|code| sign * code)
}

/// Split a batch's output back into its probes. Text before the first marker (a banner) is ignored.
pub fn parse_probe_output(output: &str) -> ProbeResults {
    let mut results = ProbeResults::new();
    let separator = format!("\n{SECTION}");
    for part in output.split(separator.as_str()).skip(1) {
        let (id, body) = match part.find('\n') {
            Some(newline) => (&part[..newline], &part[newline + 1..]),
            None => (part, ""),
        };
        let id = muxus_core::js::js_trim(id);
        if id.is_empty() {
            continue;
        }
        if muxus_core::js::js_trim_start(body).starts_with(MISSING) {
            results.insert(id.to_string(), ProbeResult::Missing);
            continue;
        }
        let exit_marker = format!("\n{EXIT}");
        let exit_start: Option<isize> = match body.rfind(exit_marker.as_str()) {
            Some(at) => Some(at as isize),
            None if body.starts_with(EXIT) => Some(-1),
            None => None,
        };
        let Some(exit_start) = exit_start else {
            // Cut off mid-probe: the batch timed out or the channel closed.
            results.insert(
                id.to_string(),
                ProbeResult::Ran {
                    exit_code: 124,
                    stdout: body.to_string(),
                    stderr: "timed out".into(),
                },
            );
            continue;
        };
        let stdout = if exit_start < 0 {
            ""
        } else {
            &body[..exit_start as usize]
        };
        let tail = &body[(exit_start + 1) as usize + EXIT.len()..];
        let (code_text, stderr) = match tail.find('\n') {
            Some(newline) => (&tail[..newline], &tail[newline + 1..]),
            None => (tail, ""),
        };
        results.insert(
            id.to_string(),
            ProbeResult::Ran {
                exit_code: parse_exit_code(code_text).unwrap_or(1),
                stdout: stdout.to_string(),
                stderr: muxus_core::js::js_trim(stderr).to_string(),
            },
        );
    }
    results
}

/// Read a script's stdout, up to the size limit, until it ends or the batch times out.
async fn collect(mut reader: impl AsyncRead + Unpin) -> String {
    let mut output = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let reading = async {
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if output.len() + read <= MAX_OUTPUT {
                        output.extend_from_slice(&buffer[..read]);
                    }
                }
            }
        }
    };
    let _ = tokio::time::timeout(BATCH_TIMEOUT, reading).await;
    String::from_utf8_lossy(&output).into_owned()
}

/// Probes over an exec channel on a live SSH transport; `sh -s` keeps the
/// login shell's syntax out of it.
pub fn remote_probe_runner(remote: Arc<dyn RemoteCommand>) -> ProbeRunner {
    Arc::new(move |probes| {
        let remote = remote.clone();
        Box::pin(async move {
            if probes.is_empty() {
                return Ok(ProbeResults::new());
            }
            let stdout = remote
                .exec("sh -s", probe_script(&probes).into_bytes())
                .await?;
            Ok(parse_probe_output(&collect(stdout).await))
        })
    })
}

/// Probes on the computer running Muxus, in the environment local terminals start from (the
/// login shell's PATH on a GUI launch; the sandbox's HOME in tests). `None` keeps this
/// process's environment.
pub fn local_probe_runner(env: Option<HashMap<String, String>>) -> ProbeRunner {
    let env = Arc::new(env);
    Arc::new(move |probes| {
        let env = env.clone();
        Box::pin(async move { Ok(run_local(probes, env.as_ref().as_ref()).await) })
    })
}

fn local_command(program: &str, env: Option<&HashMap<String, String>>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    if let Some(env) = env {
        command.env_clear().envs(env);
    }
    command
}

#[cfg(unix)]
async fn run_local(probes: Vec<Probe>, env: Option<&HashMap<String, String>>) -> ProbeResults {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    if probes.is_empty() {
        return ProbeResults::new();
    }
    let child = local_command("sh", env)
        .arg("-s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = child else {
        return ProbeResults::new();
    };
    let script = probe_script(&probes);
    if let Some(mut stdin) = child.stdin.take() {
        // The script is written while its output is read, so a full pipe never stalls it.
        tokio::spawn(async move {
            let _ = stdin.write_all(script.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    let output = match child.stdout.take() {
        Some(stdout) => collect(stdout).await,
        None => String::new(),
    };
    let _ = child.start_kill();
    let _ = child.wait().await;
    parse_probe_output(&output)
}

/// Windows has no sh: each probe runs on its own, side by side.
#[cfg(not(unix))]
async fn run_local(probes: Vec<Probe>, env: Option<&HashMap<String, String>>) -> ProbeResults {
    let runs = probes.into_iter().map(|probe| async move {
        let Some((file, args)) = probe.argv.split_first() else {
            return (probe.id, ProbeResult::Missing);
        };
        let mut command = local_command(file, env);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let ran =
            tokio::time::timeout(Duration::from_secs(PROBE_TIMEOUT_SECONDS), command.output())
                .await;
        let result = match ran {
            Err(_) => ProbeResult::Ran {
                exit_code: 124,
                stdout: String::new(),
                stderr: "timed out".into(),
            },
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => ProbeResult::Missing,
            Ok(Err(error)) => ProbeResult::Ran {
                exit_code: 1,
                stdout: String::new(),
                stderr: error.to_string(),
            },
            Ok(Ok(output)) => ProbeResult::Ran {
                exit_code: output.status.code().unwrap_or(1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: muxus_core::js::js_trim(&String::from_utf8_lossy(&output.stderr))
                    .to_string(),
            },
        };
        (probe.id, result)
    });
    futures_util::future::join_all(runs)
        .await
        .into_iter()
        .collect()
}

/// Log lines from Go's klog (kubectl) and locale warnings: not the reason itself.
static NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[IWEF]\d{4} \d\d:\d\d:\d\d|setlocale:|perl: warning:)").expect("noise pattern")
});
static ERROR_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^error:\s*").expect("error prefix"));

/// The tool's own words for why a probe failed, kept short enough for one row.
pub fn probe_failure(result: Option<&ProbeResult>) -> Option<String> {
    let ProbeResult::Ran {
        exit_code,
        stdout,
        stderr,
    } = result?
    else {
        return None;
    };
    if *exit_code == 0 {
        return None;
    }
    if *exit_code == 124 {
        return Some("did not answer in time".into());
    }
    let source = if stderr.is_empty() { stdout } else { stderr };
    // Tools end with the sentence meant for people: kubectl's "The connection
    // to the server … was refused", virsh's "authentication unavailable".
    let line = source
        .split('\n')
        .map(muxus_core::js::js_trim)
        .rfind(|candidate| !candidate.is_empty() && !NOISE.is_match(candidate))
        .map(|line| ERROR_PREFIX.replace(line, "").into_owned())
        .unwrap_or_else(|| format!("exit code {exit_code}"));
    if muxus_core::js::utf16_len(&line) > 240 {
        Some(format!("{}…", muxus_core::js::utf16_prefix(&line, 237)))
    } else {
        Some(line)
    }
}

/// stdout of a probe that ran and succeeded.
pub fn probe_output(result: Option<&ProbeResult>) -> Option<&str> {
    match result? {
        ProbeResult::Ran {
            exit_code: 0,
            stdout,
            ..
        } => Some(stdout),
        _ => None,
    }
}
