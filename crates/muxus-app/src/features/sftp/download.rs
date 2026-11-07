//! Saving several downloaded files at once (port of `client/src/sftp-download.ts`). One file
//! asks where to save it; several ask for a folder once and every file is written into it,
//! without replacing anything there: a name already taken gets a number, as browsers number
//! repeated downloads.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

/// Characters Windows refuses in a file name, path separators included.
const UNSAFE_NAME_CHARACTERS: &str = "\\/:*?\"<>|";

/// A remote name that every local file system accepts; browsers clean download names the same
/// way.
pub fn local_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if UNSAFE_NAME_CHARACTERS.contains(character) || (character as u32) < 32 {
                '_'
            } else {
                character
            }
        })
        .collect();
    // Windows cannot keep trailing dots and spaces on a name.
    let cleaned = cleaned.trim_end_matches(['.', ' ']);
    if cleaned.is_empty() {
        "download".into()
    } else {
        cleaned.into()
    }
}

/// "report.txt", then "report (1).txt", "report (2).txt" …; a dotfile keeps its leading dot.
pub fn numbered_file_name(name: &str, copy: usize) -> String {
    if copy == 0 {
        return name.into();
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} ({copy}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({copy})"),
    }
}

/// Write `bytes` into `folder` without replacing anything there: a taken name (a file or a
/// folder) gets a number. Returns the name written.
pub fn save_into_folder(folder: &Path, name: &str, bytes: &[u8]) -> io::Result<String> {
    let base = local_file_name(name);
    for copy in 0.. {
        let candidate = numbered_file_name(&base, copy);
        let path = folder.join(&candidate);
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        return Ok(candidate);
    }
    unreachable!("a free name is always found")
}

/// "Downloaded 3 files", or "Downloaded 3 files to Downloads" when the folder has a name.
pub fn downloaded_message(count: usize, folder: &Path) -> String {
    let summary = format!("{count} files");
    match folder.file_name().and_then(|name| name.to_str()) {
        Some(name) if !name.is_empty() => format!("Downloaded {summary} to {name}"),
        _ => format!("Downloaded {summary}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_file_under_its_own_name() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(
            save_into_folder(folder.path(), "report.txt", b"payload").unwrap(),
            "report.txt"
        );
        assert_eq!(
            std::fs::read(folder.path().join("report.txt")).unwrap(),
            b"payload"
        );
    }

    #[test]
    fn numbers_a_name_that_is_taken_instead_of_replacing_what_is_there() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("report.txt"), "old").unwrap();
        std::fs::create_dir(folder.path().join("report (1).txt")).unwrap();
        assert_eq!(
            save_into_folder(folder.path(), "report.txt", b"new").unwrap(),
            "report (2).txt"
        );
        assert_eq!(
            std::fs::read_to_string(folder.path().join("report.txt")).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(folder.path().join("report (2).txt")).unwrap(),
            "new"
        );
    }

    #[test]
    fn cleans_remote_names_that_local_file_systems_refuse() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(
            save_into_folder(folder.path(), "show run: 10/24?.txt", b"x").unwrap(),
            "show run_ 10_24_.txt"
        );
        assert!(folder.path().join("show run_ 10_24_.txt").is_file());
    }

    #[test]
    fn numbers_before_the_extension_and_after_a_dotfile() {
        assert_eq!(numbered_file_name("report.txt", 0), "report.txt");
        assert_eq!(numbered_file_name("report.txt", 3), "report (3).txt");
        assert_eq!(numbered_file_name(".bashrc", 1), ".bashrc (1)");
        assert_eq!(numbered_file_name("Makefile", 1), "Makefile (1)");
    }

    #[test]
    fn replaces_separators_reserved_and_control_characters_and_trailing_dots() {
        assert_eq!(local_file_name("a\\b|c*\"d\"<e>"), "a_b_c__d__e_");
        assert_eq!(local_file_name("tab\there"), "tab_here");
        assert_eq!(local_file_name("notes. "), "notes");
        assert_eq!(local_file_name(".."), "download");
    }

    #[test]
    fn names_the_folder_in_the_message() {
        assert_eq!(
            downloaded_message(3, Path::new("/home/me/Downloads")),
            "Downloaded 3 files to Downloads"
        );
        assert_eq!(downloaded_message(2, Path::new("/")), "Downloaded 2 files");
    }
}
