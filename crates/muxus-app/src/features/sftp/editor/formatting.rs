//! Format Document (`editor.action.formatDocument`) for the languages Monaco's workers format
//! in the client: JSON (the JSON worker), CSS, SCSS and Less (the CSS worker). Every other
//! language has no formatter, as in Monaco (the HTML and TypeScript workers' are not ported;
//! see `docs/parity/sftp.md`).

use super::css_format::format_css;
use super::json::{FormatOptions, format_json};

/// Whether Format Document does anything for the Monaco language `language`.
pub fn can_format(language: &str) -> bool {
    matches!(language, "json" | "css" | "scss" | "less")
}

/// `text` formatted for `language` with the editor's indentation, in the text's line
/// endings (Monaco's model keeps its own end of line for the edit); `None` without a
/// formatter.
pub fn format_document(
    language: &str,
    text: &str,
    tab_size: usize,
    insert_spaces: bool,
) -> Option<String> {
    let options = FormatOptions {
        tab_size,
        insert_spaces,
    };
    match language {
        "json" => Some(format_json(text, options)),
        "css" | "scss" | "less" => {
            let formatted = format_css(text, tab_size, insert_spaces);
            Some(if text.contains("\r\n") {
                formatted.replace('\n', "\r\n")
            } else {
                formatted
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_languages_monaco_formats() {
        assert_eq!(
            format_document("json", "{\"a\":1}", 2, true).as_deref(),
            Some("{\n  \"a\": 1\n}")
        );
        assert_eq!(
            format_document("scss", "a{b:c}", 4, true).as_deref(),
            Some("a {\n    b: c\n}")
        );
        // The model's line endings stay.
        assert_eq!(
            format_document("css", "a{b:c}\r\n", 2, true).as_deref(),
            Some("a {\r\n  b: c\r\n}")
        );
        assert_eq!(format_document("yaml", "a: 1", 2, true), None);
        assert!(can_format("less") && !can_format("python"));
    }
}
