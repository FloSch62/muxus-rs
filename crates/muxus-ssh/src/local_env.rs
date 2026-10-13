//! The local account facts ssh_config token expansion needs (`~`, `%d`, `%u`, `%i`, `%l`, ...).
//!
//! They are captured once and passed around explicitly, so tests can point `home` at a scratch
//! directory without touching the process environment.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEnv {
    /// The home directory `~` and `%d` expand to.
    pub home: PathBuf,
    /// The local account name (`%u`, and the default remote user).
    pub username: String,
    /// The local host name (`%l`, `%L`, `%C`).
    pub hostname: String,
    /// The local user id (`%i`); -1 where the platform has none.
    pub uid: i64,
}

impl LocalEnv {
    /// The current process's account, read the way Node's `os.homedir()` / `os.userInfo()` do:
    /// `HOME` first, then the password database.
    pub fn current() -> Self {
        let (pw_home, pw_name, uid) = account_info();
        let home = std::env::var_os(home_var())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or(pw_home)
            .unwrap_or_else(|| PathBuf::from("/"));
        let username = pw_name
            .or_else(|| std::env::var(user_var()).ok())
            .unwrap_or_else(|| "user".to_string());
        Self {
            home,
            username,
            hostname: local_hostname(),
            uid,
        }
    }

    /// A fixed environment for tests and sandboxed runs.
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            ..Self::current()
        }
    }

    /// `~/.ssh`.
    pub fn ssh_dir(&self) -> PathBuf {
        self.home.join(".ssh")
    }

    /// `~/.ssh/config`: the OpenSSH per-user config location on macOS, Linux and Windows.
    pub fn default_ssh_config_path(&self) -> PathBuf {
        self.ssh_dir().join("config")
    }
}

#[cfg(windows)]
fn home_var() -> &'static str {
    "USERPROFILE"
}

#[cfg(not(windows))]
fn home_var() -> &'static str {
    "HOME"
}

#[cfg(windows)]
fn user_var() -> &'static str {
    "USERNAME"
}

#[cfg(not(windows))]
fn user_var() -> &'static str {
    "USER"
}

#[cfg(unix)]
fn account_info() -> (Option<PathBuf>, Option<String>, i64) {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: getuid never fails and has no preconditions.
    let uid = unsafe { libc::getuid() };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to live, correctly sized storage owned by this frame.
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() {
        return (None, None, i64::from(uid));
    }
    // SAFETY: getpwuid_r succeeded, so the fields point at NUL-terminated strings inside `buf`.
    let (dir, name) = unsafe {
        (
            (!pwd.pw_dir.is_null()).then(|| CStr::from_ptr(pwd.pw_dir)),
            (!pwd.pw_name.is_null()).then(|| CStr::from_ptr(pwd.pw_name)),
        )
    };
    (
        dir.map(|d| PathBuf::from(std::ffi::OsStr::from_bytes(d.to_bytes()))),
        name.map(|n| n.to_string_lossy().into_owned()),
        i64::from(uid),
    )
}

#[cfg(not(unix))]
fn account_info() -> (Option<PathBuf>, Option<String>, i64) {
    (None, None, -1)
}

#[cfg(unix)]
fn local_hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname NUL-terminates on success.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return "localhost".to_string();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(not(unix))]
fn local_hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "localhost".to_string())
}
