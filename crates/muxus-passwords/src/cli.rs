//! Running a password manager's command-line program (`cli.ts`).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::host::HostEnv;
use crate::provider::{PasswordManagerError, PasswordManagerProblem, PasswordManagerResult};

/// Long enough for a listing of a large vault on a slow machine.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(60);
/// A read can sit behind a fingerprint or pinentry prompt the person has to answer.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(15);

const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// What a program printed and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Default)]
pub struct CliOptions {
    /// Added to the environment for this run; `None` removes a variable.
    pub env: Vec<(String, Option<String>)>,
    /// [`LIST_TIMEOUT`] when left out.
    pub timeout: Option<Duration>,
    /// Written to stdin, which is then closed. Stdin is closed at once without it.
    pub input: Option<String>,
}

impl CliOptions {
    pub fn timeout(timeout: Duration) -> Self {
        Self {
            timeout: Some(timeout),
            ..Self::default()
        }
    }

    pub fn with_env(mut self, name: &str, value: Option<&str>) -> Self {
        self.env.push((name.to_owned(), value.map(str::to_owned)));
        self
    }
}

/// Run a password manager's command-line program and collect what it prints. Nothing goes
/// through a shell, stdin is never a terminal, and a run that outlives its timeout is
/// killed, so a program waiting for input cannot hang a connection attempt for good.
pub async fn run_cli<S: AsRef<str>>(
    host: &HostEnv,
    program: &str,
    args: &[S],
    options: CliOptions,
) -> PasswordManagerResult<CliResult> {
    let Some(resolved) = resolve_program(host, program) else {
        return Err(PasswordManagerError::unavailable(format!(
            "{program} was not found on this computer."
        )));
    };
    // Batch files only run through cmd.exe, whose quoting cannot carry every argument
    // safely; refuse the ones it would reinterpret.
    if cfg!(windows)
        && is_batch_file(&resolved)
        && args.iter().any(|arg| {
            arg.as_ref()
                .contains(['"', '%', '!', '^', '&', '|', '<', '>', '\r', '\n'])
        })
    {
        return Err(PasswordManagerError::failed(format!(
            "{program} cannot be given this value on Windows."
        )));
    }

    let mut command = Command::new(&resolved);
    command
        .args(args.iter().map(AsRef::as_ref))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let changes = host
        .overrides()
        .map(|(name, value)| (name.to_owned(), value.map(str::to_owned)))
        .chain(options.env);
    for (name, value) in changes {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    #[cfg(windows)]
    {
        // No console window flashes up behind Muxus.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn().map_err(|err| {
        PasswordManagerError::unavailable(format!("{program} could not be started: {err}"))
    })?;
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let input = options.input.unwrap_or_default();

    let run = async {
        let write = async {
            if let Some(mut stdin) = stdin.take() {
                // A program that exits before reading its input closes the pipe first.
                let _ = stdin.write_all(input.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }
            Ok(())
        };
        // The first pipe over the limit ends the run: nobody reads the other any longer.
        let ((), stdout, stderr) = tokio::try_join!(write, collect(stdout), collect(stderr))?;
        if stdout.len() + stderr.len() > MAX_OUTPUT_BYTES {
            return Err(TooMuchOutput);
        }
        Ok((stdout, stderr))
    };
    let timeout = options.timeout.unwrap_or(LIST_TIMEOUT);
    let collected = tokio::time::timeout(timeout, async {
        let output = run.await;
        (output, child.wait().await)
    })
    .await;
    match collected {
        Err(_) => {
            let _ = child.kill().await;
            Err(PasswordManagerError::new(
                PasswordManagerProblem::Timeout,
                format!(
                    "{} did not answer in time. If it is waiting for you to approve or unlock, do that and try again.",
                    basename(program)
                ),
            ))
        }
        Ok((Err(TooMuchOutput), _)) => {
            let _ = child.kill().await;
            Err(PasswordManagerError::failed(format!(
                "{program} printed too much output."
            )))
        }
        Ok((Ok((stdout, stderr)), status)) => Ok(CliResult {
            code: status.ok().and_then(|status| status.code()).unwrap_or(1),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }),
    }
}

struct TooMuchOutput;

/// Everything a pipe carries, up to the output limit.
async fn collect(pipe: Option<impl AsyncRead + Unpin>) -> Result<Vec<u8>, TooMuchOutput> {
    let mut collected = Vec::new();
    let Some(mut pipe) = pipe else {
        return Ok(collected);
    };
    let mut chunk = vec![0; 64 * 1024];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => return Ok(collected),
            Ok(read) => {
                if collected.len() + read > MAX_OUTPUT_BYTES {
                    return Err(TooMuchOutput);
                }
                collected.extend_from_slice(&chunk[..read]);
            }
        }
    }
}

fn is_batch_file(program: &Path) -> bool {
    program
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

/// `path.basename`: what follows the last separator.
pub(crate) fn basename(file: &str) -> &str {
    let separators: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    file.trim_end_matches(separators)
        .rsplit(separators)
        .next()
        .unwrap_or(file)
}

/// Whether a program can be found, for the "installed" hint in the settings.
pub fn program_installed(host: &HostEnv, program: &str) -> bool {
    resolve_program(host, program).is_some()
}

/// The file a program name stands for: a path as given, otherwise the first match on PATH,
/// trying the Windows executable extensions there.
pub fn resolve_program(host: &HostEnv, program: &str) -> Option<PathBuf> {
    let extensions: Vec<String> = if cfg!(windows) {
        let listed = host
            .var("PATHEXT")
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_owned());
        std::iter::once(String::new())
            .chain(
                listed
                    .split(';')
                    .filter(|extension| !extension.is_empty())
                    .map(str::to_owned),
            )
            .collect()
    } else {
        vec![String::new()]
    };
    let is_path = program.contains('/') || program.contains(std::path::MAIN_SEPARATOR);
    let directories: Vec<PathBuf> = if is_path {
        vec![PathBuf::new()]
    } else {
        let path = host.var("PATH").unwrap_or_default();
        std::env::split_paths(&path)
            .filter(|directory| !directory.as_os_str().is_empty())
            .collect()
    };
    for directory in &directories {
        for extension in &extensions {
            let candidate = directory.join(format!("{program}{extension}"));
            if runnable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn runnable(candidate: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(candidate)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn runnable(candidate: &Path) -> bool {
    candidate.is_file()
}

/// Color codes, which lpass and op print even to a pipe.
static ANSI_COLORS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("\u{1b}\\[[0-9;]*m").expect("a valid pattern"));
/// "[ERROR] 2024/01/01 12:00:00 message", as op prints it.
static OP_ERROR_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[ERROR\]\s+[0-9]{4}/[0-9]{2}/[0-9]{2}\s+[0-9]{2}:[0-9]{2}:[0-9]{2}\s+")
        .expect("a valid pattern")
});

/// The last thing a program said on stderr, without log prefixes or colors.
pub fn cli_message(result: &CliResult, fallback: &str) -> String {
    let stderr = muxus_core::js::js_trim(&result.stderr);
    let said = if stderr.is_empty() {
        muxus_core::js::js_trim(&result.stdout)
    } else {
        stderr
    };
    let plain = ANSI_COLORS.replace_all(said, "");
    let Some(last) = plain
        .split('\n')
        .map(muxus_core::js::js_trim)
        .rfind(|line| !line.is_empty())
    else {
        return fallback.to_owned();
    };
    let message = OP_ERROR_PREFIX.replace(last, "");
    muxus_core::js::utf16_prefix(&message, 400).to_owned()
}

/// Parse a program's JSON output, or say that it printed something else. Empty output
/// reads as `empty`, for the listings that print nothing when there is nothing.
pub(crate) fn parse_json(
    program: &str,
    output: &str,
    empty: Option<&str>,
) -> PasswordManagerResult<serde_json::Value> {
    let text = match empty {
        Some(empty) if output.is_empty() => empty,
        _ => output,
    };
    serde_json::from_str(text).map_err(|_| {
        PasswordManagerError::failed(format!("{program} printed something Muxus could not read."))
    })
}

/// A field that holds text which is not empty (`item.id` where the TypeScript tested
/// `!item.id`).
pub(crate) fn text_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(stdout: &str, stderr: &str) -> CliResult {
        CliResult {
            code: 1,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }
    }

    #[test]
    fn reports_the_last_line_a_program_said_without_colors_and_log_prefixes() {
        assert_eq!(
            cli_message(
                &said(
                    "",
                    "first line\r\n\u{1b}[1;31mError\u{1b}[0m: Could not find specified account(s).\n\n"
                ),
                "fallback"
            ),
            "Error: Could not find specified account(s)."
        );
        assert_eq!(
            cli_message(
                &said(
                    "",
                    "[ERROR] 2026/01/02 03:04:05 \"x\" isn't an item in the \"y\" vault"
                ),
                "fallback"
            ),
            "\"x\" isn't an item in the \"y\" vault"
        );
        // stdout speaks when stderr said nothing.
        assert_eq!(
            cli_message(&said("Not found.\n", "  \n"), "fallback"),
            "Not found."
        );
        assert_eq!(cli_message(&said("", ""), "fallback"), "fallback");
        assert_eq!(
            cli_message(&said("", &"x".repeat(900)), "fallback").len(),
            400
        );
    }

    #[test]
    fn reads_json_and_says_so_when_a_program_printed_something_else() {
        assert_eq!(
            parse_json("op", "", Some("[]")).unwrap(),
            serde_json::json!([])
        );
        assert_eq!(
            parse_json("bw", "{\"status\":\"locked\"}", None).unwrap()["status"],
            "locked"
        );
        let error = parse_json("bw", "", None).unwrap_err();
        assert_eq!(error.problem, PasswordManagerProblem::Failed);
        assert_eq!(error.message, "bw printed something Muxus could not read.");
    }

    #[test]
    fn names_a_program_by_its_file() {
        assert_eq!(basename("/opt/1password/op"), "op");
        assert_eq!(basename("op"), "op");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn finds_programs_on_the_path_and_by_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("fake-op");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let data = dir.path().join("not-a-program");
        std::fs::write(&data, "data").unwrap();
        std::fs::create_dir(dir.path().join("a-directory")).unwrap();

        let host = HostEnv::system().with_var("PATH", dir.path().to_string_lossy());
        assert_eq!(resolve_program(&host, "fake-op"), Some(program.clone()));
        assert!(program_installed(&host, "fake-op"));
        // A file that may not be run and a directory are not programs.
        assert!(!program_installed(&host, "not-a-program"));
        assert!(!program_installed(&host, "a-directory"));
        assert!(!program_installed(&host, "missing"));
        // A path is taken as given, whatever PATH says.
        let nowhere = HostEnv::system().with_var("PATH", "");
        assert_eq!(
            resolve_program(&nowhere, &program.to_string_lossy()),
            Some(program)
        );
        assert!(!program_installed(&nowhere, "fake-op"));
    }
}
