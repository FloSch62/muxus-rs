//! Importing several color scheme files at once (`readTerminalSchemeFiles`,
//! `applyTerminalSchemeImport` and `summarizeTerminalSchemeImport` of
//! `client/src/terminal/custom-schemes.ts`): a file that cannot be read does not stop the others,
//! a scheme whose colors the user already has is skipped, and one message reports the result.

use std::collections::HashSet;

use crate::custom_schemes::{
    CustomTerminalScheme, MAX_TERMINAL_SCHEME_FILE_BYTES, SCHEME_COLOR_KEYS, SchemeError,
    merge_custom_terminal_schemes, parse_terminal_scheme_file,
};

/// A file whose schemes could not be read, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSchemeFileFailure {
    pub filename: String,
    pub message: String,
}

/// The schemes of every file that could be read, and the files that could not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalSchemeFilesRead {
    pub schemes: Vec<CustomTerminalScheme>,
    pub failures: Vec<TerminalSchemeFileFailure>,
}

/// One picked file: its name, its size, and how to read it as text. The size is checked before
/// the file is read, so an oversized file is never loaded.
pub struct TerminalSchemeFile<F: FnOnce() -> Result<String, String>> {
    pub filename: String,
    pub size: u64,
    pub read: F,
}

/// Read the schemes in several files; a file that cannot be read does not stop the others.
pub fn read_terminal_scheme_files<F>(
    files: impl IntoIterator<Item = TerminalSchemeFile<F>>,
) -> TerminalSchemeFilesRead
where
    F: FnOnce() -> Result<String, String>,
{
    let mut result = TerminalSchemeFilesRead::default();
    for file in files {
        let read = if file.size > MAX_TERMINAL_SCHEME_FILE_BYTES as u64 {
            Err("That color scheme file is too large.".to_string())
        } else {
            (file.read)().and_then(|text| {
                parse_terminal_scheme_file(&text, &file.filename).map_err(|error| error.0)
            })
        };
        match read {
            Ok(schemes) => result.schemes.extend(schemes),
            Err(message) => result.failures.push(TerminalSchemeFileFailure {
                filename: file.filename,
                message,
            }),
        }
    }
    result
}

/// What an import changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSchemeImport {
    /// The user's schemes with the import applied.
    pub schemes: Vec<CustomTerminalScheme>,
    /// The stored form of each scheme the import added or updated.
    pub imported: Vec<CustomTerminalScheme>,
    /// Schemes left out because the user already has the same colors.
    pub already_present: usize,
}

/// Apply imported schemes, leaving out any whose colors the user already has, so importing the
/// same files twice adds nothing. The name cannot decide this: a scheme named like a built-in is
/// stored under another name. A known id always goes through, so an updated export can still
/// rename its scheme.
pub fn apply_terminal_scheme_import(
    current: &[CustomTerminalScheme],
    imported: &[CustomTerminalScheme],
) -> Result<TerminalSchemeImport, SchemeError> {
    let known_ids: HashSet<&str> = current.iter().map(|scheme| scheme.id.as_str()).collect();
    let mut known_colors: HashSet<String> = current.iter().map(scheme_colors_key).collect();
    let accepted: Vec<CustomTerminalScheme> = imported
        .iter()
        .filter(|scheme| {
            if known_ids.contains(scheme.id.as_str()) {
                return true;
            }
            known_colors.insert(scheme_colors_key(scheme))
        })
        .cloned()
        .collect();
    let schemes = merge_custom_terminal_schemes(current, &accepted)?;
    let imported_now = accepted
        .iter()
        .filter_map(|scheme| {
            schemes
                .iter()
                .find(|candidate| candidate.id == scheme.id)
                .cloned()
        })
        .collect();
    Ok(TerminalSchemeImport {
        schemes,
        imported: imported_now,
        already_present: imported.len() - accepted.len(),
    })
}

/// How an import's message is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSeverity {
    Success,
    Info,
    Warning,
    Error,
}

/// The one message that reports an import of any number of files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSchemeImportSummary {
    pub severity: ImportSeverity,
    pub message: String,
    /// Each failed file and why, for the toast's details.
    pub details: Option<String>,
}

/// `summarizeTerminalSchemeImport`.
pub fn summarize_terminal_scheme_import(
    imported: &[CustomTerminalScheme],
    already_present: usize,
    failures: &[TerminalSchemeFileFailure],
) -> TerminalSchemeImportSummary {
    let nothing_else = imported.is_empty() && already_present == 0;
    if nothing_else && failures.len() == 1 {
        return TerminalSchemeImportSummary {
            severity: ImportSeverity::Error,
            message: failures[0].message.clone(),
            details: None,
        };
    }
    let mut parts: Vec<String> = Vec::new();
    match imported {
        [] => {}
        [one] => parts.push(format!("Imported {}.", one.name)),
        many => parts.push(format!("Imported {} color schemes.", many.len())),
    }
    let others = !imported.is_empty() || !failures.is_empty();
    match already_present {
        0 => {}
        1 => parts.push(if others {
            "1 was already in your list.".into()
        } else {
            "That color scheme is already in your list.".into()
        }),
        count => parts.push(if others {
            format!("{count} were already in your list.")
        } else {
            "Those color schemes are already in your list.".into()
        }),
    }
    if failures.is_empty() {
        return TerminalSchemeImportSummary {
            severity: if imported.is_empty() {
                ImportSeverity::Info
            } else {
                ImportSeverity::Success
            },
            message: parts.join(" "),
            details: None,
        };
    }
    parts.push(if failures.len() == 1 {
        "1 file could not be imported.".into()
    } else {
        format!("{} files could not be imported.", failures.len())
    });
    TerminalSchemeImportSummary {
        severity: if nothing_else {
            ImportSeverity::Error
        } else {
            ImportSeverity::Warning
        },
        message: parts.join(" "),
        details: Some(
            failures
                .iter()
                .map(|failure| format!("{}: {}", failure.filename, failure.message))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    }
}

fn scheme_colors_key(scheme: &CustomTerminalScheme) -> String {
    SCHEME_COLOR_KEYS
        .iter()
        .map(|key| scheme.colors.get(key).unwrap_or_default().to_lowercase())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    //! The multi-file import cases of `tests/unit/client/custom-terminal-schemes.test.ts`.

    use super::*;
    use crate::custom_schemes::MAX_CUSTOM_TERMINAL_SCHEMES;
    use serde_json::{Value, json};

    fn frappe_json() -> Value {
        json!({ "id": "scheme-frappe", "name": "Catppuccin Frappé", "colors": {
            "background": "#303446", "foreground": "#c6d0f5", "cursor": "#f2d5cf",
            "selectionBackground": "#626880", "black": "#51576d", "red": "#e78284",
            "green": "#a6d189", "yellow": "#e5c890", "blue": "#8caaee", "magenta": "#f4b8e4",
            "cyan": "#81c8be", "white": "#b5bfe2", "brightBlack": "#626880", "brightRed": "#e78284",
            "brightGreen": "#a6d189", "brightYellow": "#e5c890", "brightBlue": "#8caaee",
            "brightMagenta": "#f4b8e4", "brightCyan": "#81c8be", "brightWhite": "#a5adce",
        }})
    }

    fn frappe() -> CustomTerminalScheme {
        serde_json::from_value(frappe_json()).unwrap()
    }

    /// A scheme whose colors differ from every other `index`.
    fn with_background(index: u32, id: &str) -> CustomTerminalScheme {
        let mut scheme = frappe();
        scheme.id = id.into();
        scheme.name = format!("Scheme {id}");
        scheme.colors.background = format!("#{index:06x}");
        scheme
    }

    fn windows_terminal(name: &str, background: &str) -> String {
        json!({
            "name": name, "cursorColor": "#F2D5CF", "selectionBackground": "#626880",
            "background": background, "foreground": "#C6D0F5", "black": "#51576D", "red": "#E78284",
            "green": "#A6D189", "yellow": "#E5C890", "blue": "#8CAAEE", "purple": "#F4B8E4",
            "cyan": "#81C8BE", "white": "#B5BFE2", "brightBlack": "#626880", "brightRed": "#E78284",
            "brightGreen": "#A6D189", "brightYellow": "#E5C890", "brightBlue": "#8CAAEE",
            "brightPurple": "#F4B8E4", "brightCyan": "#81C8BE", "brightWhite": "#A5ADCE",
        })
        .to_string()
    }

    type Reader = Box<dyn FnOnce() -> Result<String, String>>;

    fn file(name: &str, text: String) -> TerminalSchemeFile<Reader> {
        TerminalSchemeFile {
            filename: name.into(),
            size: text.len() as u64,
            read: Box::new(move || Ok(text)),
        }
    }

    fn read_one_two() -> TerminalSchemeFilesRead {
        read_terminal_scheme_files([
            file("One.json", windows_terminal("One", "#111111")),
            file("Two.json", windows_terminal("Two", "#222222")),
        ])
    }

    #[test]
    fn reads_every_file_and_reports_the_ones_it_cannot_read() {
        let oversized = TerminalSchemeFile::<Reader> {
            filename: "huge.json".into(),
            size: MAX_TERMINAL_SCHEME_FILE_BYTES as u64 + 1,
            read: Box::new(|| panic!("an oversized file is not read")),
        };
        let read = read_terminal_scheme_files([
            file("One.json", windows_terminal("One", "#111111")),
            file("broken.json", "not a scheme".into()),
            oversized,
            file("Two.json", windows_terminal("Two", "#222222")),
        ]);
        let names: Vec<&str> = read.schemes.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["One", "Two"]);
        assert_eq!(
            read.failures,
            [
                TerminalSchemeFileFailure {
                    filename: "broken.json".into(),
                    message: "This file is neither a JSON color scheme nor an iTerm2 color preset."
                        .into(),
                },
                TerminalSchemeFileFailure {
                    filename: "huge.json".into(),
                    message: "That color scheme file is too large.".into(),
                },
            ]
        );
    }

    #[test]
    fn adds_nothing_when_the_same_files_are_imported_again() {
        let first = apply_terminal_scheme_import(&[], &read_one_two().schemes).unwrap();
        assert_eq!(first.imported.len(), 2);
        assert_eq!(first.already_present, 0);

        let again = apply_terminal_scheme_import(&first.schemes, &read_one_two().schemes).unwrap();
        assert_eq!(again.schemes, first.schemes);
        assert!(again.imported.is_empty());
        assert_eq!(again.already_present, 2);
    }

    #[test]
    fn recognizes_a_scheme_stored_under_another_name_than_the_builtin_it_shares_a_name_with() {
        let imported = CustomTerminalScheme {
            name: "Dracula".into(),
            ..with_background(1, "scheme-new")
        };
        let first = apply_terminal_scheme_import(&[], std::slice::from_ref(&imported)).unwrap();
        assert_eq!(first.imported[0].name, "Dracula 2");

        let again = apply_terminal_scheme_import(
            &first.schemes,
            &[CustomTerminalScheme {
                id: "scheme-newer".into(),
                ..imported
            }],
        )
        .unwrap();
        assert_eq!(again.schemes, first.schemes);
        assert_eq!(again.already_present, 1);
    }

    #[test]
    fn keeps_one_of_several_identical_schemes_in_the_same_import() {
        let result = apply_terminal_scheme_import(
            &[],
            &[
                with_background(1, "scheme-a"),
                with_background(1, "scheme-b"),
            ],
        )
        .unwrap();
        let ids: Vec<&str> = result.schemes.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["scheme-a"]);
        assert_eq!(result.already_present, 1);
    }

    #[test]
    fn still_updates_a_scheme_with_a_known_id_when_only_its_name_changed() {
        let renamed = CustomTerminalScheme {
            name: "Frappé, renamed".into(),
            ..frappe()
        };
        let result =
            apply_terminal_scheme_import(&[frappe()], std::slice::from_ref(&renamed)).unwrap();
        assert_eq!(result.schemes, std::slice::from_ref(&renamed));
        assert_eq!(result.imported, [renamed]);
        assert_eq!(result.already_present, 0);
    }

    #[test]
    fn lets_schemes_that_are_already_present_stay_out_of_the_scheme_limit() {
        let full: Vec<CustomTerminalScheme> = (0..MAX_CUSTOM_TERMINAL_SCHEMES as u32)
            .map(|index| with_background(index, &format!("scheme-{index}")))
            .collect();
        let result =
            apply_terminal_scheme_import(&full, &[with_background(3, "scheme-repeat")]).unwrap();
        assert_eq!(result.schemes, full);
        assert_eq!(result.already_present, 1);
    }

    #[test]
    fn says_how_many_more_schemes_fit_when_an_import_is_too_large() {
        let current: Vec<CustomTerminalScheme> = (0..MAX_CUSTOM_TERMINAL_SCHEMES as u32 - 1)
            .map(|index| with_background(index, &format!("scheme-{index}")))
            .collect();
        let imported = [
            with_background(200, "scheme-a"),
            with_background(201, "scheme-b"),
        ];
        let error = merge_custom_terminal_schemes(&current, &imported).unwrap_err();
        assert!(error.0.contains("it adds 2, and 1 more fit"), "{}", error.0);
    }

    #[test]
    fn summarizes_an_import_in_one_message() {
        let one = CustomTerminalScheme {
            name: "One".into(),
            ..frappe()
        };
        let two = CustomTerminalScheme {
            id: "scheme-two".into(),
            name: "Two".into(),
            ..frappe()
        };
        let failure = TerminalSchemeFileFailure {
            filename: "broken.json".into(),
            message: "Not a scheme.".into(),
        };
        let other = TerminalSchemeFileFailure {
            filename: "other.json".into(),
            message: "Also not a scheme.".into(),
        };
        use ImportSeverity::*;
        type Case = (
            Vec<CustomTerminalScheme>,
            usize,
            Vec<TerminalSchemeFileFailure>,
            ImportSeverity,
            &'static str,
        );
        let cases: Vec<Case> = vec![
            (vec![one.clone()], 0, vec![], Success, "Imported One."),
            (
                vec![one.clone(), two.clone()],
                0,
                vec![],
                Success,
                "Imported 2 color schemes.",
            ),
            (
                vec![one.clone(), two.clone()],
                1,
                vec![],
                Success,
                "Imported 2 color schemes. 1 was already in your list.",
            ),
            (
                vec![one.clone()],
                2,
                vec![],
                Success,
                "Imported One. 2 were already in your list.",
            ),
            (
                vec![],
                1,
                vec![],
                Info,
                "That color scheme is already in your list.",
            ),
            (
                vec![],
                2,
                vec![],
                Info,
                "Those color schemes are already in your list.",
            ),
            (vec![], 0, vec![failure.clone()], Error, "Not a scheme."),
            (
                vec![],
                0,
                vec![failure.clone(), other.clone()],
                Error,
                "2 files could not be imported.",
            ),
            (
                vec![one.clone()],
                0,
                vec![failure.clone()],
                Warning,
                "Imported One. 1 file could not be imported.",
            ),
            (
                vec![],
                2,
                vec![failure.clone()],
                Warning,
                "2 were already in your list. 1 file could not be imported.",
            ),
        ];
        for (imported, already, failures, severity, message) in cases {
            let summary = summarize_terminal_scheme_import(&imported, already, &failures);
            assert_eq!(summary.severity, severity, "{message}");
            assert_eq!(summary.message, message);
        }
        assert_eq!(
            summarize_terminal_scheme_import(&[one], 0, &[failure, other]).details,
            Some("broken.json: Not a scheme.\nother.json: Also not a scheme.".into())
        );
    }
}
