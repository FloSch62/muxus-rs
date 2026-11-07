//! Typing a local text file into a session (port of the pure half of
//! `client/src/terminal/send-text-file.ts`; asking for the file and typing it live in the app).

use muxus_core::paste_pacing::MAX_PACED_PASTE_LENGTH;

pub use crate::send_break::can_send_break as can_send_text_file;

/// A file's text, or why it cannot be typed. NUL bytes or invalid UTF-8 mean binary; a UTF-8 BOM
/// is dropped. Line breaks stay as they are: typing sends CRLF, LF and CR each as one press of
/// the host's Enter key.
pub fn text_file_content(name: &str, bytes: &[u8]) -> Result<String, String> {
    let binary = || format!("“{name}” is not a text file, or not in UTF-8.");
    if bytes.contains(&0) {
        return Err(binary());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| binary())?;
    Ok(text.strip_prefix('\u{feff}').unwrap_or(text).to_string())
}

/// Why a file of `size` bytes is refused before it is read, if it is. Typing sends longer text
/// unpaced; UTF-8 never takes fewer bytes than UTF-16 code units, so the size bounds the text.
pub fn text_file_size_problem(name: &str, size: u64) -> Option<String> {
    (size > MAX_PACED_PASTE_LENGTH as u64).then(|| {
        format!(
            "“{name}” is larger than {} MB. Send it in parts.",
            MAX_PACED_PASTE_LENGTH / 1_000_000
        )
    })
}

/// What a file says once read: its text to type, or the problem and whether it is only an
/// empty file (`info` rather than `error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextFileOutcome {
    Type(String),
    Empty(String),
    Problem(String),
}

/// The whole check of `sendTextFile` before typing: size, binary content and emptiness.
pub fn text_file_outcome(name: &str, size: u64, read: impl FnOnce() -> Vec<u8>) -> TextFileOutcome {
    if let Some(problem) = text_file_size_problem(name, size) {
        return TextFileOutcome::Problem(problem);
    }
    match text_file_content(name, &read()) {
        Err(problem) => TextFileOutcome::Problem(problem),
        Ok(text) if text.is_empty() => TextFileOutcome::Empty(format!("“{name}” is empty.")),
        Ok(text) => TextFileOutcome::Type(text),
    }
}

/// Port of the pure parts of `tests/unit/client/send-text-file.test.ts`.
#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "hostname R1\r\ninterface Gi0/1\r\n description uplink\r\n";

    #[test]
    fn decodes_utf8_drops_a_bom_and_leaves_line_breaks_to_the_typing_path() {
        let bytes = format!("\u{feff}{CONFIG}banner motd ^Ü^\r");
        assert_eq!(
            text_file_content("R1.cfg", bytes.as_bytes()),
            Ok(format!("{CONFIG}banner motd ^Ü^\r"))
        );
    }

    #[test]
    fn refuses_nul_bytes_and_invalid_utf8_as_binary() {
        let problem = Err("“R1.cfg” is not a text file, or not in UTF-8.".to_string());
        // A UTF-16 file, as Windows Notepad saves "Unicode".
        assert_eq!(
            text_file_content("R1.cfg", &[0xff, 0xfe, 0x68, 0x00]),
            problem
        );
        // Latin-1 "é".
        assert_eq!(
            text_file_content("R1.cfg", &[0x63, 0x61, 0x66, 0xe9]),
            problem
        );
    }

    #[test]
    fn refuses_a_file_over_the_paced_paste_limit_before_reading_it() {
        let outcome = text_file_outcome("big.cfg", MAX_PACED_PASTE_LENGTH as u64 + 1, || {
            panic!("read")
        });
        assert_eq!(
            outcome,
            TextFileOutcome::Problem("“big.cfg” is larger than 1 MB. Send it in parts.".into())
        );
    }

    #[test]
    fn says_an_empty_file_is_empty_bom_or_not() {
        for content in [&b""[..], "\u{feff}".as_bytes()] {
            assert_eq!(
                text_file_outcome("R1.cfg", content.len() as u64, || content.to_vec()),
                TextFileOutcome::Empty("“R1.cfg” is empty.".into())
            );
        }
        assert_eq!(
            text_file_outcome("R1.cfg", CONFIG.len() as u64, || CONFIG.into()),
            TextFileOutcome::Type(CONFIG.into())
        );
    }
}
