//! The shell wrapper a script runs in on the host (`SCRIPT_WRAPPER`, `killJobCommand` and
//! `scriptCommand` in `server/src/scripts/script-runs.ts`).

use muxus_core::environments::shell_quote;

/// Runs the script from stdin as a temporary file, so it may be any size and may itself read
/// stdin (which is then empty). A `#!` line picks the interpreter, as running the file would;
/// otherwise POSIX sh. The file is removed when the run ends. Exit 125 means the script never
/// started.
///
/// sshd starts each exec session in a session of its own, so the run — this wrapper, the
/// script and whatever it starts — is one process group. The wrapper reports that group on
/// stderr before the script starts, so a cancel can stop all of it; closing the channel alone
/// would not. Without mktemp, the fallback file is created exclusively and private.
pub const SCRIPT_WRAPPER: &str = concat!(
    // A TMPDIR that points nowhere should not stop the script: try /tmp too.
    "muxus_f=$(mktemp 2>/dev/null) || muxus_f=$(mktemp /tmp/muxus-script.XXXXXX 2>/dev/null) || {\n",
    "  muxus_f=\"/tmp/muxus-script-$$\"\n",
    "  (umask 077; set -C; : > \"$muxus_f\") 2>/dev/null || exit 125\n",
    "}\n",
    "trap 'rm -f \"$muxus_f\"' EXIT\n",
    "trap 'exit 143' HUP INT TERM\n",
    "cat > \"$muxus_f\" || exit 125\n",
    "muxus_g=$(ps -o pgid= -p $$ 2>/dev/null | tr -d ' ')\n",
    "printf '\\036muxus-job %s\\n' \"${muxus_g:-$$}\" >&2\n",
    "IFS= read -r muxus_first < \"$muxus_f\" || muxus_first=\n",
    "case \"$muxus_first\" in\n",
    "  '#!'*) muxus_interp=${muxus_first#??}; $muxus_interp \"$muxus_f\" </dev/null ;;\n",
    "  *) sh \"$muxus_f\" </dev/null ;;\n",
    "esac",
);

/// Written to stderr before the script runs: the run's process group (`JOB_MARKER`).
pub(crate) const JOB_MARKER_PREFIX: &str = "\u{1e}muxus-job ";

/// Stop a run's process group: TERM, then KILL if it is still there.
pub fn kill_job_command(group: u32) -> String {
    let kill = |signal: &str| {
        format!("kill -{signal} -{group} 2>/dev/null || kill -{signal} {group} 2>/dev/null")
    };
    format!(
        "{}; (sleep 5; {}) </dev/null >/dev/null 2>&1 &",
        kill("TERM"),
        kill("KILL")
    )
}

/// The exec command of every run: the wrapper, with the script on stdin.
pub fn script_command() -> String {
    format!("sh -c {}", shell_quote(SCRIPT_WRAPPER))
}

/// The job marker in `stderr`: where it starts, its length and the process group it names.
pub(crate) fn find_job_marker(stderr: &str) -> Option<(usize, usize, u32)> {
    let mut from = 0;
    while let Some(found) = stderr[from..].find(JOB_MARKER_PREFIX) {
        let start = from + found;
        let digits_at = start + JOB_MARKER_PREFIX.len();
        let digits = stderr[digits_at..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let end = digits_at + digits;
        if digits > 0 && stderr[end..].starts_with('\n') {
            let group = stderr[digits_at..end].parse().unwrap_or(0);
            return Some((start, end + 1 - start, group));
        }
        from = digits_at;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wrapper_matches_the_typescript_one() {
        assert!(SCRIPT_WRAPPER.starts_with("muxus_f=$(mktemp 2>/dev/null) || "));
        assert!(SCRIPT_WRAPPER.contains("printf '\\036muxus-job %s\\n' \"${muxus_g:-$$}\" >&2\n"));
        assert!(SCRIPT_WRAPPER.ends_with("esac"));
        assert_eq!(SCRIPT_WRAPPER.lines().count(), 14);
        assert!(script_command().starts_with("sh -c 'muxus_f="));
    }

    #[test]
    fn kills_the_group_then_the_process() {
        assert_eq!(
            kill_job_command(42),
            "kill -TERM -42 2>/dev/null || kill -TERM 42 2>/dev/null; \
             (sleep 5; kill -KILL -42 2>/dev/null || kill -KILL 42 2>/dev/null) \
             </dev/null >/dev/null 2>&1 &"
        );
    }

    #[test]
    fn finds_the_job_marker() {
        assert_eq!(
            find_job_marker("oops\n\u{1e}muxus-job 1234\nrest"),
            Some((5, 16, 1234))
        );
        assert_eq!(find_job_marker("\u{1e}muxus-job 12"), None);
        assert_eq!(find_job_marker("\u{1e}muxus-job x\n"), None);
    }
}
