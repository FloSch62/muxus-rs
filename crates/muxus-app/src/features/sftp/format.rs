//! Formatting shared by the file browser, the editor and the transfer bars:
//! sizes (`formatSize` in `TransferProgress.tsx`), speeds, modification
//! times and remote path helpers of `SftpPanel.tsx`.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::{Local, TimeZone};
use muxus_core::api::{SftpEntry, SftpEntryType};
use muxus_core::collate::locale_compare;

pub use muxus_core::js::to_fixed;

/// `formatSize`: bytes below 1 KiB, else one decimal under 10 of a unit.
pub fn format_size(size: Option<u64>) -> String {
    let Some(size) = size else {
        return String::new();
    };
    format_size_f(size as f64)
}

fn format_size_f(size: f64) -> String {
    if size < 1024. {
        return format!("{size} B");
    }
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    let mut value = size;
    let mut unit = 0usize;
    value /= 1024.;
    while value >= 1024. && unit < UNITS.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    format!(
        "{} {}",
        to_fixed(value, if value >= 10. { 0 } else { 1 }),
        UNITS[unit]
    )
}

/// `formatSpeed`: a rate once one is known.
pub fn format_speed(bytes_per_second: f64) -> String {
    if bytes_per_second > 0. {
        // Speeds are fractional; `formatSize` prints them as the number they are.
        let text = if bytes_per_second < 1024. {
            format!("{} B", js_number(bytes_per_second))
        } else {
            format_size_f(bytes_per_second)
        };
        format!("{text}/s")
    } else {
        "Starting…".into()
    }
}

/// A JavaScript number as `String(n)` prints it: both print the shortest
/// text that reads back as the same value.
fn js_number(value: f64) -> String {
    format!("{value}")
}

// A directory listing re-renders on every selection and transfer tick, and a
// timestamp always renders the same string.
const MTIME_CACHE_LIMIT: usize = 4_000;

thread_local! {
    static MTIME_LABELS: RefCell<HashMap<i64, String>> = RefCell::new(HashMap::new());
}

/// `formatMtime`: `YYYY-MM-DD HH:MM` in local time; nothing for 0 or unknown.
pub fn format_mtime(ms: Option<i64>) -> String {
    let Some(ms) = ms.filter(|ms| *ms != 0) else {
        return String::new();
    };
    if let Some(label) = MTIME_LABELS.with(|cache| cache.borrow().get(&ms).cloned()) {
        return label;
    }
    let label = match Local.timestamp_millis_opt(ms).single() {
        Some(date) => date.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    };
    MTIME_LABELS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= MTIME_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(ms, label.clone());
    });
    label
}

/// `parentPath`: the folder above `dir`, `/` at the top.
pub fn parent_path(dir: &str) -> String {
    if dir == "/" {
        return "/".into();
    }
    match dir.rfind('/') {
        Some(index) if index > 0 => dir[..index].to_string(),
        _ => "/".into(),
    }
}

/// `baseName` of the editor: the last path segment, or the path itself.
pub fn base_name(path: &str) -> String {
    let name = &path[path.rfind('/').map_or(0, |index| index + 1)..];
    if name.is_empty() {
        path.to_string()
    } else {
        name.to_string()
    }
}

/// Folders first, then by name as `localeCompare` orders them.
pub fn sort_entries(entries: &[SftpEntry]) -> Vec<SftpEntry> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| {
        let (a_dir, b_dir) = (a.kind == SftpEntryType::Dir, b.kind == SftpEntryType::Dir);
        if a_dir != b_dir {
            return if a_dir {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        locale_compare(&a.name, &b.name)
    });
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_fixed_rounds_ties_up_as_javascript_does() {
        assert_eq!(to_fixed(1.25, 1), "1.3");
        assert_eq!(to_fixed(1.05, 1), "1.1");
        assert_eq!(to_fixed(9.96, 1), "10.0");
        assert_eq!(to_fixed(10.5, 0), "11");
        assert_eq!(to_fixed(0.0, 1), "0.0");
        assert_eq!(to_fixed(2.0, 0), "2");
    }

    #[test]
    fn sizes_read_as_the_client_prints_them() {
        assert_eq!(format_size(None), "");
        assert_eq!(format_size(Some(0)), "0 B");
        assert_eq!(format_size(Some(198)), "198 B");
        assert_eq!(format_size(Some(1023)), "1023 B");
        assert_eq!(format_size(Some(1024)), "1.0 KiB");
        assert_eq!(format_size(Some(1536)), "1.5 KiB");
        assert_eq!(format_size(Some(10 * 1024)), "10 KiB");
        assert_eq!(format_size(Some(5 * 1024 * 1024)), "5.0 MiB");
        assert_eq!(format_size(Some(3 * 1024 * 1024 * 1024)), "3.0 GiB");
        assert_eq!(
            format_size(Some(2048 * 1024 * 1024 * 1024 * 1024)),
            "2048 TiB"
        );
    }

    #[test]
    fn speeds_say_starting_until_known() {
        assert_eq!(format_speed(0.), "Starting…");
        assert_eq!(format_speed(512.), "512 B/s");
        assert_eq!(format_speed(512.5), "512.5 B/s");
        assert_eq!(format_speed(2048.), "2.0 KiB/s");
    }

    #[test]
    fn modification_times_use_local_minutes() {
        assert_eq!(format_mtime(None), "");
        assert_eq!(format_mtime(Some(0)), "");
        let ms = Local
            .with_ymd_and_hms(2026, 7, 25, 20, 17, 42)
            .single()
            .unwrap()
            .timestamp_millis();
        assert_eq!(format_mtime(Some(ms)), "2026-07-25 20:17");
    }

    #[test]
    fn parent_paths_stop_at_the_root() {
        assert_eq!(parent_path("/"), "/");
        assert_eq!(parent_path("/home"), "/");
        assert_eq!(parent_path("/home/deploy"), "/home");
        assert_eq!(parent_path("relative"), "/");
        assert_eq!(base_name("/home/deploy/a.txt"), "a.txt");
        assert_eq!(base_name("/"), "/");
    }

    #[test]
    fn folders_sort_first() {
        let entry = |name: &str, kind| SftpEntry {
            name: name.into(),
            kind,
            size: None,
            mtime_ms: None,
            mode: None,
            owner: None,
            group: None,
            download_ticket: None,
        };
        let sorted = sort_entries(&[
            entry("README.md", SftpEntryType::File),
            entry("logs", SftpEntryType::Dir),
            entry("deploy.sh", SftpEntryType::File),
            entry("backups", SftpEntryType::Dir),
        ]);
        let names: Vec<_> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["backups", "logs", "deploy.sh", "README.md"]);
    }
}
