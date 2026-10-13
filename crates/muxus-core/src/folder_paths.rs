//! Sidebar folder paths (port of `server/src/util/folder-paths.ts`, the
//! backend twin of the client's folder-path helpers).
//!
//! A folder is a `/`-separated path stored on host metadata; segments are
//! trimmed and matched case-insensitively. Folder settings are keyed by the
//! lowercased path so "Prod/EU" and "prod/eu" address the same folder.

use crate::js;

const FOLDER_SEPARATOR: &str = "/";

/// The trimmed, non-empty segments of a folder path.
pub fn folder_path_segments(path: &str) -> Vec<&str> {
    path.split(FOLDER_SEPARATOR)
        .map(js::js_trim)
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// The canonical stored form of a folder path; `""` means "no folder".
pub fn normalize_folder_path(path: &str) -> String {
    folder_path_segments(path).join(FOLDER_SEPARATOR)
}

/// Case-insensitive identity of a folder path.
pub fn folder_path_key(path: &str) -> String {
    folder_path_segments(path)
        .iter()
        .map(|segment| segment.to_lowercase())
        .collect::<Vec<_>>()
        .join(FOLDER_SEPARATOR)
}

/// The path itself and every ancestor, nearest first ("a/b/c", "a/b", "a").
pub fn folder_chain(path: &str) -> Vec<String> {
    let segments = folder_path_segments(path);
    (1..=segments.len())
        .rev()
        .map(|count| segments[..count].join(FOLDER_SEPARATOR))
        .collect()
}

/// Whether `candidate` sits strictly inside `ancestor`.
pub fn is_descendant_folder_path(candidate: &str, ancestor: &str) -> bool {
    let parent = folder_path_key(ancestor);
    let child = folder_path_key(candidate);
    !parent.is_empty() && child.starts_with(&format!("{parent}{FOLDER_SEPARATOR}"))
}

/// Where `path` lands when the folder `from` is renamed or moved to `to`.
pub fn rename_folder_path_under(path: &str, from: &str, to: &str) -> Option<String> {
    if folder_path_key(path) == folder_path_key(from) {
        return Some(normalize_folder_path(to));
    }
    if !is_descendant_folder_path(path, from) {
        return None;
    }
    let from_depth = folder_path_segments(from).len();
    let mut segments = folder_path_segments(to);
    segments.extend(folder_path_segments(path).into_iter().skip(from_depth));
    Some(segments.join(FOLDER_SEPARATOR))
}

/// The sidebar's identity of a folder (`folderKey` of `client/src/host-tree.ts`):
/// `folder:` and the case-insensitive path, so casing edits keep their identity.
pub fn sidebar_folder_key(path: &str) -> String {
    format!("folder:{}", folder_path_key(path))
}

/// The last segment of a path: the folder's own name.
pub fn folder_label(path: &str) -> String {
    folder_path_segments(path)
        .last()
        .map(|segment| segment.to_string())
        .unwrap_or_default()
}

/// How many folders deep a path is.
pub fn folder_depth(path: &str) -> usize {
    folder_path_segments(path).len()
}

/// The parent of a path; `""` for a top-level folder.
pub fn folder_parent_path(path: &str) -> String {
    let segments = folder_path_segments(path);
    segments[..segments.len().saturating_sub(1)].join(FOLDER_SEPARATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_keys_paths() {
        assert_eq!(normalize_folder_path(" Prod / EU //"), "Prod/EU");
        assert_eq!(folder_path_key(" Prod / EU "), "prod/eu");
        assert_eq!(folder_chain("a/b/c"), vec!["a/b/c", "a/b", "a"]);
        assert!(folder_chain("").is_empty());
    }

    #[test]
    fn moves_descendants_with_their_folder() {
        assert!(is_descendant_folder_path("Prod/EU/Edge", "prod"));
        assert!(!is_descendant_folder_path("Prod", "prod"));
        assert!(!is_descendant_folder_path("Production", "Prod"));
        assert_eq!(
            rename_folder_path_under("prod/eu", "Prod", "Live").as_deref(),
            Some("Live/eu")
        );
        assert_eq!(
            rename_folder_path_under("PROD", "prod", "Live/Main").as_deref(),
            Some("Live/Main")
        );
        assert_eq!(rename_folder_path_under("Dev", "Prod", "Live"), None);
    }
}
