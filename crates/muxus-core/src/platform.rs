//! The operating system a decision is made for.
//!
//! Pure helpers take it as an argument so tests can exercise every platform on any machine; the
//! running build uses [`Platform::current`].

use std::collections::HashMap;

/// An operating system, as Node's `process.platform` tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    FreeBsd,
    /// Another Unix.
    Other,
}

impl Platform {
    /// The platform this binary was built for.
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "freebsd") {
            Self::FreeBsd
        } else {
            Self::Other
        }
    }

    /// Node's `process.platform` name (`win32`, `darwin`, `linux`, `freebsd`), which the
    /// renderer keys platform-specific behaviour on.
    pub const fn node_name(self) -> &'static str {
        match self {
            Self::Windows => "win32",
            Self::MacOs => "darwin",
            Self::Linux => "linux",
            Self::FreeBsd => "freebsd",
            Self::Other => {
                if cfg!(target_os = "openbsd") {
                    "openbsd"
                } else if cfg!(target_os = "netbsd") {
                    "netbsd"
                } else if cfg!(target_os = "solaris") || cfg!(target_os = "illumos") {
                    "sunos"
                } else {
                    "linux"
                }
            }
        }
    }
}

/// An environment variable by name, in a map taken from the environment. Windows matches
/// names regardless of case and keeps them as they were set (`ComSpec`, `Path`), and some
/// launchers (MSYS, Cygwin) hand them on in capitals, so there an exact match comes first,
/// then any casing. `std::env::var` already does this; a copied map does not.
pub fn env_var<'a, V>(
    env: &'a HashMap<String, V>,
    name: &str,
    platform: Platform,
) -> Option<&'a V> {
    env.get(name).or_else(|| {
        if platform != Platform::Windows {
            return None;
        }
        env.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}

/// Set an environment variable. On Windows the same name under another casing is the same
/// variable, so it goes first; otherwise a spawned program would get either value.
pub fn set_env_var<V>(env: &mut HashMap<String, V>, name: String, value: V, platform: Platform) {
    if platform == Platform::Windows {
        env.retain(|key, _| !key.eq_ignore_ascii_case(&name));
    }
    env.insert(name, value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_node() {
        assert_eq!(Platform::Windows.node_name(), "win32");
        assert_eq!(Platform::MacOs.node_name(), "darwin");
        assert_eq!(Platform::Linux.node_name(), "linux");
        if cfg!(target_os = "linux") {
            assert_eq!(Platform::current(), Platform::Linux);
        }
    }

    fn env(vars: &[(&str, &str)]) -> HashMap<String, String> {
        vars.iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn reads_a_windows_variable_under_any_casing() {
        let vars = env(&[("ComSpec", r"C:\Windows\system32\cmd.exe")]);
        assert_eq!(
            env_var(&vars, "COMSPEC", Platform::Windows).map(String::as_str),
            Some(r"C:\Windows\system32\cmd.exe")
        );
        assert_eq!(env_var(&vars, "COMSPEC", Platform::Linux), None);
        let both = env(&[("Path", "other"), ("PATH", "exact")]);
        assert_eq!(
            env_var(&both, "PATH", Platform::Windows).map(String::as_str),
            Some("exact")
        );
    }

    #[test]
    fn replaces_a_windows_variable_under_any_casing() {
        let mut vars = env(&[("Prompt", "$G"), ("HOME", "/h")]);
        set_env_var(&mut vars, "PROMPT".into(), "$P$G".into(), Platform::Windows);
        assert_eq!(vars, env(&[("PROMPT", "$P$G"), ("HOME", "/h")]));
        let mut vars = env(&[("Prompt", "$G")]);
        set_env_var(&mut vars, "PROMPT".into(), "$P$G".into(), Platform::Linux);
        assert_eq!(vars, env(&[("Prompt", "$G"), ("PROMPT", "$P$G")]));
    }
}
