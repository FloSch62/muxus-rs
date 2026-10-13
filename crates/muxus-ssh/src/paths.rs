//! Small path helpers with Node's `path.resolve` / `path.relative` semantics (lexical, no
//! symlink resolution).

use std::path::{Component, Path, PathBuf};

/// `path.resolve(p)`: absolute against the current directory, `.` and `..` folded lexically.
pub fn lexical_absolute(p: &Path) -> PathBuf {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    normalize(&joined)
}

/// Folds `.` and `..` components without touching the file system.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in p.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Whether `child` is `base` or sits below it, compared lexically.
pub fn is_within(child: &Path, base: &Path) -> bool {
    normalize(child).starts_with(normalize(base))
}

/// `path.relative(from, to)` for the case where `to` is inside `from`; otherwise the absolute
/// `to`.
pub fn relative_inside(from: &Path, to: &Path) -> PathBuf {
    normalize(to)
        .strip_prefix(normalize(from))
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| to.to_path_buf())
}

/// A path as display text (lossy on non-UTF-8 names).
pub fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}
