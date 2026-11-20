//! The computer a provider runs on: its environment and home directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The environment providers look programs up in and run them with. The process's own,
/// read when asked, plus changes laid over it: a test points `GNUPGHOME` at its key ring
/// this way without touching the environment other tests run in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostEnv {
    /// `Some` sets a variable, `None` removes it.
    overrides: BTreeMap<String, Option<String>>,
    home: Option<PathBuf>,
}

impl HostEnv {
    /// The process's own environment, unchanged.
    pub fn system() -> Self {
        Self::default()
    }

    /// With `name` set to `value` for everything looked up or run.
    pub fn with_var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.overrides.insert(name.into(), Some(value.into()));
        self
    }

    /// Without `name`, whatever the process's environment says.
    pub fn without_var(mut self, name: impl Into<String>) -> Self {
        self.overrides.insert(name.into(), None);
        self
    }

    /// With `~` standing for `home`.
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// A variable's value; an empty one counts as unset, as it does for `process.env.X || …`.
    pub fn var(&self, name: &str) -> Option<String> {
        let value = match self.overrides.get(name) {
            Some(value) => value.clone(),
            None => std::env::var(name).ok(),
        };
        value.filter(|value| !value.is_empty())
    }

    /// What is laid over the process's environment for a program that is run.
    pub(crate) fn overrides(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.overrides
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_deref()))
    }

    /// The home directory (`os.homedir()`).
    pub fn home(&self) -> PathBuf {
        if let Some(home) = &self.home {
            return home.clone();
        }
        let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        self.var(variable)
            .map(PathBuf::from)
            .or_else(std::env::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    /// A path with a leading `~` replaced by the home directory (`expandHome`).
    pub fn expand_home(&self, file: &str) -> String {
        if file == "~" {
            return self.home().to_string_lossy().into_owned();
        }
        match file.strip_prefix("~/").or_else(|| file.strip_prefix("~\\")) {
            Some(rest) => self.home().join(rest).to_string_lossy().into_owned(),
            None => file.to_owned(),
        }
    }
}

/// `path.resolve(base, path)`: absolute, with `.` and `..` worked out by name alone.
pub(crate) fn resolve_path(base: &Path, path: &Path) -> PathBuf {
    use std::path::Component;
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let joined = std::path::absolute(&joined).unwrap_or(joined);
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // The root has no parent to go to.
                if matches!(
                    resolved.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    resolved.pop();
                }
            }
            other => resolved.push(other),
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_changes_over_the_process_environment() {
        let host = HostEnv::system()
            .with_var("MUXUS_TEST_SET", "value")
            .with_var("MUXUS_TEST_EMPTY", "")
            .without_var("PATH");
        assert_eq!(host.var("MUXUS_TEST_SET").as_deref(), Some("value"));
        assert_eq!(host.var("MUXUS_TEST_EMPTY"), None);
        assert_eq!(host.var("PATH"), None);
        assert_eq!(host.var("MUXUS_TEST_NEVER_SET"), None);
        assert!(HostEnv::system().var("PATH").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn expands_a_leading_tilde_only() {
        let host = HostEnv::system().with_home("/home/me");
        assert_eq!(host.expand_home("~"), "/home/me");
        assert_eq!(
            host.expand_home("~/Passwords.kdbx"),
            "/home/me/Passwords.kdbx"
        );
        assert_eq!(host.expand_home("~other/x"), "~other/x");
        assert_eq!(host.expand_home("/srv/~/x"), "/srv/~/x");
        assert_eq!(host.expand_home(""), "");
    }

    #[cfg(unix)]
    #[test]
    fn resolves_paths_by_name() {
        let base = Path::new("/store");
        assert_eq!(
            resolve_path(base, Path::new("a/b.gpg")),
            Path::new("/store/a/b.gpg")
        );
        assert_eq!(
            resolve_path(base, Path::new("../x.gpg")),
            Path::new("/x.gpg")
        );
        assert_eq!(
            resolve_path(base, Path::new("a/./../b")),
            Path::new("/store/b")
        );
        assert_eq!(
            resolve_path(base, Path::new("/etc/passwd")),
            Path::new("/etc/passwd")
        );
        assert_eq!(resolve_path(base, Path::new("../../../x")), Path::new("/x"));
        assert_eq!(
            resolve_path(Path::new("/store/"), Path::new("")),
            Path::new("/store")
        );
    }
}
