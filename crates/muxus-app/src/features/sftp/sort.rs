//! The column the file browser sorts by (port of `client/src/sftp-sort.ts`). Folders stay above
//! everything else either way.

use std::cmp::Ordering;

use muxus_core::api::{SftpEntry, SftpEntryType};
use muxus_core::collate::locale_compare;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SftpSortKey {
    Name,
    Size,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SftpSort {
    pub key: SftpSortKey,
    pub descending: bool,
}

pub const DEFAULT_SFTP_SORT: SftpSort = SftpSort {
    key: SftpSortKey::Name,
    descending: false,
};

impl Default for SftpSort {
    fn default() -> Self {
        DEFAULT_SFTP_SORT
    }
}

/// A header click: the sorted column reverses; another starts A to Z, or largest and newest
/// first.
pub fn next_sftp_sort(current: SftpSort, key: SftpSortKey) -> SftpSort {
    if current.key == key {
        SftpSort {
            key,
            descending: !current.descending,
        }
    } else {
        SftpSort {
            key,
            descending: key != SftpSortKey::Name,
        }
    }
}

/// Only files show a size; anything else sorts below an empty file.
fn shown_size(entry: &SftpEntry) -> i128 {
    match (entry.kind, entry.size) {
        (SftpEntryType::File, Some(size)) => i128::from(size),
        _ => -1,
    }
}

/// The listing in `sort` order; ties, such as folders by size, go by name A to Z.
pub fn sorted_sftp_entries(entries: &[SftpEntry], sort: SftpSort) -> Vec<SftpEntry> {
    let directed = |ordering: Ordering| {
        if sort.descending {
            ordering.reverse()
        } else {
            ordering
        }
    };
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
        let by_column = match sort.key {
            SftpSortKey::Name => return directed(locale_compare(&a.name, &b.name)),
            SftpSortKey::Size => shown_size(a).cmp(&shown_size(b)),
            SftpSortKey::Modified => a.mtime_ms.unwrap_or(0).cmp(&b.mtime_ms.unwrap_or(0)),
        };
        directed(by_column).then_with(|| locale_compare(&a.name, &b.name))
    });
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        name: &str,
        kind: SftpEntryType,
        size: Option<u64>,
        mtime_ms: Option<i64>,
    ) -> SftpEntry {
        SftpEntry {
            name: name.into(),
            kind,
            size,
            mtime_ms,
            mode: None,
            owner: None,
            group: None,
            download_ticket: None,
        }
    }

    fn listing() -> Vec<SftpEntry> {
        vec![
            entry("b.log", SftpEntryType::File, Some(300), Some(3_000)),
            entry("logs", SftpEntryType::Dir, Some(4096), Some(1_000)),
            entry("a.txt", SftpEntryType::File, Some(10), Some(5_000)),
            entry("current", SftpEntryType::Link, Some(50), Some(4_000)),
            entry("C.bin", SftpEntryType::File, Some(300), Some(2_000)),
            entry("archive", SftpEntryType::Dir, Some(64), Some(6_000)),
        ]
    }

    fn names_of(entries: &[SftpEntry], key: SftpSortKey, descending: bool) -> Vec<String> {
        sorted_sftp_entries(entries, SftpSort { key, descending })
            .into_iter()
            .map(|entry| entry.name)
            .collect()
    }

    fn names(key: SftpSortKey, descending: bool) -> Vec<String> {
        names_of(&listing(), key, descending)
    }

    #[test]
    fn lists_folders_first_then_names_a_to_z_by_default() {
        assert_eq!(
            sorted_sftp_entries(&listing(), DEFAULT_SFTP_SORT)
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            ["archive", "logs", "a.txt", "b.log", "C.bin", "current"]
        );
        assert_eq!(SftpSort::default(), DEFAULT_SFTP_SORT);
    }

    #[test]
    fn orders_names_as_locale_compare_does() {
        let listing = [
            entry("README.md", SftpEntryType::File, None, None),
            entry("logs", SftpEntryType::Dir, None, None),
            entry("deploy.sh", SftpEntryType::File, None, None),
            entry("backups", SftpEntryType::Dir, None, None),
        ];
        assert_eq!(
            names_of(&listing, SftpSortKey::Name, false),
            ["backups", "logs", "deploy.sh", "README.md"]
        );
    }

    #[test]
    fn reverses_names_within_folders_and_files_folders_still_first() {
        assert_eq!(
            names(SftpSortKey::Name, true),
            ["logs", "archive", "current", "C.bin", "b.log", "a.txt"]
        );
    }

    #[test]
    fn sorts_files_by_size_and_folders_by_name_equal_sizes_by_name() {
        assert_eq!(
            names(SftpSortKey::Size, true),
            ["archive", "logs", "b.log", "C.bin", "a.txt", "current"]
        );
        assert_eq!(
            names(SftpSortKey::Size, false),
            ["archive", "logs", "current", "a.txt", "b.log", "C.bin"]
        );
    }

    #[test]
    fn sorts_folders_and_files_by_modification_time_each_on_their_own() {
        assert_eq!(
            names(SftpSortKey::Modified, true),
            ["archive", "logs", "a.txt", "current", "b.log", "C.bin"]
        );
        assert_eq!(
            names(SftpSortKey::Modified, false),
            ["logs", "archive", "C.bin", "b.log", "current", "a.txt"]
        );
    }

    #[test]
    fn ranks_a_missing_size_or_time_as_the_smallest() {
        let listing = [
            entry("upload.part", SftpEntryType::File, None, None),
            entry("empty.txt", SftpEntryType::File, Some(0), Some(1_000)),
        ];
        assert_eq!(
            names_of(&listing, SftpSortKey::Size, false),
            ["upload.part", "empty.txt"]
        );
        assert_eq!(
            names_of(&listing, SftpSortKey::Modified, true),
            ["empty.txt", "upload.part"]
        );
    }

    #[test]
    fn leaves_the_listing_it_was_given_in_place() {
        let listing = listing();
        let before: Vec<String> = listing.iter().map(|entry| entry.name.clone()).collect();
        names_of(&listing, SftpSortKey::Size, true);
        assert_eq!(
            listing
                .iter()
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn reverses_the_sorted_column() {
        assert_eq!(
            next_sftp_sort(DEFAULT_SFTP_SORT, SftpSortKey::Name),
            SftpSort {
                key: SftpSortKey::Name,
                descending: true
            }
        );
        let by_size = SftpSort {
            key: SftpSortKey::Size,
            descending: true,
        };
        assert_eq!(
            next_sftp_sort(by_size, SftpSortKey::Size),
            SftpSort {
                key: SftpSortKey::Size,
                descending: false
            }
        );
    }

    #[test]
    fn starts_names_at_a_and_sizes_and_times_at_the_largest_and_newest() {
        let oldest_first = SftpSort {
            key: SftpSortKey::Modified,
            descending: false,
        };
        assert_eq!(
            next_sftp_sort(oldest_first, SftpSortKey::Name),
            DEFAULT_SFTP_SORT
        );
        assert_eq!(
            next_sftp_sort(DEFAULT_SFTP_SORT, SftpSortKey::Size),
            SftpSort {
                key: SftpSortKey::Size,
                descending: true
            }
        );
        let z_to_a = SftpSort {
            key: SftpSortKey::Name,
            descending: true,
        };
        assert_eq!(
            next_sftp_sort(z_to_a, SftpSortKey::Modified),
            SftpSort {
                key: SftpSortKey::Modified,
                descending: true
            }
        );
    }
}
