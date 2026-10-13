//! Installed Windows Subsystem for Linux distributions, read from the
//! registry (`server/src/local/wsl.ts`). The parsing is platform-neutral so it
//! can be tested anywhere; only the registry query itself is Windows-only.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::LazyLock;

use base64::Engine as _;
use regex::Regex;

use crate::platform::Platform;
use crate::profile::WslDistribution;

/// Where WSL registers each distribution for the current user, one subkey per
/// distribution. Windows Terminal discovers its WSL profiles from here too.
pub const LXSS_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

/// Docker Desktop and Rancher Desktop keep their engines in WSL distributions
/// that are not meant to be entered, and Windows Terminal hides them as well.
static UTILITY_DISTRIBUTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:docker-desktop|rancher-desktop)").expect("valid regex"));
static REG_KEY_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^HKEY_[A-Z_]+\\").expect("valid regex"));
static REG_VALUE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ {4}(.+?) {4}(REG_[A-Z_]+)(?: {4}(.*))?$").expect("valid regex")
});

/// Where WSL keeps the icon a distribution ships for its Start menu shortcut
/// and Windows Terminal profile, inside the distribution's own folder.
const ICON_FILE: &str = "shortcut.ico";
/// Distribution icons are tens of kilobytes; anything far larger is not one.
const MAX_ICON_BYTES: u64 = 256 * 1024;
const ICO_HEADER: [u8; 4] = [0, 0, 1, 0];

/// One registered distribution, with the folder WSL keeps its files in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslRegistration {
    pub name: String,
    pub is_default: bool,
    pub base_path: Option<String>,
}

fn lxss_root() -> String {
    format!(r"HKEY_CURRENT_USER\{LXSS_PATH}").to_lowercase()
}

/// A name that did not survive the console code page, or could not be passed
/// to wsl.exe intact, is left out rather than listed and failing to launch.
fn unusable_name(name: &str) -> bool {
    name.chars()
        .any(|ch| ch <= '\u{1f}' || ch == '\u{7f}' || ch == '\u{fffd}')
}

/// Parse `reg query <Lxss> /s` output into launchable distributions: the
/// default first, then by name.
pub fn parse_wsl_registry(output: &str) -> Vec<WslRegistration> {
    let root = lxss_root();
    // Values belong to the Lxss key itself (`Some("")`) or to one distribution
    // subkey, named by the distribution's GUID; anything else is skipped.
    let mut id: Option<String> = None;
    let mut default_id: Option<String> = None;
    let mut names: Vec<(String, String)> = Vec::new();
    let mut base_paths: Vec<(String, String)> = Vec::new();
    for line in output
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
    {
        if REG_KEY_LINE.is_match(line) {
            let key = line.trim().to_lowercase();
            let child = key.strip_prefix(&format!("{root}\\"));
            id = if key == root {
                Some(String::new())
            } else {
                child
                    .filter(|child| !child.contains('\\'))
                    .map(str::to_string)
            };
            continue;
        }
        let Some(current) = id.as_deref() else {
            continue;
        };
        let Some(captures) = REG_VALUE_LINE.captures(line) else {
            continue;
        };
        if &captures[2] != "REG_SZ" {
            continue;
        }
        let value_name = &captures[1];
        let data = captures.get(3).map_or("", |data| data.as_str()).trim();
        if current.is_empty() && value_name == "DefaultDistribution" {
            default_id = Some(data.to_lowercase());
        } else if !current.is_empty() && value_name == "DistributionName" {
            upsert(&mut names, current, data);
        } else if !current.is_empty() && value_name == "BasePath" && !data.is_empty() {
            upsert(&mut base_paths, current, data);
        }
    }

    let mut seen = HashSet::new();
    let mut distributions = Vec::new();
    for (guid, name) in names {
        if name.is_empty() || name.encode_utf16().count() > 256 || unusable_name(&name) {
            continue;
        }
        if UTILITY_DISTRIBUTION.is_match(&name) || !seen.insert(name.to_lowercase()) {
            continue;
        }
        let base_path = base_paths
            .iter()
            .find(|(id, _)| *id == guid)
            .map(|(_, path)| path.clone());
        distributions.push(WslRegistration {
            is_default: default_id.as_deref() == Some(guid.as_str()),
            name,
            base_path,
        });
    }
    distributions.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| natural_cmp_base(&a.name, &b.name))
    });
    distributions
}

/// Map insertion that keeps first-insertion order, as a JavaScript `Map` does.
fn upsert(entries: &mut Vec<(String, String)>, key: &str, value: &str) {
    match entries.iter_mut().find(|(existing, _)| existing == key) {
        Some(entry) => entry.1 = value.to_string(),
        None => entries.push((key.to_string(), value.to_string())),
    }
}

/// `localeCompare` with `{ numeric: true, sensitivity: 'base' }`: case is
/// ignored and digit runs compare by their value.
pub(crate) fn natural_cmp_base(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut digits = String::new();
                    while let Some(ch) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(ch);
                        chars.next();
                    }
                    digits
                };
                let (x, y) = (take(&mut left), take(&mut right));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                left.next();
                right.next();
            }
        }
    }
}

/// The icon a distribution in WSL's own package format ships, as a data URL.
/// WSL copies it into the distribution's folder and points its Start menu
/// shortcut and Windows Terminal profile at it; Store-packaged distributions
/// keep theirs inside the app package and have none here.
pub fn read_wsl_distribution_icon(base_path: &Path) -> Option<String> {
    let icon_path = base_path.join(ICON_FILE);
    if std::fs::metadata(&icon_path).ok()?.len() > MAX_ICON_BYTES {
        return None;
    }
    let icon = std::fs::read(&icon_path).ok()?;
    if !icon.starts_with(&ICO_HEADER) {
        return None;
    }
    Some(format!(
        "data:image/x-icon;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(icon)
    ))
}

/// The WSL distributions for `platform`, from a registry query and an icon
/// reader. Nothing is queried off Windows; a failed query (the Lxss key does
/// not exist when WSL was never set up) lists nothing.
pub fn list_wsl_distributions_with(
    platform: Platform,
    query: impl FnOnce() -> io::Result<String>,
    read_icon: impl Fn(&Path) -> Option<String>,
) -> Vec<WslDistribution> {
    if platform != Platform::Windows {
        return Vec::new();
    }
    let Ok(output) = query() else {
        return Vec::new();
    };
    parse_wsl_registry(&output)
        .into_iter()
        .map(|registration| WslDistribution {
            icon: registration
                .base_path
                .as_deref()
                .and_then(|path| read_icon(Path::new(path))),
            name: registration.name,
            is_default: registration.is_default,
        })
        .collect()
}

/// The WSL distributions installed for the current user. Reading the registry
/// never starts WSL, and on a system without it the key simply does not
/// exist, where the wsl.exe stub could offer to install WSL and wait for a key
/// press. Blocking: run it off the UI thread. Read on every call, so a
/// distribution installed or removed while Muxus runs shows up next time.
pub fn list_wsl_distributions() -> Vec<WslDistribution> {
    list_wsl_distributions_with(
        Platform::current(),
        query_lxss_key,
        read_wsl_distribution_icon,
    )
}

#[cfg(windows)]
fn query_lxss_key() -> io::Result<String> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const MAX_OUTPUT: usize = 1024 * 1024;
    // An absolute path, so a reg.exe in the working directory or on PATH is never run.
    let system_root = std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .unwrap_or_else(|_| r"C:\Windows".to_string());
    let reg = Path::new(&system_root).join("System32").join("reg.exe");
    let mut child = Command::new(reg)
        .args(["query", &format!(r"HKCU\{LXSS_PATH}"), "/s"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = (&mut stdout)
            .take(MAX_OUTPUT as u64 + 1)
            .read_to_end(&mut output);
        output
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "reg.exe timed out"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = reader.join().unwrap_or_default();
    if output.len() > MAX_OUTPUT {
        return Err(io::Error::other("reg.exe output too large"));
    }
    if !status.success() {
        return Err(io::Error::other(format!("reg.exe exited with {status}")));
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(not(windows))]
fn query_lxss_key() -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "WSL exists only on Windows",
    ))
}
