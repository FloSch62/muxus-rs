//! POSIX path helpers with Node's `path.posix` semantics, which the TypeScript routes used for
//! remote paths.

/// `path.posix.normalize`: collapses `.`, `..` and repeated slashes, keeps a trailing slash, and
/// answers `.` for an empty relative path.
pub fn normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let mut out = parts.join("/");
    if out.is_empty() && !absolute {
        out.push('.');
    }
    if trailing && !out.is_empty() {
        out.push('/');
    }
    if absolute { format!("/{out}") } else { out }
}

/// `path.posix.join`: joins the non-empty segments with `/` and normalizes the result.
pub fn join(base: &str, name: &str) -> String {
    let joined = [base, name]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() {
        ".".into()
    } else {
        normalize(&joined)
    }
}

/// `path.posix.basename`: the last segment, ignoring trailing slashes.
pub fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_like_node_posix_paths() {
        assert_eq!(normalize("/srv//app/./x/../y"), "/srv/app/y");
        assert_eq!(normalize("/../etc"), "/etc");
        assert_eq!(normalize("a/../../b"), "../b");
        assert_eq!(normalize("a/.."), ".");
        assert_eq!(normalize("/a/b/"), "/a/b/");
        assert_eq!(normalize(""), ".");
        assert_eq!(normalize("/"), "/");
    }

    #[test]
    fn joins_and_takes_basenames() {
        assert_eq!(join("/srv/app", "notes.txt"), "/srv/app/notes.txt");
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join(".", "a"), "a");
        assert_eq!(join("/srv", "../etc"), "/etc");
        assert_eq!(basename("/remote/report.txt"), "report.txt");
        assert_eq!(basename("/remote/dir/"), "dir");
        assert_eq!(basename("plain"), "plain");
    }
}
