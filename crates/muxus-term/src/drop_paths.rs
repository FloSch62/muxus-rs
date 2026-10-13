//! Dropped files typed into a terminal as paths, quoted for the shell that reads them: one word
//! per path, separated and followed by a space, the way desktop terminals insert them (port of
//! `client/src/terminal/drop-paths.ts`).

use std::sync::LazyLock;

use regex::Regex;

/// How a shell reads a quoted word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellQuoting {
    Posix,
    Fish,
    Powershell,
    Cmd,
}

/// The shell runs inside WSL: Windows paths are rewritten to where that distribution sees them.
/// No distribution means the default one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Wsl {
    pub distribution: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedPathStyle {
    pub quoting: ShellQuoting,
    pub wsl: Option<Wsl>,
}

impl DroppedPathStyle {
    pub fn new(quoting: ShellQuoting) -> Self {
        Self { quoting, wsl: None }
    }
}

static POSIX_SAFE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}_@%+=:,./-]+$").unwrap());
static WINDOWS_SAFE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}_:\\/.-]+$").unwrap());
static SYSTEM32_BASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[\\/]system32[\\/]bash(?:\.exe)?$").unwrap());
static WSL_DRIVE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^([a-z]):(?:[\\/](.*))?$").unwrap());
static WSL_SHARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\\\\(?:wsl\$|wsl\.localhost)\\([^\\]+)(\\.*)?$").unwrap());

fn is_control(c: char) -> bool {
    (c as u32) <= 0x1f || c as u32 == 0x7f
}

fn executable_name(shell: &str) -> String {
    let trimmed = shell.trim();
    let unquoted = if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    let last = unquoted
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    last.strip_suffix(".exe")
        .map(str::to_string)
        .unwrap_or(last)
}

fn wsl_distribution(args: &[&str]) -> Option<String> {
    let index = args
        .iter()
        .position(|arg| *arg == "-d" || *arg == "--distribution")?;
    args.get(index + 1).map(|value| value.to_string())
}

/// The quoting a local shell profile needs. `default_shell` is what the server starts when the
/// profile names none; `platform` is the server's (`linux`, `darwin`, `win32`).
pub fn local_shell_path_style(
    shell: Option<&str>,
    args: &[&str],
    default_shell: &str,
    platform: &str,
) -> DroppedPathStyle {
    let shell = shell
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_shell);
    let name = executable_name(shell);
    if name == "pwsh" || name == "powershell" {
        return DroppedPathStyle::new(ShellQuoting::Powershell);
    }
    if name == "fish" {
        return DroppedPathStyle::new(ShellQuoting::Fish);
    }
    if platform != "win32" {
        return DroppedPathStyle::new(ShellQuoting::Posix);
    }
    if name == "cmd" {
        return DroppedPathStyle::new(ShellQuoting::Cmd);
    }
    if name == "wsl" {
        return DroppedPathStyle {
            quoting: ShellQuoting::Posix,
            wsl: Some(Wsl {
                distribution: wsl_distribution(args),
            }),
        };
    }
    // bash.exe in System32 (or found there first) is the WSL launcher; Git Bash, MSYS2 and Cygwin
    // shells read Windows paths themselves.
    if name == "bash" && (!shell.contains(['\\', '/']) || SYSTEM32_BASH.is_match(shell)) {
        return DroppedPathStyle {
            quoting: ShellQuoting::Posix,
            wsl: Some(Wsl::default()),
        };
    }
    if ["bash", "sh", "zsh", "dash", "ksh", "mksh"].contains(&name.as_str()) {
        return DroppedPathStyle::new(ShellQuoting::Posix);
    }
    DroppedPathStyle::new(ShellQuoting::Cmd)
}

/// Where a WSL distribution sees a Windows path, e.g. `C:\Users` → `/mnt/c/Users`.
pub fn wsl_path(path: &str, distribution: Option<&str>) -> String {
    if let Some(drive) = WSL_DRIVE.captures(path) {
        let rest = drive.get(2).map_or("", |m| m.as_str()).replace('\\', "/");
        return format!("/mnt/{}/{rest}", drive[1].to_lowercase());
    }
    if let Some(share) = WSL_SHARE.captures(path)
        && distribution.is_none_or(|d| share[1].to_lowercase() == d.to_lowercase())
    {
        return share.get(2).map_or("\\", |m| m.as_str()).replace('\\', "/");
    }
    path.to_string()
}

fn hex(c: char) -> String {
    format!("\\x{:02x}", c as u32)
}

fn quote_posix(path: &str) -> String {
    if POSIX_SAFE.is_match(path) {
        return path.into();
    }
    if !path.chars().any(is_control) {
        return format!("'{}'", path.replace('\'', "'\\''"));
    }
    // A newline or tab inside plain quotes would reach the shell as typed input.
    let mut quoted = String::new();
    for c in path.chars() {
        if c == '\\' || c == '\'' {
            quoted.push('\\');
            quoted.push(c);
        } else if is_control(c) {
            quoted += &hex(c);
        } else {
            quoted.push(c);
        }
    }
    format!("$'{quoted}'")
}

fn quote_fish(path: &str) -> String {
    if POSIX_SAFE.is_match(path) {
        return path.into();
    }
    // fish has no escapes inside quotes but joins adjacent words, so control characters go between
    // quoted runs as escapes.
    let mut out = String::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if !run.is_empty() {
            *out += &format!("'{}'", run.replace('\\', "\\\\").replace('\'', "\\'"));
            run.clear();
        }
    };
    for c in path.chars() {
        if is_control(c) {
            flush(&mut run, &mut out);
            out += &hex(c);
        } else {
            run.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn quote_powershell(path: &str) -> String {
    if WINDOWS_SAFE.is_match(path) {
        return path.into();
    }
    // PowerShell also closes a single-quoted string on the typographic quotes.
    let mut quoted = String::from("'");
    for c in path.chars() {
        quoted.push(c);
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            quoted.push(c);
        }
    }
    quoted.push('\'');
    quoted
}

fn quote_cmd(path: &str) -> String {
    // Windows file names cannot contain double quotes.
    if WINDOWS_SAFE.is_match(path) {
        path.into()
    } else {
        format!("\"{path}\"")
    }
}

pub fn quote_path(path: &str, quoting: ShellQuoting) -> String {
    match quoting {
        ShellQuoting::Posix => quote_posix(path),
        ShellQuoting::Fish => quote_fish(path),
        ShellQuoting::Powershell => quote_powershell(path),
        ShellQuoting::Cmd => quote_cmd(path),
    }
}

/// The text a drop types: every path quoted, each followed by a space.
pub fn dropped_paths_text(paths: &[&str], style: &DroppedPathStyle) -> String {
    paths
        .iter()
        .map(|path| {
            let path = match &style.wsl {
                Some(wsl) => wsl_path(path, wsl.distribution.as_deref()),
                None => path.to_string(),
            };
            format!("{} ", quote_path(&path, style.quoting))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ShellQuoting::*;

    #[test]
    fn leaves_plain_paths_alone_and_single_quotes_the_rest() {
        assert_eq!(
            quote_path("/home/me/notes.txt", Posix),
            "/home/me/notes.txt"
        );
        assert_eq!(
            quote_path("/home/me/Größe_1.0+final@v2,x=y:z", Posix),
            "/home/me/Größe_1.0+final@v2,x=y:z"
        );
        assert_eq!(
            quote_path("/home/me/My Documents/a b.txt", Posix),
            "'/home/me/My Documents/a b.txt'"
        );
        assert_eq!(
            quote_path("/tmp/$HOME `id` *.log ~ & | ; ( ) < > ! # ?", Posix),
            "'/tmp/$HOME `id` *.log ~ & | ; ( ) < > ! # ?'"
        );
    }

    #[test]
    fn closes_and_reopens_the_quotes_around_an_apostrophe() {
        assert_eq!(
            quote_path("/home/me/it's here", Posix),
            "'/home/me/it'\\''s here'"
        );
    }

    #[test]
    fn spells_control_characters_out_so_a_newline_is_never_typed_as_enter() {
        assert_eq!(
            quote_path("/tmp/two\nlines", Posix),
            "$'/tmp/two\\x0alines'"
        );
        assert_eq!(
            quote_path("/tmp/tab\there's \\ back", Posix),
            "$'/tmp/tab\\x09here\\'s \\\\ back'"
        );
    }

    #[test]
    fn fish_escapes_backslashes_and_apostrophes_inside_the_quotes() {
        assert_eq!(quote_path("/srv/app/main.go", Fish), "/srv/app/main.go");
        assert_eq!(quote_path("/srv/it's a\\b", Fish), "'/srv/it\\'s a\\\\b'");
    }

    #[test]
    fn fish_puts_control_characters_between_quoted_runs() {
        assert_eq!(quote_path("/tmp/a\nb c", Fish), "'/tmp/a'\\x0a'b c'");
    }

    #[test]
    fn single_quotes_for_powershell_doubling_every_kind_of_single_quote() {
        assert_eq!(
            quote_path("C:\\Users\\me\\notes.txt", Powershell),
            "C:\\Users\\me\\notes.txt"
        );
        assert_eq!(
            quote_path("C:\\Program Files\\App $x\\a.txt", Powershell),
            "'C:\\Program Files\\App $x\\a.txt'"
        );
        assert_eq!(
            quote_path("C:\\Users\\me\\Bob's \u{2019}file\u{2019}.txt", Powershell),
            "'C:\\Users\\me\\Bob''s \u{2019}\u{2019}file\u{2019}\u{2019}.txt'"
        );
    }

    #[test]
    fn double_quotes_for_cmd() {
        assert_eq!(
            quote_path("C:\\Users\\me\\notes.txt", Cmd),
            "C:\\Users\\me\\notes.txt"
        );
        assert_eq!(
            quote_path("C:\\Program Files\\a&b.txt", Cmd),
            "\"C:\\Program Files\\a&b.txt\""
        );
    }

    #[test]
    fn moves_drive_paths_under_mnt() {
        assert_eq!(
            wsl_path("C:\\Users\\me\\My file.txt", None),
            "/mnt/c/Users/me/My file.txt"
        );
        assert_eq!(wsl_path("D:\\", None), "/mnt/d/");
        assert_eq!(wsl_path("e:", None), "/mnt/e/");
    }

    #[test]
    fn reads_the_distributions_own_files_from_its_network_share() {
        assert_eq!(
            wsl_path("\\\\wsl$\\Ubuntu\\home\\me\\a.txt", Some("Ubuntu")),
            "/home/me/a.txt"
        );
        assert_eq!(
            wsl_path("\\\\wsl.localhost\\Ubuntu\\home\\me", Some("ubuntu")),
            "/home/me"
        );
        assert_eq!(wsl_path("\\\\wsl.localhost\\Ubuntu", None), "/");
    }

    #[test]
    fn leaves_other_distributions_and_servers_shares_as_they_are() {
        assert_eq!(
            wsl_path("\\\\wsl$\\Debian\\home\\me", Some("Ubuntu")),
            "\\\\wsl$\\Debian\\home\\me"
        );
        assert_eq!(
            wsl_path("\\\\server\\share\\a.txt", None),
            "\\\\server\\share\\a.txt"
        );
    }

    #[test]
    fn follows_the_profile_then_the_default_shell() {
        assert_eq!(
            local_shell_path_style(None, &[], "/bin/zsh", "darwin"),
            DroppedPathStyle::new(Posix)
        );
        assert_eq!(
            local_shell_path_style(Some("/usr/bin/fish"), &[], "/bin/bash", "linux"),
            DroppedPathStyle::new(Fish)
        );
        assert_eq!(
            local_shell_path_style(Some(" "), &[], "/usr/local/bin/fish", "linux"),
            DroppedPathStyle::new(Fish)
        );
        assert_eq!(
            local_shell_path_style(
                Some("/opt/microsoft/powershell/7/pwsh"),
                &[],
                "/bin/bash",
                "linux"
            ),
            DroppedPathStyle::new(Powershell)
        );
    }

    #[test]
    fn tells_the_windows_shells_apart() {
        let cmd = "C:\\Windows\\system32\\cmd.exe";
        assert_eq!(
            local_shell_path_style(None, &[], cmd, "win32"),
            DroppedPathStyle::new(Cmd)
        );
        assert_eq!(
            local_shell_path_style(Some("powershell.exe"), &[], cmd, "win32"),
            DroppedPathStyle::new(Powershell)
        );
        assert_eq!(
            local_shell_path_style(
                Some("\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\""),
                &[],
                cmd,
                "win32"
            ),
            DroppedPathStyle::new(Powershell)
        );
        assert_eq!(
            local_shell_path_style(
                Some("C:\\Program Files\\Git\\bin\\bash.exe"),
                &[],
                cmd,
                "win32"
            ),
            DroppedPathStyle::new(Posix)
        );
        assert_eq!(
            local_shell_path_style(Some("nu.exe"), &[], cmd, "win32"),
            DroppedPathStyle::new(Cmd)
        );
    }

    #[test]
    fn recognizes_wsl_and_the_distribution_it_starts() {
        let cmd = "C:\\Windows\\system32\\cmd.exe";
        assert_eq!(
            local_shell_path_style(
                Some("wsl.exe"),
                &["-d", "Ubuntu", "--cd", "~"],
                cmd,
                "win32"
            ),
            DroppedPathStyle {
                quoting: Posix,
                wsl: Some(Wsl {
                    distribution: Some("Ubuntu".into())
                })
            }
        );
        assert_eq!(
            local_shell_path_style(Some("wsl"), &[], cmd, "win32"),
            DroppedPathStyle {
                quoting: Posix,
                wsl: Some(Wsl::default())
            }
        );
        assert_eq!(
            local_shell_path_style(Some("C:\\Windows\\System32\\bash.exe"), &[], cmd, "win32"),
            DroppedPathStyle {
                quoting: Posix,
                wsl: Some(Wsl::default())
            }
        );
    }

    #[test]
    fn quotes_every_path_and_ends_each_with_a_space() {
        assert_eq!(
            dropped_paths_text(
                &["/a/one.txt", "/a/two words.txt"],
                &DroppedPathStyle::new(Posix)
            ),
            "/a/one.txt '/a/two words.txt' "
        );
    }

    #[test]
    fn rewrites_windows_paths_for_a_wsl_shell_before_quoting_them() {
        let style = DroppedPathStyle {
            quoting: Posix,
            wsl: Some(Wsl {
                distribution: Some("Ubuntu".into()),
            }),
        };
        assert_eq!(
            dropped_paths_text(
                &[
                    "C:\\Users\\me\\a b.txt",
                    "\\\\wsl$\\Ubuntu\\home\\me\\c.txt"
                ],
                &style
            ),
            "'/mnt/c/Users/me/a b.txt' /home/me/c.txt "
        );
    }
}
