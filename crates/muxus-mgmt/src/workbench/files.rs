//! Paths of the gNOI File tool (the pure part of
//! `client/src/components/management/tools/FilesTool.tsx`).
//!
//! Paths are the device's own: `/var/log` on most, `cf3:\config.cfg` on
//! SR OS, which names its file systems and separates with backslashes.

use muxus_core::management::{GnmiModel, GrpcJson};
use serde_json::Value;

/// Where browsing starts and what an upload is created with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystem {
    pub home: String,
    /// Octal digits written as a decimal number (644 is rw-r--r--).
    pub permissions: u32,
}

/// SR OS has no `/` (its first flash card is cf3:) and takes only 555 or
/// 777 as permissions.
pub fn file_system_of(models: Option<&[GnmiModel]>) -> FileSystem {
    let sros = models.is_some_and(|models| models.iter().any(|model| model.name == "nokia-conf"));
    if sros {
        FileSystem {
            home: "cf3:\\".into(),
            permissions: 777,
        }
    } else {
        FileSystem {
            home: "/".into(),
            permissions: 644,
        }
    }
}

fn last_separator(path: &str) -> Option<usize> {
    path.rfind(['/', '\\'])
}

pub fn trim_separators(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

/// The top of a file system: `/` or `cf3:\`.
pub fn is_top(path: &str) -> bool {
    if path == "/" {
        return true;
    }
    match path.rfind(':') {
        Some(colon) if colon > 0 => {
            let (name, rest) = (&path[..colon], &path[colon + 1..]);
            !name.contains(['/', '\\']) && rest.chars().all(|c| c == '/' || c == '\\')
        }
        _ => false,
    }
}

pub fn basename(path: &str) -> String {
    let trimmed = trim_separators(path);
    let name = match last_separator(trimmed) {
        Some(index) => &trimmed[index + 1..],
        None => trimmed,
    };
    if name.is_empty() {
        path.to_owned()
    } else {
        name.to_owned()
    }
}

pub fn join_path(directory: &str, name: &str) -> String {
    let separator = if directory.contains('\\') { '\\' } else { '/' };
    format!("{}{separator}{name}", trim_separators(directory))
}

pub fn parent_of(path: &str) -> String {
    let trimmed = trim_separators(path);
    let Some(index) = last_separator(trimmed) else {
        return path.to_owned();
    };
    if index == 0 {
        return "/".into();
    }
    let parent = &trimmed[..index];
    // The top of a named file system keeps its separator: cf3:\, not cf3:.
    if parent.ends_with(':') {
        trimmed[..=index].to_owned()
    } else {
        parent.to_owned()
    }
}

/// A folder leading to a path, with the path that opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub path: String,
}

pub fn crumbs_of(path: &str) -> Vec<Crumb> {
    let separator = if path.contains('\\') { '\\' } else { '/' };
    let named = path
        .find(':')
        .is_some_and(|colon| colon > 0 && !path[..colon].contains(['/', '\\']));
    let mut crumbs: Vec<Crumb> = Vec::new();
    for label in path.split(['/', '\\']).filter(|part| !part.is_empty()) {
        let next = match crumbs.last() {
            None if named => format!("{label}{separator}"),
            None => format!("/{label}"),
            Some(previous) => join_path(&previous.path, label),
        };
        crumbs.push(Crumb {
            label: label.to_owned(),
            path: next,
        });
    }
    crumbs
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Unknown,
}

/// A directory entry from a File.Stat reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub size: Option<u64>,
    pub modified: Option<String>,
    pub permissions: Option<u64>,
    pub kind: EntryKind,
}

fn json_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_f64().map(|value| value as u64)),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

pub fn entry_from_stat(stat: &GrpcJson) -> FileEntry {
    let path = stat
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    FileEntry {
        name: basename(&path),
        size: stat.get("size").and_then(json_u64),
        modified: stat.get("last_modified").map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        }),
        permissions: stat.get("permissions").and_then(json_u64),
        // SR OS marks folders with a trailing separator; elsewhere a stat tells.
        kind: if path.ends_with(['/', '\\']) {
            EntryKind::Dir
        } else {
            EntryKind::Unknown
        },
        path,
    }
}

/// Directories are only told apart from files by statting them: a file stats as itself.
pub fn kind_from_stat(path: &str, children: &[FileEntry]) -> EntryKind {
    if children.len() == 1 && children[0].path == path {
        EntryKind::File
    } else {
        EntryKind::Dir
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn walks_posix_and_sros_paths() {
        assert_eq!(join_path("cf3:\\", "config.cfg"), "cf3:\\config.cfg");
        assert_eq!(join_path("/var/log/", "messages"), "/var/log/messages");
        assert_eq!(parent_of("cf3:\\logs\\x.log"), "cf3:\\logs");
        assert_eq!(parent_of("cf3:\\logs"), "cf3:\\");
        assert_eq!(parent_of("/var"), "/");
        assert_eq!(parent_of("/var/log"), "/var");
        assert!(is_top("cf3:\\") && is_top("/") && is_top("cf3:"));
        assert!(!is_top("cf3:\\logs"));
        assert_eq!(basename("cf3:\\logs\\"), "logs");
        assert_eq!(basename("/"), "/");
        assert_eq!(
            crumbs_of("cf3:\\logs\\old"),
            vec![
                Crumb {
                    label: "cf3:".into(),
                    path: "cf3:\\".into()
                },
                Crumb {
                    label: "logs".into(),
                    path: "cf3:\\logs".into()
                },
                Crumb {
                    label: "old".into(),
                    path: "cf3:\\logs\\old".into()
                },
            ]
        );
        assert_eq!(crumbs_of("/var/log")[1].path, "/var/log");
    }

    #[test]
    fn knows_sros_file_systems_and_permissions() {
        let sros = [GnmiModel {
            name: "nokia-conf".into(),
            organization: "Nokia".into(),
            version: "25.10".into(),
        }];
        assert_eq!(
            file_system_of(Some(&sros)),
            FileSystem {
                home: "cf3:\\".into(),
                permissions: 777
            }
        );
        assert_eq!(file_system_of(None).permissions, 644);
        let entry = entry_from_stat(
            json!({"path": "cf3:\\logs\\", "size": "1024", "permissions": 777, "last_modified": 1})
                .as_object()
                .unwrap(),
        );
        assert_eq!(entry.kind, EntryKind::Dir);
        assert_eq!(entry.name, "logs");
        assert_eq!(entry.size, Some(1024));
        let file = entry_from_stat(json!({"path": "/etc/hosts"}).as_object().unwrap());
        assert_eq!(
            kind_from_stat("/etc/hosts", std::slice::from_ref(&file)),
            EntryKind::File
        );
    }
}
