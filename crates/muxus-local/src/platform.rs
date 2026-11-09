//! The operating system a decision is made for: the shared [`muxus_core::platform::Platform`].
//! Pure helpers take it as an argument so tests can exercise every platform on any machine.

use std::collections::HashMap;

pub use muxus_core::platform::Platform;

/// An environment variable by name. Windows matches names regardless of case and keeps
/// them as they were set (`ComSpec`), so there an exact match comes first, then any casing.
pub fn env_var<'a>(
    env: &'a HashMap<String, String>,
    name: &str,
    platform: Platform,
) -> Option<&'a String> {
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
pub fn set_env_var(
    env: &mut HashMap<String, String>,
    name: String,
    value: String,
    platform: Platform,
) {
    if platform == Platform::Windows {
        env.retain(|key, _| !key.eq_ignore_ascii_case(&name));
    }
    env.insert(name, value);
}

#[cfg(test)]
mod tests {
    use super::*;

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
