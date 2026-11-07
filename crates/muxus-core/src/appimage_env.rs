//! Environment cleanup for programs started from an AppImage (port of
//! `shared/src/appimage-env.ts`).

use std::collections::HashMap;

/// An AppImage puts its own mount first in PATH, XDG_DATA_DIRS and
/// LD_LIBRARY_PATH. Those entries must not reach programs of the host system:
/// a system binary would load the AppImage's libraries, and a space in the
/// mount's name breaks scripts that loop over these lists.
pub fn without_appimage_paths(env: &HashMap<String, String>) -> HashMap<String, String> {
    let mount = env.get("APPDIR").filter(|mount| !mount.is_empty());
    let appimage = env.get("APPIMAGE").filter(|path| !path.is_empty());
    let (Some(mount), Some(_)) = (mount, appimage) else {
        return env.clone();
    };
    let nested = format!("{mount}/");
    let mut cleaned = env.clone();
    for name in ["PATH", "XDG_DATA_DIRS", "LD_LIBRARY_PATH"] {
        let Some(value) = cleaned.get(name) else {
            continue;
        };
        let kept: Vec<&str> = value
            .split(':')
            .filter(|entry| !entry.is_empty() && entry != mount && !entry.starts_with(&nested))
            .collect();
        if kept.is_empty() {
            cleaned.remove(name);
        } else {
            let joined = kept.join(":");
            cleaned.insert(name.to_owned(), joined);
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn drops_the_mount_from_search_paths_only_inside_an_appimage() {
        let outside = env(&[("PATH", "/tmp/.mount_x/usr/bin:/usr/bin")]);
        assert_eq!(without_appimage_paths(&outside), outside);
        let inside = env(&[
            ("APPIMAGE", "/home/u/Muxus.AppImage"),
            ("APPDIR", "/tmp/.mount_x"),
            ("PATH", "/tmp/.mount_x/usr/bin:/usr/bin::/tmp/.mount_x"),
            ("LD_LIBRARY_PATH", "/tmp/.mount_x/usr/lib"),
            ("HOME", "/home/u"),
        ]);
        let cleaned = without_appimage_paths(&inside);
        assert_eq!(cleaned["PATH"], "/usr/bin");
        assert!(!cleaned.contains_key("LD_LIBRARY_PATH"));
        assert!(!cleaned.contains_key("XDG_DATA_DIRS"));
        assert_eq!(cleaned["HOME"], "/home/u");
    }
}
